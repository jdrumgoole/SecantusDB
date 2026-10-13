//! Geo shapes as mongod parses and tests them.
//!
//! Two families, and mongod keeps them apart:
//!
//! * the FLAT shapes of the legacy operators (`$box`, `$polygon`, `$center`)
//!   are tested in the plane, and only a POINT is ever inside one: a legacy
//!   pair, a two-number document, or a GeoJSON `Point`;
//! * `$geometry` and `$centerSphere` are tested on the sphere. An edge is the
//!   great-circle arc between its ends, so `[1, 1]` is not on the line from
//!   `[0, 0]` to `[2, 2]`, and a polygon across the antimeridian is the small
//!   one.
//!
//! mongod's own test of a point against a polygon is CLOSED: a point on an
//! edge or a vertex is inside (it asks whether the polygon may intersect the
//! point's leaf cell). So is every flat shape.
//!
//! Every message here is one mongod 8.2.11 gave for the same argument
//! (`tools/probes/geo_queries.py`).

use crate::fallback::Fallback;
use crate::query::{bson_type_name, bson_value_repr};
use bson::{Bson, Document};

/// `(x, y)`: longitude and latitude in degrees for a shape on the sphere.
pub type P = (f64, f64);

/// A GeoJSON geometry.
#[derive(Clone, Debug)]
pub enum Geom {
    Point(P),
    Line(Vec<P>),
    /// The outer loop, then the holes. Each loop without its closing vertex.
    Polygon(Vec<Vec<P>>),
    /// `MultiPoint`, `MultiLineString`, `MultiPolygon`, `GeometryCollection`.
    Multi(Vec<Geom>),
}

/// What a geo query tests documents against.
#[derive(Clone, Debug)]
pub enum Shape {
    Box(P, P),
    Polygon(Vec<P>),
    Circle(P, f64),
    /// A centre and a radius in radians.
    Cap(P, f64),
    Geo(Geom),
}

type Parsed<T> = Result<T, Fallback>;

fn bad<T>(message: impl Into<String>) -> Parsed<T> {
    Err(Fallback::mongo(2, message))
}

fn num(b: &Bson) -> Option<f64> {
    match b {
        Bson::Double(d) => Some(*d),
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        _ => None,
    }
}

/// The members of an array or a document, in order.
fn members(v: &Bson) -> Option<Vec<&Bson>> {
    match v {
        Bson::Array(a) => Some(a.iter().collect()),
        Bson::Document(d) => Some(d.values().collect()),
        _ => None,
    }
}

fn type_of(v: Option<&Bson>) -> &'static str {
    v.map_or("missing", bson_type_name)
}

/// A point as the legacy operators take one: an array or a document whose
/// first two members are numbers. Anything after them is ignored.
fn flat_point(v: Option<&Bson>) -> Parsed<P> {
    let Some(parts) = v.and_then(members) else {
        return bad(format!(
            "Point must be an array or object, instead got type {}",
            type_of(v)
        ));
    };
    let coord = |at: usize| -> Parsed<f64> {
        let part = parts.get(at).copied();
        part.and_then(num).ok_or_else(|| {
            Fallback::mongo(
                2,
                format!(
                    "Point must only contain numeric elements, instead got type {}",
                    type_of(part)
                ),
            )
        })
    };
    Ok((coord(0)?, coord(1)?))
}

fn in_bounds(p: P) -> Parsed<P> {
    if (-180.0..=180.0).contains(&p.0) && (-90.0..=90.0).contains(&p.1) {
        Ok(p)
    } else {
        bad(format!(
            "Longitude/latitude is out of bounds, lng: {} lat: {}",
            p.0, p.1
        ))
    }
}

fn radius(v: Option<&Bson>) -> Parsed<f64> {
    match v.and_then(num) {
        Some(r) if r >= 0.0 => Ok(r),
        _ => bad(format!(
            "Radius must be a non-negative number: {}",
            v.map_or("EOO".to_string(), bson_value_repr)
        )),
    }
}

fn circle(v: &Bson) -> Parsed<(P, f64)> {
    let parts = members(v).unwrap_or_default();
    let centre = flat_point(parts.first().copied())?;
    let r = radius(parts.get(1).copied())?;
    if parts.len() > 2 {
        return bad("Only 2 fields allowed for circular region, but more were provided");
    }
    Ok((centre, r))
}

fn coordinates(v: &Bson) -> String {
    bson_value_repr(v)
}

/// The vertices of a GeoJSON line or loop.
fn vertices(v: &Bson) -> Parsed<Vec<P>> {
    let Some(list) = v.as_array() else {
        return bad(format!(
            "GeoJSON coordinates must be an array of coordinates, instead got type {}",
            bson_type_name(v)
        ));
    };
    list.iter()
        .map(|p| flat_point(Some(p)).and_then(in_bounds))
        .collect()
}

fn degrees(p: P) -> String {
    format!("[{:.7}, {:.7}]", p.1, p.0)
}

