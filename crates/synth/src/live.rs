//! Runtime note-off coordinates for held voices. Templates remain immutable data.
use crate::template::{CurveClock, Implicit, InitExpr, InitScalar, Op, ParamId, Source};
use crate::{GraphTemplate, Lifetime, LowerError, Note};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// One held note's release coordinate, in seconds since its audio onset.
/// Clones address the same gate. Releasing is idempotent: the earliest release
/// wins, including when a host schedules a future release before rendering it.
#[derive(Clone, Debug)]
pub struct LiveGate(Arc<AtomicU64>);

impl Default for LiveGate {
    fn default() -> Self {
        Self::new()
    }
}
impl LiveGate {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(f64::INFINITY.to_bits())))
    }
    pub fn release_after(&self, seconds: f64) -> Result<(), LowerError> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(LowerError::InvalidLiveRelease(seconds));
        }
        self.0
            .fetch_min(seconds.max(0.0).to_bits(), Ordering::Relaxed);
        Ok(())
    }
    pub fn release_seconds(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Reject computations requiring the duration of a note that is still held.
/// ADSR release is the explicit live boundary; note-seconds curves remain valid.
pub fn validate_live_voice(template: &GraphTemplate) -> Result<(), LowerError> {
    template.validate()?;
    if template.inputs != 0 {
        return Err(LowerError::LiveInputsUnsupported);
    }
    fn param(id: ParamId) -> bool {
        id == ParamId::Implicit(Implicit::Duration)
    }
    fn expr(e: &InitExpr) -> bool {
        match e {
            InitExpr::Param(id) => param(*id),
            InitExpr::Add(a, b)
            | InitExpr::Sub(a, b)
            | InitExpr::Mul(a, b)
            | InitExpr::Div(a, b) => expr(a) || expr(b),
            InitExpr::Neg(e) => expr(e),
            InitExpr::Const(_) => false,
        }
    }
    fn scalar(s: InitScalar) -> bool {
        matches!(s, InitScalar::Param(id) if param(id))
    }
    fn source(s: Source) -> bool {
        matches!(s, Source::Param(id) if param(id))
    }
    let invalid_sources = template.outputs.iter().copied().any(source)
        || template
            .sends
            .iter()
            .flat_map(|s| &s.outputs)
            .any(|i| source(i.source));
    if invalid_sources {
        return Err(LowerError::LiveDurationRequired);
    }
    for node in &template.nodes {
        if node.inputs.iter().any(|i| source(i.source)) {
            return Err(LowerError::LiveDurationRequired);
        }
        let requires_duration = match &node.op {
            Op::Curve(curve) => curve.clock == CurveClock::NotePhase,
            Op::InitRandom { min, max, .. } => expr(min) || expr(max),
            Op::Pluck {
                frequency, damping, ..
            } => scalar(*frequency) || scalar(*damping),
            Op::Slew { response_time, .. } => scalar(*response_time),
            Op::Portamento {
                target,
                response_time,
            } => scalar(*target) || scalar(*response_time),
            Op::BreakpointCurve { times, horizon, .. } => {
                times.iter().any(expr)
                    || !matches!(horizon, crate::template::BreakpointHorizon::Absolute(_))
            }
            _ => false,
        };
        if requires_duration {
            return Err(LowerError::LiveDurationRequired);
        }
    }
    Ok(())
}

pub(crate) fn release_delay(template: &GraphTemplate) -> f64 {
    template
        .nodes
        .iter()
        .filter_map(|n| match &n.op {
            Op::Adsr(a) => Some(a.release),
            _ => None,
        })
        .fold(0.0, f64::max)
}

// Prove terminal zero from graph data, without observing audio samples.
// Conservative for other operators: a biased sum, raw source, or stateful
// response retains the existing late safety-fade deadline.
fn release_bounded_outputs(template: &GraphTemplate) -> bool {
    let mut bounded = vec![false; template.nodes.len()];
    for (index, node) in template.nodes.iter().enumerate() {
        let source_is_bounded = |source: Source| match source {
            Source::Const(value) => value == 0.0,
            Source::Port { node, .. } => bounded[node],
            _ => false,
        };
        bounded[index] = match node.op {
            Op::Adsr(_) => true,
            Op::Mul => node
                .inputs
                .iter()
                .any(|input| source_is_bounded(input.source)),
            Op::Add | Op::Sub => node
                .inputs
                .iter()
                .all(|input| source_is_bounded(input.source)),
            Op::Div => {
                source_is_bounded(node.inputs[0].source)
                    && matches!(node.inputs[1].source, Source::Const(value) if value.is_finite() && value != 0.0)
            }
            Op::Neg | Op::Pan => source_is_bounded(node.inputs[0].source),
            _ => false,
        };
    }
    template
        .outputs
        .iter()
        .copied()
        .chain(
            template
                .sends
                .iter()
                .flat_map(|send| send.outputs.iter().map(|input| input.source)),
        )
        .all(|source| match source {
            Source::Const(value) => value == 0.0,
            Source::Port { node, .. } => bounded[node],
            _ => false,
        })
}

/// Conservative removal deadline for a live voice. Nonterminating sources
/// fade over 5ms after every ADSR has completed, then downstream DSP drains.
/// The scheduler uses `end_after_onset(actual_note_off_seconds)` only at release.
pub fn live_voice_lifetime(template: &GraphTemplate, note: &Note) -> Result<Lifetime, LowerError> {
    validate_live_voice(template)?;
    let mut lifetime = template.lifetime_for(note)?;
    if !release_bounded_outputs(template) {
        lifetime.gate_tail += release_delay(template) + 0.005;
    }
    Ok(lifetime)
}
