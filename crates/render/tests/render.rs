//! What an offline render has to be true of before anything is measured
//! against it.
//!
//! These tests are the reason the renderer is worth having: they check that
//! the file agrees with the score about *when* and *how long*, and that two
//! runs agree with each other. An analysis pass comparing a render to a
//! reference recording inherits every one of those properties, and inherits
//! the bugs just as directly if they are not pinned here.

use apteronotus_lua::evaluate;
use apteronotus_pattern::Frac;
use apteronotus_render::{
    RenderOptions, RenderSpan, Rendered, SampleFormat, TrackSelection, render, stem_lane_labels,
    write_wav,
};
use apteronotus_songs::SONGS;

/// One short click on the first of four steps, so onsets are exactly one cycle
/// apart and each is over long before the next.
const CLICKS: &str = r#"
tempo(120)
local click = voice {
  graph = function()
    return sine(880) * decay(ms(20)) * 0.5
  end,
}
play(click, "x ~ ~ ~")
"#;

fn options(seconds: f64) -> RenderOptions {
    RenderOptions {
        span: RenderSpan::Seconds(seconds),
        ..RenderOptions::default()
    }
}

/// Start times of audible runs, in seconds, from a 10 ms RMS envelope.
fn onsets(rendered: &Rendered) -> Vec<f64> {
    let hop = (rendered.sample_rate * 0.010) as usize;
    let channels = rendered.channels;
    let mut onsets = Vec::new();
    let mut sounding = false;
    for (index, window) in rendered.samples.chunks(hop * channels).enumerate() {
        let energy = window.iter().map(|sample| sample * sample).sum::<f32>();
        let rms = (energy / window.len() as f32).sqrt();
        // Well below a click's peak and well above the numerical floor between
        // them, so the threshold is not itself the measurement.
        let loud = rms > 0.01;
        if loud && !sounding {
            onsets.push(index as f64 * hop as f64 / rendered.sample_rate);
        }
        sounding = loud;
    }
    onsets
}

#[test]
fn onsets_land_where_the_tempo_map_says() {
    let program = evaluate(CLICKS).unwrap();
    let cycle = program.tempo.cycle_to_seconds(Frac::ONE);
    let rendered = render(&program, &options(4.0 * cycle)).unwrap();

    let onsets = onsets(&rendered);
    assert_eq!(
        onsets.len(),
        4,
        "expected one click per cycle across four cycles, found {onsets:?}"
    );
    for (index, onset) in onsets.iter().enumerate() {
        let expected = index as f64 * cycle;
        assert!(
            (onset - expected).abs() < 0.02,
            "click {index} landed at {onset:.3} s, not {expected:.3} s"
        );
    }
}

#[test]
fn the_tail_is_rendered_but_nothing_new_is_scheduled_into_it() {
    let program = evaluate(CLICKS).unwrap();
    let cycle = program.tempo.cycle_to_seconds(Frac::ONE);
    // Two cycles of music, then two more cycles of tail: long enough that a
    // scheduled third and fourth click would be unmissable.
    let rendered = render(
        &program,
        &RenderOptions {
            span: RenderSpan::Seconds(2.0 * cycle),
            tail_seconds: 2.0 * cycle,
            ..RenderOptions::default()
        },
    )
    .unwrap();

    assert!((rendered.duration_seconds() - 4.0 * cycle).abs() < 0.01);
    assert!((rendered.scheduled_seconds - 2.0 * cycle).abs() < 1e-9);
    assert_eq!(
        onsets(&rendered).len(),
        2,
        "the tail must let the last note decay, not keep the pattern running"
    );
}

#[test]
fn a_nonzero_cycle_window_prerolls_and_then_discards_the_prefix() {
    let program = evaluate(CLICKS).unwrap();
    let end = Frac::new(3, 200);
    let begin = Frac::new(1, 200);
    let full = render(
        &program,
        &RenderOptions {
            span: RenderSpan::Cycles(end),
            tail_seconds: 0.0,
            ..RenderOptions::default()
        },
    )
    .unwrap();
    let window = render(
        &program,
        &RenderOptions {
            span: RenderSpan::CyclesRange { begin, end },
            tail_seconds: 0.0,
            ..RenderOptions::default()
        },
    )
    .unwrap();
    let first_frame = (window.start_seconds * window.sample_rate).ceil() as usize;

    assert_eq!(window.voices, 0, "the only onset belongs to the prefix");
    assert!(
        window.peak > 0.0,
        "the prefix voice must ring into the window"
    );
    assert_eq!(
        window.samples,
        full.samples[first_frame * full.channels..],
        "window rendering must be the exact suffix of a render from transport zero"
    );
}

