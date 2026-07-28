//! Explicit instrument lifetimes.

use crate::{GraphTemplate, Op, ParamId, Source, TemplateError};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstrumentLifetime {
    Voice,
    Patch,
}

/// One persistent graph instance.
///
/// A patch has no note clock and no per-onset values. Its writable inputs are
/// program-scope [`crate::ControlId`] handles, and the concrete unit is created
/// once and kept alive by the host.
#[derive(Clone, PartialEq, Debug)]
pub struct PatchTemplate {
    graph: GraphTemplate,
}

impl PatchTemplate {
    pub fn new(graph: GraphTemplate) -> Result<PatchTemplate, PatchError> {
        graph.validate()?;
        if !graph.params.is_empty() {
            return Err(PatchError::VoiceParameters);
        }
        for node in &graph.nodes {
            if matches!(node.op, Op::Adsr(_) | Op::Curve(_) | Op::InitRandom { .. }) {
                return Err(PatchError::NoteClockNode);
            }
            for input in &node.inputs {
                check_source(input.source)?;
            }
        }
        for source in &graph.outputs {
            check_source(*source)?;
        }
        for send in &graph.sends {
            for input in &send.outputs {
                check_source(input.source)?;
            }
        }
        Ok(PatchTemplate { graph })
    }

    pub fn graph(&self) -> &GraphTemplate {
        &self.graph
    }
}

fn check_source(source: Source) -> Result<(), PatchError> {
    match source {
        Source::Param(ParamId::Implicit(_)) => Err(PatchError::VoiceInput),
        Source::Param(ParamId::Declared(_)) => Err(PatchError::VoiceParameters),
        _ => Ok(()),
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum PatchError {
    Template(TemplateError),
    VoiceInput,
    VoiceParameters,
    NoteClockNode,
}

impl From<TemplateError> for PatchError {
    fn from(error: TemplateError) -> PatchError {
        PatchError::Template(error)
    }
}

impl core::fmt::Display for PatchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PatchError::Template(error) => error.fmt(f),
            PatchError::VoiceInput => write!(f, "persistent patch refers to a per-note input"),
            PatchError::VoiceParameters => {
                write!(f, "persistent patch declares per-note parameters")
            }
            PatchError::NoteClockNode => {
                write!(
                    f,
                    "persistent patch contains a per-note envelope, curve, or initializer"
                )
            }
        }
    }
}

impl core::error::Error for PatchError {}
