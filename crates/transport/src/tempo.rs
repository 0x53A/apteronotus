//! Piecewise tempo as a signal whose integral is transport position.
//!
//! A point without `over` is a step. A point with `over` reaches its declared
//! BPM at `at`, interpolating linearly in BPM over the preceding duration. This
//! makes a section marker such as `{ at = bars(12), bpm = 46, over = bars(4) }`
//! mean "arrive at 46 BPM for bar 12".

use crate::transport::{CycleTime, TransportError};
use apteronotus_pattern::{Frac, Span};

const APPROX_DENOMINATOR: i64 = 1_000_000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TempoPoint {
    pub at: Frac,
    pub bpm: f64,
    pub over: Option<Frac>,
}

impl TempoPoint {
    pub fn step(at: Frac, bpm: f64) -> TempoPoint {
        TempoPoint {
            at,
            bpm,
            over: None,
        }
    }

    pub fn ramp(at: Frac, bpm: f64, over: Frac) -> TempoPoint {
        TempoPoint {
            at,
            bpm,
            over: Some(over),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct TempoMap {
    beats_per_cycle: f64,
    points: Vec<TempoPoint>,
}

impl TempoMap {
    pub fn new(beats_per_cycle: f64, points: Vec<TempoPoint>) -> Result<TempoMap, TempoMapError> {
        if !beats_per_cycle.is_finite() || beats_per_cycle <= 0.0 {
            return Err(TempoMapError::InvalidMeter);
        }
        if points.is_empty() {
            return Err(TempoMapError::Empty);
        }
        for (index, point) in points.iter().enumerate() {
            if !point.bpm.is_finite() || point.bpm <= 0.0 {
                return Err(TempoMapError::InvalidBpm { index });
            }
            if index > 0 && point.at <= points[index - 1].at {
                return Err(TempoMapError::NonIncreasing { index });
            }
            if let Some(over) = point.over {
                if index == 0 || over <= Frac::ZERO {
                    return Err(TempoMapError::InvalidRamp { index });
                }
                let start = point.at - over;
                if start < points[index - 1].at {
                    return Err(TempoMapError::OverlappingRamp { index });
                }
            }
        }
        Ok(TempoMap {
            beats_per_cycle,
            points,
        })
    }

    pub fn constant(bpm: f64, beats_per_cycle: f64) -> Result<TempoMap, TempoMapError> {
        TempoMap::new(beats_per_cycle, vec![TempoPoint::step(Frac::ZERO, bpm)])
    }

    pub fn beats_per_cycle(&self) -> f64 {
        self.beats_per_cycle
    }

    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    pub fn bpm_at(&self, cycle: Frac) -> f64 {
        self.regime_at(cycle.to_f64()).bpm_at(cycle.to_f64())
    }

    pub fn cycle_to_seconds(&self, cycle: Frac) -> f64 {
        self.seconds_between(Frac::ZERO, cycle)
    }

    pub fn span_to_seconds(&self, span: Span) -> f64 {
        self.seconds_between(span.begin, span.end)
    }

    pub fn seconds_to_cycle(&self, seconds: f64) -> Result<Frac, TransportError> {
        if !seconds.is_finite() {
            return Err(TransportError::InvalidSeconds);
        }
        if seconds == 0.0 {
            return Ok(Frac::ZERO);
        }

        let (mut lo, mut hi) = if seconds > 0.0 {
            (0.0, 1.0)
        } else {
            (-1.0, 0.0)
        };
        if seconds > 0.0 {
            while self.seconds_at_f64(hi) < seconds {
                lo = hi;
                hi *= 2.0;
            }
        } else {
            while self.seconds_at_f64(lo) > seconds {
                hi = lo;
                lo *= 2.0;
            }
        }
        for _ in 0..80 {
            let mid = (lo + hi) * 0.5;
            if self.seconds_at_f64(mid) < seconds {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok(Frac::approx((lo + hi) * 0.5, APPROX_DENOMINATOR))
    }

    fn seconds_between(&self, begin: Frac, end: Frac) -> f64 {
        if begin == end {
            return 0.0;
        }
        if begin > end {
            return -self.seconds_between(end, begin);
        }
        self.integrate(begin.to_f64(), end.to_f64())
    }

    fn seconds_at_f64(&self, cycle: f64) -> f64 {
        if cycle >= 0.0 {
            self.integrate(0.0, cycle)
        } else {
            -self.integrate(cycle, 0.0)
        }
    }

    fn integrate(&self, begin: f64, end: f64) -> f64 {
        let mut cuts = vec![begin, end];
        for (index, point) in self.points.iter().enumerate().skip(1) {
            let at = point.at.to_f64();
            if begin < at && at < end {
                cuts.push(at);
            }
            if let Some(over) = point.over {
                let start = (point.at - over).to_f64();
                if begin < start && start < end {
                    cuts.push(start);
                }
            } else {
                debug_assert!(index > 0);
            }
        }
        cuts.sort_by(f64::total_cmp);

        let seconds_per_beat = 60.0 * self.beats_per_cycle;
        cuts.windows(2)
            .map(|window| {
                let x0 = window[0];
                let x1 = window[1];
                let regime = self.regime_at((x0 + x1) * 0.5);
                let bpm0 = regime.bpm_at(x0);
                if regime.slope == 0.0 {
                    seconds_per_beat * (x1 - x0) / bpm0
                } else {
                    let bpm1 = regime.bpm_at(x1);
                    seconds_per_beat / regime.slope * (bpm1 / bpm0).ln()
                }
            })
            .sum()
    }

    fn regime_at(&self, cycle: f64) -> Regime {
        let mut bpm = self.points[0].bpm;
        for point in self.points.iter().skip(1) {
            let at = point.at.to_f64();
            match point.over {
                None => {
                    if cycle < at {
                        break;
                    }
                    bpm = point.bpm;
                }
                Some(over) => {
                    let start = (point.at - over).to_f64();
                    if cycle < start {
                        break;
                    }
                    if cycle < at {
                        return Regime {
                            origin: start,
                            bpm,
                            slope: (point.bpm - bpm) / over.to_f64(),
                        };
                    }
                    bpm = point.bpm;
                }
            }
        }
        Regime {
            origin: cycle,
            bpm,
            slope: 0.0,
        }
    }
}

impl CycleTime for TempoMap {
    fn cycle_to_seconds(&self, cycle: Frac) -> f64 {
        TempoMap::cycle_to_seconds(self, cycle)
    }

    fn span_to_seconds(&self, span: Span) -> f64 {
        TempoMap::span_to_seconds(self, span)
    }

    fn seconds_to_cycle(&self, seconds: f64) -> Result<Frac, TransportError> {
        TempoMap::seconds_to_cycle(self, seconds)
    }
}

#[derive(Clone, Copy)]
struct Regime {
    origin: f64,
    bpm: f64,
    slope: f64,
}

impl Regime {
    fn bpm_at(self, cycle: f64) -> f64 {
        self.bpm + self.slope * (cycle - self.origin)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TempoMapError {
    Empty,
    InvalidMeter,
    InvalidBpm { index: usize },
    NonIncreasing { index: usize },
    InvalidRamp { index: usize },
    OverlappingRamp { index: usize },
}

impl core::fmt::Display for TempoMapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TempoMapError::Empty => write!(f, "tempo map has no points"),
            TempoMapError::InvalidMeter => {
                write!(f, "beats per cycle must be finite and greater than zero")
            }
            TempoMapError::InvalidBpm { index } => {
                write!(f, "tempo point {index} has invalid BPM")
            }
            TempoMapError::NonIncreasing { index } => {
                write!(f, "tempo point {index} is not later than its predecessor")
            }
            TempoMapError::InvalidRamp { index } => {
                write!(f, "tempo point {index} has an invalid ramp duration")
            }
            TempoMapError::OverlappingRamp { index } => {
                write!(f, "tempo point {index}'s ramp overlaps its predecessor")
            }
        }
    }
}

impl core::error::Error for TempoMapError {}
