//! Lowering a [`GraphTemplate`] onto fundsp.
//!
//! This is the only file in the crate that knows fundsp exists, and it is the
//! only place a `Box<dyn AudioUnit>` is allowed to appear. Everything above it
//! is data.
//!
//! Instantiation runs **on the control thread, once per onset**, ahead of the
//! audio clock. It allocates freely — a `Net`, a unit per node, a constant per
//! lifted scalar — which is exactly why it must not happen anywhere else. Once
//! `Sequencer::push` hands the unit over it is rendered on the audio thread,
//! where an allocation is a dropout, nondeterministically, under load.
//!
//! The lookahead has to cover this work plus a GC pause: 100–200 ms makes a
//! 10 ms collection inaudible, and 20 ms does not.

use crate::control::{ControlError, ControlId, ControlLayout};
use crate::instrument::PatchTemplate;
use crate::note::Note;
use crate::routing::{BusLayout, EventRouting, RoutingError};
use crate::template::{GraphTemplate, Op, ShapeKind, Source, TemplateError};
use fundsp::net::{Net, NodeId as FundspNode};
use fundsp::prelude32::*;
use std::collections::HashMap;

/// Build one playable voice.
///
/// The returned unit has zero inputs, so it must be pushed to a `Sequencer`
/// built with zero inputs — 0.23's `push` asserts `unit.inputs() ==
/// self.inputs()`, not that either is zero. fundsp is therefore *not* the
/// reason a voice is a generator; that is this crate's choice, and the
/// lifetime question for `audio_input` stays open on its own merits. (0.20 did
/// assert zero. Do not carry that forward as an architectural argument.)
pub fn instantiate(
    template: &GraphTemplate,
    note: &Note,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    template.validate()?;
    if !template.sends.is_empty() {
        return Err(LowerError::LayoutRequired);
    }

    instantiate_to_lanes(
        template,
        note,
        None,
        template.channels(),
        template
            .outputs
            .iter()
            .enumerate()
            .map(|(lane, source)| (lane, *source, 1.0)),
    )
}

/// Instantiate a voice that may read program-scope writable controls.
pub fn instantiate_with_controls(
    template: &GraphTemplate,
    note: &Note,
    controls: &ControlStore,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    template.validate()?;
    if !template.sends.is_empty() {
        return Err(LowerError::LayoutRequired);
    }
    instantiate_to_lanes(
        template,
        note,
        Some(controls),
        template.channels(),
        template
            .outputs
            .iter()
            .enumerate()
            .map(|(lane, source)| (lane, *source, 1.0)),
    )
}

/// Instantiate one persistent patch. The caller keeps this unit alive instead
/// of submitting a fresh copy per onset.
pub fn instantiate_patch(
    patch: &PatchTemplate,
    controls: &ControlStore,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    instantiate_with_controls(patch.graph(), &Note::new(440.0), controls)
}

/// Build one voice whose outputs are the flattened main and bus stems.
///
/// This does not run bus effects. It makes routing concrete: the returned
/// channel order is exactly [`BusLayout`]'s order, ready for a persistent
/// processor to consume without learning anything about graph-local sources.
pub fn instantiate_routed(
    template: &GraphTemplate,
    note: &Note,
    layout: &BusLayout,
    event: &EventRouting,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    instantiate_routed_impl(template, note, layout, event, None)
}

pub fn instantiate_routed_with_controls(
    template: &GraphTemplate,
    note: &Note,
    layout: &BusLayout,
    event: &EventRouting,
    controls: &ControlStore,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    instantiate_routed_impl(template, note, layout, event, Some(controls))
}

