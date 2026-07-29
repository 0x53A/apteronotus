//! The pattern algebra.
//!
//! A pattern is a pure function from a time span to the events occurring in it.
//! It is represented as a tree rather than as a boxed closure, which costs a
//! little indirection and buys three things the closure form cannot give:
//! source spans survive every combinator (so the editor can highlight what is
//! sounding), the tree can be inspected and serialised, and a query is
//! obviously reproducible because there is nowhere for state to hide.
//!
//! `query` must be **idempotent over overlapping windows**. The scheduler runs
//! ahead of the audio clock and the editor asks about the same instant again;
//! both must get the same answer. Nothing in here may advance a cursor.

use crate::event::{
    ControlMap, ControlMapError, ControlValue, Event, EventOrigin, GroupNode, GroupProvenance,
    SrcSpan, Value, ValueLimitError, ValueLimits,
};
use crate::frac::Frac;
use crate::rand;
use crate::span::Span;
use crate::timeline::Timeline;

/// A continuous signal: defined everywhere, with an onset nowhere.
///
/// Sampling one yields an event with no `whole`, so a voice never triggers on
/// it — it is something to *steer* a parameter with, not something to play.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Signal {
    /// A value defined at every transport coordinate.
    Constant(f64),
    /// Unipolar sine, one cycle per cycle, in `[0, 1]`.
    Sine,
    Cosine,
    /// Rising ramp, `[0, 1)` across each cycle.
    Saw,
    /// Falling ramp.
    Isaw,
    Tri,
    Square,
    /// Uniform noise, resampled continuously.
    Rand(u64),
    /// Smoothed value noise, one control point per cycle.
    Perlin(u64),
    /// Cycle position itself, unbounded.
    Time,
    /// A left-closed transport step.
    Step {
        at: Frac,
    },
    /// A clamped linear transition beginning at transport zero.
    Line {
        from: f64,
        to: f64,
        length: Frac,
    },
    /// One inside `[begin, end)`, zero elsewhere.
    Window {
        begin: Frac,
        end: Frac,
    },
}

impl Signal {
    pub fn at(self, t: Frac) -> f64 {
        use core::f64::consts::TAU;
        let x = t.to_f64();
        match self {
            Signal::Constant(value) => value,
            Signal::Sine => 0.5 + 0.5 * (x * TAU).sin(),
            Signal::Cosine => 0.5 + 0.5 * (x * TAU).cos(),
            Signal::Saw => t.cycle_pos().to_f64(),
            Signal::Isaw => 1.0 - t.cycle_pos().to_f64(),
            Signal::Tri => {
                let p = t.cycle_pos().to_f64();
                if p < 0.5 { p * 2.0 } else { 2.0 - p * 2.0 }
            }
            Signal::Square => {
                if t.cycle_pos().to_f64() < 0.5 {
                    0.0
                } else {
                    1.0
                }
            }
            Signal::Rand(seed) => rand::at(t, seed),
            Signal::Perlin(seed) => {
                let a = rand::at_cycle(t, seed);
                let b = rand::at_cycle(t + Frac::ONE, seed);
                let f = t.cycle_pos().to_f64();
                // smoothstep, so the joins have no corner
                let f = f * f * (3.0 - 2.0 * f);
                a + (b - a) * f
            }
            Signal::Time => x,
            Signal::Step { at } => f64::from(t >= at),
            Signal::Line { from, to, length } => {
                if length <= Frac::ZERO {
                    return if t < Frac::ZERO { from } else { to };
                }
                let phase = (t / length).to_f64().clamp(0.0, 1.0);
                from + (to - from) * phase
            }
            Signal::Window { begin, end } => f64::from(begin <= t && t < end),
        }
    }
}

/// Pointwise numeric arithmetic in transport time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatternMathOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArpMode {
    Up,
    Down,
    OutsideIn,
    InsideOut,
}

impl PatternMathOp {
    fn apply(self, left: f64, right: f64) -> f64 {
        match self {
            PatternMathOp::Add => left + right,
            PatternMathOp::Sub => left - right,
            PatternMathOp::Mul => left * right,
            PatternMathOp::Div => left / right,
        }
    }
}

/// A pattern of values in cycle time.
///
/// Query results have a stable **structural order**. Stack and group children
/// are visited in stored order, and transforms preserve that order unless
/// their documented semantics select or reflect branches. This order is
/// deterministic but has no musical meaning: chord order comes from
/// [`GroupProvenance`], never from a result vector index.
#[derive(Clone, PartialEq, Debug)]
pub enum Pattern {
    Silence,
    Pure {
        value: Value,
        src: Option<SrcSpan>,
        origin: EventOrigin,
    },
    /// Everything at once.
    Stack(Vec<Pattern>),
    /// Related simultaneous members, unlike a plain [`Pattern::Stack`].
    Group {
        node: GroupNode,
        members: Vec<Pattern>,
        primary: Option<u32>,
    },
    /// One per cycle, in turn. `<a b>` in the mini-notation.
    Slowcat(Vec<Pattern>),
    /// Choose one stored branch reproducibly per cycle.
    Choose {
        seed: u64,
        choices: Vec<Pattern>,
    },
    /// A weighted sequence filling one cycle. This is what a bare mini-notation
    /// sequence compiles to; `a@3 b` gives weights 3 and 1.
    Timecat(Vec<(Frac, Pattern)>),
    Fast {
        factor: Frac,
        inner: Box<Pattern>,
    },
    /// Move later in time by `by` (negative moves earlier).
    Shift {
        by: Frac,
        inner: Box<Pattern>,
    },
    /// Replace each discrete event's extent with an onset-relative duration.
    ///
    /// Querying widens backwards by `duration`, so an event that began before
    /// the requested slice still appears as a continuation. This is why hold
    /// is an AST node rather than a value sampled later by the scheduler.
    Hold {
        duration: Frac,
        inner: Box<Pattern>,
    },
    Rev(Box<Pattern>),
    /// Choose a branch per cycle. `every n f p` is the case where `then` is
    /// `f(p)` and `otherwise` is `p` — the transform is applied when the tree
    /// is built, so the tree itself stays free of function values.
    When {
        modulo: i64,
        offset: i64,
        then: Box<Pattern>,
        otherwise: Box<Pattern>,
    },
    /// Drop (or, with `keep`, retain only) events whose position hashes below
    /// `amount`.
    Degrade {
        amount: f64,
        seed: u64,
        keep: bool,
        inner: Box<Pattern>,
    },
    /// Repeat each discrete event within its existing extent. One count is
    /// selected reproducibly from `counts` using the event's stable identity.
    Ply {
        counts: Vec<u32>,
        seed: u64,
        inner: Box<Pattern>,
    },
    /// Consume simultaneous group provenance and emit members serially within
    /// the group's original span.
    Arp {
        mode: ArpMode,
        spacing: Option<Frac>,
        inner: Box<Pattern>,
    },
    /// Retain only a group's construction-time primary member.
    GroupPrimary {
        inner: Box<Pattern>,
    },
    /// Add to a numeric primary value without changing event structure.
    PrimaryAdd {
        amount: f64,
        inner: Box<Pattern>,
    },
    Signal {
        signal: Signal,
        src: Option<SrcSpan>,
    },
    /// Sample `inner` into `steps` discrete events per cycle, giving a
    /// continuous signal onsets so it can be played rather than only steered.
    Segment {
        steps: i64,
        inner: Box<Pattern>,
    },
    /// Affinely map numeric values from `[0, 1]` onto `[lo, hi]`.
    Range {
        lo: f64,
        hi: f64,
        inner: Box<Pattern>,
    },
    /// Numeric arithmetic where at least one operand is continuous.
    ///
    /// If one side has event structure, that structure is retained and the
    /// continuous side is sampled at its onset. A zero-width query samples at
    /// the query coordinate, which is how `Merge` projects a transport signal
    /// into an init value at the left event's onset.
    Math {
        op: PatternMathOp,
        left: Box<Pattern>,
        right: Box<Pattern>,
    },
    /// Lift every bare value into one named control field. Values already
    /// lifted into a map pass through unchanged.
    Named {
        name: String,
        inner: Box<Pattern>,
    },
    /// Preserve the left event structure and merge one right map sampled at
    /// each left event onset.
    Merge {
        structure: Box<Pattern>,
        controls: ControlPattern,
    },
    /// Finite, non-repeating material.
    Timeline(Timeline),
}

