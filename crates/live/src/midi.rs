//! Device-neutral MIDI keys and owned, releasable polyphonic voices.
//! Parsing and allocation happen off the audio thread. Hardware and Web MIDI
//! share this boundary; a held key is never represented by a guessed duration.

use crate::{RoutedRuntime, ScheduleError};
use apteronotus_synth::{
    EventRouting, GraphTemplate, Lifetime, LiveGate, Note, instantiate_live_routed_with_controls,
    live_voice_lifetime,
};
use fundsp::prelude32::{EventId, Fade, Sequencer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiMessage {
    NoteOn { channel: u8, key: u8, velocity: u8 },
    NoteOff { channel: u8, key: u8 },
    AllNotesOff { channel: Option<u8> },
    ControlChange { channel: u8, cc: u8, value: u8 },
}

impl MidiMessage {
    /// Complete MIDI 1.0 messages supplied by the device backend. Other
    /// messages, including pitch bend, are ignored. CC interpretation belongs to the host.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes == [0xff] {
            return Some(Self::AllNotesOff { channel: None });
        }
        let [status, key, value] = *bytes else {
            return None;
        };
        if key > 127 || value > 127 {
            return None;
        }
        let channel = status & 15;
        match status & 0xf0 {
            0x80 => Some(Self::NoteOff { channel, key }),
            0x90 if value == 0 => Some(Self::NoteOff { channel, key }),
            0x90 => Some(Self::NoteOn {
                channel,
                key,
                velocity: value,
            }),
            0xb0 if key == 120 || key == 123 => Some(Self::AllNotesOff {
                channel: Some(channel),
            }),
            0xb0 if key < 120 => Some(Self::ControlChange {
                channel,
                cc: key,
                value,
            }),
            _ => None,
        }
    }
}

struct LiveNote {
    channel: u8,
    key: u8,
    start: f64,
    end: Option<f64>,
    gate: LiveGate,
    lifetime: Lifetime,
    event: EventId,
}

/// Includes releasing voices in the budget. At capacity a released voice is
/// faded out before accepting a new key; held voices are never silently stolen.
pub struct MidiVoices {
    notes: Vec<LiveNote>,
    capacity: usize,
}

impl Default for MidiVoices {
    fn default() -> Self {
        Self::new(256)
    }
}

impl MidiVoices {
    pub fn new(capacity: usize) -> Self {
        Self {
            notes: Vec::with_capacity(capacity),
            capacity,
        }
    }

    pub fn prune(&mut self, now: f64) {
        self.notes
            .retain(|note| note.end.is_none_or(|end| end > now));
    }

    pub fn held_count(&self) -> usize {
        self.notes.iter().filter(|note| note.end.is_none()).count()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn note_on(
        &mut self,
        channel: u8,
        key: u8,
        velocity: u8,
        at: f64,
        seed: u64,
        template: &GraphTemplate,
        routing: &EventRouting,
        sequencer: &mut Sequencer,
        runtime: RoutedRuntime<'_>,
    ) -> Result<(), ScheduleError> {
        if !at.is_finite() || at < 0.0 {
            return Err(ScheduleError::InvalidExternalTime(at));
        }
        if channel > 15 || key > 127 || velocity == 0 || velocity > 127 {
            return Err(ScheduleError::InvalidFrequency(f64::NAN));
        }
        let retiring = if self.notes.len() >= self.capacity {
            // Fast retriggers may overlap many release tails. Retire the oldest
            // released voice with a short fade while preserving held keys.
            Some(
                self.notes
                    .iter()
                    .enumerate()
                    .filter(|(_, note)| note.end.is_some())
                    .min_by(|(_, a), (_, b)| a.start.total_cmp(&b.start))
                    .map(|(index, _)| index)
                    .ok_or(ScheduleError::MidiVoiceLimit(self.capacity))?,
            )
        } else {
            None
        };
        let hz = 440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0);
        // The scalar is only needed by duration-independent initializers and
        // lifetime metadata. Live lowering rejects every duration consumer.
        let note = Note::new(hz)
            .velocity(f64::from(velocity) / 127.0)
            .seed(seed);
        let lifetime = live_voice_lifetime(template, &note).map_err(ScheduleError::Lower)?;
        let gate = LiveGate::new();
        let unit = instantiate_live_routed_with_controls(
            template,
            &note,
            runtime.layout,
            routing,
            runtime.controls,
            &gate,
        )
        .map_err(ScheduleError::Lower)?;
        // Prove the new voice before changing any existing release tail.
        if let Some(index) = retiring {
            let previous = self.notes.swap_remove(index);
            let end = previous.end.expect("released voice").min(at + 0.005);
            sequencer.edit(
                previous.event,
                end,
                (end - previous.start).clamp(0.0, 0.005),
            );
        }
        let event = sequencer.push(at, f64::INFINITY, Fade::Smooth, 0.0, 0.0, unit);
        self.notes.push(LiveNote {
            channel,
            key,
            start: at,
            end: None,
            gate,
            lifetime,
            event,
        });
        Ok(())
    }

    pub fn note_off(&mut self, channel: u8, key: u8, at: f64, sequencer: &mut Sequencer) {
        self.release(Some(channel), Some(key), at, sequencer);
    }

    pub fn all_notes_off(&mut self, channel: Option<u8>, at: f64, sequencer: &mut Sequencer) {
        self.release(channel, None, at, sequencer);
    }

    fn release(
        &mut self,
        channel: Option<u8>,
        key: Option<u8>,
        at: f64,
        sequencer: &mut Sequencer,
    ) {
        if !at.is_finite() || at < 0.0 {
            return;
        }
        for note in &mut self.notes {
            if note.end.is_some()
                || channel.is_some_and(|c| c != note.channel)
                || key.is_some_and(|k| k != note.key)
            {
                continue;
            }
            let duration = (at - note.start).max(0.0);
            let end = note.start
                + note
                    .lifetime
                    .end_after_onset(duration)
                    .max(duration + 0.005);
            note.gate
                .release_after(duration)
                .expect("validated note-off coordinate");
            sequencer.edit(note.event, end, 0.0);
            note.end = Some(end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_parser_handles_zero_velocity_channels_panic_and_cc() {
        assert_eq!(
            MidiMessage::parse(&[0x92, 60, 100]),
            Some(MidiMessage::NoteOn {
                channel: 2,
                key: 60,
                velocity: 100
            })
        );
        for message in [[0x92, 60, 0], [0x82, 60, 12]] {
            assert_eq!(
                MidiMessage::parse(&message),
                Some(MidiMessage::NoteOff {
                    channel: 2,
                    key: 60
                })
            );
        }
        assert_eq!(
            MidiMessage::parse(&[0xb2, 123, 0]),
            Some(MidiMessage::AllNotesOff { channel: Some(2) })
        );
        assert_eq!(
            MidiMessage::parse(&[0xb2, 11, 127]),
            Some(MidiMessage::ControlChange {
                channel: 2,
                cc: 11,
                value: 127
            })
        );
        for bytes in [
            &[0x90, 60][..],
            &[0x90, 200, 1],
            &[0xe0, 0, 64],
            &[0x90, 60, 1, 0],
        ] {
            assert_eq!(MidiMessage::parse(bytes), None);
        }
    }
}
