//! What the graph layer owes, tested without a sound card.
//!
//! `crates/pattern` is testable offline because a query is pure. This crate is
//! testable offline because a template is data and instantiation is
//! deterministic — render into a buffer and ask the samples. Anything that can
//! only be checked by listening is a bug in the seam, not a fact of audio.

use apteronotus_synth::lower::{render, rms, zero_crossing_hz};
use apteronotus_synth::{
    Adsr, Basis, BusLayout, ControlLayout, ControlStore, Curve, CurveClock, DelayRange,
    DelayRangeError, FdnConfig, GraphBuilder, GraphLimitError, GraphLimits, GraphTemplate,
    InitScalarError, Input, Note, ONSET_PULSE_SECONDS, Op, ParamSpec, PatchTemplate, ShapeKind,
    Source, TemplateError, TransportSlot, instantiate, instantiate_timed_patch_routed, n,
};

const SR: f64 = 48_000.0;

#[test]
fn retained_string_validates_bounds_and_declares_memory_and_response() {
    let mut g = GraphBuilder::with_inputs(3);
    let string = g.string_resonator(
        Source::Input(0),
        Source::Input(1),
        Source::Input(2),
        40.0,
        20.0,
    );
    let graph = g.out_mono(string).unwrap();
    assert_eq!(graph.cost().delay_buffer_seconds, 0.025);
    assert!((graph.tail() - 20.025).abs() < 1e-9);
    assert!(PatchTemplate::new(graph.clone()).is_ok());
    assert!(graph.nodes[0].op.activity_input(0));
    assert!(!graph.nodes[0].op.activity_input(1));
    assert!(!graph.nodes[0].op.activity_input(2));
    for (min_hz, decay) in [(0.0, 20.0), (40.0, 0.0), (40.0, 121.0), (f64::NAN, 20.0)] {
        let mut invalid = graph.clone();
        invalid.nodes[0].op = Op::StringResonator { min_hz, decay };
        assert!(invalid.validate().is_err());
    }
}

/// A bare oscillator at the symbolic note frequency.
fn tone() -> GraphTemplate {
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    g.out_mono(osc).unwrap()
}

fn play(template: &GraphTemplate, note: &Note, seconds: f64) -> Vec<Vec<f32>> {
    let mut unit = instantiate(template, note).unwrap();
    render(unit.as_mut(), SR, seconds)
}

// ------------------------------------------------------------ the symbolic n

#[test]
fn triangle_has_odd_harmonics_and_rejects_out_of_band_partials() {
    let mut g = GraphBuilder::new();
    let signal = g.triangle(n::HZ);
    let voice = g.out_mono(signal).unwrap();
    assert_eq!(voice.nodes.len(), 1);
    assert_eq!(voice.lifetime().absolute_horizon, 0.0);
    let amplitude = |samples: &[f32], hz: f64| {
        let (re, im) = samples
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(re, im), (i, x)| {
                let phase = std::f64::consts::TAU * hz * i as f64 / SR;
                (
                    re + f64::from(*x) * phase.cos(),
                    im + f64::from(*x) * phase.sin(),
                )
            });
        2.0 * re.hypot(im) / samples.len() as f64
    };
    let note = Note::new(250.0);
    let audio = play(&voice, &note, 0.4);
    assert_eq!(audio, play(&voice, &note, 0.4));
    let fundamental = amplitude(&audio[0], 250.0);
    assert!(fundamental > 0.5);
    assert!((amplitude(&audio[0], 750.0) / fundamental - 1.0 / 9.0).abs() < 0.01);
    assert!(amplitude(&audio[0], 500.0) < fundamental * 0.001);

    // A naive triangle's third harmonic at 30 kHz folds to 18 kHz here.
    let high = play(&voice, &Note::new(10_000.0), 0.4);
    assert!(amplitude(&high[0], 10_000.0) > 0.5);
    assert!(amplitude(&high[0], 18_000.0) < 0.01);
}

#[test]
fn a_symbolic_note_input_becomes_the_notes_number() {
    let voice = tone();
    for hz in [110.0, 440.0, 1_000.0] {
        let audio = play(&voice, &Note::new(hz), 0.5);
        let measured = zero_crossing_hz(&audio[0], SR);
        assert!(
            (measured - hz).abs() < 2.0,
            "asked for {hz} Hz, measured {measured} Hz"
        );
    }
}

