//! A host-injected external source renders through the normal offline path, and
//! its monitored fault stops the render with the block that observed it.
#![recursion_limit = "256"]
use apteronotus_lua::{Evaluator, ExternalRegistry};
use apteronotus_render::{RenderError, RenderOptions, RenderSpan, render};
use apteronotus_synth::{External, ExternalMonitor, ExternalSource, PreparedExternal};
use fundsp::prelude32::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Constant output that reports a fault once it has rendered `fail_after` frames.
#[derive(Debug)]
struct Faulty {
    fail_after: u64,
}
struct Counter(Arc<AtomicU64>, u64);
impl ExternalMonitor for Counter {
    fn fault(&self) -> Option<String> {
        (self.0.load(Ordering::Relaxed) >= self.1).then(|| "latched silent".into())
    }
}
impl ExternalSource for Faulty {
    fn kind(&self) -> &str {
        "faulty"
    }
    fn outputs(&self) -> usize {
        1
    }
    fn horizon_seconds(&self) -> f64 {
        1.0
    }
    fn prepare(&self, _: f64, _: f64) -> Result<PreparedExternal, String> {
        let frames = Arc::new(AtomicU64::new(0));
        let counter = frames.clone();
        let unit = dc(0.5f32)
            >> map(move |i: &Frame<f32, U1>| {
                counter.fetch_add(1, Ordering::Relaxed);
                i[0]
            });
        Ok(PreparedExternal {
            unit: Box::new(unit),
            monitor: Some(Arc::new(Counter(frames, self.fail_after))),
        })
    }
}
fn program(fail_after: u64) -> apteronotus_lua::Program {
    let mut registry = ExternalRegistry::new(1, 0);
    registry
        .register("under_test", External::new(Faulty { fail_after }))
        .unwrap();
    Evaluator::default()
        .evaluate_with_externals("run(external('under_test'))", &registry)
        .unwrap()
}

#[test]
fn external_source_renders_and_faults_propagate() {
    let options = RenderOptions {
        sample_rate: 48_000.,
        span: RenderSpan::Seconds(0.25),
        tail_seconds: 0.,
        ..Default::default()
    };
    let audio = render(&program(u64::MAX), &options).unwrap();
    assert!(audio.samples.iter().any(|sample| *sample != 0.));
    match render(&program(6_000), &options) {
        Err(RenderError::External { frame_end, source }) => {
            assert!(frame_end >= 6_000 && frame_end < 12_000, "{frame_end}");
            assert_eq!((source.instance, source.kind.as_str()), (0, "faulty"));
        }
        other => panic!("expected external fault, got {other:?}"),
    }
}
