//! Exact cycle time.
//!
//! Cycle positions are rationals, not floats, and deliberately so. A triplet
//! divides a cycle into thirds and a quintuplet into fifths; in `f64` those
//! boundaries stop meeting after a few operations, and events that should abut
//! start overlapping or leaving gaps. Every span boundary in this crate is a
//! `Frac`, and `f64` appears only at the very edge, where the scheduler
//! converts cycles into seconds.

use core::cmp::Ordering;
use core::fmt;
use core::ops::{Add, Div, Mul, Neg, Sub};

/// A rational, always normalised: `den > 0` and `gcd(|num|, den) == 1`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frac {
    n: i64,
    d: i64,
}

impl Frac {
    pub const ZERO: Frac = Frac { n: 0, d: 1 };
    pub const ONE: Frac = Frac { n: 1, d: 1 };

    /// Construct and normalise.
    ///
    /// Goes through [`reduce`], so `i64::MIN` in either position — which would
    /// overflow a naive `abs()` or negation — is handled rather than wrapped.
    pub fn new(n: i64, d: i64) -> Frac {
        reduce(n as i128, d as i128)
    }

    /// Normalise untrusted input without panicking on a zero denominator or
    /// a reduced value that cannot fit in the 64-bit representation.
    pub fn checked_new(n: i64, d: i64) -> Option<Frac> {
        checked_reduce(n as i128, d as i128)
    }

    /// Add untrusted coordinates, returning `None` if the reduced result
    /// exceeds this representation. Ordinary arithmetic retains its invariant
    /// checks for callers that have already established their bounds.
    pub fn checked_add(self, other: Frac) -> Option<Frac> {
        checked_reduce(
            self.n as i128 * other.d as i128 + other.n as i128 * self.d as i128,
            self.d as i128 * other.d as i128,
        )
    }

    /// Subtract untrusted coordinates without overflowing their representation.
    pub fn checked_sub(self, other: Frac) -> Option<Frac> {
        checked_reduce(
            self.n as i128 * other.d as i128 - other.n as i128 * self.d as i128,
            self.d as i128 * other.d as i128,
        )
    }

    pub const fn int(n: i64) -> Frac {
        Frac { n, d: 1 }
    }

    pub fn num(self) -> i64 {
        self.n
    }

    pub fn den(self) -> i64 {
        self.d
    }

    pub fn to_f64(self) -> f64 {
        self.n as f64 / self.d as f64
    }

    /// Nearest rational to `x` with denominator at most `limit`.
    pub fn approx(x: f64, limit: i64) -> Frac {
        let rounded = x.round() as i64;
        // Preserve the old total behavior outside the useful approximation
        // domain. All language callers reject non-finite values before here.
        if !x.is_finite() || limit < 1 {
            return Frac::int(rounded);
        }

        let negative = x.is_sign_negative();
        let magnitude = x.abs();
        if magnitude > i64::MAX as f64 {
            return Frac::int(rounded);
        }
        // The historical contract compares errors after f64 division and keeps
        // the first (smallest-denominator) tie. At very large magnitudes, two
        // distinct bounded rationals can round to the same f64. Continued
        // fractions cannot observe that artificial tie, so retain the scan
        // only in the range where f64 spacing is wide enough for it to occur.
        if denominator_rounding_can_alias(magnitude, limit) {
            return denominator_scan(x, limit);
        }
        let x = magnitude;

        // Consecutive continued-fraction convergents. Once the next convergent
        // exceeds the denominator budget, the optimum is either the previous
        // convergent or the furthest admissible semiconvergent between them.
        // This is logarithmic in `limit`; the former implementation inspected
        // every denominator independently.
        let limit = limit as i128;
        let (mut p0, mut q0) = (0_i128, 1_i128);
        let (mut p1, mut q1) = (1_i128, 0_i128);
        let mut remainder = x;
        let max_numerator = i64::MAX as i128;
        loop {
            let coefficient = remainder.floor() as i128;
            let denominator_scale = if q1 == 0 {
                i128::MAX
            } else {
                (limit - q0) / q1
            };
            let numerator_scale = if p1 == 0 {
                i128::MAX
            } else {
                (max_numerator - p0) / p1
            };
            if coefficient > denominator_scale.min(numerator_scale) {
                break;
            }
            let q2 = q0 + coefficient * q1;
            let p2 = p0 + coefficient * p1;
            (p0, q0, p1, q1) = (p1, q1, p2, q2);

            let fractional = remainder - coefficient as f64;
            if fractional == 0.0 {
                return signed_frac(p1, q1, negative);
            }
            remainder = fractional.recip();
        }

        let denominator_scale = (limit - q0) / q1;
        let numerator_scale = if p1 == 0 {
            i128::MAX
        } else {
            (max_numerator - p0) / p1
        };
        let scale = denominator_scale.min(numerator_scale);
        let semiconvergent = (p0 + scale * p1, q0 + scale * q1);
        let convergent = (p1, q1);
        let best = closer_candidate(x, semiconvergent, convergent);
        signed_frac(best.0, best.1, negative)
    }