/// A pattern proven to emit maps whenever it emits an event.
///
/// The inner pattern is private so [`Pattern::Merge`] cannot contain a
/// leaf-producing right side. Silence is vacuously valid, which lets ordinary
/// transforms such as `degrade` and `every` retain their silent branches.
#[derive(Clone, PartialEq, Debug)]
pub struct ControlPattern {
    inner: Box<Pattern>,
}

impl ControlPattern {
    pub fn as_pattern(&self) -> &Pattern {
        &self.inner
    }

    pub fn into_pattern(self) -> Pattern {
        *self.inner
    }
}

impl TryFrom<Pattern> for ControlPattern {
    type Error = ControlPatternError;

    fn try_from(pattern: Pattern) -> Result<Self, Self::Error> {
        if let Some(src) = first_leaf_producer(&pattern) {
            return Err(ControlPatternError { src });
        }
        Ok(ControlPattern {
            inner: Box::new(pattern),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ControlPatternError {
    src: Option<SrcSpan>,
}

impl ControlPatternError {
    pub fn src(self) -> Option<SrcSpan> {
        self.src
    }
}

impl core::fmt::Display for ControlPatternError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "merge controls must be named fields; wrap the value in a control setter"
        )
    }
}

impl core::error::Error for ControlPatternError {}

impl Pattern {
    /// A conservative finite transport extent when it is structurally
    /// knowable without querying an arbitrary horizon.
    ///
    /// This is intentionally partial. It exists for lifecycle consumers such
    /// as a persistent patch driven by a captured timeline; cyclic patterns
    /// and transforms whose support cannot be proven finite return `None`.
    pub fn finite_extent(&self) -> Option<Span> {
        let union = |patterns: &[Pattern]| {
            let mut patterns = patterns.iter();
            let first = patterns.next()?.finite_extent()?;
            patterns.try_fold(first, |extent, pattern| {
                let next = pattern.finite_extent()?;
                Some(Span::new(
                    extent.begin.min(next.begin),
                    extent.end.max(next.end),
                ))
            })
        };
        match self {
            Pattern::Timeline(timeline) => Some(timeline.extent()),
            Pattern::Stack(patterns) => union(patterns),
            Pattern::Group { members, .. } => union(members),
            Pattern::Fast { factor, inner } if *factor > Frac::ZERO => {
                let extent = inner.finite_extent()?;
                Some(Span::new(extent.begin / *factor, extent.end / *factor))
            }
            Pattern::Shift { by, inner } => {
                let extent = inner.finite_extent()?;
                Some(Span::new(extent.begin + *by, extent.end + *by))
            }
            Pattern::Hold { duration, inner } => {
                let extent = inner.finite_extent()?;
                Some(Span::new(
                    extent.begin,
                    extent.end.max(extent.begin + *duration),
                ))
            }
            Pattern::Degrade { inner, .. }
            | Pattern::Ply { inner, .. }
            | Pattern::Arp { inner, .. }
            | Pattern::GroupPrimary { inner }
            | Pattern::PrimaryAdd { inner, .. }
            | Pattern::Range { inner, .. }
            | Pattern::Named { inner, .. } => inner.finite_extent(),
            Pattern::Merge { structure, .. } => structure.finite_extent(),
            Pattern::Silence
            | Pattern::Pure { .. }
            | Pattern::Slowcat(_)
            | Pattern::Choose { .. }
            | Pattern::Timecat(_)
            | Pattern::Rev(_)
            | Pattern::When { .. }
            | Pattern::Signal { .. }
            | Pattern::Segment { .. }
            | Pattern::Math { .. }
            | Pattern::Fast { .. } => None,
        }
    }

    // ---------------------------------------------------------------- query

