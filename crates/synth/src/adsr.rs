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

/// Live envelopes are evaluated directly at the sample coordinate. They read
/// one atomic release time and never allocate or invoke language callbacks.
#[derive(Clone)]
pub(crate) struct LiveEnvelopeUnit {
    gate: crate::LiveGate,
    adsr: Option<Adsr>,
    delay: f64,
    sample_rate: f64,
    position: u64,
}
impl LiveEnvelopeUnit {
    pub(crate) fn adsr(adsr: Adsr, gate: crate::LiveGate) -> Self {
        Self {
            gate,
            adsr: Some(adsr),
            delay: 0.0,
            sample_rate: 44_100.0,
            position: 0,
        }
    }
    pub(crate) fn lifecycle(gate: crate::LiveGate, delay: f64) -> Self {
        Self {
            gate,
            adsr: None,
            delay,
            sample_rate: 44_100.0,
            position: 0,
        }
    }
    pub(crate) fn constant_block(&mut self, size: usize) -> Option<f32> {
        if size == 0 {
            return None;
        }
        let release = self.gate.release_seconds();
        let begin = self.position as f64 / self.sample_rate;
        let last = (self.position + size as u64 - 1) as f64 / self.sample_rate;
        let constant = if let Some(adsr) = self.adsr {
            if begin >= release + adsr.release {
                Some(0.0)
            } else if begin >= adsr.attack + adsr.decay && last < release {
                Some(adsr.sustain as f32)
            } else {
                None
            }
        } else if last < release + self.delay {
            Some(1.0)
        } else if begin >= release + self.delay + 0.005 {
            Some(0.0)
        } else {
            None
        };
        if constant.is_some() {
            self.position = self.position.saturating_add(size as u64);
        }
        constant
    }
    fn render_linear_block(&mut self, size: usize, release: f64, output: &mut BufferMut) -> bool {
        let Some(adsr) = self.adsr else {
            return false;
        };
        let begin = self.position as f64 / self.sample_rate;
        let last = (self.position + size as u64 - 1) as f64 / self.sample_rate;
        let end = release + adsr.release;
        if end.is_finite()
            && self.position.saturating_add(size as u64 - 1)
                >= (end * self.sample_rate).ceil() as u64
        {
            return false;
        }
        let phase = if last < adsr.attack && last < release {
            0
        } else if begin >= adsr.attack && last < adsr.attack + adsr.decay && last < release {
            1
        } else if begin >= release && last < release + adsr.release {
            2
        } else {
            return false;
        };
        let level = if phase == 2 {
            adsr.at(release, f64::INFINITY)
        } else {
            0.0
        };
        for start in (0..size).step_by(4) {
            let time = wide::f64x4::new(std::array::from_fn(|lane| {
                (self.position + (start + lane) as u64) as f64
            })) / self.sample_rate;
            let values = match phase {
                0 => time / adsr.attack,
                1 => wide::f64x4::ONE + (time - adsr.attack) * (adsr.sustain - 1.0) / adsr.decay,
                2 => (wide::f64x4::ONE - (time - release) / adsr.release) * level,
                _ => unreachable!(),
            };
            for (lane, value) in values
                .to_array()
                .iter()
                .enumerate()
                .take((size - start).min(4))
            {
                output.set_f32(0, start + lane, *value as f32);
            }
        }
        self.position = self.position.saturating_add(size as u64);
        true
    }
    fn sample(&mut self, release: f64) -> f32 {
        let time = self.position as f64 / self.sample_rate;
        let end = release + self.adsr.map_or(self.delay + 0.005, |adsr| adsr.release);
        let ended = end.is_finite() && self.position >= (end * self.sample_rate).ceil() as u64;
        self.position = self.position.saturating_add(1);
        if ended {
            return 0.0;
        }
        if let Some(adsr) = self.adsr {
            adsr.at(time, release) as f32
        } else {
            let phase = ((time - release - self.delay) / 0.005).clamp(0.0, 1.0);
            (1.0 - phase * phase * phase * (phase * (phase * 6.0 - 15.0) + 10.0)) as f32
        }
    }
}
impl AudioUnit for LiveEnvelopeUnit {
    fn reset(&mut self) {
        self.position = 0;
    }
    fn set_sample_rate(&mut self, rate: f64) {
        self.sample_rate = rate;
        self.reset();
    }
    fn tick(&mut self, _: &[f32], output: &mut [f32]) {
        output[0] = self.sample(self.gate.release_seconds());
    }
    fn process(&mut self, size: usize, _: &BufferRef, output: &mut BufferMut) {
        if size == 0 {
            return;
        }
        if let Some(value) = self.constant_block(size) {
            output.channel_f32_mut(0)[..size].fill(value);
        } else {
            let release = self.gate.release_seconds();
            if self.render_linear_block(size, release, output) {
                return;
            }
            for i in 0..size {
                output.set_f32(0, i, self.sample(release));
            }
        }
    }
    fn inputs(&self) -> usize {
        0
    }
    fn outputs(&self) -> usize {
        1
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        SignalFrame::new(1)
    }
    fn get_id(&self) -> u64 {
        0x4150_5445_4c49_5645
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::LiveGate;
    use fundsp::prelude32::BufferVec;

    #[test]
    fn held_envelope_has_no_default_duration_and_releases_at_exact_coordinate() {
        let gate = LiveGate::new();
        let mut unit = LiveEnvelopeUnit::adsr(Adsr::new(0.01, 0.02, 0.7, 0.1), gate.clone());
        unit.set_sample_rate(1000.0);
        let mut output = [0.0];
        for _ in 0..5000 {
            unit.tick(&[], &mut output);
        }
        assert_eq!(output[0], 0.7);
        gate.release_after(5.0).unwrap();
        for _ in 0..100 {
            unit.tick(&[], &mut output);
        }
        assert!(output[0] > 0.0);
        unit.tick(&[], &mut output);
        assert_eq!(output[0], 0.0);
    }

    #[test]
    fn release_during_attack_uses_reached_level_and_tick_matches_every_block_size() {
        let gate = LiveGate::new();
        gate.release_after(0.025).unwrap();
        let mut ticks = LiveEnvelopeUnit::adsr(Adsr::new(0.1, 0.0, 1.0, 0.05), gate.clone());
        ticks.set_sample_rate(8000.0);
        let mut samples = Vec::new();
        for _ in 0..1024 {
            let mut out = [0.0];
            ticks.tick(&[], &mut out);
            samples.push(out[0]);
        }
        assert_eq!(samples[200], 0.25);
        assert!((samples[400] - 0.125).abs() < 1e-6);
        let terminal = ((0.025_f64 + 0.05) * 8000.0).ceil() as usize;
        assert!(samples[terminal..].iter().all(|s| *s == 0.0));
        for block in [1, 7, 17, 64] {
            let mut unit = LiveEnvelopeUnit::adsr(Adsr::new(0.1, 0.0, 1.0, 0.05), gate.clone());
            unit.set_sample_rate(8000.0);
            let input = BufferVec::new(0);
            let mut output = BufferVec::new(1);
            for start in (0..samples.len()).step_by(block) {
                let size = block.min(samples.len() - start);
                unit.process(size, &input.buffer_ref(), &mut output.buffer_mut());
                for i in 0..size {
                    assert_eq!(samples[start + i], output.buffer_ref().at_f32(0, i));
                }
            }
        }
    }

    #[test]
    fn release_is_validated_and_earliest_coordinate_wins() {
        let gate = LiveGate::new();
        for value in [f64::NAN, f64::INFINITY, -0.01] {
            assert!(gate.release_after(value).is_err());
        }
        gate.release_after(2.0).unwrap();
        gate.release_after(3.0).unwrap();
        assert_eq!(gate.release_seconds(), 2.0);
        gate.release_after(1.0).unwrap();
        assert_eq!(gate.release_seconds(), 1.0);
    }
}

/// A safety gate at the voice edge. Render directly into the caller's lanes;
/// the held gate is exactly one and needs no extra mixer or buffer round trip.
#[derive(Clone)]
pub(crate) struct LiveOutputGate {
    inner: Box<dyn AudioUnit>,
    gate: LiveEnvelopeUnit,
    buffer: fundsp::prelude32::BufferVec,
}
impl LiveOutputGate {
    pub(crate) fn new(inner: Box<dyn AudioUnit>, gate: LiveEnvelopeUnit) -> Self {
        Self {
            inner,
            gate,
            buffer: fundsp::prelude32::BufferVec::new(1),
        }
    }
}
impl AudioUnit for LiveOutputGate {
    fn reset(&mut self) {
        self.inner.reset();
        self.gate.reset();
    }
    fn set_sample_rate(&mut self, rate: f64) {
        self.inner.set_sample_rate(rate);
        self.gate.set_sample_rate(rate);
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        self.inner.tick(input, output);
        let mut gain = [0.0];
        self.gate.tick(&[], &mut gain);
        for sample in output {
            *sample *= gain[0];
        }
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        if size == 0 {
            return;
        }
        self.inner.process(size, input, output);
        if let Some(value) = self.gate.constant_block(size) {
            if value != 1.0 {
                for channel in 0..self.outputs() {
                    for sample in &mut output.channel_mut(channel)[..fundsp::simd_items(size)] {
                        *sample *= fundsp::prelude32::F32x::splat(value);
                    }
                }
            }
        } else {
            self.gate
                .process(size, &BufferRef::empty(), &mut self.buffer.buffer_mut());
            let gains = self.buffer.buffer_ref();
            for channel in 0..self.outputs() {
                for (sample, gain) in output.channel_mut(channel)[..fundsp::simd_items(size)]
                    .iter_mut()
                    .zip(gains.channel(0))
                {
                    *sample *= *gain;
                }
            }
        }
    }
    fn inputs(&self) -> usize {
        self.inner.inputs()
    }
    fn outputs(&self) -> usize {
        self.inner.outputs()
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        SignalFrame::new(self.outputs())
    }
    fn allocate(&mut self) {
        self.inner.allocate();
    }
    fn set_hash(&mut self, hash: u64) {
        self.inner.set_hash(hash);
    }
    fn get_id(&self) -> u64 {
        0x4150_544f_5554_4741
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.inner.footprint()
            + std::mem::size_of::<[fundsp::prelude32::F32x; fundsp::SIMD_LEN]>()
    }
}
