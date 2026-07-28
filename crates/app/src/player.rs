use apteronotus_live::{
    AudioOutput, PitchScheduler, ProgramScheduler, RevisionSlot, ScheduledTrack, Transport,
};
use apteronotus_lua::{Evaluator, Program};
use apteronotus_pattern::Frac;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const LOOKAHEAD_SECONDS: f64 = 0.20;
const MIN_REVISION_WINDOW_SECONDS: f64 = 0.05;
const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub enum Command {
    Run { request: u64, source: String },
    Shutdown,
}

pub enum PlayerEvent {
    Active {
        request: u64,
        generation: u64,
        boundary: String,
        voices: usize,
    },
    Error {
        request: u64,
        message: String,
    },
    RuntimeError(String),
}

pub struct PlayerWorker {
    thread: Option<JoinHandle<()>>,
}

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
        }
    }

    fn activate(&mut self, candidate: Program) -> Result<(u64, String, usize), String> {
        let channels = playable_channels(&candidate)?;
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
        let first_template = candidate
            .voice(candidate.tracks[0].voice)
            .expect("playable_channels checked every track");
        let mut preview = PitchScheduler::sequencer(first_template);
        let mut preview_scheduler = ProgramScheduler::new(boundary);
        let preview_tracks = scheduled_tracks(&candidate)?;
        let preview_report = preview_scheduler
            .fill_to_seconds(target_seconds, preview_tracks, self.transport, &mut preview)
            .map_err(|error| error.to_string())?;

        if self.output.is_none() {
            self.output = Some(AudioOutput::open(channels).map_err(|error| error.to_string())?);
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

        Ok((
            generation.get(),
            boundary.to_string(),
            preview_report.voices,
        ))
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

fn playable_channels(program: &Program) -> Result<usize, String> {
    if program.tracks.is_empty() {
        return Err("the program has no tracks; add play(voice, pattern)".into());
    }
    if !program.runs.is_empty() {
        return Err(
            "persistent run(patch) processors are not connected in the first GUI player yet".into(),
        );
    }
    if !program.controls.specs().is_empty() {
        return Err(
            "writable program controls are not connected in the first GUI player yet".into(),
        );
    }

    let mut channels = None;
    for (index, track) in program.tracks.iter().enumerate() {
        let graph = program
            .voice(track.voice)
            .ok_or_else(|| format!("track {index} refers to a missing voice"))?;
        if graph.inputs != 0 {
            return Err(format!(
                "track {index} has an input graph; live input racks are not connected in the first GUI player yet"
            ));
        }
        if !graph.sends.is_empty() {
            return Err(format!(
                "track {index} has bus sends; routed bus processing is not connected in the first GUI player yet"
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
    Ok(channels.expect("at least one track was checked"))
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

fn run_worker(command_rx: Receiver<Command>, event_tx: Sender<PlayerEvent>) {
    let mut player = Player::new();
    let (evaluation_tx, evaluation_rx) = mpsc::channel();
    let mut evaluation_thread = None;
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
            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        if let Ok((request, result)) = evaluation_rx.try_recv() {
            if let Some(thread) = evaluation_thread.take() {
                let _ = thread.join();
            }
            let event = match result.and_then(|candidate| player.activate(candidate)) {
                Ok((generation, boundary, voices)) => PlayerEvent::Active {
                    request,
                    generation,
                    boundary,
                    voices,
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
    use super::playable_channels;
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
}
