//! The first playable bridge: pitch pattern → instantiated voice → sequencer.
//!
//! This is intentionally one narrow path, not the future score model. It gives
//! one [`GraphTemplate`] every onset from a pattern of note names or MIDI
//! numbers. Instrument selection, control maps, generations and activation at
//! musical boundaries come later, after their shapes are settled.

use crate::transport::{Transport, TransportError};
use apteronotus_music::{Pitch, PitchError};
use apteronotus_pattern::{Frac, Pattern, Span, Value};
use apteronotus_synth::{GraphTemplate, Note, TemplateError, instantiate};
use fundsp::prelude32::{AudioUnit, Fade, ReplayMode, Sequencer};

/// A scheduler owns exactly one frontier. Windows it submits are adjacent and
/// therefore cannot duplicate an onset even when callers poll irregularly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PitchScheduler {
    frontier: Frac,
}

impl PitchScheduler {
    pub fn new(start: Frac) -> PitchScheduler {
        PitchScheduler { frontier: start }
    }

    pub fn frontier(&self) -> Frac {
        self.frontier
    }

    /// Fill the half-open window `[frontier, end)` into `sequencer`.
    ///
    /// All fallible work happens before the first push and the frontier moves
    /// only after every voice is accepted. A bad note or template therefore
    /// leaves both the old frontier and the sequencer untouched.
    pub fn fill_to(
        &mut self,
        end: Frac,
        pattern: &Pattern,
        template: &GraphTemplate,
        transport: Transport,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        let begin = self.frontier;
        if end <= begin {
            return Ok(FillReport {
                span: Span::new(begin, begin),
                voices: 0,
            });
        }

        template.validate().map_err(ScheduleError::Template)?;
        if sequencer.inputs() != 0 || sequencer.outputs() != template.channels() {
            return Err(ScheduleError::ChannelMismatch {
                expected: template.channels(),
                found: sequencer.outputs(),
            });
        }

        let span = Span::new(begin, end);
        let tail = template.tail();
        let mut pending = Vec::new();
        for event in pattern.onsets(span) {
            let whole = event.whole.expect("onsets always have a whole span");
            let pitch = pitch_from_value(&event.value)?;
            let hz = pitch.hz();
            if !hz.is_finite() || hz <= 0.0 {
                return Err(ScheduleError::InvalidFrequency(hz));
            }
            let start = transport.cycle_to_seconds(whole.begin);
            let gate = transport.duration_to_seconds(whole.length());
            let note = Note::new(hz).duration(gate);
            let unit = instantiate(template, &note).map_err(ScheduleError::Template)?;
            pending.push((start, start + gate + tail, unit));
        }

        let voices = pending.len();
        for (start, end, unit) in pending {
            sequencer.push(start, end, Fade::Smooth, 0.0, 0.0, unit);
        }
        self.frontier = end;
        Ok(FillReport { span, voices })
    }

    /// Fill to `seconds` on the sequencer clock.
    pub fn fill_to_seconds(
        &mut self,
        seconds: f64,
        pattern: &Pattern,
        template: &GraphTemplate,
        transport: Transport,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        let end = transport
            .seconds_to_cycle(seconds)
            .map_err(ScheduleError::Transport)?;
        self.fill_to(end, pattern, template, transport, sequencer)
    }

    /// A correctly shaped empty sequencer for offline tests and simple hosts.
    pub fn sequencer(template: &GraphTemplate) -> Sequencer {
        Sequencer::new(0, template.channels(), ReplayMode::None)
    }
}

impl Default for PitchScheduler {
    fn default() -> Self {
        PitchScheduler::new(Frac::ZERO)
    }
}

fn pitch_from_value(value: &Value) -> Result<Pitch, ScheduleError> {
    match value {
        Value::F(midi) => Ok(Pitch::from_midi(*midi)),
        Value::S(name) => Pitch::parse(name).map_err(|source| ScheduleError::Pitch {
            value: name.clone(),
            source,
        }),
        Value::B(_) => Err(ScheduleError::NotPitch(value.clone())),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FillReport {
    pub span: Span,
    pub voices: usize,
}

#[derive(Clone, PartialEq, Debug)]
pub enum ScheduleError {
    Pitch { value: String, source: PitchError },
    NotPitch(Value),
    InvalidFrequency(f64),
    Template(TemplateError),
    Transport(TransportError),
    ChannelMismatch { expected: usize, found: usize },
}

impl core::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ScheduleError::Pitch { value, source } => {
                write!(f, "cannot schedule {value:?} as a pitch: {source}")
            }
            ScheduleError::NotPitch(value) => write!(f, "{value} is not a pitch"),
            ScheduleError::InvalidFrequency(hz) => {
                write!(f, "pitch produced invalid frequency {hz}")
            }
            ScheduleError::Template(error) => write!(f, "invalid graph template: {error}"),
            ScheduleError::Transport(error) => error.fmt(f),
            ScheduleError::ChannelMismatch { expected, found } => write!(
                f,
                "voice has {expected} output channels but sequencer has {found}"
            ),
        }
    }
}

impl core::error::Error for ScheduleError {}
