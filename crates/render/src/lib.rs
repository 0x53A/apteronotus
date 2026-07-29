//! Offline rendering: an owned `Program` to a block of audio, with no device.
//!
//! The realtime path in `crates/live` exists to meet a deadline it does not
//! control. This one has no deadline, and that removes the whole lookahead
//! discipline: the entire window is scheduled before the first block is
//! rendered, and `fundsp`'s `Sequencer` is driven directly rather than split
//! into a frontend and a backend. What remains is the same lowering, the same
//! scheduler and the same persistent arena, so what comes out of here is what
//! would have come out of the device.
//!
//! Rendering is therefore reproducible. Pattern randomness derives from
//! position and per-voice randomness from event provenance, so nothing in the
//! chain draws from a generator whose state depends on when a block happened
//! to be computed. Two renders of one source at one sample rate are identical
//! sample for sample, which is what makes an automated comparison against a
//! reference recording mean anything.

pub mod program;

use apteronotus_live::{PitchScheduler, ProgramScheduler, ScheduleError};
use apteronotus_lua::Program;
use apteronotus_pattern::Frac;
use fundsp::net::Net;
use fundsp::prelude32::{AudioUnit, BufferVec, MAX_BUFFER_SIZE};
use std::path::Path;

pub use program::{
    needs_persistent_runtime, persistent_runtime, playable_channels, routed_runtime,
    scheduled_runs, scheduled_tracks,
};

/// How much of the program to render.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RenderSpan {
    /// A wall-clock duration. What an analysis pass usually wants.
    Seconds(f64),
    /// A musical duration, projected through the program's own tempo map.
    /// Survives a tempo edit as the same amount of music.
    Cycles(Frac),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SampleFormat {
    /// 32-bit float. The default, because an analysis chain should measure the
    /// engine's output and not a quantisation of it — and because a float file
    /// cannot clip, so an overloaded mix stays diagnosable instead of arriving
    /// already flattened.
    #[default]
    Float32,
    /// 16-bit PCM, clamped. For handing the result to something that insists.
    Pcm16,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderOptions {
    pub sample_rate: f64,
    pub span: RenderSpan,
    /// Extra time rendered past the end of the scheduled window, with nothing
    /// new scheduled into it.
    ///
    /// Without this the last note's release, and every delay and reverb tail
    /// under it, is cut mid-decay — which reads to a spectral analysis as an
    /// enormously fast decay rather than as a truncation.
    pub tail_seconds: f64,
    /// Emit the complete routed stem layout rather than the main channels.
    ///
    /// The per-bus signal is what makes an automated comparison tractable:
    /// matching a drum bus against a separated drum stem is a real error
    /// signal, where matching mix against mix confounds every part at once.
    pub stems: bool,
    pub format: SampleFormat,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            sample_rate: 48_000.0,
            span: RenderSpan::Seconds(30.0),
            tail_seconds: 2.0,
            stems: false,
            format: SampleFormat::default(),
        }
    }
}

/// One rendered block of interleaved audio, plus what the render learned about
/// itself on the way past.
pub struct Rendered {
    pub sample_rate: f64,
    pub channels: usize,
    /// Interleaved, `frames * channels` long.
    pub samples: Vec<f32>,
    pub format: SampleFormat,
    /// Voices the scheduler instantiated across the whole window.
    pub voices: usize,
    /// Seconds of scheduled music, excluding the tail.
    pub scheduled_seconds: f64,
    /// Largest absolute sample seen, before any clamping.
    pub peak: f32,
    /// Samples outside [−1, 1], plus any that were not finite. Zero in a
    /// healthy render; a float file keeps them, a 16-bit one flattens them.
    pub clipped: usize,
}

impl Rendered {
    pub fn frames(&self) -> usize {
        self.samples.len().checked_div(self.channels).unwrap_or(0)
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate
    }

    /// The peak in decibels relative to full scale; `None` for pure silence,
    /// which has no meaningful logarithm and usually means a real mistake.
    pub fn peak_decibels(&self) -> Option<f32> {
        (self.peak > 0.0).then(|| 20.0 * self.peak.log10())
    }
}

/// Written by hand rather than derived: a render holds minutes of samples, and
/// a failing assertion that prints all of them is not a diagnostic.
impl core::fmt::Debug for Rendered {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rendered")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("frames", &self.frames())
            .field("format", &self.format)
            .field("voices", &self.voices)
            .field("scheduled_seconds", &self.scheduled_seconds)
            .field("peak", &self.peak)
            .field("clipped", &self.clipped)
            .finish_non_exhaustive()
    }
}

