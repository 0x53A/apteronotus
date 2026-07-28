//! Note names, MIDI numbers and frequency.
//!
//! **Scientific pitch notation**: `c4` is middle C, MIDI 60, and `a4` is
//! MIDI 69 at 440 Hz. This is a convention and the trackers disagree with it —
//! Tidal calls middle C `c5` — but it is the convention the rest of the world
//! outside sequencer software uses, and the songs were written expecting it:
//! `poles.eod` strikes a tom at `g2` (98 Hz) and a bell at `c5` (523 Hz), both
//! of which land where the instrument names suggest.
//!
//! A pitch is a *fractional* MIDI number, not an integer. Bends, drift and
//! detune are ordinary arithmetic on it, and rounding to a semitone at the
//! wrong moment is the classic way to lose them.

use core::fmt;

/// Concert pitch. The one number in here that is a taste rather than a fact.
pub const A4_HZ: f64 = 440.0;

/// MIDI number of `a4`.
const A4_MIDI: f64 = 69.0;

/// Octave assumed when a note name gives none. `c` is `c4`.
pub const DEFAULT_OCTAVE: i32 = 4;

/// Semitone offsets of the natural letters within an octave, from `c`.
const NATURAL: [i32; 7] = [9, 11, 0, 2, 4, 5, 7]; // a b c d e f g

/// A pitch, as a fractional MIDI number.
///
/// Fractional because everything that makes a synthesiser sound alive lives
/// between the semitones: a bend, a per-voice detune, the component tolerance
/// `neon.eod` reproduces as deterministic drift.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct Pitch(f64);

impl Pitch {
    pub const fn from_midi(midi: f64) -> Pitch {
        Pitch(midi)
    }

    pub fn from_hz(hz: f64) -> Pitch {
        Pitch(hz_to_midi(hz))
    }

    pub const fn midi(self) -> f64 {
        self.0
    }

    pub fn hz(self) -> f64 {
        midi_to_hz(self.0)
    }

    /// Transpose by a signed number of semitones, fractional allowed.
    pub fn transpose(self, semitones: f64) -> Pitch {
        Pitch(self.0 + semitones)
    }

    /// Parse a note name: a letter, any number of accidentals, an optional
    /// octave.
    ///
    /// `#` and `s` raise, `b` and `f` lower. The `b`/`f` pair exists because
    /// `b` is also a letter — `bb3` is B flat, since the first character is
    /// always the letter and every accidental follows it.
    ///
    /// A bare number parses as a MIDI number, so `"60"` and `"c4"` agree. That
    /// is not sugar: mini-notation has no types, and a pattern of degrees or of
    /// transposition amounts is numbers all the way down.
    pub fn parse(text: &str) -> Result<Pitch, PitchError> {
        let s = text.trim();
        if s.is_empty() {
            return Err(PitchError::Empty);
        }

        if let Ok(midi) = s.parse::<f64>() {
            return Ok(Pitch(midi));
        }

        let mut chars = s.chars();
        let letter = chars.next().unwrap().to_ascii_lowercase();
        let index = match letter {
            'a'..='g' => (letter as u8 - b'a') as usize,
            _ => return Err(PitchError::NotANote),
        };

        let mut semitone = NATURAL[index];
        let rest = chars.as_str();
        let mut split = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '#' | 's' => semitone += 1,
                'b' | 'f' => semitone -= 1,
                _ => {
                    split = i;
                    break;
                }
            }
        }

        let tail = &rest[split..];
        let octave = if tail.is_empty() {
            DEFAULT_OCTAVE
        } else {
            tail.parse::<i32>().map_err(|_| PitchError::BadOctave)?
        };

        // MIDI 0 is c-1, so c0 is 12.
        Ok(Pitch(((octave + 1) * 12 + semitone) as f64))
    }
}

impl fmt::Display for Pitch {
    /// Round-trips through [`Pitch::parse`] for whole semitones; anything else
    /// prints its cent deviation, because silently dropping it would make the
    /// display lie about a detuned voice.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [&str; 12] = [
            "c", "c#", "d", "d#", "e", "f", "f#", "g", "g#", "a", "a#", "b",
        ];
        let nearest = self.0.round();
        let cents = (self.0 - nearest) * 100.0;
        let n = nearest as i64;
        let name = NAMES[n.rem_euclid(12) as usize];
        let octave = n.div_euclid(12) - 1;
        write!(f, "{name}{octave}")?;
        if cents.abs() >= 0.5 {
            write!(f, "{cents:+.0}c")?;
        }
        Ok(())
    }
}

