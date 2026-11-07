//! The hardware/browser-independent production path, rendered into memory.
use apteronotus_live::{MidiVoices, PersistentRuntime, RoutedRuntime};
use apteronotus_lua::evaluate;
use apteronotus_synth::EventRouting;
use fundsp::prelude32::{AudioUnit, BufferVec};

fn render(unit: &mut dyn AudioUnit, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * unit.outputs());
    let input = BufferVec::new(0);
    let mut buffer = BufferVec::new(unit.outputs());
    for start in (0..frames).step_by(64) {
        let size = (frames - start).min(64);
        unit.process(size, &input.buffer_ref(), &mut buffer.buffer_mut());
        for i in 0..size {
            for channel in 0..unit.outputs() {
                out.push(buffer.buffer_ref().at_f32(channel, i));
            }
        }
    }
    out
}
fn render_room(seq: &mut dyn AudioUnit, processor: &mut dyn AudioUnit, frames: usize) -> Vec<f32> {
    let dry = render(seq, frames);
    let mut output = vec![0.0; processor.outputs()];
    let mut result = Vec::with_capacity(frames * output.len());
    for input in dry.chunks_exact(seq.outputs()) {
        processor.tick(input, &mut output);
        result.extend_from_slice(&output);
    }
    result
}
fn energy(x: &[f32]) -> f64 {
    x.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / x.len() as f64
}

#[test]
fn keyboard_chord_holds_releases_independently_and_preserves_room_tail() {
    let program = evaluate(
        r#"
        local room=send{graph=reverb(12,secs(.2),.65),level=.4}
        local v=voice{graph=function(n)
            local dry=organ_pipe(n.hz,{kind="principal"})*.15 >> pan(0)
            return dry >> to(room,.7)
        end}
        midi(v)
    "#,
    )
    .unwrap();
    let mut runtime = PersistentRuntime::new(
        &program.buses,
        &program.controls,
        program
            .runs
            .iter()
            .map(|run| program.patch(run.patch).unwrap()),
    )
    .unwrap();
    let mut processor = runtime.take_processor();
    processor.set_sample_rate(8000.0);
    let mut seq = runtime.sequencer();
    seq.set_sample_rate(8000.0);
    let template = program.voice(program.midi_bindings[0].voice).unwrap();
    let mut keys = MidiVoices::default();
    let arena = RoutedRuntime::new(runtime.layout(), runtime.controls());
    for key in [60, 64, 67] {
        keys.note_on(
            0,
            key,
            100,
            0.0,
            u64::from(key),
            template,
            &EventRouting::new(),
            &mut seq,
            arena,
        )
        .unwrap();
    }
    let held = render_room(&mut seq, processor.as_mut(), 24_000);
    assert!(
        energy(&held[held.len() - 4000..]) > 1e-5,
        "held beyond known-duration default"
    );
    keys.note_off(0, 64, 3.0, &mut seq);
    assert_eq!(keys.held_count(), 2);
    render_room(&mut seq, processor.as_mut(), 4000);
    keys.all_notes_off(None, 3.5, &mut seq);
    assert_eq!(keys.held_count(), 0);
    let release = render_room(&mut seq, processor.as_mut(), 8000);
    assert!(
        energy(&release[800..1600]) > 1e-10,
        "persistent room continues after the dry gate closes"
    );
    assert!(energy(&release[release.len() - 2000..]) < 1e-12);
    keys.prune(4.5);
    // Released history leaves the budget; a new chord can be played.
    keys.note_on(
        1,
        72,
        127,
        4.5,
        42,
        template,
        &EventRouting::new(),
        &mut seq,
        arena,
    )
    .unwrap();
}

#[test]
fn release_tails_are_retired_at_capacity_and_midi_channels_do_not_alias() {
    let program=evaluate("local v=voice{graph=function(n) return sine(n.hz)*adsr(ms(1),ms(1),1,ms(50)) >> pan(0) end}; midi(v)").unwrap();
    let runtime = PersistentRuntime::new(&program.buses, &program.controls, &[]).unwrap();
    let mut seq = runtime.sequencer();
    let graph = program.voice(program.midi_bindings[0].voice).unwrap();
    let arena = RoutedRuntime::new(runtime.layout(), runtime.controls());
    let mut keys = MidiVoices::new(2);
    for channel in [0, 1] {
        keys.note_on(
            channel,
            60,
            100,
            0.0,
            0,
            graph,
            &EventRouting::new(),
            &mut seq,
            arena,
        )
        .unwrap();
    }
    keys.note_off(0, 60, 0.1, &mut seq);
    assert_eq!(keys.held_count(), 1);
    assert!(
        keys.note_on(
            0,
            62,
            100,
            0.1,
            0,
            graph,
            &EventRouting::new(),
            &mut seq,
            arena
        )
        .is_ok()
    );
    assert_eq!(keys.held_count(), 2);
    assert!(
        keys.note_on(
            0,
            65,
            100,
            0.1,
            0,
            graph,
            &EventRouting::new(),
            &mut seq,
            arena
        )
        .is_err()
    );
    keys.note_off(0, 62, 0.2, &mut seq);
    keys.prune(0.3);
    keys.note_on(
        0,
        62,
        100,
        0.3,
        0,
        graph,
        &EventRouting::new(),
        &mut seq,
        arena,
    )
    .unwrap();
    keys.all_notes_off(Some(1), 0.4, &mut seq);
    assert_eq!(keys.held_count(), 1);
}

#[test]
fn midi_voice_accepts_six_hundred_retriggers_without_dropping_held_keys() {
    let program = evaluate(include_str!("../../test-fixtures/midi-load.eod")).unwrap();
    let graph = program.voice(program.midi_bindings[0].voice).unwrap();
    assert!(
        apteronotus_synth::live_voice_lifetime(graph, &apteronotus_synth::Note::new(220.0))
            .unwrap()
            .gate_tail
            <= 0.111
    );
    let runtime =
        PersistentRuntime::new(&program.buses, &program.controls, std::iter::empty()).unwrap();
    let mut sequencer = runtime.sequencer();
    sequencer.set_sample_rate(48000.0);
    let mut backend = sequencer.backend();
    backend.set_sample_rate(48000.0);
    let mut keys = MidiVoices::default();
    let routing = EventRouting::new();
    let mut now = 0.0;
    for burst in 0..6 {
        for key in 24..124 {
            keys.note_on(
                0,
                key,
                100,
                now,
                burst * 128 + u64::from(key),
                graph,
                &routing,
                &mut sequencer,
                RoutedRuntime::new(runtime.layout(), runtime.controls()),
            )
            .unwrap();
        }
        assert_eq!(keys.held_count(), 100);
        let audio = render(&mut backend, 3840);
        assert!(audio.iter().all(|sample| sample.is_finite()));
        assert!(energy(&audio) > 0.0);
        now += 0.08;
        for key in 24..124 {
            keys.note_off(0, key, now, &mut sequencer);
        }
        assert_eq!(keys.held_count(), 0);
        let audio = render(&mut backend, 3840);
        assert!(audio.iter().all(|sample| sample.is_finite()));
        now += 0.08;
        keys.prune(now);
    }
    let _ = render(&mut backend, 48000);
    keys.prune(now + 1.0);
    assert_eq!(keys.held_count(), 0);
}
