use apteronotus_synth::lower::{render, zero_crossing_hz};
use apteronotus_synth::{
    Adsr, ControlError, ControlLayout, ControlSpec, ControlStore, DelayRange, GraphBuilder,
    LowerError, Note, PatchError, PatchTemplate, instantiate, instantiate_patch,
    instantiate_with_controls, n,
};

const SR: f64 = 48_000.0;

#[test]
fn control_values_default_clamp_and_reject_non_finite_updates() {
    let mut layout = ControlLayout::new();
    let cutoff = layout
        .add(ControlSpec::new("cutoff", 100.0, 10_000.0, 800.0))
        .unwrap();
    let store = ControlStore::new(&layout);

    assert_eq!(store.value(cutoff).unwrap(), 800.0);
    store.set(cutoff, 20_000.0).unwrap();
    assert_eq!(store.value(cutoff).unwrap(), 10_000.0);
    assert_eq!(
        store.set(cutoff, f64::NAN),
        Err(ControlError::NonFiniteValue)
    );
    assert_eq!(store.value(cutoff).unwrap(), 10_000.0);
}

#[test]
fn handles_cannot_be_accidentally_resolved_in_another_program_arena() {
    let mut first = ControlLayout::new();
    let foreign = first
        .add(ControlSpec::new("pitch", 20.0, 20_000.0, 220.0))
        .unwrap();
    let mut second = ControlLayout::new();
    second
        .add(ControlSpec::new("pitch", 20.0, 20_000.0, 330.0))
        .unwrap();
    let store = ControlStore::new(&second);

    assert!(matches!(
        store.value(foreign),
        Err(ControlError::UnknownControl(id)) if id == foreign
    ));
}

#[test]
fn one_patch_instance_survives_control_updates() {
    let mut layout = ControlLayout::new();
    let pitch = layout
        .add(ControlSpec::new("pitch", 20.0, 20_000.0, 220.0))
        .unwrap();
    let store = ControlStore::new(&layout);

    let mut g = GraphBuilder::new();
    let pitch_signal = g.control(pitch);
    let oscillator = g.sine(pitch_signal);
    let graph = g.out_mono(oscillator).unwrap();
    let patch = PatchTemplate::new(graph).unwrap();
    let mut unit = instantiate_patch(&patch, &store).unwrap();

    let low = render(unit.as_mut(), SR, 0.1);
    store.set(pitch, 440.0).unwrap();
    let high = render(unit.as_mut(), SR, 0.1);

    assert!((zero_crossing_hz(&low[0], SR) - 220.0).abs() < 3.0);
    assert!((zero_crossing_hz(&high[0], SR) - 440.0).abs() < 3.0);
}

#[test]
fn global_controls_can_also_feed_ordinary_polyphonic_voices() {
    let mut layout = ControlLayout::new();
    let gain = layout
        .add(ControlSpec::new("gain", 0.0, 1.0, 0.25))
        .unwrap();
    let store = ControlStore::new(&layout);
    let mut g = GraphBuilder::new();
    let oscillator = g.sine(n::HZ);
    let controlled = g.mul(oscillator, g.control(gain));
    let voice = g.out_mono(controlled).unwrap();

    assert!(matches!(
        instantiate(&voice, &Note::new(440.0)),
        Err(LowerError::ControlStoreRequired)
    ));
    assert!(instantiate_with_controls(&voice, &Note::new(440.0), &store).is_ok());
}

#[test]
fn note_inputs_and_note_clocks_are_rejected_from_patches() {
    let mut g = GraphBuilder::new();
    let oscillator = g.sine(n::HZ);
    let graph = g.out_mono(oscillator).unwrap();
    assert_eq!(PatchTemplate::new(graph), Err(PatchError::VoiceInput));

    let mut g = GraphBuilder::new();
    let envelope = g.adsr(Adsr::new(0.01, 0.1, 0.5, 0.2));
    let graph = g.out_mono(envelope).unwrap();
    assert_eq!(PatchTemplate::new(graph), Err(PatchError::NoteClockNode));
}

