//! The first playable bridge: pitch pattern → instantiated voice → sequencer.
//!
//! This is intentionally one narrow path, not the future score model. It gives
//! one [`GraphTemplate`] every onset from a pattern of note names or MIDI
//! numbers. Instrument selection, control maps, generations and activation at
//! musical boundaries come later, after their shapes are settled.

use crate::tempo::TempoMap;
use crate::transport::{CycleTime, Transport, TransportError};
use apteronotus_music::{Pitch, PitchError};
use apteronotus_pattern::{ControlValue, Frac, PRIMARY_FIELD, Pattern, Span, Value};
use apteronotus_synth::{
    BusLayout, ControlStore, EventRouting, GraphTemplate, Implicit, LowerError, Note, ParamId,
    ParamValue, ParamValueError, TemplateError, instantiate, instantiate_routed_with_controls,
};
use fundsp::prelude32::{AudioUnit, Fade, ReplayMode, Sequencer};

/// One pattern/template pair participating in an atomic scheduling window.
#[derive(Clone, Copy)]
pub struct ScheduledTrack<'a> {
    pub pattern: &'a Pattern,
    pub template: &'a GraphTemplate,
}

impl<'a> ScheduledTrack<'a> {
    pub fn new(pattern: &'a Pattern, template: &'a GraphTemplate) -> ScheduledTrack<'a> {
        ScheduledTrack { pattern, template }
    }
}

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
        self.fill_with_clock(end, pattern, template, &transport, sequencer)
    }

    /// The same scheduler path over a variable tempo map.
    pub fn fill_to_tempo_map(
        &mut self,
        end: Frac,
        pattern: &Pattern,
        template: &GraphTemplate,
        tempo: &TempoMap,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        self.fill_with_clock(end, pattern, template, tempo, sequencer)
    }

    fn fill_with_clock(
        &mut self,
        end: Frac,
        pattern: &Pattern,
        template: &GraphTemplate,
        clock: &impl CycleTime,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        let begin = self.frontier;
        if end <= begin {
            return Ok(FillReport {
                span: Span::new(begin, begin),
                voices: 0,
            });
        }

        let span = Span::new(begin, end);
        let pending = prepare_window(
            span,
            [ScheduledTrack::new(pattern, template)],
            clock,
            sequencer,
            VoiceLayout::MainOnly,
        )?;
        let voices = pending.len();
        commit_window(pending, sequencer);
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

    pub fn fill_to_seconds_tempo_map(
        &mut self,
        seconds: f64,
        pattern: &Pattern,
        template: &GraphTemplate,
        tempo: &TempoMap,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        let end = tempo
            .seconds_to_cycle(seconds)
            .map_err(ScheduleError::Transport)?;
        self.fill_to_tempo_map(end, pattern, template, tempo, sequencer)
    }

    /// A correctly shaped empty sequencer for offline tests and simple hosts.
    pub fn sequencer(template: &GraphTemplate) -> Sequencer {
        Sequencer::new(template.inputs, template.channels(), ReplayMode::None)
    }
}

/// A single frontier shared by every track in one program.
///
/// Unlike one [`PitchScheduler`] per track, this prepares every voice in the
/// window before the first sequencer push. A bad pitch or graph in any track
/// therefore leaves the entire window unpublished.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProgramScheduler {
    frontier: Frac,
}

impl ProgramScheduler {
    pub fn new(start: Frac) -> ProgramScheduler {
        ProgramScheduler { frontier: start }
    }

    pub fn frontier(&self) -> Frac {
        self.frontier
    }

