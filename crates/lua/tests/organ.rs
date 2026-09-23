use apteronotus_lua::evaluate;
use apteronotus_synth::{
    Note, Op, instantiate,
    lower::{render, rms},
};

fn pipe(kind: &str, feet: f64, gate: f64, velocity: f64) -> Vec<f32> {
    let program = evaluate(&format!(
        r#"
        voice {{ graph = function(n)
          return organ_pipe(n.hz, {{kind="{kind}", feet={feet}, chiff=0}})
        end }}
    "#
    ))
    .unwrap();
    let mut unit = instantiate(
        &program.voices[0],
        &Note::new(220.0).duration(gate).velocity(velocity),
    )
    .unwrap();
    render(unit.as_mut(), 24_000.0, gate + 0.5).remove(0)
}

fn amplitude(samples: &[f32], hz: f64) -> f64 {
    let mut real = 0.0;
    let mut imag = 0.0;
    for (i, sample) in samples.iter().enumerate() {
        let phase = std::f64::consts::TAU * hz * i as f64 / 24_000.0;
        real += f64::from(*sample) * phase.cos();
        imag += f64::from(*sample) * phase.sin();
    }
    2.0 * real.hypot(imag) / samples.len() as f64
}

#[test]
fn pipes_hold_without_decay_release_to_zero_and_ignore_key_velocity() {
    for kind in ["principal", "flute", "string", "reed"] {
        let samples = pipe(kind, 8.0, 10.0, 1.0);
        let early = rms(&samples[24_000..48_000]);
        let late = rms(&samples[9 * 24_000..10 * 24_000]);
        assert!(early > 0.05);
        assert!((late / early - 1.0).abs() < 0.001, "{kind}: {early} {late}");
        assert!(samples[252_000..].iter().all(|x| *x == 0.0));
        assert_eq!(pipe(kind, 8.0, 1.0, 0.1), pipe(kind, 8.0, 1.0, 1.0));
        // Key release during speech must not leave a long or resurrected note.
        let short = pipe(kind, 32.0, 0.005, 1.0);
        assert!(short[7200..].iter().all(|x| *x == 0.0));
    }
}

#[test]
fn footage_and_rank_spectra_are_audibly_distinct() {
    for feet in [4.0, 8.0, 16.0, 32.0] {
        let samples = pipe("flute", feet, 2.0, 1.0);
        let steady = &samples[24_000..48_000];
        let fundamental = 220.0 * 8.0 / feet;
        let a = amplitude(steady, fundamental);
        assert!(a > 0.5, "{feet}: {a}");
        assert!(amplitude(steady, fundamental * 2.0) / a < 0.03);
        assert!(amplitude(steady, fundamental * 3.0) / a > 0.15);
    }
    let flute = pipe("flute", 8.0, 2.0, 1.0);
    let reed = pipe("reed", 8.0, 2.0, 1.0);
    let ratio = |samples: &[f32]| {
        amplitude(&samples[24_000..48_000], 880.0) / amplitude(&samples[24_000..48_000], 220.0)
    };
    assert!(ratio(&reed) > ratio(&flute) * 20.0);
}

