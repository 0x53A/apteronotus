//! Apteronotus — typed pitch.
//!
//! Deliberately *not* inside `pattern`, which has no musical domain knowledge
//! and should keep it that way. Also deliberately much smaller than this crate
//! will eventually be: scales, modes, chord symbols and voicing dictionaries
//! are not here, because where their boundary falls — expanded at construction
//! time, or surviving as runtime nodes because a song patterns their inputs —
//! is still an open question. Pitch ↔ frequency is not open, so it is built.
//!
//! ```
//! use apteronotus_music::Pitch;
//!
//! assert_eq!(Pitch::parse("a4").unwrap().midi(), 69.0);
//! assert_eq!(Pitch::parse("c4").unwrap().hz().round(), 262.0);
//! ```

pub mod pitch;

pub use pitch::{Pitch, PitchError, hz_to_midi, midi_to_hz};
