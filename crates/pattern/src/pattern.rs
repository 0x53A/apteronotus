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
    ControlMap, ControlMapError, Event, EventOrigin, GroupNode, GroupProvenance, SrcSpan, Value,
    ValueLimitError, ValueLimits,
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
}

impl Signal {
    pub fn at(self, t: Frac) -> f64 {
        use core::f64::consts::TAU;
        let x = t.to_f64();
        match self {
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
    },
    /// One per cycle, in turn. `<a b>` in the mini-notation.
    Slowcat(Vec<Pattern>),
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
    Signal(Signal),
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

            Pattern::Group { node, members } => {
                let count = members.len() as u32;
                for (index, member) in members.iter().enumerate() {
                    for mut event in member.query(span) {
                        let occurrence = event.whole.unwrap_or(event.part);
                        event.group =
                            Some(GroupProvenance::new(*node, occurrence, index as u32, count));
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

            Pattern::Signal(sig) => {
                out.push(Event {
                    whole: None,
                    part: span,
                    value: Value::number(sig.at(span.midpoint())),
                    src: None,
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
            Pattern::Pure { .. } | Pattern::Signal(_) => 1.0,
            Pattern::Stack(ps) | Pattern::Group { members: ps, .. } => {
                ps.iter().map(Pattern::density).sum()
            }
            Pattern::Slowcat(ps) => ps.iter().map(Pattern::density).fold(0.0, f64::max),
            Pattern::Timecat(parts) => parts.iter().map(|(_, p)| p.density()).sum(),
            Pattern::Fast { factor, inner } => inner.density() * factor.to_f64().abs(),
            Pattern::Shift { inner, .. }
            | Pattern::Rev(inner)
            | Pattern::Range { inner, .. }
            | Pattern::Named { inner, .. }
            | Pattern::Degrade { inner, .. } => inner.density(),
            Pattern::Merge { structure, .. } => structure.density(),
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
        match members.len() {
            0 => Pattern::Silence,
            _ => Pattern::Group { node, members },
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

    pub fn signal(sig: Signal) -> Pattern {
        Pattern::Signal(sig)
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

    pub fn rev(self) -> Pattern {
        Pattern::Rev(Box::new(self))
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
            | Pattern::Rev(inner)
            | Pattern::Degrade { inner, .. }
            | Pattern::Segment { inner, .. }
            | Pattern::Range { inner, .. }
            | Pattern::Named { inner, .. } => inner.validate_stored_values(limits),
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
            Pattern::Silence | Pattern::Signal(_) => Ok(()),
        }
    }

    fn value_cost(&self) -> ValueCost {
        match self {
            Pattern::Silence | Pattern::Signal(_) => ValueCost::leaf(),
            Pattern::Pure { value, .. } => ValueCost::of(value),
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
            | Pattern::Rev(inner)
            | Pattern::Degrade { inner, .. }
            | Pattern::Segment { inner, .. }
            | Pattern::Range { inner, .. } => inner.value_cost(),
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

/// Return the first source location proving that `pattern` may emit a leaf.
///
/// `None` means every possible event is map-valued. Silence is vacuously
/// map-producing. Merge is map-producing by construction because it always
/// lifts its left event, even when the right side has no event at that onset.
fn first_leaf_producer(pattern: &Pattern) -> Option<Option<SrcSpan>> {
    match pattern {
        Pattern::Silence | Pattern::Named { .. } | Pattern::Merge { .. } => None,
        Pattern::Pure {
            value: Value::Map(_),
            ..
        } => None,
        Pattern::Pure {
            value: Value::Leaf(_),
            src,
            ..
        } => Some(*src),
        Pattern::Signal(_) => Some(None),
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
        | Pattern::Rev(inner)
        | Pattern::Degrade { inner, .. }
        | Pattern::Segment { inner, .. }
        | Pattern::Range { inner, .. } => first_leaf_producer(inner),
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