pub fn render(program: &Program, options: &RenderOptions) -> Result<Rendered, RenderError> {
    if !(options.sample_rate.is_finite() && options.sample_rate > 0.0) {
        return Err(RenderError::SampleRate(options.sample_rate));
    }
    let scheduled_seconds = match options.span {
        RenderSpan::Seconds(seconds) => seconds,
        RenderSpan::Cycles(cycles) => program.tempo.cycle_to_seconds(cycles),
    };
    if !(scheduled_seconds.is_finite() && scheduled_seconds > 0.0) {
        return Err(RenderError::Duration(scheduled_seconds));
    }
    let tail_seconds = if options.tail_seconds.is_finite() && options.tail_seconds > 0.0 {
        options.tail_seconds
    } else {
        0.0
    };

    let main_channels = playable_channels(program).map_err(RenderError::Binding)?;
    let mut persistent = needs_persistent_runtime(program)
        .then(|| persistent_runtime(program).map_err(RenderError::Binding))
        .transpose()?;

    let mut sequencer = match &persistent {
        Some(runtime) => runtime.sequencer(),
        None => {
            let first = program
                .tracks
                .first()
                .and_then(|track| program.voice(track.voice))
                .ok_or(RenderError::NothingToRender)?;
            PitchScheduler::sequencer(first)
        }
    };
    sequencer.set_sample_rate(options.sample_rate);
    sequencer.allocate();

    // Offline, the whole window is scheduled up front. There is no frontier to
    // advance and no deadline to miss, so lookahead buys nothing here.
    let mut scheduler = ProgramScheduler::default();
    let tracks = scheduled_tracks(program).map_err(RenderError::Binding)?;
    let report = match &persistent {
        Some(runtime) => {
            let runs = scheduled_runs(program).map_err(RenderError::Binding)?;
            scheduler.fill_routed_program_to_seconds_tempo_map(
                scheduled_seconds,
                tracks,
                runs,
                &program.tempo,
                &mut sequencer,
                routed_runtime(runtime),
            )?
        }
        None => scheduler.fill_to_seconds_tempo_map(
            scheduled_seconds,
            tracks,
            &program.tempo,
            &mut sequencer,
        )?,
    };

    let stem_channels = persistent
        .as_ref()
        .map_or(main_channels, |runtime| runtime.layout().total_channels());
    let channels = if options.stems {
        stem_channels
    } else {
        main_channels
    };

    let mut renderer: Box<dyn AudioUnit> = match persistent.as_mut() {
        Some(runtime) => {
            // `take_processor` is the arena's own offline route: it supplies
            // every declared logical input with its silence fallback, which is
            // the correct reading of a live input that nobody is playing.
            let processor = runtime.take_processor();
            let mut net = Net::new(0, channels);
            let voices = net.push(Box::new(sequencer));
            let stems = net.push(processor);
            for channel in 0..stem_channels {
                net.connect(voices, channel, stems, channel);
            }
            for channel in 0..channels {
                net.connect_output(stems, channel, channel);
            }
            Box::new(net)
        }
        None => Box::new(sequencer),
    };
    renderer.set_sample_rate(options.sample_rate);
    renderer.allocate();

    let frames =
        (((scheduled_seconds + tail_seconds) * options.sample_rate).ceil() as usize).max(1);
    let input = BufferVec::new(renderer.inputs());
    let mut block = BufferVec::new(channels);
    let mut samples = Vec::with_capacity(frames * channels);
    let mut peak = 0.0f32;
    let mut clipped = 0usize;

    let mut done = 0;
    while done < frames {
        let size = (frames - done).min(MAX_BUFFER_SIZE);
        renderer.process(size, &input.buffer_ref(), &mut block.buffer_mut());
        for frame in 0..size {
            for channel in 0..channels {
                let sample = block.at_f32(channel, frame);
                // A NaN escaping a graph poisons every downstream measurement
                // and is invisible in a waveform view. Fold it to zero so the
                // peak stays meaningful; the clip count records that something
                // was wrong here.
                let sample = if sample.is_finite() {
                    let magnitude = sample.abs();
                    if magnitude > peak {
                        peak = magnitude;
                    }
                    if magnitude > 1.0 {
                        clipped += 1;
                    }
                    sample
                } else {
                    clipped += 1;
                    0.0
                };
                samples.push(sample);
            }
        }
        done += size;
    }

    Ok(Rendered {
        sample_rate: options.sample_rate,
        channels,
        samples,
        format: options.format,
        voices: report.voices,
        scheduled_seconds,
        peak,
        clipped,
    })
}

/// Write a render to a WAV file.
pub fn write_wav(path: impl AsRef<Path>, rendered: &Rendered) -> Result<(), RenderError> {
    let spec = hound::WavSpec {
        channels: u16::try_from(rendered.channels)
            .map_err(|_| RenderError::ChannelCount(rendered.channels))?,
        sample_rate: rendered.sample_rate.round() as u32,
        bits_per_sample: match rendered.format {
            SampleFormat::Float32 => 32,
            SampleFormat::Pcm16 => 16,
        },
        sample_format: match rendered.format {
            SampleFormat::Float32 => hound::SampleFormat::Float,
            SampleFormat::Pcm16 => hound::SampleFormat::Int,
        },
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(RenderError::Wav)?;
    match rendered.format {
        SampleFormat::Float32 => {
            for &sample in &rendered.samples {
                writer.write_sample(sample).map_err(RenderError::Wav)?;
            }
        }
        SampleFormat::Pcm16 => {
            for &sample in &rendered.samples {
                let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
                writer.write_sample(value).map_err(RenderError::Wav)?;
            }
        }
    }
    writer.finalize().map_err(RenderError::Wav)
}

#[derive(Debug)]
pub enum RenderError {
    /// The program does not reach audio at all. Carries the binding layer's
    /// own explanation.
    Binding(String),
    NothingToRender,
    SampleRate(f64),
    Duration(f64),
    ChannelCount(usize),
    Schedule(ScheduleError),
    Wav(hound::Error),
}

impl From<ScheduleError> for RenderError {
    fn from(error: ScheduleError) -> RenderError {
        RenderError::Schedule(error)
    }
}

impl core::fmt::Display for RenderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RenderError::Binding(message) => write!(f, "{message}"),
            RenderError::NothingToRender => {
                write!(f, "the program has no playable tracks or persistent runs")
            }
            RenderError::SampleRate(rate) => write!(f, "{rate} is not a usable sample rate"),
            RenderError::Duration(seconds) => write!(f, "{seconds} is not a usable duration"),
            RenderError::ChannelCount(channels) => {
                write!(f, "a WAV file cannot carry {channels} channels")
            }
            RenderError::Schedule(error) => write!(f, "scheduling: {error}"),
            RenderError::Wav(error) => write!(f, "writing the audio file: {error}"),
        }
    }
}

impl core::error::Error for RenderError {}
