//! The documents the app ships with.
//!
//! These are not the specification corpus in `songs/` — those are written
//! against the whole language, including parts that do not exist yet. Every
//! document here evaluates and plays on the current backend, and
//! `tests::every_example_is_playable` is what keeps that true as the backend
//! moves underneath them.

/// One shipped document.
pub struct Example {
    /// What the picker shows.
    pub name: &'static str,
    /// One line on what the document demonstrates.
    pub summary: &'static str,
    pub source: &'static str,
}

pub const EXAMPLES: &[Example] = &[
    Example {
        name: "Three notes",
        summary: "a voice, an envelope and mini-notation",
        source: include_str!("../assets/examples/three-notes.eod"),
    },
    Example {
        name: "Live controls",
        summary: "a persistent patch with faders you can move while it runs",
        source: include_str!("../assets/examples/live-controls.eod"),
    },
    Example {
        name: "Pattern algebra",
        summary: "two tracks, rational time and structural transforms",
        source: include_str!("../assets/examples/pattern-algebra.eod"),
    },
    Example {
        name: "Struck bars",
        summary: "percussion as pole placement — no oscillators, no samples",
        source: include_str!("../assets/examples/struck.eod"),
    },
];

/// The document the editor opens with.
pub fn starter() -> &'static str {
    EXAMPLES[0].source
}

#[cfg(test)]
mod tests {
    use super::EXAMPLES;
    use crate::player::{needs_persistent_runtime, persistent_runtime, playable_channels};
    use apteronotus_lua::evaluate;

    /// A shipped example that does not evaluate is worse than no example, and
    /// the backend is moving. This is the guard.
    #[test]
    fn every_example_is_playable() {
        for example in EXAMPLES {
            let program = evaluate(example.source)
                .unwrap_or_else(|error| panic!("{} failed to evaluate: {error}", example.name));
            let channels = playable_channels(&program)
                .unwrap_or_else(|error| panic!("{} is not playable: {error}", example.name));
            assert_eq!(
                channels, 2,
                "{} should reach the stereo output the app opens",
                example.name
            );
            if needs_persistent_runtime(&program) {
                persistent_runtime(&program).unwrap_or_else(|error| {
                    panic!("{} could not build its patch arena: {error}", example.name)
                });
            }
        }
    }

    #[test]
    fn examples_are_distinct_and_described() {
        for example in EXAMPLES {
            assert!(!example.name.is_empty());
            assert!(!example.summary.is_empty());
            assert!(
                example.source.trim_start().starts_with("--"),
                "{} should open with a comment explaining itself",
                example.name
            );
        }
        for (index, example) in EXAMPLES.iter().enumerate() {
            assert!(
                !EXAMPLES[..index]
                    .iter()
                    .any(|earlier| earlier.name == example.name),
                "duplicate example name {}",
                example.name
            );
        }
    }
}
