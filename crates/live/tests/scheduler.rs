use apteronotus_live::{
    PitchScheduler, ProgramScheduler, RoutedRuntime, ScheduleError, ScheduledTrack, Transport,
    schedule_external_routed,
};
use apteronotus_pattern::{Frac, mini};
use apteronotus_synth::lower::{render, rms, zero_crossing_hz};
use apteronotus_synth::{
    Adsr, BusLayout, ControlLayout, ControlSpec, ControlStore, EventRouting, GraphBuilder,
    GraphTemplate, Implicit, InitControlBinding, ParamId, ParamSpec, n,
};
use fundsp::prelude32::AudioUnit;

const SR: f64 = 48_000.0;

fn voice() -> GraphTemplate {
    let mut graph = GraphBuilder::new();
    let oscillator = graph.sine(n::HZ);
    let filtered = graph.lowpass(oscillator, 4_000.0, 0.7);
    let envelope = graph.adsr(Adsr::new(0.005, 0.03, 0.7, 0.1));
    let shaped = graph.mul(filtered, envelope);
    let scaled = graph.mul(shaped, 0.2);
    graph.out_mono(scaled).unwrap()
}

#[test]
fn four_beats_is_a_default_meter_not_a_cycle_property() {
    let common = Transport::new(96.0).unwrap();
    assert!((common.cycles_per_second() - 0.4).abs() < 1e-12);
    assert!((common.cycle_to_seconds(Frac::ONE) - 2.5).abs() < 1e-12);

    let three_four = Transport::with_meter(96.0, 3.0).unwrap();
    assert!((three_four.cycle_to_seconds(Frac::ONE) - 1.875).abs() < 1e-12);
}

#[test]
fn the_frontier_tiles_time_without_duplicate_onsets() {
    let pattern = mini::parse("c4 e4 g4 c5").unwrap();
    let voice = voice();
    let transport = Transport::new(120.0).unwrap();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(&voice);

    let first = scheduler
        .fill_to(Frac::new(1, 2), &pattern, &voice, transport, &mut sequencer)
        .unwrap();
    let second = scheduler
        .fill_to(Frac::ONE, &pattern, &voice, transport, &mut sequencer)
        .unwrap();
    let repeated = scheduler
        .fill_to(Frac::new(3, 4), &pattern, &voice, transport, &mut sequencer)
        .unwrap();

    assert_eq!(first.voices, 2);
    assert_eq!(second.voices, 2);
    assert_eq!(repeated.voices, 0);
    assert_eq!(scheduler.frontier(), Frac::ONE);
}

#[test]
fn a_scheduling_failure_does_not_advance_or_publish() {
    let pattern = mini::parse("not_a_note").unwrap();
    let voice = voice();
    let transport = Transport::default();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(&voice);

    assert!(matches!(
        scheduler.fill_to(Frac::ONE, &pattern, &voice, transport, &mut sequencer),
        Err(ScheduleError::Pitch { .. })
    ));
    assert_eq!(scheduler.frontier(), Frac::ZERO);
    assert_eq!(sequencer.time(), Some(0.0));
}

#[test]
fn fixed_frequency_voices_accept_nonpitch_trigger_labels() {
    let pattern = mini::parse("bd ~ x ~").unwrap();
    let mut graph = GraphBuilder::new();
    let oscillator = graph.sine(90.0);
    let voice = graph.out_mono(oscillator).unwrap();
    let transport = Transport::default();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(&voice);

    let report = scheduler
        .fill_to(Frac::ONE, &pattern, &voice, transport, &mut sequencer)
        .unwrap();

    assert_eq!(report.voices, 2);
}

#[test]
fn pattern_through_scheduler_and_synth_makes_first_sound() {
    let pattern = mini::parse("c4").unwrap();
    let voice = voice();
    let transport = Transport::new(120.0).unwrap();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(&voice);
    sequencer.set_sample_rate(SR);

    let report = scheduler
        .fill_to(Frac::ONE, &pattern, &voice, transport, &mut sequencer)
        .unwrap();
    assert_eq!(report.voices, 1);

    let audio = render(&mut sequencer, SR, 0.5);
    assert!(rms(&audio[0]) > 0.01);
    let measured = zero_crossing_hz(&audio[0][2_000..20_000], SR);
    assert!((measured - 261.63).abs() < 3.0, "measured {measured} Hz");
}

