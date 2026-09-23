//! Binding an owned `Program` to the scheduler and the persistent arena.
//!
//! These functions are the whole answer to "does this program reach
//! audio, and with what shape". They are device-free and host-neutral on
//! purpose: the GUI player asks them at a Run, the corpus test asks them
//! without opening a device, and the offline renderer asks them before writing
//! a file. Keeping one copy is not tidiness — a renderer that disagreed with
//! the player about what is playable would silently invalidate every
//! comparison made against its output.

use apteronotus_live::{PersistentRuntime, RoutedRuntime, ScheduledRun, ScheduledTrack};
use apteronotus_lua::Program;

/// Stable, source-independent labels for the flattened routed layout.
///
/// Lua bus variable names do not survive evaluation, so buses are named by
/// their authoritative declaration ordinal rather than guessed from source.
pub fn stem_lane_labels(program: &Program) -> Vec<String> {
    let mut labels = channel_labels("main", program.buses.main_channels()).collect::<Vec<_>>();
    for (bus, &channels) in program.buses.bus_channel_counts().iter().enumerate() {
        labels.extend(channel_labels(&format!("bus{bus}"), channels));
    }
    labels
}

fn channel_labels(group: &str, channels: usize) -> impl Iterator<Item = String> + '_ {
    (0..channels).map(move |channel| {
        let suffix = match (channels, channel) {
            (2, 0) => "L".to_string(),
            (2, 1) => "R".to_string(),
            _ => channel.to_string(),
        };
        format!("{group}.{suffix}")
    })
}

/// The number of main output channels the program will produce, or why it
/// produces none.
///
/// Routed lowering deliberately broadcasts a mono voice across the persistent
/// main layout, so a mono voice is accepted against a wider persistent layout.
/// Main-only scheduling still requires every track to have the same width,
/// because its `Sequencer` is created from the first voice template.
pub fn playable_channels(program: &Program) -> Result<usize, String> {
    let tracks = (0..program.tracks.len()).collect::<Vec<_>>();
    playable_channels_for_tracks(program, &tracks)
}

/// The output shape of a program when only the selected tracks are scheduled.
///
/// Persistent runs and processors remain part of the program. This selection
/// happens after evaluation and only changes the scheduler views, so source
/// coordinates and every provenance-derived random decision stay untouched.
pub fn playable_channels_for_tracks(
    program: &Program,
    track_indices: &[usize],
) -> Result<usize, String> {
    let persistent = needs_persistent_runtime(program);
    if program.tracks.is_empty() && program.runs.is_empty() {
        return Err("the program has no playable tracks or persistent runs".into());
    }

    let mut channels = persistent.then(|| program.buses.main_channels());
    for &index in track_indices {
        let track = program.tracks.get(index).ok_or_else(|| {
            format!(
                "track {index} does not exist; program track count is {}",
                program.tracks.len()
            )
        })?;
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

    // Muting every track is a useful measurement and should produce a silent
    // file rather than lose the program's output shape. A main-only program
    // has no explicit layout, so retain the first track's established width
    // while scheduling none of its events.
    if channels.is_none()
        && let Some(track) = program.tracks.first()
    {
        let graph = program
            .voice(track.voice)
            .ok_or_else(|| "track 0 refers to a missing voice".to_string())?;
        if graph.inputs != 0 {
            return Err(
                "track 0 has an input graph; live input racks are not connected to voice tracks yet"
                    .into(),
            );
        }
        if graph.channels() == 0 {
            return Err("track 0 has no audio outputs".into());
        }
        channels = Some(graph.channels());
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
    persistent_runtime_at(program, 0.0)
}

/// Build a persistent arena whose coordinate-derived controls begin at the
/// supplied absolute transport time.
pub fn persistent_runtime_at(
    program: &Program,
    transport_seconds: f64,
) -> Result<PersistentRuntime, String> {
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
    PersistentRuntime::with_audio_inputs_at_transport(
        &program.buses,
        &program.controls,
        &program.audio_inputs,
        patches,
        transport_seconds,
    )
    .map_err(|error| error.to_string())
}

/// The program's tracks, resolved against its voice table.
pub fn scheduled_tracks(program: &Program) -> Result<Vec<ScheduledTrack<'_>>, String> {
    let tracks = (0..program.tracks.len()).collect::<Vec<_>>();
    scheduled_tracks_for(program, &tracks)
}

/// Resolve only the requested tracks, retaining their original program order
/// and their unmodified pattern/provenance data.
pub fn scheduled_tracks_for<'a>(
    program: &'a Program,
    track_indices: &[usize],
) -> Result<Vec<ScheduledTrack<'a>>, String> {
    track_indices
        .iter()
        .map(|&index| {
            let track = program.tracks.get(index).ok_or_else(|| {
                format!(
                    "track {index} does not exist; program track count is {}",
                    program.tracks.len()
                )
            })?;
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
