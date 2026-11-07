use apteronotus_lua::{Evaluator, Limits, evaluate};

#[test]
fn keyboard_declarations_are_owned_and_channel_filtered_with_silent_pattern_fallback() {
    let program = evaluate(r#"
        local v=voice {graph=function(n) return sine(n.hz)*adsr(ms(5),ms(10),.8,ms(100)) >> pan(0) end}
        midi(v)
        midi(v,{channel=3})
    "#).unwrap();
    assert!(program.tracks.is_empty());
    assert_eq!(program.midi_bindings.len(), 2);
    assert_eq!(program.midi_bindings[0].channel, None);
    assert_eq!(program.midi_bindings[1].channel, Some(2));
    assert!(program.voice(program.midi_bindings[0].voice).is_some());
}

#[test]
fn keyboard_rejects_unknown_duration_and_invalid_channels_at_evaluation() {
    for graph in [
        "sine(n.hz)*n.duration",
        "sine(n.hz)*window(secs(0),n.duration)",
    ] {
        let error = evaluate(&format!(
            "local v=voice {{graph=function(n) return {graph} end}}; midi(v)"
        ))
        .unwrap_err();
        assert!(error.to_string().contains("duration"), "{error}");
    }
    for options in [
        "{channel=0}",
        "{channel=17}",
        "{channel=1.5}",
        "{slider=1}",
        "true",
    ] {
        assert!(
            evaluate(&format!(
                "local v=voice {{graph=function(n) return sine(n.hz) end}}; midi(v,{options})"
            ))
            .is_err()
        );
    }
    assert!(evaluate("midi(patch{graph=function() return sine(220) end})").is_err());
    assert!(
        Evaluator::new(Limits {
            tracks: 0,
            ..Limits::default()
        })
        .evaluate("local v=voice{graph=function(n) return sine(n.hz) end}; midi(v)")
        .is_err()
    );
}

#[test]
fn tracks_and_keyboard_bindings_share_the_publication_budget() {
    let evaluator = Evaluator::new(Limits {
        tracks: 2,
        ..Limits::default()
    });
    let voice = "local v=voice{graph=function(n) return sine(n.hz) end}; ";
    assert!(
        evaluator
            .evaluate(&format!("{voice}midi(v); midi(v)"))
            .is_ok()
    );
    assert!(
        evaluator
            .evaluate(&format!("{voice}play(v, 'c4'); midi(v)"))
            .is_ok()
    );
    for bindings in [
        "play(v, 'c4'); midi(v); midi(v)",
        "midi(v); midi(v); play(v, 'c4')",
    ] {
        assert!(evaluator.evaluate(&format!("{voice}{bindings}")).is_err());
    }
}

#[test]
fn suggested_slider_mapping_is_owned_validated_and_rebound_across_edits() {
    let source = r#"
        local c=control{name="expression",range={-2,4},default=1}
        midi(c,{cc=82,channel=3})
    "#;
    let active = evaluate(source).unwrap();
    let mut candidate = evaluate(&source.replace("cc=82", "cc=83")).unwrap();
    let binding = &candidate.midi_controls[0];
    assert_eq!(binding.channel, Some(2));
    assert_eq!(binding.cc, 83);
    assert_ne!(binding.control, active.midi_controls[0].control);
    let spec = candidate.controls.spec(binding.control).unwrap();
    assert_eq!(binding.value(spec, 0), -2.0);
    assert_eq!(binding.value(spec, 127), 4.0);
    candidate
        .validate(Limits::default().graph_publication)
        .unwrap();
    assert!(candidate.reuse_persistent_from(&active));
    assert_eq!(
        candidate.midi_controls[0].control,
        active.midi_controls[0].control
    );
    assert_eq!(candidate.midi_controls[0].cc, 83);
    candidate
        .validate(Limits::default().graph_publication)
        .unwrap();
}

#[test]
fn invalid_slider_mappings_are_refused() {
    let declaration = "local c=control{name='c',range={0,1},default=.5}; ";
    for options in [
        "{}",
        "{cc=-1}",
        "{cc=120}",
        "{cc=128}",
        "{cc=1.5}",
        "{cc=82,channel=0}",
        "{cc=82,channel=17}",
        "{cc=82,unknown=1}",
        "true",
    ] {
        assert!(
            evaluate(&format!("{declaration}midi(c,{options})")).is_err(),
            "{options}"
        );
    }
    assert!(
        evaluate(&format!(
            "{declaration}midi(c,{{cc=82}}); midi(c,{{cc=83}})"
        ))
        .is_err()
    );
    assert!(evaluate("midi(control{range={0,1},default=.5},{cc=82})").is_err());
}
