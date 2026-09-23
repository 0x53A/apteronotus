use apteronotus_synth::{
    Adsr, BusLayout, ControlLayout, ControlStore, GraphBuilder, GraphLimitError, GraphLimits,
    PatchTemplate, instantiate_timed_patch_routed,
    lower::{render, rms},
    n,
};

#[test]
fn physical_state_is_budgeted_and_only_pressure_extends_its_lifetime() {
    for min_hz in [0.0, 19.0, 1001.0, f64::NAN, f64::INFINITY] {
        let mut graph = GraphBuilder::new();
        let pipe = graph.flue_pipe(110.0, 0.85, 0.0, min_hz);
        assert!(graph.out_mono(pipe).is_err());
    }
    let mut graph = GraphBuilder::new();
    let pressure = graph.adsr(Adsr::new(0.02, 0.0, 1.0, 0.08));
    let irrelevant = graph.adsr(Adsr::new(0.0, 0.0, 1.0, 20.0));
    let pipe = graph.flue_pipe(n::HZ, pressure, irrelevant, 40.0);
    let graph = graph.out_mono(pipe).unwrap();
    assert!((graph.tail() - (0.08 + 0.08 + 64.0 / 40.0)).abs() < 1e-12);
    let cost = graph.cost();
    assert!((cost.delay_buffer_seconds - 16.16 / 40.0).abs() < 1e-12);
    let limits = GraphLimits {
        nodes: 100,
        connections: 100,
        input_channels: 0,
        output_channels: 1,
        data_entries: 100,
        delay_buffer_seconds: cost.delay_buffer_seconds / 2.0,
        tail_seconds: 30.0,
    };
    assert!(matches!(
        graph.validate_limits(limits),
        Err(GraphLimitError::DelayBuffer { .. })
    ));
}

#[test]
fn finite_patch_closes_wind_before_the_pipe_so_stored_energy_can_drain() {
    let mut graph = GraphBuilder::new();
    let pipe = graph.flue_pipe(110.0, 0.85, 0.0, 40.0);
    let patch = PatchTemplate::new(graph.out_mono(pipe).unwrap()).unwrap();
    let controls = ControlStore::new(&ControlLayout::new());
    let layout = BusLayout::new(1).unwrap();
    let (mut unit, lifetime) =
        instantiate_timed_patch_routed(&patch, 1.0, 0.0, &layout, &controls).unwrap();
    assert!(lifetime.end_after_onset(1.0) > 2.0);
    let audio = render(unit.as_mut(), 24_000.0, 3.0);
    assert!(rms(&audio[0][12_000..23_000]) > 0.05);
    assert!(
        rms(&audio[0][24_000..24_120]) > 0.01,
        "must retain bore motion at closure"
    );
    assert!(
        rms(&audio[0][48_000..]) < 1e-8,
        "constant pressure must not bypass lifecycle gating"
    );
}
