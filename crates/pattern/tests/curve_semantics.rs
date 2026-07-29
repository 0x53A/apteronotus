use apteronotus_pattern::{Basis, Curve, CurveActivity, CurveClock, CurveError, RangeProof};

#[test]
fn window_has_an_exact_unipolar_range() {
    let window = Curve::window(CurveClock::NoteSeconds, 0.2, 0.8);
    assert!(matches!(
        window.prove_range(0.0, 1.0).unwrap(),
        RangeProof::Safe { enclosure }
            if enclosure.min == 0.0 && enclosure.max == 1.0
    ));
}

#[test]
fn note_phase_range_uses_only_the_reachable_domain() {
    let partial = Curve::new(CurveClock::NotePhase, 0.0).term(Basis::Ramp, 1.0, 0.8, 0.5);
    assert!(matches!(
        partial.prove_range(0.0, 0.4).unwrap(),
        RangeProof::Safe { enclosure } if (enclosure.max - 0.4).abs() < 1.0e-12
    ));
    assert!((partial.at(4.0, 2.0).unwrap() - 0.4).abs() < 1.0e-12);
}

#[test]
fn note_phase_reports_unreachable_terms_but_keeps_phase_one_reachable() {
    let unreachable = Curve::new(CurveClock::NotePhase, 0.0).term(Basis::Step, 1.0, 1.01, 0.0);
    assert_eq!(
        unreachable.validate(),
        Err(CurveError::UnreachablePhaseTerm { index: 0 })
    );

    let at_release = Curve::new(CurveClock::NotePhase, 0.0).term(Basis::Step, 1.0, 1.0, 0.0);
    assert_eq!(at_release.at(1.0, 1.0).unwrap(), 1.0);
    assert_eq!(at_release.at(2.0, 1.0).unwrap(), 1.0);
}

#[test]
fn finite_activity_is_distinct_from_gate_bounded_sources() {
    assert_eq!(
        Curve::window(CurveClock::NoteSeconds, 0.0, 2.0)
            .activity()
            .unwrap(),
        CurveActivity::Finite(2.0)
    );
    assert_eq!(
        Curve::new(CurveClock::NoteSeconds, 0.0)
            .term(Basis::Step, 1.0, 0.0, 0.0)
            .activity()
            .unwrap(),
        CurveActivity::GateBounded
    );
    assert_eq!(
        Curve::new(CurveClock::NoteSeconds, 0.0)
            .term(Basis::Sine, 1.0, 0.0, 1.0)
            .activity()
            .unwrap(),
        CurveActivity::GateBounded
    );
}

#[test]
fn rounded_breakpoint_differences_still_reach_a_finite_horizon() {
    // Successive differences for levels 0.0 -> 0.1 -> 0.3 -> 0.0.
    // In binary floating point, 0.1 + 0.2 - 0.3 is not exactly zero.
    let envelope = Curve::new(CurveClock::NoteSeconds, 0.0)
        .term(Basis::Ramp, 0.1, 0.0, 1.0)
        .term(Basis::Ramp, 0.2, 1.0, 1.0)
        .term(Basis::Ramp, -0.3, 2.0, 1.0);
    assert_eq!(envelope.activity().unwrap(), CurveActivity::Finite(3.0));

    let genuinely_nonzero = envelope.term(Basis::Step, 1.0e-12, 3.0, 0.0);
    assert_eq!(
        genuinely_nonzero.activity().unwrap(),
        CurveActivity::GateBounded
    );
}

#[test]
fn nonlinear_cancellation_is_not_falsely_called_out_of_range() {
    let curve = Curve::new(CurveClock::NoteSeconds, 0.0)
        .term(Basis::Decay, 1.0, 0.0, 1.0)
        .term(Basis::Decay, -1.0, 0.0, 1.0);
    assert!(matches!(
        curve.prove_range(0.0, 0.0).unwrap(),
        RangeProof::Inconclusive { .. }
    ));
}
