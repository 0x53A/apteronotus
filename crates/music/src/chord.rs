//! Literal chord symbols and deterministic construction-time voicings.
//!
//! This module intentionally accepts owned strings and returns owned pitches.
//! Patterned chord symbols and anchors need a query-time music node and are a
//! separate boundary; putting that case here would smuggle pattern semantics
//! into this pure crate.

use crate::{Pitch, PitchClass};
use core::fmt;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chord {
    root: PitchClass,
    bass: Option<PitchClass>,
    intervals: Vec<i32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoicingShape {
    Close(usize),
    Open(usize),
    Wide(usize),
    Drop2,
}

impl VoicingShape {
    pub fn parse(source: &str) -> Result<Self, ChordError> {
        let source = source.trim().to_ascii_lowercase();
        if source == "drop-2" {
            return Ok(VoicingShape::Drop2);
        }
        let Some((family, count)) = source.rsplit_once('-') else {
            return Err(ChordError::Voicing(source));
        };
        let count = count
            .parse::<usize>()
            .map_err(|_| ChordError::Voicing(source.clone()))?;
        if !(2..=12).contains(&count) {
            return Err(ChordError::Voicing(source));
        }
        match family {
            "close" => Ok(VoicingShape::Close(count)),
            "open" => Ok(VoicingShape::Open(count)),
            "wide" => Ok(VoicingShape::Wide(count)),
            _ => Err(ChordError::Voicing(source)),
        }
    }
}

impl Chord {
    pub fn parse(source: &str) -> Result<Self, ChordError> {
        let source = source.trim();
        if source.is_empty() {
            return Err(ChordError::Empty);
        }

        let (root_text, rest) = split_pitch_class(source)?;
        let root =
            PitchClass::parse(root_text).map_err(|_| ChordError::Root(root_text.to_string()))?;
        let (quality, bass) = split_bass(rest)?;
        let bass = bass
            .map(PitchClass::parse)
            .transpose()
            .map_err(|_| ChordError::Bass(bass.unwrap_or_default().to_string()))?;
        let intervals = quality_intervals(quality)
            .ok_or_else(|| ChordError::Quality(quality.to_string()))?
            .to_vec();

        Ok(Chord {
            root,
            bass,
            intervals,
        })
    }

    pub fn root(&self) -> PitchClass {
        self.root
    }

    pub fn bass(&self) -> Option<PitchClass> {
        self.bass
    }

    pub fn intervals(&self) -> &[i32] {
        &self.intervals
    }

    /// Expand a named voicing whose highest note is at or below `anchor`.
    ///
    /// Close position uses successive chord tones. Open and wide position
    /// retain that harmonic order while requiring at least five and seven
    /// semitones between adjacent voices, respectively. This is a compact
    /// deterministic v1 dictionary, not an automatic voice-leading engine.
    pub fn voice(&self, anchor: Pitch, shape: VoicingShape) -> Vec<Pitch> {
        let (count, minimum_gap) = match shape {
            VoicingShape::Close(count) => (count, 1),
            VoicingShape::Open(count) => (count, 5),
            VoicingShape::Wide(count) => (count, 7),
            VoicingShape::Drop2 => (self.intervals.len().max(4), 1),
        };
        let ordered = self.ordered_intervals();
        let mut intervals = extend_intervals(&ordered, count, minimum_gap);

        if matches!(shape, VoicingShape::Drop2) && intervals.len() >= 2 {
            let index = intervals.len() - 2;
            intervals[index] -= 12;
            intervals.sort_unstable();
        }

        let root_midi = 60 + self.root.semitones() as i32;
        let top = root_midi + intervals.last().copied().unwrap_or_default();
        let anchor = anchor.midi().floor() as i32;
        let octave_shift = (anchor - top).div_euclid(12) * 12;
        intervals
            .into_iter()
            .map(|interval| Pitch::from_midi((root_midi + interval + octave_shift) as f64))
            .collect()
    }

    fn ordered_intervals(&self) -> Vec<i32> {
        let Some(bass) = self.bass else {
            return self.intervals.clone();
        };
        let bass_from_root =
            (bass.semitones() as i32 - self.root.semitones() as i32).rem_euclid(12);
        let Some(index) = self
            .intervals
            .iter()
            .position(|interval| interval.rem_euclid(12) == bass_from_root)
        else {
            // A slash bass need not be a chord tone. Put it below the chord
            // rather than silently discarding the authored inversion.
            let mut intervals = vec![bass_from_root - 12];
            intervals.extend(self.intervals.iter().copied());
            return intervals;
        };
        self.intervals[index..]
            .iter()
            .copied()
            .chain(
                self.intervals[..index]
                    .iter()
                    .copied()
                    .map(|interval| interval + 12),
            )
            .collect()
    }
}

fn extend_intervals(source: &[i32], count: usize, minimum_gap: i32) -> Vec<i32> {
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let mut interval = source[index % source.len()] + 12 * (index / source.len()) as i32;
        if let Some(previous) = result.last().copied() {
            while interval < previous + minimum_gap {
                interval += 12;
            }
        }
        result.push(interval);
    }
    result
}

