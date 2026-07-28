use apteronotus_live::{TempoMap, TempoMapError, TempoPoint};
use apteronotus_pattern::{Frac, Span};

fn f(n: i64, d: i64) -> Frac {
    Frac::new(n, d)
}

#[test]
fn a_one_point_map_is_the_constant_transport() {
    let tempo = TempoMap::constant(60.0, 4.0).unwrap();
    assert_eq!(tempo.bpm_at(Frac::int(-10)), 60.0);
    assert_eq!(tempo.bpm_at(Frac::int(10)), 60.0);
    assert!((tempo.cycle_to_seconds(Frac::int(3)) - 12.0).abs() < 1e-12);
}

#[test]
fn a_missing_over_is_a_step() {
    let tempo = TempoMap::new(
        4.0,
        vec![
            TempoPoint::step(Frac::ZERO, 60.0),
            TempoPoint::step(Frac::int(2), 120.0),
        ],
    )
    .unwrap();

    assert_eq!(tempo.bpm_at(f(3, 2)), 60.0);
    assert_eq!(tempo.bpm_at(Frac::int(2)), 120.0);
    // Two 4-second cycles, then one 2-second cycle.
    assert!((tempo.cycle_to_seconds(Frac::int(3)) - 10.0).abs() < 1e-12);
}

#[test]
fn over_arrives_at_the_declared_tempo_and_position() {
    let tempo = TempoMap::new(
        4.0,
        vec![
            TempoPoint::step(Frac::ZERO, 60.0),
            TempoPoint::ramp(Frac::int(2), 120.0, Frac::ONE),
        ],
    )
    .unwrap();

    assert_eq!(tempo.bpm_at(Frac::ONE), 60.0);
    assert!((tempo.bpm_at(f(3, 2)) - 90.0).abs() < 1e-12);
    assert_eq!(tempo.bpm_at(Frac::int(2)), 120.0);

    // One constant cycle plus ∫₁² 240 / (60 + 60(x-1)) dx.
    let expected = 4.0 + 4.0 * 2.0f64.ln();
    assert!((tempo.cycle_to_seconds(Frac::int(2)) - expected).abs() < 1e-12);
}

#[test]
fn note_duration_integrates_across_tempo_changes() {
    let tempo = TempoMap::new(
        4.0,
        vec![
            TempoPoint::step(Frac::ZERO, 60.0),
            TempoPoint::step(Frac::ONE, 120.0),
        ],
    )
    .unwrap();
    let duration = tempo.span_to_seconds(Span::new(f(1, 2), f(3, 2)));
    assert!((duration - 3.0).abs() < 1e-12);
}

#[test]
fn seconds_and_cycles_round_trip_on_both_sides_of_zero() {
    let tempo = TempoMap::new(
        4.0,
        vec![
            TempoPoint::step(Frac::ZERO, 60.0),
            TempoPoint::ramp(Frac::int(2), 100.0, Frac::ONE),
            TempoPoint::step(Frac::int(4), 80.0),
        ],
    )
    .unwrap();

    for cycle in [f(-3, 2), f(1, 3), f(7, 4), f(9, 2)] {
        let seconds = tempo.cycle_to_seconds(cycle);
        let round_trip = tempo.seconds_to_cycle(seconds).unwrap();
        assert!(
            (round_trip.to_f64() - cycle.to_f64()).abs() < 1.0e-6,
            "{cycle} became {round_trip}"
        );
    }
}

#[test]
fn malformed_maps_are_diagnostics() {
    assert_eq!(TempoMap::new(4.0, vec![]), Err(TempoMapError::Empty));
    assert!(matches!(
        TempoMap::new(
            4.0,
            vec![
                TempoPoint::step(Frac::ZERO, 60.0),
                TempoPoint::ramp(Frac::ONE, 80.0, Frac::int(2)),
            ],
        ),
        Err(TempoMapError::OverlappingRamp { index: 1 })
    ));
}