/// Nothing in the chain draws from a generator whose state depends on when a
/// block was computed, so this is exact rather than approximate.
#[test]
fn two_renders_of_one_program_are_identical_sample_for_sample() {
    let program = evaluate(SONGS[0].source).unwrap();
    let first = render(&program, &options(4.0)).unwrap();
    let second = render(&program, &options(4.0)).unwrap();
    assert_eq!(first.samples, second.samples);
    assert_eq!(first.voices, second.voices);
}

#[test]
fn every_specification_song_renders_to_finite_audible_stereo() {
    let mut failures = Vec::new();
    for song in SONGS {
        let program = match evaluate(song.source) {
            Ok(program) => program,
            Err(error) => {
                failures.push(format!("{}: evaluation: {error}", song.name));
                continue;
            }
        };
        let rendered = match render(&program, &options(4.0)) {
            Ok(rendered) => rendered,
            Err(error) => {
                failures.push(format!("{}: {error}", song.name));
                continue;
            }
        };
        if rendered.channels != 2 {
            failures.push(format!("{}: {} channels", song.name, rendered.channels));
        }
        if rendered.peak == 0.0 {
            failures.push(format!("{}: silent", song.name));
        }
        if rendered.clipped > 0 {
            failures.push(format!(
                "{}: {} samples past full scale or not finite",
                song.name, rendered.clipped
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the corpus no longer renders cleanly:\n  {}",
        failures.join("\n  ")
    );
}

/// The per-bus signal an automated comparison actually wants. `synthwave` is
/// the corpus's sends-and-sidechain song, so its stem layout is wider than its
/// main output.
#[test]
fn stems_expose_the_whole_routed_layout() {
    let program = evaluate(apteronotus_songs::SYNTHWAVE).unwrap();
    let main = render(&program, &options(2.0)).unwrap();
    let stems = render(
        &program,
        &RenderOptions {
            stems: true,
            ..options(2.0)
        },
    )
    .unwrap();

    assert_eq!(main.channels, 2);
    assert!(
        stems.channels > main.channels,
        "a song with sends has buses beyond its main output"
    );
    assert_eq!(stems.frames(), main.frames());
}

#[test]
fn raw_stems_expose_bus_inputs_before_returns_clear_them() {
    let program = evaluate(apteronotus_songs::SYNTHWAVE).unwrap();
    let processed = render(
        &program,
        &RenderOptions {
            stems: true,
            ..options(2.0)
        },
    )
    .unwrap();
    let raw = render(
        &program,
        &RenderOptions {
            raw_stems: true,
            ..options(2.0)
        },
    )
    .unwrap();

    assert_eq!(raw.channels, program.buses.total_channels());
    assert_eq!(raw.frames(), processed.frames());
    assert!(
        raw.channel_metrics()[2..]
            .iter()
            .any(|metrics| metrics.rms > 0.0),
        "a raw send lane must retain signal before its return consumes it"
    );
    assert!(
        processed.channel_metrics()[2..]
            .iter()
            .all(|metrics| metrics.rms == 0.0),
        "post-processor bus lanes are cleared after their returns"
    );
}

#[test]
fn track_selection_is_post_evaluation_and_keeps_original_program_order() {
    let program = evaluate(
        r#"
tempo(120)
local click = voice {
  graph = function(n)
    return noise() * decay(ms(20)) * n.velocity * 0.1
  end,
}
play(click, "x ~" >> velocity(0.7))
play(click, "~ x" >> velocity(0.4))
"#,
    )
    .unwrap();

    let render_with = |tracks: TrackSelection| {
        render(
            &program,
            &RenderOptions {
                span: RenderSpan::Cycles(Frac::ONE),
                tail_seconds: 0.0,
                tracks,
                ..RenderOptions::default()
            },
        )
        .unwrap()
    };

    let full = render_with(TrackSelection::all());
    let mut first_only = TrackSelection::all();
    first_only.include_only([0]);
    let first = render_with(first_only);
    let mut second_only = TrackSelection::all();
    second_only.include_only([1]);
    let second = render_with(second_only);

    assert_eq!(full.voices, 2);
    assert_eq!(full.tracks.len(), 2);
    assert_eq!(full.tracks[0].index, 0);
    assert_eq!(full.tracks[1].index, 1);
    assert_eq!(first.voices, 1);
    assert_eq!(first.tracks[0].voices, 1);
    assert_eq!(second.voices, 1);
    assert_eq!(full.samples.len(), first.samples.len());
    for ((mixed, first), second) in full.samples.iter().zip(&first.samples).zip(&second.samples) {
        assert!(
            (mixed - (first + second)).abs() < 1.0e-6,
            "the isolated tracks no longer sum to the unchanged full render"
        );
    }
}

#[test]
fn muting_every_track_keeps_the_output_shape_and_schedules_no_voices() {
    let program = evaluate(CLICKS).unwrap();
    let mut tracks = TrackSelection::all();
    tracks.exclude([0]);
    let rendered = render(
        &program,
        &RenderOptions {
            tracks,
            tail_seconds: 0.0,
            ..options(1.0)
        },
    )
    .unwrap();

    assert_eq!(rendered.channels, 1);
    assert_eq!(rendered.voices, 0);
    assert_eq!(rendered.peak, 0.0);
}

#[test]
fn an_isolated_track_keeps_the_persistent_routed_layout() {
    let program = evaluate(apteronotus_songs::SYNTHWAVE).unwrap();
    let full = render(
        &program,
        &RenderOptions {
            stems: true,
            ..options(2.0)
        },
    )
    .unwrap();
    let mut tracks = TrackSelection::all();
    tracks.include_only([0]);
    let isolated = render(
        &program,
        &RenderOptions {
            stems: true,
            tracks,
            ..options(2.0)
        },
    )
    .unwrap();

    assert_eq!(isolated.channels, full.channels);
    assert_eq!(isolated.frames(), full.frames());
    assert!(isolated.voices > 0);
    assert!(isolated.voices < full.voices);
}

#[test]
fn an_unknown_selected_track_is_a_diagnostic() {
    let program = evaluate(CLICKS).unwrap();
    let mut tracks = TrackSelection::all();
    tracks.include_only([1]);
    let error = render(
        &program,
        &RenderOptions {
            tracks,
            ..options(1.0)
        },
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "track 1 does not exist; program track count is 1"
    );
}

#[test]
fn channel_metrics_measure_interleaved_lanes_independently() {
    let rendered = Rendered {
        sample_rate: 48_000.0,
        channels: 2,
        samples: vec![1.0, -0.5, -1.0, 0.5, 0.0, 0.0],
        format: SampleFormat::Float32,
        voices: 0,
        tracks: Vec::new(),
        scheduled_seconds: 0.0,
        start_seconds: 0.0,
        peak: 1.0,
        clipped: 0,
    };
    let metrics = rendered.channel_metrics();

    assert_eq!(metrics.len(), 2);
    assert!(metrics[0].dc.abs() < 1.0e-12);
    assert!(metrics[1].dc.abs() < 1.0e-12);
    assert!((metrics[0].rms - (2.0f64 / 3.0).sqrt()).abs() < 1.0e-12);
    assert!((metrics[1].rms - (0.5f64 / 3.0).sqrt()).abs() < 1.0e-12);
    assert_eq!(metrics[0].peak, 1.0);
    assert_eq!(metrics[1].peak, 0.5);
    let whole = rendered.metrics();
    assert!(whole.dc.abs() < 1.0e-12);
    assert!((whole.rms - (2.5f64 / 6.0).sqrt()).abs() < 1.0e-12);
    assert_eq!(whole.peak, 1.0);
}

#[test]
fn onset_fingerprints_use_scheduler_clock_and_normalized_waveforms() {
    let mut samples = vec![0.0; 30];
    samples[0..2].copy_from_slice(&[1.0, -1.0]);
    samples[10..12].copy_from_slice(&[0.5, -0.5]);
    samples[20..22].copy_from_slice(&[-1.0, 1.0]);
    let rendered = Rendered {
        sample_rate: 10.0,
        channels: 1,
        samples,
        format: SampleFormat::Float32,
        voices: 3,
        tracks: Vec::new(),
        scheduled_seconds: 3.0,
        start_seconds: 5.0,
        peak: 1.0,
        clipped: 0,
    };

    let metrics = rendered
        .onset_fingerprint_metrics(&[5.0, 6.0, 7.0], 0.2)
        .unwrap();
    assert_eq!(metrics.window_seconds, 0.2);
    assert_eq!(metrics.correlations, [1.0, -1.0]);
    assert_eq!(metrics.median_correlation, 0.0);
    assert_eq!(metrics.maximum_correlation, 1.0);
    assert!(
        rendered
            .onset_fingerprint_metrics(&[4.0, 5.0], 0.2)
            .is_none()
    );
}

#[test]
fn spectral_centroid_finds_an_antiphase_stereo_tone_without_cancellation() {
    let sample_rate = 32_768.0;
    let frequency = 1_024.0;
    let mut samples = Vec::with_capacity(32_768 * 2);
    for frame in 0..32_768 {
        let sample = (std::f64::consts::TAU * frequency * frame as f64 / sample_rate).sin() as f32;
        samples.extend([sample, -sample]);
    }
    let rendered = Rendered {
        sample_rate,
        channels: 2,
        samples,
        format: SampleFormat::Float32,
        voices: 0,
        tracks: Vec::new(),
        scheduled_seconds: 1.0,
        start_seconds: 0.0,
        peak: 1.0,
        clipped: 0,
    };

    let spectral = rendered.spectral_metrics().unwrap();
    let centroid = spectral.centroid_hz;
    assert!(
        (centroid - frequency).abs() < 2.0,
        "expected {frequency} Hz, measured {centroid} Hz"
    );
    let bands = spectral.band_decibels();
    assert!((bands[1].unwrap() - -3.0103).abs() < 0.05);
    assert!(bands[0].is_none_or(|level| level < -100.0));
    assert!(bands[2].is_none_or(|level| level < -100.0));
    let thirds = spectral.third_octave_decibels();
    assert!((thirds[16].unwrap() - -3.0103).abs() < 0.05);
    assert!(thirds[15].is_none_or(|level| level < -100.0));
    assert!(thirds[17].is_none_or(|level| level < -100.0));
}

#[test]
fn stereo_width_and_envelope_spread_have_declared_coordinates() {
    let mut samples = Vec::new();
    // Twenty 20 ms windows: ten at -20 dBFS, ten at 0 dBFS. A signal present
    // only on the left has equal sum and difference energy, hence 0 dB S/M.
    for window in 0..20 {
        let level = if window < 10 { 0.1 } else { 1.0 };
        for _ in 0..20 {
            samples.extend([level, 0.0]);
        }
    }
    let rendered = Rendered {
        sample_rate: 1_000.0,
        channels: 2,
        samples,
        format: SampleFormat::Float32,
        voices: 0,
        tracks: Vec::new(),
        scheduled_seconds: 0.4,
        start_seconds: 0.0,
        peak: 1.0,
        clipped: 0,
    };

    let stereo = rendered.stereo_metrics().unwrap();
    assert!(stereo.side_mid_decibels().unwrap().abs() < 1.0e-12);
    let envelope = rendered.envelope_metrics_20ms().unwrap();
    assert!((envelope.p5_dbfs - -23.0102999566).abs() < 1.0e-6);
    assert!((envelope.p95_dbfs - -3.0102999566).abs() < 1.0e-6);
    assert!((envelope.spread_decibels - 20.0).abs() < 1.0e-6);
}

#[test]
fn level_stability_excludes_the_response_tail() {
    let mut samples = vec![0.1; 200];
    samples.extend(vec![1.0; 200]);
    // This explicit tail is louder still; it must not enter either window.
    samples.extend(vec![4.0; 100]);
    let rendered = Rendered {
        sample_rate: 1_000.0,
        channels: 1,
        samples,
        format: SampleFormat::Float32,
        voices: 0,
        tracks: Vec::new(),
        scheduled_seconds: 0.4,
        start_seconds: 0.0,
        peak: 4.0,
        clipped: 100,
    };

    let stability = rendered.level_stability(0.2).unwrap();
    assert_eq!(stability.windows, 2);
    assert!((stability.min_rms_dbfs - -20.0).abs() < 1.0e-6);
    assert!(stability.max_rms_dbfs.abs() < 1.0e-12);
    assert!((stability.spread_decibels - 20.0).abs() < 1.0e-6);
    assert!(stability.peak_max_dbfs.unwrap().abs() < 1.0e-12);
}

#[test]
fn stem_labels_follow_the_authoritative_flattened_layout() {
    let program = evaluate(apteronotus_songs::SYNTHWAVE).unwrap();
    let labels = stem_lane_labels(&program);

    assert_eq!(labels.len(), program.buses.total_channels());
    assert_eq!(&labels[..2], ["main.L", "main.R"]);
    assert_eq!(&labels[2..4], ["bus0.L", "bus0.R"]);
}

#[test]
fn a_program_that_reaches_no_audio_is_refused_rather_than_written() {
    let program = evaluate("tempo(120)").unwrap();
    let error = render(&program, &options(1.0)).unwrap_err();
    assert!(
        error.to_string().contains("no playable"),
        "unhelpful refusal: {error}"
    );
}

#[test]
fn an_unusable_duration_or_rate_is_refused() {
    let program = evaluate(CLICKS).unwrap();
    for span in [
        RenderSpan::Seconds(0.0),
        RenderSpan::Seconds(f64::NAN),
        RenderSpan::Cycles(Frac::ZERO),
    ] {
        assert!(
            render(
                &program,
                &RenderOptions {
                    span,
                    ..RenderOptions::default()
                }
            )
            .is_err(),
            "{span:?} should not render"
        );
    }
    assert!(
        render(
            &program,
            &RenderOptions {
                sample_rate: 0.0,
                ..options(1.0)
            }
        )
        .is_err()
    );
}

#[test]
fn a_cycle_span_is_projected_through_the_programs_own_tempo_map() {
    let program = evaluate(CLICKS).unwrap();
    let cycle = program.tempo.cycle_to_seconds(Frac::ONE);
    let rendered = render(
        &program,
        &RenderOptions {
            span: RenderSpan::Cycles(Frac::new(3, 1)),
            tail_seconds: 0.0,
            ..RenderOptions::default()
        },
    )
    .unwrap();
    assert!((rendered.duration_seconds() - 3.0 * cycle).abs() < 0.01);
}

#[test]
fn a_written_file_carries_the_render_it_was_given() {
    let program = evaluate(CLICKS).unwrap();
    let rendered = render(
        &program,
        &RenderOptions {
            tail_seconds: 0.0,
            ..options(1.0)
        },
    )
    .unwrap();

    let path = std::env::temp_dir().join(format!(
        "apteronotus-render-{}-{}.wav",
        std::process::id(),
        line!()
    ));
    write_wav(&path, &rendered).unwrap();

    let reader = hound::WavReader::open(&path).unwrap();
    let spec = reader.spec();
    assert_eq!(spec.channels as usize, rendered.channels);
    assert_eq!(spec.sample_rate, 48_000);
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(spec.bits_per_sample, 32);
    assert_eq!(reader.len() as usize, rendered.samples.len());

    let written: Vec<f32> = reader.into_samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(
        written, rendered.samples,
        "a float file must round-trip exactly; that is why it is the default"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn pcm16_clamps_rather_than_wrapping() {
    let rendered = Rendered {
        sample_rate: 48_000.0,
        channels: 1,
        samples: vec![0.0, 1.0, -1.0, 4.0, -4.0],
        format: SampleFormat::Pcm16,
        voices: 0,
        tracks: Vec::new(),
        scheduled_seconds: 0.0,
        start_seconds: 0.0,
        peak: 4.0,
        clipped: 2,
    };
    let path = std::env::temp_dir().join(format!(
        "apteronotus-render-{}-{}.wav",
        std::process::id(),
        line!()
    ));
    write_wav(&path, &rendered).unwrap();

    let written: Vec<i16> = hound::WavReader::open(&path)
        .unwrap()
        .into_samples::<i16>()
        .map(Result::unwrap)
        .collect();
    assert_eq!(written, vec![0, i16::MAX, -i16::MAX, i16::MAX, -i16::MAX]);

    let _ = std::fs::remove_file(&path);
}