#[test]
fn one_template_instantiates_many_voices() {
    // The point of staging: the template is built once and never mutated, and
    // two simultaneous notes are two independent graphs rather than one graph
    // with a knob. A monosynth would fail this.
    let voice = tone();
    let low = play(&voice, &Note::new(220.0), 0.5);
    let high = play(&voice, &Note::new(880.0), 0.5);
    assert!((zero_crossing_hz(&low[0], SR) - 220.0).abs() < 2.0);
    assert!((zero_crossing_hz(&high[0], SR) - 880.0).abs() < 2.0);
    // ...and the template survived both unchanged.
    assert_eq!(voice, tone());
}

#[test]
fn per_voice_noise_derives_from_event_provenance() {
    let mut graph = GraphBuilder::new();
    let noise = graph.noise();
    let voice = graph.out_mono(noise).unwrap();

    let first = play(&voice, &Note::new(440.0).seed(0x1234), 0.01);
    let repeated = play(&voice, &Note::new(440.0).seed(0x1234), 0.01);
    let other = play(&voice, &Note::new(440.0).seed(0x5678), 0.01);

    assert_eq!(
        first, repeated,
        "one event provenance must reproduce sample for sample"
    );
    assert_ne!(
        first, other,
        "different events must not restart the same noise waveform"
    );
}

#[test]
fn arithmetic_on_a_symbolic_input_stages_as_nodes() {
    // `n.hz * 2` cannot be computed at build time — there is no number yet —
    // so it must have become a graph node.
    let mut g = GraphBuilder::new();
    let doubled = g.mul(n::HZ, 2.0);
    let osc = g.sine(doubled);
    let voice = g.out_mono(osc).unwrap();

    assert!(voice.nodes.iter().any(|node| node.op == Op::Mul));
    let audio = play(&voice, &Note::new(220.0), 0.5);
    assert!((zero_crossing_hz(&audio[0], SR) - 440.0).abs() < 2.0);
}

#[test]
fn division_on_a_symbolic_input_stages_as_a_node() {
    let mut graph = GraphBuilder::new();
    let halved = graph.div(n::HZ, 2.0);
    let oscillator = graph.sine(halved);
    let voice = graph.out_mono(oscillator).unwrap();

    assert!(voice.nodes.iter().any(|node| node.op == Op::Div));
    let audio = play(&voice, &Note::new(880.0), 0.5);
    assert!((zero_crossing_hz(&audio[0], SR) - 440.0).abs() < 2.0);
}

#[test]
fn instantiation_is_deterministic() {
    // Two instantiations of one template with one note must be sample-identical.
    // Nothing may be drawn from a global generator or an allocation counter.
    let voice = tone();
    let note = Note::new(333.0);
    assert_eq!(play(&voice, &note, 0.05), play(&voice, &note, 0.05));
}

// ------------------------------------------------------------- declared params

#[test]
fn a_declared_parameter_defaults_and_clamps() {
    let mut g = GraphBuilder::new();
    let ring = g.param(ParamSpec::new("ring", 0.02, 2.0, 0.18).with_unit("s"));
    let osc = g.sine(n::HZ);
    let out = g.mul(osc, ring);
    let voice = g.out_mono(osc).unwrap_or_else(|_| unreachable!());
    let _ = out;

    // Unset: the declaration's default.
    let quiet = play(&voice, &Note::new(440.0), 0.1);
    assert!(rms(&quiet[0]) > 0.0);

    // Out of range: clamped to the declaration, not passed through.
    let mut g = GraphBuilder::new();
    let gain = g.param(ParamSpec::new("gain", 0.0, 1.0, 1.0));
    let osc = g.sine(n::HZ);
    let out = g.mul(osc, gain);
    let voice = g.out_mono(out).unwrap();

    let sane = play(&voice, &Note::new(440.0).set(0, 1.0), 0.1);
    let silly = play(&voice, &Note::new(440.0).set(0, 100.0), 0.1);
    assert!((rms(&sane[0]) - rms(&silly[0])).abs() < 1e-6);
}

// ------------------------------------------------------------------ envelopes

#[test]
fn adsr_is_keyed_to_the_notes_own_length() {
    // fundsp's adsr_live waits on a gate because a keyboard player might hold
    // the key forever. A scheduled voice knows its length up front, so the same
    // template gives a short note and a long one different shapes with no gate
    // signal anywhere.
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let env = g.adsr(Adsr::new(0.01, 0.05, 0.7, 0.1));
    let out = g.mul(osc, env);
    let voice = g.out_mono(out).unwrap();

    let short = play(&voice, &Note::new(440.0).duration(0.1), 0.6);
    let long = play(&voice, &Note::new(440.0).duration(0.4), 0.6);
    assert!(rms(&long[0]) > rms(&short[0]) * 1.5);

    // Both are silent once release has run out.
    let tail = (0.5 * SR) as usize;
    assert!(rms(&short[0][tail..]) < 1e-6);
}

