use apteronotus_synth::bowed_bridge::{BridgeMode, BridgeSpec, ModalBridge, PolarizedBridge};

fn specs() -> [BridgeSpec; 2] {
    [275., 550.].map(|hz| {
        BridgeSpec::new(&[BridgeMode {
            hz,
            q: 30.,
            mobility: 0.02,
        }])
        .unwrap()
    })
}

#[test]
fn all_transverse_port_work_is_accounted_for_at_rotated_bridge() {
    for rate in [24000., 48000., 96000., 192000.] {
        for angle in [0., 0.37, 1.2] {
            let impedances = [0.39, 0.26, 0.17, 0.12];
            let mut body = PolarizedBridge::new(specs(), angle, rate, &impedances).unwrap();
            for sample in 0..12000 {
                let incoming = std::array::from_fn(|port| {
                    std::array::from_fn(|axis| {
                        if sample >= 6000 {
                            0.
                        } else {
                            0.2 * (sample as f64
                                * (0.017 + port as f64 * 0.009 + axis as f64 * 0.011))
                                .sin()
                        }
                    })
                });
                let before = body.energy();
                let frame = body.step(incoming);
                let mut work = 0.;
                for port in 0..4 {
                    for axis in 0..2 {
                        let a = incoming[port][axis];
                        let b = frame.velocity[axis] - a;
                        work += impedances[port] * (a * a - b * b) / rate;
                    }
                }
                assert!((body.energy() - before + frame.dissipated_energy - work).abs() < 1e-12);
                assert!(frame.dissipated_energy >= 0.);
                if sample >= 6000 {
                    assert!(body.energy() <= before + 1e-16);
                }
            }
            body.reset();
            assert_eq!(body.energy(), 0.);
            assert_eq!(body.step([[0.; 2]; 4]).velocity, [0.; 2]);
        }
    }
}

#[test]
fn isotropic_body_is_orientation_invariant_and_matches_scalar_bridge() {
    let spec = specs()[0];
    let mut scalar = ModalBridge::new(spec, 48000., &[0.26]).unwrap();
    let mut vector = PolarizedBridge::new([spec; 2], 0.71, 48000., &[0.26]).unwrap();
    for i in 0..10000 {
        let a = (i as f64 * 0.039).sin();
        let expected = scalar.step([a, 0., 0., 0.]);
        let actual = vector.step([[a, 0.], [0.; 2], [0.; 2], [0.; 2]]);
        assert!((actual.velocity[0] - expected.velocity).abs() < 1e-12);
        assert!(actual.velocity[1].abs() < 1e-12);
    }
}

#[test]
fn anisotropic_cross_axis_response_is_reciprocal() {
    let mut x = PolarizedBridge::new(specs(), 0.53, 48000., &[0.26]).unwrap();
    let mut y = x.clone();
    let mut coupling = 0.;
    for i in 0..10000 {
        let a = if i == 0 { 1. } else { 0. };
        let from_x = x.step([[a, 0.], [0.; 2], [0.; 2], [0.; 2]]);
        let from_y = y.step([[0., a], [0.; 2], [0.; 2], [0.; 2]]);
        assert!((from_x.velocity[1] - from_y.velocity[0]).abs() < 1e-14);
        coupling += from_x.velocity[1].abs();
    }
    assert!(coupling > 1e-3);
}

#[test]
fn invalid_orientations_rates_and_impedances_are_rejected() {
    for angle in [f64::NAN, f64::INFINITY] {
        assert!(PolarizedBridge::new(specs(), angle, 48000., &[0.26]).is_err());
    }
    assert!(PolarizedBridge::new(specs(), 0., 8000., &[0.26]).is_err());
    assert!(PolarizedBridge::new(specs(), 0., 48000., &[]).is_err());
    assert!(PolarizedBridge::new(specs(), 0., 48000., &[-0.26]).is_err());
}

#[test]
fn uncompensated_two_plane_load_retains_tuning_across_rate_replacement() {
    use apteronotus_synth::bowed::{BowedConfig, BowedControls, BowedModel, BowedString};
    use fundsp::prelude32::AudioUnit;
    let load = PolarizedBridge::new(specs(), 0.53, 48000., &[0.26])
        .unwrap()
        .load(0)
        .unwrap();
    assert_eq!(load.spec, None);
    assert_eq!(load.phase_delay(294.8, 48000.), 0.);
    let make = |rate| {
        let mut s = BowedString::new(
            BowedConfig {
                model: BowedModel::C4,
                ..Default::default()
            },
            rate,
        )
        .unwrap();
        s.connect_bridge(load).unwrap();
        s.set_controls(BowedControls {
            hz: 294.8,
            speed: 0.4,
            force: 0.3,
            position: 0.18,
        })
        .unwrap();
        s
    };
    let mut changed = make(48000.);
    changed.set_sample_rate(96000.);
    let mut fresh = make(96000.);
    for i in 0..6000 {
        let velocity = 0.001 * (i as f64 * 0.03).sin();
        assert_eq!(
            changed.next_sample_at_bridge(velocity),
            fresh.next_sample_at_bridge(velocity)
        );
    }
    changed.reset();
    fresh.reset();
    assert_eq!(
        changed.next_sample_at_bridge(0.),
        fresh.next_sample_at_bridge(0.)
    );
}
