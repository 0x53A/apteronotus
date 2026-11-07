//! Native and browser device ownership, feeding one bounded note queue.
use apteronotus_live::MidiMessage;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use web_time::Instant;

#[derive(Clone)]
pub struct MidiPort {
    pub id: String,
    pub name: String,
}
pub struct MidiPacket {
    pub message: MidiMessage,
    pub observed: Instant,
}

#[derive(Clone)]
pub struct MidiIngress {
    sender: SyncSender<MidiPacket>,
    overflow: Arc<AtomicBool>,
}
impl MidiIngress {
    pub fn send(&self, message: MidiMessage) {
        if self
            .sender
            .try_send(MidiPacket {
                message,
                observed: Instant::now(),
            })
            .is_err()
        {
            self.overflow.store(true, Ordering::Release);
        }
    }
}

pub struct MidiReceiver {
    receiver: Receiver<MidiPacket>,
    overflow: Arc<AtomicBool>,
}
impl MidiReceiver {
    /// An overflow invalidates key state: discard the incomplete burst and
    /// release every held voice before accepting subsequent messages.
    pub fn drain(&self) -> (bool, Vec<MidiPacket>) {
        let overflow = self.overflow.swap(false, Ordering::AcqRel);
        let packets = self.receiver.try_iter().take(512).collect();
        (overflow, packets)
    }
}
pub fn queue() -> (MidiIngress, MidiReceiver) {
    let (sender, receiver) = mpsc::sync_channel(512);
    let overflow = Arc::new(AtomicBool::new(false));
    (
        MidiIngress {
            sender,
            overflow: overflow.clone(),
        },
        MidiReceiver { receiver, overflow },
    )
}

#[cfg(target_arch = "wasm32")]
pub use crate::midi_web::MidiWeb as MidiInputHost;

#[cfg(not(target_arch = "wasm32"))]
pub struct MidiInputHost {
    ingress: MidiIngress,
    ports: Vec<(MidiPort, midir::MidiInputPort)>,
    connection: Option<midir::MidiInputConnection<()>>,
    selected: Option<String>,
    notice: Option<String>,
    refreshed: Instant,
}

#[cfg(not(target_arch = "wasm32"))]
impl MidiInputHost {
    pub fn new(ingress: MidiIngress) -> Self {
        // Device enumeration is opt-in, including on systems without MIDI.
        Self {
            ingress,
            ports: Vec::new(),
            connection: None,
            selected: None,
            notice: Some("Enable MIDI to find connected keyboards.".into()),
            refreshed: Instant::now(),
        }
    }
    pub fn request_access(&mut self) {
        self.refresh();
    }
    pub fn ports(&self) -> Vec<MidiPort> {
        self.ports.iter().map(|(port, _)| port.clone()).collect()
    }
    pub fn selected(&self) -> Option<String> {
        self.selected.clone()
    }
    pub fn notice(&self) -> Option<String> {
        self.notice.clone()
    }
    pub fn connect(&mut self, id: Option<&str>) -> Result<(), String> {
        if id == self.selected.as_deref() {
            return Ok(());
        }
        let candidate = if let Some(id) = id {
            let input = midir::MidiInput::new("Apteronotus keys").map_err(|e| e.to_string())?;
            let port = input
                .ports()
                .into_iter()
                .find(|port| port.id() == id)
                .ok_or_else(|| "the selected MIDI input is no longer connected".to_string())?;
            let ingress = self.ingress.clone();
            Some(
                input
                    .connect(
                        &port,
                        "Apteronotus keys",
                        move |_, bytes, _| {
                            if let Some(message) = MidiMessage::parse(bytes) {
                                ingress.send(message);
                            }
                        },
                        (),
                    )
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        self.connection = None;
        self.ingress
            .send(MidiMessage::AllNotesOff { channel: None });
        self.connection = candidate;
        self.selected = id.map(str::to_owned);
        self.notice = None;
        Ok(())
    }
    pub fn poll(&mut self) {
        if self.notice.as_deref() == Some("Enable MIDI to find connected keyboards.") {
            return;
        }
        if self.refreshed.elapsed().as_secs_f64() >= 1.0 {
            self.refresh();
        }
    }
    fn refresh(&mut self) {
        self.refreshed = Instant::now();
        match midir::MidiInput::new("Apteronotus MIDI inputs") {
            Ok(input) => {
                self.ports = input
                    .ports()
                    .into_iter()
                    .filter_map(|port| {
                        let name = input.port_name(&port).ok()?;
                        Some((
                            MidiPort {
                                id: port.id(),
                                name,
                            },
                            port,
                        ))
                    })
                    .collect();
                if self
                    .selected
                    .as_ref()
                    .is_some_and(|id| !self.ports.iter().any(|(port, _)| &port.id == id))
                {
                    let _ = self.connect(None);
                    self.notice = Some("MIDI keyboard disconnected; held keys released.".into());
                } else {
                    self.notice = self
                        .ports
                        .is_empty()
                        .then(|| "No MIDI inputs found. Connect a keyboard and refresh.".into());
                }
            }
            Err(error) => {
                let _ = self.connect(None);
                self.ports.clear();
                self.notice = Some(format!("MIDI unavailable: {error}"));
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for MidiInputHost {
    fn drop(&mut self) {
        self.connection = None;
        self.ingress
            .send(MidiMessage::AllNotesOff { channel: None });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lost_note_off_invalidates_entire_bounded_burst() {
        let (input, receiver) = queue();
        for _ in 0..512 {
            input.send(MidiMessage::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100,
            });
        }
        input.send(MidiMessage::NoteOff {
            channel: 0,
            key: 60,
        });
        let (overflow, messages) = receiver.drain();
        assert!(overflow);
        assert_eq!(messages.len(), 512);
        assert!(!receiver.drain().0);
    }
}