#[test]
fn stop_controls_remain_graph_inputs_and_invalid_specs_are_diagnostic() {
    let program = evaluate(
        r#"
      local stop = control {name="Reed", range={0,1}, default=.4}
      voice {graph=function(n)
        return organ(n.hz, {{kind="principal"}, {kind="reed", feet=16, level=stop}}) >> pan(0)
      end}
    "#,
    )
    .unwrap();
    assert_eq!(program.controls.specs().len(), 1);
    assert!(
        program.voices[0]
            .nodes
            .iter()
            .any(|n| matches!(n.op, Op::Harmonics { .. }))
    );
    for expression in [
        "harmonics(n.hz, {})",
        "harmonics(n.hz, {1, 1})",
        "harmonics(n.hz, {0/0})",
        "harmonics(n.hz, {1/0})",
        "organ_pipe(n.hz, {kind='unknown'})",
        "organ_pipe(n.hz, {feet=0})",
        "organ_pipe(n.hz, {cents=1/0})",
        "organ_pipe(n.hz, {chiff=-1})",
        "organ_pipe(n.hz, {attack=secs(-1)})",
        "organ_pipe(n.hz, {foot=8})",
        "organ(n.hz, {})",
        "organ_pipe(n.hz, {voicing='unknown'})",
        "flue_pipe(n.hz, .85, 0, {min_hz=0})",
        "flue_pipe(n.hz, .85, 0, {min_hz=1001})",
        "flue_pipe(n.hz, .85, 0, {min_hz=0/0})",
    ] {
        let source = format!("voice {{graph=function(n) return {expression} end}}");
        assert!(evaluate(&source).is_err(), "accepted {expression}");
    }
    assert!(evaluate("patch {graph=function() return organ_pipe(220) end}").is_err());
}

#[test]
fn drawing_a_stop_changes_a_held_key_without_reinstantiation() {
    use apteronotus_synth::{ControlStore, instantiate_with_controls};
    let program = evaluate(
        r#"
      local stop = control {name="Stop", range={0,1}, default=0}
      voice {graph=function(n)
        return organ_pipe(n.hz, {level=slew(stop, .015), chiff=0})
      end}
    "#,
    )
    .unwrap();
    let controls = ControlStore::new(&program.controls);
    let stop = program.controls.id("Stop").unwrap();
    let mut unit = instantiate_with_controls(
        &program.voices[0],
        &Note::new(220.0).duration(10.0),
        &controls,
    )
    .unwrap();
    unit.set_sample_rate(24_000.0);
    let mut output = [0.0];
    let mut window = || {
        let mut samples = Vec::new();
        for _ in 0..24_000 {
            unit.tick(&[], &mut output);
            samples.push(output[0]);
        }
        rms(&samples[12_000..])
    };
    assert_eq!(window(), 0.0);
    controls.set(stop, 1.0).unwrap();
    assert!(window() > 0.1);
    controls.set(stop, 0.0).unwrap();
    assert!(window() < 1e-5);
    controls.set(stop, 1.0).unwrap();
    assert!(window() > 0.1);
}

#[test]
fn speech_changes_attack_spectrum_and_keyboard_voicing_but_preserves_classic_option() {
    let tone = |voicing: &str| {
        let program = evaluate(&format!(
            r#"
          voice {{graph=function(n)
            return organ_pipe(n.hz, {{kind="string", chiff=0, voicing="{voicing}"}})
          end}}
        "#
        ))
        .unwrap();
        let mut unit = instantiate(&program.voices[0], &Note::new(500.0).duration(2.0)).unwrap();
        render(unit.as_mut(), 24_000.0, 2.5).remove(0)
    };
    let speech = tone("speech");
    let classic = tone("classic");
    let ratio = |samples: &[f32]| amplitude(samples, 3500.0) / amplitude(samples, 500.0);
    let early = &speech[240..720]; // 10..30 ms, ten complete fundamental periods
    let settled = &speech[24_000..48_000];
    assert!(
        ratio(early) > ratio(settled) * 1.3,
        "upper modes should speak earlier than foundation"
    );
    let classic_early = &classic[240..720];
    let classic_settled = &classic[24_000..48_000];
    assert!((ratio(classic_early) / ratio(classic_settled) - 1.0).abs() < 0.08);
    // Same rank, lower pipe: stronger upper spectrum instead of mere transposition.
    let low = pipe("flute", 32.0, 2.0, 1.0);
    let high = pipe("flute", 4.0, 2.0, 1.0);
    let low_ratio = amplitude(&low[24_000..48_000], 165.0) / amplitude(&low[24_000..48_000], 55.0);
    let high_ratio =
        amplitude(&high[24_000..48_000], 1320.0) / amplitude(&high[24_000..48_000], 440.0);
    assert!(low_ratio > high_ratio * 1.4);
}
