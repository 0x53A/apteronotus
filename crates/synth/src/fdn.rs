//! A compact modulated Hadamard feedback-delay network.
//!
//! Construction and sample-rate changes allocate delay storage on the control
//! thread. [`FdnUnit::tick`] and [`FdnUnit::process`] only mutate preallocated
//! buffers, which keeps persistent send returns safe on the audio thread.

use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};

/// Mono feedback-delay network with a live T60 input.
#[derive(Clone)]
pub(crate) struct FdnUnit {
    delays: Vec<f64>,
    damping: f32,
    modulation_rate: f64,
    modulation_depth: f64,
    sample_rate: f64,
    buffers: Vec<Vec<f32>>,
    positions: Vec<usize>,
    damping_state: Vec<f32>,
    delayed: Vec<f32>,
    mixed: Vec<f32>,
    sample_clock: u64,
}

impl FdnUnit {
    pub(crate) fn new(
        delays: Vec<f64>,
        damping: f64,
        modulation_rate: f64,
        modulation_depth: f64,
    ) -> Self {
        let lines = delays.len();
        let mut unit = Self {
            delays,
            damping: damping as f32,
            modulation_rate,
            modulation_depth,
            sample_rate: 44_100.0,
            buffers: Vec::new(),
            positions: vec![0; lines],
            damping_state: vec![0.0; lines],
            delayed: vec![0.0; lines],
            mixed: vec![0.0; lines],
            sample_clock: 0,
        };
        unit.allocate_delays();
        unit
    }

    fn allocate_delays(&mut self) {
        self.buffers = self
            .delays
            .iter()
            .map(|delay| {
                let samples =
                    ((delay + self.modulation_depth) * self.sample_rate).ceil() as usize + 3;
                vec![0.0; samples.max(4)]
            })
            .collect();
        self.positions.fill(0);
        self.damping_state.fill(0.0);
        self.delayed.fill(0.0);
        self.mixed.fill(0.0);
        self.sample_clock = 0;
    }

    #[inline]
    fn read_line(&self, line: usize, delay_samples: f64) -> f32 {
        let buffer = &self.buffers[line];
        let length = buffer.len() as f64;
        let mut position = self.positions[line] as f64 - delay_samples;
        position = position.rem_euclid(length);
        let lower = position.floor() as usize;
        let upper = (lower + 1) % buffer.len();
        let fraction = (position - lower as f64) as f32;
        buffer[lower] + (buffer[upper] - buffer[lower]) * fraction
    }

    fn hadamard(values: &mut [f32]) {
        let mut width = 1;
        while width < values.len() {
            for start in (0..values.len()).step_by(width * 2) {
                for offset in 0..width {
                    let a = values[start + offset];
                    let b = values[start + width + offset];
                    values[start + offset] = a + b;
                    values[start + width + offset] = a - b;
                }
            }
            width *= 2;
        }
        let scale = 1.0 / (values.len() as f32).sqrt();
        for value in values {
            *value *= scale;
        }
    }

    #[inline]
    fn process_sample(&mut self, input: f32, t60: f32) -> f32 {
        let time = self.sample_clock as f64 / self.sample_rate;
        let lines = self.delays.len();
        let tau = std::f64::consts::TAU;
        for line in 0..lines {
            let phase = line as f64 / lines as f64;
            let modulation =
                self.modulation_depth * (tau * (self.modulation_rate * time + phase)).sin();
            let seconds = (self.delays[line] + modulation).max(1.0 / self.sample_rate);
            let sample = self.read_line(line, seconds * self.sample_rate);
            let damped = sample * (1.0 - self.damping) + self.damping_state[line] * self.damping;
            self.damping_state[line] = damped;
            self.delayed[line] = sample;
            self.mixed[line] = damped;
        }

        Self::hadamard(&mut self.mixed);
        let t60 = if t60.is_finite() {
            t60.clamp(0.02, 120.0)
        } else {
            1.0
        };
        let injection = input / (lines as f32).sqrt();
        for line in 0..lines {
            // Three decades of decay in T60 seconds, adjusted for each line's
            // propagation time so unequal delays share the same decay time.
            let feedback = 0.001_f32.powf(self.delays[line] as f32 / t60);
            let write = injection + self.mixed[line] * feedback;
            self.buffers[line][self.positions[line]] = if write.is_finite() { write } else { 0.0 };
            self.positions[line] = (self.positions[line] + 1) % self.buffers[line].len();
        }
        self.sample_clock = self.sample_clock.wrapping_add(1);
        self.delayed.iter().copied().sum::<f32>() / (lines as f32).sqrt()
    }
}

impl AudioUnit for FdnUnit {
    fn reset(&mut self) {
        for buffer in &mut self.buffers {
            buffer.fill(0.0);
        }
        self.positions.fill(0);
        self.damping_state.fill(0.0);
        self.delayed.fill(0.0);
        self.mixed.fill(0.0);
        self.sample_clock = 0;
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        if sample_rate.is_finite() && sample_rate > 0.0 && sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.allocate_delays();
        }
    }

    #[inline]
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        output[0] = self.process_sample(input[0], input[1]);
    }

    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for sample in 0..size {
            let value = self.process_sample(input.at_f32(0, sample), input.at_f32(1, sample));
            output.set_f32(0, sample, value);
        }
    }

    fn inputs(&self) -> usize {
        2
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
        0x4150_5445_524f_4644
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .buffers
                .iter()
                .map(|buffer| buffer.capacity() * std::mem::size_of::<f32>())
                .sum::<usize>()
    }
}
