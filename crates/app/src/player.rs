use apteronotus_live::{
    AudioOutput, PersistentRuntime, PitchScheduler, ProgramScheduler, RevisionSlot, ScheduledTrack,
    Transport,
};
use apteronotus_lua::{Evaluator, Program};
use apteronotus_pattern::Frac;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
#[cfg(not(target_arch = "wasm32"))]
use std::{
    sync::mpsc::RecvTimeoutError,
    thread::{self, JoinHandle},
    time::Duration,
};
use web_time::Instant;

const LOOKAHEAD_SECONDS: f64 = 0.20;
const MIN_REVISION_WINDOW_SECONDS: f64 = 0.05;
#[cfg(not(target_arch = "wasm32"))]
const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub enum Command {
    Run { request: u64, source: String },
    Stop,
    SetControl { name: String, value: f64 },
    Shutdown,
}

#[derive(Clone)]
pub struct ControlView {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub value: f64,
}

pub enum PlayerEvent {
    Active {
        request: u64,
        generation: u64,
        boundary: String,
        voices: usize,
        controls: Vec<ControlView>,
    },
    Error {
        request: u64,
        message: String,
    },
    Stopped,
    RuntimeError(String),
}

#[cfg(not(target_arch = "wasm32"))]
pub struct PlayerWorker {
    thread: Option<JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl PlayerWorker {
    pub fn spawn() -> (PlayerWorker, Sender<Command>, Receiver<PlayerEvent>) {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("apteronotus-player".into())
            .spawn(move || run_worker(command_rx, event_tx))
            .expect("failed to start the player worker");
        (
            PlayerWorker {
                thread: Some(thread),
            },
            command_tx,
            event_rx,
        )
    }

    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Native playback owns a dedicated worker and needs no UI-thread pump.
    pub fn pump(&mut self) {}
}

/// Browsers do not provide `std::thread::spawn` without a cross-origin-isolated
/// shared-memory deployment. Evaluation and scheduling therefore run at the
/// explicit Run boundary and once per UI frame, while CPAL's WebAudio backend
/// continues to own the actual audio callbacks.
#[cfg(target_arch = "wasm32")]
pub struct PlayerWorker {
    command_rx: Receiver<Command>,
    event_tx: Sender<PlayerEvent>,
    player: Player,
}

#[cfg(target_arch = "wasm32")]
impl PlayerWorker {
    pub fn spawn() -> (PlayerWorker, Sender<Command>, Receiver<PlayerEvent>) {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        (
            PlayerWorker {
                command_rx,
                event_tx,
                player: Player::new(),
            },
            command_tx,
            event_rx,
        )
    }

    pub fn pump(&mut self) {
        while let Ok(command) = self.command_rx.try_recv() {
            let event = match command {
                Command::Run { request, source } => {
                    match self
                        .player
                        .evaluator
                        .evaluate(&source)
                        .map_err(|error| error.to_string())
                        .and_then(|candidate| self.player.activate(candidate))
                    {
                        Ok((generation, boundary, voices, controls)) => PlayerEvent::Active {
                            request,
                            generation,
                            boundary,
                            voices,
                            controls,
                        },
                        Err(message) => PlayerEvent::Error { request, message },
                    }
                }
                Command::Stop => match self.player.stop() {
                    Ok(()) => PlayerEvent::Stopped,
                    Err(message) => PlayerEvent::RuntimeError(message),
                },
                Command::SetControl { name, value } => {
                    if let Err(message) = self.player.set_control(&name, value) {
                        PlayerEvent::RuntimeError(message)
                    } else {
                        continue;
                    }
                }
                Command::Shutdown => break,
            };
            if self.event_tx.send(event).is_err() {
                return;
            }
        }

        if let Err(message) = self.player.fill_active_lookahead() {
            let _ = self.event_tx.send(PlayerEvent::RuntimeError(message));
        }
    }

