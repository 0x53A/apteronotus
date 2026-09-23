#[cfg(target_arch = "wasm32")]
mod browser_files;
#[cfg(test)]
mod corpus;
#[cfg(not(target_arch = "wasm32"))]
mod document;
mod examples;
mod highlight;
mod player;
mod search;
mod style;
mod syntax;

use apteronotus_live::MasterGain;
use apteronotus_lua::Program;
use apteronotus_pattern::Frac;
use eframe::egui;
use player::{Command, ControlView, PlayerEvent, PlayerWorker};
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::{
    Arc,
    mpsc::{Receiver, Sender},
};
use std::time::Duration;
use style::Edge;

/// The master fader's travel, taken from the engine so the two cannot disagree.
///
/// The slider is linear in decibels rather than in amplitude, which is what
/// gives it a usable taper: a linear-amplitude fader does its whole audible job
/// in the bottom fifth of its travel and spends the rest of it moving between
/// levels that all sound about the same.
const MIN_VOLUME_DECIBELS: f64 = MasterGain::MIN_DECIBELS as f64;
const MAX_VOLUME_DECIBELS: f64 = MasterGain::MAX_DECIBELS as f64;
const FLASH_SECONDS: f64 = 0.060;
const MAX_ACTIVE_SOURCE_SPANS: usize = 256;
#[cfg(not(target_arch = "wasm32"))]
const HIGHLIGHT_LATENCY_SECONDS: f64 = 0.0;
#[cfg(target_arch = "wasm32")]
const HIGHLIGHT_LATENCY_SECONDS: f64 = 2_048.0 / 48_000.0;

/// The fader's readout. The bottom of the travel is silence, so it says so
/// rather than claiming −60 dB.
fn format_decibels(decibels: f64) -> String {
    if decibels <= MIN_VOLUME_DECIBELS {
        "−∞ dB".into()
    } else {
        format!("{decibels:.1} dB")
    }
}

/// Launch the native desktop application.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() -> eframe::Result {
    run_native_file(None)
}

/// Open an optional source document without evaluating it or opening audio.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_native_file(path: Option<&std::path::Path>) -> eframe::Result {
    let document = path
        .map(document::SavedDocument::open)
        .transpose()
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 700.0])
            .with_min_inner_size([620.0, 400.0])
            .with_app_id("apteronotus"),
        ..Default::default()
    };
    eframe::run_native(
        "Apteronotus",
        options,
        Box::new(move |creation| {
            style::install(&creation.egui_ctx);
            let mut app = ApteronotusApp::new();
            if let Some(document) = document {
                app.source = document.source().into();
                app.files.saved = Some(document);
            }
            Ok(Box::new(app))
        }),
    )
}

struct ApteronotusApp {
    source: String,
    syntax: syntax::SyntaxState,
    search: search::Search,
    select_source: Option<(Range<usize>, bool)>,
    #[cfg(not(target_arch = "wasm32"))]
    files: FileState,
    #[cfg(target_arch = "wasm32")]
    browser_files: browser_files::BrowserFiles,
    /// The last edited buffer displaced by browsing the library.
    ///
    /// Documents have no file-level undo yet. Keeping one complete
    /// buffer makes replacement recoverable without interrupting every choice
    /// with a confirmation dialog.
    replaced_source: Option<String>,
    command_tx: Sender<Command>,
    event_rx: Receiver<PlayerEvent>,
    worker: Option<PlayerWorker>,
    next_request: u64,
    latest_request: u64,
    /// Source text waiting for the matching activation response.
    pending: Option<(u64, String)>,
    status: Status,
    controls: Vec<ControlView>,
    /// Whether any Run has ever reached audio, which is what makes a later
    /// failure a *refused replacement* rather than simply a failure.
    sounding: bool,
    /// The revision that has reached the device clock, distinct from the last
    /// accepted command and from revisions already scheduled for the future.
    audible: Option<Sounding>,
    staged: VecDeque<Sounding>,
    /// Taken from the editor's previous frame, only to light the gutter.
    cursor_line: usize,
    jump_to: Option<usize>,
    /// The master fader's position. Held here as well as in the player because
    /// the widget is the source of truth for where it is drawn, and the player
    /// owns whether the device has heard about it yet.
    volume_decibels: f64,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct FileState {
    saved: Option<document::SavedDocument>,
    replaced: Option<document::SavedDocument>,
    path: String,
    open: bool,
    notice: Option<(bool, String)>,
}

enum Status {
    Ready,
    Evaluating,
    Active {
        generation: u64,
        voices: usize,
        warning: Option<String>,
    },
    Stopped,
    Error(String),
}

#[derive(Clone)]
struct Sounding {
    source: String,
    program: Arc<Program>,
    origin: web_time::Instant,
    effective_at: Frac,
}

impl Status {
    fn accent(&self) -> egui::Color32 {
        match self {
            Status::Ready | Status::Stopped => style::MUTED,
            Status::Evaluating => style::CAUTION,
            Status::Active { .. } => style::DISCHARGE,
            Status::Error(_) => style::ALERT,
        }
    }

    /// `sounding` distinguishes a refused edit over a live program from a
    /// first run that never reached audio; claiming a previous program when
    /// there is none would misdescribe the failure.
    fn headline(&self, sounding: bool) -> String {
        match self {
            Status::Ready => "idle — audio opens on the first run".into(),
            Status::Evaluating => "evaluating".into(),
            Status::Active { generation, .. } => format!("playing generation {generation}"),
            Status::Stopped => "stopped — the transport is back at cycle 0".into(),
            Status::Error(_) if sounding => {
                "refused — the previous program is still playing".into()
            }
            Status::Error(_) => "refused — nothing is playing".into(),
        }
    }
}

impl ApteronotusApp {
    fn new() -> Self {
        let (worker, command_tx, event_rx) = PlayerWorker::spawn();
        Self {
            source: examples::starter().into(),
            syntax: syntax::SyntaxState::default(),
            search: search::Search::default(),
            select_source: None,
            #[cfg(not(target_arch = "wasm32"))]
            files: FileState::default(),
            #[cfg(target_arch = "wasm32")]
            browser_files: browser_files::BrowserFiles::default(),
            replaced_source: None,
            command_tx,
            event_rx,
            worker: Some(worker),
            next_request: 1,
            latest_request: 0,
            pending: None,
            status: Status::Ready,
            controls: Vec::new(),
            sounding: false,
            audible: None,
            staged: VecDeque::new(),
            cursor_line: 0,
            jump_to: None,
            volume_decibels: MAX_VOLUME_DECIBELS,
        }
    }

