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

pub mod builder;
pub mod lower;
pub mod note;
pub mod template;

pub use builder::{GraphBuilder, n};
pub use lower::instantiate;
pub use note::Note;
pub use template::{
    Adsr, Basis, Curve, CurveTerm, GraphTemplate, Implicit, Input, Node, NodeId, Op, ParamId,
    ParamSpec, ShapeKind, Source, TemplateError,
};
