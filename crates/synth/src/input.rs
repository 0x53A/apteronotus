//! Program-scope optional audio inputs.
//!
//! Device identity belongs to the host, not to a staged graph. The owned
//! program carries a logical name, channel count and fallback policy; graph
//! sources carry only an arena-scoped opaque handle plus channel. When no host
//! binding exists, lowering uses the declared fallback without changing graph
//! topology.

use core::sync::atomic::{AtomicUsize, Ordering};

static NEXT_LAYOUT: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AudioInputId {
    layout: usize,
    index: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioInputFallback {
    Silence,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AudioInputSpec {
    pub name: String,
    pub channels: usize,
    pub fallback: AudioInputFallback,
}

impl AudioInputSpec {
    pub fn silence(name: &str, channels: usize) -> AudioInputSpec {
        AudioInputSpec {
            name: name.to_owned(),
            channels,
            fallback: AudioInputFallback::Silence,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AudioInputLayout {
    identity: usize,
    specs: Vec<AudioInputSpec>,
}

impl AudioInputLayout {
    pub fn new() -> AudioInputLayout {
        AudioInputLayout {
            identity: NEXT_LAYOUT.fetch_add(1, Ordering::Relaxed),
            specs: Vec::new(),
        }
    }

    pub fn add(&mut self, spec: AudioInputSpec) -> Result<AudioInputId, AudioInputError> {
        if spec.name.is_empty() || spec.channels == 0 {
            return Err(AudioInputError::InvalidSpec);
        }
        if self.specs.iter().any(|existing| existing.name == spec.name) {
            return Err(AudioInputError::DuplicateName(spec.name));
        }
        let id = AudioInputId {
            layout: self.identity,
            index: self.specs.len(),
        };
        self.specs.push(spec);
        Ok(id)
    }

    pub fn spec(&self, id: AudioInputId) -> Option<&AudioInputSpec> {
        (id.layout == self.identity)
            .then(|| self.specs.get(id.index))
            .flatten()
    }

    pub fn specs(&self) -> &[AudioInputSpec] {
        &self.specs
    }

    /// Flattened host channel count in declaration order.
    pub fn total_channels(&self) -> usize {
        self.specs.iter().map(|spec| spec.channels).sum()
    }

    /// Resolve one opaque logical input channel into the host's flattened
    /// declaration-order lane layout.
    pub fn channel_index(&self, id: AudioInputId, channel: usize) -> Option<usize> {
        let spec = self.spec(id)?;
        (channel < spec.channels).then(|| {
            self.specs[..id.index]
                .iter()
                .map(|prior| prior.channels)
                .sum::<usize>()
                + channel
        })
    }

    pub fn corresponding_id(
        &self,
        id: AudioInputId,
        target: &AudioInputLayout,
    ) -> Option<AudioInputId> {
        let spec = self.spec(id)?;
        let index = target
            .specs
            .iter()
            .position(|candidate| candidate == spec)?;
        Some(AudioInputId {
            layout: target.identity,
            index,
        })
    }
}

impl Default for AudioInputLayout {
    fn default() -> Self {
        AudioInputLayout::new()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AudioInputError {
    InvalidSpec,
    DuplicateName(String),
}

impl core::fmt::Display for AudioInputError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AudioInputError::InvalidSpec => {
                write!(f, "audio input must have a name and at least one channel")
            }
            AudioInputError::DuplicateName(name) => {
                write!(f, "audio input name {name:?} is declared twice")
            }
        }
    }
}

impl core::error::Error for AudioInputError {}
