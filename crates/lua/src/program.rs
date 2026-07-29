use apteronotus_pattern::{Pattern, ValueLimitError, ValueLimits};
use apteronotus_synth::{
    BusLayout, ControlLayout, GraphLimitError, GraphLimits, GraphTemplate, PatchTemplate, Source,
    TemplateError,
};

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
}

/// The data-only result of one successful edit evaluation.
#[derive(Clone, PartialEq, Debug)]
pub struct Program {
    pub controls: ControlLayout,
    pub buses: BusLayout,
    pub voices: Vec<GraphTemplate>,
    pub patches: Vec<PatchTemplate>,
    pub runs: Vec<PatchId>,
    pub tracks: Vec<Track>,
}

impl Program {
    pub fn voice(&self, id: VoiceId) -> Option<&GraphTemplate> {
        self.voices.get(id.0)
    }

    pub fn patch(&self, id: PatchId) -> Option<&PatchTemplate> {
        self.patches.get(id.0)
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
            && self.runs.len() == other.runs.len()
            && self.runs.iter().zip(&other.runs).all(|(left, right)| {
                match (self.patch(*left), other.patch(*right)) {
                    (Some(left), Some(right)) => graph_compatible_across_arenas(
                        left.graph(),
                        right.graph(),
                        &self.controls,
                        &other.controls,
                        &self.buses,
                        &other.buses,
                    ),
                    _ => false,
                }
            })
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
        for (candidate_run, active_run) in self.runs.iter().zip(&active.runs) {
            let Some(active_patch) = active.patch(*active_run) else {
                return false;
            };
            let Some(candidate_patch) = patches.get_mut(candidate_run.0) else {
                return false;
            };
            *candidate_patch = active_patch.clone();
        }

        self.voices = voices;
        self.controls = active.controls.clone();
        self.buses = active.buses.clone();
        self.patches = patches;
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
        }
        for (index, track) in self.tracks.iter().enumerate() {
            if self.voice(track.voice).is_none() {
                return Err(ProgramError::MissingTrackVoice { index });
            }
            track
                .pattern
                .validate_value_limits(value_limits)
                .map_err(|source| ProgramError::TrackValues { index, source })?;
        }
        for (index, patch) in self.runs.iter().enumerate() {
            if self.patch(*patch).is_none() {
                return Err(ProgramError::MissingRunPatch { index });
            }
        }
        Ok(())
    }
}

fn graph_compatible_across_arenas(
    left: &GraphTemplate,
    right: &GraphTemplate,
    left_controls: &ControlLayout,
    right_controls: &ControlLayout,
    left_buses: &BusLayout,
    right_buses: &BusLayout,
) -> bool {
    left.inputs == right.inputs
        && left.params == right.params
        && left.nodes.len() == right.nodes.len()
        && left.nodes.iter().zip(&right.nodes).all(|(left, right)| {
            left.op == right.op
                && left.tail == right.tail
                && sources_compatible(
                    left.inputs.iter().map(|input| input.source),
                    right.inputs.iter().map(|input| input.source),
                    left_controls,
                    right_controls,
                )
        })
        && sources_compatible(
            left.outputs.iter().copied(),
            right.outputs.iter().copied(),
            left_controls,
            right_controls,
        )
        && left.sends.len() == right.sends.len()
        && left.sends.iter().zip(&right.sends).all(|(left, right)| {
            left_buses.corresponding_id(left.bus, right_buses) == Some(right.bus)
                && sources_compatible(
                    left.outputs.iter().map(|input| input.source),
                    right.outputs.iter().map(|input| input.source),
                    left_controls,
                    right_controls,
                )
        })
}

fn sources_compatible(
    left: impl IntoIterator<Item = Source>,
    right: impl IntoIterator<Item = Source>,
    left_controls: &ControlLayout,
    right_controls: &ControlLayout,
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
            (Some(left), Some(right)) if left == right => {}
            _ => return false,
        }
    }
}

fn rebind_graph_arenas(
    graph: &mut GraphTemplate,
    from_controls: &ControlLayout,
    to_controls: &ControlLayout,
    from_buses: &BusLayout,
    to_buses: &BusLayout,
) -> bool {
    for node in &mut graph.nodes {
        for input in &mut node.inputs {
            if !rebind_source(&mut input.source, from_controls, to_controls) {
                return false;
            }
        }
    }
    for output in &mut graph.outputs {
        if !rebind_source(output, from_controls, to_controls) {
            return false;
        }
    }
    for send in &mut graph.sends {
        let Some(bus) = from_buses.corresponding_id(send.bus, to_buses) else {
            return false;
        };
        send.bus = bus;
        for output in &mut send.outputs {
            if !rebind_source(&mut output.source, from_controls, to_controls) {
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
) -> bool {
    let Source::Control(control) = source else {
        return true;
    };
    let Some(rebound) = from_controls.corresponding_id(*control, to_controls) else {
        return false;
    };
    *control = rebound;
    true
}

impl Default for Program {
    fn default() -> Self {
        Program {
            controls: ControlLayout::new(),
            // First-slice policy, not an IR invariant. This is primarily the
            // silent initial value used by RevisionSlot; a later host-facing
            // program declaration should choose its main layout explicitly.
            buses: BusLayout::new(2).expect("the default stereo layout is valid"),
            voices: Vec::new(),
            patches: Vec::new(),
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
    TrackValues {
        index: usize,
        source: ValueLimitError,
    },
    MissingRunPatch {
        index: usize,
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
            ProgramError::TrackValues { index, source } => {
                write!(f, "track {index} has invalid control values: {source}")
            }
            ProgramError::MissingRunPatch { index } => {
                write!(f, "run {index} refers to a missing patch")
            }
        }
    }
}

impl core::error::Error for ProgramError {}
