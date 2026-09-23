//! Bounded additive oscillator. One phase is shared by all harmonics, so a
//! timbre stays periodic; recurrence needs only one sin/cos pair per sample.
//! The taper is spectral protection for static/slowly modulated pitches, not
//! a promise of alias-free arbitrary audio-rate FM.
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};

#[derive(Clone)]
pub(crate) struct HarmonicUnit {
    amplitudes: [f64; 32],
    count: usize,
    phase: f64,
    rate: f64,
}

impl HarmonicUnit {
    pub(crate) fn new(amplitudes: &[f64]) -> Self {
        assert!(!amplitudes.is_empty() && amplitudes.len() <= 32);
        let mut unit = Self {
            amplitudes: [0.0; 32],
            count: amplitudes.len(),
            phase: 0.0,
            rate: 44_100.0,
        };
        unit.amplitudes[..amplitudes.len()].copy_from_slice(amplitudes);
        unit
    }

    fn sample(&mut self, hz: f32) -> f32 {
        // Invalid/negative/ultrasonic input is silent, and cannot poison phase.
        let frequency = f64::from(hz);
        if !frequency.is_finite() || frequency <= 0.0 || frequency >= self.rate * 0.48 {
            return 0.0;
        }
        let (sin, cos) = (self.phase * std::f64::consts::TAU).sin_cos();
        let mut previous = 0.0;
        let mut current = sin;
        let mut value = 0.0;
        for index in 0..self.count {
            let normalized = frequency * (index + 1) as f64 / self.rate;
            if normalized >= 0.48 {
                break;
            }
            let x = ((0.48 - normalized) / 0.08).clamp(0.0, 1.0);
            let taper = x * x * (3.0 - 2.0 * x);
            value += self.amplitudes[index] * taper * current;
            let next = 2.0 * cos * current - previous;
            previous = current;
            current = next;
        }
        self.phase = (self.phase + frequency / self.rate).fract();
        value as f32
    }
}

impl AudioUnit for HarmonicUnit {
    fn reset(&mut self) {
        self.phase = 0.0;
    }
    fn set_sample_rate(&mut self, rate: f64) {
        assert!(rate.is_finite() && rate > 0.0);
        self.rate = rate;
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        output[0] = self.sample(input[0]);
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for i in 0..size {
            output.set_f32(0, i, self.sample(input.at_f32(0, i)));
        }
    }
    fn inputs(&self) -> usize {
        1
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
        0x4150_5448_4152_4d4f
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fundsp::prelude32::BufferVec;

    #[test]
    fn spectrum_preserves_tuning_and_suppresses_foldback_at_each_rate() {
        for rate in [24_000.0, 44_100.0, 48_000.0, 96_000.0] {
            let mut unit = HarmonicUnit::new(&[0.5, 0.3, 0.2]);
            unit.set_sample_rate(rate);
            let hz = (rate / 8.0) as f32;
            for i in 0..4096 {
                let phase = std::f64::consts::TAU * i as f64 / 8.0;
                let expected =
                    0.5 * phase.sin() + 0.3 * (2.0 * phase).sin() + 0.2 * (3.0 * phase).sin();
                assert!((f64::from(unit.sample(hz)) - expected).abs() < 1e-6);
            }
            // Third harmonic would fold to 1/4 rate without suppression.
            let mut high = HarmonicUnit::new(&[0.0, 0.0, 1.0]);
            high.set_sample_rate(rate);
            for _ in 0..4096 {
                assert_eq!(high.sample((rate / 4.0) as f32), 0.0);
            }
        }
    }

    #[test]
    fn blocks_reset_modulation_and_invalid_inputs_are_bounded() {
        let mut tick = HarmonicUnit::new(&[1.0 / 32.0; 32]);
        let mut block = tick.clone();
        let mut input = BufferVec::new(1);
        let mut output = BufferVec::new(1);
        for size in [1, 17, 64] {
            tick.reset();
            block.reset();
            for start in (0..4096).step_by(size) {
                for i in 0..size {
                    input
                        .buffer_mut()
                        .set_f32(0, i, 20.0 + (start + i) as f32 * 4.0);
                }
                block.process(size, &input.buffer_ref(), &mut output.buffer_mut());
                for i in 0..size {
                    let value = tick.sample(input.buffer_ref().at_f32(0, i));
                    assert_eq!(value, output.buffer_ref().at_f32(0, i));
                    assert!(value.is_finite() && value.abs() <= 1.00001);
                }
            }
        }
        for hz in [f32::NAN, f32::INFINITY, -1.0, 0.0, f32::MAX] {
            assert_eq!(tick.sample(hz), 0.0);
        }
        tick.reset();
        block.reset();
        for _ in 0..1024 {
            assert_eq!(tick.sample(110.0), block.sample(110.0));
        }
    }
}
