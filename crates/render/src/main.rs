//! `apteronotus-render` — evaluate a song and write it to an audio file.
//!
//! The point of this binary is not export, it is measurement. A rendered file
//! is the only way anything outside the process can inspect what the engine
//! produced: a spectral analysis, a regression against a previous render, or a
//! comparison against the reference recording a song is being built to
//! resemble. Playback already exists; being able to look at the result did
//! not.

use apteronotus_lua::{Program, evaluate};
use apteronotus_pattern::Frac;
use apteronotus_render::{
    ChannelMetrics, OnsetFingerprintMetrics, RenderOptions, RenderSpan, Rendered,
    RenderedTrackReport, SampleFormat, SpectralMetrics, render, stem_lane_labels, write_wav,
    write_wav_new,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: apteronotus-render <song.eod> [options]
       apteronotus-render --song <name> [options]
       apteronotus-render --list-songs

      --song <name>       render an embedded corpus song, from any directory
      --list-songs        list embedded song names and descriptions; write no file
      --check-syntax      compile Lua syntax without evaluation or output files
  -o, --output <file.wav>  where to write        (default: the song's name, beside it)
      --seconds <n|a..b>   duration or window   (default: 30)
      --cycles <n[/d]|a..b> duration or exact cycle window, through the tempo map
      --tail <n>           rendered past the end (default: 2)
      --sample-rate <n>    in hertz              (default: 48000)
      --list-tracks        evaluate and list zero-based track indices; write no file
      --solo-track <i,...> schedule only these tracks, after evaluation
      --mute-track <i,...> exclude these tracks, after evaluation
      --track-summary      render each selected track and print level/DC measurements
      --spectrum           print fixed-band levels and spectral centroid for the mix
      --third-octaves      print the Welch curve relative to its 1 kHz band
      --dynamics           print stereo side/mid and 20 ms envelope spread
      --fingerprints       compare consecutive 50 ms onset-aligned track waveforms
      --reference <wav>    compare third octaves, regions, width and envelope
      --stability <secs>   report fixed-window RMS spread over scheduled music
      --json               print a complete versioned analysis document (implies track analysis)
      --stems              write every post-processor routed lane
      --raw-stems          write routed lanes before persistent processing
      --pcm16              16-bit PCM instead of 32-bit float
  -f, --force              overwrite an existing output file
  -q, --quiet              print nothing on success
  -h, --help               this text
";

const FINGERPRINT_WINDOW_SECONDS: f64 = 0.050;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--list-songs"] {
        for song in apteronotus_songs::SONGS {
            println!("{} — {}\n  {}", song.name, song.title, song.stresses);
        }
        return ExitCode::SUCCESS;
    }
    let invocation = match parse(&arguments) {
        Ok(Some(invocation)) => invocation,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("apteronotus-render: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match run(&invocation) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("apteronotus-render: {message}");
            ExitCode::FAILURE
        }
    }
}

enum Input {
    File(PathBuf),
    Song(&'static apteronotus_songs::Song),
}

impl Input {
    fn label(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::Song(song) => format!("embedded:{}", song.name),
        }
    }

    fn source(&self) -> Result<std::borrow::Cow<'static, str>, String> {
        match self {
            Self::File(path) => std::fs::read_to_string(path)
                .map(std::borrow::Cow::Owned)
                .map_err(|error| format!("cannot read {}: {error}", path.display())),
            Self::Song(song) => Ok(std::borrow::Cow::Borrowed(song.source)),
        }
    }

    fn default_output(&self) -> PathBuf {
        match self {
            Self::File(path) => default_output(path),
            Self::Song(song) => PathBuf::from(format!("{}.wav", song.name)),
        }
    }
}

struct Invocation {
    input: Input,
    output: PathBuf,
    options: RenderOptions,
    list_tracks: bool,
    check_syntax: bool,
    track_summary: bool,
    spectrum: bool,
    third_octaves: bool,
    dynamics: bool,
    fingerprints: bool,
    reference: Option<PathBuf>,
    stability_seconds: Option<f64>,
    json: bool,
    force: bool,
    quiet: bool,
}

fn run(invocation: &Invocation) -> Result<(), String> {
    if !invocation.list_tracks
        && !invocation.check_syntax
        && invocation.output.exists()
        && !invocation.force
    {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            invocation.output.display()
        ));
    }

    let source = invocation.input.source()?;
    if invocation.check_syntax {
        apteronotus_lua::check_syntax(&source).map_err(|error| error.to_string())?;
        if !invocation.quiet {
            println!(
                "{}: Lua syntax valid (not evaluated)",
                invocation.input.label()
            );
        }
        return Ok(());
    }
    let program = evaluate(&source).map_err(|error| format!("{error}"))?;
    if invocation.list_tracks {
        print!("{}", describe_tracks(&program, &invocation.options)?);
        return Ok(());
    }
    // Read before writing: with --force, an accidentally identical output and
    // reference path must not turn the candidate into its own reference.
    let reference = invocation
        .reference
        .as_ref()
        .map(|path| read_reference(path))
        .transpose()?;

    let rendered = render(&program, &invocation.options).map_err(|error| format!("{error}"))?;
    if invocation.force {
        write_wav(&invocation.output, &rendered)
    } else {
        write_wav_new(&invocation.output, &rendered)
    }
    .map_err(|error| format!("cannot write {}: {error}", invocation.output.display()))?;

    // Silence and clipping are both worth interrupting for even in quiet mode:
    // one means the render proved nothing, the other means it is not a
    // faithful measurement of the mix.
    if rendered.peak == 0.0 {
        eprintln!(
            "apteronotus-render: warning: {} is entirely silent",
            invocation.output.display()
        );
    }
    if rendered.clipped > 0 {
        eprintln!(
            "apteronotus-render: warning: {} samples exceed full scale (peak {:+.1} dBFS)",
            rendered.clipped,
            rendered.peak_decibels().unwrap_or(0.0)
        );
    }
    let track_analysis = (invocation.track_summary || invocation.fingerprints || invocation.json)
        .then(|| {
            analyze_tracks(
                &program,
                &invocation.options,
                invocation.fingerprints || invocation.json,
            )
        })
        .transpose()?;
    if invocation.json {
        println!(
            "{}",
            analysis_json(
                &program,
                &rendered,
                &invocation.options,
                track_analysis.as_deref().unwrap_or_default(),
                invocation.reference.as_deref().zip(reference.as_ref()),
            )?
        );
        return Ok(());
    }
    if invocation.quiet {
        return Ok(());
    }

    let peak = match rendered.peak_decibels() {
        Some(decibels) => format!("{decibels:+.1} dBFS"),
        None => "silent".to_string(),
    };
    println!(
        "{} → {}",
        invocation.input.label(),
        invocation.output.display()
    );
    println!(
        "  {:.2}..{:.2} s ({:.2} s scheduled + {:.2} s tail), {} Hz, {} ch, {}",
        rendered.start_seconds,
        rendered.start_seconds + rendered.scheduled_seconds,
        rendered.scheduled_seconds,
        rendered.duration_seconds() - rendered.scheduled_seconds,
        rendered.sample_rate.round(),
        rendered.channels,
        match rendered.format {
            SampleFormat::Float32 => "float32",
            SampleFormat::Pcm16 => "pcm16",
        }
    );
    println!("  {} voices, peak {peak}", rendered.voices);
    if invocation.options.stems || invocation.options.raw_stems {
        print!(
            "{}",
            describe_stems(&program, &rendered, invocation.options.raw_stems)?
        );
    }
    let spectral = (invocation.spectrum || invocation.third_octaves)
        .then(|| rendered.spectral_metrics())
        .flatten();
    if invocation.spectrum {
        print!("{}", describe_spectrum(spectral));
    }
    if invocation.third_octaves {
        print!("{}", describe_third_octaves(spectral));
    }
    if invocation.dynamics {
        print!("{}", describe_dynamics(&rendered));
    }
    if let Some(reference) = &reference {
        print!("{}", describe_reference(&rendered, reference));
    }
    if let Some(seconds) = invocation.stability_seconds {
        print!("{}", describe_stability(&rendered, seconds)?);
    }
    if invocation.track_summary {
        print!(
            "{}",
            describe_track_summary(track_analysis.as_deref().unwrap_or_default())
        );
    }
    if invocation.fingerprints {
        print!(
            "{}",
            describe_fingerprints(track_analysis.as_deref().unwrap_or_default())
        );
    }
    Ok(())
}

