//! The finite chamber score through the production scheduler and shared room.
use apteronotus_lua::evaluate;
use apteronotus_pattern::Frac;
use apteronotus_render::{RenderOptions, RenderSpan, render};
use apteronotus_songs::THE_WIDOWS_CLOCK;

#[test]
fn widows_clock_has_seven_audible_parts_acceleration_and_a_finite_ending() {
    let program = evaluate(THE_WIDOWS_CLOCK).unwrap();
    let audio = render(
        &program,
        &RenderOptions {
            span: RenderSpan::Cycles(Frac::from(36)),
            tail_seconds: 7.0,
            sample_rate: 24_000.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(audio.channels, 2);
    assert_eq!(audio.clipped, 0);
    assert_eq!(audio.tracks.len(), 7);
    assert!(audio.tracks.iter().all(|track| track.voices > 0));
    assert!(audio.samples.iter().all(|x| x.is_finite()));
    let window_rms = |start: f64, end: f64| {
        let samples = &audio.samples[(start * 48_000.0) as usize..(end * 48_000.0) as usize];
        (samples.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    };
    let before = program.tempo.cycle_to_seconds(Frac::from(9));
    let hunt = program.tempo.cycle_to_seconds(Frac::from(21));
    assert!(window_rms(2.0, 6.0) > 0.01);
    assert!(window_rms(hunt, hunt + 4.0) > window_rms(2.0, 6.0) * 1.15);
    let lead = &audio.tracks[0];
    let spacing_near = |time: f64| {
        let pair = lead
            .onsets_seconds
            .windows(2)
            .find(|pair| pair[0] >= time)
            .unwrap();
        pair[1] - pair[0]
    };
    assert!(spacing_near(hunt) < spacing_near(before) * 0.6);
    let end = audio.scheduled_seconds;
    assert!(
        window_rms(end + 6.0, end + 7.0) < 0.0001,
        "room must drain after coda"
    );
}
