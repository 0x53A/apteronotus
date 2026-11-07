//! Owned shoebox scene descriptions and a bounded binaural room processor.
//! All geometry, HRTF selection and convolution preparation happen off-thread.
use crate::convolution::{ConvolutionUnit, ImpulseResponse, resample_kernel};
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, BufferVec, Signal, SignalFrame};

pub type Vec3 = [f64; 3];
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Material {
    Wood,
    Plaster,
    Curtain,
    Stone,
}
impl Material {
    fn reflection(self) -> f64 {
        match self {
            Self::Wood => 0.46,
            Self::Plaster => 0.62,
            Self::Curtain => 0.20,
            Self::Stone => 0.75,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Listener {
    pub position: Vec3,
    pub facing: Vec3,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Emitter {
    pub name: String,
    pub position: Vec3,
}
#[derive(Clone, Debug, PartialEq)]
pub struct RoomSpec {
    pub size: Vec3,
    pub walls: Material,
    pub floor: Material,
    pub ceiling: Material,
    pub decay: f64,
    pub listener: Listener,
    pub emitters: Vec<Emitter>,
}
impl RoomSpec {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self
            .size
            .iter()
            .all(|x| x.is_finite() && (1.0..=50.0).contains(x))
        {
            return Err("room dimensions must be within 1..50 metres");
        }
        if !self.decay.is_finite() || !(0.05..=3.0).contains(&self.decay) {
            return Err("room decay must be within 0.05..3 seconds");
        }
        let inside = |p: Vec3| {
            p.iter()
                .zip(self.size)
                .all(|(p, s)| p.is_finite() && *p > 0.0 && *p < s)
        };
        if !inside(self.listener.position) {
            return Err("listener must be inside the room");
        }
        let f = self.listener.facing;
        if !f.iter().all(|x| x.is_finite()) || norm(f) < 1e-9 || norm(cross(f, [0., 0., 1.])) < 1e-6
        {
            return Err("listener facing must be finite, nonzero and not vertical");
        }
        if self.emitters.is_empty() || self.emitters.len() > 16 {
            return Err("room requires 1..16 mono emitters");
        }
        for (i, e) in self.emitters.iter().enumerate() {
            if e.name.is_empty() || self.emitters[..i].iter().any(|p| p.name == e.name) {
                return Err("room emitter names must be nonempty and unique");
            }
            if !inside(e.position) || norm(sub(e.position, self.listener.position)) < 0.25 {
                return Err(
                    "emitter must be inside the room and at least 0.25 metres from listener",
                );
            }
        }
        Ok(())
    }
    pub fn tail_seconds(&self) -> f64 {
        self.decay * 1.5 + norm(self.size) * 3.0 / 343.0 + 0.05
    }
    /// Conservative byte-equivalent delay-storage cost (including FFT partitions).
    pub fn storage_seconds(&self) -> f64 {
        16.0 * (self.emitters.len() as f64 * 2.0 * (norm(self.size) * 4.0 / 343.0 + 0.06)
            + 2.0 * self.tail_seconds())
    }
}
fn dot(a: Vec3, b: Vec3) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn norm(v: Vec3) -> f64 {
    dot(v, v).sqrt()
}
fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(v: Vec3, s: f64) -> Vec3 {
    [v[0] * s, v[1] * s, v[2] * s]
}
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
const DATA: &[u8] = include_bytes!("../assets/hrtf/kemar.bin");
fn u32_at(n: usize) -> usize {
    u32::from_le_bytes(DATA[n..n + 4].try_into().unwrap()) as usize
}
fn f32_at(n: usize) -> f32 {
    f32::from_le_bytes(DATA[n..n + 4].try_into().unwrap())
}
/// Select measured HRTFs in listener-local (forward,left,up) coordinates.
pub fn kemar(direction: Vec3, rate: f64) -> [Vec<f32>; 2] {
    let count = u32_at(4);
    let taps = u32_at(8);
    let original_rate = u32_at(12) as f64;
    let index = (0..count)
        .max_by(|a, b| {
            let score = |i: usize| {
                (0..3)
                    .map(|axis| f32_at(16 + i * 12 + axis * 4) as f64 * direction[axis])
                    .sum::<f64>()
            };
            score(*a).total_cmp(&score(*b))
        })
        .unwrap();
    std::array::from_fn(|ear| {
        let start = 16 + count * 12 + (index * 2 + ear) * taps * 4;
        let raw = (0..taps).map(|i| f32_at(start + i * 4)).collect::<Vec<_>>();
        resample_kernel(&raw, original_rate, rate)
    })
}
#[derive(Clone, Debug)]
pub struct PreparedEmitter {
    pub direct: [Vec<f32>; 2],
    pub early: [Vec<f32>; 2],
    pub distance: f64,
}
/// Deterministic direct and image-source responses. Exported for offline verification.
pub fn prepare_emitter(
    spec: &RoomSpec,
    index: usize,
    rate: f64,
) -> Result<PreparedEmitter, &'static str> {
    spec.validate()?;
    if index >= spec.emitters.len() || !rate.is_finite() || !(8000.0..=192000.0).contains(&rate) {
        return Err("invalid emitter index or sample rate");
    }
    let forward = scale(spec.listener.facing, 1.0 / norm(spec.listener.facing));
    let left = scale(
        cross([0., 0., 1.], forward),
        1.0 / norm(cross([0., 0., 1.], forward)),
    );
    let up = cross(forward, left);
    let response = |point: Vec3| {
        let v = sub(point, spec.listener.position);
        let d = norm(v);
        (
            kemar(
                [dot(v, forward) / d, dot(v, left) / d, dot(v, up) / d],
                rate,
            ),
            d,
        )
    };
    let source = spec.emitters[index].position;
    let (h, d) = response(source);
    let offset = (d / 343.0 * rate).round() as usize;
    let mut direct = std::array::from_fn(|_| vec![0.; offset + h[0].len()]);
    for ear in 0..2 {
        for (i, &v) in h[ear].iter().enumerate() {
            direct[ear][offset + i] = v * (2.7 / d) as f32;
        }
    }
    let length = ((norm(spec.size) * 3.0 / 343.0 + 0.04) * rate).ceil() as usize;
    let mut early: [Vec<f32>; 2] = std::array::from_fn(|_| vec![0.; length]);
    for x in -1_i32..=1 {
        for y in -1_i32..=1 {
            for z in -1_i32..=1 {
                let indices = [x, y, z];
                let order = x.abs() + y.abs() + z.abs();
                if order == 0 || order > 2 {
                    continue;
                }
                let mut image = source;
                let mut gain = 1.;
                for axis in 0..3 {
                    if indices[axis] == 0 {
                        continue;
                    }
                    image[axis] = if indices[axis] < 0 {
                        -source[axis]
                    } else {
                        2. * spec.size[axis] - source[axis]
                    };
                    gain *= if axis == 2 {
                        if indices[axis] < 0 {
                            spec.floor
                        } else {
                            spec.ceiling
                        }
                    } else {
                        spec.walls
                    }
                    .reflection();
                }
                let (mut rh, rd) = response(image);
                let delay = (rd / 343. * rate).round() as usize;
                let coefficient = (1.
                    - (-2. * std::f64::consts::PI * (4000. / (1. + 0.4 * (order - 1) as f64))
                        / rate)
                        .exp()) as f32;
                for ear in 0..2 {
                    let mut low = 0.;
                    for (i, v) in rh[ear].iter_mut().enumerate() {
                        low += coefficient * (*v - low);
                        if delay + i < length {
                            early[ear][delay + i] += low * (gain * 2.7 / rd) as f32;
                        }
                    }
                }
            }
        }
    }
    Ok(PreparedEmitter {
        direct,
        early,
        distance: d,
    })
}

#[derive(Clone)]
struct SourceUnit {
    direct: [ConvolutionUnit; 2],
    early: [ConvolutionUnit; 2],
    distance_gain: f32,
}
#[derive(Clone)]
pub struct RoomUnit {
    spec: RoomSpec,
    rate: f64,
    sources: Vec<SourceUnit>,
    late: [ConvolutionUnit; 2],
    input: BufferVec,
    scratch: BufferVec,
    direct: BufferVec,
    early: BufferVec,
    late_input: BufferVec,
    late_output: BufferVec,
    side_low: f32,
    smoothed: [f32; 5],
    controls_ready: bool,
    control_alpha: f32,
    side_alpha: f32,
}
impl RoomUnit {
    /// Inputs: one mono channel per emitter, then direct / early / late gains,
    /// room amount, and low-frequency room Side retention (0..1).
    pub fn new(spec: RoomSpec, rate: f64) -> Result<Self, &'static str> {
        spec.validate()?;
        if !rate.is_finite() || !(8000.0..=192000.0).contains(&rate) {
            return Err("invalid room sample rate");
        }
        let convolver = |samples: Vec<f32>| {
            ConvolutionUnit::new(
                ImpulseResponse {
                    sample_rate: rate,
                    samples,
                },
                rate,
            )
            .unwrap()
        };
        let mut sources = Vec::new();
        for i in 0..spec.emitters.len() {
            let response = prepare_emitter(&spec, i, rate)?;
            sources.push(SourceUnit {
                direct: response.direct.map(&convolver),
                early: response.early.map(&convolver),
                distance_gain: (2.7 / response.distance) as f32,
            });
        }
        // One shared diffuse field: deterministic impulse responses, not one
        // independent late reverb per instrument. Early paths establish position.
        let length = (spec.tail_seconds() * rate).ceil() as usize;
        let late = std::array::from_fn(|ear| {
            let mut seed = 0x193afc7_u64 + ear as u64;
            let mut low = 0.;
            let alpha = (1. - (-2. * std::f64::consts::PI * 2700. / rate).exp()) as f32;
            let mut impulse = vec![0.; length];
            for (i, v) in impulse.iter_mut().enumerate() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let noise = ((seed >> 40) as f32 / 8388608. - 1.) as f32;
                low += alpha * (noise - low);
                let t = i as f64 / rate;
                *v = low
                    * ((-std::f64::consts::LN_10 * 3. * (t - 0.045).max(0.) / spec.decay).exp()
                        * ((t - 0.035) / 0.025).clamp(0., 1.)) as f32;
            }
            let energy = impulse.iter().map(|v| v * v).sum::<f32>();
            let gain = (0.065 / energy.max(1e-20)).sqrt();
            for v in &mut impulse {
                *v *= gain;
            }
            convolver(impulse)
        });
        Ok(Self {
            spec,
            rate,
            sources,
            late,
            input: BufferVec::new(1),
            scratch: BufferVec::new(1),
            direct: BufferVec::new(2),
            early: BufferVec::new(2),
            late_input: BufferVec::new(1),
            late_output: BufferVec::new(2),
            side_low: 0.,
            smoothed: [0.; 5],
            controls_ready: false,
            control_alpha: (1. - (-1. / (rate * 0.02)).exp()) as f32,
            side_alpha: (1. - (-2. * std::f64::consts::PI * 250. / rate).exp()) as f32,
        })
    }
    pub fn response_seconds(&self) -> f64 {
        self.spec.tail_seconds()
    }
    fn block(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for i in 0..size {
            self.late_input.buffer_mut().set_f32(0, i, 0.);
            for ear in 0..2 {
                self.direct.buffer_mut().set_f32(ear, i, 0.);
                self.early.buffer_mut().set_f32(ear, i, 0.);
            }
        }
        for (channel, source) in self.sources.iter_mut().enumerate() {
            for i in 0..size {
                let x = input.at_f32(channel, i);
                self.input.buffer_mut().set_f32(0, i, x);
                let prev = self.late_input.buffer_ref().at_f32(0, i);
                self.late_input
                    .buffer_mut()
                    .set_f32(0, i, prev + x * source.distance_gain);
            }
            for ear in 0..2 {
                source.direct[ear].process(
                    size,
                    &self.input.buffer_ref(),
                    &mut self.scratch.buffer_mut(),
                );
                for i in 0..size {
                    let prev = self.direct.buffer_ref().at_f32(ear, i);
                    self.direct.buffer_mut().set_f32(
                        ear,
                        i,
                        prev + self.scratch.buffer_ref().at_f32(0, i),
                    );
                }
                source.early[ear].process(
                    size,
                    &self.input.buffer_ref(),
                    &mut self.scratch.buffer_mut(),
                );
                for i in 0..size {
                    let prev = self.early.buffer_ref().at_f32(ear, i);
                    self.early.buffer_mut().set_f32(
                        ear,
                        i,
                        prev + self.scratch.buffer_ref().at_f32(0, i),
                    );
                }
            }
        }
        for ear in 0..2 {
            self.late[ear].process(
                size,
                &self.late_input.buffer_ref(),
                &mut self.scratch.buffer_mut(),
            );
            for i in 0..size {
                self.late_output.buffer_mut().set_f32(
                    ear,
                    i,
                    self.scratch.buffer_ref().at_f32(0, i),
                );
            }
        }
        let n = self.sources.len();
        for i in 0..size {
            let safe = |v: f32, max: f32| if v.is_finite() { v.clamp(0., max) } else { 0. };
            for ch in 0..5 {
                let target = safe(input.at_f32(n + ch, i), if ch < 3 { 2. } else { 1. });
                if !self.controls_ready {
                    self.smoothed[ch] = target;
                } else {
                    self.smoothed[ch] += self.control_alpha * (target - self.smoothed[ch]);
                }
            }
            self.controls_ready = true;
            let [dg, eg, lg, amount, width] = self.smoothed;
            let l = self.early.buffer_ref().at_f32(0, i) * eg
                + self.late_output.buffer_ref().at_f32(0, i) * lg;
            let r = self.early.buffer_ref().at_f32(1, i) * eg
                + self.late_output.buffer_ref().at_f32(1, i) * lg;
            let mid = (l + r) * 0.5;
            let side = (l - r) * 0.5;
            self.side_low += self.side_alpha * (side - self.side_low);
            let side = side - (1. - width) * self.side_low;
            output.set_f32(
                0,
                i,
                self.direct.buffer_ref().at_f32(0, i) * dg + (mid + side) * amount,
            );
            output.set_f32(
                1,
                i,
                self.direct.buffer_ref().at_f32(1, i) * dg + (mid - side) * amount,
            );
        }
    }
}
impl AudioUnit for RoomUnit {
    fn reset(&mut self) {
        for s in &mut self.sources {
            for unit in s.direct.iter_mut().chain(s.early.iter_mut()) {
                unit.reset();
            }
        }
        for unit in &mut self.late {
            unit.reset();
        }
        self.side_low = 0.;
        self.controls_ready = false;
    }
    fn set_sample_rate(&mut self, rate: f64) {
        if rate != self.rate && rate.is_finite() && (8000.0..=192000.0).contains(&rate) {
            *self = Self::new(self.spec.clone(), rate).expect("validated room");
        }
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        // Fixed stack buffers: sample-mode is correct but block mode is cheaper.
        let mut ins = fundsp::prelude32::BufferArray::<fundsp::prelude32::U32>::new();
        let mut outs = fundsp::prelude32::BufferArray::<fundsp::prelude32::U2>::new();
        for (i, v) in input.iter().enumerate() {
            ins.buffer_mut().set_f32(i, 0, *v);
        }
        self.block(1, &ins.buffer_ref(), &mut outs.buffer_mut());
        output[0] = outs.buffer_ref().at_f32(0, 0);
        output[1] = outs.buffer_ref().at_f32(1, 0);
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        self.block(size, input, output);
    }
    fn inputs(&self) -> usize {
        self.sources.len() + 5
    }
    fn outputs(&self) -> usize {
        2
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        let mut out = SignalFrame::new(2);
        out.fill(Signal::Unknown);
        out
    }
    fn get_id(&self) -> u64 {
        0x4150_5445_524f_4f4d
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .sources
                .iter()
                .flat_map(|s| s.direct.iter().chain(s.early.iter()))
                .map(AudioUnit::footprint)
                .sum::<usize>()
            + self.late.iter().map(AudioUnit::footprint).sum::<usize>()
    }
}
