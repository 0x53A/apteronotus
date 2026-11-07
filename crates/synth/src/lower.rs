//! Lowering a [`GraphTemplate`] onto fundsp.
//!
//! This is the only place a staged template is assembled into a fundsp `Net`
//! and the only place a `Box<dyn AudioUnit>` is allowed to appear. Small
//! realtime-safe custom `AudioUnit` implementations live in backend-private
//! sibling modules; everything in the public template/builder layer is data.
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
use crate::input::AudioInputLayout;
use crate::instrument::PatchTemplate;
use crate::note::{Note, ParamValue, ParamValueError};
use crate::routing::{BusLayout, EventRouting, RoutingError};
use crate::template::{
    GraphTemplate, InitExpr, InitScalar, Input, Lifetime, Node, Op, ShapeKind, Source,
    TemplateError,
};
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
        None,
        0.0,
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
        None,
        0.0,
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

/// Instantiate one persistent patch into the flattened main/bus layout.
///
/// Zero-input patches become autonomous persistent sources. A patch whose
/// explicit inputs are the full flattened layout can process routed stems.
/// The live host decides how those two forms compose; lowering only makes the
/// lane order and graph sends concrete.
pub fn instantiate_patch_routed(
    patch: &PatchTemplate,
    layout: &BusLayout,
    controls: &ControlStore,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    instantiate_patch_routed_at(patch, layout, controls, 0.0)
}

/// Instantiate a persistent patch at an absolute transport coordinate.
///
/// Stateful nodes still begin with empty history. Coordinate-derived nodes,
/// notably compiled transport sequences, begin at the phase they would have
/// had if the instance had existed since transport zero.
pub fn instantiate_patch_routed_at(
    patch: &PatchTemplate,
    layout: &BusLayout,
    controls: &ControlStore,
    transport_seconds: f64,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    instantiate_routed_impl(
        patch.graph(),
        &Note::new(440.0),
        layout,
        &EventRouting::new(),
        Some(controls),
        None,
        transport_seconds,
    )
}

/// Instantiate a persistent patch whose declared program audio inputs become
/// additional unit inputs after its explicit graph inputs.
///
/// The live host supplies those flattened lanes in declaration order. The
/// template remains device-neutral, and callers that do not choose this path
/// continue to receive the declared silence fallback.
pub fn instantiate_patch_routed_with_audio_inputs(
    patch: &PatchTemplate,
    layout: &BusLayout,
    controls: &ControlStore,
    audio_inputs: &AudioInputLayout,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    instantiate_patch_routed_with_audio_inputs_at(patch, layout, controls, audio_inputs, 0.0)
}

/// Instantiate an input-bearing persistent patch at an absolute transport
/// coordinate. See [`instantiate_patch_routed_at`].
pub fn instantiate_patch_routed_with_audio_inputs_at(
    patch: &PatchTemplate,
    layout: &BusLayout,
    controls: &ControlStore,
    audio_inputs: &AudioInputLayout,
    transport_seconds: f64,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    instantiate_routed_impl(
        patch.graph(),
        &Note::new(440.0),
        layout,
        &EventRouting::new(),
        Some(controls),
        Some(audio_inputs),
        transport_seconds,
    )
}

/// Prepare a persistent patch for an explicit host clock, off the audio thread.
/// This supplies the rate to node constructors, before the graph is published.
/// As with other transport constructors, stateful history starts empty; this
/// does not seek or pre-roll. Legacy entry points retain their 44.1 kHz default.
/// Fixed-clock asset admission can use this boundary; no such asset node is
/// currently connected to the production graph.
pub fn prepare_patch_routed(
    patch: &PatchTemplate,
    layout: &BusLayout,
    controls: &ControlStore,
    audio_inputs: Option<&AudioInputLayout>,
    transport_seconds: f64,
    sample_rate: f64,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return Err(LowerError::InvalidPreparationRate(sample_rate));
    }
    instantiate_routed_live_impl(
        patch.graph(), &Note::new(440.0), layout, &EventRouting::new(),
        Some(controls), audio_inputs, transport_seconds, None, sample_rate,
    )
}

/// Instantiate one autonomous patch for a finite transport span.
///
/// The lifecycle gate is inserted immediately after every nonterminating audio
/// source; pressure-driven flues instead have their wind input gated.
/// This is intentionally not an output gain: stateful processors
/// downstream receive silence when the span closes and continue rendering
/// until their declared response tails have drained.
pub fn instantiate_timed_patch_routed(
    patch: &PatchTemplate,
    active_seconds: f64,
    fade_seconds: f64,
    layout: &BusLayout,
    controls: &ControlStore,
) -> Result<(Box<dyn AudioUnit>, Lifetime), LowerError> {
    instantiate_timed_patch_routed_with_routing(
        patch,
        active_seconds,
        fade_seconds,
        layout,
        &EventRouting::new(),
        controls,
    )
}

/// Instantiate one finite patch while routing its completed outputs to
/// program buses. This is the persistent-patch counterpart of score-level
/// voice sends.
pub fn instantiate_timed_patch_routed_with_routing(
    patch: &PatchTemplate,
    active_seconds: f64,
    fade_seconds: f64,
    layout: &BusLayout,
    routing: &EventRouting,
    controls: &ControlStore,
) -> Result<(Box<dyn AudioUnit>, Lifetime), LowerError> {
    if !active_seconds.is_finite() || active_seconds <= 0.0 {
        return Err(LowerError::InvalidDuration(active_seconds));
    }
    let fade_seconds = fade_seconds.clamp(0.0, active_seconds * 0.5);
    let graph = lifecycle_gated_graph(patch.graph(), active_seconds, fade_seconds, false);
    let lifetime = graph.lifetime();
    let unit = instantiate_routed_impl(
        &graph,
        &Note::new(440.0).duration(active_seconds),
        layout,
        routing,
        Some(controls),
        None,
        0.0,
    )?;
    Ok((unit, lifetime))
}

