use apteronotus_music::Key;
use apteronotus_pattern::{Pattern, Span, ValueLimitError, ValueLimits};
use apteronotus_synth::{
    AudioInputLayout, BusLayout, ControlLayout, EventRouting, GraphLimitError, GraphLimits,
    GraphTemplate, InitControlBinding, Op, ParamId, PatchTemplate, Source, TemplateError,
};
use apteronotus_transport::TempoMap;

/// An index into [`Program::voices`].
///
/// It is deliberately opaque at the API boundary. Lua sees one-based handles;
/// Rust's vector index remains an implementation detail.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct VoiceId(pub(crate) usize);

impl VoiceId {
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PatchId(pub(crate) usize);

impl PatchId {
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Track {
    pub voice: VoiceId,
    pub pattern: Pattern,
    pub routing: EventRouting,
    pub onset_bindings: Vec<InitControlBinding>,
    /// Pattern-rate parameter values sampled at the observed live onset.
    ///
    /// These patterns remain pure transport-coordinate data; the host queries
    /// them only after an external edge has arrived and never asks them to
    /// predict that edge.
    pub external_controls: Vec<ExternalControlBinding>,
    /// Event-rate random filters applied to captured arrival identity.
    pub external_degrades: Vec<ExternalDegrade>,
    /// Realtime onset source for the host's minimum-latency scheduling path.
    ///
    /// Its pure-pattern fallback is silence because a live stream cannot be
    /// queried ahead. Recording turns those onsets into an ordinary Timeline.
    pub external_trigger: Option<apteronotus_synth::ControlId>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ExternalControlBinding {
    pub param: ParamId,
    pub pattern: Pattern,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ExternalDegrade {
    pub amount: f64,
    pub seed: u64,
}

/// One requested persistent-patch activation.
///
/// `span == None` is the original program-lifetime `run(patch)`. A finite span
/// is scheduled as one stateful instance: sources stop at the span boundary,
/// downstream response tails drain, and then the instance is removed.
#[derive(Clone, PartialEq, Debug)]
pub struct PatchRun {
    pub patch: PatchId,
    pub span: Option<Span>,
    /// Static score routing applied to this persistent instance. A plain
    /// `run(patch)` leaves it empty; `play(patch, finite_pattern)` uses the
    /// pattern's track-level sends while the control driver performs notes.
    pub routing: EventRouting,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatchControlKind {
    Number,
    Note,
    Gate,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PatchControl {
    pub name: String,
    pub id: apteronotus_synth::ControlId,
    pub kind: PatchControlKind,
}

/// The data-only result of one successful edit evaluation.
#[derive(Clone, PartialEq, Debug)]
pub struct Program {
    /// The score clock. Keeping it in the owned result makes scheduling
    /// independent of the language runtime and preserves tempo ramps.
    pub tempo: TempoMap,
    pub key: Option<Key>,
    pub controls: ControlLayout,
    /// Logical controls whose default is used until a host binds hardware.
    pub control_inputs: Vec<String>,
    pub audio_inputs: AudioInputLayout,
    pub buses: BusLayout,
    pub voices: Vec<GraphTemplate>,
    pub patches: Vec<PatchTemplate>,
    /// Lexical parameter ports aligned one-for-one with `patches`.
    pub patch_controls: Vec<Vec<PatchControl>>,
    pub runs: Vec<PatchRun>,
    pub tracks: Vec<Track>,
}

impl Program {
    pub fn voice(&self, id: VoiceId) -> Option<&GraphTemplate> {
        self.voices.get(id.0)
    }

    pub fn patch(&self, id: PatchId) -> Option<&PatchTemplate> {
        self.patches.get(id.0)
    }

    pub fn patch_controls(&self, id: PatchId) -> Option<&[PatchControl]> {
        self.patch_controls.get(id.0).map(Vec::as_slice)
    }

    /// Whether the program-scoped runtime arena can continue across this edit.
    ///
    /// Opaque control and bus handles intentionally differ between evaluation
    /// arenas, so raw `PartialEq` is not a compatibility fingerprint. This
    /// compares logical layouts and graph sources while ignoring arena
    /// identities and source spans.
    pub fn persistent_compatible_with(&self, other: &Program) -> bool {
        self.controls.specs() == other.controls.specs()
            && self.buses.main_channels() == other.buses.main_channels()
            && self.buses.bus_channel_counts() == other.buses.bus_channel_counts()
            && self.audio_inputs.specs() == other.audio_inputs.specs()
            && self.control_inputs == other.control_inputs
            && self.runs.iter().filter(|run| run.span.is_none()).count()
                == other.runs.iter().filter(|run| run.span.is_none()).count()
            && self
                .runs
                .iter()
                .filter(|run| run.span.is_none())
                .zip(other.runs.iter().filter(|run| run.span.is_none()))
                .all(
                    |(left, right)| match (self.patch(left.patch), other.patch(right.patch)) {
                        (Some(left), Some(right)) => graph_compatible_across_arenas(
                            left.graph(),
                            right.graph(),
                            ArenaView::new(self),
                            ArenaView::new(other),
                        ),
                        _ => false,
                    },
                )
    }

    /// Retain `active`'s compatible persistent arena handles and templates.
    ///
    /// Voice graphs are still the newly evaluated graphs, but any references
    /// they make to program controls or buses are rebound onto the retained
    /// arena. Returns `false` without changing `self` when compatibility or
    /// rebinding cannot be proven.
    pub fn reuse_persistent_from(&mut self, active: &Program) -> bool {
        if !self.persistent_compatible_with(active) {
            return false;
        }
        let mut voices = self.voices.clone();
        if !voices.iter_mut().all(|voice| {
            rebind_graph_arenas(
                voice,
                &self.controls,
                &active.controls,
                &self.audio_inputs,
                &active.audio_inputs,
                &self.buses,
                &active.buses,
            )
        }) {
            return false;
        }

        // Inert patch declarations are allowed to change without resetting an
        // unrelated running rack. Rebind them so the new Program remains
        // internally valid in the retained arena, then restore only the
        // activated templates from `active`: those are the graphs whose live
        // DSP instances the host is actually continuing.
        let mut patches = Vec::with_capacity(self.patches.len());
        for patch in &self.patches {
            let mut graph = patch.graph().clone();
            if !rebind_graph_arenas(
                &mut graph,
                &self.controls,
                &active.controls,
                &self.audio_inputs,
                &active.audio_inputs,
                &self.buses,
                &active.buses,
            ) {
                return false;
            }
            let Ok(patch) = PatchTemplate::new(graph) else {
                return false;
            };
            patches.push(patch);
        }
        let mut patch_controls = self.patch_controls.clone();
        for controls in &mut patch_controls {
            for control in controls {
                let Some(id) = self.controls.corresponding_id(control.id, &active.controls) else {
                    return false;
                };
                control.id = id;
            }
        }
        for (candidate_run, active_run) in self
            .runs
            .iter()
            .filter(|run| run.span.is_none())
            .zip(active.runs.iter().filter(|run| run.span.is_none()))
        {
            let Some(active_patch) = active.patch(active_run.patch) else {
                return false;
            };
            let Some(candidate_patch) = patches.get_mut(candidate_run.patch.0) else {
                return false;
            };
            *candidate_patch = active_patch.clone();
        }

        let mut tracks = self.tracks.clone();
        for track in &mut tracks {
            let Ok(routing) = track.routing.rebound(&self.buses, &active.buses) else {
                return false;
            };
            track.routing = routing;
            if let Some(trigger) = &mut track.external_trigger {
                let Some(rebound) = self.controls.corresponding_id(*trigger, &active.controls)
                else {
                    return false;
                };
                *trigger = rebound;
            }
            for binding in &mut track.onset_bindings {
                let Some(rebound) = self
                    .controls
                    .corresponding_id(binding.control, &active.controls)
                else {
                    return false;
                };
                binding.control = rebound;
            }
        }
        let mut runs = self.runs.clone();
        for run in &mut runs {
            let Ok(routing) = run.routing.rebound(&self.buses, &active.buses) else {
                return false;
            };
            run.routing = routing;
        }
        self.voices = voices;
        self.tracks = tracks;
        self.runs = runs;
        self.controls = active.controls.clone();
        self.audio_inputs = active.audio_inputs.clone();
        self.buses = active.buses.clone();
        self.patches = patches;
        self.patch_controls = patch_controls;
        true
    }

    /// Validate one complete candidate before publication.
    ///
    /// Evaluation performs the same graph checks while staging. This boundary
    /// deliberately repeats them so a future cached/non-Lua `Program` cannot
    /// bypass host policy.
    pub fn validate(&self, limits: GraphLimits) -> Result<(), ProgramError> {
        self.validate_with_value_limits(limits, ValueLimits::default())
    }

    pub fn validate_with_value_limits(
        &self,
        limits: GraphLimits,
        value_limits: ValueLimits,
    ) -> Result<(), ProgramError> {
        for (index, graph) in self.voices.iter().enumerate() {
            graph
                .validate()
                .map_err(|source| ProgramError::VoiceTemplate { index, source })?;
            graph
                .validate_limits(limits)
                .map_err(|source| ProgramError::VoiceLimits { index, source })?;
            if !graph_audio_inputs_valid(graph, &self.audio_inputs) {
                return Err(ProgramError::UnknownVoiceAudioInput { index });
            }
            if !graph_controls_valid(graph, &self.controls) {
                return Err(ProgramError::UnknownVoiceControl { index });
            }
            for send in &graph.sends {
                let expected = self
                    .buses
                    .bus_channels(send.bus)
                    .ok_or(ProgramError::UnknownVoiceBus { index })?;
                if send.outputs.len() != expected {
                    return Err(ProgramError::VoiceBusChannels {
                        index,
                        expected,
                        found: send.outputs.len(),
                    });
                }
            }
        }
        for (index, patch) in self.patches.iter().enumerate() {
            patch
                .graph()
                .validate_limits(limits)
                .map_err(|source| ProgramError::PatchLimits { index, source })?;
            if !graph_audio_inputs_valid(patch.graph(), &self.audio_inputs) {
                return Err(ProgramError::UnknownPatchAudioInput { index });
            }
            if !graph_controls_valid(patch.graph(), &self.controls) {
                return Err(ProgramError::UnknownPatchControl { index });
            }
        }
        if self.patch_controls.len() != self.patches.len() {
            return Err(ProgramError::PatchControlLayout);
        }
        for controls in &self.patch_controls {
            for control in controls {
                if self.controls.spec(control.id).is_none() {
                    return Err(ProgramError::PatchControlLayout);
                }
            }
        }
        for (index, track) in self.tracks.iter().enumerate() {
            let Some(voice) = self.voice(track.voice) else {
                return Err(ProgramError::MissingTrackVoice { index });
            };
            if track
                .external_trigger
                .is_some_and(|trigger| self.controls.spec(trigger).is_none())
            {
                return Err(ProgramError::UnknownTrackTrigger { index });
            }
            if !track.onset_bindings.is_empty() && track.external_trigger.is_none() {
                return Err(ProgramError::OnsetBindingsRequireTrigger { index });
            }
            if (!track.external_controls.is_empty() || !track.external_degrades.is_empty())
                && track.external_trigger.is_none()
            {
                return Err(ProgramError::ExternalModifiersRequireTrigger { index });
            }
            for binding in &track.onset_bindings {
                if self.controls.spec(binding.control).is_none()
                    || matches!(binding.param, ParamId::Declared(param) if param >= voice.params.len())
                    || matches!(
                        binding.param,
                        ParamId::Implicit(apteronotus_synth::Implicit::Duration)
                    )
                {
                    return Err(ProgramError::UnknownTrackOnsetBinding { index });
                }
            }
            for binding in &track.external_controls {
                if matches!(binding.param, ParamId::Declared(param) if param >= voice.params.len())
                    || matches!(
                        binding.param,
                        ParamId::Implicit(apteronotus_synth::Implicit::Duration)
                    )
                {
                    return Err(ProgramError::UnknownTrackExternalControl { index });
                }
                binding
                    .pattern
                    .validate_value_limits(value_limits)
                    .map_err(|source| ProgramError::TrackValues { index, source })?;
            }
            if track.external_degrades.iter().any(|degrade| {
                !degrade.amount.is_finite() || !(0.0..=1.0).contains(&degrade.amount)
            }) {
                return Err(ProgramError::UnknownTrackExternalControl { index });
            }
            for send in track.routing.sends() {
                let Some(expected) = self.buses.bus_channels(send.bus) else {
                    return Err(ProgramError::UnknownTrackBus { index });
                };
                if voice.channels() != expected && voice.channels() != 1 {
                    return Err(ProgramError::TrackBusChannels {
                        index,
                        expected,
                        found: voice.channels(),
                    });
                }
            }
            track
                .pattern
                .validate_value_limits(value_limits)
                .map_err(|source| ProgramError::TrackValues { index, source })?;
            if let Some(duck) = track.routing.duck_control() {
                duck.pattern
                    .validate_value_limits(value_limits)
                    .map_err(|source| ProgramError::TrackValues { index, source })?;
            }
        }
        for (index, run) in self.runs.iter().enumerate() {
            let Some(patch) = self.patch(run.patch) else {
                return Err(ProgramError::MissingRunPatch { index });
            };
            if let Some(span) = run.span {
                if span.begin >= span.end {
                    return Err(ProgramError::InvalidRunSpan { index });
                }
                if patch.graph().inputs != 0 {
                    return Err(ProgramError::TimedRunInputs { index });
                }
            }
            for send in run.routing.sends() {
                let Some(expected) = self.buses.bus_channels(send.bus) else {
                    return Err(ProgramError::UnknownRunBus { index });
                };
                if patch.graph().channels() != expected && patch.graph().channels() != 1 {
                    return Err(ProgramError::RunBusChannels {
                        index,
                        expected,
                        found: patch.graph().channels(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn graph_audio_inputs_valid(graph: &GraphTemplate, layout: &AudioInputLayout) -> bool {
    graph
        .nodes
        .iter()
        .flat_map(|node| node.inputs.iter().map(|input| input.source))
        .chain(graph.outputs.iter().copied())
        .chain(
            graph
                .sends
                .iter()
                .flat_map(|send| send.outputs.iter().map(|input| input.source)),
        )
        .all(|source| match source {
            Source::ExternalAudio { input, channel } => layout
                .spec(input)
                .is_some_and(|spec| channel < spec.channels),
            _ => true,
        })
}

fn graph_controls_valid(graph: &GraphTemplate, layout: &ControlLayout) -> bool {
    graph.nodes.iter().all(|node| {
        let op_valid = match node.op {
            Op::ControlWrite { target } => layout.spec(target).is_some(),
            _ => true,
        };
        op_valid
            && node.inputs.iter().all(|input| match input.source {
                Source::Control(control) => layout.spec(control).is_some(),
                _ => true,
            })
    }) && graph.outputs.iter().all(|source| match source {
        Source::Control(control) => layout.spec(*control).is_some(),
        _ => true,
    }) && graph.sends.iter().all(|send| {
        send.outputs.iter().all(|input| match input.source {
            Source::Control(control) => layout.spec(control).is_some(),
            _ => true,
        })
    })
}

#[derive(Clone, Copy)]
struct ArenaView<'a> {
    controls: &'a ControlLayout,
    audio_inputs: &'a AudioInputLayout,
    buses: &'a BusLayout,
}

impl<'a> ArenaView<'a> {
    fn new(program: &'a Program) -> ArenaView<'a> {
        ArenaView {
            controls: &program.controls,
            audio_inputs: &program.audio_inputs,
            buses: &program.buses,
        }
    }
}

fn graph_compatible_across_arenas(
    left: &GraphTemplate,
    right: &GraphTemplate,
    left_arena: ArenaView<'_>,
    right_arena: ArenaView<'_>,
) -> bool {
    left.inputs == right.inputs
        && left.params == right.params
        && left.nodes.len() == right.nodes.len()
        && left.nodes.iter().zip(&right.nodes).all(|(left, right)| {
            ops_compatible(
                &left.op,
                &right.op,
                left_arena.controls,
                right_arena.controls,
            ) && left.tail == right.tail
                && sources_compatible(
                    left.inputs.iter().map(|input| input.source),
                    right.inputs.iter().map(|input| input.source),
                    left_arena.controls,
                    right_arena.controls,
                    left_arena.audio_inputs,
                    right_arena.audio_inputs,
                )
        })
        && sources_compatible(
            left.outputs.iter().copied(),
            right.outputs.iter().copied(),
            left_arena.controls,
            right_arena.controls,
            left_arena.audio_inputs,
            right_arena.audio_inputs,
        )
        && left.sends.len() == right.sends.len()
        && left.sends.iter().zip(&right.sends).all(|(left, right)| {
            left_arena
                .buses
                .corresponding_id(left.bus, right_arena.buses)
                == Some(right.bus)
                && sources_compatible(
                    left.outputs.iter().map(|input| input.source),
                    right.outputs.iter().map(|input| input.source),
                    left_arena.controls,
                    right_arena.controls,
                    left_arena.audio_inputs,
                    right_arena.audio_inputs,
                )
        })
}

fn ops_compatible(
    left: &Op,
    right: &Op,
    left_controls: &ControlLayout,
    right_controls: &ControlLayout,
) -> bool {
    match (left, right) {
        (
            Op::ControlWrite {
                target: left_target,
            },
            Op::ControlWrite {
                target: right_target,
            },
        ) => left_controls.corresponding_id(*left_target, right_controls) == Some(*right_target),
        _ => left == right,
    }
}

fn sources_compatible(
    left: impl IntoIterator<Item = Source>,
    right: impl IntoIterator<Item = Source>,
    left_controls: &ControlLayout,
    right_controls: &ControlLayout,
    left_audio_inputs: &AudioInputLayout,
    right_audio_inputs: &AudioInputLayout,
) -> bool {
    let mut left = left.into_iter();
    let mut right = right.into_iter();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(Source::Control(left)), Some(Source::Control(right))) => {
                if left_controls.corresponding_id(left, right_controls) != Some(right) {
                    return false;
                }
            }
            (
                Some(Source::ExternalAudio {
                    input: left,
                    channel: left_channel,
                }),
                Some(Source::ExternalAudio {
                    input: right,
                    channel: right_channel,
                }),
            ) => {
                if left_channel != right_channel
                    || left_audio_inputs.corresponding_id(left, right_audio_inputs) != Some(right)
                {
                    return false;
                }
            }
            (Some(left), Some(right)) if left == right => {}
            _ => return false,
        }
    }
}

fn rebind_graph_arenas(
    graph: &mut GraphTemplate,
    from_controls: &ControlLayout,
    to_controls: &ControlLayout,
    from_audio_inputs: &AudioInputLayout,
    to_audio_inputs: &AudioInputLayout,
    from_buses: &BusLayout,
    to_buses: &BusLayout,
) -> bool {
    for node in &mut graph.nodes {
        if let Op::ControlWrite { target } = &mut node.op {
            let Some(rebound) = from_controls.corresponding_id(*target, to_controls) else {
                return false;
            };
            *target = rebound;
        }
        for input in &mut node.inputs {
            if !rebind_source(
                &mut input.source,
                from_controls,
                to_controls,
                from_audio_inputs,
                to_audio_inputs,
            ) {
                return false;
            }
        }
    }
    for output in &mut graph.outputs {
        if !rebind_source(
            output,
            from_controls,
            to_controls,
            from_audio_inputs,
            to_audio_inputs,
        ) {
            return false;
        }
    }
    for send in &mut graph.sends {
        let Some(bus) = from_buses.corresponding_id(send.bus, to_buses) else {
            return false;
        };
        send.bus = bus;
        for output in &mut send.outputs {
            if !rebind_source(
                &mut output.source,
                from_controls,
                to_controls,
                from_audio_inputs,
                to_audio_inputs,
            ) {
                return false;
            }
        }
    }
    true
}

fn rebind_source(
    source: &mut Source,
    from_controls: &ControlLayout,
    to_controls: &ControlLayout,
    from_audio_inputs: &AudioInputLayout,
    to_audio_inputs: &AudioInputLayout,
) -> bool {
    match source {
        Source::Control(control) => {
            let Some(rebound) = from_controls.corresponding_id(*control, to_controls) else {
                return false;
            };
            *control = rebound;
            true
        }
        Source::ExternalAudio { input, .. } => {
            let Some(rebound) = from_audio_inputs.corresponding_id(*input, to_audio_inputs) else {
                return false;
            };
            *input = rebound;
            true
        }
        _ => true,
    }
}

impl Default for Program {
    fn default() -> Self {
        Program {
            tempo: TempoMap::constant(120.0, 4.0).expect("the default score tempo is valid"),
            key: None,
            controls: ControlLayout::new(),
            control_inputs: Vec::new(),
            audio_inputs: AudioInputLayout::new(),
            // First-slice policy, not an IR invariant. This is primarily the
            // silent initial value used by RevisionSlot; a later host-facing
            // program declaration should choose its main layout explicitly.
            buses: BusLayout::new(2).expect("the default stereo layout is valid"),
            voices: Vec::new(),
            patches: Vec::new(),
            patch_controls: Vec::new(),
            runs: Vec::new(),
            tracks: Vec::new(),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum ProgramError {
    VoiceTemplate {
        index: usize,
        source: TemplateError,
    },
    VoiceLimits {
        index: usize,
        source: GraphLimitError,
    },
    PatchLimits {
        index: usize,
        source: GraphLimitError,
    },
    UnknownVoiceAudioInput {
        index: usize,
    },
    UnknownVoiceControl {
        index: usize,
    },
    UnknownPatchAudioInput {
        index: usize,
    },
    UnknownPatchControl {
        index: usize,
    },
    PatchControlLayout,
    UnknownVoiceBus {
        index: usize,
    },
    VoiceBusChannels {
        index: usize,
        expected: usize,
        found: usize,
    },
    MissingTrackVoice {
        index: usize,
    },
    UnknownTrackBus {
        index: usize,
    },
    UnknownTrackTrigger {
        index: usize,
    },
    UnknownTrackOnsetBinding {
        index: usize,
    },
    OnsetBindingsRequireTrigger {
        index: usize,
    },
    UnknownTrackExternalControl {
        index: usize,
    },
    ExternalModifiersRequireTrigger {
        index: usize,
    },
    TrackBusChannels {
        index: usize,
        expected: usize,
        found: usize,
    },
    TrackValues {
        index: usize,
        source: ValueLimitError,
    },
    MissingRunPatch {
        index: usize,
    },
    InvalidRunSpan {
        index: usize,
    },
    TimedRunInputs {
        index: usize,
    },
    UnknownRunBus {
        index: usize,
    },
    RunBusChannels {
        index: usize,
        expected: usize,
        found: usize,
    },
}

impl core::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProgramError::VoiceTemplate { index, source } => {
                write!(f, "voice {index} is invalid: {source}")
            }
            ProgramError::VoiceLimits { index, source } => {
                write!(f, "voice {index} exceeds publication limits: {source}")
            }
            ProgramError::PatchLimits { index, source } => {
                write!(f, "patch {index} exceeds publication limits: {source}")
            }
            ProgramError::UnknownVoiceAudioInput { index } => {
                write!(
                    f,
                    "voice {index} refers to an audio input outside this program"
                )
            }
            ProgramError::UnknownVoiceControl { index } => {
                write!(f, "voice {index} refers to a control outside this program")
            }
            ProgramError::UnknownPatchAudioInput { index } => {
                write!(
                    f,
                    "patch {index} refers to an audio input outside this program"
                )
            }
            ProgramError::UnknownPatchControl { index } => {
                write!(f, "patch {index} refers to a control outside this program")
            }
            ProgramError::PatchControlLayout => {
                write!(f, "patch controls do not belong to this program")
            }
            ProgramError::UnknownVoiceBus { index } => {
                write!(f, "voice {index} sends to a bus outside this program")
            }
            ProgramError::VoiceBusChannels {
                index,
                expected,
                found,
            } => write!(
                f,
                "voice {index} sends {found} channels to a {expected}-channel bus"
            ),
            ProgramError::MissingTrackVoice { index } => {
                write!(f, "track {index} refers to a missing voice")
            }
            ProgramError::UnknownTrackBus { index } => {
                write!(f, "track {index} sends to a bus outside this program")
            }
            ProgramError::UnknownTrackTrigger { index } => {
                write!(f, "track {index} uses an onset source outside this program")
            }
            ProgramError::UnknownTrackOnsetBinding { index } => {
                write!(
                    f,
                    "track {index} has an invalid onset-sampled control binding"
                )
            }
            ProgramError::OnsetBindingsRequireTrigger { index } => {
                write!(
                    f,
                    "track {index} samples live controls at onset but has no realtime trigger"
                )
            }
            ProgramError::UnknownTrackExternalControl { index } => {
                write!(f, "track {index} has an invalid external-onset modifier")
            }
            ProgramError::ExternalModifiersRequireTrigger { index } => {
                write!(
                    f,
                    "track {index} has external-onset modifiers but no realtime trigger"
                )
            }
            ProgramError::TrackBusChannels {
                index,
                expected,
                found,
            } => write!(
                f,
                "track {index} sends {found} channels to a {expected}-channel bus"
            ),
            ProgramError::TrackValues { index, source } => {
                write!(f, "track {index} has invalid control values: {source}")
            }
            ProgramError::MissingRunPatch { index } => {
                write!(f, "run {index} refers to a missing patch")
            }
            ProgramError::InvalidRunSpan { index } => {
                write!(f, "run {index} has an empty or reversed finite span")
            }
            ProgramError::TimedRunInputs { index } => write!(
                f,
                "run {index} is finite but its patch consumes audio inputs; \
                 timed whole-stem processors are not implemented"
            ),
            ProgramError::UnknownRunBus { index } => {
                write!(f, "run {index} sends to a bus outside this program")
            }
            ProgramError::RunBusChannels {
                index,
                expected,
                found,
            } => write!(
                f,
                "run {index} sends {found} channels to a {expected}-channel bus"
            ),
        }
    }
}

impl core::error::Error for ProgramError {}