fn describe_spectrum(spectral: Option<SpectralMetrics>) -> String {
    let Some(spectral) = spectral else {
        return "  spectrum: silent\n".to_string();
    };
    let mut text = format!("  spectrum: centroid {:.0} Hz\n", spectral.centroid_hz);
    for ((low, high), level) in apteronotus_render::ANALYSIS_BANDS_HZ
        .into_iter()
        .zip(spectral.band_decibels())
    {
        let label = if high.is_finite() {
            format!("{low:.0}-{high:.0} Hz")
        } else {
            format!("{low:.0}-nyquist Hz")
        };
        match level {
            Some(level) => text.push_str(&format!("    {label:<17} {level:+6.1} dBFS\n")),
            None => text.push_str(&format!("    {label:<17} silent\n")),
        }
    }
    text
}

fn describe_third_octaves(spectral: Option<SpectralMetrics>) -> String {
    let Some(spectral) = spectral else {
        return "  third octaves: silent\n".to_string();
    };
    let levels = spectral.third_octave_decibels();
    let anchor = levels[16];
    let mut text = String::from("  third octaves (relative to 1 kHz):\n");
    for ((center, absolute), relative) in apteronotus_render::THIRD_OCTAVE_CENTERS_HZ
        .into_iter()
        .zip(levels)
        .map(|(center, level)| {
            (
                (center, level),
                level.zip(anchor).map(|(level, anchor)| level - anchor),
            )
        })
    {
        match (absolute, relative) {
            (Some(absolute), Some(relative)) => text.push_str(&format!(
                "    {center:>7.1} Hz  {absolute:+6.1} dBFS  {relative:+6.1} dB\n"
            )),
            _ => text.push_str(&format!("    {center:>7.1} Hz  silent\n")),
        }
    }
    text
}

fn describe_dynamics(rendered: &Rendered) -> String {
    let stereo = rendered.stereo_metrics().map_or_else(
        || "unavailable".to_string(),
        |metrics| {
            metrics.side_mid_decibels().map_or_else(
                || {
                    if metrics.mid_rms > 0.0 && metrics.side_rms == 0.0 {
                        "-inf dB (mono)".to_string()
                    } else {
                        "undefined".to_string()
                    }
                },
                |decibels| format!("{decibels:+.1} dB"),
            )
        },
    );
    let envelope = rendered.envelope_metrics_20ms().map_or_else(
        || "unavailable".to_string(),
        |metrics| {
            format!(
                "{:.1} dB (p5 {:+.1}, p95 {:+.1} dBFS)",
                metrics.spread_decibels, metrics.p5_dbfs, metrics.p95_dbfs
            )
        },
    );
    format!("  dynamics: side/mid {stereo}, envelope spread {envelope}\n")
}

fn describe_reference(candidate: &Rendered, reference: &Rendered) -> String {
    let candidate_thirds = relative_third_octaves(candidate.spectral_metrics());
    let reference_thirds = relative_third_octaves(reference.spectral_metrics());
    let deltas: [Option<f64>; 30] = std::array::from_fn(|index| {
        candidate_thirds[index]
            .zip(reference_thirds[index])
            .map(|(candidate, reference)| candidate - reference)
    });
    let mut text = format!(
        "  reference: {:.2} s, {} Hz, {} ch\n",
        reference.duration_seconds(),
        reference.sample_rate.round(),
        reference.channels
    );
    text.push_str("    third-octave delta (candidate - reference, relative to 1 kHz):\n");
    for (center, delta) in apteronotus_render::THIRD_OCTAVE_CENTERS_HZ
        .into_iter()
        .zip(deltas)
    {
        match delta {
            Some(delta) => text.push_str(&format!("      {center:>7.1} Hz  {delta:+6.1} dB\n")),
            None => text.push_str(&format!("      {center:>7.1} Hz  unavailable\n")),
        }
    }
    text.push_str("    mean delta by region:\n");
    for (name, begin, end) in THIRD_OCTAVE_REGIONS {
        match mean_options(&deltas[begin..end]) {
            Some(delta) => text.push_str(&format!("      {name:<9} {delta:+6.1} dB\n")),
            None => text.push_str(&format!("      {name:<9} unavailable\n")),
        }
    }
    let side_mid_delta = candidate
        .stereo_metrics()
        .and_then(|metrics| metrics.side_mid_decibels())
        .zip(
            reference
                .stereo_metrics()
                .and_then(|metrics| metrics.side_mid_decibels()),
        )
        .map(|(candidate, reference)| candidate - reference);
    let envelope_delta = candidate
        .envelope_metrics_20ms()
        .zip(reference.envelope_metrics_20ms())
        .map(|(candidate, reference)| candidate.spread_decibels - reference.spread_decibels);
    text.push_str(&format!(
        "    side/mid delta {}\n",
        optional_delta(side_mid_delta)
    ));
    text.push_str(&format!(
        "    envelope-spread delta {}\n",
        optional_delta(envelope_delta)
    ));
    text
}

