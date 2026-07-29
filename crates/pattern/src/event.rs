//! What a query returns.

use crate::curve::Curve;
use crate::span::Span;
use crate::{Frac, rand};
use core::fmt;

pub const PRIMARY_FIELD: &str = "value";

/// A non-map event or control-field value.
#[derive(Clone, PartialEq, Debug)]
pub enum ControlValue {
    Number(f64),
    Text(String),
    Bool(bool),
    Curve(Curve),
}

impl ControlValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ControlValue::Number(x) => Some(*x),
            ControlValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            ControlValue::Text(_) | ControlValue::Curve(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ControlValue::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            ControlValue::Bool(b) => *b,
            ControlValue::Number(x) => *x != 0.0,
            ControlValue::Text(s) => !s.is_empty() && s != "~",
            ControlValue::Curve(_) => true,
        }
    }
}

impl fmt::Display for ControlValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControlValue::Number(x) => write!(f, "{x}"),
            ControlValue::Text(s) => write!(f, "{s}"),
            ControlValue::Bool(b) => write!(f, "{b}"),
            ControlValue::Curve(curve) => {
                write!(f, "curve({:?}, {} terms)", curve.clock, curve.terms.len())
            }
        }
    }
}

/// A byte range in the source text that produced a value.
///
/// Carried through every combinator so the editor can highlight what is
/// sounding right now. This is the reason the pattern is an AST rather than a
/// tree of closures: a closure cannot say where it came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SrcSpan {
    pub start: u32,
    pub end: u32,
}

impl SrcSpan {
    pub fn new(start: usize, end: usize) -> SrcSpan {
        SrcSpan {
            start: start as u32,
            end: end as u32,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct ControlField {
    name: String,
    value: ControlValue,
    src: Option<SrcSpan>,
}

impl ControlField {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &ControlValue {
        &self.value
    }

    pub fn src(&self) -> Option<SrcSpan> {
        self.src
    }
}

/// Canonically sorted, unique named event fields.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ControlMap {
    fields: Vec<ControlField>,
}

impl ControlMap {
    pub fn new() -> ControlMap {
        ControlMap::default()
    }

    pub fn primary(value: ControlValue, src: Option<SrcSpan>) -> ControlMap {
        ControlMap {
            fields: vec![ControlField {
                name: PRIMARY_FIELD.into(),
                value,
                src,
            }],
        }
    }

    pub fn named(
        name: impl Into<String>,
        value: ControlValue,
        src: Option<SrcSpan>,
    ) -> Result<ControlMap, ControlMapError> {
        let mut map = ControlMap::new();
        map.insert(name, value, src)?;
        Ok(map)
    }

    pub fn insert(
        &mut self,
        name: impl Into<String>,
        value: ControlValue,
        src: Option<SrcSpan>,
    ) -> Result<(), ControlMapError> {
        let name = name.into();
        validate_field_name(&name, false)?;
        self.insert_internal(name, value, src);
        Ok(())
    }

    fn insert_internal(&mut self, name: String, value: ControlValue, src: Option<SrcSpan>) {
        match self
            .fields
            .binary_search_by(|field| field.name.as_str().cmp(&name))
        {
            Ok(index) => {
                self.fields[index] = ControlField { name, value, src };
            }
            Err(index) => self.fields.insert(index, ControlField { name, value, src }),
        }
    }

    pub fn get(&self, name: &str) -> Option<&ControlValue> {
        self.field(name).map(ControlField::value)
    }

    pub fn field(&self, name: &str) -> Option<&ControlField> {
        self.fields
            .binary_search_by(|field| field.name.as_str().cmp(name))
            .ok()
            .map(|index| &self.fields[index])
    }

    pub fn fields(&self) -> &[ControlField] {
        &self.fields
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Right-hand fields replace matching left-hand fields, including spans.
    pub fn merged(&self, right: &ControlMap) -> ControlMap {
        let mut merged = self.clone();
        for field in &right.fields {
            merged.insert_internal(field.name.clone(), field.value.clone(), field.src);
        }
        merged
    }

    pub fn offset_primary(&self, amount: f64) -> Option<ControlMap> {
        let value = self.get(PRIMARY_FIELD)?.as_f64()?;
        let mut result = self.clone();
        let src = self.field(PRIMARY_FIELD).and_then(ControlField::src);
        result.insert_internal(
            PRIMARY_FIELD.to_string(),
            ControlValue::Number(value + amount),
            src,
        );
        Some(result)
    }

    pub fn curve_terms(&self) -> usize {
        self.fields
            .iter()
            .map(|field| match &field.value {
                ControlValue::Curve(curve) => curve.terms.len(),
                _ => 0,
            })
            .sum()
    }
}

fn validate_field_name(name: &str, allow_primary: bool) -> Result<(), ControlMapError> {
    if name.is_empty() {
        return Err(ControlMapError::EmptyName);
    }
    if !allow_primary && name == PRIMARY_FIELD {
        return Err(ControlMapError::ReservedName);
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlMapError {
    EmptyName,
    ReservedName,
}

impl fmt::Display for ControlMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControlMapError::EmptyName => write!(f, "control field name cannot be empty"),
            ControlMapError::ReservedName => {
                write!(
                    f,
                    "{PRIMARY_FIELD:?} is reserved for the event's primary value"
                )
            }
        }
    }
}

impl core::error::Error for ControlMapError {}

/// A value carried by a pattern.
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Leaf(ControlValue),
    Map(ControlMap),
}

impl Value {
    pub fn number(value: f64) -> Value {
        Value::Leaf(ControlValue::Number(value))
    }

