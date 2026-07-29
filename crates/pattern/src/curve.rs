//! Data-only control curves shared by patterns and synthesis.
//!
//! A curve is a sum of basis functions over an explicit note clock. It owns no
//! callback, runtime state, or audio object, so carrying one through an event
//! does not sample it. Synthesis chooses the clock once when a voice is
//! instantiated.

/// Which note-relative coordinate a curve's term times use.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CurveClock {
    /// Seconds since note onset.
    NoteSeconds,
    /// Fraction of the scheduled note duration: 0 at onset, 1 at release.
    ///
    /// Evaluation clamps this coordinate to `[0, 1]`. A term beginning at
    /// exactly 1 is reachable; one beginning after 1 is invalid rather than
    /// silently dead.
    NotePhase,
}

/// One basis in a curve sum.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Basis {
    Step,
    Ramp,
    Decay,
    Sine,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CurveTerm {
    pub basis: Basis,
    pub coefficient: f64,
    pub delay: f64,
    pub length: f64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CurveRange {
    pub min: f64,
    pub max: f64,
}

impl CurveRange {
    pub fn contains(self, min: f64, max: f64) -> bool {
        self.min >= min && self.max <= max
    }
}

/// Whether the range of a curve satisfies a requested closed interval.
///
/// `Inconclusive` is deliberately distinct from `Unsafe`: a conservative
/// enclosure may be wider than the actual curve when decay or sine terms
/// cancel. Callers must not report such a curve as out of range.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RangeProof {
    Safe { enclosure: CurveRange },
    Unsafe { proven: CurveRange },
    Inconclusive { enclosure: CurveRange },
}

/// How a note-clock curve participates in voice lifetime.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CurveActivity {
    /// The curve becomes zero at this coordinate and may retain a voice until
    /// then. Decay uses its documented one-length (~5%) settling convention.
    Finite(f64),
    /// The curve does not terminate: it contains a sine or settles to a
    /// nonzero value. Like oscillators, noise and DC, it is deliberately cut
    /// at the note gate rather than allowed to create an infinite voice.
    GateBounded,
}

/// `offset + Σ coefficientᵢ · basisᵢ(t - delayᵢ)`.
#[derive(Clone, PartialEq, Debug)]
pub struct Curve {
    pub clock: CurveClock,
    pub offset: f64,
    pub terms: Vec<CurveTerm>,
}

impl Curve {
    pub fn new(clock: CurveClock, offset: f64) -> Curve {
        Curve {
            clock,
            offset,
            terms: Vec::new(),
        }
    }

    pub fn constant(clock: CurveClock, value: f64) -> Curve {
        Curve::new(clock, value)
    }

    pub fn term(mut self, basis: Basis, coefficient: f64, delay: f64, length: f64) -> Curve {
        self.terms.push(CurveTerm {
            basis,
            coefficient,
            delay,
            length,
        });
        self
    }

    pub fn decay(clock: CurveClock, length: f64) -> Curve {
        Curve::new(clock, 0.0).term(Basis::Decay, 1.0, 0.0, length)
    }

    pub fn window(clock: CurveClock, begin: f64, end: f64) -> Curve {
        Curve::new(clock, 0.0)
            .term(Basis::Step, 1.0, begin, 0.0)
            .term(Basis::Step, -1.0, end, 0.0)
    }

    pub fn validate(&self) -> Result<(), CurveError> {
        if !self.offset.is_finite() {
            return Err(CurveError::NonFiniteOffset);
        }
        for (index, term) in self.terms.iter().enumerate() {
            if !term.coefficient.is_finite() || !term.delay.is_finite() || !term.length.is_finite()
            {
                return Err(CurveError::NonFiniteTerm { index });
            }
            if term.length < 0.0 {
                return Err(CurveError::NegativeLength { index });
            }
            if self.clock == CurveClock::NotePhase && term.delay > 1.0 {
                return Err(CurveError::UnreachablePhaseTerm { index });
            }
        }
        Ok(())
    }

