//! The first playable bridge: pitch pattern → instantiated voice → sequencer.
//!
//! This is intentionally one narrow path, not the future score model. It gives
//! one [`GraphTemplate`] every onset from a pattern of note names or MIDI
//! numbers. Instrument selection, control maps, generations and activation at
//! musical boundaries come later, after their shapes are settled.

use apteronotus_music::{Pitch, PitchError};
use apteronotus_pattern::{ControlValue, Frac, PRIMARY_FIELD, Pattern, Span, Value};
use apteronotus_synth::{
    BusLayout, ControlError, ControlStore, DuckControl, EventRouting, GraphTemplate, Implicit,
    InitControlBinding, LowerError, Note, ParamId, ParamValue, ParamValueError, PatchTemplate,
    TemplateError, instantiate, instantiate_routed_with_controls,
    instantiate_timed_patch_routed_with_routing,
};
use apteronotus_transport::{CycleTime, TempoMap, Transport, TransportError};
use fundsp::net::Net;
use fundsp::prelude32::{AudioUnit, Fade, ReplayMode, Sequencer, envelope, pass};

/// One pattern/template pair participating in an atomic scheduling window.
#[derive(Clone, Copy)]
pub struct ScheduledTrack<'a> {
    pub pattern: &'a Pattern,
    pub template: &'a GraphTemplate,
    pub routing: Option<&'a EventRouting>,
    pub onset_bindings: &'a [InitControlBinding],
}

/// One finite autonomous patch activation.
#[derive(Clone, Copy)]
pub struct ScheduledRun<'a> {
    pub patch: &'a PatchTemplate,
    pub span: Span,
    pub routing: Option<&'a EventRouting>,
}

impl<'a> ScheduledRun<'a> {
    pub fn new(patch: &'a PatchTemplate, span: Span) -> ScheduledRun<'a> {
        ScheduledRun {
            patch,
            span,
            routing: None,
        }
    }

    pub fn with_routing(
        patch: &'a PatchTemplate,
        span: Span,
        routing: &'a EventRouting,
    ) -> ScheduledRun<'a> {
        ScheduledRun {
            patch,
            span,
            routing: Some(routing),
        }
    }
}

/// Concrete arena required by routed voice and finite-run lowering.
#[derive(Clone, Copy)]
pub struct RoutedRuntime<'a> {
    pub layout: &'a BusLayout,
    pub controls: &'a ControlStore,
}

impl<'a> RoutedRuntime<'a> {
    pub fn new(layout: &'a BusLayout, controls: &'a ControlStore) -> RoutedRuntime<'a> {
        RoutedRuntime { layout, controls }
    }
}

/// One observed realtime onset, already placed on the host audio clock.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ExternalOnset {
    pub at_seconds: f64,
    pub gate_seconds: f64,
    pub event_seed: u64,
}