    /// Largest integer not greater than `self`. Tidal calls this the *sam*.
    pub fn floor(self) -> i64 {
        self.n.div_euclid(self.d)
    }

    /// The start of the cycle containing `self`.
    pub fn sam(self) -> Frac {
        Frac::int(self.floor())
    }

    /// Position within the cycle, in `[0, 1)`.
    pub fn cycle_pos(self) -> Frac {
        self - self.sam()
    }

    pub fn min(self, other: Frac) -> Frac {
        if self <= other { self } else { other }
    }

    pub fn max(self, other: Frac) -> Frac {
        if self >= other { self } else { other }
    }

    pub fn recip(self) -> Frac {
        Frac::new(self.d, self.n)
    }

    pub fn is_zero(self) -> bool {
        self.n == 0
    }

    pub fn is_negative(self) -> bool {
        self.n < 0
    }
}

fn closer_candidate(x: f64, left: (i128, i128), right: (i128, i128)) -> (i128, i128) {
    let left_error = (left.0 as f64 / left.1 as f64 - x).abs();
    let right_error = (right.0 as f64 / right.1 as f64 - x).abs();
    match left_error.total_cmp(&right_error) {
        Ordering::Less => left,
        Ordering::Greater => right,
        Ordering::Equal => {
            // The exhaustive implementation visited denominators in ascending
            // order. If both candidates occur at the same denominator, f64
            // round() breaks a midpoint away from zero.
            if left.1 < right.1 || (left.1 == right.1 && left.0 > right.0) {
                left
            } else {
                right
            }
        }
    }
}

fn signed_frac(n: i128, d: i128, negative: bool) -> Frac {
    reduce(if negative { -n } else { n }, d)
}

fn denominator_rounding_can_alias(x: f64, limit: i64) -> bool {
    if limit <= 1 {
        return false;
    }
    let spacing = f64::from_bits(x.to_bits() + 1) - x;
    let closest_distinct_rationals = 1.0 / (limit as f64 * (limit.saturating_sub(1)) as f64);
    spacing * 2.0 >= closest_distinct_rationals
}

fn denominator_scan(x: f64, limit: i64) -> Frac {
    let mut best = Frac::int(x.round() as i64);
    let mut best_err = (best.to_f64() - x).abs();
    for d in 1..=limit {
        let n = (x * d as f64).round() as i64;
        let err = (n as f64 / d as f64 - x).abs();
        if err < best_err {
            best = Frac::new(n, d);
            best_err = err;
        }
    }
    best
}

impl Ord for Frac {
    fn cmp(&self, other: &Frac) -> Ordering {
        // i128 so that comparing two long-denominator fractions cannot wrap.
        (self.n as i128 * other.d as i128).cmp(&(other.n as i128 * self.d as i128))
    }
}

