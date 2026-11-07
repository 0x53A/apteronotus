//! Sample-rate-aware, zero-added-latency partitioned convolution.
//! Preparation and rate changes allocate; rendering and reset never do.
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};

#[derive(Clone, Debug, PartialEq)]
pub struct ImpulseResponse {
    pub sample_rate: f64,
    pub samples: Vec<f32>,
}
impl ImpulseResponse {
    pub fn validate(&self) -> bool {
        self.sample_rate.is_finite()
            && (8_000.0..=192_000.0).contains(&self.sample_rate)
            && !self.samples.is_empty()
            && self.samples.len() <= (self.sample_rate * 10.0) as usize
            && self.samples.iter().all(|v| v.is_finite())
    }
    pub fn seconds(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate
    }
    pub fn at_rate(&self, rate: f64) -> Vec<f32> {
        resample_kernel(&self.samples, self.sample_rate, rate)
    }
}

/// Windowed-sinc resampling of filter coefficients, including rate-ratio gain.
/// Unlike resampling an audio recording this preserves filter DC gain.
pub fn resample_kernel(input: &[f32], from: f64, to: f64) -> Vec<f32> {
    if from == to {
        return input.to_vec();
    }
    assert!(from.is_finite() && to.is_finite() && from > 0.0 && to > 0.0);
    let ratio = to / from;
    let cutoff = ratio.min(1.0);
    let radius = 16.0 / cutoff;
    (0..(input.len() as f64 * ratio).ceil() as usize)
        .map(|i| {
            let center = i as f64 / ratio;
            let first = (center - radius).ceil().max(0.0) as usize;
            let last = ((center + radius).floor() as usize + 1).min(input.len());
            let mut value = 0.0;
            for (j, &sample) in input.iter().enumerate().take(last).skip(first) {
                let distance = center - j as f64;
                let x = std::f64::consts::PI * distance * cutoff;
                let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
                let window = 0.5 + 0.5 * (std::f64::consts::PI * distance / radius).cos();
                value += sample as f64 * cutoff * sinc * window / ratio;
            }
            value as f32
        })
        .collect()
}

#[derive(Clone)]
pub struct ConvolutionUnit {
    response: ImpulseResponse,
    rate: f64,
    kernel_len: usize,
    convolver: fft_convolver::FFTConvolver<f32>,
}
impl ConvolutionUnit {
    pub fn new(response: ImpulseResponse, rate: f64) -> Result<Self, &'static str> {
        if !response.validate() || !rate.is_finite() || !(8_000.0..=192_000.0).contains(&rate) {
            return Err("invalid impulse response or sample rate");
        }
        let samples = response.at_rate(rate);
        let mut convolver = fft_convolver::FFTConvolver::default();
        // Long responses benefit from larger partitions; the library computes
        // partial partitions immediately, retaining zero added block latency.
        convolver
            .init(if samples.len() > 4096 { 256 } else { 64 }, &samples)
            .expect("valid FFT partition");
        Ok(Self {
            kernel_len: samples.len(),
            response,
            rate,
            convolver,
        })
    }
}
impl AudioUnit for ConvolutionUnit {
    fn reset(&mut self) {
        self.convolver.reset();
    }
    fn set_sample_rate(&mut self, rate: f64) {
        if rate != self.rate && rate.is_finite() && (8_000.0..=192_000.0).contains(&rate) {
            let samples = self.response.at_rate(rate);
            self.kernel_len = samples.len();
            self.convolver
                .init(if samples.len() > 4096 { 256 } else { 64 }, &samples)
                .expect("valid FFT partition");
            self.rate = rate;
        }
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        self.convolver
            .process(input, output)
            .expect("prepared FFT buffers");
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        self.convolver
            .process(
                &input.channel_f32(0)[..size],
                &mut output.channel_f32_mut(0)[..size],
            )
            .expect("prepared FFT buffers");
    }
    fn inputs(&self) -> usize {
        1
    }
    fn outputs(&self) -> usize {
        1
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        let mut out = SignalFrame::new(1);
        out.fill(Signal::Unknown);
        out
    }
    fn get_id(&self) -> u64 {
        0x4150_5445_434f_4e56
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>() + self.response.samples.len() * 4 + self.kernel_len * 32 + 8192
    }
}
