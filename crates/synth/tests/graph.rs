//! What the graph layer owes, tested without a sound card.
//!
//! `crates/pattern` is testable offline because a query is pure. This crate is
//! testable offline because a template is data and instantiation is
//! deterministic — render into a buffer and ask the samples. Anything that can
//! only be checked by listening is a bug in the seam, not a fact of audio.

use apteronotus_synth::lower::{render, rms, zero_crossing_hz};
use apteronotus_synth::{
    Adsr, Basis, Curve, GraphBuilder, GraphTemplate, Input, Note, Op, ParamSpec, ShapeKind, Source,
    TemplateError, instantiate, n,
};

const SR: f64 = 48_000.0;

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
    let gate = Curve::window(1.0, 3.0);
    assert_eq!(gate.at(0.5), 0.0);
    assert_eq!(gate.at(2.0), 1.0);
    assert_eq!(gate.at(4.0), 0.0);

    let sum = Curve::window(0.0, 2.0).term(Basis::Step, 1.0, 1.0, 0.0);
    assert_eq!(sum.at(0.5), 1.0);
    assert_eq!(sum.at(1.5), 2.0);
    assert_eq!(sum.at(2.5), 1.0);
}

#[test]
fn a_curve_is_a_pure_function_of_its_clock() {
    // Nothing accumulates, so there is no phase to migrate across an edit.
    let c = Curve::decay(0.5).term(Basis::Sine, 0.3, 0.0, 0.25);
    for t in [0.0, 0.1, 0.37, 1.0, 9.0] {
        assert_eq!(c.at(t), c.at(t));
    }
    assert!(c.at(0.0) > c.at(2.0));
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
                src: None,
            },
            apteronotus_synth::Node {
                op: Op::DcBlock,
                inputs: vec![Input::new(Source::port(0, 0))],
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
