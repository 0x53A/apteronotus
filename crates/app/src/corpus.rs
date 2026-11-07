//! Production-path checks for focused test programs.
use crate::player::{needs_persistent_runtime, playable_channels};
use apteronotus_lua::evaluate;
use apteronotus_render::persistent_runtime_prepared;

/// Why this song does not reach audio on the current backend, or `None` if it
/// does. Exercises the same three steps the player takes at a Run.
fn refusal(source: &str) -> Option<String> {
    if let Err(error) = apteronotus_lua::check_syntax(source) {
        return Some(format!("syntax check: {error}"));
    }
    let program = match evaluate(source) {
        Ok(program) => program,
        Err(error) => return Some(format!("evaluation: {error}")),
    };
    match playable_channels(&program) {
        Ok(2) => {}
        Ok(channels) => return Some(format!("outputs {channels} channels, not stereo")),
        Err(error) => return Some(format!("lowering: {error}")),
    }
    if needs_persistent_runtime(&program)
        && let Err(error) = persistent_runtime_prepared(&program, 0.0, 48_000.0)
    {
        return Some(format!("patch arena: {error}"));
    }
    None
}

#[test]
fn synthetic_programs_reach_stereo_output() {
    for source in [
        include_str!("../../test-fixtures/routed.eod"),
        include_str!("../../test-fixtures/external-trigger.eod"),
        include_str!("../../test-fixtures/continuous.eod"),
    ] {
        assert!(refusal(source).is_none(), "{:?}", refusal(source));
    }
}
