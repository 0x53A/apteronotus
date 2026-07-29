//! Program-scope writable scalar controls.
//!
//! A lexical binding owns an opaque [`ControlId`]. Graph templates carry that
//! handle as data; the fundsp-specific atomic value lives only in lowering.

use core::sync::atomic::{AtomicUsize, Ordering};

static NEXT_LAYOUT: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ControlId {
    layout: usize,
    index: usize,
}

impl ControlId {
    pub(crate) fn index(self) -> usize {
        self.index
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct ControlSpec {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: Option<String>,
}

impl ControlSpec {
    pub fn new(name: &str, min: f64, max: f64, default: f64) -> ControlSpec {
        ControlSpec {
            name: name.to_string(),
            min,
            max,
            default,
            unit: None,
        }
    }

    pub fn with_unit(mut self, unit: &str) -> ControlSpec {
        self.unit = Some(unit.to_string());
        self
    }

    pub fn clamp(&self, value: f64) -> f64 {
        value.clamp(self.min, self.max)
    }

    fn validate(&self) -> bool {
        !self.name.is_empty()
            && self.min.is_finite()
            && self.max.is_finite()
            && self.default.is_finite()
            && self.min <= self.max
            && (self.min..=self.max).contains(&self.default)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct ControlLayout {
    identity: usize,
    specs: Vec<ControlSpec>,
}

impl ControlLayout {
    pub fn new() -> ControlLayout {
        ControlLayout {
            identity: NEXT_LAYOUT.fetch_add(1, Ordering::Relaxed),
            specs: Vec::new(),
        }
    }

    pub fn add(&mut self, spec: ControlSpec) -> Result<ControlId, ControlError> {
        if !spec.validate() {
            return Err(ControlError::InvalidSpec);
        }
        if self.specs.iter().any(|existing| existing.name == spec.name) {
            return Err(ControlError::DuplicateName(spec.name));
        }
        let id = ControlId {
            layout: self.identity,
            index: self.specs.len(),
        };
        self.specs.push(spec);
        Ok(id)
    }

    pub fn spec(&self, id: ControlId) -> Option<&ControlSpec> {
        (id.layout == self.identity)
            .then(|| self.specs.get(id.index))
            .flatten()
    }

    pub fn specs(&self) -> &[ControlSpec] {
        &self.specs
    }

    /// Resolve a host-facing logical name inside this program arena.
    pub fn id(&self, name: &str) -> Option<ControlId> {
        self.specs
            .iter()
            .position(|spec| spec.name == name)
            .map(|index| ControlId {
                layout: self.identity,
                index,
            })
    }

    /// Resolve an equivalent handle from another compatible arena.
    ///
    /// Arena identities deliberately differ across evaluations. Reconciliation
    /// may reuse a live store only when the logical specification at the same
    /// position agrees; the returned handle always belongs to `target`.
    pub fn corresponding_id(&self, id: ControlId, target: &ControlLayout) -> Option<ControlId> {
        let spec = self.spec(id)?;
        let target_id = target.id(&spec.name)?;
        (target.spec(target_id) == Some(spec)).then_some(target_id)
    }
}

impl Default for ControlLayout {
    fn default() -> Self {
        ControlLayout::new()
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum ControlError {
    InvalidSpec,
    DuplicateName(String),
    UnknownControl(ControlId),
    NonFiniteValue,
}

impl core::fmt::Display for ControlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ControlError::InvalidSpec => {
                write!(f, "control has an invalid name, range, or default")
            }
            ControlError::DuplicateName(name) => {
                write!(f, "control name {name:?} is declared twice")
            }
            ControlError::UnknownControl(id) => write!(f, "unknown control {id:?}"),
            ControlError::NonFiniteValue => write!(f, "control value is not finite"),
        }
    }
}

impl core::error::Error for ControlError {}
