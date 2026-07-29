//! cpal output for a fundsp sequencer frontend/backend pair.
//!
//! Graph construction and voice instantiation stay on the caller's control
//! thread. The device callback owns only fundsp's realtime backend and
//! preallocated SIMD buffers. It renders through `AudioUnit::process` in
//! fundsp's native 64-frame blocks; calling `tick` once per device frame gives
//! the same samples but forfeits the DSP layer's SIMD path.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Device, FromSample, I24, OutputCallbackInfo, Sample, SampleFormat, SizedSample,
    Stream, StreamConfig, SupportedBufferSize, U24,
};
use fundsp::net::Net;
use fundsp::prelude32::{AudioUnit, BufferVec, MAX_BUFFER_SIZE, ReplayMode, Sequencer};

/// About 10.7 ms at 48 kHz: enough headroom for a desktop UI without turning
/// note input into an obviously sluggish instrument later.
#[cfg(not(target_arch = "wasm32"))]
const TARGET_BUFFER_FRAMES: u32 = 512;

pub struct AudioOutput {
    sequencer: Sequencer,
    stream: Stream,
    sample_rate: f64,
    device_channels: usize,
}

impl AudioOutput {
    /// Open the default output device. The returned stream is paused, so the
    /// caller can fill its initial lookahead before calling [`play`](Self::play).
    pub fn open(synth_channels: usize) -> Result<AudioOutput, OutputError> {
        Self::open_inner(synth_channels, synth_channels, None)
    }

    /// Open an output whose sequencer emits routed stems through one persistent
    /// processor before the main lanes reach the device.
    pub fn open_processed(
        stem_channels: usize,
        main_channels: usize,
        processor: Box<dyn AudioUnit>,
    ) -> Result<AudioOutput, OutputError> {
        if processor.inputs() != stem_channels {
            return Err(OutputError::ProcessorInputChannels {
                expected: stem_channels,
                found: processor.inputs(),
            });
        }
        if processor.outputs() != stem_channels {
            return Err(OutputError::ProcessorOutputChannels {
                expected: stem_channels,
                found: processor.outputs(),
            });
        }
        if main_channels > stem_channels {
            return Err(OutputError::MainChannels {
                stems: stem_channels,
                main: main_channels,
            });
        }
        Self::open_inner(stem_channels, main_channels, Some(processor))
    }

    fn open_inner(
        sequencer_channels: usize,
        synth_channels: usize,
        processor: Option<Box<dyn AudioUnit>>,
    ) -> Result<AudioOutput, OutputError> {
        if sequencer_channels == 0 || synth_channels == 0 {
            return Err(OutputError::NoSynthChannels);
        }
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(OutputError::NoOutputDevice)?;
        let supported = device
            .default_output_config()
            .map_err(OutputError::DefaultConfig)?;
        let sample_format = supported.sample_format();
        let mut config = supported.config();
        config.buffer_size = preferred_buffer_size(supported.buffer_size());
        let sample_rate = config.sample_rate as f64;
        let device_channels = config.channels as usize;

        let mut sequencer = Sequencer::new(0, sequencer_channels, ReplayMode::None);
        sequencer.set_sample_rate(sample_rate);
        sequencer.allocate();
        let backend = sequencer.backend();
        let renderer: Box<dyn AudioUnit> = match processor {
            Some(processor) => {
                let mut net = Net::new(0, synth_channels);
                let voices = net.push(Box::new(backend));
                let persistent = net.push(processor);
                for channel in 0..sequencer_channels {
                    net.connect(voices, channel, persistent, channel);
                }
                for channel in 0..synth_channels {
                    net.connect_output(persistent, channel, channel);
                }
                Box::new(net)
            }
            None => Box::new(backend),
        };
        let stream = match sample_format {
            SampleFormat::I8 => build_stream::<i8>(&device, config, renderer, synth_channels),
            SampleFormat::I16 => build_stream::<i16>(&device, config, renderer, synth_channels),
            SampleFormat::I24 => build_stream::<I24>(&device, config, renderer, synth_channels),
            SampleFormat::I32 => build_stream::<i32>(&device, config, renderer, synth_channels),
            SampleFormat::I64 => build_stream::<i64>(&device, config, renderer, synth_channels),
            SampleFormat::U8 => build_stream::<u8>(&device, config, renderer, synth_channels),
            SampleFormat::U16 => build_stream::<u16>(&device, config, renderer, synth_channels),
            SampleFormat::U24 => build_stream::<U24>(&device, config, renderer, synth_channels),
            SampleFormat::U32 => build_stream::<u32>(&device, config, renderer, synth_channels),
            SampleFormat::U64 => build_stream::<u64>(&device, config, renderer, synth_channels),
            SampleFormat::F32 => build_stream::<f32>(&device, config, renderer, synth_channels),
            SampleFormat::F64 => build_stream::<f64>(&device, config, renderer, synth_channels),
            other => return Err(OutputError::UnsupportedSampleFormat(other)),
        }
        .map_err(OutputError::BuildStream)?;

        Ok(AudioOutput {
            sequencer,
            stream,
            sample_rate,
            device_channels,
        })
    }

