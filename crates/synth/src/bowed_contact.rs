//! Explicit physical calibration for the experimental bow contact.
//! This supplies units, not measured properties of a particular instrument.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BowContactCalibration {
    /// Transverse characteristic impedance sqrt(tension * linear density), N s/m.
    pub impedance: f64,
    /// Physical velocity represented by a unit traveling-wave sample, m/s.
    pub velocity_unit: f64,
    /// Normal bow force represented by controls.force = 1, N.
    pub normal_force_unit: f64,
    pub static_friction: f64,
    pub sliding_friction: f64,
    /// Hyperbolic sliding-law knee in m/s, not a thermal time constant.
    pub slip_knee: f64,
}
impl BowContactCalibration {
    pub fn valid(self) -> bool {
        [
            self.impedance,
            self.velocity_unit,
            self.normal_force_unit,
            self.static_friction,
            self.slip_knee,
        ]
        .into_iter()
        .all(|x| x.is_finite() && x > 0. && x <= 100.)
            && self.sliding_friction.is_finite()
            && self.sliding_friction >= 0.
            && self.sliding_friction <= self.static_friction
            && self.impedance >= 0.001
            && self.velocity_unit >= 0.001
            && self.slip_knee >= 0.000001
    }
    /// Velocity injection F/(2 Z V_unit) on each side of a point bow contact.
    pub fn normalized_limits(self, normal_control: f64) -> (f64, f64, f64) {
        let scale =
            self.normal_force_unit * normal_control / (2. * self.impedance * self.velocity_unit);
        (
            self.static_friction * scale,
            self.sliding_friction * scale,
            self.slip_knee / self.velocity_unit,
        )
    }
    pub fn physical_force(self, injection: f64) -> f64 {
        2. * self.impedance * self.velocity_unit * injection
    }
}

/// Ideal flexible-string impedance from open pitch, tension and open length.
/// Stopping changes length and pitch together, not this characteristic impedance.
pub fn string_impedance(tension_n: f64, length_m: f64, open_hz: f64) -> Result<f64, &'static str> {
    if ![tension_n, length_m, open_hz]
        .into_iter()
        .all(|v| v.is_finite() && v > 0.)
    {
        return Err("positive finite string tension, length and open frequency required");
    }
    let impedance = tension_n / (2. * length_m * open_hz);
    if !impedance.is_finite() || impedance <= 0. {
        return Err("string impedance is outside finite representable range");
    }
    Ok(impedance)
}
