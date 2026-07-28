//! Progressive transcription of `songs/poles.eod`.

use apteronotus_synth::lower::{render, rms, zero_crossing_hz};
use apteronotus_synth::stdlib::{RingError, ring};
use apteronotus_synth::{
    Curve, GraphBuilder, GraphTemplate, Note, Op, ParamSpec, ShapeKind, instantiate, n,
};

const SR: f64 = 48_000.0;

fn rim() -> GraphTemplate {
    let mut graph = GraphBuilder::new();
    let strike = graph.impulse();
    let resonator = ring(&mut graph, strike, 1_700.0, 0.028).unwrap();
    let filtered = graph.highpass(resonator, 400.0, 0.7);
    let weighted = graph.mul(filtered, n::VELOCITY);
    let clean = graph.dcblock(weighted);
    let (left, right) = graph.pan(clean, -0.25);
    graph.out(&[left, right]).unwrap()
}

fn cowbell() -> GraphTemplate {
    let mut graph = GraphBuilder::new();
    let decay = graph.param(ParamSpec::new("ring", 0.02, 2.0, 0.18).with_unit("s"));
    let strike = graph.impulse();
    let low = ring(&mut graph, strike, 587.0, decay).unwrap();
    let short_decay = graph.mul(decay, 0.85);
    let high = ring(&mut graph, strike, 845.0, short_decay).unwrap();
    let high = graph.mul(high, 0.7);
    let modes = graph.add(low, high);
    // fundsp's SVF impulse response peaks well below full scale. Drive the
    // modes into the specified waveshaper so it contributes the bright upper
    // partials that distinguish a cowbell from two hollow wooden modes.
    let modes = graph.mul(modes, 7.0);
    let driven = graph.shape(modes, ShapeKind::Tanh, 1.3);
    let clean = graph.dcblock(driven);
    graph.out_panned(clean).unwrap()
}

fn bell() -> GraphTemplate {
    let mut graph = GraphBuilder::new();
    let decay = graph.param(ParamSpec::new("ring", 0.1, 8.0, 2.6).with_unit("s"));
    let strike = graph.impulse();
    let mut modes = Vec::new();
    for (index, ratio) in [1.0, 2.76, 5.40, 8.93, 13.34].into_iter().enumerate() {
        let member = (index + 1) as f64;
        let hz = graph.mul(n::HZ, ratio);
        let member_decay = graph.div(decay, member);
        let mode = ring(&mut graph, strike, hz, member_decay).unwrap();
        modes.push(graph.mul(mode, 1.0 / member));
    }
    let modes = graph.mix(modes);
    let velocity = graph.mul(n::VELOCITY, 0.4);
    let weighted = graph.mul(modes, velocity);
    let clean = graph.dcblock(weighted);
    graph.out_panned(clean).unwrap()
}

fn tom() -> GraphTemplate {
    let mut graph = GraphBuilder::new();
    let decay = graph.param(ParamSpec::new("ring", 0.05, 1.5, 0.34).with_unit("s"));
    let bend = graph.param(ParamSpec::new("bend", 0.0, 2.0, 0.7));
    let pitch_drop = graph.curve(Curve::decay(0.045));
    let pitch_drop = graph.mul(bend, pitch_drop);
    let pitch_drop = graph.add(1.0, pitch_drop);
    let swept = graph.mul(n::HZ, pitch_drop);
    let strike = graph.impulse();
    let body = ring(&mut graph, strike, swept, decay).unwrap();
    let driven = graph.shape(body, ShapeKind::Tanh, 1.2);
    let weighted = graph.mul(driven, n::VELOCITY);
    let clean = graph.dcblock(weighted);
    graph.out_panned(clean).unwrap()
}

fn cymbal() -> GraphTemplate {
    let mut graph = GraphBuilder::new();
    let noise = graph.noise();
    let burst = graph.curve(Curve::decay(0.004));
    let strike = graph.mul(noise, burst);
    let mut modes = Vec::new();
    for hz in [2_810.0, 3_730.0, 4_520.0, 5_890.0, 7_200.0, 9_430.0] {
        let mode = ring(&mut graph, strike, hz, 1.6).unwrap();
        modes.push(graph.mul(mode, 0.2));
    }
    let wash = graph.mix(modes);
    let weighted = graph.mul(wash, n::VELOCITY);
    let clean = graph.dcblock(weighted);
    graph.out_panned(clean).unwrap()
}

