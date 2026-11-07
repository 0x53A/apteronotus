//! A focused computer keyboard feeds the same bounded ingress as MIDI devices.
use crate::midi_input::MidiIngress;
use apteronotus_live::MidiMessage;
use eframe::egui::{self, Event, Key};

const KEYS: [Key; 13] = [
    Key::A,
    Key::W,
    Key::S,
    Key::E,
    Key::D,
    Key::F,
    Key::T,
    Key::G,
    Key::Y,
    Key::H,
    Key::U,
    Key::J,
    Key::K,
];

pub struct ComputerMidi {
    pub enabled: bool,
    pub octave: u8,
    pub velocity: u8,
    /// Displayed channels are 1–16; MIDI messages use 0–15.
    pub channel: u8,
    held: [Option<(u8, u8)>; 13],
    ingress: MidiIngress,
    focus_id: Option<egui::Id>,
}

impl ComputerMidi {
    pub fn new(ingress: MidiIngress) -> Self {
        Self {
            enabled: false,
            octave: 4,
            velocity: 100,
            channel: 1,
            held: [None; 13],
            ingress,
            focus_id: None,
        }
    }

    pub fn release(&mut self) {
        for note in &mut self.held {
            if let Some((channel, key)) = note.take() {
                self.ingress.send(MidiMessage::NoteOff { channel, key });
            }
        }
    }

    fn event(&mut self, event: &Event) -> bool {
        let Event::Key {
            key,
            pressed,
            repeat,
            modifiers,
            ..
        } = event
        else {
            return false;
        };
        // Use the labeled key on both platforms; eframe's web backend has no physical key.
        let key = *key;
        // Shortcut key-ups still release the note originally pressed without modifiers.
        if *pressed && (modifiers.command || modifiers.ctrl || modifiers.alt) {
            return false;
        }
        if let Some(index) = KEYS.iter().position(|candidate| *candidate == key) {
            if *pressed {
                if !*repeat && self.held[index].is_none() {
                    let channel = self.channel - 1;
                    let key = (self.octave + 1) * 12 + index as u8;
                    self.held[index] = Some((channel, key));
                    self.ingress.send(MidiMessage::NoteOn {
                        channel,
                        key,
                        velocity: self.velocity,
                    });
                }
            } else if let Some((channel, key)) = self.held[index].take() {
                self.ingress.send(MidiMessage::NoteOff { channel, key });
            }
            return true;
        }
        if matches!(key, Key::Z | Key::X) {
            if *pressed && !*repeat {
                self.release();
                self.octave = if key == Key::Z {
                    self.octave.saturating_sub(1)
                } else {
                    (self.octave + 1).min(8)
                };
            }
            return true;
        }
        false
    }

    fn has_focus(&self, ctx: &egui::Context) -> bool {
        self.focus_id
            .is_some_and(|id| ctx.memory(|memory| memory.has_focus(id)))
    }

    pub fn panel(&mut self, ui: &mut egui::Ui) {
        if !self.enabled {
            return;
        }
        egui::Panel::top("computer-midi").show(ui, |ui| {
            let previous = (self.octave, self.channel);
            ui.horizontal_wrapped(|ui| {
                let active = self.has_focus(ui.ctx());
                let response = ui.button(if active {
                    "Playing — Esc to leave"
                } else {
                    "Play computer keyboard"
                });
                self.focus_id = Some(response.id);
                if response.clicked() {
                    response.request_focus();
                }
                ui.label("Octave");
                ui.add(egui::DragValue::new(&mut self.octave).range(0..=8));
                ui.label("Velocity");
                ui.add(egui::DragValue::new(&mut self.velocity).range(1..=127));
                ui.label("Channel");
                ui.add(egui::DragValue::new(&mut self.channel).range(1..=16));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("C{}–C{}", self.octave, self.octave + 1));
                for (index, label) in [
                    "A", "W#", "S", "E#", "D", "F", "T#", "G", "Y#", "H", "U#", "J", "K",
                ]
                .iter()
                .enumerate()
                {
                    ui.colored_label(
                        if self.held[index].is_some() {
                            crate::style::DISCHARGE
                        } else {
                            crate::style::MUTED
                        },
                        *label,
                    );
                }
                ui.label("Z / X: octave down / up");
            });
            if previous != (self.octave, self.channel) {
                self.release();
            }
        });
    }

    /// Run after widgets have settled focus, before the next audio/UI poll.
    pub fn process(&mut self, ctx: &egui::Context) {
        let active = self.enabled && ctx.input(|input| input.focused) && self.has_focus(ctx);
        if !active {
            self.release();
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            if let Some(id) = self.focus_id {
                ctx.memory_mut(|memory| memory.surrender_focus(id));
            }
            self.release();
            return;
        }
        ctx.input_mut(|input| input.events.retain(|event| !self.event(event)));
    }
}

impl Drop for ComputerMidi {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_input::{MidiReceiver, queue};

