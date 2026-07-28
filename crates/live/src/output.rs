//! cpal output for a fundsp sequencer frontend/backend pair.
//!
//! Graph construction and voice instantiation stay on the caller's control
//! thread. The device callback owns only fundsp's realtime backend and one
//! preallocated frame.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Device, FromSample, I24, OutputCallbackInfo, Sample, SampleFormat, SizedSample, Stream,
    StreamConfig, U24,
};
use fundsp::prelude32::{AudioUnit, ReplayMode, Sequencer};

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
        if synth_channels == 0 {
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
        let config: StreamConfig = supported.into();
        let sample_rate = config.sample_rate as f64;
        let device_channels = config.channels as usize;

        let mut sequencer = Sequencer::new(0, synth_channels, ReplayMode::None);
        sequencer.set_sample_rate(sample_rate);
        sequencer.allocate();
        let backend = sequencer.backend();
        let stream = match sample_format {
            SampleFormat::I8 => build_stream::<i8>(&device, config, backend, synth_channels),
            SampleFormat::I16 => build_stream::<i16>(&device, config, backend, synth_channels),
            SampleFormat::I24 => build_stream::<I24>(&device, config, backend, synth_channels),
            SampleFormat::I32 => build_stream::<i32>(&device, config, backend, synth_channels),
            SampleFormat::I64 => build_stream::<i64>(&device, config, backend, synth_channels),
            SampleFormat::U8 => build_stream::<u8>(&device, config, backend, synth_channels),
            SampleFormat::U16 => build_stream::<u16>(&device, config, backend, synth_channels),
            SampleFormat::U24 => build_stream::<U24>(&device, config, backend, synth_channels),
            SampleFormat::U32 => build_stream::<u32>(&device, config, backend, synth_channels),
            SampleFormat::U64 => build_stream::<u64>(&device, config, backend, synth_channels),
            SampleFormat::F32 => build_stream::<f32>(&device, config, backend, synth_channels),
            SampleFormat::F64 => build_stream::<f64>(&device, config, backend, synth_channels),
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
    mut backend: fundsp::realseq::SequencerBackend,
    synth_channels: usize,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let device_channels = config.channels as usize;
    let mut frame = vec![0.0f32; synth_channels];
    device.build_output_stream(
        config,
        move |output: &mut [T], _: &OutputCallbackInfo| {
            for device_frame in output.chunks_mut(device_channels) {
                backend.tick(&[], &mut frame);
                write_frame(device_frame, &frame);
            }
        },
        |error| eprintln!("audio output error: {error}"),
        None,
    )
}

fn write_frame<T>(device: &mut [T], synth: &[f32])
where
    T: Sample + FromSample<f32>,
{
    if device.len() == 1 {
        let mono = synth.iter().copied().sum::<f32>() / synth.len() as f32;
        device[0] = T::from_sample(mono.clamp(-1.0, 1.0));
        return;
    }
    for (channel, sample) in device.iter_mut().enumerate() {
        let value = synth.get(channel).copied().unwrap_or(0.0);
        *sample = T::from_sample(value.clamp(-1.0, 1.0));
    }
}

#[derive(Debug)]
pub enum OutputError {
    NoSynthChannels,
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
