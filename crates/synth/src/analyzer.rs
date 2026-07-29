//! Realtime-safe custom units used by graph lowering.
//!
//! Stateful audio-to-control analysers live here beside the small units whose
//! behavior fundsp does not provide directly. Template data remains in
//! `template`; every type here is backend implementation and may retain only
//! bounded, allocation-free audio-thread state.

use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};

/// Lightweight monophonic period tracker.
///
/// It estimates pitch from positive zero-crossing periods once the input has
/// exceeded a small adaptive activity threshold. This is intentionally a
/// bounded stateful primitive: a more sophisticated autocorrelation tracker
/// can replace it without changing the graph or language boundary.
#[derive(Clone)]
pub(crate) struct PitchTrackerUnit {
    min_hz: f32,
    max_hz: f32,
    default_hz: f32,
    hold_seconds: f64,
    sample_rate: f64,
    previous: f32,
    peak: f32,
    since_crossing: u64,
    since_valid: u64,
    last_hz: f32,
    have_crossing: bool,
}

impl PitchTrackerUnit {
    pub(crate) fn new(min_hz: f64, max_hz: f64, default_hz: f64, hold_seconds: f64) -> Self {
        Self {
            min_hz: min_hz as f32,
            max_hz: max_hz as f32,
            default_hz: default_hz as f32,
            hold_seconds,
            sample_rate: 44_100.0,
            previous: 0.0,
            peak: 0.0,
            since_crossing: 0,
            since_valid: u64::MAX,
            last_hz: default_hz as f32,
            have_crossing: false,
        }
    }

    #[inline]
    fn sample(&mut self, input: f32) -> f32 {
        self.peak = self.peak.max(input.abs()) * 0.9995;
        self.since_crossing = self.since_crossing.saturating_add(1);
        self.since_valid = self.since_valid.saturating_add(1);
        if self.previous <= 0.0 && input > 0.0 && self.peak > 0.002 {
            if self.have_crossing && self.since_crossing > 0 {
                let hz = (self.sample_rate / self.since_crossing as f64) as f32;
                if (self.min_hz..=self.max_hz).contains(&hz) {
                    self.last_hz = hz;
                    self.since_valid = 0;
                }
            }
            self.since_crossing = 0;
            self.have_crossing = true;
        }
        self.previous = input;
        let hold_samples = (self.hold_seconds * self.sample_rate) as u64;
        if self.since_valid > hold_samples {
            self.default_hz
        } else {
            self.last_hz
        }
    }
}

