//! Keyboard approximation for one retained bowed string. Last held key wins;
//! releasing it restores the previous held key without a new bow attack.
use crate::MidiMessage;
use apteronotus_synth::bowed_gesture::BowAction;
use apteronotus_synth::bowed_host::BowedHandle;

#[derive(Clone, Copy)]
struct Held {
    channel: u8,
    key: u8,
    velocity: u8,
}
#[derive(Default)]
pub struct BowedKeyboard {
    held: Vec<Held>,
}
impl BowedKeyboard {
    pub fn held_count(&self) -> usize {
        self.held.len()
    }
    pub fn message(
        &mut self,
        message: MidiMessage,
        handle: &BowedHandle,
        at: f64,
        channel_filter: Option<u8>,
        min_key: u8,
        max_key: u8,
    ) -> Result<(), &'static str> {
        let channel = match message {
            MidiMessage::NoteOn { channel, .. }
            | MidiMessage::NoteOff { channel, .. }
            | MidiMessage::ControlChange { channel, .. } => Some(channel),
            MidiMessage::AllNotesOff { channel } => channel,
        };
        if channel.is_some_and(|c| channel_filter.is_some_and(|f| f != c)) {
            return Ok(());
        }
        let mut next = self.held.clone();
        match message {
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
            } => {
                if !(min_key..=max_key).contains(&key) {
                    return Err("key outside this violin string's declared range");
                }
                next.retain(|n| n.channel != channel || n.key != key);
                next.push(Held {
                    channel,
                    key,
                    velocity,
                });
            }
            MidiMessage::NoteOff { channel, key } => {
                next.retain(|n| n.channel != channel || n.key != key)
            }
            MidiMessage::AllNotesOff { channel } => {
                next.retain(|n| channel.is_some_and(|c| c != n.channel))
            }
            MidiMessage::ControlChange { .. } => return Ok(()),
        }
        if next.is_empty() && matches!(message, MidiMessage::AllNotesOff { .. }) {
            handle.panic_detach();
            self.held = next;
            return Ok(());
        }
        let current = self.held.last();
        let target = next.last();
        if current.map(|n| (n.channel, n.key, n.velocity))
            == target.map(|n| (n.channel, n.key, n.velocity))
        {
            self.held = next;
            return Ok(());
        }
        if let Some(n) = target {
            let finger = if n.key == min_key {
                BowAction::ReleaseFinger { string: 0, over: 0 }
            } else {
                BowAction::Stop {
                    string: 0,
                    hz: 440.0 * 2.0_f64.powf((f64::from(n.key) - 69.0) / 12.0),
                    over: 0,
                }
            };
            handle.send(
                Some(at),
                &[
                    finger,
                    BowAction::Bow {
                        string: Some(0),
                        speed: None,
                        force: Some(f64::from(n.velocity) / 127.0),
                        position: None,
                        over: 0,
                    },
                ],
            )?;
        } else {
            // Reserved detach cannot be lost when the ordinary queue is full.
            if matches!(message, MidiMessage::AllNotesOff { .. }) {
                handle.panic_detach();
            } else {
                handle
                    .send(Some(at), &[BowAction::Lift { over: 0 }])
                    .inspect_err(|_| {
                        handle.panic_detach();
                    })?;
            }
        }
        self.held = next;
        Ok(())
    }
}