#[test]
fn adsr_release_starts_from_wherever_it_got_to() {
    // A note shorter than its own attack must release from the level it
    // reached, not jump to full and then fall.
    let env = Adsr::new(1.0, 0.5, 0.5, 0.5);
    let level_at_cut = env.at(0.2, 0.2);
    assert!((level_at_cut - 0.2).abs() < 1e-9);
    assert!(env.at(0.2 + 0.25, 0.2) < level_at_cut);
    assert_eq!(env.at(0.2 + 0.5, 0.2), 0.0);
}

#[test]
fn curves_superpose() {
    // The whole argument for a basis sum over a breakpoint list: two curves
    // add. `window(a, b)` is literally step(a) - step(b).
    let gate = Curve::window(CurveClock::NoteSeconds, 1.0, 3.0);
    assert_eq!(gate.at(0.5, 1.0).unwrap(), 0.0);
    assert_eq!(gate.at(2.0, 1.0).unwrap(), 1.0);
    assert_eq!(gate.at(4.0, 1.0).unwrap(), 0.0);

    let sum = Curve::window(CurveClock::NoteSeconds, 0.0, 2.0).term(Basis::Step, 1.0, 1.0, 0.0);
    assert_eq!(sum.at(0.5, 1.0).unwrap(), 1.0);
    assert_eq!(sum.at(1.5, 1.0).unwrap(), 2.0);
    assert_eq!(sum.at(2.5, 1.0).unwrap(), 1.0);
}

#[test]
fn a_curve_is_a_pure_function_of_its_clock() {
    // Nothing accumulates, so there is no phase to migrate across an edit.
    let c = Curve::decay(CurveClock::NoteSeconds, 0.5).term(Basis::Sine, 0.3, 0.0, 0.25);
    for t in [0.0, 0.1, 0.37, 1.0, 9.0] {
        assert_eq!(c.at(t, 1.0), c.at(t, 1.0));
    }
    assert!(c.at(0.0, 1.0).unwrap() > c.at(2.0, 1.0).unwrap());
}

#[test]
fn an_impulse_into_a_resonator_rings_and_then_stops() {
    // poles.eod in one graph: delta at audio rate into a narrow bandpass is a
    // struck bar, and the decay length is pole placement rather than an
    // envelope multiply.
    let mut g = GraphBuilder::new();
    let strike = g.impulse();
    let ring = g.bandpass(strike, 587.0, 40.0);
    let out = g.dcblock(ring);
    let voice = g.out_mono(out).unwrap();

    let audio = play(&voice, &Note::new(440.0), 1.0);
    let early = rms(&audio[0][..(0.05 * SR) as usize]);
    let late = rms(&audio[0][(0.9 * SR) as usize..]);
    assert!(early > 0.0);
    assert!(late < early * 0.5, "early {early}, late {late}");
}

// --------------------------------------------------------------------- delay

#[test]
fn delay_ranges_are_validated_before_they_reach_a_backend() {
    assert_eq!(
        DelayRange::new(f64::NAN, 1.0),
        Err(DelayRangeError::NonFinite)
    );
    assert_eq!(
        DelayRange::new(-0.1, 1.0),
        Err(DelayRangeError::Negative { min_seconds: -0.1 })
    );
    assert_eq!(
        DelayRange::new(1.0, 0.5),
        Err(DelayRangeError::Reversed {
            min_seconds: 1.0,
            max_seconds: 0.5
        })
    );
}

#[test]
fn an_interpolating_delay_places_an_impulse_at_the_requested_time() {
    let seconds = 0.01;
    let mut graph = GraphBuilder::new();
    let impulse = graph.impulse();
    let delayed = graph.delay(impulse, seconds, DelayRange::fixed(seconds).unwrap());
    let voice = graph.out_mono(delayed).unwrap();

    assert_eq!(voice.tail(), seconds);
    let audio = play(&voice, &Note::new(440.0), 0.02);
    let peak = audio[0]
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
        .unwrap()
        .0;
    let expected = (seconds * SR) as usize;
    assert!(
        peak.abs_diff(expected) <= 1,
        "delayed impulse appeared at sample {peak}, expected {expected}"
    );
}

