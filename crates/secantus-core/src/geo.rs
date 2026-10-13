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
use geo::{BoundingRect, Coord, Geometry, LineString, MultiLineString, MultiPoint};
use geo::{MultiPolygon, Point, Polygon};

type R = Result<bool, Fallback>;

/// A parsed query geometry.
enum QGeom {
    /// Planar shape — DE-9IM `Relate` against the doc geometry.
    Planar(Geometry<f64>),
    /// Great-circle cap on the unit sphere (`$centerSphere`): center + radius
    /// in radians.
    Sphere { lng: f64, lat: f64, rad: f64 },
    /// Planar disk (`$center`): centre + radius, exact Euclidean containment.
    /// mongod's legacy `$center` is an exact disk (`secantus.geo` approximates it
    /// with a Shapely 64-gon; the exact form is what real `mongod` matches).
    Disk { cx: f64, cy: f64, r: f64 },
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
    if let Some(b) = arg.get("$center") {
        return parse_center(b);
    }
    if let Some(b) = arg.get("$centerSphere") {
        return parse_center_sphere(b);
    }
    None
}

/// `$center: [[cx, cy], r]` — a planar disk. Mirrors `geo._parse_center` but as
/// an exact disk (mongod semantics) rather than a Shapely polygon buffer.
fn parse_center(b: &Bson) -> Option<QGeom> {
    let a = b.as_array()?;
    if a.len() != 2 {
        return None;
    }
    let c = pair(&a[0])?;
    let r = num(&a[1])?;
    if r < 0.0 {
        return None;
    }
    Some(QGeom::Disk {
        cx: c.x,
        cy: c.y,
        r,
    })
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

/// `$geoWithin` (and its old name `$within`): whether some value of the
/// field is a geometry inside the query's shape. The shape is parsed, and
/// refused as mongod refuses it, whether or not there is a value to test.
pub fn op_geo_within(values: &[crate::query::Cand], arg: &Bson, op: &str) -> R {
    let shape = crate::geoshape::within_shape(op, arg)?;
    Ok(values
        .iter()
        .filter_map(|c| c.value)
        .flat_map(crate::geoshape::stored)
        .any(|g| crate::geoshape::is_within(&g, &shape)))
}

/// `$geoIntersects`: whether some value of the field is a geometry with a
/// point in common with the query's.
pub fn op_geo_intersects(values: &[crate::query::Cand], arg: &Bson) -> R {
    let query = crate::geoshape::intersects_shape("$geoIntersects", arg)?;
    Ok(values
        .iter()
        .filter_map(|c| c.value)
        .flat_map(crate::geoshape::stored)
        .any(|g| crate::geoshape::is_intersecting(&g, &query)))
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

/// The bounding box `(min_x, min_y, max_x, max_y)` of a `$geoWithin` query
/// geometry, for the `2d` covering range. `None` for shapes the post-filter
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
        QGeom::Disk { cx, cy, r } => Some((cx - r, cy - r, cx + r, cy + r)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn within(field: Bson, q: bson::Document) -> R {
        op_geo_within(
            &[crate::query::Cand::plain(Some(&field))],
            &Bson::Document(q),
            "$geoWithin",
        )
    }

    fn xy(x: f64, y: f64) -> Bson {
        Bson::Array(vec![Bson::Double(x), Bson::Double(y)])
    }

    #[test]
    fn a_flat_shape_holds_a_point_on_its_edge() {
        let b = doc! {"$box": [[0.0, 0.0], [3.0, 3.0]]};
        assert_eq!(within(xy(3.0, 3.0), b.clone()), Ok(true));
        assert_eq!(within(xy(4.0, 1.0), b), Ok(false));
        let c = doc! {"$center": [[0.0, 0.0], 2.0_f64.sqrt()]};
        assert_eq!(within(xy(1.0, 1.0), c), Ok(true));
    }

    #[test]
    fn several_locations_match_through_any_one() {
        let both = Bson::Array(vec![xy(1.0, 1.0), xy(30.0, 30.0)]);
        assert_eq!(
            within(both, doc! {"$box": [[0.0, 0.0], [3.0, 3.0]]}),
            Ok(true)
        );
    }

    #[test]
    fn a_value_that_is_no_geometry_does_not_match() {
        let b = doc! {"$box": [[0.0, 0.0], [3.0, 3.0]]};
        assert_eq!(within(Bson::String("x".into()), b.clone()), Ok(false));
        assert_eq!(within(Bson::Null, b), Ok(false));
    }

    #[test]
    fn a_bad_shape_is_refused_with_no_value_to_test() {
        assert!(op_geo_within(&[], &Bson::Int32(1), "$geoWithin").is_err());
        assert!(op_geo_intersects(&[], &Bson::Document(doc! {"$box": [[0, 0], [1, 1]]})).is_err());
    }

    #[test]
    fn covering_contains_in_box_cell() {
        let (lo, hi) = covering_2d(0.0, 0.0, 10.0, 10.0, 26, -180.0, 180.0);
        let cell = cell_2d(5.0, 5.0, 26, -180.0, 180.0);
        assert!(lo <= cell && cell <= hi);
    }

    #[test]
    fn query_bbox_shapes() {
        assert_eq!(
            query_within_bbox(&doc! {"$box": [[0.0, 0.0], [10.0, 10.0]]}),
            Some((0.0, 0.0, 10.0, 10.0))
        );
        assert_eq!(
            query_within_bbox(&doc! {"$center": [[5.0, 5.0], 2.0]}),
            Some((3.0, 3.0, 7.0, 7.0))
        );
    }
}
