//! Graph compositions that do not deserve Rust DSP primitives.
//!
//! These functions build ordinary [`GraphTemplate`](crate::GraphTemplate)
//! nodes. They are Rust sketches of the readable scripting-language stdlib
//! that will eventually expose the same compositions.

use crate::{GraphBuilder, Source};

/// Feed `audio` into a second-order resonator centered at `hz`.
///
/// The requested decay is the settling-time convention used by the songs,
/// `t_se ≈ 3 / (ζω₀)`. Since `Q = 1 / (2ζ)`, the band-pass Q is
/// `π · hz · decay / 3`. `ring` is therefore composition—multiply plus the
/// existing modulatable band-pass—not a new backend primitive.
///
/// A decay may be a literal, a declared parameter, or arithmetic over those.
/// The audio graph receives the expression itself while tail metadata receives
/// its construction-time upper bound. Runtime signals are rejected: sampling
/// audio or a curve to decide when a voice dies would cross rate boundaries.
pub fn ring(
    graph: &mut GraphBuilder,
    audio: Source,
    hz: impl Into<Source>,
    decay_seconds: impl Into<Source>,
) -> Result<Source, RingError> {
    let decay_seconds = decay_seconds.into();
    let bounds = graph
        .bounds(decay_seconds)
        .ok_or(RingError::UnboundedDecay)?;
    if bounds.min <= 0.0 {
        return Err(RingError::NonPositiveDecay {
            minimum: bounds.min,
        });
    }

    let hz = hz.into();
    let hz_times_decay = graph.mul(hz, decay_seconds);
    let q = graph.mul(hz_times_decay, core::f64::consts::PI / 3.0);
    let resonator = graph.bandpass(audio, hz, q);
    graph.set_tail(resonator, bounds.max);
    Ok(resonator)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RingError {
    UnboundedDecay,
    NonPositiveDecay { minimum: f64 },
}

impl core::fmt::Display for RingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RingError::UnboundedDecay => write!(
                f,
                "ring decay must be a bounded scalar expression, not a runtime signal"
            ),
            RingError::NonPositiveDecay { minimum } => {
                write!(f, "ring decay can reach non-positive value {minimum}")
            }
        }
    }
}

impl core::error::Error for RingError {}
