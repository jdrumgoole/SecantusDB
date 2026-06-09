//! Geospatial query operators (`$geoWithin` / `$geoIntersects`) for the Rust
//! query engine — a bounded port of `secantus.geo` (Phase 4, slice geo-1).
//!
//! Planar containment/intersection uses the `geo` crate's DE-9IM `Relate`, which
//! shares the OGC lineage of Shapely (the pure-Python path), so `is_within` /
//! `is_intersects` line up. The spherical cap (`$centerSphere`) is done directly
//! with haversine (Shapely is planar), matching `geo.py`.
//!
//! Deferred to Python via `Fallback` (the engine contract):
//! * `$center` — Shapely approximates the disk with a 64-vertex buffer we can't
//!   reproduce exactly, so we don't try (an exact circle would diverge on the
//!   sub-degree annulus).
//! * `$near` / `$nearSphere` — slice geo-1b.
//! * malformed / unrecognised query shapes — Python raises the proper
//!   `QueryError`.

use crate::query::Fallback;
use bson::{Bson, Document};
use geo::{Coord, CoordsIter, Geometry, LineString, MultiLineString, MultiPoint, MultiPolygon};
use geo::{Point, Polygon, Relate};

type R = Result<bool, Fallback>;

/// A parsed query geometry.
enum QGeom {
    /// Planar shape — DE-9IM `Relate` against the doc geometry.
    Planar(Geometry<f64>),
    /// Great-circle cap on the unit sphere (`$centerSphere`): center + radius
    /// in radians.
    Sphere { lng: f64, lat: f64, rad: f64 },
}

fn num(b: &Bson) -> Option<f64> {
    match b {
        Bson::Double(d) => Some(*d),
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        _ => None,
    }
}

/// A `[x, y]` coordinate pair (a 2-element numeric array).
fn pair(b: &Bson) -> Option<Coord<f64>> {
    let a = b.as_array()?;
    if a.len() != 2 {
        return None;
    }
    Some(Coord {
        x: num(&a[0])?,
        y: num(&a[1])?,
    })
}

/// An array of coordinate pairs.
fn coord_list(b: &Bson) -> Option<Vec<Coord<f64>>> {
    b.as_array()?.iter().map(pair).collect()
}

fn ring(b: &Bson) -> Option<LineString<f64>> {
    Some(LineString(coord_list(b)?))
}

/// GeoJSON polygon `coordinates`: `[exterior_ring, hole_ring, ...]`.
fn polygon_from_rings(b: &Bson) -> Option<Polygon<f64>> {
    let a = b.as_array()?;
    let (first, rest) = a.split_first()?;
    let exterior = ring(first)?;
    let holes = rest.iter().map(ring).collect::<Option<Vec<_>>>()?;
    Some(Polygon::new(exterior, holes))
}

fn geojson_to_geometry(d: &Document) -> Option<Geometry<f64>> {
    let t = d.get_str("type").ok()?;
    let c = d.get("coordinates")?;
    Some(match t {
        "Point" => Geometry::Point(Point::from(pair(c)?)),
        "LineString" => Geometry::LineString(ring(c)?),
        "Polygon" => Geometry::Polygon(polygon_from_rings(c)?),
        "MultiPoint" => Geometry::MultiPoint(MultiPoint(
            coord_list(c)?.into_iter().map(Point::from).collect(),
        )),
        "MultiLineString" => Geometry::MultiLineString(MultiLineString(
            c.as_array()?.iter().map(ring).collect::<Option<Vec<_>>>()?,
        )),
        "MultiPolygon" => Geometry::MultiPolygon(MultiPolygon(
            c.as_array()?
                .iter()
                .map(polygon_from_rings)
                .collect::<Option<Vec<_>>>()?,
        )),
        _ => return None,
    })
}

