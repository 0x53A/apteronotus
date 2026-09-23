use apteronotus_synth::{Adsr, GraphBuilder, n};

#[test]
fn harmonic_data_and_key_envelopes_are_validated_before_publication() {
    for amplitudes in [
        vec![],
        vec![0.0; 33],
        vec![f64::NAN],
        vec![f64::INFINITY],
        vec![0.8, -0.3],
    ] {
        let mut graph = GraphBuilder::new();
        let source = graph.harmonics(n::HZ, amplitudes);
        assert!(graph.out_panned(source).is_err());
    }
    for envelope in [
        Adsr::new(-1.0, 0.0, 1.0, 0.1),
        Adsr::new(0.01, f64::NAN, 1.0, 0.1),
        Adsr::new(0.01, 0.0, 1.1, 0.1),
        Adsr::new(0.01, 0.0, 1.0, -0.1),
    ] {
        let mut graph = GraphBuilder::new();
        let source = graph.adsr(envelope);
        assert!(graph.out_panned(source).is_err());
    }
}
