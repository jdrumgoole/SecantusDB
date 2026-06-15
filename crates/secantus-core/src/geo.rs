//! Geospatial query operators (`$geoWithin` / `$geoIntersects` / `$near` /
//! `$nearSphere`) for the Rust query engine — a bounded port of `secantus.geo`
//! (Phase 4, slices geo-1 + geo-1b).
//!
//! Planar containment/intersection uses the `geo` crate's DE-9IM `Relate`, which
//! shares the OGC lineage of Shapely (the pure-Python path), so `is_within` /
//! `is_intersects` line up. The spherical cap (`$centerSphere`) and `$near`
//! distance use haversine directly (Shapely is planar), matching `geo.py`.
//! `$near` is a containment test here (within `[$minDistance, $maxDistance]`);
//! sort-by-distance is the command layer's job.
//!
//! Deferred to Python via `Fallback` (the engine contract):
//! * `$center` — Shapely approximates the disk with a 64-vertex buffer we can't
//!   reproduce exactly (an exact circle would diverge on the sub-degree annulus).
//! * the legacy `$near` *sibling* `$maxDistance`/`$minDistance` form — those are
//!   separate (unknown) operators in the condition doc, so it falls back
//!   automatically.
//! * malformed / unrecognised query shapes — Python raises the proper
//!   `QueryError`.

use crate::query::Fallback;
use bson::{Bson, Document};
use geo::{BoundingRect, Coord, CoordsIter, Geometry, LineString, MultiLineString, MultiPoint};
use geo::{MultiPolygon, Point, Polygon, Relate};

type R = Result<bool, Fallback>;

/// Mean Earth radius in metres — mongod's `$centerSphere` / `$nearSphere`
/// constant. Mirrors `geo.EARTH_RADIUS_METERS`.
const EARTH_RADIUS_METERS: f64 = 6_378_100.0;

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

/// `(center, max, min, spherical, legacy)` for a `$near` arg, or `Fallback` on a
/// shape Python rejects / we don't reproduce. Mirrors `query._parse_near_spec`
/// for the non-sibling forms: the legacy `[x,y]`/`[x,y,max]` list and the
/// GeoJSON `{$geometry: Point, $maxDistance, $minDistance}` doc. (The legacy
/// *sibling* `$maxDistance` form falls back automatically — `$maxDistance` is a
/// separate, unknown operator in the condition doc.)
#[allow(clippy::type_complexity)]
fn parse_near_spec(
    arg: &Bson,
    default_spherical: bool,
) -> Result<((f64, f64), Option<f64>, Option<f64>, bool, bool), Fallback> {
    let opt_number = |b: Option<&Bson>| -> Result<Option<f64>, Fallback> {
        match b {
            None => Ok(None),
            Some(v) => num(v).map(Some).ok_or(Fallback),
        }
    };
    match arg {
        Bson::Document(d) => {
            let geom = d.get_document("$geometry").map_err(|_| Fallback)?;
            if geom.get_str("type") != Ok("Point") {
                return Err(Fallback);
            }
            let c = pair(geom.get("coordinates").ok_or(Fallback)?).ok_or(Fallback)?;
            Ok((
                (c.x, c.y),
                opt_number(d.get("$maxDistance"))?,
                opt_number(d.get("$minDistance"))?,
                true,  // GeoJSON form is spherical
                false, // distances already in metres
            ))
        }
        Bson::Array(a) if a.len() == 2 || a.len() == 3 => {
            let cx = num(&a[0]).ok_or(Fallback)?;
            let cy = num(&a[1]).ok_or(Fallback)?;
            let max_d = if a.len() == 3 {
                Some(num(&a[2]).ok_or(Fallback)?)
            } else {
                None
            };
            Ok(((cx, cy), max_d, None, default_spherical, true))
        }
        _ => Err(Fallback),
    }
}

