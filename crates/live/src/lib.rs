//! Apteronotus — transport, scheduling and the audio-device edge.
//!
//! The scheduler advances one exact, non-overlapping frontier in cycle time.
//! Pattern queries remain pure; this crate is where repeated scheduling is
//! prevented. cpal is confined to [`output`], while [`scheduler`] can be tested
//! by rendering a fundsp sequencer into memory.

pub mod output;
pub mod scheduler;
pub mod transport;

pub use output::{AudioOutput, OutputError};
pub use scheduler::{FillReport, PitchScheduler, ScheduleError};
pub use transport::{Transport, TransportError};
