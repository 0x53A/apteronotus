use apteronotus_lua::evaluate;
use apteronotus_synth::{
    Note, instantiate,
    lower::{render, rms},
};
fn sound(hz: f64, options: &str, gate: f64, rate: f64) -> Vec<f32> {
    let p = evaluate(&format!(
        "voice {{graph=function(n) return flute(n.hz,{{{options}}}) end}}"
    ))
    .unwrap();
    let mut u = instantiate(&p.voices[0], &Note::new(hz).duration(gate)).unwrap();
    render(u.as_mut(), rate, gate + 0.5).remove(0)
}
fn amplitude(x: &[f32], hz: f64, sr: f64) -> f64 {
    let (mut r, mut i) = (0., 0.);
    for (n, v) in x.iter().enumerate() {
        let phase = std::f64::consts::TAU * hz * n as f64 / sr;
        r += *v as f64 * phase.cos();
        i += *v as f64 * phase.sin();
    }
    2. * r.hypot(i) / x.len() as f64
}
#[test]
fn flute_registers_keep_tuning_and_have_a_real_second_harmonic() {
    for rate in [24000., 48000.] {
        for hz in [523.2511306011972, 880.] {
            let x = sound(hz, "vibrato=0,breath=0", 1.5, rate);
            let steady = &x[(rate * 0.5) as usize..rate as usize];
            let a = amplitude(steady, hz, rate);
            let b = amplitude(steady, hz * 2., rate);
            assert!(a > 0.3);
            assert!(b / a > 0.07);
            assert!(b / a < 0.5);
            assert!(rms(&x[(rate * 1.8) as usize..]) < 1e-7);
        }
    }
    let low = sound(523.2511306011972, "vibrato=0,breath=0", 1.5, 24000.);
    let high = sound(880., "vibrato=0,breath=0", 1.5, 24000.);
    let ratio = |x: &[f32], f: f64| {
        amplitude(&x[12000..24000], 2. * f, 24000.) / amplitude(&x[12000..24000], f, 24000.)
    };
    assert!(ratio(&low, 523.2511306011972) > ratio(&high, 880.) * 2.);
}
#[test]
fn flute_pressure_silence_short_release_and_validation() {
    assert!(
        sound(587.33, "pressure=0", 1., 24000.)
            .iter()
            .all(|x| *x == 0.)
    );
    let short = sound(587.33, "", 0.01, 24000.);
    assert!(rms(&short[7200..]) < 1e-7);
    assert_eq!(sound(587.33, "", 1., 24000.), sound(587.33, "", 1., 24000.));
    for spec in [
        "vibrato=-1",
        "rate=0/0",
        "breath=1",
        "brightness=2",
        "unknown=1",
    ] {
        assert!(
            evaluate(&format!(
                "voice {{graph=function(n) return flute(n.hz,{{{spec}}}) end}}"
            ))
            .is_err()
        );
    }
}