/// `$near` / `$nearSphere` *matching* (not ranking): a doc matches if a
/// point-valued geometry lies within `[$minDistance, $maxDistance]` of the
/// centre. Sort-by-distance is the command layer's job. Mirrors
/// `query._op_geo_near`.
pub fn op_geo_near(values: &[Option<Bson>], arg: &Bson, default_spherical: bool) -> R {
    let (center, mut max_d, mut min_d, spherical, legacy) =
        parse_near_spec(arg, default_spherical)?;
    // Legacy + spherical: the bound is radians on the unit sphere; convert to
    // the metres that `distance(spherical=true)` returns.
    if legacy && spherical {
        max_d = max_d.map(|m| m * EARTH_RADIUS_METERS);
        min_d = min_d.map(|m| m * EARTH_RADIUS_METERS);
    }
    for v in values.iter().flatten() {
        // $near is restricted to point geometries (mirroring geo.distance,
        // which returns None for non-points -> skipped).
        let p = match parse_doc_geometry(v) {
            Some(Geometry::Point(pt)) => (pt.x(), pt.y()),
            _ => continue,
        };
        let d = if spherical {
            haversine(center.0, center.1, p.0, p.1) * EARTH_RADIUS_METERS
        } else {
            ((center.0 - p.0).powi(2) + (center.1 - p.1).powi(2)).sqrt()
        };
        if max_d.is_some_and(|mx| d > mx) || min_d.is_some_and(|mn| d < mn) {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

/// Distance from `near` (lng, lat) to the point geometry in `value`, for
/// `$geoNear`'s `distanceField`. Metres (great-circle / haversine) when
/// `spherical`, planar units otherwise. `None` when `value` isn't a point
/// geometry (GeoJSON `Point` or legacy `[x, y]`). Mirrors `geo.distance` for the
/// point case. Sort-by-distance is the command layer's job.
pub fn point_distance(near: (f64, f64), value: &Bson, spherical: bool) -> Option<f64> {
    let p = match parse_doc_geometry(value) {
        Some(Geometry::Point(pt)) => (pt.x(), pt.y()),
        _ => return None,
    };
    Some(if spherical {
        haversine(near.0, near.1, p.0, p.1) * EARTH_RADIUS_METERS
    } else {
        ((near.0 - p.0).powi(2) + (near.1 - p.1).powi(2)).sqrt()
    })
}

// --- 2d geohash index support (geo-2) -------------------------------------
//
// `2d` indexes bucket a planar point into a bit-interleaved (Z-order) geohash.
// Storage writes one cell per point doc; a `$geoWithin` query scans the Z-order
// range spanning the query geometry's bounding box (a superset — the post-filter
// `matches()` discards false positives). Mirrors `secantus.geo_index`'s 2d path.

fn bucket(value: f64, lo: f64, hi: f64, bits: u32) -> u64 {
    if hi <= lo {
        return 0;
    }
    let norm = (value - lo) / (hi - lo);
    if norm <= 0.0 {
        return 0;
    }
    if norm >= 1.0 {
        return (1u64 << bits) - 1;
    }
    (norm * (1u64 << bits) as f64) as u64
}

fn interleave(bx: u64, by: u64, bits: u32) -> u64 {
    let mut r = 0u64;
    for i in 0..bits {
        r |= ((bx >> i) & 1) << (2 * i);
        r |= ((by >> i) & 1) << (2 * i + 1);
    }
    r
}

/// The interleaved geohash cell for a planar point under a `2d` index's
/// (`bits`, `min`, `max`). Mirrors `geo_index.planar_2d_index_for_point`.
pub fn cell_2d(x: f64, y: f64, bits: u32, lo: f64, hi: f64) -> u64 {
    interleave(bucket(x, lo, hi, bits), bucket(y, lo, hi, bits), bits)
}

/// The coarse Z-order `(lo, hi)` cell range covering a bounding box (a superset;
/// false positives are filtered by the per-doc `matches()`). Mirrors
/// `geo_index.planar_2d_covering`.
pub fn covering_2d(
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    bits: u32,
    lo: f64,
    hi: f64,
) -> (u64, u64) {
    (
        cell_2d(min_x, min_y, bits, lo, hi),
        cell_2d(max_x, max_y, bits, lo, hi),
    )
}

/// A cell ID as fixed-width 8-byte big-endian (lex order == cell order, so a WT
/// range scan visits cells in order). Mirrors `geo_index.encode_cell`.
pub fn encode_cell(cell: u64) -> [u8; 8] {
    cell.to_be_bytes()
}

/// The `(x, y)` of a point-like doc field value (GeoJSON Point, legacy `[x,y]`,
/// `{x,y}`/`{lng,lat}`), or `None` — used to write `2d` index entries (mongod's
/// `2d` index is point-only).
pub fn doc_point(v: &Bson) -> Option<(f64, f64)> {
    match parse_doc_geometry(v) {
        Some(Geometry::Point(p)) => Some((p.x(), p.y())),
        _ => None,
    }
}

/// The bounding box `(min_x, min_y, max_x, max_y)` of any doc geometry, or
/// `None` if the value isn't a geometry — used to S2-cover non-point docs for a
/// `2dsphere` index (mongod / s2sphere cover shapes via their bounding rect).
pub fn doc_bbox(v: &Bson) -> Option<(f64, f64, f64, f64)> {
    let r = parse_doc_geometry(v)?.bounding_rect()?;
    Some((r.min().x, r.min().y, r.max().x, r.max().y))
}

/// The bounding box `(min_x, min_y, max_x, max_y)` of a `$geoWithin` query
/// geometry, for the `2d` covering range. `None` for shapes whose *matching*
/// also defers to Python (`$center`) — no point indexing a query the post-filter
/// can't evaluate.
pub fn query_within_bbox(arg: &Document) -> Option<(f64, f64, f64, f64)> {
    match parse_query_geometry(arg)? {
        QGeom::Planar(g) => {
            let r = g.bounding_rect()?;
            Some((r.min().x, r.min().y, r.max().x, r.max().y))
        }
        QGeom::Sphere { lng, lat, rad } => {
            let d = rad.to_degrees();
            Some((lng - d, lat - d, lng + d, lat + d))
        }
    }
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

    #[test]
    fn near_legacy_planar_bound() {
        // {$near: [0,0, 5]} (planar): a point ~1.41 away matches, ~7.07 doesn't.
        let arg = Bson::Array(vec![0.0.into(), 0.0.into(), 5.0.into()]);
        assert!(op_geo_near(&[Some(xy(1.0, 1.0))], &arg, false).unwrap());
        assert!(!op_geo_near(&[Some(xy(5.0, 5.0))], &arg, false).unwrap());
        // Bound-less {$near: [0,0]} matches any point geometry.
        let bare = Bson::Array(vec![0.0.into(), 0.0.into()]);
        assert!(op_geo_near(&[Some(xy(99.0, 99.0))], &bare, false).unwrap());
        // ...but not a non-geometry value.
        assert!(!op_geo_near(&[Some(Bson::Int32(3))], &bare, false).unwrap());
    }

    #[test]
    fn near_geojson_is_spherical_meters() {
        // GeoJSON form: distances in metres. 0.2 rad ~ 1.28e6 m at the equator.
        let arg = Bson::Document(doc! {
            "$geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
            "$maxDistance": 1_500_000.0_f64,
        });
        assert!(op_geo_near(&[Some(xy(0.0, 0.0))], &arg, true).unwrap()); // dist 0
                                                                          // ~10 deg away is ~1.11e6 m (< 1.5e6) -> in; tighten the bound to exclude.
        let tight = Bson::Document(doc! {
            "$geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
            "$maxDistance": 100_000.0_f64,
        });
        assert!(!op_geo_near(&[Some(xy(10.0, 10.0))], &tight, true).unwrap());
    }

    #[test]
    fn near_malformed_defers() {
        // A doc arg without $geometry -> Python raises QueryError -> Fallback.
        let arg = Bson::Document(doc! {"$maxDistance": 5.0_f64});
        assert!(op_geo_near(&[Some(xy(0.0, 0.0))], &arg, false).is_err());
    }

    #[test]
    fn doc_point_extracts_point_shapes_only() {
        assert_eq!(doc_point(&xy(3.0, 4.0)), Some((3.0, 4.0)));
        assert_eq!(
            doc_point(&Bson::Document(
                doc! {"type": "Point", "coordinates": [1.0, 2.0]}
            )),
            Some((1.0, 2.0))
        );
        assert_eq!(
            doc_point(&Bson::Document(doc! {"lng": 5.0, "lat": 6.0})),
            Some((5.0, 6.0))
        );
        assert_eq!(doc_point(&Bson::Int32(7)), None);
        // A polygon doc isn't a point.
        assert_eq!(
            doc_point(&Bson::Document(doc! {
                "type": "Polygon",
                "coordinates": [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]]],
            })),
            None
        );
    }

    #[test]
    fn covering_contains_in_box_cell() {
        let (bits, lo, hi) = (8u32, -180.0, 180.0);
        let (clo, chi) = covering_2d(0.0, 0.0, 10.0, 10.0, bits, lo, hi);
        // Every cell of a point inside the bbox falls in the Z-order range.
        for &(x, y) in &[(0.0, 0.0), (5.0, 5.0), (10.0, 10.0), (2.5, 7.5)] {
            let c = cell_2d(x, y, bits, lo, hi);
            assert!(
                clo <= c && c <= chi,
                "({x},{y}) cell {c} not in [{clo},{chi}]"
            );
        }
        // encode_cell is order-preserving (lex bytes == numeric order).
        assert!(encode_cell(clo) <= encode_cell(chi));
    }

    #[test]
    fn query_bbox_shapes() {
        let b = query_within_bbox(&doc! {"$box": [[0.0, 0.0], [10.0, 20.0]]}).unwrap();
        assert_eq!(b, (0.0, 0.0, 10.0, 20.0));
        // $centerSphere -> center +/- radius-in-degrees.
        let s = query_within_bbox(&doc! {"$centerSphere": [[0.0, 0.0], 0.1]}).unwrap();
        let d = 0.1_f64.to_degrees();
        assert!((s.0 + d).abs() < 1e-9 && (s.2 - d).abs() < 1e-9);
        // $center defers (matching also defers) -> no bbox.
        assert_eq!(
            query_within_bbox(&doc! {"$center": [[0.0, 0.0], 5.0]}),
            None
        );
    }
}