/// One loop of a GeoJSON polygon, without its closing vertex.
fn polygon_loop(v: &Bson) -> Parsed<Vec<P>> {
    let mut points = vertices(v)?;
    if points.len() < 2 || points.first() != points.last() {
        return bad(format!(
            "Loop is not closed, first vertex does not equal last vertex: {}",
            coordinates(v)
        ));
    }
    points.pop();
    let mut unique: Vec<P> = Vec::new();
    for p in &points {
        if !unique.contains(p) {
            unique.push(*p);
        }
    }
    if unique.len() < 3 {
        return bad(format!(
            "Loop must have at least 3 different vertices, {} unique vertices were provided: {}",
            unique.len(),
            coordinates(v)
        ));
    }
    let n = points.len();
    let edge = |i: usize| (points[i], points[(i + 1) % n]);
    for i in 0..n {
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            let ((a, b), (c, d)) = (edge(i), edge(j));
            if sphere::arcs_cross(
                sphere::unit(a),
                sphere::unit(b),
                sphere::unit(c),
                sphere::unit(d),
            ) {
                return bad(format!(
                    "Loop is not valid: {} Edges {i} and {j} cross. Edge locations in degrees: \
                     {}-{} and {}-{}",
                    coordinates(v),
                    degrees(a),
                    degrees(b),
                    degrees(c),
                    degrees(d)
                ));
            }
        }
    }
    Ok(points)
}

fn polygon(v: Option<&Bson>) -> Parsed<Geom> {
    let Some(Bson::Array(loops)) = v else {
        return bad(format!(
            "Polygon coordinates must be an array, instead got type {}",
            type_of(v)
        ));
    };
    if loops.is_empty() {
        return bad("Polygon has no loops.");
    }
    Ok(Geom::Polygon(
        loops.iter().map(polygon_loop).collect::<Parsed<_>>()?,
    ))
}

fn line(v: Option<&Bson>) -> Parsed<Geom> {
    let Some(v) = v else {
        return bad("GeoJSON LineString must have at least 2 vertices, instead got 0 vertices: []");
    };
    // A vertex repeated straight after itself is one vertex.
    let mut points = vertices(v)?;
    points.dedup();
    if points.len() < 2 {
        return bad(format!(
            "GeoJSON LineString must have at least 2 vertices, instead got {} vertices: {}",
            points.len(),
            coordinates(v)
        ));
    }
    Ok(Geom::Line(points))
}

fn each<'a>(v: Option<&'a Bson>, what: &str) -> Parsed<&'a Vec<Bson>> {
    match v {
        Some(Bson::Array(list)) => Ok(list),
        _ => bad(format!(
            "{what} coordinates must be an array, instead got type {}",
            type_of(v)
        )),
    }
}

/// A GeoJSON object.
pub fn geojson(d: &Document) -> Parsed<Geom> {
    let coords = d.get("coordinates");
    match d.get_str("type") {
        Ok("Point") => Ok(Geom::Point(in_bounds(flat_point(coords)?)?)),
        Ok("LineString") => line(coords),
        Ok("Polygon") => polygon(coords),
        Ok("MultiPoint") => Ok(Geom::Multi(
            each(coords, "MultiPoint")?
                .iter()
                .map(|p| Ok(Geom::Point(in_bounds(flat_point(Some(p))?)?)))
                .collect::<Parsed<_>>()?,
        )),
        Ok("MultiLineString") => Ok(Geom::Multi(
            each(coords, "MultiLineString")?
                .iter()
                .map(|l| line(Some(l)))
                .collect::<Parsed<_>>()?,
        )),
        Ok("MultiPolygon") => Ok(Geom::Multi(
            each(coords, "MultiPolygon")?
                .iter()
                .map(|p| polygon(Some(p)))
                .collect::<Parsed<_>>()?,
        )),
        Ok("GeometryCollection") => Ok(Geom::Multi(
            each(d.get("geometries"), "GeometryCollection")?
                .iter()
                .map(|g| match g {
                    Bson::Document(inner) => geojson(inner),
                    other => bad(format!("unknown GeoJSON type: {}", bson_value_repr(other))),
                })
                .collect::<Parsed<_>>()?,
        )),
        _ => bad(format!(
            "unknown GeoJSON type: {}",
            bson_value_repr(&Bson::Document(d.clone()))
        )),
    }
}

/// One shape specifier of a geo query (`$box: ...`), or `None` for a key
/// that is not one.
fn specifier(key: &str, v: &Bson) -> Option<Parsed<Shape>> {
    if !matches!(
        key,
        "$box" | "$polygon" | "$center" | "$centerSphere" | "$geometry"
    ) {
        return None;
    }
    if !matches!(v, Bson::Array(_) | Bson::Document(_)) {
        return Some(bad(format!(
            "unknown geo specifier: {key}: {}",
            bson_value_repr(v)
        )));
    }
    let parts = members(v).unwrap_or_default();
    Some(match key {
        "$box" => (|| {
            Ok(Shape::Box(
                flat_point(parts.first().copied())?,
                flat_point(parts.get(1).copied())?,
            ))
        })(),
        "$polygon" => (|| {
            if parts.len() < 3 {
                return bad(format!(
                    "Polygon must have at least 3 points, instead got {} vertices",
                    parts.len()
                ));
            }
            Ok(Shape::Polygon(
                parts
                    .iter()
                    .map(|p| flat_point(Some(p)))
                    .collect::<Parsed<_>>()?,
            ))
        })(),
        "$center" => circle(v).map(|(c, r)| Shape::Circle(c, r)),
        "$centerSphere" => circle(v).and_then(|(c, r)| Ok(Shape::Cap(in_bounds(c)?, r))),
        _ => match v {
            Bson::Document(d) => geojson(d).map(Shape::Geo),
            // A legacy pair stands for a point.
            other => flat_point(Some(other))
                .and_then(in_bounds)
                .map(|p| Shape::Geo(Geom::Point(p))),
        },
    })
}

