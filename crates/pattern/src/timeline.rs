//! Finite, non-repeating pattern material.
//!
//! A [`Timeline`] is how through-composed material and recorded live events
//! enter the otherwise cyclic pattern algebra. It is still queried with the
//! same pure `query(span) -> events` contract; finiteness is a property of this
//! constructor, not a second query model.

use crate::{Event, EventOrigin, Span, SrcSpan, Value};
use std::collections::HashSet;

/// Identity of one timeline binding within an evaluated program.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TimelineId(u64);

impl TimelineId {
    pub const fn new(binding: u64) -> TimelineId {
        TimelineId(binding)
    }
}

/// One captured or composed event.
///
/// `ordinal` is captured at the input boundary. It is not inferred by sorting:
/// simultaneous live events are legal and their arrival order cannot be
/// reconstructed later.
#[derive(Clone, PartialEq, Debug)]
pub struct TimelineEvent {
    pub whole: Span,
    pub value: Value,
    pub ordinal: u64,
    pub src: Option<SrcSpan>,
}

impl TimelineEvent {
    pub fn new(whole: Span, value: Value, ordinal: u64) -> TimelineEvent {
        TimelineEvent {
            whole,
            value,
            ordinal,
            src: None,
        }
    }

    pub fn at(mut self, src: SrcSpan) -> TimelineEvent {
        self.src = Some(src);
        self
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Timeline {
    id: TimelineId,
    extent: Span,
    events: Vec<TimelineEvent>,
}

impl Timeline {
    pub fn new(
        id: TimelineId,
        extent: Span,
        events: Vec<TimelineEvent>,
    ) -> Result<Timeline, TimelineError> {
        if extent.begin > extent.end {
            return Err(TimelineError::InvalidExtent);
        }
        let mut ordinals = HashSet::with_capacity(events.len());
        for event in &events {
            if event.whole.begin >= event.whole.end
                || event.whole.begin < extent.begin
                || event.whole.end > extent.end
            {
                return Err(TimelineError::EventOutsideExtent {
                    ordinal: event.ordinal,
                });
            }
            if !ordinals.insert(event.ordinal) {
                return Err(TimelineError::DuplicateOrdinal(event.ordinal));
            }
        }
        Ok(Timeline { id, extent, events })
    }

    pub fn extent(&self) -> Span {
        self.extent
    }

    pub fn events(&self) -> &[TimelineEvent] {
        &self.events
    }

    pub(crate) fn query_into(&self, span: Span, out: &mut Vec<Event>) {
        let Some(query) = span.sect(self.extent) else {
            return;
        };
        for event in &self.events {
            if let Some(part) = query.sect(event.whole) {
                out.push(Event {
                    whole: Some(event.whole),
                    part,
                    value: event.value.clone(),
                    src: event.src,
                    origin: EventOrigin::recorded(self.id.0, event.ordinal),
                    group: None,
                });
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimelineError {
    InvalidExtent,
    EventOutsideExtent { ordinal: u64 },
    DuplicateOrdinal(u64),
}

impl core::fmt::Display for TimelineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TimelineError::InvalidExtent => write!(f, "timeline extent runs backwards"),
            TimelineError::EventOutsideExtent { ordinal } => {
                write!(f, "timeline event {ordinal} is empty or outside its extent")
            }
            TimelineError::DuplicateOrdinal(ordinal) => {
                write!(f, "timeline event ordinal {ordinal} is duplicated")
            }
        }
    }
}

impl core::error::Error for TimelineError {}
