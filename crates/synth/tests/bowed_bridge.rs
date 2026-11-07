use apteronotus_synth::bowed_bridge::{BridgeMode, BridgeSpec, ModalBridge};
use std::f64::consts::TAU;

#[test]
fn modal_bridge_accounts_for_all_port_work_and_only_dissipates_energy() {
    for rate in [24000., 48000., 96000.] {
        for ports in [&[1.][..], &[0.8, 1.1, 0.7, 0.5][..]] {
            let mut body =
                ModalBridge::new(BridgeSpec::experiment(1.).unwrap(), rate, ports).unwrap();
            for sample in 0..30000 {
                let incoming = std::array::from_fn(|i| {
                    if sample < 12000 {
                        0.2 * (TAU * (173. + 87. * i as f64) * sample as f64 / rate).sin()
                    } else {
                        0.
                    }
                });
                let before = body.energy();
                let frame = body.step(incoming);
                let work = ports
                    .iter()
                    .enumerate()
                    .map(|(i, z)| {
                        let outgoing = frame.velocity - incoming[i];
                        z * (incoming[i] * incoming[i] - outgoing * outgoing) / rate
                    })
                    .sum::<f64>();
                let after = body.energy();
                assert!(
                    (after - before + frame.dissipated_energy - work).abs() < 1e-12,
                    "energy accounting failed: {rate} {sample}"
                );
                assert!(frame.dissipated_energy >= 0. && after >= 0.);
                if sample >= 12000 {
                    assert!(after <= before + 1e-16);
                }
            }
            body.reset();
            assert_eq!(body.energy(), 0.);
            assert_eq!(body.step([0.; 4]).velocity, 0.);
        }
    }
}

#[test]
fn measured_loaded_reflection_phase_matches_the_tuning_formula() {
    let spec = BridgeSpec::experiment(1.).unwrap();
    for rate in [24000., 48000.] {
        for period in [32, 64, 96, 128] {
            let mut body = ModalBridge::new(spec, rate, &[1., 1., 1., 1.]).unwrap();
            let mut re = 0.;
            let mut im = 0.;
            for i in 0..period * 512 {
                let phase = TAU * i as f64 / period as f64;
                let incident = phase.cos();
                let frame = body.step([incident, 0., 0., 0.]);
                if i >= period * 256 {
                    let negative_reflection = incident - frame.velocity;
                    re += negative_reflection * phase.cos();
                    im -= negative_reflection * phase.sin();
                }
            }
            let measured = im.atan2(re);
            let expected = spec.reflection_phase(rate / period as f64, rate, 1., 4.);
            assert!(
                (measured - expected).abs() < 1e-5,
                "{rate} {period}: {measured} != {expected}"
            );
        }
    }
}

#[test]
fn invalid_modal_parameters_and_ports_are_rejected_before_rendering() {
    assert!(BridgeSpec::new(&[]).is_err());
    for mode in [
        BridgeMode {
            hz: 0.,
            q: 30.,
            mobility: 0.02,
        },
        BridgeMode {
            hz: 400.,
            q: f64::NAN,
            mobility: 0.02,
        },
        BridgeMode {
            hz: 400.,
            q: 30.,
            mobility: -0.02,
        },
    ] {
        assert!(BridgeSpec::new(&[mode]).is_err());
    }
    let spec = BridgeSpec::experiment(1.).unwrap();
    for ports in [
        &[][..],
        &[0.][..],
        &[f64::NAN][..],
        &[1., 1., 1., 1., 1.][..],
    ] {
        assert!(ModalBridge::new(spec, 48000., ports).is_err());
    }
}