#[test]
fn a_finite_patch_stops_sources_before_downstream_state_drains() {
    let mut graph = GraphBuilder::new();
    let tone = graph.sine(220.0);
    let delayed = graph.delay(tone, 0.05, DelayRange::fixed(0.05).unwrap());
    let patch = PatchTemplate::new(graph.out_mono(delayed).unwrap()).unwrap();
    let layout = BusLayout::new(1).unwrap();
    let controls = ControlStore::new(&ControlLayout::new());
    let (mut unit, lifetime) =
        instantiate_timed_patch_routed(&patch, 0.1, 0.005, &layout, &controls).unwrap();

    assert!(lifetime.absolute_horizon >= 0.15);
    let audio = render(unit.as_mut(), SR, 0.2);
    let after_boundary = rms(&audio[0][(0.105 * SR) as usize..(0.14 * SR) as usize]);
    let after_tail = rms(&audio[0][(0.17 * SR) as usize..]);
    assert!(
        after_boundary > 0.05,
        "the output was gated instead of allowing the delay to drain"
    );
    assert!(
        after_tail < 1.0e-5,
        "the nonterminating source was not stopped at the run boundary"
    );
}

#[test]
fn a_finite_triangle_patch_stops_its_source_and_drains_its_delay() {
    let mut g = GraphBuilder::new();
    let oscillator = g.triangle(250.0);
    let delayed = g.delay(oscillator, 0.05, DelayRange::fixed(0.05).unwrap());
    let patch = PatchTemplate::new(g.out_mono(delayed).unwrap()).unwrap();
    let layout = BusLayout::new(1).unwrap();
    let controls = ControlStore::new(&ControlLayout::new());
    let (mut unit, lifetime) =
        instantiate_timed_patch_routed(&patch, 0.1, 0.005, &layout, &controls).unwrap();
    assert!(lifetime.absolute_horizon >= 0.15);
    let audio = render(unit.as_mut(), SR, 0.2);
    assert!(rms(&audio[0][(0.105 * SR) as usize..(0.14 * SR) as usize]) > 0.05);
    assert!(rms(&audio[0][(0.17 * SR) as usize..]) < 1e-5);
}

#[test]
fn delay_tails_accumulate_in_series_and_buffers_sum_in_parallel() {
    let mut graph = GraphBuilder::new();
    let impulse = graph.impulse();
    let first = graph.delay(impulse, 0.1, DelayRange::fixed(0.1).unwrap());
    let second = graph.delay(first, 0.2, DelayRange::fixed(0.2).unwrap());
    let parallel = graph.delay(impulse, 0.4, DelayRange::fixed(0.4).unwrap());
    let mixed = graph.add(second, parallel);
    let voice = graph.out_mono(mixed).unwrap();

    assert!((voice.tail() - 0.4).abs() < 1e-12);
    assert!((voice.cost().delay_buffer_seconds - 0.7).abs() < 1e-12);

    let limits = GraphLimits {
        nodes: usize::MAX,
        connections: usize::MAX,
        input_channels: usize::MAX,
        output_channels: usize::MAX,
        data_entries: usize::MAX,
        delay_buffer_seconds: 0.69,
        tail_seconds: f64::INFINITY,
    };
    assert!(matches!(
        voice.validate_limits(limits),
        Err(GraphLimitError::DelayBuffer { found, limit })
            if (found - 0.7).abs() < 1e-12 && limit == 0.69
    ));
}

// --------------------------------------------------------------------- stereo

#[test]
fn pan_is_the_one_node_with_two_outputs() {
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let voice = g.out_panned(osc).unwrap();
    assert_eq!(voice.channels(), 2);

    let left = play(&voice, &Note::new(440.0).pan(-1.0), 0.1);
    assert!(rms(&left[0]) > rms(&left[1]) * 10.0);

    let right = play(&voice, &Note::new(440.0).pan(1.0), 0.1);
    assert!(rms(&right[1]) > rms(&right[0]) * 10.0);
}

// ----------------------------------------------------------------- validation

#[test]
fn arity_is_a_property_of_the_op() {
    let template = GraphTemplate {
        nodes: vec![apteronotus_synth::Node {
            op: Op::Lowpass,
            inputs: vec![Input::new(Source::Const(0.0))],
            tail: 0.0,
            src: None,
        }],
        outputs: vec![Source::port(0, 0)],
        ..GraphTemplate::default()
    };
    assert_eq!(
        template.validate(),
        Err(TemplateError::Arity {
            node: 0,
            expected: 3,
            found: 1
        })
    );
}

#[test]
fn a_dangling_port_never_reaches_the_backend() {
    let template = GraphTemplate {
        outputs: vec![Source::port(7, 0)],
        ..GraphTemplate::default()
    };
    assert!(matches!(
        template.validate(),
        Err(TemplateError::BadOutput { channel: 0 })
    ));

    // Channel 1 of a mono node is equally dangling.
    let template = GraphTemplate {
        nodes: vec![apteronotus_synth::Node {
            op: Op::Noise,
            inputs: vec![],
            tail: 0.0,
            src: None,
        }],
        outputs: vec![Source::port(0, 1)],
        ..GraphTemplate::default()
    };
    assert!(matches!(
        template.validate(),
        Err(TemplateError::BadOutput { channel: 0 })
    ));
}

