//! Native callback elapsed-time observations. These measure render work plus
//! any preemption inside that work, not device underruns or callback wake jitter.
use std::sync::atomic::{AtomicU64,Ordering};

/// Cumulative observations since this output stream was prepared. Concurrent
/// reads are approximate (independent monotonic counters), not a clock snapshot.
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct OutputWorkload {
    pub callbacks:u64,
    pub frames:u64,
    pub elapsed_seconds:f64,
    /// Total measured callback time divided by total requested audio duration.
    pub mean_ratio:f64,
    /// Largest single-callback elapsed time / requested audio duration.
    pub peak_ratio:f64,
    pub over_budget_callbacks:u64,
}
#[derive(Default)]
pub(crate) struct WorkloadCounters {
    callbacks:AtomicU64,frames:AtomicU64,nanos:AtomicU64,peak:AtomicU64,over:AtomicU64,
}
impl WorkloadCounters {
    #[cfg(not(target_arch="wasm32"))]
    pub(crate) fn measure<T>(&self,frames:usize,rate:f64,render:impl FnOnce()->T)->T {
        let started=std::time::Instant::now();let result=render();
        self.record(frames,rate,started.elapsed());result
    }
    #[cfg(any(not(target_arch="wasm32"),test))]
    pub(crate) fn record(&self,frames:usize,rate:f64,elapsed:std::time::Duration) {
        if frames==0 {return;}
        let nanos=elapsed.as_nanos().min(u64::MAX as u128) as u64;
        let ratio=nanos as f64*rate/(frames as f64*1e9);
        self.frames.fetch_add(frames as u64,Ordering::Relaxed);
        self.nanos.fetch_add(nanos,Ordering::Relaxed);
        // Nonnegative finite f64 bit order is numeric order.
        self.peak.fetch_max(ratio.to_bits(),Ordering::Relaxed);
        if ratio>1. {self.over.fetch_add(1,Ordering::Relaxed);}
        self.callbacks.fetch_add(1,Ordering::Release);
    }
    pub(crate) fn snapshot(&self,rate:f64)->Option<OutputWorkload> {
        let callbacks=self.callbacks.load(Ordering::Acquire);
        if callbacks==0 {return None;}
        let frames=self.frames.load(Ordering::Relaxed);
        let elapsed_seconds=self.nanos.load(Ordering::Relaxed) as f64*1e-9;
        Some(OutputWorkload{callbacks,frames,elapsed_seconds,mean_ratio:elapsed_seconds*rate/frames as f64,
            peak_ratio:f64::from_bits(self.peak.load(Ordering::Relaxed)),over_budget_callbacks:self.over.load(Ordering::Relaxed)})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn duration_budget_handles_variable_callbacks_and_exact_boundary() {
        let c=WorkloadCounters::default();assert!(c.snapshot(48000.).is_none());
        c.record(0,48000.,Duration::from_secs(1));assert!(c.snapshot(48000.).is_none());
        c.record(480,48000.,Duration::from_millis(5));
        c.record(960,48000.,Duration::from_millis(20));
        c.record(240,48000.,Duration::from_millis(10));
        let v=c.snapshot(48000.).unwrap();assert_eq!(v.callbacks,3);assert_eq!(v.frames,1680);
        assert!((v.mean_ratio-1.).abs()<1e-12);assert_eq!(v.peak_ratio,2.);assert_eq!(v.over_budget_callbacks,1);
    }
    #[test]
    fn histories_are_stream_local_and_survive_observer_reads() {
        let a=WorkloadCounters::default();let b=WorkloadCounters::default();
        a.record(441,44100.,Duration::from_millis(15));
        assert_eq!(a.snapshot(44100.),a.snapshot(44100.));assert!(b.snapshot(44100.).is_none());
        assert_eq!(a.snapshot(44100.).unwrap().peak_ratio,1.5);
    }
}
