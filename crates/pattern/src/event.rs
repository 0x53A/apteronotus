//! What a query returns.

use crate::span::Span;
use crate::{Frac, rand};
use core::fmt;

/// A value carried by a pattern.
///
/// Deliberately monomorphic rather than `Pattern<T>`. A generic pattern is
/// lovely until a stack has to hold both a note name and a cutoff frequency,
/// and this is also the shape that has to cross a process or ABI boundary
/// later, so there is no point pretending otherwise in between.
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    F(f64),
    S(String),
    B(bool),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::F(x) => Some(*x),
            Value::B(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::S(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::S(s) => Some(s),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::B(b) => *b,
            Value::F(x) => *x != 0.0,
            Value::S(s) => !s.is_empty() && s != "~",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::F(x) => write!(f, "{x}"),
            Value::S(s) => write!(f, "{s}"),
            Value::B(b) => write!(f, "{b}"),
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
    node: GroupNode,
}

impl GroupProvenance {
    pub(crate) fn new(node: GroupNode, occurrence: Span, index: u32, count: u32) -> Self {
        GroupProvenance {
            key: group_key(node, occurrence),
            index,
            count,
            node,
        }
    }

    pub(crate) fn at_occurrence(self, occurrence: Span) -> GroupProvenance {
        GroupProvenance::new(self.node, occurrence, self.index, self.count)
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
