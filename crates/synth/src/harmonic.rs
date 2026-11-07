//! Bounded additive oscillator. One phase is shared by all harmonics, so a
//! timbre stays periodic; constant-pitch blocks rotate the fundamental, while
//! modulated pitch uses one sin/cos pair per sample.
//! The taper is spectral protection for static/slowly modulated pitches, not
//! a promise of alias-free arbitrary audio-rate FM.
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};
// WebAssembly has two f64 lanes per SIMD register. Exercise that same
// kernel in unit tests; native production keeps four lanes on wide SIMD.
#[cfg(any(target_arch = "wasm32", test))]
use wide::f64x2 as HarmonicVector;
#[cfg(all(not(target_arch = "wasm32"), not(test)))]
use wide::f64x4 as HarmonicVector;
#[cfg(any(target_arch = "wasm32", test))]
const HARMONIC_LANES: usize = 2;
#[cfg(all(not(target_arch = "wasm32"), not(test)))]
const HARMONIC_LANES: usize = 4;

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
            count: amplitudes
                .iter()
                .rposition(|value| *value != 0.0)
                .map_or(0, |last| last + 1),
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
        let value = self.at_phase(frequency, sin, cos);
        self.phase = (self.phase + frequency / self.rate).fract();
        value
    }

    fn tapered_amplitudes(&self, frequency: f64) -> impl Iterator<Item = f64> + '_ {
        self.amplitudes[..self.count]
            .iter()
            .enumerate()
            .map_while(move |(index, amplitude)| {
                let normalized = frequency * (index + 1) as f64 / self.rate;
                if normalized >= 0.48 {
                    return None;
                }
                let x = ((0.48 - normalized) / 0.08).clamp(0.0, 1.0);
                let taper = x * x * (3.0 - 2.0 * x);
                Some(amplitude * taper)
            })
    }

    fn at_phase(&self, frequency: f64, sin: f64, cos: f64) -> f32 {
        let mut previous = 0.0;
        let mut current = sin;
        let mut value = 0.0;
        for weight in self.tapered_amplitudes(frequency) {
            value += weight * current;
            let next = 2.0 * cos * current - previous;
            previous = current;
            current = next;
        }
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
        if size == 0 {
            return;
        }
        let hz = input.at_f32(0, 0);
        let frequency = f64::from(hz);
        if frequency.is_finite()
            && frequency > 0.0
            && frequency < self.rate * 0.48
            && (1..size).all(|i| input.at_f32(0, i) == hz)
        {
            // At constant pitch, rotate the fundamental instead of calling
            // sin/cos for every sample. Re-anchor every native block to bound
            // rounding drift; retain the original sample-by-sample phase clock.
            // Spectral taper is constant throughout this block. Compute it
            // once, rather than dividing/clamping for every partial/sample.
            let mut weights = [0.0; 32];
            let mut active = 0;
            for (destination, weight) in weights.iter_mut().zip(self.tapered_amplitudes(frequency))
            {
                *destination = weight;
                active += 1;
            }
            let step = frequency / self.rate;
            let (step_sin, step_cos) = (step * std::f64::consts::TAU).sin_cos();
            let (mut sin, mut cos) = (self.phase * std::f64::consts::TAU).sin_cos();
            // Render successive samples together, retaining f64 precision
            // for the partial recurrence. No wavetable or partial is removed.
            let mut sins = [0.0; HARMONIC_LANES];
            let mut coses = [0.0; HARMONIC_LANES];
            for lane in 0..HARMONIC_LANES {
                sins[lane] = sin;
                coses[lane] = cos;
                (sin, cos) = (
                    sin * step_cos + cos * step_sin,
                    cos * step_cos - sin * step_sin,
                );
            }
            let mut sin = HarmonicVector::new(sins);
            let mut cos = HarmonicVector::new(coses);
            let (advance_sin, advance_cos) =
                (HARMONIC_LANES as f64 * step * std::f64::consts::TAU).sin_cos();
            for start in (0..size).step_by(HARMONIC_LANES) {
                let mut previous = HarmonicVector::ZERO;
                let mut current = sin;
                let mut value = HarmonicVector::ZERO;
                for weight in &weights[..active] {
                    value += current * *weight;
                    let next = cos * 2.0 * current - previous;
                    previous = current;
                    current = next;
                }
                for (lane, value) in value
                    .to_array()
                    .iter()
                    .enumerate()
                    .take((size - start).min(HARMONIC_LANES))
                {
                    output.set_f32(0, start + lane, *value as f32);
                    // Keep the scalar phase coordinate exactly, including for
                    // partial blocks and modulation on the following block.
                    self.phase = (self.phase + step).fract();
                }
                (sin, cos) = (
                    sin * advance_cos + cos * advance_sin,
                    cos * advance_cos - sin * advance_sin,
                );
            }
        } else {
            for i in 0..size {
                output.set_f32(0, i, self.sample(input.at_f32(0, i)));
            }
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
    fn constant_pitch_rotation_matches_scalar_across_blocks_and_rate_changes() {
        let mut scalar = HarmonicUnit::new(&[1.0 / 32.0; 32]);
        let mut block = scalar.clone();
        let mut input = BufferVec::new(1);
        let mut output = BufferVec::new(1);
        for rate in [24000.0, 44100.0, 48000.0, 96000.0] {
            scalar.set_sample_rate(rate);
            block.set_sample_rate(rate);
            for hz in [20.0, 440.0, 587.3295, 9000.0] {
                for size in [0, 1, 17, 63, 64] {
                    for _ in 0..40 {
                        for i in 0..size {
                            input.buffer_mut().set_f32(0, i, hz);
                        }
                        block.process(size, &input.buffer_ref(), &mut output.buffer_mut());
                        for i in 0..size {
                            let expected = scalar.sample(hz);
                            assert!((expected - output.buffer_ref().at_f32(0, i)).abs() < 1e-6);
                        }
                        assert_eq!(scalar.phase, block.phase);
                    }
                }
            }
        }
    }

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

/// Phase-coherent banks driven by the same init-rate frequency share the
/// fundamental rotation and harmonic recurrence, while retaining separate
/// weighted outputs for their independent envelopes.
#[derive(Clone)]
pub(crate) struct HarmonicBanks {
    banks: Vec<HarmonicUnit>,
    contributors: Vec<(usize, usize, f64)>,
    offsets: [usize; 33],
    weights: Vec<f64>,
    values: Vec<HarmonicVector>,
    polynomials: Vec<[f64; 16]>,
    polynomial_degrees: Vec<usize>,
    fixed_frequency: Option<f64>,
    pitch: Option<BankPitch>,
}
#[derive(Clone, Copy)]
struct BankPitch {
    frequency: f64,
    rate: f64,
    active: usize,
    step: f64,
    step_sin: f64,
    step_cos: f64,
    advance_sin: f64,
    advance_cos: f64,
}
impl HarmonicBanks {
    pub(crate) fn new(amplitudes: &[Vec<f64>]) -> Self {
        let banks: Vec<_> = amplitudes.iter().map(|a| HarmonicUnit::new(a)).collect();
        let mut contributors = Vec::new();
        let mut offsets = [0; 33];
        for (partial, offset) in offsets.iter_mut().enumerate().take(32) {
            *offset = contributors.len();
            for (channel, bank) in banks.iter().enumerate() {
                if bank.amplitudes[partial] != 0.0 {
                    contributors.push((channel, partial, bank.amplitudes[partial]));
                }
            }
        }
        offsets[32] = contributors.len();
        Self {
            weights: vec![0.0; contributors.len()],
            contributors,
            offsets,
            banks,
            values: vec![HarmonicVector::ZERO; amplitudes.len()],
            polynomials: vec![[0.0; 16]; amplitudes.len()],
            polynomial_degrees: vec![0; amplitudes.len()],
            fixed_frequency: None,
            pitch: None,
        }
    }
    pub(crate) fn fixed(amplitudes: &[Vec<f64>], frequency: f32) -> Self {
        let mut unit = Self::new(amplitudes);
        unit.fixed_frequency = Some(f64::from(frequency));
        unit
    }
    fn pitch(&mut self, frequency: f64) -> BankPitch {
        let rate = self.banks[0].rate;
        if let Some(pitch) = self.pitch
            && pitch.frequency == frequency
            && pitch.rate == rate
        {
            return pitch;
        }
        let mut active = 0;
        for (index, (_, partial, amplitude)) in self.contributors.iter().enumerate() {
            let normalized = frequency * (*partial + 1) as f64 / rate;
            if normalized >= 0.48 {
                break;
            }
            let x = ((0.48 - normalized) / 0.08).clamp(0.0, 1.0);
            self.weights[index] = amplitude * (x * x * (3.0 - 2.0 * x));
            active = active.max(partial + 1);
        }
        if active <= 16 {
            // sin(kθ) = sin(θ) U_{k-1}(cos(θ)). Compile the unchanged
            // harmonic sum into a polynomial once per pitch, then use Horner
            // evaluation per sample. Restrict the degree to 15 so f64 rounding
            // has bounded amplification; longer banks retain recurrence.
            self.polynomials.fill([0.0; 16]);
            self.polynomial_degrees.fill(0);
            let mut previous = [0.0; 16];
            let mut current = [0.0; 16];
            current[0] = 1.0;
            for partial in 0..active {
                for index in self.offsets[partial]..self.offsets[partial + 1] {
                    let channel = self.contributors[index].0;
                    let weight = self.weights[index];
                    if weight != 0.0 {
                        self.polynomial_degrees[channel] = partial;
                        for (coefficient, basis) in
                            self.polynomials[channel].iter_mut().zip(current)
                        {
                            *coefficient += weight * basis;
                        }
                    }
                }
                let mut next = [0.0; 16];
                for degree in 0..16 {
                    next[degree] = if degree == 0 {
                        0.0
                    } else {
                        2.0 * current[degree - 1]
                    } - previous[degree];
                }
                previous = current;
                current = next;
            }
        }
        let step = frequency / rate;
        let (step_sin, step_cos) = (step * std::f64::consts::TAU).sin_cos();
        let (advance_sin, advance_cos) =
            (HARMONIC_LANES as f64 * step * std::f64::consts::TAU).sin_cos();
        let pitch = BankPitch {
            frequency,
            rate,
            active,
            step,
            step_sin,
            step_cos,
            advance_sin,
            advance_cos,
        };
        self.pitch = Some(pitch);
        pitch
    }
}
impl AudioUnit for HarmonicBanks {
    fn reset(&mut self) {
        for bank in &mut self.banks {
            bank.reset();
        }
    }
    fn set_sample_rate(&mut self, rate: f64) {
        for bank in &mut self.banks {
            bank.set_sample_rate(rate);
        }
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        let hz = self
            .fixed_frequency
            .map_or_else(|| input[0], |hz| hz as f32);
        for (bank, out) in self.banks.iter_mut().zip(output) {
            *out = bank.sample(hz);
        }
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        if size == 0 {
            return;
        }
        let frequency = self
            .fixed_frequency
            .unwrap_or_else(|| f64::from(input.at_f32(0, 0)));
        let first = &self.banks[0];
        if !frequency.is_finite()
            || frequency <= 0.0
            || frequency >= first.rate * 0.48
            || (self.fixed_frequency.is_none()
                && (1..size).any(|i| f64::from(input.at_f32(0, i)) != frequency))
        {
            for i in 0..size {
                for (channel, bank) in self.banks.iter_mut().enumerate() {
                    let hz = self
                        .fixed_frequency
                        .map_or_else(|| input.at_f32(0, i), |hz| hz as f32);
                    output.set_f32(channel, i, bank.sample(hz));
                }
            }
            return;
        }
        let phase = first.phase;
        let BankPitch {
            active,
            step,
            step_sin,
            step_cos,
            advance_sin,
            advance_cos,
            ..
        } = self.pitch(frequency);
        let (mut sin, mut cos) = (phase * std::f64::consts::TAU).sin_cos();
        let mut sins = [0.0; HARMONIC_LANES];
        let mut coses = [0.0; HARMONIC_LANES];
        for lane in 0..HARMONIC_LANES {
            sins[lane] = sin;
            coses[lane] = cos;
            (sin, cos) = (
                sin * step_cos + cos * step_sin,
                cos * step_cos - sin * step_sin,
            );
        }
        let mut sin = HarmonicVector::new(sins);
        let mut cos = HarmonicVector::new(coses);
        let mut phase = phase;
        for start in (0..size).step_by(HARMONIC_LANES) {
            let mut previous = HarmonicVector::ZERO;
            let mut current = sin;
            if active <= 16 {
                for ((value, coefficients), degree) in self
                    .values
                    .iter_mut()
                    .zip(&self.polynomials)
                    .zip(&self.polynomial_degrees)
                {
                    let mut polynomial = HarmonicVector::splat(coefficients[*degree]);
                    for coefficient in coefficients[..*degree].iter().rev() {
                        polynomial = polynomial * cos + *coefficient;
                    }
                    *value = polynomial * sin;
                }
            } else if self.values.len() <= 4 {
                // Keep the usual foundation/body/upper/chiff accumulators in
                // registers rather than indexing mutable heap vectors for each
                // partial. The arithmetic and its order remain unchanged.
                let (mut v0, mut v1, mut v2, mut v3) = (
                    HarmonicVector::ZERO,
                    HarmonicVector::ZERO,
                    HarmonicVector::ZERO,
                    HarmonicVector::ZERO,
                );
                for partial in 0..active {
                    for index in self.offsets[partial]..self.offsets[partial + 1] {
                        let value = current * self.weights[index];
                        match self.contributors[index].0 {
                            0 => v0 += value,
                            1 => v1 += value,
                            2 => v2 += value,
                            3 => v3 += value,
                            _ => unreachable!(),
                        }
                    }
                    let next = cos * 2.0 * current - previous;
                    previous = current;
                    current = next;
                }
                let channels = self.values.len();
                self.values.copy_from_slice(&[v0, v1, v2, v3][..channels]);
            } else {
                self.values.fill(HarmonicVector::ZERO);
                for partial in 0..active {
                    for index in self.offsets[partial]..self.offsets[partial + 1] {
                        self.values[self.contributors[index].0] += current * self.weights[index];
                    }
                    let next = cos * 2.0 * current - previous;
                    previous = current;
                    current = next;
                }
            }
            for (channel, value) in self.values.iter().enumerate() {
                for (lane, value) in value
                    .to_array()
                    .iter()
                    .enumerate()
                    .take((size - start).min(HARMONIC_LANES))
                {
                    output.set_f32(channel, start + lane, *value as f32);
                }
            }
            for _ in 0..(size - start).min(HARMONIC_LANES) {
                phase = (phase + step).fract();
            }
            (sin, cos) = (
                sin * advance_cos + cos * advance_sin,
                cos * advance_cos - sin * advance_sin,
            );
        }
        for bank in &mut self.banks {
            bank.phase = phase;
        }
    }
    fn inputs(&self) -> usize {
        usize::from(self.fixed_frequency.is_none())
    }
    fn outputs(&self) -> usize {
        self.banks.len()
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        let mut output = SignalFrame::new(self.outputs());
        output.fill(Signal::Unknown);
        output
    }
    fn get_id(&self) -> u64 {
        0x4150_5448_4241_4e4b
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.banks.len()
                * (std::mem::size_of::<HarmonicUnit>() + std::mem::size_of::<HarmonicVector>())
            + self.contributors.len()
                * (std::mem::size_of::<(usize, usize, f64)>() + std::mem::size_of::<f64>())
    }
}

#[cfg(test)]
mod bank_tests {
    use super::*;
    use fundsp::prelude32::BufferVec;
    #[test]
    fn fused_banks_preserve_separate_scalar_oscillators_across_pitch_and_rate_changes() {
        let amplitudes = vec![
            vec![0.5, 0.3],
            vec![0.0, 0.0, 0.1, 0.06, 0.04],
            vec![0.0, 0.2],
            vec![0.0; 16],
        ];
        let mut fused = HarmonicBanks::new(&amplitudes);
        let mut scalar: Vec<_> = amplitudes.iter().map(|a| HarmonicUnit::new(a)).collect();
        let mut input = BufferVec::new(1);
        let mut output = BufferVec::new(4);
        for rate in [24000.0, 44100.0, 48000.0, 96000.0] {
            fused.set_sample_rate(rate);
            for unit in &mut scalar {
                unit.set_sample_rate(rate);
            }
            for hz in [20.0, 440.0, 587.3295, 9000.0] {
                for size in [1, 7, 17, 63, 64] {
                    for repeat in 0..10 {
                        for i in 0..size {
                            input.buffer_mut().set_f32(
                                0,
                                i,
                                if repeat == 9 { hz + i as f32 } else { hz },
                            );
                        }
                        fused.process(size, &input.buffer_ref(), &mut output.buffer_mut());
                        for (channel, unit) in scalar.iter_mut().enumerate() {
                            for i in 0..size {
                                let expected = unit.sample(input.buffer_ref().at_f32(0, i));
                                assert!(
                                    (expected - output.buffer_ref().at_f32(channel, i)).abs()
                                        < 1e-6
                                );
                            }
                            assert_eq!(unit.phase, fused.banks[channel].phase);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn polynomial_banks_preserve_dense_spectra_and_long_bank_fallback() {
        let amplitudes: [Vec<f64>; 3] = [
            (0..16).map(|i| 0.5 / (i + 1) as f64).collect(),
            (0..16)
                .map(|i| if i % 2 == 0 { -0.3 } else { 0.2 })
                .collect(),
            (0..32).map(|i| 0.2 / (i + 1) as f64).collect(),
        ];
        // Separate groups exercise the polynomial and recurrence paths.
        for spectra in [&amplitudes[..2], &amplitudes[..]] {
            let mut fused = HarmonicBanks::new(spectra);
            let mut scalar: Vec<_> = spectra.iter().map(|a| HarmonicUnit::new(a)).collect();
            let mut input = BufferVec::new(1);
            let mut output = BufferVec::new(spectra.len());
            for rate in [8000.0, 44100.0, 48000.0, 96000.0] {
                fused.set_sample_rate(rate);
                for unit in &mut scalar {
                    unit.set_sample_rate(rate);
                }
                for hz in [0.1, 20.0, 65.4, 440.0, 1000.0, 19000.0] {
                    for block in 0..100 {
                        let size = [1, 7, 17, 63, 64][block % 5];
                        for i in 0..size {
                            input.buffer_mut().set_f32(0, i, hz);
                        }
                        fused.process(size, &input.buffer_ref(), &mut output.buffer_mut());
                        for (channel, unit) in scalar.iter_mut().enumerate() {
                            for i in 0..size {
                                let expected = unit.sample(hz);
                                let actual = output.buffer_ref().at_f32(channel, i);
                                assert!(
                                    (expected - actual).abs() < 1e-6,
                                    "rate={rate} hz={hz} expected={expected} actual={actual}"
                                );
                            }
                            assert_eq!(unit.phase, fused.banks[channel].phase);
                        }
                    }
                }
            }
        }
    }
}
