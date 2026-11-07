use apteronotus_synth::{
    Adsr, BusLayout, ControlLayout, ControlStore, Curve, CurveClock, EventRouting, GraphBuilder,
    LiveGate, LowerError, Note, instantiate_live_routed_with_controls, live_voice_lifetime, n,
    validate_live_voice,
};
use fundsp::prelude32::AudioUnit;

fn ticks(unit: &mut dyn AudioUnit, count: usize) -> Vec<Vec<f32>> {
    let mut result = vec![Vec::new(); unit.outputs()];
    let mut sample = vec![0.0; unit.outputs()];
    for _ in 0..count {
        unit.tick(&[], &mut sample);
        for (lane, value) in result.iter_mut().zip(&sample) {
            lane.push(*value);
        }
    }
    result
}

#[test]
fn live_voices_hold_and_release_graph_and_event_sends() {
    let mut layout = BusLayout::new(1).unwrap();
    let bus = layout.add_bus(1).unwrap();
    let copy = layout.add_bus(1).unwrap();
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let envelope = g.adsr(Adsr::new(0.002, 0.01, 0.8, 0.05));
    let dry = g.mul(osc, envelope);
    g.send(bus, &[dry], 0.5);
    let graph = g.out_mono(dry).unwrap();
    let mut event = EventRouting::new();
    event.send(copy, 0.25).unwrap();
    let controls = ControlStore::new(&ControlLayout::new());
    let gate = LiveGate::new();
    let mut unit = instantiate_live_routed_with_controls(
        &graph,
        &Note::new(100.0),
        &layout,
        &event,
        &controls,
        &gate,
    )
    .unwrap();
    unit.set_sample_rate(8000.0);
    let held = ticks(unit.as_mut(), 16000);
    assert!(held[0][12000..].iter().any(|v| v.abs() > 0.2));
    gate.release_after(2.0).unwrap();
    let released = ticks(unit.as_mut(), 1000);
    for samples in &released {
        assert!(samples[400..].iter().all(|s| *s == 0.0));
    }
    for ((dry, half), quarter) in held[0].iter().zip(&held[1]).zip(&held[2]) {
        assert_eq!(*half, *dry * 0.5);
        assert_eq!(*quarter, *dry * 0.25);
    }
}

#[test]
fn unenveloped_sources_fade() {
    let mut g = GraphBuilder::new();
    let oscillator = g.sine(100.0);
    let graph = g.out_mono(oscillator).unwrap();
    let gate = LiveGate::new();
    gate.release_after(0.1).unwrap();
    let mut unit = instantiate_live_routed_with_controls(
        &graph,
        &Note::new(100.0),
        &BusLayout::new(1).unwrap(),
        &EventRouting::new(),
        &ControlStore::new(&ControlLayout::new()),
        &gate,
    )
    .unwrap();
    unit.set_sample_rate(8000.0);
    let samples = ticks(unit.as_mut(), 1000);
    assert!(samples[0][..800].iter().any(|v| v.abs() > 0.5));
    let terminal = ((0.1_f64 + 0.005) * 8000.0).ceil() as usize;
    assert!(samples[0][terminal..].iter().all(|v| *v == 0.0));
    assert!(
        live_voice_lifetime(&graph, &Note::new(100.0))
            .unwrap()
            .gate_tail
            >= 0.005
    );
}

#[test]
fn unknown_duration_is_rejected_in_graph_sources_init_expressions_and_phase_curves() {
    let mut g = GraphBuilder::new();
    let graph = g.out_mono(n::DURATION).unwrap();
    assert!(matches!(
        validate_live_voice(&graph),
        Err(LowerError::LiveDurationRequired)
    ));
    let mut g = GraphBuilder::new();
    let random = g.init_random(0, n::DURATION, 3.0).unwrap();
    let graph = g.out_mono(random).unwrap();
    assert!(matches!(
        validate_live_voice(&graph),
        Err(LowerError::LiveDurationRequired)
    ));
    let mut g = GraphBuilder::new();
    let curve = g.curve(Curve::new(CurveClock::NotePhase, 1.0));
    let graph = g.out_mono(curve).unwrap();
    assert!(matches!(
        validate_live_voice(&graph),
        Err(LowerError::LiveDurationRequired)
    ));
    let mut g = GraphBuilder::new();
    let curve = g.curve(Curve::new(CurveClock::NoteSeconds, 0.0));
    let graph = g.out_mono(curve).unwrap();
    validate_live_voice(&graph).unwrap();
}