    pub fn join(self) {}
}

struct Player {
    evaluator: Evaluator,
    revisions: RevisionSlot<Program>,
    scheduler: ProgramScheduler,
    transport: Transport,
    output: Option<AudioOutput>,
    output_channels: Option<usize>,
    clock_started: Option<Instant>,
    fallback: Option<Arc<Program>>,
    persistent: Option<PersistentRuntime>,
    activation_generation: u64,
}

impl Player {
    fn new() -> Player {
        Player {
            evaluator: Evaluator::default(),
            revisions: RevisionSlot::new(Program::default(), Frac::ZERO),
            scheduler: ProgramScheduler::default(),
            transport: Transport::default(),
            output: None,
            output_channels: None,
            clock_started: None,
            fallback: None,
            persistent: None,
            activation_generation: 0,
        }
    }

    fn activate(
        &mut self,
        mut candidate: Program,
    ) -> Result<(u64, String, usize, Vec<ControlView>), String> {
        let channels = playable_channels(&candidate)?;
        let needs_persistent = needs_persistent_runtime(&candidate);
        let active_persistent = self.persistent.is_some();
        let reuse_persistent = self.output.is_some()
            && active_persistent
            && needs_persistent
            && self.output_channels == Some(channels)
            && candidate.reuse_persistent_from(&self.revisions.active().program);
        let mut persistent = (needs_persistent && !reuse_persistent)
            .then(|| persistent_runtime(&candidate))
            .transpose()?;
        let reset = needs_hard_reset(
            self.output.is_some(),
            active_persistent,
            needs_persistent,
            reuse_persistent,
            self.output_channels,
            channels,
        );
        if reset {
            return self.hard_reset(candidate, channels, persistent);
        }
        if let Some(active_channels) = self.output_channels
            && channels != active_channels
        {
            return Err(format!(
                "this edit outputs {channels} channels, but the open audio stream has \
                 {active_channels}; restart the app to change the output layout"
            ));
        }

        let boundary = self.scheduler.frontier();
        let clock_seconds = self
            .clock_started
            .map(|started| started.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let boundary_seconds = self.transport.cycle_to_seconds(boundary);
        let target_seconds =
            (clock_seconds + LOOKAHEAD_SECONDS).max(boundary_seconds + MIN_REVISION_WINDOW_SECONDS);

        // Lower into a throwaway sequencer first. This proves that every track
        // in the first candidate window is schedulable before either the
        // revision slot or the audible sequencer changes.
        let preview_runtime = if reuse_persistent {
            self.persistent.as_ref()
        } else {
            persistent.as_ref()
        };
        let mut preview = if let Some(runtime) = preview_runtime {
            runtime.sequencer()
        } else {
            let first_template = candidate
                .voice(candidate.tracks[0].voice)
                .expect("playable_channels checked every track");
            PitchScheduler::sequencer(first_template)
        };
        let mut preview_scheduler = ProgramScheduler::new(boundary);
        let preview_tracks = scheduled_tracks(&candidate)?;
        let preview_report = if let Some(runtime) = preview_runtime {
            preview_scheduler
                .fill_routed_to_seconds(
                    target_seconds,
                    preview_tracks,
                    self.transport,
                    &mut preview,
                    runtime.layout(),
                    runtime.controls(),
                )
                .map_err(|error| error.to_string())?
        } else {
            preview_scheduler
                .fill_to_seconds(target_seconds, preview_tracks, self.transport, &mut preview)
                .map_err(|error| error.to_string())?
        };

        if self.output.is_none() {
            self.output = Some(
                match &mut persistent {
                    Some(runtime) => AudioOutput::open_processed(
                        runtime.layout().total_channels(),
                        runtime.layout().main_channels(),
                        runtime.take_processor(),
                    ),
                    None => AudioOutput::open(channels),
                }
                .map_err(|error| error.to_string())?,
            );
            self.output_channels = Some(channels);
        }

        let limits = self.evaluator.limits().graph_publication;
        let previous = Arc::clone(&self.revisions.active().program);
        let generation = self
            .revisions
            .submit(candidate, boundary, |program| program.validate(limits))
            .map_err(|error| error.to_string())?;
        let revision = self.revisions.advance_to(boundary);
        self.fallback = (!previous.tracks.is_empty()).then_some(previous);
        if !reuse_persistent {
            self.persistent = persistent;
        }
        if let Err(error) = self.fill_revision_to(target_seconds, revision.program) {
            self.restore_fallback(target_seconds, &error)?;
            return Err(error);
        }

        if self.clock_started.is_none() {
            self.output
                .as_ref()
                .expect("the output was opened above")
                .play()
                .map_err(|error| error.to_string())?;
            self.clock_started = Some(Instant::now());
        }

        self.activation_generation = self.activation_generation.saturating_add(1);
        let controls = control_views(&self.revisions.active().program, self.persistent.as_ref());
        Ok((
            self.activation_generation.max(generation.get()),
            boundary.to_string(),
            preview_report.voices,
            controls,
        ))
    }

    fn hard_reset(
        &mut self,
        candidate: Program,
        channels: usize,
        mut persistent: Option<PersistentRuntime>,
    ) -> Result<(u64, String, usize, Vec<ControlView>), String> {
        let target_seconds = LOOKAHEAD_SECONDS.max(MIN_REVISION_WINDOW_SECONDS);
        let mut preview = if let Some(runtime) = &persistent {
            runtime.sequencer()
        } else {
            let first = candidate
                .tracks
                .first()
                .and_then(|track| candidate.voice(track.voice))
                .ok_or_else(|| "a voice-only program has no playable track".to_string())?;
            PitchScheduler::sequencer(first)
        };
        let mut preview_scheduler = ProgramScheduler::default();
        let preview_tracks = scheduled_tracks(&candidate)?;
        let report = match &persistent {
            Some(runtime) => preview_scheduler
                .fill_routed_to_seconds(
                    target_seconds,
                    preview_tracks,
                    self.transport,
                    &mut preview,
                    runtime.layout(),
                    runtime.controls(),
                )
                .map_err(|error| error.to_string())?,
            None => preview_scheduler
                .fill_to_seconds(target_seconds, preview_tracks, self.transport, &mut preview)
                .map_err(|error| error.to_string())?,
        };

        // Construct the complete replacement stream while the old stream is
        // still playing. Only a fully lowered, filled, and opened candidate is
        // allowed to interrupt the active program.
        let mut output = match &mut persistent {
            Some(runtime) => AudioOutput::open_processed(
                runtime.layout().total_channels(),
                runtime.layout().main_channels(),
                runtime.take_processor(),
            ),
            None => AudioOutput::open(channels),
        }
        .map_err(|error| error.to_string())?;
        let mut scheduler = ProgramScheduler::default();
        let tracks = scheduled_tracks(&candidate)?;
        match &persistent {
            Some(runtime) => scheduler
                .fill_routed_to_seconds(
                    target_seconds,
                    tracks,
                    self.transport,
                    output.sequencer_mut(),
                    runtime.layout(),
                    runtime.controls(),
                )
                .map_err(|error| error.to_string())?,
            None => scheduler
                .fill_to_seconds(
                    target_seconds,
                    tracks,
                    self.transport,
                    output.sequencer_mut(),
                )
                .map_err(|error| error.to_string())?,
        };

        if let Some(active) = &self.output {
            active.pause().map_err(|error| error.to_string())?;
        }
        if let Err(error) = output.play() {
            if let Some(active) = &self.output {
                let _ = active.play();
            }
            return Err(error.to_string());
        }

        self.activation_generation = self.activation_generation.saturating_add(1);
        let controls = control_views(&candidate, persistent.as_ref());
        self.revisions = RevisionSlot::new(candidate, Frac::ZERO);
        self.scheduler = scheduler;
        self.output = Some(output);
        self.output_channels = Some(channels);
        self.clock_started = Some(Instant::now());
        self.fallback = None;
        self.persistent = persistent;

        Ok((
            self.activation_generation,
            Frac::ZERO.to_string(),
            report.voices,
            controls,
        ))
    }

    /// Silence everything and return to the pre-audio state.
    ///
    /// This is deliberately not a pause. The transport is driven by an
    /// `Instant` taken at activation, so pausing the device would leave the
    /// clock running and there would be no coherent point to resume from.
    /// Stop therefore closes the stream, drops the persistent arena and rewinds
    /// the transport, which makes the next Run a first Run again — the one
    /// path `hard_reset` already guarantees.
    fn stop(&mut self) -> Result<(), String> {
        let Some(output) = self.output.take() else {
            return Ok(());
        };
        let paused = output.pause().map_err(|error| error.to_string());
        drop(output);
        self.revisions = RevisionSlot::new(Program::default(), Frac::ZERO);
        self.scheduler = ProgramScheduler::default();
        self.output_channels = None;
        self.clock_started = None;
        self.fallback = None;
        self.persistent = None;
        paused
    }

    fn set_control(&self, name: &str, value: f64) -> Result<(), String> {
        let runtime = self
            .persistent
            .as_ref()
            .ok_or_else(|| "the active program has no persistent control arena".to_string())?;
        let id = self
            .revisions
            .active()
            .program
            .controls
            .id(name)
            .ok_or_else(|| format!("the active program has no control named {name:?}"))?;
        runtime
            .controls()
            .set(id, value)
            .map_err(|error| error.to_string())
    }

    fn fill_active_lookahead(&mut self) -> Result<(), String> {
        let Some(started) = self.clock_started else {
            return Ok(());
        };
        let target_seconds = started.elapsed().as_secs_f64() + LOOKAHEAD_SECONDS;
        if target_seconds <= self.transport.cycle_to_seconds(self.scheduler.frontier()) {
            return Ok(());
        }
        let program = Arc::clone(&self.revisions.active().program);
        match self.fill_revision_to(target_seconds, program) {
            Ok(()) => Ok(()),
            Err(error) => self.restore_fallback(target_seconds, &error),
        }
    }

    fn fill_revision_to(
        &mut self,
        target_seconds: f64,
        program: Arc<Program>,
    ) -> Result<(), String> {
        let tracks = scheduled_tracks(&program)?;
        if let Some(runtime) = &self.persistent {
            self.scheduler
                .fill_routed_to_seconds(
                    target_seconds,
                    tracks,
                    self.transport,
                    self.output
                        .as_mut()
                        .expect("revision filling requires an audio output")
                        .sequencer_mut(),
                    runtime.layout(),
                    runtime.controls(),
                )
                .map_err(|error| error.to_string())?;
        } else {
            self.scheduler
                .fill_to_seconds(
                    target_seconds,
                    tracks,
                    self.transport,
                    self.output
                        .as_mut()
                        .expect("revision filling requires an audio output")
                        .sequencer_mut(),
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn restore_fallback(
        &mut self,
        target_seconds: f64,
        candidate_error: &str,
    ) -> Result<(), String> {
        let Some(fallback) = self.fallback.take() else {
            return Err(candidate_error.into());
        };
        let boundary = self.scheduler.frontier();
        let limits = self.evaluator.limits().graph_publication;
        let generation = self
            .revisions
            .submit((*fallback).clone(), boundary, |program| {
                program.validate(limits)
            })
            .map_err(|restore_error| {
                format!(
                    "{candidate_error}; restoring the previous program also failed: {restore_error}"
                )
            })?;
        let revision = self.revisions.advance_to(boundary);
        self.fill_revision_to(target_seconds, revision.program)
            .map_err(|restore_error| {
                format!(
                    "{candidate_error}; restoring the previous program also failed: {restore_error}"
                )
            })?;
        Err(format!(
            "the active edit failed while scheduling: {candidate_error}; restored generation {} at cycle {boundary}",
            generation.get()
        ))
    }
}

pub(crate) fn playable_channels(program: &Program) -> Result<usize, String> {
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
                "track {index} has an input graph; live input racks are not connected in the first GUI player yet"
            ));
        }
        if graph.channels() == 0 {
            return Err(format!("track {index} has no audio outputs"));
        }
        match channels {
            Some(expected) if graph.channels() != expected => {
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

pub(crate) fn needs_persistent_runtime(program: &Program) -> bool {
    !program.runs.is_empty()
        || !program.controls.specs().is_empty()
        || program.buses.total_channels() != program.buses.main_channels()
        || program.voices.iter().any(|voice| !voice.sends.is_empty())
}

fn needs_hard_reset(
    output_open: bool,
    active_persistent: bool,
    candidate_persistent: bool,
    reuse_persistent: bool,
    active_channels: Option<usize>,
    candidate_channels: usize,
) -> bool {
    output_open
        && (active_channels != Some(candidate_channels)
            || active_persistent != candidate_persistent
            || (active_persistent && candidate_persistent && !reuse_persistent))
}

pub(crate) fn persistent_runtime(program: &Program) -> Result<PersistentRuntime, String> {
    let patches = program
        .runs
        .iter()
        .enumerate()
        .map(|(index, id)| {
            program
                .patch(*id)
                .ok_or_else(|| format!("run {index} refers to a missing patch"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    PersistentRuntime::new(&program.buses, &program.controls, patches)
        .map_err(|error| error.to_string())
}

fn control_views(program: &Program, persistent: Option<&PersistentRuntime>) -> Vec<ControlView> {
    program
        .controls
        .specs()
        .iter()
        .map(|spec| ControlView {
            name: spec.name.clone(),
            min: spec.min,
            max: spec.max,
            value: persistent
                .zip(program.controls.id(&spec.name))
                .and_then(|(runtime, id)| runtime.controls().value(id).ok())
                .unwrap_or(spec.default),
        })
        .collect()
}

fn scheduled_tracks(program: &Program) -> Result<Vec<ScheduledTrack<'_>>, String> {
    program
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let template = program
                .voice(track.voice)
                .ok_or_else(|| format!("track {index} refers to a missing voice"))?;
            Ok(ScheduledTrack::new(&track.pattern, template))
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn run_worker(command_rx: Receiver<Command>, event_tx: Sender<PlayerEvent>) {
    let mut player = Player::new();
    let (evaluation_tx, evaluation_rx) = mpsc::channel();
    let mut evaluation_thread = None;
    // The request an in-flight evaluation is allowed to activate. A Stop
    // clears it, so an evaluation that was already running cannot reopen the
    // audio device after the user has asked for silence.
    let mut awaited = None;
    loop {
        match command_rx.recv_timeout(POLL_INTERVAL) {
            Ok(Command::Run { request, source }) => {
                if evaluation_thread.is_some() {
                    let event = PlayerEvent::Error {
                        request,
                        message: "an evaluation is already in progress".into(),
                    };
                    if event_tx.send(event).is_err() {
                        break;
                    }
                } else {
                    awaited = Some(request);
                    let evaluator = player.evaluator;
                    let result_tx = evaluation_tx.clone();
                    evaluation_thread = Some(
                        thread::Builder::new()
                            .name("apteronotus-evaluation".into())
                            .spawn(move || {
                                let result = evaluator
                                    .evaluate(&source)
                                    .map_err(|error| error.to_string());
                                let _ = result_tx.send((request, result));
                            })
                            .expect("failed to start an evaluation job"),
                    );
                }
            }
            Ok(Command::Stop) => {
                awaited = None;
                let event = match player.stop() {
                    Ok(()) => PlayerEvent::Stopped,
                    Err(message) => PlayerEvent::RuntimeError(message),
                };
                if event_tx.send(event).is_err() {
                    break;
                }
            }
            Ok(Command::SetControl { name, value }) => {
                if let Err(message) = player.set_control(&name, value)
                    && event_tx.send(PlayerEvent::RuntimeError(message)).is_err()
                {
                    break;
                }
            }
            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        if let Ok((request, result)) = evaluation_rx.try_recv() {
            if let Some(thread) = evaluation_thread.take() {
                let _ = thread.join();
            }
            if awaited != Some(request) {
                // A Stop landed while this evaluation was running. Discard it
                // rather than activating a program nobody is waiting for.
                continue;
            }
            awaited = None;
            let event = match result.and_then(|candidate| player.activate(candidate)) {
                Ok((generation, boundary, voices, controls)) => PlayerEvent::Active {
                    request,
                    generation,
                    boundary,
                    voices,
                    controls,
                },
                Err(message) => PlayerEvent::Error { request, message },
            };
            if event_tx.send(event).is_err() {
                break;
            }
        }

        if let Err(message) = player.fill_active_lookahead()
            && event_tx.send(PlayerEvent::RuntimeError(message)).is_err()
        {
            break;
        }
    }
    if let Some(thread) = evaluation_thread {
        let _ = thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::{control_views, needs_hard_reset, persistent_runtime, playable_channels};
    use apteronotus_lua::evaluate;

    #[test]
    fn player_accepts_the_starter_program_shape() {
        let program = evaluate(
            r#"
            local v = voice {
              graph = function(n)
                return sine(n.hz) >> pan(n.pan)
              end,
            }
            play(v, "c4 e4 g4")
            "#,
        )
        .unwrap();
        assert_eq!(playable_channels(&program).unwrap(), 2);
    }

    #[test]
    fn player_rejects_mixed_track_layouts_before_opening_audio() {
        let program = evaluate(
            r#"
            local mono = voice { graph = function(n) return sine(n.hz) end }
            local stereo = voice {
              graph = function(n) return sine(n.hz) >> pan(0) end
            }
            play(mono, "c4")
            play(stereo, "e4")
            "#,
        )
        .unwrap();
        assert!(playable_channels(&program).is_err());
    }

    #[test]
    fn player_accepts_an_autonomous_persistent_program() {
        let program = evaluate(
            r#"
            local field = control {
              name = "field",
              range = { 0, 1 },
              default = 0.5,
            }
            local drone = patch {
              graph = function()
                return sine(110) * field * 0.1 >> pan(0)
              end,
            }
            run(drone)
            "#,
        )
        .unwrap();
        assert_eq!(playable_channels(&program).unwrap(), 2);
        let runtime = persistent_runtime(&program).unwrap();
        assert_eq!(runtime.runs(), 1);
        assert_eq!(runtime.layout().main_channels(), 2);
    }

    #[test]
    fn retained_control_views_report_live_values_not_defaults() {
        let program = evaluate(
            r#"
            local field = control {
              name = "field",
              range = { 0, 1 },
              default = 0.2,
            }
            local drone = patch {
              graph = function() return sine(110) * field * 0.1 >> pan(0) end,
            }
            run(drone)
            "#,
        )
        .unwrap();
        let runtime = persistent_runtime(&program).unwrap();
        runtime
            .controls()
            .set(program.controls.id("field").unwrap(), 0.73)
            .unwrap();
        let views = control_views(&program, Some(&runtime));
        assert!((views[0].value - 0.73).abs() < 1.0e-6);
    }

    #[test]
    fn player_rejects_a_run_that_cannot_consume_the_whole_stem_layout() {
        let program = evaluate(
            r#"
            local partial = patch {
              inputs = 1,
              graph = function(c)
                return c.input >> pan(0)
              end,
            }
            run(partial)
            "#,
        )
        .unwrap();
        let error = persistent_runtime(&program)
            .err()
            .expect("partial stem input must be rejected");
        assert!(error.contains("consume all 2 flattened main/bus lanes"));
    }

    #[test]
    fn persistent_and_layout_changes_request_a_transport_reset() {
        assert!(!needs_hard_reset(false, false, true, false, None, 2));
        assert!(!needs_hard_reset(true, false, false, false, Some(2), 2));
        assert!(!needs_hard_reset(true, true, true, true, Some(2), 2));
        assert!(needs_hard_reset(true, true, true, false, Some(2), 2));
        assert!(needs_hard_reset(true, true, false, false, Some(2), 2));
        assert!(needs_hard_reset(true, false, true, false, Some(2), 2));
        assert!(needs_hard_reset(true, false, false, false, Some(1), 2));
    }
}
