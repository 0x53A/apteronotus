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

const ANALYSIS_FFT_SIZE: usize = 32_768;
const ANALYSIS_FFT_HOP: usize = ANALYSIS_FFT_SIZE / 2;

pub use program::{
    needs_persistent_runtime, persistent_runtime, persistent_runtime_at, playable_channels,
    playable_channels_for_tracks, routed_runtime, scheduled_runs, scheduled_tracks,
    scheduled_tracks_for, stem_lane_labels,
};

/// How much of the program to render.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RenderSpan {
    /// A wall-clock duration. What an analysis pass usually wants.
    Seconds(f64),
    /// A musical duration, projected through the program's own tempo map.
    /// Survives a tempo edit as the same amount of music.
    Cycles(Frac),
    /// A wall-clock window. State is advanced from zero to `begin` before any
    /// samples are retained.
    SecondsRange { begin: f64, end: f64 },
    /// An exact musical window projected through the program tempo map. As
    /// with [`Self::SecondsRange`], the prefix is rendered and discarded so
    /// voices and persistent processors arrive with truthful history.
    CyclesRange { begin: Frac, end: Frac },
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

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TrackSelection {
    solo: Option<Vec<usize>>,
    muted: Vec<usize>,
}

impl TrackSelection {
    /// Schedule every track.
    pub fn all() -> TrackSelection {
        TrackSelection::default()
    }

    /// Add tracks to the inclusion set. The first call changes the selection
    /// from "all" to "only these"; later calls extend that set.
    pub fn include_only(&mut self, tracks: impl IntoIterator<Item = usize>) {
        self.solo.get_or_insert_default().extend(tracks);
    }

    /// Exclude tracks after applying the optional inclusion set. Muting wins
    /// when one index appears in both sets.
    pub fn exclude(&mut self, tracks: impl IntoIterator<Item = usize>) {
        self.muted.extend(tracks);
    }

    /// Resolve the filters to unique indices in original program order.
    pub fn selected_indices(&self, track_count: usize) -> Result<Vec<usize>, RenderError> {
        for &index in self.solo.iter().flatten().chain(self.muted.iter()) {
            if index >= track_count {
                return Err(RenderError::TrackIndex {
                    index,
                    tracks: track_count,
                });
            }
        }
        Ok((0..track_count)
            .filter(|index| {
                self.solo.as_ref().is_none_or(|solo| solo.contains(index))
                    && !self.muted.contains(index)
            })
            .collect())
    }
}

#[derive(Clone, PartialEq, Debug)]
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
    /// Emit the complete post-processor routed layout rather than the main
    /// channels.
    ///
    /// The per-bus signal is what makes an automated comparison tractable:
    /// matching a drum bus against a separated drum stem is a real error
    /// signal, where matching mix against mix confounds every part at once.
    pub stems: bool,
    /// Emit the flattened sequencer lanes before persistent bus returns and
    /// master processing. This exposes raw event/graph sends; autonomous
    /// persistent sources exist inside the skipped processor and therefore do
    /// not appear in this diagnostic view.
    pub raw_stems: bool,
    /// Post-evaluation track filtering. The program's persistent runs and
    /// processing layout remain active, and no source text or provenance data
    /// is rewritten.
    pub tracks: TrackSelection,
    pub format: SampleFormat,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            sample_rate: 48_000.0,
            span: RenderSpan::Seconds(30.0),
            tail_seconds: 2.0,
            stems: false,
            raw_stems: false,
            tracks: TrackSelection::all(),
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
    /// Scheduler-native activity for each selected note track, retaining the
    /// original program index.
    pub tracks: Vec<RenderedTrackReport>,
    /// Seconds of scheduled music, excluding the tail.
    pub scheduled_seconds: f64,
    /// Absolute transport time at the first retained sample. Zero for the
    /// original duration-only render modes.
    pub start_seconds: f64,
    /// Largest absolute sample seen, before any clamping.
    pub peak: f32,
    /// Samples outside [−1, 1], plus any that were not finite. Zero in a
    /// healthy render; a float file keeps them, a 16-bit one flattens them.
    pub clipped: usize,
}

