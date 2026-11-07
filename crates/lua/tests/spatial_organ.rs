use apteronotus_lua::evaluate;
use apteronotus_synth::{Note, instantiate, lower::render};

fn sound(graph: &str, hz: f64) -> Vec<Vec<f32>> {
    let p = evaluate(&format!("voice {{graph=function(n) {graph} end}}")).unwrap();
    let mut u = instantiate(&p.voices[0], &Note::new(hz).duration(0.5)).unwrap();
    render(u.as_mut(), 48000., 1.)
}

#[test]
fn spatial_field_preserves_mono_and_drains_after_release() {
    for hz in [65., 440., 1760.] {
        let wide = sound(
            "local t=organ(n.hz,{{chiff=0}}); return (organ_space(t,{width=1.2}) | t) >> (mul(1)|mul(1)|mul(1))",
            hz,
        );
        assert_eq!(wide.len(), 3);
        for i in 0..wide[2].len() {
            assert!(((wide[0][i] + wide[1][i]) * 0.5 - wide[2][i]).abs() < 2e-6);
            assert!(wide[0][i].is_finite() && wide[1][i].is_finite());
        }
        assert!(
            wide.iter()
                .all(|c| c[40000..].iter().all(|x| x.abs() < 1e-6))
        );
    }
}

#[test]
fn zero_width_is_centered_and_bass_side_is_suppressed() {
    let center = sound("return spatial_organ(n.hz,nil,{width=0})", 220.);
    assert_eq!(center[0], center[1]);
    let side_ratio = |hz| {
        let x = sound(
            "return organ_space(sine(n.hz)*adsr(ms(5),ms(1),1,ms(20)),{width=1})",
            hz,
        );
        let mut side = 0.;
        let mut mid = 0.;
        for i in 4800..19200 {
            side += (x[0][i] - x[1][i]).powi(2);
            mid += (x[0][i] + x[1][i]).powi(2);
        }
        side / mid
    };
    assert!(side_ratio(65.) < 0.0001);
    assert!(side_ratio(1100.) > 0.05);
}

#[test]
fn spatial_options_are_validated_and_controls_can_modulate() {
    for spec in [
        "width=-1",
        "cutoff=0",
        "delay_a_ms=51",
        "delay_b_ms=0/0",
        "unknown=1",
    ] {
        assert!(
            evaluate(&format!(
                "voice {{graph=function(n) return spatial_organ(n.hz,nil,{{{spec}}}) end}}"
            ))
            .is_err()
        );
    }
    assert!(evaluate("local w=control{name='width',range={0,2},default=.7}; voice {graph=function(n) return spatial_organ(n.hz,nil,{width=w,cutoff=300+n.velocity*100}) end}").is_ok());
}
