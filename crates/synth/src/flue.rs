//! Experimental pressure-driven flue: a jet delay/nonlinearity coupled to a
//! lossy bore waveguide. Independently implemented from the public waveguide
//! flute topology described by Cook/Scavone (STK Flute) and Faust physmodels.
//! This is a reduced model, not calibrated organ geometry. Its units are
//! normalized pressure and wave amplitude; no claim of Pascals or real dimensions.
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};
use std::f64::consts::TAU;

const OVERSAMPLE: usize = 4;
const BORE_PERIODS: f64 = 1.512;

#[derive(Clone)]
struct Delay {
    data: Vec<f64>,
    cursor: usize,
}

impl Delay {
    fn new(length: usize) -> Self {
        Self {
            data: vec![0.0; length],
            cursor: 0,
        }
    }
    fn read(&self, delay: f64) -> f64 {
        let delay = delay.clamp(1.0, (self.data.len() - 2) as f64);
        let position = (self.cursor as f64 - delay).rem_euclid(self.data.len() as f64);
        let index = position as usize;
        let fraction = position - index as f64;
        self.data[index] * (1.0 - fraction) + self.data[(index + 1) % self.data.len()] * fraction
    }
    fn push(&mut self, value: f64) {
        self.data[self.cursor] = if value.abs() < 1e-25 { 0.0 } else { value };
        self.cursor = (self.cursor + 1) % self.data.len();
    }
    fn reset(&mut self) {
        self.data.fill(0.0);
        self.cursor = 0;
    }
}

#[derive(Clone)]
pub(crate) struct FlueUnit {
    min_hz: f64,
    rate: f64,
    bore: Delay,
    jet: Delay,
    bore_loss: f64,
    jet_dc: f64,
    pressure: f64,
    hz: f64,
    delay: f64,
    loss_pole: f64,
    dc_alpha: f64,
    pressure_alpha: f64,
    pitch_alpha: f64,
    output_alpha: f64,
    output_filter: [f64; 4],
    previous_noise: f64,
    control_tick: usize,
    initialized: bool,
}

impl FlueUnit {
    pub(crate) fn new(min_hz: f64) -> Self {
        let mut unit = Self {
            min_hz,
            rate: 0.0,
            bore: Delay::new(4),
            jet: Delay::new(4),
            bore_loss: 0.0,
            jet_dc: 0.0,
            pressure: 0.0,
            hz: min_hz,
            delay: 2.0,
            loss_pole: 0.0,
            dc_alpha: 0.0,
            pressure_alpha: 0.0,
            pitch_alpha: 0.0,
            output_alpha: 0.0,
            output_filter: [0.0; 4],
            previous_noise: 0.0,
            control_tick: 0,
            initialized: false,
        };
        unit.set_sample_rate(44_100.0);
        unit
    }