    fn key(key: Key, pressed: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn messages(receiver: &MidiReceiver) -> Vec<MidiMessage> {
        let (overflow, packets) = receiver.drain();
        assert!(!overflow);
        packets.into_iter().map(|packet| packet.message).collect()
    }

    #[test]
    fn chords_repeat_and_release_preserve_the_original_note_and_channel() {
        let (ingress, receiver) = queue();
        let mut keyboard = ComputerMidi::new(ingress);
        keyboard.channel = 3;
        keyboard.velocity = 74;
        keyboard.event(&key(Key::A, true));
        keyboard.event(&key(Key::D, true));
        keyboard.event(&key(Key::K, true));
        keyboard.event(&key(Key::A, true));
        let mut repeat = key(Key::W, true);
        if let Event::Key { repeat, .. } = &mut repeat {
            *repeat = true;
        }
        keyboard.event(&repeat);
        assert_eq!(
            messages(&receiver),
            [60, 64, 72].map(|key| MidiMessage::NoteOn {
                channel: 2,
                key,
                velocity: 74
            })
        );
        keyboard.octave = 5;
        keyboard.channel = 7;
        let mut release = key(Key::A, false);
        if let Event::Key { modifiers, .. } = &mut release {
            *modifiers = egui::Modifiers::CTRL;
        }
        keyboard.event(&release);
        keyboard.release();
        assert_eq!(
            messages(&receiver),
            [60, 64, 72].map(|key| MidiMessage::NoteOff { channel: 2, key })
        );
        keyboard.release();
        assert!(messages(&receiver).is_empty());
    }

    #[test]
    fn octave_changes_release_chords_and_stay_inside_midi_range() {
        let (ingress, receiver) = queue();
        let mut keyboard = ComputerMidi::new(ingress);
        keyboard.event(&key(Key::A, true));
        keyboard.event(&key(Key::X, true));
        keyboard.event(&key(Key::A, false));
        keyboard.event(&key(Key::A, true));
        assert_eq!(
            messages(&receiver),
            vec![
                MidiMessage::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100
                },
                MidiMessage::NoteOff {
                    channel: 0,
                    key: 60
                },
                MidiMessage::NoteOn {
                    channel: 0,
                    key: 72,
                    velocity: 100
                },
            ]
        );
        for _ in 0..20 {
            keyboard.event(&key(Key::X, true));
        }
        keyboard.event(&key(Key::K, true));
        assert_eq!(keyboard.octave, 8);
        assert_eq!(
            messages(&receiver).last(),
            Some(&MidiMessage::NoteOn {
                channel: 0,
                key: 120,
                velocity: 100
            })
        );
        for _ in 0..20 {
            keyboard.event(&key(Key::Z, true));
        }
        assert_eq!(keyboard.octave, 0);
    }

    #[test]
    fn only_the_play_widget_gets_notes_and_focus_loss_releases_them() {
        let (ingress, receiver) = queue();
        let mut keyboard = ComputerMidi::new(ingress);
        keyboard.enabled = true;
        let ctx = egui::Context::default();
        let frame = |keyboard: &mut ComputerMidi, events, focused| {
            let _ = ctx.run_ui(
                egui::RawInput {
                    events,
                    focused,
                    ..Default::default()
                },
                |ui| {
                    keyboard.panel(ui);
                    keyboard.process(ui.ctx());
                },
            );
        };
        frame(
            &mut keyboard,
            vec![key(Key::A, true), key(Key::A, false)],
            true,
        );
        assert!(messages(&receiver).is_empty());
        let id = keyboard.focus_id.unwrap();
        ctx.memory_mut(|memory| memory.request_focus(id));
        frame(
            &mut keyboard,
            vec![key(Key::A, true), key(Key::D, true)],
            true,
        );
        assert_eq!(messages(&receiver).len(), 2);
        frame(&mut keyboard, vec![], true);
        assert!(
            keyboard.has_focus(&ctx),
            "play widget must keep focus across frames"
        );
        frame(&mut keyboard, vec![], false);
        assert_eq!(
            messages(&receiver),
            vec![
                MidiMessage::NoteOff {
                    channel: 0,
                    key: 60
                },
                MidiMessage::NoteOff {
                    channel: 0,
                    key: 64
                }
            ]
        );
        ctx.memory_mut(|memory| memory.surrender_focus(id));
        frame(&mut keyboard, vec![key(Key::A, true)], true);
        assert!(messages(&receiver).is_empty());
        frame(&mut keyboard, vec![key(Key::A, false)], true);
        ctx.memory_mut(|memory| memory.request_focus(id));
        frame(&mut keyboard, vec![key(Key::A, true)], true);
        assert_eq!(messages(&receiver).len(), 1);
        keyboard.enabled = false;
        frame(&mut keyboard, vec![], true);
        assert_eq!(
            messages(&receiver),
            vec![MidiMessage::NoteOff {
                channel: 0,
                key: 60
            }]
        );
    }
}