/// Equal temperament, referred to [`A4_HZ`].
pub fn midi_to_hz(midi: f64) -> f64 {
    A4_HZ * ((midi - A4_MIDI) / 12.0).exp2()
}

pub fn hz_to_midi(hz: f64) -> f64 {
    A4_MIDI + 12.0 * (hz / A4_HZ).log2()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PitchError {
    Empty,
    /// Did not begin with a letter `a`–`g` and was not a number.
    NotANote,
    BadOctave,
}

impl fmt::Display for PitchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PitchError::Empty => write!(f, "empty note name"),
            PitchError::NotANote => write!(f, "not a note name"),
            PitchError::BadOctave => write!(f, "bad octave"),
        }
    }
}

impl core::error::Error for PitchError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn midi(s: &str) -> f64 {
        Pitch::parse(s).unwrap().midi()
    }

    #[test]
    fn scientific_pitch_notation() {
        assert_eq!(midi("c4"), 60.0);
        assert_eq!(midi("a4"), 69.0);
        assert_eq!(midi("c-1"), 0.0);
        assert_eq!(midi("g9"), 127.0);
    }

    #[test]
    fn accidentals() {
        assert_eq!(midi("c#4"), 61.0);
        assert_eq!(midi("cs4"), 61.0);
        assert_eq!(midi("db4"), 61.0);
        assert_eq!(midi("df4"), 61.0);
        // The letter always comes first, so this is B flat and not flat-flat.
        assert_eq!(midi("bb3"), 58.0);
        assert_eq!(midi("c##4"), 62.0);
    }

    #[test]
    fn octave_defaults_to_middle() {
        assert_eq!(midi("c"), midi("c4"));
        assert_eq!(midi("f#"), midi("f#4"));
    }

    #[test]
    fn bare_numbers_are_midi_numbers() {
        assert_eq!(midi("60"), 60.0);
        assert_eq!(midi("60.5"), 60.5);
        assert_eq!(midi("-3"), -3.0);
    }

    #[test]
    fn the_songs_land_where_their_names_promise() {
        // poles.eod strikes a tom at g2 and a bell at c5.
        assert_eq!(Pitch::parse("g2").unwrap().hz().round(), 98.0);
        assert_eq!(Pitch::parse("c5").unwrap().hz().round(), 523.0);
        assert_eq!(Pitch::parse("d#5").unwrap().hz().round(), 622.0);
        assert_eq!(Pitch::parse("a#4").unwrap().hz().round(), 466.0);
    }

    #[test]
    fn hz_round_trips() {
        for m in [0.0, 21.0, 60.0, 69.0, 100.5, 127.0] {
            let p = Pitch::from_midi(m);
            assert!((Pitch::from_hz(p.hz()).midi() - m).abs() < 1e-9);
        }
    }

    #[test]
    fn fractional_pitches_survive() {
        let p = Pitch::parse("a4").unwrap().transpose(0.5);
        assert!((p.hz() - 440.0 * 2f64.powf(1.0 / 24.0)).abs() < 1e-9);
        // Exactly half a semitone is a tie; it rounds away from zero, so this
        // quarter-tone spells as the upper neighbour flattened.
        assert_eq!(p.to_string(), "a#4-50c");
        assert_eq!(
            Pitch::parse("a4").unwrap().transpose(0.2).to_string(),
            "a4+20c"
        );
    }

    #[test]
    fn display_round_trips_whole_semitones() {
        for m in [0, 12, 58, 60, 61, 69, 127] {
            let p = Pitch::from_midi(m as f64);
            assert_eq!(Pitch::parse(&p.to_string()).unwrap(), p, "{p}");
        }
    }

    #[test]
    fn rejects_nonsense() {
        assert_eq!(Pitch::parse(""), Err(PitchError::Empty));
        assert_eq!(Pitch::parse("h4"), Err(PitchError::NotANote));
        assert_eq!(Pitch::parse("c4x"), Err(PitchError::BadOctave));
    }
}
