//! Prepared stereo hall: geometric reflection taps and a passive sixteen-line
//! feedback-delay network. Geometry and absorption are prepared off-thread;
//! only reflected paths are damped. The output excludes the direct signal.
//! Delays are prepared sample counts at one explicit clock. A host changing
//! sample rate must prepare a replacement off-thread, not retune live history.
#[derive(Clone)]
/// Retained room history. Construction allocates; tick and reset do not.
pub struct Hall {
    sample_rate: f64,
    buffers: [Vec<f64>; 16],
    positions: [usize; 16],
    lowpass: [f64; 16],
    gains: [f64; 16],
    poles: [f64; 16],
    injection: [usize; 16],
    history: Vec<f64>,
    cursor: usize,
    taps: Vec<(usize, [f64; 2])>,
    late_gain: f64,
}
impl Hall {
    /// Prepare sample-count delays, per-line feedback gains and absorption poles,
    /// staggered source injection delays, and stereo early-reflection taps.
    /// Feedback uses an orthogonal Hadamard matrix; gains and one-pole filters
    /// are contractive. Early and late output gains do not enter the feedback.
    pub fn new(
        sample_rate: f64,
        lengths: [usize; 16],
        gains: [f64; 16],
        poles: [f64; 16],
        injection: [usize; 16],
        taps: Vec<(usize, [f64; 2])>,
        late_gain: f64,
    ) -> Result<Self, &'static str> {
        if !sample_rate.is_finite()
            || !(8000.0..=192000.0).contains(&sample_rate)
            || lengths.iter().any(|n| *n == 0 || *n > 192000)
            || gains.iter().any(|g| !g.is_finite() || *g < 0. || *g >= 1.)
            || poles.iter().any(|p| !p.is_finite() || *p < 0. || *p >= 1.)
            || injection.iter().any(|n| *n > 192000)
            || taps.len() > 512
            || taps
                .iter()
                .any(|(n, g)| *n > 192000 || g.iter().any(|g| !g.is_finite()))
            || !late_gain.is_finite()
            || late_gain < 0.
        {
            return Err("invalid prepared hall");
        }
        let history_len = taps.iter().map(|t| t.0).chain(injection).max().unwrap() + 1;
        Ok(Self {
            sample_rate,
            buffers: std::array::from_fn(|i| vec![0.; lengths[i]]),
            positions: [0; 16],
            lowpass: [0.; 16],
            gains,
            poles,
            injection,
            history: vec![0.; history_len],
            cursor: 0,
            taps,
            late_gain,
        })
    }
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }
    pub fn reset(&mut self) {
        for b in &mut self.buffers {
            b.fill(0.);
        }
        self.positions.fill(0);
        self.lowpass.fill(0.);
        self.history.fill(0.);
        self.cursor = 0;
    }
    fn delayed(&self, n: usize) -> f64 {
        let i = if n <= self.cursor {
            self.cursor - n
        } else {
            self.history.len() - (n - self.cursor)
        };
        self.history[i]
    }
    /// Advance one sample at the prepared rate. The caller supplies finite mono
    /// pressure; the returned stereo signal contains reflections only.
    pub fn tick(&mut self, input: f64) -> [f64; 2] {
        self.history[self.cursor] = input;
        let mut late = [0.; 2];
        let mut mixed = [0.; 16];
        for i in 0..16 {
            let x = self.buffers[i][self.positions[i]];
            self.lowpass[i] = (1. - self.poles[i]) * x + self.poles[i] * self.lowpass[i];
            mixed[i] = self.gains[i] * self.lowpass[i];
            late[0] += sign(1, i) * x;
            late[1] += sign(7, i) * x;
        }
        // Orthogonal normalized Hadamard transform. Independent sums are
        // reassociated relative to the Python dense matrix, not approximated.
        for width in [1, 2, 4, 8] {
            for start in (0..16).step_by(width * 2) {
                for j in 0..width {
                    let a = mixed[start + j];
                    let b = mixed[start + width + j];
                    mixed[start + j] = a + b;
                    mixed[start + width + j] = a - b;
                }
            }
        }
        for i in 0..16 {
            self.buffers[i][self.positions[i]] =
                0.25 * mixed[i] + sign(3, i) * self.delayed(self.injection[i]);
            self.positions[i] += 1;
            if self.positions[i] == self.buffers[i].len() {
                self.positions[i] = 0;
            }
        }
        let mut out = late.map(|v| v * self.late_gain);
        for &(n, gain) in &self.taps {
            let x = self.delayed(n);
            out[0] += gain[0] * x;
            out[1] += gain[1] * x;
        }
        self.cursor += 1;
        if self.cursor == self.history.len() {
            self.cursor = 0;
        }
        out
    }
}
fn sign(row: usize, column: usize) -> f64 {
    if (row & column).count_ones() % 2 == 0 {
        0.25
    } else {
        -0.25
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_reflection_and_reset_preserve_sample_coordinates() {
        let mut h = Hall::new(
            48000.,
            [5; 16],
            [0.; 16],
            [0.; 16],
            [3; 16],
            vec![(2, [0.5, -0.25])],
            0.,
        )
        .unwrap();
        assert_eq!(h.sample_rate(), 48000.);
        assert_eq!(h.tick(1.), [0.; 2]);
        assert_eq!(h.tick(0.), [0.; 2]);
        assert_eq!(h.tick(0.), [0.5, -0.25]);
        h.reset();
        for _ in 0..100 {
            assert_eq!(h.tick(0.), [0.; 2]);
        }
        assert!(Hall::new(48000., [0; 16], [0.; 16], [0.; 16], [0; 16], vec![], 1.).is_err());
        assert!(Hall::new(48000., [5; 16], [1.; 16], [0.; 16], [0; 16], vec![], 1.).is_err());
        assert!(Hall::new(f64::NAN, [5; 16], [0.; 16], [0.; 16], [0; 16], vec![], 1.).is_err());
    }
}

#[path="concert_hall_preset.rs"]
pub mod preset;

/// Fixed-clock fundsp adapter for the prepared audition hall. Off-thread
/// construction allocates; rate notification, processing and reset do not.
#[derive(Clone)]
pub struct HallUnit {
    hall: Hall,
    compatible: bool,
}
impl HallUnit {
    pub fn new(rate:f64) -> Result<Self,&'static str> {
        Ok(Self{hall:preset::prepare(rate)?,compatible:true})
    }
}
impl fundsp::prelude32::AudioUnit for HallUnit {
    fn reset(&mut self) { self.hall.reset(); }
    fn set_sample_rate(&mut self, rate:f64) {
        let compatible=rate==self.hall.sample_rate();
        if compatible!=self.compatible {self.hall.reset();}
        self.compatible=compatible;
    }
    fn tick(&mut self,input:&[f32],output:&mut[f32]) {
        if self.compatible {
            let wet=self.hall.tick(input[0] as f64);
            output[0]=wet[0] as f32;output[1]=wet[1] as f32;
        } else {output.fill(0.);}
    }
    fn process(&mut self,size:usize,input:&fundsp::prelude32::BufferRef,output:&mut fundsp::prelude32::BufferMut) {
        for i in 0..size {
            let mut wet=[0.;2];
            self.tick(&[input.at_f32(0,i)],&mut wet);
            output.set_f32(0,i,wet[0]);output.set_f32(1,i,wet[1]);
        }
    }
    fn inputs(&self)->usize {1}
    fn outputs(&self)->usize {2}
    fn route(&mut self,_:&fundsp::prelude32::SignalFrame,_:f64)->fundsp::prelude32::SignalFrame {
        let mut out=fundsp::prelude32::SignalFrame::new(2);
        out.fill(fundsp::prelude32::Signal::Unknown);out
    }
    fn get_id(&self)->u64 {0x4150_5445_4841_4c4c}
    fn footprint(&self)->usize {
        std::mem::size_of::<Self>() + self.hall.buffers.iter().map(|b|b.capacity()*8).sum::<usize>()
            + self.hall.history.capacity()*8 + self.hall.taps.capacity()*std::mem::size_of::<(usize,[f64;2])>()
    }
}
