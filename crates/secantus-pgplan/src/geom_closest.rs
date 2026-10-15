//! The closest-point operators `##` and the distances that are defined by
//! them, transcribed from PostgreSQL 15's geo_ops.c (`lseg_closept_point`,
//! `line_closept_point`, `lseg_closept_line`, `lseg_closept_lseg`,
//! `box_closept_point`, `box_closept_lseg`, `box_interpt_lseg`).

use super::geom::{
    fp_le, line_construct, line_interpt_line, line_invsl, lseg_interpt_line, lseg_interpt_lseg,
    point_dt, P,
};

type Line = (f64, f64, f64);

/// `point_invsl`.
fn point_invsl(a: P, b: P) -> f64 {
    if super::geom::fp_eq(a.x, b.x) {
        return 0.0;
    }
    if super::geom::fp_eq(a.y, b.y) {
        return f64::INFINITY;
    }
    (a.x - b.x) / (b.y - a.y)
}

/// `line_closept_point`: a perpendicular dropped onto the line.
pub(crate) fn line_closept_point(line: Line, pt: P) -> (P, f64) {
    let tmp = line_construct(pt, line_invsl(line));
    let closest = line_interpt_line(tmp, line).unwrap_or(pt);
    (closest, point_dt(closest, pt))
}

/// `lseg_closept_line`: where the segment meets the line, else the nearer
/// endpoint.
pub(crate) fn lseg_closept_line(p0: P, p1: P, line: Line) -> (P, f64) {
    if let Some(p) = lseg_interpt_line(p0, p1, line) {
        return (p, 0.0);
    }
    let d0 = line_closept_point(line, p0).1;
    let d1 = line_closept_point(line, p1).1;
    if d0 < d1 {
        (p0, d0)
    } else {
        (p1, d1)
    }
}

/// `lseg_closept_point`: the perpendicular from `pt` meeting the segment.
pub(crate) fn lseg_closept_point(p0: P, p1: P, pt: P) -> (P, f64) {
    let tmp = line_construct(pt, point_invsl(p0, p1));
    let (closest, _) = lseg_closept_line(p0, p1, tmp);
    (closest, point_dt(closest, pt))
}

/// `lseg_closept_lseg`: the point on `on` closest to `to`.
pub(crate) fn lseg_closept_lseg(on: (P, P), to: (P, P)) -> (P, f64) {
    if let Some(p) = lseg_interpt_lseg(on.0, on.1, to.0, to.1) {
        return (p, 0.0);
    }
    let (mut best, mut dist) = lseg_closept_point(on.0, on.1, to.0);
    let (p, d) = lseg_closept_point(on.0, on.1, to.1);
    if d < dist {
        (best, dist) = (p, d);
    }
    let d = lseg_closept_point(to.0, to.1, on.0).1;
    if d < dist {
        (best, dist) = (on.0, d);
    }
    let d = lseg_closept_point(to.0, to.1, on.1).1;
    if d < dist {
        (best, dist) = (on.1, d);
    }
    (best, dist)
}

/// `box_contain_point`, which compares exactly.
fn box_contains(high: P, low: P, pt: P) -> bool {
    high.x >= pt.x && low.x <= pt.x && high.y >= pt.y && low.y <= pt.y
}

/// The box's four sides, in the order geo_ops.c tries them.
fn sides(high: P, low: P) -> [(P, P); 4] {
    let a = P {
        x: low.x,
        y: high.y,
    };
    let b = P {
        x: high.x,
        y: low.y,
    };
    [(low, a), (high, a), (low, b), (high, b)]
}

/// `box_closept_point`.
pub(crate) fn box_closept_point(high: P, low: P, pt: P) -> (P, f64) {
    if box_contains(high, low, pt) {
        return (pt, 0.0);
    }
    let [first, rest @ ..] = sides(high, low);
    let (mut best, mut dist) = lseg_closept_point(first.0, first.1, pt);
    for (a, b) in rest {
        let (p, d) = lseg_closept_point(a, b, pt);
        if d < dist {
            (best, dist) = (p, d);
        }
    }
    (best, dist)
}

/// `box_interpt_lseg`: whether the segment meets the box, and (when its
/// bounding box overlaps the box) the segment's point nearest the centre.
fn box_interpt_lseg(high: P, low: P, seg: (P, P)) -> Option<P> {
    let (lh, ll) = (
        P {
            x: seg.0.x.max(seg.1.x),
            y: seg.0.y.max(seg.1.y),
        },
        P {
            x: seg.0.x.min(seg.1.x),
            y: seg.0.y.min(seg.1.y),
        },
    );
    let overlap =
        fp_le(ll.x, high.x) && fp_le(low.x, lh.x) && fp_le(ll.y, high.y) && fp_le(low.y, lh.y);
    if !overlap {
        return None;
    }
    let centre = P {
        x: (high.x + low.x) / 2.0,
        y: (high.y + low.y) / 2.0,
    };
    let (at, _) = lseg_closept_point(seg.0, seg.1, centre);
    let meets = box_contains(high, low, seg.0)
        || box_contains(high, low, seg.1)
        || sides(high, low)
            .iter()
            .any(|(a, b)| lseg_interpt_lseg(*a, *b, seg.0, seg.1).is_some());
    meets.then_some(at)
}

/// `box_closept_lseg`: the point on the box closest to the segment.
pub(crate) fn box_closept_lseg(high: P, low: P, seg: (P, P)) -> (P, f64) {
    if let Some(p) = box_interpt_lseg(high, low, seg) {
        return (p, 0.0);
    }
    let [first, rest @ ..] = sides(high, low);
    let (mut best, mut dist) = lseg_closept_lseg(first, seg);
    for side in rest {
        let (p, d) = lseg_closept_lseg(side, seg);
        if d < dist {
            (best, dist) = (p, d);
        }
    }
    (best, dist)
}