fn instantiate_routed_impl(
    template: &GraphTemplate,
    note: &Note,
    layout: &BusLayout,
    event: &EventRouting,
    controls: Option<&ControlStore>,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    template.validate()?;
    if template.channels() != layout.main_channels() {
        return Err(RoutingError::MainChannelMismatch {
            template: template.channels(),
            layout: layout.main_channels(),
        }
        .into());
    }

    // `(flattened lane, graph source, gain)`. Graph sends already carry their
    // possibly symbolic gain as a Mul node; event sends bind one scalar per
    // onset and are scaled during concrete lowering.
    let mut routes = Vec::new();
    routes.extend(
        template
            .outputs
            .iter()
            .enumerate()
            .map(|(lane, source)| (lane, *source, 1.0)),
    );

    for send in &template.sends {
        let range = layout
            .bus_range(send.bus)
            .ok_or(RoutingError::UnknownBus(send.bus))?;
        if send.outputs.len() != range.len() {
            return Err(RoutingError::BusChannelMismatch {
                bus: send.bus,
                expected: range.len(),
                found: send.outputs.len(),
            }
            .into());
        }
        routes.extend(
            range
                .zip(&send.outputs)
                .map(|(lane, input)| (lane, input.source, 1.0)),
        );
    }

    for send in event.sends() {
        let range = layout
            .bus_range(send.bus)
            .ok_or(RoutingError::UnknownBus(send.bus))?;
        if template.channels() != range.len() {
            return Err(RoutingError::BusChannelMismatch {
                bus: send.bus,
                expected: range.len(),
                found: template.channels(),
            }
            .into());
        }
        routes.extend(
            range
                .zip(&template.outputs)
                .map(|(lane, source)| (lane, *source, send.level)),
        );
    }

    instantiate_to_lanes(template, note, controls, layout.total_channels(), routes)
}

fn instantiate_to_lanes(
    template: &GraphTemplate,
    note: &Note,
    controls: Option<&ControlStore>,
    output_channels: usize,
    routes: impl IntoIterator<Item = (usize, Source, f64)>,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    let mut net = Net::new(template.inputs, output_channels);

    // Push every node first, so wiring is a second pass and node order in the
    // template need not be a topological order for the walk to work. (It still
    // must be one — `validate` rejects forward references. A Delay node owns
    // history, but it does not make an arbitrary Net cycle well-defined;
    // feedback needs an explicit graph representation before it is legal.)
    let mut nodes: Vec<FundspNode> = Vec::with_capacity(template.nodes.len());
    for node in &template.nodes {
        nodes.push(net.push(unit_for(&node.op, note)));
    }

    // Lifted scalars, keyed by bit pattern. A voice typically reuses `0`, `1`
    // and a handful of literals; sharing them keeps the instantiated net closer
    // in size to the template than to its edge count.
    let mut resolver = Resolver {
        template,
        note,
        controls,
        nodes: &nodes,
        net: &mut net,
        constants: HashMap::new(),
        control_nodes: HashMap::new(),
        input_nodes: HashMap::new(),
    };

    for (index, node) in template.nodes.iter().enumerate() {
        for (port, input) in node.inputs.iter().enumerate() {
            let (from, channel) = resolver.resolve(input.source)?;
            resolver.net.connect(from, channel, nodes[index], port);
        }
    }

    let mut lanes = vec![Vec::new(); output_channels];
    for (lane, source, gain) in routes {
        let concrete = resolver.resolve(source)?;
        let concrete = if gain == 1.0 {
            concrete
        } else {
            let scaled = resolver.net.push(Box::new(mul(gain as f32)));
            resolver.net.connect(concrete.0, concrete.1, scaled, 0);
            (scaled, 0)
        };
        lanes[lane].push(concrete);
    }

    for (lane, sources) in lanes.into_iter().enumerate() {
        let (from, channel) = mix_sources(sources, resolver.net, &mut resolver.constants);
        resolver.net.connect_output(from, channel, lane);
    }

    drop(resolver);
    Ok(Box::new(net))
}

fn mix_sources(
    mut sources: Vec<(FundspNode, usize)>,
    net: &mut Net,
    constants: &mut HashMap<u64, FundspNode>,
) -> (FundspNode, usize) {
    let Some(mut mixed) = sources.pop() else {
        return (constant_node(0.0, net, constants), 0);
    };
    for source in sources {
        let add = net.push(Box::new(pass() + pass()));
        net.connect(mixed.0, mixed.1, add, 0);
        net.connect(source.0, source.1, add, 1);
        mixed = (add, 0);
    }
    mixed
}

#[derive(Clone, PartialEq, Debug)]
pub enum LowerError {
    Template(TemplateError),
    Routing(RoutingError),
    Control(ControlError),
    /// A graph-local send cannot be discarded by the main-only lowering path.
    LayoutRequired,
    ControlStoreRequired,
}

impl From<TemplateError> for LowerError {
    fn from(error: TemplateError) -> LowerError {
        LowerError::Template(error)
    }
}