    fn sample(&mut self, hz: f32, pressure: f32, turbulence: f32) -> f32 {
        let valid_hz = hz.is_finite() && hz > 0.0;
        let target_hz = if valid_hz {
            f64::from(hz)
                .max(self.min_hz)
                .min(1200.0_f64.min(self.rate / 32.0).max(1.0))
        } else {
            self.hz
        };
        let target_pressure = if valid_hz && pressure.is_finite() {
            f64::from(pressure).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let noise = if turbulence.is_finite() {
            f64::from(turbulence).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        if !self.initialized {
            self.hz = target_hz;
            self.initialized = true;
        }
        for sub in 0..OVERSAMPLE {
            self.hz += (target_hz - self.hz) * self.pitch_alpha;
            self.pressure += (target_pressure - self.pressure) * self.pressure_alpha;
            if target_pressure == 0.0 && self.pressure < 1e-8 {
                self.pressure = 0.0;
            }
            if self.control_tick == 0 {
                let angular = TAU * self.hz / (self.rate * OVERSAMPLE as f64);
                let filter_delay = (self.loss_pole * angular.sin())
                    .atan2(1.0 - self.loss_pole * angular.cos())
                    / angular;
                // Empirical fundamental-mode compensation; phase loss is
                // removed separately. Pressure still influences sounding pitch.
                self.delay = (BORE_PERIODS * TAU / angular - filter_delay).max(2.0);
                self.dc_alpha = 1.0 - (-angular * 0.02).exp();
            }
            self.control_tick = (self.control_tick + 1) % 32;
            let wave = self.bore.read(self.delay);
            self.bore_loss = wave * (1.0 - self.loss_pole) + self.bore_loss * self.loss_pole;
            let reflected = -self.bore_loss;
            let inlet_noise = self.previous_noise
                + (noise - self.previous_noise) * (sub + 1) as f64 / OVERSAMPLE as f64;
            let inlet = self.pressure * 1.25 * (1.0 + inlet_noise * 0.002) - reflected * 0.5;
            let jet = self.jet.read(self.delay * 0.32);
            self.jet.push(inlet);
            // Smooth saturation avoids the sharp corner of a clipped cubic.
            let flow = (1.6 * jet * (jet * jet - 1.0)).tanh();
            self.jet_dc += (flow - self.jet_dc) * self.dc_alpha;
            let drive = (flow - self.jet_dc) * self.pressure;
            // |flow - jet_dc| <= 2; |reflection| <= 4. Convex delay/loss
            // and 0.5 reflection bound bore state by 4, including retuning.
            // Zero pressure removes energy injection, not the output signal.
            self.bore.push(drive + reflected * 0.5);
            let mut output = wave * 0.22;
            // Four positive one-poles before 4:1 decimation. This deliberately
            // dark prototype suppresses foldback; it is not alias-free audio FM.
            for state in &mut self.output_filter {
                *state += (output - *state) * self.output_alpha;
                output = *state;
            }
        }
        self.previous_noise = noise;
        self.output_filter[3] as f32
    }
}

impl AudioUnit for FlueUnit {
    fn reset(&mut self) {
        self.bore.reset();
        self.jet.reset();
        self.bore_loss = 0.0;
        self.jet_dc = 0.0;
        self.pressure = 0.0;
        self.hz = self.min_hz;
        self.output_filter = [0.0; 4];
        self.previous_noise = 0.0;
        self.control_tick = 0;
        self.initialized = false;
    }
    fn set_sample_rate(&mut self, rate: f64) {
        assert!(rate.is_finite() && rate > 0.0);
        self.rate = rate;
        let internal_rate = rate * OVERSAMPLE as f64;
        self.bore = Delay::new((1.52 * internal_rate / self.min_hz).ceil() as usize + 4);
        self.jet = Delay::new((0.50 * internal_rate / self.min_hz).ceil() as usize + 4);
        self.loss_pole = (-TAU * 4500.0_f64.min(rate * 0.2) / internal_rate).exp();
        self.pressure_alpha = 1.0 - (-1.0 / (internal_rate * 0.003)).exp();
        self.pitch_alpha = 1.0 - (-1.0 / (internal_rate * 0.020)).exp();
        self.output_alpha = 1.0 - (-TAU * 6000.0_f64.min(rate * 0.15) / internal_rate).exp();
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
        0x4150_5446_4c55_4520
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + (self.bore.data.capacity() + self.jet.data.capacity()) * std::mem::size_of::<f64>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fundsp::prelude32::BufferVec;

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }
    fn pitch(samples: &[f32], rate: f64) -> f64 {
        let crossings: Vec<f64> = samples
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair[0] < 0.0 && pair[1] >= 0.0)
            .map(|(i, pair)| i as f64 - f64::from(pair[0] / (pair[1] - pair[0])))
            .collect();
        assert!(crossings.len() > 5, "pipe never spoke");
        (crossings.len() - 1) as f64 * rate / (crossings.last().unwrap() - crossings[0])
    }

    #[test]
    fn pipe_speaks_in_tune_at_multiple_rates_and_registers() {
        for rate in [24_000.0, 44_100.0, 48_000.0, 96_000.0] {
            for hz in [55.0, 110.0, 220.0, 440.0, 660.0] {
                let mut pipe = FlueUnit::new(40.0);
                pipe.set_sample_rate(rate);
                let samples: Vec<_> = (0..(rate * 3.0) as usize)
                    .map(|_| pipe.sample(hz, 0.85, 0.0))
                    .collect();
                let steady = &samples[(rate * 2.0) as usize..];
                let frequency = pitch(steady, rate);
                let cents = 1200.0 * (frequency / f64::from(hz)).log2();
                assert!(
                    cents.abs() < 15.0,
                    "rate {rate}, requested {hz}, got {frequency} ({cents} cents)"
                );
                assert!(rms(steady) > 0.05);
                assert!(samples.iter().all(|x| x.is_finite() && x.abs() <= 0.88001));
            }
        }
    }

    #[test]
    fn pressure_drives_energy_and_closing_wind_drains_instead_of_hiding_it() {
        let mut pipe = FlueUnit::new(40.0);
        pipe.set_sample_rate(24_000.0);
        for _ in 0..24_000 {
            assert_eq!(pipe.sample(110.0, 0.0, 1.0), 0.0);
        }
        let low: Vec<_> = (0..48_000).map(|_| pipe.sample(110.0, 0.70, 0.0)).collect();
        let high: Vec<_> = (0..48_000).map(|_| pipe.sample(110.0, 0.95, 0.0)).collect();
        assert!(rms(&high[24_000..]) > 1.25 * rms(&low[24_000..]));
        let drain: Vec<_> = (0..48_000).map(|_| pipe.sample(110.0, 0.0, 0.0)).collect();
        assert!(
            rms(&drain[..120]) > 0.001,
            "stored vibration survives valve closure"
        );
        assert!(
            rms(&drain[36_000..]) < 1e-8,
            "closed pressure must dissipate energy"
        );
        let reopened: Vec<_> = (0..48_000).map(|_| pipe.sample(110.0, 0.85, 0.0)).collect();
        assert!(rms(&reopened[24_000..]) > 0.05);
    }

    #[test]
    fn quick_reopening_uses_old_vibration_without_allocating_another_pipe() {
        let mut pipe = FlueUnit::new(40.0);
        for _ in 0..44_100 {
            pipe.sample(110.0, 0.85, 0.0);
        }
        let footprint = pipe.footprint();
        for _ in 0..220 {
            pipe.sample(110.0, 0.0, 0.0);
        }
        let mut fresh = FlueUnit::new(40.0);
        let mut difference = 0.0;
        for _ in 0..4410 {
            difference += (pipe.sample(110.0, 0.85, 0.0) - fresh.sample(110.0, 0.85, 0.0)).abs();
        }
        assert!(difference > 1.0);
        assert_eq!(pipe.footprint(), footprint);
    }

    #[test]
    fn blocks_reset_and_extreme_controls_keep_bounded_state() {
        let mut tick = FlueUnit::new(20.0);
        let mut block = tick.clone();
        let mut input = BufferVec::new(3);
        let mut output = BufferVec::new(1);
        let footprint = tick.footprint();
        for size in [1, 17, 64] {
            tick.reset();
            block.reset();
            for start in (0..12_000).step_by(size) {
                for i in 0..size {
                    let time = (start + i) as f32;
                    input.buffer_mut().set_f32(0, i, 20.0 + time);
                    input
                        .buffer_mut()
                        .set_f32(1, i, if start < 6000 { 10.0 } else { -10.0 });
                    input.buffer_mut().set_f32(2, i, (time * 0.31).sin() * 10.0);
                }
                block.process(size, &input.buffer_ref(), &mut output.buffer_mut());
                for i in 0..size {
                    let value = tick.sample(
                        input.buffer_ref().at_f32(0, i),
                        input.buffer_ref().at_f32(1, i),
                        input.buffer_ref().at_f32(2, i),
                    );
                    assert_eq!(value, output.buffer_ref().at_f32(0, i));
                    assert!(value.is_finite() && value.abs() <= 0.88001);
                }
            }
        }
        for invalid in [f32::NAN, f32::INFINITY, -f32::INFINITY, f32::MAX] {
            for _ in 0..4410 {
                assert!(tick.sample(invalid, invalid, invalid).is_finite());
            }
        }
        assert_eq!(tick.footprint(), footprint);
        tick.reset();
        block.reset();
        for _ in 0..44_100 {
            assert_eq!(
                tick.sample(110.0, 0.85, 0.0),
                block.sample(110.0, 0.85, 0.0)
            );
        }
    }
}
