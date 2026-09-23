//! A bounded, re-excitable string loop. Deliberately a plucked-string PoC:
//! interpolation/filter losses shorten the nominal T60, and retuning is a
//! smoothed delay change rather than an energy-conserving contact simulation.
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};

#[derive(Clone)]
pub(crate) struct StringUnit {
    min_hz: f64,
    decay: f64,
    rate: f64,
    buffer: Vec<f32>,
    cursor: usize,
    previous: f32,
    delay: f64,
    mute: f64,
    smoothing: f64,
    gain: f32,
    control_tick: usize,
    initialized: bool,
}

impl StringUnit {
    pub(crate) fn new(min_hz: f64, decay: f64) -> Self {
        let mut unit = Self {
            min_hz,
            decay,
            rate: 44_100.0,
            buffer: Vec::new(),
            cursor: 0,
            previous: 0.0,
            delay: 0.0,
            mute: 0.0,
            smoothing: 0.0,
            gain: 0.0,
            control_tick: 0,
            initialized: false,
        };
        unit.set_sample_rate(44_100.0);
        unit
    }

    fn sample(&mut self, excitation: f32, hz: f32, mute: f32) -> f32 {
        let hz = if hz.is_finite() {
            f64::from(hz)
        } else {
            self.min_hz
        };
        let hz = hz.max(self.min_hz).min(self.rate / 4.0);
        // The two-tap loss filter contributes approximately 0.1 samples.
        let target = (self.rate / hz - 0.1).clamp(2.0, (self.buffer.len() - 2) as f64);
        let mute = if mute.is_finite() {
            f64::from(mute).clamp(0.0, 1.0)
        } else {
            1.0
        };
        if !self.initialized {
            self.delay = target;
            self.mute = mute;
            self.initialized = true;
        }
        self.delay += (target - self.delay) * self.smoothing;
        self.mute += (mute - self.mute) * self.smoothing;
        if self.control_tick == 0 {
            // Full pressure gives a 25 ms nominal T60. Calculate loss at a
            // bounded control rate, never pow/exp six times per audio sample.
            let loss = 1.0 / self.decay + self.mute * (1.0 / 0.025);
            self.gain = (-std::f64::consts::LN_10 * 3.0 * loss * (self.delay + 0.1) / self.rate)
                .exp() as f32;
        }
        self.control_tick = (self.control_tick + 1) % 32;
        let read = (self.cursor as f64 - self.delay).rem_euclid(self.buffer.len() as f64);
        let index = read as usize;
        let fraction = (read - index as f64) as f32;
        let delayed = self.buffer[index] * (1.0 - fraction)
            + self.buffer[(index + 1) % self.buffer.len()] * fraction;
        // Convex interpolation and filtering plus gain < 1 cannot amplify
        // the maximum stored magnitude, including while the delay moves.
        let feedback = (delayed * 0.9 + self.previous * 0.1) * self.gain;
        self.previous = delayed;
        let excitation = if excitation.is_finite() {
            excitation
        } else {
            0.0
        };
        let value = (excitation + feedback).clamp(-16.0, 16.0);
        self.buffer[self.cursor] = if value.abs() < 1.0e-20 { 0.0 } else { value };
        self.cursor = (self.cursor + 1) % self.buffer.len();
        delayed
    }
}

