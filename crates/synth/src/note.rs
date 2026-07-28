//! What a scheduled onset hands to a template.
//!
//! A voice is a *function from note to graph*, not a graph with knobs. `n.hz`
//! is baked in when the unit is instantiated, and that is what gives real
//! polyphony: a persistent graph with `Shared` control values is a monosynth
//! wearing a costume. `Shared` keeps its place for what it is genuinely good
//! at — global, continuously varying controls that outlive any note.

use crate::template::{GraphTemplate, Implicit, ParamId};

/// The concrete values one onset supplies.
#[derive(Clone, PartialEq, Debug)]
pub struct Note {
    pub hz: f64,
    pub velocity: f64,
    /// Length in seconds. Known here because the sequencer scheduled it, which
    /// is what lets envelopes be note-clock functions rather than gate
    /// followers.
    pub duration: f64,
    pub pan: f64,
    /// Derived from event provenance — never a counter.
    ///
    /// `neon.eod` reproduces analogue component tolerance as deterministic
    /// per-voice drift, so anything a voice seeds from must be a pure function
    /// of position. A counter would change with query chunking, with offline
    /// versus live scheduling, and with the order a `Stack`'s members come back
    /// in. Addressing a *sounding* voice is a different job with a different
    /// token, and that one may never be seeded from.
    pub seed: u64,
    /// Declared parameters by index, `None` falling back to the declaration's
    /// default.
    declared: Vec<Option<f64>>,
}

impl Note {
    pub fn new(hz: f64) -> Note {
        Note {
            hz,
            velocity: Implicit::Velocity.default_value(),
            duration: Implicit::Duration.default_value(),
            pan: Implicit::Pan.default_value(),
            seed: 0,
            declared: Vec::new(),
        }
    }

    pub fn velocity(mut self, velocity: f64) -> Note {
        self.velocity = velocity;
        self
    }

    pub fn duration(mut self, seconds: f64) -> Note {
        self.duration = seconds;
        self
    }

    pub fn pan(mut self, pan: f64) -> Note {
        self.pan = pan;
        self
    }

    pub fn seed(mut self, seed: u64) -> Note {
        self.seed = seed;
        self
    }

    /// Set a declared parameter by index.
    pub fn set(mut self, index: usize, value: f64) -> Note {
        if self.declared.len() <= index {
            self.declared.resize(index + 1, None);
        }
        self.declared[index] = Some(value);
        self
    }

    /// Resolve a symbolic source against this note, clamping declared values to
    /// their declared range.
    pub fn value(&self, id: ParamId, template: &GraphTemplate) -> f64 {
        match id {
            ParamId::Implicit(Implicit::Hz) => self.hz,
            ParamId::Implicit(Implicit::Velocity) => self.velocity,
            ParamId::Implicit(Implicit::Duration) => self.duration,
            ParamId::Implicit(Implicit::Pan) => self.pan,
            ParamId::Declared(i) => {
                let spec = &template.params[i];
                let raw = self
                    .declared
                    .get(i)
                    .copied()
                    .flatten()
                    .unwrap_or(spec.default);
                spec.clamp(raw)
            }
        }
    }
}
