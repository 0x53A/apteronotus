//! Pure transport clocks shared by authored programs and live scheduling.
//!
//! This crate is deliberately below both Lua and the audio-device edge. A
//! [`TempoMap`] is owned program data; converting it into scheduler seconds
//! must not require linking cpal, and the language must not invent a parallel
//! tempo representation merely to remain portable.

mod tempo;
mod transport;

pub use tempo::{TempoMap, TempoMapError, TempoPoint};
pub use transport::{CycleTime, Transport, TransportError};
