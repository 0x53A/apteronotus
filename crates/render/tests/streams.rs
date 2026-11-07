use apteronotus_lua::evaluate;
use apteronotus_render::{RenderOptions, RenderSpan, render};
fn samples(source: &str) -> Vec<f32> {
    let program = evaluate(source).unwrap();
    render(
        &program,
        &RenderOptions {
            sample_rate: 24000.,
            span: RenderSpan::Seconds(0.2),
            tail_seconds: 0.,
            ..Default::default()
        },
    )
    .unwrap()
    .samples
}
#[test]
fn explicit_voice_destination_has_no_duplicate_main_path() {
    let x = samples(
        r#"local b=bus{channels=1}
      local v=voice{graph=function(n) return dc(.1) end}
      play(v,'c4',{out=b})
      output(b >> mul(2) >> pan(0))"#,
    );
    let last = &x[x.len() - 100..];
    assert!(
        last.iter()
            .all(|v| (*v - 0.2_f32 * 0.5_f32.sqrt()).abs() < 1e-5)
    );
}
#[test]
fn patch_destinations_and_explicit_inputs_compose() {
    let x = samples(
        r#"local a=bus(1);local b=bus(1)
      local source=patch{graph=function() return dc(.1) end}
      local effect=patch{inputs=1,graph=function(p) return p.inputs[1]*3 end}
      run(source,{out=a});run(effect,{inputs={a},out=b})
      output(b >> pan(0))"#,
    );
    assert!(
        x[x.len() - 100..]
            .iter()
            .all(|v| (*v - 0.3_f32 * 0.5_f32.sqrt()).abs() < 1e-5)
    );
}
#[test]
fn cycles_and_ambiguous_channel_layouts_are_rejected() {
    assert!(
        evaluate(
            r#"local a=bus(1);local b=bus(1)
      local p=patch{inputs=1,graph=function(p)return p.inputs[1] end}
      run(p,{inputs={a},out=b});run(p,{inputs={b},out=a});output(a>>pan(0))"#
        )
        .unwrap_err()
        .to_string()
        .contains("cycle")
    );
    assert!(
        evaluate(
            r#"local b=bus(2);local v=voice{graph=function()return dc(.1)end};play(v,'c4',{out=b})"#
        )
        .unwrap_err()
        .to_string()
        .contains("channel")
    );
}
#[test]
fn room_accepts_a_patch_stream_and_is_deterministic() {
    let source = r#"local b=bus(1)
      local p=patch{graph=function() return impulse()*.1 end};run(p,{out=b})
      local r=room{size={8,7,3.1},decay=secs(.2)}
      r:place(b,{name='Left',position={2.5,3.2,1.2}})
      output(r:render{listener=listener{position={4,1.3,1.25}},amount=0})"#;
    let a = samples(source);
    assert_eq!(a, samples(source));
    let left = a.chunks_exact(2).map(|x| x[0] * x[0]).sum::<f32>();
    let right = a.chunks_exact(2).map(|x| x[1] * x[1]).sum::<f32>();
    assert!(left > right * 2.);
    assert!(right > 0.);
}