    pub fn fill_to<'a>(
        &mut self,
        end: Frac,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        transport: Transport,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        self.fill_with_clock(end, tracks, &transport, sequencer)
    }

    pub fn fill_to_tempo_map<'a>(
        &mut self,
        end: Frac,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        tempo: &TempoMap,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        self.fill_with_clock(end, tracks, tempo, sequencer)
    }

    fn fill_with_clock<'a>(
        &mut self,
        end: Frac,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        clock: &impl CycleTime,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        let begin = self.frontier;
        if end <= begin {
            return Ok(FillReport {
                span: Span::new(begin, begin),
                voices: 0,
            });
        }
        let span = Span::new(begin, end);
        let pending = prepare_window(span, tracks, clock, sequencer, VoiceLayout::MainOnly)?;
        let voices = pending.len();
        commit_window(pending, sequencer);
        self.frontier = end;
        Ok(FillReport { span, voices })
    }

    /// Fill a program whose voices produce flattened main/bus stems and may
    /// read program-scope controls.
    pub fn fill_routed_to<'a>(
        &mut self,
        end: Frac,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        transport: Transport,
        sequencer: &mut Sequencer,
        layout: &BusLayout,
        controls: &ControlStore,
    ) -> Result<FillReport, ScheduleError> {
        self.fill_routed_with_clock(end, tracks, &transport, sequencer, layout, controls)
    }

    fn fill_routed_with_clock<'a>(
        &mut self,
        end: Frac,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        clock: &impl CycleTime,
        sequencer: &mut Sequencer,
        layout: &BusLayout,
        controls: &ControlStore,
    ) -> Result<FillReport, ScheduleError> {
        let begin = self.frontier;
        if end <= begin {
            return Ok(FillReport {
                span: Span::new(begin, begin),
                voices: 0,
            });
        }
        let span = Span::new(begin, end);
        let pending = prepare_window(
            span,
            tracks,
            clock,
            sequencer,
            VoiceLayout::Routed { layout, controls },
        )?;
        let voices = pending.len();
        commit_window(pending, sequencer);
        self.frontier = end;
        Ok(FillReport { span, voices })
    }

    pub fn fill_to_seconds<'a>(
        &mut self,
        seconds: f64,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        transport: Transport,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        let end = transport
            .seconds_to_cycle(seconds)
            .map_err(ScheduleError::Transport)?;
        self.fill_to(end, tracks, transport, sequencer)
    }

    pub fn fill_routed_to_seconds<'a>(
        &mut self,
        seconds: f64,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        transport: Transport,
        sequencer: &mut Sequencer,
        layout: &BusLayout,
        controls: &ControlStore,
    ) -> Result<FillReport, ScheduleError> {
        let end = transport
            .seconds_to_cycle(seconds)
            .map_err(ScheduleError::Transport)?;
        self.fill_routed_to(end, tracks, transport, sequencer, layout, controls)
    }
}

impl Default for ProgramScheduler {
    fn default() -> Self {
        ProgramScheduler::new(Frac::ZERO)
    }
}

type PendingVoice = (f64, f64, Box<dyn AudioUnit>);

#[derive(Clone, Copy)]
enum VoiceLayout<'a> {
    MainOnly,
    Routed {
        layout: &'a BusLayout,
        controls: &'a ControlStore,
    },
}

fn prepare_window<'a>(
    span: Span,
    tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
    clock: &impl CycleTime,
    sequencer: &Sequencer,
    voice_layout: VoiceLayout<'_>,
) -> Result<Vec<PendingVoice>, ScheduleError> {
    let mut pending = Vec::new();
    for track in tracks {
        let template = track.template;
        template.validate().map_err(ScheduleError::Template)?;
        if sequencer.inputs() != template.inputs {
            return Err(ScheduleError::InputChannelMismatch {
                expected: template.inputs,
                found: sequencer.inputs(),
            });
        }
        let expected_outputs = match voice_layout {
            VoiceLayout::MainOnly => template.channels(),
            VoiceLayout::Routed { layout, .. } => layout.total_channels(),
        };
        if sequencer.outputs() != expected_outputs {
            return Err(ScheduleError::ChannelMismatch {
                expected: expected_outputs,
                found: sequencer.outputs(),
            });
        }

        for event in track.pattern.onsets(span) {
            let whole = event.whole.expect("onsets always have a whole span");
            let pitch = pitch_from_value(&event.value)?;
            let hz = pitch.hz();
            if !hz.is_finite() || hz <= 0.0 {
                return Err(ScheduleError::InvalidFrequency(hz));
            }
            let start = clock.cycle_to_seconds(whole.begin);
            let gate = clock.span_to_seconds(whole);
            let note = note_from_value(&event.value, template, hz, gate, event.seed())?;
            let lifetime = template
                .lifetime_for(&note)
                .map_err(ScheduleError::ParamValue)?;
            let unit = match voice_layout {
                VoiceLayout::MainOnly => {
                    instantiate(template, &note).map_err(ScheduleError::Lower)?
                }
                VoiceLayout::Routed { layout, controls } => instantiate_routed_with_controls(
                    template,
                    &note,
                    layout,
                    &EventRouting::new(),
                    controls,
                )
                .map_err(ScheduleError::Lower)?,
            };
            pending.push((start, start + lifetime.end_after_onset(gate), unit));
        }
    }
    Ok(pending)
}

fn commit_window(pending: Vec<PendingVoice>, sequencer: &mut Sequencer) {
    for (start, end, unit) in pending {
        sequencer.push(start, end, Fade::Smooth, 0.0, 0.0, unit);
    }
}

