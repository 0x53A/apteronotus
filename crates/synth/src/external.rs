//! Host-injected persistent sources: instruments that live outside this crate.
//!
//! An [`ExternalSource`] is an immutable, host-owned declaration (for example a
//! captured physical-instrument performance) that the graph treats as one
//! persistent, zero-input node. Preparation runs on the control thread when the
//! enclosing patch is prepared; the returned unit is then rendered like any
//! other node. Nothing here performs filesystem access or runs an interpreter
//! on the audio thread; that remains the responsibility of the implementation.
//!
//! This is how out-of-tree experiments (see `apteronotus-research`) put an
//! instrument under test into the normal evaluator, renderer and app without
//! modifying Apteronotus.
use fundsp::prelude32::AudioUnit;
use std::sync::Arc;

/// Immutable declaration of a host-provided persistent source.
pub trait ExternalSource: std::any::Any + Send + Sync + std::fmt::Debug {
    /// Short label used in diagnostics, e.g. `"prepared violin"`.
    fn kind(&self) -> &str;
    /// Output channel count, 1..=16. Must be stable for the declaration.
    fn outputs(&self) -> usize;
    /// Finite activity horizon from transport zero, in seconds. Use 0 when the
    /// source has no authored end (it is then bounded by its run).
    fn horizon_seconds(&self) -> f64 {
        0.0
    }
    /// Immutable payload size counted against graph publication budgets, in
    /// 8-byte data entries.
    fn data_entries(&self) -> usize {
        0
    }
    /// Relative per-sample contact/solver work, counted in `GraphCost::contact_steps`.
    fn contact_steps(&self) -> usize {
        0
    }
    /// Whether `other` declares the same source, so a compatible live edit may
    /// keep the running instance. Identical handles always compare equal; the
    /// default treats distinct declarations as different. Implementations can
    /// compare content via `(other as &dyn Any).downcast_ref::<Self>()`.
    fn equivalent(&self, _other: &dyn ExternalSource) -> bool {
        false
    }
    /// Prepare one concrete unit for `sample_rate`, starting at `transport_seconds`.
    /// The unit must have zero inputs and [`Self::outputs`] outputs.
    fn prepare(&self, sample_rate: f64, transport_seconds: f64)
    -> Result<PreparedExternal, String>;
}

/// Control-thread health view of one prepared external unit.
pub trait ExternalMonitor: Send + Sync + 'static {
    /// `None` while healthy or finished; otherwise a short reason. Sampled on the
    /// host/control thread, never from DSP.
    fn fault(&self) -> Option<String>;
}

/// A prepared unit plus an optional monitor retained by the control store.
pub struct PreparedExternal {
    pub unit: Box<dyn AudioUnit>,
    pub monitor: Option<Arc<dyn ExternalMonitor>>,
}

/// Shared handle to an [`ExternalSource`]; equal if identical or [`ExternalSource::equivalent`].
#[derive(Clone)]
pub struct External(Arc<dyn ExternalSource>);
impl External {
    pub fn new(source: impl ExternalSource) -> Self {
        Self(Arc::new(source))
    }
    pub fn from_arc(source: Arc<dyn ExternalSource>) -> Self {
        Self(source)
    }
    pub fn source(&self) -> &dyn ExternalSource {
        &*self.0
    }
    pub(crate) fn valid(&self) -> bool {
        let h = self.0.horizon_seconds();
        (1..=16).contains(&self.0.outputs()) && h.is_finite() && h >= 0.0
    }
}
impl PartialEq for External {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.equivalent(&*other.0)
    }
}
impl std::fmt::Debug for External {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// First failed external instance, in persistent node preparation order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalFault {
    /// Zero-based ordinal among monitored external units of one control store.
    pub instance: usize,
    pub kind: String,
    pub reason: String,
}
impl std::fmt::Display for ExternalFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} instance {}: {}",
            self.kind,
            self.instance + 1,
            self.reason
        )
    }
}
impl std::error::Error for ExternalFault {}
