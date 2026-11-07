use apteronotus_lua::evaluate;
use apteronotus_synth::{ControlStore, instantiate_patch};

#[test]
fn c1_is_a_retained_patch_and_invalid_declarations_are_diagnostic() {
    let p = evaluate("local v=patch {graph=function() return bowed_string_model(293.6648,.4,.55,.12,{min_hz=196,max_hz=660}) >> pan(0) end}; run(v)").unwrap();
    let controls = ControlStore::new(&p.controls);
    let mut unit = instantiate_patch(&p.patches[0], &controls).unwrap();
    let sound = apteronotus_synth::lower::render(unit.as_mut(), 48_000.0, 2.0);
    assert!(apteronotus_synth::lower::rms(&sound[0][48000..]) > 0.001);
    assert!((p.patches[0].graph().cost().delay_buffer_seconds - 2.0 / 196.0).abs() < 1e-10);
    for bounds in [
        "min_hz=0,max_hz=660",
        "min_hz=660,max_hz=196",
        "min_hz=196,max_hz=1/0",
    ] {
        assert!(evaluate(&format!("patch {{graph=function() return bowed_string_model(293.6648,.4,.55,.12,{{{bounds}}}) end}}" )).is_err());
    }
}

#[test]
fn authored_gestures_execute_at_samples_and_retain_physical_history() {
    let p = evaluate(
        r#"
      tempo(120)
      local v = violin {string="D"}
      play(v, timeline {
        at(secs(1), v:bow {string="D", speed=.4, force=.55, over=ms(60)}),
        at(secs(2), v:lift_bow {over=ms(60)}),
        at(secs(2.02), v:bow {force=.55, over=ms(20)}),
        at(secs(3), v:stop {string="D", pitch="E4", over=ms(80)}),
        at(secs(3.02), v:release_finger {string="D", over=ms(20)}),
        at(secs(4), v:lift_bow {over=ms(60)}),
      })
      run(v)
    "#,
    )
    .unwrap();
    let controls = ControlStore::new(&p.controls);
    let mut unit = instantiate_patch(&p.patches[0], &controls).unwrap();
    let sound = apteronotus_synth::lower::render(unit.as_mut(), 48_000.0, 5.0);
    assert!(sound[0][..48000].iter().all(|v| *v == 0.0));
    assert!(
        apteronotus_synth::lower::rms(&sound[0][120000..144000]) > 0.005,
        "rms={} guards={:?}",
        apteronotus_synth::lower::rms(&sound[0][120000..144000]),
        controls
            .bowed_handle(p.controls.id("Violin expression").unwrap())
            .unwrap()
            .diagnostics()
    );
    assert_eq!(
        controls
            .bowed_handle(p.controls.id("Violin expression").unwrap())
            .unwrap()
            .diagnostics()
            .guards,
        0
    );
}

#[test]
fn unsupported_gestures_fail_before_publication() {
    for command in [
        "v:bow {speed=.4,force=.55}",
        "v:bow {string='A',speed=.4,force=.55}",
        "v:stop {string='D',pitch='A5'}",
        "v:bow {string='D',speed=2,force=.55}",
    ] {
        let source = format!(
            "tempo(120); local v=violin {{string='D'}}; play(v,timeline {{at(secs(0),{command})}}); run(v)"
        );
        assert!(evaluate(&source).is_err(), "{source}");
    }
}

#[test]
fn live_keyboard_ownership_is_owned_and_exclusive() {
    let p=evaluate("tempo(120); local v=violin {string='D',mode='live'}; midi(v); midi(v.pressure,{cc=83}); run(v)").unwrap();
    assert_eq!(p.midi_violins.len(), 1);
    assert_eq!(
        (p.midi_violins[0].min_key, p.midi_violins[0].max_key),
        (62, 76)
    );
    assert_eq!(p.controls.specs().len(), 4);
    for source in [
        "local v=violin {string='D'}; midi(v); run(v)",
        "local v=violin {mode='live'}; midi(v); run(v)",
        "local v=violin {string='D',mode='live'}; midi(v)",
        "tempo(120); local v=violin {string='D',mode='live'}; play(v,timeline {at(secs(0),v:bow {string='D',force=.5})}); run(v)",
    ] {
        assert!(evaluate(source).is_err(), "{source}");
    }
}

