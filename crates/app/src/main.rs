mod player;

use eframe::egui;
use player::{Command, PlayerEvent, PlayerWorker};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

const STARTER_SOURCE: &str = r#"local v = voice {
  graph = function(n)
    local env = adsr(ms(5), ms(80), 0.6, ms(180))
    return (sine(n.hz) * env * n.velocity * 0.12) >> pan(n.pan)
  end,
}

play(v, "c4 e4 g4")
"#;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 650.0])
            .with_min_inner_size([560.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Apteronotus",
        options,
        Box::new(|creation| Ok(Box::new(ApteronotusApp::new(creation)))),
    )
}

struct ApteronotusApp {
    source: String,
    command_tx: Sender<Command>,
    event_rx: Receiver<PlayerEvent>,
    worker: Option<PlayerWorker>,
    next_request: u64,
    latest_request: u64,
    status: Status,
}

enum Status {
    Ready,
    Evaluating,
    Active {
        generation: u64,
        boundary: String,
        voices: usize,
    },
    Error(String),
}

impl ApteronotusApp {
    fn new(creation: &eframe::CreationContext<'_>) -> Self {
        creation.egui_ctx.set_zoom_factor(1.1);
        let (worker, command_tx, event_rx) = PlayerWorker::spawn();
        Self {
            source: STARTER_SOURCE.into(),
            command_tx,
            event_rx,
            worker: Some(worker),
            next_request: 1,
            latest_request: 0,
            status: Status::Ready,
        }
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
        }
    }

    fn receive_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                PlayerEvent::Active {
                    request,
                    generation,
                    boundary,
                    voices,
                } if request >= self.latest_request => {
                    self.latest_request = request;
                    self.status = Status::Active {
                        generation,
                        boundary,
                        voices,
                    };
                }
                PlayerEvent::Error { request, message } if request >= self.latest_request => {
                    self.latest_request = request;
                    self.status = Status::Error(message);
                }
                PlayerEvent::RuntimeError(message) => {
                    self.status = Status::Error(message);
                }
                PlayerEvent::Active { .. } | PlayerEvent::Error { .. } => {}
            }
        }
    }
}

impl eframe::App for ApteronotusApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive_events();
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(40));

        let run_shortcut =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let run = ui
                    .add_enabled(
                        !matches!(self.status, Status::Evaluating),
                        egui::Button::new("▶ Run"),
                    )
                    .on_hover_text("Evaluate and activate (Ctrl/Cmd+Enter)");
                if run.clicked()
                    || (run_shortcut && !matches!(self.status, Status::Evaluating))
                {
                    self.run();
                }
                ui.separator();
                ui.label("120 BPM");
                ui.separator();
                match &self.status {
                    Status::Ready => {
                        ui.label("Ready — press Run to open audio");
                    }
                    Status::Evaluating => {
                        ui.spinner();
                        ui.label("Evaluating…");
                    }
                    Status::Active {
                        generation,
                        boundary,
                        voices,
                    } => {
                        ui.colored_label(
                            egui::Color32::from_rgb(96, 190, 120),
                            format!(
                                "Playing generation {generation} from cycle {boundary} · {voices} voices staged"
                            ),
                        );
                    }
                    Status::Error(_) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 115, 105),
                            "Run failed — playback was not replaced",
                        );
                    }
                }
            });
        });

        if let Status::Error(message) = &self.status {
            egui::Panel::bottom("diagnostics")
                .resizable(true)
                .default_size(90.0)
                .show(ui, |ui| {
                    ui.strong("Diagnostic");
                    ui.add_space(4.0);
                    ui.colored_label(egui::Color32::from_rgb(235, 140, 125), message);
                });
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_sized(
                ui.available_size(),
                egui::TextEdit::multiline(&mut self.source)
                    .font(egui::TextStyle::Monospace)
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY),
            );
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
