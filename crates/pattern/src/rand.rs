//! Randomness that is a pure function of position in time.
//!
//! Nothing here draws from a global generator, and the crate deliberately does
//! not depend on `rand`. Two reasons, both load-bearing:
//!
//! * A query must be **idempotent over overlapping windows.** The scheduler
//!   queries ahead of the clock to fill the audio buffer, and the editor
//!   queries the same instant again to know what to highlight. If `degrade`
//!   consumed generator state, those two answers would differ and the
//!   highlight would drift away from the sound.
//! * The algorithm is spelled out rather than imported, so that a dependency
//!   improving its generator can never silently rewrite everybody's patterns.

use crate::frac::Frac;

/// Mix one structural identity word into a reproducible seed.
///
/// This is the finaliser from splitmix64. It is public so hosts deriving
/// identity for events that cannot be queried ahead—such as captured live
/// edges—use the same pinned algorithm as pattern-owned events.
pub fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A value in `[0, 1)` determined by an exact time and a seed.
///
/// Hashing the rational's numerator and denominator rather than a float keeps
/// the result exact: a third is a third however it was arrived at.
pub fn at(t: Frac, seed: u64) -> f64 {
    let h = mix(seed ^ mix(t.num() as u64).wrapping_add(mix(t.den() as u64).rotate_left(17)));
    // 53 bits is the whole mantissa; anything beyond it would not survive the
    // conversion anyway.
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// A value in `[0, 1)` for a whole cycle, so that per-cycle choices hold still
/// for the length of that cycle.
pub fn at_cycle(t: Frac, seed: u64) -> f64 {
    at(t.sam(), seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic() {
        let t = Frac::new(3, 8);
        assert_eq!(at(t, 0), at(t, 0));
        assert_ne!(at(t, 0), at(t, 1));
    }

    #[test]
    fn exact_time_beats_float_rounding() {
        let a = Frac::new(1, 3);
        let b = Frac::new(1, 3) + Frac::new(1, 3) - Frac::new(1, 3);
        assert_eq!(at(a, 7), at(b, 7));
    }

    #[test]
    fn stays_in_range_and_spreads() {
        let mut lo = 0;
        let mut hi = 0;
        for i in 0..2000 {
            let v = at(Frac::new(i, 97), 42);
            assert!((0.0..1.0).contains(&v), "{v} out of range");
            if v < 0.5 { lo += 1 } else { hi += 1 }
        }
        // Not a statistical test, just a smoke alarm for a constant function.
        assert!(lo > 800 && hi > 800, "lopsided: {lo}/{hi}");
    }
}