    fn stop(&mut self) {
        // The GUI does not go quiet on its own: it waits for the player to
        // confirm the device is closed, the same way Run waits for activation.
        self.send(Command::Stop);
    }

    fn run(&mut self) {
        let request = self.next_request;
        self.next_request = self.next_request.saturating_add(1);
        self.latest_request = request;
        self.status = Status::Evaluating;
        self.pending = Some((request, self.source.clone()));
        if self
            .command_tx
            .send(Command::Run {
                request,
                source: self.source.clone(),
            })
            .is_err()
        {
            self.status = Status::Error("the audio worker stopped unexpectedly".into());
        } else if let Some(worker) = &mut self.worker {
            // On the web this keeps AudioContext creation inside the trusted
            // Run click/key event. Native playback's pump is intentionally a
            // no-op because its worker is already running independently.
            worker.pump();
        }
    }

    fn receive_events(&mut self) {
        if let Some(worker) = &mut self.worker {
            worker.pump();
        }
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                PlayerEvent::Active {
                    request,
                    generation,
                    program,
                    origin,
                    effective_at,
                    voices,
                    controls,
                    warning,
                } if request >= self.latest_request => {
                    let source = self
                        .pending
                        .take()
                        .filter(|(pending, _)| *pending == request)
                        .map(|(_, source)| source);
                    self.latest_request = request;
                    self.status = Status::Active {
                        generation,
                        voices,
                        warning,
                    };
                    self.controls = controls;
                    self.sounding = true;
                    if let Some(source) = source {
                        stage_sounding(
                            &mut self.audible,
                            &mut self.staged,
                            Sounding {
                                source,
                                program,
                                origin,
                                effective_at,
                            },
                        );
                    } else {
                        // An accepted revision without its exact source cannot
                        // be highlighted honestly.
                        clear_sounding(&mut self.audible, &mut self.staged);
                    }
                }
                PlayerEvent::Error { request, message } if request >= self.latest_request => {
                    if self
                        .pending
                        .as_ref()
                        .is_some_and(|(pending, _)| *pending == request)
                    {
                        self.pending = None;
                    }
                    self.latest_request = request;
                    self.status = Status::Error(message);
                }
                PlayerEvent::Stopped => {
                    self.status = Status::Stopped;
                    self.controls.clear();
                    self.sounding = false;
                    self.pending = None;
                    clear_sounding(&mut self.audible, &mut self.staged);
                }
                PlayerEvent::RuntimeError(message) => {
                    self.status = Status::Error(message);
                    clear_sounding(&mut self.audible, &mut self.staged);
                }
                PlayerEvent::Active { .. } | PlayerEvent::Error { .. } => {}
            }
        }
        self.promote_sounding();
    }

    fn send(&mut self, command: Command) {
        if self.command_tx.send(command).is_err() {
            self.status = Status::Error("the audio worker stopped unexpectedly".into());
        } else if let Some(worker) = &mut self.worker {
            worker.pump();
        }
    }

    fn promote_sounding(&mut self) {
        let Some(next) = self.staged.front() else {
            return;
        };
        let elapsed = next.origin.elapsed().as_secs_f64() - HIGHLIGHT_LATENCY_SECONDS;
        let Ok(now) = next.program.tempo.seconds_to_cycle(elapsed) else {
            return;
        };
        promote_sounding(&mut self.audible, &mut self.staged, now);
    }

    fn active_source_ranges(&self) -> Vec<Range<usize>> {
        let Some(audible) = &self.audible else {
            return Vec::new();
        };
        active_source_ranges(
            audible,
            &self.source,
            audible.origin.elapsed().as_secs_f64(),
        )
    }

    fn current_cycle(&self) -> Option<Frac> {
        let audible = self.audible.as_ref()?;
        audible_cycle(audible, audible.origin.elapsed().as_secs_f64())
    }
}

fn audible_cycle(audible: &Sounding, elapsed_seconds: f64) -> Option<Frac> {
    let now_seconds = elapsed_seconds - HIGHLIGHT_LATENCY_SECONDS;
    let now = audible.program.tempo.seconds_to_cycle(now_seconds).ok()?;
    (now >= audible.effective_at).then_some(now)
}

fn stage_sounding(
    audible: &mut Option<Sounding>,
    staged: &mut VecDeque<Sounding>,
    candidate: Sounding,
) {
    let current_origin = staged
        .back()
        .map(|sounding| sounding.origin)
        .or_else(|| audible.as_ref().map(|sounding| sounding.origin));
    if current_origin.is_some_and(|origin| origin != candidate.origin) {
        *audible = None;
        staged.clear();
    }
    if let Some(previous) = staged.back() {
        debug_assert!(previous.effective_at <= candidate.effective_at);
    }
    staged.push_back(candidate);
}

fn clear_sounding(audible: &mut Option<Sounding>, staged: &mut VecDeque<Sounding>) {
    *audible = None;
    staged.clear();
}

fn promote_sounding(audible: &mut Option<Sounding>, staged: &mut VecDeque<Sounding>, now: Frac) {
    while staged
        .front()
        .is_some_and(|sounding| sounding.effective_at <= now)
    {
        *audible = staged.pop_front();
    }
}

