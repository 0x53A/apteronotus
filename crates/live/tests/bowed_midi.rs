use apteronotus_live::{BowedKeyboard, MidiMessage, PersistentRuntime};
use apteronotus_lua::evaluate;

#[test]
fn keyboard_uses_production_retained_path_and_last_key_priority() {
    let p = evaluate(r#"local v=violin {string="D",mode="live",model="c3"}; midi(v); run(v)"#).unwrap();
    let mut runtime = PersistentRuntime::new(
        &p.buses,
        &p.controls,
        p.runs.iter().map(|r| p.patch(r.patch).unwrap()),
    )
    .unwrap();
    let mut unit = runtime.take_processor();
    unit.set_sample_rate(24_000.0);
    let b = &p.midi_violins[0];
    let handle = runtime.controls().bowed_handle(b.key).unwrap();
    let mut keys = BowedKeyboard::default();
    let send = |keys: &mut BowedKeyboard, message| {
        keys.message(
            message,
            &handle,
            handle.frontier_seconds() + 0.005,
            b.channel,
            b.min_key,
            b.max_key,
        )
        .unwrap()
    };
    send(
        &mut keys,
        MidiMessage::NoteOn {
            channel: 0,
            key: 62,
            velocity: 100,
        },
    );
    let mut output = vec![0.0; unit.outputs()];
    let input = vec![0.0; unit.inputs()];
    let mut energy = 0.0;
    for _ in 0..12000 {
        unit.tick(&input, &mut output);
        energy += f64::from(output[0]).powi(2);
    }
    assert!(energy > 0.001, "retained violin is silent");
    send(
        &mut keys,
        MidiMessage::NoteOn {
            channel: 0,
            key: 64,
            velocity: 90,
        },
    );
    assert_eq!(keys.held_count(), 2);
    for _ in 0..1200 {
        unit.tick(&input, &mut output);
    }
    send(
        &mut keys,
        MidiMessage::NoteOff {
            channel: 0,
            key: 64,
        },
    );
    assert_eq!(keys.held_count(), 1);
    for _ in 0..1200 {
        unit.tick(&input, &mut output);
    }
    send(
        &mut keys,
        MidiMessage::NoteOff {
            channel: 0,
            key: 62,
        },
    );
    for _ in 0..240000 {
        unit.tick(&input, &mut output);
    }
    assert!(output.iter().all(|v| v.abs() < 1e-6));
    assert_eq!(handle.diagnostics().guards, 0);
    assert_eq!(handle.diagnostics().rejected, 0);
    assert_eq!(handle.diagnostics().late, 0);
}