    /// Every event overlapping `span`, in stable structural order.
    pub fn query(&self, span: Span) -> Vec<Event> {
        if span.begin > span.end {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.query_into(span, &mut out);
        out
    }

    fn query_into(&self, span: Span, out: &mut Vec<Event>) {
        match self {
            Pattern::Silence => {}

            Pattern::Pure { value, src, origin } => {
                for c in span.cycles() {
                    let sam = c.begin.sam();
                    out.push(Event {
                        whole: Some(Span::new(sam, sam + Frac::ONE)),
                        part: c,
                        value: value.clone(),
                        src: *src,
                        origin: *origin,
                        group: None,
                    });
                }
            }

            Pattern::Stack(ps) => {
                for p in ps {
                    p.query_into(span, out);
                }
            }

            Pattern::Group {
                node,
                members,
                primary,
            } => {
                let count = members.len() as u32;
                for (index, member) in members.iter().enumerate() {
                    for mut event in member.query(span) {
                        let occurrence = event.whole.unwrap_or(event.part);
                        event.group = Some(GroupProvenance::new(
                            *node,
                            occurrence,
                            index as u32,
                            count,
                            *primary,
                        ));
                        out.push(event);
                    }
                }
            }

            Pattern::Slowcat(ps) => {
                if ps.is_empty() {
                    return;
                }
                let n = ps.len() as i64;
                for c in span.cycles() {
                    let cyc = c.begin.floor();
                    let i = cyc.rem_euclid(n);
                    // Each branch advances its own cycle only on the turns it
                    // is chosen, so `<a b>` steps a and b rather than sampling
                    // them at the outer cycle.
                    let offset = Frac::int(cyc - (cyc - i).div_euclid(n));
                    let inner = ps[i as usize].query(c.map(|t| t - offset));
                    out.extend(inner.into_iter().map(|e| e.map_time(|t| t + offset)));
                }
            }

            Pattern::Choose { seed, choices } => {
                for cycle in span.cycles() {
                    let choice = (rand::at_cycle(cycle.begin, *seed) * choices.len() as f64).floor()
                        as usize;
                    choices[choice.min(choices.len() - 1)].query_into(cycle, out);
                }
            }

            Pattern::Timecat(parts) => {
                if parts.is_empty() {
                    return;
                }
                let total = parts
                    .iter()
                    .fold(Frac::ZERO, |acc, (w, _)| acc + (*w).max(Frac::ZERO));
                if total.is_zero() {
                    return;
                }
                for c in span.cycles() {
                    let cyc = c.begin.sam();
                    let mut acc = Frac::ZERO;
                    for (w, p) in parts {
                        let w = (*w).max(Frac::ZERO);
                        let begin = cyc + acc / total;
                        acc = acc + w;
                        let end = cyc + acc / total;
                        let len = end - begin;
                        if len.is_zero() {
                            continue;
                        }
                        let Some(q) = c.sect(Span::new(begin, end)) else {
                            continue;
                        };
                        // The slot holds one cycle of the child, compressed.
                        let inner = p.query(q.map(|t| cyc + (t - begin) / len));
                        out.extend(
                            inner
                                .into_iter()
                                .map(|e| e.map_time(|t| begin + (t - cyc) * len)),
                        );
                    }
                }
            }

            Pattern::Fast { factor, inner } => {
                let r = *factor;
                if r.is_zero() {
                    return;
                }
                if r.is_negative() {
                    let flipped = Pattern::Rev(Box::new(Pattern::Fast {
                        factor: -r,
                        inner: inner.clone(),
                    }));
                    flipped.query_into(span, out);
                    return;
                }
                let inner = inner.query(span.map(|t| t * r));
                out.extend(inner.into_iter().map(|e| e.map_time(|t| t / r)));
            }

            Pattern::Shift { by, inner } => {
                let by = *by;
                let inner = inner.query(span.map(|t| t - by));
                out.extend(inner.into_iter().map(|e| e.map_time(|t| t + by)));
            }

            Pattern::Hold { duration, inner } => {
                let duration = (*duration).max(Frac::ZERO);
                if duration.is_zero() {
                    return;
                }
                // A held event may begin before the requested window while
                // still overlapping it. Looking back by exactly its maximum
                // extent retrieves every such event without retained state.
                let widened = Span::new(span.begin - duration, span.end);
                for mut event in inner.query(widened) {
                    let Some(original) = event.whole else {
                        // Hold is defined only for discrete event structure.
                        // Language builders reject continuous inputs.
                        continue;
                    };
                    let held = Span::new(original.begin, original.begin + duration);
                    let Some(part) = span.sect(held) else {
                        continue;
                    };
                    event.whole = Some(held);
                    event.part = part;
                    event.group = event.group.map(|group| group.at_occurrence(held));
                    out.push(event);
                }
            }

            Pattern::Rev(inner) => {
                for c in span.cycles() {
                    let cyc = c.begin.sam();
                    for e in inner.query(c.reflect(cyc)) {
                        out.push(Event {
                            whole: e.whole.map(|w| w.reflect(cyc)),
                            part: e.part.reflect(cyc),
                            value: e.value,
                            src: e.src,
                            origin: e.origin,
                            group: e.group.map(|group| {
                                group.at_occurrence(
                                    e.whole
                                        .map(|whole| whole.reflect(cyc))
                                        .unwrap_or_else(|| e.part.reflect(cyc)),
                                )
                            }),
                        });
                    }
                }
            }

            Pattern::When {
                modulo,
                offset,
                then,
                otherwise,
            } => {
                let m = (*modulo).max(1);
                for c in span.cycles() {
                    let cyc = c.begin.floor();
                    let branch = if (cyc - offset).rem_euclid(m) == 0 {
                        then
                    } else {
                        otherwise
                    };
                    branch.query_into(c, out);
                }
            }

            Pattern::Degrade {
                amount,
                seed,
                keep,
                inner,
            } => {
                for e in inner.query(span) {
                    // Sampled at the event's own beginning, not at the query's,
                    // so a window that bisects a note decides the same way as
                    // one that contains it.
                    let t = e.whole.map(|w| w.begin).unwrap_or(e.part.begin);
                    // Provenance and group membership distinguish simultaneous
                    // events without making the result depend on query order.
                    let r = rand::at(t, *seed ^ e.seed());
                    if (r < *amount) == *keep {
                        out.push(e);
                    }
                }
            }

            Pattern::Ply {
                counts,
                seed,
                inner,
            } => {
                for event in inner.query(span) {
                    let Some(whole) = event.whole else {
                        continue;
                    };
                    let choice =
                        (rand::at_cycle(whole.begin, *seed) * counts.len() as f64).floor() as usize;
                    let count = counts[choice.min(counts.len() - 1)];
                    let step = (whole.end - whole.begin) / Frac::int(i64::from(count));
                    for index in 0..count {
                        let begin = whole.begin + step * Frac::int(i64::from(index));
                        let repeated = Span::new(begin, begin + step);
                        if let Some(part) = span.sect(repeated) {
                            let mut repeated_event = event.clone();
                            repeated_event.whole = Some(repeated);
                            repeated_event.part = part;
                            out.push(repeated_event);
                        }
                    }
                }
            }

            Pattern::Arp {
                mode,
                spacing,
                inner,
            } => {
                let mut arpeggiated_events = Vec::new();
                for mut event in inner.query(span) {
                    let (Some(whole), Some(group)) = (event.whole, event.group) else {
                        arpeggiated_events.push(event);
                        continue;
                    };
                    let order = arp_order(*mode, group.count);
                    let rank = order
                        .iter()
                        .position(|index| *index == group.index)
                        .expect("group member index is within its declared count");
                    let step = spacing
                        .unwrap_or_else(|| {
                            (whole.end - whole.begin) / Frac::int(i64::from(group.count))
                        })
                        .max(Frac::ZERO);
                    let begin = whole.begin + Frac::int(rank as i64) * step;
                    if step.is_zero() || begin >= whole.end {
                        continue;
                    }
                    // The source group's extent remains authoritative. This
                    // keeps repeated-pattern queries pure and makes explicit
                    // spacing a clipping policy rather than a hidden timeline
                    // overflow.
                    let arpeggiated = Span::new(begin, (begin + step).min(whole.end));
                    if let Some(part) = span.sect(arpeggiated) {
                        event.whole = Some(arpeggiated);
                        event.part = part;
                        event.group = None;
                        arpeggiated_events.push(event);
                    }
                }
                arpeggiated_events.sort_by_key(|event| {
                    event
                        .whole
                        .map(|whole| whole.begin)
                        .unwrap_or(event.part.begin)
                });
                out.extend(arpeggiated_events);
            }

            Pattern::GroupPrimary { inner } => {
                for mut event in inner.query(span) {
                    match event.group {
                        None => out.push(event),
                        Some(group) if group.primary == Some(group.index) => {
                            event.group = None;
                            out.push(event);
                        }
                        Some(_) => {}
                    }
                }
            }

            Pattern::PrimaryAdd { amount, inner } => {
                for mut event in inner.query(span) {
                    event.value = match event.value {
                        Value::Leaf(ControlValue::Number(value)) => Value::number(value + amount),
                        Value::Map(map) => map
                            .offset_primary(*amount)
                            .map(Value::Map)
                            .unwrap_or(Value::Map(map)),
                        value => value,
                    };
                    out.push(event);
                }
            }

            Pattern::Signal { signal, src } => {
                out.push(Event {
                    whole: None,
                    part: span,
                    value: Value::number(signal.at(span.midpoint())),
                    src: *src,
                    origin: EventOrigin::ANONYMOUS,
                    group: None,
                });
            }

            Pattern::Segment { steps, inner } => {
                let n = (*steps).max(1);
                let step = Frac::new(1, n);
                for c in span.cycles() {
                    let cyc = c.begin.sam();
                    let first = ((c.begin - cyc) / step).floor();
                    let mut k = first;
                    loop {
                        let begin = cyc + Frac::int(k) * step;
                        if begin >= c.end && !c.is_empty() {
                            break;
                        }
                        if k > first && c.is_empty() {
                            break;
                        }
                        let slot = Span::new(begin, begin + step);
                        if let Some(part) = c.sect(slot)
                            && let Some(v) = inner.query(slot).into_iter().next()
                        {
                            out.push(Event {
                                whole: Some(slot),
                                part,
                                value: v.value,
                                src: v.src,
                                origin: v.origin,
                                group: v.group.map(|group| group.at_occurrence(slot)),
                            });
                        }
                        k += 1;
                        if k - first > n + 1 {
                            break;
                        }
                    }
                }
            }

            Pattern::Range { lo, hi, inner } => {
                for mut e in inner.query(span) {
                    if let Some(x) = e.value.as_f64() {
                        e.value = Value::number(lo + x * (hi - lo));
                    }
                    out.push(e);
                }
            }

            Pattern::Math { op, left, right } => {
                let left_continuous = left.is_continuous_signal();
                let right_continuous = right.is_continuous_signal();
                if left_continuous && right_continuous {
                    if let (Some(left_event), Some(right_event)) = (
                        sample_numeric_event(left, span),
                        sample_numeric_event(right, span),
                    ) && let (Some(left), Some(right)) =
                        (left_event.value.as_f64(), right_event.value.as_f64())
                    {
                        out.push(Event {
                            whole: None,
                            part: span,
                            value: Value::number(op.apply(left, right)),
                            src: left_event.src.or(right_event.src),
                            origin: EventOrigin::ANONYMOUS,
                            group: None,
                        });
                    }
                } else {
                    let (structured, continuous, structured_is_left) = if right_continuous {
                        (left.as_ref(), right.as_ref(), true)
                    } else {
                        (right.as_ref(), left.as_ref(), false)
                    };
                    for mut event in structured.query(span) {
                        let sample_span = if span.is_empty() {
                            span
                        } else {
                            let at = event
                                .whole
                                .map(|whole| whole.begin)
                                .unwrap_or(event.part.begin);
                            Span::new(at, at)
                        };
                        let Some(structured) = event.value.as_f64() else {
                            // `Pattern::math` rejects this with a source-aware
                            // diagnostic. A manually constructed invalid enum
                            // node must not silently preserve the old value.
                            debug_assert!(false, "invalid nonnumeric Pattern::Math operand");
                            continue;
                        };
                        let Some(continuous) = sample_numeric(continuous, sample_span) else {
                            continue;
                        };
                        let (left, right) = if structured_is_left {
                            (structured, continuous)
                        } else {
                            (continuous, structured)
                        };
                        event.value = Value::number(op.apply(left, right));
                        out.push(event);
                    }
                }
            }

            Pattern::Named { name, inner } => {
                for mut event in inner.query(span) {
                    if let Value::Leaf(value) = event.value {
                        let map = ControlMap::named(name.clone(), value, event.src)
                            .expect("Named field names are validated by the builder");
                        event.value = Value::Map(map);
                    }
                    out.push(event);
                }
            }

            Pattern::Merge {
                structure,
                controls,
            } => {
                for mut event in structure.query(span) {
                    let onset = event
                        .whole
                        .map(|whole| whole.begin)
                        .unwrap_or(event.part.begin);
                    let sampled = controls
                        .as_pattern()
                        .query(Span::new(onset, onset))
                        .into_iter()
                        .next()
                        .map(|sample| match sample.value {
                            Value::Map(map) => map,
                            Value::Leaf(_) => {
                                unreachable!("ControlPattern cannot emit a leaf value")
                            }
                        });
                    let left = event.value.into_map(event.src);
                    if let Some(right) = sampled {
                        event.value = Value::Map(left.merged(&right));
                    } else {
                        event.value = Value::Map(left);
                    }
                    out.push(event);
                }
            }

            Pattern::Timeline(timeline) => timeline.query_into(span, out),
        }
    }

    /// Every event whose beginning falls inside `span`.
    ///
    /// This is what triggers voices, and it solves exactly one of the two
    /// duplicate-note problems:
    ///
    /// * A window that **bisects** a note does not retrigger it. The fragment
    ///   has `whole.begin < part.begin`, so it is filtered out here.
    /// * A window that **overlaps** a previous window *does* return the same
    ///   onset again, and deliberately so. Two identical onsets at the same
    ///   instant are legal — stack a line on a copy of itself and you have
    ///   two — so nothing at this level can distinguish an intended pair from
    ///   an accidental repeat, and silently collapsing them would be a bug in
    ///   the other direction.
    ///
    /// **So a scheduler must advance a non-overlapping frontier**: fill from
    /// `[filled, filled + n)`, then set `filled` to the end. Adjacent windows
    /// tile a timeline exactly once, whatever the split points are, which is
    /// the property `tests/algebra.rs` pins down. Re-querying an already-filled
    /// region is a scheduler bug, not something this call can absorb.
    ///
    /// A live edit is the other half of the same problem and also belongs to
    /// the scheduler: replacing the program while events are already queued
    /// needs a generation counter, so that pending events from the previous
    /// version can be dropped without touching intentional duplicates.
    pub fn onsets(&self, span: Span) -> Vec<Event> {
        self.query(span)
            .into_iter()
            .filter(Event::has_onset)
            .collect()
    }

    /// Roughly how many events one cycle will produce.
    ///
    /// A node count cannot bound this, because speed factors *multiply* down
    /// the tree: `[[bd*32]*32]*32` is about ten nodes and thirty-two thousand
    /// events. So the ceiling has to be computed over the built tree rather
    /// than counted while parsing.
    ///
    /// An estimate, deliberately: `Degrade` only ever removes events, and a
    /// slowed pattern is charged its full density, so this is an upper bound
    /// on the common cases rather than an exact figure. `f64` because the
    /// products it is guarding against are precisely the ones that overflow.
    pub fn density(&self) -> f64 {
        match self {
            Pattern::Silence => 0.0,
            Pattern::Pure { .. } | Pattern::Signal { .. } => 1.0,
            Pattern::Stack(ps) | Pattern::Group { members: ps, .. } => {
                ps.iter().map(Pattern::density).sum()
            }
            Pattern::Slowcat(ps) | Pattern::Choose { choices: ps, .. } => {
                ps.iter().map(Pattern::density).fold(0.0, f64::max)
            }
            Pattern::Timecat(parts) => parts.iter().map(|(_, p)| p.density()).sum(),
            Pattern::Fast { factor, inner } => inner.density() * factor.to_f64().abs(),
            Pattern::Shift { inner, .. }
            | Pattern::Hold { inner, .. }
            | Pattern::Rev(inner)
            | Pattern::Range { inner, .. }
            | Pattern::Named { inner, .. }
            | Pattern::Degrade { inner, .. }
            | Pattern::Arp { inner, .. }
            | Pattern::GroupPrimary { inner }
            | Pattern::PrimaryAdd { inner, .. } => inner.density(),
            Pattern::Ply { counts, inner, .. } => {
                inner.density() * counts.iter().copied().max().unwrap_or(1) as f64
            }
            Pattern::Merge { structure, .. } => structure.density(),
            Pattern::Math { left, right, .. } => {
                if left.is_continuous_signal() {
                    right.density()
                } else {
                    left.density()
                }
            }
            Pattern::When {
                then, otherwise, ..
            } => then.density().max(otherwise.density()),
            Pattern::Segment { steps, .. } => *steps as f64,
            Pattern::Timeline(timeline) => timeline.events().len() as f64,
        }
    }

    // -------------------------------------------------------------- builders

    pub fn silence() -> Pattern {
        Pattern::Silence
    }

    pub fn pure(value: Value) -> Pattern {
        Pattern::Pure {
            value,
            src: None,
            origin: EventOrigin::ANONYMOUS,
        }
    }

    pub fn num(x: f64) -> Pattern {
        Pattern::pure(Value::number(x))
    }

    pub fn word(s: &str) -> Pattern {
        Pattern::pure(Value::text(s))
    }

    /// A privileged primary-value constructor.
    ///
    /// Public named fields may not use the reserved `"value"` name. Language
    /// bindings use this constructor for typed `note(...)` values instead.
    pub fn primary(value: crate::ControlValue) -> Pattern {
        Pattern::pure(Value::Map(ControlMap::primary(value, None)))
    }

    /// A privileged primary value attributed to a frontend source site.
    pub fn primary_at(value: crate::ControlValue, src: SrcSpan) -> Pattern {
        Pattern::Pure {
            value: Value::Map(ControlMap::primary(value, Some(src))),
            src: Some(src),
            origin: EventOrigin::ANONYMOUS,
        }
    }

    /// An unweighted sequence filling one cycle.
    pub fn seq(items: Vec<Pattern>) -> Pattern {
        match items.len() {
            0 => Pattern::Silence,
            1 => items.into_iter().next().unwrap(),
            _ => Pattern::Timecat(items.into_iter().map(|p| (Frac::ONE, p)).collect()),
        }
    }

    pub fn stack(items: Vec<Pattern>) -> Pattern {
        match items.len() {
            0 => Pattern::Silence,
            1 => items.into_iter().next().unwrap(),
            _ => Pattern::Stack(items),
        }
    }

    pub fn group(node: GroupNode, members: Vec<Pattern>) -> Pattern {
        Pattern::group_with_primary(node, members, None)
    }

    pub fn group_with_primary(
        node: GroupNode,
        members: Vec<Pattern>,
        primary: Option<u32>,
    ) -> Pattern {
        match members.len() {
            0 => Pattern::Silence,
            _ => {
                let primary = primary.filter(|index| (*index as usize) < members.len());
                Pattern::Group {
                    node,
                    members,
                    primary,
                }
            }
        }
    }

    /// One item per cycle, in turn.
    pub fn cat(items: Vec<Pattern>) -> Pattern {
        match items.len() {
            0 => Pattern::Silence,
            1 => items.into_iter().next().unwrap(),
            _ => Pattern::Slowcat(items),
        }
    }

    pub fn choose(seed: u64, choices: Vec<Pattern>) -> Pattern {
        match choices.len() {
            0 => Pattern::Silence,
            1 => choices.into_iter().next().expect("one choice"),
            _ => Pattern::Choose { seed, choices },
        }
    }

    pub fn signal(sig: Signal) -> Pattern {
        Pattern::Signal {
            signal: sig,
            src: None,
        }
    }

    /// Construct a continuous signal attributed to a frontend source site.
    pub fn signal_at(sig: Signal, src: SrcSpan) -> Pattern {
        Pattern::Signal {
            signal: sig,
            src: Some(src),
        }
    }

    pub fn timeline(timeline: Timeline) -> Pattern {
        Pattern::Timeline(timeline)
    }

    // --------------------------------------------------------------- methods

    pub fn fast(self, factor: Frac) -> Pattern {
        Pattern::Fast {
            factor,
            inner: Box::new(self),
        }
    }

    pub fn slow(self, factor: Frac) -> Pattern {
        if factor.is_zero() {
            return Pattern::Silence;
        }
        self.fast(factor.recip())
    }

    pub fn late(self, by: Frac) -> Pattern {
        Pattern::Shift {
            by,
            inner: Box::new(self),
        }
    }

    pub fn early(self, by: Frac) -> Pattern {
        self.late(-by)
    }

    /// Give every discrete event an onset-relative duration in cycles.
    pub fn hold(self, duration: Frac) -> Pattern {
        Pattern::Hold {
            duration,
            inner: Box::new(self),
        }
    }

    pub fn rev(self) -> Pattern {
        Pattern::Rev(Box::new(self))
    }

    pub fn ply(self, counts: Vec<u32>, seed: u64) -> Pattern {
        debug_assert!(!counts.is_empty());
        debug_assert!(counts.iter().all(|count| *count > 0));
        Pattern::Ply {
            counts,
            seed,
            inner: Box::new(self),
        }
    }

    pub fn arp(self, mode: ArpMode) -> Pattern {
        Pattern::Arp {
            mode,
            spacing: None,
            inner: Box::new(self),
        }
    }

    pub fn arp_spaced(self, mode: ArpMode, spacing: Frac) -> Pattern {
        Pattern::Arp {
            mode,
            spacing: Some(spacing),
            inner: Box::new(self),
        }
    }

    pub fn group_primary(self) -> Pattern {
        Pattern::GroupPrimary {
            inner: Box::new(self),
        }
    }

    pub fn add_to_primary(self, amount: f64) -> Result<Pattern, PatternMathError> {
        if let Some(src) = first_non_numeric_primary(&self) {
            return Err(PatternMathError::NonNumeric { src });
        }
        Ok(Pattern::PrimaryAdd {
            amount,
            inner: Box::new(self),
        })
    }

    pub fn segment(self, steps: i64) -> Pattern {
        Pattern::Segment {
            steps,
            inner: Box::new(self),
        }
    }

    pub fn range(self, lo: f64, hi: f64) -> Pattern {
        Pattern::Range {
            lo,
            hi,
            inner: Box::new(self),
        }
    }

    pub fn math(self, op: PatternMathOp, right: Pattern) -> Result<Pattern, PatternMathError> {
        if !self.is_continuous_signal() && !right.is_continuous_signal() {
            return Err(PatternMathError::NeedsJoin);
        }
        let structured = if self.is_continuous_signal() {
            &right
        } else {
            &self
        };
        if let Some(src) = first_non_numeric_producer(structured) {
            return Err(PatternMathError::NonNumeric { src });
        }
        Ok(Pattern::Math {
            op,
            left: Box::new(self),
            right: Box::new(right),
        })
    }

    pub fn is_continuous_signal(&self) -> bool {
        match self {
            Pattern::Silence | Pattern::Signal { .. } => true,
            Pattern::Choose { .. } => false,
            Pattern::Fast { inner, .. }
            | Pattern::Shift { inner, .. }
            | Pattern::Hold { inner, .. }
            | Pattern::Rev(inner)
            | Pattern::Range { inner, .. }
            | Pattern::Ply { inner, .. }
            | Pattern::Arp { inner, .. }
            | Pattern::GroupPrimary { inner }
            | Pattern::PrimaryAdd { inner, .. } => inner.is_continuous_signal(),
            Pattern::When {
                then, otherwise, ..
            } => then.is_continuous_signal() && otherwise.is_continuous_signal(),
            Pattern::Math { left, right, .. } => {
                left.is_continuous_signal() && right.is_continuous_signal()
            }
            Pattern::Pure { .. }
            | Pattern::Stack(_)
            | Pattern::Group { .. }
            | Pattern::Slowcat(_)
            | Pattern::Timecat(_)
            | Pattern::Degrade { .. }
            | Pattern::Segment { .. }
            | Pattern::Named { .. }
            | Pattern::Merge { .. }
            | Pattern::Timeline(_) => false,
        }
    }

    /// Whether this tree contains an explicit event-duration transform.
    ///
    /// Frontends use this to reject ambiguous duplicate `hold` declarations;
    /// it conveys no transport or music-domain information.
    pub fn has_hold(&self) -> bool {
        match self {
            Pattern::Hold { .. } => true,
            Pattern::Choose { choices, .. } => choices.iter().any(Pattern::has_hold),
            Pattern::Stack(patterns)
            | Pattern::Slowcat(patterns)
            | Pattern::Group {
                members: patterns, ..
            } => patterns.iter().any(Pattern::has_hold),
            Pattern::Timecat(parts) => parts.iter().any(|(_, pattern)| pattern.has_hold()),
            Pattern::Fast { inner, .. }
            | Pattern::Shift { inner, .. }
            | Pattern::Rev(inner)
            | Pattern::Degrade { inner, .. }
            | Pattern::Ply { inner, .. }
            | Pattern::Segment { inner, .. }
            | Pattern::Range { inner, .. }
            | Pattern::Arp { inner, .. }
            | Pattern::GroupPrimary { inner }
            | Pattern::PrimaryAdd { inner, .. }
            | Pattern::Named { inner, .. } => inner.has_hold(),
            Pattern::When {
                then, otherwise, ..
            } => then.has_hold() || otherwise.has_hold(),
            Pattern::Math { left, right, .. } => left.has_hold() || right.has_hold(),
            Pattern::Merge {
                structure,
                controls,
            } => structure.has_hold() || controls.as_pattern().has_hold(),
            Pattern::Silence
            | Pattern::Pure { .. }
            | Pattern::Signal { .. }
            | Pattern::Timeline(_) => false,
        }
    }

    /// Whether any stored event value carries a note-phase curve.
    ///
    /// This is structural evidence used by language frontends to require an
    /// explicit note duration before publishing phase-clock automation.
    pub fn has_note_phase_curve(&self) -> bool {
        match self {
            Pattern::Pure { value, .. } => value_has_note_phase_curve(value),
            Pattern::Choose { choices, .. } => choices.iter().any(Pattern::has_note_phase_curve),
            Pattern::Stack(patterns)
            | Pattern::Slowcat(patterns)
            | Pattern::Group {
                members: patterns, ..
            } => patterns.iter().any(Pattern::has_note_phase_curve),
            Pattern::Timecat(parts) => parts
                .iter()
                .any(|(_, pattern)| pattern.has_note_phase_curve()),
            Pattern::Fast { inner, .. }
            | Pattern::Shift { inner, .. }
            | Pattern::Hold { inner, .. }
            | Pattern::Rev(inner)
            | Pattern::Degrade { inner, .. }
            | Pattern::Ply { inner, .. }
            | Pattern::Segment { inner, .. }
            | Pattern::Range { inner, .. }
            | Pattern::Arp { inner, .. }
            | Pattern::GroupPrimary { inner }
            | Pattern::PrimaryAdd { inner, .. }
            | Pattern::Named { inner, .. } => inner.has_note_phase_curve(),
            Pattern::When {
                then, otherwise, ..
            } => then.has_note_phase_curve() || otherwise.has_note_phase_curve(),
            Pattern::Math { left, right, .. } => {
                left.has_note_phase_curve() || right.has_note_phase_curve()
            }
            Pattern::Merge {
                structure,
                controls,
            } => structure.has_note_phase_curve() || controls.as_pattern().has_note_phase_curve(),
            Pattern::Timeline(timeline) => timeline
                .events()
                .iter()
                .any(|event| value_has_note_phase_curve(&event.value)),
            Pattern::Silence | Pattern::Signal { .. } => false,
        }
    }

    pub fn named(self, name: impl Into<String>) -> Result<Pattern, ControlMapError> {
        let name = name.into();
        // Use the map constructor as the single field-name policy boundary.
        ControlMap::named(name.clone(), crate::ControlValue::Bool(false), None)?;
        Ok(Pattern::Named {
            name,
            inner: Box::new(self),
        })
    }

    pub fn merge(self, controls: Pattern) -> Result<Pattern, ControlPatternError> {
        Ok(Pattern::Merge {
            structure: Box::new(self),
            controls: ControlPattern::try_from(controls)?,
        })
    }

    /// Validate intrinsic values and caller-selected per-event complexity.
    pub fn validate_value_limits(&self, limits: ValueLimits) -> Result<(), ValueLimitError> {
        self.validate_stored_values(limits)?;
        let cost = self.value_cost();
        if cost.map_fields > limits.map_fields {
            return Err(ValueLimitError::MapFields {
                found: cost.map_fields,
                limit: limits.map_fields,
                src: None,
            });
        }
        if cost.curve_terms > limits.curve_terms {
            return Err(ValueLimitError::CurveTerms {
                found: cost.curve_terms,
                limit: limits.curve_terms,
                src: None,
            });
        }
        Ok(())
    }

    fn validate_stored_values(&self, limits: ValueLimits) -> Result<(), ValueLimitError> {
        match self {
            Pattern::Pure { value, src, .. } => value.validate_limits(limits, *src),
            Pattern::Choose { choices, .. } => {
                for choice in choices {
                    choice.validate_stored_values(limits)?;
                }
                Ok(())
            }
            Pattern::Stack(patterns)
            | Pattern::Slowcat(patterns)
            | Pattern::Group {
                members: patterns, ..
            } => {
                for pattern in patterns {
                    pattern.validate_stored_values(limits)?;
                }
                Ok(())
            }
            Pattern::Timecat(parts) => {
                for (_, pattern) in parts {
                    pattern.validate_stored_values(limits)?;
                }
                Ok(())
            }
            Pattern::Fast { inner, .. }
            | Pattern::Shift { inner, .. }
            | Pattern::Hold { inner, .. }
            | Pattern::Rev(inner)
            | Pattern::Degrade { inner, .. }
            | Pattern::Ply { inner, .. }
            | Pattern::Segment { inner, .. }
            | Pattern::Range { inner, .. }
            | Pattern::Arp { inner, .. }
            | Pattern::GroupPrimary { inner }
            | Pattern::PrimaryAdd { inner, .. }
            | Pattern::Named { inner, .. } => inner.validate_stored_values(limits),
            Pattern::Math { left, right, .. } => {
                left.validate_stored_values(limits)?;
                right.validate_stored_values(limits)
            }
            Pattern::When {
                then, otherwise, ..
            } => {
                then.validate_stored_values(limits)?;
                otherwise.validate_stored_values(limits)
            }
            Pattern::Merge {
                structure,
                controls,
            } => {
                structure.validate_stored_values(limits)?;
                controls.as_pattern().validate_stored_values(limits)
            }
            Pattern::Timeline(timeline) => {
                for event in timeline.events() {
                    event.value.validate_limits(limits, event.src)?;
                }
                Ok(())
            }
            Pattern::Silence | Pattern::Signal { .. } => Ok(()),
        }
    }

    fn value_cost(&self) -> ValueCost {
        match self {
            Pattern::Silence | Pattern::Signal { .. } => ValueCost::leaf(),
            Pattern::Pure { value, .. } => ValueCost::of(value),
            Pattern::Choose { choices, .. } => choices
                .iter()
                .map(Pattern::value_cost)
                .fold(ValueCost::leaf(), ValueCost::max),
            Pattern::Stack(patterns)
            | Pattern::Slowcat(patterns)
            | Pattern::Group {
                members: patterns, ..
            } => patterns
                .iter()
                .map(Pattern::value_cost)
                .fold(ValueCost::leaf(), ValueCost::max),
            Pattern::Timecat(parts) => parts
                .iter()
                .map(|(_, pattern)| pattern.value_cost())
                .fold(ValueCost::leaf(), ValueCost::max),
            Pattern::Fast { inner, .. }
            | Pattern::Shift { inner, .. }
            | Pattern::Hold { inner, .. }
            | Pattern::Rev(inner)
            | Pattern::Degrade { inner, .. }
            | Pattern::Ply { inner, .. }
            | Pattern::Segment { inner, .. }
            | Pattern::Range { inner, .. }
            | Pattern::Arp { inner, .. }
            | Pattern::GroupPrimary { inner }
            | Pattern::PrimaryAdd { inner, .. } => inner.value_cost(),
            Pattern::Math { left, right, .. } => {
                if left.is_continuous_signal() {
                    right.value_cost()
                } else {
                    left.value_cost()
                }
            }
            Pattern::Named { inner, .. } => {
                let inner = inner.value_cost();
                ValueCost {
                    map_fields: inner.map_fields.max(1),
                    curve_terms: inner.curve_terms,
                }
            }
            Pattern::When {
                then, otherwise, ..
            } => then.value_cost().max(otherwise.value_cost()),
            Pattern::Merge {
                structure,
                controls,
            } => {
                let structure = structure.value_cost();
                let controls = controls.as_pattern().value_cost();
                ValueCost {
                    map_fields: structure.map_fields.max(1) + controls.map_fields,
                    curve_terms: structure.curve_terms + controls.curve_terms,
                }
            }
            Pattern::Timeline(timeline) => timeline
                .events()
                .iter()
                .map(|event| ValueCost::of(&event.value))
                .fold(ValueCost::leaf(), ValueCost::max),
        }
    }

    /// Keep events whose position hashes at or above `amount`.
    pub fn degrade_by(self, amount: f64, seed: u64) -> Pattern {
        Pattern::Degrade {
            amount,
            seed,
            keep: false,
            inner: Box::new(self),
        }
    }

    /// The complement of [`Pattern::degrade_by`] — exactly the events it drops.
    pub fn undegrade_by(self, amount: f64, seed: u64) -> Pattern {
        Pattern::Degrade {
            amount,
            seed,
            keep: true,
            inner: Box::new(self),
        }
    }

    /// Apply `f` on every `n`th cycle.
    ///
    /// `f` runs once, here, while the tree is being built; what lands in the
    /// tree is the transformed branch, not the function.
    pub fn every(self, n: i64, f: impl FnOnce(Pattern) -> Pattern) -> Pattern {
        self.every_offset(n, 0, f)
    }

    pub fn every_offset(self, n: i64, offset: i64, f: impl FnOnce(Pattern) -> Pattern) -> Pattern {
        Pattern::When {
            modulo: n.max(1),
            offset,
            then: Box::new(f(self.clone())),
            otherwise: Box::new(self),
        }
    }

    /// Overlay a shifted, transformed copy of the pattern on itself.
    pub fn off(self, by: Frac, f: impl FnOnce(Pattern) -> Pattern) -> Pattern {
        let echo = f(self.clone().late(by));
        Pattern::Stack(vec![self, echo])
    }

    /// Apply `f` to a random `amount` of the events, leaving the rest alone.
    pub fn sometimes_by(
        self,
        amount: f64,
        seed: u64,
        f: impl FnOnce(Pattern) -> Pattern,
    ) -> Pattern {
        let untouched = self.clone().degrade_by(amount, seed);
        let touched = f(self.undegrade_by(amount, seed));
        Pattern::Stack(vec![untouched, touched])
    }

    /// Distribute `self` over a euclidean rhythm of `k` onsets in `n` steps.
    pub fn euclid(self, k: i64, n: i64, rotation: i64) -> Pattern {
        if n <= 0 {
            return Pattern::Silence;
        }
        let bits = bjorklund(k, n);
        let slots = (0..n)
            .map(|i| {
                let on = bits[(i + rotation).rem_euclid(n) as usize];
                (Frac::ONE, if on { self.clone() } else { Pattern::Silence })
            })
            .collect();
        Pattern::Timecat(slots)
    }
}

fn sample_numeric(pattern: &Pattern, span: Span) -> Option<f64> {
    sample_numeric_event(pattern, span).and_then(|event| event.value.as_f64())
}

fn value_has_note_phase_curve(value: &Value) -> bool {
    match value {
        Value::Leaf(ControlValue::Curve(curve)) => curve.clock == crate::CurveClock::NotePhase,
        Value::Map(map) => map.fields().iter().any(|field| {
            matches!(
                field.value(),
                ControlValue::Curve(curve) if curve.clock == crate::CurveClock::NotePhase
            )
        }),
        Value::Leaf(_) => false,
    }
}

fn sample_numeric_event(pattern: &Pattern, span: Span) -> Option<Event> {
    pattern.query(span).into_iter().next()
}

fn arp_order(mode: ArpMode, count: u32) -> Vec<u32> {
    let mut order = (0..count).collect::<Vec<_>>();
    match mode {
        ArpMode::Up => {}
        ArpMode::Down => order.reverse(),
        ArpMode::OutsideIn => {
            order.clear();
            let mut low = 0;
            let mut high = count.saturating_sub(1);
            while low <= high && order.len() < count as usize {
                order.push(low);
                if high != low {
                    order.push(high);
                }
                low += 1;
                high = high.saturating_sub(1);
            }
        }
        ArpMode::InsideOut => {
            order.clear();
            if count > 0 {
                let mut low = (count - 1) / 2;
                let mut high = count / 2;
                loop {
                    order.push(low);
                    if high != low {
                        order.push(high);
                    }
                    if low == 0 && high + 1 >= count {
                        break;
                    }
                    low = low.saturating_sub(1);
                    high = (high + 1).min(count - 1);
                }
            }
        }
    }
    order
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatternMathError {
    NeedsJoin,
    NonNumeric { src: Option<SrcSpan> },
}

impl core::fmt::Display for PatternMathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PatternMathError::NeedsJoin => write!(
                f,
                "arithmetic between two event patterns needs an explicit temporal join; \
                 at least one operand must be a continuous transport signal"
            ),
            PatternMathError::NonNumeric {
                src: Some(SrcSpan { start, end }),
            } => write!(
                f,
                "pattern arithmetic requires numeric event values; nonnumeric value at source \
                 bytes {start}..{end}"
            ),
            PatternMathError::NonNumeric { src: None } => {
                write!(f, "pattern arithmetic requires numeric event values")
            }
        }
    }
}

