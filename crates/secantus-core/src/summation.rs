//! mongod's `DoubleDoubleSummation` (`src/mongo/util/summation.{h,cpp}`,
//! 8.2), transcribed: the compensated sum behind `$sum`, `$avg` and the
//! window functions built on them.
//!
//! It keeps an unevaluated pair, a rounded `sum` and an `addend`, updated by
//! Fast2Sum then 2Sum. The answer is `sum` alone, NOT `sum + addend`, and
//! that is not the correctly rounded total: for 0.1, 0.2, 0.3, 1e16, 1.0,
//! -1e16 mongod answers 2.0 where a plain sum answers 0.0 and the exact sum
//! is 1.6. A different compensated algorithm (Neumaier's, or a normalised
//! double-double) answers differently again, so the steps stay as written.

use crate::decimal::{self, Dec};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DoubleDoubleSum {
    sum: f64,
    addend: f64,
    /// A plain sum, answered when `sum` is NaN, so that infinities of one
    /// sign do not become NaN through the compensation.
    special: f64,
}

/// Fast2Sum: exact when `|a| >= |b|`.
fn fast_2sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let z = s - a;
    (s, b - z)
}

/// 2Sum (Møller, Knuth): exact for any `a`, `b`.
fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let a_prime = s - b;
    let b_prime = s - a_prime;
    (s, (a - a_prime) + (b - b_prime))
}

impl DoubleDoubleSum {
    pub(crate) fn add_double(&mut self, x: f64) {
        self.special += x;
        let (x, addend) = fast_2sum(x, self.addend);
        self.addend = addend;
        let (sum, x) = two_sum(self.sum, x);
        self.sum = sum;
        self.addend += x;
    }

    /// Split into two doubles first, so the sum stays exact (to 2^106).
    pub(crate) fn add_long(&mut self, x: i64) {
        // C++ `/` truncates toward zero, as Rust's does.
        let high = x / (1i64 << 32) * (1i64 << 32);
        let low = x - high;
        self.add_double(low as f64);
        self.add_double(high as f64);
    }

    pub(crate) fn add_int(&mut self, x: i32) {
        self.add_double(f64::from(x));
    }

    pub(crate) fn get_double(&self) -> f64 {
        if self.sum.is_nan() {
            self.special
        } else {
            self.sum
        }
    }

    pub(crate) fn fits_long(&self) -> bool {
        let min = i64::MIN as f64;
        let max = i64::MAX as f64; // rounds up to 2^63
        if self.sum > min && self.sum < max {
            return true;
        }
        if self.sum == max {
            return self.addend < -0.5;
        }
        if self.sum == min {
            return self.addend >= -0.5;
        }
        false
    }

    /// The sum rounded to the nearest integer, halves away from zero. Only
    /// meaningful when [`fits_long`](Self::fits_long).
    pub(crate) fn get_long(&self) -> i64 {
        if self.sum == i64::MAX as f64 {
            return (self.addend.round() as i64).wrapping_sub(i64::MIN);
        }
        // `llround`: halves away from zero, as `f64::round` does.
        let sum = self.sum.round() as i64;
        sum.wrapping_add(((self.sum - sum as f64) + self.addend).round() as i64)
    }

    /// `Decimal128(sum, kRoundTo34Digits) + Decimal128(addend, ...)`, or the
    /// special sum when `sum` is not finite.
    pub(crate) fn get_decimal(&self) -> Option<Dec> {
        if !self.sum.is_finite() {
            return decimal::from_bson_accumulator(&bson::Bson::Double(self.special));
        }
        let s = decimal::from_bson_accumulator(&bson::Bson::Double(self.sum))?;
        let a = decimal::from_bson_accumulator(&bson::Bson::Double(self.addend))?;
        decimal::add(&s, &a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum_of(xs: &[f64]) -> f64 {
        let mut s = DoubleDoubleSum::default();
        for &x in xs {
            s.add_double(x);
        }
        s.get_double()
    }

    #[test]
    fn cancellation_answers_what_mongod_answers() {
        // mongod 8.2.11: `$sum` of these is 2.0.
        assert_eq!(sum_of(&[0.1, 0.2, 0.3, 1e16, 1.0, -1e16]), 2.0);
        // ... and `{$sum: [0.1, 0.2, 0.3]}` is 0.6, not 0.6000000000000001.
        assert_eq!(sum_of(&[0.1, 0.2, 0.3]), 0.6);
    }

    #[test]
    fn infinities_do_not_become_nan() {
        assert_eq!(sum_of(&[f64::INFINITY, 1.0]), f64::INFINITY);
        assert!(sum_of(&[f64::INFINITY, f64::NEG_INFINITY]).is_nan());
    }

    #[test]
    fn longs_stay_exact() {
        let mut s = DoubleDoubleSum::default();
        s.add_long(i64::MAX);
        s.add_long(-1);
        assert!(s.fits_long());
        assert_eq!(s.get_long(), i64::MAX - 1);
        s.add_long(2);
        assert!(!s.fits_long());
        let mut m = DoubleDoubleSum::default();
        m.add_long(i64::MIN);
        assert!(m.fits_long());
        assert_eq!(m.get_long(), i64::MIN);
    }
}
