//! The specification corpus, embedded as static strings.
//!
//! `songs/` at the repository root is the target: the first seven documents
//! were written before the runtime, and the implementation is finished when
//! they play. `drift.eod` came afterwards and is the other direction — a
//! piece written against the working backend, which is what the corpus is
//! for once it stops being a specification. This crate is only their
//! delivery mechanism — it makes the corpus
//! available to any program that links Apteronotus, so a host application
//! (`crates/app`, the study app in `~/src/idiosepius`, a renderer, a test
//! harness) does not have to keep its own copy drifting out of date.
//!
//! It is deliberately inert: no dependencies, no evaluator, no audio. A song
//! is text.
//!
//! There is no `playable` flag, because as of the corpus pin in
//! `crates/app/src/corpus.rs` every song evaluates, lowers and opens a stereo
//! output — a flag that is uniformly true is noise, and that test is where the
//! claim belongs anyway, since deciding it needs the evaluator this crate
//! deliberately does not depend on.

/// One document from the specification corpus.
pub struct Song {
    /// The stem of its file in `songs/`, and the stable identifier callers
    /// should key on.
    pub name: &'static str,
    /// The document's own first line, minus the leading comment marker: what
    /// the piece is, in the author's words.
    pub title: &'static str,
    /// What the song exists to stress, from the table in `songs/CLAUDE.md`.
    pub stresses: &'static str,
    /// The complete source text.
    pub source: &'static str,
}

/// `synthwave.eod` — polyphony, sends, sidechain, curve automation.
pub const SYNTHWAVE: &str = include_str!("../../../songs/synthwave.eod");
/// `waves.eod` — continuous signals steering everything, no onsets.
pub const WAVES: &str = include_str!("../../../songs/waves.eod");
/// `techno.eod` — a rigid grid and a 32-bar riser that must stay coherent.
pub const TECHNO: &str = include_str!("../../../songs/techno.eod");
/// `poles.eod` — percussion as pole placement; no oscillators, no samples.
pub const POLES: &str = include_str!("../../../songs/poles.eod");
/// `neon.eod` — finite through-composed time, tempo changes, off-grid entrances.
pub const NEON: &str = include_str!("../../../songs/neon.eod");
/// `jamming.eod` — live audio as a graph source, `patch` as a persistent rack.
pub const JAMMING: &str = include_str!("../../../songs/jamming.eod");
/// `supersaws.eod` — an external Strudel port, in its author's habits.
pub const SUPERSAWS: &str = include_str!("../../../songs/supersaws.eod");
/// `drift.eod` — a stationary loop with no arrangement to run out.
pub const DRIFT: &str = include_str!("../../../songs/drift.eod");
/// `outbound.eod` — polymetric layers, so the combination is the form.
pub const OUTBOUND: &str = include_str!("../../../songs/outbound.eod");

/// Every song, in the order `songs/CLAUDE.md` introduces them: the four cyclic
/// pieces first, then the two that needed finite time and live input, then the
/// external port, and last the two written after the engine rather than
/// before it.
///
/// A `static` rather than a `const` on purpose. A `const` is substituted at
/// every use site, so two mentions of it can produce two different addresses
/// and `&'static Song` stops being an identity a caller can compare or cache.
pub static SONGS: &[Song] = &[
    Song {
        name: "synthwave",
        title: "1984, but the DeLorean has a modem",
        stresses: "polyphony, sends, sidechain, chords into arpeggios, \
                   curve automation spanning 64 bars, gated reverb",
        source: SYNTHWAVE,
    },
    Song {
        name: "waves",
        title: "background for reading — nothing has an onset",
        stresses: "continuous signals steering everything, multi-second \
                   envelopes, sparse reproducible randomness, a voice with no \
                   oscillator in it",
        source: WAVES,
    },
    Song {
        name: "techno",
        title: "the other mode",
        stresses: "rigid grid, a 32-bar riser that must stay coherent, \
                   euclidean rhythms, ducking deep enough to be composition",
        source: TECHNO,
    },
    Song {
        name: "poles",
        title: "a whole drum kit with no oscillators and no samples",
        stresses: "δ at audio rate, percussion as pole placement, the \
                   stdlib-vs-primitive split, voices whose entire parameter \
                   set is two numbers",
        source: POLES,
    },
    Song {
        name: "neon",
        title: "eighty storeys of weather and one light left on",
        stresses: "finite through-composed time, tempo changes and off-grid \
                   entrances, extended voicings, two-layer raw subtractive \
                   synthesis, per-voice drift, note-relative gestures, a \
                   persistent mono lead, a large modulated reverb",
        source: NEON,
    },
    Song {
        name: "jamming",
        title: "the knifefish's other trick, as a piece of music",
        stresses: "live audio as a graph source, the audio → control crossing, \
                   `patch` as a persistent rack, the pattern algebra driving \
                   effects rather than notes, and a piece that is not \
                   reproducible until its input is recorded",
        source: JAMMING,
    },
    Song {
        name: "supersaws",
        title: "port of \"just a bunch of supersaws\" by lofi.scifi",
        stresses: "stereo topology generation, weighted and polymetric event \
                   structure, per-event filter envelopes, channel-wise \
                   distortion, and probabilistic ratchets",
        source: SUPERSAWS,
    },
    Song {
        name: "drift",
        title: "twenty hours of it, and nothing ever arrives",
        stresses: "a stationary loop with no arrangement, variation derived \
                   from mutually prime transport periods, a persistent \
                   stateless noise floor, and a deliberately dark mix",
        source: DRIFT,
    },
    Song {
        name: "outbound",
        title: "the same road as drift.eod, forty miles an hour faster",
        stresses: "polymetric loop lengths and offsets as the form itself, a \
                   fast pulse against a low event count, and pentatonic cells \
                   that stay consonant wherever the harmony has got to",
        source: OUTBOUND,
    },
];

/// Look one up by [`Song::name`].
pub fn song(name: &str) -> Option<&'static Song> {
    SONGS.iter().find(|song| song.name == name)
}

#[cfg(test)]
mod tests {
    use super::{SONGS, song};

    #[test]
    fn every_song_carries_its_source() {
        for entry in SONGS {
            assert!(
                entry.source.len() > 512,
                "{} looks truncated: {} bytes",
                entry.name,
                entry.source.len()
            );
            assert!(
                entry.source.starts_with(&format!("-- {}.eod", entry.name)),
                "{} does not open with its own filename; the include_str! paths \
                 may have been crossed",
                entry.name
            );
        }
    }

    #[test]
    fn names_are_unique_and_resolvable() {
        for entry in SONGS {
            let found = song(entry.name).expect("every listed song resolves by name");
            assert!(
                std::ptr::eq(found, entry),
                "{} resolves to a different entry, so a name is duplicated",
                entry.name
            );
        }
        assert!(song("nothing-by-this-name").is_none());
    }
}