fn active_source_ranges(
    audible: &Sounding,
    editor_source: &str,
    elapsed_seconds: f64,
) -> Vec<Range<usize>> {
    if editor_source != audible.source {
        return Vec::new();
    }
    let now_seconds = elapsed_seconds - HIGHLIGHT_LATENCY_SECONDS;
    let Ok(now) = audible.program.tempo.seconds_to_cycle(now_seconds) else {
        return Vec::new();
    };
    if now < audible.effective_at {
        return Vec::new();
    }
    let Ok(back) = audible
        .program
        .tempo
        .seconds_to_cycle(now_seconds - FLASH_SECONDS)
    else {
        return Vec::new();
    };
    let begin = back.min(now).max(audible.effective_at);
    let window = apteronotus_pattern::Span::new(begin, now);
    let mut ranges = Vec::new();
    for track in &audible.program.tracks {
        // This cap protects layout work between tracks. A single query still
        // allocates its complete event vector; a bounded visiting query in the
        // pattern crate is the genuinely hard bound shared diagnostics need.
        if ranges.len() >= MAX_ACTIVE_SOURCE_SPANS {
            break;
        }
        for event in track.pattern.query(window) {
            if event.whole.is_none() {
                continue;
            }
            if let Some(span) = event.src {
                ranges.push(span.start as usize..span.end as usize);
                if ranges.len() >= MAX_ACTIVE_SOURCE_SPANS {
                    break;
                }
            }
        }
    }
    normalize_source_ranges(editor_source, ranges)
}