    pub fn text(value: impl Into<String>) -> Value {
        Value::Leaf(ControlValue::Text(value.into()))
    }

    pub fn boolean(value: bool) -> Value {
        Value::Leaf(ControlValue::Bool(value))
    }

    pub fn curve(value: Curve) -> Value {
        Value::Leaf(ControlValue::Curve(value))
    }

    pub fn as_leaf(&self) -> Option<&ControlValue> {
        match self {
            Value::Leaf(value) => Some(value),
            Value::Map(_) => None,
        }
    }

    pub fn as_map(&self) -> Option<&ControlMap> {
        match self {
            Value::Map(map) => Some(map),
            Value::Leaf(_) => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        self.as_leaf().and_then(ControlValue::as_f64)
    }

    pub fn as_str(&self) -> Option<&str> {
        self.as_leaf().and_then(ControlValue::as_str)
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Leaf(value) => value.truthy(),
            Value::Map(map) => !map.is_empty(),
        }
    }

    pub fn into_map(self, src: Option<SrcSpan>) -> ControlMap {
        match self {
            Value::Leaf(value) => ControlMap::primary(value, src),
            Value::Map(map) => map,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Leaf(value) => value.fmt(f),
            Value::Map(map) => {
                write!(f, "{{")?;
                for (index, field) in map.fields().iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{} = {}", field.name(), field.value())?;
                }
                write!(f, "}}")
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ValueLimits {
    pub map_fields: usize,
    pub curve_terms: usize,
}

impl Default for ValueLimits {
    fn default() -> Self {
        ValueLimits {
            map_fields: 256,
            curve_terms: 4_096,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum ValueLimitError {
    MapFields {
        found: usize,
        limit: usize,
        src: Option<SrcSpan>,
    },
    CurveTerms {
        found: usize,
        limit: usize,
        src: Option<SrcSpan>,
    },
    InvalidCurve {
        source: crate::CurveError,
        src: Option<SrcSpan>,
    },
    NonFiniteNumber {
        src: Option<SrcSpan>,
    },
}

impl ValueLimitError {
    pub fn src(&self) -> Option<SrcSpan> {
        match self {
            ValueLimitError::MapFields { src, .. }
            | ValueLimitError::CurveTerms { src, .. }
            | ValueLimitError::InvalidCurve { src, .. }
            | ValueLimitError::NonFiniteNumber { src } => *src,
        }
    }
}

impl fmt::Display for ValueLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueLimitError::MapFields { found, limit, .. } => {
                write!(f, "control map has {found} fields, limit is {limit}")
            }
            ValueLimitError::CurveTerms { found, limit, .. } => {
                write!(f, "control map has {found} curve terms, limit is {limit}")
            }
            ValueLimitError::InvalidCurve { source, .. } => source.fmt(f),
            ValueLimitError::NonFiniteNumber { .. } => {
                write!(f, "control number must be finite")
            }
        }
    }
}

impl core::error::Error for ValueLimitError {}

impl ControlValue {
    pub fn validate(&self, src: Option<SrcSpan>) -> Result<(), ValueLimitError> {
        match self {
            ControlValue::Number(value) if !value.is_finite() => {
                Err(ValueLimitError::NonFiniteNumber { src })
            }
            ControlValue::Curve(curve) => curve
                .validate()
                .map_err(|source| ValueLimitError::InvalidCurve { source, src }),
            _ => Ok(()),
        }
    }
}

impl ControlMap {
    pub fn validate_limits(&self, limits: ValueLimits) -> Result<(), ValueLimitError> {
        if self.len() > limits.map_fields {
            return Err(ValueLimitError::MapFields {
                found: self.len(),
                limit: limits.map_fields,
                src: self
                    .fields
                    .get(limits.map_fields)
                    .and_then(ControlField::src),
            });
        }
        for field in &self.fields {
            field.value.validate(field.src)?;
        }
        let terms = self.curve_terms();
        if terms > limits.curve_terms {
            let mut seen = 0;
            let src = self.fields.iter().find_map(|field| {
                let ControlValue::Curve(curve) = &field.value else {
                    return None;
                };
                seen += curve.terms.len();
                (seen > limits.curve_terms).then_some(field.src).flatten()
            });
            return Err(ValueLimitError::CurveTerms {
                found: terms,
                limit: limits.curve_terms,
                src,
            });
        }
        Ok(())
    }
}

impl Value {
    pub fn validate_limits(
        &self,
        limits: ValueLimits,
        src: Option<SrcSpan>,
    ) -> Result<(), ValueLimitError> {
        match self {
            Value::Leaf(value) => {
                value.validate(src)?;
                if let ControlValue::Curve(curve) = value
                    && curve.terms.len() > limits.curve_terms
                {
                    return Err(ValueLimitError::CurveTerms {
                        found: curve.terms.len(),
                        limit: limits.curve_terms,
                        src,
                    });
                }
                Ok(())
            }
            Value::Map(map) => map.validate_limits(limits),
        }
    }
}

/// Stable construction-site identity carried by an event.
///
/// This is not a runtime voice handle. It exists only so reproducible values
/// can derive a seed from semantic provenance and exact time rather than from
/// query order or an allocation counter.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EventOrigin(u64);

impl EventOrigin {
    pub const ANONYMOUS: EventOrigin = EventOrigin(0);

    pub const fn binding(binding: u64) -> EventOrigin {
        EventOrigin(binding)
    }

    pub fn source(binding: u64, src: Option<SrcSpan>) -> EventOrigin {
        match src {
            Some(src) => EventOrigin(
                rand::mix(0x5352_4300 ^ binding ^ src.start as u64)
                    ^ rand::mix((src.end as u64).rotate_left(17)),
            ),
            None => EventOrigin::binding(binding),
        }
    }

    pub(crate) fn recorded(timeline: u64, ordinal: u64) -> EventOrigin {
        EventOrigin(rand::mix(0x5449_4d45 ^ timeline) ^ rand::mix(ordinal))
    }
}

/// Identity of a group constructor within one evaluated program.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GroupNode(u64);

impl GroupNode {
    pub const fn new(binding: u64) -> GroupNode {
        GroupNode(binding)
    }

    pub(crate) fn from_source(binding: u64, byte: usize) -> GroupNode {
        GroupNode(rand::mix(0x4752_4f55 ^ binding ^ byte as u64))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GroupKey(u64);

/// Immutable provenance attached beside an event value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GroupProvenance {
    pub key: GroupKey,
    /// Original member index, zero-based in the IR.
    pub index: u32,
    pub count: u32,
    /// Optional construction-time leader within the group.
    ///
    /// Pattern gives this no musical meaning. The chord builder uses it for
    /// the harmonic root; other group constructors leave it absent.
    pub primary: Option<u32>,
    node: GroupNode,
}

impl GroupProvenance {
    pub(crate) fn new(
        node: GroupNode,
        occurrence: Span,
        index: u32,
        count: u32,
        primary: Option<u32>,
    ) -> Self {
        GroupProvenance {
            key: group_key(node, occurrence),
            index,
            count,
            primary,
            node,
        }
    }

    pub(crate) fn at_occurrence(self, occurrence: Span) -> GroupProvenance {
        GroupProvenance::new(self.node, occurrence, self.index, self.count, self.primary)
    }
}

fn exact_time_hash(t: Frac) -> u64 {
    rand::mix(t.num() as u64) ^ rand::mix((t.den() as u64).rotate_left(17))
}

fn span_hash(span: Span) -> u64 {
    rand::mix(exact_time_hash(span.begin)) ^ rand::mix(exact_time_hash(span.end).rotate_left(23))
}

fn group_key(node: GroupNode, occurrence: Span) -> GroupKey {
    GroupKey(rand::mix(node.0) ^ rand::mix(span_hash(occurrence)))
}

/// One occurrence of a value in time.
#[derive(Clone, PartialEq, Debug)]
pub struct Event {
    /// The event's full extent. `None` for a continuous signal, which has a
    /// value everywhere and an onset nowhere.
    pub whole: Option<Span>,
    /// The fragment of `whole` that fell inside the query.
    pub part: Span,
    pub value: Value,
    pub src: Option<SrcSpan>,
    pub origin: EventOrigin,
    pub group: Option<GroupProvenance>,
}

impl Event {
    /// Whether this fragment contains the event's beginning.
    ///
    /// The distinction between `whole` and `part` exists for exactly this
    /// question: a voice is triggered on an onset and merely continues
    /// otherwise. Querying a window that happens to bisect a note must not
    /// retrigger it.
    pub fn has_onset(&self) -> bool {
        match self.whole {
            Some(w) => w.begin == self.part.begin,
            None => false,
        }
    }

    /// Duration of the whole event, falling back to the queried fragment.
    pub fn duration(&self) -> crate::frac::Frac {
        self.whole.unwrap_or(self.part).length()
    }

    /// A reproducible seed for this exact occurrence.
    ///
    /// Group membership is included explicitly, so simultaneous members do not
    /// reshuffle when query order changes. A runtime voice handle must never be
    /// fed into this value.
    pub fn seed(&self) -> u64 {
        let occurrence = self.whole.unwrap_or(self.part);
        let group = self
            .group
            .map(|group| {
                rand::mix(group.key.0)
                    ^ rand::mix(group.index as u64)
                    ^ rand::mix(group.count as u64)
            })
            .unwrap_or(0);
        rand::mix(self.origin.0) ^ rand::mix(span_hash(occurrence)) ^ group
    }

    pub fn map_time(&self, f: impl Fn(crate::frac::Frac) -> crate::frac::Frac + Copy) -> Event {
        let whole = self.whole.map(|w| w.map(f));
        let part = self.part.map(f);
        let occurrence = whole.unwrap_or(part);
        Event {
            whole,
            part,
            value: self.value.clone(),
            src: self.src,
            origin: self.origin,
            group: self.group.map(|group| group.at_occurrence(occurrence)),
        }
    }
}
