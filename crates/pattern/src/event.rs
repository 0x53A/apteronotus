//! What a query returns.

use crate::span::Span;
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

    pub fn map_time(&self, f: impl Fn(crate::frac::Frac) -> crate::frac::Frac + Copy) -> Event {
        Event {
            whole: self.whole.map(|w| w.map(f)),
            part: self.part.map(f),
            value: self.value.clone(),
            src: self.src,
        }
    }
}