impl core::error::Error for PatternMathError {}

/// Return the first source location proving that `pattern` may emit a
/// nonnumeric event.
///
/// Every stored pattern value has a known shape, so ordinary builders can
/// reject this mismatch before an invalid `Math` node reaches query time.
/// Silence is vacuously numeric.
fn first_non_numeric_producer(pattern: &Pattern) -> Option<Option<SrcSpan>> {
    match pattern {
        Pattern::Silence | Pattern::Signal { .. } | Pattern::Math { .. } => None,
        Pattern::Choose { choices, .. } => choices.iter().find_map(first_non_numeric_producer),
        Pattern::Pure { value, src, .. } => match value {
            Value::Leaf(value) if value.as_f64().is_some() => None,
            Value::Leaf(_) | Value::Map(_) => Some(*src),
        },
        Pattern::Stack(patterns)
        | Pattern::Slowcat(patterns)
        | Pattern::Group {
            members: patterns, ..
        } => patterns.iter().find_map(first_non_numeric_producer),
        Pattern::Timecat(parts) => parts
            .iter()
            .find_map(|(_, pattern)| first_non_numeric_producer(pattern)),
        Pattern::Fast { inner, .. }
        | Pattern::Shift { inner, .. }
        | Pattern::Hold { inner, .. }
        | Pattern::Rev(inner)
        | Pattern::Degrade { inner, .. }
        | Pattern::Ply { inner, .. }
        | Pattern::Segment { inner, .. }
        | Pattern::Range { inner, .. }
        | Pattern::Arp { inner, .. }
        | Pattern::GroupPrimary { inner }
        | Pattern::PrimaryAdd { inner, .. } => first_non_numeric_producer(inner),
        Pattern::When {
            then, otherwise, ..
        } => first_non_numeric_producer(then).or_else(|| first_non_numeric_producer(otherwise)),
        Pattern::Named { .. } | Pattern::Merge { .. } => Some(None),
        Pattern::Timeline(timeline) => timeline.events().iter().find_map(|event| {
            let numeric = matches!(
                &event.value,
                Value::Leaf(value) if value.as_f64().is_some()
            );
            (!numeric).then_some(event.src)
        }),
    }
}