impl PartialOrd for Frac {
    fn partial_cmp(&self, other: &Frac) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Reduce a 128-bit ratio back into a `Frac`.
///
/// Every operation on `Frac` funnels through here, in `i128`, so nothing can
/// silently wrap: a result too large for a 64-bit rational is a panic with a
/// message rather than a quietly wrong cycle position.
///
/// That panic is an internal invariant, not an input check. `Frac` is total
/// over musically sized values, and it is the *parser's* job to keep user text
/// inside them — see the limits in [`crate::mini`]. Nothing a person can type
/// may reach this assertion.
fn reduce(n: i128, d: i128) -> Frac {
    assert!(d != 0, "Frac with zero denominator");
    checked_reduce(n, d)
        .unwrap_or_else(|| panic!("cycle time overflowed a 64-bit rational: {n}/{d}"))
}

fn checked_reduce(n: i128, d: i128) -> Option<Frac> {
    if d == 0 {
        return None;
    }
    let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
    let mut a = n.abs();
    let mut b = d;
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    let g = if a == 0 { 1 } else { a };
    let (n, d) = (n / g, d / g);
    Some(Frac {
        n: i64::try_from(n).ok()?,
        d: i64::try_from(d).ok()?,
    })
}

impl Add for Frac {
    type Output = Frac;
    fn add(self, o: Frac) -> Frac {
        reduce(
            self.n as i128 * o.d as i128 + o.n as i128 * self.d as i128,
            self.d as i128 * o.d as i128,
        )
    }
}

impl Sub for Frac {
    type Output = Frac;
    fn sub(self, o: Frac) -> Frac {
        reduce(
            self.n as i128 * o.d as i128 - o.n as i128 * self.d as i128,
            self.d as i128 * o.d as i128,
        )
    }
}

impl Mul for Frac {
    type Output = Frac;
    fn mul(self, o: Frac) -> Frac {
        reduce(self.n as i128 * o.n as i128, self.d as i128 * o.d as i128)
    }
}

impl Div for Frac {
    type Output = Frac;
    fn div(self, o: Frac) -> Frac {
        assert!(!o.is_zero(), "division by zero cycle time");
        reduce(self.n as i128 * o.d as i128, self.d as i128 * o.n as i128)
    }
}

impl Neg for Frac {
    type Output = Frac;
    fn neg(self) -> Frac {
        // Through `reduce` rather than `-self.n`, which overflows at i64::MIN.
        reduce(-(self.n as i128), self.d as i128)
    }
}

impl From<i64> for Frac {
    fn from(n: i64) -> Frac {
        Frac::int(n)
    }
}

impl fmt::Debug for Frac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.d == 1 {
            write!(f, "{}", self.n)
        } else {
            write!(f, "{}/{}", self.n, self.d)
        }
    }
}

