//! PostgreSQL's geometric types beyond `box` (which lives in `geo`): `point`,
//! `lseg`, `line`, `path`, `polygon` and `circle`, with their text and binary
//! forms, the explicit casts between them, their operators and functions.
//!
//! Like a box, a value is a single-key tagged document -- `{"__point": [x,
//! y]}` -- so it knows its type wherever it travels. Comparisons use
//! PostgreSQL's `EPSILON` (1e-6) exactly where `geo_ops.c` does (`FPeq`,
//! `FPlt`, ...). Outputs were measured against PostgreSQL 14.

use bson::{Bson, Document};

use crate::geo;
use crate::{Error, Result};

const EPSILON: f64 = 1.0e-06;

fn fp_eq(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= EPSILON
}
fn fp_lt(a: f64, b: f64) -> bool {
    a + EPSILON < b
}
fn fp_le(a: f64, b: f64) -> bool {
    a <= b + EPSILON
}
fn fp_gt(a: f64, b: f64) -> bool {
    a > b + EPSILON
}
fn fp_ge(a: f64, b: f64) -> bool {
    a + EPSILON >= b
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct P {
    pub x: f64,
    pub y: f64,
}

impl P {
    fn dist(self, o: P) -> f64 {
        (self.x - o.x).hypot(self.y - o.y)
    }
    fn same(self, o: P) -> bool {
        fp_eq(self.x, o.x) && fp_eq(self.y, o.y)
    }
}

/// A geometric value.
#[derive(Debug, Clone, PartialEq)]
pub enum Geo {
    Point(P),
    Lseg(P, P),
    /// `A x + B y + C = 0`.
    Line(f64, f64, f64),
    /// High corner, low corner.
    Box(P, P),
    Path {
        closed: bool,
        pts: Vec<P>,
    },
    Polygon(Vec<P>),
    Circle(P, f64),
}

pub const TYPES: &[&str] = &["point", "lseg", "line", "path", "polygon", "circle"];

impl Geo {
    pub fn type_name(&self) -> &'static str {
        match self {
            Geo::Point(_) => "point",
            Geo::Lseg(..) => "lseg",
            Geo::Line(..) => "line",
            Geo::Box(..) => "box",
            Geo::Path { .. } => "path",
            Geo::Polygon(_) => "polygon",
            Geo::Circle(..) => "circle",
        }
    }
}

fn nums(v: &[Bson]) -> Option<Vec<f64>> {
    v.iter()
        .map(|b| match b {
            Bson::Double(d) => Some(*d),
            Bson::Int32(i) => Some(f64::from(*i)),
            Bson::Int64(i) => Some(*i as f64),
            _ => None,
        })
        .collect()
}

fn pairs(v: &[f64]) -> Vec<P> {
    v.chunks(2).map(|c| P { x: c[0], y: c[1] }).collect()
}

fn flat(pts: &[P]) -> Vec<Bson> {
    pts.iter()
        .flat_map(|p| [Bson::Double(p.x), Bson::Double(p.y)])
        .collect()
}

/// The geometric value `v` holds, `box` included.
pub fn from_bson(v: &Bson) -> Option<Geo> {
    if let Some(c) = geo::box_coords(v) {
        return Some(Geo::Box(P { x: c[0], y: c[1] }, P { x: c[2], y: c[3] }));
    }
    let Bson::Document(d) = v else { return None };
    if d.len() != 1 {
        return None;
    }
    let (key, val) = d.iter().next()?;
    let Bson::Array(items) = val else { return None };
    Some(match key.as_str() {
        "__point" => {
            let n = nums(items)?;
            Geo::Point(P { x: n[0], y: n[1] })
        }
        "__lseg" => {
            let n = nums(items)?;
            Geo::Lseg(P { x: n[0], y: n[1] }, P { x: n[2], y: n[3] })
        }
        "__line" => {
            let n = nums(items)?;
            Geo::Line(n[0], n[1], n[2])
        }
        "__path" => {
            let closed = matches!(items.first()?, Bson::Boolean(true));
            Geo::Path {
                closed,
                pts: pairs(&nums(&items[1..])?),
            }
        }
        "__polygon" => Geo::Polygon(pairs(&nums(items)?)),
        "__circle" => {
            let n = nums(items)?;
            Geo::Circle(P { x: n[0], y: n[1] }, n[2])
        }
        _ => return None,
    })
}

/// Is `v` one of these six types (not a box)?
pub fn is_geom(v: &Bson) -> bool {
    matches!(from_bson(v), Some(g) if !matches!(g, Geo::Box(..)))
}

pub fn to_bson(g: &Geo) -> Bson {
    let (key, items): (&str, Vec<Bson>) = match g {
        Geo::Box(h, l) => return geo::box_value(normalise_box(*h, *l)),
        Geo::Point(p) => ("__point", flat(&[*p])),
        Geo::Lseg(a, b) => ("__lseg", flat(&[*a, *b])),
        Geo::Line(a, b, c) => (
            "__line",
            vec![Bson::Double(*a), Bson::Double(*b), Bson::Double(*c)],
        ),
        Geo::Path { closed, pts } => {
            let mut items = vec![Bson::Boolean(*closed)];
            items.extend(flat(pts));
            ("__path", items)
        }
        Geo::Polygon(pts) => ("__polygon", flat(pts)),
        Geo::Circle(c, r) => (
            "__circle",
            vec![Bson::Double(c.x), Bson::Double(c.y), Bson::Double(*r)],
        ),
    };
    let mut d = Document::new();
    d.insert(key, Bson::Array(items));
    Bson::Document(d)
}

fn normalise_box(a: P, b: P) -> [f64; 4] {
    [a.x.max(b.x), a.y.max(b.y), a.x.min(b.x), a.y.min(b.y)]
}