/// Coerce a doc field value into a geometry, or `None` if it isn't one (treated
/// as "no match", mirroring `parse_doc_geometry` returning `None`). Mirrors
/// `geo.parse_doc_geometry`.
fn parse_doc_geometry(v: &Bson) -> Option<Geometry<f64>> {
    match v {
        Bson::Document(d) => {
            if d.get_str("type").is_ok() {
                geojson_to_geometry(d)
            } else {
                for (kx, ky) in [("x", "y"), ("lng", "lat"), ("longitude", "latitude")] {
                    if let (Some(x), Some(y)) = (d.get(kx).and_then(num), d.get(ky).and_then(num)) {
                        return Some(Geometry::Point(Point::new(x, y)));
                    }
                }
                None
            }
        }
        Bson::Array(_) => pair(v).map(|c| Geometry::Point(Point::from(c))),
        _ => None,
    }
}

/// Build the query geometry, or `None` to defer to Python (`$center`, malformed,
/// or unrecognised). Mirrors `geo.parse_query_geometry`.
fn parse_query_geometry(arg: &Document) -> Option<QGeom> {
    if let Ok(g) = arg.get_document("$geometry") {
        return geojson_to_geometry(g).map(QGeom::Planar);
    }
    if let Some(b) = arg.get("$box") {
        return parse_box(b).map(QGeom::Planar);
    }
    if let Some(b) = arg.get("$polygon") {
        return parse_polygon(b).map(QGeom::Planar);
    }
    if arg.contains_key("$center") {
        return None; // Shapely 64-gon buffer — defer to Python.
    }
    if let Some(b) = arg.get("$centerSphere") {
        return parse_center_sphere(b);
    }
    None
}

fn parse_box(b: &Bson) -> Option<Geometry<f64>> {
    let a = b.as_array()?;
    if a.len() != 2 {
        return None;
    }
    let p1 = pair(&a[0])?;
    let p2 = pair(&a[1])?;
    let (lox, hix) = (p1.x.min(p2.x), p1.x.max(p2.x));
    let (loy, hiy) = (p1.y.min(p2.y), p1.y.max(p2.y));
    Some(Geometry::Polygon(Polygon::new(
        LineString(vec![
            Coord { x: lox, y: loy },
            Coord { x: hix, y: loy },
            Coord { x: hix, y: hiy },
            Coord { x: lox, y: hiy },
            Coord { x: lox, y: loy },
        ]),
        vec![],
    )))
}

fn parse_polygon(b: &Bson) -> Option<Geometry<f64>> {
    let a = b.as_array()?;
    if a.len() < 3 {
        return None;
    }
    let mut pts = coord_list(b)?;
    let first = *pts.first()?;
    let last = *pts.last()?;
    if (first.x, first.y) != (last.x, last.y) {
        pts.push(first);
    }
    Some(Geometry::Polygon(Polygon::new(LineString(pts), vec![])))
}

fn parse_center_sphere(b: &Bson) -> Option<QGeom> {
    let a = b.as_array()?;
    if a.len() != 2 {
        return None;
    }
    let c = pair(&a[0])?;
    let rad = num(&a[1])?;
    if rad < 0.0 {
        return None;
    }
    Some(QGeom::Sphere {
        lng: c.x,
        lat: c.y,
        rad,
    })
}

/// Haversine angle (radians) between two `[lng, lat]` points. Mirrors
/// `geo._great_circle_radians`.
fn haversine(lng_a: f64, lat_a: f64, lng_b: f64, lat_b: f64) -> f64 {
    let phi_a = lat_a.to_radians();
    let phi_b = lat_b.to_radians();
    let d_phi = phi_b - phi_a;
    let d_lambda = (lng_b - lng_a).to_radians();
    let h =
        (d_phi / 2.0).sin().powi(2) + phi_a.cos() * phi_b.cos() * (d_lambda / 2.0).sin().powi(2);
    2.0 * h.clamp(0.0, 1.0).sqrt().asin()
}

fn within(doc: &Geometry<f64>, q: &QGeom) -> bool {
    match q {
        QGeom::Planar(g) => doc.relate(g).is_within(),
        // Every coordinate of the doc geometry must lie inside the cap (a Point
        // is one coord), matching `geo.geo_within`'s vertex test.
        QGeom::Sphere { lng, lat, rad } => doc
            .coords_iter()
            .all(|c| haversine(*lng, *lat, c.x, c.y) <= *rad),
    }
}

