//! `S2EdgeUtil`: edge crossings, wedges, and the rectangle bound of a chain
//! of edges.

use super::interval::{LatLngRect, S1Interval};
use super::point::{
    latitude, longitude, ordered_ccw, ortho, robust_ccw_with, robust_cross_prod, Point,
};
use super::{cmax, cmin};
use std::f64::consts::FRAC_PI_2;

/// `S2EdgeUtil::EdgeCrosser`: crossings of edge AB with a chain CD, DE...
pub struct EdgeCrosser {
    a: Point,
    b: Point,
    a_cross_b: Point,
    c: Point,
    acb: i32,
}

impl EdgeCrosser {
    pub fn new(a: Point, b: Point, c: Point) -> EdgeCrosser {
        let mut e = EdgeCrosser {
            a,
            b,
            a_cross_b: a.cross(b),
            c,
            acb: 0,
        };
        e.restart_at(c);
        e
    }

    pub fn restart_at(&mut self, c: Point) {
        self.c = c;
        self.acb = -robust_ccw_with(self.a, self.b, c, self.a_cross_b);
    }

    /// `RobustCrossing`: 1 a proper crossing, 0 a shared vertex, -1 none.
    pub fn robust_crossing(&mut self, d: Point) -> i32 {
        let bda = robust_ccw_with(self.a, self.b, d, self.a_cross_b);
        let result = if bda == -self.acb && bda != 0 {
            -1
        } else if (bda & self.acb) == 0 {
            0
        } else {
            self.robust_crossing_internal(d)
        };
        self.c = d;
        self.acb = -bda;
        result
    }

    fn robust_crossing_internal(&self, d: Point) -> i32 {
        let c_cross_d = self.c.cross(d);
        let cbd = -robust_ccw_with(self.c, d, self.b, c_cross_d);
        if cbd != self.acb {
            return -1;
        }
        let dac = robust_ccw_with(self.c, d, self.a, c_cross_d);
        if dac == self.acb {
            1
        } else {
            -1
        }
    }

    pub fn edge_or_vertex_crossing(&mut self, d: Point) -> bool {
        let c = self.c;
        let crossing = self.robust_crossing(d);
        if crossing < 0 {
            return false;
        }
        if crossing > 0 {
            return true;
        }
        vertex_crossing(self.a, self.b, c, d)
    }
}

fn simple_ccw(a: Point, b: Point, c: Point) -> bool {
    c.cross(a).dot(b) > 0.0
}

/// `S2EdgeUtil::GetDistance(x, a, b)`: the angle from `x` to the edge AB.
pub fn distance_to_edge(x: Point, a: Point, b: Point) -> f64 {
    let a_cross_b = robust_cross_prod(a, b);
    if simple_ccw(a_cross_b, a, x) && simple_ccw(x, b, a_cross_b) {
        let sin_dist = x.dot(a_cross_b).abs() / a_cross_b.norm();
        return cmin(1.0, sin_dist).asin();
    }
    let linear_dist2 = cmin((x - a).norm2(), (x - b).norm2());
    2.0 * cmin(1.0, 0.5 * linear_dist2.sqrt()).asin()
}

/// `S2EdgeUtil::GetClosestPoint(x, a, b)`.
pub fn closest_point(x: Point, a: Point, b: Point) -> Point {
    let a_cross_b = robust_cross_prod(a, b);
    let p = x - a_cross_b * (x.dot(a_cross_b) / a_cross_b.norm2());
    if simple_ccw(a_cross_b, a, p) && simple_ccw(p, b, a_cross_b) {
        return p.normalize();
    }
    if (x - a).norm2() <= (x - b).norm2() {
        a
    } else {
        b
    }
}

/// `S2EdgeUtil::VertexCrossing`.
pub fn vertex_crossing(a: Point, b: Point, c: Point, d: Point) -> bool {
    if a == b || c == d {
        return false;
    }
    if a == d {
        return ordered_ccw(ortho(a), c, b, a);
    }
    if b == c {
        return ordered_ccw(ortho(b), d, a, b);
    }
    if a == c {
        return ordered_ccw(ortho(a), d, b, a);
    }
    if b == d {
        return ordered_ccw(ortho(b), c, a, b);
    }
    false
}

/// `S2EdgeUtil::WedgeContains`.
pub fn wedge_contains(a0: Point, ab1: Point, a2: Point, b0: Point, b2: Point) -> bool {
    ordered_ccw(a2, b2, b0, ab1) && ordered_ccw(b0, a0, a2, ab1)
}

/// `S2EdgeUtil::WedgeIntersects`.
pub fn wedge_intersects(a0: Point, ab1: Point, a2: Point, b0: Point, b2: Point) -> bool {
    !(ordered_ccw(a0, b2, b0, ab1) && ordered_ccw(b0, a2, a0, ab1))
}

/// `S2EdgeUtil::RectBounder`.
pub struct RectBounder {
    a: Point,
    a_latlng: (f64, f64),
    pub bound: LatLngRect,
}

impl RectBounder {
    pub fn new() -> RectBounder {
        RectBounder {
            a: Point::new(0.0, 0.0, 0.0),
            a_latlng: (0.0, 0.0),
            bound: LatLngRect::empty(),
        }
    }

    pub fn add_point(&mut self, b: Point) {
        let b_latlng = (latitude(b), longitude(b));
        if self.bound.is_empty() {
            self.bound.add_latlng(b_latlng);
        } else {
            self.bound = self
                .bound
                .union(LatLngRect::from_point_pair(self.a_latlng, b_latlng));
            let a_cross_b = robust_cross_prod(self.a, b);
            let dir = a_cross_b.cross(Point::new(0.0, 0.0, 1.0));
            let da = dir.dot(self.a);
            let db = dir.dot(b);
            if da * db < 0.0 {
                let abs_lat = (a_cross_b.z() / a_cross_b.norm()).abs().acos();
                if da < 0.0 {
                    self.bound.lat.hi = cmax(abs_lat, self.bound.lat.hi);
                } else {
                    self.bound.lat.lo = cmin(-abs_lat, self.bound.lat.lo);
                }
                if abs_lat >= FRAC_PI_2 - 1e-15 {
                    self.bound.lng = S1Interval::full();
                }
            }
        }
        self.a = b;
        self.a_latlng = b_latlng;
    }
}

impl Default for RectBounder {
    fn default() -> Self {
        RectBounder::new()
    }
}
