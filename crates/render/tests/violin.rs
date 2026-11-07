//! Finite timeline scheduling across a changing tempo.
use apteronotus_lua::evaluate;
use apteronotus_pattern::Frac;
use apteronotus_render::{RenderOptions, RenderSpan, render};

#[test]
fn finite_timeline_tracks_tempo_and_ends() {
    let program = evaluate(
        r#"
      tempo { {at=bars(0), bpm=60}, {at=bars(2), bpm=120} }
      local tone = voice { graph=function(n)
        return sine(n.hz) * adsr(ms(5), ms(20), 0.7, ms(50)) * 0.1 >> pan(0)
      end }
      play(tone, timeline {
        at(bars(0), pattern("a4") >> hold(bars(0.1))),
        at(bars(1), pattern("a4") >> hold(bars(0.1))),
        at(bars(2), pattern("a4") >> hold(bars(0.1))),
        at(bars(3), pattern("a4") >> hold(bars(0.1))),
      })
    "#,
    )
    .unwrap();
    let audio = render(
        &program,
        &RenderOptions {
            span: RenderSpan::Cycles(Frac::from(4)),
            tail_seconds: 1.0,
            sample_rate: 24_000.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(audio.channels, 2);
    assert_eq!(audio.clipped, 0);
    let onsets = &audio.tracks[0].onsets_seconds;
    assert_eq!(onsets.len(), 4);
    assert!(onsets[3] - onsets[2] < (onsets[1] - onsets[0]) * 0.6);
    let tail = &audio.samples[audio.samples.len() - 24_000..];
    assert!(tail.iter().all(|x| x.abs() < 0.00001));
}