fn magnitude_at(samples: &[f32], hz: f64) -> f64 {
    let mut real = 0.0;
    let mut imaginary = 0.0;
    for (index, sample) in samples.iter().enumerate() {
        let phase = core::f64::consts::TAU * hz * index as f64 / SR;
        real += *sample as f64 * phase.cos();
        imaginary -= *sample as f64 * phase.sin();
    }
    real.hypot(imaginary) / samples.len() as f64
}

#[test]
fn ring_is_composition_with_declared_tail() {
    let mut graph = GraphBuilder::new();
    let strike = graph.impulse();
    let first = ring(&mut graph, strike, 1_000.0, 0.1).unwrap();
    let second = ring(&mut graph, first, 1_000.0, 0.2).unwrap();
    let voice = graph.out_mono(second).unwrap();

    assert!((voice.tail() - 0.3).abs() < 1e-12);
    assert_eq!(voice.nodes.iter().filter(|node| node.tail > 0.0).count(), 2);
}

#[test]
fn ring_rejects_an_invalid_decay_before_building_nodes() {
    let mut graph = GraphBuilder::new();
    let strike = graph.impulse();
    let nodes_before = graph.node_id(strike).unwrap() + 1;
    assert_eq!(
        ring(&mut graph, strike, 1_700.0, 0.0),
        Err(RingError::NonPositiveDecay { minimum: 0.0 })
    );
    let voice = graph.out_mono(strike).unwrap();
    assert_eq!(voice.nodes.len(), nodes_before);
}

#[test]
fn symbolic_decay_uses_declared_parameter_bounds() {
    let voice = cowbell();
    let mut tails: Vec<_> = voice
        .nodes
        .iter()
        .filter_map(|node| (node.tail > 0.0).then_some(node.tail))
        .collect();
    tails.sort_by(f64::total_cmp);
    assert_eq!(tails, vec![1.7, 2.0]);
    assert_eq!(voice.tail(), 2.0);
}

#[test]
fn runtime_signals_cannot_secretly_decide_voice_lifetime() {
    let mut graph = GraphBuilder::new();
    let strike = graph.impulse();
    let decay = graph.curve(apteronotus_synth::Curve::decay(0.2));
    assert_eq!(
        ring(&mut graph, strike, 587.0, decay),
        Err(RingError::UnboundedDecay)
    );
}

#[test]
fn the_poles_cowbell_is_two_differently_damped_modes() {
    let voice = cowbell();
    assert_eq!(
        voice
            .nodes
            .iter()
            .filter(|node| node.op == Op::Bandpass)
            .count(),
        2
    );
    assert!(voice.nodes.iter().any(|node| {
        matches!(
            node.op,
            Op::Shape {
                kind: ShapeKind::Tanh,
                amount
            } if amount == 1.3
        )
    }));

    let mut default = instantiate(&voice, &Note::new(440.0)).unwrap();
    let long = render(default.as_mut(), SR, 0.6);
    let mut short = instantiate(&voice, &Note::new(440.0).set(0, 0.03)).unwrap();
    let short = render(short.as_mut(), SR, 0.6);

    let onset = rms(&long[0][..(0.03 * SR) as usize]);
    let peak = long[0].iter().copied().map(f32::abs).fold(0.0, f32::max);
    let long_late = rms(&long[0][(0.15 * SR) as usize..(0.3 * SR) as usize]);
    let short_late = rms(&short[0][(0.15 * SR) as usize..(0.3 * SR) as usize]);
    assert!(onset > 0.001, "cowbell did not sound: RMS {onset}");
    assert!(
        long_late > short_late * 4.0,
        "ring parameter did not lengthen decay: {long_late} vs {short_late}"
    );
    assert!(peak > 0.6, "cowbell never reached its waveshaper: {peak}");
    assert!((rms(&long[0]) - rms(&long[1])).abs() < 1e-6);
}

