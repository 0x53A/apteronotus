//! cpal output for a fundsp sequencer frontend/backend pair.
//!
//! Graph construction and voice instantiation stay on the caller's control
//! thread. The device callback owns only fundsp's realtime backend and
//! preallocated SIMD buffers. It renders through `AudioUnit::process` in
//! fundsp's native 64-frame blocks; calling `tick` once per device frame gives
//! the same samples but forfeits the DSP layer's SIMD path.

#[cfg(not(target_arch = "wasm32"))]
use crate::capture::CaptureProducer;
use crate::capture::CaptureUnit;
#[cfg(not(target_arch = "wasm32"))]
use cpal::InputCallbackInfo;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Device, FromSample, I24, OutputCallbackInfo, Sample, SampleFormat, SizedSample,
    Stream, StreamConfig, SupportedBufferSize, U24,
};
use fundsp::net::Net;
use fundsp::prelude32::{
    AudioUnit, BufferMut, BufferRef, BufferVec, MAX_BUFFER_SIZE, ReplayMode, Sequencer, Shared,
    Signal, SignalFrame, shared, zero,
};

/// About 43 ms at 48 kHz.
///
/// The earlier 512 (10.7 ms) assumed the callback thread would be scheduled
/// promptly, and on a desktop that is running a full-core `cargo build` it is
/// not: cpal's ALSA worker is an ordinary `SCHED_OTHER` thread, so a compile
/// starves it past the deadline and ALSA reports an underrun. Real-time
/// scheduling is the proper fix, but cpal only promotes for direct `hw:`
/// devices — it deliberately refuses for the PipeWire ioplug that is `default`
/// on most current systems — so latency is the lever that works everywhere.
///
/// 43 ms is still under the threshold where a fader move or a Run command
/// feels detached. Live *note* input, if it ever arrives, wants the real-time
/// thread rather than a smaller number here.
#[cfg(not(target_arch = "wasm32"))]
const TARGET_BUFFER_FRAMES: u32 = 2_048;

/// Halfway response time of the master fader's smoother, in seconds.
///
/// A `Shared` read straight into a multiplier steps once per block, and a step
/// in gain is a click — audible on any fader move, and worst at exactly the
/// moment somebody reaches for the master because something is too loud. 10 ms
/// is below the threshold at which a fader feels laggy and far above the one at
/// which a gain change is heard as an edge.
const MASTER_RESPONSE_SECONDS: f32 = 0.010;

pub struct AudioOutput {
    sequencer: Sequencer,
    stream: Stream,
    sample_rate: f64,
    device_channels: usize,
    input_stream: Option<Stream>,
    input_binding: InputBinding,
    master: MasterGain,
}

/// The master fader: one smoothed gain stage between the engine and the device.
///
/// It is deliberately *not* an Apteronotus `control` the score declares and the
/// host drives. `master(...)` may be declared only once, so a score that
/// already has one — which is every interesting score — cannot be given a gain
/// stage from outside, and a master volume that stops working the moment a real
/// song is pasted in is not a master volume. This sits downstream of everything
/// the language can express, so it works on any program and asks nothing of it.
///
/// The handle is cheap to clone and independent of any one stream: it is a
/// reference to the atomic the running graph already reads. Moving it stores a
/// float. Nothing is re-evaluated, re-lowered or restarted, and a host may hold
/// one across a hard reset — see [`AudioOutput::master`].
#[derive(Clone)]
pub struct MasterGain {
    level: Shared,
}

impl MasterGain {
    /// The bottom of the fader, in decibels. Anything at or below it is exact
    /// silence rather than a very small number, so the fader has a real off.
    pub const MIN_DECIBELS: f32 = -60.0;

    /// The top of the fader, in decibels.
    ///
    /// Unity, and no higher. A master that can boost turns "it is too quiet"
    /// into clipping at the one point in the signal path with no headroom left
    /// and no meter on it. Gain belongs in the score, where it is written down
    /// and can be seen; this control exists to take gain away.
    pub const MAX_DECIBELS: f32 = 0.0;

    /// A fader at unity — the level a host that never offers the control gets.
    pub fn unity() -> MasterGain {
        MasterGain { level: shared(1.0) }
    }

    fn at_decibels(decibels: f32) -> MasterGain {
        let master = MasterGain::unity();
        master.set_decibels(decibels);
        master
    }

    /// The linear amplitude the graph is currently multiplying by.
    pub fn amplitude(&self) -> f32 {
        self.level.value()
    }

