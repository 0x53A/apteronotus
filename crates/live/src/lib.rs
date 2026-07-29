//! Apteronotus — transport, scheduling and the audio-device edge.
//!
//! The scheduler advances one exact, non-overlapping frontier in cycle time.
//! Pattern queries remain pure; this crate is where repeated scheduling is
//! prevented. cpal is confined to [`output`], while [`scheduler`] can be tested
//! by rendering a fundsp sequencer into memory.

mod capture;
pub mod output;
pub mod persistent;
pub mod revision;
pub mod scheduler;
pub mod trigger;

pub use apteronotus_transport::{
    CycleTime, TempoMap, TempoMapError, TempoPoint, Transport, TransportError,
};
pub use output::{AudioOutput, InputBinding, MasterGain, OutputError};
pub use persistent::{PersistentError, PersistentRuntime};
pub use revision::{Generation, Revision, RevisionSlot, SubmitError};
pub use scheduler::{
    ExternalOnset, FillReport, PitchScheduler, ProgramScheduler, RoutedRuntime, ScheduleError,
    ScheduledRun, ScheduledTrack, schedule_external_routed,
};
pub use trigger::{ExternalTrigger, TriggerRecordError, TriggerRecorder};
