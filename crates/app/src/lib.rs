#[cfg(test)]
mod corpus;
mod examples;
mod highlight;
mod player;
mod style;

use apteronotus_live::MasterGain;
use eframe::egui;
use player::{Command, ControlView, PlayerEvent, PlayerWorker};
use std::sync::mpsc::{Receiver, Sender};
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
        Box::new(|creation| {
            style::install(&creation.egui_ctx);
            Ok(Box::new(ApteronotusApp::new()))
        }),
    )
}

struct ApteronotusApp {
    source: String,
    /// The one unsaved buffer displaced by the most recent example choice.
    ///
    /// Example documents have no file-level undo yet. Keeping one complete
    /// buffer makes replacement recoverable without interrupting every choice
    /// with a confirmation dialog.
    replaced_source: Option<String>,
    command_tx: Sender<Command>,
    event_rx: Receiver<PlayerEvent>,
    worker: Option<PlayerWorker>,
    next_request: u64,
    latest_request: u64,
    status: Status,
    controls: Vec<ControlView>,
    /// Whether any Run has ever reached audio, which is what makes a later
    /// failure a *refused replacement* rather than simply a failure.
    sounding: bool,
    /// Taken from the editor's previous frame, only to light the gutter.
    cursor_line: usize,
    /// The master fader's position. Held here as well as in the player because
    /// the widget is the source of truth for where it is drawn, and the player
    /// owns whether the device has heard about it yet.
    volume_decibels: f64,
}

enum Status {
    Ready,
    Evaluating,
    Active {
        generation: u64,
        boundary: String,
        voices: usize,
        warning: Option<String>,
    },
    Stopped,
    Error(String),
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
            replaced_source: None,
            command_tx,
            event_rx,
            worker: Some(worker),
            next_request: 1,
            latest_request: 0,
            status: Status::Ready,
            controls: Vec::new(),
            sounding: false,
            cursor_line: 0,
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
                    boundary,
                    voices,
                    controls,
                    warning,
                } if request >= self.latest_request => {
                    self.latest_request = request;
                    self.status = Status::Active {
                        generation,
                        boundary,
                        voices,
                        warning,
                    };
                    self.controls = controls;
                    self.sounding = true;
                }
                PlayerEvent::Error { request, message } if request >= self.latest_request => {
                    self.latest_request = request;
                    self.status = Status::Error(message);
                }
                PlayerEvent::Stopped => {
                    self.status = Status::Stopped;
                    self.controls.clear();
                    self.sounding = false;
                }
                PlayerEvent::RuntimeError(message) => {
                    self.status = Status::Error(message);
                }
                PlayerEvent::Active { .. } | PlayerEvent::Error { .. } => {}
            }
        }
    }

    fn send(&mut self, command: Command) {
        if self.command_tx.send(command).is_err() {
            self.status = Status::Error("the audio worker stopped unexpectedly".into());
        } else if let Some(worker) = &mut self.worker {
            worker.pump();
        }
    }
}