/// Return the first source location proving that the event's primary field is
/// not numeric.
fn first_non_numeric_primary(pattern: &Pattern) -> Option<Option<SrcSpan>> {
    match pattern {
        Pattern::Silence | Pattern::Signal { .. } | Pattern::Math { .. } => None,
        Pattern::Choose { choices, .. } => choices.iter().find_map(first_non_numeric_primary),
        Pattern::Pure { value, src, .. } => {
            let numeric = match value {
                Value::Leaf(value) => value.as_f64().is_some(),
                Value::Map(map) => map
                    .get(crate::PRIMARY_FIELD)
                    .and_then(ControlValue::as_f64)
                    .is_some(),
            };
            (!numeric).then_some(*src)
        }
        Pattern::Stack(patterns)
        | Pattern::Slowcat(patterns)
        | Pattern::Group {
            members: patterns, ..
        } => patterns.iter().find_map(first_non_numeric_primary),
        Pattern::Timecat(parts) => parts
            .iter()
            .find_map(|(_, pattern)| first_non_numeric_primary(pattern)),
        Pattern::Fast { inner, .. }
        | Pattern::Shift { inner, .. }
        | Pattern::Hold { inner, .. }
        | Pattern::Rev(inner)
        | Pattern::Degrade { inner, .. }
        | Pattern::Ply { inner, .. }
        | Pattern::Segment { inner, .. }
        | Pattern::Range { inner, .. }
        | Pattern::Arp { inner, .. }
        | Pattern::GroupPrimary { inner }
        | Pattern::PrimaryAdd { inner, .. }
        | Pattern::Named { inner, .. } => first_non_numeric_primary(inner),
        Pattern::When {
            then, otherwise, ..
        } => first_non_numeric_primary(then).or_else(|| first_non_numeric_primary(otherwise)),
        Pattern::Merge { structure, .. } => first_non_numeric_primary(structure),
        Pattern::Timeline(timeline) => timeline.events().iter().find_map(|event| {
            let numeric = match &event.value {
                Value::Leaf(value) => value.as_f64().is_some(),
                Value::Map(map) => map
                    .get(crate::PRIMARY_FIELD)
                    .and_then(ControlValue::as_f64)
                    .is_some(),
            };
            (!numeric).then_some(event.src)
        }),
    }
}

