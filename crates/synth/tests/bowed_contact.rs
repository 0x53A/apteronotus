use apteronotus_synth::bowed::{BowedConfig, BowedControls, BowedModel, BowedString};
use apteronotus_synth::bowed_bridge::{BridgeSpec, ModalBridge};
use apteronotus_synth::bowed_contact::{BowContactCalibration, string_impedance};
use fundsp::prelude32::AudioUnit;
fn legacy_equivalent() -> BowContactCalibration {
    BowContactCalibration {
        impedance: 0.5,
        velocity_unit: 1.,
        normal_force_unit: 1.,
        static_friction: 0.3,
        sliding_friction: 0.02,
        slip_knee: 0.04,
    }
}
fn string() -> BowedString {
    BowedString::new(
        BowedConfig {
            model: BowedModel::C4,
            ..Default::default()
        },
        48000.,
    )
    .unwrap()
}
#[test]
fn units_preserve_existing_contact_when_parameters_are_equivalent() {
    let mut original = string();
    let mut calibrated = string();
    calibrated.configure_contact(legacy_equivalent()).unwrap();
    for rate in [48000., 96000., 24000.] {
        original.set_sample_rate(rate);
        calibrated.set_sample_rate(rate);
        for i in 0..12000 {
            let controls = BowedControls {
                hz: 293.6648,
                speed: if i < 6000 { 0.4 } else { -0.3 },
                force: 0.35,
                position: 0.18,
            };
            original.set_controls(controls).unwrap();
            calibrated.set_controls(controls).unwrap();
            assert_eq!(original.next_sample(), calibrated.next_sample());
        }
        original.reset();
        calibrated.reset();
    }
}
#[test]
fn calibration_and_bridge_reject_inconsistent_units_in_either_order() {
    let body = ModalBridge::new(BridgeSpec::experiment(1.).unwrap(), 48000., &[1.]).unwrap();
    let mut a = string();
    a.configure_contact(legacy_equivalent()).unwrap();
    assert!(a.connect_bridge(body.load(0).unwrap()).is_err());
    let mut b = string();
    b.connect_bridge(body.load(0).unwrap()).unwrap();
    assert!(b.configure_contact(legacy_equivalent()).is_err());
    let mut c = string();
    c.next_sample();
    assert!(c.configure_contact(legacy_equivalent()).is_err());
    for value in [f64::NAN, f64::INFINITY, -1., 0.] {
        assert!(string_impedance(value, 0.328, 440.).is_err());
        let mut calibration = legacy_equivalent();
        calibration.impedance = value;
        assert!(string().configure_contact(calibration).is_err());
    }
}
#[test]
fn physical_string_parameters_obey_wave_speed_and_impedance() {
    for hz in [196., 293.6648, 440., 659.2551] {
        let z = string_impedance(50., 0.328, hz).unwrap();
        let wave_speed = 2. * 0.328 * hz;
        let linear_density = 50. / wave_speed.powi(2);
        assert!((z - (50. * linear_density).sqrt()).abs() < 1e-14);
        // Stopping at half length doubles f but does not change tension/density.
        assert_eq!(z, string_impedance(50., 0.164, 2. * hz).unwrap());
    }
}
