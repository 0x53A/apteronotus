//! Organ acceptance through the same scheduler and room processor as playback.
use apteronotus_lua::evaluate;
use apteronotus_render::{RenderOptions, RenderSpan, render};
use apteronotus_songs::CATHEDRAL_ORGAN;

fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}

#[test]
fn organ_study_has_dry_stops_keyed_polyphony_and_a_shared_room_tail() {
    let program = evaluate(CATHEDRAL_ORGAN).unwrap();
    let options = RenderOptions {
        span: RenderSpan::Seconds(48.0),
        tail_seconds: 6.0,
        sample_rate: 24_000.0,
        ..Default::default()
    };
    let audio = render(&program, &options).unwrap();
    assert_eq!(audio.channels, 2);
    assert_eq!(audio.clipped, 0);
    assert!(audio.samples.iter().all(|x| x.is_finite()));
    let window = |start: f64, end: f64| {
        &audio.samples[(start * 48_000.0) as usize..(end * 48_000.0) as usize]
    };
    for start in [0.0, 4.0, 8.0, 12.0] {
        assert!(rms(window(start + 1.0, start + 2.0)) > 0.01);
        assert!(
            rms(window(start + 2.8, start + 2.95)) < 1e-6,
            "dry stop should close"
        );
    }
    assert!(rms(window(18.0, 19.0)) > 0.015, "held registration");
    assert!(rms(window(30.0, 31.0)) > 0.015, "rhythmic keys");
    assert!(
        rms(window(48.1, 48.3)) > 1e-5,
        "room outlives released pipes"
    );
    assert!(rms(window(53.0, 54.0)) < rms(window(48.1, 48.3)) * 0.05);
    assert_eq!(audio.samples, render(&program, &options).unwrap().samples);
}

#[test]
fn dry_comparison_renders_both_engines_and_a_single_retained_pipe() {
    use apteronotus_songs::ORGAN_LABORATORY;
    use apteronotus_synth::Op;
    let program = evaluate(ORGAN_LABORATORY).unwrap();
    assert_eq!(
        program
            .patches
            .iter()
            .flat_map(|p| &p.graph().nodes)
            .filter(|node| matches!(node.op, Op::FluePipe { .. }))
            .count(),
        1
    );
    let options = RenderOptions {
        span: RenderSpan::Seconds(30.0),
        tail_seconds: 1.0,
        sample_rate: 24_000.0,
        ..Default::default()
    };
    let audio = render(&program, &options).unwrap();
    assert_eq!(audio.voices, 9);
    assert_eq!(audio.clipped, 0);
    let window = |a: f64, b: f64| &audio.samples[(a * 48_000.0) as usize..(b * 48_000.0) as usize];
    for start in [0.0, 6.0, 12.0] {
        for note in [0.0, 2.0, 4.0] {
            assert!(rms(window(start + note + 0.7, start + note + 1.4)) > 0.03);
        }
    }
    let additive = rms(window(2.7, 3.4));
    let physical = rms(window(14.7, 15.4));
    assert!(
        (physical / additive).log10().abs() * 20.0 < 3.0,
        "A3 comparison should be roughly level matched"
    );
    assert!(rms(window(20.8, 21.8)) > rms(window(18.8, 19.8)) * 1.2);
    assert!(rms(window(23.0, 23.5)) > 0.03, "speaks after short closure");
    assert!(
        rms(window(24.8, 24.99)) < 1e-6,
        "wind closure dissipates pipe state"
    );
    assert!(rms(window(25.8, 26.8)) > 0.03, "speaks after long closure");
    assert!(rms(window(29.0, 31.0)) < 1e-8);
    assert_eq!(audio.samples, render(&program, &options).unwrap().samples);
}