/// `$geoWithin` field operator. `Fallback` for malformed / `$center` /
/// non-document args (Python raises the proper `QueryError`).
pub fn op_geo_within(values: &[Option<Bson>], arg: &Bson) -> R {
    let arg = arg.as_document().ok_or(Fallback)?;
    let q = parse_query_geometry(arg).ok_or(Fallback)?;
    for v in values.iter().flatten() {
        if let Some(g) = parse_doc_geometry(v) {
            if within(&g, &q) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// `$geoIntersects` field operator — `$geometry` (planar) only, mirroring
/// mongod / `geo.py`. `Fallback` otherwise.
pub fn op_geo_intersects(values: &[Option<Bson>], arg: &Bson) -> R {
    let arg = arg.as_document().ok_or(Fallback)?;
    if !arg.contains_key("$geometry") {
        return Err(Fallback);
    }
    let q = match parse_query_geometry(arg).ok_or(Fallback)? {
        QGeom::Planar(g) => g,
        QGeom::Sphere { .. } => return Err(Fallback),
    };
    for v in values.iter().flatten() {
        if let Some(g) = parse_doc_geometry(v) {
            if g.relate(&q).is_intersects() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn within(field: Bson, q: bson::Document) -> R {
        op_geo_within(&[Some(field)], &Bson::Document(q))
    }
    fn xy(x: f64, y: f64) -> Bson {
        Bson::Array(vec![Bson::Double(x), Bson::Double(y)])
    }

    #[test]
    fn box_contains_point() {
        let q = doc! {"$box": [[0.0, 0.0], [10.0, 10.0]]};
        assert!(within(xy(5.0, 5.0), q.clone()).unwrap());
        assert!(!within(xy(50.0, 5.0), q).unwrap());
    }

    #[test]
    fn polygon_query_contains_point() {
        let q = doc! {"$polygon": [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]};
        assert!(within(xy(5.0, 5.0), q.clone()).unwrap());
        assert!(!within(xy(20.0, 20.0), q).unwrap());
    }

    #[test]
    fn geometry_polygon_contains_point() {
        let q = doc! {"$geometry": {
            "type": "Polygon",
            "coordinates": [[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]],
        }};
        // GeoJSON Point doc + legacy {lng,lat} doc both work.
        assert!(within(
            Bson::Document(doc! {"type": "Point", "coordinates": [5.0, 5.0]}),
            q.clone()
        )
        .unwrap());
        assert!(within(Bson::Document(doc! {"lng": 5.0, "lat": 5.0}), q.clone()).unwrap());
        assert!(!within(xy(99.0, 99.0), q).unwrap());
    }

    #[test]
    fn center_sphere_cap() {
        // ~0.1 rad cap at the origin. The origin is in; a point ~14 deg away is out.
        let q = doc! {"$centerSphere": [[0.0, 0.0], 0.1]};
        assert!(within(xy(0.0, 0.0), q.clone()).unwrap());
        assert!(!within(xy(10.0, 10.0), q).unwrap());
    }

    #[test]
    fn center_defers_to_python() {
        // $center uses a Shapely 64-gon buffer we don't reproduce -> Fallback.
        let q = doc! {"$center": [[0.0, 0.0], 5.0]};
        assert!(within(xy(1.0, 1.0), q).is_err());
    }

    #[test]
    fn non_geometry_value_does_not_match() {
        let q = doc! {"$box": [[0.0, 0.0], [10.0, 10.0]]};
        // A scalar field value isn't a geometry -> skipped, no match (not Fallback).
        assert!(!within(Bson::Int32(5), q).unwrap());
    }

    #[test]
    fn geo_intersects_requires_geometry() {
        let arg = Bson::Document(doc! {"$box": [[0.0, 0.0], [10.0, 10.0]]});
        assert!(op_geo_intersects(&[Some(xy(5.0, 5.0))], &arg).is_err());
        let q = Bson::Document(doc! {"$geometry": {
            "type": "Polygon",
            "coordinates": [[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]],
        }});
        assert!(op_geo_intersects(&[Some(xy(5.0, 5.0))], &q).unwrap());
    }
}