fn boxed(a: P, b: P) -> Geo {
    let c = normalise_box(a, b);
    Geo::Box(P { x: c[0], y: c[1] }, P { x: c[2], y: c[3] })
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

fn f(v: f64) -> String {
    geo::float8_text(v)
}

fn pt(p: P) -> String {
    format!("({},{})", f(p.x), f(p.y))
}

fn pts(ps: &[P]) -> String {
    ps.iter().map(|p| pt(*p)).collect::<Vec<_>>().join(",")
}

/// The output form.
pub fn text(g: &Geo) -> String {
    match g {
        Geo::Point(p) => pt(*p),
        Geo::Lseg(a, b) => format!("[{},{}]", pt(*a), pt(*b)),
        Geo::Line(a, b, c) => format!("{{{},{},{}}}", f(*a), f(*b), f(*c)),
        Geo::Box(h, l) => format!("{},{}", pt(*h), pt(*l)),
        Geo::Path {
            closed: true,
            pts: p,
        } => format!("({})", pts(p)),
        Geo::Path {
            closed: false,
            pts: p,
        } => format!("[{}]", pts(p)),
        Geo::Polygon(p) => format!("({})", pts(p)),
        Geo::Circle(c, r) => format!("<{},{}>", pt(*c), f(*r)),
    }
}

fn bad_input(ty: &str, text: &str) -> Error {
    Error::Sqlstate(
        "22P02",
        format!("invalid input syntax for type {ty}: \"{text}\""),
    )
}

/// The numbers of an input text, in order, and its first delimiter.
fn scan(ty: &str, text: &str) -> Result<(Vec<f64>, Option<char>)> {
    let trimmed = text.trim();
    let first = trimmed.chars().next();
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut Vec<f64>| -> Result<()> {
        let t = token.trim();
        if !t.is_empty() {
            let v = match t.to_ascii_lowercase().as_str() {
                "inf" | "infinity" | "+inf" | "+infinity" => f64::INFINITY,
                "-inf" | "-infinity" => f64::NEG_INFINITY,
                "nan" => f64::NAN,
                other => other.parse::<f64>().map_err(|_| bad_input(ty, text))?,
            };
            out.push(v);
        }
        token.clear();
        Ok(())
    };
    for c in trimmed.chars() {
        match c {
            '(' | '[' | '{' | '<' => {
                if !token.trim().is_empty() {
                    return Err(bad_input(ty, text));
                }
                depth += 1;
            }
            ')' | ']' | '}' | '>' => {
                flush(&mut token, &mut out)?;
                depth -= 1;
                if depth < 0 {
                    return Err(bad_input(ty, text));
                }
            }
            ',' => flush(&mut token, &mut out)?,
            c => token.push(c),
        }
    }
    flush(&mut token, &mut out)?;
    if depth != 0 {
        return Err(bad_input(ty, text));
    }
    Ok((out, first))
}

/// Parse the input form of `ty` (one of `TYPES`, or `box`).
pub fn parse(ty: &str, text: &str) -> Result<Geo> {
    let (n, first) = scan(ty, text)?;
    let need = |k: usize| -> Result<()> {
        if n.len() == k {
            Ok(())
        } else {
            Err(bad_input(ty, text))
        }
    };
    Ok(match ty {
        "point" => {
            need(2)?;
            Geo::Point(P { x: n[0], y: n[1] })
        }
        "lseg" => {
            need(4)?;
            Geo::Lseg(P { x: n[0], y: n[1] }, P { x: n[2], y: n[3] })
        }
        "box" => {
            need(4)?;
            boxed(P { x: n[0], y: n[1] }, P { x: n[2], y: n[3] })
        }
        "line" => {
            if first == Some('{') {
                need(3)?;
                if fp_eq(n[0], 0.0) && fp_eq(n[1], 0.0) {
                    return Err(Error::Sqlstate(
                        "22P02",
                        "invalid line specification: A and B cannot both be zero".into(),
                    ));
                }
                Geo::Line(n[0], n[1], n[2])
            } else {
                need(4)?;
                let (a, b) = (P { x: n[0], y: n[1] }, P { x: n[2], y: n[3] });
                if a.same(b) {
                    return Err(Error::Sqlstate(
                        "22P02",
                        "invalid line specification: must be two distinct points".into(),
                    ));
                }
                line_from(a, b)
            }
        }
        "path" => {
            if n.is_empty() || n.len() % 2 != 0 {
                return Err(bad_input(ty, text));
            }
            Geo::Path {
                closed: first != Some('['),
                pts: pairs(&n),
            }
        }
        "polygon" => {
            if n.is_empty() || n.len() % 2 != 0 {
                return Err(bad_input(ty, text));
            }
            Geo::Polygon(pairs(&n))
        }
        "circle" => {
            need(3)?;
            if n[2] < 0.0 {
                return Err(bad_input(ty, text));
            }
            Geo::Circle(P { x: n[0], y: n[1] }, n[2])
        }
        _ => return Err(bad_input(ty, text)),
    })
}

fn line_from(a: P, b: P) -> Geo {
    if fp_eq(a.x, b.x) {
        Geo::Line(-1.0, 0.0, a.x)
    } else {
        let m = (b.y - a.y) / (b.x - a.x);
        Geo::Line(m, -1.0, a.y - m * a.x)
    }
}

// ---------------------------------------------------------------------------
// Binary (the `*_send` / `*_recv` layouts)
// ---------------------------------------------------------------------------

pub fn to_binary(g: &Geo) -> Vec<u8> {
    let mut out = Vec::new();
    let put = |out: &mut Vec<u8>, v: f64| out.extend_from_slice(&v.to_be_bytes());
    match g {
        Geo::Point(p) => {
            put(&mut out, p.x);
            put(&mut out, p.y);
        }
        Geo::Lseg(a, b) | Geo::Box(a, b) => {
            for v in [a.x, a.y, b.x, b.y] {
                put(&mut out, v);
            }
        }
        Geo::Line(a, b, c) => {
            for v in [*a, *b, *c] {
                put(&mut out, v);
            }
        }
        Geo::Path { closed, pts } => {
            out.push(u8::from(*closed));
            out.extend_from_slice(&(pts.len() as i32).to_be_bytes());
            for p in pts {
                put(&mut out, p.x);
                put(&mut out, p.y);
            }
        }
        Geo::Polygon(pts) => {
            out.extend_from_slice(&(pts.len() as i32).to_be_bytes());
            for p in pts {
                put(&mut out, p.x);
                put(&mut out, p.y);
            }
        }
        Geo::Circle(c, r) => {
            for v in [c.x, c.y, *r] {
                put(&mut out, v);
            }
        }
    }
    out
}

pub fn from_binary(ty: &str, bytes: &[u8]) -> Result<Geo> {
    let bad = || Error::Sqlstate("22P03", format!("incorrect binary data format for {ty}"));
    let float = |i: usize| -> Result<f64> {
        let b: [u8; 8] = bytes
            .get(i..i + 8)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?;
        Ok(f64::from_be_bytes(b))
    };
    let points_at = |start: usize, n: usize| -> Result<Vec<P>> {
        (0..n)
            .map(|k| {
                Ok(P {
                    x: float(start + 16 * k)?,
                    y: float(start + 16 * k + 8)?,
                })
            })
            .collect()
    };
    let count = |i: usize| -> Result<usize> {
        let b: [u8; 4] = bytes
            .get(i..i + 4)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?;
        usize::try_from(i32::from_be_bytes(b)).map_err(|_| bad())
    };
    Ok(match ty {
        "point" => Geo::Point(P {
            x: float(0)?,
            y: float(8)?,
        }),
        "lseg" => Geo::Lseg(
            P {
                x: float(0)?,
                y: float(8)?,
            },
            P {
                x: float(16)?,
                y: float(24)?,
            },
        ),
        "box" => boxed(
            P {
                x: float(0)?,
                y: float(8)?,
            },
            P {
                x: float(16)?,
                y: float(24)?,
            },
        ),
        "line" => Geo::Line(float(0)?, float(8)?, float(16)?),
        "circle" => Geo::Circle(
            P {
                x: float(0)?,
                y: float(8)?,
            },
            float(16)?,
        ),
        "path" => {
            let closed = *bytes.first().ok_or_else(bad)? != 0;
            Geo::Path {
                closed,
                pts: points_at(5, count(1)?)?,
            }
        }
        "polygon" => Geo::Polygon(points_at(4, count(0)?)?),
        _ => return Err(bad()),
    })
}

// ---------------------------------------------------------------------------
// Measures
// ---------------------------------------------------------------------------

fn seg_closest(p: P, a: P, b: P) -> P {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return a;
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    P {
        x: a.x + t * dx,
        y: a.y + t * dy,
    }
}

fn dist_ps(p: P, a: P, b: P) -> f64 {
    p.dist(seg_closest(p, a, b))
}

fn segments(pts: &[P], closed: bool) -> Vec<(P, P)> {
    let mut out: Vec<(P, P)> = pts.windows(2).map(|w| (w[0], w[1])).collect();
    if closed && pts.len() > 2 {
        out.push((pts[pts.len() - 1], pts[0]));
    }
    out
}

fn cross(o: P, a: P, b: P) -> f64 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn on_segment(p: P, a: P, b: P) -> bool {
    fp_eq(dist_ps(p, a, b), 0.0)
}

fn segs_intersect(a: P, b: P, c: P, d: P) -> bool {
    let d1 = cross(c, d, a);
    let d2 = cross(c, d, b);
    let d3 = cross(a, b, c);
    let d4 = cross(a, b, d);
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    on_segment(a, c, d) || on_segment(b, c, d) || on_segment(c, a, b) || on_segment(d, a, b)
}

/// Inside, or on the boundary of, the polygon.
fn in_polygon(p: P, poly: &[P]) -> bool {
    if poly.is_empty() {
        return false;
    }
    for (a, b) in segments(poly, true) {
        if on_segment(p, a, b) {
            return true;
        }
    }
    if poly.len() == 1 {
        return p.same(poly[0]);
    }
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn in_box(p: P, h: P, l: P) -> bool {
    fp_le(l.x, p.x) && fp_le(p.x, h.x) && fp_le(l.y, p.y) && fp_le(p.y, h.y)
}

fn bbox(ps: &[P]) -> (P, P) {
    let (mut hx, mut hy, mut lx, mut ly) = (f64::MIN, f64::MIN, f64::MAX, f64::MAX);
    for p in ps {
        hx = hx.max(p.x);
        hy = hy.max(p.y);
        lx = lx.min(p.x);
        ly = ly.min(p.y);
    }
    (P { x: hx, y: hy }, P { x: lx, y: ly })
}

fn box_pts(h: P, l: P) -> Vec<P> {
    vec![l, P { x: l.x, y: h.y }, h, P { x: h.x, y: l.y }]
}

fn center(g: &Geo) -> Option<P> {
    Some(match g {
        Geo::Point(p) => *p,
        Geo::Lseg(a, b) => P {
            x: (a.x + b.x) / 2.0,
            y: (a.y + b.y) / 2.0,
        },
        Geo::Box(h, l) => P {
            x: (h.x + l.x) / 2.0,
            y: (h.y + l.y) / 2.0,
        },
        Geo::Circle(c, _) => *c,
        Geo::Polygon(ps) => {
            let n = ps.len() as f64;
            P {
                x: ps.iter().map(|p| p.x).sum::<f64>() / n,
                y: ps.iter().map(|p| p.y).sum::<f64>() / n,
            }
        }
        _ => return None,
    })
}

fn line_dist(p: P, a: f64, b: f64, c: f64) -> f64 {
    (a * p.x + b * p.y + c).abs() / a.hypot(b)
}

/// The distance from a point to any shape.
fn dist_point(p: P, g: &Geo) -> Option<f64> {
    Some(match g {
        Geo::Point(q) => p.dist(*q),
        Geo::Lseg(a, b) => dist_ps(p, *a, *b),
        Geo::Line(a, b, c) => line_dist(p, *a, *b, *c),
        Geo::Box(h, l) => {
            if in_box(p, *h, *l) {
                0.0
            } else {
                segments(&box_pts(*h, *l), true)
                    .into_iter()
                    .map(|(a, b)| dist_ps(p, a, b))
                    .fold(f64::MAX, f64::min)
            }
        }
        Geo::Path { closed, pts } => {
            if pts.len() == 1 {
                p.dist(pts[0])
            } else {
                segments(pts, *closed)
                    .into_iter()
                    .map(|(a, b)| dist_ps(p, a, b))
                    .fold(f64::MAX, f64::min)
            }
        }
        Geo::Polygon(ps) => {
            if in_polygon(p, ps) {
                0.0
            } else {
                segments(ps, true)
                    .into_iter()
                    .map(|(a, b)| dist_ps(p, a, b))
                    .fold(f64::MAX, f64::min)
            }
        }
        Geo::Circle(c, r) => (p.dist(*c) - r).max(0.0),
    })
}

fn distance(a: &Geo, b: &Geo) -> Option<f64> {
    Some(match (a, b) {
        (Geo::Point(p), other) => dist_point(*p, other)?,
        (other, Geo::Point(p)) => dist_point(*p, other)?,
        (Geo::Circle(c1, r1), Geo::Circle(c2, r2)) => (c1.dist(*c2) - r1 - r2).max(0.0),
        (Geo::Circle(c, r), Geo::Polygon(ps)) | (Geo::Polygon(ps), Geo::Circle(c, r)) => {
            (dist_point(*c, &Geo::Polygon(ps.clone()))? - r).max(0.0)
        }
        (Geo::Lseg(a1, a2), Geo::Lseg(b1, b2)) => {
            if segs_intersect(*a1, *a2, *b1, *b2) {
                0.0
            } else {
                [
                    dist_ps(*a1, *b1, *b2),
                    dist_ps(*a2, *b1, *b2),
                    dist_ps(*b1, *a1, *a2),
                    dist_ps(*b2, *a1, *a2),
                ]
                .into_iter()
                .fold(f64::MAX, f64::min)
            }
        }
        // Box to box is between their CENTERS (`box_distance`).
        (Geo::Box(..), Geo::Box(..)) => center(a)?.dist(center(b)?),
        (
            Geo::Path {
                closed: c1,
                pts: p1,
            },
            Geo::Path {
                closed: c2,
                pts: p2,
            },
        ) => {
            let mut best = f64::MAX;
            for (a1, a2) in segments(p1, *c1) {
                for (b1, b2) in segments(p2, *c2) {
                    let d = distance(&Geo::Lseg(a1, a2), &Geo::Lseg(b1, b2))?;
                    best = best.min(d);
                }
            }
            best
        }
        (Geo::Polygon(p1), Geo::Polygon(p2)) => {
            if overlaps(a, b)? {
                0.0
            } else {
                distance(
                    &Geo::Path {
                        closed: true,
                        pts: p1.clone(),
                    },
                    &Geo::Path {
                        closed: true,
                        pts: p2.clone(),
                    },
                )?
            }
        }
        _ => return None,
    })
}

fn area(g: &Geo) -> Option<f64> {
    Some(match g {
        Geo::Box(h, l) => (h.x - l.x) * (h.y - l.y),
        Geo::Circle(_, r) => std::f64::consts::PI * r * r,
        Geo::Path { closed: false, .. } => return None,
        Geo::Path { pts, .. } => {
            let mut s = 0.0;
            for (a, b) in segments(pts, true) {
                s += a.x * b.y - b.x * a.y;
            }
            (s / 2.0).abs()
        }
        _ => return None,
    })
}

fn length(g: &Geo) -> Option<f64> {
    Some(match g {
        Geo::Lseg(a, b) => a.dist(*b),
        Geo::Path { closed, pts } => segments(pts, *closed)
            .into_iter()
            .map(|(a, b)| a.dist(b))
            .sum(),
        _ => return None,
    })
}

fn contains(outer: &Geo, inner: &Geo) -> Option<bool> {
    Some(match (outer, inner) {
        (Geo::Box(h, l), Geo::Point(p)) => in_box(*p, *h, *l),
        (Geo::Polygon(ps), Geo::Point(p)) => in_polygon(*p, ps),
        (Geo::Circle(c, r), Geo::Point(p)) => fp_le(c.dist(*p), *r),
        (Geo::Path { closed: true, pts }, Geo::Point(p)) => in_polygon(*p, pts),
        (Geo::Path { closed: false, pts }, Geo::Point(p)) => segments(pts, false)
            .into_iter()
            .any(|(a, b)| on_segment(*p, a, b)),
        (Geo::Lseg(a, b), Geo::Point(p)) => on_segment(*p, *a, *b),
        (Geo::Line(a, b, c), Geo::Point(p)) => fp_eq(a * p.x + b * p.y + c, 0.0),
        (Geo::Circle(c1, r1), Geo::Circle(c2, r2)) => fp_le(c1.dist(*c2) + r2, *r1),
        (Geo::Polygon(ps), Geo::Polygon(qs)) => {
            qs.iter().all(|q| in_polygon(*q, ps))
                && !segments(qs, true).into_iter().any(|(a, b)| {
                    segments(ps, true)
                        .into_iter()
                        .any(|(c, d)| proper_cross(a, b, c, d))
                })
        }
        (Geo::Box(h, l), Geo::Lseg(a, b)) => in_box(*a, *h, *l) && in_box(*b, *h, *l),
        (Geo::Line(..), Geo::Lseg(a, b)) => {
            contains(outer, &Geo::Point(*a))? && contains(outer, &Geo::Point(*b))?
        }
        (Geo::Polygon(_), Geo::Box(h, l)) => contains(outer, &Geo::Polygon(box_pts(*h, *l)))?,
        (Geo::Box(h, l), Geo::Polygon(ps)) => ps.iter().all(|p| in_box(*p, *h, *l)),
        _ => return None,
    })
}

/// Segments that cross at an interior point of both.
fn proper_cross(a: P, b: P, c: P, d: P) -> bool {
    let d1 = cross(c, d, a);
    let d2 = cross(c, d, b);
    let d3 = cross(a, b, c);
    let d4 = cross(a, b, d);
    ((d1 > EPSILON && d2 < -EPSILON) || (d1 < -EPSILON && d2 > EPSILON))
        && ((d3 > EPSILON && d4 < -EPSILON) || (d3 < -EPSILON && d4 > EPSILON))
}

fn overlaps(a: &Geo, b: &Geo) -> Option<bool> {
    Some(match (a, b) {
        (Geo::Circle(c1, r1), Geo::Circle(c2, r2)) => fp_le(c1.dist(*c2), r1 + r2),
        (Geo::Polygon(p1), Geo::Polygon(p2)) => {
            let (h1, l1) = bbox(p1);
            let (h2, l2) = bbox(p2);
            if !(fp_le(l1.x, h2.x) && fp_le(l2.x, h1.x) && fp_le(l1.y, h2.y) && fp_le(l2.y, h1.y)) {
                return Some(false);
            }
            segments(p1, true).into_iter().any(|(x, y)| {
                segments(p2, true)
                    .into_iter()
                    .any(|(z, w)| segs_intersect(x, y, z, w))
            }) || p1.iter().any(|p| in_polygon(*p, p2))
                || p2.iter().any(|p| in_polygon(*p, p1))
        }
        _ => return None,
    })
}

fn same(a: &Geo, b: &Geo) -> Option<bool> {
    Some(match (a, b) {
        (Geo::Point(p), Geo::Point(q)) => p.same(*q),
        (Geo::Circle(c1, r1), Geo::Circle(c2, r2)) => c1.same(*c2) && fp_eq(*r1, *r2),
        (Geo::Polygon(p1), Geo::Polygon(p2)) => {
            if p1.len() != p2.len() {
                return Some(false);
            }
            let n = p1.len();
            // The same vertices in the same cyclic order, either direction.
            (0..n).any(|s| (0..n).all(|i| p1[i].same(p2[(s + i) % n])))
                || (0..n).any(|s| (0..n).all(|i| p1[i].same(p2[(s + n - i) % n])))
        }
        _ => return None,
    })
}

fn pmul(a: P, b: P) -> P {
    P {
        x: a.x * b.x - a.y * b.y,
        y: a.x * b.y + a.y * b.x,
    }
}

fn pdiv(a: P, b: P) -> Result<P> {
    let div = b.x * b.x + b.y * b.y;
    if div == 0.0 {
        return Err(Error::DivisionByZero);
    }
    Ok(P {
        x: (a.x * b.x + a.y * b.y) / div,
        y: (a.y * b.x - a.x * b.y) / div,
    })
}

fn map_points(g: &Geo, f: &dyn Fn(P) -> Result<P>) -> Result<Geo> {
    let all = |ps: &[P]| ps.iter().map(|p| f(*p)).collect::<Result<Vec<_>>>();
    Ok(match g {
        Geo::Point(p) => Geo::Point(f(*p)?),
        Geo::Box(h, l) => boxed(f(*h)?, f(*l)?),
        Geo::Path { closed, pts } => Geo::Path {
            closed: *closed,
            pts: all(pts)?,
        },
        Geo::Circle(c, r) => Geo::Circle(f(*c)?, *r),
        other => other.clone(),
    })
}

fn b(v: bool) -> Bson {
    Bson::Boolean(v)
}

fn d(v: f64) -> Bson {
    Bson::Double(v)
}

/// A binary operator between two values of which at least one is geometric;
/// `None` when this pair has no such operator here.
pub fn operator(op: &str, lhs: &Geo, rhs: &Geo) -> Option<Result<Bson>> {
    use Geo::*;
    let r = |g: Result<Geo>| Some(g.map(|g| to_bson(&g)));
    match (op, lhs, rhs) {
        ("<->", _, _) => distance(lhs, rhs).map(|v| Ok(d(v))),
        ("@>", _, _) => contains(lhs, rhs).map(|v| Ok(b(v))),
        ("<@", _, _) => contains(rhs, lhs).map(|v| Ok(b(v))),
        ("&&", _, _) => overlaps(lhs, rhs).map(|v| Ok(b(v))),
        ("~=", _, _) => same(lhs, rhs).map(|v| Ok(b(v))),
        // Point arithmetic is complex arithmetic; the shapes are translated
        // (+ -) or scaled-and-rotated (* /) point by point.
        ("+", Point(a), Point(q)) => r(Ok(Point(P {
            x: a.x + q.x,
            y: a.y + q.y,
        }))),
        ("-", Point(a), Point(q)) => r(Ok(Point(P {
            x: a.x - q.x,
            y: a.y - q.y,
        }))),
        ("*", Point(a), Point(q)) => r(Ok(Point(pmul(*a, *q)))),
        ("/", Point(a), Point(q)) => r(pdiv(*a, *q).map(Point)),
        ("+", Box(..) | Path { .. } | Circle(..), Point(q)) => {
            let q = *q;
            r(map_points(lhs, &|p| {
                Ok(P {
                    x: p.x + q.x,
                    y: p.y + q.y,
                })
            }))
        }
        ("-", Box(..) | Path { .. } | Circle(..), Point(q)) => {
            let q = *q;
            r(map_points(lhs, &|p| {
                Ok(P {
                    x: p.x - q.x,
                    y: p.y - q.y,
                })
            }))
        }
        ("*", Box(..) | Path { .. }, Point(q)) => {
            let q = *q;
            r(map_points(lhs, &|p| Ok(pmul(p, q))))
        }
        ("/", Box(..) | Path { .. }, Point(q)) => {
            let q = *q;
            r(map_points(lhs, &|p| pdiv(p, q)))
        }
        ("*", Circle(c, rad), Point(q)) => r(Ok(Circle(pmul(*c, *q), rad * q.x.hypot(q.y)))),
        ("/", Circle(c, rad), Point(q)) => {
            let scale = q.x.hypot(q.y);
            r(pdiv(*c, *q).map(|c| Circle(c, rad / scale)))
        }
        // Two OPEN paths concatenate; a closed one has no sum.
        (
            "+",
            Path {
                closed: false,
                pts: a,
            },
            Path {
                closed: false,
                pts: q,
            },
        ) => {
            let mut all = a.clone();
            all.extend(q.iter().copied());
            r(Ok(Path {
                closed: false,
                pts: all,
            }))
        }
        ("+", Path { .. }, Path { .. }) => Some(Ok(Bson::Null)),
        // Positional operators on points.
        ("<<", Point(a), Point(q)) => Some(Ok(b(fp_lt(a.x, q.x)))),
        (">>", Point(a), Point(q)) => Some(Ok(b(fp_gt(a.x, q.x)))),
        ("<^", Point(a), Point(q)) => Some(Ok(b(fp_lt(a.y, q.y)))),
        (">^", Point(a), Point(q)) => Some(Ok(b(fp_gt(a.y, q.y)))),
        ("?-", Point(a), Point(q)) => Some(Ok(b(fp_eq(a.y, q.y)))),
        ("?|", Point(a), Point(q)) => Some(Ok(b(fp_eq(a.x, q.x)))),
        // Circles compare by AREA.
        ("=" | "<>" | "!=" | "<" | "<=" | ">" | ">=", Circle(_, r1), Circle(_, r2)) => {
            let (a1, a2) = (r1 * r1, r2 * r2);
            Some(Ok(b(match op {
                "=" => fp_eq(a1, a2),
                "<>" | "!=" => !fp_eq(a1, a2),
                "<" => fp_lt(a1, a2),
                "<=" => fp_le(a1, a2),
                ">" => fp_gt(a1, a2),
                _ => fp_ge(a1, a2),
            })))
        }
        ("<<", Circle(c1, r1), Circle(c2, r2)) => Some(Ok(b(fp_lt(c1.x + r1, c2.x - r2)))),
        (">>", Circle(c1, r1), Circle(c2, r2)) => Some(Ok(b(fp_gt(c1.x - r1, c2.x + r2)))),
        ("<<|", Circle(c1, r1), Circle(c2, r2)) => Some(Ok(b(fp_lt(c1.y + r1, c2.y - r2)))),
        ("|>>", Circle(c1, r1), Circle(c2, r2)) => Some(Ok(b(fp_gt(c1.y - r1, c2.y + r2)))),
        // Line segments: equality by endpoints, ordering by length.
        ("=", Lseg(a1, a2), Lseg(b1, b2)) => Some(Ok(b(a1.same(*b1) && a2.same(*b2)))),
        ("<>" | "!=", Lseg(a1, a2), Lseg(b1, b2)) => Some(Ok(b(!(a1.same(*b1) && a2.same(*b2))))),
        ("<" | "<=" | ">" | ">=", Lseg(a1, a2), Lseg(b1, b2)) => {
            let (l1, l2) = (a1.dist(*a2), b1.dist(*b2));
            Some(Ok(b(match op {
                "<" => fp_lt(l1, l2),
                "<=" => fp_le(l1, l2),
                ">" => fp_gt(l1, l2),
                _ => fp_ge(l1, l2),
            })))
        }
        ("?#", Lseg(a1, a2), Lseg(b1, b2)) => Some(Ok(b(segs_intersect(*a1, *a2, *b1, *b2)))),
        // `inter_sb`: an endpoint inside the box, or the segment crossing
        // one of its edges.
        ("?#", Lseg(a1, a2), Box(h, l)) => {
            let corners = box_pts(*h, *l);
            Some(Ok(b(in_box(*a1, *h, *l)
                || in_box(*a2, *h, *l)
                || (0..4).any(|i| {
                    segs_intersect(*a1, *a2, corners[i], corners[(i + 1) % 4])
                }))))
        }
        ("#", Lseg(a1, a2), Lseg(b1, b2)) => Some(Ok(match seg_intersection(*a1, *a2, *b1, *b2) {
            Some(p) => to_bson(&Point(p)),
            None => Bson::Null,
        })),
        ("?-|", Lseg(a1, a2), Lseg(b1, b2)) => {
            let dot = (a2.x - a1.x) * (b2.x - b1.x) + (a2.y - a1.y) * (b2.y - b1.y);
            Some(Ok(b(fp_eq(dot, 0.0))))
        }
        ("?||", Lseg(a1, a2), Lseg(b1, b2)) => {
            let cr = (a2.x - a1.x) * (b2.y - b1.y) - (a2.y - a1.y) * (b2.x - b1.x);
            Some(Ok(b(fp_eq(cr, 0.0))))
        }
        ("=", Line(a1, b1, c1), Line(a2, b2, c2)) => {
            // Proportional coefficients.
            let k = if !fp_eq(*a2, 0.0) {
                a1 / a2
            } else if !fp_eq(*b2, 0.0) {
                b1 / b2
            } else {
                c1 / c2
            };
            Some(Ok(b(fp_eq(*a1, k * a2)
                && fp_eq(*b1, k * b2)
                && fp_eq(*c1, k * c2))))
        }
        ("?#", Line(a1, b1, _), Line(a2, b2, _)) => Some(Ok(b(!fp_eq(a1 * b2, a2 * b1)))),
        ("?||", Line(a1, b1, _), Line(a2, b2, _)) => Some(Ok(b(fp_eq(a1 * b2, a2 * b1)))),
        ("?-|", Line(a1, b1, _), Line(a2, b2, _)) => Some(Ok(b(fp_eq(a1 * a2 + b1 * b2, 0.0)))),
        (
            "?#",
            Path {
                closed: c1,
                pts: p1,
            },
            Path {
                closed: c2,
                pts: p2,
            },
        ) => Some(Ok(b(segments(p1, *c1).into_iter().any(|(x, y)| {
            segments(p2, *c2)
                .into_iter()
                .any(|(z, w)| segs_intersect(x, y, z, w))
        })))),
        // Positional operators on polygons, by bounding box.
        ("<<" | ">>" | "&<" | "&>" | "<<|" | "|>>" | "&<|" | "|&>", Polygon(p1), Polygon(p2)) => {
            let (h1, l1) = bbox(p1);
            let (h2, l2) = bbox(p2);
            Some(Ok(b(match op {
                "<<" => fp_lt(h1.x, l2.x),
                ">>" => fp_gt(l1.x, h2.x),
                "&<" => fp_le(h1.x, h2.x),
                "&>" => fp_ge(l1.x, l2.x),
                "<<|" => fp_lt(h1.y, l2.y),
                "|>>" => fp_gt(l1.y, h2.y),
                "&<|" => fp_le(h1.y, h2.y),
                _ => fp_ge(l1.y, l2.y),
            })))
        }
        _ => None,
    }
}

fn seg_intersection(a: P, b: P, c: P, d: P) -> Option<P> {
    if !segs_intersect(a, b, c, d) {
        return None;
    }
    let den = (a.x - b.x) * (c.y - d.y) - (a.y - b.y) * (c.x - d.x);
    if den == 0.0 {
        return None;
    }
    let t = ((a.x - c.x) * (c.y - d.y) - (a.y - c.y) * (c.x - d.x)) / den;
    Some(P {
        x: a.x + t * (b.x - a.x),
        y: a.y + t * (b.y - a.y),
    })
}

/// A prefix operator on a geometric value.
pub fn unary(op: &str, g: &Geo) -> Option<Result<Bson>> {
    Some(Ok(match (op, g) {
        ("@-@", _) => d(length(g)?),
        ("@@", _) => to_bson(&Geo::Point(center(g)?)),
        ("#", Geo::Path { pts, .. } | Geo::Polygon(pts)) => Bson::Int32(pts.len() as i32),
        ("?-", Geo::Lseg(a, b2)) => b(fp_eq(a.y, b2.y)),
        ("?|", Geo::Lseg(a, b2)) => b(fp_eq(a.x, b2.x)),
        ("?-", Geo::Line(a, _, _)) => b(fp_eq(*a, 0.0)),
        ("?|", Geo::Line(_, bb, _)) => b(fp_eq(*bb, 0.0)),
        _ => return None,
    }))
}

/// `point[0]` / `point[1]`, and a box / lseg's corner points.
pub fn subscript(g: &Geo, i: i64) -> Option<Bson> {
    match g {
        Geo::Point(p) => match i {
            0 => Some(d(p.x)),
            1 => Some(d(p.y)),
            _ => Some(Bson::Null),
        },
        Geo::Lseg(a, b2) | Geo::Box(a, b2) => match i {
            0 => Some(to_bson(&Geo::Point(*a))),
            1 => Some(to_bson(&Geo::Point(*b2))),
            _ => Some(Bson::Null),
        },
        _ => None,
    }
}

/// An explicit cast between geometric types (or to text).
pub fn cast(g: &Geo, target: &str) -> Result<Bson> {
    let no = || {
        Error::CannotCoerce(format!(
            "cannot cast type {} to {}",
            g.type_name(),
            crate::display_type(target)
        ))
    };
    let out = match (g, target) {
        (_, "text" | "varchar" | "bpchar" | "name") => return Ok(Bson::String(text(g))),
        (_, t) if t == g.type_name() => g.clone(),
        (Geo::Box(..) | Geo::Circle(..) | Geo::Lseg(..) | Geo::Polygon(_), "point") => {
            Geo::Point(center(g).ok_or_else(no)?)
        }
        (Geo::Box(h, l), "lseg") => Geo::Lseg(*l, *h),
        (Geo::Box(h, l), "polygon") => Geo::Polygon(box_pts(*h, *l)),
        (Geo::Box(h, l), "circle") => {
            let c = center(g).ok_or_else(no)?;
            Geo::Circle(c, c.dist(*h).max(c.dist(*l)))
        }
        (Geo::Circle(c, r), "box") => {
            let k = r / std::f64::consts::SQRT_2;
            boxed(
                P {
                    x: c.x + k,
                    y: c.y + k,
                },
                P {
                    x: c.x - k,
                    y: c.y - k,
                },
            )
        }
        (Geo::Circle(..), "polygon") => circle_polygon(12, g)?,
        (Geo::Path { closed: false, .. }, "polygon") => {
            return Err(Error::Sqlstate(
                "22023",
                "open path cannot be converted to polygon".into(),
            ))
        }
        (Geo::Path { pts, .. }, "polygon") => Geo::Polygon(pts.clone()),
        (Geo::Polygon(ps), "path") => Geo::Path {
            closed: true,
            pts: ps.clone(),
        },
        (Geo::Polygon(ps), "box") => {
            let (h, l) = bbox(ps);
            Geo::Box(h, l)
        }
        (Geo::Polygon(ps), "circle") => {
            let c = center(g).ok_or_else(no)?;
            let r = ps.iter().map(|p| c.dist(*p)).sum::<f64>() / ps.len() as f64;
            Geo::Circle(c, r)
        }
        _ => return Err(no()),
    };
    Ok(to_bson(&out))
}

fn circle_polygon(n: i64, g: &Geo) -> Result<Geo> {
    let Geo::Circle(c, r) = g else {
        return Err(Error::Internal("not a circle".into()));
    };
    if n < 2 {
        return Err(Error::Sqlstate(
            "22023",
            "must request at least 2 points".into(),
        ));
    }
    if *r == 0.0 {
        return Err(Error::Sqlstate(
            "0A000",
            "cannot convert circle with radius zero to polygon".into(),
        ));
    }
    let step = 2.0 * std::f64::consts::PI / n as f64;
    Ok(Geo::Polygon(
        (0..n)
            .map(|i| {
                let angle = i as f64 * step;
                P {
                    x: c.x - r * angle.cos(),
                    y: c.y + r * angle.sin(),
                }
            })
            .collect(),
    ))
}

/// The functions, by name.
pub const FUNCTIONS: &[&str] = &[
    "box",
    "point",
    "geom_length",
    "lseg",
    "line",
    "path",
    "polygon",
    "circle",
    "area",
    "center",
    "radius",
    "diameter",
    "height",
    "width",
    "length",
    "npoints",
    "isclosed",
    "isopen",
    "pclose",
    "popen",
    "diagonal",
    "bound_box",
    "slope",
    "isvertical",
    "ishorizontal",
    "isparallel",
    "isperp",
];

fn num(v: &Bson) -> Option<f64> {
    match v {
        Bson::Double(d) => Some(*d),
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Decimal128(_) | Bson::String(_) => crate::value_text(v).parse().ok(),
        _ => None,
    }
}

/// A function over geometric arguments; `None` when these arguments are not
/// one of its signatures (so another implementation may take the name).
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !FUNCTIONS.contains(&name) {
        return None;
    }
    let geos: Vec<Option<Geo>> = args.iter().map(from_bson).collect();
    // Every one of these is strict.
    if args.contains(&Bson::Null) && name != "length" {
        // (`length(NULL)` is text length's too, and answers NULL either way.)
        return Some(Ok(Bson::Null));
    }
    let out = |g: Geo| Some(Ok(to_bson(&g)));
    match (name, geos.as_slice()) {
        ("point", [None, None]) => {
            let (x, y) = (num(&args[0])?, num(&args[1])?);
            out(Geo::Point(P { x, y }))
        }
        ("point", [Some(g)]) => Some(cast(g, "point")),
        ("lseg", [Some(Geo::Point(a)), Some(Geo::Point(q))]) => out(Geo::Lseg(*a, *q)),
        ("lseg", [Some(g @ Geo::Box(..))]) => Some(cast(g, "lseg")),
        ("line", [Some(Geo::Point(a)), Some(Geo::Point(q))]) => {
            if a.same(*q) {
                return Some(Err(Error::Sqlstate(
                    "22023",
                    "invalid line specification: must be two distinct points".into(),
                )));
            }
            out(line_from(*a, *q))
        }
        ("path", [Some(g)]) => Some(cast(g, "path")),
        ("polygon", [Some(g)]) => Some(cast(g, "polygon")),
        ("polygon", [None, Some(g @ Geo::Circle(..))]) => {
            let n = num(&args[0])? as i64;
            Some(circle_polygon(n, g).map(|p| to_bson(&p)))
        }
        ("circle", [Some(Geo::Point(c)), None]) => {
            let r = num(&args[1])?;
            out(Geo::Circle(*c, r))
        }
        ("circle", [Some(g)]) => Some(cast(g, "circle")),
        ("area", [Some(g @ (Geo::Box(..) | Geo::Circle(..) | Geo::Path { .. }))]) => {
            Some(Ok(area(g).map_or(Bson::Null, d)))
        }
        ("area", [Some(g)]) => Some(Err(Error::UndefinedFunction(format!(
            "function area({}) does not exist",
            g.type_name()
        )))),
        ("box", [Some(Geo::Point(a))]) => out(boxed(*a, *a)),
        ("box", [Some(g)]) => Some(cast(g, "box")),
        ("box", [Some(Geo::Point(a)), Some(Geo::Point(q))]) => out(boxed(*a, *q)),
        ("center", [Some(g @ (Geo::Box(..) | Geo::Circle(..)))]) => out(Geo::Point(center(g)?)),
        ("radius", [Some(Geo::Circle(_, r))]) => Some(Ok(d(*r))),
        ("diameter", [Some(Geo::Circle(_, r))]) => Some(Ok(d(2.0 * r))),
        ("height", [Some(Geo::Box(h, l))]) => Some(Ok(d(h.y - l.y))),
        ("width", [Some(Geo::Box(h, l))]) => Some(Ok(d(h.x - l.x))),
        ("length" | "geom_length", [Some(g)]) => length(g).map(|v| Ok(d(v))),
        ("npoints", [Some(Geo::Path { pts, .. } | Geo::Polygon(pts))]) => {
            Some(Ok(Bson::Int32(pts.len() as i32)))
        }
        ("isclosed", [Some(Geo::Path { closed, .. })]) => Some(Ok(b(*closed))),
        ("isopen", [Some(Geo::Path { closed, .. })]) => Some(Ok(b(!closed))),
        ("pclose", [Some(Geo::Path { pts, .. })]) => out(Geo::Path {
            closed: true,
            pts: pts.clone(),
        }),
        ("popen", [Some(Geo::Path { pts, .. })]) => out(Geo::Path {
            closed: false,
            pts: pts.clone(),
        }),
        ("diagonal", [Some(g @ Geo::Box(..))]) => Some(cast(g, "lseg")),
        ("bound_box", [Some(Geo::Box(h1, l1)), Some(Geo::Box(h2, l2))]) => out(boxed(
            P {
                x: h1.x.max(h2.x),
                y: h1.y.max(h2.y),
            },
            P {
                x: l1.x.min(l2.x),
                y: l1.y.min(l2.y),
            },
        )),
        ("slope", [Some(Geo::Point(a)), Some(Geo::Point(q))]) => Some(Ok(d(if fp_eq(a.x, q.x) {
            f64::INFINITY
        } else {
            (q.y - a.y) / (q.x - a.x)
        }))),
        ("isvertical", [Some(Geo::Point(a)), Some(Geo::Point(q))]) => Some(Ok(b(fp_eq(a.x, q.x)))),
        ("ishorizontal", [Some(Geo::Point(a)), Some(Geo::Point(q))]) => {
            Some(Ok(b(fp_eq(a.y, q.y))))
        }
        ("isvertical" | "ishorizontal", [Some(g)]) => {
            unary(if name == "isvertical" { "?|" } else { "?-" }, g)
        }
        ("isparallel", [Some(a), Some(q)]) => operator("?||", a, q),
        ("isperp", [Some(a), Some(q)]) => operator("?-|", a, q),
        _ => None,
    }
}

/// The result type of a geometric function, when `name` is one.
pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "point" | "center" => "point",
        "lseg" | "diagonal" => "lseg",
        "line" => "line",
        "path" | "pclose" | "popen" => "path",
        "polygon" => "polygon",
        "circle" => "circle",
        "bound_box" | "box" => "box",
        "area" | "radius" | "diameter" | "height" | "width" | "slope" | "geom_length" => "float8",
        "npoints" => "int4",
        "isclosed" | "isopen" | "isvertical" | "ishorizontal" | "isparallel" | "isperp" => "bool",
        _ => return None,
    })
}

/// The result type of an operator over these operand types, when it is
/// geometric.
pub fn operator_type(op: &str, lt: &str, rt: &str) -> Option<&'static str> {
    let geo_t = |t: &str| TYPES.contains(&t) || t == "box";
    if !(geo_t(lt) || geo_t(rt)) {
        return None;
    }
    Some(match op {
        "<->" => "float8",
        "+" | "-" | "*" | "/" if geo_t(lt) => match lt {
            "point" => "point",
            "box" => "box",
            "path" => "path",
            "circle" => "circle",
            _ => return None,
        },
        "#" if lt == "lseg" || lt == "line" => "point",
        _ => "bool",
    })
}

/// The result type of a prefix operator over a geometric operand.
pub fn unary_type(op: &str, t: &str) -> Option<&'static str> {
    if !(TYPES.contains(&t) || t == "box") {
        return None;
    }
    Some(match op {
        "@-@" => "float8",
        "@@" => "point",
        "#" => "int4",
        "?-" | "?|" => "bool",
        _ => return None,
    })
}
