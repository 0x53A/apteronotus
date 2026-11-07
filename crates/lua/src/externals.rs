//! Host-owned names for injected external sources. No filesystem resolver.
use apteronotus_synth::External;
use std::{collections::BTreeMap, sync::Arc};

/// Explicit host allowance for registered external declarations. Payload is
/// counted from each source's declared `data_entries` (8 bytes per entry); it is
/// not an aggregate prepared-DSP heap bound.
#[derive(Clone, Debug, Default)]
pub struct ExternalRegistry {
    entries: Arc<BTreeMap<String, External>>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}
impl ExternalRegistry {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            ..Self::default()
        }
    }
    /// Register a new name. Replacing an existing name requires a new registry.
    /// Failure leaves the registry unchanged. Shared payloads count per entry.
    pub fn register(&mut self, name: impl Into<String>, source: External) -> Result<(), String> {
        let name = name.into();
        if name.is_empty() || name.len() > 256 {
            return Err("external source name must contain 1..=256 UTF-8 bytes".into());
        }
        if self.entries.contains_key(&name) {
            return Err(format!("external source {name:?} already registered"));
        }
        let payload = source
            .source()
            .data_entries()
            .checked_mul(8)
            .ok_or("external payload byte count overflow")?;
        let bytes = self
            .bytes
            .checked_add(payload)
            .ok_or("external payload byte count overflow")?;
        if self.entries.len() >= self.max_entries || bytes > self.max_bytes {
            return Err("external registry payload/entry limit exceeded".into());
        }
        Arc::make_mut(&mut self.entries).insert(name, source);
        self.bytes = bytes;
        Ok(())
    }
    pub fn get(&self, name: &str) -> Option<&External> {
        self.entries.get(name)
    }
    pub fn payload_bytes(&self) -> usize {
        self.bytes
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