#[derive(Clone, PartialEq, Debug)]
pub struct RenderedTrackReport {
    pub index: usize,
    pub voices: usize,
    pub distinct_onsets: usize,
    pub min_interval_seconds: Option<f64>,
    /// Absolute scheduler-native wall-clock positions of distinct onsets.
    pub onsets_seconds: Vec<f64>,
    /// Tempo-projected distance between every consecutive distinct onset.
    pub intervals_seconds: Vec<f64>,
}

/// Scalar measurements for one interleaved output channel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ChannelMetrics {
    /// Signed arithmetic mean. A non-negligible value indicates DC offset.
    pub dc: f64,
    /// Root-mean-square amplitude over the complete render.
    pub rms: f64,
    /// Largest absolute sample amplitude.
    pub peak: f32,
}

pub const ANALYSIS_BANDS_HZ: [(f64, f64); 4] = [
    (0.0, 200.0),
    (200.0, 2_000.0),
    (2_000.0, 8_000.0),
    (8_000.0, f64::INFINITY),
];

/// Preferred display centres for the 30 one-third-octave bands from 25 Hz to
/// 20 kHz. Accumulation uses the corresponding exact base-2 centres around
/// 1 kHz, so adjacent band edges meet without gaps introduced by these rounded
/// labels.
pub const THIRD_OCTAVE_CENTERS_HZ: [f64; 30] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0,
    630.0, 800.0, 1_000.0, 1_250.0, 1_600.0, 2_000.0, 2_500.0, 3_150.0, 4_000.0, 5_000.0, 6_300.0,
    8_000.0, 10_000.0, 12_500.0, 16_000.0, 20_000.0,
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SpectralMetrics {
    pub centroid_hz: f64,
    /// Mean-square energy in each [`ANALYSIS_BANDS_HZ`] range, averaged over
    /// Welch windows and channels.
    pub band_powers: [f64; 4],
    /// Mean-square energy in standard one-third-octave bands. Bands wholly
    /// above Nyquist remain zero.
    pub third_octave_powers: [f64; 30],
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StereoMetrics {
    /// RMS of `L + R`. The common factor omitted from conventional mid/side
    /// encoding cancels in the ratio.
    pub mid_rms: f64,
    /// RMS of `L - R`.
    pub side_rms: f64,
}

impl StereoMetrics {
    /// `20 log10(side / mid)`. Exact mono has no finite logarithm and returns
    /// `None`, as does a signal with no mid component.
    pub fn side_mid_decibels(self) -> Option<f64> {
        (self.mid_rms.is_finite()
            && self.side_rms.is_finite()
            && self.mid_rms > 0.0
            && self.side_rms > 0.0)
            .then(|| 20.0 * (self.side_rms / self.mid_rms).log10())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EnvelopeMetrics {
    pub p5_dbfs: f64,
    pub p95_dbfs: f64,
    /// `p95_dbfs - p5_dbfs`, a compact proxy for pumping and long dynamic
    /// excursions.
    pub spread_decibels: f64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LevelStabilityMetrics {
    pub window_seconds: f64,
    pub windows: usize,
    pub min_rms_dbfs: f64,
    pub max_rms_dbfs: f64,
    pub spread_decibels: f64,
    pub peak_max_dbfs: Option<f64>,
}

/// Similarity of consecutive, scheduler-aligned waveform windows.
///
/// Correlation is a normalized dot product over every interleaved channel.
/// It is amplitude-independent but deliberately phase-sensitive: alternating
/// polarity is not the same waveform reset.
#[derive(Clone, PartialEq, Debug)]
pub struct OnsetFingerprintMetrics {
    pub window_seconds: f64,
    pub correlations: Vec<f64>,
    pub median_correlation: f64,
    pub maximum_correlation: f64,
}

impl SpectralMetrics {
    pub fn band_decibels(self) -> [Option<f64>; 4] {
        self.band_powers
            .map(|power| (power.is_finite() && power > 0.0).then(|| 10.0 * power.log10()))
    }

    pub fn third_octave_decibels(self) -> [Option<f64>; 30] {
        self.third_octave_powers
            .map(|power| (power.is_finite() && power > 0.0).then(|| 10.0 * power.log10()))
    }
}

impl ChannelMetrics {
    pub fn rms_decibels(self) -> Option<f64> {
        amplitude_decibels(self.rms)
    }

    pub fn peak_decibels(self) -> Option<f64> {
        amplitude_decibels(f64::from(self.peak))
    }
}

/// Convert a positive linear amplitude to dBFS. Exact silence has no finite
/// logarithmic level and is returned as `None`.
pub fn amplitude_decibels(amplitude: f64) -> Option<f64> {
    (amplitude.is_finite() && amplitude > 0.0).then(|| 20.0 * amplitude.log10())
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

    /// Measure every interleaved channel independently without allocating or
    /// transforming another copy of the audio.
    pub fn channel_metrics(&self) -> Vec<ChannelMetrics> {
        let mut sums = vec![0.0f64; self.channels];
        let mut squares = vec![0.0f64; self.channels];
        let mut peaks = vec![0.0f32; self.channels];
        let mut frames = 0usize;
        for frame in self.samples.chunks_exact(self.channels) {
            frames += 1;
            for (channel, &sample) in frame.iter().enumerate() {
                let sample64 = f64::from(sample);
                sums[channel] += sample64;
                squares[channel] += sample64 * sample64;
                peaks[channel] = peaks[channel].max(sample.abs());
            }
        }
        if frames == 0 {
            return peaks
                .into_iter()
                .map(|peak| ChannelMetrics {
                    dc: 0.0,
                    rms: 0.0,
                    peak,
                })
                .collect();
        }
        let frames = frames as f64;
        sums.into_iter()
            .zip(squares)
            .zip(peaks)
            .map(|((sum, square), peak)| ChannelMetrics {
                dc: sum / frames,
                rms: (square / frames).sqrt(),
                peak,
            })
            .collect()
    }

    /// Measure the complete interleaved output as one signal. This is the
    /// appropriate scalar summary for comparing tracks with different channel
    /// counts; per-lane diagnosis should use [`Self::channel_metrics`].
    pub fn metrics(&self) -> ChannelMetrics {
        if self.samples.is_empty() {
            return ChannelMetrics {
                dc: 0.0,
                rms: 0.0,
                peak: 0.0,
            };
        }
        let mut sum = 0.0;
        let mut squares = 0.0;
        let mut peak = 0.0f32;
        for &sample in &self.samples {
            let sample64 = f64::from(sample);
            sum += sample64;
            squares += sample64 * sample64;
            peak = peak.max(sample.abs());
        }
        let samples = self.samples.len() as f64;
        ChannelMetrics {
            dc: sum / samples,
            rms: (squares / samples).sqrt(),
            peak,
        }
    }

    /// Compare consecutive onset-aligned waveform windows.
    ///
    /// `onsets_seconds` uses the absolute clock reported by the scheduler;
    /// [`Self::start_seconds`] maps it into this render's retained sample
    /// window. Comparisons with silence on either side are omitted because a
    /// normalized correlation is undefined there.
    pub fn onset_fingerprint_metrics(
        &self,
        onsets_seconds: &[f64],
        window_seconds: f64,
    ) -> Option<OnsetFingerprintMetrics> {
        if self.channels == 0
            || !self.sample_rate.is_finite()
            || self.sample_rate <= 0.0
            || !window_seconds.is_finite()
            || window_seconds <= 0.0
        {
            return None;
        }
        let window_frames = (window_seconds * self.sample_rate).round() as usize;
        if window_frames == 0 {
            return None;
        }
        let starts = onsets_seconds
            .iter()
            .filter_map(|onset| {
                let absolute_frame = (onset * self.sample_rate).ceil();
                let first_frame = (self.start_seconds * self.sample_rate).ceil();
                if !absolute_frame.is_finite() || absolute_frame < first_frame {
                    return None;
                }
                // Sequencer starts and the retained render boundary both use
                // the first sample at or after their wall-clock coordinate.
                // Subtract those absolute frame indices so a fractional range
                // boundary cannot introduce a one-sample alignment error.
                let frame = (absolute_frame - first_frame) as usize;
                (frame.saturating_add(window_frames) <= self.frames()).then_some(frame)
            })
            .collect::<Vec<_>>();
        let correlations = starts
            .windows(2)
            .filter_map(|pair| {
                let left_begin = pair[0] * self.channels;
                let right_begin = pair[1] * self.channels;
                let samples = window_frames * self.channels;
                normalized_correlation(
                    &self.samples[left_begin..left_begin + samples],
                    &self.samples[right_begin..right_begin + samples],
                )
            })
            .collect::<Vec<_>>();
        if correlations.is_empty() {
            return None;
        }
        let mut ordered = correlations.clone();
        ordered.sort_by(f64::total_cmp);
        let middle = ordered.len() / 2;
        let median_correlation = if ordered.len() % 2 == 0 {
            (ordered[middle - 1] + ordered[middle]) * 0.5
        } else {
            ordered[middle]
        };
        let maximum_correlation = *ordered.last().expect("correlations is not empty");
        Some(OnsetFingerprintMetrics {
            window_seconds: window_frames as f64 / self.sample_rate,
            correlations,
            median_correlation,
            maximum_correlation,
        })
    }

    /// Welch spectral centroid using 32,768-sample Hann windows at 50%
    /// overlap. Channel powers are accumulated independently so an
    /// antiphase stereo signal does not disappear during analysis.
    pub fn spectral_metrics(&self) -> Option<SpectralMetrics> {
        if self.channels == 0 || self.samples.is_empty() {
            return None;
        }
        let frames = self.frames();
        let starts = if frames <= ANALYSIS_FFT_SIZE {
            vec![0]
        } else {
            (0..=frames - ANALYSIS_FFT_SIZE)
                .step_by(ANALYSIS_FFT_HOP)
                .collect()
        };
        let mut powers = vec![0.0f64; ANALYSIS_FFT_SIZE / 2];
        let mut window = Box::new([0.0f32; ANALYSIS_FFT_SIZE]);
        let mut window_square_sum = 0.0f64;
        for offset in 0..ANALYSIS_FFT_SIZE {
            let phase = offset as f64 / (ANALYSIS_FFT_SIZE - 1) as f64;
            let hann = 0.5 - 0.5 * (core::f64::consts::TAU * phase).cos();
            window_square_sum += hann * hann;
        }
        for channel in 0..self.channels {
            for &start in &starts {
                window.fill(0.0);
                let available = frames.saturating_sub(start).min(ANALYSIS_FFT_SIZE);
                for offset in 0..available {
                    let phase = offset as f64 / (ANALYSIS_FFT_SIZE - 1) as f64;
                    let hann = 0.5 - 0.5 * (core::f64::consts::TAU * phase).cos();
                    window[offset] =
                        self.samples[(start + offset) * self.channels + channel] * hann as f32;
                }
                let spectrum = microfft::real::rfft_32768(window.as_mut());
                // microfft packs the real Nyquist coefficient into DC's
                // imaginary lane. DC is excluded from the centroid anyway.
                spectrum[0].im = 0.0;
                for (bin, value) in spectrum.iter().enumerate().skip(1) {
                    powers[bin] += f64::from(value.norm_sqr());
                }
            }
        }
        let total = powers.iter().sum::<f64>();
        if !(total.is_finite() && total > 0.0) {
            return None;
        }
        let bin_hz = self.sample_rate / ANALYSIS_FFT_SIZE as f64;
        let centroid_hz = powers
            .iter()
            .enumerate()
            .map(|(bin, power)| bin as f64 * bin_hz * power)
            .sum::<f64>()
            / total;
        let observations = (starts.len() * self.channels) as f64;
        let power_scale = 2.0 / (ANALYSIS_FFT_SIZE as f64 * window_square_sum * observations);
        let mut band_powers = [0.0; 4];
        let mut third_octave_powers = [0.0; 30];
        let edge_ratio = 2.0f64.powf(1.0 / 6.0);
        for (bin, &power) in powers.iter().enumerate().skip(1) {
            let hz = bin as f64 * bin_hz;
            if let Some((band, _)) = ANALYSIS_BANDS_HZ
                .iter()
                .enumerate()
                .find(|(_, (low, high))| hz >= *low && hz < *high)
            {
                band_powers[band] += power * power_scale;
            }
            if let Some(band) = (0..THIRD_OCTAVE_CENTERS_HZ.len()).find(|&band| {
                let exponent = (band as f64 - 16.0) / 3.0;
                let exact_center = 1_000.0 * 2.0f64.powf(exponent);
                hz >= exact_center / edge_ratio && hz < exact_center * edge_ratio
            }) {
                third_octave_powers[band] += power * power_scale;
            }
        }
        Some(SpectralMetrics {
            centroid_hz,
            band_powers,
            third_octave_powers,
        })
    }

    pub fn spectral_centroid_hz(&self) -> Option<f64> {
        self.spectral_metrics().map(|metrics| metrics.centroid_hz)
    }

    /// Stereo width as energy in the difference signal relative to the sum.
    /// Non-stereo layouts deliberately return `None` instead of inventing a
    /// downmix policy.
    pub fn stereo_metrics(&self) -> Option<StereoMetrics> {
        if self.channels != 2 || self.frames() == 0 {
            return None;
        }
        let mut mid_square = 0.0;
        let mut side_square = 0.0;
        for frame in self.samples.chunks_exact(2) {
            let left = f64::from(frame[0]);
            let right = f64::from(frame[1]);
            mid_square += (left + right).powi(2);
            side_square += (left - right).powi(2);
        }
        let frames = self.frames() as f64;
        Some(StereoMetrics {
            mid_rms: (mid_square / frames).sqrt(),
            side_rms: (side_square / frames).sqrt(),
        })
    }

    /// Percentile spread of a 20 ms whole-layout RMS envelope. Exact-silent
    /// windows use a declared -120 dBFS analysis floor so arrangements with
    /// rests remain finite and comparable.
    pub fn envelope_metrics_20ms(&self) -> Option<EnvelopeMetrics> {
        if self.channels == 0
            || self.samples.is_empty()
            || !(self.sample_rate.is_finite() && self.sample_rate > 0.0)
        {
            return None;
        }
        let window_frames = (self.sample_rate * 0.020).round().max(1.0) as usize;
        let window_samples = window_frames.saturating_mul(self.channels);
        let mut levels = self
            .samples
            .chunks(window_samples)
            .map(|window| {
                let mean_square = window
                    .iter()
                    .map(|&sample| f64::from(sample).powi(2))
                    .sum::<f64>()
                    / window.len() as f64;
                amplitude_decibels(mean_square.sqrt())
                    .unwrap_or(-120.0)
                    .max(-120.0)
            })
            .collect::<Vec<_>>();
        levels.sort_by(f64::total_cmp);
        let percentile = |fraction: f64| {
            let position = fraction * (levels.len() - 1) as f64;
            let low = position.floor() as usize;
            let high = position.ceil() as usize;
            levels[low] + (levels[high] - levels[low]) * position.fract()
        };
        let p5_dbfs = percentile(0.05);
        let p95_dbfs = percentile(0.95);
        Some(EnvelopeMetrics {
            p5_dbfs,
            p95_dbfs,
            spread_decibels: p95_dbfs - p5_dbfs,
        })
    }

    /// Whole-layout RMS variation across fixed wall-clock windows. Only the
    /// scheduled music is measured; the explicit response tail is excluded so
    /// it cannot look like a decaying arrangement. Silent windows use the same
    /// -120 dBFS floor as [`Self::envelope_metrics_20ms`].
    pub fn level_stability(&self, window_seconds: f64) -> Option<LevelStabilityMetrics> {
        if self.channels == 0
            || self.samples.is_empty()
            || !(self.sample_rate.is_finite() && self.sample_rate > 0.0)
            || !(window_seconds.is_finite() && window_seconds > 0.0)
        {
            return None;
        }
        let scheduled_frames =
            ((self.scheduled_seconds * self.sample_rate).ceil() as usize).min(self.frames());
        if scheduled_frames == 0 {
            return None;
        }
        let window_frames = (window_seconds * self.sample_rate).round().max(1.0) as usize;
        let complete_windows = scheduled_frames / window_frames;
        let measured_windows = complete_windows.max(1);
        let mut min_rms_dbfs = f64::INFINITY;
        let mut max_rms_dbfs = f64::NEG_INFINITY;
        let mut peak = 0.0f32;
        for index in 0..measured_windows {
            let begin = index * window_frames * self.channels;
            let end_frame = if complete_windows == 0 {
                scheduled_frames
            } else {
                (index + 1) * window_frames
            };
            let end = end_frame * self.channels;
            let window = &self.samples[begin..end];
            let mean_square = window
                .iter()
                .map(|&sample| {
                    peak = peak.max(sample.abs());
                    f64::from(sample).powi(2)
                })
                .sum::<f64>()
                / window.len() as f64;
            let rms_dbfs = amplitude_decibels(mean_square.sqrt())
                .unwrap_or(-120.0)
                .max(-120.0);
            min_rms_dbfs = min_rms_dbfs.min(rms_dbfs);
            max_rms_dbfs = max_rms_dbfs.max(rms_dbfs);
        }
        Some(LevelStabilityMetrics {
            window_seconds,
            windows: measured_windows,
            min_rms_dbfs,
            max_rms_dbfs,
            spread_decibels: max_rms_dbfs - min_rms_dbfs,
            peak_max_dbfs: amplitude_decibels(f64::from(peak)),
        })
    }
}

fn normalized_correlation(left: &[f32], right: &[f32]) -> Option<f64> {
    debug_assert_eq!(left.len(), right.len());
    let (dot, left_energy, right_energy) = left.iter().zip(right).fold(
        (0.0, 0.0, 0.0),
        |(dot, left_energy, right_energy), (&left, &right)| {
            let left = f64::from(left);
            let right = f64::from(right);
            (
                dot + left * right,
                left_energy + left * left,
                right_energy + right * right,
            )
        },
    );
    let denominator = (left_energy * right_energy).sqrt();
    (denominator.is_finite() && denominator > 0.0).then(|| (dot / denominator).clamp(-1.0, 1.0))
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
            .field("tracks", &self.tracks)
            .field("scheduled_seconds", &self.scheduled_seconds)
            .field("start_seconds", &self.start_seconds)
            .field("peak", &self.peak)
            .field("clipped", &self.clipped)
            .finish_non_exhaustive()
    }
}

pub fn render(program: &Program, options: &RenderOptions) -> Result<Rendered, RenderError> {
    if !(options.sample_rate.is_finite() && options.sample_rate > 0.0) {
        return Err(RenderError::SampleRate(options.sample_rate));
    }
    let (start_seconds, end_seconds) = match options.span {
        RenderSpan::Seconds(seconds) => (0.0, seconds),
        RenderSpan::Cycles(cycles) => (0.0, program.tempo.cycle_to_seconds(cycles)),
        RenderSpan::SecondsRange { begin, end } => (begin, end),
        RenderSpan::CyclesRange { begin, end } => (
            program.tempo.cycle_to_seconds(begin),
            program.tempo.cycle_to_seconds(end),
        ),
    };
    if !(start_seconds.is_finite()
        && end_seconds.is_finite()
        && start_seconds >= 0.0
        && end_seconds > start_seconds)
    {
        return Err(RenderError::Window {
            begin: start_seconds,
            end: end_seconds,
        });
    }
    let scheduled_seconds = end_seconds - start_seconds;
    if !(scheduled_seconds.is_finite() && scheduled_seconds > 0.0) {
        return Err(RenderError::Duration(scheduled_seconds));
    }
    let tail_seconds = if options.tail_seconds.is_finite() && options.tail_seconds > 0.0 {
        options.tail_seconds
    } else {
        0.0
    };

    let track_indices = options.tracks.selected_indices(program.tracks.len())?;
    let main_channels =
        playable_channels_for_tracks(program, &track_indices).map_err(RenderError::Binding)?;
    let mut persistent = needs_persistent_runtime(program)
        .then(|| persistent_runtime(program).map_err(RenderError::Binding))
        .transpose()?;

    let tracks = scheduled_tracks_for(program, &track_indices).map_err(RenderError::Binding)?;

    let mut sequencer = match &persistent {
        Some(runtime) => runtime.sequencer(),
        None => {
            let first = tracks
                .first()
                .map(|track| track.template)
                .or_else(|| {
                    program
                        .tracks
                        .first()
                        .and_then(|track| program.voice(track.voice))
                })
                .ok_or(RenderError::NothingToRender)?;
            PitchScheduler::sequencer(first)
        }
    };
    sequencer.set_sample_rate(options.sample_rate);
    sequencer.allocate();

    // Offline, the whole window is scheduled up front. There is no frontier to
    // advance and no deadline to miss, so lookahead buys nothing here.
    let mut scheduler = ProgramScheduler::default();
    let runs = if persistent.is_some() {
        scheduled_runs(program).map_err(RenderError::Binding)?
    } else {
        Vec::new()
    };
    if start_seconds > 0.0 {
        match &persistent {
            Some(runtime) => {
                scheduler.fill_routed_program_to_seconds_tempo_map(
                    start_seconds,
                    tracks.iter().copied(),
                    runs.iter().copied(),
                    &program.tempo,
                    &mut sequencer,
                    routed_runtime(runtime),
                )?;
            }
            None => {
                scheduler.fill_to_seconds_tempo_map(
                    start_seconds,
                    tracks.iter().copied(),
                    &program.tempo,
                    &mut sequencer,
                )?;
            }
        }
    }
    let report = match &persistent {
        Some(runtime) => scheduler.fill_routed_program_to_seconds_tempo_map(
            end_seconds,
            tracks.iter().copied(),
            runs.iter().copied(),
            &program.tempo,
            &mut sequencer,
            routed_runtime(runtime),
        )?,
        None => scheduler.fill_to_seconds_tempo_map(
            end_seconds,
            tracks.iter().copied(),
            &program.tempo,
            &mut sequencer,
        )?,
    };

    let stem_channels = persistent
        .as_ref()
        .map_or(main_channels, |runtime| runtime.layout().total_channels());
    let channels = if options.stems || options.raw_stems {
        stem_channels
    } else {
        main_channels
    };

    let mut renderer: Box<dyn AudioUnit> = if options.raw_stems {
        Box::new(sequencer)
    } else {
        match persistent.as_mut() {
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
        }
    };
    renderer.set_sample_rate(options.sample_rate);
    renderer.allocate();

    let first_frame = (start_seconds * options.sample_rate).ceil() as usize;
    let frames = (((end_seconds + tail_seconds) * options.sample_rate).ceil() as usize).max(1);
    let input = BufferVec::new(renderer.inputs());
    let mut block = BufferVec::new(channels);
    let mut samples = Vec::with_capacity(frames.saturating_sub(first_frame) * channels);
    let mut peak = 0.0f32;
    let mut clipped = 0usize;

    let mut done = 0;
    while done < frames {
        let size = (frames - done).min(MAX_BUFFER_SIZE);
        renderer.process(size, &input.buffer_ref(), &mut block.buffer_mut());
        for frame in 0..size {
            if done + frame < first_frame {
                continue;
            }
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

    let track_reports = track_indices
        .into_iter()
        .zip(report.tracks)
        .map(|(index, track)| RenderedTrackReport {
            index,
            voices: track.voices,
            distinct_onsets: track.distinct_onsets,
            min_interval_seconds: track.min_interval_seconds,
            onsets_seconds: track.onsets_seconds,
            intervals_seconds: track.intervals_seconds,
        })
        .collect();

    Ok(Rendered {
        sample_rate: options.sample_rate,
        channels,
        samples,
        format: options.format,
        voices: report.voices,
        tracks: track_reports,
        scheduled_seconds,
        start_seconds,
        peak,
        clipped,
    })
}

/// Write a render to a WAV file, replacing an existing file.
pub fn write_wav(path: impl AsRef<Path>, rendered: &Rendered) -> Result<(), RenderError> {
    write_wav_file(path.as_ref(), rendered, true)
}

/// Write a render only if the destination does not exist. The file is claimed
/// with `create_new`, so a concurrent writer cannot slip between an existence
/// check and file creation and have its material overwritten.
pub fn write_wav_new(path: impl AsRef<Path>, rendered: &Rendered) -> Result<(), RenderError> {
    write_wav_file(path.as_ref(), rendered, false)
}

fn write_wav_file(path: &Path, rendered: &Rendered, overwrite: bool) -> Result<(), RenderError> {
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
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let file = options
        .open(path)
        .map_err(|error| RenderError::Wav(hound::Error::IoError(error)))?;
    let mut writer =
        hound::WavWriter::new(std::io::BufWriter::new(file), spec).map_err(RenderError::Wav)?;
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
    Window {
        begin: f64,
        end: f64,
    },
    TrackIndex {
        index: usize,
        tracks: usize,
    },
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
            RenderError::Window { begin, end } => write!(
                f,
                "{begin}..{end} is not a usable render window; require 0 <= begin < end"
            ),
            RenderError::TrackIndex { index, tracks } => write!(
                f,
                "track {index} does not exist; program track count is {tracks}"
            ),
            RenderError::ChannelCount(channels) => {
                write!(f, "a WAV file cannot carry {channels} channels")
            }
            RenderError::Schedule(error) => write!(f, "scheduling: {error}"),
            RenderError::Wav(error) => write!(f, "writing the audio file: {error}"),
        }
    }
}

impl core::error::Error for RenderError {}