fn echoed(op: &str, arg: &Document) -> String {
    format!(
        "{{ {op}: {} }}",
        bson_value_repr(&Bson::Document(arg.clone()))
    )
}

/// The shape of a `$geoWithin` (or `$within`) query.
pub fn within_shape(op: &str, arg: &Bson) -> Parsed<Shape> {
    let Bson::Document(arg) = arg else {
        return bad("geometry must be an object");
    };
    let mut shape: Option<Shape> = None;
    for (key, v) in arg {
        if shape.is_some() {
            return bad(format!(
                "$geoWithin doesn't accept multiple shapes {op}: {}",
                bson_value_repr(&Bson::Document(arg.clone()))
            ));
        }
        match specifier(key, v) {
            Some(parsed) => shape = Some(parsed?),
            None if key == "$uniqueDocs" => {}
            None => {
                return bad(format!(
                    "unknown geo specifier: {key}: {}",
                    bson_value_repr(v)
                ))
            }
        }
    }
    let Some(shape) = shape else {
        return bad("geo query doesn't have any geometry");
    };
    // Only something with an inside can have a document within it.
    let area = |g: &Geom| match g {
        Geom::Polygon(_) => true,
        Geom::Multi(parts) => {
            !parts.is_empty() && parts.iter().all(|p| matches!(p, Geom::Polygon(_)))
        }
        _ => false,
    };
    if matches!(&shape, Shape::Geo(g) if !area(g)) {
        return bad(format!(
            "$within not supported with provided geometry: {}",
            echoed(op, arg)
        ));
    }
    Ok(shape)
}

/// The geometry of a `$geoIntersects` query.
pub fn intersects_shape(op: &str, arg: &Bson) -> Parsed<Geom> {
    let Bson::Document(arg) = arg else {
        return bad("geometry must be an object");
    };
    let mut shape: Option<Shape> = None;
    for (key, v) in arg {
        match specifier(key, v) {
            Some(parsed) => shape = Some(parsed?),
            None if key == "$uniqueDocs" => {}
            None => {
                return bad(format!(
                    "unknown geo specifier: {key}: {}",
                    bson_value_repr(v)
                ))
            }
        }
    }
    match shape {
        None => bad("geo query doesn't have any geometry"),
        Some(Shape::Geo(g)) => Ok(g),
        Some(_) => bad(format!(
            "$geoIntersect not supported with provided geometry: {}",
            echoed(op, arg)
        )),
    }
}

/// The geometries a stored value holds: one, or for an array of legacy
/// points (`[[1, 1], [30, 30]]`) one each, any of which may match. Lenient:
/// a value mongod could not index is simply not matched.
pub fn stored(v: &Bson) -> Vec<Geom> {
    let one = |v: &Bson| match v {
        Bson::Document(d) if d.get("type").is_some_and(|t| matches!(t, Bson::String(_))) => {
            geojson(d).ok()
        }
        Bson::Document(_) | Bson::Array(_) => flat_point(Some(v)).ok().map(Geom::Point),
        _ => None,
    };
    match v {
        Bson::Array(list) if list.first().is_some_and(|p| num(p).is_none()) => {
            list.iter().filter_map(one).collect()
        }
        other => one(other).into_iter().collect(),
    }
}

/// Every vertex of a geometry.
fn all_points(g: &Geom, out: &mut Vec<P>) {
    match g {
        Geom::Point(p) => out.push(*p),
        Geom::Line(l) => out.extend(l),
        Geom::Polygon(loops) => out.extend(loops.iter().flatten()),
        Geom::Multi(parts) => parts.iter().for_each(|g| all_points(g, out)),
    }
}

