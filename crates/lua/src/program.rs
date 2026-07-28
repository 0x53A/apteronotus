use apteronotus_pattern::Pattern;
use apteronotus_synth::{
    BusLayout, ControlLayout, GraphLimitError, GraphLimits, GraphTemplate, PatchTemplate,
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

    /// Validate one complete candidate before publication.
    ///
    /// Evaluation performs the same graph checks while staging. This boundary
    /// deliberately repeats them so a future cached/non-Lua `Program` cannot
    /// bypass host policy.
    pub fn validate(&self, limits: GraphLimits) -> Result<(), ProgramError> {
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
        }
        for (index, patch) in self.runs.iter().enumerate() {
            if self.patch(*patch).is_none() {
                return Err(ProgramError::MissingRunPatch { index });
            }
        }
        Ok(())
    }
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
            ProgramError::MissingRunPatch { index } => {
                write!(f, "run {index} refers to a missing patch")
            }
        }
    }
}

impl core::error::Error for ProgramError {}