#[test]
fn the_poles_bell_exercises_loops_fanout_and_symbolic_pitch() {
    let voice = bell();
    assert_eq!(
        voice
            .nodes
            .iter()
            .filter(|node| node.op == Op::Bandpass)
            .count(),
        5
    );
    assert_eq!(
        voice.nodes.iter().filter(|node| node.op == Op::Div).count(),
        5
    );

    let mut tails: Vec<_> = voice
        .nodes
        .iter()
        .filter_map(|node| (node.tail > 0.0).then_some(node.tail))
        .collect();
    tails.sort_by(f64::total_cmp);
    let expected = [1.6, 2.0, 8.0 / 3.0, 4.0, 8.0];
    for (actual, expected) in tails.into_iter().zip(expected) {
        assert!((actual - expected).abs() < 1e-12);
    }
    // Parallel modes take the longest branch; they do not add as serial
    // resonators do.
    assert_eq!(voice.tail(), 8.0);

    let c5 = 523.251;
    let g4 = 391.995;
    let mut c_unit = instantiate(&voice, &Note::new(c5)).unwrap();
    let c_audio = render(c_unit.as_mut(), SR, 0.5);
    let mut g_unit = instantiate(&voice, &Note::new(g4)).unwrap();
    let g_audio = render(g_unit.as_mut(), SR, 0.5);
    assert!(magnitude_at(&c_audio[0], c5) > magnitude_at(&c_audio[0], g4) * 3.0);
    assert!(magnitude_at(&g_audio[0], g4) > magnitude_at(&g_audio[0], c5) * 3.0);
}

#[test]
fn the_poles_rim_is_a_short_tuned_stereo_strike() {
    let voice = rim();
    assert_eq!(voice.channels(), 2);
    assert_eq!(voice.tail(), 0.028);

    let mut unit = instantiate(&voice, &Note::new(440.0).velocity(0.8)).unwrap();
    let audio = render(unit.as_mut(), SR, 0.12);
    let early = rms(&audio[0][..(0.012 * SR) as usize]);
    let late = rms(&audio[0][(0.08 * SR) as usize..]);
    assert!(early > 0.001, "rim did not sound: RMS {early}");
    assert!(late < early * 0.15, "rim did not decay: {early} -> {late}");

    let measured = zero_crossing_hz(&audio[0][20..600], SR);
    assert!(
        (measured - 1_700.0).abs() < 100.0,
        "expected a 1700 Hz rim mode, measured {measured}"
    );
    assert!(rms(&audio[0]) > rms(&audio[1]));
}

#[test]
fn the_poles_tom_proves_a_control_signal_can_sweep_a_filter_port() {
    let voice = tom();
    assert_eq!(
        voice
            .nodes
            .iter()
            .filter(|node| node.op == Op::Bandpass)
            .count(),
        1
    );
    assert!(
        voice
            .nodes
            .iter()
            .any(|node| matches!(node.op, Op::Curve(_)))
    );
    // The control sweep can still affect the resonator for 45 ms, after which
    // the longest allowed ring contributes its own 1.5 seconds.
    assert_eq!(voice.tail(), 1.545);

    let note = Note::new(98.0).duration(0.2);
    let mut default = instantiate(&voice, &note).unwrap();
    let default = render(default.as_mut(), SR, 0.5);
    let mut flat = instantiate(&voice, &note.clone().set(1, 0.0)).unwrap();
    let flat = render(flat.as_mut(), SR, 0.5);

    assert!(rms(&default[0]) > 0.001);
    assert_ne!(
        default[0], flat[0],
        "bend parameter did not reach the graph"
    );
}

#[test]
fn the_poles_cymbal_is_one_short_noise_burst_fanned_into_six_modes() {
    let voice = cymbal();
    assert_eq!(
        voice
            .nodes
            .iter()
            .filter(|node| node.op == Op::Bandpass)
            .count(),
        6
    );
    assert_eq!(
        voice
            .nodes
            .iter()
            .filter(|node| node.op == Op::Noise)
            .count(),
        1
    );
    assert_eq!(voice.tail(), 1.604);

    let mut unit = instantiate(&voice, &Note::new(440.0)).unwrap();
    let audio = render(unit.as_mut(), SR, 0.5);
    let early = rms(&audio[0][..(0.05 * SR) as usize]);
    let late = rms(&audio[0][(0.4 * SR) as usize..]);
    assert!(early > 0.001, "cymbal fixture did not sound: RMS {early}");
    assert!(
        late < early,
        "noise excitation did not decay: {early} -> {late}"
    );
}
