//! Transactional, generation-aware publication at musical boundaries.
//!
//! This module is generic over the eventual `Program` type. It owns no editor
//! and performs no evaluation; it enforces the runtime half of the contract:
//! failed validation changes nothing, successful candidates receive monotonic
//! generations, and activation happens only when the scheduler frontier reaches
//! an exact cycle coordinate.

use apteronotus_pattern::Frac;
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Generation(u64);

impl Generation {
    pub const INITIAL: Generation = Generation(0);

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub struct Revision<T> {
    pub generation: Generation,
    pub effective_at: Frac,
    pub program: Arc<T>,
}

impl<T> Clone for Revision<T> {
    fn clone(&self) -> Self {
        Revision {
            generation: self.generation,
            effective_at: self.effective_at,
            program: Arc::clone(&self.program),
        }
    }
}

pub struct RevisionSlot<T> {
    active: Revision<T>,
    pending: VecDeque<Revision<T>>,
    next_generation: u64,
}

impl<T> RevisionSlot<T> {
    pub fn new(program: T, effective_at: Frac) -> RevisionSlot<T> {
        RevisionSlot {
            active: Revision {
                generation: Generation::INITIAL,
                effective_at,
                program: Arc::new(program),
            },
            pending: VecDeque::new(),
            next_generation: 1,
        }
    }

    pub fn active(&self) -> &Revision<T> {
        &self.active
    }

    pub fn pending(&self) -> impl ExactSizeIterator<Item = &Revision<T>> {
        self.pending.iter()
    }

    /// Validate and queue one candidate as a single control-thread operation.
    ///
    /// `validate` may allocate and perform unbounded work. It runs before the
    /// slot changes, so an error leaves the active and pending revisions
    /// byte-for-byte untouched.
    pub fn submit<E>(
        &mut self,
        program: T,
        effective_at: Frac,
        validate: impl FnOnce(&T) -> Result<(), E>,
    ) -> Result<Generation, SubmitError<E>> {
        validate(&program).map_err(SubmitError::Validation)?;
        let not_before = self
            .pending
            .back()
            .map(|revision| revision.effective_at)
            .unwrap_or(self.active.effective_at);
        if effective_at < not_before {
            return Err(SubmitError::OutOfOrder {
                effective_at,
                not_before,
            });
        }

        let generation = Generation(self.next_generation);
        self.next_generation = self.next_generation.saturating_add(1);
        self.pending.push_back(Revision {
            generation,
            effective_at,
            program: Arc::new(program),
        });
        Ok(generation)
    }

    /// Activate every queued revision whose boundary has been reached.
    ///
    /// Returning a clone lets the scheduler retain the exact revision it
    /// queried while already-sounding voices retain their own `Arc`s.
    pub fn advance_to(&mut self, frontier: Frac) -> Revision<T> {
        while self
            .pending
            .front()
            .is_some_and(|revision| revision.effective_at <= frontier)
        {
            self.active = self.pending.pop_front().expect("front was present");
        }
        self.active.clone()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SubmitError<E> {
    Validation(E),
    OutOfOrder {
        effective_at: Frac,
        not_before: Frac,
    },
}

impl<E: core::fmt::Display> core::fmt::Display for SubmitError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SubmitError::Validation(error) => error.fmt(f),
            SubmitError::OutOfOrder {
                effective_at,
                not_before,
            } => write!(
                f,
                "revision at {effective_at} precedes the queued boundary {not_before}"
            ),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> core::error::Error for SubmitError<E> {}
