//! `R1Interval`, `S1Interval` and `S2LatLngRect`.

use super::cap::Cap;
use super::point::{from_radians, latitude, longitude, Point};
use super::{cmax, cmin, remainder};
use std::f64::consts::{FRAC_PI_2, PI};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R1Interval {
    pub lo: f64,
    pub hi: f64,
}

impl R1Interval {
    pub fn new(lo: f64, hi: f64) -> R1Interval {
        R1Interval { lo, hi }
    }
    pub fn empty() -> R1Interval {
        R1Interval { lo: 1.0, hi: 0.0 }
    }
    pub fn from_point_pair(p1: f64, p2: f64) -> R1Interval {
        if p1 <= p2 {
            R1Interval::new(p1, p2)
        } else {
            R1Interval::new(p2, p1)
        }
    }
    pub fn bound(self, i: usize) -> f64 {
        if i == 0 {
            self.lo
        } else {
            self.hi
        }
    }
    pub fn is_empty(self) -> bool {
        self.lo > self.hi
    }
    pub fn center(self) -> f64 {
        0.5 * (self.lo + self.hi)
    }
    pub fn contains(self, p: f64) -> bool {
        p >= self.lo && p <= self.hi
    }
    pub fn contains_interval(self, y: R1Interval) -> bool {
        if y.is_empty() {
            return true;
        }
        y.lo >= self.lo && y.hi <= self.hi
    }
    pub fn intersects(self, y: R1Interval) -> bool {
        if self.lo <= y.lo {
            y.lo <= self.hi && y.lo <= y.hi
        } else {
            self.lo <= y.hi && self.lo <= self.hi
        }
    }
    pub fn expanded(self, radius: f64) -> R1Interval {
        if self.is_empty() {
            return self;
        }
        R1Interval::new(self.lo - radius, self.hi + radius)
    }
    pub fn union(self, y: R1Interval) -> R1Interval {
        if self.is_empty() {
            return y;
        }
        if y.is_empty() {
            return self;
        }
        R1Interval::new(cmin(self.lo, y.lo), cmax(self.hi, y.hi))
    }
    pub fn intersection(self, y: R1Interval) -> R1Interval {
        R1Interval::new(cmax(self.lo, y.lo), cmin(self.hi, y.hi))
    }
    fn same(self, y: R1Interval) -> bool {
        (self.lo == y.lo && self.hi == y.hi) || (self.is_empty() && y.is_empty())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct S1Interval {
    pub lo: f64,
    pub hi: f64,
}

fn positive_distance(a: f64, b: f64) -> f64 {
    let d = b - a;
    if d >= 0.0 {
        return d;
    }
    (b + PI) - (a - PI)
}

impl S1Interval {
    /// The checking constructor: `-Pi` becomes `Pi` unless the other end is
    /// `Pi` too.
    pub fn new(lo: f64, hi: f64) -> S1Interval {
        let mut s = S1Interval { lo, hi };
        if lo == -PI && hi != PI {
            s.lo = PI;
        }
        if hi == -PI && lo != PI {
            s.hi = PI;
        }
        s
    }
    /// The `ARGS_CHECKED` constructor.
    pub fn raw(lo: f64, hi: f64) -> S1Interval {
        S1Interval { lo, hi }
    }
    pub fn empty() -> S1Interval {
        S1Interval::raw(PI, -PI)
    }
    pub fn full() -> S1Interval {
        S1Interval::raw(-PI, PI)
    }
    pub fn bound(self, i: usize) -> f64 {
        if i == 0 {
            self.lo
        } else {
            self.hi
        }
    }
    pub fn is_full(self) -> bool {
        self.hi - self.lo == 2.0 * PI
    }
    pub fn is_empty(self) -> bool {
        self.lo - self.hi == 2.0 * PI
    }
    pub fn is_inverted(self) -> bool {
        self.lo > self.hi
    }
    pub fn center(self) -> f64 {
        let center = 0.5 * (self.lo + self.hi);
        if !self.is_inverted() {
            return center;
        }
        if center <= 0.0 {
            center + PI
        } else {
            center - PI
        }
    }
    pub fn length(self) -> f64 {
        let mut length = self.hi - self.lo;
        if length >= 0.0 {
            return length;
        }
        length += 2.0 * PI;
        if length > 0.0 {
            length
        } else {
            -1.0
        }
    }
    pub fn fast_contains(self, p: f64) -> bool {
        if self.is_inverted() {
            (p >= self.lo || p <= self.hi) && !self.is_empty()
        } else {
            p >= self.lo && p <= self.hi
        }
    }
    pub fn contains(self, mut p: f64) -> bool {
        if p == -PI {
            p = PI;
        }
        self.fast_contains(p)
    }
    pub fn contains_interval(self, y: S1Interval) -> bool {
        if self.is_inverted() {
            if y.is_inverted() {
                return y.lo >= self.lo && y.hi <= self.hi;
            }
            (y.lo >= self.lo || y.hi <= self.hi) && !self.is_empty()
        } else {
            if y.is_inverted() {
                return self.is_full() || y.is_empty();
            }
            y.lo >= self.lo && y.hi <= self.hi
        }
    }
    pub fn intersects(self, y: S1Interval) -> bool {
        if self.is_empty() || y.is_empty() {
            return false;
        }
        if self.is_inverted() {
            y.is_inverted() || y.lo <= self.hi || y.hi >= self.lo
        } else {
            if y.is_inverted() {
                return y.lo <= self.hi || y.hi >= self.lo;
            }
            y.lo <= self.hi && y.hi >= self.lo
        }
    }
    pub fn add_point(&mut self, mut p: f64) {
        if p == -PI {
            p = PI;
        }
        if self.fast_contains(p) {
            return;
        }
        if self.is_empty() {
            self.hi = p;
            self.lo = p;
        } else {
            let dlo = positive_distance(p, self.lo);
            let dhi = positive_distance(self.hi, p);
            if dlo < dhi {
                self.lo = p;
            } else {
                self.hi = p;
            }
        }
    }
    pub fn from_point_pair(mut p1: f64, mut p2: f64) -> S1Interval {
        if p1 == -PI {
            p1 = PI;
        }
        if p2 == -PI {
            p2 = PI;
        }
        if positive_distance(p1, p2) <= PI {
            S1Interval::raw(p1, p2)
        } else {
            S1Interval::raw(p2, p1)
        }
    }
    pub fn expanded(self, radius: f64) -> S1Interval {
        if self.is_empty() {
            return self;
        }
        if 2.0f64.mul_add(radius, self.length()) >= 2.0 * PI - 1e-15 {
            return S1Interval::full();
        }
        let mut result = S1Interval::new(
            remainder(self.lo - radius, 2.0 * PI),
            remainder(self.hi + radius, 2.0 * PI),
        );
        if result.lo <= -PI {
            result.lo = PI;
        }
        result
    }
    pub fn union(self, y: S1Interval) -> S1Interval {
        if y.is_empty() {
            return self;
        }
        if self.fast_contains(y.lo) {
            if self.fast_contains(y.hi) {
                if self.contains_interval(y) {
                    return self;
                }
                return S1Interval::full();
            }
            return S1Interval::raw(self.lo, y.hi);
        }
        if self.fast_contains(y.hi) {
            return S1Interval::raw(y.lo, self.hi);
        }
        if self.is_empty() || y.fast_contains(self.lo) {
            return y;
        }
        let dlo = positive_distance(y.hi, self.lo);
        let dhi = positive_distance(self.hi, y.lo);
        if dlo < dhi {
            S1Interval::raw(y.lo, self.hi)
        } else {
            S1Interval::raw(self.lo, y.hi)
        }
    }
    pub fn intersection(self, y: S1Interval) -> S1Interval {
        if y.is_empty() {
            return S1Interval::empty();
        }
        if self.fast_contains(y.lo) {
            if self.fast_contains(y.hi) {
                if y.length() < self.length() {
                    return y;
                }
                return self;
            }
            return S1Interval::raw(y.lo, self.hi);
        }
        if self.fast_contains(y.hi) {
            return S1Interval::raw(self.lo, y.hi);
        }
        if y.fast_contains(self.lo) {
            return self;
        }
        S1Interval::empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatLngRect {
    pub lat: R1Interval,
    pub lng: S1Interval,
}

impl LatLngRect {
    pub fn new(lat: R1Interval, lng: S1Interval) -> LatLngRect {
        LatLngRect { lat, lng }
    }
    pub fn empty() -> LatLngRect {
        LatLngRect::new(R1Interval::empty(), S1Interval::empty())
    }
    pub fn full_lat() -> R1Interval {
        R1Interval::new(-FRAC_PI_2, FRAC_PI_2)
    }
    pub fn full() -> LatLngRect {
        LatLngRect::new(LatLngRect::full_lat(), S1Interval::full())
    }
    /// `S2LatLngRect(lo, hi)` of two `S2LatLng`s, the checking constructor.
    pub fn from_lo_hi(lo: (f64, f64), hi: (f64, f64)) -> LatLngRect {
        LatLngRect::new(R1Interval::new(lo.0, hi.0), S1Interval::new(lo.1, hi.1))
    }
    /// `FromPointPair` of two `(lat, lng)` in radians.
    pub fn from_point_pair(p1: (f64, f64), p2: (f64, f64)) -> LatLngRect {
        LatLngRect::new(
            R1Interval::from_point_pair(p1.0, p2.0),
            S1Interval::from_point_pair(p1.1, p2.1),
        )
    }
    pub fn is_empty(self) -> bool {
        self.lat.is_empty()
    }
    pub fn is_full(self) -> bool {
        self.lat.same(LatLngRect::full_lat()) && self.lng.is_full()
    }
    /// `GetVertex(k)` as `(lat, lng)`.
    pub fn vertex(self, k: usize) -> (f64, f64) {
        (self.lat.bound(k >> 1), self.lng.bound((k >> 1) ^ (k & 1)))
    }
    pub fn center(self) -> (f64, f64) {
        (self.lat.center(), self.lng.center())
    }
    pub fn contains_latlng(self, ll: (f64, f64)) -> bool {
        self.lat.contains(ll.0) && self.lng.contains(ll.1)
    }
    /// `Contains(S2Point)`.
    pub fn contains_point(self, p: Point) -> bool {
        self.contains_latlng((latitude(p), longitude(p)))
    }
    pub fn contains(self, other: LatLngRect) -> bool {
        self.lat.contains_interval(other.lat) && self.lng.contains_interval(other.lng)
    }
    pub fn intersects(self, other: LatLngRect) -> bool {
        self.lat.intersects(other.lat) && self.lng.intersects(other.lng)
    }
    pub fn add_latlng(&mut self, ll: (f64, f64)) {
        // R1Interval::AddPoint.
        if self.lat.is_empty() {
            self.lat = R1Interval::new(ll.0, ll.0);
        } else if ll.0 < self.lat.lo {
            self.lat.lo = ll.0;
        } else if ll.0 > self.lat.hi {
            self.lat.hi = ll.0;
        }
        self.lng.add_point(ll.1);
    }
    pub fn union(self, other: LatLngRect) -> LatLngRect {
        LatLngRect::new(self.lat.union(other.lat), self.lng.union(other.lng))
    }
    pub fn intersection(self, other: LatLngRect) -> LatLngRect {
        let lat = self.lat.intersection(other.lat);
        let lng = self.lng.intersection(other.lng);
        if lat.is_empty() || lng.is_empty() {
            return LatLngRect::empty();
        }
        LatLngRect::new(lat, lng)
    }
    pub fn cap_bound(self) -> Cap {
        if self.is_empty() {
            return Cap::empty();
        }
        let (pole_z, pole_angle) = if self.lat.lo + self.lat.hi < 0.0 {
            (-1.0, FRAC_PI_2 + self.lat.hi)
        } else {
            (1.0, FRAC_PI_2 - self.lat.lo)
        };
        let pole_cap = Cap::from_axis_angle(Point::new(0.0, 0.0, pole_z), pole_angle);
        let lng_span = self.lng.hi - self.lng.lo;
        if remainder(lng_span, 2.0 * PI) >= 0.0 && lng_span < 2.0 * PI {
            let c = self.center();
            let mut mid_cap = Cap::from_axis_angle(from_radians(c.0, c.1), 0.0);
            for k in 0..4 {
                let v = self.vertex(k);
                mid_cap.add_point(from_radians(v.0, v.1));
            }
            if mid_cap.height < pole_cap.height {
                return mid_cap;
            }
        }
        pole_cap
    }
}