impl From<RoutingError> for LowerError {
    fn from(error: RoutingError) -> LowerError {
        LowerError::Routing(error)
    }
}

impl From<ControlError> for LowerError {
    fn from(error: ControlError) -> LowerError {
        LowerError::Control(error)
    }
}

impl core::fmt::Display for LowerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LowerError::Template(error) => error.fmt(f),
            LowerError::Routing(error) => error.fmt(f),
            LowerError::Control(error) => error.fmt(f),
            LowerError::LayoutRequired => {
                write!(f, "graph has sends and requires a program bus layout")
            }
            LowerError::ControlStoreRequired => {
                write!(
                    f,
                    "graph has writable controls and requires a control store"
                )
            }
        }
    }
}

impl core::error::Error for LowerError {}

/// Turn a template source into a concrete `(node, channel)` pair, lifting
/// scalars and symbolic parameters to constants.
///
/// This is where "auto-lift scalars to `dc()`" happens, and it is why the
/// primitives are bound in their modulatable form — `lowpass()` with cutoff and
/// Q as inputs, never `lowpass_hz()`. One rule, and then any parameter of
/// anything accepts any signal, with a plain number as the degenerate case.
struct Resolver<'a> {
    template: &'a GraphTemplate,
    note: &'a Note,
    controls: Option<&'a ControlStore>,
    nodes: &'a [FundspNode],
    net: &'a mut Net,
    constants: HashMap<u64, FundspNode>,
    control_nodes: HashMap<ControlId, FundspNode>,
    input_nodes: HashMap<usize, FundspNode>,
}

impl Resolver<'_> {
    fn resolve(&mut self, source: Source) -> Result<(FundspNode, usize), LowerError> {
        match source {
            Source::Port { node, channel } => Ok((self.nodes[node], channel as usize)),
            Source::Const(x) => Ok((constant_node(x, self.net, &mut self.constants), 0)),
            Source::Param(id) => Ok((
                constant_node(
                    self.note.value(id, self.template),
                    self.net,
                    &mut self.constants,
                ),
                0,
            )),
            Source::Control(id) => {
                let controls = self.controls.ok_or(LowerError::ControlStoreRequired)?;
                let node = match self.control_nodes.get(&id) {
                    Some(node) => *node,
                    None => {
                        let shared = controls.shared(id)?;
                        let node = self.net.push(Box::new(var(shared)));
                        self.control_nodes.insert(id, node);
                        node
                    }
                };
                Ok((node, 0))
            }
            Source::Input(channel) => {
                let node = match self.input_nodes.get(&channel) {
                    Some(node) => *node,
                    None => {
                        let node = self.net.push(Box::new(pass()));
                        self.net.connect_input(channel, node, 0);
                        self.input_nodes.insert(channel, node);
                        node
                    }
                };
                Ok((node, 0))
            }
        }
    }
}

/// Backend values corresponding one-for-one with a data-only [`ControlLayout`].
pub struct ControlStore {
    layout: ControlLayout,
    values: Vec<Shared>,
}

impl ControlStore {
    pub fn new(layout: &ControlLayout) -> ControlStore {
        ControlStore {
            layout: layout.clone(),
            values: layout
                .specs()
                .iter()
                .map(|spec| shared(spec.default as f32))
                .collect(),
        }
    }

    pub fn set(&self, id: ControlId, value: f64) -> Result<(), ControlError> {
        if !value.is_finite() {
            return Err(ControlError::NonFiniteValue);
        }
        let spec = self
            .layout
            .spec(id)
            .ok_or(ControlError::UnknownControl(id))?;
        self.values[id.index()].set(spec.clamp(value) as f32);
        Ok(())
    }

    pub fn value(&self, id: ControlId) -> Result<f64, ControlError> {
        self.layout
            .spec(id)
            .ok_or(ControlError::UnknownControl(id))?;
        self.values
            .get(id.index())
            .map(|value| value.value() as f64)
            .ok_or(ControlError::UnknownControl(id))
    }

    fn shared(&self, id: ControlId) -> Result<&Shared, ControlError> {
        self.layout
            .spec(id)
            .ok_or(ControlError::UnknownControl(id))?;
        self.values
            .get(id.index())
            .ok_or(ControlError::UnknownControl(id))
    }
}

fn constant_node(x: f64, net: &mut Net, constants: &mut HashMap<u64, FundspNode>) -> FundspNode {
    *constants
        .entry((x as f32).to_bits() as u64)
        .or_insert_with(|| net.push(Box::new(dc(x as f32))))
}

