//! Control-rate ADSR with a sample-exact terminal zero.
//!
//! fundsp interpolates jittered control points. That interpolation can straddle
//! release end; the outer sample counter makes the lifetime's hard-zero promise
//! exact without changing the envelope's normal 500 Hz evaluation.

use crate::Adsr;
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, SignalFrame, envelope};

#[derive(Clone)]
pub(crate) struct AdsrUnit {
    inner: Box<dyn AudioUnit>,
    end_seconds: f64,
    end_sample: u64,
    position: u64,
}

impl AdsrUnit {
    pub(crate) fn new(adsr: Adsr, gate: f64) -> Self {
        let mut unit = Self {
            inner: Box::new(envelope(move |t: f32| adsr.at(f64::from(t), gate) as f32)),
            end_seconds: gate + adsr.tail(),
            end_sample: 0,
            position: 0,
        };
        unit.set_sample_rate(44_100.0);
        unit
    }
}

impl AudioUnit for AdsrUnit {
    fn reset(&mut self) {
        self.position = 0;
        self.inner.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        self.end_sample = (self.end_seconds * sample_rate).ceil() as u64;
        self.inner.set_sample_rate(sample_rate);
        self.reset();
    }

    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        if self.position < self.end_sample {
            self.inner.tick(input, output);
        } else {
            output[0] = 0.0;
        }
        self.position = self.position.saturating_add(1);
    }

    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        let active = self
            .end_sample
            .saturating_sub(self.position)
            .min(size as u64) as usize;
        if active > 0 {
            self.inner.process(active, input, output);
        }
        output.channel_f32_mut(0)[active..size].fill(0.0);
        self.position = self.position.saturating_add(size as u64);
    }

    fn inputs(&self) -> usize {
        0
    }
    fn outputs(&self) -> usize {
        1
    }

    fn route(&mut self, input: &SignalFrame, frequency: f64) -> SignalFrame {
        self.inner.route(input, frequency)
    }

    fn set_hash(&mut self, hash: u64) {
        self.inner.set_hash(hash);
    }

    fn allocate(&mut self) {
        self.inner.allocate();
    }

    fn get_id(&self) -> u64 {
        0x4150_5445_4144_5352
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>() + self.inner.footprint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fundsp::prelude32::BufferVec;

    #[test]
    fn release_is_exact_for_ticks_blocks_and_reset_at_multiple_rates() {
        for rate in [8_000.0, 44_100.0, 48_000.0, 96_000.0] {
            for release in [0.0, 0.0073] {
                let gate = 0.0037;
                let mut unit = AdsrUnit::new(Adsr::new(0.001, 0.002, 0.8, release), gate);
                unit.set_sample_rate(rate);
                unit.set_hash(12345);
                let cutoff = ((gate + release) * rate).ceil() as usize;
                let mut sample = [0.0];
                let mut energy = 0.0;
                for i in 0..cutoff + 100 {
                    unit.tick(&[], &mut sample);
                    if i >= cutoff {
                        assert_eq!(sample[0], 0.0);
                    }
                    energy += sample[0].abs();
                }
                assert!(energy > 0.0);

                for block in [1, 17, 64] {
                    unit.reset();
                    let input = BufferVec::new(0);
                    let mut output = BufferVec::new(1);
                    let mut energy = 0.0;
                    for start in (0..cutoff + 100).step_by(block) {
                        unit.process(block, &input.buffer_ref(), &mut output.buffer_mut());
                        for i in 0..block {
                            let sample = output.buffer_ref().at_f32(0, i);
                            if start + i >= cutoff {
                                assert_eq!(sample, 0.0);
                            }
                            energy += sample.abs();
                        }
                    }
                    assert!(energy > 0.0);
                }
            }
        }
    }
}