impl eframe::App for ApteronotusApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive_events();
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(40));

        let busy = matches!(self.status, Status::Evaluating);
        let shortcuts = ctx.input_mut(|input| Shortcuts {
            run: input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter) && !busy,
            stop: input.consume_key(egui::Modifiers::COMMAND, egui::Key::Period) && self.sounding,
        });

        self.masthead(ui, busy, shortcuts);
        self.status_bar(ui);
        self.diagnostics(ui);
        self.controls_panel(ui);
        self.editor(ui);
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

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        style::readout(ui, "TEMPO", "score");
                        style::divider(ui);
                        style::readout(ui, "CLOCK", "mapped");
                    });
                });
            });
    }

    /// The shipped documents. Picking one replaces the editor outright and
    /// retains the displaced buffer in a single recoverable slot.
    fn example_picker(&mut self, ui: &mut egui::Ui) {
        enum Choice {
            Example(usize),
            Restore,
        }

        let opened = self.opened_example();
        let picked = style::menu_button(ui, "EXAMPLES", 260.0, |ui| {
            let mut picked = None;
            if self.replaced_source.is_some() {
                let restore = ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Restore previous buffer").color(style::DISCHARGE),
                        )
                        .min_size(egui::vec2(ui.available_width(), 0.0)),
                    )
                    .on_hover_text("Restore the unsaved buffer replaced by the last example");
                if restore.clicked() {
                    picked = Some(Choice::Restore);
                }
                ui.separator();
            }
            for (index, example) in examples::EXAMPLES.iter().enumerate() {
                let current = opened == Some(index);
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
                    picked = Some(Choice::Example(index));
                }
            }
            picked
        });
        match picked {
            Some(Some(Choice::Example(index)))
                if self.source != examples::EXAMPLES[index].source =>
            {
                self.replaced_source = Some(std::mem::replace(
                    &mut self.source,
                    examples::EXAMPLES[index].source.into(),
                ));
                self.cursor_line = 0;
            }
            Some(Some(Choice::Restore)) => {
                if let Some(previous) = self.replaced_source.take() {
                    self.source = previous;
                    self.cursor_line = 0;
                }
            }
            _ => {}
        }
    }

    /// Which shipped example the editor currently holds verbatim, if any.
    fn opened_example(&self) -> Option<usize> {
        examples::EXAMPLES
            .iter()
            .position(|example| example.source == self.source)
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
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
                        let lines = self.source.lines().count().max(1);
                        style::readout(ui, "LN", format!("{}", self.cursor_line + 1));
                        style::divider(ui);
                        style::readout(ui, "LINES", format!("{lines}"));
                        if let Status::Active {
                            boundary, voices, ..
                        } = &self.status
                        {
                            style::divider(ui);
                            style::readout(ui, "VOICES", format!("{voices}"));
                            style::divider(ui);
                            style::readout(ui, "CYCLE", boundary.clone());
                        }
                    });
                });
            });
    }

    fn diagnostics(&mut self, ui: &mut egui::Ui) {
        let (label, message, color) = match &self.status {
            Status::Error(message) => ("diagnostic", message.clone(), style::ALERT),
            Status::Active {
                warning: Some(message),
                ..
            } => ("input fallback", message.clone(), style::CAUTION),
            _ => return,
        };
        egui::Panel::bottom("diagnostics")
            .frame(style::chrome(Edge::Top))
            .resizable(true)
            .default_size(96.0)
            .max_size(280.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    style::tick(ui, color);
                    style::field_label(ui, label);
                });
                ui.add_space(style::UNIT * 0.5);
                style::scrollbars(ui);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(message)
                                    .text_style(egui::TextStyle::Monospace)
                                    .color(color),
                            )
                            .selectable(true),
                        );
                    });
            });
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

    fn editor(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(style::DEEP)
                    .inner_margin(egui::Margin::same(style::UNIT as i8)),
            )
            .show(ui, |ui| {
                style::well().show(ui, |ui| {
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
                    let lines = self.source.lines().count().max(1);
                    let cursor_line = self.cursor_line;

                    let mut layouter =
                        |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, _width: f32| {
                            let job = highlight::layout(buffer.as_str(), font.clone());
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
                                style::gutter(ui, lines, cursor_line, row_height, &font);

                                let output = egui::TextEdit::multiline(&mut self.source)
                                    .font(font.clone())
                                    .layouter(&mut layouter)
                                    .lock_focus(true)
                                    .frame(egui::Frame::NONE)
                                    .margin(egui::Margin::ZERO)
                                    .desired_rows(rows.max(lines))
                                    .desired_width(f32::INFINITY)
                                    .show(ui);

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
    use wasm_bindgen_futures::spawn_local;

    use super::{ApteronotusApp, style};

    #[derive(WebComponent)]
    #[web_component(name = "apteronotus-app")]
    pub struct ApteronotusComponent {
        element: Option<web_sys::HtmlElement>,
        mount: Option<EguiMount>,
    }

    impl ApteronotusComponent {
        fn new() -> Self {
            let _ = eframe::WebLogger::init(log::LevelFilter::Info);
            Self {
                element: None,
                mount: None,
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
