//! `S2Point` (`Vector3<double>`), the face projections of `s2.h`, and the
//! orientation predicates of `s2.cc`.
//!
//! Fused multiply-adds. clang (`-ffp-contract=on`, its default) fuses a
//! product and a sum written in one source expression into one `fma`,
//! and the reference mongod (8.2.11 on arm64) is built that way: a
//! dot product `a*b + c*d + e*f` is `fma(e, f, fma(a, b, c*d))`, a cross
//! product's `a*b - c*d` is `fma(a, b, -(c*d))`, and so on, the left-hand
//! product fused when both sides are products. The rounding is not the
//! same as the unfused arithmetic, and where a result should be exactly
//! zero (a point on a line through the centre) only the fused one gives
//! mongod's answer: mongod measures 6.42567661e-12 metres from a point to a
//! line that starts there, the error term of one fused product. So every
//! expression here is fused where clang fuses its C++ original, and only
//! there.

use std::ops::{Add, Mul, Neg, Sub};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point(pub [f64; 3]);

impl Point {
    pub const fn new(x: f64, y: f64, z: f64) -> Point {
        Point([x, y, z])
    }

    pub fn x(self) -> f64 {
        self.0[0]
    }
    pub fn y(self) -> f64 {
        self.0[1]
    }
    pub fn z(self) -> f64 {
        self.0[2]
    }

    /// `DotProd`: `c0*v0 + c1*v1 + c2*v2`, fused as clang fuses it (see
    /// the module notes).
    pub fn dot(self, o: Point) -> f64 {
        let (a, b) = (self.0, o.0);
        a[2].mul_add(b[2], a[0].mul_add(b[0], a[1] * b[1]))
    }

    /// `CrossProd`: each `x*y - z*w` fused as `fma(x, y, -(z*w))`.
    pub fn cross(self, o: Point) -> Point {
        let (a, b) = (self.0, o.0);
        Point([
            a[1].mul_add(b[2], -(a[2] * b[1])),
            a[2].mul_add(b[0], -(a[0] * b[2])),
            a[0].mul_add(b[1], -(a[1] * b[0])),
        ])
    }

    pub fn norm2(self) -> f64 {
        self.dot(self)
    }

    pub fn norm(self) -> f64 {
        self.norm2().sqrt()
    }

    /// `Normalize`: multiplies by the reciprocal of the norm, as the C++ does.
    pub fn normalize(self) -> Point {
        let mut n = self.norm();
        if n != 0.0 {
            n = 1.0 / n;
        }
        self * n
    }

    /// `Angle`: `atan2(|a x b|, a . b)`.
    pub fn angle(self, o: Point) -> f64 {
        self.cross(o).norm().atan2(self.dot(o))
    }

    pub fn largest_abs_component(self) -> usize {
        let t = [self.0[0].abs(), self.0[1].abs(), self.0[2].abs()];
        if t[0] > t[1] {
            if t[0] > t[2] {
                0
            } else {
                2
            }
        } else if t[1] > t[2] {
            1
        } else {
            2
        }
    }

    /// `Vector3::operator<`: lexicographic.
    pub fn less(self, o: Point) -> bool {
        for k in 0..3 {
            if self.0[k] < o.0[k] {
                return true;
            }
            if o.0[k] < self.0[k] {
                return false;
            }
        }
        false
    }

    pub fn is_zero(self) -> bool {
        self.0 == [0.0, 0.0, 0.0]
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point([self.0[0] + o.0[0], self.0[1] + o.0[1], self.0[2] + o.0[2]])
    }
}

impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point([self.0[0] - o.0[0], self.0[1] - o.0[1], self.0[2] - o.0[2]])
    }
}

impl Mul<f64> for Point {
    type Output = Point;
    fn mul(self, k: f64) -> Point {
        Point([self.0[0] * k, self.0[1] * k, self.0[2] * k])
    }
}

impl Neg for Point {
    type Output = Point;
    fn neg(self) -> Point {
        Point([-self.0[0], -self.0[1], -self.0[2]])
    }
}