fn describe_stability(rendered: &Rendered, window_seconds: f64) -> Result<String, String> {
    let metrics = rendered
        .level_stability(window_seconds)
        .ok_or_else(|| format!("{window_seconds} is not a usable stability window"))?;
    let peak = metrics
        .peak_max_dbfs
        .map_or_else(|| "silent".into(), |level| format!("{level:+.1} dBFS"));
    Ok(format!(
        "  stability: {} × {:.2} s windows, RMS {:+.1}..{:+.1} dBFS \
         (spread {:.1} dB), peak max {peak}\n",
        metrics.windows,
        metrics.window_seconds,
        metrics.min_rms_dbfs,
        metrics.max_rms_dbfs,
        metrics.spread_decibels,
    ))
}

const THIRD_OCTAVE_REGIONS: [(&str, usize, usize); 6] = [
    ("sub", 2, 5),
    ("low", 5, 9),
    ("lowmid", 9, 12),
    ("mid", 12, 17),
    ("presence", 17, 22),
    ("air", 22, 26),
];

fn relative_third_octaves(spectral: Option<SpectralMetrics>) -> [Option<f64>; 30] {
    let Some(spectral) = spectral else {
        return [None; 30];
    };
    let levels = spectral.third_octave_decibels();
    let anchor = levels[16];
    levels.map(|level| level.zip(anchor).map(|(level, anchor)| level - anchor))
}

fn mean_options(values: &[Option<f64>]) -> Option<f64> {
    let (sum, count) = values
        .iter()
        .flatten()
        .fold((0.0, 0usize), |(sum, count), value| {
            (sum + value, count + 1)
        });
    (count > 0).then(|| sum / count as f64)
}

fn optional_delta(value: Option<f64>) -> String {
    value.map_or_else(|| "unavailable".into(), |value| format!("{value:+.1} dB"))
}

#[derive(Clone, Debug)]
struct TrackAnalysis {
    timing: RenderedTrackReport,
    metrics: ChannelMetrics,
    spectral: Option<SpectralMetrics>,
    fingerprints: Option<OnsetFingerprintMetrics>,
}

fn analyze_tracks(
    program: &Program,
    options: &RenderOptions,
    fingerprints: bool,
) -> Result<Vec<TrackAnalysis>, String> {
    let selected = options
        .tracks
        .selected_indices(program.tracks.len())
        .map_err(|error| error.to_string())?;
    selected
        .into_iter()
        .map(|index| {
            let mut isolated = options.clone();
            isolated.stems = false;
            isolated.raw_stems = false;
            isolated.tracks = Default::default();
            isolated.tracks.include_only([index]);
            let rendered = render(program, &isolated).map_err(|error| error.to_string())?;
            let timing = rendered
                .tracks
                .iter()
                .find(|track| track.index == index)
                .cloned()
                .ok_or_else(|| format!("scheduler omitted track {index} from its report"))?;
            let fingerprint_metrics = fingerprints.then(|| {
                rendered
                    .onset_fingerprint_metrics(&timing.onsets_seconds, FINGERPRINT_WINDOW_SECONDS)
            });
            Ok(TrackAnalysis {
                timing,
                metrics: rendered.metrics(),
                spectral: rendered.spectral_metrics(),
                fingerprints: fingerprint_metrics.flatten(),
            })
        })
        .collect()
}

fn describe_fingerprints(analysis: &[TrackAnalysis]) -> String {
    let mut text = format!(
        "  onset fingerprints ({:.0} ms, consecutive normalized waveform correlation):\n",
        FINGERPRINT_WINDOW_SECONDS * 1_000.0
    );
    for analysis in analysis {
        let index = analysis.timing.index;
        if let Some(metrics) = &analysis.fingerprints {
            text.push_str(&format!(
                "    track {index:<2} comparisons {:<5} median {:+.3}  max {:+.3}\n",
                metrics.correlations.len(),
                metrics.median_correlation,
                metrics.maximum_correlation,
            ));
        } else {
            text.push_str(&format!(
                "    track {index:<2} unavailable (needs two nonsilent complete windows)\n"
            ));
        }
    }
    text
}

fn describe_track_summary(analysis: &[TrackAnalysis]) -> String {
    let mut text = String::from("  track summary (isolated through production processing):\n");
    for analysis in analysis {
        let index = analysis.timing.index;
        let metrics = analysis.metrics;
        let timing = &analysis.timing;
        let centroid = analysis.spectral.map_or_else(
            || "-".to_string(),
            |value| format!("{:.0} Hz", value.centroid_hz),
        );
        let interval = timing.min_interval_seconds.map_or_else(
            || "-".to_string(),
            |seconds| format!("{:.1} ms", seconds * 1_000.0),
        );
        match (metrics.rms_decibels(), metrics.peak_decibels()) {
            (Some(rms), Some(peak)) => text.push_str(&format!(
                "    track {index:<2} voices {:<5} onsets {:<5} min {interval:<9} \
                 rms {rms:+6.1} dBFS  peak {peak:+6.1} dBFS  centroid {centroid:<9} \
                 dc {:+.6}\n",
                timing.voices, timing.distinct_onsets, metrics.dc
            )),
            _ => text.push_str(&format!(
                "    track {index:<2} voices {:<5} onsets {:<5} min {interval:<9} silent\n",
                timing.voices, timing.distinct_onsets
            )),
        }
    }
    text
}

