use apteronotus_lua::evaluate;
use apteronotus_synth::{
    Note, instantiate,
    lower::{render, rms},
};

fn bowed(options: &str, hz: f64, gate: f64) -> Vec<f32> {
    let program = evaluate(&format!(
        "voice {{graph=function(n) return bowed_string(n.hz, {{{options}}}) end}}"
    ))
    .unwrap();
    let mut unit = instantiate(&program.voices[0], &Note::new(hz).duration(gate)).unwrap();
    render(unit.as_mut(), 24_000.0, gate + 0.6).remove(0)
}

#[test]
fn bowed_families_sustain_and_close_even_when_released_during_attack() {
    for (kind, hz) in [("violin", 440.0), ("viola", 220.0), ("cello", 110.0)] {
        let options = format!("kind='{kind}', vibrato=0, bow=0");
        let samples = bowed(&options, hz, 3.0);
        assert!(samples.iter().all(|x| x.is_finite()));
        let early = rms(&samples[24_000..48_000]);
        let late = rms(&samples[48_000..72_000]);
        assert!(early > 0.02, "{kind}: {early}");
        assert!(
            (late / early - 1.0).abs() < 0.025,
            "sustain decayed: {kind}"
        );
        assert!(samples[84_000..].iter().all(|x| *x == 0.0));
        let short = bowed(&options, hz, 0.01);
        assert!(short[12_000..].iter().all(|x| *x == 0.0));
    }
}

#[test]
fn vibrato_waits_for_the_bow_and_repeated_renders_are_identical() {
    let straight = bowed("vibrato=0", 440.0, 2.0);
    let moving = bowed("vibrato=24", 440.0, 2.0);
    assert_eq!(
        &straight[..3600],
        &moving[..3600],
        "initial pitch should settle first"
    );
    let difference: Vec<_> = straight[24_000..48_000]
        .iter()
        .zip(&moving[24_000..48_000])
        .map(|(a, b)| a - b)
        .collect();
    assert!(rms(&difference) > 0.02, "held pitch should acquire vibrato");
    assert_eq!(moving, bowed("vibrato=24", 440.0, 2.0));
}

#[test]
fn invalid_options_are_diagnostic_and_pressure_can_be_a_live_graph_input() {
    for options in [
        "kind='bassoon'",
        "vibrato=-1",
        "rate=0",
        "bow=.2",
        "brightness=2",
        "vibrato=0/0",
        "rate=1/0",
        "attack=ms(-2)",
        "release=ms(-2)",
        "rosin=.1",
    ] {
        assert!(
            evaluate(&format!(
                "voice {{graph=function(n) return bowed_string(n.hz, {{{options}}}) end}}"
            ))
            .is_err(),
            "accepted {options}"
        );
    }
    assert!(evaluate("voice {graph=function(n) return bowed_string(n.hz, 3) end}").is_err());
    assert!(evaluate("patch {graph=function() return bowed_string(440) end}").is_err());
    let program = evaluate(
        r#"
      local pressure=control {name="Bow", range={0,1}, default=.5}
      voice {graph=function(n) return bowed_string(n.hz, {pressure=pressure}) end}
    "#,
    )
    .unwrap();
    assert_eq!(program.controls.specs().len(), 1);
    let soft = bowed("pressure=0, vibrato=0, bow=0", 440.0, 1.0);
    let hard = bowed("pressure=1, vibrato=0, bow=0", 440.0, 1.0);
    assert_ne!(soft, hard, "bow pressure must affect body colour");
}