fn lifecycle_gated_graph(
    template: &GraphTemplate,
    active_seconds: f64,
    fade_seconds: f64,
    protect_adsr: bool,
) -> GraphTemplate {
    let mut needs_gate = vec![!protect_adsr; template.nodes.len()];
    if protect_adsr {
        // A live ADSR is already exactly zero before the source safety fade
        // starts. If every audible path from a source crosses such a product,
        // a second source gate is redundant, including downstream DSP history.
        let mut hard_zero = vec![false; template.nodes.len()];
        for (index, node) in template.nodes.iter().enumerate() {
            hard_zero[index] = matches!(node.op, Op::Adsr(_)) ||
                (matches!(node.op, Op::Mul) && node.inputs.iter().any(|input|
                    matches!(input.source, Source::Port { node, .. } if hard_zero[node])));
        }
        for source in template.outputs.iter().copied().chain(
            template
                .sends
                .iter()
                .flat_map(|send| send.outputs.iter().map(|input| input.source)),
        ) {
            if let Source::Port { node, .. } = source {
                needs_gate[node] = true;
            }
        }
        for (index, node) in template.nodes.iter().enumerate().rev() {
            if needs_gate[index] && !hard_zero[index] {
                for input in &node.inputs {
                    if let Source::Port { node, .. } = input.source {
                        needs_gate[node] = true;
                    }
                }
            }
        }
    }
    let gate = Node {
        op: Op::RunGate {
            active_seconds,
            fade_seconds,
        },
        inputs: Vec::new(),
        tail: 0.0,
        src: None,
    };
    let mut nodes = vec![gate];
    let gate_source = Source::port(0, 0);
    let mut remapped = Vec::<Vec<Source>>::with_capacity(template.nodes.len());

    let rewrite = |source: Source, remapped: &[Vec<Source>]| match source {
        Source::Port { node, channel } => remapped
            .get(node)
            .and_then(|channels| channels.get(channel as usize))
            .copied()
            .expect("validated graph sources only refer to earlier node outputs"),
        other => other,
    };

    for (original_index, node) in template.nodes.iter().enumerate() {
        let mut inputs: Vec<Input> = node
            .inputs
            .iter()
            .map(|input| Input {
                source: rewrite(input.source, &remapped),
                src: input.src,
            })
            .collect();
        let driven_port = match node.op {
            Op::FluePipe { .. } => Some(1),
            Op::BowedString(_) => Some(2),
            _ => None,
        };
        if let Some(port) = driven_port {
            let drive_gate = nodes.len();
            nodes.push(Node {
                op: Op::Mul,
                inputs: vec![
                    inputs[port],
                    Input::new(if needs_gate[original_index] {
                        gate_source
                    } else {
                        Source::Const(1.0)
                    }),
                ],
                tail: 0.0,
                src: node.src,
            });
            inputs[port].source = Source::port(drive_gate, 0);
        }
        let node_id = nodes.len();
        nodes.push(Node {
            op: node.op.clone(),
            inputs,
            tail: node.tail,
            src: node.src,
        });
        let mut outputs = (0..node.op.outputs())
            .map(|channel| Source::port(node_id, channel as u32))
            .collect::<Vec<_>>();
        if node.op.begins_audio_activity() {
            for output in &mut outputs {
                let mul_id = nodes.len();
                nodes.push(Node {
                    op: Op::Mul,
                    inputs: vec![
                        Input::new(*output),
                        Input::new(if needs_gate[original_index] {
                            gate_source
                        } else {
                            Source::Const(1.0)
                        }),
                    ],
                    tail: 0.0,
                    src: node.src,
                });
                *output = Source::port(mul_id, 0);
            }
        }
        remapped.push(outputs);
    }

    let mut graph = template.clone();
    graph.nodes = nodes;
    graph.outputs = template
        .outputs
        .iter()
        .map(|source| rewrite(*source, &remapped))
        .collect();
    for (send, original) in graph.sends.iter_mut().zip(&template.sends) {
        for (input, original) in send.outputs.iter_mut().zip(&original.outputs) {
            input.source = rewrite(original.source, &remapped);
        }
    }
    graph
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
    instantiate_routed_impl(template, note, layout, event, None, None, 0.0)
}

pub fn instantiate_routed_with_controls(
    template: &GraphTemplate,
    note: &Note,
    layout: &BusLayout,
    event: &EventRouting,
    controls: &ControlStore,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    instantiate_routed_impl(template, note, layout, event, Some(controls), None, 0.0)
}

/// Instantiate a polyphonic held voice with an explicit runtime release gate.
/// Unknown note duration is never used to drive live ADSRs. The scheduler must
/// keep the unit until [`crate::live_voice_lifetime`] after the eventual release.
pub fn instantiate_live_routed_with_controls(
    template: &GraphTemplate,
    note: &Note,
    layout: &BusLayout,
    event: &EventRouting,
    controls: &ControlStore,
    gate: &crate::LiveGate,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    crate::validate_live_voice(template)?;
    for id in crate::template::Implicit::ALL
        .into_iter()
        .map(crate::template::ParamId::Implicit)
        .chain((0..template.params.len()).map(crate::template::ParamId::Declared))
    {
        if let ParamValue::Curve(curve) = note.value(id, template)?
            && curve.clock == crate::template::CurveClock::NotePhase
        {
            return Err(LowerError::LiveDurationRequired);
        }
    }
    let delay = crate::live::release_delay(template);
    // Reuse the finite-run source rewrite, substituting an atomic runtime gate
    // for its construction-time gate. The finite placeholder never renders.
    let mut graph = lifecycle_gated_graph(template, 1.0, 0.0, true);
    // Lifted constants/controls and curves can themselves be audio outputs.
    // Gate those direct routes too, while leaving ordinary ADSR/DSP paths alone.
    for source in &mut graph.outputs {
        if !matches!(source, Source::Port { .. }) {
            let node = graph.nodes.len();
            graph.nodes.push(Node {
                op: Op::Mul,
                inputs: vec![Input::new(*source), Input::new(Source::port(0, 0))],
                tail: 0.0,
                src: None,
            });
            *source = Source::port(node, 0);
        }
    }
    let voice = instantiate_routed_live_impl(
        &graph,
        note,
        layout,
        event,
        Some(controls),
        None,
        0.0,
        Some((gate, delay)),
        44_100.0,
    )?;
    // A late safety fade also covers biased arithmetic and raw control/curve
    // routes which can keep producing DC after their oscillators stop. Place
    // it after the declared response tail, so it cannot truncate an ADSR or
    // downstream resonator's normal release.
    let tail = template.lifetime_for(note)?.gate_tail;
    Ok(Box::new(crate::adsr::LiveOutputGate::new(
        voice,
        crate::adsr::LiveEnvelopeUnit::lifecycle(gate.clone(), delay + tail),
    )))
}

fn instantiate_routed_impl(
    template: &GraphTemplate,
    note: &Note,
    layout: &BusLayout,
    event: &EventRouting,
    controls: Option<&ControlStore>,
    audio_inputs: Option<&AudioInputLayout>,
    transport_seconds: f64,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    reject_prepared_voice(template)?;
    instantiate_routed_live_impl(
        template,
        note,
        layout,
        event,
        controls,
        audio_inputs,
        transport_seconds,
        None,
        44_100.0,
    )
}

#[allow(clippy::too_many_arguments)]
fn instantiate_routed_live_impl(
    template: &GraphTemplate,
    note: &Note,
    layout: &BusLayout,
    event: &EventRouting,
    controls: Option<&ControlStore>,
    audio_inputs: Option<&AudioInputLayout>,
    transport_seconds: f64,
    live: Option<(&crate::LiveGate, f64)>,
    sample_rate: f64,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    if live.is_some() { reject_prepared_voice(template)?; }
    if !transport_seconds.is_finite() {
        return Err(LowerError::InvalidTransportTime(transport_seconds));
    }
    template.validate()?;
    if template.channels() != layout.main_channels() && template.channels() != 1 {
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
    if template.channels() == 1 {
        routes.extend(
            layout
                .main_range()
                .map(|lane| (lane, template.outputs[0], 1.0)),
        );
    } else {
        routes.extend(
            template
                .outputs
                .iter()
                .enumerate()
                .map(|(lane, source)| (lane, *source, 1.0)),
        );
    }

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
        if template.channels() != range.len() && template.channels() != 1 {
            return Err(RoutingError::BusChannelMismatch {
                bus: send.bus,
                expected: range.len(),
                found: template.channels(),
            }
            .into());
        }
        if template.channels() == 1 {
            routes.extend(range.map(|lane| (lane, template.outputs[0], send.level)));
        } else {
            routes.extend(
                range
                    .zip(&template.outputs)
                    .map(|(lane, source)| (lane, *source, send.level)),
            );
        }
    }

    instantiate_to_lanes_live(
        template,
        note,
        controls,
        audio_inputs,
        transport_seconds,
        layout.total_channels(),
        routes,
        live,
        sample_rate,
    )
}