/// Instantiate one realtime-triggered voice at a host-selected minimum-latency
/// timestamp. Unlike pattern scheduling, this has no lookahead query: the host
/// calls it only after observing an external rising edge.
pub fn schedule_external_routed(
    onset: ExternalOnset,
    template: &GraphTemplate,
    routing: &EventRouting,
    onset_bindings: &[InitControlBinding],
    event_bindings: &[(ParamId, ParamValue)],
    sequencer: &mut Sequencer,
    runtime: RoutedRuntime<'_>,
) -> Result<(), ScheduleError> {
    if !onset.at_seconds.is_finite() || onset.at_seconds < 0.0 {
        return Err(ScheduleError::InvalidExternalTime(onset.at_seconds));
    }
    if !onset.gate_seconds.is_finite() || onset.gate_seconds <= 0.0 {
        return Err(ScheduleError::InvalidExternalGate(onset.gate_seconds));
    }
    let mut hz = Implicit::Hz.default_value();
    if let Some(binding) = onset_bindings
        .iter()
        .find(|binding| binding.param == ParamId::Implicit(Implicit::Hz))
    {
        hz = runtime
            .controls
            .value(binding.control)
            .map_err(ScheduleError::Control)?;
    }
    if let Some((_, ParamValue::Number(value))) = event_bindings
        .iter()
        .find(|(param, _)| *param == ParamId::Implicit(Implicit::Hz))
    {
        hz = *value;
    }
    if !hz.is_finite() || hz <= 0.0 {
        return Err(ScheduleError::InvalidFrequency(hz));
    }
    let mut note = Note::new(hz)
        .duration(onset.gate_seconds)
        .seed(onset.event_seed);
    for binding in onset_bindings {
        if binding.param == ParamId::Implicit(Implicit::Hz) {
            continue;
        }
        let value = runtime
            .controls
            .value(binding.control)
            .map_err(ScheduleError::Control)?;
        note = note.bind(binding.param, ParamValue::Number(value));
    }
    for (param, value) in event_bindings {
        if *param != ParamId::Implicit(Implicit::Hz) {
            note = note.bind(*param, value.clone());
        }
    }
    let lifetime = template
        .lifetime_for(&note)
        .map_err(ScheduleError::ParamValue)?;
    let unit = instantiate_routed_with_controls(
        template,
        &note,
        runtime.layout,
        routing,
        runtime.controls,
    )
    .map_err(ScheduleError::Lower)?;
    sequencer.push(
        onset.at_seconds,
        onset.at_seconds + lifetime.end_after_onset(onset.gate_seconds),
        Fade::Smooth,
        0.0,
        0.0,
        unit,
    );
    Ok(())
}

impl<'a> ScheduledTrack<'a> {
    pub fn new(pattern: &'a Pattern, template: &'a GraphTemplate) -> ScheduledTrack<'a> {
        ScheduledTrack {
            pattern,
            template,
            routing: None,
            onset_bindings: &[],
        }
    }

    pub fn with_routing(
        pattern: &'a Pattern,
        template: &'a GraphTemplate,
        routing: &'a EventRouting,
    ) -> ScheduledTrack<'a> {
        ScheduledTrack {
            pattern,
            template,
            routing: Some(routing),
            onset_bindings: &[],
        }
    }

    pub fn with_routing_and_bindings(
        pattern: &'a Pattern,
        template: &'a GraphTemplate,
        routing: &'a EventRouting,
        onset_bindings: &'a [InitControlBinding],
    ) -> ScheduledTrack<'a> {
        ScheduledTrack {
            pattern,
            template,
            routing: Some(routing),
            onset_bindings,
        }
    }
}

/// A scheduler owns exactly one frontier. Windows it submits are adjacent and
/// therefore cannot duplicate an onset even when callers poll irregularly.
#[derive(Clone, PartialEq, Debug)]
pub struct PitchScheduler {
    frontier: Frac,
    previous_hz: Option<f64>,
}

