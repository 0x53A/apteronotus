//! The documents the app ships with.
//!
//! The short teaching examples precede the complete embedded song corpus in
//! the library picker. Every document evaluates and plays on the backend, and
//! the example tests and `corpus::the_whole_corpus_still_lowers` keep that
//! true as the backend moves underneath them.

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

/// Teaching examples first, then the authoritative corpus without source copies.
pub fn documents() -> impl Iterator<Item = Example> {
    EXAMPLES
        .iter()
        .map(|example| Example {
            name: example.name,
            summary: example.summary,
            source: example.source,
        })
        .chain(apteronotus_songs::SONGS.iter().map(|song| Example {
            name: song.name,
            summary: song.stresses,
            source: song.source,
        }))
}

/// Keep the last edited buffer while browsing untouched library documents.
/// Editing a loaded document makes it the next buffer to preserve. Returns
/// whether the editor changed; this never evaluates the source.
pub fn open_document(source: &mut String, displaced: &mut Option<String>, document: &str) -> bool {
    if source.as_str() == document {
        return false;
    }
    let was_library_document = documents().any(|entry| entry.source == source.as_str());
    let previous = std::mem::replace(source, document.into());
    if displaced.is_none() || !was_library_document {
        *displaced = Some(previous);
    }
    true
}

/// The document the editor opens with.
pub fn starter() -> &'static str {
    EXAMPLES[0].source
}

#[cfg(test)]
mod tests {
    use super::EXAMPLES;
    use crate::player::{needs_persistent_runtime, persistent_runtime, playable_channels};
    use apteronotus_lua::evaluate;

    #[test]
    fn browsing_songs_retains_the_edited_buffer_until_another_edit() {
        let original = "-- my unsaved composition\n";
        let mut source = original.to_string();
        let mut displaced = None;
        for song in apteronotus_songs::SONGS {
            assert!(super::open_document(
                &mut source,
                &mut displaced,
                song.source
            ));
            assert_eq!(displaced.as_deref(), Some(original));
        }
        let current = source.clone();
        assert!(!super::open_document(&mut source, &mut displaced, &current));
        assert_eq!(displaced.as_deref(), Some(original));
        source.push_str("\n-- my variation\n");
        let edited = source.clone();
        assert!(super::open_document(
            &mut source,
            &mut displaced,
            super::starter()
        ));
        assert_eq!(displaced.as_deref(), Some(edited.as_str()));
        source = displaced.take().unwrap();
        assert_eq!(source, edited);
    }

    #[test]
    fn the_initial_library_document_is_also_recoverable() {
        let mut source = super::starter().to_string();
        let mut displaced = None;
        super::open_document(
            &mut source,
            &mut displaced,
            apteronotus_songs::NIGHTSHIFT_DUB,
        );
        assert_eq!(displaced.as_deref(), Some(super::starter()));
    }

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