fn normalize_source_ranges(
    source: &str,
    ranges: impl IntoIterator<Item = Range<usize>>,
) -> Vec<Range<usize>> {
    let mut ranges = ranges
        .into_iter()
        .filter_map(|range| {
            let mut start = range.start.min(source.len());
            let mut end = range.end.min(source.len());
            while start < end && !source.is_char_boundary(start) {
                start += 1;
            }
            while end > start && !source.is_char_boundary(end) {
                end -= 1;
            }
            (start < end).then_some(start..end)
        })
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start < previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

#[cfg(test)]
mod sounding_tests {
    use super::{
        MAX_ACTIVE_SOURCE_SPANS, Sounding, active_source_ranges, audible_cycle, clear_sounding,
        normalize_source_ranges, promote_sounding, stage_sounding,
    };
    use apteronotus_lua::{Program, evaluate};
    use apteronotus_pattern::Frac;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::Duration;
    use web_time::Instant;

    fn state(source: &str, effective_at: Frac, origin: Instant) -> Sounding {
        Sounding {
            source: source.into(),
            program: Arc::new(Program::default()),
            origin,
            effective_at,
        }
    }

    #[test]
    fn queued_revisions_promote_at_their_own_boundaries_without_skipping() {
        let origin = Instant::now();
        let mut audible = Some(state("old", Frac::ZERO, origin));
        let mut staged = VecDeque::new();
        stage_sounding(&mut audible, &mut staged, state("a", Frac::ONE, origin));
        stage_sounding(&mut audible, &mut staged, state("b", Frac::int(2), origin));

        promote_sounding(&mut audible, &mut staged, Frac::new(1, 2));
        assert_eq!(audible.as_ref().unwrap().source, "old");
        assert_eq!(staged.len(), 2);
        promote_sounding(&mut audible, &mut staged, Frac::ONE);
        assert_eq!(audible.as_ref().unwrap().source, "a");
        assert_eq!(staged.len(), 1);
        promote_sounding(&mut audible, &mut staged, Frac::int(2));
        assert_eq!(audible.as_ref().unwrap().source, "b");
        assert!(staged.is_empty());
    }

    #[test]
    fn audible_cycle_advances_from_the_device_transport_origin() {
        let sounding = state("score", Frac::ONE, Instant::now());
        let before = sounding.program.tempo.cycle_to_seconds(Frac::new(1, 2));
        let after = sounding.program.tempo.cycle_to_seconds(Frac::new(3, 2));

        assert!(audible_cycle(&sounding, before).is_none());
        let cycle = audible_cycle(&sounding, after).unwrap();
        assert!((cycle.to_f64() - 1.5).abs() < 1.0e-9);
    }

    #[test]
    fn queueing_does_not_use_source_text_as_revision_identity() {
        let origin = Instant::now();
        let mut audible = None;
        let mut staged = VecDeque::new();
        stage_sounding(&mut audible, &mut staged, state("same", Frac::ONE, origin));
        stage_sounding(
            &mut audible,
            &mut staged,
            state("same", Frac::int(2), origin),
        );
        promote_sounding(&mut audible, &mut staged, Frac::int(2));
        assert_eq!(audible.as_ref().unwrap().effective_at, Frac::int(2));
        assert!(staged.is_empty());
    }

    #[test]
    fn a_new_transport_origin_discards_old_clock_state() {
        let origin = Instant::now();
        let replacement_origin = origin + Duration::from_secs(1);
        let mut audible = Some(state("old", Frac::ZERO, origin));
        let mut staged = VecDeque::from([state("queued", Frac::ONE, origin)]);
        stage_sounding(
            &mut audible,
            &mut staged,
            state("reset", Frac::ZERO, replacement_origin),
        );

        assert!(audible.is_none());
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].source, "reset");
    }

    #[test]
    fn runtime_uncertainty_clears_every_highlight_revision() {
        let origin = Instant::now();
        let mut audible = Some(state("old", Frac::ZERO, origin));
        let mut staged = VecDeque::from([state("new", Frac::ONE, origin)]);
        clear_sounding(&mut audible, &mut staged);
        assert!(audible.is_none());
        assert!(staged.is_empty());
    }

    #[test]
    fn active_ranges_require_exact_source_and_respect_the_activation_boundary() {
        let source = r#"
local v = voice { graph = function() return sine(220) * 0.01 end }
play(v, "c4 e4")
"#;
        let program = Arc::new(evaluate(source).unwrap());
        let now = program.tempo.cycle_to_seconds(Frac::new(1, 4));
        let sounding = Sounding {
            source: source.into(),
            program,
            origin: Instant::now(),
            effective_at: Frac::ZERO,
        };
        let ranges = active_source_ranges(&sounding, source, now);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&source[ranges[0].clone()], "c4");
        assert!(active_source_ranges(&sounding, "-- edited", now).is_empty());

        let source = source.replace("c4 e4", "~ c4");
        let program = Arc::new(evaluate(&source).unwrap());
        let just_after = program.tempo.cycle_to_seconds(Frac::new(101, 100));
        let sounding = Sounding {
            source: source.clone(),
            program,
            origin: Instant::now(),
            effective_at: Frac::ONE,
        };
        assert!(active_source_ranges(&sounding, &source, just_after).is_empty());
    }

    #[test]
    fn rock_guitars_have_bounded_cost_and_retire_after_their_release() {
        let program = evaluate(apteronotus_songs::RUST_AND_VOLTAGE).unwrap();
        for track in program.tracks.iter().take(2) {
            let graph = program.voice(track.voice).unwrap();
            assert!(
                graph.nodes.len() < 100,
                "guitar grew to {} nodes",
                graph.nodes.len()
            );
            let lifetime = graph.lifetime();
            assert_eq!(lifetime.absolute_horizon, 0.0);
            assert_eq!(lifetime.gate_tail, 0.14);
        }
    }

    #[test]
    fn rock_riff_tables_keep_sounding_tokens_through_the_arrangement() {
        let source = apteronotus_songs::RUST_AND_VOLTAGE;
        let program = Arc::new(evaluate(source).unwrap());
        let whole = apteronotus_pattern::Span::new(Frac::ZERO, Frac::int(64));
        for (index, track) in program.tracks.iter().enumerate() {
            let events = track.pattern.onsets(whole);
            assert!(!events.is_empty(), "track {index} is silent");
            for event in events {
                let span = event.src.expect("every rock note retains its literal span");
                let token = &source[span.start as usize..span.end as usize];
                assert!(
                    matches!(
                        token,
                        "x" | "e1"
                            | "f1"
                            | "g1"
                            | "a1"
                            | "bb1"
                            | "c2"
                            | "d2"
                            | "e2"
                            | "f2"
                            | "g2"
                            | "a2"
                            | "bb2"
                            | "c3"
                            | "d3"
                            | "d4"
                            | "e4"
                            | "g4"
                            | "a4"
                            | "b4"
                            | "d5"
                            | "e5"
                            | "g5"
                    ),
                    "track {index} points at {token:?} instead of a note"
                );
            }
        }
        let sounding = Sounding {
            source: source.into(),
            program,
            origin: Instant::now(),
            effective_at: Frac::ZERO,
        };
        // Check the table's original token, including when its riff returns.
        let start = source.find("e2 e2 ~ [e2 e2]").unwrap();
        for bar in [0, 4, 24, 48] {
            let now = sounding.program.tempo.cycle_to_seconds(Frac::int(bar))
                + super::HIGHLIGHT_LATENCY_SECONDS
                + 0.02;
            let ranges = active_source_ranges(&sounding, source, now);
            assert!(
                ranges.contains(&(start..start + 2)),
                "bar {bar}: {ranges:?}"
            );
        }
    }

    #[test]
    fn active_span_work_stops_between_tracks_at_the_declared_cap() {
        let mut source =
            String::from("local v = voice { graph = function() return sine(220) * 0.001 end }\n");
        for index in 0..MAX_ACTIVE_SOURCE_SPANS + 20 {
            source.push_str(&format!("play(v, \"x{index}\")\n"));
        }
        let program = Arc::new(evaluate(&source).unwrap());
        let now = program.tempo.cycle_to_seconds(Frac::new(1, 4));
        let sounding = Sounding {
            source: source.clone(),
            program,
            origin: Instant::now(),
            effective_at: Frac::ZERO,
        };
        assert_eq!(
            active_source_ranges(&sounding, &source, now).len(),
            MAX_ACTIVE_SOURCE_SPANS
        );
    }

    #[test]
    fn defensive_range_normalisation_clamps_snaps_and_coalesces() {
        let source = "aébc";
        assert_eq!(
            normalize_source_ranges(source, [1..2, 2..99, 0..1]),
            vec![0..1, 3..5]
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use super::*;

    #[test]
    fn library_recovery_restores_the_saved_file_identity_without_running() {
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Directory(
            std::env::temp_dir().join(format!("apteronotus-file-recovery-{}", std::process::id())),
        );
        std::fs::create_dir(&directory.0).unwrap();
        let path = directory.0.join("mine.eod");
        let original = "local my_composition = 1\n";
        let saved = document::SavedDocument::save_as(&path, original).unwrap();
        let mut app = ApteronotusApp::new();
        app.source = original.into();
        app.files.saved = Some(saved);
        app.replace_source(apteronotus_songs::NIGHTSHIFT_DUB);
        app.replace_source(apteronotus_songs::SEVEN_LANTERNS);
        assert!(app.files.saved.is_none());
        assert_eq!(app.files.replaced.as_ref().unwrap().path, path);
        app.restore_source();
        assert_eq!(app.source, original);
        assert_eq!(app.files.saved.as_ref().unwrap().path, path);
        app.source.push_str("-- edited after browsing\n");
        app.save_document();
        assert_eq!(std::fs::read_to_string(path).unwrap(), app.source);
        assert!(matches!(app.status, Status::Ready));
        assert_eq!(app.latest_request, 0);
        assert!(app.pending.is_none());
    }
}

impl eframe::App for ApteronotusApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive_events();
        #[cfg(target_arch = "wasm32")]
        if let Some(import) = self.browser_files.take_import() {
            match import.result {
                Ok((name, source)) if self.source == import.previous_source => {
                    self.replace_source(&source);
                    self.browser_files.name = name;
                    self.browser_files.notice = None;
                }
                Ok(_) => self.browser_files.notice = Some((true, "The editor changed while reading the file; import it again to replace this buffer.".into())),
                Err(error) => self.browser_files.notice = Some((true, format!("Cannot import: {error}"))),
            }
        }
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(40));

        let busy = matches!(self.status, Status::Evaluating);
        let shortcuts = ctx.input_mut(|input| Shortcuts {
            run: input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter) && !busy,
            stop: input.consume_key(egui::Modifiers::COMMAND, egui::Key::Period) && self.sounding,
        });
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
            #[cfg(not(target_arch = "wasm32"))]
            self.show_file_dialog();
            #[cfg(target_arch = "wasm32")]
            self.browser_import();
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            #[cfg(not(target_arch = "wasm32"))]
            self.save_document();
            #[cfg(target_arch = "wasm32")]
            self.browser_export();
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            self.search.open = true;
            self.search.focus = true;
        }

        self.masthead(ui, busy, shortcuts);
        #[cfg(not(target_arch = "wasm32"))]
        self.file_dialog(&ctx);
        self.syntax
            .update(&self.source, ctx.input(|input| input.time));
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F8)) {
            self.jump_to_diagnostic();
        }
        self.status_bar(ui);
        self.diagnostics(ui);
        self.controls_panel(ui);
        let active_source_ranges = self.active_source_ranges();
        self.editor(ui, &active_source_ranges);
    }
}

