use apteronotus_synth::bowed_gesture::{
    BowAction as A, BowCommand, BowContact, BowedPerformance, c2_config,
};
use fundsp::prelude32::AudioUnit;

fn command(sample: u64, action: A) -> BowCommand {
    BowCommand {
        sample,
        ordinal: sample,
        action,
        beats: None,
    }
}
fn bow() -> A {
    A::Bow {
        string: Some(0),
        speed: Some(0.4),
        force: Some(0.55),
        position: Some(0.12),
        over: 0,
    }
}
fn performance(commands: Vec<BowCommand>) -> BowedPerformance {
    BowedPerformance::new(c2_config(), &[293.6648], commands, 48_000.0).unwrap()
}
fn advance(p: &mut BowedPerformance, to: u64) {
    while p.sample_position() < to {
        p.next_frame();
    }
}

#[test]
fn vibrato_depth_fades_and_interruption_preserves_pitch_and_phase() {
    let mut p = performance(vec![]);
    p.apply_now(A::Stop {
        string: 0,
        hz: 440.0,
        over: 0,
    })
    .unwrap();
    p.apply_now(A::Vibrato {
        string: 0,
        cents: 20.0,
        rate: 5.0,
        over: 48_000,
    })
    .unwrap();
    advance(&mut p, 2400);
    let hz = p.state().controls(2400, 48_000.0, 0).hz;
    // 50 ms: first sine maximum, 5% of the depth fade = one cent.
    assert!((1200.0 * (hz / 440.0).log2() - 1.0).abs() < 1e-8);
    advance(&mut p, 12_000);
    let before = p.state().controls(12_000, 48_000.0, 0).hz;
    p.apply_now(A::Vibrato {
        string: 0,
        cents: 0.0,
        rate: 7.0,
        over: 12_000,
    })
    .unwrap();
    assert_eq!(p.state().controls(12_000, 48_000.0, 0).hz, before);
    advance(&mut p, 18_000);
    let cents = 1200.0 * (p.state().controls(18_000, 48_000.0, 0).hz / 440.0).log2();
    let expected = 2.5 * (std::f64::consts::TAU * (5.0 * 0.25 + 7.0 * 0.125)).sin();
    assert!((cents - expected).abs() < 1e-8);
    advance(&mut p, 24_000);
    assert!((p.state().controls(24_000, 48_000.0, 0).hz - 440.0).abs() < 1e-8);
}

#[test]
fn vibrato_publication_checks_the_entire_depth_fade_and_live_rejection_is_atomic() {
    let mut p = performance(vec![]);
    p.apply_now(A::Stop {
        string: 0,
        hz: 659.0,
        over: 0,
    })
    .unwrap();
    let mut unchanged = p.clone();
    assert!(
        p.apply_now(A::Vibrato {
            string: 0,
            cents: 20.0,
            rate: 6.0,
            over: 48_000
        })
        .is_err()
    );
    for _ in 0..1000 {
        assert_eq!(p.next_frame(), unchanged.next_frame());
    }
    assert_eq!(p.rejected_commands, 1);
    p.apply_now(A::Stop {
        string: 0,
        hz: 440.0,
        over: 0,
    })
    .unwrap();
    p.apply_now(A::Vibrato {
        string: 0,
        cents: 20.0,
        rate: 6.0,
        over: 48_000,
    })
    .unwrap();
    // The current depth is still zero; its scheduled target must constrain pitch.
    assert!(
        p.apply_now(A::Stop {
            string: 0,
            hz: 659.0,
            over: 0
        })
        .is_err()
    );
}

#[test]
fn renewed_force_cancels_lift_but_speed_alone_does_not() {
    for renew in [true, false] {
        let mut p = performance(vec![
            command(0, bow()),
            command(100, A::Lift { over: 60 }),
            command(
                120,
                A::Bow {
                    string: None,
                    speed: Some(-0.4),
                    force: renew.then_some(0.55),
                    position: None,
                    over: 20,
                },
            ),
        ]);
        advance(&mut p, 161);
        assert_eq!(
            p.state().contact,
            if renew {
                BowContact::Engaged
            } else {
                BowContact::Detached
            }
        );
        p.apply_now(A::Bow {
            string: None,
            speed: Some(0.4),
            force: None,
            position: None,
            over: 0,
        })
        .unwrap();
        assert_eq!(
            p.state().contact,
            if renew {
                BowContact::Engaged
            } else {
                BowContact::Detached
            }
        );
    }
}

