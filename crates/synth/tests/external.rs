//! Host-injected external sources: declaration, preparation and monitoring.
#![recursion_limit = "256"]
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use apteronotus_synth::{
    BusLayout, ControlLayout, ControlStore, External, ExternalMonitor, ExternalSource, GraphBuilder,
    LowerError, Note, PatchTemplate, PreparedExternal, instantiate, instantiate_patch_routed_at,
    prepare_patch_routed,
};
use fundsp::prelude32::{AudioUnit, Net, dc};

#[derive(Debug)]
struct Tone { failed: Arc<AtomicBool>, channels: usize }
struct Health(Arc<AtomicBool>);
impl ExternalMonitor for Health {
    fn fault(&self) -> Option<String> { self.0.load(Ordering::Relaxed).then(|| "latched silent".into()) }
}
impl ExternalSource for Tone {
    fn kind(&self) -> &str { "test tone" }
    fn outputs(&self) -> usize { self.channels }
    fn horizon_seconds(&self) -> f64 { 0.5 }
    fn data_entries(&self) -> usize { 7 }
    fn prepare(&self, rate: f64, transport: f64) -> Result<PreparedExternal, String> {
        if transport != 0. { return Err("requires transport zero".into()); }
        if rate != 48_000. { return Err("clock differs".into()); }
        let mut net = Net::new(0, self.channels);
        for (channel, value) in [0.25f32, -0.25].into_iter().take(self.channels).enumerate() {
            let id = net.push(Box::new(dc(value)));
            net.connect_output(id, 0, channel);
        }
        let unit: Box<dyn AudioUnit> = Box::new(net);
        Ok(PreparedExternal { unit, monitor: Some(Arc::new(Health(self.failed.clone()))) })
    }
}

fn graph(channels: usize, failed: Arc<AtomicBool>) -> apteronotus_synth::GraphTemplate {
    let mut builder = GraphBuilder::new();
    let outputs = builder.external(External::new(Tone { failed, channels }));
    assert_eq!(outputs.len(), channels);
    builder.out(&outputs).unwrap()
}

#[test]
fn declaration_carries_horizon_and_budget_and_rejects_voice_paths() {
    let graph = graph(2, Arc::default());
    assert_eq!(graph.lifetime().absolute_horizon, 0.5);
    assert!(matches!(instantiate(&graph, &Note::new(440.)), Err(LowerError::PreparedPatchRequired)));
    let patch = PatchTemplate::new(graph).unwrap();
    let buses = BusLayout::new(2).unwrap();
    let controls = ControlStore::new(&ControlLayout::new());
    assert!(matches!(instantiate_patch_routed_at(&patch, &buses, &controls, 0.), Err(LowerError::PreparedPatchRequired)));
}

#[test]
fn preparation_errors_register_no_monitor_and_faults_are_reported() {
    let failed = Arc::new(AtomicBool::new(false));
    let patch = PatchTemplate::new(graph(2, failed.clone())).unwrap();
    let buses = BusLayout::new(2).unwrap();
    let controls = ControlStore::new(&ControlLayout::new());
    for (transport, rate, expected) in [(0., 44_100., "clock differs"), (1., 48_000., "transport zero")] {
        let error = prepare_patch_routed(&patch, &buses, &controls, None, transport, rate).err().unwrap();
        assert!(error.to_string().contains(expected), "{error}");
        assert_eq!(controls.external_monitor_count(), 0);
    }
    let mut unit = prepare_patch_routed(&patch, &buses, &controls, None, 0., 48_000.).unwrap();
    assert_eq!(controls.external_monitor_count(), 1);
    assert_eq!(controls.external_fault(), None);
    let mut out = [0.0f32; 2];
    unit.tick(&[], &mut out);
    assert_eq!(out, [0.25, -0.25]);
    failed.store(true, Ordering::Relaxed);
    let fault = controls.external_fault().unwrap();
    assert_eq!((fault.instance, fault.kind.as_str()), (0, "test tone"));
    assert!(fault.to_string().contains("instance 1: latched silent"));
}

#[derive(Debug, PartialEq)]
struct Level(u32);
impl ExternalSource for Level {
    fn kind(&self) -> &str { "level" }
    fn outputs(&self) -> usize { 1 }
    fn equivalent(&self, other: &dyn ExternalSource) -> bool {
        (other as &dyn std::any::Any).downcast_ref::<Self>() == Some(self)
    }
    fn prepare(&self, _: f64, _: f64) -> Result<PreparedExternal, String> {
        Ok(PreparedExternal { unit: Box::new(dc(self.0 as f32)), monitor: None })
    }
}

#[test]
fn equality_is_identity_or_declared_equivalence() {
    let a = External::new(Level(1));
    assert_eq!(a, a.clone());
    assert_eq!(a, External::new(Level(1)));
    assert_ne!(a, External::new(Level(2)));
    let tone = External::new(Tone { failed: Arc::default(), channels: 1 });
    assert_ne!(tone, External::new(Tone { failed: Arc::default(), channels: 1 }));
    assert_ne!(a, tone);
}
