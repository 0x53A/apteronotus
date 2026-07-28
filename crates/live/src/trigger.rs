//! The boundary between non-queryable live events and recorded pattern data.

use apteronotus_pattern::{Frac, Span, Timeline, TimelineError, TimelineEvent, TimelineId, Value};

/// One event arriving from the outside world.
///
/// There is deliberately no `query` method: a live stream cannot answer what
/// will happen inside the scheduler's lookahead window.
#[derive(Clone, PartialEq, Debug)]
pub struct ExternalTrigger {
    pub at: Frac,
    pub duration: Frac,
    pub value: Value,
}

impl ExternalTrigger {
    pub fn new(at: Frac, duration: Frac, value: Value) -> ExternalTrigger {
        ExternalTrigger {
            at,
            duration,
            value,
        }
    }
}

/// Captures arrival order so simultaneous events retain distinct identities.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct TriggerRecorder {
    next_ordinal: u64,
    events: Vec<TimelineEvent>,
}

impl TriggerRecorder {
    pub fn new() -> TriggerRecorder {
        TriggerRecorder::default()
    }

    pub fn record(&mut self, trigger: ExternalTrigger) -> Result<u64, TriggerRecordError> {
        if trigger.duration <= Frac::ZERO {
            return Err(TriggerRecordError::InvalidDuration);
        }
        let ordinal = self.next_ordinal;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or(TriggerRecordError::OrdinalExhausted)?;
        self.events.push(TimelineEvent::new(
            Span::new(trigger.at, trigger.at + trigger.duration),
            trigger.value,
            ordinal,
        ));
        Ok(ordinal)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Snapshot the recording without stopping it.
    pub fn snapshot(&self, id: TimelineId, extent: Span) -> Result<Timeline, TimelineError> {
        Timeline::new(id, extent, self.events.clone())
    }

    /// Finish and transfer the captured events into immutable pattern data.
    pub fn finish(self, id: TimelineId, extent: Span) -> Result<Timeline, TimelineError> {
        Timeline::new(id, extent, self.events)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TriggerRecordError {
    InvalidDuration,
    OrdinalExhausted,
}

impl core::fmt::Display for TriggerRecordError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TriggerRecordError::InvalidDuration => {
                write!(f, "external trigger duration must be greater than zero")
            }
            TriggerRecordError::OrdinalExhausted => {
                write!(f, "external trigger ordinal space is exhausted")
            }
        }
    }
}

impl core::error::Error for TriggerRecordError {}