/// Return the first source location proving that `pattern` may emit a leaf.
///
/// `None` means every possible event is map-valued. Silence is vacuously
/// map-producing. Merge is map-producing by construction because it always
/// lifts its left event, even when the right side has no event at that onset.
fn first_leaf_producer(pattern: &Pattern) -> Option<Option<SrcSpan>> {
    match pattern {
        Pattern::Silence | Pattern::Named { .. } | Pattern::Merge { .. } => None,
        Pattern::Choose { choices, .. } => choices.iter().find_map(first_leaf_producer),
        Pattern::Pure {
            value: Value::Map(_),
            ..
        } => None,
        Pattern::Pure {
            value: Value::Leaf(_),
            src,
            ..
        } => Some(*src),
        Pattern::Signal { src, .. } => Some(*src),
        Pattern::Stack(patterns)
        | Pattern::Slowcat(patterns)
        | Pattern::Group {
            members: patterns, ..
        } => patterns.iter().find_map(first_leaf_producer),
        Pattern::Timecat(parts) => parts
            .iter()
            .find_map(|(_, pattern)| first_leaf_producer(pattern)),
        Pattern::Fast { inner, .. }
        | Pattern::Shift { inner, .. }
        | Pattern::Hold { inner, .. }
        | Pattern::Rev(inner)
        | Pattern::Degrade { inner, .. }
        | Pattern::Ply { inner, .. }
        | Pattern::Segment { inner, .. }
        | Pattern::Range { inner, .. }
        | Pattern::Arp { inner, .. }
        | Pattern::GroupPrimary { inner }
        | Pattern::PrimaryAdd { inner, .. } => first_leaf_producer(inner),
        Pattern::Math { left, right, .. } => {
            if left.is_continuous_signal() {
                first_leaf_producer(right)
            } else {
                first_leaf_producer(left)
            }
        }
        Pattern::When {
            then, otherwise, ..
        } => first_leaf_producer(then).or_else(|| first_leaf_producer(otherwise)),
        Pattern::Timeline(timeline) => timeline
            .events()
            .iter()
            .find_map(|event| matches!(event.value, Value::Leaf(_)).then_some(event.src)),
    }
}

