//! Apteronotus — typed pitch.
//!
//! Deliberately *not* inside `pattern`, which has no musical domain knowledge
//! and should keep it that way. Literal chord symbols, anchors, and a compact
//! named voicing dictionary expand here at construction time. Patterned music
//! inputs still require a runtime-node design and are deliberately not
//! disguised as this literal path.
//!
//! ```
//! use apteronotus_music::Pitch;
//!
//! assert_eq!(Pitch::parse("a4").unwrap().midi(), 69.0);
//! assert_eq!(Pitch::parse("c4").unwrap().hz().round(), 262.0);
//! ```

pub mod chord;
pub mod key;
pub mod pitch;

pub use chord::{Chord, ChordError, VoicingShape};
pub use key::{Key, KeyError, Mode, PitchClass};
pub use pitch::{Pitch, PitchError, hz_to_midi, midi_to_hz};
