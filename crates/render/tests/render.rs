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
use apteronotus_render::{RenderOptions, RenderSpan, Rendered, SampleFormat, render, write_wav};
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
        scheduled_seconds: 0.0,
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