#[test]
fn pitch_slew_begins_at_the_preceding_track_onset_and_reaches_its_target() {
    let pattern = mini::parse("a3 a4").unwrap();
    let mut graph = GraphBuilder::new();
    let glide = graph.param(ParamSpec::new("glide", 0.0, 0.5, 0.2).with_unit("s"));
    let hz = graph.slew(n::HZ, glide).unwrap();
    let oscillator = graph.sine(hz);
    let voice = graph.out_mono(oscillator).unwrap();
    let transport = Transport::new(120.0).unwrap();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(&voice);
    sequencer.set_sample_rate(SR);

    scheduler
        .fill_to(Frac::ONE, &pattern, &voice, transport, &mut sequencer)
        .unwrap();
    let audio = render(&mut sequencer, SR, 2.0);
    let boundary = SR as usize;
    let early = zero_crossing_hz(
        &audio[0][boundary + (0.015 * SR) as usize..boundary + (0.075 * SR) as usize],
        SR,
    );
    let settled = zero_crossing_hz(
        &audio[0][boundary + (0.35 * SR) as usize..boundary + (0.75 * SR) as usize],
        SR,
    );

    assert!(
        early < 330.0,
        "the second note jumped to its target instead of gliding: {early} Hz"
    );
    assert!(
        (settled - 440.0).abs() < 4.0,
        "the glide never reached its target: {settled} Hz"
    );
}

#[test]
fn a_program_window_is_atomic_across_tracks() {
    let valid = mini::parse("c4 e4").unwrap();
    let invalid = mini::parse("not_a_note").unwrap();
    let voice = voice();
    let transport = Transport::new(120.0).unwrap();
    let mut scheduler = ProgramScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(&voice);
    sequencer.set_sample_rate(SR);

    let error = scheduler
        .fill_to(
            Frac::ONE,
            [
                ScheduledTrack::new(&valid, &voice),
                ScheduledTrack::new(&invalid, &voice),
            ],
            transport,
            &mut sequencer,
        )
        .unwrap_err();
    assert!(matches!(error, ScheduleError::Pitch { .. }));
    assert_eq!(scheduler.frontier(), Frac::ZERO);
    let audio = render(&mut sequencer, SR, 0.25);
    assert!(rms(&audio[0]) < 1.0e-9);

    let report = scheduler
        .fill_to(
            Frac::ONE,
            [
                ScheduledTrack::new(&valid, &voice),
                ScheduledTrack::new(&valid, &voice),
            ],
            transport,
            &mut sequencer,
        )
        .unwrap();
    assert_eq!(report.voices, 4);
}

#[test]
fn routed_onset_bindings_sample_live_controls_into_one_new_voice() {
    let pattern = mini::parse("c4").unwrap();
    let voice = voice();
    let transport = Transport::new(120.0).unwrap();
    let mut controls = ControlLayout::new();
    let hz = controls
        .add(ControlSpec::new("tracked_hz", 65.0, 1_100.0, 330.0))
        .unwrap();
    let velocity = controls
        .add(ControlSpec::new("tracked_amp", 0.0, 1.0, 0.4))
        .unwrap();
    let store = ControlStore::new(&controls);
    let layout = BusLayout::new(2).unwrap();
    let routing = EventRouting::new();
    let bindings = [
        InitControlBinding {
            param: ParamId::Implicit(Implicit::Hz),
            control: hz,
        },
        InitControlBinding {
            param: ParamId::Implicit(Implicit::Velocity),
            control: velocity,
        },
    ];
    let track = ScheduledTrack::with_routing_and_bindings(&pattern, &voice, &routing, &bindings);
    let mut sequencer = fundsp::prelude32::Sequencer::new(
        0,
        layout.total_channels(),
        fundsp::prelude32::ReplayMode::None,
    );
    sequencer.set_sample_rate(SR);
    ProgramScheduler::default()
        .fill_routed_to(
            Frac::ONE,
            [track],
            transport,
            &mut sequencer,
            &layout,
            &store,
        )
        .unwrap();

    let audio = render(&mut sequencer, SR, 0.4);
    let hz = zero_crossing_hz(&audio[0][2_000..18_000], SR);
    assert!((hz - 330.0).abs() < 4.0);
}

#[test]
fn an_external_edge_schedules_without_pattern_lookahead() {
    let voice = voice();
    let mut controls = ControlLayout::new();
    let hz = controls
        .add(ControlSpec::new("tracked_hz", 65.0, 1_100.0, 392.0))
        .unwrap();
    let store = ControlStore::new(&controls);
    let layout = BusLayout::new(2).unwrap();
    let routing = EventRouting::new();
    let bindings = [InitControlBinding {
        param: ParamId::Implicit(Implicit::Hz),
        control: hz,
    }];
    let mut sequencer = fundsp::prelude32::Sequencer::new(
        0,
        layout.total_channels(),
        fundsp::prelude32::ReplayMode::None,
    );
    sequencer.set_sample_rate(SR);
    schedule_external_routed(
        apteronotus_live::ExternalOnset {
            at_seconds: 0.03,
            gate_seconds: 0.01,
            event_seed: 7,
        },
        &voice,
        &routing,
        &bindings,
        &[],
        &mut sequencer,
        RoutedRuntime::new(&layout, &store),
    )
    .unwrap();

    let audio = render(&mut sequencer, SR, 0.25);
    let hz = zero_crossing_hz(&audio[0][2_000..5_500], SR);
    assert!((hz - 392.0).abs() < 5.0, "measured {hz}");
}