fn analysis_json(
    program: &Program,
    rendered: &Rendered,
    options: &RenderOptions,
    tracks: &[TrackAnalysis],
    reference: Option<(&Path, &Rendered)>,
) -> Result<String, String> {
    let metrics = rendered.metrics();
    let spectral = rendered.spectral_metrics();
    let stereo = rendered.stereo_metrics();
    let envelope = rendered.envelope_metrics_20ms();
    let stability = rendered.level_stability(5.0);
    let labels = output_lane_labels(program, rendered, options)?;
    let lanes = labels
        .into_iter()
        .zip(rendered.channel_metrics())
        .enumerate()
        .map(|(index, (label, metrics))| {
            metric_json(
                metrics,
                json!({
                    "index": index,
                    "label": label,
                }),
            )
        })
        .collect::<Vec<_>>();
    let tracks = tracks
        .iter()
        .map(|analysis| {
            let index = analysis.timing.index;
            let track = &program.tracks[index];
            let graph = program
                .voice(track.voice)
                .expect("a rendered track has a validated voice");
            metric_json(analysis.metrics, json!({
                "index": index,
                "voice_index": track.voice.index(),
                "channels": graph.channels(),
                "external_trigger": track.external_trigger.is_some(),
                "event_sends": track.routing.sends().len(),
                "graph_sends": graph.sends.len(),
                "voices": analysis.timing.voices,
                "distinct_onsets": analysis.timing.distinct_onsets,
                "min_interval_ms": analysis.timing.min_interval_seconds.map(|seconds| seconds * 1_000.0),
                "onsets_ms": analysis.timing.onsets_seconds.iter().map(|seconds| seconds * 1_000.0).collect::<Vec<_>>(),
                "intervals_ms": analysis.timing.intervals_seconds.iter().map(|seconds| seconds * 1_000.0).collect::<Vec<_>>(),
                "spectral_centroid_hz": analysis.spectral.map(|value| value.centroid_hz),
                "onset_fingerprints": analysis.fingerprints.as_ref().map(|metrics| json!({
                    "window_ms": metrics.window_seconds * 1_000.0,
                    "comparisons": metrics.correlations.len(),
                    "correlations": metrics.correlations,
                    "median_correlation": metrics.median_correlation,
                    "maximum_correlation": metrics.maximum_correlation,
                })),
            }))
        })
        .collect::<Vec<_>>();
    let spectrum = spectral.map(|spectral| {
        let bands = apteronotus_render::ANALYSIS_BANDS_HZ
            .into_iter()
            .zip(spectral.band_decibels())
            .map(|((low, high), dbfs)| {
                json!({
                    "low_hz": low,
                    "high_hz": high.is_finite().then_some(high),
                    "dbfs": dbfs,
                })
            })
            .collect::<Vec<_>>();
        let third_octave_levels = spectral.third_octave_decibels();
        let third_octave_anchor = third_octave_levels[16];
        let third_octaves = apteronotus_render::THIRD_OCTAVE_CENTERS_HZ
            .into_iter()
            .zip(third_octave_levels)
            .map(|(center_hz, dbfs)| {
                json!({
                    "center_hz": center_hz,
                    "dbfs": dbfs,
                    "relative_to_1khz_db": dbfs.zip(third_octave_anchor)
                        .map(|(level, anchor)| level - anchor),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "centroid_hz": spectral.centroid_hz,
            "bands": bands,
            "third_octaves": third_octaves,
        })
    });
    let requested_span = match options.span {
        RenderSpan::Seconds(seconds) => json!({
            "unit": "seconds",
            "amount": seconds,
        }),
        RenderSpan::Cycles(cycles) => json!({
            "unit": "cycles",
            "numerator": cycles.num(),
            "denominator": cycles.den(),
        }),
        RenderSpan::SecondsRange { begin, end } => json!({
            "unit": "seconds",
            "begin": begin,
            "end": end,
        }),
        RenderSpan::CyclesRange { begin, end } => json!({
            "unit": "cycles",
            "begin": {
                "numerator": begin.num(),
                "denominator": begin.den(),
            },
            "end": {
                "numerator": end.num(),
                "denominator": end.den(),
            },
        }),
    };
    let reference = reference.map(|(path, reference)| reference_json(path, rendered, reference));
    let document = json!({
        "schema": "apteronotus.render-analysis.v1",
        "window": {
            "requested": requested_span,
            "scheduled_seconds": rendered.scheduled_seconds,
            "start_seconds": rendered.start_seconds,
            "end_seconds": rendered.start_seconds + rendered.scheduled_seconds,
            "tail_seconds": rendered.duration_seconds() - rendered.scheduled_seconds,
        },
        "output": {
            "sample_rate_hz": rendered.sample_rate,
            "channels": rendered.channels,
            "frames": rendered.frames(),
            "format": match rendered.format {
                SampleFormat::Float32 => "float32",
                SampleFormat::Pcm16 => "pcm16",
            },
            "layout": if options.raw_stems {
                "raw_stems"
            } else if options.stems {
                "stems"
            } else {
                "main"
            },
            "voices": rendered.voices,
            "clipped_samples": rendered.clipped,
        },
        "mix": metric_json(metrics, json!({})),
        "dynamics": {
            "side_mid_db": stereo.and_then(|metrics| metrics.side_mid_decibels()),
            "mid_rms": stereo.map(|metrics| metrics.mid_rms),
            "side_rms": stereo.map(|metrics| metrics.side_rms),
            "envelope_20ms": envelope.map(|metrics| json!({
                "p5_dbfs": metrics.p5_dbfs,
                "p95_dbfs": metrics.p95_dbfs,
                "spread_db": metrics.spread_decibels,
                "floor_dbfs": -120.0,
            })),
            "level_stability_5s": stability.map(|metrics| json!({
                "windows": metrics.windows,
                "min_rms_dbfs": metrics.min_rms_dbfs,
                "max_rms_dbfs": metrics.max_rms_dbfs,
                "spread_db": metrics.spread_decibels,
                "peak_max_dbfs": metrics.peak_max_dbfs,
                "tail_excluded": true,
            })),
        },
        "spectrum": spectrum,
        "tracks": tracks,
        "lanes": lanes,
        "reference": reference,
    });
    serde_json::to_string_pretty(&document).map_err(|error| error.to_string())
}

fn reference_json(path: &Path, candidate: &Rendered, reference: &Rendered) -> Value {
    let candidate_thirds = relative_third_octaves(candidate.spectral_metrics());
    let reference_thirds = relative_third_octaves(reference.spectral_metrics());
    let deltas: [Option<f64>; 30] = std::array::from_fn(|index| {
        candidate_thirds[index]
            .zip(reference_thirds[index])
            .map(|(candidate, reference)| candidate - reference)
    });
    let thirds = apteronotus_render::THIRD_OCTAVE_CENTERS_HZ
        .into_iter()
        .zip(deltas)
        .map(|(center_hz, delta_db)| {
            json!({
                "center_hz": center_hz,
                "delta_db": delta_db,
            })
        })
        .collect::<Vec<_>>();
    let regions = THIRD_OCTAVE_REGIONS
        .into_iter()
        .map(|(name, begin, end)| {
            json!({
                "name": name,
                "mean_delta_db": mean_options(&deltas[begin..end]),
            })
        })
        .collect::<Vec<_>>();
    let candidate_stereo = candidate
        .stereo_metrics()
        .and_then(|metrics| metrics.side_mid_decibels());
    let reference_stereo = reference
        .stereo_metrics()
        .and_then(|metrics| metrics.side_mid_decibels());
    let candidate_envelope = candidate.envelope_metrics_20ms();
    let reference_envelope = reference.envelope_metrics_20ms();
    json!({
        "path": path.to_string_lossy(),
        "sample_rate_hz": reference.sample_rate,
        "channels": reference.channels,
        "duration_seconds": reference.duration_seconds(),
        "third_octaves": thirds,
        "regions": regions,
        "dynamics": {
            "candidate_side_mid_db": candidate_stereo,
            "reference_side_mid_db": reference_stereo,
            "side_mid_delta_db": candidate_stereo.zip(reference_stereo)
                .map(|(candidate, reference)| candidate - reference),
            "candidate_envelope_spread_db": candidate_envelope.map(|metrics| metrics.spread_decibels),
            "reference_envelope_spread_db": reference_envelope.map(|metrics| metrics.spread_decibels),
            "envelope_spread_delta_db": candidate_envelope.zip(reference_envelope)
                .map(|(candidate, reference)| candidate.spread_decibels - reference.spread_decibels),
        },
    })
}

fn read_reference(path: &Path) -> Result<Rendered, String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| format!("cannot read reference {}: {error}", path.display()))?;
    let spec = reader.spec();
    let samples = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("cannot decode reference {}: {error}", path.display()))?,
        (hound::SampleFormat::Int, bits @ 1..=32) => {
            let scale = integer_pcm_scale(bits);
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|sample| sample as f32 / scale))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("cannot decode reference {}: {error}", path.display()))?
        }
        _ => {
            return Err(format!(
                "reference {} uses unsupported {:?}/{}-bit WAV; expected float32 or integer PCM through 32 bits",
                path.display(),
                spec.sample_format,
                spec.bits_per_sample
            ));
        }
    };
    let mut samples = samples;
    let channels = usize::from(spec.channels);
    if channels == 0 || samples.len() % channels != 0 {
        return Err(format!(
            "reference {} has an incomplete channel frame",
            path.display()
        ));
    }
    let mut peak = 0.0f32;
    let mut clipped = 0usize;
    for sample in &mut samples {
        if sample.is_finite() {
            peak = peak.max(sample.abs());
            clipped += usize::from(sample.abs() > 1.0);
        } else {
            *sample = 0.0;
            clipped += 1;
        }
    }
    let frames = samples.len() / channels;
    let sample_rate = f64::from(spec.sample_rate);
    Ok(Rendered {
        sample_rate,
        channels,
        samples,
        // References are normalized into f32 regardless of their on-disk
        // encoding. `format` describes this in-memory representation here;
        // candidate output still retains its requested write format.
        format: SampleFormat::Float32,
        voices: 0,
        tracks: Vec::new(),
        scheduled_seconds: frames as f64 / sample_rate,
        start_seconds: 0.0,
        peak,
        clipped,
    })
}

