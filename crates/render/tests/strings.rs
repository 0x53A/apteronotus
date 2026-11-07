//! Retained strings through the production scheduler and patch boundary.
use apteronotus_lua::evaluate;
use apteronotus_render::{RenderOptions, RenderSpan, render};
use apteronotus_synth::{Op, PatchTemplate, Source};

fn options() -> RenderOptions {
    RenderOptions {
        span: RenderSpan::Seconds(4.0),
        tail_seconds: 0.0,
        sample_rate: 24_000.0,
        ..Default::default()
    }
}

#[test]
fn retained_strings_ring_and_one_pick_does_not_disturb_the_other() {
    let mut program = evaluate(include_str!("../../test-fixtures/strings.eod")).unwrap();
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
    assert_eq!(strings.len(), 2);
    graph.outputs = vec![strings[0], strings[0]];
    program.patches[0] = PatchTemplate::new(graph).unwrap();
    let full = render(&program, &options()).unwrap();
    let mut selected = options();
    selected.tracks.exclude([1]);
    let low_only = render(&program, &selected).unwrap();
    assert_eq!(full.samples, low_only.samples);
    let rms = |a: f64, b: f64| {
        let window = &full.samples[(a * 48_000.0) as usize..(b * 48_000.0) as usize];
        (window.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / window.len() as f64).sqrt()
    };
    assert!(rms(0.1, 0.8) > 0.00001);
    assert!(rms(3.8, 4.0) < rms(0.1, 0.8) * 0.05);
}