impl fmt::Display for Frac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exhaustive_approx(x: f64, limit: i64) -> Frac {
        let mut best = Frac::int(x.round() as i64);
        let mut best_err = (best.to_f64() - x).abs();
        for d in 1..=limit {
            let n = (x * d as f64).round() as i64;
            let err = (n as f64 / d as f64 - x).abs();
            if err < best_err {
                best = Frac::new(n, d);
                best_err = err;
            }
        }
        best
    }

    #[test]
    fn thirds_are_exact() {
        let third = Frac::new(1, 3);
        assert_eq!(third + third + third, Frac::ONE);
    }

    #[test]
    fn normalises() {
        assert_eq!(Frac::new(2, 4), Frac::new(1, 2));
        assert_eq!(Frac::new(1, -2), Frac::new(-1, 2));
        assert_eq!(Frac::new(0, 7), Frac::ZERO);
    }

    #[test]
    fn checked_construction_reduces_before_checking_representation() {
        assert_eq!(Frac::checked_new(1, 0), None);
        assert_eq!(Frac::checked_new(i64::MIN, -1), None);
        assert_eq!(Frac::checked_new(1, i64::MIN), None);
        assert_eq!(Frac::checked_new(i64::MIN, i64::MIN), Some(Frac::ONE));
        assert_eq!(Frac::checked_new(0, i64::MIN), Some(Frac::ZERO));
        assert_eq!(
            Frac::checked_new(2, i64::MIN),
            Some(Frac::new(-1, 1_i64 << 62))
        );
        assert_eq!(Frac::checked_new(i64::MIN, 1), Some(Frac::int(i64::MIN)));
    }

    #[test]
    fn checked_coordinate_arithmetic_never_wraps() {
        assert_eq!(Frac::int(i64::MAX).checked_add(Frac::ONE), None);
        assert_eq!(Frac::int(i64::MIN).checked_sub(Frac::ONE), None);
        assert_eq!(
            Frac::int(i64::MIN).checked_sub(Frac::int(i64::MIN)),
            Some(Frac::ZERO)
        );
        assert_eq!(
            Frac::new(1, 3).checked_add(Frac::new(2, 3)),
            Some(Frac::ONE)
        );
        assert_eq!(
            Frac::new(7, 8).checked_sub(Frac::new(1, 8)),
            Some(Frac::new(3, 4))
        );
    }

    #[test]
    fn floor_is_euclidean() {
        assert_eq!(Frac::new(3, 2).floor(), 1);
        assert_eq!(Frac::new(-1, 2).floor(), -1);
        assert_eq!(Frac::int(-2).floor(), -2);
        assert_eq!(Frac::new(-1, 2).cycle_pos(), Frac::new(1, 2));
    }

    #[test]
    fn ordering_survives_large_denominators() {
        let a = Frac::new(1, 3_000_000_000);
        let b = Frac::new(1, 3_000_000_001);
        assert!(a > b);
    }

    #[test]
    fn survives_the_extremes() {
        // i64::MIN has no positive counterpart, so a naive abs() or negation
        // in normalisation would overflow.
        assert_eq!(Frac::int(i64::MIN).num(), i64::MIN);
        assert_eq!(Frac::new(i64::MIN, 2), Frac::int(i64::MIN / 2));
        assert_eq!(Frac::new(i64::MIN, i64::MIN), Frac::ONE);
        assert_eq!(Frac::new(0, i64::MIN), Frac::ZERO);
        assert_eq!(-Frac::int(i64::MAX), Frac::int(-i64::MAX));
        assert_eq!(Frac::int(i64::MIN).floor(), i64::MIN);
    }

    #[test]
    #[should_panic(expected = "overflowed")]
    fn a_result_too_large_to_represent_is_loud() {
        // The documented invariant: `Frac` is total over musically sized
        // values and says so rather than wrapping. The parser's limits are
        // what keep user text from ever reaching this.
        let _ = Frac::new(1, i64::MIN);
    }

    #[test]
    fn approx_finds_simple_ratios() {
        assert_eq!(Frac::approx(0.333_333_333, 16), Frac::new(1, 3));
        assert_eq!(Frac::approx(0.75, 16), Frac::new(3, 4));
    }

    #[test]
    fn continued_fraction_approx_matches_the_exhaustive_definition() {
        for limit in 1..=64 {
            for numerator in -256..=256 {
                let x = numerator as f64 / 64.0;
                assert_eq!(
                    Frac::approx(x, limit),
                    exhaustive_approx(x, limit),
                    "x={x}, limit={limit}"
                );
            }
        }
        for x in [
            9_223_372_036_854.775,
            -9_223_372_036_854.775,
            1_000_000_000_000.125,
        ] {
            for limit in [1, 2, 10, 1_000, 1_000_000] {
                assert_eq!(
                    Frac::approx(x, limit),
                    exhaustive_approx(x, limit),
                    "x={x}, limit={limit}"
                );
            }
        }

        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..20_000 {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut bits = state;
            bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            bits ^= bits >> 31;
            let unit = (bits >> 11) as f64 / (1_u64 << 53) as f64;
            let scale = [1.0, 1_000.0, 1_000_000.0][(bits % 3) as usize];
            let x = (unit * 2.0 - 1.0) * 64.0 * scale;
            let limit = (bits % 1_000 + 1) as i64;
            assert_eq!(
                Frac::approx(x, limit),
                exhaustive_approx(x, limit),
                "x={x}, limit={limit}"
            );
        }
    }
}