fn split_pitch_class(source: &str) -> Result<(&str, &str), ChordError> {
    let mut chars = source.char_indices();
    let Some((_, first)) = chars.next() else {
        return Err(ChordError::Empty);
    };
    if !matches!(first.to_ascii_lowercase(), 'a'..='g') {
        return Err(ChordError::Root(source.to_string()));
    }
    let end = match chars.next() {
        Some((index, '#' | 'b')) => index + 1,
        Some((index, _)) => index,
        None => source.len(),
    };
    Ok((&source[..end], &source[end..]))
}

fn split_bass(source: &str) -> Result<(&str, Option<&str>), ChordError> {
    let Some(index) = source.rfind('/') else {
        return Ok((source, None));
    };
    let candidate = &source[index + 1..];
    if candidate.is_empty() {
        return Err(ChordError::Bass(String::new()));
    }
    if PitchClass::parse(candidate).is_ok() {
        Ok((&source[..index], Some(candidate)))
    } else {
        // `6/9` is a quality name, not an inversion.
        Ok((source, None))
    }
}

fn quality_intervals(quality: &str) -> Option<&'static [i32]> {
    match quality {
        "" | "maj" => Some(&[0, 4, 7]),
        "m" => Some(&[0, 3, 7]),
        "m(add9)" | "madd9" => Some(&[0, 3, 7, 14]),
        "maj9" => Some(&[0, 4, 7, 11, 14]),
        "6(add9)" | "6/9" | "69" => Some(&[0, 4, 7, 9, 14]),
        "m6/9" | "m69" => Some(&[0, 3, 7, 9, 14]),
        "7sus4(b9)" | "7b9sus" => Some(&[0, 5, 7, 10, 13]),
        "maj7(#11)" => Some(&[0, 4, 7, 11, 18]),
        "m11" => Some(&[0, 3, 7, 10, 14, 17]),
        "m(maj9)" | "mmaj9" => Some(&[0, 3, 7, 11, 14]),
        "maj9(#11)" => Some(&[0, 4, 7, 11, 14, 18]),
        _ => None,
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChordError {
    Empty,
    Root(String),
    Quality(String),
    Bass(String),
    Voicing(String),
}

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChordError::Empty => write!(f, "empty chord symbol"),
            ChordError::Root(root) => write!(f, "invalid chord root {root:?}"),
            ChordError::Quality(quality) => write!(f, "unsupported chord quality {quality:?}"),
            ChordError::Bass(bass) => write!(f, "invalid slash bass {bass:?}"),
            ChordError::Voicing(shape) => write!(f, "unknown voicing shape {shape:?}"),
        }
    }
}

impl core::error::Error for ChordError {}

#[cfg(test)]
mod tests {
    use super::{Chord, VoicingShape};
    use crate::Pitch;

    #[test]
    fn parses_every_literal_corpus_quality_and_inversion() {
        for symbol in [
            "Dm(add9)",
            "Cmaj9/E",
            "G6(add9)",
            "A7sus4(b9)",
            "Dm6/9",
            "Bbmaj7(#11)/D",
            "Em11",
            "Gm(maj9)/D",
            "Dm(add9)/A",
            "Cmaj9(#11)",
        ] {
            Chord::parse(symbol).unwrap();
        }
    }

    #[test]
    fn named_shapes_are_deterministic_and_anchor_the_top_voice() {
        let chord = Chord::parse("Dm(add9)").unwrap();
        for shape in [
            VoicingShape::parse("close-5").unwrap(),
            VoicingShape::parse("open-5").unwrap(),
            VoicingShape::parse("wide-6").unwrap(),
            VoicingShape::parse("drop-2").unwrap(),
        ] {
            let notes = chord.voice(Pitch::parse("a4").unwrap(), shape);
            assert!(
                notes
                    .windows(2)
                    .all(|pair| pair[0].midi() <= pair[1].midi())
            );
            assert!(notes.last().unwrap().midi() <= Pitch::parse("a4").unwrap().midi());
            assert_eq!(notes, chord.voice(Pitch::parse("a4").unwrap(), shape));
        }
    }

    #[test]
    fn slash_bass_is_the_lowest_pitch_class() {
        let chord = Chord::parse("Cmaj9/E").unwrap();
        let notes = chord.voice(
            Pitch::parse("e4").unwrap(),
            VoicingShape::parse("wide-5").unwrap(),
        );
        assert_eq!(notes[0].midi() as i32 % 12, 4);
    }
}