    /// Prove the curve's relation to `[min, max]` over its reachable domain.
    ///
    /// Step and ramp sums are piecewise linear, so their exact extrema occur
    /// at term boundaries. Decay and sine terms receive a conservative
    /// enclosure. This is exact for windows and breakpoint envelopes while
    /// remaining sound for arbitrary basis sums.
    pub fn prove_range(&self, min: f64, max: f64) -> Result<RangeProof, CurveError> {
        self.validate()?;
        if !min.is_finite() || !max.is_finite() || min > max {
            return Err(CurveError::InvalidRange);
        }

        let linear = self.linear_range();
        let nonlinear = self.nonlinear_enclosure();
        let enclosure = CurveRange {
            min: linear.min + nonlinear.min,
            max: linear.max + nonlinear.max,
        };
        if !enclosure.min.is_finite() || !enclosure.max.is_finite() {
            return Err(CurveError::NonFiniteBounds);
        }
        if enclosure.contains(min, max) {
            return Ok(RangeProof::Safe { enclosure });
        }

        let has_nonlinear = self
            .terms
            .iter()
            .any(|term| matches!(term.basis, Basis::Decay | Basis::Sine));
        if !has_nonlinear {
            return Ok(RangeProof::Unsafe { proven: enclosure });
        }

        let witnessed = self
            .range_candidates()
            .into_iter()
            .map(|t| self.at_coordinate(t))
            .fold(
                CurveRange {
                    min: f64::INFINITY,
                    max: f64::NEG_INFINITY,
                },
                |range, value| CurveRange {
                    min: range.min.min(value),
                    max: range.max.max(value),
                },
            );
        if witnessed.min < min || witnessed.max > max {
            Ok(RangeProof::Unsafe { proven: witnessed })
        } else {
            Ok(RangeProof::Inconclusive { enclosure })
        }
    }

    /// A sound enclosure retained for callers that only need budgeting.
    pub fn bounds(&self) -> Result<(f64, f64), CurveError> {
        self.validate()?;
        let linear = self.linear_range();
        let nonlinear = self.nonlinear_enclosure();
        let min = linear.min + nonlinear.min;
        let max = linear.max + nonlinear.max;
        if !min.is_finite() || !max.is_finite() {
            return Err(CurveError::NonFiniteBounds);
        }
        Ok((min, max))
    }

    /// Determine whether this curve can extend a note beyond its gate.
    pub fn activity(&self) -> Result<CurveActivity, CurveError> {
        self.validate()?;
        if self
            .terms
            .iter()
            .any(|term| term.basis == Basis::Sine && term.coefficient != 0.0)
        {
            return Ok(CurveActivity::GateBounded);
        }

        let (terminal_delta, coefficient_magnitude, linear_term_count) = self
            .terms
            .iter()
            .filter(|term| matches!(term.basis, Basis::Step | Basis::Ramp))
            .fold((0.0, 0.0, 0usize), |(sum, magnitude, count), term| {
                (
                    sum + term.coefficient,
                    magnitude + term.coefficient.abs(),
                    count + 1,
                )
            });
        let terminal = self.offset + terminal_delta;
        // Breakpoint envelopes are represented as successive differences.
        // Even when the authored terminal level is exactly zero, summing those
        // independently rounded coefficients often leaves a few ulps behind.
        // Scale the tolerance by both magnitude and operation count: a real
        // nonzero terminal still remains gate-bounded, while arithmetic residue
        // does not truncate the curve's finite activity horizon.
        let terminal_magnitude = self.offset.abs() + coefficient_magnitude;
        let terminal_tolerance = f64::EPSILON * (linear_term_count + 1) as f64 * terminal_magnitude;
        if terminal.abs() > terminal_tolerance {
            return Ok(CurveActivity::GateBounded);
        }

        let horizon = self
            .terms
            .iter()
            .map(|term| match term.basis {
                Basis::Step => term.delay,
                Basis::Ramp | Basis::Decay => term.delay + term.length,
                Basis::Sine => 0.0,
            })
            .fold(0.0, f64::max);
        Ok(CurveActivity::Finite(match self.clock {
            CurveClock::NoteSeconds => horizon,
            CurveClock::NotePhase => horizon.clamp(0.0, 1.0),
        }))
    }

    /// Evaluate using this curve's declared note clock.
    pub fn at(&self, elapsed_seconds: f64, note_seconds: f64) -> Result<f64, CurveError> {
        let t = match self.clock {
            CurveClock::NoteSeconds => elapsed_seconds,
            CurveClock::NotePhase => {
                if !note_seconds.is_finite() || note_seconds <= 0.0 {
                    return Err(CurveError::DurationRequired);
                }
                (elapsed_seconds / note_seconds).clamp(0.0, 1.0)
            }
        };
        Ok(self.at_coordinate(t))
    }

    pub fn at_coordinate(&self, t: f64) -> f64 {
        let mut sum = self.offset;
        for term in &self.terms {
            sum += term.coefficient * basis_at(*term, t);
        }
        sum
    }

    fn domain_end(&self) -> Option<f64> {
        match self.clock {
            CurveClock::NoteSeconds => None,
            CurveClock::NotePhase => Some(1.0),
        }
    }

