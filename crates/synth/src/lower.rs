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

use crate::note::Note;
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
) -> Result<Box<dyn AudioUnit>, TemplateError> {
    template.validate()?;

    let mut net = Net::new(0, template.channels());

    // Push every node first, so wiring is a second pass and node order in the
    // template need not be a topological order for the walk to work. (It still
    // must be one — `validate` rejects forward references, because a cycle
    // without a delay has no meaning and there is no delay primitive yet.)
    let mut nodes: Vec<FundspNode> = Vec::with_capacity(template.nodes.len());
    for node in &template.nodes {
        nodes.push(net.push(unit_for(&node.op, note)));
    }

    // Lifted scalars, keyed by bit pattern. A voice typically reuses `0`, `1`
    // and a handful of literals; sharing them keeps the instantiated net closer
    // in size to the template than to its edge count.
    let mut constants: HashMap<u64, FundspNode> = HashMap::new();

    for (index, node) in template.nodes.iter().enumerate() {
        for (port, input) in node.inputs.iter().enumerate() {
            let (from, channel) = resolve(
                input.source,
                template,
                note,
                &nodes,
                &mut net,
                &mut constants,
            );
            net.connect(from, channel, nodes[index], port);
        }
    }

    for (channel, source) in template.outputs.iter().enumerate() {
        let (from, from_channel) =
            resolve(*source, template, note, &nodes, &mut net, &mut constants);
        net.connect_output(from, from_channel, channel);
    }

    Ok(Box::new(net))
}

/// Turn a template source into a concrete `(node, channel)` pair, lifting
/// scalars and symbolic parameters to constants.
///
/// This is where "auto-lift scalars to `dc()`" happens, and it is why the
/// primitives are bound in their modulatable form — `lowpass()` with cutoff and
/// Q as inputs, never `lowpass_hz()`. One rule, and then any parameter of
/// anything accepts any signal, with a plain number as the degenerate case.
fn resolve(
    source: Source,
    template: &GraphTemplate,
    note: &Note,
    nodes: &[FundspNode],
    net: &mut Net,
    constants: &mut HashMap<u64, FundspNode>,
) -> (FundspNode, usize) {
    match source {
        Source::Port { node, channel } => (nodes[node], channel as usize),
        Source::Const(x) => (constant_node(x, net, constants), 0),
        Source::Param(id) => (constant_node(note.value(id, template), net, constants), 0),
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

        Op::Add => Box::new(pass() + pass()),
        Op::Sub => Box::new(pass() - pass()),
        Op::Mul => Box::new(pass() * pass()),
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
