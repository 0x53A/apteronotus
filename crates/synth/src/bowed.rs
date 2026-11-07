//! Retained velocity waveguide with versioned bow-contact experiments.
//! Model units and the exact approximation are documented in Task 008.
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};
use std::f64::consts::TAU;

/// Fixed synthetic instrument-body profiles, not measured impulse responses.
#[derive(Clone)]
pub struct BowedBody {
    filters: [Box<dyn AudioUnit>; 3],
    weights: [f32; 3],
    direct: f32,
    pub normalization: f64,
    /// FunDSP Simper SVF a1/a2/a3/m0/m1/m2, one row per band.
    pub coefficients: [[f32; 6]; 3],
}
impl BowedBody {
    pub fn new(rate: f64) -> Self {
        Self::for_kind(rate, BowedKind::Violin)
    }
    pub fn for_kind(rate: f64, kind: BowedKind) -> Self {
        use fundsp::prelude32::bandpass_hz;
        let (bands, weights, direct) = match kind {
            BowedKind::Violin => (
                [(460.0_f32, 1.5_f32), (1050.0, 1.1), (2700.0, 0.8)],
                [0.58_f32, 0.34, 0.60],
                0.72_f32,
            ),
            BowedKind::Cello => (
                [(190.0_f32, 1.5_f32), (560.0, 1.1), (1700.0, 0.8)],
                [0.68_f32, 0.48, 0.42],
                0.62_f32,
            ),
        };
        let mut filters: [Box<dyn AudioUnit>; 3] = bands.map(|(hz, q)| {
            let mut filter: Box<dyn AudioUnit> = Box::new(bandpass_hz(hz, q));
            filter.set_sample_rate(rate);
            filter
        });
        let mut real = if kind == BowedKind::Violin {
            0.72
        } else {
            0.62
        };
        let mut imaginary = 0.0;
        for (filter, weight) in filters.iter_mut().zip(if kind == BowedKind::Violin {
            [0.58, 0.34, 0.60]
        } else {
            [0.68, 0.48, 0.42]
        }) {
            let response = filter
                .response(0, 1000.0)
                .expect("linear fixed body filter");
            real += weight * response.re;
            imaginary += weight * response.im;
        }
        let coefficients = bands.map(|(hz, q)| {
            let c = fundsp::svf::SvfCoefs::<f32>::bandpass(rate as f32, hz, q);
            [c.a1, c.a2, c.a3, c.m0, c.m1, c.m2]
        });
        Self {
            filters,
            weights,
            direct,
            normalization: 1.0 / (real * real + imaginary * imaginary).sqrt(),
            coefficients,
        }
    }
    pub fn next_sample(&mut self, input: f32) -> f32 {
        let mut sum = input * self.direct;
        for (filter, weight) in self.filters.iter_mut().zip(self.weights) {
            let mut output = [0.0];
            filter.tick(&[input], &mut output);
            sum += output[0] * weight;
        }
        sum * self.normalization as f32
    }
    pub fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.reset();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BowedModel {
    C1,
    C2,
    /// Hyperbolic sliding friction with retained stick/slip branch selection.
    C3,
    /// Experimental C3 contact with third-order Lagrange string propagation.
    C4,
}

/// Normalized Friedlander load-line solve. Coefficients are experimental, not
/// measured rosin parameters. See Task 008's C3 experiment for the derivation.
/// Only the selected contact branch is history; there is no iterative solver.
#[derive(Clone, Default)]
struct BowFriction {
    slipping: bool,
}
impl BowFriction {
    fn injection(&mut self, delta: f64, force: f64) -> f64 {
        if force == 0.0 {
            self.slipping = false;
            return 0.0;
        }
        self.injection_with_limits(delta, 0.3 * force, 0.02 * force, 0.04)
    }
    fn injection_with_limits(
        &mut self,
        delta: f64,
        static_limit: f64,
        dynamic_limit: f64,
        knee: f64,
    ) -> f64 {
        if static_limit == 0. {
            self.slipping = false;
            return 0.;
        }
        let speed = delta.abs();
        if !self.slipping && speed <= static_limit {
            return delta;
        }
        // speed = slip + dynamic_limit +
        //         (static_limit-dynamic_limit)*knee/(knee+slip).
        // The larger nonnegative quadratic root is the stable sliding branch.
        let b = knee + dynamic_limit - speed;
        let c = knee * (static_limit - speed);
        let discriminant = b * b - 4.0 * c;
        if discriminant >= 0.0 {
            let root = discriminant.sqrt();
            let slip = if b > 0.0 {
                // Avoid subtracting nearly equal numbers at tiny force/slip.
                -2.0 * c / (b + root)
            } else {
                0.5 * (-b + root)
            };
            if slip > 0.0 {
                self.slipping = true;
                return delta.signum()
                    * (dynamic_limit + (static_limit - dynamic_limit) * knee / (knee + slip));
            }
        }
        self.slipping = false;
        delta
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BowedKind {
    #[default]
    Violin,
    Cello,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BowedConfig {
    pub kind: BowedKind,
    pub min_hz: f64,
    pub max_hz: f64,
    pub model: BowedModel,
}
impl Default for BowedConfig {
    fn default() -> Self {
        Self {
            kind: BowedKind::Violin,
            min_hz: 196.0,
            max_hz: 660.0,
            model: BowedModel::C1,
        }
    }
}
impl BowedConfig {
    pub fn valid(self) -> bool {
        self.min_hz.is_finite()
            && self.max_hz.is_finite()
            && self.min_hz
                >= if self.kind == BowedKind::Cello {
                    50.0
                } else {
                    80.0
                }
            && self.max_hz <= 2000.0
            && self.min_hz <= self.max_hz
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BowedControls {
    pub hz: f64,
    pub speed: f64,
    pub force: f64,
    pub position: f64,
}
impl Default for BowedControls {
    fn default() -> Self {
        Self {
            hz: 293.6648,
            speed: 0.0,
            force: 0.0,
            position: 0.12,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BowedDiagnostics {
    pub rejected_controls: u64,
    pub emergency_resets: u64,
}

#[derive(Clone)]
struct Rail {
    data: Vec<f32>,
    cursor: usize,
}
impl Rail {
    fn new(length: usize) -> Self {
        Self {
            data: vec![0.0; length],
            cursor: 0,
        }
    }
    fn read(&self, delay: f64) -> f32 {
        let position = (self.cursor as f64 - delay).rem_euclid(self.data.len() as f64);
        let index = position as usize;
        let fraction = (position - index as f64) as f32;
        self.data[index] * (1.0 - fraction) + self.data[(index + 1) % self.data.len()] * fraction
    }
    /// Four-tap interpolation centered on the desired sample. Its fractional
    /// coordinate remains in [0, 1], the central interval of taps [-1, 0, 1, 2].
    /// This reduces artificial high-frequency loss in the propagation rails;
    /// the explicit termination loss still controls physical decay. Delay >= 2
    /// guarantees that every nonzero tap refers to an already written sample.
    fn read_cubic(&self, delay: f64) -> f32 {
        let position = (self.cursor as f64 - delay).rem_euclid(self.data.len() as f64);
        let index = position as usize;
        let f = (position - index as f64) as f32;
        let length = self.data.len();
        let a = self.data[(index + length - 1) % length];
        let b = self.data[index];
        let c = self.data[(index + 1) % length];
        let d = self.data[(index + 2) % length];
        -f * (f - 1.0) * (f - 2.0) * a / 6.0 + (f + 1.0) * (f - 1.0) * (f - 2.0) * b / 2.0
            - (f + 1.0) * f * (f - 2.0) * c / 2.0
            + (f + 1.0) * f * (f - 1.0) * d / 6.0
    }
    fn push(&mut self, value: f32) {
        self.data[self.cursor] = if value.abs() < 1e-25 { 0.0 } else { value };
        self.cursor = (self.cursor + 1) % self.data.len();
    }
    fn reset(&mut self) {
        self.data.fill(0.0);
        self.cursor = 0;
    }
}

/// Four inputs (Hz, signed speed, force, bridge distance ratio), one output:
/// normalized bridge force. Allocation occurs only at construction/rate setup.
#[derive(Clone)]
pub struct BowedString {
    config: BowedConfig,
    rate: f64,
    rails: [Rail; 4],
    previous_bridge: f32,
    previous_nut: f32,
    controls: BowedControls,
    total_delay: f64,
    diagnostics: BowedDiagnostics,
    faulted: bool,
    damping: f64,
    loss_alpha: f64,
    friction: BowFriction,
    contact_calibration: Option<crate::bowed_contact::BowContactCalibration>,
    bridge_load: Option<crate::bowed_bridge::BridgeLoad>,
    has_processed: bool,
}
impl BowedString {
    pub fn new(config: BowedConfig, rate: f64) -> Result<Self, &'static str> {
        if !config.valid() || !rate.is_finite() || !(24_000.0..=192_000.0).contains(&rate) {
            return Err("bowed string requires valid pitch bounds and rate in 24000..192000 Hz");
        }
        let size = (rate / config.min_hz / 2.0).ceil() as usize + 4;
        let controls = BowedControls {
            hz: 293.6648_f64.clamp(config.min_hz, config.max_hz),
            ..Default::default()
        };
        let mut unit = Self {
            config,
            rate,
            rails: std::array::from_fn(|_| Rail::new(size)),
            previous_bridge: 0.0,
            previous_nut: 0.0,
            controls,
            total_delay: 0.0,
            diagnostics: BowedDiagnostics::default(),
            faulted: false,
            damping: 0.0,
            loss_alpha: (-TAU
                * if config.kind == BowedKind::Cello {
                    4000.0
                } else {
                    6000.0
                }
                / rate)
                .exp(),
            friction: BowFriction::default(),
            contact_calibration: None,
            bridge_load: None,
            has_processed: false,
        };
        unit.total_delay = unit.propagation_delay(controls.hz);
        if !unit.accepts(controls) {
            return Err("bowed string initial pitch/position has unsupported delays");
        }
        Ok(unit)
    }
    /// Explicit experimental SI mapping, installed before the first sample.
    /// Controls.speed still represents 0.2 traveling-wave units; returned bridge
    /// force remains normalized and must be multiplied by Z * velocity_unit.
    pub fn configure_contact(
        &mut self,
        calibration: crate::bowed_contact::BowContactCalibration,
    ) -> Result<(), &'static str> {
        if self.has_processed || self.config.model != BowedModel::C4 || !calibration.valid() {
            return Err("physical contact requires valid calibration and an unstarted C4 string");
        }
        if self
            .bridge_load
            .is_some_and(|load| load.impedance != calibration.impedance)
        {
            return Err("contact and bridge must use the same string impedance");
        }
        self.contact_calibration = Some(calibration);
        Ok(())
    }
    fn propagation_delay(&self, hz: f64) -> f64 {
        let w = TAU * hz / self.rate;
        let lag = if let Some(load) = self.bridge_load {
            // The original propagation loss must remain inside the string when
            // a moving bridge replaces its old fixed termination. It is lumped
            // at the nut in this experiment, retaining the same round-trip pole.
            load.phase_delay(hz, self.rate)
                + (self.loss_alpha * w.sin()).atan2(1.0 - self.loss_alpha * w.cos()) / w
        } else {
            match self.config.model {
                BowedModel::C1 => (0.1 * w.sin()).atan2(0.9 + 0.1 * w.cos()) / w,
                BowedModel::C2 | BowedModel::C3 | BowedModel::C4 => {
                    (self.loss_alpha * w.sin()).atan2(1.0 - self.loss_alpha * w.cos()) / w
                }
            }
        };
        self.rate / hz - lag
    }
    pub fn accepts(&self, c: BowedControls) -> bool {
        if !c.hz.is_finite()
            || !(self.config.min_hz..=self.config.max_hz).contains(&c.hz)
            || !c.speed.is_finite()
            || !(-1.0..=1.0).contains(&c.speed)
            || !c.force.is_finite()
            || !(0.0..=1.0).contains(&c.force)
            || !c.position.is_finite()
            || !(0.02..=0.45).contains(&c.position)
        {
            return false;
        }
        let t = if c.hz == self.controls.hz {
            self.total_delay
        } else {
            self.propagation_delay(c.hz)
        };
        c.position * t / 2.0 >= 2.0 && (1.0 - c.position) * t / 2.0 >= 2.0
    }
    /// Branch selected by the most recent C3/C4 contact solve. None means
    /// no solved contact (zero normal force, detached, or a different model).
    /// This observation does not advance or alter the string. Read immediately
    /// after sampling, before changing controls, for sample-aligned diagnostics.
    pub fn contact_slipping(&self) -> Option<bool> {
        if matches!(self.config.model, BowedModel::C3 | BowedModel::C4)
            && self.controls.force > 0.
            && self.has_processed
            && !self.faulted
        {
            Some(self.friction.slipping)
        } else {
            None
        }
    }

    pub fn diagnostics(&self) -> BowedDiagnostics {
        self.diagnostics
    }
    pub fn controls(&self) -> BowedControls {
        self.controls
    }
    pub fn sample_rate(&self) -> f64 {
        self.rate
    }
    pub fn config(&self) -> BowedConfig {
        self.config
    }
    /// Invalid multi-field changes are rejected as a whole, retaining prior controls.
    pub fn set_controls(&mut self, c: BowedControls) -> Result<(), &'static str> {
        if !self.accepts(c) {
            self.diagnostics.rejected_controls += 1;
            return Err("invalid bowed controls or joint pitch/position domain");
        }
        if c.hz != self.controls.hz {
            self.total_delay = self.propagation_delay(c.hz);
        }
        self.controls = c;
        Ok(())
    }
    /// A host panic detaches without erasing vibration. Numerical faults latch
    /// separately and cannot be hidden by this operation.
    pub fn detach(&mut self) {
        self.controls.force = 0.0;
        self.friction = BowFriction::default();
    }
    /// Additional physical loss, independent of bow force and output expression.
    pub fn set_damping(&mut self, amount: f64) -> Result<(), &'static str> {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            self.diagnostics.rejected_controls += 1;
            return Err("bowed damping must be in 0..1");
        }
        self.damping = amount;
        Ok(())
    }
    /// Install a mechanical boundary before sampling. Rejected load/domain
    /// changes retain the prior boundary and tuning. A shared ModalBridge must
    /// be advanced exactly once per sample before calling next_sample_at_bridge.
    pub fn connect_bridge(
        &mut self,
        load: crate::bowed_bridge::BridgeLoad,
    ) -> Result<(), &'static str> {
        if self.has_processed || !load.valid() || self.config.model != BowedModel::C4 {
            return Err("external mechanical bridge requires an unstarted C4 and a valid load");
        }
        if self
            .contact_calibration
            .is_some_and(|c| c.impedance != load.impedance)
        {
            return Err("contact and bridge must use the same string impedance");
        }
        let previous = self.bridge_load;
        let delay = self.total_delay;
        self.bridge_load = Some(load);
        self.total_delay = self.propagation_delay(self.controls.hz);
        if !self.accepts(self.controls) {
            self.bridge_load = previous;
            self.total_delay = delay;
            return Err("mechanical bridge tuning exceeds joint delay bounds");
        }
        Ok(())
    }
    pub fn bridge_incident(&self) -> f32 {
        if self.faulted {
            return 0.0;
        }
        let delay = self.controls.position * self.total_delay / 2.0;
        if self.config.model == BowedModel::C4 {
            self.rails[0].read_cubic(delay)
        } else {
            self.rails[0].read(delay)
        }
    }
    /// Bridge velocity supplies a reciprocal boundary, replacing the fixed
    /// reflection filter. No original body coloration is applied by this path.
    pub fn next_sample_at_bridge(&mut self, velocity: f64) -> f32 {
        assert!(
            self.bridge_load.is_some(),
            "connect a bridge before advancing its port"
        );
        self.next_sample_with_bridge(Some(velocity))
    }
    pub fn next_sample(&mut self) -> f32 {
        assert!(
            self.bridge_load.is_none(),
            "coupled string requires a shared bridge step"
        );
        self.next_sample_with_bridge(None)
    }
    fn next_sample_with_bridge(&mut self, bridge_velocity: Option<f64>) -> f32 {
        self.has_processed = true;
        if self.faulted {
            return 0.0;
        }
        let bridge_delay = self.controls.position * self.total_delay / 2.0;
        let neck_delay = (1.0 - self.controls.position) * self.total_delay / 2.0;
        let read = if self.config.model == BowedModel::C4 {
            Rail::read_cubic
        } else {
            Rail::read
        };
        let u_bridge = read(&self.rails[0], bridge_delay);
        let a_bridge = read(&self.rails[1], bridge_delay);
        let u_neck = read(&self.rails[2], neck_delay);
        let a_neck = read(&self.rails[3], neck_delay);
        let damping_gain = (1.0 - 0.95 * self.damping) as f32;
        let filtered = match self.config.model {
            BowedModel::C1 => 0.9 * u_bridge + 0.1 * self.previous_bridge,
            BowedModel::C2 | BowedModel::C3 | BowedModel::C4 => {
                (1.0 - self.loss_alpha as f32) * u_bridge
                    + self.loss_alpha as f32 * self.previous_bridge
            }
        };
        let reflected = if let Some(velocity) = bridge_velocity {
            // Preserve the mechanical port identity exactly. Dissipative
            // finger contact belongs inside the string, not between this
            // velocity and the body force that was just solved from it.
            velocity as f32 - u_bridge
        } else {
            -0.995 * filtered * damping_gain
        };
        let d = (0.2 * self.controls.speed) as f32 - (a_bridge + a_neck);
        let injection = if matches!(self.config.model, BowedModel::C3 | BowedModel::C4) {
            if let Some(calibration) = self.contact_calibration {
                let (static_limit, dynamic_limit, knee) =
                    calibration.normalized_limits(self.controls.force);
                self.friction
                    .injection_with_limits(d as f64, static_limit, dynamic_limit, knee)
                    as f32
            } else {
                self.friction.injection(d as f64, self.controls.force) as f32
            }
        } else {
            let width = (0.01 + 0.20 * self.controls.force) as f32;
            let shape = if d.abs() <= width {
                1.0
            } else {
                ((4.0 * width - d.abs()) / (3.0 * width)).max(0.0)
            };
            let strength = match self.config.model {
                BowedModel::C1 => self.controls.force,
                BowedModel::C2 | BowedModel::C3 | BowedModel::C4 => {
                    (self.controls.force / 0.05).min(1.0)
                }
            };
            let plateau = match self.config.model {
                BowedModel::C1 => 0.98,
                BowedModel::C2 | BowedModel::C3 | BowedModel::C4 => 1.0,
            };
            (plateau * strength) as f32 * shape * d
        };
        let nut = if bridge_velocity.is_some() {
            let filtered_nut = (1.0 - self.loss_alpha as f32) * u_neck
                + self.loss_alpha as f32 * self.previous_nut;
            self.previous_nut = filtered_nut;
            -0.9995 * 0.995 * filtered_nut * damping_gain
        } else {
            -0.9995 * u_neck
        };
        let values = [a_neck + injection, reflected, a_bridge + injection, nut];
        if values.iter().any(|v| !v.is_finite() || v.abs() > 16.0) {
            for rail in &mut self.rails {
                rail.reset();
            }
            self.previous_bridge = 0.0;
            self.previous_nut = 0.0;
            self.detach();
            self.faulted = true;
            self.diagnostics.emergency_resets += 1;
            return 0.0;
        }
        self.previous_bridge = match self.config.model {
            BowedModel::C1 => u_bridge,
            BowedModel::C2 | BowedModel::C3 | BowedModel::C4 => filtered,
        };
        for (rail, value) in self.rails.iter_mut().zip(values) {
            rail.push(value);
        }
        u_bridge - reflected
    }
}
impl AudioUnit for BowedString {
    fn reset(&mut self) {
        for rail in &mut self.rails {
            rail.reset();
        }
        self.previous_bridge = 0.0;
        self.previous_nut = 0.0;
        self.faulted = false;
        self.has_processed = false;
        self.friction = BowFriction::default();
    }
    fn set_sample_rate(&mut self, rate: f64) {
        if rate == self.rate {
            return;
        }
        if let Ok(mut replacement) = Self::new(self.config, rate) {
            if let Some(load) = self.bridge_load {
                if replacement.connect_bridge(load).is_err() {
                    self.faulted = true;
                    self.diagnostics.rejected_controls += 1;
                    return;
                }
            }
            if replacement.set_controls(self.controls).is_err() {
                self.faulted = true;
                self.diagnostics.rejected_controls += 1;
                return;
            }
            replacement.contact_calibration = self.contact_calibration;
            replacement.damping = self.damping;
            replacement.diagnostics = self.diagnostics;
            *self = replacement;
        } else {
            self.faulted = true;
            self.diagnostics.emergency_resets += 1;
        }
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        let c = BowedControls {
            hz: input[0] as f64,
            speed: input[1] as f64,
            force: input[2] as f64,
            position: input[3] as f64,
        };
        let _ = self.set_controls(c);
        output[0] = self.next_sample();
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        let mut y = [0.0];
        for i in 0..size {
            self.tick(
                &std::array::from_fn::<_, 4, _>(|c| input.at_f32(c, i)),
                &mut y,
            );
            output.set_f32(0, i, y[0]);
        }
    }
    fn inputs(&self) -> usize {
        4
    }
    fn outputs(&self) -> usize {
        1
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        let mut output = SignalFrame::new(1);
        output.set(0, Signal::Unknown);
        output
    }
    fn get_id(&self) -> u64 {
        0x4150_5442_4f57_4331
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .rails
                .iter()
                .map(|r| r.data.capacity() * 4)
                .sum::<usize>()
    }
}

#[cfg(test)]
mod contact_tests {
    use super::BowFriction;

    #[test]
    fn sliding_satisfies_the_load_line_and_retains_the_selected_branch() {
        let mut contact = BowFriction::default();
        assert_eq!(contact.injection(0.14, 0.5), 0.14);
        assert!(!contact.slipping);
        for delta in [0.2, 0.14, 0.12] {
            let injection = contact.injection(delta, 0.5);
            let slip = delta - injection;
            assert!(contact.slipping && slip > 0.0);
            let friction = 0.5 * (0.02 + (0.3 - 0.02) / (1.0 + slip / 0.04));
            assert!((injection - friction).abs() < 1e-14);
        }
        // Same delta as the first call, different admissible solution because
        // the previous branch is actual contact history.
        assert!(contact.injection(0.14, 0.5) < 0.14);
        assert_eq!(contact.injection(0.03, 0.5), 0.03);
        assert!(!contact.slipping);
    }

    #[test]
    fn friction_is_passive_relative_to_the_bow_and_detach_clears_contact_only() {
        let mut positive = BowFriction::default();
        let mut negative = BowFriction::default();
        for force in [0.0, 1e-12, 0.001, 0.03, 0.3, 0.55, 1.0] {
            for i in (0..2000).chain((0..2000).rev()) {
                let delta = i as f64 * 0.001;
                let injection = positive.injection(delta, force);
                assert!(injection.is_finite());
                assert!(injection >= 0.0 && injection <= delta + 1e-14);
                assert!(injection <= 0.3 * force + 1e-14);
                assert_eq!(negative.injection(-delta, force), -injection);
            }
        }
        assert_eq!(positive.injection(10.0, 0.0), 0.0);
        assert!(!positive.slipping);
        assert_eq!(positive.injection(0.1, 0.5), 0.1);
        for force in [1e-30, 1e-20, 1e-12] {
            let delta = force * 0.31;
            let injection = positive.injection(delta, force);
            assert!(positive.slipping);
            assert!(injection <= force * (0.3 + 4.0 * f64::EPSILON) && injection > 0.0);
        }
    }
}

#[cfg(test)]
mod propagation_tests {
    use super::Rail;
    use std::f64::consts::TAU;

    #[test]
    fn cubic_delay_preserves_dc_and_reduces_passband_error_without_static_gain() {
        // Exercise the circular read itself, including wraparound and all
        // fractional coordinates. A 1/8-cycle sinusoid exposes linear loss.
        for fraction in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9] {
            let delay = 6.0 + fraction;
            let mut dc = Rail::new(32);
            for _ in 0..64 {
                dc.push(0.37);
            }
            assert!((dc.read_cubic(delay) - 0.37).abs() < 1e-6);
            for frequency in [0.01, 0.05, 0.125, 0.25, 0.4, 0.49] {
                let mut rail = Rail::new(32);
                let (mut cubic_error, mut linear_error, mut cubic_power, mut target_power) =
                    (0.0, 0.0, 0.0, 0.0);
                for sample in 0..4096 {
                    // The current cursor is the next sample to be written.
                    let expected = (TAU * frequency * (sample as f64 - delay)).sin();
                    let cubic = f64::from(rail.read_cubic(delay));
                    let linear = f64::from(rail.read(delay));
                    if sample >= 64 {
                        cubic_error += (cubic - expected).powi(2);
                        linear_error += (linear - expected).powi(2);
                        cubic_power += cubic * cubic;
                        target_power += expected * expected;
                    }
                    rail.push((TAU * frequency * sample as f64).sin() as f32);
                }
                assert!(
                    cubic_power <= target_power * 1.001,
                    "static interpolation must not amplify"
                );
                if frequency <= 0.125 && fraction != 0.0 {
                    assert!(
                        cubic_error < linear_error * 0.1,
                        "cubic must improve passband accuracy"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod contact_physics_tests {
    use super::*;
    use crate::bowed_contact::BowContactCalibration;
    #[test]
    fn calibrated_contact_accounts_for_bow_work_and_nonnegative_slip_heat() {
        for z in [0.1, 0.25, 0.5] {
            for velocity_unit in [0.5, 1., 5.] {
                let c = BowContactCalibration {
                    impedance: z,
                    velocity_unit,
                    normal_force_unit: 1.,
                    static_friction: 0.8,
                    sliding_friction: 0.05,
                    slip_knee: 0.2,
                };
                let mut friction = BowFriction::default();
                for i in 0..20000 {
                    let t = i as f64 * 0.001;
                    let a = 0.3 * (t * 17.).sin() / velocity_unit;
                    let b = 0.2 * (t * 13.).cos() / velocity_unit;
                    let bow = 0.4 * (t * 2.).sin() / velocity_unit;
                    let normal = if i % 107 == 0 {
                        0.
                    } else {
                        0.4 * (1. + t.sin())
                    };
                    let (s, d, k) = c.normalized_limits(normal);
                    let j = friction.injection_with_limits(bow - a - b, s, d, k);
                    let force = c.physical_force(j);
                    let heat = force * (bow - a - b - j) * velocity_unit;
                    let bow_power = force * bow * velocity_unit;
                    let string_power = z
                        * velocity_unit.powi(2)
                        * ((a + j).powi(2) + (b + j).powi(2) - a * a - b * b);
                    assert!(heat >= -1e-12);
                    assert!((bow_power - string_power - heat).abs() < 1e-12);
                    assert!(force.abs() <= normal * c.static_friction + 1e-12);
                }
            }
        }
    }
}

#[cfg(test)]
mod contact_branch_audit {
    use super::BowFriction;

    // Independent monotone bisection above the load-line turning point. This
    // does not reuse the production quadratic expression or its root choice.
    #[test]
    fn calibrated_sliding_agrees_with_independent_stable_branch_bisection() {
        for scale in [1e-9, 1e-5, 1., 1e3] {
            for ratio in [0., 0.05, 0.5, 1.] {
                for knee_ratio in [0.001, 0.1, 1., 10.] {
                    let s: f64 = scale;
                    let d = ratio * s;
                    let k = knee_ratio * s;
                    let turning = (((s - d) * k).sqrt() - k).max(0.);
                    let load = |v: f64| v + d + (s - d) * k / (k + v);
                    let minimum = load(turning);
                    for fraction in [1e-7, 0.001, 0.1, 1., 10.] {
                        let delta = minimum + fraction * s;
                        let mut lo = turning;
                        let mut hi = delta;
                        for _ in 0..100 {
                            let middle = (lo + hi) * 0.5;
                            if load(middle) < delta {
                                lo = middle;
                            } else {
                                hi = middle;
                            }
                        }
                        let expected_slip = (lo + hi) * 0.5;
                        for sign in [-1., 1.] {
                            let mut contact = BowFriction { slipping: true };
                            let injection = contact.injection_with_limits(sign * delta, s, d, k);
                            assert!(contact.slipping);
                            let slip = delta - injection.abs();
                            assert!(
                                (slip - expected_slip).abs() < 1e-9 * s,
                                "scale={scale} ratio={ratio} knee={knee_ratio}"
                            );
                            assert!(1. - (s - d) * k / (k + slip).powi(2) >= -1e-9);
                        }
                    }
                    if turning > 0. {
                        let mut contact = BowFriction { slipping: true };
                        let delta = minimum - 1e-7 * s;
                        assert_eq!(contact.injection_with_limits(delta, s, d, k), delta);
                        assert!(!contact.slipping);
                    }
                }
            }
        }
    }

    #[test]
    fn prescribed_ramp_switches_at_analytic_thresholds_within_one_sample() {
        // Deliberately bistable friction: loading sticks to s; unloading
        // slips to d + 2 sqrt((s-d)k) - k. No string propagation in this test.
        let (s, d, k): (f64, f64, f64) = (0.15, 0.01, 0.04);
        let return_threshold = d + 2. * ((s - d) * k).sqrt() - k;
        for rate in [24000., 48000., 96000., 192000.] {
            let mut contact = BowFriction::default();
            let mut changes = Vec::new();
            for i in 0..(rate as usize) {
                let t = i as f64 / rate;
                let delta = if t < 0.5 { 0.4 * t } else { 0.4 * (1. - t) };
                let before = contact.slipping;
                contact.injection_with_limits(delta, s, d, k);
                if contact.slipping != before {
                    changes.push(t);
                }
            }
            assert_eq!(changes.len(), 2);
            for (observed, expected) in changes
                .into_iter()
                .zip([s / 0.4, 1. - return_threshold / 0.4])
            {
                assert!((observed - expected).abs() <= 1.01 / rate);
            }
        }
    }
}