/// Which keyboard shortcuts fired this frame, already gated on whether the
/// action they name is available.
#[derive(Clone, Copy)]
struct Shortcuts {
    run: bool,
    stop: bool,
}

impl ApteronotusApp {
    fn masthead(&mut self, ui: &mut egui::Ui, busy: bool, shortcuts: Shortcuts) {
        egui::Panel::top("masthead")
            .frame(style::chrome(Edge::Bottom))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = style::UNIT * 1.5;
                    style::wordmark(ui);
                    style::divider(ui);

                    ui.spacing_mut().item_spacing.x = style::UNIT;
                    let run = style::primary_button(ui, !busy, "RUN")
                        .on_hover_text("Evaluate and activate at the next cycle boundary");
                    if (run.clicked() && !busy) || shortcuts.run {
                        self.run();
                    }

                    let stop = style::secondary_button(ui, self.sounding, "STOP").on_hover_text(
                        "Close the audio device and rewind the transport; the next Run \
                         starts from cycle 0",
                    );
                    if (stop.clicked() && self.sounding) || shortcuts.stop {
                        self.stop();
                    }

                    style::key_hint(ui, "CTRL+\u{21b5}");

                    ui.spacing_mut().item_spacing.x = style::UNIT * 1.5;
                    style::divider(ui);
                    self.example_picker(ui);
                    #[cfg(target_arch = "wasm32")]
                    self.browser_file_menu(ui);
                    #[cfg(not(target_arch = "wasm32"))]
                    if style::secondary_button(
                        ui,
                        true,
                        if self
                            .files
                            .saved
                            .as_ref()
                            .is_some_and(|saved| !saved.is_modified(&self.source))
                        {
                            "FILE"
                        } else {
                            "FILE *"
                        },
                    )
                    .clicked()
                    {
                        self.show_file_dialog();
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        style::readout(ui, "TEMPO", "score");
                        style::divider(ui);
                        style::readout(ui, "CLOCK", "mapped");
                    });
                });
            });
    }

    /// Picking a library document keeps the last edited buffer recoverable
    /// while browsing, and leaves evaluation to the explicit Run command.
    fn example_picker(&mut self, ui: &mut egui::Ui) {
        enum Choice {
            Document(&'static str),
            Restore,
        }

        let picked = style::menu_button(ui, "LIBRARY", 300.0, |ui| {
            let mut picked = None;
            if self.replaced_source.is_some() {
                let restore = ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Restore previous buffer").color(style::DISCHARGE),
                        )
                        .min_size(egui::vec2(ui.available_width(), 0.0)),
                    )
                    .on_hover_text("Restore the editor buffer saved before browsing the library");
                if restore.clicked() {
                    picked = Some(Choice::Restore);
                }
                ui.separator();
            }
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    for (index, example) in examples::documents().enumerate() {
                        if index == examples::EXAMPLES.len() {
                            ui.separator();
                            ui.label(egui::RichText::new("Songs").color(style::MUTED));
                        }
                        let current = self.source == example.source;
                        let color = if current {
                            style::DISCHARGE
                        } else {
                            style::TEXT
                        };
                        let entry = ui
                            .add(
                                egui::Button::new(egui::RichText::new(example.name).color(color))
                                    .right_text(
                                        egui::RichText::new(if current { "open" } else { "" })
                                            .text_style(egui::TextStyle::Small)
                                            .color(style::MUTED),
                                    )
                                    .min_size(egui::vec2(ui.available_width(), 0.0)),
                            )
                            .on_hover_text(example.summary);
                        if entry.clicked() {
                            picked = Some(Choice::Document(example.source));
                        }
                    }
                });
            picked
        });
        match picked {
            Some(Some(Choice::Document(source))) => {
                self.replace_source(source);
            }
            Some(Some(Choice::Restore)) => {
                self.restore_source();
            }
            _ => {}
        }
    }

    fn replace_source(&mut self, source: &str) {
        let preserves = self.replaced_source.is_none()
            || !examples::documents().any(|entry| entry.source == self.source);
        if examples::open_document(&mut self.source, &mut self.replaced_source, source) {
            self.cursor_line = 0;
            #[cfg(not(target_arch = "wasm32"))]
            {
                if preserves {
                    self.files.replaced = self.files.saved.take();
                }
                self.files.saved = None;
            }
            #[cfg(target_arch = "wasm32")]
            {
                if preserves {
                    self.browser_files.replaced_name = Some(self.browser_files.name.clone());
                }
                self.browser_files.name = apteronotus_songs::SONGS
                    .iter()
                    .find(|song| song.source == source)
                    .map_or_else(
                        || "composition.eod".into(),
                        |song| format!("{}.eod", song.name),
                    );
            }
        }
    }

    fn restore_source(&mut self) {
        if let Some(previous) = self.replaced_source.take() {
            self.source = previous;
            self.cursor_line = 0;
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.files.saved = self.files.replaced.take();
            }
            #[cfg(target_arch = "wasm32")]
            if let Some(name) = self.browser_files.replaced_name.take() {
                self.browser_files.name = name;
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn browser_import(&mut self) {
        self.browser_files.notice = self
            .browser_files
            .import(&self.source)
            .err()
            .map(|error| (true, error));
    }

    #[cfg(target_arch = "wasm32")]
    fn browser_export(&mut self) {
        self.browser_files.notice = self
            .browser_files
            .export(&self.source)
            .err()
            .map(|error| (true, error));
    }

    #[cfg(target_arch = "wasm32")]
    fn browser_file_menu(&mut self, ui: &mut egui::Ui) {
        let action = style::menu_button(ui, "FILE", style::FILE_WINDOW_WIDTH, |ui| {
            let mut action = None;
            if style::secondary_button(ui, true, "IMPORT SOURCE").clicked() {
                action = Some(false);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.browser_files.name)
                    .hint_text("composition.eod"),
            );
            if style::secondary_button(ui, true, "DOWNLOAD SOURCE").clicked() {
                action = Some(true);
            }
            if let Some((error, notice)) = &self.browser_files.notice {
                ui.label(egui::RichText::new(notice).color(if *error {
                    style::ALERT
                } else {
                    style::DISCHARGE
                }));
            }
            action
        });
        if let Some(Some(download)) = action {
            if download {
                self.browser_export();
            } else {
                self.browser_import();
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn show_file_dialog(&mut self) {
        if let Some(saved) = &self.files.saved {
            self.files.path = saved.path.to_string_lossy().into_owned();
        } else if self.files.path.is_empty() {
            self.files.path = "composition.eod".into();
        }
        self.files.open = true;
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_document(&mut self) {
        let Some(saved) = &mut self.files.saved else {
            self.show_file_dialog();
            return;
        };
        self.files.notice = Some(match saved.save(&self.source) {
            Ok(()) => (false, format!("Saved {}", saved.path.display())),
            Err(error) => (
                true,
                format!("Cannot save {}: {error}", saved.path.display()),
            ),
        });
        if self.files.notice.as_ref().is_some_and(|(error, _)| *error) {
            self.files.open = true;
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn file_dialog(&mut self, ctx: &egui::Context) {
        enum Action {
            Open,
            Save,
            SaveAs,
        }
        let mut open = self.files.open;
        let mut action = None;
        egui::Window::new("SOURCE FILE")
            .open(&mut open).default_width(style::FILE_WINDOW_WIDTH).resizable(true)
            .show(ctx, |ui| {
                if let Some(saved) = &self.files.saved {
                    ui.label(egui::RichText::new(saved.path.display().to_string()).color(style::TEXT));
                    style::field_label(ui, if saved.is_modified(&self.source) { "modified" } else { "saved" });
                } else {
                    style::field_label(ui, "unsaved document");
                }
                ui.add_space(style::UNIT);
                ui.add(egui::TextEdit::singleline(&mut self.files.path)
                    .desired_width(f32::INFINITY).hint_text("Path to a .eod source file"));
                ui.horizontal(|ui| {
                    if style::secondary_button(ui, true, "OPEN").clicked() { action = Some(Action::Open); }
                    if style::secondary_button(ui, self.files.saved.is_some(), "SAVE").clicked() { action = Some(Action::Save); }
                    if style::secondary_button(ui, true, "SAVE AS NEW FILE").clicked() { action = Some(Action::SaveAs); }
                });
                ui.label(egui::RichText::new("Open keeps the previous buffer recoverable. Save As requires a new filename.")
                    .text_style(egui::TextStyle::Small).color(style::MUTED));
                if let Some((error, notice)) = &self.files.notice {
                    ui.label(egui::RichText::new(notice).color(if *error { style::ALERT } else { style::DISCHARGE }));
                }
            });
        self.files.open = open;
        match action {
            Some(Action::Open) => {
                match document::SavedDocument::open(std::path::Path::new(&self.files.path)) {
                    Ok(document) => {
                        self.replace_source(document.source());
                        self.files.saved = Some(document);
                        self.files.notice = None;
                        self.files.open = false;
                    }
                    Err(error) => self.files.notice = Some((true, format!("Cannot open: {error}"))),
                }
            }
            Some(Action::Save) => self.save_document(),
            Some(Action::SaveAs) => match document::SavedDocument::save_as(
                std::path::Path::new(&self.files.path),
                &self.source,
            ) {
                Ok(document) => {
                    self.files.notice = Some((false, format!("Saved {}", document.path.display())));
                    self.files.saved = Some(document);
                }
                Err(error) => self.files.notice = Some((true, format!("Cannot save: {error}"))),
            },
            None => {}
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let current_cycle = self.current_cycle();
        egui::Panel::bottom("status")
            .frame(style::chrome(Edge::Top))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let playing = matches!(self.status, Status::Active { .. });
                    style::discharge_meter(ui, self.status.accent(), playing);
                    ui.label(
                        egui::RichText::new(self.status.headline(self.sounding))
                            .color(self.status.accent()),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let lines = self.source.bytes().filter(|byte| *byte == b'\n').count() + 1;
                        style::readout(ui, "LN", format!("{}", self.cursor_line + 1));
                        style::divider(ui);
                        style::readout(ui, "LINES", format!("{lines}"));
                        if let Status::Active { voices, .. } = &self.status {
                            style::divider(ui);
                            style::readout(ui, "VOICES", format!("{voices}"));
                            if let Some(cycle) = current_cycle {
                                style::divider(ui);
                                style::readout(ui, "CYCLE", format!("{:.2}", cycle.to_f64()));
                            }
                        }
                    });
                });
            });
    }

    fn diagnostics(&mut self, ui: &mut egui::Ui) {
        let mut messages = Vec::new();
        #[cfg(target_arch = "wasm32")]
        if let Some((error, notice)) = &self.browser_files.notice {
            messages.push((
                "source file",
                notice.clone(),
                if *error {
                    style::ALERT
                } else {
                    style::DISCHARGE
                },
            ));
        }
        if let Some(diagnostic) = &self.syntax.diagnostic {
            messages.push((
                "syntax — current buffer",
                diagnostic.to_string(),
                style::ALERT,
            ));
        }
        match &self.status {
            Status::Error(message) => messages.push(("last Run", message.clone(), style::ALERT)),
            Status::Active {
                warning: Some(message),
                ..
            } => messages.push(("input fallback", message.clone(), style::CAUTION)),
            _ => {}
        }
        if messages.is_empty() {
            return;
        }
        egui::Panel::bottom("diagnostics")
            .frame(style::chrome(Edge::Top))
            .resizable(true)
            .default_size(96.0)
            .max_size(280.0)
            .show(ui, |ui| {
                if self
                    .syntax
                    .diagnostic
                    .as_ref()
                    .is_some_and(|diagnostic| diagnostic.line_span.is_some())
                    && style::secondary_button(ui, true, "GO TO LINE · F8").clicked()
                {
                    self.jump_to_diagnostic();
                }
                style::scrollbars(ui);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (label, message, color) in messages {
                            ui.horizontal(|ui| {
                                style::tick(ui, color);
                                style::field_label(ui, label);
                            });
                            ui.add_space(style::UNIT * 0.5);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(message)
                                        .text_style(egui::TextStyle::Monospace)
                                        .color(color),
                                )
                                .selectable(true),
                            );
                            ui.add_space(style::UNIT);
                        }
                    });
            });
    }

    fn jump_to_diagnostic(&mut self) {
        self.jump_to = self
            .syntax
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.line_span.as_ref())
            .and_then(|span| self.source.get(..span.start))
            .map(|prefix| prefix.chars().count());
    }

    /// The right-hand rack: the master fader, then whatever faders the program
    /// declared.
    ///
    /// The panel is unconditional, because the master is. A volume control that
    /// appears only once a score happens to declare a control is not a volume
    /// control, and one that moves down the panel as controls come and go is a
    /// control you have to look for. It sits above the divider, always in the
    /// same place, and the heading says whose it is.
    fn controls_panel(&mut self, ui: &mut egui::Ui) {
        let mut updates = Vec::new();
        let mut volume_moved = false;
        egui::Panel::right("controls")
            .frame(style::chrome(Edge::Left))
            .resizable(true)
            .default_size(232.0)
            .min_size(180.0)
            .show(ui, |ui| {
                style::section_heading(ui, "OUTPUT");
                ui.add_space(style::UNIT);
                ui.horizontal(|ui| {
                    style::field_label(ui, "master");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format_decibels(self.volume_decibels))
                                .text_style(egui::TextStyle::Small)
                                .color(style::DISCHARGE),
                        );
                    });
                });
                let fader = style::fader(
                    ui,
                    &mut self.volume_decibels,
                    MIN_VOLUME_DECIBELS..=MAX_VOLUME_DECIBELS,
                )
                .on_hover_text(
                    "A gain stage between the engine and the device. It is not part of \
                     the score, so it works on any document and survives a reset",
                );
                if fader.changed() {
                    volume_moved = true;
                }

                if self.controls.is_empty() {
                    return;
                }
                ui.add_space(style::UNIT * 2.0);
                style::section_heading(ui, "CONTROLS");
                ui.add_space(style::UNIT);
                for control in &mut self.controls {
                    ui.horizontal(|ui| {
                        style::field_label(ui, &control.name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("{:.3}", control.value))
                                    .text_style(egui::TextStyle::Small)
                                    .color(style::DISCHARGE),
                            );
                        });
                    });
                    let slider = style::fader(ui, &mut control.value, control.min..=control.max);
                    if slider.changed() {
                        updates.push((control.name.clone(), control.value));
                    }
                    ui.add_space(style::UNIT * 1.5);
                }
            });
        if volume_moved {
            self.send(Command::SetVolume {
                decibels: self.volume_decibels as f32,
            });
        }
        for (name, value) in updates {
            self.send(Command::SetControl { name, value });
        }
    }

    fn search_bar(&mut self, ui: &mut egui::Ui) {
        if !self.search.open {
            if style::secondary_button(ui, true, "FIND · CTRL+F").clicked() {
                self.search.open = true;
                self.search.focus = true;
            }
            return;
        }
        let editor_id = egui::Id::new("apteronotus-source-editor");
        let cursor = egui::TextEdit::load_state(ui.ctx(), editor_id)
            .and_then(|state| state.cursor.char_range())
            .map_or(0, |range| range.sorted_cursors()[0].index.0);
        let mut backwards = false;
        let mut navigate = false;
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            let opening = self.search.focus;
            let search_id = egui::Id::new("apteronotus-search");
            if opening {
                self.search.anchor = cursor;
                let mut state = egui::TextEdit::load_state(ui.ctx(), search_id).unwrap_or_default();
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(self.search.query.chars().count()),
                    )));
                state.store(ui.ctx(), search_id);
            }
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.search.query)
                    .id(search_id)
                    .desired_width(200.0)
                    .hint_text("Find exact text"),
            );
            if self.search.focus {
                input.request_focus();
                self.search.focus = false;
            }
            let refreshed = self.search.refresh(&self.source, self.search.anchor);
            if opening {
                self.search.select_from(cursor);
            }
            if refreshed || opening {
                self.select_source = self.search.current().map(|range| (range, false));
            }
            let previous_key =
                ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::F3));
            let next_key =
                ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F3));
            // TextEdit releases focus on Enter; lost_focus still identifies
            // that submission, without stealing Enter from the source editor.
            if input.has_focus() || input.lost_focus() {
                backwards = ui
                    .input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter));
                navigate = backwards
                    || ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    });
                if navigate {
                    input.request_focus();
                }
            }
            let enabled = self.search.current().is_some();
            if style::secondary_button(ui, enabled, "PREV").clicked() || previous_key {
                navigate = true;
                backwards = true;
            }
            if style::secondary_button(ui, enabled, "NEXT").clicked() || next_key {
                navigate = true;
            }
            ui.label(egui::RichText::new(self.search.label()).color(style::MUTED));
            close = style::secondary_button(ui, true, "CLOSE").clicked()
                || ui
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        });
        if navigate {
            self.select_source = self.search.advance(backwards).map(|range| (range, false));
        }
        if close {
            self.search.open = false;
            self.select_source = self.search.current().map(|range| (range, true));
            if self.select_source.is_none() {
                ui.memory_mut(|memory| memory.request_focus(editor_id));
            }
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui, active: &[Range<usize>]) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(style::DEEP)
                    .inner_margin(egui::Margin::same(style::UNIT as i8)),
            )
            .show(ui, |ui| {
                self.search_bar(ui);
                style::well().show(ui, |ui| {
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
                    let lines = self.source.bytes().filter(|byte| *byte == b'\n').count() + 1;
                    let cursor_line = self.cursor_line;
                    let audible_source = self
                        .audible
                        .as_ref()
                        .map(|revision| revision.source.as_str());
                    let error_line = self
                        .syntax
                        .diagnostic
                        .as_ref()
                        .and_then(|diagnostic| diagnostic.line_span.as_ref())
                        .map(|span| {
                            self.source[..span.start]
                                .bytes()
                                .filter(|byte| *byte == b'\n')
                                .count()
                        });

                    let mut layouter =
                        |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, _width: f32| {
                            let mut job = highlight::layout_for_revision(
                                buffer.as_str(),
                                font.clone(),
                                audible_source,
                                active,
                            );
                            highlight::mark_search(&mut job, self.search.highlight().as_ref());
                            ui.fonts_mut(|fonts| fonts.layout_job(job))
                        };

                    style::scrollbars(ui);
                    egui::ScrollArea::both()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = style::UNIT;
                                // Floor, so a document that exactly fits does
                                // not summon a scrollbar for one stray pixel.
                                let rows = (ui.available_height() / row_height).floor() as usize;
                                style::gutter(
                                    ui,
                                    lines,
                                    cursor_line,
                                    error_line,
                                    row_height,
                                    &font,
                                );

                                let id = egui::Id::new("apteronotus-source-editor");
                                let search_selection = self.select_source.take();
                                let selection = self
                                    .jump_to
                                    .take()
                                    .map(|index| (index..index, true))
                                    .or(search_selection);
                                let jump = selection
                                    .as_ref()
                                    .map(|(range, _)| egui::text::CCursor::new(range.start));
                                if let Some((range, focus)) = selection {
                                    let mut state = egui::TextEdit::load_state(ui.ctx(), id)
                                        .unwrap_or_default();
                                    state.cursor.set_char_range(Some(
                                        egui::text::CCursorRange::two(
                                            egui::text::CCursor::new(range.start),
                                            egui::text::CCursor::new(range.end),
                                        ),
                                    ));
                                    state.store(ui.ctx(), id);
                                    if focus {
                                        ui.memory_mut(|memory| memory.request_focus(id));
                                    }
                                }
                                let output = egui::TextEdit::multiline(&mut self.source)
                                    .id(id)
                                    .font(font.clone())
                                    .layouter(&mut layouter)
                                    .lock_focus(true)
                                    .frame(egui::Frame::NONE)
                                    .margin(egui::Margin::ZERO)
                                    .desired_rows(rows.max(lines))
                                    .desired_width(f32::INFINITY)
                                    .show(ui);
                                if let Some(cursor) = jump {
                                    let rect = output
                                        .galley
                                        .pos_from_cursor(cursor)
                                        .translate(output.galley_pos.to_vec2());
                                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                                }
                                if output.response.changed() {
                                    self.syntax
                                        .observe(&self.source, ui.input(|input| input.time));
                                }

                                // The layouter never wraps, so a source line is
                                // exactly a gutter row: count the newlines the
                                // cursor has passed.
                                if let Some(cursor) = output.cursor_range {
                                    self.cursor_line = self
                                        .source
                                        .chars()
                                        .take(cursor.primary.index.0)
                                        .filter(|character| *character == '\n')
                                        .count();
                                }
                            });
                        });
                });
            });
    }
}