#[test]
fn a_forward_reference_is_a_cycle_and_is_refused() {
    let template = GraphTemplate {
        nodes: vec![
            apteronotus_synth::Node {
                op: Op::DcBlock,
                inputs: vec![Input::new(Source::port(1, 0))],
                tail: 0.0,
                src: None,
            },
            apteronotus_synth::Node {
                op: Op::DcBlock,
                inputs: vec![Input::new(Source::port(0, 0))],
                tail: 0.0,
                src: None,
            },
        ],
        outputs: vec![Source::port(1, 0)],
        ..GraphTemplate::default()
    };
    assert!(matches!(
        template.validate(),
        Err(TemplateError::Cycle { node: 0, port: 0 })
    ));
}

#[test]
fn a_graph_with_no_outputs_is_not_a_graph() {
    assert_eq!(
        GraphTemplate::default().validate(),
        Err(TemplateError::NoOutputs)
    );
}

#[test]
fn invalid_tail_metadata_never_reaches_the_scheduler() {
    let template = GraphTemplate {
        nodes: vec![apteronotus_synth::Node {
            op: Op::Noise,
            inputs: vec![],
            tail: f64::NAN,
            src: None,
        }],
        outputs: vec![Source::port(0, 0)],
        ..GraphTemplate::default()
    };
    assert!(matches!(
        template.validate(),
        Err(TemplateError::InvalidTail { node: 0, .. })
    ));
}

#[test]
fn invalid_parameter_ranges_never_reach_note_binding() {
    let template = GraphTemplate {
        params: vec![ParamSpec::new("broken", 2.0, 1.0, 1.5)],
        outputs: vec![Source::Const(0.0)],
        ..GraphTemplate::default()
    };
    assert_eq!(
        template.validate(),
        Err(TemplateError::InvalidParamRange { param: 0 })
    );
}

#[test]
fn publication_budgets_use_a_deterministic_graph_cost() {
    let voice = tone();
    let cost = voice.cost();
    assert_eq!(cost.nodes, 1);
    assert_eq!(cost.input_channels, 0);
    assert_eq!(cost.output_channels, 1);

    let limits = GraphLimits {
        nodes: 0,
        connections: usize::MAX,
        input_channels: usize::MAX,
        output_channels: usize::MAX,
        data_entries: usize::MAX,
        delay_buffer_seconds: f64::INFINITY,
        tail_seconds: f64::INFINITY,
    };
    assert_eq!(
        voice.validate_limits(limits),
        Err(GraphLimitError::Nodes { found: 1, limit: 0 })
    );
}

#[test]
fn per_voice_initialization_depends_only_on_event_seed_and_stream() {
    let mut g = GraphBuilder::new();
    let initialized = g.init_random(17, -1.0, 1.0).unwrap();
    let voice = g.out_mono(initialized).unwrap();

    let sample = |seed, stream_voice: &GraphTemplate| {
        play(stream_voice, &Note::new(440.0).seed(seed), 1.0 / SR)[0][0]
    };
    let a = sample(123, &voice);
    assert_eq!(a, sample(123, &voice));
    assert_ne!(a, sample(124, &voice));

    let mut g = GraphBuilder::new();
    let initialized = g.init_random(18, -1.0, 1.0).unwrap();
    let other_stream = g.out_mono(initialized).unwrap();
    assert_ne!(a, sample(123, &other_stream));
}

#[test]
fn init_random_cannot_collapse_a_runtime_signal_implicitly() {
    let mut g = GraphBuilder::new();
    let moving = g.sine(1.0);
    assert_eq!(g.init_random(0, moving, 1.0), Err(InitScalarError::Dynamic));
}

// --------------------------------------------------- the data-only invariant

#[test]
fn a_template_is_plain_data() {
    // Not a slogan — these bounds are what a template being data *means*, and
    // they are exactly what a fundsp node, a boxed unit or a host-language
    // closure would take away.
    fn assert_data<T: Clone + PartialEq + core::fmt::Debug + Send + Sync + 'static>() {}
    assert_data::<GraphTemplate>();

    let mut g = GraphBuilder::new();
    let osc = g.saw(n::HZ);
    let filtered = g.moog(osc, 1_200.0, 0.6);
    let shaped = g.shape(filtered, ShapeKind::Tanh, 1.3);
    let env = g.adsr(Adsr::new(0.005, 0.1, 0.5, 0.2));
    let out = g.mul(shaped, env);
    let voice = g.out_panned(out).unwrap();

    assert_eq!(voice.clone(), voice);
    // And a source span rides along for the editor to point at.
    assert!(voice.nodes.iter().all(|node| node.src.is_none()));
}

