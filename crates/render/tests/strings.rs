//! Acceptance at the production scheduler/patch boundary, without a device.
use apteronotus_lua::evaluate;
use apteronotus_render::{RenderOptions, RenderSpan, render};
use apteronotus_songs::SIX_STRINGS;
use apteronotus_synth::{Op, PatchTemplate, Source};

fn options() -> RenderOptions {
    RenderOptions {
        span: RenderSpan::Seconds(32.0),
        tail_seconds: 0.0,
        sample_rate: 24_000.0,
        ..Default::default()
    }
}

#[test]
fn guitar_demo_rings_between_short_picks_and_hand_mute_removes_energy() {
    let program = evaluate(SIX_STRINGS).unwrap();
    assert_eq!(program.patches.len(), 1);
    assert_eq!(
        program.patches[0]
            .graph()
            .nodes
            .iter()
            .filter(|node| matches!(node.op, Op::StringResonator { .. }))
            .count(),
        6
    );
    let rendered = render(&program, &options()).unwrap();
    assert_eq!(rendered.voices, 30); // excitation bursts, not thirty retained strings
    assert_eq!(rendered.clipped, 0);
    let rms = |start: usize, end: usize| {
        let samples = &rendered.samples[start * 24_000 * 2..end * 24_000 * 2];
        (samples.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    };
    assert!(
        rms(10, 11) > 0.0005,
        "strings should remain audible between picks"
    );
    assert!(
        rms(19, 20) < 1e-6,
        "hand mute should remove stored vibration"
    );
    assert!(rms(21, 22) > 0.001, "picking after mute should work");
    assert!(rms(31, 32) < 1e-6);
    assert_eq!(
        rendered.samples,
        render(&program, &options()).unwrap().samples
    );
}

#[test]
fn upper_string_picks_do_not_disturb_lower_string_history() {
    let mut program = evaluate(SIX_STRINGS).unwrap();
    // Expose the actual retained strings before the shared nonlinear amp.
    // This leaves excitation tracks, control data, and provenance unchanged.
    let mut graph = program.patches[0].graph().clone();
    let strings: Vec<_> = graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(node, item)| {
            matches!(item.op, Op::StringResonator { .. })
                .then_some(Source::Port { node, channel: 0 })
        })
        .collect();
    for source in &strings[..3] {
        graph.outputs = vec![*source, *source];
        program.patches[0] = PatchTemplate::new(graph.clone()).unwrap();
        let mut opts = options();
        opts.span = RenderSpan::Seconds(12.0);
        let all = render(&program, &opts).unwrap();
        opts.tracks.exclude([3, 4, 5]);
        let lower_only = render(&program, &opts).unwrap();
        assert_eq!(all.samples, lower_only.samples);
        assert!(all.samples.iter().map(|x| x * x).sum::<f32>() > 0.01);
    }
}