impl Drop for ApteronotusApp {
    fn drop(&mut self) {
        let _ = self.command_tx.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            worker.join();
        }
    }
}

/// Browser entry point: expose the same egui application as a reusable
/// `<apteronotus-app>` custom element. The element owns its canvas and runner,
/// so the package can be embedded in a larger page without global DOM IDs.
#[cfg(target_arch = "wasm32")]
mod component {
    use egui_web_component::EguiMount;
    use rust_web_component::WebComponent;
    use rust_web_component_macro::WebComponent;
    use wasm_bindgen::{JsCast, closure::Closure};
    use wasm_bindgen_futures::spawn_local;

    use super::{ApteronotusApp, style};

    #[derive(WebComponent)]
    #[web_component(name = "apteronotus-app")]
    pub struct ApteronotusComponent {
        element: Option<web_sys::HtmlElement>,
        mount: Option<EguiMount>,
        search_keys: Option<Closure<dyn FnMut(web_sys::KeyboardEvent)>>,
    }

    impl ApteronotusComponent {
        fn new() -> Self {
            let _ = eframe::WebLogger::init(log::LevelFilter::Info);
            Self {
                element: None,
                mount: None,
                search_keys: None,
            }
        }
    }

    impl WebComponent for ApteronotusComponent {
        fn attach(&mut self, element: &web_sys::HtmlElement) {
            self.element = Some(element.clone());
        }

