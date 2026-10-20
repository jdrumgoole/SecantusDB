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
use crate::query::{bson_type_name, bson_value_repr, format_g};
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
    /// A polygon under the `strictwinding` crs: one loop, its inside on the
    /// left of its edges whatever its size (mongod's `BigSimplePolygon`).
    /// Only a query holds one.
    BigPolygon(Vec<P>),
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
    loop_quoting(v, v)
}

/// [`polygon_loop`], quoting `shown` where mongod quotes more than the loop
/// (a big polygon's errors quote all its coordinates).
fn loop_quoting(v: &Bson, shown: &Bson) -> Parsed<Vec<P>> {
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
            coordinates(shown)
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
                    coordinates(shown),
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

/// A polygon under the `strictwinding` crs (`parseBigSimplePolygonCoordinates`).
fn big_polygon(v: Option<&Bson>) -> Parsed<Geom> {
    let Some(whole @ Bson::Array(loops)) = v else {
        return bad(format!(
            "Coordinates of polygon must be an array, instead got type {}",
            type_of(v)
        ));
    };
    if loops.len() != 1 {
        return bad(format!(
            "Only one simple loop is allowed in a big polygon, instead provided {} loops: {}",
            loops.len(),
            coordinates(whole)
        ));
    }
    Ok(Geom::BigPolygon(loop_quoting(&loops[0], whole)?))
}

/// The crs mongod reads a big polygon by.
const STRICT_WINDING: &str = "urn:x-mongodb:crs:strictwinding:EPSG:4326";

/// A GeoJSON object's `crs` (`parseGeoJSONCRS`, r8.2): whether it is the
/// `strictwinding` one, which only a polygon may name. Absent, it is the
/// sphere.
fn strict_winding(d: &Document, polygon: bool) -> Parsed<bool> {
    let Some(crs) = d.get("crs") else {
        return Ok(false);
    };
    let Bson::Document(crs) = crs else {
        return bad(format!(
            "GeoJSON CRS must be an object, instead got type {}",
            bson_type_name(crs)
        ));
    };
    if crs.get_str("type") != Ok("name") {
        return bad("GeoJSON CRS must have field \"type\": \"name\"");
    }
    let properties = crs.get("properties");
    let Some(Bson::Document(properties)) = properties else {
        return bad(format!(
            "CRS must have field \"properties\" which is an object, instead got type {}",
            type_of(properties)
        ));
    };
    let name = properties.get("name");
    let Some(Bson::String(name)) = name else {
        return bad(format!(
            "In CRS, \"properties.name\" must be a string, instead got type {}",
            type_of(name)
        ));
    };
    match name.as_str() {
        "urn:ogc:def:crs:OGC:1.3:CRS84" | "EPSG:4326" => Ok(false),
        STRICT_WINDING if polygon => Ok(true),
        STRICT_WINDING => bad("Strict winding order CRS is only supported by polygon"),
        other => bad(format!("Unknown CRS name: {other}")),
    }
}

/// Why a GeoJSON point cannot be read for its `crs`.
pub fn point_crs_problem(d: &Document) -> Option<String> {
    match strict_winding(d, false) {
        Err(Fallback::Mongo { message, .. }) => Some(message),
        _ => None,
    }
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
    let kind = d.get_str("type");
    if matches!(
        kind,
        Ok("Point" | "LineString" | "Polygon" | "MultiPoint" | "MultiLineString" | "MultiPolygon")
    ) && strict_winding(d, kind == Ok("Polygon"))?
    {
        return big_polygon(coords);
    }
    match kind {
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
        Geom::Polygon(_) | Geom::BigPolygon(_) => true,
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
        // A document's polygon under the `strictwinding` crs is matched by
        // nothing (measured 8.2.11, 2026-10-15), and no index holds one.
        Bson::Document(d) if d.get("type").is_some_and(|t| matches!(t, Bson::String(_))) => {
            geojson(d)
                .ok()
                .filter(|g| !matches!(g, Geom::BigPolygon(_)))
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
        Geom::BigPolygon(ring) => out.extend(ring),
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
                match geojson(d) {
                    Err(f) => Some(reason(f)),
                    Ok(Geom::BigPolygon(_)) => {
                        Some("can't index geometry with strict winding order".to_string())
                    }
                    Ok(_) => None,
                }
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
                format!(
                    "point not in interval of [ {}, {} ]",
                    format_g(lo),
                    format_g(hi)
                ),
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

/// What a `2d` index reads its points with: mongod's `GeoHashConverter`
/// parameters, `bits` of precision over the square `[min, max]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatIndex {
    pub bits: u32,
    pub min: f64,
    pub max: f64,
}

/// A number option as mongod's `FieldParser::extractNumber` reads it into an
/// `int`: a double (or a decimal, through one) truncated and saturated, NaN
/// as 0.
fn option_int(v: &Bson) -> Option<i32> {
    match v {
        Bson::Int32(n) => Some(*n),
        Bson::Int64(n) => Some((*n).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32),
        other => num(other).map(|d| d as i32),
    }
}

/// The `2d` index a spec builds, or mongod's refusal of its options:
/// `GeoHashConverter::createFromDoc` and the converter's own check of the
/// error it computes (`hash.cpp`, r8.2). `bits` defaults to 26 and the range
/// to `[-180, 180]`; a value of the wrong type was refused when the spec was
/// parsed, so it is read as absent here. Measured on 8.2.11 (2026-10-15):
/// each message, `bits` truncated (2.5 is 2, NaN is 0, 2^40 is 2147483647),
/// and a range so wide its cell size overflows refused by 4799400.
pub fn flat_index(spec: &Document) -> Result<FlatIndex, (i32, String)> {
    let bits = spec.get("bits").and_then(option_int).unwrap_or(26);
    let min = spec.get("min").and_then(num).unwrap_or(-180.0);
    let max = spec.get("max").and_then(num).unwrap_or(180.0);
    if !(1..=32).contains(&bits) {
        return Err((
            72,
            format!("bits for hash must be > 0 and <= 32, but {bits} bits were specified"),
        ));
    }
    let range = format!("[{}, {}]", format_g(min), format_g(max));
    // NaN compares false either way: written as mongod writes it.
    let ordered = min < max;
    if !ordered || min.is_infinite() || max.is_infinite() {
        return Err((
            72,
            format!(
                "region for hash must be valid and have positive area, but {range} was specified"
            ),
        ));
    }
    let scaling = 4.0 * 1024.0 * 1024.0 * 1024.0 / (max - min);
    let positive = scaling > 0.0;
    if !positive || scaling.is_infinite() {
        return Err((72, format!("range {range} is too small.")));
    }
    // The distance between the corners of two neighbouring cells, as the
    // converter unhashes them, plus a hundredth of a cell.
    let cell = (1u64 << (32 - bits)) as f64;
    let (a, b) = (0.0 / scaling + min, cell / scaling + min);
    let d = b - a;
    let error = (d * d + d * d).sqrt() + 0.001 / scaling;
    let enough = max - min >= error / 2.0;
    if !enough {
        return Err((4799400, "Invalid GeoHashConverter parameters".to_string()));
    }
    Ok(FlatIndex {
        bits: bits as u32,
        min,
        max,
    })
}

/// mongod's refusal of a `$near` or `$geoNear` centre a `2d` index cannot
/// hash: one outside its range (16433, measured 8.2.11).
pub fn flat_centre_problem(index: &FlatIndex, centre: P) -> Option<String> {
    let (x, y) = centre;
    if (index.min..=index.max).contains(&x) && (index.min..=index.max).contains(&y) {
        return None;
    }
    let mut at = Document::new();
    at.insert("0", x);
    at.insert("1", y);
    Some(format!(
        "point not in interval of [ {}, {} ] :: caused by :: {}",
        format_g(index.min),
        format_g(index.max),
        bson_value_repr(&Bson::Document(at))
    ))
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
                    if let Some(why) = point_crs_problem(gd) {
                        return Err(wrong(why));
                    }
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
    //! Geometry on the sphere as mongod's S2 library tests it (`s2loop.cc`,
    //! `s2cell.cc`, `s2cap.cc`, `s2polyline.cc` in mongod's `third_party`,
    //! and `geometry_container.cpp`, r8.2): an edge is the great-circle arc
    //! between its ends, a loop's inside is on the left of its edges, and a
    //! point is tested as the leaf cell it falls in.
    use super::{Geom, P};

    pub type V = [f64; 3];

    /// The angle mongod reports from a point to a line it lies on: not
    /// nothing, but 6.42567661e-12 metres of it.
    const ON_A_LINE: f64 = 1.007_459_37e-18;

    /// What counts as two lines touching, or a line touching a loop: about
    /// a centimetre on the ground, the width of a leaf cell. (Only a point
    /// is tested as its leaf cell itself.)
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

    fn sum(a: V, b: V) -> V {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }

    fn normalized(a: V) -> V {
        let n = norm(a);
        if n == 0.0 {
            a
        } else {
            scaled(a, 1.0 / n)
        }
    }

    fn neg(a: V) -> V {
        [-a[0], -a[1], -a[2]]
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

    /// `a` x `b`, written as S2's `RobustCrossProd` writes it, `(b + a) x
    /// (b - a)`, twice as long and exact enough for two points a leaf cell
    /// apart, where the plain product is all rounding.
    fn robust_cross(a: V, b: V) -> V {
        cross(sum(b, a), [b[0] - a[0], b[1] - a[1], b[2] - a[2]])
    }

    /// S2's `SimpleCrossing`: the arcs cross at a point inside both. Arcs
    /// that share an end do not (S2's `RobustCrossing` is 0 for them).
    fn crosses(a: V, b: V, c: V, d: V) -> bool {
        if a == c || a == d || b == c || b == d {
            return false;
        }
        let ab = robust_cross(a, b);
        let acb = -dot(ab, c);
        let bda = dot(ab, d);
        if acb * bda <= 0.0 {
            return false;
        }
        let cd = robust_cross(c, d);
        let cbd = -dot(cd, b);
        let dac = dot(cd, a);
        acb * cbd > 0.0 && acb * dac > 0.0
    }

    // ------------------------------------------------------------- loops

    /// A loop with its inside on the left of its edges, and points just
    /// inside it to count edge crossings from.
    pub struct Ring {
        v: Vec<V>,
        inside: Vec<V>,
    }

    /// The angle a path turns through at `b`, left positive.
    fn turn(a: V, b: V, c: V) -> f64 {
        let (x, y) = (cross(a, b), cross(b, c));
        let angle = norm(cross(x, y)).atan2(dot(x, y));
        if dot(cross(a, b), c) > 0.0 {
            angle
        } else {
            -angle
        }
    }

    impl Ring {
        /// A loop as given: its inside on the left, whatever its size.
        fn as_given(v: Vec<V>) -> Ring {
            let n = v.len();
            let mut inside: Vec<(f64, V)> = Vec::new();
            for i in 0..n {
                let (a, b) = (v[i], v[(i + 1) % n]);
                let mid = sum(a, b);
                let left = cross(a, b);
                if norm(mid) == 0.0 || norm(left) == 0.0 {
                    continue;
                }
                let mid = normalized(mid);
                // Off the middle of an edge, towards its left, by less than
                // the way to any other edge.
                let room = (0..n)
                    .filter(|j| *j != i)
                    .map(|j| to_arc(mid, v[j], v[(j + 1) % n]))
                    .fold(angle(a, b) / 2.0, f64::min);
                if room <= 0.0 {
                    continue;
                }
                let q = normalized(sum(mid, scaled(normalized(left), room / 4.0)));
                inside.push((room, q));
            }
            inside.sort_by(|x, y| y.0.total_cmp(&x.0));
            Ring {
                v,
                inside: inside.into_iter().map(|(_, q)| q).collect(),
            }
        }

        /// A loop of a GeoJSON polygon: the side no larger than a hemisphere
        /// (`S2Loop::Normalize`).
        fn smaller_side(mut v: Vec<V>) -> Ring {
            let n = v.len();
            let turning: f64 = (0..n)
                .map(|i| turn(v[(i + n - 1) % n], v[i], v[(i + 1) % n]))
                .sum();
            if turning < -1e-14 {
                v.reverse();
            }
            Ring::as_given(v)
        }

        fn edges(&self) -> impl Iterator<Item = (V, V)> + '_ {
            let n = self.v.len();
            (0..n).map(move |i| (self.v[i], self.v[(i + 1) % n]))
        }

        /// Whether `p` is inside the loop (its boundary aside): an even number
        /// of edges between it and a point known to be inside.
        fn contains(&self, p: V) -> bool {
            for q in &self.inside {
                if angle(*q, p) > std::f64::consts::PI - 1e-6 {
                    continue;
                }
                // A path through a vertex would count its two edges as
                // one crossing or none: take another.
                let path = cross(*q, p);
                if self.v.iter().any(|v| dot(path, *v) == 0.0) {
                    continue;
                }
                let crossings = self.edges().filter(|(a, b)| crosses(*q, p, *a, *b)).count();
                return crossings % 2 == 0;
            }
            false
        }

        fn on_boundary(&self, p: V) -> bool {
            self.edges().any(|(a, b)| on_arc(p, a, b))
        }
    }

    // --------------------------------------------------------- leaf cells

    /// The leaf cell a point falls in, as S2 numbers them (the quadratic
    /// projection, level 30): its cube face and its range on the face.
    #[derive(PartialEq)]
    struct Leaf {
        face: usize,
        u: [f64; 2],
        v: [f64; 2],
    }

    /// S2's `FaceXYZtoUV`, `None` when `p` is not on the face's side.
    fn face_uv(face: usize, p: V) -> Option<(f64, f64)> {
        let axis = face % 3;
        if (face < 3 && p[axis] <= 0.0) || (face >= 3 && p[axis] >= 0.0) {
            return None;
        }
        Some(match face {
            0 => (p[1] / p[0], p[2] / p[0]),
            1 => (-p[0] / p[1], p[2] / p[1]),
            2 => (-p[0] / p[2], -p[1] / p[2]),
            3 => (p[2] / p[0], p[1] / p[0]),
            4 => (p[2] / p[1], -p[0] / p[1]),
            _ => (-p[1] / p[2], -p[0] / p[2]),
        })
    }

    fn face_xyz(face: usize, u: f64, v: f64) -> V {
        match face {
            0 => [1.0, u, v],
            1 => [-u, 1.0, v],
            2 => [-u, -v, 1.0],
            3 => [-1.0, -v, -u],
            4 => [v, -1.0, -u],
            _ => [v, u, -1.0],
        }
    }

    fn uv_to_st(u: f64) -> f64 {
        if u >= 0.0 {
            0.5 * (1.0 + 3.0 * u).sqrt()
        } else {
            1.0 - 0.5 * (1.0 - 3.0 * u).sqrt()
        }
    }

    fn st_to_uv(s: f64) -> f64 {
        if s >= 0.5 {
            (1.0 / 3.0) * (4.0 * s * s - 1.0)
        } else {
            (1.0 / 3.0) * (1.0 - 4.0 * (1.0 - s) * (1.0 - s))
        }
    }

    impl Leaf {
        fn of(p: V) -> Leaf {
            let a = [p[0].abs(), p[1].abs(), p[2].abs()];
            let axis = if a[0] > a[1] {
                if a[0] > a[2] {
                    0
                } else {
                    2
                }
            } else if a[1] > a[2] {
                1
            } else {
                2
            };
            let face = if p[axis] < 0.0 { axis + 3 } else { axis };
            let (u, v) = face_uv(face, p).unwrap_or((0.0, 0.0));
            const SIZE: f64 = (1u64 << 30) as f64;
            let range = |x: f64| {
                let ij = (SIZE * uv_to_st(x)).floor().clamp(0.0, SIZE - 1.0);
                [st_to_uv(ij / SIZE), st_to_uv((ij + 1.0) / SIZE)]
            };
            Leaf {
                face,
                u: range(u),
                v: range(v),
            }
        }

        fn corners(&self) -> [V; 4] {
            let (u, v) = (self.u, self.v);
            [
                normalized(face_xyz(self.face, u[0], v[0])),
                normalized(face_xyz(self.face, u[1], v[0])),
                normalized(face_xyz(self.face, u[1], v[1])),
                normalized(face_xyz(self.face, u[0], v[1])),
            ]
        }

        fn edges(&self) -> [(V, V); 4] {
            let c = self.corners();
            [(c[0], c[1]), (c[1], c[2]), (c[2], c[3]), (c[3], c[0])]
        }

        fn contains(&self, p: V) -> bool {
            face_uv(self.face, p).is_some_and(|(u, v)| {
                (self.u[0]..=self.u[1]).contains(&u) && (self.v[0]..=self.v[1]).contains(&v)
            })
        }

        /// Whether a line may meet the cell (`S2Polyline::MayIntersect`).
        fn meets_line(&self, line: &[V]) -> bool {
            line.iter().any(|p| self.contains(*p))
                || line.windows(2).any(|e| {
                    self.edges()
                        .iter()
                        .any(|(c, d)| crosses(e[0], e[1], *c, *d))
                })
        }

        /// Whether a polygon may meet the cell (`S2Polygon::MayIntersect`).
        fn meets_polygon(&self, rings: &[Ring]) -> bool {
            self.corners().iter().any(|c| polygon_contains(rings, *c))
                || rings.iter().any(|ring| {
                    ring.v.iter().any(|p| self.contains(*p))
                        || ring
                            .edges()
                            .any(|(a, b)| self.edges().iter().any(|(c, d)| crosses(a, b, *c, *d)))
                })
        }

        /// The normal of edge `k`, pointing into the cell (`S2Cell::GetEdgeRaw`).
        fn edge_normal(&self, k: usize) -> V {
            let (f, u, v) = (self.face, self.u, self.v);
            let u_norm = |u: f64| match f {
                0 => [u, -1.0, 0.0],
                1 => [1.0, u, 0.0],
                2 => [1.0, 0.0, u],
                3 => [-u, 0.0, 1.0],
                4 => [0.0, -u, 1.0],
                _ => [0.0, -1.0, -u],
            };
            let v_norm = |v: f64| match f {
                0 => [-v, 0.0, 1.0],
                1 => [0.0, -v, 1.0],
                2 => [0.0, -1.0, -v],
                3 => [v, -1.0, 0.0],
                4 => [1.0, v, 0.0],
                _ => [1.0, 0.0, v],
            };
            match k {
                0 => v_norm(v[0]),
                1 => u_norm(u[1]),
                2 => neg(v_norm(v[1])),
                _ => neg(u_norm(u[0])),
            }
        }

        /// Whether a cap may meet the cell (`S2Cap::MayIntersect`).
        fn meets_cap(&self, axis: V, radius: f64) -> bool {
            let height = cap_height(radius);
            let corners = self.corners();
            if corners.iter().any(|c| {
                let d = diff(axis, *c);
                dot(d, d) <= 2.0 * height
            }) {
                return true;
            }
            // A hemisphere or more holds a corner of any cell it meets.
            if height >= 1.0 {
                return false;
            }
            if self.contains(axis) {
                return true;
            }
            // Otherwise the cap can only meet the inside of an edge.
            let sin2 = height * (2.0 - height);
            for k in 0..4 {
                let edge = self.edge_normal(k);
                let d = dot(axis, edge);
                if d > 0.0 {
                    continue;
                }
                if d * d > sin2 * dot(edge, edge) {
                    return false;
                }
                let dir = cross(edge, axis);
                if dot(dir, corners[k]) < 0.0 && dot(dir, corners[(k + 1) & 3]) > 0.0 {
                    return true;
                }
            }
            false
        }
    }

    /// The height of the cap of a radius (`S2Cap::FromAxisAngle`): a radius
    /// of pi or more is the whole sphere.
    fn cap_height(radius: f64) -> f64 {
        if radius >= std::f64::consts::PI {
            2.0
        } else {
            let d = (0.5 * radius).sin();
            2.0 * d * d
        }
    }

    // ------------------------------------------------------------ polygons

    fn rings(loops: &[Vec<P>]) -> Vec<Ring> {
        loops
            .iter()
            .map(|l| Ring::smaller_side(l.iter().map(|p| unit(*p)).collect()))
            .collect()
    }

    /// Inside the polygon, its boundary aside.
    fn polygon_contains(rings: &[Ring], p: V) -> bool {
        let Some((outer, holes)) = rings.split_first() else {
            return false;
        };
        outer.contains(p) && !holes.iter().any(|hole| hole.contains(p))
    }

    /// Inside the polygon or on its boundary; a hole's boundary is the
    /// polygon's too.
    fn in_polygon(p: V, rings: &[Ring]) -> bool {
        rings.iter().any(|ring| ring.on_boundary(p)) || polygon_contains(rings, p)
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
        Polygon(Vec<Ring>),
    }

    fn parts(g: &Geom, out: &mut Vec<Part>) {
        match g {
            Geom::Point(p) => out.push(Part::Point(unit(*p))),
            Geom::Line(l) => out.push(Part::Line(l.iter().map(|p| unit(*p)).collect())),
            Geom::Polygon(loops) => out.push(Part::Polygon(rings(loops))),
            Geom::BigPolygon(ring) => out.push(Part::Polygon(vec![Ring::as_given(
                ring.iter().map(|p| unit(*p)).collect(),
            )])),
            Geom::Multi(list) => list.iter().for_each(|g| parts(g, out)),
        }
    }

    fn part_edges(part: &Part) -> Vec<(V, V)> {
        match part {
            Part::Point(_) => Vec::new(),
            Part::Line(l) => edges(l, false),
            Part::Polygon(rings) => rings.iter().flat_map(|r| r.edges()).collect(),
        }
    }

    fn part_points(part: &Part) -> Vec<V> {
        match part {
            Part::Point(p) => vec![*p],
            Part::Line(l) => l.clone(),
            Part::Polygon(rings) => rings.iter().flat_map(|r| r.v.iter().copied()).collect(),
        }
    }

    fn parts_meet(x: &Part, y: &Part) -> bool {
        match (x, y) {
            // A point is its leaf cell (`GeometryContainer::intersects`).
            (Part::Point(p), Part::Point(q)) => Leaf::of(*p) == Leaf::of(*q),
            (Part::Point(p), Part::Line(l)) | (Part::Line(l), Part::Point(p)) => {
                Leaf::of(*p).meets_line(l)
            }
            (Part::Point(p), Part::Polygon(rings)) | (Part::Polygon(rings), Part::Point(p)) => {
                Leaf::of(*p).meets_polygon(rings)
            }
            (Part::Line(l), Part::Polygon(rings)) | (Part::Polygon(rings), Part::Line(l)) => {
                line_meets_polygon(l, rings)
            }
            (Part::Polygon(x), Part::Polygon(y)) => polygons_meet(x, y),
            // `S2Polyline::Intersects`: an edge of one crosses an edge of
            // the other, or they share a vertex (`RobustCrossing >= 0`),
            // settled by S2's exact predicates. A line ending exactly on
            // another's edge does not meet it (measured 8.2.11).
            (Part::Line(x), Part::Line(y)) => lines_meet(x, y),
        }
    }

    fn lines_meet(x: &[V], y: &[V]) -> bool {
        use crate::s2legacy::edge::EdgeCrosser;
        use crate::s2legacy::point::Point;
        if x.is_empty() || y.is_empty() {
            return false;
        }
        x.windows(2).any(|e| {
            let mut crosser = EdgeCrosser::new(Point(e[0]), Point(e[1]), Point(y[0]));
            y[1..]
                .iter()
                .any(|d| crosser.robust_crossing(Point(*d)) >= 0)
        })
    }

    /// Strictly inside the polygon: in it, and not on its boundary.
    fn well_inside(rings: &[Ring], p: V) -> bool {
        polygon_contains(rings, p) && !rings.iter().any(|ring| ring.on_boundary(p))
    }

    /// Whether some of a line is inside a polygon: mongod's
    /// `polygonLineIntersection`, which clips the line to the polygon with
    /// `S2Polygon::IntersectWithPolyline` and asks whether anything is left
    /// (a line along the boundary meets it only where S2's crossing rules
    /// put some of it inside; measured 8.2.11).
    fn line_meets_polygon(line: &[V], rings: &[Ring]) -> bool {
        use crate::s2legacy::loops::{Loop, Polygon};
        use crate::s2legacy::point::Point;
        let loops: Vec<Loop> = rings
            .iter()
            .map(|ring| {
                let mut l = Loop::new(ring.v.iter().map(|v| Point(*v)).collect());
                l.normalize();
                l
            })
            .collect();
        let polygon = Polygon::new(loops);
        let points: Vec<Point> = line.iter().map(|v| Point(*v)).collect();
        polygon.clips_polyline(&points)
    }

    /// Whether the insides of two polygons overlap (`S2Polygon::Intersects`):
    /// sharing a boundary is not enough, unless an edge is shared the same
    /// way round, with both insides on its left.
    fn polygons_meet(x: &[Ring], y: &[Ring]) -> bool {
        let ex: Vec<(V, V)> = x.iter().flat_map(|r| r.edges()).collect();
        let ey: Vec<(V, V)> = y.iter().flat_map(|r| r.edges()).collect();
        x.iter()
            .flat_map(|r| r.v.iter())
            .any(|p| well_inside(y, *p))
            || y.iter()
                .flat_map(|r| r.v.iter())
                .any(|p| well_inside(x, *p))
            || ex.iter().any(|(a, b)| {
                ey.iter()
                    .any(|(c, d)| (a, b) == (c, d) || crosses(*a, *b, *c, *d))
            })
    }

    /// Whether two geometries have a point in common.
    pub fn intersects(x: &Geom, y: &Geom) -> bool {
        let (mut px, mut py) = (Vec::new(), Vec::new());
        parts(x, &mut px);
        parts(y, &mut py);
        px.iter().any(|a| py.iter().any(|b| parts_meet(a, b)))
    }

    /// Whether one piece lies wholly in a polygon: every vertex in it, and no
    /// edge leaving through its boundary. A point is in it when the polygon
    /// holds it or may meet its leaf cell.
    fn part_in_polygon(part: &Part, rings: &[Ring]) -> bool {
        if let Part::Point(p) = part {
            return polygon_contains(rings, *p) || Leaf::of(*p).meets_polygon(rings);
        }
        if !part_points(part).iter().all(|p| in_polygon(*p, rings)) {
            return false;
        }
        // A polygon that has one of the area's holes inside it is not in
        // the area, though every vertex of it is.
        if let Part::Polygon(own) = part {
            let swallowed = |hole: &Ring| {
                hole.v
                    .iter()
                    .any(|p| own.first().is_some_and(|outer| outer.contains(*p)))
                    || hole
                        .v
                        .iter()
                        .all(|p| own.iter().any(|ring| ring.on_boundary(*p)))
            };
            if rings.iter().skip(1).any(swallowed) {
                return false;
            }
        }
        let boundary: Vec<(V, V)> = rings.iter().flat_map(|r| r.edges()).collect();
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
                    Part::Polygon(rings) => part_in_polygon(piece, rings),
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
                Part::Polygon(rings) if in_polygon(c, rings) => 0.0,
                other => part_edges(other)
                    .iter()
                    .map(|(a, b)| to_arc(c, *a, *b))
                    .fold(f64::INFINITY, f64::min)
                    .max(ON_A_LINE),
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// S2's `SimpleCCW`: `a`, `b`, `c` turn counter-clockwise.
    fn simple_ccw(a: V, b: V, c: V) -> bool {
        dot(cross(c, a), b) > 0.0
    }

    fn diff(a: V, b: V) -> V {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    /// S2's `RobustCrossProd`, with its fallback for equal or opposite
    /// points.
    fn s2_robust_cross(a: V, b: V) -> V {
        let x = cross(sum(b, a), diff(b, a));
        if x != [0.0; 3] {
            return x;
        }
        // `S2Point::Ortho`.
        let k = {
            let m = [a[0].abs(), a[1].abs(), a[2].abs()];
            let largest = if m[0] > m[1] {
                if m[0] > m[2] {
                    0
                } else {
                    2
                }
            } else if m[1] > m[2] {
                1
            } else {
                2
            };
            (largest + 2) % 3
        };
        let mut t = [0.012, 0.0053, 0.00457];
        t[k] = 1.0;
        let o = cross(a, t);
        let n = dot(o, o).sqrt();
        scaled(o, if n != 0.0 { 1.0 / n } else { n })
    }

    /// S2's `S2EdgeUtil::GetDistance`, which picks the edge `Project` uses.
    fn s2_edge_distance(x: V, a: V, b: V) -> f64 {
        let n = s2_robust_cross(a, b);
        if simple_ccw(n, a, x) && simple_ccw(x, b, n) {
            let sin_dist = dot(x, n).abs() / dot(n, n).sqrt();
            return sin_dist.min(1.0).asin();
        }
        let linear = dot(diff(x, a), diff(x, a)).min(dot(diff(x, b), diff(x, b)));
        2.0 * (0.5 * linear.sqrt()).min(1.0).asin()
    }

    /// S2's `S2EdgeUtil::GetClosestPoint`, its arithmetic kept: for a point
    /// almost at the pole of the edge's great circle the projection is
    /// rounding, and so is where mongod puts the closest point.
    fn s2_closest_point(x: V, a: V, b: V) -> V {
        let n = s2_robust_cross(a, b);
        if simple_ccw(n, a, x) && simple_ccw(x, b, n) {
            let k = dot(x, n) / dot(n, n);
            let p = [x[0] - k * n[0], x[1] - k * n[1], x[2] - k * n[2]];
            let len = dot(p, p).sqrt();
            return scaled(p, if len != 0.0 { 1.0 / len } else { len });
        }
        if dot(diff(x, a), diff(x, a)) <= dot(diff(x, b), diff(x, b)) {
            a
        } else {
            b
        }
    }

    /// `S2Distance::minDistanceRad(point, polyline)`: the angle to the point
    /// `S2Polyline::Project` finds, on the first nearest edge.
    fn s2_line_distance(x: V, line: &[(V, V)]) -> f64 {
        let mut best: Option<(f64, V, V)> = None;
        for (a, b) in line {
            let d = s2_edge_distance(x, *a, *b);
            if best.is_none_or(|(m, _, _)| d < m) {
                best = Some((d, *a, *b));
            }
        }
        match best {
            Some((_, a, b)) => angle(x, s2_closest_point(x, a, b)),
            None => f64::INFINITY,
        }
    }

    const NORTH: V = [0.0, 0.0, 1.0];
    const SOUTH: V = [0.0, 0.0, -1.0];

    /// Latitude and longitude, in degrees, of a point.
    fn lat_lng(p: V) -> (f64, f64) {
        (
            p[2].atan2((p[0] * p[0] + p[1] * p[1]).sqrt()).to_degrees(),
            p[1].atan2(p[0]).to_degrees(),
        )
    }

    /// A box of latitudes and longitudes holding every point of the area:
    /// `(lng lo, lat lo, lng hi, lat hi)` in degrees, the longitudes wrapping
    /// through 180 when lo is above hi (an S1 interval). The latitudes reach
    /// as far as the edges do, which is past their ends when an edge bows
    /// towards a pole.
    pub fn rect_bound(g: &Geom) -> (f64, f64, f64, f64) {
        let mut pieces = Vec::new();
        parts(g, &mut pieces);
        let (mut lat_lo, mut lat_hi) = (f64::INFINITY, f64::NEG_INFINITY);
        // Each edge covers the short way between its ends' longitudes.
        let mut spans: Vec<(f64, f64)> = Vec::new();
        let (mut north, mut south) = (false, false);
        for piece in &pieces {
            if let Part::Polygon(rings) = piece {
                north |= in_polygon(NORTH, rings);
                south |= in_polygon(SOUTH, rings);
            }
            for p in part_points(piece) {
                let (lat, lng) = lat_lng(p);
                lat_lo = lat_lo.min(lat);
                lat_hi = lat_hi.max(lat);
                spans.push((lng, lng));
            }
            for (a, b) in part_edges(piece) {
                let n = normalized(cross(a, b));
                // The points of the edge's great circle nearest the poles.
                for (pole, high) in [(NORTH, true), (SOUTH, false)] {
                    let top = normalized(diff(pole, scaled(n, dot(pole, n))));
                    if norm(top) > 0.0 && between(top, a, b) {
                        let lat = lat_lng(top).0;
                        if high {
                            lat_hi = lat_hi.max(lat);
                        } else {
                            lat_lo = lat_lo.min(lat);
                        }
                    }
                }
                let ((_, x), (_, y)) = (lat_lng(a), lat_lng(b));
                let mut d = y - x;
                if d > 180.0 {
                    d -= 360.0;
                } else if d < -180.0 {
                    d += 360.0;
                }
                spans.push(if d >= 0.0 { (x, x + d) } else { (x + d, x) });
            }
        }
        if north {
            lat_hi = 90.0;
        }
        if south {
            lat_lo = -90.0;
        }
        if north || south || spans.is_empty() {
            return (-180.0, lat_lo.max(-90.0), 180.0, lat_hi.min(90.0));
        }
        // The longitudes: everything but the widest stretch no edge covers.
        let wrap = |x: f64| {
            if x > 180.0 {
                x - 360.0
            } else if x < -180.0 {
                x + 360.0
            } else {
                x
            }
        };
        let covered = |x: f64| {
            spans.iter().any(|(lo, hi)| {
                let (lo, hi) = (wrap(*lo), wrap(*hi));
                if lo <= hi {
                    (lo..=hi).contains(&x)
                } else {
                    x >= lo || x <= hi
                }
            })
        };
        let mut ends: Vec<f64> = spans
            .iter()
            .flat_map(|(lo, hi)| [wrap(*lo), wrap(*hi)])
            .collect();
        ends.sort_by(f64::total_cmp);
        ends.dedup();
        let mut widest: Option<(f64, f64, f64)> = None;
        for (i, lo) in ends.iter().enumerate() {
            let hi = if i + 1 < ends.len() {
                ends[i + 1]
            } else {
                ends[0] + 360.0
            };
            if hi - lo <= 0.0 || covered(wrap((lo + hi) / 2.0)) {
                continue;
            }
            if widest.is_none_or(|(w, _, _)| hi - lo > w) {
                widest = Some((hi - lo, *lo, hi));
            }
        }
        match widest {
            None => (-180.0, lat_lo, 180.0, lat_hi),
            Some((_, gap_lo, gap_hi)) => (wrap(gap_hi), lat_lo, gap_lo, lat_hi),
        }
    }

    /// The box around a cap (`S2Cap::GetRectBound`), as [`rect_bound`] gives
    /// one.
    pub fn cap_bound(centre: P, radius: f64) -> (f64, f64, f64, f64) {
        if radius >= std::f64::consts::PI {
            return (-180.0, -90.0, 180.0, 90.0);
        }
        let (lng, lat) = (centre.0.to_radians(), centre.1.to_radians());
        let half = std::f64::consts::FRAC_PI_2;
        let (mut lo, mut hi) = (lat - radius, lat + radius);
        let mut all = false;
        if lo <= -half {
            lo = -half;
            all = true;
        }
        if hi >= half {
            hi = half;
            all = true;
        }
        let (mut lng_lo, mut lng_hi) = (-std::f64::consts::PI, std::f64::consts::PI);
        if !all {
            let (sin_a, sin_c) = (radius.sin(), lat.cos());
            if sin_a <= sin_c {
                let a = (sin_a / sin_c).asin();
                let tau = 2.0 * std::f64::consts::PI;
                let rem = |x: f64| x - tau * (x / tau).round();
                lng_lo = rem(lng - a);
                lng_hi = rem(lng + a);
            }
        }
        (
            lng_lo.to_degrees(),
            lo.to_degrees(),
            lng_hi.to_degrees(),
            hi.to_degrees(),
        )
    }

    /// Whether the geometry is within the cap of `radius` around `centre`
    /// (`GeometryContainer::contains` for a `$centerSphere`): a point when
    /// the cap may meet its leaf cell; a line or a polygon when it keeps out
    /// of the complement of the cap, which an edge bulging out of the cap
    /// does not, though both its ends are inside.
    pub fn in_cap(g: &Geom, centre: P, radius: f64) -> bool {
        let mut pieces = Vec::new();
        parts(g, &mut pieces);
        let c = unit(centre);
        let height = cap_height(radius);
        // The complement: around the antipode, with what is left of the
        // sphere (`S2Cap::Complement`, whose angle is -1 when empty).
        let far = neg(c);
        let far_angle = if height >= 2.0 {
            -1.0
        } else {
            2.0 * (0.5 * (2.0 - height)).sqrt().asin()
        };
        !pieces.is_empty()
            && pieces.iter().all(|piece| match piece {
                Part::Point(p) => Leaf::of(*p).meets_cap(c, radius),
                Part::Polygon(rings) if polygon_contains(rings, far) => 0.0 >= far_angle,
                // `S2Polyline::Project` and `S2Polygon::Project` both take the
                // closest point of the first nearest edge.
                other => s2_line_distance(far, &part_edges(other)) >= far_angle,
            })
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

/// A box of longitudes and latitudes holding every document a `2dsphere`
/// `$geoWithin` can match, for its index lookup: `(lng lo, lat lo, lng hi,
/// lat hi)`, wrapping through 180 when lo is above hi. `None` for a flat
/// shape, whose own box is the bound.
pub fn sphere_within_bound(arg: &Document) -> Option<(f64, f64, f64, f64)> {
    match within_shape("$geoWithin", &Bson::Document(arg.clone())).ok()? {
        Shape::Cap(c, r) => Some(sphere::cap_bound(c, r)),
        // A big polygon can be most of the sphere; the box says nothing.
        Shape::Geo(Geom::BigPolygon(_)) => Some((-180.0, -90.0, 180.0, 90.0)),
        Shape::Geo(g) => Some(sphere::rect_bound(&g)),
        _ => None,
    }
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
    fn a_2d_index_is_read_as_mongods_converter_reads_it() {
        let got = |spec: Document| flat_index(&spec);
        assert_eq!(
            got(doc! {}),
            Ok(FlatIndex {
                bits: 26,
                min: -180.0,
                max: 180.0
            })
        );
        assert_eq!(got(doc! { "bits": 2.5 }).map(|f| f.bits), Ok(2));
        assert_eq!(
            got(doc! { "bits": f64::INFINITY }).unwrap_err().1,
            "bits for hash must be > 0 and <= 32, but 2147483647 bits were specified"
        );
        assert_eq!(
            got(doc! { "min": 0.1, "max": 0.1000000001 }).map(|f| f.max),
            Ok(0.1000000001)
        );
        assert_eq!(
            got(doc! { "min": f64::NAN }).unwrap_err().1,
            "region for hash must be valid and have positive area, but [nan, 180] was specified"
        );
        let index = got(doc! { "min": -10, "max": 10 }).unwrap();
        assert_eq!(flat_centre_problem(&index, (10.0, -10.0)), None);
        assert_eq!(
            flat_centre_problem(&index, (20.5, 1.25)).as_deref(),
            Some("point not in interval of [ -10, 10 ] :: caused by :: { 0: 20.5, 1: 1.25 }")
        );
    }

    #[test]
    fn a_polygon_is_the_smaller_side_of_its_loop() {
        // The same ring both ways round is the same square.
        let ccw = vec![(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)];
        let mut cw = ccw.clone();
        cw.reverse();
        for ring in [ccw, cw.clone()] {
            let area = Shape::Geo(Geom::Polygon(vec![ring]));
            assert!(is_within(&Geom::Point((1.0, 1.0)), &area));
            assert!(!is_within(&Geom::Point((100.0, 50.0)), &area));
        }
        // Taken as given, the clockwise ring is the rest of the sphere.
        let big = Shape::Geo(Geom::BigPolygon(cw));
        assert!(!is_within(&Geom::Point((1.0, 1.0)), &big));
        assert!(is_within(&Geom::Point((100.0, 50.0)), &big));
    }

    #[test]
    fn a_cap_holds_a_line_only_when_its_arc_stays_inside() {
        let line = Geom::Line(vec![(-80.0, 20.0), (80.0, 20.0)]);
        assert!(!is_within(&line, &Shape::Cap((0.0, -90.0), 2.0)));
        assert!(is_within(
            &Geom::Point((0.0, 20.0)),
            &Shape::Cap((0.0, -90.0), 2.0)
        ));
        // The arc reaches 64.5 degrees north; this cap, 70.4.
        assert!(is_within(&line, &Shape::Cap((0.0, -90.0), 2.8)));
    }

    #[test]
    fn a_bound_reaches_as_far_as_the_edges_do() {
        let bowed = Shape::Geo(Geom::Polygon(vec![vec![
            (-80.0, 20.0),
            (80.0, 20.0),
            (80.0, 30.0),
            (-80.0, 30.0),
        ]]));
        let Shape::Geo(g) = bowed else { unreachable!() };
        let (lng_lo, lat_lo, lng_hi, lat_hi) = sphere::rect_bound(&g);
        assert_eq!((lng_lo, lng_hi), (-80.0, 80.0));
        assert!(
            (lat_lo - 20.0).abs() < 1e-9 && lat_hi > 70.0,
            "{lat_lo} {lat_hi}"
        );
        // A cap over a pole holds every longitude.
        assert_eq!(sphere::cap_bound((0.0, -80.0), 0.5).0, -180.0);
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
