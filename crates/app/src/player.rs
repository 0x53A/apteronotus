use crate::midi_input::{MidiPacket, MidiReceiver};
use apteronotus_live::{
    AudioOutput, ExternalOnset, InputBinding, MasterGain, MidiMessage, MidiVoices,
    PersistentRuntime, PitchScheduler, ProgramScheduler, RevisionSlot, RoutedRuntime,
    schedule_external_routed,
};
use apteronotus_lua::{Evaluator, Program, Track};
use apteronotus_pattern::{ControlValue, Frac, Span, Value};
// The device-free half of a Run — what the program is, and whether it reaches
// audio at all — is shared with the offline renderer. Two copies of this would
// mean the file a render is measured from could disagree with the sound.
use apteronotus_render::persistent_runtime_prepared;
pub(crate) use apteronotus_render::{
    needs_persistent_runtime, playable_channels, scheduled_runs, scheduled_tracks,
};
#[cfg(test)]
pub(crate) use apteronotus_render::{persistent_runtime, persistent_runtime_at};
use apteronotus_synth::{ParamId, ParamValue};
use fundsp::prelude32::AudioUnit;
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
const MIN_EXTERNAL_LATENCY_SECONDS: f64 = 0.03;
const MIDI_LEAD_SECONDS: f64 = 0.010;
#[cfg(not(target_arch = "wasm32"))]
const POLL_INTERVAL: Duration = Duration::from_millis(5);
#[cfg(not(target_arch = "wasm32"))]
const REPLACEMENT_LEAD_SECONDS: f64 = LOOKAHEAD_SECONDS * 2.0;
#[cfg(not(target_arch = "wasm32"))]
const REPLACEMENT_CROSSFADE: Duration = Duration::from_millis(80);

pub enum Command {
    Run {
        request: u64,
        source: String,
        externals: apteronotus_lua::ExternalRegistry,
    },
    Stop,
    MidiPanic,
    SetControl {
        name: String,
        value: f64,
    },
    /// Move the master fader, in decibels. Unlike `SetControl` this is valid
    /// with nothing playing: the level is the player's, not the program's.
    SetVolume {
        decibels: f32,
    },
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
        program: Arc<Program>,
        origin: Instant,
        effective_at: Frac,
        voices: usize,
        controls: Vec<ControlView>,
        warning: Option<String>,
    },
    Error {
        request: u64,
        message: String,
    },
    ControlsChanged(Vec<ControlView>),
    OutputWorkload(apteronotus_live::OutputWorkload),
    Stopped,
    RuntimeError(String),
}

