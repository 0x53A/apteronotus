//! Apteronotus — the pattern algebra.
//!
//! A pattern is a pure function from a span of cycle time to the events inside
//! it. Nothing here knows about audio, files, threads or the clock; the only
//! way in is [`mini::parse`] and the only way out is [`Pattern::query`]. That
//! is deliberate — it is what keeps the whole thing testable without a sound
//! card, and what will let the engine be lifted behind an ABI later without
//! disturbing anything above it.
//!
//! ```
//! use apteronotus_pattern::{mini, Frac, Span};
//!
//! let p = mini::parse("bd*2 [~ sd]").unwrap();
//! let hits = p.onsets(Span::cycle(0));
//! assert_eq!(hits.len(), 3);
//! assert_eq!(hits[0].part.begin, Frac::ZERO);
//! assert_eq!(hits[2].part.begin, Frac::new(3, 4));
//! ```
//!
//! Two invariants everything else depends on:
//!
//! * **Queries are pure.** Asking the same question twice gives the same
//!   answer, and asking about a sub-window gives exactly that window's share.
//!   The scheduler runs ahead of the audio clock while the editor asks about
//!   *now*; if a query moved a cursor or drew from a global generator, those
//!   two would disagree. Note that purity is not deduplication: overlapping
//!   windows return the shared onsets twice, which is why a scheduler must
//!   advance a non-overlapping frontier. See [`Pattern::onsets`].
//! * **`whole` and `part` are different things.** `part` is the fragment the
//!   query saw; `whole` is the event's real extent. A voice triggers on
//!   [`Event::has_onset`], so a window that bisects a note continues it rather
//!   than restriking it.

pub mod event;
pub mod frac;
pub mod mini;
pub mod pattern;
pub mod rand;
pub mod span;

pub use event::{Event, SrcSpan, Value};
pub use frac::Frac;
pub use mini::{ParseError, parse};
pub use pattern::{Pattern, Signal, bjorklund};
pub use span::Span;
