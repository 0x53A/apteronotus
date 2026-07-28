use apteronotus_live::{PitchScheduler, ScheduleError, Transport};
use apteronotus_pattern::{Frac, mini};
use apteronotus_synth::lower::{render, rms, zero_crossing_hz};
use apteronotus_synth::{Adsr, GraphBuilder, GraphTemplate, n};
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