#[test]
fn compatible_edit_reuses_gesture_port_and_pending_plan() {
    let source = "tempo(120); local v=violin {string='D'}; play(v,timeline {at(secs(4),v:bow {string='D',force=.5,speed=.4})}); run(v)";
    let active = evaluate(source).unwrap();
    let mut next = evaluate(source).unwrap();
    assert!(next.reuse_persistent_from(&active));
    assert_eq!(next.controls, active.controls);
    assert_eq!(next.patches, active.patches);
    let changed = evaluate(&source.replace("secs(4)", "secs(5)")).unwrap();
    assert!(!changed.persistent_compatible_with(&active));
}

#[test]
fn violin_contact_models_are_explicit_and_changing_model_requires_replacement() {
    use apteronotus_synth::{Op, bowed::BowedModel};
    let source = "local v=violin {string='D',model='c3'}; run(v)";
    let active = evaluate(source).unwrap();
    assert!(active.patches.iter().any(|p| {
        p.graph().nodes.iter().any(|n|
        matches!(&n.op, Op::BowedPerformance {score, ..} if score.config.model == BowedModel::C3))
    }));
    let mut same = evaluate(source).unwrap();
    assert!(same.reuse_persistent_from(&active));
    for model in ["c1", "c2", "c4"] {
        let changed = evaluate(&source.replace("c3", model)).unwrap();
        assert!(!changed.persistent_compatible_with(&active));
    }
    assert!(evaluate(&source.replace("c3", "unknown")).is_err());
    assert!(evaluate("patch {graph=function() return bowed_string_model(293.6648,.4,.55,.12,{min_hz=196,max_hz=660,model='c4'}) end}").is_ok());
    assert!(evaluate("patch {graph=function() return bowed_string_model(293.6648,.4,.55,.12,{min_hz=196,max_hz=660,model='c3'}) end}").is_ok());
}

#[test]
fn beat_slide_follows_tempo_integral_instead_of_seconds_fraction() {
    use apteronotus_synth::Op;
    let p = evaluate(
        r#"
        tempo {{at=bars(0),bpm=60},{at=bars(2),bpm=120,over=bars(2)}}
        local v=violin {string="D"}
        play(v,timeline {at(bars(0),v:slide {string="D",pitch="G4",over=beats(8)})})
        run(v)
    "#,
    )
    .unwrap();
    let score = p.patches[0]
        .graph()
        .nodes
        .iter()
        .find_map(|n| match &n.op {
            Op::BowedPerformance { score, .. } => Some(score),
            _ => None,
        })
        .unwrap();
    let mut performance = score.prepare(24_000.0).unwrap();
    for _ in 0..48000 {
        performance.next_frame();
    }
    performance.next_frame();
    let cycle = score.clock.as_ref().unwrap().cycle_at(2.0);
    let expected = 293.6647679174076 * 2.0_f64.powf(5.0 * cycle / 2.0 / 12.0);
    assert!((performance.state().controls(48000, 24000.0, 0).hz - expected).abs() < 1e-8);
}

#[test]
fn vibrato_depth_fade_uses_device_samples_and_beat_coordinates() {
    use apteronotus_synth::Op;
    for beat_ramp in [false, true] {
        let (tempo, duration, t, rate) = if beat_ramp {
            (
                "tempo {{at=bars(0),bpm=60},{at=bars(2),bpm=120,over=bars(2)}}",
                "beats(8)",
                2.0,
                0.125,
            )
        } else {
            ("tempo(120)", "secs(1)", 0.125, 2.0)
        };
        let source = format!(
            "{tempo}; local v=violin {{string='D',model='c3'}}; play(v,timeline {{at(secs(0),v:stop {{string='D',pitch='A4'}}),at(secs(0),v:vibrato {{string='D',cents=20,rate={rate},over={duration}}})}}); run(v)"
        );
        let p = evaluate(&source).unwrap();
        let score = p.patches[0]
            .graph()
            .nodes
            .iter()
            .find_map(|n| match &n.op {
                Op::BowedPerformance { score, .. } => Some(score),
                _ => None,
            })
            .unwrap();
        let depth = 20.0
            * if beat_ramp {
                score.clock.as_ref().unwrap().cycle_at(t) / 2.0
            } else {
                t
            };
        for sample_rate in [24_000.0, 48_000.0] {
            let at = (t * sample_rate) as u64;
            let mut performance = score.prepare(sample_rate).unwrap();
            for _ in 0..=at {
                performance.next_frame();
            }
            let hz = performance.state().controls(at, sample_rate, 0).hz;
            assert!((1200.0 * (hz / 440.0).log2() - depth).abs() < 1e-8);
        }
    }
    let error=evaluate("tempo(120); local v=violin {string='D',model='c3'}; play(v,timeline {at(secs(0),v:stop {string='D',pitch='E5'}),at(secs(0),v:vibrato {string='D',cents=20,over=secs(1)})}); run(v)").unwrap_err().to_string();
    assert!(error.contains("joint pitch/position"), "{error}");
}

