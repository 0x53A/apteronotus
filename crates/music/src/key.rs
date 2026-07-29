//! Typed tonal context owned by an evaluated program.

use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PitchClass(u8);

impl PitchClass {
    pub fn parse(source: &str) -> Result<PitchClass, KeyError> {
        let normalized = source.trim().to_ascii_lowercase();
        let semitones = match normalized.as_str() {
            "c" | "b#" => 0,
            "c#" | "db" => 1,
            "d" => 2,
            "d#" | "eb" => 3,
            "e" | "fb" => 4,
            "f" | "e#" => 5,
            "f#" | "gb" => 6,
            "g" => 7,
            "g#" | "ab" => 8,
            "a" => 9,
            "a#" | "bb" => 10,
            "b" | "cb" => 11,
            _ => return Err(KeyError::Tonic(source.into())),
        };
        Ok(PitchClass(semitones))
    }

    pub fn semitones(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Major,
    Minor,
    Dorian,
}

impl Mode {
    pub fn parse(source: &str) -> Result<Mode, KeyError> {
        match source.trim().to_ascii_lowercase().as_str() {
            "major" | "ionian" => Ok(Mode::Major),
            "minor" | "aeolian" => Ok(Mode::Minor),
            "dorian" => Ok(Mode::Dorian),
            _ => Err(KeyError::Mode(source.into())),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Key {
    pub tonic: PitchClass,
    pub mode: Mode,
}

impl Key {
    pub fn parse(tonic: &str, mode: &str) -> Result<Key, KeyError> {
        Ok(Key {
            tonic: PitchClass::parse(tonic)?,
            mode: Mode::parse(mode)?,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyError {
    Tonic(String),
    Mode(String),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Tonic(tonic) => write!(f, "unknown key tonic {tonic:?}"),
            KeyError::Mode(mode) => write!(f, "unknown key mode {mode:?}"),
        }
    }
}

impl core::error::Error for KeyError {}

#[cfg(test)]
mod tests {
    use super::{Key, Mode};

    #[test]
    fn corpus_keys_are_typed_and_enharmonic_tonics_agree() {
        assert_eq!(Key::parse("f#", "minor").unwrap().tonic.semitones(), 6);
        assert_eq!(Key::parse("gb", "aeolian").unwrap().tonic.semitones(), 6);
        assert_eq!(Key::parse("d", "dorian").unwrap().mode, Mode::Dorian);
    }
}
