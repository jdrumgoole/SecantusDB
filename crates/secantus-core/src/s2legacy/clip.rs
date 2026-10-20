//! `S2Polygon::IntersectWithPolyline`, as far as mongod's
//! `polygonLineIntersection` asks it (`geometry_container.cpp`, r8.2): whether
//! clipping the line to the polygon leaves anything. A transcription of
//! `InternalClipPolyline`, `ClipEdge` and `AddIntersection`
//! (`s2polygon.cc`) and `S2EdgeUtil::GetIntersection` (`s2edgeutil.cc`).

use super::edge::{vertex_crossing, EdgeCrosser};
use super::loops::Polygon;
use super::point::{ordered_ccw, robust_cross_prod, Point};

fn add(a: Point, b: Point) -> Point {
    Point([a.0[0] + b.0[0], a.0[1] + b.0[1], a.0[2] + b.0[2]])
}

fn neg(a: Point) -> Point {
    Point([-a.0[0], -a.0[1], -a.0[2]])
}

fn sub(a: Point, b: Point) -> Point {
    Point([a.0[0] - b.0[0], a.0[1] - b.0[1], a.0[2] - b.0[2]])
}

/// `ReplaceIfCloser`: `y` when it is nearer `x` than the best so far.
fn replace_if_closer(x: Point, y: Point, dmin2: &mut f64, vmin: &mut Point) {
    let d2 = sub(x, y).norm2();
    if d2 < *dmin2 || (d2 == *dmin2 && y.less(*vmin)) {
        *dmin2 = d2;
        *vmin = y;
    }
}

/// `S2EdgeUtil::GetIntersection`, for two edges that cross properly.
pub fn get_intersection(a0: Point, a1: Point, b0: Point, b1: Point) -> Point {
    let a_norm = robust_cross_prod(a0, a1).normalize();
    let b_norm = robust_cross_prod(b0, b1).normalize();
    let mut x = robust_cross_prod(a_norm, b_norm).normalize();
    if x.dot(add(add(a0, a1), add(b0, b1))) < 0.0 {
        x = neg(x);
    }
    if ordered_ccw(a0, x, a1, a_norm) && ordered_ccw(b0, x, b1, b_norm) {
        return x;
    }
    let mut dmin2 = 10.0;
    let mut vmin = x;
    if ordered_ccw(b0, a0, b1, b_norm) {
        replace_if_closer(x, a0, &mut dmin2, &mut vmin);
    }
    if ordered_ccw(b0, a1, b1, b_norm) {
        replace_if_closer(x, a1, &mut dmin2, &mut vmin);
    }
    if ordered_ccw(a0, b0, a1, a_norm) {
        replace_if_closer(x, b0, &mut dmin2, &mut vmin);
    }
    if ordered_ccw(a0, b1, a1, a_norm) {
        replace_if_closer(x, b1, &mut dmin2, &mut vmin);
    }
    vmin
}

/// `S2EdgeUtil::GetDistanceFraction`.
fn distance_fraction(x: Point, a0: Point, a1: Point) -> f64 {
    let d0 = x.angle(a0);
    let d1 = x.angle(a1);
    d0 / (d0 + d1)
}

/// `AddIntersection`, with `add_shared_edges` as `InternalClipPolyline`
/// passes it (true).
fn add_intersection(
    a0: Point,
    a1: Point,
    b0: Point,
    b1: Point,
    crossing: i32,
    out: &mut Vec<(f64, Point)>,
) {
    if crossing > 0 {
        let x = get_intersection(a0, a1, b0, b1);
        out.push((distance_fraction(x, a0, a1), x));
    } else if vertex_crossing(a0, a1, b0, b1) {
        let t = if a0 == b0 || a0 == b1 { 0.0 } else { 1.0 };
        out.push((t, if t == 0.0 { a0 } else { a1 }));
    }
}

/// `std::pair<double, S2Point>`'s order: the fraction, then the point.
fn pair_order(x: &(f64, Point), y: &(f64, Point)) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match x.0.partial_cmp(&y.0).unwrap_or(Ordering::Equal) {
        Ordering::Equal if x.1.less(y.1) => Ordering::Less,
        Ordering::Equal if y.1.less(x.1) => Ordering::Greater,
        other => other,
    }
}

impl Polygon {
    /// The polygon's edges as `S2PolygonIndex(poly, false)` walks them: a
    /// shell's forwards, a hole's backwards.
    fn index_edges(&self) -> Vec<(Point, Point)> {
        let mut edges = Vec::new();
        for l in &self.loops {
            let n = l.num_vertices();
            let hole = l.depth % 2 == 1;
            for i in 0..n {
                edges.push(if hole {
                    (l.vertex(n - 1 - i), l.vertex(2 * n - 2 - i))
                } else {
                    (l.vertex(i), l.vertex(i + 1))
                });
            }
        }
        edges
    }

    /// Whether `IntersectWithPolyline` would return any polyline: some
    /// piece of `line` lies in the polygon.
    pub fn clips_polyline(&self, line: &[Point]) -> bool {
        if line.len() < 2 {
            return false;
        }
        let edges = self.index_edges();
        let mut inside = self.contains_point(line[0]);
        for pair in line.windows(2) {
            let (a0, a1) = (pair[0], pair[1]);
            let mut xs: Vec<(f64, Point)> = Vec::new();
            // `ClipEdge`.
            let mut crosser = EdgeCrosser::new(a0, a1, a0);
            let mut previous_to: Option<Point> = None;
            for (from, to) in &edges {
                if previous_to != Some(*from) {
                    crosser.restart_at(*from);
                }
                previous_to = Some(*to);
                let crossing = crosser.robust_crossing(*to);
                if crossing >= 0 {
                    add_intersection(a0, a1, *from, *to, crossing, &mut xs);
                }
            }
            if inside {
                xs.push((0.0, a0));
            }
            inside = xs.len() % 2 == 1;
            if inside {
                xs.push((1.0, a1));
            }
            xs.sort_by(pair_order);
            if xs
                .chunks(2)
                .any(|p| p.len() == 2 && (p[0].0 != p[1].0 || p[0].1 != p[1].1))
            {
                return true;
            }
        }
        false
    }
}