#[test]
fn release_cancels_slide_and_vibrato_is_suppressed_while_open() {
    let mut p = performance(vec![
        command(0, bow()),
        command(
            100,
            A::Stop {
                string: 0,
                hz: 440.0,
                over: 500,
            },
        ),
        command(
            200,
            A::Vibrato {
                string: 0,
                cents: 20.0,
                rate: 5.0,
                over: 0,
            },
        ),
        command(
            300,
            A::ReleaseFinger {
                string: 0,
                over: 60,
            },
        ),
    ]);
    advance(&mut p, 1000);
    let controls = p.state().controls(p.sample_position(), 48_000.0, 0);
    assert!((controls.hz - 293.6648).abs() < 1e-8);
}

#[test]
fn expression_changes_never_freeze_or_reset_physical_history() {
    let mut audible = performance(vec![
        command(0, bow()),
        command(
            400,
            A::Stop {
                string: 0,
                hz: 330.0,
                over: 200,
            },
        ),
    ]);
    let mut muted = audible.clone();
    muted
        .apply_now(A::Expression {
            amount: 0.0,
            over: 0,
        })
        .unwrap();
    for _ in 0..2000 {
        let a = audible.next_frame();
        let b = muted.next_frame();
        assert_eq!(a[1], b[1]);
        assert_eq!(b[0], 0.0);
    }
    muted
        .apply_now(A::Expression {
            amount: 1.0,
            over: 0,
        })
        .unwrap();
    for _ in 0..2000 {
        assert_eq!(audible.next_frame(), muted.next_frame());
    }
}

#[test]
fn unchanged_fingering_does_not_add_an_attack_or_dissipate_vibration() {
    let mut a = performance(vec![
        command(0, bow()),
        command(
            1,
            A::Stop {
                string: 0,
                hz: 330.0,
                over: 0,
            },
        ),
    ]);
    let mut b = a.clone();
    advance(&mut a, 2000);
    advance(&mut b, 2000);
    b.apply_now(A::Stop {
        string: 0,
        hz: 330.0,
        over: 0,
    })
    .unwrap();
    for _ in 0..2000 {
        assert_eq!(a.next_frame(), b.next_frame());
    }
}

#[test]
fn invalid_multi_field_changes_are_atomic_and_density_is_bounded() {
    let mut p = performance(vec![command(0, bow())]);
    advance(&mut p, 1);
    let before = p.state().controls(1, 48_000.0, 0);
    assert!(
        p.apply_now(A::Bow {
            string: None,
            speed: Some(-0.4),
            force: Some(2.0),
            position: Some(0.3),
            over: 0
        })
        .is_err()
    );
    assert_eq!(before, p.state().controls(1, 48_000.0, 0));
    assert_eq!(p.rejected_commands, 1);
    let dense = (0..129)
        .map(|ordinal| BowCommand {
            sample: 0,
            ordinal,
            action: bow(),
            beats: None,
        })
        .collect();
    assert!(BowedPerformance::new(c2_config(), &[293.6648], dense, 48_000.0).is_err());
}

#[test]
fn damping_dissipates_history_without_resurrection() {
    let mut p = performance(vec![
        command(0, bow()),
        command(48000, A::Lift { over: 0 }),
        command(
            48000 + 1,
            A::Damp {
                string: 0,
                amount: 1.0,
                over: 0,
            },
        ),
    ]);
    advance(&mut p, 96000);
    p.apply_now(A::Damp {
        string: 0,
        amount: 0.0,
        over: 0,
    })
    .unwrap();
    for _ in 0..4800 {
        assert!(p.next_frame()[1].abs() < 1e-10);
    }
}

#[test]
fn reset_and_repeated_runs_preserve_the_prepared_plan() {
    let mut a = performance(vec![command(0, bow()), command(300, A::Lift { over: 60 })]);
    let reference: Vec<_> = (0..1000).map(|_| a.next_frame()).collect();
    a.reset();
    for sample in reference {
        assert_eq!(sample, a.next_frame());
    }
}
