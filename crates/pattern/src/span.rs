//! Half-open time spans, measured in cycles.

use crate::frac::Frac;

/// `[begin, end)` in cycles. Zero-width spans are legal and meaningful: they
/// are how the scheduler asks "what is happening exactly now" of a continuous
/// signal without also collecting a cycle's worth of discrete events.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub begin: Frac,
    pub end: Frac,
}

impl Span {
    pub fn new(begin: Frac, end: Frac) -> Span {
        Span { begin, end }
    }

    /// The whole of cycle `n`.
    pub fn cycle(n: i64) -> Span {
        Span::new(Frac::int(n), Frac::int(n + 1))
    }

    pub fn is_empty(self) -> bool {
        self.begin == self.end
    }

    pub fn length(self) -> Frac {
        self.end - self.begin
    }

    pub fn midpoint(self) -> Frac {
        self.begin + self.length() / Frac::int(2)
    }

    /// Overlap of two spans, or `None` when they do not touch.
    ///
    /// Two spans that merely share an endpoint do not overlap — the interval is
    /// half-open, so a note ending at 1/2 and one starting at 1/2 are adjacent,
    /// not simultaneous. The exception is a zero-width query, which must still
    /// find the event it lands inside.
    pub fn sect(self, other: Span) -> Option<Span> {
        let begin = self.begin.max(other.begin);
        let end = self.end.min(other.end);
        if begin > end {
            return None;
        }
        if begin == end {
            let zero_width_inside = (self.is_empty() && other.begin <= begin && begin < other.end)
                || (other.is_empty() && self.begin <= begin && begin < self.end);
            let both_empty = self.is_empty() && other.is_empty();
            if !(zero_width_inside || both_empty) {
                return None;
            }
        }
        Some(Span::new(begin, end))
    }

    /// Split at cycle boundaries, so that every piece lies within one cycle.
    ///
    /// Most combinators are defined per cycle — alternation, `rev`, `every` —
    /// and this is what lets them be written as if a query never straddled a
    /// boundary.
    pub fn cycles(self) -> Vec<Span> {
        if self.begin > self.end {
            return Vec::new();
        }
        if self.is_empty() {
            return vec![self];
        }
        let mut out = Vec::new();
        let mut b = self.begin;
        while b < self.end {
            let next = b.sam() + Frac::ONE;
            let e = next.min(self.end);
            out.push(Span::new(b, e));
            b = e;
        }
        out
    }

    /// Apply `f` to both endpoints.
    pub fn map(self, f: impl Fn(Frac) -> Frac) -> Span {
        Span::new(f(self.begin), f(self.end))
    }

    /// Reflect within the cycle containing `pivot_cycle`.
    pub fn reflect(self, cycle: Frac) -> Span {
        let axis = cycle + cycle + Frac::ONE;
        Span::new(axis - self.end, axis - self.begin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(n: i64, d: i64) -> Frac {
        Frac::new(n, d)
    }

    #[test]
    fn splits_on_cycle_boundaries() {
        let s = Span::new(f(1, 2), f(5, 2));
        let cs = s.cycles();
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0], Span::new(f(1, 2), f(1, 1)));
        assert_eq!(cs[1], Span::new(f(1, 1), f(2, 1)));
        assert_eq!(cs[2], Span::new(f(2, 1), f(5, 2)));
    }

    #[test]
    fn whole_cycle_is_one_piece() {
        assert_eq!(Span::cycle(3).cycles(), vec![Span::cycle(3)]);
    }

    #[test]
    fn zero_width_survives_splitting() {
        let s = Span::new(f(3, 2), f(3, 2));
        assert_eq!(s.cycles(), vec![s]);
    }

    #[test]
    fn adjacent_spans_do_not_intersect() {
        let a = Span::new(Frac::ZERO, f(1, 2));
        let b = Span::new(f(1, 2), Frac::ONE);
        assert_eq!(a.sect(b), None);
    }

    #[test]
    fn zero_width_query_finds_its_event() {
        let note = Span::new(Frac::ZERO, f(1, 2));
        let now = Span::new(f(1, 4), f(1, 4));
        assert_eq!(now.sect(note), Some(now));
        // ...but not one it merely abuts.
        let after = Span::new(f(1, 2), f(1, 2));
        assert_eq!(after.sect(note), None);
    }

    #[test]
    fn reflection_is_an_involution() {
        let s = Span::new(f(1, 4), f(1, 2));
        assert_eq!(s.reflect(Frac::ZERO).reflect(Frac::ZERO), s);
        assert_eq!(s.reflect(Frac::ZERO), Span::new(f(1, 2), f(3, 4)));
    }
}