/// The points a stored value holds, for a `2d` index: a legacy pair, or
/// each of several.
pub fn stored_points(v: &Bson) -> Vec<P> {
    stored(v)
        .into_iter()
        .filter_map(|g| match g {
            Geom::Point(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// The box around each geometry a stored value holds: `(min x, min y, max x,
/// max y)`, a point's box being the point.
pub fn stored_boxes(v: &Bson) -> Vec<(f64, f64, f64, f64)> {
    stored(v)
        .iter()
        .filter_map(|g| {
            let mut points = Vec::new();
            all_points(g, &mut points);
            let first = *points.first()?;
            Some(
                points
                    .iter()
                    .fold((first.0, first.1, first.0, first.1), |b, p| {
                        (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1))
                    }),
            )
        })
        .collect()
}

// ------------------------------------------------------- what an index holds

/// Why a `2dsphere` index cannot hold `v`, the value of its field in a
/// document being written: mongod's reason, or `None` when it can (or has
/// nothing to index: null, a missing field, an empty array or document).
pub fn sphere_key_problem(field: &str, v: Option<&Bson>) -> Option<String> {
    let reason = |f: Fallback| match f {
        Fallback::Mongo { message, .. } => message,
        Fallback::Defer => "unknown GeoJSON type".to_string(),
    };
    let legacy = |v: &Bson| -> Option<String> {
        match flat_point(Some(v)) {
            Err(f) => Some(reason(f)),
            Ok(p) if in_bounds(p).is_err() => Some(format!(
                "can't project geometry into spherical CRS: {}",
                bson_value_repr(v)
            )),
            Ok(_) => None,
        }
    };
    match v? {
        Bson::Null | Bson::Undefined => None,
        Bson::Array(list) if list.is_empty() => None,
        Bson::Document(d) if d.is_empty() => None,
        v @ Bson::Array(_) => legacy(v),
        v @ Bson::Document(d) => {
            let numbers = d.values().take(2).filter(|x| num(x).is_some()).count();
            if d.contains_key("type") {
                geojson(d).err().map(reason)
            } else if numbers == 2 {
                legacy(v)
            } else {
                Some(format!("unknown GeoJSON type: {}", bson_value_repr(v)))
            }
        }
        other => Some(format!(
            "geo element must be an array or object: {field}: {}",
            bson_value_repr(other)
        )),
    }
}

/// Why a `2d` index over `[lo, hi]` cannot hold `v`: mongod's code and
/// reason, and whether it names the document after it.
pub fn flat_key_problem(v: Option<&Bson>, lo: f64, hi: f64) -> Option<(i32, String, bool)> {
    const SHAPE: (i32, &str, bool) = (
        16804,
        "location object expected, location array not in correct format",
        false,
    );
    const PAIRS: (i32, &str, bool) = (
        13026,
        "geo values must be 'legacy coordinate pairs' for 2d indexes",
        true,
    );
    let own = |(code, text, names): (i32, &str, bool)| Some((code, text.to_string(), names));
    let pair = |v: &Bson, in_array: bool| -> Option<(i32, String, bool)> {
        let parts = members(v)?;
        let first = parts.first().copied()?;
        if num(first).is_none() {
            return own(if in_array { PAIRS } else { SHAPE });
        }
        let Some(second) = parts.get(1).copied() else {
            return Some((13068, "geo field only has 1 element".to_string(), true));
        };
        let (Some(x), Some(y)) = (num(first), num(second)) else {
            return own(PAIRS);
        };
        if [x, y].iter().any(|c| *c < lo || *c > hi) {
            return Some((
                13027,
                format!("point not in interval of [ {lo}, {hi} ]"),
                true,
            ));
        }
        None
    };
    let v = v?;
    let parts = members(v)?;
    match parts.first().copied() {
        None => None,
        // Locations inside the value, each a pair of its own: an array of
        // them, or a document whose members are.
        Some(Bson::Array(_) | Bson::Document(_)) => parts.iter().find_map(|p| match p {
            Bson::Array(_) | Bson::Document(_) => pair(p, true),
            _ => own(PAIRS),
        }),
        Some(_) => pair(v, matches!(v, Bson::Array(_))),
    }
}

// ----------------------------------------------------------------- $near

/// mongod's radius of the Earth, in metres.
pub const EARTH_RADIUS_METERS: f64 = 6_378_100.0;

/// A `$near` / `$nearSphere` query.
#[derive(Clone, Debug)]
pub struct Near {
    pub centre: P,
    /// Distances are along the sphere, in metres; otherwise in the plane, in
    /// the units of the coordinates.
    pub spherical: bool,
    /// The centre was given as GeoJSON, which only a `2dsphere` index reads.
    pub geojson: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

fn bound(v: &Bson, name: &str, codes: (i32, i32)) -> Parsed<f64> {
    match num(v) {
        None => Err(Fallback::mongo(codes.0, format!("{name} must be a number"))),
        Some(n) if n < 0.0 => Err(Fallback::mongo(
            codes.1,
            format!("{name} must be non-negative"),
        )),
        Some(n) => Ok(n),
    }
}

/// The `$near` query a condition document holds (`{$near: ..., $maxDistance:
/// ...}`), or `None` when it holds none. Nothing else may share the document.
pub fn near_condition(cond: &Document) -> Parsed<Option<Near>> {
    let is_near = |k: &str| matches!(k, "$near" | "$nearSphere" | "$geoNear");
    if !cond.keys().any(|k| is_near(k)) {
        return Ok(None);
    }
    let whole = || bson_value_repr(&Bson::Document(cond.clone()));
    let Some((op, arg)) = cond.iter().next().filter(|(k, _)| is_near(k)) else {
        return bad(format!("near must be first in: {}", whole()));
    };
    let mut spherical = op == "$nearSphere" || cond.contains_key("$nearSphere");
    let (mut min, mut max) = (None, None);
    let geometry = match arg {
        Bson::Document(d) => d.get("$geometry"),
        _ => None,
    };
    let (centre, geojson) = if let (Bson::Document(d), Some(g)) = (arg, geometry) {
        let shown = bson_value_repr(g);
        let wrong = |why: String| {
            Fallback::mongo(
                2,
                format!("invalid point in geo near query $geometry argument: {shown}  {why}"),
            )
        };
        let reason = |f: Fallback| match f {
            Fallback::Mongo { message, .. } => wrong(message),
            other => other,
        };
        let centre = match g {
            Bson::Document(gd) => match gd.get_str("type") {
                Ok("Point") | Err(_) => {
                    in_bounds(flat_point(gd.get("coordinates")).map_err(reason)?).map_err(reason)?
                }
                Ok(other) => {
                    return Err(wrong(format!(
                        "Expected geojson geometry with type Point, but got type {other}"
                    )))
                }
            },
            other => in_bounds(flat_point(Some(other)).map_err(reason)?).map_err(reason)?,
        };
        for (k, v) in d {
            match k.as_str() {
                "$geometry" => {}
                "$maxDistance" => max = Some(bound(v, "$maxDistance", (2, 2))?),
                "$minDistance" => min = Some(bound(v, "$minDistance", (2, 2))?),
                other => return bad(format!("invalid argument in geo near query: {other}")),
            }
        }
        spherical = true;
        (centre, true)
    } else {
        let Some(parts) = members(arg) else {
            return bad(format!("near must be first in: {}", whole()));
        };
        if parts.is_empty() {
            return bad("$geometry is required for geo near query");
        }
        let (Some(x), Some(y)) = (
            parts.first().and_then(|v| num(v)),
            parts.get(1).and_then(|v| num(v)),
        ) else {
            return bad("invalid argument in geo near query: 0");
        };
        if let Some(third) = parts.get(2).and_then(|v| num(v)) {
            max = Some(third);
        }
        ((x, y), false)
    };
    for (k, v) in cond.iter().skip(1) {
        match k.as_str() {
            "$maxDistance" => max = Some(bound(v, "$maxDistance", (16895, 16896))?),
            "$minDistance" => min = Some(bound(v, "$minDistance", (16897, 16898))?),
            "$near" | "$nearSphere" | "$geoNear" | "$uniqueDocs" => {}
            other => {
                return Err(Fallback::mongo(
                    34413,
                    format!("invalid argument in geo near query: {other}"),
                ))
            }
        }
    }
    if spherical && !geojson {
        if in_bounds(centre).is_err() {
            return bad("Legacy point is out of bounds for spherical query");
        }
        // A legacy point on the sphere measures in radians.
        min = min.map(|r| r * EARTH_RADIUS_METERS);
        max = max.map(|r| r * EARTH_RADIUS_METERS);
    }
    Ok(Some(Near {
        centre,
        spherical,
        geojson,
        min,
        max,
    }))
}

impl Near {
    /// How far a stored value is from the centre: the least over the
    /// geometries it holds. `None` when it holds none this query measures.
    pub fn distance(&self, v: &Bson) -> Option<f64> {
        stored(v)
            .iter()
            .filter_map(|g| {
                if self.spherical {
                    Some(sphere::distance(g, self.centre) * EARTH_RADIUS_METERS)
                } else {
                    match g {
                        Geom::Point(p) => Some((p.0 - self.centre.0).hypot(p.1 - self.centre.1)),
                        _ => None,
                    }
                }
            })
            .min_by(f64::total_cmp)
    }

    /// Whether a distance is inside the query's bounds.
    pub fn admits(&self, d: f64) -> bool {
        self.min.is_none_or(|lo| d >= lo) && self.max.is_none_or(|hi| d <= hi)
    }
}

// ----------------------------------------------------------------- the plane

mod plane {
    use super::P;

    pub fn in_box(p: P, a: P, b: P) -> bool {
        let (lo_x, hi_x) = (a.0.min(b.0), a.0.max(b.0));
        let (lo_y, hi_y) = (a.1.min(b.1), a.1.max(b.1));
        (lo_x..=hi_x).contains(&p.0) && (lo_y..=hi_y).contains(&p.1)
    }

    fn on_segment(p: P, a: P, b: P) -> bool {
        let cross = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        cross == 0.0
            && (a.0.min(b.0)..=a.0.max(b.0)).contains(&p.0)
            && (a.1.min(b.1)..=a.1.max(b.1)).contains(&p.1)
    }

    /// Inside the polygon or on its boundary.
    pub fn in_polygon(p: P, ring: &[P]) -> bool {
        let n = ring.len();
        let mut inside = false;
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            if on_segment(p, a, b) {
                return true;
            }
            if (a.1 > p.1) != (b.1 > p.1) {
                let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
                if p.0 < x {
                    inside = !inside;
                }
            }
        }
        inside
    }

    pub fn in_circle(p: P, c: P, r: f64) -> bool {
        (p.0 - c.0).hypot(p.1 - c.1) <= r
    }
}

// ---------------------------------------------------------------- the sphere

pub(crate) mod sphere {
    use super::{Geom, P};

    pub type V = [f64; 3];

    /// The angle mongod reports from a point to a line it lies on: not
    /// nothing, but 6.42567661e-12 metres of it.
    const ON_A_LINE: f64 = 1.007_459_37e-18;

    /// What counts as touching: about a centimetre on the ground, the width
    /// of the leaf cell mongod tests a point as.
    const NEAR: f64 = 1.5e-9;

    pub fn unit(p: P) -> V {
        let (lng, lat) = (p.0.to_radians(), p.1.to_radians());
        [lat.cos() * lng.cos(), lat.cos() * lng.sin(), lat.sin()]
    }

    fn dot(a: V, b: V) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn cross(a: V, b: V) -> V {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn norm(a: V) -> f64 {
        dot(a, a).sqrt()
    }

    fn scaled(a: V, k: f64) -> V {
        [a[0] * k, a[1] * k, a[2] * k]
    }

    /// The angle between two points, in radians.
    pub fn angle(a: V, b: V) -> f64 {
        norm(cross(a, b)).atan2(dot(a, b))
    }

    /// Whether `p`, already on the great circle of `a` and `b`, is between them.
    fn between(p: V, a: V, b: V) -> bool {
        let n = cross(a, b);
        dot(cross(a, p), n) >= 0.0 && dot(cross(p, b), n) >= 0.0
    }

    /// The angle from `p` to the arc from `a` to `b`.
    pub fn to_arc(p: V, a: V, b: V) -> f64 {
        let n = cross(a, b);
        let len = norm(n);
        if len == 0.0 {
            return angle(p, a);
        }
        if between(p, a, b) {
            return (dot(p, n) / len).abs().min(1.0).asin();
        }
        angle(p, a).min(angle(p, b))
    }

    fn on_arc(p: V, a: V, b: V) -> bool {
        to_arc(p, a, b) <= NEAR
    }

    /// Whether two arcs have a point in common.
    pub fn arcs_touch(a: V, b: V, c: V, d: V) -> bool {
        if on_arc(a, c, d) || on_arc(b, c, d) || on_arc(c, a, b) || on_arc(d, a, b) {
            return true;
        }
        arcs_cross(a, b, c, d)
    }

    /// Whether two arcs cross away from their ends.
    pub fn arcs_cross(a: V, b: V, c: V, d: V) -> bool {
        let (n1, n2) = (cross(a, b), cross(c, d));
        let x = cross(n1, n2);
        let len = norm(x);
        if len < 1e-15 {
            return false;
        }
        let x = scaled(x, 1.0 / len);
        let inner = |p: V| {
            between(p, a, b)
                && between(p, c, d)
                && [a, b, c, d].iter().all(|end| angle(p, *end) > NEAR)
        };
        inner(x) || inner(scaled(x, -1.0))
    }

    /// Whether `p` is strictly inside the loop: it winds once around `p`, and
    /// `p` is on the loop's own side of the sphere (a loop winds around the
    /// point opposite its inside as well).
    fn winds_around(p: V, ring: &[V]) -> bool {
        let n = ring.len();
        let mut total = 0.0;
        let mut centre = [0.0; 3];
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            total += dot(p, cross(a, b)).atan2(dot(cross(a, p), cross(b, p)));
            centre = [centre[0] + a[0], centre[1] + a[1], centre[2] + a[2]];
        }
        total.abs() > std::f64::consts::PI && dot(p, centre) > 0.0
    }

    fn on_ring(p: V, ring: &[V]) -> bool {
        let n = ring.len();
        (0..n).any(|i| on_arc(p, ring[i], ring[(i + 1) % n]))
    }

    fn rings(loops: &[Vec<P>]) -> Vec<Vec<V>> {
        loops
            .iter()
            .map(|l| l.iter().map(|p| unit(*p)).collect())
            .collect()
    }

    /// Inside the polygon or on its boundary; a hole's boundary is the
    /// polygon's too.
    pub fn in_polygon(p: V, loops: &[Vec<V>]) -> bool {
        let Some((outer, holes)) = loops.split_first() else {
            return false;
        };
        if loops.iter().any(|ring| on_ring(p, ring)) {
            return true;
        }
        winds_around(p, outer) && !holes.iter().any(|hole| winds_around(p, hole))
    }

    fn edges(ring: &[V], closed: bool) -> Vec<(V, V)> {
        let n = ring.len();
        let last = if closed { n } else { n.saturating_sub(1) };
        (0..last).map(|i| (ring[i], ring[(i + 1) % n])).collect()
    }

    /// The pieces a geometry is made of.
    enum Part {
        Point(V),
        Line(Vec<V>),
        Polygon(Vec<Vec<V>>),
    }

    fn parts(g: &Geom, out: &mut Vec<Part>) {
        match g {
            Geom::Point(p) => out.push(Part::Point(unit(*p))),
            Geom::Line(l) => out.push(Part::Line(l.iter().map(|p| unit(*p)).collect())),
            Geom::Polygon(loops) => out.push(Part::Polygon(rings(loops))),
            Geom::Multi(list) => list.iter().for_each(|g| parts(g, out)),
        }
    }

    fn part_edges(part: &Part) -> Vec<(V, V)> {
        match part {
            Part::Point(_) => Vec::new(),
            Part::Line(l) => edges(l, false),
            Part::Polygon(loops) => loops.iter().flat_map(|r| edges(r, true)).collect(),
        }
    }

    fn part_points(part: &Part) -> Vec<V> {
        match part {
            Part::Point(p) => vec![*p],
            Part::Line(l) => l.clone(),
            Part::Polygon(loops) => loops.iter().flatten().copied().collect(),
        }
    }

    fn covers(part: &Part, p: V) -> bool {
        match part {
            Part::Point(q) => angle(p, *q) <= NEAR,
            Part::Line(l) => edges(l, false).iter().any(|(a, b)| on_arc(p, *a, *b)),
            Part::Polygon(loops) => in_polygon(p, loops),
        }
    }

    fn parts_meet(x: &Part, y: &Part) -> bool {
        if part_points(x).iter().any(|p| covers(y, *p))
            || part_points(y).iter().any(|p| covers(x, *p))
        {
            return true;
        }
        let (ex, ey) = (part_edges(x), part_edges(y));
        ex.iter()
            .any(|(a, b)| ey.iter().any(|(c, d)| arcs_touch(*a, *b, *c, *d)))
    }

    /// Whether two geometries have a point in common.
    pub fn intersects(x: &Geom, y: &Geom) -> bool {
        let (mut px, mut py) = (Vec::new(), Vec::new());
        parts(x, &mut px);
        parts(y, &mut py);
        px.iter().any(|a| py.iter().any(|b| parts_meet(a, b)))
    }

    /// Whether one piece lies wholly in a polygon: every vertex in it, and no
    /// edge leaving through its boundary.
    fn part_in_polygon(part: &Part, loops: &[Vec<V>]) -> bool {
        if !part_points(part).iter().all(|p| in_polygon(*p, loops)) {
            return false;
        }
        // A polygon that has one of the area's holes inside it is not in
        // the area, though every vertex of it is.
        if let Part::Polygon(own) = part {
            let swallowed = |hole: &Vec<V>| {
                hole.iter()
                    .any(|p| own.first().is_some_and(|outer| winds_around(*p, outer)))
                    || hole
                        .iter()
                        .all(|p| own.iter().any(|ring| on_ring(*p, ring)))
            };
            if loops.iter().skip(1).any(swallowed) {
                return false;
            }
        }
        let boundary: Vec<(V, V)> = loops.iter().flat_map(|r| edges(r, true)).collect();
        !part_edges(part)
            .iter()
            .any(|(a, b)| boundary.iter().any(|(c, d)| arcs_cross(*a, *b, *c, *d)))
    }

    /// Whether every piece of `inner` lies wholly in one polygon of `area`.
    pub fn within(inner: &Geom, area: &Geom) -> bool {
        let (mut pieces, mut polygons) = (Vec::new(), Vec::new());
        parts(inner, &mut pieces);
        parts(area, &mut polygons);
        !pieces.is_empty()
            && pieces.iter().all(|piece| {
                polygons.iter().any(|polygon| match polygon {
                    Part::Polygon(loops) => part_in_polygon(piece, loops),
                    _ => false,
                })
            })
    }

    /// The angle from `centre` to the nearest point of the geometry: nothing
    /// for a point on it or in it. A line is never at nothing, as mongod
    /// measures it: `$maxDistance: 0` leaves out a line that starts at the
    /// centre.
    pub fn distance(g: &Geom, centre: P) -> f64 {
        let mut pieces = Vec::new();
        parts(g, &mut pieces);
        let c = unit(centre);
        pieces
            .iter()
            .map(|piece| match piece {
                Part::Point(p) => angle(*p, c),
                Part::Polygon(loops) if in_polygon(c, loops) => 0.0,
                other => part_edges(other)
                    .iter()
                    .map(|(a, b)| to_arc(c, *a, *b))
                    .fold(f64::INFINITY, f64::min)
                    .max(ON_A_LINE),
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// Whether every vertex of the geometry is within `radius` of `centre`.
    pub fn in_cap(g: &Geom, centre: P, radius: f64) -> bool {
        let mut pieces = Vec::new();
        parts(g, &mut pieces);
        let c = unit(centre);
        !pieces.is_empty()
            && pieces
                .iter()
                .all(|piece| part_points(piece).iter().all(|p| angle(*p, c) <= radius))
    }
}

/// Whether a stored geometry is within a query shape.
pub fn is_within(doc: &Geom, shape: &Shape) -> bool {
    match (shape, doc) {
        (Shape::Box(a, b), Geom::Point(p)) => plane::in_box(*p, *a, *b),
        (Shape::Polygon(ring), Geom::Point(p)) => plane::in_polygon(*p, ring),
        (Shape::Circle(c, r), Geom::Point(p)) => plane::in_circle(*p, *c, *r),
        // A flat shape holds points and nothing else.
        (Shape::Box(..) | Shape::Polygon(_) | Shape::Circle(..), _) => false,
        (Shape::Cap(c, r), g) => sphere::in_cap(g, *c, *r),
        (Shape::Geo(area), g) => sphere::within(g, area),
    }
}

/// Whether a stored geometry and a query geometry have a point in common.
pub fn is_intersecting(doc: &Geom, query: &Geom) -> bool {
    sphere::intersects(doc, query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn square() -> Geom {
        Geom::Polygon(vec![vec![(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)]])
    }

    fn message(r: Parsed<Shape>) -> String {
        match r {
            Err(Fallback::Mongo { message, .. }) => message,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn an_edge_is_a_great_circle_arc() {
        let line = Geom::Line(vec![(0.0, 0.0), (2.0, 2.0)]);
        // mongod: `[1, 1]` does not intersect the line from `[0, 0]` to `[2, 2]`.
        assert!(!is_intersecting(&line, &Geom::Point((1.0, 1.0))));
        assert!(is_intersecting(&line, &Geom::Point((0.0, 0.0))));
        assert!(is_intersecting(
            &line,
            &Geom::Line(vec![(0.0, 3.0), (3.0, 0.0)])
        ));
    }

    #[test]
    fn a_polygon_holds_its_boundary_and_not_its_hole() {
        let area = Shape::Geo(square());
        for corner in [(0.0, 0.0), (3.0, 3.0), (0.0, 3.0), (3.0, 0.0), (1.0, 1.0)] {
            assert!(is_within(&Geom::Point(corner), &area), "{corner:?}");
        }
        assert!(!is_within(&Geom::Point((10.0, 10.0)), &area));
        let holed = Shape::Geo(Geom::Polygon(vec![
            vec![(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)],
            vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0)],
        ]));
        assert!(!is_within(&Geom::Point((1.5, 1.5)), &holed));
        assert!(is_within(&Geom::Point((1.0, 1.0)), &holed));
        assert!(is_within(&Geom::Point((2.5, 2.5)), &holed));
    }

    #[test]
    fn a_polygon_across_the_antimeridian_is_the_small_one() {
        let area = Shape::Geo(Geom::Polygon(vec![vec![
            (179.0, -1.0),
            (-179.0, -1.0),
            (-179.0, 1.0),
            (179.0, 1.0),
        ]]));
        assert!(is_within(&Geom::Point((179.9, 0.0)), &area));
        assert!(is_within(&Geom::Point((-179.9, 0.0)), &area));
        assert!(!is_within(&Geom::Point((0.0, 0.0)), &area));
    }

    #[test]
    fn a_flat_shape_holds_points_only() {
        let b = Shape::Box((0.0, 0.0), (3.0, 3.0));
        assert!(is_within(&Geom::Point((3.0, 3.0)), &b));
        assert!(!is_within(&Geom::Line(vec![(0.0, 0.0), (2.0, 2.0)]), &b));
        assert!(!is_within(&square(), &b));
        let triangle = Shape::Polygon(vec![(0.0, 0.0), (3.0, 0.0), (0.0, 3.0)]);
        assert!(is_within(&Geom::Point((1.5, 1.5)), &triangle));
        assert!(is_within(&Geom::Point((0.0, 3.0)), &triangle));
        assert!(!is_within(&Geom::Point((2.5, 2.5)), &triangle));
    }

    #[test]
    fn the_refusals_are_mongods() {
        let within = |arg: Bson| message(within_shape("$geoWithin", &arg));
        assert_eq!(within(Bson::Int32(1)), "geometry must be an object");
        assert_eq!(
            within(doc! {}.into()),
            "geo query doesn't have any geometry"
        );
        assert_eq!(
            within(doc! { "$box": 1 }.into()),
            "unknown geo specifier: $box: 1"
        );
        assert_eq!(
            within(doc! { "$box": [[0, 0]] }.into()),
            "Point must be an array or object, instead got type missing"
        );
        assert_eq!(
            within(doc! { "$polygon": [[0, 0], [1, 1]] }.into()),
            "Polygon must have at least 3 points, instead got 2 vertices"
        );
        assert_eq!(
            within(doc! { "$center": [[0, 0]] }.into()),
            "Radius must be a non-negative number: EOO"
        );
        assert_eq!(
            within(doc! { "$centerSphere": [[200, 0], 1] }.into()),
            "Longitude/latitude is out of bounds, lng: 200 lat: 0"
        );
        assert_eq!(
            within(doc! { "$geometry": { "type": "Point", "coordinates": [1, 1] } }.into()),
            "$within not supported with provided geometry: { $geoWithin: { $geometry: { type: \
             \"Point\", coordinates: [ 1, 1 ] } } }"
        );
        assert_eq!(
            within(
                doc! { "$geometry": { "type": "Polygon", "coordinates": [[[0, 0], [3, 3], [3, 0], [0, 3], [0, 0]]] } }
                    .into()
            ),
            "Loop is not valid: [ [ 0, 0 ], [ 3, 3 ], [ 3, 0 ], [ 0, 3 ], [ 0, 0 ] ] Edges 0 and 2 \
             cross. Edge locations in degrees: [0.0000000, 0.0000000]-[3.0000000, 3.0000000] and \
             [0.0000000, 3.0000000]-[3.0000000, 0.0000000]"
        );
    }
}