impl AudioUnit for StringUnit {
    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.cursor = 0;
        self.previous = 0.0;
        self.delay = 0.0;
        self.mute = 0.0;
        self.gain = 0.0;
        self.control_tick = 0;
        self.initialized = false;
    }
    fn set_sample_rate(&mut self, rate: f64) {
        assert!(rate.is_finite() && rate > 0.0);
        self.rate = rate;
        // Allocate the exact published extent instead of Vec's geometric
        // growth when the host replaces the constructor's default rate.
        self.buffer = vec![0.0; ((rate / self.min_hz).ceil() as usize).max(4) + 4];
        self.smoothing = 1.0 - (-1.0 / (rate * 0.002)).exp();
        self.reset();
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        output[0] = self.sample(input[0], input[1], input[2]);
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for i in 0..size {
            output.set_f32(
                0,
                i,
                self.sample(input.at_f32(0, i), input.at_f32(1, i), input.at_f32(2, i)),
            );
        }
    }
    fn inputs(&self) -> usize {
        3
    }
    fn outputs(&self) -> usize {
        1
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        let mut output = SignalFrame::new(1);
        output.fill(Signal::Unknown);
        output
    }
    fn get_id(&self) -> u64 {
        0x4150_5453_5452_494e
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>() + self.buffer.capacity() * std::mem::size_of::<f32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fundsp::prelude32::BufferVec;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn retained_string_rings_repicks_and_mutes_without_resurrection() {
        let mut unit = StringUnit::new(40.0, 35.0);
        unit.set_sample_rate(48_000.0);
        assert_eq!(unit.sample(0.0, 110.0, 0.0), 0.0);
        let mut samples = Vec::new();
        for i in 0..12 * 48_000 {
            samples.push(unit.sample(if i == 0 { 1.0 } else { 0.0 }, 110.0, 0.0));
        }
        let early = rms(&samples[48_000..96_000]);
        let late = rms(&samples[11 * 48_000..]);
        assert!(late > early * 0.015, "early {early}, late {late}");
        assert!(late < early);
        let mut unpicked = unit.clone();
        let mut difference = 0.0;
        for i in 0..48_000 {
            let picked = unit.sample(if i == 0 { 1.0 } else { 0.0 }, 110.0, 0.0);
            difference += (picked - unpicked.sample(0.0, 110.0, 0.0)).abs();
        }
        assert!(difference > 1.0);
        for _ in 0..24_000 {
            unit.sample(0.0, 110.0, 1.0);
        }
        let released: Vec<_> = (0..48_000).map(|_| unit.sample(0.0, 110.0, 0.0)).collect();
        assert!(rms(&released) < 1e-8, "mute left {}", rms(&released));
        unit.sample(1.0, 110.0, 0.0);
        let repicked: Vec<_> = (0..48_000).map(|_| unit.sample(0.0, 110.0, 0.0)).collect();
        assert!(rms(&repicked) > 1e-4);
    }

    #[test]
    fn bends_retain_bounded_state_and_change_pitch() {
        let mut high_minimum = StringUnit::new(20_000.0, 20.0);
        high_minimum.set_sample_rate(8_000.0);
        high_minimum.sample(0.0, 20_000.0, 0.0);
        assert!((high_minimum.delay - 3.9).abs() < 1e-9);
        for rate in [8_000.0, 44_100.0, 48_000.0, 96_000.0] {
            let mut unit = StringUnit::new(40.0, 35.0);
            unit.set_sample_rate(rate);
            let capacity = unit.buffer.capacity();
            assert_eq!(capacity, ((rate / 40.0).ceil() as usize).max(4) + 4);
            for i in 0..rate as usize {
                unit.sample(if i == 0 { 1.0 } else { 0.0 }, 220.0, 0.0);
            }
            let hz = 220.0 * 2.0_f32.powf(2.0 / 12.0);
            let samples: Vec<_> = (0..rate as usize)
                .map(|_| unit.sample(0.0, hz, 0.0))
                .collect();
            let tail = &samples[(rate / 2.0) as usize..];
            let error = |lag: usize| -> f32 {
                tail[lag..]
                    .iter()
                    .zip(tail)
                    .map(|(a, b)| (a - b).powi(2))
                    .sum()
            };
            assert!(
                error((rate / f64::from(hz)).round() as usize)
                    < error((rate / 220.0).round() as usize)
            );
            assert!(rms(tail) > 1e-6);
            for i in 0..10_000 {
                let hz = [f32::NAN, -100.0, 1e20, 40.0, 1500.0][i % 5];
                let output = unit.sample(0.0, hz, 0.0);
                assert!(output.is_finite() && output.abs() <= 1.0);
            }
            assert_eq!(unit.buffer.capacity(), capacity);
        }
    }

    #[test]
    fn ticks_blocks_and_reset_agree_with_modulation() {
        for size in [1, 17, 64] {
            let mut tick = StringUnit::new(40.0, 20.0);
            let mut block = tick.clone();
            let mut input = BufferVec::new(3);
            let mut output = BufferVec::new(1);
            for restart in 0..2 {
                for start in (0..4096).step_by(size) {
                    for i in 0..size {
                        input.buffer_mut().set_f32(
                            0,
                            i,
                            if (start + i) % 997 == 0 { 0.5 } else { 0.0 },
                        );
                        input
                            .buffer_mut()
                            .set_f32(1, i, 110.0 + (start + i) as f32 / 100.0);
                        input
                            .buffer_mut()
                            .set_f32(2, i, if start > 3000 { 1.0 } else { 0.0 });
                    }
                    block.process(size, &input.buffer_ref(), &mut output.buffer_mut());
                    for i in 0..size {
                        let value = tick.sample(
                            input.buffer_ref().at_f32(0, i),
                            input.buffer_ref().at_f32(1, i),
                            input.buffer_ref().at_f32(2, i),
                        );
                        assert_eq!(value, output.buffer_ref().at_f32(0, i), "restart {restart}");
                    }
                }
                tick.reset();
                block.reset();
                assert!(tick.buffer.iter().all(|x| *x == 0.0));
            }
        }
    }
}
