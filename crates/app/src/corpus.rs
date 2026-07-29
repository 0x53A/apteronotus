//! The specification corpus, checked against the backend.
//!
//! `songs/CLAUDE.md` says the implementation is finished when the seven
//! specification songs play, and as of this test every song in the corpus
//! evaluates, lowers and opens a stereo output. That is a property worth
//! nailing down rather than rediscovering: it is the difference between a
//! corpus that specifies the engine and one that merely predates it.
//!
//! It is a *lowering* guarantee, not a musical one. Nothing here listens.
//!
//! The test lives in `crates/app` rather than in `crates/songs` because
//! deciding it needs the evaluator, the lowering path and the app's own
//! stereo-output assumption. `apteronotus-songs` stays inert.

use crate::player::{needs_persistent_runtime, persistent_runtime, playable_channels};
use apteronotus_lua::evaluate;
use apteronotus_songs::{SONGS, Song};

/// Why this song does not reach audio on the current backend, or `None` if it
/// does. Exercises the same three steps the player takes at a Run.
fn refusal(song: &Song) -> Option<String> {
    let program = match evaluate(song.source) {
        Ok(program) => program,
        Err(error) => return Some(format!("evaluation: {error}")),
    };
    match playable_channels(&program) {
        Ok(2) => {}
        Ok(channels) => return Some(format!("outputs {channels} channels, not stereo")),
        Err(error) => return Some(format!("lowering: {error}")),
    }
    if needs_persistent_runtime(&program)
        && let Err(error) = persistent_runtime(&program)
    {
        return Some(format!("patch arena: {error}"));
    }
    None
}

#[test]
fn the_whole_corpus_still_lowers() {
    // Report every song at once. A backend change usually moves more than one,
    // and stopping at the first is a slow way to learn that.
    let refusals: Vec<String> = SONGS
        .iter()
        .filter_map(|song| {
            refusal(song).map(|reason| format!("songs/{}.eod — {reason}", song.name))
        })
        .collect();
    assert!(
        refusals.is_empty(),
        "the specification corpus no longer reaches audio:\n  {}",
        refusals.join("\n  ")
    );
}
