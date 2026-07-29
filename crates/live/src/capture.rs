//! Lock-free audio capture transfer from a device callback to the DSP graph.
//!
//! CPAL may deliver input and output on different realtime threads. The
//! producer writes complete frames into a preallocated ring and publishes each
//! slot with a sequence number; the consumer keeps a small latency margin,
//! detects overwrite/underflow, and emits silence rather than blocking.

use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const DEFAULT_LATENCY_FRAMES: u64 = 512;

struct CaptureRing {
    channels: usize,
    capacity_frames: u64,
    samples: Box<[AtomicU32]>,
    sequences: Box<[AtomicU64]>,
    published_frames: AtomicU64,
}

impl CaptureRing {
    #[cfg(not(target_arch = "wasm32"))]
    fn new(channels: usize, capacity_frames: usize) -> CaptureRing {
        assert!(channels > 0);
        assert!(capacity_frames > DEFAULT_LATENCY_FRAMES as usize);
        CaptureRing {
            channels,
            capacity_frames: capacity_frames as u64,
            samples: (0..channels * capacity_frames)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            sequences: (0..capacity_frames).map(|_| AtomicU64::new(0)).collect(),
            published_frames: AtomicU64::new(0),
        }
    }
}

/// Single producer owned by the input-device callback.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct CaptureProducer {
    ring: Arc<CaptureRing>,
    next_frame: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl CaptureProducer {
    /// Publish one complete frame without allocating.
    pub(crate) fn push_frame(&mut self, mut sample: impl FnMut(usize) -> f32) {
        let frame = self.next_frame;
        let slot = (frame % self.ring.capacity_frames) as usize;
        let writing = frame.wrapping_mul(2).wrapping_add(1);
        let complete = writing.wrapping_add(1);
        self.ring.sequences[slot].store(writing, Ordering::Release);
        let base = slot * self.ring.channels;
        for channel in 0..self.ring.channels {
            self.ring.samples[base + channel].store(sample(channel).to_bits(), Ordering::Relaxed);
        }
        self.ring.sequences[slot].store(complete, Ordering::Release);
        self.next_frame = frame.wrapping_add(1);
        self.ring
            .published_frames
            .store(self.next_frame, Ordering::Release);
    }
}

/// Multi-channel realtime source owned by the output graph.
#[derive(Clone)]
pub(crate) struct CaptureUnit {
    ring: Arc<CaptureRing>,
    next_frame: Option<u64>,
    scratch: Vec<f32>,
}

impl CaptureUnit {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn pair(channels: usize, capacity_frames: usize) -> (CaptureProducer, CaptureUnit) {
        let ring = Arc::new(CaptureRing::new(channels, capacity_frames));
        (
            CaptureProducer {
                ring: Arc::clone(&ring),
                next_frame: 0,
            },
            CaptureUnit {
                ring,
                next_frame: None,
                scratch: vec![0.0; channels],
            },
        )
    }

    fn read_frame(ring: &CaptureRing, next_frame: &mut Option<u64>, output: &mut [f32]) {
        output.fill(0.0);
        let published = ring.published_frames.load(Ordering::Acquire);
        let Some(mut frame) = *next_frame else {
            if published < DEFAULT_LATENCY_FRAMES {
                return;
            }
            *next_frame = Some(published - DEFAULT_LATENCY_FRAMES);
            return;
        };

        if published.saturating_sub(frame) > ring.capacity_frames {
            frame = published.saturating_sub(DEFAULT_LATENCY_FRAMES);
            *next_frame = Some(frame);
        }
        if frame >= published {
            return;
        }

        let slot = (frame % ring.capacity_frames) as usize;
        let expected = frame.wrapping_mul(2).wrapping_add(2);
        if ring.sequences[slot].load(Ordering::Acquire) != expected {
            return;
        }
        let base = slot * ring.channels;
        for (channel, value) in output.iter_mut().enumerate() {
            *value = f32::from_bits(ring.samples[base + channel].load(Ordering::Relaxed));
        }
        if ring.sequences[slot].load(Ordering::Acquire) != expected {
            output.fill(0.0);
            return;
        }
        *next_frame = Some(frame.wrapping_add(1));
    }
}

impl AudioUnit for CaptureUnit {
    fn reset(&mut self) {
        self.next_frame = None;
    }

    fn tick(&mut self, _input: &[f32], output: &mut [f32]) {
        Self::read_frame(&self.ring, &mut self.next_frame, output);
    }

    fn process(&mut self, size: usize, _input: &BufferRef, output: &mut BufferMut) {
        for sample in 0..size {
            Self::read_frame(&self.ring, &mut self.next_frame, &mut self.scratch);
            for (channel, value) in self.scratch.iter().enumerate() {
                output.set_f32(channel, sample, *value);
            }
        }
    }

    fn inputs(&self) -> usize {
        0
    }

    fn outputs(&self) -> usize {
        self.ring.channels
    }

    fn route(&mut self, _input: &SignalFrame, _frequency: f64) -> SignalFrame {
        let mut output = SignalFrame::new(self.ring.channels);
        output.fill(Signal::Unknown);
        output
    }

    fn get_id(&self) -> u64 {
        0x4150_5445_4341_5054
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.ring.samples.len() * std::mem::size_of::<AtomicU32>()
            + self.ring.sequences.len() * std::mem::size_of::<AtomicU64>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_frames_cross_the_ring_and_overwrite_is_detected() {
        let (mut producer, mut consumer) = CaptureUnit::pair(2, 1_024);
        for frame in 0..DEFAULT_LATENCY_FRAMES {
            producer.push_frame(|channel| frame as f32 + channel as f32 * 0.25);
        }

        let mut output = [1.0, 1.0];
        consumer.tick(&[], &mut output);
        assert_eq!(output, [0.0, 0.0]);
        producer.push_frame(|channel| 512.0 + channel as f32 * 0.25);
        consumer.tick(&[], &mut output);
        assert_eq!(output, [0.0, 0.25]);

        for frame in 513..2_000 {
            producer.push_frame(|channel| frame as f32 + channel as f32 * 0.25);
        }
        consumer.tick(&[], &mut output);
        assert!(output[0] >= 1_488.0);
        assert_eq!(output[1] - output[0], 0.25);
    }
}