#[test]
fn downstream_delay_retains_release_history() {
    let mut g = GraphBuilder::new();
    let osc = g.sine(100.0);
    let delayed = g.delay(
        osc,
        0.02,
        apteronotus_synth::DelayRange::new(0.02, 0.02).unwrap(),
    );
    let graph = g.out_mono(delayed).unwrap();
    let gate = LiveGate::new();
    gate.release_after(0.1).unwrap();
    let mut unit = instantiate_live_routed_with_controls(
        &graph,
        &Note::new(100.0),
        &BusLayout::new(1).unwrap(),
        &EventRouting::new(),
        &ControlStore::new(&ControlLayout::new()),
        &gate,
    )
    .unwrap();
    unit.set_sample_rate(8000.0);
    let audio = ticks(unit.as_mut(), 1200);
    assert!(
        audio[0][850..950].iter().any(|v| v.abs() > 0.4),
        "delayed pre-release sound must survive note-off"
    );
    let lifetime = live_voice_lifetime(&graph, &Note::new(100.0)).unwrap();
    let terminal = (lifetime.end_after_onset(0.1) * 8000.0).ceil() as usize;
    assert!(audio[0][terminal..].iter().all(|v| *v == 0.0));
}

#[test]
fn event_phase_curve_is_rejected_and_seconds_curve_is_supported() {
    use apteronotus_synth::ParamSpec;
    let mut g = GraphBuilder::new();
    let param = g.param(ParamSpec::new("level", 0.0, 1.0, 0.5).with_curve_horizon(2.0));
    let osc = g.sine(100.0);
    let scaled = g.mul(osc, param);
    let graph = g.out_mono(scaled).unwrap();
    let id = match param {
        apteronotus_synth::Source::Param(id) => id,
        _ => unreachable!(),
    };
    let controls = ControlStore::new(&ControlLayout::new());
    let layout = BusLayout::new(1).unwrap();
    let phase = Note::new(100.0).bind(id, Curve::new(CurveClock::NotePhase, 0.5));
    assert!(matches!(
        instantiate_live_routed_with_controls(
            &graph,
            &phase,
            &layout,
            &EventRouting::new(),
            &controls,
            &LiveGate::new()
        ),
        Err(LowerError::LiveDurationRequired)
    ));
    let seconds = Note::new(100.0).bind(id, Curve::new(CurveClock::NoteSeconds, 0.5));
    instantiate_live_routed_with_controls(
        &graph,
        &seconds,
        &layout,
        &EventRouting::new(),
        &controls,
        &LiveGate::new(),
    )
    .unwrap();
}

#[test]
fn exact_gated_outputs_retire_at_release_but_bias_and_raw_sends_keep_safety_tail() {
    let note = Note::new(220.0);
    let mut builder = GraphBuilder::new();
    let first = builder.harmonics(n::HZ, vec![0.5, 0.3]);
    let second = builder.harmonics(n::HZ, vec![0.0, 0.0, 0.1]);
    let envelope = builder.adsr(Adsr::new(0.01, 0.02, 0.72, 0.05));
    let first = builder.mul(first, envelope);
    let second = builder.mul(second, envelope);
    let second = builder.div(second, 0.72);
    let combined = builder.add(first, second);
    let (left, right) = builder.pan(combined, -0.4);
    let graph = builder.out(&[left, right]).unwrap();
    assert_eq!(live_voice_lifetime(&graph, &note).unwrap().gate_tail, 0.05);

    let mut builder = GraphBuilder::new();
    let envelope = builder.adsr(Adsr::new(0.01, 0.02, 0.72, 0.05));
    let biased = builder.add(envelope, 0.1);
    let graph = builder.out_mono(biased).unwrap();
    assert!(live_voice_lifetime(&graph, &note).unwrap().gate_tail > 0.1);

    let mut buses = BusLayout::new(1).unwrap();
    let room = buses.add_bus(1).unwrap();
    let mut builder = GraphBuilder::new();
    let raw = builder.harmonics(n::HZ, vec![1.0]);
    let envelope = builder.adsr(Adsr::new(0.01, 0.02, 0.72, 0.05));
    let dry = builder.mul(raw, envelope);
    builder.send(room, &[raw], 0.5);
    let graph = builder.out_mono(dry).unwrap();
    assert!(live_voice_lifetime(&graph, &note).unwrap().gate_tail > 0.1);
}
