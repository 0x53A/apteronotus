use apteronotus_synth::bowed::{BowedConfig, BowedControls, BowedModel, BowedString};
use fundsp::prelude32::{AudioUnit, BufferVec};

fn rms(s: &[f32]) -> f64 {
    (s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
}
fn controls() -> BowedControls {
    BowedControls {
        speed: 0.4,
        force: 0.55,
        ..Default::default()
    }
}

#[test]
fn c1_static_deflection_is_bounded_and_detaches_without_guard_events() {
    for rate in [24_000.0, 48_000.0] {
        let mut unit = BowedString::new(BowedConfig::default(), rate).unwrap();
        unit.set_controls(controls()).unwrap();
        let sound: Vec<_> = (0..(rate * 3.0) as usize)
            .map(|_| unit.next_sample())
            .collect();
        assert!(sound.iter().all(|v| v.is_finite()));
        let late = rms(&sound[(rate * 2.0) as usize..]);
        eprintln!(
            "rate {rate}: sustain RMS {late}, peak {}",
            sound.iter().map(|v| v.abs()).fold(0.0_f32, f32::max)
        );
        assert!(late > 0.001, "no driven deflection: {late}");
        unit.detach();
        let tail: Vec<_> = (0..(rate * 8.0) as usize)
            .map(|_| unit.next_sample())
            .collect();
        assert!(rms(&tail[(rate * 7.0) as usize..]) < late * 0.001);
        assert_eq!(unit.diagnostics().emergency_resets, 0);
    }
}

#[test]
fn c2_sustains_ac_vibration_instead_of_only_static_deflection() {
    for rate in [24_000.0, 48_000.0] {
        for hz in [196.0, 293.6648, 440.0, 659.2551] {
            let mut unit = BowedString::new(
                BowedConfig {
                    model: BowedModel::C2,
                    ..Default::default()
                },
                rate,
            )
            .unwrap();
            unit.set_controls(BowedControls { hz, ..controls() })
                .unwrap();
            let sound: Vec<_> = (0..(rate * 3.0) as usize)
                .map(|_| unit.next_sample())
                .collect();
            let sustain = &sound[(rate * 2.0) as usize..];
            let mean = sustain.iter().map(|x| *x as f64).sum::<f64>() / sustain.len() as f64;
            let ac: Vec<_> = sustain.iter().map(|x| *x - mean as f32).collect();
            assert!(
                rms(&ac) > 0.01,
                "only static deflection at {hz} Hz, {rate} Hz rate"
            );
            assert_eq!(unit.diagnostics().emergency_resets, 0);
            unit.detach();
            let tail: Vec<_> = (0..(rate * 8.0) as usize)
                .map(|_| unit.next_sample())
                .collect();
            assert!(rms(&tail[(rate * 7.0) as usize..]) < rms(sustain) * 0.001);
        }
    }
}

#[test]
fn controls_validate_joint_delays_and_reject_atomically() {
    let mut unit = BowedString::new(BowedConfig::default(), 24_000.0).unwrap();
    unit.set_controls(controls()).unwrap();
    for c in [
        BowedControls {
            hz: 660.0,
            position: 0.02,
            ..controls()
        },
        BowedControls {
            speed: f64::NAN,
            ..controls()
        },
        BowedControls {
            force: 1.01,
            ..controls()
        },
    ] {
        assert!(unit.set_controls(c).is_err());
        assert_eq!(unit.controls(), controls());
    }
    assert_eq!(unit.diagnostics().rejected_controls, 3);
}

#[test]
fn tick_block_and_repeated_runs_are_identical() {
    for model in [
        BowedModel::C1,
        BowedModel::C2,
        BowedModel::C3,
        BowedModel::C4,
    ] {
        let mut tick = BowedString::new(
            BowedConfig {
                model,
                ..Default::default()
            },
            48_000.0,
        )
        .unwrap();
        let mut block = tick.clone();
        let mut input = BufferVec::new(4);
        let mut output = BufferVec::new(1);
        for offset in (0..4800).step_by(64) {
            let n = (4800 - offset).min(64);
            let mut expected = Vec::new();
            for i in 0..n {
                let speed = if offset + i < 2400 { 0.4 } else { -0.4 };
                let values = [293.6648, speed, 0.55, 0.12];
                for (ch, value) in values.iter().enumerate() {
                    input.set_f32(ch, i, *value);
                }
                let mut y = [0.0];
                tick.tick(&values, &mut y);
                expected.push(y[0]);
            }
            block.process(n, &input.buffer_ref(), &mut output.buffer_mut());
            for (i, value) in expected.iter().enumerate() {
                assert_eq!(*value, output.at_f32(0, i));
            }
        }
        assert_eq!(tick.diagnostics(), block.diagnostics());
    }
}

#[test]
fn supported_domain_transitions_are_finite_without_hidden_guard_recovery() {
    for model in [BowedModel::C2, BowedModel::C3, BowedModel::C4] {
        for rate in [24_000.0, 48_000.0] {
            let mut s = BowedString::new(
                BowedConfig {
                    model,
                    ..Default::default()
                },
                rate,
            )
            .unwrap();
            for hz in [196.0, 293.6648, 440.0, 659.2551] {
                for position in [0.12, 0.3, 0.45] {
                    for speed in [-1.0, -0.4, 0.0, 0.4, 1.0] {
                        for force in [0.0, 0.025, 0.3, 0.55, 1.0] {
                            s.set_controls(BowedControls {
                                hz,
                                position,
                                speed,
                                force,
                            })
                            .unwrap();
                            for _ in 0..2048 {
                                assert!(s.next_sample().is_finite());
                            }
                        }
                    }
                }
            }
            assert_eq!(s.diagnostics().emergency_resets, 0);
            assert_eq!(s.diagnostics().rejected_controls, 0);
        }
    }
}

#[test]
fn c3_recovers_sustained_vibration_after_a_slow_bow_reversal() {
    fn ac_rms(samples: &[f32]) -> f64 {
        let mean = samples.iter().map(|x| *x as f64).sum::<f64>() / samples.len() as f64;
        (samples
            .iter()
            .map(|x| (*x as f64 - mean).powi(2))
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt()
    }
    // This is the audible C2 regression: most nominal fixtures settle toward
    // static deflection after the 90 ms reversal despite unchanged bow force.
    for model in [BowedModel::C3, BowedModel::C4] {
        for rate in [24_000.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            for (hz, force, position) in [
                (196.0, 0.55, 0.12),
                (293.6648, 0.55, 0.12),
                (440.0, 0.55, 0.12),
                (659.2551, 0.55, 0.12),
                (293.6648, 0.30, 0.18),
                (293.6648, 0.80, 0.12),
                (293.6648, 0.55, 0.30),
            ] {
                let mut string = BowedString::new(
                    BowedConfig {
                        model,
                        ..Default::default()
                    },
                    rate,
                )
                .unwrap();
                let sound: Vec<_> = (0..(rate * 5.0) as usize)
                    .map(|i| {
                        let t = i as f64 / rate;
                        let speed = 0.4 - 0.8 * ((t - 3.0) / 0.09).clamp(0.0, 1.0);
                        string
                            .set_controls(BowedControls {
                                hz,
                                speed,
                                force: force * (t / 0.06).min(1.0),
                                position,
                            })
                            .unwrap();
                        string.next_sample()
                    })
                    .collect();
                let before = ac_rms(&sound[rate as usize..2 * rate as usize]);
                let after = ac_rms(&sound[4 * rate as usize..]);
                assert!(
                    before > 0.05 && after > 0.05,
                    "no sustain: {rate} {hz} {force}: {before} -> {after}"
                );
                assert!(
                    (after / before).log2().abs() < 0.5,
                    "reversal changes sustain >3 dB: {rate} {hz} {force}: {before} -> {after}"
                );
                string.detach();
                let tail: Vec<_> = (0..(rate * 8.0) as usize)
                    .map(|_| string.next_sample())
                    .collect();
                assert!(rms(&tail[7 * rate as usize..]) < after * 0.001);
                assert_eq!(string.diagnostics().emergency_resets, 0);
            }
        }
    }
}

#[test]
fn c3_clone_retains_contact_history_and_reset_repeats_the_trace() {
    fn trace(string: &mut BowedString) -> Vec<f32> {
        (0..24_000)
            .map(|sample| {
                string
                    .set_controls(BowedControls {
                        speed: if sample < 12_000 { 0.4 } else { -0.4 },
                        ..controls()
                    })
                    .unwrap();
                string.next_sample()
            })
            .collect()
    }
    let mut string = BowedString::new(
        BowedConfig {
            model: BowedModel::C3,
            ..Default::default()
        },
        24_000.0,
    )
    .unwrap();
    let first = trace(&mut string);
    let mut retained = string.clone();
    assert_eq!(trace(&mut string), trace(&mut retained));
    string.reset();
    assert_eq!(trace(&mut string), first);
}

#[test]
fn cello_low_strings_sustain_and_damp_at_supported_rates() {
    use apteronotus_synth::bowed::BowedKind;
    for rate in [24000., 48000.] {
        for hz in [
            65.40639132514966,
            97.99885899543733,
            146.8323839587038,
            220.,
        ] {
            let mut string = BowedString::new(
                BowedConfig {
                    kind: BowedKind::Cello,
                    min_hz: 65.,
                    max_hz: 660.,
                    model: BowedModel::C3,
                },
                rate,
            )
            .unwrap();
            string
                .set_controls(BowedControls { hz, ..controls() })
                .unwrap();
            let audio: Vec<_> = (0..(rate * 2.) as usize)
                .map(|_| string.next_sample())
                .collect();
            let sustain = &audio[rate as usize..];
            assert!(rms(sustain) > 0.01);
            // Periodicity at the authored fundamental; catches unstable low-C
            // operation without using an amplitude-only smoke assertion.
            let lag = (rate / hz).round() as usize;
            let mismatch = sustain
                .iter()
                .zip(&sustain[lag..])
                .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
                .sum::<f64>()
                / (sustain.len() - lag) as f64;
            assert!(
                mismatch.sqrt() / rms(sustain) < 0.25,
                "{hz} Hz at {rate}: mismatch {mismatch}"
            );
            string.detach();
            string.set_damping(0.7).unwrap();
            let tail: Vec<_> = (0..rate as usize).map(|_| string.next_sample()).collect();
            assert!(rms(&tail[(rate * 0.8) as usize..]) < 1e-8);
            assert_eq!(string.diagnostics().emergency_resets, 0);
            assert_eq!(string.diagnostics().rejected_controls, 0);
        }
    }
}
