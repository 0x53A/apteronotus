// fundsp's type-level channel arithmetic needs this depth on newer rustc.
#![recursion_limit = "256"]

//! Apteronotus — the synthesis layer.
//!
//! A voice is staged **once per edit** as a [`GraphTemplate`] of plain data,
//! and instantiated **once per note** onto fundsp. That split is the whole
//! architecture of this crate, and it is why `graph = function(n)` may contain
//! loops and tables but not an `if` on a note value: while it runs, `n.hz` is
//! symbolic.
//!
//! ```
//! use apteronotus_synth::{Adsr, GraphBuilder, Note, lower, n};
//!
//! // Stage: n.hz is symbolic here, and no number is bound yet.
//! let mut g = GraphBuilder::new();
//! let tone = g.sine(n::HZ);
//! let env = g.adsr(Adsr::new(0.005, 0.08, 0.6, 0.15));
//! let out = g.mul(tone, env);
//! let voice = g.out_panned(out).unwrap();
//!
//! // Instantiate: one note, one graph.
//! let note = Note::new(440.0).duration(0.25);
//! let mut unit = lower::instantiate(&voice, &note).unwrap();
//! let audio = lower::render(unit.as_mut(), 48_000.0, 0.2);
//! assert!(lower::rms(&audio[0]) > 0.01);
//! ```
//!
//! This crate never learns that a scripting language exists, in the same way
//! `pattern` never learns that fundsp does. That seam is what keeps the engine
//! liftable behind an ABI later without disturbing anything above it.

mod adsr;
mod analyzer;
pub mod builder;
pub mod control;
mod fdn;
mod flue;
mod harmonic;
pub mod input;
pub mod instrument;
pub mod lower;
pub mod note;
pub mod routing;
pub mod stdlib;
mod string;
pub mod template;

pub use builder::{FdnConfig, GraphBuilder, n};
pub use control::{ControlError, ControlId, ControlLayout, ControlSpec};
pub use input::{
    AudioInputError, AudioInputFallback, AudioInputId, AudioInputLayout, AudioInputSpec,
};
pub use instrument::{InstrumentLifetime, PatchError, PatchTemplate};
pub use lower::{
    ControlStore, LowerError, instantiate, instantiate_patch, instantiate_patch_routed,
    instantiate_patch_routed_at, instantiate_patch_routed_with_audio_inputs,
    instantiate_patch_routed_with_audio_inputs_at, instantiate_routed,
    instantiate_routed_with_controls, instantiate_timed_patch_routed,
    instantiate_timed_patch_routed_with_routing, instantiate_with_controls,
};
pub use note::{Note, ParamValue, ParamValueError};
pub use routing::{BusId, BusLayout, DuckControl, EventRouting, EventSend, RoutingError};
pub use template::{
    Adsr, Basis, Curve, CurveClock, CurveTerm, DelayRange, DelayRangeError, GraphCost,
    GraphLimitError, GraphLimits, GraphSend, GraphTemplate, Implicit, InitControlBinding,
    InitScalar, InitScalarError, Input, Lifetime, Node, NodeId, ONSET_PULSE_SECONDS, Op, ParamId,
    ParamSpec, ShapeKind, Source, TemplateError, TransportSlot,
};