#[test]
fn a_persistent_patch_can_process_host_audio_inputs() {
    let mut layout = ControlLayout::new();
    let gain = layout.add(ControlSpec::new("gain", 0.0, 2.0, 0.5)).unwrap();
    let store = ControlStore::new(&layout);

    let mut g = GraphBuilder::with_inputs(2);
    let left = g.input(0);
    let right = g.input(1);
    let control = g.control(gain);
    let left = g.mul(left, control);
    let right = g.mul(right, control);
    let graph = g.out(&[left, right]).unwrap();
    let patch = PatchTemplate::new(graph).unwrap();
    let mut unit = instantiate_patch(&patch, &store).unwrap();
    unit.set_sample_rate(SR);
    unit.allocate();

    let mut output = [0.0; 2];
    unit.tick(&[0.8, -0.4], &mut output);
    assert!((output[0] - 0.4).abs() < 1.0e-6);
    assert!((output[1] + 0.2).abs() < 1.0e-6);

    store.set(gain, 1.5).unwrap();
    unit.tick(&[0.8, -0.4], &mut output);
    assert!((output[0] - 1.2).abs() < 1.0e-6);
    assert!((output[1] + 0.6).abs() < 1.0e-6);
}

#[test]
fn a_persistent_delay_retains_real_audio_history() {
    let controls = ControlLayout::new();
    let store = ControlStore::new(&controls);
    let mut graph = GraphBuilder::with_inputs(1);
    let delayed = graph.delay(
        graph.input(0),
        3.0 / SR,
        DelayRange::fixed(3.0 / SR).unwrap(),
    );
    let graph = graph.out_mono(delayed).unwrap();
    let patch = PatchTemplate::new(graph).unwrap();
    let mut unit = instantiate_patch(&patch, &store).unwrap();
    unit.set_sample_rate(SR);
    unit.allocate();

    let mut output = [0.0];
    unit.tick(&[1.0], &mut output);
    assert!(output[0].abs() < 1.0e-6);
    unit.tick(&[0.0], &mut output);
    assert!(output[0].abs() < 1.0e-6);
    unit.tick(&[0.0], &mut output);
    assert!(output[0].abs() < 1.0e-6);
    unit.tick(&[0.0], &mut output);
    assert!(
        (output[0] - 1.0).abs() < 1.0e-6,
        "delayed history was not retained: {}",
        output[0]
    );
}

#[test]
fn an_out_of_range_graph_input_is_rejected_before_lowering() {
    let mut g = GraphBuilder::with_inputs(1);
    let missing = g.input(1);
    assert_eq!(
        g.out_mono(missing),
        Err(apteronotus_synth::TemplateError::BadInput { channel: 1 })
    );
}

#[test]
fn routed_voice_stems_feed_a_persistent_bus_patch() {
    use apteronotus_synth::{BusLayout, EventRouting, instantiate_routed};

    let mut buses = BusLayout::new(1).unwrap();
    let wet_bus = buses.add_bus(1).unwrap();
    let mut voice_builder = GraphBuilder::new();
    let oscillator = voice_builder.sine(n::HZ);
    voice_builder.send(wet_bus, &[oscillator], 0.25);
    let voice = voice_builder.out_mono(oscillator).unwrap();
    let mut voice =
        instantiate_routed(&voice, &Note::new(440.0), &buses, &EventRouting::new()).unwrap();

    let controls = ControlLayout::new();
    let store = ControlStore::new(&controls);
    let mut rack = GraphBuilder::with_inputs(buses.total_channels());
    let dry = rack.input(0);
    let wet = rack.input(1);
    let mixed = rack.add(dry, wet);
    let rack = PatchTemplate::new(rack.out_mono(mixed).unwrap()).unwrap();
    let mut rack = instantiate_patch(&rack, &store).unwrap();

    voice.set_sample_rate(SR);
    voice.allocate();
    rack.set_sample_rate(SR);
    rack.allocate();
    let mut stems = [0.0; 2];
    let mut output = [0.0; 1];
    for _ in 0..100 {
        voice.tick(&[], &mut stems);
        rack.tick(&stems, &mut output);
        assert!((output[0] - stems[0] * 1.25).abs() < 1.0e-6);
    }
}
