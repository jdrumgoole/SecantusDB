//! Geometry as mongod's `GeoParser` builds it for the sphere: a stored
//! value or a query shape, as S2 points, lines and polygons.

use crate::s2legacy::{latlng_point, Cap, Cell, Loop, Point, Polygon, Polyline, Region};
use bson::{Bson, Document};

/// A geometry on the sphere.
#[derive(Clone, Debug)]
pub enum Shape {
    /// A point (`GeometryContainer::isPoint`): indexed at its leaf cell.
    Point(Point),
    Line(Polyline),
    Polygon(Polygon),
    /// `MultiPoint`, `MultiLineString`, `MultiPolygon`, and a
    /// `GeometryCollection` (its members grouped by kind, as mongod groups
    /// them).
    Multi(Vec<Shape>),
}

impl Shape {
    /// The region a covering is asked for: a point is its leaf cell, a
    /// multi-geometry the union of its members.
    pub fn region(&self) -> Region {
        match self {
            Shape::Point(p) => Region::Cell(Cell::from_point(*p)),
            Shape::Line(l) => Region::Polyline(l.clone()),
            Shape::Polygon(p) => Region::Polygon(p.clone()),
            Shape::Multi(parts) => Region::Union(parts.iter().map(Shape::region).collect()),
        }
    }

    /// `GeometryContainer::minDistance` in radians: from `centre` to the
    /// nearest part of the shape. A point the same as the centre is 0 (the
    /// SERVER-52953 test, made for a lone point only).
    pub fn distance(&self, centre: Point) -> f64 {
        match self {
            Shape::Point(p) if *p == centre => 0.0,
            other => other.member_distance(centre),
        }
    }

    fn member_distance(&self, centre: Point) -> f64 {
        match self {
            Shape::Point(p) => centre.angle(*p),
            Shape::Line(l) => centre.angle(l.project(centre)),
            Shape::Polygon(p) => centre.angle(p.project(centre)),
            Shape::Multi(parts) => {
                let mut min = -1.0;
                for part in parts {
                    let d = part.member_distance(centre);
                    if min < 0.0 || d < min {
                        min = d;
                    }
                }
                min
            }
        }
    }
}

fn num(v: &Bson) -> Option<f64> {
    match v {
        Bson::Double(d) => Some(*d),
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        _ => None,
    }
}

/// The first two numbers of an array or a document (`parseFlatPoint`).
pub fn flat_pair(v: &Bson) -> Option<(f64, f64)> {
    let mut it: Box<dyn Iterator<Item = &Bson>> = match v {
        Bson::Array(a) => Box::new(a.iter()),
        Bson::Document(d) => Box::new(d.values()),
        _ => return None,
    };
    let x = num(it.next()?)?;
    let y = num(it.next()?)?;
    Some((x, y))
}

fn valid(p: (f64, f64)) -> bool {
    p.0.abs() <= 180.0 && p.1.abs() <= 90.0
}

/// `coordToPoint` of a GeoJSON coordinate.
fn coordinate(v: &Bson) -> Option<Point> {
    let Bson::Array(_) = v else { return None };
    let p = flat_pair(v)?;
    valid(p).then(|| latlng_point(p.0, p.1))
}

fn coordinates(v: Option<&Bson>) -> Option<Vec<Point>> {
    match v? {
        Bson::Array(a) => a.iter().map(coordinate).collect(),
        _ => None,
    }
}

/// `eraseDuplicatePoints`: consecutive repeats removed.
fn erase_duplicates(mut points: Vec<Point>) -> Vec<Point> {
    points.dedup();
    points
}

fn line(v: Option<&Bson>) -> Option<Polyline> {
    let vertices = erase_duplicates(coordinates(v)?);
    (vertices.len() >= 2).then_some(Polyline { vertices })
}

/// `parseGeoJSONPolygonCoordinates`: each ring closed, its repeats and its
/// closing vertex dropped, and normalised (a ring larger than a hemisphere
/// is inverted).
fn polygon(v: Option<&Bson>) -> Option<Polygon> {
    let Bson::Array(rings) = v? else { return None };
    let mut loops = Vec::new();
    for ring in rings {
        let points = coordinates(Some(ring))?;
        if points.first() != points.last() {
            return None;
        }
        let mut points = erase_duplicates(points);
        points.pop();
        if points.len() < 3 {
            return None;
        }
        let mut l = Loop::new(points);
        l.normalize();
        loops.push(l);
    }
    if loops.is_empty() {
        return None;
    }
    Some(Polygon::new(loops))
}

/// A GeoJSON object with a `crs` other than the default is mongod's
/// `STRICT_SPHERE` big polygon, which an index never holds and which is not
/// transcribed here.
fn plain_crs(d: &Document) -> bool {
    d.get("crs").is_none_or(|crs| {
        crs.as_document()
            .and_then(|c| c.get_document("properties").ok())
            .and_then(|p| p.get_str("name").ok())
            .is_some_and(|name| {
                matches!(
                    name,
                    "EPSG:4326" | "urn:ogc:def:crs:OGC:1.3:CRS84" | "urn:ogc:def:crs:EPSG::4326"
                )
            })
    })
}

const STRICT_WINDING: &str = "urn:x-mongodb:crs:strictwinding:EPSG:4326";

