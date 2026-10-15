//! The S2 geometry mongod 8.2 indexes and queries `2dsphere` with.
//!
//! mongod ships the original C++ S2 library (`src/third_party/s2`, the 2011
//! release), not the newer s2geometry the `s2` crate ports from Go. The two
//! cover regions differently: the newer coverer starts from another set of
//! candidate cells and merges its result, so the cells differ, and so do the
//! keys an index holds and the order an index scan returns documents in.
//! This module is a transcription of the parts mongod reaches for a
//! `2dsphere` index: cell ids, cells, caps, latitude-longitude rectangles,
//! loops, polygons, polylines, the region coverer and cell unions. Each
//! function follows its C++ original statement for statement, with the same
//! floating-point operations in the same order, because a covering is a
//! chain of predicates and one flipped comparison changes it.
//!
//! What is not transcribed: anything mongod does not call for a covering
//! (areas, centroids, polygon boolean operations, the edge index, which only
//! speeds up a test whose answer is the same without it).

mod cap;
mod cell;
mod cellid;
mod coverer;
mod edge;
mod interval;
mod loops;
mod point;
mod region;

pub use cap::Cap;
pub use cell::Cell;
pub use cellid::CellId;
pub use coverer::{
    cell_union_difference, normalize, union_contains, union_intersects, RegionCoverer,
};
pub use interval::{LatLngRect, R1Interval, S1Interval};
pub use loops::{Loop, Polygon};
pub use point::{from_radians as point_from_radians, latlng_point, Point};
pub use region::{Polyline, Region};

/// `S2::kMaxCellLevel`.
pub const MAX_LEVEL: i32 = 30;

/// The IEEE remainder of `x / y` (C's `remainder`): `x - n * y` for the
/// integer `n` nearest `x / y`, ties to even. Exact, as the C function is.
pub fn remainder(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() || x.is_infinite() || y == 0.0 {
        return f64::NAN;
    }
    if y.is_infinite() {
        return x;
    }
    let y = y.abs();
    // `x % (2y)` is exact and keeps the sign of x; it says whether the
    // truncated quotient of x / y is odd.
    let mut r = x % (2.0 * y);
    let sign = if r < 0.0 { -1.0 } else { 1.0 };
    r = r.abs();
    if r >= y {
        r -= y;
        // The quotient so far is odd: a tie rounds up to the even one.
        if r >= 0.5 * y {
            r -= y;
        }
    } else if r > 0.5 * y {
        r -= y;
    }
    let out = sign * r;
    if out == 0.0 {
        // C keeps the sign of x on a zero result.
        return 0.0f64.copysign(x);
    }
    out
}

/// `std::min` of two doubles: the first unless the second is less (so a
/// signed zero keeps the sign `std::min` gives it, which `f64::min` does not
/// promise).
pub fn cmin(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// `std::max` of two doubles.
pub fn cmax(a: f64, b: f64) -> f64 {
    if a < b {
        b
    } else {
        a
    }
}

/// `S2::Metric<1>` (`kMinWidth`, `kAvgEdge`...) for the quadratic projection.
#[derive(Clone, Copy)]
pub struct Metric(pub f64);

/// `S2::kMinWidth`.
pub const MIN_WIDTH: Metric = Metric(0.942_809_041_582_063_4); // 2 * sqrt(2) / 3
/// `S2::kAvgEdge`.
pub const AVG_EDGE: Metric = Metric(1.459_213_746_386_106_1); // 1.459213746386106062

impl Metric {
    /// `GetValue`: the metric at a level, `ldexp(deriv, -level)`.
    pub fn value(self, level: i32) -> f64 {
        self.0 * 2f64.powi(-level)
    }

    /// `GetMinLevel`: the least level whose value is at most `value`.
    pub fn min_level(self, value: f64) -> i32 {
        if value <= 0.0 {
            return MAX_LEVEL;
        }
        let level = frexp_exp(value / self.0);
        (-(level - 1)).clamp(0, MAX_LEVEL)
    }

    /// `GetMaxLevel`: the greatest level whose value is at least `value`.
    pub fn max_level(self, value: f64) -> i32 {
        if value <= 0.0 {
            return MAX_LEVEL;
        }
        let level = frexp_exp(self.0 / value);
        (level - 1).clamp(0, MAX_LEVEL)
    }

    /// `GetClosestLevel`.
    pub fn closest_level(self, value: f64) -> i32 {
        self.min_level(std::f64::consts::SQRT_2 * value)
    }
}

/// The exponent `frexp` returns: `x = m * 2^e` with `0.5 <= |m| < 1`.
fn frexp_exp(x: f64) -> i32 {
    if x == 0.0 || !x.is_finite() {
        return 0;
    }
    let bits = x.abs().to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // Subnormal: normalise first.
        return frexp_exp(x * 2f64.powi(54)) - 54;
    }
    exp - 1022
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remainder_matches_c() {
        let two_pi = 2.0 * std::f64::consts::PI;
        assert_eq!(remainder(5.0, 2.0), 1.0);
        assert_eq!(remainder(3.0, 2.0), -1.0);
        assert_eq!(remainder(-3.0, 2.0), 1.0);
        assert_eq!(remainder(1.0, 2.0), 1.0);
        assert_eq!(remainder(7.0, 2.0), -1.0);
        assert_eq!(remainder(4.0, two_pi), 4.0 - two_pi);
        assert_eq!(remainder(-4.0, two_pi), two_pi - 4.0);
        assert_eq!(remainder(1.0, two_pi), 1.0);
        assert!(remainder(-0.0, 2.0).is_sign_negative());
    }

    #[test]
    fn metric_levels() {
        let radius = 6_378_100.0;
        // mongod's 2dsphere v3 defaults.
        assert_eq!(AVG_EDGE.closest_level(110.0 / radius), 16);
        assert_eq!(AVG_EDGE.closest_level(2_000_000.0 / radius), 2);
        assert_eq!(MIN_WIDTH.max_level(0.0), MAX_LEVEL);
    }
}