    pub fn sequencer_mut(&mut self) -> &mut Sequencer {
        &mut self.sequencer
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn device_channels(&self) -> usize {
        self.device_channels
    }

    pub fn play(&self) -> Result<(), OutputError> {
        self.stream.play().map_err(OutputError::PlayStream)
    }

    pub fn pause(&self) -> Result<(), OutputError> {
        self.stream.pause().map_err(OutputError::PauseStream)
    }
}

fn build_stream<T>(
    device: &Device,
    config: StreamConfig,
    mut renderer: Box<dyn AudioUnit>,
    synth_channels: usize,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let device_channels = config.channels as usize;
    // fundsp's block path operates on fixed, non-interleaved SIMD buffers.
    // Both are allocated before the callback starts.
    renderer.set_sample_rate(config.sample_rate as f64);
    renderer.allocate();
    let input = BufferVec::new(renderer.inputs());
    let mut synth = BufferVec::new(synth_channels);
    device.build_output_stream(
        config,
        move |output: &mut [T], _: &OutputCallbackInfo| {
            let frames = output.len() / device_channels;
            let mut rendered = 0;
            while rendered < frames {
                let block = (frames - rendered).min(MAX_BUFFER_SIZE);
                renderer.process(block, &input.buffer_ref(), &mut synth.buffer_mut());
                for frame in 0..block {
                    let begin = (rendered + frame) * device_channels;
                    write_buffer_frame(&mut output[begin..begin + device_channels], &synth, frame);
                }
                rendered += block;
            }
        },
        |error| eprintln!("audio output error: {error}"),
        None,
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn preferred_buffer_size(supported: &SupportedBufferSize) -> BufferSize {
    match *supported {
        SupportedBufferSize::Range { min, max } => {
            BufferSize::Fixed(TARGET_BUFFER_FRAMES.clamp(min, max))
        }
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

#[cfg(target_arch = "wasm32")]
fn preferred_buffer_size(_supported: &SupportedBufferSize) -> BufferSize {
    // CPAL's WebAudio backend schedules two AudioBufferSourceNodes from
    // main-thread callbacks. Its advertised 1..=u32::MAX range is synthetic,
    // not a latency promise. Keep CPAL's deliberate 2048-frame default instead
    // of forcing the 512-frame native target and starving those callbacks
    // whenever egui, Lua evaluation, or the browser occupies the main thread.
    BufferSize::Default
}

fn write_buffer_frame<T>(device: &mut [T], synth: &BufferVec, frame: usize)
where
    T: Sample + FromSample<f32>,
{
    if device.len() == 1 {
        let mono = (0..synth.channels())
            .map(|channel| synth.at_f32(channel, frame))
            .sum::<f32>()
            / synth.channels() as f32;
        device[0] = T::from_sample(mono.clamp(-1.0, 1.0));
        return;
    }
    for (channel, sample) in device.iter_mut().enumerate() {
        let value = if channel < synth.channels() {
            synth.at_f32(channel, frame)
        } else {
            0.0
        };
        *sample = T::from_sample(value.clamp(-1.0, 1.0));
    }
}

#[derive(Debug)]
pub enum OutputError {
    NoSynthChannels,
    ProcessorInputChannels { expected: usize, found: usize },
    ProcessorOutputChannels { expected: usize, found: usize },
    MainChannels { stems: usize, main: usize },
    NoOutputDevice,
    DefaultConfig(cpal::Error),
    UnsupportedSampleFormat(SampleFormat),
    BuildStream(cpal::Error),
    PlayStream(cpal::Error),
    PauseStream(cpal::Error),
}

impl core::fmt::Display for OutputError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            OutputError::NoSynthChannels => write!(f, "synth graph has no output channels"),
            OutputError::ProcessorInputChannels { expected, found } => write!(
                f,
                "persistent processor has {found} inputs but the sequencer emits {expected} stems"
            ),
            OutputError::ProcessorOutputChannels { expected, found } => write!(
                f,
                "persistent processor has {found} outputs but must preserve all {expected} stems"
            ),
            OutputError::MainChannels { stems, main } => write!(
                f,
                "cannot expose {main} main channels from a {stems}-channel stem layout"
            ),
            OutputError::NoOutputDevice => write!(f, "no default audio output device"),
            OutputError::DefaultConfig(error) => {
                write!(f, "cannot read the default audio configuration: {error}")
            }
            OutputError::UnsupportedSampleFormat(format) => {
                write!(f, "unsupported audio sample format {format}")
            }
            OutputError::BuildStream(error) => write!(f, "cannot open audio output: {error}"),
            OutputError::PlayStream(error) => write!(f, "cannot start audio output: {error}"),
            OutputError::PauseStream(error) => write!(f, "cannot pause audio output: {error}"),
        }
    }
}

impl core::error::Error for OutputError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_a_supported_desktop_buffer() {
        assert_eq!(
            preferred_buffer_size(&SupportedBufferSize::Range {
                min: 64,
                max: 2_048
            }),
            BufferSize::Fixed(512)
        );
        assert_eq!(
            preferred_buffer_size(&SupportedBufferSize::Range {
                min: 1_024,
                max: 4_096
            }),
            BufferSize::Fixed(1_024)
        );
        assert_eq!(
            preferred_buffer_size(&SupportedBufferSize::Unknown),
            BufferSize::Default
        );
    }

    #[test]
    fn block_output_is_interleaved_clamped_and_downmixed() {
        let mut synth = BufferVec::new(2);
        synth.set_f32(0, 3, 1.5);
        synth.set_f32(1, 3, -0.5);

        let mut stereo = [0.0f32; 2];
        write_buffer_frame(&mut stereo, &synth, 3);
        assert_eq!(stereo, [1.0, -0.5]);

        let mut mono = [0.0f32; 1];
        write_buffer_frame(&mut mono, &synth, 3);
        assert_eq!(mono, [0.5]);
    }
}