#[test]
fn source_spans_survive_onto_graph_nodes() {
    use apteronotus_pattern::SrcSpan;
    let mut g = GraphBuilder::new();
    g.at(SrcSpan::new(10, 20));
    let osc = g.sine(n::HZ);
    g.anywhere();
    let voice = g.out_mono(osc).unwrap();
    assert_eq!(voice.nodes[0].src, Some(SrcSpan::new(10, 20)));
    assert_eq!(voice.nodes[0].inputs[0].src, Some(SrcSpan::new(10, 20)));

    // A parser knows each argument's narrower byte range and can override the
    // call-level span rather than losing that precision in the builder.
    let mut g = GraphBuilder::new();
    g.at(SrcSpan::new(30, 50));
    let sum = g.node(
        Op::Add,
        [
            Input::at(Source::Const(1.0), SrcSpan::new(34, 35)),
            Input::at(Source::Const(2.0), SrcSpan::new(45, 46)),
        ],
    );
    let voice = g.out_mono(sum).unwrap();
    assert_eq!(voice.nodes[0].src, Some(SrcSpan::new(30, 50)));
    assert_eq!(voice.nodes[0].inputs[0].src, Some(SrcSpan::new(34, 35)));
    assert_eq!(voice.nodes[0].inputs[1].src, Some(SrcSpan::new(45, 46)));
}

#[test]
fn tail_is_conservative() {
    // Better to hold a dead voice a moment longer than to cut a bell short.
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let env = g.adsr(Adsr::new(0.01, 0.1, 0.5, 2.5));
    let out = g.mul(osc, env);
    let voice = g.out_mono(out).unwrap();
    assert_eq!(voice.tail(), 2.5);
}

#[test]
fn lifetime_keeps_gate_relative_and_absolute_components_separate() {
    let mut g = GraphBuilder::new();
    let noise = g.noise();
    let envelope = g.curve(Curve::window(CurveClock::NoteSeconds, 0.0, 2.0));
    let long_envelope_branch = g.mul(noise, envelope);
    let delayed_gate_branch = g.delay(noise, 0.5, DelayRange::fixed(0.5).unwrap());
    let output = g.add(long_envelope_branch, delayed_gate_branch);
    let voice = g.out_mono(output).unwrap();

    let lifetime = voice.lifetime();
    assert_eq!(lifetime.gate_tail, 0.5);
    assert_eq!(lifetime.absolute_horizon, 2.0);
    assert_eq!(lifetime.end_after_onset(1.0), 2.0);
}

#[test]
fn final_adsr_retires_long_decay_and_keeps_the_audible_release() {
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let long = g.curve(Curve::decay(CurveClock::NoteSeconds, 8.0));
    let audio = g.mul(osc, long);
    let envelope = g.adsr(Adsr::new(0.001, 0.03, 0.8, 0.14));
    // Match the score's scaled envelope, not just audio * bare ADSR.
    let envelope = g.mul(envelope, 0.22);
    let output = g.mul(audio, envelope);
    let voice = g.out_mono(output).unwrap();
    let note = Note::new(110.0).duration(0.3);
    let lifetime = voice.lifetime_for(&note).unwrap();
    assert_eq!(lifetime.absolute_horizon, 0.0);
    assert_eq!(lifetime.gate_tail, 0.14);
    let audio = play(&voice, &note, 0.6);
    assert!(rms(&audio[0][(0.32 * SR) as usize..(0.4 * SR) as usize]) > 0.01);
    assert!(
        audio[0][(0.44 * SR).ceil() as usize..]
            .iter()
            .all(|s| *s == 0.0)
    );
}

#[test]
fn final_adsr_does_not_discard_downstream_or_ungated_tails() {
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let long = g.curve(Curve::decay(CurveClock::NoteSeconds, 8.0));
    let audio = g.mul(osc, long);
    let envelope = g.adsr(Adsr::new(0.001, 0.03, 0.8, 0.14));
    let gated = g.mul(audio, envelope);
    let delayed = g.delay(gated, 0.2, DelayRange::fixed(0.2).unwrap());
    let voice = g.out_mono(delayed).unwrap();
    assert!((voice.lifetime().gate_tail - 0.34).abs() < 1e-12);
    assert_eq!(voice.lifetime().absolute_horizon, 0.0);
    let note = Note::new(110.0).duration(0.3);
    let samples = play(&voice, &note, 0.8);
    assert!(rms(&samples[0][(0.52 * SR) as usize..(0.6 * SR) as usize]) > 0.01);

    // A separate output before the gate must still retain its entire history.
    let mut ungated = voice.clone();
    ungated.outputs = vec![gated, audio];
    ungated.validate().unwrap();
    assert_eq!(ungated.lifetime().absolute_horizon, 8.0);
}

