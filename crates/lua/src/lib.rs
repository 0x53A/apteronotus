//! Sandboxed Lua evaluation for Apteronotus.
//!
//! This crate is the only layer that knows both Lua and the engine's data
//! structures. Evaluation happens once per explicit edit and returns an owned
//! [`Program`]. The Piccolo VM, its closures, and all userdata are dropped
//! before the program can reach a scheduler or audio thread.

mod bindings;
mod error;
mod program;
mod source;

pub use error::EvalError;
pub use program::{PatchId, Program, ProgramError, Track, VoiceId};

use apteronotus_pattern::ValueLimits;
use apteronotus_synth::GraphLimits;
use bindings::{BuildState, PRELUDE, install};
use piccolo::{Closure, Executor, Fuel, Lua};
use source::inject_call_sites;
use std::{cell::RefCell, rc::Rc};

/// Resource ceilings for one evaluation.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Limits {
    /// Approximate Lua VM instructions.
    pub fuel: u32,
    /// Bytes allocated inside Piccolo's GC arena.
    pub memory_bytes: usize,
    /// UTF-8 bytes accepted as one edit.
    pub source_bytes: usize,
    /// Graph nodes built across the whole candidate evaluation.
    ///
    /// Failed graphs caught by Lua `pcall` do not refund this budget; otherwise
    /// a loop could evade the ceiling by repeatedly building and aborting.
    pub graph_nodes: usize,
    /// Per-template publication limits, checked after ordinary graph
    /// validation and independently of construction accounting.
    pub graph_publication: GraphLimits,
    /// Pattern AST nodes allocated across parsed values and stored tracks.
    pub pattern_nodes: usize,
    /// Maximum map width and curve terms in one pattern event.
    pub pattern_values: ValueLimits,
    pub voices: usize,
    pub patches: usize,
    pub controls: usize,
    pub buses: usize,
    pub tracks: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel: 1_000_000,
            memory_bytes: 8 * 1024 * 1024,
            source_bytes: 1024 * 1024,
            graph_nodes: 20_000,
            graph_publication: GraphLimits {
                nodes: 20_000,
                connections: 100_000,
                input_channels: 256,
                output_channels: 256,
                delay_buffer_seconds: 600.0,
                tail_seconds: 600.0,
            },
            pattern_nodes: 200_000,
            pattern_values: ValueLimits::default(),
            voices: 256,
            patches: 256,
            controls: 2_048,
            buses: 256,
            tracks: 2_048,
        }
    }
}

/// Evaluates source into a candidate [`Program`].
///
/// Constructing a new VM per edit gives evaluation transactional semantics:
/// on any parse, runtime, validation, or budget error, the partially built
/// candidate is dropped and the caller's currently playing program is
/// untouched.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Evaluator {
    limits: Limits,
}

impl Evaluator {
    pub fn new(limits: Limits) -> Self {
        Self { limits }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn evaluate(&self, source: &str) -> Result<Program, EvalError> {
        if source.len() > self.limits.source_bytes {
            return Err(EvalError::SourceLimit {
                used: source.len(),
                limit: self.limits.source_bytes,
            });
        }

        let state = Rc::new(RefCell::new(BuildState::new(self.limits)));
        let mut lua = Lua::core();
        lua.try_enter(|ctx| install(ctx, state.clone()))?;
        let prelude = lua.try_enter(|ctx| {
            let closure = Closure::load(ctx, Some("apteronotus prelude"), PRELUDE.as_bytes())?;
            Ok(ctx.stash(Executor::start(ctx, closure.into(), ())))
        })?;
        lua.execute::<()>(&prelude)?;

        let attributed_source = inject_call_sites(source);
        let executor = lua.try_enter(|ctx| {
            let closure = Closure::load(ctx, Some("edit"), attributed_source.as_bytes())?;
            Ok(ctx.stash(Executor::start(ctx, closure.into(), ())))
        })?;

        let mut remaining = self.limits.fuel;
        loop {
            if lua.total_memory() > self.limits.memory_bytes {
                return Err(EvalError::MemoryLimit {
                    used: lua.total_memory(),
                    limit: self.limits.memory_bytes,
                });
            }
            if remaining == 0 {
                return Err(EvalError::FuelLimit {
                    limit: self.limits.fuel,
                });
            }

            let slice = remaining.min(4_096) as i32;
            let mut fuel = Fuel::with(slice);
            let done = lua
                .enter(|ctx| ctx.fetch(&executor).step(ctx, &mut fuel))
                .map_err(|error| EvalError::Binding(error.to_string()))?;
            let consumed = slice.saturating_sub(fuel.remaining().max(0)) as u32;
            remaining = remaining.saturating_sub(consumed.max(1));

            if lua.total_memory() > self.limits.memory_bytes {
                return Err(EvalError::MemoryLimit {
                    used: lua.total_memory(),
                    limit: self.limits.memory_bytes,
                });
            }

            if done {
                break;
            }
        }

        lua.try_enter(|ctx| {
            let executor = ctx.fetch(&executor);
            executor.take_result::<()>(ctx)??;
            Ok(())
        })?;

        let program = {
            let mut state = state.borrow_mut();
            if state.active.is_some() {
                return Err(EvalError::Binding(
                    "evaluation ended while a graph was still being staged".into(),
                ));
            }
            std::mem::take(&mut state.program)
        };
        program
            .validate_with_value_limits(self.limits.graph_publication, self.limits.pattern_values)
            .map_err(|error| EvalError::Binding(error.to_string()))?;
        Ok(program)
    }
}

/// Evaluate with [`Limits::default`].
pub fn evaluate(source: &str) -> Result<Program, EvalError> {
    Evaluator::default().evaluate(source)
}