fn integer_pcm_scale(bits_per_sample: u16) -> f32 {
    (1_u64 << (bits_per_sample - 1)) as f32
}

fn metric_json(metrics: ChannelMetrics, mut object: Value) -> Value {
    let fields = object
        .as_object_mut()
        .expect("metric JSON starts as an object");
    fields.insert("dc".into(), json!(metrics.dc));
    fields.insert("rms".into(), json!(metrics.rms));
    fields.insert("rms_dbfs".into(), json!(metrics.rms_decibels()));
    fields.insert("peak".into(), json!(metrics.peak));
    fields.insert("peak_dbfs".into(), json!(metrics.peak_decibels()));
    object
}

fn output_lane_labels(
    program: &Program,
    rendered: &Rendered,
    options: &RenderOptions,
) -> Result<Vec<String>, String> {
    if options.stems || options.raw_stems {
        let labels = stem_lane_labels(program);
        if labels.len() != rendered.channels {
            return Err(format!(
                "stem manifest has {} lanes but the render has {} channels",
                labels.len(),
                rendered.channels
            ));
        }
        return Ok(labels);
    }
    Ok(match rendered.channels {
        1 => vec!["main".into()],
        2 => vec!["main.L".into(), "main.R".into()],
        channels => (0..channels).map(|index| format!("main.{index}")).collect(),
    })
}

fn describe_stems(program: &Program, rendered: &Rendered, raw: bool) -> Result<String, String> {
    let labels = stem_lane_labels(program);
    if labels.len() != rendered.channels {
        return Err(format!(
            "stem manifest has {} lanes but the render has {} channels",
            labels.len(),
            rendered.channels
        ));
    }
    let mut text = if raw {
        String::from("  raw pre-processor stem lanes:\n")
    } else {
        String::from("  post-processor stem lanes:\n")
    };
    for (index, (label, metrics)) in labels
        .into_iter()
        .zip(rendered.channel_metrics())
        .enumerate()
    {
        match (metrics.rms_decibels(), metrics.peak_decibels()) {
            (Some(rms), Some(peak)) => text.push_str(&format!(
                "    lane {index:<2} {label:<10} rms {rms:+6.1} dBFS  \
                 peak {peak:+6.1} dBFS  dc {:+.6}\n",
                metrics.dc
            )),
            _ => text.push_str(&format!("    lane {index:<2} {label:<10} silent\n")),
        }
    }
    Ok(text)
}

fn describe_tracks(program: &Program, options: &RenderOptions) -> Result<String, String> {
    let selected = options
        .tracks
        .selected_indices(program.tracks.len())
        .map_err(|error| error.to_string())?;
    let noun = if program.tracks.len() == 1 {
        "track"
    } else {
        "tracks"
    };
    let mut text = format!("{} {noun}\n", program.tracks.len());
    for (index, track) in program.tracks.iter().enumerate() {
        let graph = program
            .voice(track.voice)
            .ok_or_else(|| format!("track {index} refers to a missing voice"))?;
        let marker = if selected.contains(&index) { '*' } else { '-' };
        let event_sends = track.routing.sends().len();
        let graph_sends = graph.sends.len();
        let trigger = if track.external_trigger.is_some() {
            ", external trigger"
        } else {
            ""
        };
        text.push_str(&format!(
            "{marker} track {index}: voice {}, {} ch, {event_sends} event sends, \
             {graph_sends} graph sends{trigger}\n",
            track.voice.index(),
            graph.channels(),
        ));
    }
    Ok(text)
}