struct Activation {
    generation: u64,
    program: Arc<Program>,
    origin: Instant,
    effective_at: Frac,
    voices: usize,
    controls: Vec<ControlView>,
    warning: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
pub struct PlayerWorker {
    thread: Option<JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl PlayerWorker {
    pub fn spawn(midi_rx: MidiReceiver) -> (PlayerWorker, Sender<Command>, Receiver<PlayerEvent>) {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("apteronotus-player".into())
            .spawn(move || run_worker(command_rx, event_tx, midi_rx))
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
    midi_rx: MidiReceiver,
}

#[cfg(target_arch = "wasm32")]
impl PlayerWorker {
    pub fn spawn(midi_rx: MidiReceiver) -> (PlayerWorker, Sender<Command>, Receiver<PlayerEvent>) {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        (
            PlayerWorker {
                command_rx,
                event_tx,
                player: Player::new(),
                midi_rx,
            },
            command_tx,
            event_rx,
        )
    }

    pub fn pump(&mut self) {
        while let Ok(command) = self.command_rx.try_recv() {
            let event = match command {
                Command::Run {
                    request,
                    source,
                    externals,
                } => {
                    match crate::externals::evaluate(self.player.evaluator, &source, &externals)
                        .and_then(|candidate| self.player.activate(candidate))
                    {
                        Ok(activation) => PlayerEvent::Active {
                            request,
                            generation: activation.generation,
                            program: activation.program,
                            origin: activation.origin,
                            effective_at: activation.effective_at,
                            voices: activation.voices,
                            controls: activation.controls,
                            warning: activation.warning,
                        },
                        Err(message) => PlayerEvent::Error { request, message },
                    }
                }
                Command::MidiPanic => {
                    self.midi_rx.drain();
                    self.player.midi_panic();
                    continue;
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
                Command::SetVolume { decibels } => {
                    self.player.set_volume(decibels);
                    continue;
                }
                Command::Shutdown => break,
            };
            if self.event_tx.send(event).is_err() {
                return;
            }
        }

        if let Err(message) = self.player.poll_midi(&self.midi_rx) {
            let _ = self.event_tx.send(PlayerEvent::RuntimeError(message));
        }
        if let Some(controls) = self.player.take_midi_controls() {
            let _ = self.event_tx.send(PlayerEvent::ControlsChanged(controls));
        }
        if let Some(workload) = self.player.take_workload_update() {
            let _ = self.event_tx.send(PlayerEvent::OutputWorkload(workload));
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
    output: Option<AudioOutput>,
    output_channels: Option<usize>,
    clock_started: Option<Instant>,
    /// Absolute transport seconds represented by local time zero in the
    /// current output stream's sequencer.
    sequencer_origin_seconds: f64,
    fallback: Option<Arc<Program>>,
    persistent: Option<PersistentRuntime>,
    external_levels: Vec<bool>,
    external_ordinal: u64,
    midi_voices: MidiVoices,
    bowed_keys:
        std::collections::HashMap<apteronotus_synth::ControlId, apteronotus_live::BowedKeyboard>,
    bowed_ports: std::collections::HashMap<
        apteronotus_synth::ControlId,
        apteronotus_synth::bowed_host::BowedHandle,
    >,
    midi_program: Arc<Program>,
    midi_pending: std::collections::VecDeque<(f64, Arc<Program>)>,
    midi_ordinal: u64,
    midi_controls_changed: bool,
    activation_generation: u64,
    /// Where the master fader sits. The *number* is the player's, because the
    /// fader itself belongs to a stream and a hard reset opens a new one; a
    /// level that reset with the transport would be a fader the user has to
    /// find again after every incompatible edit.
    volume_decibels: f32,
    reported_prepared_fault: Option<apteronotus_synth::ExternalFault>,
    workload_reported_frames: u64,
}

impl Player {
    fn take_workload_update(&mut self) -> Option<apteronotus_live::OutputWorkload> {
        let output = self.output.as_ref()?;
        let observed = output.workload()?;
        if self.workload_reported_frames == 0
            || observed.frames
                >= self
                    .workload_reported_frames
                    .saturating_add(output.sample_rate() as u64)
            || observed.frames < self.workload_reported_frames
        {
            self.workload_reported_frames = observed.frames;
            Some(observed)
        } else {
            None
        }
    }

    fn new() -> Player {
        Player {
            evaluator: Evaluator::default(),
            revisions: RevisionSlot::new(Program::default(), Frac::ZERO),
            scheduler: ProgramScheduler::default(),
            output: None,
            output_channels: None,
            clock_started: None,
            sequencer_origin_seconds: 0.0,
            fallback: None,
            persistent: None,
            external_levels: Vec::new(),
            external_ordinal: 0,
            midi_voices: MidiVoices::default(),
            bowed_keys: std::collections::HashMap::new(),
            bowed_ports: std::collections::HashMap::new(),
            midi_program: Arc::new(Program::default()),
            midi_pending: Default::default(),
            midi_ordinal: 0,
            midi_controls_changed: false,
            activation_generation: 0,
            volume_decibels: MasterGain::MAX_DECIBELS,
            reported_prepared_fault: None,
            workload_reported_frames: 0,
        }
    }

    /// Move the master fader. An atomic store into a node the running graph
    /// already holds; nothing is re-evaluated, re-lowered or restarted, and it
    /// is equally valid with the device closed.
    fn set_volume(&mut self, decibels: f32) {
        self.volume_decibels = decibels.clamp(MasterGain::MIN_DECIBELS, MasterGain::MAX_DECIBELS);
        if let Some(output) = &self.output {
            output.master().set_decibels(self.volume_decibels);
        }
    }

    fn activate(&mut self, candidate: Program) -> Result<Activation, String> {
        let clock = self.clock_started;
        let origin = self.sequencer_origin_seconds;
        if let Some(runtime) = &self.persistent {
            for binding in &self.revisions.active().program.midi_violins {
                if let Some(handle) = runtime.controls().bowed_handle(binding.key) {
                    self.bowed_ports.insert(binding.key, handle);
                }
            }
        }
        let activation = self.activate_candidate(candidate)?;
        if clock != self.clock_started || origin != self.sequencer_origin_seconds {
            self.midi_voices = MidiVoices::default();
            self.bowed_keys.clear();
            self.bowed_ports.clear();
            self.midi_program = Arc::new(Program::default());
            self.midi_pending.clear();
        }
        let effective = activation
            .program
            .tempo
            .cycle_to_seconds(activation.effective_at);
        self.midi_pending
            .push_back((effective, Arc::clone(&activation.program)));
        Ok(activation)
    }

    fn activate_candidate(&mut self, mut candidate: Program) -> Result<Activation, String> {
        let channels = playable_channels(&candidate)?;
        let needs_persistent = needs_persistent_runtime(&candidate);
        let active_persistent = self.persistent.is_some();
        let tempo_changed =
            self.output.is_some() && candidate.tempo != self.revisions.active().program.tempo;
        let mut room_edits = Vec::new();
        let reuse_persistent = self.output.is_some()
            && !tempo_changed
            && active_persistent
            && needs_persistent
            && self.output_channels == Some(channels)
            && if let Some(changes) =
                candidate.reuse_with_room_edits(&self.revisions.active().program)
            {
                room_edits = self
                    .persistent
                    .as_ref()
                    .expect("active arena")
                    .controls()
                    .prepare_room_edits(&changes)
                    .map_err(str::to_owned)?;
                true
            } else {
                false
            };
        let reset = needs_hard_reset(
            self.output.is_some(),
            active_persistent,
            needs_persistent,
            reuse_persistent,
            tempo_changed,
            self.output_channels,
            channels,
        );
        #[cfg(not(target_arch = "wasm32"))]
        if can_crossfade_replace(
            self.output.is_some(),
            active_persistent,
            needs_persistent,
            reuse_persistent,
            tempo_changed,
            self.output_channels,
            channels,
        ) {
            return self.crossfade_replace(candidate, channels);
        }
        // Select the clock and prepare the new arena together. The candidate
        // stream remains paused; no active state changes if preparation fails.
        let (mut pending_output, persistent) = if needs_persistent && !reuse_persistent {
            let (output, runtime) =
                open_prepared_persistent_output(&candidate, 0.0, self.volume_decibels)?;
            (Some(output), Some(runtime))
        } else {
            (None, None)
        };
        if reset {
            return self.hard_reset(candidate, channels, persistent, pending_output);
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
        let boundary_seconds = candidate.tempo.cycle_to_seconds(boundary);
        // Compatible revisions share a tempo map: any tempo change took the
        // hard-reset path above. One transport origin can therefore place both
        // the old audible revision and this future activation unambiguously.
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
        if let Some(output) = pending_output.as_ref().or(self.output.as_ref()) {
            preview.set_sample_rate(output.sample_rate());
        }
        let mut preview_scheduler = ProgramScheduler::new(boundary);
        let preview_tracks = scheduled_tracks(&candidate)?;
        let preview_runs = scheduled_runs(&candidate)?;
        let preview_report = if let Some(runtime) = preview_runtime {
            preview_scheduler
                .fill_routed_program_to_seconds_tempo_map_from(
                    target_seconds,
                    preview_tracks,
                    preview_runs,
                    &candidate.tempo,
                    &mut preview,
                    RoutedRuntime::new_rebased(
                        runtime.layout(),
                        runtime.controls(),
                        self.sequencer_origin_seconds,
                    ),
                )
                .map_err(|error| error.to_string())?
        } else {
            preview_scheduler
                .fill_to_seconds_tempo_map_from(
                    target_seconds,
                    preview_tracks,
                    &candidate.tempo,
                    &mut preview,
                    self.sequencer_origin_seconds,
                )
                .map_err(|error| error.to_string())?
        };

        if self.output.is_none() {
            self.output = Some(match pending_output.take() {
                Some(output) => output,
                None => AudioOutput::open_at_level(channels, self.volume_decibels)
                    .map_err(|error| error.to_string())?,
            });
            self.output_channels = Some(channels);
        }

        let limits = self.evaluator.limits().graph_publication;
        let previous = Arc::clone(&self.revisions.active().program);
        let generation = self
            .revisions
            .submit(candidate, boundary, |program| program.validate(limits))
            .map_err(|error| error.to_string())?;
        let revision = self.revisions.advance_to(boundary);
        self.fallback = (!previous.tracks.is_empty()
            || !previous.midi_bindings.is_empty()
            || !previous.midi_violins.is_empty())
        .then_some(previous);
        if !reuse_persistent {
            self.persistent = persistent;
            self.reported_prepared_fault = None;
        }
        self.sync_external_levels(&revision.program);
        // Track positions are not yet reconciled across source evaluations.
        // Starting the next glide at its target is preferable to carrying a
        // previous pitch from an unrelated track that moved in declaration
        // order. Preserve the old history until the candidate has proved it
        // can fill, so rollback remains faithful.
        let previous_scheduler = self.scheduler.clone();
        self.scheduler.clear_track_history();
        if let Err(error) = self.fill_revision_to(target_seconds, Arc::clone(&revision.program)) {
            self.scheduler = previous_scheduler;
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

        for edit in room_edits {
            edit.commit();
        }

        self.activation_generation = self.activation_generation.saturating_add(1);
        let controls = control_views(&self.revisions.active().program, self.persistent.as_ref());
        Ok(Activation {
            generation: self.activation_generation.max(generation.get()),
            program: Arc::clone(&revision.program),
            origin: self
                .clock_started
                .expect("activation started the transport"),
            effective_at: boundary,
            voices: preview_report.voices,
            controls,
            warning: self.input_warning(),
        })
    }

    /// Replace incompatible persistent state without restarting musical time.
    ///
    /// The old program is first filled to a future exact frontier. The new
    /// arena and stream are then lowered against that absolute coordinate and
    /// remain paused and silent until the frontier arrives. No fallible state
    /// change occurs after the candidate starts: from that point the bounded
    /// gain overlap completes and the old stream is dropped.
    #[cfg(not(target_arch = "wasm32"))]
    fn crossfade_replace(
        &mut self,
        candidate: Program,
        channels: usize,
    ) -> Result<Activation, String> {
        let started = self
            .clock_started
            .expect("replacement requires a running transport");
        let active = Arc::clone(&self.revisions.active().program);
        let reserve_until = started.elapsed().as_secs_f64() + REPLACEMENT_LEAD_SECONDS;
        self.fill_revision_to(reserve_until, active)?;

        let boundary = self.scheduler.frontier();
        let boundary_seconds = candidate.tempo.cycle_to_seconds(boundary);
        let target_seconds = boundary_seconds + LOOKAHEAD_SECONDS;
        let (mut output, persistent) = open_prepared_persistent_output(
            &candidate,
            boundary_seconds,
            MasterGain::MIN_DECIBELS,
        )?;
        if output.sample_rate() != self.output.as_ref().expect("active output").sample_rate() {
            return Err(
                "replacement device clock changed; previous performance remains active".into(),
            );
        }

        // Prove the complete candidate window against the selected clock.
        // The candidate stream is paused and active handles are untouched.
        let mut preview = persistent.sequencer();
        preview.set_sample_rate(output.sample_rate());
        let mut preview_scheduler = ProgramScheduler::new(boundary);
        let preview_report = preview_scheduler
            .fill_routed_program_to_seconds_tempo_map_from(
                target_seconds,
                scheduled_tracks(&candidate)?,
                scheduled_runs(&candidate)?,
                &candidate.tempo,
                &mut preview,
                RoutedRuntime::new_rebased(
                    persistent.layout(),
                    persistent.controls(),
                    boundary_seconds,
                ),
            )
            .map_err(|error| error.to_string())?;

        let mut scheduler = ProgramScheduler::new(boundary);
        scheduler
            .fill_routed_program_to_seconds_tempo_map_from(
                target_seconds,
                scheduled_tracks(&candidate)?,
                scheduled_runs(&candidate)?,
                &candidate.tempo,
                output.sequencer_mut(),
                RoutedRuntime::new_rebased(
                    persistent.layout(),
                    persistent.controls(),
                    boundary_seconds,
                ),
            )
            .map_err(|error| error.to_string())?;

        let remaining = boundary_seconds - started.elapsed().as_secs_f64();
        if remaining <= 0.0 {
            return Err(
                "persistent replacement missed its prepared musical boundary; the previous program remains active"
                    .into(),
            );
        }
        thread::sleep(Duration::from_secs_f64(remaining));
        output.play().map_err(|error| error.to_string())?;

        output.master().set_decibels(self.volume_decibels);
        self.output
            .as_ref()
            .expect("replacement requires an active output")
            .master()
            .set_decibels(MasterGain::MIN_DECIBELS);
        thread::sleep(REPLACEMENT_CROSSFADE);

        let previous_output = self.output.replace(output);
        if let Some(previous_output) = previous_output {
            let _ = previous_output.pause();
        }
        self.activation_generation = self.activation_generation.saturating_add(1);
        self.revisions = RevisionSlot::new(candidate, boundary);
        self.scheduler = scheduler;
        self.output_channels = Some(channels);
        self.sequencer_origin_seconds = boundary_seconds;
        self.fallback = None;
        self.persistent = Some(persistent);
        self.reported_prepared_fault = None;
        let active = Arc::clone(&self.revisions.active().program);
        self.sync_external_levels(&active);

        Ok(Activation {
            generation: self.activation_generation,
            program: Arc::clone(&active),
            origin: started,
            effective_at: boundary,
            voices: preview_report.voices,
            controls: control_views(&active, self.persistent.as_ref()),
            warning: self.input_warning(),
        })
    }

    fn hard_reset(
        &mut self,
        candidate: Program,
        channels: usize,
        persistent: Option<PersistentRuntime>,
        pending_output: Option<AudioOutput>,
    ) -> Result<Activation, String> {
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
        if let Some(output) = pending_output.as_ref() {
            preview.set_sample_rate(output.sample_rate());
        }
        let mut preview_scheduler = ProgramScheduler::default();
        let preview_tracks = scheduled_tracks(&candidate)?;
        let preview_runs = scheduled_runs(&candidate)?;
        let report = match &persistent {
            Some(runtime) => preview_scheduler
                .fill_routed_program_to_seconds_tempo_map(
                    target_seconds,
                    preview_tracks,
                    preview_runs,
                    &candidate.tempo,
                    &mut preview,
                    RoutedRuntime::new(runtime.layout(), runtime.controls()),
                )
                .map_err(|error| error.to_string())?,
            None => preview_scheduler
                .fill_to_seconds_tempo_map(
                    target_seconds,
                    preview_tracks,
                    &candidate.tempo,
                    &mut preview,
                )
                .map_err(|error| error.to_string())?,
        };

        // Construct the complete replacement stream while the old stream is
        // still playing. Only a fully lowered, filled, and opened candidate is
        // allowed to interrupt the active program.
        let mut output = match pending_output {
            Some(output) => output,
            None => AudioOutput::open_at_level(channels, self.volume_decibels)
                .map_err(|error| error.to_string())?,
        };
        let mut scheduler = ProgramScheduler::default();
        let tracks = scheduled_tracks(&candidate)?;
        let runs = scheduled_runs(&candidate)?;
        match &persistent {
            Some(runtime) => scheduler
                .fill_routed_program_to_seconds_tempo_map(
                    target_seconds,
                    tracks,
                    runs,
                    &candidate.tempo,
                    output.sequencer_mut(),
                    RoutedRuntime::new(runtime.layout(), runtime.controls()),
                )
                .map_err(|error| error.to_string())?,
            None => scheduler
                .fill_to_seconds_tempo_map(
                    target_seconds,
                    tracks,
                    &candidate.tempo,
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
        self.sequencer_origin_seconds = 0.0;
        self.fallback = None;
        self.persistent = persistent;
        self.reported_prepared_fault = None;
        let active = Arc::clone(&self.revisions.active().program);
        self.sync_external_levels(&active);

        Ok(Activation {
            generation: self.activation_generation,
            program: Arc::clone(&active),
            origin: self
                .clock_started
                .expect("hard reset started the transport"),
            effective_at: Frac::ZERO,
            voices: report.voices,
            controls,
            warning: self.input_warning(),
        })
    }

    fn input_warning(&self) -> Option<String> {
        match self.output.as_ref().map(AudioOutput::input_binding) {
            Some(InputBinding::Fallback { reason }) => Some(reason.clone()),
            Some(InputBinding::Live {
                device,
                channels,
                requested_channels,
                external_channels,
            }) if channels < requested_channels || channels < external_channels => Some(format!(
                "{device:?} supplies {channels} of {external_channels} declared input lanes \
                 ({requested_channels} requested for the selected logical input); the remaining \
                 lanes use the declared silence fallback"
            )),
            _ => None,
        }
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
        self.midi_voices = MidiVoices::default();
        self.bowed_keys.clear();
        self.bowed_ports.clear();
        self.midi_program = Arc::new(Program::default());
        self.midi_pending.clear();
        let Some(output) = self.output.take() else {
            return Ok(());
        };
        let paused = output.pause().map_err(|error| error.to_string());
        drop(output);
        self.revisions = RevisionSlot::new(Program::default(), Frac::ZERO);
        self.scheduler = ProgramScheduler::default();
        self.output_channels = None;
        self.clock_started = None;
        self.sequencer_origin_seconds = 0.0;
        self.fallback = None;
        self.persistent = None;
        self.reported_prepared_fault = None;
        self.external_levels.clear();
        self.external_ordinal = 0;
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
        let fault = self
            .persistent
            .as_ref()
            .and_then(|runtime| runtime.controls().external_fault());
        if new_prepared_fault(&mut self.reported_prepared_fault, fault.clone()) {
            return Err(fault.expect("new fault is present").to_string());
        }
        let target_seconds = started.elapsed().as_secs_f64() + LOOKAHEAD_SECONDS;
        if target_seconds
            <= self
                .revisions
                .active()
                .program
                .tempo
                .cycle_to_seconds(self.scheduler.frontier())
        {
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
            let runs = scheduled_runs(&program)?;
            self.scheduler
                .fill_routed_program_to_seconds_tempo_map_from(
                    target_seconds,
                    tracks,
                    runs,
                    &program.tempo,
                    self.output
                        .as_mut()
                        .expect("revision filling requires an audio output")
                        .sequencer_mut(),
                    RoutedRuntime::new_rebased(
                        runtime.layout(),
                        runtime.controls(),
                        self.sequencer_origin_seconds,
                    ),
                )
                .map_err(|error| error.to_string())?;
        } else {
            self.scheduler
                .fill_to_seconds_tempo_map_from(
                    target_seconds,
                    tracks,
                    &program.tempo,
                    self.output
                        .as_mut()
                        .expect("revision filling requires an audio output")
                        .sequencer_mut(),
                    self.sequencer_origin_seconds,
                )
                .map_err(|error| error.to_string())?;
        }
        self.poll_external_triggers(&program)?;
        Ok(())
    }

    fn midi_panic(&mut self) {
        for port in self.bowed_ports.values() {
            port.panic_detach();
        }
        if let Some(runtime) = &self.persistent {
            for binding in &self.midi_program.midi_violins {
                if let Some(handle) = runtime.controls().bowed_handle(binding.key) {
                    handle.panic_detach();
                }
            }
        }
        self.bowed_keys.clear();
        if let Some(output) = &mut self.output {
            let at = output.rendered_seconds() + MIDI_LEAD_SECONDS;
            self.midi_voices
                .all_notes_off(None, at, output.sequencer_mut());
        }
    }

    fn poll_midi(&mut self, receiver: &MidiReceiver) -> Result<(), String> {
        let (overflow, packets) = receiver.drain();
        if overflow {
            self.midi_panic();
            return Err(
                "MIDI input queue overflowed; held keys released. Play the keys again.".into(),
            );
        }
        if let Some(output) = &self.output {
            let now = output.rendered_seconds();
            self.midi_voices.prune(now);
            // Retire elapsed revisions even when no keys arrive. Keeping them
            // until the next key would retain every edit in an idle session.
            self.advance_midi_program(self.sequencer_origin_seconds + now);
        }
        for packet in packets {
            if let Err(error) = self.midi_packet(packet) {
                self.midi_panic();
                return Err(error);
            }
        }
        Ok(())
    }

    fn take_midi_controls(&mut self) -> Option<Vec<ControlView>> {
        if !std::mem::take(&mut self.midi_controls_changed) {
            return None;
        }
        // The UI exposes the latest accepted control layout. By name it also
        // reflects CC writes from the revision still sounding at the frontier.
        Some(control_views(
            &self.revisions.active().program,
            self.persistent.as_ref(),
        ))
    }

    fn advance_midi_program(&mut self, seconds: f64) {
        while self
            .midi_pending
            .front()
            .is_some_and(|(at, _)| *at <= seconds)
        {
            let next = self
                .midi_pending
                .pop_front()
                .expect("checked pending revision")
                .1;
            for binding in &self.midi_program.midi_violins {
                if !next.midi_violins.contains(binding) {
                    if let Some(runtime) = &self.persistent {
                        if let Some(handle) = runtime.controls().bowed_handle(binding.key) {
                            handle.panic_detach();
                        }
                    }
                    if let Some(port) = self.bowed_ports.remove(&binding.key) {
                        port.panic_detach();
                    }
                    self.bowed_keys.remove(&binding.key);
                }
            }
            self.midi_program = next;
        }
    }

    fn midi_packet(&mut self, packet: MidiPacket) -> Result<(), String> {
        let Some(started) = self.clock_started else {
            return Ok(());
        };
        if packet.observed < started {
            return Ok(());
        }
        let Some(output) = &self.output else {
            return Ok(());
        };
        // Schedule at the device's render frontier, never elapsed wall time.
        // An underrun or browser pause must not become permanent extra latency.
        let at = output.rendered_seconds() + MIDI_LEAD_SECONDS;
        self.advance_midi_program(self.sequencer_origin_seconds + at);
        let (Some(output), Some(runtime)) = (&mut self.output, &self.persistent) else {
            return Ok(());
        };
        for binding in &self.midi_program.midi_violins {
            let handle = runtime
                .controls()
                .bowed_handle(binding.key)
                .or_else(|| self.bowed_ports.get(&binding.key).cloned())
                .ok_or("missing retained violin runtime")?;
            // Retained instruments use their own transport clock, including after a compatible edit.
            let gesture_at = self.sequencer_origin_seconds + at;
            self.bowed_keys
                .entry(binding.key)
                .or_default()
                .message(
                    packet.message,
                    &handle,
                    gesture_at,
                    binding.channel,
                    binding.min_key,
                    binding.max_key,
                )
                .map_err(str::to_string)?;
        }
        match packet.message {
            MidiMessage::ControlChange { channel, cc, value } => {
                self.midi_controls_changed |=
                    apply_midi_control(&self.midi_program, runtime.controls(), channel, cc, value)?;
            }
            MidiMessage::NoteOff { channel, key } => {
                self.midi_voices
                    .note_off(channel, key, at, output.sequencer_mut())
            }
            MidiMessage::AllNotesOff { channel } => {
                self.midi_voices
                    .all_notes_off(channel, at, output.sequencer_mut())
            }
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
            } => {
                // Repeated note-on is an explicit retrigger. Release every old
                // layer for this channel/key before creating the new layers.
                self.midi_voices
                    .note_off(channel, key, at, output.sequencer_mut());
                let ordinal = self.midi_ordinal;
                self.midi_ordinal = self.midi_ordinal.wrapping_add(1);
                for (index, binding) in self.midi_program.midi_bindings.iter().enumerate() {
                    if binding.channel.is_some_and(|c| c != channel) {
                        continue;
                    }
                    let template = self
                        .midi_program
                        .voice(binding.voice)
                        .ok_or_else(|| "MIDI binding refers to a missing voice".to_string())?;
                    let seed = apteronotus_pattern::rand::mix(0x4d49_4449_4b45_5953 ^ ordinal)
                        ^ apteronotus_pattern::rand::mix(
                            (u64::from(channel) << 8) | u64::from(key),
                        )
                        ^ apteronotus_pattern::rand::mix(index as u64);
                    self.midi_voices
                        .note_on(
                            channel,
                            key,
                            velocity,
                            at,
                            seed,
                            template,
                            &apteronotus_synth::EventRouting::new(),
                            output.sequencer_mut(),
                            RoutedRuntime::new(runtime.layout(), runtime.controls()),
                        )
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        Ok(())
    }

    fn sync_external_levels(&mut self, program: &Program) {
        let Some(runtime) = &self.persistent else {
            self.external_levels.clear();
            return;
        };
        self.external_levels = program
            .tracks
            .iter()
            .map(|track| {
                track
                    .external_trigger
                    .and_then(|trigger| runtime.controls().value(trigger).ok())
                    .is_some_and(|value| value >= 0.5)
            })
            .collect();
    }

    fn poll_external_triggers(&mut self, program: &Program) -> Result<(), String> {
        let (Some(runtime), Some(started)) = (&self.persistent, self.clock_started) else {
            return Ok(());
        };
        self.external_levels.resize(program.tracks.len(), false);
        let mut fired = Vec::new();
        for (index, track) in program.tracks.iter().enumerate() {
            let Some(trigger) = track.external_trigger else {
                self.external_levels[index] = false;
                continue;
            };
            let high = runtime
                .controls()
                .value(trigger)
                .map_err(|error| error.to_string())?
                >= 0.5;
            if high && !self.external_levels[index] {
                fired.push(index);
            }
            self.external_levels[index] = high;
        }
        if fired.is_empty() {
            return Ok(());
        }

        let observed_seconds = started.elapsed().as_secs_f64();
        let at_seconds =
            observed_seconds + MIN_EXTERNAL_LATENCY_SECONDS - self.sequencer_origin_seconds;
        let at_cycle = program
            .tempo
            .seconds_to_cycle(observed_seconds)
            .map_err(|error| error.to_string())?;
        for index in fired {
            let track = &program.tracks[index];
            let template = program
                .voice(track.voice)
                .ok_or_else(|| format!("track {index} refers to a missing voice"))?;
            let ordinal = self.external_ordinal;
            let seed = external_event_seed(index, track, ordinal);
            self.external_ordinal = self.external_ordinal.wrapping_add(1);
            if !external_event_passes(track, seed) {
                continue;
            }
            let event_bindings = external_event_bindings(track, at_cycle)?;
            schedule_external_routed(
                ExternalOnset {
                    at_seconds,
                    gate_seconds: 0.01,
                    event_seed: seed,
                },
                template,
                &track.routing,
                &track.onset_bindings,
                &event_bindings,
                self.output
                    .as_mut()
                    .expect("external triggering requires an open output")
                    .sequencer_mut(),
                RoutedRuntime::new(runtime.layout(), runtime.controls()),
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

/// Report one observed failure transition, without flooding each lookahead poll.
fn new_prepared_fault(
    previous: &mut Option<apteronotus_synth::ExternalFault>,
    current: Option<apteronotus_synth::ExternalFault>,
) -> bool {
    let changed = current.is_some() && *previous != current;
    *previous = current;
    changed
}

fn needs_hard_reset(
    output_open: bool,
    active_persistent: bool,
    candidate_persistent: bool,
    reuse_persistent: bool,
    tempo_changed: bool,
    active_channels: Option<usize>,
    candidate_channels: usize,
) -> bool {
    output_open
        && (tempo_changed
            || active_channels != Some(candidate_channels)
            || active_persistent != candidate_persistent
            || (active_persistent && candidate_persistent && !reuse_persistent))
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn can_crossfade_replace(
    output_open: bool,
    active_persistent: bool,
    candidate_persistent: bool,
    reuse_persistent: bool,
    tempo_changed: bool,
    active_channels: Option<usize>,
    candidate_channels: usize,
) -> bool {
    output_open
        && active_persistent
        && candidate_persistent
        && !reuse_persistent
        && !tempo_changed
        && active_channels == Some(candidate_channels)
}

/// Open a paused candidate, retaining exactly the control arena that owns its
/// prepared processor. Neither this helper nor its factory touches Player state.
fn open_prepared_persistent_output(
    program: &Program,
    transport_seconds: f64,
    initial_decibels: f32,
) -> Result<(AudioOutput, PersistentRuntime), String> {
    open_prepared_persistent_with(program, transport_seconds, |prepare| {
        let requested_channels = program
            .audio_inputs
            .specs()
            .first()
            .map_or(0, |s| s.channels);
        AudioOutput::open_prepared_with_default_input_at_level(
            program.buses.total_channels(),
            program.buses.main_channels(),
            program.audio_inputs.total_channels(),
            requested_channels,
            initial_decibels,
            prepare,
        )
        .map_err(|error| error.to_string())
    })
}

/// The injectable opener lets tests exercise the production preparation and
/// ownership transfer without opening speakers. A real opener calls the factory
/// once after selecting its clock and returns a paused stream.
fn open_prepared_persistent_with<O>(
    program: &Program,
    transport_seconds: f64,
    open: impl FnOnce(
        &mut dyn FnMut(f64) -> Result<Box<dyn fundsp::prelude32::AudioUnit>, String>,
    ) -> Result<O, String>,
) -> Result<(O, PersistentRuntime), String> {
    let mut runtime = None;
    let mut called = false;
    let output = open(&mut |rate| {
        if called {
            return Err("persistent preparation factory called more than once".into());
        }
        called = true;
        let mut prepared = persistent_runtime_prepared(program, transport_seconds, rate)?;
        let processor = prepared.take_processor_with_audio_inputs();
        runtime = Some(prepared);
        Ok(processor)
    })?;
    let runtime =
        runtime.ok_or_else(|| "output did not prepare its persistent processor".to_string())?;
    Ok((output, runtime))
}

fn apply_midi_control(
    program: &Program,
    controls: &apteronotus_synth::ControlStore,
    channel: u8,
    cc: u8,
    value: u8,
) -> Result<bool, String> {
    let mut changed = false;
    for binding in &program.midi_controls {
        if binding.cc != cc || binding.channel.is_some_and(|c| c != channel) {
            continue;
        }
        let spec = program
            .controls
            .spec(binding.control)
            .ok_or_else(|| "MIDI mapping refers to a missing control".to_string())?;
        controls
            .set(binding.control, binding.value(spec, value))
            .map_err(|error| error.to_string())?;
        changed = true;
    }
    Ok(changed)
}

fn control_views(program: &Program, persistent: Option<&PersistentRuntime>) -> Vec<ControlView> {
    program
        .controls
        .specs()
        .iter()
        .filter(|spec| !spec.name.starts_with("__apteronotus."))
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

fn external_event_bindings(track: &Track, at: Frac) -> Result<Vec<(ParamId, ParamValue)>, String> {
    let sample = Span::new(at, at);
    track
        .external_controls
        .iter()
        .map(|binding| {
            let event = binding
                .pattern
                .query(sample)
                .into_iter()
                .next()
                .ok_or_else(|| {
                    format!(
                        "external-onset control {:?} has no value at cycle {at}",
                        binding.param
                    )
                })?;
            let value = match event.value {
                Value::Leaf(ControlValue::Number(value)) => ParamValue::Number(value),
                Value::Leaf(ControlValue::Curve(curve)) => ParamValue::Curve(curve),
                Value::Leaf(ControlValue::Text(_))
                | Value::Leaf(ControlValue::Bool(_))
                | Value::Map(_) => {
                    return Err(format!(
                        "external-onset control {:?} is not numeric or curve-valued at cycle {at}",
                        binding.param
                    ));
                }
            };
            Ok((binding.param, value))
        })
        .collect()
}

fn external_event_seed(track_index: usize, track: &Track, ordinal: u64) -> u64 {
    apteronotus_pattern::rand::mix(0x4558_5445_524e_414c)
        ^ apteronotus_pattern::rand::mix(0x5452_4143_4b00_0000 ^ track_index as u64)
        ^ apteronotus_pattern::rand::mix(0x564f_4943_4500_0000 ^ track.voice.index() as u64)
        ^ apteronotus_pattern::rand::mix(0x4f52_4449_4e41_4c00 ^ ordinal)
}

fn external_event_passes(track: &Track, event_seed: u64) -> bool {
    track.external_degrades.iter().all(|degrade| {
        apteronotus_pattern::rand::at(Frac::ZERO, degrade.seed ^ event_seed) >= degrade.amount
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn run_worker(command_rx: Receiver<Command>, event_tx: Sender<PlayerEvent>, midi_rx: MidiReceiver) {
    let mut player = Player::new();
    let (evaluation_tx, evaluation_rx) = mpsc::channel();
    let mut evaluation_thread = None;
    // The request an in-flight evaluation is allowed to activate. A Stop
    // clears it, so an evaluation that was already running cannot reopen the
    // audio device after the user has asked for silence.
    let mut awaited = None;
    loop {
        match command_rx.recv_timeout(POLL_INTERVAL) {
            Ok(Command::Run {
                request,
                source,
                externals,
            }) => {
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
                                let result =
                                    crate::externals::evaluate(evaluator, &source, &externals);
                                let _ = result_tx.send((request, result));
                            })
                            .expect("failed to start an evaluation job"),
                    );
                }
            }
            Ok(Command::MidiPanic) => {
                midi_rx.drain();
                player.midi_panic();
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
            Ok(Command::SetVolume { decibels }) => player.set_volume(decibels),
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
                Ok(activation) => PlayerEvent::Active {
                    request,
                    generation: activation.generation,
                    program: activation.program,
                    origin: activation.origin,
                    effective_at: activation.effective_at,
                    voices: activation.voices,
                    controls: activation.controls,
                    warning: activation.warning,
                },
                Err(message) => PlayerEvent::Error { request, message },
            };
            if event_tx.send(event).is_err() {
                break;
            }
        }

        if let Err(message) = player.poll_midi(&midi_rx) {
            let _ = event_tx.send(PlayerEvent::RuntimeError(message));
        }
        if let Some(controls) = player.take_midi_controls() {
            let _ = event_tx.send(PlayerEvent::ControlsChanged(controls));
        }
        if let Some(workload) = player.take_workload_update() {
            let _ = event_tx.send(PlayerEvent::OutputWorkload(workload));
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

// `Program` is sent from native evaluation into playback and now back to the
// UI as the exact revision used for coordinate-derived highlighting. Keep the
// thread-safety requirement adjacent to that boundary so a future `Rc` fails
// here, not as an obscure channel error.
fn assert_program_send_sync()
where
    Program: Send + Sync,
{
}

const _: fn() = assert_program_send_sync;

#[cfg(test)]
mod tests {
    use super::{
        MasterGain, Player, can_crossfade_replace, control_views, external_event_bindings,
        external_event_passes, external_event_seed, needs_hard_reset, persistent_runtime,
        persistent_runtime_at, playable_channels,
    };
    use apteronotus_lua::evaluate;
    use apteronotus_pattern::Frac;

    #[test]
    fn prepared_runtime_faults_report_once_and_can_report_after_recovery() {
        let fault = Some(apteronotus_synth::ExternalFault {
            instance: 0,
            kind: "test".into(),
            reason: "latched silent".into(),
        });
        let mut previous = None;
        assert!(!super::new_prepared_fault(&mut previous, None));
        assert!(super::new_prepared_fault(&mut previous, fault.clone()));
        assert!(!super::new_prepared_fault(&mut previous, fault.clone()));
        assert!(!super::new_prepared_fault(&mut previous, None));
        assert!(super::new_prepared_fault(&mut previous, fault.clone()));
    }

    #[test]
    fn selected_clock_factory_retains_the_playing_processors_control_arena() {
        let program = evaluate(
            r#"
            local level=control{name="level",range={0,1},default=.2}
            run(patch{graph=function() return sine(1000)*level end})
        "#,
        )
        .unwrap();
        for rate in [32_000.0, 48_000.0, 96_000.0] {
            let (mut unit, runtime) =
                super::open_prepared_persistent_with(&program, 0.0, |prepare| prepare(rate))
                    .unwrap();
            let mut reference = super::persistent_runtime_prepared(&program, 0.0, rate).unwrap();
            let mut expected = reference.take_processor_with_audio_inputs();
            let input = vec![0.; unit.inputs()];
            let mut actual = vec![0.; unit.outputs()];
            let mut target = vec![0.; expected.outputs()];
            for _ in 0..4096 {
                unit.tick(&input, &mut actual);
                expected.tick(&input, &mut target);
                assert_eq!(actual, target);
            }
            runtime
                .controls()
                .set(program.controls.id("level").unwrap(), 0.)
                .unwrap();
            for _ in 0..128 {
                unit.tick(&input, &mut actual);
                assert!(actual.iter().all(|v| *v == 0.));
            }
        }
    }

    #[test]
    fn selected_clock_factory_preserves_transport_coordinate_and_rejects_bad_rate() {
        let program = evaluate(
            r#"
            tempo(120)
            run(patch{graph=function()
                return control_signal("0.2 0.8" >> segment(2),bars(1))
            end})
        "#,
        )
        .unwrap();
        let (mut unit, _) =
            super::open_prepared_persistent_with(&program, 1.5, |prepare| prepare(48_000.))
                .unwrap();
        let mut output = vec![0.; unit.outputs()];
        unit.tick(&vec![0.; unit.inputs()], &mut output);
        assert!(output.iter().all(|x| (*x - 0.8).abs() < 1e-6));
        let result =
            super::open_prepared_persistent_with(&program, 0., |prepare| prepare(f64::NAN));
        assert!(result.err().unwrap().contains("sample rate"));
    }

    #[test]
    fn output_open_failure_does_not_publish_a_candidate_arena() {
        let program = evaluate(r#"run(patch{graph=function()return dc(.2)end})"#).unwrap();
        let result: Result<((), _), String> =
            super::open_prepared_persistent_with(&program, 0., |prepare| {
                let _candidate = prepare(48_000.)?;
                Err("injected stream failure".into())
            });
        assert_eq!(result.err().unwrap(), "injected stream failure");
        let skipped: Result<((), _), String> =
            super::open_prepared_persistent_with(&program, 0., |_| Ok(()));
        assert!(skipped.err().unwrap().contains("did not prepare"));
        let repeated = super::open_prepared_persistent_with(&program, 0., |prepare| {
            let _first = prepare(48_000.)?;
            prepare(48_000.)
        });
        assert!(repeated.err().unwrap().contains("more than once"));
    }

    #[test]
    fn midi_sliders_filter_channels_span_ranges_and_update_the_visible_faders() {
        let program = evaluate(
            r#"
            local c=control{name="expression",range={-2,4},default=1}
            local d=control{name="brightness",range={0,1},default=.5}
            midi(c,{cc=82,channel=3})
            midi(d,{cc=83})
        "#,
        )
        .unwrap();
        let runtime = persistent_runtime(&program).unwrap();
        let id = program.controls.id("expression").unwrap();
        assert!(!super::apply_midi_control(&program, runtime.controls(), 1, 82, 127).unwrap());
        assert_eq!(runtime.controls().value(id).unwrap(), 1.0);
        assert!(super::apply_midi_control(&program, runtime.controls(), 2, 82, 0).unwrap());
        assert_eq!(runtime.controls().value(id).unwrap(), -2.0);
        super::apply_midi_control(&program, runtime.controls(), 2, 82, 127).unwrap();
        assert_eq!(control_views(&program, Some(&runtime))[0].value, 4.0);
        assert!(super::apply_midi_control(&program, runtime.controls(), 15, 83, 0).unwrap());
        assert_eq!(control_views(&program, Some(&runtime))[1].value, 0.0);
        assert!(!super::apply_midi_control(&program, runtime.controls(), 2, 84, 0).unwrap());
    }

    /// The fader belongs to the player, not to a stream and not to a program.
    /// Setting it with the device closed has to be legal, because that is the
    /// state the app starts in and the state `stop` returns to.
    #[test]
    fn the_master_level_is_settable_before_and_after_any_audio_exists() {
        let mut player = Player::new();
        assert_eq!(player.volume_decibels, MasterGain::MAX_DECIBELS);
        assert!(player.output.is_none());

        player.set_volume(-18.0);
        assert_eq!(player.volume_decibels, -18.0);

        // Stop clears the transport, the arena and the stream. The level is
        // none of those: a fader that reset with the transport is one the user
        // has to find again after every incompatible edit.
        player.stop().expect("stopping without a stream succeeds");
        assert_eq!(player.volume_decibels, -18.0);

        player.set_volume(40.0);
        assert_eq!(player.volume_decibels, MasterGain::MAX_DECIBELS);
        player.set_volume(-400.0);
        assert_eq!(player.volume_decibels, MasterGain::MIN_DECIBELS);
    }

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
    fn mono_voice_is_accepted_in_stereo_routed_layout() {
        let program = evaluate(include_str!("../../test-fixtures/routed.eod")).unwrap();
        assert_eq!(playable_channels(&program).unwrap(), 2);
    }

    #[test]
    fn live_trigger_keeps_controls_and_degradation() {
        let program = evaluate(include_str!("../../test-fixtures/external-trigger.eod")).unwrap();
        let track = program
            .tracks
            .iter()
            .find(|track| track.external_trigger.is_some())
            .expect("fixture must contain its live onset-triggered voice");

        assert!(!track.external_controls.is_empty());
        assert!(
            !external_event_bindings(track, Frac::ZERO)
                .unwrap()
                .is_empty()
        );
        assert!(!track.external_degrades.is_empty());

        let outcomes: Vec<_> = (0..128)
            .map(|ordinal| external_event_passes(track, external_event_seed(0, track, ordinal)))
            .collect();
        assert!(outcomes.iter().any(|passes| *passes));
        assert!(outcomes.iter().any(|passes| !*passes));
        assert_ne!(
            external_event_seed(0, track, 7),
            external_event_seed(1, track, 7),
            "structurally distinct tracks must not share external event seeds"
        );
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
    fn program_binding_passes_nonzero_transport_into_persistent_lowering() {
        let program = evaluate(
            r#"
            tempo(120)
            local rack = patch {
              graph = function()
                local pulse = control_signal(
                  "0.2 0.8" >> segment(2),
                  bars(1))
                return pulse
              end,
            }
            run(rack)
            "#,
        )
        .unwrap();
        let mut runtime = persistent_runtime_at(&program, 1.5).unwrap();
        let lanes = runtime.layout().total_channels();
        let mut unit = runtime.take_processor();
        unit.set_sample_rate(48_000.0);
        let input = vec![0.0; lanes];
        let mut output = vec![0.0; lanes];
        unit.tick(&input, &mut output);
        assert!(output.iter().all(|sample| (*sample - 0.8).abs() < 1.0e-6));
    }

    #[test]
    fn engine_owned_signal_controls_do_not_appear_as_user_faders() {
        let program = evaluate(
            r#"
            local them = audio_input {
              name = "them",
              channels = 1,
              fallback = "silence",
            }
            local tracked = them >> envelope_follower(ms(6), ms(120))
            local field = control {
              name = "field",
              range = { 0, 1 },
              default = 0.2,
            }
            local rack = patch {
              graph = function() return sine(110) * (tracked + field) * 0.01 >> pan(0) end,
            }
            run(rack)
            "#,
        )
        .unwrap();
        let runtime = persistent_runtime(&program).unwrap();
        let views = control_views(&program, Some(&runtime));
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "field");
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
        assert!(!needs_hard_reset(false, false, true, false, false, None, 2));
        assert!(!needs_hard_reset(
            true,
            false,
            false,
            false,
            false,
            Some(2),
            2
        ));
        assert!(!needs_hard_reset(true, true, true, true, false, Some(2), 2));
        assert!(needs_hard_reset(true, true, true, false, false, Some(2), 2));
        assert!(needs_hard_reset(
            true,
            true,
            false,
            false,
            false,
            Some(2),
            2
        ));
        assert!(needs_hard_reset(
            true,
            false,
            true,
            false,
            false,
            Some(2),
            2
        ));
        assert!(needs_hard_reset(
            true,
            false,
            false,
            false,
            false,
            Some(1),
            2
        ));
        assert!(needs_hard_reset(
            true,
            false,
            false,
            false,
            true,
            Some(2),
            2
        ));
    }

    #[test]
    fn only_same_clock_same_layout_persistent_replacement_crossfades() {
        assert!(can_crossfade_replace(
            true,
            true,
            true,
            false,
            false,
            Some(2),
            2
        ));
        assert!(!can_crossfade_replace(
            true,
            true,
            true,
            false,
            true,
            Some(2),
            2
        ));
        assert!(!can_crossfade_replace(
            true,
            true,
            true,
            false,
            false,
            Some(1),
            2
        ));
        assert!(!can_crossfade_replace(
            true,
            true,
            false,
            false,
            false,
            Some(2),
            2
        ));
        assert!(!can_crossfade_replace(
            true,
            true,
            true,
            true,
            false,
            Some(2),
            2
        ));
    }
}
