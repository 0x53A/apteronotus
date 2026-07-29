//! Apteronotus — transport, scheduling and the audio-device edge.
//!
//! The scheduler advances one exact, non-overlapping frontier in cycle time.
//! Pattern queries remain pure; this crate is where repeated scheduling is
//! prevented. cpal is confined to [`output`], while [`scheduler`] can be tested
//! by rendering a fundsp sequencer into memory.

pub mod output;
pub mod persistent;
pub mod revision;
pub mod scheduler;
pub mod tempo;
pub mod transport;
pub mod trigger;

pub use output::{AudioOutput, OutputError};
pub use persistent::{PersistentError, PersistentRuntime};
pub use revision::{Generation, Revision, RevisionSlot, SubmitError};
pub use scheduler::{FillReport, PitchScheduler, ProgramScheduler, ScheduleError, ScheduledTrack};
pub use tempo::{TempoMap, TempoMapError, TempoPoint};
pub use transport::{CycleTime, Transport, TransportError};
pub use trigger::{ExternalTrigger, TriggerRecordError, TriggerRecorder};
