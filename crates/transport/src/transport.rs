//! Constant-tempo cycle ↔ seconds mapping.
//!
//! A cycle is a pattern coordinate, not permanently a 4/4 bar. Four
//! quarter-note beats per cycle is the default meter; keeping it in transport
//! means a 3/4 cycle needs no change to the pattern algebra.

use apteronotus_pattern::{Frac, Span};

const APPROX_DENOMINATOR: i64 = 1_000_000;

/// A monotonic mapping between exact cycle coordinates and backend seconds.
pub trait CycleTime {
    fn cycle_to_seconds(&self, cycle: Frac) -> f64;
    fn span_to_seconds(&self, span: Span) -> f64;
    fn seconds_to_cycle(&self, seconds: f64) -> Result<Frac, TransportError>;
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transport {
    /// Quarter-note beats per minute.
    bpm: f64,
    /// Quarter-note beats represented by one pattern cycle.
    beats_per_cycle: f64,
}

impl Transport {
    /// A constant tempo with the default meter of four beats per cycle.
    pub fn new(bpm: f64) -> Result<Transport, TransportError> {
        Transport::with_meter(bpm, 4.0)
    }

    pub fn with_meter(bpm: f64, beats_per_cycle: f64) -> Result<Transport, TransportError> {
        if !bpm.is_finite() || bpm <= 0.0 {
            return Err(TransportError::InvalidBpm);
        }
        if !beats_per_cycle.is_finite() || beats_per_cycle <= 0.0 {
            return Err(TransportError::InvalidMeter);
        }
        Ok(Transport {
            bpm,
            beats_per_cycle,
        })
    }

    pub fn bpm(self) -> f64 {
        self.bpm
    }

    pub fn beats_per_cycle(self) -> f64 {
        self.beats_per_cycle
    }

    pub fn cycles_per_second(self) -> f64 {
        self.bpm / (60.0 * self.beats_per_cycle)
    }

    pub fn seconds_per_cycle(self) -> f64 {
        1.0 / self.cycles_per_second()
    }

    /// Convert an absolute pattern coordinate to sequencer time.
    pub fn cycle_to_seconds(self, cycle: Frac) -> f64 {
        cycle.to_f64() * self.seconds_per_cycle()
    }

    /// Convert a duration in cycles to seconds.
    pub fn duration_to_seconds(self, cycles: Frac) -> f64 {
        self.cycle_to_seconds(cycles)
    }

    pub fn span_to_seconds(self, span: Span) -> f64 {
        self.duration_to_seconds(span.length())
    }

    /// Convert wall/sequencer time back to a stable rational query boundary.
    ///
    /// cpal supplies floating-point seconds at the outer edge. The pattern
    /// algebra stays rational: this approximation happens once, here, and a
    /// scheduler's monotonic frontier ensures rounding can never make it
    /// revisit an already filled window.
    pub fn seconds_to_cycle(self, seconds: f64) -> Result<Frac, TransportError> {
        if !seconds.is_finite() {
            return Err(TransportError::InvalidSeconds);
        }
        Ok(Frac::approx(
            seconds * self.cycles_per_second(),
            APPROX_DENOMINATOR,
        ))
    }
}

impl CycleTime for Transport {
    fn cycle_to_seconds(&self, cycle: Frac) -> f64 {
        (*self).cycle_to_seconds(cycle)
    }

    fn span_to_seconds(&self, span: Span) -> f64 {
        (*self).span_to_seconds(span)
    }

    fn seconds_to_cycle(&self, seconds: f64) -> Result<Frac, TransportError> {
        (*self).seconds_to_cycle(seconds)
    }
}

impl Default for Transport {
    fn default() -> Self {
        // Constants are known-valid.
        Transport::new(120.0).unwrap()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportError {
    InvalidBpm,
    InvalidMeter,
    InvalidSeconds,
}

impl core::fmt::Display for TransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TransportError::InvalidBpm => write!(f, "tempo must be finite and greater than zero"),
            TransportError::InvalidMeter => {
                write!(f, "beats per cycle must be finite and greater than zero")
            }
            TransportError::InvalidSeconds => write!(f, "time in seconds must be finite"),
        }
    }
}

impl core::error::Error for TransportError {}
