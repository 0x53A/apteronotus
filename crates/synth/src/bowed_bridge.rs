//! Experimental reciprocal string/bridge coupling in normalized mechanical units.
//! Each modal displacement is driven by the same bridge force; modal velocities
//! sum at the string notch. Implicit midpoint integration solves the string load
//! and all modes together, without iteration. This is not an output EQ bank.
use std::f64::consts::{PI, TAU};

pub const MAX_BRIDGE_MODES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BridgeMode {
    pub hz: f64,
    pub q: f64,
    /// Peak velocity/force admittance of this mode. String impedance is explicit.
    pub mobility: f64,
}
const EMPTY_MODE: BridgeMode = BridgeMode {
    hz: 0.,
    q: 0.,
    mobility: 0.,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BridgeSpec {
    modes: [BridgeMode; MAX_BRIDGE_MODES],
    count: usize,
}
impl BridgeSpec {
    pub fn new(modes: &[BridgeMode]) -> Result<Self, &'static str> {
        if modes.is_empty()
            || modes.len() > MAX_BRIDGE_MODES
            || modes.iter().any(|m| {
                !m.hz.is_finite()
                    || !(50.0..=8000.0).contains(&m.hz)
                    || !m.q.is_finite()
                    || !(0.5..=200.0).contains(&m.q)
                    || !m.mobility.is_finite()
                    || !(0.00001..=0.5).contains(&m.mobility)
            })
        {
            return Err("invalid bounded bridge modal specification");
        }
        let mut result = Self {
            modes: [EMPTY_MODE; MAX_BRIDGE_MODES],
            count: modes.len(),
        };
        result.modes[..modes.len()].copy_from_slice(modes);
        Ok(result)
    }
    pub fn modes(&self) -> &[BridgeMode] {
        &self.modes[..self.count]
    }

    /// Synthetic mechanical testbed, not modal identification of the recording.
    /// These parameters establish coupling tests only; they are not a preset
    /// asserted to reproduce a particular acoustic violin.
    pub fn experiment(scale: f64) -> Result<Self, &'static str> {
        let modes = [
            (275., 18., 0.020),
            (470., 35., 0.040),
            (550., 40., 0.050),
            (850., 35., 0.030),
            (1200., 30., 0.020),
            (1700., 25., 0.025),
            (2400., 18., 0.040),
            (3200., 15., 0.025),
        ]
        .map(|(hz, q, mobility)| BridgeMode {
            hz,
            q,
            mobility: mobility * scale,
        });
        Self::new(&modes)
    }
    fn parameters(mode: BridgeMode, rate: f64) -> (f64, f64, f64) {
        // Prewarp each declared modal frequency through the midpoint transform.
        let omega = 2. * rate * (PI * mode.hz / rate).tan();
        let resistance = 1. / mode.mobility;
        let mass = mode.q * resistance / omega;
        (mass, resistance, mass * omega * omega)
    }
    /// Phase of the individual port reflection with other incoming waves zero.
    /// It is a local small-signal tuning approximation, not compensation for
    /// sympathetic return waves or nonlinear bow-induced pitch shifts.
    pub fn reflection_phase(
        &self,
        hz: f64,
        rate: f64,
        impedance: f64,
        total_impedance: f64,
    ) -> f64 {
        let omega = 2. * rate * (PI * hz / rate).tan();
        let (mut re, mut im) = (0., 0.);
        for mode in self.modes() {
            let (mass, resistance, stiffness) = Self::parameters(*mode, rate);
            let reactance = mass * omega - stiffness / omega;
            let den = resistance * resistance + reactance * reactance;
            re += resistance / den;
            im -= reactance / den;
        }
        // -r = 1 - 2 Z_i Y / (1 + Z_total Y).
        let dr = 1. + total_impedance * re;
        let di = total_impedance * im;
        let den = dr * dr + di * di;
        let rr = 1. - 2. * impedance * (re * dr + im * di) / den;
        let ri = -2. * impedance * (im * dr - re * di) / den;
        ri.atan2(rr)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BridgeLoad {
    /// None retains the rigid-end string length, allowing physical pitch shifts
    /// from the moving boundary. A scalar phase correction is not valid for a
    /// general coupled two-plane boundary.
    pub spec: Option<BridgeSpec>,
    pub impedance: f64,
    pub total_impedance: f64,
}
impl BridgeLoad {
    pub fn valid(self) -> bool {
        self.impedance.is_finite()
            && (0.01..=4.).contains(&self.impedance)
            && self.total_impedance.is_finite()
            && (self.impedance..=16.).contains(&self.total_impedance)
    }
    pub fn phase_delay(self, hz: f64, rate: f64) -> f64 {
        self.spec.map_or(0., |spec| {
            -spec.reflection_phase(hz, rate, self.impedance, self.total_impedance)
                / (TAU * hz / rate)
        })
    }
}

#[derive(Clone, Copy, Default)]
struct ModeState {
    mass: f64,
    resistance: f64,
    stiffness: f64,
    inverse_denominator: f64,
    displacement: f64,
    velocity: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct BridgeFrame {
    /// Common midpoint velocity at every connected string notch.
    pub velocity: f64,
    pub force: f64,
    /// Derivative of bridge-point velocity, a diagnostic radiation proxy.
    /// It omits surface-mode shapes and directional radiation; it is not
    /// calibrated microphone pressure.
    pub acceleration: f64,
    pub dissipated_energy: f64,
}

#[derive(Clone)]
pub struct ModalBridge {
    modes: [ModeState; MAX_BRIDGE_MODES],
    count: usize,
    dt: f64,
    mobility_step: f64,
    impedances: [f64; 4],
    total_impedance: f64,
    spec: BridgeSpec,
}
impl ModalBridge {
    pub fn new(spec: BridgeSpec, rate: f64, impedances: &[f64]) -> Result<Self, &'static str> {
        if !rate.is_finite() || !(24000.0..=192000.0).contains(&rate) {
            return Err("bridge requires sample rate in 24000..192000");
        }
        if impedances.is_empty()
            || impedances.len() > 4
            || impedances
                .iter()
                .any(|z| !z.is_finite() || !(0.01..=4.).contains(z))
        {
            return Err("bridge requires one to four positive bounded string impedances");
        }
        let mut ports = [0.; 4];
        ports[..impedances.len()].copy_from_slice(impedances);
        let mut result = Self {
            modes: [ModeState::default(); MAX_BRIDGE_MODES],
            count: spec.count,
            dt: 1. / rate,
            mobility_step: 0.,
            impedances: ports,
            total_impedance: impedances.iter().sum(),
            spec,
        };
        for (state, mode) in result.modes.iter_mut().zip(spec.modes()) {
            let (mass, resistance, stiffness) = BridgeSpec::parameters(*mode, rate);
            let inverse_denominator =
                1. / (2. * mass * rate + resistance + stiffness / (2. * rate));
            *state = ModeState {
                mass,
                resistance,
                stiffness,
                inverse_denominator,
                ..Default::default()
            };
            result.mobility_step += inverse_denominator;
        }
        Ok(result)
    }
    pub fn load(&self, port: usize) -> Option<BridgeLoad> {
        self.impedances
            .get(port)
            .filter(|z| **z > 0.)
            .map(|z| BridgeLoad {
                spec: Some(self.spec),
                impedance: *z,
                total_impedance: self.total_impedance,
            })
    }
    /// incident_force = sum(2 Z_i a_i), total_impedance = sum(Z_i).
    /// Caller uses b_i = velocity - a_i as the outgoing wave for every port.
    /// Normal bounded callers cannot inject energy through the load itself.
    pub fn step(&mut self, incoming: [f64; 4]) -> BridgeFrame {
        let incident_force: f64 = incoming
            .iter()
            .zip(self.impedances)
            .map(|(a, z)| 2. * z * a)
            .sum();
        let total_impedance = self.total_impedance;
        let mut histories = [0.; MAX_BRIDGE_MODES];
        let mut sum = 0.;
        for (history, state) in histories.iter_mut().zip(&self.modes[..self.count]) {
            *history = (2. * state.mass / self.dt * state.velocity
                - state.stiffness * state.displacement)
                * state.inverse_denominator;
            sum += *history;
        }
        let force =
            (incident_force - total_impedance * sum) / (1. + total_impedance * self.mobility_step);
        let velocity = sum + self.mobility_step * force;
        let (mut acceleration, mut dissipated_energy) = (0., 0.);
        for (history, state) in histories.iter().zip(&mut self.modes[..self.count]) {
            let mid = history + state.inverse_denominator * force;
            let next = 2. * mid - state.velocity;
            acceleration += (next - state.velocity) / self.dt;
            state.displacement += self.dt * mid;
            state.velocity = next;
            dissipated_energy += self.dt * state.resistance * mid * mid;
        }
        BridgeFrame {
            velocity,
            force,
            acceleration,
            dissipated_energy,
        }
    }
    pub fn energy(&self) -> f64 {
        self.modes[..self.count]
            .iter()
            .map(|s| {
                0.5 * (s.mass * s.velocity * s.velocity
                    + s.stiffness * s.displacement * s.displacement)
            })
            .sum()
    }
    pub fn reset(&mut self) {
        for state in &mut self.modes {
            state.displacement = 0.;
            state.velocity = 0.;
        }
    }
}

/// Two transverse bridge directions with fixed orthogonal principal axes.
/// Each string contributes the same characteristic impedance in both planes.
/// Rotating incident waves and returned velocities preserves port power; each
/// principal response retains its own mechanical state and positive damping.
/// This is a restricted anisotropic body, not a general measured violin tensor.
#[derive(Clone)]
pub struct PolarizedBridge {
    axes: [ModalBridge; 2],
    cosine: f64,
    sine: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct PolarizedBridgeFrame {
    pub velocity: [f64; 2],
    pub force: [f64; 2],
    /// Bridge-point acceleration only, not microphone pressure.
    pub acceleration: [f64; 2],
    pub dissipated_energy: f64,
}

impl PolarizedBridge {
    pub fn new(
        specs: [BridgeSpec; 2],
        angle_radians: f64,
        rate: f64,
        impedances: &[f64],
    ) -> Result<Self, &'static str> {
        if !angle_radians.is_finite() {
            return Err("bridge orientation must be finite");
        }
        let (sine, cosine) = angle_radians.sin_cos();
        Ok(Self {
            axes: [
                ModalBridge::new(specs[0], rate, impedances)?,
                ModalBridge::new(specs[1], rate, impedances)?,
            ],
            cosine,
            sine,
        })
    }

    /// Advance once for all strings and both planes. For each port and axis,
    /// the caller returns b = frame.velocity - incoming to the string rail.
    /// Do not feed these velocities into a separate output-only body filter.
    pub fn step(&mut self, incoming: [[f64; 2]; 4]) -> PolarizedBridgeFrame {
        let x = incoming.map(|a| self.cosine * a[0] + self.sine * a[1]);
        let y = incoming.map(|a| -self.sine * a[0] + self.cosine * a[1]);
        let frames = [self.axes[0].step(x), self.axes[1].step(y)];
        let rotate = |u: f64, v: f64| {
            [self.cosine * u - self.sine * v, self.sine * u + self.cosine * v]
        };
        PolarizedBridgeFrame {
            velocity: rotate(frames[0].velocity, frames[1].velocity),
            force: rotate(frames[0].force, frames[1].force),
            acceleration: rotate(frames[0].acceleration, frames[1].acceleration),
            dissipated_energy: frames[0].dissipated_energy + frames[1].dissipated_energy,
        }
    }

    /// Same load applies to both planes. Physical boundary-induced pitch shifts
    /// are retained; no single-plane phase compensation is imposed on the pair.
    pub fn load(&self, port: usize) -> Option<BridgeLoad> {
        self.axes[0].load(port).map(|mut load| {
            load.spec = None;
            load
        })
    }

    pub fn energy(&self) -> f64 {
        self.axes[0].energy() + self.axes[1].energy()
    }

    pub fn reset(&mut self) {
        for axis in &mut self.axes {
            axis.reset();
        }
    }
}