impl PitchScheduler {
    pub fn new(start: Frac) -> PitchScheduler {
        PitchScheduler {
            frontier: start,
            previous_hz: None,
        }
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
        let mut track_history = vec![self.previous_hz];
        let pending = prepare_window(
            span,
            [ScheduledTrack::new(pattern, template)],
            clock,
            sequencer,
            VoiceLayout::MainOnly,
            &mut track_history,
        )?;
        let voices = pending.len();
        commit_window(pending, sequencer);
        self.previous_hz = track_history[0];
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
#[derive(Clone, PartialEq, Debug)]
pub struct ProgramScheduler {
    frontier: Frac,
    previous_hz: Vec<Option<f64>>,
}

impl ProgramScheduler {
    pub fn new(start: Frac) -> ProgramScheduler {
        ProgramScheduler {
            frontier: start,
            previous_hz: Vec::new(),
        }
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
        let mut previous_hz = self.previous_hz.clone();
        let pending = prepare_window(
            span,
            tracks,
            clock,
            sequencer,
            VoiceLayout::MainOnly,
            &mut previous_hz,
        )?;
        let voices = pending.len();
        commit_window(pending, sequencer);
        self.previous_hz = previous_hz;
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

    pub fn fill_routed_to_tempo_map<'a>(
        &mut self,
        end: Frac,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        tempo: &TempoMap,
        sequencer: &mut Sequencer,
        layout: &BusLayout,
        controls: &ControlStore,
    ) -> Result<FillReport, ScheduleError> {
        self.fill_routed_with_clock(end, tracks, tempo, sequencer, layout, controls)
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
        self.fill_routed_program_with_clock(
            end,
            tracks,
            core::iter::empty(),
            clock,
            sequencer,
            RoutedRuntime::new(layout, controls),
        )
    }

    /// Fill routed voices and finite autonomous patch instances atomically.
    pub fn fill_routed_program_to_tempo_map<'a>(
        &mut self,
        end: Frac,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        runs: impl IntoIterator<Item = ScheduledRun<'a>>,
        tempo: &TempoMap,
        sequencer: &mut Sequencer,
        routed: RoutedRuntime<'_>,
    ) -> Result<FillReport, ScheduleError> {
        self.fill_routed_program_with_clock(end, tracks, runs, tempo, sequencer, routed)
    }

    fn fill_routed_program_with_clock<'a>(
        &mut self,
        end: Frac,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        runs: impl IntoIterator<Item = ScheduledRun<'a>>,
        clock: &impl CycleTime,
        sequencer: &mut Sequencer,
        routed: RoutedRuntime<'_>,
    ) -> Result<FillReport, ScheduleError> {
        let begin = self.frontier;
        if end <= begin {
            return Ok(FillReport {
                span: Span::new(begin, begin),
                voices: 0,
            });
        }
        let span = Span::new(begin, end);
        let mut previous_hz = self.previous_hz.clone();
        let mut pending = prepare_window(
            span,
            tracks,
            clock,
            sequencer,
            VoiceLayout::Routed {
                layout: routed.layout,
                controls: routed.controls,
            },
            &mut previous_hz,
        )?;
        prepare_timed_runs(
            span,
            runs,
            clock,
            sequencer,
            routed.layout,
            routed.controls,
            &mut pending,
        )?;
        let voices = pending.len();
        commit_window(pending, sequencer);
        self.previous_hz = previous_hz;
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

    pub fn fill_to_seconds_tempo_map<'a>(
        &mut self,
        seconds: f64,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        tempo: &TempoMap,
        sequencer: &mut Sequencer,
    ) -> Result<FillReport, ScheduleError> {
        let end = tempo
            .seconds_to_cycle(seconds)
            .map_err(ScheduleError::Transport)?;
        self.fill_to_tempo_map(end, tracks, tempo, sequencer)
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

    pub fn fill_routed_to_seconds_tempo_map<'a>(
        &mut self,
        seconds: f64,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        tempo: &TempoMap,
        sequencer: &mut Sequencer,
        layout: &BusLayout,
        controls: &ControlStore,
    ) -> Result<FillReport, ScheduleError> {
        let end = tempo
            .seconds_to_cycle(seconds)
            .map_err(ScheduleError::Transport)?;
        self.fill_routed_to_tempo_map(end, tracks, tempo, sequencer, layout, controls)
    }

