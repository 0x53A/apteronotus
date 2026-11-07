use apteronotus_lua::evaluate;
use apteronotus_render::{RenderOptions, RenderSpan, render};

#[test]
fn bowed_gestures_render_deterministically_without_a_device() {
    let program = evaluate(
        r#"
        tempo(120)
        local v=violin {string="D",model="c3"}
        play(v,timeline {
          at(secs(.1),v:bow {string="D",speed=.4,force=.55,over=ms(60)}),
          at(secs(1),v:stop {string="D",pitch="A4",over=ms(100)}),
          at(secs(1.15),v:vibrato {string="D",cents=12,rate=5.2,over=ms(700)})
        })
        run(v)
    "#,
    )
    .unwrap();
    for sample_rate in [24_000.0, 48_000.0] {
        let options = RenderOptions {
            sample_rate,
            span: RenderSpan::Seconds(8.0),
            tail_seconds: 0.0,
            ..RenderOptions::default()
        };
        let first = render(&program, &options).unwrap();
        let repeated = render(&program, &options).unwrap();
        assert_eq!(first.samples, repeated.samples);
        assert_eq!(first.clipped, 0);
        assert!(first.peak > 0.01);
    }
}

#[test]
fn middle_window_replays_actual_string_history_and_sample_order() {
    for model in ["c2", "c3"] {
        let p = evaluate(
            r#"
        tempo(120)
        local v=violin {string="D",model="MODEL"}
        play(v,timeline {
            at(secs(.1),v:bow {string="D",speed=.4,force=.55,over=ms(60)}),
            at(secs(1),v:stop {string="D",pitch="A4",over=ms(100)}),
            at(secs(1.15),v:vibrato {string="D",cents=12,rate=5.2,over=ms(700)}),
            at(secs(2),v:bow {speed=-.4,over=ms(90)}),
            at(secs(2.5),v:vibrato {string="D",cents=0,rate=5.2,over=ms(350)}),
            at(secs(3),v:lift_bow {over=ms(60)}),
        })
        run(v)
    "#
            .replace("MODEL", model)
            .as_str(),
        )
        .unwrap();
        let options = RenderOptions {
            sample_rate: 24000.0,
            span: RenderSpan::Seconds(4.0),
            tail_seconds: 0.0,
            ..RenderOptions::default()
        };
        let full = render(&p, &options).unwrap();
        let window = render(
            &p,
            &RenderOptions {
                span: RenderSpan::SecondsRange {
                    begin: 1.25,
                    end: 3.5,
                },
                ..options
            },
        )
        .unwrap();
        assert_eq!(window.samples, full.samples[30000 * 2..84000 * 2]);
        assert!(window.samples.iter().any(|v| v.abs() > 0.01));
    }
}