    /// Where the fader sits, in decibels; [`Self::MIN_DECIBELS`] when silent.
    pub fn decibels(&self) -> f32 {
        let amplitude = self.level.value();
        if amplitude <= 0.0 {
            return Self::MIN_DECIBELS;
        }
        (20.0 * amplitude.log10()).clamp(Self::MIN_DECIBELS, Self::MAX_DECIBELS)
    }

    /// Move the fader. Clamped to `[MIN_DECIBELS, MAX_DECIBELS]`; the bottom of
    /// that range is silence, not −60 dB of residual signal.
    pub fn set_decibels(&self, decibels: f32) {
        let amplitude = if decibels <= Self::MIN_DECIBELS || decibels.is_nan() {
            0.0
        } else {
            10.0f32.powf(decibels.min(Self::MAX_DECIBELS) / 20.0)
        };
        self.level.set(amplitude);
    }

    /// One channel of the gain stage: pass-through scaled by the smoothed level.
    fn stage(&self) -> Box<dyn AudioUnit> {
        Box::new(MasterGainUnit::new(self.level.clone()))
    }
}

/// Allocation-free stream-edge gain whose initial value is fixed at graph
/// construction time.
///
/// fundsp's general `follow` intentionally snaps to its input on the first
/// sample. That is convenient for an ordinary parameter, but a paused
/// replacement stream can have its target changed between `play()` and its
/// first callback; snapping there would skip the fade-in. This node preserves
/// the level with which the stream was opened and smooths every later target.
#[derive(Clone)]
struct MasterGainUnit {
    target: Shared,
    value: f32,
    coefficient: f32,
}

impl MasterGainUnit {
    fn new(target: Shared) -> Self {
        let value = target.value();
        let mut unit = Self {
            target,
            value,
            coefficient: 1.0,
        };
        unit.set_sample_rate(44_100.0);
        unit
    }

    fn update(&mut self) -> f32 {
        self.value += self.coefficient * (self.target.value() - self.value);
        self.value
    }
}