fn unit_for(op: &Op, note: &Note) -> Box<dyn AudioUnit> {
    match op {
        Op::Sine => Box::new(sine()),
        Op::Saw => Box::new(saw()),
        Op::Pulse => Box::new(pulse()),
        Op::Noise => Box::new(noise()),
        Op::Impulse => Box::new(impulse::<U1>()),
        Op::InitRandom { stream } => {
            let unit = seed_unit(note.seed ^ stream);
            Box::new(map(move |input: &Frame<f32, U2>| {
                input[0] + unit * (input[1] - input[0])
            }))
        }

        Op::Lowpass => Box::new(lowpass()),
        Op::Highpass => Box::new(highpass()),
        Op::Bandpass => Box::new(bandpass()),
        Op::Moog => Box::new(moog()),

        Op::Shape { kind, amount } => {
            let a = *amount as f32;
            match kind {
                ShapeKind::Tanh => Box::new(shape(Tanh(a))),
                ShapeKind::Atan => Box::new(shape(Atan(a))),
                ShapeKind::Softsign => Box::new(shape(Softsign(a))),
                ShapeKind::Clip => Box::new(shape(Clip(a))),
                ShapeKind::Crush => Box::new(shape(Crush(a))),
            }
        }
        Op::DcBlock => Box::new(dcblock()),
        Op::Delay(range) => Box::new(tap(range.min_seconds() as f32, range.max_seconds() as f32)),

        Op::Add => Box::new(pass() + pass()),
        Op::Sub => Box::new(pass() - pass()),
        Op::Mul => Box::new(pass() * pass()),
        Op::Div => Box::new(map(|input: &Frame<f32, U2>| input[0] / input[1])),
        Op::Neg => Box::new(mul(-1.0)),

        // Both envelopes read the note clock, which starts at zero when the
        // sequencer starts the unit. They are pure functions of `t`, so an edit
        // costs them nothing: there is no phase to migrate, warm or crossfade.
        Op::Adsr(adsr) => {
            let adsr = *adsr;
            let gate = note.duration;
            Box::new(envelope(move |t: f32| adsr.at(t as f64, gate) as f32))
        }
        Op::Curve(curve) => {
            let curve = curve.clone();
            Box::new(envelope(move |t: f32| curve.at(t as f64) as f32))
        }

        Op::Pan => Box::new(panner()),
    }
}

fn seed_unit(mut seed: u64) -> f32 {
    seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    seed = (seed ^ (seed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    seed = (seed ^ (seed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    seed ^= seed >> 31;
    ((seed >> 40) as f32) / ((1u32 << 24) as f32)
}

/// Render a voice offline into interleaved samples.
///
/// The reason `crates/synth` can be tested without a sound card, and the same
/// path an offline render of a whole piece will take. `crates/pattern` earned
/// its 65 tests by being pure; this is how the graph layer earns its own.
pub fn render(unit: &mut dyn AudioUnit, sample_rate: f64, seconds: f64) -> Vec<Vec<f32>> {
    unit.set_sample_rate(sample_rate);
    unit.allocate();
    let channels = unit.outputs();
    let frames = (sample_rate * seconds) as usize;
    let mut out = vec![Vec::with_capacity(frames); channels];
    let mut frame = vec![0.0f32; channels];
    for _ in 0..frames {
        unit.tick(&[], &mut frame);
        for (c, x) in frame.iter().enumerate() {
            out[c].push(*x);
        }
    }
    out
}

/// Root-mean-square of a channel. Handy in tests for "did this make a sound".
pub fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|x| (*x as f64) * (*x as f64)).sum();
    (sum / samples.len() as f64).sqrt()
}

/// Dominant frequency of a channel, by zero-crossing count.
///
/// Crude on purpose — it needs no FFT and no dependency, and the question a
/// test asks is "is this a 440 Hz sine and not a 220 Hz one", which counting
/// answers well enough.
pub fn zero_crossing_hz(samples: &[f32], sample_rate: f64) -> f64 {
    let crossings = samples
        .windows(2)
        .filter(|w| (w[0] <= 0.0) != (w[1] <= 0.0))
        .count();
    crossings as f64 * sample_rate / (2.0 * samples.len() as f64)
}