/// `S2LatLng::FromDegrees(lat, lng).Normalized().ToPoint()`, how mongod
/// turns a longitude and latitude into a point.
pub fn latlng_point(lng: f64, lat: f64) -> Point {
    let deg = std::f64::consts::PI / 180.0;
    let (lat, lng) = (lat * deg, lng * deg);
    let lat = lat.clamp(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
    let lng = super::remainder(lng, 2.0 * std::f64::consts::PI);
    from_radians(lat, lng)
}

/// `S2LatLng::ToPoint` of a latitude and longitude in radians.
pub fn from_radians(lat: f64, lng: f64) -> Point {
    let cosphi = lat.cos();
    Point([lng.cos() * cosphi, lng.sin() * cosphi, lat.sin()])
}

/// `S2LatLng::Latitude`.
pub fn latitude(p: Point) -> f64 {
    p.z().atan2(p.x().mul_add(p.x(), p.y() * p.y()).sqrt())
}

/// `S2LatLng::Longitude`.
pub fn longitude(p: Point) -> f64 {
    p.y().atan2(p.x())
}

// ---------------------------------------------------------------- projections

pub fn st_to_uv(s: f64) -> f64 {
    if s >= 0.5 {
        (1.0 / 3.0) * (4.0 * s).mul_add(s, -1.0)
    } else {
        (1.0 / 3.0) * (-(4.0 * (1.0 - s))).mul_add(1.0 - s, 1.0)
    }
}

pub fn uv_to_st(u: f64) -> f64 {
    if u >= 0.0 {
        0.5 * 3.0f64.mul_add(u, 1.0).sqrt()
    } else {
        (-0.5f64).mul_add((-3.0f64).mul_add(u, 1.0).sqrt(), 1.0)
    }
}

pub fn face_uv_to_xyz(face: i32, u: f64, v: f64) -> Point {
    match face {
        0 => Point([1.0, u, v]),
        1 => Point([-u, 1.0, v]),
        2 => Point([-u, -v, 1.0]),
        3 => Point([-1.0, -v, -u]),
        4 => Point([v, -1.0, -u]),
        _ => Point([v, u, -1.0]),
    }
}

fn valid_face_xyz_to_uv(face: i32, p: Point) -> (f64, f64) {
    let p = p.0;
    match face {
        0 => (p[1] / p[0], p[2] / p[0]),
        1 => (-p[0] / p[1], p[2] / p[1]),
        2 => (-p[0] / p[2], -p[1] / p[2]),
        3 => (p[2] / p[0], p[1] / p[0]),
        4 => (p[2] / p[1], -p[0] / p[1]),
        _ => (-p[1] / p[2], -p[0] / p[2]),
    }
}

pub fn xyz_to_face_uv(p: Point) -> (i32, f64, f64) {
    let mut face = p.largest_abs_component() as i32;
    if p.0[face as usize] < 0.0 {
        face += 3;
    }
    let (u, v) = valid_face_xyz_to_uv(face, p);
    (face, u, v)
}

/// `FaceXYZtoUV`: `None` when the point is not on that face's side.
pub fn face_xyz_to_uv(face: i32, p: Point) -> Option<(f64, f64)> {
    if face < 3 {
        if p.0[face as usize] <= 0.0 {
            return None;
        }
    } else if p.0[(face - 3) as usize] >= 0.0 {
        return None;
    }
    Some(valid_face_xyz_to_uv(face, p))
}

pub fn u_norm(face: i32, u: f64) -> Point {
    match face {
        0 => Point([u, -1.0, 0.0]),
        1 => Point([1.0, u, 0.0]),
        2 => Point([1.0, 0.0, u]),
        3 => Point([-u, 0.0, 1.0]),
        4 => Point([0.0, -u, 1.0]),
        _ => Point([0.0, -1.0, -u]),
    }
}

pub fn v_norm(face: i32, v: f64) -> Point {
    match face {
        0 => Point([-v, 0.0, 1.0]),
        1 => Point([0.0, -v, 1.0]),
        2 => Point([0.0, -1.0, -v]),
        3 => Point([v, -1.0, 0.0]),
        4 => Point([1.0, v, 0.0]),
        _ => Point([1.0, 0.0, v]),
    }
}

/// `GetUAxis(face)[2]` and `GetVAxis(face)[2]`, all `GetRectBound` reads.
pub fn u_axis_z(face: i32) -> f64 {
    match face {
        3 | 4 => -1.0,
        _ => 0.0,
    }
}

pub fn v_axis_z(face: i32) -> f64 {
    match face {
        0 | 1 => 1.0,
        _ => 0.0,
    }
}

// ----------------------------------------------------------------- predicates

/// `S2::Ortho`.
pub fn ortho(a: Point) -> Point {
    let mut k = a.largest_abs_component() as i32 - 1;
    if k < 0 {
        k = 2;
    }
    let mut temp = [0.012, 0.0053, 0.00457];
    temp[k as usize] = 1.0;
    a.cross(Point(temp)).normalize()
}

/// `S2::Origin`.
pub fn origin() -> Point {
    Point([0.00457, 1.0, 0.0321]).normalize()
}

/// `S2::RobustCrossProd`.
pub fn robust_cross_prod(a: Point, b: Point) -> Point {
    let x = (b + a).cross(b - a);
    if !x.is_zero() {
        return x;
    }
    ortho(a)
}

const MAX_DET_ERROR: f64 = 0.8e-15;

/// `S2::RobustCCW(a, b, c, a_cross_b)`.
pub fn robust_ccw_with(a: Point, b: Point, c: Point, a_cross_b: Point) -> i32 {
    let det = a_cross_b.dot(c);
    if det > MAX_DET_ERROR {
        return 1;
    }
    if det < -MAX_DET_ERROR {
        return -1;
    }
    expensive_ccw(a, b, c)
}

pub fn robust_ccw(a: Point, b: Point, c: Point) -> i32 {
    robust_ccw_with(a, b, c, a.cross(b))
}

fn planar_ccw(a: (f64, f64), b: (f64, f64)) -> i32 {
    // `Vector2_d`: `DotProd` and `Norm2` are `x*x' + y*y'`, `CrossProd`
    // `x*y' - y*x'`, each fused like `Vector3`'s.
    let dot2 = |p: (f64, f64), q: (f64, f64)| p.0.mul_add(q.0, p.1 * q.1);
    let sab = if dot2(a, b) > 0.0 { -1.0 } else { 1.0 };
    let vab = (a.0 + sab * b.0, a.1 + sab * b.1);
    let da = dot2(a, a);
    let db = dot2(b, b);
    let less = |p: (f64, f64), q: (f64, f64)| p.0 < q.0 || (p.0 == q.0 && p.1 < q.1);
    let cross = |p: (f64, f64), q: (f64, f64)| p.0.mul_add(q.1, -(p.1 * q.0));
    let sign = if da < db || (da == db && less(a, b)) {
        cross(a, vab) * sab
    } else {
        cross(vab, b)
    };
    if sign > 0.0 {
        1
    } else if sign < 0.0 {
        -1
    } else {
        0
    }
}

fn planar_ordered_ccw(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> i32 {
    let sum = planar_ccw(a, b) + planar_ccw(b, c) + planar_ccw(c, a);
    sum.signum()
}

/// `S2::ExpensiveCCW`, the double-precision version mongod compiles
/// (`SIMULATION_OF_SIMPLICITY` is undefined there).
pub fn expensive_ccw(a: Point, b: Point, c: Point) -> i32 {
    if a == b || b == c || c == a {
        return 0;
    }
    let sab = if a.dot(b) > 0.0 { -1.0 } else { 1.0 };
    let sbc = if b.dot(c) > 0.0 { -1.0 } else { 1.0 };
    let sca = if c.dot(a) > 0.0 { -1.0 } else { 1.0 };
    let vab = a + b * sab;
    let vbc = b + c * sbc;
    let vca = c + a * sca;
    let dab = vab.norm2();
    let dbc = vbc.norm2();
    let dca = vca.norm2();
    let sign = if dca < dbc || (dca == dbc && a.less(b)) {
        if dab < dbc || (dab == dbc && a.less(c)) {
            vab.cross(vca).dot(a) * sab
        } else {
            vca.cross(vbc).dot(c) * sca
        }
    } else if dab < dca || (dab == dca && b.less(c)) {
        vbc.cross(vab).dot(b) * sbc
    } else {
        vca.cross(vbc).dot(c) * sca
    };
    if sign > 0.0 {
        return 1;
    }
    if sign < 0.0 {
        return -1;
    }
    let yz = |p: Point| (p.y(), p.z());
    let zx = |p: Point| (p.z(), p.x());
    let xy = |p: Point| (p.x(), p.y());
    let mut ccw = planar_ordered_ccw(yz(a), yz(b), yz(c));
    if ccw == 0 {
        ccw = planar_ordered_ccw(zx(a), zx(b), zx(c));
        if ccw == 0 {
            ccw = planar_ordered_ccw(xy(a), xy(b), xy(c));
        }
    }
    ccw
}

/// `S2::OrderedCCW`.
pub fn ordered_ccw(a: Point, b: Point, c: Point, o: Point) -> bool {
    let mut sum = 0;
    if robust_ccw(b, o, a) >= 0 {
        sum += 1;
    }
    if robust_ccw(c, o, b) >= 0 {
        sum += 1;
    }
    if robust_ccw(a, o, c) > 0 {
        sum += 1;
    }
    sum >= 2
}

/// `S2::TurnAngle`.
pub fn turn_angle(a: Point, b: Point, c: Point) -> f64 {
    let angle = robust_cross_prod(b, a).angle(robust_cross_prod(c, b));
    if robust_ccw(a, b, c) > 0 {
        angle
    } else {
        -angle
    }
}
