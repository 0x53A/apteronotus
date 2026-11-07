//! Organ voices through the production scheduler.
use apteronotus_lua::evaluate;
use apteronotus_render::{RenderOptions, RenderSpan, render};

#[test]
fn additive_and_physical_pipes_sound_and_release() {
    let program = evaluate(include_str!("../../test-fixtures/organ.eod")).unwrap();
    let options = RenderOptions {
        span: RenderSpan::Seconds(4.0),
        tail_seconds: 0.0,
        sample_rate: 24_000.0,
        ..Default::default()
    };
    let audio = render(&program, &options).unwrap();
    assert_eq!(audio.channels, 2);
    assert_eq!(audio.clipped, 0);
    let rms = |a: f64, b: f64| {
        let window = &audio.samples[(a * 48_000.0) as usize..(b * 48_000.0) as usize];
        (window.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / window.len() as f64).sqrt()
    };
    assert!(rms(0.2, 0.8) > 0.001);
    assert!(rms(2.2, 2.8) > 0.001);
    assert!(rms(3.6, 4.0) < 0.00001);
    assert_eq!(audio.samples, render(&program, &options).unwrap().samples);
}