#[test]
fn disconnected_cycles_are_rejected_and_stream_mix_sums() {
    assert!(evaluate(r#"local a=bus(1);local p=patch{inputs=1,graph=function(p)return p.inputs[1]end};run(p,{inputs={a},out=a})"#).unwrap_err().to_string().contains("cycle"));
    let audio = samples(
        r#"local a=bus(1);local p=patch{graph=function()return dc(.1)end};run(p,{out=a});output(mix{a,a>>mul(2)}>>pan(0))"#,
    );
    assert!((audio[audio.len() - 1] - 0.3 * 0.5_f32.sqrt()).abs() < 1e-5);
}
#[test]
fn room_geometry_reuses_the_arena_but_topology_does_not() {
    let source = r#"local b=bus(1);local p=patch{graph=function()return dc(.1)end};run(p,{out=b});local r=room{size={8,7,3}};r:place(b,{name='a',position={2,3,1}});output(r:render{listener=listener{position={4,1,1}}})"#;
    let active = evaluate(source).unwrap();
    let mut moved = evaluate(&source.replace("{2,3,1}", "{6,3,1}")).unwrap();
    assert!(!moved.persistent_compatible_with(&active));
    let changes = moved.reuse_with_room_edits(&active).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(moved.controls, active.controls);
    let mut changed = evaluate(&source.replace("dc(.1)", "dc(.2)")).unwrap();
    assert!(changed.reuse_with_room_edits(&active).is_none());
}
#[test]
fn explicit_stereo_layout_and_unknown_options_are_checked() {
    let prefix = "local r=room{size={8,7,3}};local b=bus(2);";
    assert!(
        evaluate(&format!(
            "{prefix}r:place(b,{{name='s',position={{3,3,1}}}})"
        ))
        .is_err()
    );
    let source = format!(
        "{prefix}r:place(b,{{name='s',position={{3,3,1}},layout=stereo_pair{{spacing=.8}}}});output(r:render{{listener=listener{{position={{4,1,1}}}}}})"
    );
    evaluate(&source).unwrap();
    assert!(evaluate("room{size={8,7,3},decaay=1}").is_err());
}

#[test]
fn redirected_event_sends_and_legacy_returns_still_sound() {
    let audio = samples(
        r#"local b=bus(1);local fx=send{graph=mul(1),level=.5}
      local v=voice{graph=function()return dc(.1)end}
      play(v,pattern('c4')>>to(fx,.4),{out=b});output(b>>pan(0))"#,
    );
    let expected = 0.1 * 0.5_f32.sqrt() + 0.1 * 0.4 * 0.5;
    assert!((audio[audio.len() - 1] - expected).abs() < 1e-5);
}
#[test]
fn generic_convolution_and_external_streams_use_the_same_graph() {
    let audio = samples(
        r#"local a=audio_input{name='test',channels=1,fallback='silence'}
      local b=bus(1);local p=patch{graph=function()return dc(.1)end};run(p,{out=b})
      output(mix{a,b}>>convolve{sample_rate=24000,samples={.5,.25,.25}}>>pan(0))"#,
    );
    assert!((audio[audio.len() - 1] - 0.1 * 0.5_f32.sqrt()).abs() < 1e-5);
}

#[test]
fn room_replacement_keeps_the_producer_running_and_reaches_the_new_response() {
    use apteronotus_render::persistent_runtime;
    use fundsp::prelude32::BufferVec;
    let source = r#"local b=bus(1);local p=patch{graph=function()return sine(700)*.1 end};run(p,{out=b});local r=room{size={8,7,3},decay=.05};r:place(b,{name='a',position={2,3,1}});output(r:render{listener=listener{position={4,1,1}}})"#;
    let old = evaluate(source).unwrap();
    let mut new = evaluate(&source.replace("{2,3,1}", "{6,3,1}")).unwrap();
    let mut runtime = persistent_runtime(&old).unwrap();
    let mut unit = runtime.take_processor();
    unit.set_sample_rate(24000.);
    let mut reference_runtime = persistent_runtime(&new).unwrap();
    let mut reference = reference_runtime.take_processor();
    reference.set_sample_rate(24000.);
    let input = BufferVec::new(old.buses.total_channels());
    let mut output = BufferVec::new(old.buses.total_channels());
    let mut expected = BufferVec::new(old.buses.total_channels());
    for _ in 0..100 {
        unit.process(64, &input.buffer_ref(), &mut output.buffer_mut());
        reference.process(64, &input.buffer_ref(), &mut expected.buffer_mut());
    }
    let edits = new.reuse_with_room_edits(&old).unwrap();
    for edit in runtime.controls().prepare_room_edits(&edits).unwrap() {
        edit.commit();
    }
    for _ in 0..200 {
        unit.process(64, &input.buffer_ref(), &mut output.buffer_mut());
        reference.process(64, &input.buffer_ref(), &mut expected.buffer_mut());
    }
    for ch in 0..2 {
        for i in 0..64 {
            assert!(
                (output.buffer_ref().at_f32(ch, i) - expected.buffer_ref().at_f32(ch, i)).abs()
                    < 2e-5
            );
        }
    }
}

#[test]
fn live_instruments_retain_their_bindings_when_routed() {
    let program=evaluate(r#"local b=bus(1);local v=voice{graph=function(n)return sine(n.hz)*adsr(.01,.02,.7,.1)end};midi(v,{out=b,channel=2});output(b>>pan(0))"#).unwrap();
    let voice = program.voice(program.midi_bindings[0].voice).unwrap();
    assert_eq!(voice.channels(), 2);
    assert_eq!(voice.sends.len(), 1);
    let retained=evaluate(r#"local b=bus(2);local v=violin{name='V',string='D',mode='live',model='c3'};midi(v);run(v,{out=b});output(b)"#).unwrap();
    assert!(
        retained
            .runs
            .iter()
            .any(|r| r.patch == retained.midi_violins[0].patch)
    );
}