#[test]
fn joint_bounds_use_each_unbowed_strings_retained_contact_position() {
    let source = r#"
        tempo(120)
        local v=violin {}
        play(v,timeline {
            at(secs(0),v:bow {string="G",position=.035,speed=.4,force=.55}),
            at(secs(.1),v:lift_bow {}),
            at(secs(.1),v:bow {string="D",position=.12,speed=.4,force=.55}),
            at(secs(.2),v:stop {string="G",pitch="A4"}),
        })
        run(v)
    "#;
    let error = evaluate(source).unwrap_err().to_string();
    assert!(error.contains("joint pitch/position"), "{error}");
}

#[test]
fn owned_program_validation_rejects_aliasing_retained_addresses() {
    let mut p = evaluate("local v=violin {string='D'}; run(v)").unwrap();
    p.runs.push(p.runs[0].clone());
    let error = p
        .validate(apteronotus_lua::Limits::default().graph_publication)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("duplicate retained violin address"),
        "{error}"
    );
}

#[test]
fn measured_pitch_and_extended_score_range_keep_joint_domain_validation() {
    use apteronotus_synth::Op;
    let source = "tempo(120); local v=violin {string='G',model='c3',max_hz=1500}; play(v,timeline {at(secs(0),v:bow {string='G',position=.30,speed=.4,force=.55}),at(secs(.1),v:stop {string='G',hz=997.3,over=ms(20)}),at(secs(.4),v:lift_bow {over=ms(50)})}); run(v)";
    let p = evaluate(source).unwrap();
    let score = p.patches[0]
        .graph()
        .nodes
        .iter()
        .find_map(|n| match &n.op {
            Op::BowedPerformance { score, .. } => Some(score),
            _ => None,
        })
        .unwrap();
    for rate in [24000.0, 48000.0] {
        let mut performance = score.prepare(rate).unwrap();
        for _ in 0..rate as usize {
            assert!(performance.next_frame().iter().all(|x| x.is_finite()));
        }
        assert_eq!(performance.diagnostics().emergency_resets, 0);
        assert_eq!(performance.diagnostics().rejected_controls, 0);
    }
    for invalid in [
        source.replace("position=.30", "position=.12"),
        source.replace("hz=997.3", "hz=997.3,pitch='B5'"),
        source.replace("hz=997.3", "hz=0/0"),
        source.replace("max_hz=1500", "max_hz=2001"),
        source.replace("hz=997.3", "hz=1501"),
        source.replace("max_hz=1500", "max_hz=1500,mode='live'"),
    ] {
        assert!(evaluate(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn cello_has_its_own_body_open_strings_and_midi_range() {
    use apteronotus_synth::{Op, bowed::BowedKind};
    let p = evaluate("local c=cello {mode='live',string='C'}; midi(c); run(c)").unwrap();
    assert_eq!(p.midi_violins[0].min_key, 36);
    let score = p.patches[0]
        .graph()
        .nodes
        .iter()
        .find_map(|n| match &n.op {
            Op::BowedPerformance { score, .. } => Some(score),
            _ => None,
        })
        .unwrap();
    assert_eq!(score.config.kind, BowedKind::Cello);
    assert!((score.opens[0] - 65.40639132514966).abs() < 1e-10);
    assert!(score.prepare(24000.).is_ok());
    assert!(evaluate("local c=cello {string='E'};run(c)").is_err());
    assert!(evaluate("local v=violin {string='C'};run(v)").is_err());
    assert!(evaluate("tempo(120);local c=cello {string='C'};play(c,timeline{at(secs(0),c:stop{string='C',pitch='B1'})});run(c)").is_err());
}