#[test]
fn mul_uses_component_wise_maximum_for_safe_retention() {
    let mut g = GraphBuilder::new();
    let noise = g.noise();
    let delayed = g.delay(noise, 2.0, DelayRange::fixed(2.0).unwrap());
    let short = g.curve(Curve::decay(CurveClock::NoteSeconds, 0.05));
    let output = g.mul(delayed, short);
    let voice = g.out_mono(output).unwrap();

    let lifetime = voice.lifetime();
    assert_eq!(lifetime.gate_tail, 2.0);
    assert_eq!(lifetime.absolute_horizon, 0.05);
    assert_eq!(lifetime.end_after_onset(0.1), 2.1);
}

#[test]
fn ordinary_gain_and_an_added_dc_bias_are_not_hard_silence() {
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let delayed = g.delay(osc, 2.0, DelayRange::fixed(2.0).unwrap());
    let constant_gain = g.mul(delayed, 0.5);
    let envelope = g.adsr(Adsr::new(0.001, 0.03, 0.8, 0.14));
    let biased_envelope = g.add(envelope, 0.1);
    let biased_gain = g.mul(delayed, biased_envelope);
    let voice = g.out(&[constant_gain, biased_gain]).unwrap();
    assert_eq!(voice.lifetime().gate_tail, 2.0);
}

#[test]
fn a_curve_on_a_filter_control_port_does_not_extend_audio() {
    let mut g = GraphBuilder::new();
    let noise = g.noise();
    let cutoff = g.curve(Curve::window(CurveClock::NoteSeconds, 0.0, 8.0));
    let filtered = g.lowpass(noise, cutoff, 0.7);
    let voice = g.out_mono(filtered).unwrap();
    assert_eq!(voice.lifetime().absolute_horizon, 0.0);
}

#[test]
fn bounded_feedback_delay_produces_audible_repeats_and_declares_its_tail() {
    let mut g = GraphBuilder::new();
    let impulse = g.impulse();
    let echoed = g.feedback_delay(impulse, 0.02, Some((3_200.0, 0.707)), 0.5);
    let voice = g.out_mono(echoed).unwrap();
    assert!(voice.lifetime().gate_tail >= 0.08);

    let audio = play(&voice, &Note::new(440.0), 0.12);
    let first_echo = (SR * 0.02) as usize;
    let second_echo = (SR * 0.04) as usize;
    let peak = |center: usize| {
        audio[0][center.saturating_sub(8)..center + 24]
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0, f32::max)
    };
    assert!(peak(first_echo) > 0.01);
    assert!(peak(second_echo) > 0.001);
}

#[test]
fn hadamard_fdn_has_bounded_storage_and_an_audible_decay() {
    let mut g = GraphBuilder::new();
    let impulse = g.impulse();
    let wet = g.fdn(
        impulse,
        Source::Const(0.35),
        FdnConfig {
            delays: vec![0.011, 0.013, 0.017, 0.019],
            damping: 0.35,
            modulation_rate: 0.17,
            modulation_depth: 0.0007,
            max_t60: 0.5,
        },
    );
    let voice = g.out_mono(wet).unwrap();
    voice.validate().unwrap();
    assert_eq!(voice.lifetime().gate_tail, 0.5);
    assert!(voice.cost().delay_buffer_seconds > 0.06);

    let audio = play(&voice, &Note::new(440.0), 0.25);
    assert!(audio[0].iter().all(|sample| sample.is_finite()));
    let first_arrival = (SR * 0.010) as usize;
    assert!(rms(&audio[0][first_arrival..]) > 0.0001);
}

#[test]
fn analyser_warmup_and_audible_response_are_distinct_metadata() {
    let pitch = Op::PitchTracker {
        min_hz: 65.0,
        max_hz: 1_100.0,
        default_hz: 220.0,
        hold_seconds: 0.14,
    };
    assert_eq!(pitch.warmup_seconds(), 0.14);

    let mut g = GraphBuilder::new();
    let impulse = g.impulse();
    let followed = g.envelope_follower(impulse, 0.006, 0.12);
    let follower = g.out_mono(followed).unwrap();
    assert_eq!(follower.lifetime().gate_tail, 0.12);

    let mut g = GraphBuilder::new();
    let impulse = g.impulse();
    let onset = g.onset_detector(impulse, 0.04, 0.09);
    let detector = g.out_mono(onset).unwrap();
    assert_eq!(detector.lifetime().gate_tail, ONSET_PULSE_SECONDS);
    assert_eq!(detector.nodes[1].op.warmup_seconds(), 0.09);
}

