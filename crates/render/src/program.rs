//! Binding an owned `Program` to the scheduler and the persistent arena.
//!
//! These five functions are the whole answer to "does this program reach
//! audio, and with what shape". They are device-free and host-neutral on
//! purpose: the GUI player asks them at a Run, the corpus test asks them
//! without opening a device, and the offline renderer asks them before writing
//! a file. Keeping one copy is not tidiness — a renderer that disagreed with
//! the player about what is playable would silently invalidate every
//! comparison made against its output.

use apteronotus_live::{PersistentRuntime, RoutedRuntime, ScheduledRun, ScheduledTrack};
use apteronotus_lua::Program;

/// The number of main output channels the program will produce, or why it
/// produces none.
///
/// Routed lowering deliberately broadcasts a mono voice across the persistent
/// main layout, so a mono voice is accepted against a wider persistent layout.
/// Main-only scheduling still requires every track to have the same width,
/// because its `Sequencer` is created from the first voice template.
pub fn playable_channels(program: &Program) -> Result<usize, String> {
    let persistent = needs_persistent_runtime(program);
    if program.tracks.is_empty() && program.runs.is_empty() {
        return Err("the program has no playable tracks or persistent runs".into());
    }

    let mut channels = persistent.then(|| program.buses.main_channels());
    for (index, track) in program.tracks.iter().enumerate() {
        let graph = program
            .voice(track.voice)
            .ok_or_else(|| format!("track {index} refers to a missing voice"))?;
        if graph.inputs != 0 {
            return Err(format!(
                "track {index} has an input graph; live input racks are not connected to voice \
                 tracks yet"
            ));
        }
        if graph.channels() == 0 {
            return Err(format!("track {index} has no audio outputs"));
        }
        match channels {
            Some(expected)
                if graph.channels() != expected && !(persistent && graph.channels() == 1) =>
            {
                return Err(format!(
                    "track {index} outputs {} channels, but earlier tracks output {expected}",
                    graph.channels()
                ));
            }
            None => channels = Some(graph.channels()),
            Some(_) => {}
        }
    }
    Ok(channels.expect("a playable program has a track or persistent main layout"))
}

/// Whether the program needs a persistent arena at all.
///
/// A program with no runs, no controls, no buses beyond main and no graph
/// sends is scheduled straight into a voice sequencer, which is both simpler
/// and one net cheaper per block.
pub fn needs_persistent_runtime(program: &Program) -> bool {
    !program.runs.is_empty()
        || !program.controls.specs().is_empty()
        || program.buses.total_channels() != program.buses.main_channels()
        || program.voices.iter().any(|voice| !voice.sends.is_empty())
}

/// Build the persistent arena for one program generation.
///
/// Only spanless runs are persistent; a run carrying a span is a finite,
/// scheduled placement and belongs to [`scheduled_runs`] instead.
pub fn persistent_runtime(program: &Program) -> Result<PersistentRuntime, String> {
    let patches = program
        .runs
        .iter()
        .filter(|run| run.span.is_none())
        .enumerate()
        .map(|(index, run)| {
            program
                .patch(run.patch)
                .ok_or_else(|| format!("run {index} refers to a missing patch"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    PersistentRuntime::with_audio_inputs(
        &program.buses,
        &program.controls,
        &program.audio_inputs,
        patches,
    )
    .map_err(|error| error.to_string())
}

/// The program's tracks, resolved against its voice table.
pub fn scheduled_tracks(program: &Program) -> Result<Vec<ScheduledTrack<'_>>, String> {
    program
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let template = program
                .voice(track.voice)
                .ok_or_else(|| format!("track {index} refers to a missing voice"))?;
            Ok(ScheduledTrack::with_routing_and_bindings(
                &track.pattern,
                template,
                &track.routing,
                &track.onset_bindings,
            ))
        })
        .collect()
}

/// The program's finite patch placements, resolved against its patch table.
pub fn scheduled_runs(program: &Program) -> Result<Vec<ScheduledRun<'_>>, String> {
    program
        .runs
        .iter()
        .enumerate()
        .filter_map(|(index, run)| {
            run.span.map(|span| {
                program
                    .patch(run.patch)
                    .map(|patch| ScheduledRun::with_routing(patch, span, &run.routing))
                    .ok_or_else(|| format!("run {index} refers to a missing patch"))
            })
        })
        .collect()
}

/// The routing view a filled window needs when a persistent arena is present.
pub fn routed_runtime(runtime: &PersistentRuntime) -> RoutedRuntime<'_> {
    RoutedRuntime::new(runtime.layout(), runtime.controls())
}