fn parse(arguments: &[String]) -> Result<Option<Invocation>, String> {
    let mut input: Option<Input> = None;
    let mut output: Option<PathBuf> = None;
    let mut options = RenderOptions::default();
    let mut span: Option<RenderSpan> = None;
    let mut list_tracks = false;
    let mut check_syntax = false;
    let mut track_summary = false;
    let mut spectrum = false;
    let mut third_octaves = false;
    let mut dynamics = false;
    let mut fingerprints = false;
    let mut reference = None;
    let mut stability_seconds = None;
    let mut json = false;
    let mut force = false;
    let mut quiet = false;

    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let mut value = |name: &str| -> Result<String, String> {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argument.as_str() {
            "-h" | "--help" => return Ok(None),
            "-f" | "--force" => force = true,
            "-q" | "--quiet" => quiet = true,
            "--list-tracks" => list_tracks = true,
            "--check-syntax" => check_syntax = true,
            "--list-songs" => return Err("--list-songs must be used on its own".into()),
            "--song" => {
                let name = value("--song")?;
                let song = apteronotus_songs::song(&name)
                    .ok_or_else(|| format!("unknown embedded song {name:?}; use --list-songs"))?;
                if input.replace(Input::Song(song)).is_some() {
                    return Err("only one song can be rendered at a time".into());
                }
            }
            "--track-summary" => track_summary = true,
            "--spectrum" => spectrum = true,
            "--third-octaves" => third_octaves = true,
            "--dynamics" => dynamics = true,
            "--fingerprints" => fingerprints = true,
            "--reference" => reference = Some(PathBuf::from(value("--reference")?)),
            "--stability" => stability_seconds = Some(number(&value("--stability")?)?),
            "--json" => json = true,
            "--stems" => options.stems = true,
            "--raw-stems" => options.raw_stems = true,
            "--pcm16" => options.format = SampleFormat::Pcm16,
            "--solo-track" => options
                .tracks
                .include_only(track_indices(&value("--solo-track")?, "--solo-track")?),
            "--mute-track" => options
                .tracks
                .exclude(track_indices(&value("--mute-track")?, "--mute-track")?),
            "-o" | "--output" => output = Some(PathBuf::from(value("--output")?)),
            "--seconds" => span = Some(seconds_span(&value("--seconds")?)?),
            "--cycles" => span = Some(cycles_span(&value("--cycles")?)?),
            "--tail" => options.tail_seconds = number(&value("--tail")?)?,
            "--sample-rate" => options.sample_rate = number(&value("--sample-rate")?)?,
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option {other}"));
            }
            path => {
                if input.replace(Input::File(PathBuf::from(path))).is_some() {
                    return Err("only one song can be rendered at a time".into());
                }
            }
        }
    }

    let input = input.ok_or("no song given")?;
    if check_syntax && (list_tracks || json || reference.is_some()) {
        return Err(
            "--check-syntax cannot be combined with --list-tracks, --json or --reference".into(),
        );
    }
    if json && list_tracks {
        return Err("--json and --list-tracks are separate output modes".into());
    }
    if json && quiet {
        return Err("--json cannot be combined with --quiet".into());
    }
    if reference.is_some() && list_tracks {
        return Err("--reference cannot be combined with --list-tracks".into());
    }
    if reference.is_some() && quiet {
        return Err("--reference cannot be combined with --quiet".into());
    }
    if options.stems && options.raw_stems {
        return Err("--stems and --raw-stems are distinct output modes".into());
    }
    if stability_seconds.is_some_and(|seconds| !(seconds.is_finite() && seconds > 0.0)) {
        return Err("--stability needs a positive finite window in seconds".into());
    }
    if let Some(span) = span {
        options.span = span;
    }
    let output = output.unwrap_or_else(|| input.default_output());
    Ok(Some(Invocation {
        input,
        output,
        options,
        list_tracks,
        check_syntax,
        track_summary,
        spectrum,
        third_octaves,
        dynamics,
        fingerprints,
        reference,
        stability_seconds,
        json,
        force,
        quiet,
    }))
}

fn track_indices(text: &str, option: &str) -> Result<Vec<usize>, String> {
    text.split(',')
        .map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return Err(format!("{option} contains an empty track index"));
            }
            part.parse::<usize>()
                .map_err(|_| format!("{part:?} is not a zero-based track index"))
        })
        .collect()
}

/// The song's own name with a `.wav` extension, in the song's own directory.
fn default_output(input: &Path) -> PathBuf {
    let mut output = input.to_path_buf();
    output.set_extension("wav");
    output
}

fn number(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .map_err(|_| format!("{text:?} is not a number"))
}

fn seconds_span(text: &str) -> Result<RenderSpan, String> {
    match text.split_once("..") {
        Some((begin, end)) => Ok(RenderSpan::SecondsRange {
            begin: number(begin)?,
            end: number(end)?,
        }),
        None => Ok(RenderSpan::Seconds(number(text)?)),
    }
}

fn cycles_span(text: &str) -> Result<RenderSpan, String> {
    match text.split_once("..") {
        Some((begin, end)) => Ok(RenderSpan::CyclesRange {
            begin: cycles(begin)?,
            end: cycles(end)?,
        }),
        None => Ok(RenderSpan::Cycles(cycles(text)?)),
    }
}