fn is_strict(d: &Document) -> bool {
    d.get_document("crs")
        .ok()
        .and_then(|c| c.get_document("properties").ok())
        .and_then(|p| p.get_str("name").ok())
        == Some(STRICT_WINDING)
}

/// `parseBigSimplePolygonCoordinates`: a polygon of one ring under the
/// strict-winding `crs`, the ring as given (its inside on its left, however
/// large).
fn strict_loop(d: &Document) -> Option<Loop> {
    if d.get_str("type").ok()? != "Polygon" {
        return None;
    }
    let Bson::Array(rings) = d.get("coordinates")? else {
        return None;
    };
    let [ring] = rings.as_slice() else {
        return None;
    };
    let points = coordinates(Some(ring))?;
    if points.first() != points.last() {
        return None;
    }
    let mut points = erase_duplicates(points);
    points.pop();
    (points.len() >= 3).then(|| Loop::new(points))
}

/// A GeoJSON object (`parseFromGeoJSON`).
pub fn geojson(d: &Document) -> Option<Shape> {
    if !plain_crs(d) {
        return None;
    }
    let coords = d.get("coordinates");
    let out = match d.get_str("type").ok()? {
        "Point" => Shape::Point(coordinate(coords?)?),
        "LineString" => Shape::Line(line(coords)?),
        "Polygon" => Shape::Polygon(polygon(coords)?),
        "MultiPoint" => {
            let points = coordinates(coords)?;
            if points.is_empty() {
                return None;
            }
            Shape::Multi(points.into_iter().map(Shape::Point).collect())
        }
        "MultiLineString" => {
            let Bson::Array(lines) = coords? else {
                return None;
            };
            let parts: Option<Vec<Shape>> = lines
                .iter()
                .map(|l| line(Some(l)).map(Shape::Line))
                .collect();
            Shape::Multi(parts?)
        }
        "MultiPolygon" => {
            let Bson::Array(polys) = coords? else {
                return None;
            };
            let parts: Option<Vec<Shape>> = polys
                .iter()
                .map(|p| polygon(Some(p)).map(Shape::Polygon))
                .collect();
            Shape::Multi(parts?)
        }
        "GeometryCollection" => {
            let Bson::Array(geoms) = d.get("geometries")? else {
                return None;
            };
            // mongod groups the members by kind: points, lines, polygons,
            // then the multi-geometries' members.
            let mut groups: [Vec<Shape>; 6] = Default::default();
            for g in geoms {
                let gd = g.as_document()?;
                let at = match gd.get_str("type").ok()? {
                    "Point" => 0,
                    "LineString" => 1,
                    "Polygon" => 2,
                    "MultiPoint" => 3,
                    "MultiLineString" => 4,
                    "MultiPolygon" => 5,
                    _ => return None,
                };
                match geojson(gd)? {
                    Shape::Multi(parts) if at >= 3 => groups[at].extend(parts),
                    shape => groups[at].push(shape),
                }
            }
            Shape::Multi(groups.into_iter().flatten().collect())
        }
        _ => return None,
    };
    Some(out)
}

/// A stored value as a `2dsphere` index reads it (`parseFromStorage`, then
/// projected onto the sphere): GeoJSON, or a legacy pair.
pub fn stored(v: &Bson) -> Option<Shape> {
    if let Bson::Document(d) = v {
        if d.get("type").is_some_and(|t| matches!(t, Bson::String(_))) {
            return geojson(d);
        }
    }
    let p = flat_pair(v)?;
    valid(p).then(|| Shape::Point(latlng_point(p.0, p.1)))
}

/// `StoredGeometry::extractGeometries`: the value as one geometry, or each
/// member of an array of them.
pub fn stored_all(v: &Bson) -> Vec<Shape> {
    if let Some(s) = stored(v) {
        return vec![s];
    }
    match v {
        Bson::Array(a) => a.iter().filter_map(stored).collect(),
        _ => Vec::new(),
    }
}

/// A `$geoWithin` / `$geoIntersects` operand as a region on the sphere, or
/// `None` when it has none (a flat `$box`, `$polygon` or `$center`, which a
/// `2dsphere` index cannot answer).
pub fn query_region(arg: &Bson) -> Option<Region> {
    let Bson::Document(d) = arg else { return None };
    let (key, v) = d.iter().find(|(k, _)| k.as_str() != "$uniqueDocs")?;
    match key.as_str() {
        "$geometry" => match v {
            // A legacy pair as the geometry: a point on the sphere.
            Bson::Array(_) => {
                let p = flat_pair(v)?;
                valid(p).then(|| Region::Cell(Cell::from_point(latlng_point(p.0, p.1))))
            }
            Bson::Document(d) if is_strict(d) => strict_loop(d).map(Region::Loop),
            _ => Some(geojson(v.as_document()?)?.region()),
        },
        "$centerSphere" => {
            let Bson::Array(parts) = v else { return None };
            let centre = flat_pair(parts.first()?)?;
            let radius = num(parts.get(1)?)?;
            if !valid(centre) || radius.is_nan() || radius < 0.0 {
                return None;
            }
            Some(Region::Cap(Cap::from_axis_angle(
                latlng_point(centre.0, centre.1),
                radius,
            )))
        }
        _ => None,
    }
}