#[test]
fn init_rate_pluck_parameters_create_a_bounded_audible_string() {
    let mut g = GraphBuilder::new();
    let excitation = g.impulse();
    let string = g.pluck(excitation, n::HZ, 0.5, 0.86).unwrap();
    let voice = g.out_mono(string).unwrap();
    assert!(voice.lifetime().gate_tail > 4.0);
    assert!(voice.cost().delay_buffer_seconds >= 1.0);

    let audio = play(&voice, &Note::new(220.0), 0.25);
    assert!(audio[0].iter().all(|sample| sample.is_finite()));
    assert!(rms(&audio[0]) > 0.01);
}

#[test]
fn note_phase_parameter_curves_remain_live_for_the_held_note() {
    let mut g = GraphBuilder::new();
    let gain = g.param(ParamSpec::new("gain", 0.0, 1.0, 0.0));
    let tone = g.sine(220.0);
    let output = g.mul(tone, gain);
    let voice = g.out_mono(output).unwrap();
    let curve = Curve::new(CurveClock::NotePhase, 0.0).term(Basis::Ramp, 1.0, 0.0, 1.0);
    let note = Note::new(220.0).duration(0.4).set_value(0, curve.into());
    let mut unit = instantiate(&voice, &note).unwrap();
    let audio = render(unit.as_mut(), SR, 0.4);
    let quarter = audio[0].len() / 4;
    let early = rms(&audio[0][..quarter]);
    let late = rms(&audio[0][quarter * 3..]);
    assert!(late > early * 2.0, "early {early}, late {late}");
}

#[test]
fn note_seconds_parameter_horizons_are_declared_and_join_the_same_lifetime_walk() {
    let mut g = GraphBuilder::new();
    let gain = g.param(ParamSpec::new("gain", 0.0, 1.0, 0.0).with_curve_horizon(3.0));
    let noise = g.noise();
    let output = g.mul(noise, gain);
    let voice = g.out_mono(output).unwrap();
    assert_eq!(voice.cost().tail_seconds, 3.0);
    let curve = Curve::window(CurveClock::NoteSeconds, 0.0, 2.0);
    let note = Note::new(220.0).duration(0.1).set_value(0, curve.into());

    let lifetime = voice.lifetime_for(&note).unwrap();
    assert_eq!(lifetime.absolute_horizon, 2.0);
    assert_eq!(lifetime.end_after_onset(note.duration), 2.0);
}

#[test]
fn onset_detector_emits_one_bounded_pulse_for_one_impulse() {
    let mut graph = GraphBuilder::new();
    let impulse = graph.impulse();
    let onset = graph.onset_detector(impulse, 0.04, 0.09);
    let template = graph.out_mono(onset).unwrap();
    let audio = play(&template, &Note::new(220.0), 0.05);
    let high = audio[0].iter().filter(|sample| **sample > 0.5).count();
    assert!((1_400..=1_500).contains(&high));
}

#[test]
fn compiled_transport_sequence_repeats_without_querying_a_pattern() {
    let mut graph = GraphBuilder::new();
    let sequence = graph.transport_sequence(
        0.1,
        vec![
            TransportSlot {
                begin_seconds: 0.0,
                end_seconds: 0.05,
                value: 0.2,
            },
            TransportSlot {
                begin_seconds: 0.05,
                end_seconds: 0.1,
                value: 0.8,
            },
        ],
    );
    let template = graph.out_mono(sequence).unwrap();
    let audio = play(&template, &Note::new(220.0), 0.12);
    assert!((audio[0][100] - 0.2).abs() < 1.0e-6);
    assert!((audio[0][3_000] - 0.8).abs() < 1.0e-6);
    assert!((audio[0][5_000] - 0.2).abs() < 1.0e-6);
    assert_eq!(template.cost().data_entries, 2);
}

#[test]
fn shared_slew_can_start_at_its_published_default() {
    let mut graph = GraphBuilder::new();
    let slew = graph.slew_from(Source::Const(440.0), 0.06, 440.0).unwrap();
    let template = graph.out_mono(slew).unwrap();
    let audio = play(&template, &Note::new(220.0), 0.01);
    assert!((audio[0][0] - 440.0).abs() < 0.01);
}

#[test]
fn width_is_a_modulated_stereo_mid_side_operation() {
    let mut graph = GraphBuilder::new();
    let [left, right] = graph.width(Source::Const(1.0), Source::Const(0.0), Source::Const(1.0));
    let template = graph.out(&[left, right]).unwrap();
    let audio = play(&template, &Note::new(220.0), 0.01);
    assert!((audio[0][0] - 1.0).abs() < 1.0e-6);
    assert!(audio[1][0].abs() < 1.0e-6);
}