#[derive(Clone, Copy, Default)]
struct ValueCost {
    map_fields: usize,
    curve_terms: usize,
}

impl ValueCost {
    fn leaf() -> ValueCost {
        ValueCost::default()
    }

    fn of(value: &Value) -> ValueCost {
        match value {
            Value::Leaf(crate::ControlValue::Curve(curve)) => ValueCost {
                map_fields: 0,
                curve_terms: curve.terms.len(),
            },
            Value::Leaf(_) => ValueCost::leaf(),
            Value::Map(map) => ValueCost {
                map_fields: map.len(),
                curve_terms: map.curve_terms(),
            },
        }
    }

    fn max(self, other: ValueCost) -> ValueCost {
        ValueCost {
            map_fields: self.map_fields.max(other.map_fields),
            curve_terms: self.curve_terms.max(other.curve_terms),
        }
    }
}

/// Bjorklund's algorithm: spread `k` onsets as evenly as possible over `n`
/// steps. `bjorklund(3, 8)` is `x..x..x.`, which is most of Latin America.
pub fn bjorklund(k: i64, n: i64) -> Vec<bool> {
    if n <= 0 {
        return Vec::new();
    }
    if k <= 0 {
        return vec![false; n as usize];
    }
    if k >= n {
        return vec![true; n as usize];
    }
    let mut a: Vec<Vec<bool>> = (0..k).map(|_| vec![true]).collect();
    let mut b: Vec<Vec<bool>> = (0..n - k).map(|_| vec![false]).collect();
    while b.len() > 1 {
        let m = a.len().min(b.len());
        let mut merged = Vec::with_capacity(m);
        for i in 0..m {
            let mut v = a[i].clone();
            v.extend_from_slice(&b[i]);
            merged.push(v);
        }
        let rest_a: Vec<Vec<bool>> = a.into_iter().skip(m).collect();
        let rest_b: Vec<Vec<bool>> = b.into_iter().skip(m).collect();
        b = if rest_a.is_empty() { rest_b } else { rest_a };
        a = merged;
    }
    a.into_iter()
        .flatten()
        .chain(b.into_iter().flatten())
        .collect()
}