    pub fn fill_routed_program_to_seconds_tempo_map<'a>(
        &mut self,
        seconds: f64,
        tracks: impl IntoIterator<Item = ScheduledTrack<'a>>,
        runs: impl IntoIterator<Item = ScheduledRun<'a>>,
        tempo: &TempoMap,
        sequencer: &mut Sequencer,
        routed: RoutedRuntime<'_>,
    ) -> Result<FillReport, ScheduleError> {
        let end = tempo
            .seconds_to_cycle(seconds)
            .map_err(ScheduleError::Transport)?;
        self.fill_routed_program_to_tempo_map(end, tracks, runs, tempo, sequencer, routed)
    }

    /// Forget event-to-event instantiation history while retaining the
    /// publication frontier.
    ///
    /// Track identity is not yet reconciled across Lua evaluations. A live
    /// edit therefore starts its next portamento at the new note rather than
    /// risking a glide from an unrelated track that moved in declaration
    /// order.
    pub fn clear_track_history(&mut self) {
        self.previous_hz.clear();
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
    previous_hz: &mut Vec<Option<f64>>,
) -> Result<Vec<PendingVoice>, ScheduleError> {
    let mut pending = Vec::new();
    for (track_index, track) in tracks.into_iter().enumerate() {
        if previous_hz.len() <= track_index {
            previous_hz.resize(track_index + 1, None);
        }
        if matches!(voice_layout, VoiceLayout::MainOnly)
            && track
                .routing
                .is_some_and(|routing| !routing.sends().is_empty())
        {
            return Err(ScheduleError::RoutingRequiresLayout);
        }
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

        let mut events = track.pattern.onsets(span);
        // Structural order remains the tiebreaker for simultaneous events,
        // but previous-note state is temporal: transforms may emit events in
        // a stable order that is not chronological.
        events.sort_by_key(|event| event.whole.expect("onsets always have a whole span").begin);
        let mut event_index = 0;
        while event_index < events.len() {
            let onset = events[event_index]
                .whole
                .expect("onsets always have a whole span")
                .begin;
            let group_end = events[event_index..]
                .iter()
                .position(|event| {
                    event.whole.expect("onsets always have a whole span").begin != onset
                })
                .map_or(events.len(), |offset| event_index + offset);
            let preceding_hz = previous_hz[track_index];
            let mut last_hz = preceding_hz;
            for event in &events[event_index..group_end] {
                let whole = event.whole.expect("onsets always have a whole span");
                let mut hz = pitch_or_trigger_hz(&event.value, template)?;
                if !track.onset_bindings.is_empty() {
                    let VoiceLayout::Routed { controls, .. } = voice_layout else {
                        return Err(ScheduleError::OnsetControlsRequireRuntime);
                    };
                    if let Some(binding) = track
                        .onset_bindings
                        .iter()
                        .find(|binding| binding.param == ParamId::Implicit(Implicit::Hz))
                    {
                        hz = controls
                            .value(binding.control)
                            .map_err(ScheduleError::Control)?;
                    }
                }
                if !hz.is_finite() || hz <= 0.0 {
                    return Err(ScheduleError::InvalidFrequency(hz));
                }
                let start = clock.cycle_to_seconds(whole.begin);
                let gate = clock.span_to_seconds(whole);
                let mut note = note_from_value(&event.value, template, hz, gate, event.seed())?
                    .previous_hz(preceding_hz);
                if !track.onset_bindings.is_empty() {
                    let VoiceLayout::Routed { controls, .. } = voice_layout else {
                        return Err(ScheduleError::OnsetControlsRequireRuntime);
                    };
                    for binding in track.onset_bindings {
                        if binding.param == ParamId::Implicit(Implicit::Hz) {
                            continue;
                        }
                        let value = controls
                            .value(binding.control)
                            .map_err(ScheduleError::Control)?;
                        note = note.bind(binding.param, ParamValue::Number(value));
                    }
                }
                let lifetime = template
                    .lifetime_for(&note)
                    .map_err(ScheduleError::ParamValue)?;
                let mut unit = match voice_layout {
                    VoiceLayout::MainOnly => {
                        instantiate(template, &note).map_err(ScheduleError::Lower)?
                    }
                    VoiceLayout::Routed { layout, controls } => instantiate_routed_with_controls(
                        template,
                        &note,
                        layout,
                        track.routing.unwrap_or(&EventRouting::new()),
                        controls,
                    )
                    .map_err(ScheduleError::Lower)?,
                };
                let end = start + lifetime.end_after_onset(gate);
                if let Some(duck) = track.routing.and_then(EventRouting::duck_control) {
                    let output_channels = match voice_layout {
                        VoiceLayout::MainOnly => template.channels(),
                        VoiceLayout::Routed { layout, .. } => layout.total_channels(),
                    };
                    let schedule = compile_duck(duck, start, end, clock)?;
                    unit = apply_duck(unit, output_channels, schedule);
                }
                pending.push((start, end, unit));
                last_hz = Some(hz);
            }
            // Simultaneous chord members all glide from the same preceding
            // onset. Structural order chooses which member becomes history
            // for the following onset; it is deterministic, not a claim of
            // musically meaningful chord order.
            previous_hz[track_index] = last_hz;
            event_index = group_end;
        }
    }
    Ok(pending)
}