#[test]
fn coupled_strings_transfer_energy_retain_history_and_reset_deterministically() {
    use apteronotus_synth::bowed::{BowedConfig, BowedControls, BowedModel, BowedString};
    use fundsp::prelude32::AudioUnit;
    for rate in [24000., 48000.] {
        let mut body =
            ModalBridge::new(BridgeSpec::experiment(1.).unwrap(), rate, &[1.; 4]).unwrap();
        let mut strings: Vec<_> = [196., 294.8366557, 440., 659.255114]
            .iter()
            .enumerate()
            .map(|(i, hz)| {
                let mut s = BowedString::new(
                    BowedConfig {
                        model: BowedModel::C4,
                        min_hz: 195.,
                        max_hz: 700.,
                        ..Default::default()
                    },
                    rate,
                )
                .unwrap();
                s.set_controls(BowedControls {
                    hz: *hz,
                    speed: if i == 1 { 0.4 } else { 0. },
                    force: if i == 1 { 0.35 } else { 0. },
                    position: 0.24,
                })
                .unwrap();
                s.connect_bridge(body.load(i).unwrap()).unwrap();
                s
            })
            .collect();
        let step = |body: &mut ModalBridge, strings: &mut Vec<BowedString>| {
            let incoming = std::array::from_fn(|i| strings[i].bridge_incident() as f64);
            let frame = body.step(incoming);
            for s in strings {
                s.next_sample_at_bridge(frame.velocity);
            }
            (frame.velocity, incoming)
        };
        let mut first = Vec::new();
        let mut sympathetic = 0.;
        for i in 0..rate as usize {
            let (velocity, incoming) = step(&mut body, &mut strings);
            first.push(velocity);
            if i > rate as usize / 2 {
                sympathetic += incoming[0] * incoming[0]
                    + incoming[2] * incoming[2]
                    + incoming[3] * incoming[3];
            }
        }
        assert!(
            sympathetic > 1e-8,
            "passive strings must respond through the body"
        );
        assert!(
            strings[1].connect_bridge(body.load(1).unwrap()).is_err(),
            "reconfiguring a sounding boundary requires a new instance"
        );
        let mut copied_body = body.clone();
        let mut copied_strings = strings.clone();
        for _ in 0..2048 {
            assert_eq!(
                step(&mut body, &mut strings),
                step(&mut copied_body, &mut copied_strings)
            );
        }
        body.reset();
        for s in &mut strings {
            s.reset();
        }
        for expected in first {
            assert_eq!(expected, step(&mut body, &mut strings).0);
        }
        for s in &strings {
            assert_eq!(s.diagnostics().emergency_resets, 0);
        }
        // A detached coupled system must dissipate stored energy without a fade.
        for s in &mut strings {
            s.detach();
        }
        let mut final_rms = 0.;
        for i in 0..(rate * 10.) as usize {
            let (v, _) = step(&mut body, &mut strings);
            if i >= (rate * 9.) as usize {
                final_rms += v * v / rate;
            }
        }
        assert!(
            final_rms.sqrt() < 1e-7,
            "coupled system must drain naturally: {final_rms}"
        );
    }
}

#[test]
fn finger_damping_preserves_the_reciprocal_bridge_port_identity() {
    use apteronotus_synth::bowed::{BowedConfig, BowedControls, BowedModel, BowedString};
    let mut body = ModalBridge::new(BridgeSpec::experiment(1.).unwrap(), 48000., &[1.]).unwrap();
    let mut string = BowedString::new(
        BowedConfig {
            model: BowedModel::C4,
            ..Default::default()
        },
        48000.,
    )
    .unwrap();
    string
        .set_controls(BowedControls {
            speed: 0.4,
            force: 0.3,
            position: 0.24,
            ..Default::default()
        })
        .unwrap();
    string.connect_bridge(body.load(0).unwrap()).unwrap();
    for i in 0..12000 {
        string
            .set_damping(if i < 6000 { 0.0 } else { 0.65 })
            .unwrap();
        let incoming = string.bridge_incident();
        let frame = body.step([incoming as f64, 0., 0., 0.]);
        let force = string.next_sample_at_bridge(frame.velocity);
        assert!((force as f64 - (2. * incoming as f64 - frame.velocity)).abs() < 1e-7);
    }
    assert_eq!(string.diagnostics().emergency_resets, 0);
}

#[test]
fn measured_body_capacity_is_bounded_and_all_modes_conserve_port_work() {
    use apteronotus_synth::bowed_bridge::MAX_BRIDGE_MODES;
    let modes: Vec<_> = (0..MAX_BRIDGE_MODES)
        .map(|i| BridgeMode {
            hz: 200. * 40f64.powf(i as f64 / (MAX_BRIDGE_MODES - 1) as f64),
            q: 10. + i as f64,
            mobility: 0.02,
        })
        .collect();
    let spec = BridgeSpec::new(&modes).unwrap();
    let mut oversized = modes.clone();
    oversized.push(modes[0]);
    assert!(BridgeSpec::new(&oversized).is_err());
    for rate in [24000., 96000.] {
        let mut body = ModalBridge::new(spec, rate, &[0.26]).unwrap();
        for i in 0..10000 {
            let a = if i < 5000 {
                (i as f64 * 0.093).sin() * 0.1
            } else {
                0.
            };
            let before = body.energy();
            let frame = body.step([a, 0., 0., 0.]);
            let b = frame.velocity - a;
            let work = 0.26 * (a * a - b * b) / rate;
            assert!((body.energy() - before + frame.dissipated_energy - work).abs() < 1e-12);
        }
    }
}