/// `16` or `3/2`. Cycle counts are rational for the same reason every other
/// time in the system is: a third of a cycle has to stay a third.
fn cycles(text: &str) -> Result<Frac, String> {
    let integer = |part: &str| {
        part.trim()
            .parse::<i64>()
            .map_err(|_| format!("{text:?} is not a cycle count"))
    };
    match text.split_once('/') {
        Some((numerator, denominator)) => {
            let denominator = integer(denominator)?;
            if denominator == 0 {
                return Err(format!("{text:?} has a zero denominator"));
            }
            Frac::checked_new(integer(numerator)?, denominator)
                .ok_or_else(|| format!("{text:?} exceeds the cycle-count representation"))
        }
        None => Ok(Frac::new(integer(text)?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_output_defaults_to_the_song_beside_itself() {
        let invocation = parse(&arguments("songs/techno.eod"))
            .unwrap()
            .expect("a song is not a help request");
        assert_eq!(invocation.output, PathBuf::from("songs/techno.wav"));
        assert!(!invocation.force);
    }

    #[test]
    fn a_later_duration_flag_replaces_an_earlier_one() {
        let invocation = parse(&arguments("s.eod --seconds 4 --cycles 8"))
            .unwrap()
            .unwrap();
        assert_eq!(invocation.options.span, RenderSpan::Cycles(Frac::new(8, 1)));
    }

    #[test]
    fn cycles_stay_rational() {
        assert_eq!(cycles("3/2").unwrap(), Frac::new(3, 2));
        assert_eq!(cycles("16").unwrap(), Frac::new(16, 1));
        assert!(cycles("3/0").is_err());
        assert!(cycles("half").is_err());
        assert!(cycles("-9223372036854775808/-1").is_err());
        assert!(cycles("1/-9223372036854775808").is_err());
        assert_eq!(
            cycles("-9223372036854775808/-9223372036854775808").unwrap(),
            Frac::ONE
        );
    }

    #[test]
    fn embedded_songs_resolve_without_a_source_file() {
        let invocation = parse(&arguments("--song nightshift-dub --cycles 32"))
            .unwrap()
            .unwrap();
        assert_eq!(
            invocation.input.source().unwrap(),
            apteronotus_songs::NIGHTSHIFT_DUB
        );
        assert_eq!(invocation.output, PathBuf::from("nightshift-dub.wav"));
        assert_eq!(invocation.input.label(), "embedded:nightshift-dub");
        let explicit = parse(&arguments("--song nightshift-dub -o dub.wav"))
            .unwrap()
            .unwrap();
        assert_eq!(explicit.output, PathBuf::from("dub.wav"));
        for text in [
            "--song unknown",
            "--song",
            "s.eod --song drift",
            "--song drift s.eod",
            "--song drift --song waves",
            "--list-songs --song drift",
        ] {
            assert!(parse(&arguments(text)).is_err(), "{text}");
        }
    }

    #[test]
    fn render_windows_keep_exact_cycle_coordinates() {
        assert_eq!(
            cycles_span("3/2..11/4").unwrap(),
            RenderSpan::CyclesRange {
                begin: Frac::new(3, 2),
                end: Frac::new(11, 4),
            }
        );
        assert_eq!(
            seconds_span("1.25..3.5").unwrap(),
            RenderSpan::SecondsRange {
                begin: 1.25,
                end: 3.5,
            }
        );
    }

    #[test]
    fn options_are_carried_through() {
        let invocation = parse(&arguments(
            "s.eod -o out.wav --tail 0.5 --sample-rate 44100 --stems --pcm16 \
             --solo-track 3,1 --solo-track 2 --mute-track 1 -f -q",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(invocation.output, PathBuf::from("out.wav"));
        assert_eq!(invocation.options.tail_seconds, 0.5);
        assert_eq!(invocation.options.sample_rate, 44_100.0);
        assert!(invocation.options.stems);
        assert_eq!(invocation.options.format, SampleFormat::Pcm16);
        assert_eq!(
            invocation.options.tracks.selected_indices(5).unwrap(),
            vec![2, 3]
        );
        assert!(invocation.force);
        assert!(invocation.quiet);
    }

    #[test]
    fn raw_and_processed_stems_are_distinct_modes() {
        let raw = parse(&arguments("s.eod --raw-stems")).unwrap().unwrap();
        assert!(raw.options.raw_stems);
        assert!(!raw.options.stems);
        assert!(parse(&arguments("s.eod --stems --raw-stems")).is_err());
    }

    #[test]
    fn track_lists_are_strict_zero_based_comma_separated_indices() {
        assert_eq!(
            track_indices("0, 2,11", "--solo-track").unwrap(),
            [0, 2, 11]
        );
        assert!(track_indices("", "--solo-track").is_err());
        assert!(track_indices("1,", "--mute-track").is_err());
        assert!(track_indices("kick", "--solo-track").is_err());
    }

    #[test]
    fn listing_describes_owned_tracks_and_marks_the_effective_selection() {
        let program = evaluate(
            r#"
local mono = voice { graph = function() return sine(220) end }
local stereo = voice { graph = function() return sine(330) >> pan(0) end }
play(mono, "c4")
play(stereo, "e4")
"#,
        )
        .unwrap();
        let mut options = RenderOptions::default();
        options.tracks.include_only([1]);
        let listed = describe_tracks(&program, &options).unwrap();

        assert!(listed.starts_with("2 tracks\n"));
        assert!(listed.contains("- track 0: voice 0, 1 ch"));
        assert!(listed.contains("* track 1: voice 1, 2 ch"));
    }

    #[test]
    fn stem_manifest_reports_activity_and_omits_fake_levels_for_silence() {
        let program = evaluate(apteronotus_songs::SYNTHWAVE).unwrap();
        let channels = program.buses.total_channels();
        let mut samples = vec![0.0; channels * 2];
        samples[0] = 0.5;
        let rendered = Rendered {
            sample_rate: 48_000.0,
            channels,
            samples,
            format: SampleFormat::Float32,
            voices: 0,
            tracks: Vec::new(),
            scheduled_seconds: 0.0,
            start_seconds: 0.0,
            peak: 0.5,
            clipped: 0,
        };
        let manifest = describe_stems(&program, &rendered, false).unwrap();

        assert!(manifest.starts_with("  post-processor stem lanes:\n"));
        assert!(manifest.contains("lane 0  main.L"));
        assert!(manifest.contains("rms"));
        assert!(manifest.contains("lane 1  main.R     silent"));
        assert!(manifest.contains("bus0.L"));
    }

    #[test]
    fn track_summary_measures_each_selected_track_through_the_renderer() {
        let program = evaluate(
            r#"
tempo(120)
local low = voice { graph = function() return sine(220) * decay(ms(20)) * 0.1 end }
local high = voice { graph = function() return sine(440) * decay(ms(20)) * 0.2 end }
play(low, "x")
play(high, "x")
"#,
        )
        .unwrap();
        let options = RenderOptions {
            span: RenderSpan::Cycles(Frac::ONE),
            tail_seconds: 0.0,
            ..RenderOptions::default()
        };
        let analysis = analyze_tracks(&program, &options, false).unwrap();
        let summary = describe_track_summary(&analysis);

        assert!(summary.contains("track 0  voices 1"));
        assert!(summary.contains("track 1  voices 1"));
        assert_eq!(summary.matches("rms").count(), 2);
    }

    #[test]
    fn fingerprint_report_uses_exact_track_onsets_and_isolated_audio() {
        let program = evaluate(
            r#"
tempo(120)
local hit = voice { graph = function() return sine(440) * decay(ms(20)) end }
play(hit, "x x x x")
"#,
        )
        .unwrap();
        let options = RenderOptions {
            span: RenderSpan::Cycles(Frac::ONE),
            tail_seconds: 0.0,
            ..RenderOptions::default()
        };
        let analysis = analyze_tracks(&program, &options, true).unwrap();
        let fingerprints = analysis[0].fingerprints.as_ref().unwrap();
        let report = describe_fingerprints(&analysis);

        assert_eq!(analysis[0].timing.onsets_seconds.len(), 4);
        assert_eq!(fingerprints.correlations.len(), 3);
        assert!(fingerprints.median_correlation > 0.999);
        assert!(report.contains("track 0  comparisons 3"));
    }

    #[test]
    fn a_missing_value_is_an_error_rather_than_a_silent_default() {
        assert!(parse(&arguments("s.eod --seconds")).is_err());
        assert!(parse(&arguments("s.eod --solo-track")).is_err());
        assert!(parse(&arguments("s.eod --unknown")).is_err());
        assert!(parse(&arguments("one.eod two.eod")).is_err());
        assert!(parse(&arguments("")).is_err());
    }

    #[test]
    fn help_is_not_an_invocation() {
        assert!(parse(&arguments("--help")).unwrap().is_none());
    }

    #[test]
    fn list_tracks_is_an_evaluation_only_invocation() {
        let invocation = parse(&arguments("s.eod --list-tracks")).unwrap().unwrap();
        assert!(invocation.list_tracks);
    }

    #[test]
    fn track_summary_is_opt_in() {
        let ordinary = parse(&arguments("s.eod")).unwrap().unwrap();
        let summary = parse(&arguments("s.eod --track-summary")).unwrap().unwrap();
        assert!(!ordinary.track_summary);
        assert!(summary.track_summary);
    }

    #[test]
    fn spectrum_is_opt_in() {
        let ordinary = parse(&arguments("s.eod")).unwrap().unwrap();
        let spectrum = parse(&arguments("s.eod --spectrum")).unwrap().unwrap();
        assert!(!ordinary.spectrum);
        assert!(spectrum.spectrum);
    }

    #[test]
    fn dynamics_is_opt_in() {
        let ordinary = parse(&arguments("s.eod")).unwrap().unwrap();
        let dynamics = parse(&arguments("s.eod --dynamics")).unwrap().unwrap();
        assert!(!ordinary.dynamics);
        assert!(dynamics.dynamics);
    }

    #[test]
    fn onset_fingerprints_are_opt_in() {
        let ordinary = parse(&arguments("s.eod")).unwrap().unwrap();
        let fingerprints = parse(&arguments("s.eod --fingerprints")).unwrap().unwrap();
        assert!(!ordinary.fingerprints);
        assert!(fingerprints.fingerprints);
    }

    #[test]
    fn third_octaves_are_opt_in() {
        let ordinary = parse(&arguments("s.eod")).unwrap().unwrap();
        let thirds = parse(&arguments("s.eod --third-octaves")).unwrap().unwrap();
        assert!(!ordinary.third_octaves);
        assert!(thirds.third_octaves);
    }

    #[test]
    fn stability_has_an_explicit_positive_window() {
        let invocation = parse(&arguments("s.eod --stability 5")).unwrap().unwrap();
        assert_eq!(invocation.stability_seconds, Some(5.0));
        assert!(parse(&arguments("s.eod --stability 0")).is_err());
        assert!(parse(&arguments("s.eod --stability nope")).is_err());
    }

    #[test]
    fn json_is_a_complete_explicit_output_mode() {
        let invocation = parse(&arguments("s.eod --json")).unwrap().unwrap();
        assert!(invocation.json);
        assert!(parse(&arguments("s.eod --json --quiet")).is_err());
        assert!(parse(&arguments("s.eod --json --list-tracks")).is_err());
    }

    #[test]
    fn json_analysis_is_versioned_and_uses_null_for_undefined_levels() {
        let program = evaluate(
            r#"
tempo(120)
local sound = voice { graph = function() return sine(1024) * 0.1 end }
play(sound, "x")
"#,
        )
        .unwrap();
        let options = RenderOptions {
            span: RenderSpan::Cycles(Frac::ONE),
            tail_seconds: 0.0,
            ..RenderOptions::default()
        };
        let rendered = render(&program, &options).unwrap();
        let tracks = analyze_tracks(&program, &options, true).unwrap();
        let encoded = analysis_json(&program, &rendered, &options, &tracks, None).unwrap();
        let document: Value = serde_json::from_str(&encoded).unwrap();

        assert_eq!(document["schema"], "apteronotus.render-analysis.v1");
        assert_eq!(document["window"]["requested"]["unit"], "cycles");
        assert_eq!(document["window"]["requested"]["numerator"], 1);
        assert_eq!(document["output"]["voices"], 1);
        assert_eq!(document["tracks"][0]["index"], 0);
        assert_eq!(document["tracks"][0]["distinct_onsets"], 1);
        assert_eq!(document["tracks"][0]["onsets_ms"][0], 0.0);
        assert!(document["tracks"][0]["min_interval_ms"].is_null());
        assert!(document["tracks"][0]["onset_fingerprints"].is_null());
        assert!(document["tracks"][0]["rms_dbfs"].is_number());
        assert_eq!(document["lanes"][0]["label"], "main");
        assert!(document["spectrum"]["bands"].as_array().unwrap().len() == 4);
        assert_eq!(
            document["spectrum"]["third_octaves"]
                .as_array()
                .unwrap()
                .len(),
            30
        );
        assert!(document["spectrum"]["bands"][3]["high_hz"].is_null());
    }

    #[test]
    fn reference_mode_is_explicit_and_not_silenced() {
        let invocation = parse(&arguments("s.eod --reference ref.wav"))
            .unwrap()
            .unwrap();
        assert_eq!(invocation.reference, Some(PathBuf::from("ref.wav")));
        assert!(parse(&arguments("s.eod --reference")).is_err());
        assert!(parse(&arguments("s.eod --reference ref.wav --quiet")).is_err());
        assert!(parse(&arguments("s.eod --reference ref.wav --list-tracks")).is_err());
    }

    #[test]
    fn integer_reference_pcm_is_normalized_by_its_declared_bit_depth() {
        assert_eq!(integer_pcm_scale(8), 128.0);
        assert_eq!(integer_pcm_scale(16), 32_768.0);
        assert_eq!(integer_pcm_scale(24), 8_388_608.0);
        assert_eq!(integer_pcm_scale(32), 2_147_483_648.0);

        let negative_full_scale = -8_388_608_f32 / integer_pcm_scale(24);
        let positive_full_scale = 8_388_607_f32 / integer_pcm_scale(24);
        assert_eq!(negative_full_scale, -1.0);
        assert!(positive_full_scale <= 1.0);
        assert!(positive_full_scale > 0.999_999);
    }

    #[test]
    fn a_24_bit_pcm_reference_is_decoded_at_full_scale() {
        let path = std::env::temp_dir().join(format!(
            "apteronotus-render-reference-{}-24bit.wav",
            std::process::id()
        ));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.write_sample(-8_388_608_i32).unwrap();
        writer.write_sample(0_i32).unwrap();
        writer.write_sample(8_388_607_i32).unwrap();
        writer.finalize().unwrap();

        let decoded = read_reference(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(decoded.channels, 1);
        assert_eq!(decoded.samples[0], -1.0);
        assert_eq!(decoded.samples[1], 0.0);
        assert!(decoded.samples[2] > 0.999_999);
        assert_eq!(decoded.peak, 1.0);
        assert_eq!(decoded.clipped, 0);
    }

    #[test]
    fn an_identical_reference_has_zero_tonal_and_dynamic_deltas() {
        let program = evaluate(
            r#"
tempo(120)
local sound = voice { graph = function() return sine(1000) >> pan(0.25) end }
play(sound, "x")
"#,
        )
        .unwrap();
        let options = RenderOptions {
            span: RenderSpan::Seconds(0.8),
            tail_seconds: 0.0,
            ..RenderOptions::default()
        };
        let rendered = render(&program, &options).unwrap();
        let comparison = reference_json(Path::new("same.wav"), &rendered, &rendered);

        for band in comparison["third_octaves"].as_array().unwrap() {
            if let Some(delta) = band["delta_db"].as_f64() {
                assert!(delta.abs() < 1.0e-12);
            }
        }
        for region in comparison["regions"].as_array().unwrap() {
            assert!(region["mean_delta_db"].as_f64().unwrap().abs() < 1.0e-12);
        }
        assert!(
            comparison["dynamics"]["side_mid_delta_db"]
                .as_f64()
                .unwrap()
                .abs()
                < 1.0e-12
        );
        assert!(
            comparison["dynamics"]["envelope_spread_delta_db"]
                .as_f64()
                .unwrap()
                .abs()
                < 1.0e-12
        );
    }
}