fn prepare_timed_runs<'a>(
    query: Span,
    runs: impl IntoIterator<Item = ScheduledRun<'a>>,
    clock: &impl CycleTime,
    sequencer: &Sequencer,
    layout: &BusLayout,
    controls: &ControlStore,
    pending: &mut Vec<PendingVoice>,
) -> Result<(), ScheduleError> {
    if sequencer.inputs() != 0 {
        return Err(ScheduleError::InputChannelMismatch {
            expected: 0,
            found: sequencer.inputs(),
        });
    }
    if sequencer.outputs() != layout.total_channels() {
        return Err(ScheduleError::ChannelMismatch {
            expected: layout.total_channels(),
            found: sequencer.outputs(),
        });
    }
    for run in runs {
        if run.span.begin < query.begin || run.span.begin >= query.end {
            continue;
        }
        if run.patch.graph().inputs != 0 {
            return Err(ScheduleError::TimedRunInputs(run.patch.graph().inputs));
        }
        let start = clock.cycle_to_seconds(run.span.begin);
        let active_seconds = clock.span_to_seconds(run.span);
        let (unit, lifetime) = instantiate_timed_patch_routed_with_routing(
            run.patch,
            active_seconds,
            0.01_f64.min(active_seconds * 0.5),
            layout,
            run.routing.unwrap_or(&EventRouting::new()),
            controls,
        )
        .map_err(ScheduleError::Lower)?;
        pending.push((
            start,
            start + lifetime.end_after_onset(active_seconds),
            unit,
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct DuckSegment {
    begin: f32,
    end: f32,
    depth: f32,
}

fn compile_duck(
    duck: &DuckControl,
    voice_start: f64,
    voice_end: f64,
    clock: &impl CycleTime,
) -> Result<Vec<DuckSegment>, ScheduleError> {
    let begin_cycle = clock
        .seconds_to_cycle(voice_start)
        .map_err(ScheduleError::Transport)?;
    let end_cycle = clock
        .seconds_to_cycle(voice_end)
        .map_err(ScheduleError::Transport)?;
    let mut schedule = Vec::new();
    for event in duck.pattern.query(Span::new(begin_cycle, end_cycle)) {
        let Some(whole) = event.whole else {
            return Err(ScheduleError::ContinuousDuck);
        };
        let value = duck_value(&event.value)
            .ok_or_else(|| ScheduleError::NonNumericDuck(event.value.clone()))?;
        let depth = (duck.amount * value.clamp(0.0, 1.0)) as f32;
        if depth == 0.0 {
            continue;
        }
        let begin = clock.cycle_to_seconds(whole.begin) - voice_start;
        let end = clock.cycle_to_seconds(whole.end) - voice_start;
        if end > 0.0 && begin < voice_end - voice_start && end > begin {
            schedule.push(DuckSegment {
                begin: begin as f32,
                end: end as f32,
                depth,
            });
        }
    }
    schedule.sort_by(|left, right| left.begin.total_cmp(&right.begin));
    Ok(schedule)
}

fn duck_value(value: &Value) -> Option<f64> {
    match value {
        Value::Leaf(value) => value.as_f64(),
        Value::Map(map) => map.get(PRIMARY_FIELD).and_then(ControlValue::as_f64),
    }
}

fn apply_duck(
    unit: Box<dyn AudioUnit>,
    output_channels: usize,
    schedule: Vec<DuckSegment>,
) -> Box<dyn AudioUnit> {
    let input_channels = unit.inputs();
    let mut net = Net::new(input_channels, output_channels);
    let source = net.push(unit);
    for channel in 0..input_channels {
        net.connect_input(channel, source, channel);
    }
    let control = net.push(Box::new(envelope(move |time: f32| {
        let mut gain = 1.0_f32;
        for segment in &schedule {
            if time < segment.begin {
                break;
            }
            if time < segment.end {
                let phase =
                    ((time - segment.begin) / (segment.end - segment.begin)).clamp(0.0, 1.0);
                // The trigger value is sampled at its onset. A smoothstep
                // release over the trigger's event extent returns the whole
                // line to unity without zippering or an interpreter callback.
                let release = phase * phase * (3.0 - 2.0 * phase);
                gain = gain.min(1.0 - segment.depth * (1.0 - release));
            }
        }
        gain
    })));
    for channel in 0..output_channels {
        let multiply = net.push(Box::new(pass() * pass()));
        net.connect(source, channel, multiply, 0);
        net.connect(control, 0, multiply, 1);
        net.connect_output(multiply, 0, channel);
    }
    Box::new(net)
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

fn pitch_or_trigger_hz(value: &Value, template: &GraphTemplate) -> Result<f64, ScheduleError> {
    let primary = match value {
        Value::Leaf(value) => value,
        Value::Map(map) => map
            .get(PRIMARY_FIELD)
            .ok_or_else(|| ScheduleError::MissingPrimary(value.clone()))?,
    };
    match primary {
        ControlValue::Number(midi) => Ok(Pitch::from_midi(*midi).hz()),
        ControlValue::Text(name) => match Pitch::parse(name) {
            Ok(pitch) => Ok(pitch.hz()),
            Err(_) if !template.uses_param(ParamId::Implicit(Implicit::Hz)) => {
                // The value is an onset label, not a pitch. `Note` still
                // carries a finite placeholder because its scalar layout is
                // shared with pitched voices, but the validated graph cannot
                // observe it.
                Ok(Implicit::Hz.default_value())
            }
            Err(source) => Err(ScheduleError::Pitch {
                value: name.clone(),
                source,
            }),
        },
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
    Control(ControlError),
    Transport(TransportError),
    InputChannelMismatch { expected: usize, found: usize },
    ChannelMismatch { expected: usize, found: usize },
    RoutingRequiresLayout,
    OnsetControlsRequireRuntime,
    ContinuousDuck,
    NonNumericDuck(Value),
    TimedRunInputs(usize),
    InvalidExternalTime(f64),
    InvalidExternalGate(f64),
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
            ScheduleError::Control(error) => write!(f, "cannot sample onset control: {error}"),
            ScheduleError::Transport(error) => error.fmt(f),
            ScheduleError::InputChannelMismatch { expected, found } => write!(
                f,
                "voice has {expected} input channels but sequencer has {found}"
            ),
            ScheduleError::ChannelMismatch { expected, found } => write!(
                f,
                "voice has {expected} output channels but sequencer has {found}"
            ),
            ScheduleError::RoutingRequiresLayout => write!(
                f,
                "score sends require routed scheduling with the program bus layout"
            ),
            ScheduleError::OnsetControlsRequireRuntime => write!(
                f,
                "onset-sampled controls require the persistent program control runtime"
            ),
            ScheduleError::ContinuousDuck => {
                write!(f, "duck trigger must contain discrete events")
            }
            ScheduleError::NonNumericDuck(value) => {
                write!(f, "duck trigger value {value} is not numeric")
            }
            ScheduleError::TimedRunInputs(found) => write!(
                f,
                "finite run patch has {found} audio inputs; only autonomous patches can be timed"
            ),
            ScheduleError::InvalidExternalTime(seconds) => {
                write!(f, "external trigger has invalid start time {seconds}")
            }
            ScheduleError::InvalidExternalGate(seconds) => {
                write!(f, "external trigger has invalid gate duration {seconds}")
            }
        }
    }
}

impl core::error::Error for ScheduleError {}