        fn connected(&mut self) {
            let Some(element) = self.element.clone() else {
                return;
            };
            // eframe reserves Open/Save, but leaves browser Find shortcuts
            // alone. Reserve ours only for events inside this component;
            // don't open the browser's search box over the score's Find bar.
            if self.search_keys.is_none() {
                let listener = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
                    |event: web_sys::KeyboardEvent| {
                        let command = event.ctrl_key() || event.meta_key();
                        let find =
                            command && !event.shift_key() && event.key().eq_ignore_ascii_case("f");
                        let next = !command && event.key() == "F3";
                        if !event.alt_key() && !event.is_composing() && (find || next) {
                            event.prevent_default();
                        }
                    },
                );
                if let Err(error) = element.add_event_listener_with_callback_and_bool(
                    "keydown",
                    listener.as_ref().unchecked_ref(),
                    true,
                ) {
                    web_sys::console::error_1(&error);
                } else {
                    self.search_keys = Some(listener);
                }
            }
            let component_element = element.clone();
            spawn_local(async move {
                let result = EguiMount::connect(
                    &element,
                    eframe::WebOptions::default(),
                    Box::new(|creation| {
                        style::install(&creation.egui_ctx);
                        Ok(Box::new(ApteronotusApp::new()))
                    }),
                )
                .await;

                match result {
                    Ok(mount) => {
                        ApteronotusComponent::with_element(&component_element, |component| {
                            component.mount = Some(mount)
                        });
                    }
                    Err(error) => web_sys::console::error_1(&error),
                }
            });
        }

        fn disconnected(&mut self) {
            if let (Some(element), Some(listener)) = (&self.element, self.search_keys.take()) {
                let _ = element.remove_event_listener_with_callback_and_bool(
                    "keydown",
                    listener.as_ref().unchecked_ref(),
                    true,
                );
            }
            if let Some(mount) = self.mount.take() {
                mount.disconnect();
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    component::ApteronotusComponent::setup();
}