impl AudioUnit for PitchTrackerUnit {
    fn reset(&mut self) {
        self.previous = 0.0;
        self.peak = 0.0;
        self.since_crossing = 0;
        self.since_valid = u64::MAX;
        self.last_hz = self.default_hz;
        self.have_crossing = false;
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        if sample_rate.is_finite() && sample_rate > 0.0 {
            self.sample_rate = sample_rate;
            self.reset();
        }
    }

    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        output[0] = self.sample(input[0]);
    }

    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for sample in 0..size {
            output.set_f32(0, sample, self.sample(input.at_f32(0, sample)));
        }
    }

    fn inputs(&self) -> usize {
        1
    }

    fn outputs(&self) -> usize {
        1
    }

    fn route(&mut self, _input: &SignalFrame, _frequency: f64) -> SignalFrame {
        let mut output = SignalFrame::new(1);
        output.fill(Signal::Unknown);
        output
    }

    fn get_id(&self) -> u64 {
        0x4150_5445_5054_4348
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

/// Bounded transient detector producing a short unipolar pulse.
///
/// The detector follows rectified amplitude with an immediate attack and a
/// fixed release. A rising threshold crossing starts one pulse and a
/// user-declared refractory interval prevents a ringing transient from
/// producing a burst of events. It owns no allocation and its state is
/// bounded independently of how long the input runs.
#[derive(Clone)]
pub(crate) struct OnsetDetectorUnit {
    floor: f32,
    hold_seconds: f64,
    sample_rate: f64,
    envelope: f32,
    was_above: bool,
    refractory_samples: u64,
    pulse_samples: u64,
}

impl OnsetDetectorUnit {
    pub(crate) fn new(floor: f64, hold_seconds: f64) -> Self {
        Self {
            floor: floor as f32,
            hold_seconds,
            sample_rate: 44_100.0,
            envelope: 0.0,
            was_above: false,
            refractory_samples: 0,
            pulse_samples: 0,
        }
    }

    #[inline]
    fn sample(&mut self, input: f32) -> f32 {
        // Thirty milliseconds is long enough to keep one oscillatory attack
        // above the threshold while still resetting between ordinary hits.
        let release =
            (-1.0 / (crate::template::ONSET_PULSE_SECONDS * self.sample_rate)).exp() as f32;
        self.envelope = input.abs().max(self.envelope * release);
        if self.refractory_samples > 0 {
            self.refractory_samples -= 1;
        }
        let above = self.envelope >= self.floor;
        if above && !self.was_above && self.refractory_samples == 0 {
            // The app's control worker polls at roughly 20 ms. Keep the pulse
            // high for 30 ms so a valid onset cannot fall wholly between two
            // polls; this is a control/event bridge, not an audio impulse.
            self.pulse_samples = (crate::template::ONSET_PULSE_SECONDS * self.sample_rate)
                .round()
                .max(1.0) as u64;
            self.refractory_samples =
                (self.hold_seconds * self.sample_rate).round().max(0.0) as u64;
        }
        self.was_above = above;
        let output = f32::from(self.pulse_samples > 0);
        self.pulse_samples = self.pulse_samples.saturating_sub(1);
        output
    }
}

impl AudioUnit for OnsetDetectorUnit {
    fn reset(&mut self) {
        self.envelope = 0.0;
        self.was_above = false;
        self.refractory_samples = 0;
        self.pulse_samples = 0;
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        if sample_rate.is_finite() && sample_rate > 0.0 {
            self.sample_rate = sample_rate;
            self.reset();
        }
    }

    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        output[0] = self.sample(input[0]);
    }

    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for sample in 0..size {
            output.set_f32(0, sample, self.sample(input.at_f32(0, sample)));
        }
    }

    fn inputs(&self) -> usize {
        1
    }

    fn outputs(&self) -> usize {
        1
    }

    fn route(&mut self, _input: &SignalFrame, _frequency: f64) -> SignalFrame {
        let mut output = SignalFrame::new(1);
        output.fill(Signal::Unknown);
        output
    }

    fn get_id(&self) -> u64 {
        0x4150_5445_4f4e_5345
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

/// Allocation-free playback of one compiled transport-pattern period.
#[derive(Clone)]
pub(crate) struct TransportSequenceUnit {
    period_seconds: f64,
    slots: Vec<crate::template::TransportSlot>,
    sample_rate: f64,
    sample_index: u64,
}

impl TransportSequenceUnit {
    pub(crate) fn new(period_seconds: f64, slots: Vec<crate::template::TransportSlot>) -> Self {
        Self {
            period_seconds,
            slots,
            sample_rate: 44_100.0,
            sample_index: 0,
        }
    }

    #[inline]
    fn sample(&mut self) -> f32 {
        let elapsed = self.sample_index as f64 / self.sample_rate;
        self.sample_index = self.sample_index.wrapping_add(1);
        let phase = elapsed.rem_euclid(self.period_seconds);
        let index = self
            .slots
            .partition_point(|slot| slot.end_seconds <= phase)
            .min(self.slots.len() - 1);
        self.slots[index].value as f32
    }
}

impl AudioUnit for TransportSequenceUnit {
    fn reset(&mut self) {
        self.sample_index = 0;
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        if sample_rate.is_finite() && sample_rate > 0.0 {
            self.sample_rate = sample_rate;
            self.reset();
        }
    }

    fn tick(&mut self, _input: &[f32], output: &mut [f32]) {
        output[0] = self.sample();
    }

    fn process(&mut self, size: usize, _input: &BufferRef, output: &mut BufferMut) {
        for sample in 0..size {
            output.set_f32(0, sample, self.sample());
        }
    }

    fn inputs(&self) -> usize {
        0
    }

    fn outputs(&self) -> usize {
        1
    }

    fn route(&mut self, _input: &SignalFrame, _frequency: f64) -> SignalFrame {
        let mut output = SignalFrame::new(1);
        output.fill(Signal::Unknown);
        output
    }

    fn get_id(&self) -> u64 {
        0x4150_5445_5452_5351
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.slots.capacity() * std::mem::size_of::<crate::template::TransportSlot>()
    }
}

/// Stereo mid/side width stage.
#[derive(Clone)]
pub(crate) struct WidthUnit;

impl WidthUnit {
    pub(crate) fn new() -> Self {
        Self
    }

    #[inline]
    fn sample(&self, left: f32, right: f32, amount: f32) -> (f32, f32) {
        let mid = (left + right) * 0.5;
        let side = (left - right) * 0.5 * amount;
        (mid + side, mid - side)
    }
}

impl AudioUnit for WidthUnit {
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        (output[0], output[1]) = self.sample(input[0], input[1], input[2]);
    }

    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for sample in 0..size {
            let (left, right) = self.sample(
                input.at_f32(0, sample),
                input.at_f32(1, sample),
                input.at_f32(2, sample),
            );
            output.set_f32(0, sample, left);
            output.set_f32(1, sample, right);
        }
    }

    fn inputs(&self) -> usize {
        3
    }

    fn outputs(&self) -> usize {
        2
    }

    fn route(&mut self, _input: &SignalFrame, _frequency: f64) -> SignalFrame {
        let mut output = SignalFrame::new(2);
        output.fill(Signal::Unknown);
        output
    }

    fn get_id(&self) -> u64 {
        0x4150_5445_5749_4454
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

/// One-pole slew with an explicit construction-time initial value.
#[derive(Clone)]
pub(crate) struct SlewUnit {
    response_seconds: f64,
    initial: f32,
    sample_rate: f64,
    value: f32,
}

impl SlewUnit {
    pub(crate) fn new(response_seconds: f64, initial: f64) -> Self {
        Self {
            response_seconds,
            initial: initial as f32,
            sample_rate: 44_100.0,
            value: initial as f32,
        }
    }

    #[inline]
    fn sample(&mut self, input: f32) -> f32 {
        if self.response_seconds <= 0.0 {
            self.value = input;
        } else {
            let retain = (-1.0 / (self.response_seconds * self.sample_rate)).exp() as f32;
            self.value = retain * self.value + (1.0 - retain) * input;
        }
        self.value
    }
}

impl AudioUnit for SlewUnit {
    fn reset(&mut self) {
        self.value = self.initial;
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        if sample_rate.is_finite() && sample_rate > 0.0 {
            self.sample_rate = sample_rate;
            self.reset();
        }
    }

    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        output[0] = self.sample(input[0]);
    }

    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for sample in 0..size {
            output.set_f32(0, sample, self.sample(input.at_f32(0, sample)));
        }
    }

    fn inputs(&self) -> usize {
        1
    }

    fn outputs(&self) -> usize {
        1
    }

    fn route(&mut self, _input: &SignalFrame, _frequency: f64) -> SignalFrame {
        let mut output = SignalFrame::new(1);
        output.fill(Signal::Unknown);
        output
    }

    fn get_id(&self) -> u64 {
        0x4150_5445_534c_4557
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}