impl Default for PitchScheduler {
    fn default() -> Self {
        PitchScheduler::new(Frac::ZERO)
    }
}

fn pitch_from_value(value: &Value) -> Result<Pitch, ScheduleError> {
    let primary = match value {
        Value::Leaf(value) => value,
        Value::Map(map) => map
            .get(PRIMARY_FIELD)
            .ok_or_else(|| ScheduleError::MissingPrimary(value.clone()))?,
    };
    match primary {
        ControlValue::Number(midi) => Ok(Pitch::from_midi(*midi)),
        ControlValue::Text(name) => Pitch::parse(name).map_err(|source| ScheduleError::Pitch {
            value: name.clone(),
            source,
        }),
        ControlValue::Bool(_) | ControlValue::Curve(_) => {
            Err(ScheduleError::NotPitch(value.clone()))
        }
    }
}

fn note_from_value(
    value: &Value,
    template: &GraphTemplate,
    hz: f64,
    gate: f64,
    seed: u64,
) -> Result<Note, ScheduleError> {
    let mut note = Note::new(hz).duration(gate).seed(seed);
    let Value::Map(map) = value else {
        return Ok(note);
    };
    for field in map.fields() {
        if field.name() == PRIMARY_FIELD {
            continue;
        }
        let control =
            numeric_param_value(field.value()).ok_or_else(|| ScheduleError::NonNumericControl {
                name: field.name().to_owned(),
                value: field.value().clone(),
            })?;
        let id = match field.name() {
            "velocity" => ParamId::Implicit(Implicit::Velocity),
            "pan" => ParamId::Implicit(Implicit::Pan),
            "duration" => return Err(ScheduleError::DurationControl),
            "hz" => return Err(ScheduleError::HzControlDeferred),
            name => {
                let index = template
                    .params
                    .iter()
                    .position(|spec| spec.name == name)
                    .ok_or_else(|| ScheduleError::UnknownControl(name.to_owned()))?;
                ParamId::Declared(index)
            }
        };
        note = note.bind(id, control);
    }
    Ok(note)
}

fn numeric_param_value(value: &ControlValue) -> Option<ParamValue> {
    match value {
        ControlValue::Number(value) => Some(ParamValue::Number(*value)),
        ControlValue::Curve(curve) => Some(ParamValue::Curve(curve.clone())),
        ControlValue::Text(_) | ControlValue::Bool(_) => None,
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
    MissingPrimary(Value),
    UnknownControl(String),
    NonNumericControl { name: String, value: ControlValue },
    DurationControl,
    HzControlDeferred,
    InvalidFrequency(f64),
    ParamValue(ParamValueError),
    Template(TemplateError),
    Lower(LowerError),
    Transport(TransportError),
    InputChannelMismatch { expected: usize, found: usize },
    ChannelMismatch { expected: usize, found: usize },
}

impl core::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ScheduleError::Pitch { value, source } => {
                write!(f, "cannot schedule {value:?} as a pitch: {source}")
            }
            ScheduleError::NotPitch(value) => write!(f, "{value} is not a pitch"),
            ScheduleError::MissingPrimary(value) => {
                write!(f, "control map {value} has no primary pitch field")
            }
            ScheduleError::UnknownControl(name) => {
                write!(f, "voice has no parameter named {name:?}")
            }
            ScheduleError::NonNumericControl { name, value } => {
                write!(f, "parameter {name:?} cannot be bound to {value}")
            }
            ScheduleError::DurationControl => write!(
                f,
                "duration is determined by the event span; a live gate is a separate signal"
            ),
            ScheduleError::HzControlDeferred => write!(
                f,
                "hz controls require a typed frequency value and are not implemented yet"
            ),
            ScheduleError::InvalidFrequency(hz) => {
                write!(f, "pitch produced invalid frequency {hz}")
            }
            ScheduleError::ParamValue(error) => error.fmt(f),
            ScheduleError::Template(error) => write!(f, "invalid graph template: {error}"),
            ScheduleError::Lower(error) => write!(f, "cannot lower graph: {error}"),
            ScheduleError::Transport(error) => error.fmt(f),
            ScheduleError::InputChannelMismatch { expected, found } => write!(
                f,
                "voice has {expected} input channels but sequencer has {found}"
            ),
            ScheduleError::ChannelMismatch { expected, found } => write!(
                f,
                "voice has {expected} output channels but sequencer has {found}"
            ),
        }
    }
}

impl core::error::Error for ScheduleError {}