impl AudioUnit for MasterGainUnit {
    fn reset(&mut self) {
        self.value = self.target.value();
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        if sample_rate.is_finite() && sample_rate > 0.0 {
            let halfway_samples = f64::from(MASTER_RESPONSE_SECONDS) * sample_rate;
            self.coefficient = (1.0 - 0.5_f64.powf(1.0 / halfway_samples.max(1.0))) as f32;
        }
    }

    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        output[0] = input[0] * self.update();
    }

    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for sample in 0..size {
            output.set_f32(0, sample, input.at_f32(0, sample) * self.update());
        }
    }

    fn inputs(&self) -> usize {
        1
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
        0x4150_5445_4d47_4149
    }

    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl Default for MasterGain {
    fn default() -> Self {
        MasterGain::unity()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InputBinding {
    NotRequested,
    Live {
        device: String,
        channels: usize,
        requested_channels: usize,
        external_channels: usize,
    },
    Fallback {
        reason: String,
    },
}

impl AudioOutput {
    /// Open the default output device. The returned stream is paused, so the
    /// caller can fill its initial lookahead before calling [`play`](Self::play).
    pub fn open(synth_channels: usize) -> Result<AudioOutput, OutputError> {
        Self::open_at_level(synth_channels, MasterGain::MAX_DECIBELS)
    }

    /// Open the default output at an explicit initial device-edge level.
    pub fn open_at_level(
        synth_channels: usize,
        master_decibels: f32,
    ) -> Result<AudioOutput, OutputError> {
        Self::open_inner(synth_channels, synth_channels, None, 0, 0, master_decibels)
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
        Self::open_inner(
            stem_channels,
            main_channels,
            Some(processor),
            0,
            0,
            MasterGain::MAX_DECIBELS,
        )
    }

    /// Open a processed output and bind the first declared logical input to
    /// the native default capture device. Any unbound lanes retain silence.
    ///
    /// `external_channels` is the complete flattened logical layout;
    /// `requested_channels` is the prefix belonging to the declaration the
    /// host selected. The current GUI selects the first declaration until it
    /// grows an explicit device/input routing panel.
    pub fn open_processed_with_default_input(
        stem_channels: usize,
        main_channels: usize,
        processor: Box<dyn AudioUnit>,
        external_channels: usize,
        requested_channels: usize,
    ) -> Result<AudioOutput, OutputError> {
        Self::open_processed_with_default_input_at_level(
            stem_channels,
            main_channels,
            processor,
            external_channels,
            requested_channels,
            MasterGain::MAX_DECIBELS,
        )
    }

    /// Open a processed input-bearing output at an explicit initial master
    /// level. Replacement hosts use silence; ordinary hosts use their retained
    /// fader value.
    pub fn open_processed_with_default_input_at_level(
        stem_channels: usize,
        main_channels: usize,
        processor: Box<dyn AudioUnit>,
        external_channels: usize,
        requested_channels: usize,
        master_decibels: f32,
    ) -> Result<AudioOutput, OutputError> {
        if processor.inputs() != stem_channels + external_channels {
            return Err(OutputError::ProcessorInputChannels {
                expected: stem_channels + external_channels,
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
        Self::open_inner(
            stem_channels,
            main_channels,
            Some(processor),
            external_channels,
            requested_channels,
            master_decibels,
        )
    }

    fn open_inner(
        sequencer_channels: usize,
        synth_channels: usize,
        processor: Option<Box<dyn AudioUnit>>,
        external_channels: usize,
        requested_channels: usize,
        master_decibels: f32,
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
        let (capture, input_stream, input_binding) = prepare_input(
            &host,
            config.sample_rate,
            external_channels,
            requested_channels,
        );

        let mut sequencer = Sequencer::new(0, sequencer_channels, ReplayMode::None);
        sequencer.set_sample_rate(sample_rate);
        sequencer.allocate();
        let backend = sequencer.backend();
        // The master fader is the last thing before the device on every route,
        // including the one with no persistent processor, so there is always a
        // net here even when the sequencer backend alone would have rendered.
        // One multiply per sample per channel; at unity it changes nothing.
        let master = MasterGain::at_decibels(master_decibels);
        let renderer: Box<dyn AudioUnit> = {
            let mut net = Net::new(0, synth_channels);
            let source = match processor {
                Some(processor) => {
                    let voices = net.push(Box::new(backend));
                    let persistent = net.push(processor);
                    for channel in 0..sequencer_channels {
                        net.connect(voices, channel, persistent, channel);
                    }
                    if let Some(capture) = capture {
                        let capture = net.push(Box::new(capture));
                        for channel in 0..external_channels {
                            net.connect(capture, channel, persistent, sequencer_channels + channel);
                        }
                    } else {
                        for channel in 0..external_channels {
                            let fallback = net.push(Box::new(zero()));
                            net.connect(fallback, 0, persistent, sequencer_channels + channel);
                        }
                    }
                    persistent
                }
                None => net.push(Box::new(backend)),
            };
            for channel in 0..synth_channels {
                let trim = net.push(master.stage());
                net.connect(source, channel, trim, 0);
                net.connect_output(trim, 0, channel);
            }
            Box::new(net)
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
            input_stream,
            input_binding,
            master,
        })
    }

    /// This stream's master fader, at the level selected when it was opened.
    ///
    /// The handle belongs to the stream, so it does not outlive one: a host
    /// that keeps a level across a hard reset holds the *number* and reapplies
    /// it to the new output before calling [`play`](Self::play), which is also
    /// what stops a replacement stream from ever rendering a block at the wrong
    /// gain. Clone this only to move the fader from elsewhere.
    pub fn master(&self) -> &MasterGain {
        &self.master
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

    pub fn input_binding(&self) -> &InputBinding {
        &self.input_binding
    }

    pub fn play(&self) -> Result<(), OutputError> {
        if let Some(input) = &self.input_stream {
            input.play().map_err(OutputError::PlayInputStream)?;
        }
        if let Err(error) = self.stream.play() {
            if let Some(input) = &self.input_stream {
                let _ = input.pause();
            }
            return Err(OutputError::PlayStream(error));
        }
        Ok(())
    }

    pub fn pause(&self) -> Result<(), OutputError> {
        self.stream.pause().map_err(OutputError::PauseStream)?;
        if let Some(input) = &self.input_stream {
            input.pause().map_err(OutputError::PauseInputStream)?;
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn prepare_input(
    host: &cpal::Host,
    output_sample_rate: u32,
    external_channels: usize,
    requested_channels: usize,
) -> (Option<CaptureUnit>, Option<Stream>, InputBinding) {
    if external_channels == 0 || requested_channels == 0 {
        return (None, None, InputBinding::NotRequested);
    }
    let Some(device) = host.default_input_device() else {
        return (
            None,
            None,
            InputBinding::Fallback {
                reason: "no default audio input device; using declared silence fallback".into(),
            },
        );
    };
    let device_name = device.to_string();
    let supported = match device.default_input_config() {
        Ok(supported) => supported,
        Err(error) => {
            return (
                None,
                None,
                InputBinding::Fallback {
                    reason: format!(
                        "cannot read {device_name:?} input configuration ({error}); using declared silence fallback"
                    ),
                },
            );
        }
    };
    if supported.sample_rate() != output_sample_rate {
        return (
            None,
            None,
            InputBinding::Fallback {
                reason: format!(
                    "{device_name:?} runs at {} Hz but output runs at {output_sample_rate} Hz; \
                     resampling is not implicit, so the declared silence fallback is active",
                    supported.sample_rate()
                ),
            },
        );
    }

    let config = supported.config();
    let device_channels = config.channels as usize;
    let mapped_channels = requested_channels
        .min(device_channels)
        .min(external_channels);
    if mapped_channels == 0 {
        return (
            None,
            None,
            InputBinding::Fallback {
                reason: format!(
                    "{device_name:?} exposes no channels usable by the selected logical input"
                ),
            },
        );
    }

    let capacity_frames = (output_sample_rate as usize / 2).max(2_048);
    let (producer, capture) = CaptureUnit::pair(external_channels, capacity_frames);
    let stream = match supported.sample_format() {
        SampleFormat::I8 => {
            build_input_capture_stream::<i8>(&device, config, producer, mapped_channels)
        }
        SampleFormat::I16 => {
            build_input_capture_stream::<i16>(&device, config, producer, mapped_channels)
        }
        SampleFormat::I24 => {
            build_input_capture_stream::<I24>(&device, config, producer, mapped_channels)
        }
        SampleFormat::I32 => {
            build_input_capture_stream::<i32>(&device, config, producer, mapped_channels)
        }
        SampleFormat::I64 => {
            build_input_capture_stream::<i64>(&device, config, producer, mapped_channels)
        }
        SampleFormat::U8 => {
            build_input_capture_stream::<u8>(&device, config, producer, mapped_channels)
        }
        SampleFormat::U16 => {
            build_input_capture_stream::<u16>(&device, config, producer, mapped_channels)
        }
        SampleFormat::U24 => {
            build_input_capture_stream::<U24>(&device, config, producer, mapped_channels)
        }
        SampleFormat::U32 => {
            build_input_capture_stream::<u32>(&device, config, producer, mapped_channels)
        }
        SampleFormat::U64 => {
            build_input_capture_stream::<u64>(&device, config, producer, mapped_channels)
        }
        SampleFormat::F32 => {
            build_input_capture_stream::<f32>(&device, config, producer, mapped_channels)
        }
        SampleFormat::F64 => {
            build_input_capture_stream::<f64>(&device, config, producer, mapped_channels)
        }
        other => {
            return (
                None,
                None,
                InputBinding::Fallback {
                    reason: format!(
                        "{device_name:?} uses unsupported input format {other}; using declared silence fallback"
                    ),
                },
            );
        }
    };
    match stream {
        Ok(stream) => (
            Some(capture),
            Some(stream),
            InputBinding::Live {
                device: device_name,
                channels: mapped_channels,
                requested_channels,
                external_channels,
            },
        ),
        Err(error) => (
            None,
            None,
            InputBinding::Fallback {
                reason: format!(
                    "cannot open {device_name:?} for input ({error}); using declared silence fallback"
                ),
            },
        ),
    }
}

#[cfg(target_arch = "wasm32")]
fn prepare_input(
    _host: &cpal::Host,
    _output_sample_rate: u32,
    external_channels: usize,
    requested_channels: usize,
) -> (Option<CaptureUnit>, Option<Stream>, InputBinding) {
    if external_channels == 0 || requested_channels == 0 {
        (None, None, InputBinding::NotRequested)
    } else {
        (
            None,
            None,
            InputBinding::Fallback {
                reason: "browser input permission/device binding is not connected; using declared silence fallback"
                    .into(),
            },
        )
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn build_input_capture_stream<T>(
    device: &Device,
    config: StreamConfig,
    mut producer: CaptureProducer,
    mapped_channels: usize,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let device_channels = config.channels as usize;
    device.build_input_stream(
        config,
        move |input: &[T], _: &InputCallbackInfo| {
            for frame in input.chunks_exact(device_channels) {
                producer.push_frame(|channel| {
                    if channel < mapped_channels {
                        f32::from_sample(frame[channel])
                    } else {
                        0.0
                    }
                });
            }
        },
        |error| eprintln!("audio input error: {error}"),
        None,
    )
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
    PlayInputStream(cpal::Error),
    PauseInputStream(cpal::Error),
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
            OutputError::PlayInputStream(error) => {
                write!(f, "cannot start audio input: {error}")
            }
            OutputError::PauseInputStream(error) => {
                write!(f, "cannot pause audio input: {error}")
            }
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
                max: 4_096
            }),
            BufferSize::Fixed(2_048)
        );
        assert_eq!(
            preferred_buffer_size(&SupportedBufferSize::Range {
                min: 64,
                max: 1_024
            }),
            BufferSize::Fixed(1_024)
        );
        assert_eq!(
            preferred_buffer_size(&SupportedBufferSize::Range {
                min: 4_096,
                max: 8_192
            }),
            BufferSize::Fixed(4_096)
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

    #[test]
    fn the_master_fader_starts_at_unity_and_round_trips_decibels() {
        let master = MasterGain::unity();
        assert_eq!(master.amplitude(), 1.0);
        assert_eq!(master.decibels(), 0.0);

        for decibels in [-0.5, -6.0, -12.0, -35.25, -59.9] {
            master.set_decibels(decibels);
            assert!(
                (master.decibels() - decibels).abs() < 1e-3,
                "{decibels} dB did not survive the round trip"
            );
        }

        // −6 dB is half the amplitude, which is the one figure a reader will
        // check by hand.
        master.set_decibels(-6.020_6);
        assert!((master.amplitude() - 0.5).abs() < 1e-4);
    }

    #[test]
    fn the_bottom_of_the_fader_is_silence_not_a_small_number() {
        let master = MasterGain::unity();
        master.set_decibels(MasterGain::MIN_DECIBELS);
        assert_eq!(master.amplitude(), 0.0);
        master.set_decibels(-400.0);
        assert_eq!(master.amplitude(), 0.0);
        // Reading back a silent fader reports the bottom, not −inf.
        assert_eq!(master.decibels(), MasterGain::MIN_DECIBELS);
    }

    /// The master attenuates. Boost belongs in the score, where it is written
    /// down; here it would only clip the one point with no headroom left.
    #[test]
    fn the_fader_refuses_to_boost() {
        let master = MasterGain::unity();
        master.set_decibels(12.0);
        assert_eq!(master.amplitude(), 1.0);
    }

    #[test]
    fn a_nan_level_is_treated_as_silence_rather_than_poisoning_the_graph() {
        let master = MasterGain::unity();
        master.set_decibels(f32::NAN);
        assert_eq!(master.amplitude(), 0.0);
    }

    #[test]
    fn opposed_master_moves_keep_one_bounded_crossfade_gain() {
        let old = MasterGain::unity();
        let new = MasterGain::unity();
        new.set_decibels(MasterGain::MIN_DECIBELS);
        let mut old_stage = old.stage();
        let mut new_stage = new.stage();
        old_stage.set_sample_rate(48_000.0);
        new_stage.set_sample_rate(48_000.0);

        let mut old_out = [0.0];
        let mut new_out = [0.0];
        for _ in 0..4_800 {
            old_stage.tick(&[1.0], &mut old_out);
            new_stage.tick(&[1.0], &mut new_out);
        }

        old.set_decibels(MasterGain::MIN_DECIBELS);
        new.set_decibels(MasterGain::MAX_DECIBELS);
        for _ in 0..3_840 {
            old_stage.tick(&[1.0], &mut old_out);
            new_stage.tick(&[1.0], &mut new_out);
            assert!(((old_out[0] + new_out[0]) - 1.0).abs() < 1.0e-4);
        }
        assert!(old_out[0] < 0.01);
        assert!(new_out[0] > 0.99);
    }

    #[test]
    fn a_silent_stream_initialization_cannot_race_its_first_callback() {
        let master = MasterGain::at_decibels(MasterGain::MIN_DECIBELS);
        let mut stage = master.stage();
        stage.set_sample_rate(48_000.0);

        // Model the control thread publishing the target immediately after
        // `play`, before the device has rendered even one sample.
        master.set_decibels(MasterGain::MAX_DECIBELS);
        let mut output = [0.0];
        stage.tick(&[1.0], &mut output);
        assert!(output[0] < 0.01, "first sample jumped to {}", output[0]);

        for _ in 1..3_840 {
            stage.tick(&[1.0], &mut output);
        }
        assert!(output[0] > 0.99);
    }
}