fn instantiate_to_lanes(
    template: &GraphTemplate,
    note: &Note,
    controls: Option<&ControlStore>,
    audio_inputs: Option<&AudioInputLayout>,
    transport_seconds: f64,
    output_channels: usize,
    routes: impl IntoIterator<Item = (usize, Source, f64)>,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    reject_prepared_voice(template)?;
    instantiate_to_lanes_live(
        template,
        note,
        controls,
        audio_inputs,
        transport_seconds,
        output_channels,
        routes,
        None,
        44_100.0,
    )
}

#[allow(clippy::too_many_arguments)]
fn instantiate_to_lanes_live(
    template: &GraphTemplate,
    note: &Note,
    controls: Option<&ControlStore>,
    audio_inputs: Option<&AudioInputLayout>,
    transport_seconds: f64,
    output_channels: usize,
    routes: impl IntoIterator<Item = (usize, Source, f64)>,
    live: Option<(&crate::LiveGate, f64)>,
    sample_rate: f64,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    let external_channels = audio_inputs.map_or(0, AudioInputLayout::total_channels);
    if live.is_some()
        && template.inputs == 0
        && external_channels == 0
        && !template
            .nodes
            .iter()
            .flat_map(|node| &node.inputs)
            .any(|input| matches!(input.source, Source::ExternalAudio { .. }))
    {
        return instantiate_compiled_live(
            template,
            note,
            controls,
            transport_seconds,
            output_channels,
            routes,
            live,
        );
    }
    let mut net = Net::new(template.inputs + external_channels, output_channels);

    net.set_sample_rate(sample_rate);

    // Push every node first, so wiring is a second pass and node order in the
    // template need not be a topological order for the walk to work. (It still
    // must be one — `validate` rejects forward references. A Delay node owns
    // history, but it does not make an arbitrary Net cycle well-defined;
    // feedback needs an explicit graph representation before it is legal.)
    let folded = init_constants(template, note)?;
    let mut constants = HashMap::new();
    let mut nodes: Vec<FundspNode> = Vec::with_capacity(template.nodes.len());
    for (node_index, node) in template.nodes.iter().enumerate() {
        if let Some(value) = folded[node_index] {
            nodes.push(constant_node(f64::from(value), &mut net, &mut constants));
            continue;
        }
        nodes.push(net.push(unit_for(
            &node.op,
            node_index,
            note,
            template,
            controls,
            transport_seconds,
            live,
            sample_rate,
        )?));
    }

    // Lifted scalars, keyed by bit pattern. A voice typically reuses `0`, `1`
    // and a handful of literals; sharing them keeps the instantiated net closer
    // in size to the template than to its edge count.
    let mut resolver = Resolver {
        template,
        note,
        controls,
        audio_inputs,
        nodes: &nodes,
        net: &mut net,
        constants,
        param_nodes: HashMap::new(),
        control_nodes: HashMap::new(),
        input_nodes: HashMap::new(),
    };

    for (index, node) in template.nodes.iter().enumerate() {
        if folded[index].is_some() {
            continue;
        }
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

#[allow(clippy::too_many_arguments)]
fn instantiate_compiled_live(
    template: &GraphTemplate,
    note: &Note,
    controls: Option<&ControlStore>,
    transport_seconds: f64,
    channels: usize,
    routes: impl IntoIterator<Item = (usize, Source, f64)>,
    live: Option<(&crate::LiveGate, f64)>,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    use crate::ParamId;
    use crate::compiled::{CompiledGraph, Value};
    let folded = init_constants(template, note)?;
    // Composition may leave staged nodes that are no longer connected to any
    // output. Execute only the dependency closure of audible routes. A folded
    // scalar already owns its value, so its former inputs need no audio slots.
    let routes: Vec<_> = routes.into_iter().collect();
    let mut reachable = vec![false; template.nodes.len()];
    let mut pending: Vec<_> = routes.iter().map(|(_, source, _)| *source).collect();
    while let Some(source) = pending.pop() {
        if let Source::Port { node, .. } = source
            && !reachable[node]
        {
            reachable[node] = true;
            if folded[node].is_none() {
                pending.extend(template.nodes[node].inputs.iter().map(|input| input.source));
            }
        }
    }
    let mut compiled = CompiledGraph::new(channels);
    let mut nodes: Vec<Vec<Value>> = Vec::with_capacity(template.nodes.len());
    let mut parameters = HashMap::new();
    let mut globals = HashMap::new();
    let mut bank_outputs = HashMap::new();
    let mut common: Vec<(Op, Vec<Value>, Vec<Value>)> = Vec::new();
    #[allow(clippy::too_many_arguments)]
    fn source(
        source: Source,
        graph: &mut CompiledGraph,
        nodes: &[Vec<Value>],
        parameters: &mut HashMap<ParamId, Value>,
        globals: &mut HashMap<ControlId, Value>,
        template: &GraphTemplate,
        note: &Note,
        controls: Option<&ControlStore>,
    ) -> Result<Value, LowerError> {
        Ok(match source {
            Source::Const(value) => Value::Constant(value as f32),
            Source::Port { node, channel } => nodes[node][channel as usize],
            Source::Param(id) => {
                if let Some(value) = parameters.get(&id) {
                    return Ok(*value);
                }
                let value = match note.value(id, template)? {
                    ParamValue::Number(value) => Value::Constant(value as f32),
                    ParamValue::Curve(curve) => {
                        let gate = note.duration;
                        graph.primitive(
                            Box::new(envelope(move |time: f32| {
                                curve
                                    .at(f64::from(time), gate)
                                    .expect("parameter curve validated before lowering")
                                    as f32
                            })),
                            Vec::new(),
                        )[0]
                    }
                };
                parameters.insert(id, value);
                value
            }
            Source::Control(id) => {
                if let Some(value) = globals.get(&id) {
                    return Ok(*value);
                }
                let controls = controls.ok_or(LowerError::ControlStoreRequired)?;
                let value = graph.shared(controls.shared(id)?.clone());
                globals.insert(id, value);
                value
            }
            Source::ExternalAudio { .. } => return Err(LowerError::UnknownAudioInput),
            Source::Input(_) => unreachable!("live graph inputs validated before lowering"),
        })
    }
    for (index, node) in template.nodes.iter().enumerate() {
        if !reachable[index] {
            nodes.push(Vec::new());
            continue;
        }
        if let Some(value) = folded[index] {
            nodes.push(vec![Value::Constant(value)]);
            continue;
        }
        let mut inputs = node
            .inputs
            .iter()
            .map(|input| {
                source(
                    input.source,
                    &mut compiled,
                    &nodes,
                    &mut parameters,
                    &mut globals,
                    template,
                    note,
                    controls,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let identity = match (&node.op, inputs.as_slice()) {
            (Op::Mul | Op::Div, [value, Value::Constant(1.0)]) => Some(*value),
            (Op::Mul, [Value::Constant(1.0), value]) => Some(*value),
            _ => None,
        };
        if let Some(value) = identity {
            nodes.push(vec![value]);
            continue;
        }
        if let Some(value) = bank_outputs.get(&index) {
            nodes.push(vec![*value]);
            continue;
        }
        if let (Op::Harmonics { .. }, [Value::Constant(frequency)]) = (&node.op, inputs.as_slice())
        {
            let mut bank_indices = Vec::new();
            let mut amplitudes = Vec::new();
            for (candidate, bank) in template.nodes.iter().enumerate().skip(index) {
                if !reachable[candidate] {
                    continue;
                }
                let Op::Harmonics {
                    amplitudes: weights,
                } = &bank.op
                else {
                    continue;
                };
                let scalar = match bank.inputs[0].source {
                    Source::Const(value) => Some(value as f32),
                    Source::Port { node, channel: 0 } => folded[node],
                    Source::Param(id) => match note.value(id, template)? {
                        ParamValue::Number(value) => Some(value as f32),
                        _ => None,
                    },
                    _ => None,
                };
                if scalar.map(f32::to_bits) == Some(frequency.to_bits()) {
                    bank_indices.push(candidate);
                    amplitudes.push(weights.clone());
                }
            }
            if bank_indices.len() > 1 {
                let outputs = compiled.primitive(
                    Box::new(crate::harmonic::HarmonicBanks::fixed(
                        &amplitudes,
                        *frequency,
                    )),
                    Vec::new(),
                );
                for (candidate, output) in bank_indices.iter().zip(&outputs) {
                    bank_outputs.insert(*candidate, *output);
                }
                nodes.push(vec![bank_outputs[&index]]);
                continue;
            }
        }
        // These operations have identical clocks and deterministic initial
        // state within a voice. Noise, seeded oscillators and all other state
        // remain independent. Live ADSRs do not use jittered envelope seeds.
        let reusable = matches!(
            node.op,
            Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::Pow
                | Op::Neg
                | Op::Clamp { .. }
                | Op::Slew { .. }
                | Op::Adsr(_)
                | Op::Harmonics { .. }
        );
        if reusable
            && let Some((_, _, outputs)) = common
                .iter()
                .find(|(op, sources, _)| op == &node.op && sources == &inputs)
        {
            nodes.push(outputs.clone());
            continue;
        }
        let common_inputs = reusable.then(|| inputs.clone());
        let outputs = if let (Some((gate, _)), Op::Adsr(adsr)) = (live, &node.op) {
            compiled.envelope(crate::adsr::LiveEnvelopeUnit::adsr(*adsr, gate.clone()))
        } else if let (Some((gate, delay)), Op::RunGate { .. }) = (live, &node.op) {
            compiled.envelope(crate::adsr::LiveEnvelopeUnit::lifecycle(
                gate.clone(),
                delay,
            ))
        } else if let Op::Slew { response_time, initial } = &node.op {
            let seconds = resolve_init_scalar(*response_time, note, template)?;
            if !seconds.is_finite() || seconds < 0.0 { return Err(LowerError::InvalidSlewTime(seconds)); }
            compiled.slew(crate::analyzer::SlewUnit::new(seconds, *initial), inputs[0])
        } else if matches!(
            node.op,
            Op::Lowpass | Op::Highpass | Op::Bandpass | Op::Peak
        ) && matches!(
            inputs.as_slice(),
            [_, Value::Constant(_), Value::Constant(_)]
        ) {
            let [audio, Value::Constant(hz), Value::Constant(q)] = inputs.as_slice() else {
                unreachable!()
            };
            let unit: Box<dyn AudioUnit> = match node.op {
                Op::Lowpass => Box::new(lowpass_hz(*hz, *q)),
                Op::Highpass => Box::new(highpass_hz(*hz, *q)),
                Op::Bandpass => Box::new(bandpass_hz(*hz, *q)),
                Op::Peak => Box::new(peak_hz(*hz, *q)),
                _ => unreachable!(),
            };
            compiled.primitive(unit, vec![*audio])
        } else if matches!(node.op, Op::Pan) && matches!(inputs.get(1), Some(Value::Constant(_))) {
            let Value::Constant(position) = inputs.pop().expect("panner input validated") else {
                unreachable!()
            };
            compiled.primitive(Box::new(pan(position)), inputs)
        } else if matches!(
            node.op,
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Pow | Op::Neg | Op::Clamp { .. }
        ) {
            compiled.math(&node.op, inputs)
        } else {
            let mut unit = unit_for(
                &node.op,
                index,
                note,
                template,
                controls,
                transport_seconds,
                live,
                44_100.0,
            )?;
            // Explicit per-node seeds remain independent of runtime addresses.
            unit.set_hash(node_seed(note.seed, index, 0));
            compiled.primitive(unit, inputs)
        };
        if let Some(inputs) = common_inputs {
            common.push((node.op.clone(), inputs, outputs.clone()));
        }
        nodes.push(outputs);
    }
    for (lane, route, gain) in routes {
        let value = source(
            route,
            &mut compiled,
            &nodes,
            &mut parameters,
            &mut globals,
            template,
            note,
            controls,
        )?;
        compiled.route(lane, value, gain as f32);
    }
    compiled.finish();
    Ok(Box::new(compiled))
}

// Fold only stateless scalar arithmetic over construction literals and bound
// scalar note values. Controls, inputs, curves and stateful DSP always render.
// Round every operation to f32 exactly as the audio graph does; preserve node
// indices so oscillator/noise seeds do not change with this optimization.
fn init_constants(template: &GraphTemplate, note: &Note) -> Result<Vec<Option<f32>>, LowerError> {
    let mut values = Vec::with_capacity(template.nodes.len());
    for node in &template.nodes {
        let scalar = |source: Source| -> Result<Option<f32>, LowerError> {
            Ok(match source {
                Source::Const(value) => Some(value as f32),
                Source::Param(id) => match note.value(id, template)? {
                    ParamValue::Number(value) => Some(value as f32),
                    ParamValue::Curve(_) => None,
                },
                Source::Port { node, channel: 0 } => values[node],
                _ => None,
            })
        };
        let a = node
            .inputs
            .first()
            .map(|input| scalar(input.source))
            .transpose()?
            .flatten();
        let b = node
            .inputs
            .get(1)
            .map(|input| scalar(input.source))
            .transpose()?
            .flatten();
        let value = match (&node.op, a, b) {
            (Op::Add, Some(a), Some(b)) => Some(a + b),
            (Op::Sub, Some(a), Some(b)) => Some(a - b),
            (Op::Mul, Some(a), Some(b)) => Some(a * b),
            (Op::Div, Some(a), Some(b)) => Some(a / b),
            (Op::Pow, Some(a), Some(b)) => Some(a.powf(b)),
            (Op::Neg, Some(a), _) => Some(-a),
            (Op::Clamp { min, max }, Some(a), _) => Some(a.clamp(*min as f32, *max as f32)),
            _ => None,
        };
        values.push(value);
    }
    Ok(values)
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
    PreparedPatchRequired,
    External(String),
    Template(TemplateError),
    Routing(RoutingError),
    Control(ControlError),
    ParamValue(ParamValueError),
    /// A graph-local send cannot be discarded by the main-only lowering path.
    LayoutRequired,
    ControlStoreRequired,
    InvalidDuration(f64),
    InvalidPluckParameter,
    InvalidSlewTime(f64),
    InvalidTransportTime(f64),
    InvalidPreparationRate(f64),
    NodePreparation { node: usize, reason: &'static str },
    InvalidBreakpointCurve,
    UnknownAudioInput,
    /// A live note cannot know its duration until the player releases it.
    LiveDurationRequired,
    LiveInputsUnsupported,
    InvalidLiveRelease(f64),
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

impl From<ParamValueError> for LowerError {
    fn from(error: ParamValueError) -> LowerError {
        LowerError::ParamValue(error)
    }
}

impl core::fmt::Display for LowerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LowerError::PreparedPatchRequired => write!(f, "external source requires explicit persistent patch preparation"),
            LowerError::External(error) => write!(f, "cannot prepare external source: {error}"),
            LowerError::Template(error) => error.fmt(f),
            LowerError::Routing(error) => error.fmt(f),
            LowerError::Control(error) => error.fmt(f),
            LowerError::ParamValue(error) => error.fmt(f),
            LowerError::LayoutRequired => {
                write!(f, "graph has sends and requires a program bus layout")
            }
            LowerError::ControlStoreRequired => {
                write!(
                    f,
                    "graph has writable controls and requires a control store"
                )
            }
            LowerError::InvalidDuration(seconds) => {
                write!(
                    f,
                    "note duration must be positive and finite, got {seconds}"
                )
            }
            LowerError::InvalidPluckParameter => {
                write!(
                    f,
                    "pluck pitch, decay gain, or damping is outside its safe range"
                )
            }
            LowerError::InvalidSlewTime(seconds) => {
                write!(
                    f,
                    "slew response time must be finite and non-negative, got {seconds}"
                )
            }
            LowerError::NodePreparation { node, reason } => write!(f, "cannot prepare graph node {node}: {reason}"),
            LowerError::InvalidPreparationRate(rate) => write!(f, "preparation sample rate must be finite and positive, got {rate}"),
            LowerError::InvalidTransportTime(seconds) => write!(
                f,
                "persistent transport start must be finite, got {seconds}"
            ),
            LowerError::InvalidBreakpointCurve => {
                write!(
                    f,
                    "breakpoint curve times are not finite and strictly increasing"
                )
            }
            LowerError::LiveInputsUnsupported => {
                write!(f, "live keyboard voices must have zero graph inputs")
            }
            LowerError::LiveDurationRequired => write!(
                f,
                "live voices cannot use n.duration or note-phase curves; use an ADSR or note-seconds curve"
            ),
            LowerError::InvalidLiveRelease(seconds) => write!(
                f,
                "live note release must be finite and nonnegative, got {seconds}"
            ),
            LowerError::UnknownAudioInput => {
                write!(f, "graph refers to an audio input outside its host layout")
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
    audio_inputs: Option<&'a AudioInputLayout>,
    nodes: &'a [FundspNode],
    net: &'a mut Net,
    constants: HashMap<u64, FundspNode>,
    param_nodes: HashMap<crate::template::ParamId, FundspNode>,
    control_nodes: HashMap<ControlId, FundspNode>,
    input_nodes: HashMap<usize, FundspNode>,
}

impl Resolver<'_> {
    fn resolve(&mut self, source: Source) -> Result<(FundspNode, usize), LowerError> {
        match source {
            Source::Port { node, channel } => Ok((self.nodes[node], channel as usize)),
            Source::Const(x) => Ok((constant_node(x, self.net, &mut self.constants), 0)),
            Source::Param(id) => {
                let node = match self.param_nodes.get(&id) {
                    Some(node) => *node,
                    None => {
                        let node = match self.note.value(id, self.template)? {
                            ParamValue::Number(value) => {
                                constant_node(value, self.net, &mut self.constants)
                            }
                            ParamValue::Curve(curve) => {
                                let gate = self.note.duration;
                                if !gate.is_finite() || gate <= 0.0 {
                                    return Err(LowerError::InvalidDuration(gate));
                                }
                                self.net.push(Box::new(envelope(move |t: f32| {
                                    curve
                                        .at(t as f64, gate)
                                        .expect("parameter curve validated before lowering")
                                        as f32
                                })))
                            }
                        };
                        self.param_nodes.insert(id, node);
                        node
                    }
                };
                Ok((node, 0))
            }
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
            Source::ExternalAudio { input, channel } => {
                let Some(audio_inputs) = self.audio_inputs else {
                    // Device identity is host policy. Without an explicit
                    // binding, the owned declaration's fallback is silence.
                    return Ok((constant_node(0.0, self.net, &mut self.constants), 0));
                };
                let flattened = audio_inputs
                    .channel_index(input, channel)
                    .ok_or(LowerError::UnknownAudioInput)?;
                let channel = self.template.inputs + flattened;
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
    externals: std::sync::Mutex<Vec<(String, std::sync::Arc<dyn crate::external::ExternalMonitor>)>>,
    rooms: std::sync::Mutex<Vec<crate::spatial_host::RoomHandle>>,
    layout: ControlLayout,
    values: Vec<Shared>,
    bowed: std::sync::Mutex<HashMap<ControlId,crate::bowed_host::BowedHandle>>,
}

impl ControlStore {
    pub fn new(layout: &ControlLayout) -> ControlStore {
        ControlStore {
            externals: std::sync::Mutex::new(Vec::new()),
            rooms: std::sync::Mutex::new(Vec::new()),
            layout: layout.clone(),
            bowed: std::sync::Mutex::new(HashMap::new()),
            values: layout
                .specs()
                .iter()
                .map(|spec| shared(spec.default as f32))
                .collect(),
        }
    }

    /// Sample the first failed external unit on the host/control thread.
    /// This lock is never taken by DSP; healthy/completed instances return None.
    pub fn external_fault(&self) -> Option<crate::external::ExternalFault> {
        self.externals.lock().expect("control-thread external registry").iter()
            .enumerate().find_map(|(instance,(kind,monitor))| monitor.fault()
                .map(|reason| crate::external::ExternalFault{instance,kind:kind.clone(),reason}))
    }

    /// Number of monitored external units registered by preparation.
    pub fn external_monitor_count(&self) -> usize {
        self.externals.lock().expect("control-thread external registry").len()
    }

    /// Reserve all changed room instances before publishing any of them.
    pub fn prepare_room_edits(
        &self,
        changes: &[(crate::spatial::RoomSpec, crate::spatial::RoomSpec)],
    ) -> Result<Vec<crate::spatial_host::RoomEdit>, &'static str> {
        let rooms = self.rooms.lock().expect("control-thread room registry");
        let mut used = vec![false; rooms.len()];
        let mut edits = Vec::new();
        for (old, new) in changes {
            let index = rooms
                .iter()
                .enumerate()
                .position(|(i, h)| !used[i] && h.spec() == *old)
                .ok_or("active room instance missing")?;
            used[index] = true;
            edits.push(rooms[index].prepare(new.clone())?);
        }
        Ok(edits)
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

    /// Arena-scoped gesture port installed when its retained instrument lowers.
    pub fn bowed_handle(&self,id:ControlId)->Option<crate::bowed_host::BowedHandle> {
        self.bowed.lock().ok()?.get(&id).cloned()
    }
    fn register_bowed(&self,id:ControlId,live:bool)->Result<crate::bowed_host::BowedHandle,ControlError> {
        self.layout.spec(id).ok_or(ControlError::UnknownControl(id))?;
        let handle=crate::bowed_host::BowedHandle::new(live);
        self.bowed.lock().expect("control-thread bowed registry").insert(id,handle.clone());
        Ok(handle)
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

fn reject_prepared_voice(template: &GraphTemplate) -> Result<(),LowerError> {
    if template.nodes.iter().any(|node|matches!(node.op,Op::External(_))) {
        Err(LowerError::PreparedPatchRequired)
    } else { Ok(()) }
}

fn unit_for(
    op: &Op,
    node_index: usize,
    note: &Note,
    template: &GraphTemplate,
    controls: Option<&ControlStore>,
    transport_seconds: f64,
    live: Option<(&crate::LiveGate, f64)>,
    sample_rate: f64,
) -> Result<Box<dyn AudioUnit>, LowerError> {
    if let Some((gate, delay)) = live {
        match op {
            Op::Adsr(adsr) => {
                return Ok(Box::new(crate::adsr::LiveEnvelopeUnit::adsr(
                    *adsr,
                    gate.clone(),
                )));
            }
            Op::RunGate { .. } => {
                return Ok(Box::new(crate::adsr::LiveEnvelopeUnit::lifecycle(
                    gate.clone(),
                    delay,
                )));
            }
            _ => {}
        }
    }
    let unit: Box<dyn AudioUnit> = match op {
        Op::Sine => Box::new(sine()),
        Op::FluePipe { min_hz } => Box::new(crate::flue::FlueUnit::new(*min_hz)),
        Op::Harmonics { amplitudes } => Box::new(crate::harmonic::HarmonicUnit::new(amplitudes)),
        Op::Cosine => Box::new(sine().phase(0.25)),
        Op::Saw => Box::new(saw()),
        Op::Triangle => Box::new(triangle()),
        Op::Pulse => Box::new(pulse()),
        Op::Noise => {
            let mut unit = noise();
            unit.set(Setting::seed(node_seed(note.seed, node_index, 0)));
            Box::new(unit)
        }
        Op::Pink => {
            let mut unit = pink();
            unit.set(Setting::seed(node_seed(note.seed, node_index, 1)).left());
            Box::new(unit)
        }
        Op::Impulse => Box::new(impulse::<U1>()),
        Op::InitRandom { stream, min, max } => {
            let unit = seed_unit(note.seed ^ stream);
            let min = resolve_init_expr(min, note, template)?;
            let max = resolve_init_expr(max, note, template)?;
            if !min.is_finite() || !max.is_finite() || min > max {
                return Err(LowerError::InvalidBreakpointCurve);
            }
            Box::new(dc((min + f64::from(unit) * (max - min)) as f32))
        }
        Op::Pluck {
            frequency,
            gain_per_second,
            damping,
            max_delay_seconds,
        } => {
            let frequency = resolve_init_scalar(*frequency, note, template)?;
            let damping = resolve_init_scalar(*damping, note, template)?;
            if !frequency.is_finite()
                || frequency < 1.0 / max_delay_seconds
                || !gain_per_second.is_finite()
                || *gain_per_second <= 0.0
                || *gain_per_second >= 1.0
                || !damping.is_finite()
                || !(0.0..=1.0).contains(&damping)
            {
                return Err(LowerError::InvalidPluckParameter);
            }
            Box::new(pluck(
                frequency as f32,
                *gain_per_second as f32,
                damping as f32,
            ))
        }

        Op::Lowpass => Box::new(lowpass()),
        Op::Highpass => Box::new(highpass()),
        Op::Bandpass => Box::new(bandpass()),
        Op::Peak => Box::new(peak()),
        Op::Moog => Box::new(moog()),

        Op::Shape { kind } => match kind {
            ShapeKind::Tanh => Box::new(map(|input: &Frame<f32, U2>| (input[0] * input[1]).tanh())),
            ShapeKind::Atan => Box::new(map(|input: &Frame<f32, U2>| {
                (input[0] * (input[1] * core::f32::consts::PI * 0.5)).atan()
                    * (2.0 / core::f32::consts::PI)
            })),
            ShapeKind::Softsign => Box::new(map(|input: &Frame<f32, U2>| {
                let x = input[0] * input[1];
                x / (1.0 + x.abs())
            })),
            ShapeKind::Clip => Box::new(map(|input: &Frame<f32, U2>| {
                (input[0] * input[1]).clamp(-1.0, 1.0)
            })),
            ShapeKind::Crush => Box::new(map(|input: &Frame<f32, U2>| {
                let levels = input[1].abs().max(f32::EPSILON);
                (input[0] * levels).round() / levels
            })),
        },
        Op::DcBlock => Box::new(dcblock()),
        Op::ConcertHall => Box::new(crate::concert_hall::HallUnit::new(sample_rate)
            .map_err(|reason| LowerError::NodePreparation { node: node_index, reason })?),
        Op::SpatialRoom(spec) => {
            let unit=crate::spatial_host::ReplaceableRoom::new(spec.clone(),sample_rate).map_err(|reason| LowerError::NodePreparation { node: node_index, reason })?;
            if let Some(controls)=controls {controls.rooms.lock().expect("control-thread room registry").push(unit.handle());}
            Box::new(unit)
        },
        Op::Convolve(response) => Box::new(crate::convolution::ConvolutionUnit::new(response.clone(),sample_rate).map_err(|reason| LowerError::NodePreparation { node: node_index, reason })?),
        Op::StringResonator { min_hz, decay } => {
            Box::new(crate::string::StringUnit::new(*min_hz, *decay))
        }
        Op::BowedString(config) => Box::new(
            crate::bowed::BowedString::new(*config, sample_rate)
                .map_err(|reason| LowerError::NodePreparation { node: node_index, reason })?,
        ),
        Op::External(spec) => {
            let controls=controls.ok_or(LowerError::ControlStoreRequired)?;
            let source=spec.source();
            let prepared=source.prepare(sample_rate,transport_seconds).map_err(LowerError::External)?;
            if prepared.unit.inputs()!=0 || prepared.unit.outputs()!=source.outputs() {
                return Err(LowerError::External(format!("{} prepared a unit with the wrong channel layout",source.kind())));
            }
            if let Some(monitor)=prepared.monitor {
                controls.externals.lock().expect("control-thread external registry").push((source.kind().to_string(),monitor));
            }
            prepared.unit
        }
        Op::BowedPerformance {key,score} => {
            let controls=controls.ok_or(LowerError::ControlStoreRequired)?;
            let handle=controls.register_bowed(*key,score.live)?;
            Box::new(crate::bowed_host::BowedInstrumentUnit::new(score.clone(),handle,transport_seconds)
                .expect("validated bowed performance"))
        }
        Op::Delay(range) => Box::new(tap(range.min_seconds() as f32, range.max_seconds() as f32)),
        Op::Reverb {
            room_size,
            time,
            damping,
        } => Box::new(reverb_stereo(
            *room_size as f32,
            *time as f32,
            *damping as f32,
        )),
        Op::Limiter { attack, release } => {
            Box::new(limiter_stereo(*attack as f32, *release as f32))
        }
        Op::Chorus {
            seed,
            separation,
            variation,
            frequency,
        } => Box::new(chorus(
            *seed,
            *separation as f32,
            *variation as f32,
            *frequency as f32,
        )),
        Op::FeedbackDelay {
            delay_seconds,
            cutoff_q,
            amount,
        } => match cutoff_q {
            Some((cutoff, q)) => Box::new(
                pass()
                    & feedback(
                        delay(*delay_seconds as f32)
                            >> (lowpass_hz(*cutoff as f32, *q as f32) * *amount as f32),
                    ),
            ),
            None => Box::new(pass() & feedback(delay(*delay_seconds as f32) * *amount as f32)),
        },
        Op::AllpassDelay { seconds, gain } => {
            Box::new(allnest_c(*gain as f32, delay(*seconds as f32)))
        }
        Op::Fdn {
            delays,
            damping,
            modulation_rate,
            modulation_depth,
            ..
        } => Box::new(crate::fdn::FdnUnit::new(
            delays.clone(),
            *damping,
            *modulation_rate,
            *modulation_depth,
        )),
        Op::EnvelopeFollower { attack, release } => Box::new(
            map(|input: &Frame<f32, U1>| input[0].abs().min(1.0))
                >> afollow(*attack as f32, *release as f32),
        ),
        Op::PitchTracker {
            min_hz,
            max_hz,
            default_hz,
            hold_seconds,
        } => Box::new(crate::analyzer::PitchTrackerUnit::new(
            *min_hz,
            *max_hz,
            *default_hz,
            *hold_seconds,
        )),
        Op::OnsetDetector {
            floor,
            hold_seconds,
        } => Box::new(crate::analyzer::OnsetDetectorUnit::new(
            *floor,
            *hold_seconds,
        )),
        Op::TransportSequence {
            period_seconds,
            slots,
        } => Box::new(crate::analyzer::TransportSequenceUnit::new(
            *period_seconds,
            slots.clone(),
            transport_seconds,
        )),
        Op::Width => Box::new(crate::analyzer::WidthUnit::new()),
        Op::Slew {
            response_time,
            initial,
        } => {
            let seconds = resolve_init_scalar(*response_time, note, template)?;
            if !seconds.is_finite() || seconds < 0.0 {
                return Err(LowerError::InvalidSlewTime(seconds));
            }
            Box::new(crate::analyzer::SlewUnit::new(seconds, *initial))
        }
        Op::GateEnv {
            attack,
            decay,
            sustain,
            release,
        } => {
            let (attack, decay, sustain, release) = (
                *attack as f32,
                *decay as f32,
                *sustain as f32,
                *release as f32,
            );
            let mut high = false;
            let mut onset = 0.0_f32;
            let mut release_start = None;
            let mut release_level = 0.0_f32;
            let held = move |elapsed: f32| {
                if elapsed < attack {
                    if attack == 0.0 { 1.0 } else { elapsed / attack }
                } else if elapsed < attack + decay {
                    if decay == 0.0 {
                        sustain
                    } else {
                        1.0 - (1.0 - sustain) * ((elapsed - attack) / decay)
                    }
                } else {
                    sustain
                }
            };
            Box::new(envelope2(move |time, gate| {
                let next_high = gate > 0.0;
                if next_high && !high {
                    onset = time;
                    release_start = None;
                } else if !next_high && high {
                    release_level = held((time - onset).max(0.0));
                    release_start = Some(time);
                }
                high = next_high;
                if let Some(start) = release_start {
                    if release == 0.0 {
                        0.0
                    } else {
                        release_level * (1.0 - (time - start) / release).clamp(0.0, 1.0)
                    }
                } else if high {
                    held((time - onset).max(0.0))
                } else {
                    0.0
                }
            }))
        }
        Op::Portamento {
            target,
            response_time,
        } => {
            let target = resolve_init_scalar(*target, note, template)?;
            let seconds = resolve_init_scalar(*response_time, note, template)?;
            if !target.is_finite() || !seconds.is_finite() || seconds < 0.0 {
                return Err(LowerError::InvalidSlewTime(seconds));
            }
            let initial = note.previous_hz.unwrap_or(target);
            Box::new(envelope(move |time: f32| {
                if seconds == 0.0 {
                    return target as f32;
                }
                let phase = (f64::from(time) / seconds).clamp(0.0, 1.0);
                let smooth = phase * phase * (3.0 - 2.0 * phase);
                (initial + (target - initial) * smooth) as f32
            }))
        }
        Op::RunGate {
            active_seconds,
            fade_seconds,
        } => {
            let active = *active_seconds as f32;
            let fade = *fade_seconds as f32;
            Box::new(envelope(move |time: f32| {
                if time < 0.0 || time >= active {
                    return 0.0;
                }
                if fade == 0.0 {
                    return 1.0;
                }
                let phase = if time < fade {
                    time / fade
                } else if time > active - fade {
                    (active - time) / fade
                } else {
                    return 1.0;
                }
                .clamp(0.0, 1.0);
                phase * phase * phase * (phase * (phase * 6.0 - 15.0) + 10.0)
            }))
        }
        Op::ControlWrite { target } => {
            let controls = controls.ok_or(LowerError::ControlStoreRequired)?;
            Box::new(monitor(controls.shared(*target)?, Meter::Sample))
        }

        Op::Add => Box::new(pass() + pass()),
        Op::Sub => Box::new(pass() - pass()),
        Op::Mul => Box::new(pass() * pass()),
        Op::Div => Box::new(map(|input: &Frame<f32, U2>| input[0] / input[1])),
        Op::Pow => Box::new(map(|input: &Frame<f32, U2>| input[0].powf(input[1]))),
        Op::Neg => Box::new(mul(-1.0)),
        Op::HzToMidi => Box::new(map(|input: &Frame<f32, U1>| {
            69.0 + 12.0 * (input[0].max(f32::MIN_POSITIVE) / 440.0).log2()
        })),
        Op::Clamp { min, max } => {
            let (min, max) = (*min as f32, *max as f32);
            Box::new(map(move |input: &Frame<f32, U1>| input[0].clamp(min, max)))
        }

        // Both envelopes read the note clock, which starts at zero when the
        // sequencer starts the unit. They are pure functions of `t`, so an edit
        // costs them nothing: there is no phase to migrate, warm or crossfade.
        Op::Adsr(adsr) => Box::new(crate::adsr::AdsrUnit::new(*adsr, note.duration)),
        Op::Decay { .. } => Box::new(envelope2(|t, seconds| {
            if seconds > 0.0 {
                (-3.0 * t / seconds).exp()
            } else {
                0.0
            }
        })),
        Op::Window { .. } => Box::new(envelope3(
            |t, begin, end| {
                if t >= begin && t < end { 1.0 } else { 0.0 }
            },
        )),
        Op::Curve(curve) => {
            let curve = curve.clone();
            let gate = note.duration;
            if !gate.is_finite() || gate <= 0.0 {
                return Err(LowerError::InvalidDuration(gate));
            }
            Box::new(envelope(move |t: f32| {
                curve
                    .at(t as f64, gate)
                    .expect("curve and duration validated before lowering") as f32
            }))
        }
        Op::BreakpointCurve { times, values, .. } => {
            let times = times
                .iter()
                .map(|time| resolve_init_expr(time, note, template))
                .collect::<Result<Vec<_>, _>>()?;
            if times.iter().any(|time| !time.is_finite() || *time < 0.0)
                || times.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(LowerError::InvalidBreakpointCurve);
            }
            let values = values.clone();
            Box::new(envelope(move |time: f32| {
                breakpoint_value(time as f64, &times, &values) as f32
            }))
        }

        Op::Pan => Box::new(panner()),
    };
    Ok(unit)
}

fn resolve_init_scalar(
    value: InitScalar,
    note: &Note,
    template: &GraphTemplate,
) -> Result<f64, LowerError> {
    match value {
        InitScalar::Const(value) => Ok(value),
        InitScalar::Param(id) => match note.value(id, template)? {
            ParamValue::Number(value) => Ok(value),
            ParamValue::Curve(_) => Err(LowerError::InvalidPluckParameter),
        },
    }
}

fn resolve_init_expr(
    expression: &InitExpr,
    note: &Note,
    template: &GraphTemplate,
) -> Result<f64, LowerError> {
    let value = match expression {
        InitExpr::Const(value) => *value,
        InitExpr::Param(id) => match note.value(*id, template)? {
            ParamValue::Number(value) => value,
            ParamValue::Curve(_) => return Err(LowerError::InvalidBreakpointCurve),
        },
        InitExpr::Add(left, right) => {
            resolve_init_expr(left, note, template)? + resolve_init_expr(right, note, template)?
        }
        InitExpr::Sub(left, right) => {
            resolve_init_expr(left, note, template)? - resolve_init_expr(right, note, template)?
        }
        InitExpr::Mul(left, right) => {
            resolve_init_expr(left, note, template)? * resolve_init_expr(right, note, template)?
        }
        InitExpr::Div(left, right) => {
            resolve_init_expr(left, note, template)? / resolve_init_expr(right, note, template)?
        }
        InitExpr::Neg(inner) => -resolve_init_expr(inner, note, template)?,
    };
    value
        .is_finite()
        .then_some(value)
        .ok_or(LowerError::InvalidBreakpointCurve)
}

fn breakpoint_value(time: f64, times: &[f64], values: &[f64]) -> f64 {
    if time <= times[0] {
        return values[0];
    }
    for index in 1..times.len() {
        if time < times[index] {
            let phase = (time - times[index - 1]) / (times[index] - times[index - 1]);
            return values[index - 1] + (values[index] - values[index - 1]) * phase;
        }
    }
    *values
        .last()
        .expect("validated breakpoint curve has values")
}

fn node_seed(event_seed: u64, node_index: usize, stream: u64) -> u64 {
    seed_hash(
        event_seed
            ^ (node_index as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)
            ^ stream.wrapping_mul(0xA076_1D64_78BD_642F),
    )
}

fn seed_hash(mut seed: u64) -> u64 {
    seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    seed = (seed ^ (seed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    seed = (seed ^ (seed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    seed ^= seed >> 31;
    seed
}

fn seed_unit(seed: u64) -> f32 {
    let seed = seed_hash(seed);
    ((seed >> 40) as f32) / ((1u32 << 24) as f32)
}

/// Render a zero-input voice offline into planar (one vector per channel) samples.
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
    let input = BufferVec::new(0);
    let mut output = BufferVec::new(channels);
    // Traverse the graph once per native DSP block, letting nodes use their
    // vectorized process paths. The last block must not advance beyond the
    // requested frame count: callers may continue rendering the same unit.
    for offset in (0..frames).step_by(fundsp::MAX_BUFFER_SIZE) {
        let size = std::cmp::min(frames - offset, fundsp::MAX_BUFFER_SIZE);
        unit.process(size, &input.buffer_ref(), &mut output.buffer_mut());
        for (channel, destination) in out.iter_mut().enumerate() {
            destination.extend_from_slice(&output.buffer_ref().channel_f32(channel)[..size]);
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

#[cfg(test)]
mod live_compiler_tests {
    use super::*;
    use crate::{Adsr, ControlSpec, GraphBuilder, LiveGate};

    #[test]
    fn compiled_live_graph_matches_net_with_controls_filter_history_and_release() {
        let mut layout = ControlLayout::new();
        let gain = layout.add(ControlSpec::new("gain", 0.0, 1.0, 0.7)).unwrap();
        let controls = ControlStore::new(&layout);
        let mut builder = GraphBuilder::new();
        let first = builder.harmonics(220.0, vec![0.5, 0.3]);
        let second = builder.harmonics(220.0, vec![0.0, 0.0, 0.1, 0.05]);
        let noise = builder.noise();
        let air = builder.bandpass(noise, 1100.0, 0.7);
        let air = builder.mul(air, 0.01);
        let combined = builder.add(first, second);
        let combined = builder.add(combined, air);
        let adsr = builder.adsr(Adsr::new(0.01, 0.02, 0.72, 0.04));
        let voiced = builder.mul(combined, adsr);
        let level = builder.slew(builder.control(gain), 0.025).unwrap();
        let voiced = builder.mul(voiced, level);
        let (left, right) = builder.pan(voiced, -0.4);
        let template = builder.out(&[left, right]).unwrap();
        let gate = LiveGate::new();
        let delay = crate::live::release_delay(&template);
        let optimized = lifecycle_gated_graph(&template, 1.0, 0.0, true);
        let mut reference = lifecycle_gated_graph(&template, 1.0, 0.0, false);
        // An unused host input selects the legacy Net lowering as the reference.
        reference.inputs = 1;
        let note = Note::new(220.0);
        let mut optimized = instantiate_to_lanes_live(
            &optimized,
            &note,
            Some(&controls),
            None,
            0.0,
            2,
            optimized
                .outputs
                .iter()
                .copied()
                .enumerate()
                .map(|(lane, source)| (lane, source, 1.0)),
            Some((&gate, delay)),
            44_100.0,
        )
        .unwrap();
        let mut reference = instantiate_to_lanes_live(
            &reference,
            &note,
            Some(&controls),
            None,
            0.0,
            2,
            reference
                .outputs
                .iter()
                .copied()
                .enumerate()
                .map(|(lane, source)| (lane, source, 1.0)),
            Some((&gate, delay)),
            44_100.0,
        )
        .unwrap();
        for unit in [&mut optimized, &mut reference] {
            unit.set_sample_rate(8000.0);
            unit.allocate();
        }
        let empty = BufferVec::new(0);
        let mut host = BufferVec::new(1);
        host.buffer_mut().channel_f32_mut(0).fill(0.0);
        let mut actual = BufferVec::new(2);
        let mut expected = BufferVec::new(2);
        for block in 0..120 {
            if block == 20 {
                controls.set(gain, 0.2).unwrap();
            }
            if block == 40 {
                gate.release_after(0.15).unwrap();
            }
            let size = [1, 7, 17, 64][block % 4];
            optimized.process(size, &empty.buffer_ref(), &mut actual.buffer_mut());
            reference.process(size, &host.buffer_ref(), &mut expected.buffer_mut());
            for channel in 0..2 {
                for frame in 0..size {
                    assert!(
                        (actual.buffer_ref().at_f32(channel, frame)
                            - expected.buffer_ref().at_f32(channel, frame))
                        .abs()
                            < 1e-6
                    );
                }
            }
        }
    }
}
