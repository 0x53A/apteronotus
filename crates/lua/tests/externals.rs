//! Host-registered external sources reached from Lua with `external(name)`.
use apteronotus_lua::{Evaluator, ExternalRegistry, Limits};
use apteronotus_synth::{External, ExternalSource, Op, PreparedExternal};

#[derive(Debug, PartialEq)]
struct Stub {
    gain: u32,
}
impl ExternalSource for Stub {
    fn kind(&self) -> &str {
        "stub"
    }
    fn outputs(&self) -> usize {
        2
    }
    fn horizon_seconds(&self) -> f64 {
        1.0
    }
    fn data_entries(&self) -> usize {
        64
    }
    fn equivalent(&self, other: &dyn ExternalSource) -> bool {
        (other as &dyn std::any::Any).downcast_ref::<Self>() == Some(self)
    }
    fn prepare(&self, _: f64, _: f64) -> Result<PreparedExternal, String> {
        Err("not rendered in this test".into())
    }
}
fn registry(gain: u32) -> ExternalRegistry {
    let mut r = ExternalRegistry::new(2, 4096);
    r.register("solo", External::new(Stub { gain })).unwrap();
    r
}

#[test]
fn program_owns_the_resolved_declaration_after_registry_drop() {
    let program = {
        let r = registry(1);
        Evaluator::default()
            .evaluate_with_externals("run(external('solo'))", &r)
            .unwrap()
    };
    assert_eq!(program.runs.len(), 1);
    assert!(program.runs[0].span.is_none());
    let patch = program.patch(program.runs[0].patch).unwrap();
    assert!(
        matches!(&patch.graph().nodes[0].op, Op::External(found) if found.source().kind() == "stub")
    );
    assert_eq!(patch.graph().channels(), 2);
}

#[test]
fn names_have_no_implicit_resolution_and_placement_is_persistent() {
    let r = registry(1);
    for source in [
        "run(external('missing'))",
        "run(external('/tmp/violin'))",
        "external()",
        "external(42)",
        "external('solo',{})",
        "voice{graph=function() return external('solo') end}",
    ] {
        assert!(
            Evaluator::default()
                .evaluate_with_externals(source, &r)
                .is_err(),
            "{source}"
        );
    }
    assert!(
        Evaluator::default()
            .evaluate("run(external('solo'))")
            .unwrap_err()
            .to_string()
            .contains("not registered")
    );
}

#[test]
fn registry_snapshot_and_publication_budgets_are_independent() {
    let mut r = registry(1);
    let snapshot = r.clone();
    assert!(r.register("solo", External::new(Stub { gain: 1 })).is_err());
    r.register("second", External::new(Stub { gain: 1 }))
        .unwrap();
    assert!(snapshot.get("second").is_none());
    assert!(
        r.register("third", External::new(Stub { gain: 1 }))
            .is_err()
    );
    assert_eq!((r.len(), r.payload_bytes()), (2, 1024));
    let mut too_small = ExternalRegistry::new(1, 0);
    assert!(
        too_small
            .register("solo", External::new(Stub { gain: 1 }))
            .is_err()
    );
    assert!(too_small.is_empty());
    let mut limits = Limits::default();
    limits.graph_publication.data_entries = 0;
    assert!(
        Evaluator::new(limits)
            .evaluate_with_externals("run(external('solo'))", &snapshot)
            .is_err()
    );
}

#[test]
fn compatible_edits_use_declared_equivalence() {
    let source = "run(external('solo'))";
    let active = Evaluator::default()
        .evaluate_with_externals(source, &registry(1))
        .unwrap();
    let mut same = Evaluator::default()
        .evaluate_with_externals(source, &registry(1))
        .unwrap();
    assert!(same.reuse_persistent_from(&active));
    let mut changed = Evaluator::default()
        .evaluate_with_externals(source, &registry(2))
        .unwrap();
    assert!(!changed.reuse_persistent_from(&active));
}
