use apteronotus_live::{PersistentError, PersistentRuntime};
use apteronotus_synth::{
    AudioInputLayout, AudioInputSpec, BusLayout, ControlLayout, ControlStore, GraphBuilder,
    PatchTemplate, prepare_patch_routed,
};

#[test]
fn wrappers_preserve_prepared_clock_with_external_input_and_silence_fallback() {
    let buses = BusLayout::new(1).unwrap();
    let controls = ControlLayout::new();
    let mut inputs = AudioInputLayout::new();
    inputs
        .add(AudioInputSpec::silence("microphone", 1))
        .unwrap();
    let mut graph = GraphBuilder::new();
    let tone = graph.sine(1000.0);
    let patch = PatchTemplate::new(graph.out_mono(tone).unwrap()).unwrap();
    for rate in [32_000.0, 48_000.0, 96_000.0] {
        for fallback in [true, false] {
            let mut runtime =
                PersistentRuntime::prepare(&buses, &controls, Some(&inputs), [&patch], 0.0, rate)
                    .unwrap();
            let mut processor = if fallback {
                runtime.take_processor()
            } else {
                runtime.take_processor_with_audio_inputs()
            };
            let store = ControlStore::new(&controls);
            let mut reference =
                prepare_patch_routed(&patch, &buses, &store, None, 0.0, rate).unwrap();
            let input = vec![0.0; processor.inputs()];
            for _ in 0..4096 {
                let mut a = [0.0];
                let mut b = [0.0];
                processor.tick(&input, &mut a);
                reference.tick(&[], &mut b);
                // The arena adds an otherwise silent sequencer lane.
                assert!(
                    (a[0] - b[0]).abs() < 1e-7,
                    "rate {rate}, fallback {fallback}"
                );
            }
        }
    }
}

#[test]
fn empty_arena_still_rejects_invalid_preparation_rate() {
    for rate in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            PersistentRuntime::prepare(
                &BusLayout::new(1).unwrap(),
                &ControlLayout::new(),
                None,
                std::iter::empty::<&PatchTemplate>(),
                0.0,
                rate,
            ),
            Err(PersistentError::InvalidPreparationRate(_))
        ));
    }
}