    fn linear_range(&self) -> CurveRange {
        let mut candidates = vec![0.0];
        for term in self
            .terms
            .iter()
            .filter(|term| matches!(term.basis, Basis::Step | Basis::Ramp))
        {
            if term.delay >= 0.0 && self.domain_end().is_none_or(|end| term.delay <= end) {
                candidates.push(term.delay);
            }
            if term.basis == Basis::Ramp {
                let end = term.delay + term.length;
                if end >= 0.0 && self.domain_end().is_none_or(|domain| end <= domain) {
                    candidates.push(end);
                }
            }
        }
        if let Some(end) = self.domain_end() {
            candidates.push(end);
        } else {
            let settled = self
                .terms
                .iter()
                .filter(|term| matches!(term.basis, Basis::Step | Basis::Ramp))
                .map(|term| term.delay + term.length)
                .fold(0.0, f64::max);
            candidates.push(settled.max(0.0));
        }

        let mut range = CurveRange {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        };
        for t in candidates {
            let before = self.linear_at(t, true);
            let after = self.linear_at(t, false);
            range.min = range.min.min(before).min(after);
            range.max = range.max.max(before).max(after);
        }
        range
    }

    fn linear_at(&self, t: f64, before: bool) -> f64 {
        self.offset
            + self
                .terms
                .iter()
                .filter(|term| matches!(term.basis, Basis::Step | Basis::Ramp))
                .map(|term| {
                    let value = match term.basis {
                        Basis::Step => {
                            if term.delay < t || (!before && term.delay == t) {
                                1.0
                            } else {
                                0.0
                            }
                        }
                        Basis::Ramp => basis_at(*term, t),
                        Basis::Decay | Basis::Sine => unreachable!(),
                    };
                    term.coefficient * value
                })
                .sum::<f64>()
    }

    fn nonlinear_enclosure(&self) -> CurveRange {
        self.terms
            .iter()
            .filter(|term| matches!(term.basis, Basis::Decay | Basis::Sine))
            .filter(|term| self.domain_end().is_none_or(|end| term.delay <= end))
            .fold(CurveRange { min: 0.0, max: 0.0 }, |range, term| {
                CurveRange {
                    min: range.min + term.coefficient.min(0.0),
                    max: range.max + term.coefficient.max(0.0),
                }
            })
    }

    fn range_candidates(&self) -> Vec<f64> {
        let mut candidates = vec![0.0];
        for term in &self.terms {
            for t in [
                term.delay,
                term.delay + term.length * 0.25,
                term.delay + term.length * 0.5,
                term.delay + term.length * 0.75,
                term.delay + term.length,
            ] {
                if t >= 0.0 && self.domain_end().is_none_or(|end| t <= end) {
                    candidates.push(t);
                }
            }
        }
        if let Some(end) = self.domain_end() {
            candidates.push(end);
        }
        candidates
    }
}

fn basis_at(term: CurveTerm, t: f64) -> f64 {
    let u = t - term.delay;
    if u < 0.0 {
        return 0.0;
    }
    match term.basis {
        Basis::Step => 1.0,
        Basis::Ramp => {
            if term.length <= 0.0 {
                1.0
            } else {
                (u / term.length).min(1.0)
            }
        }
        Basis::Decay => {
            if term.length <= 0.0 {
                0.0
            } else {
                (-3.0 * u / term.length).exp()
            }
        }
        Basis::Sine => {
            if term.length <= 0.0 {
                0.0
            } else {
                0.5 - 0.5 * (core::f64::consts::TAU * u / term.length).cos()
            }
        }
    }
}

impl Default for Curve {
    fn default() -> Self {
        Curve::new(CurveClock::NoteSeconds, 0.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurveError {
    NonFiniteOffset,
    NonFiniteTerm { index: usize },
    NegativeLength { index: usize },
    UnreachablePhaseTerm { index: usize },
    NonFiniteBounds,
    InvalidRange,
    DurationRequired,
}

impl core::fmt::Display for CurveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CurveError::NonFiniteOffset => write!(f, "curve offset must be finite"),
            CurveError::NonFiniteTerm { index } => {
                write!(f, "curve term {index} contains a non-finite number")
            }
            CurveError::NegativeLength { index } => {
                write!(f, "curve term {index} has a negative length")
            }
            CurveError::UnreachablePhaseTerm { index } => {
                write!(
                    f,
                    "NotePhase curve term {index} begins after phase 1 and is unreachable"
                )
            }
            CurveError::NonFiniteBounds => write!(f, "curve bounds are not finite"),
            CurveError::InvalidRange => {
                write!(f, "curve range must be finite and ordered")
            }
            CurveError::DurationRequired => {
                write!(f, "NotePhase requires a positive finite note duration")
            }
        }
    }
}

impl core::error::Error for CurveError {}
