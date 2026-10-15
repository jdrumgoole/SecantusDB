//! The order mongod 8.2 reads documents through a geo index in.
//!
//! A `$geoWithin` or `$geoIntersects` that mongod answers from a geo index
//! is an index scan: the query's region becomes a covering of cells, the
//! cells become key intervals, and the scan returns the index entries
//! inside them by key and then by insertion, a document where its first
//! entry falls. So the order is that of the keys mongod's index holds:
//!
//! - `2dsphere` (version 3): a point is keyed by its leaf S2 cell, anything
//!   else by its covering between the index's coarsest and finest levels
//!   (defaults 2 and 16, at most 20 cells). A key is the cell id as a signed
//!   64-bit integer.
//! - `2d`: a point is keyed by its geohash at the index's `bits`.
//!
//! The query covering is mongod's own (levels 0 to 23, 20 cells, plus each
//! covering cell's ancestors down to the index's coarsest level as exact
//! keys, for the larger shapes keyed at a coarser cell). Both coverings come
//! from [`crate::s2legacy`] and [`crate::geohash2d`], transcriptions of the
//! code mongod runs.
//!
//! `$near` is in [`near`]: mongod's search in widening rings.

pub mod near;
mod parse;

use crate::geohash2d::{Box2, Converter, GeoHash, R2Region};
use crate::s2legacy::{CellId, LatLngRect, Region, RegionCoverer};
use bson::{Bson, Document};
use std::cmp::Ordering;

pub use parse::{flat_pair, query_region, stored as stored_sphere, stored_all, Shape};

/// One field of an index key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyPart {
    /// A `2dsphere` cell, as the signed integer the index stores.
    Cell(i64),
    /// A `2d` geohash: eight big-endian bytes, compared unsigned.
    Hash(u64),
    /// Any other field: its sort key (`sortkey::encode_value`).
    Value(Vec<u8>),
}

pub type Key = Vec<KeyPart>;

/// The bounds of one field of an index scan.
#[derive(Clone, Debug)]
pub enum FieldBounds {
    Cells(Vec<(i64, i64)>),
    Hashes(Vec<(u64, u64)>),
    /// One value (an equality on a trailing field).
    Point(Vec<u8>),
    /// `[MinKey, MaxKey]`.
    Full,
}

impl FieldBounds {
    fn admits(&self, part: &KeyPart) -> bool {
        match (self, part) {
            (FieldBounds::Full, _) => true,
            (FieldBounds::Cells(list), KeyPart::Cell(k)) => {
                list.iter().any(|(lo, hi)| lo <= k && k <= hi)
            }
            (FieldBounds::Hashes(list), KeyPart::Hash(k)) => {
                list.iter().any(|(lo, hi)| lo <= k && k <= hi)
            }
            (FieldBounds::Point(v), KeyPart::Value(k)) => v == k,
            _ => false,
        }
    }
}

/// What kind of geo index, with its parameters.
#[derive(Clone, Debug)]
pub enum Kind {
    Sphere {
        finest: i32,
        coarsest: i32,
        max_cells: i32,
    },
    Flat(Converter),
}

/// A geo index as mongod keys it.
#[derive(Clone, Debug)]
pub struct GeoIndex {
    pub name: String,
    pub kind: Kind,
    /// The key pattern's fields in order, and which one is the geo field.
    pub fields: Vec<String>,
    pub geo_at: usize,
}

fn as_i64(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) if d.fract() == 0.0 => Some(*d as i64),
        _ => None,
    }
}

fn as_f64(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Double(d) => Some(*d),
        _ => None,
    }
}

impl GeoIndex {
    /// The index an index spec (as `listIndexes` gives it) describes, when
    /// it is a geo index this module reads: a `2d` index, or a version 3
    /// `2dsphere` index. Older `2dsphere` versions key by cell strings.
    pub fn from_spec(spec: &Document) -> Option<GeoIndex> {
        let key = spec.get_document("key").ok()?;
        let name = spec.get_str("name").ok()?.to_string();
        let fields: Vec<String> = key.keys().cloned().collect();
        let (geo_at, kind) = key
            .values()
            .enumerate()
            .find_map(|(at, v)| match v.as_str() {
                Some(k @ ("2d" | "2dsphere")) => Some((at, k)),
                _ => None,
            })?;
        let kind = if kind == "2dsphere" {
            let version = spec
                .get("2dsphereIndexVersion")
                .and_then(as_i64)
                .unwrap_or(1);
            if version != 3 {
                return None;
            }
            let level =
                |k: &str, default: i32| spec.get(k).and_then(as_i64).map_or(default, |v| v as i32);
            let radius = crate::geoshape::EARTH_RADIUS_METERS;
            Kind::Sphere {
                finest: level(
                    "finestIndexedLevel",
                    crate::s2legacy::AVG_EDGE.closest_level(110.0 / radius),
                ),
                coarsest: level(
                    "coarsestIndexedLevel",
                    crate::s2legacy::AVG_EDGE.closest_level(2_000_000.0 / radius),
                ),
                max_cells: level("maxCellsInCovering", 20),
            }
        } else {
            if geo_at != 0 {
                return None;
            }
            let bits = spec.get("bits").and_then(as_i64).unwrap_or(26);
            let min = spec.get("min").and_then(as_f64).unwrap_or(-180.0);
            let max = spec.get("max").and_then(as_f64).unwrap_or(180.0);
            if !(1..=32).contains(&bits) || min >= max || !(max - min).is_finite() {
                return None;
            }
            Kind::Flat(Converter::new(bits as u32, min, max))
        };
        Some(GeoIndex {
            name,
            kind,
            fields,
            geo_at,
        })
    }

    pub fn geo_field(&self) -> &str {
        &self.fields[self.geo_at]
    }

    pub fn is_sphere(&self) -> bool {
        matches!(self.kind, Kind::Sphere { .. })
    }

    /// The geo field's keys for a stored value.
    pub fn geo_keys(&self, value: &Bson) -> Vec<KeyPart> {
        match &self.kind {
            Kind::Sphere {
                finest,
                coarsest,
                max_cells,
            } => match stored_sphere(value) {
                Some(Shape::Point(p)) => vec![KeyPart::Cell(CellId::from_point(p).key())],
                Some(shape) => RegionCoverer {
                    min_level: *coarsest,
                    max_level: *finest,
                    level_mod: 1,
                    max_cells: *max_cells,
                }
                .covering(&shape.region())
                .into_iter()
                .map(|c| KeyPart::Cell(c.key()))
                .collect(),
                None => Vec::new(),
            },
            Kind::Flat(conv) => flat_points(value)
                .into_iter()
                .filter_map(|(x, y)| conv.hash(x, y))
                .map(|h| KeyPart::Hash(h.hash))
                .collect(),
        }
    }

    /// Every key a document holds in the index: its geo keys, each with the
    /// values of the other fields (one key per value of a field holding an
    /// array). None when the geo field holds nothing the index keys.
    pub fn keys(&self, doc: &Document) -> Vec<Key> {
        let Some(geo) = crate::get_path(doc, self.geo_field()) else {
            return Vec::new();
        };
        let mut keys: Vec<Key> = vec![Vec::new()];
        for (at, field) in self.fields.iter().enumerate() {
            let parts: Vec<KeyPart> = if at == self.geo_at {
                self.geo_keys(geo)
            } else {
                let values = match crate::get_path(doc, field) {
                    None => vec![Bson::Null],
                    Some(Bson::Array(a)) if a.is_empty() => vec![Bson::Undefined],
                    Some(Bson::Array(a)) => a.clone(),
                    Some(v) => vec![v.clone()],
                };
                values
                    .iter()
                    .map(|v| {
                        KeyPart::Value(crate::sortkey::encode_value(v, None).unwrap_or_default())
                    })
                    .collect()
            };
            if parts.is_empty() {
                return Vec::new();
            }
            keys = keys
                .into_iter()
                .flat_map(|k| {
                    parts.iter().map(move |p| {
                        let mut k = k.clone();
                        k.push(p.clone());
                        k
                    })
                })
                .collect();
        }
        keys
    }

    /// The bounds of the geo field for a `$geoWithin` / `$geoIntersects`
    /// (`op`) with operand `arg`, or `None` when this index cannot answer
    /// it.
    pub fn predicate_bounds(&self, op: &str, arg: &Bson) -> Option<FieldBounds> {
        match &self.kind {
            Kind::Sphere { coarsest, .. } => {
                let region = query_region(arg)?;
                Some(FieldBounds::Cells(sphere_intervals(
                    &query_covering(&region),
                    *coarsest,
                )))
            }
            Kind::Flat(conv) => {
                if op == "$geoIntersects" {
                    return None;
                }
                let region = flat_region(arg)?;
                let mut cells = conv.covering(&region, 16);
                cells.sort();
                cells.dedup();
                Some(FieldBounds::Hashes(
                    cells.iter().map(|c| (c.hash_min(), c.hash_max())).collect(),
                ))
            }
        }
    }
}

/// The points a stored value holds for a `2d` index (`get2DKeys`): one
/// legacy pair, or each of an array of them.
fn flat_points(v: &Bson) -> Vec<(f64, f64)> {
    let single = match v {
        Bson::Array(a) => a.first().and_then(as_f64).is_some(),
        Bson::Document(d) => d.values().next().and_then(as_f64).is_some(),
        _ => return Vec::new(),
    };
    if single {
        return flat_pair(v).into_iter().collect();
    }
    let members: Vec<&Bson> = match v {
        Bson::Array(a) => a.iter().collect(),
        Bson::Document(d) => d.values().collect(),
        _ => Vec::new(),
    };
    members.into_iter().filter_map(flat_pair).collect()
}

/// mongod's query covering of a region on the sphere
/// (`ExpressionMapping::get2dsphereCovering`).
pub fn query_covering(region: &Region) -> Vec<CellId> {
    RegionCoverer {
        min_level: 0,
        max_level: 23,
        level_mod: 1,
        max_cells: 20,
    }
    .covering(region)
}

/// `S2CellIdsToIntervalsWithParents`: each cell's range, and each of its
/// ancestors down to `coarsest` as a single key; sorted.
pub fn sphere_intervals(cover: &[CellId], coarsest: i32) -> Vec<(i64, i64)> {
    let mut exact = std::collections::BTreeSet::new();
    for &cell in cover {
        let mut c = cell;
        while c.level() > coarsest {
            c = c.immediate_parent();
            exact.insert(c);
        }
    }
    let mut out: Vec<(i64, i64)> = exact.into_iter().map(|c| (c.key(), c.key())).collect();
    out.extend(
        cover
            .iter()
            .map(|c| (c.range_min().key(), c.range_max().key())),
    );
    out.sort();
    out
}

/// A `2d` query shape as mongod's flat region.
fn flat_region(arg: &Bson) -> Option<R2Region> {
    let Bson::Document(d) = arg else { return None };
    let (key, v) = d.iter().find(|(k, _)| k.as_str() != "$uniqueDocs")?;
    let pair = |b: &Bson| flat_pair(b);
    let parts = || -> Option<Vec<&Bson>> {
        match v {
            Bson::Array(a) => Some(a.iter().collect()),
            Bson::Document(d) => Some(d.values().collect()),
            _ => None,
        }
    };
    match key.as_str() {
        "$box" => {
            let p = parts()?;
            Some(R2Region::Box(Box2::new(
                pair(p.first()?)?,
                pair(p.get(1)?)?,
            )))
        }
        "$center" => {
            let p = parts()?;
            Some(R2Region::Circle(pair(p.first()?)?, as_f64(p.get(1)?)?))
        }
        "$polygon" => {
            let points: Option<Vec<(f64, f64)>> = parts()?.into_iter().map(pair).collect();
            let points = points?;
            let first = *points.first()?;
            let bounds = points.iter().fold(Box2::new(first, first), |b, p| {
                Box2::new(
                    (b.min.0.min(p.0), b.min.1.min(p.1)),
                    (b.max.0.max(p.0), b.max.1.max(p.1)),
                )
            });
            Some(R2Region::Polygon(points, bounds))
        }
        "$centerSphere" => {
            let Some(Region::Cap(cap)) = query_region(arg) else {
                return None;
            };
            let r: LatLngRect = cap.rect_bound();
            let deg = 180.0 / std::f64::consts::PI;
            Some(R2Region::Bounds(Box2::new(
                (r.lng.lo * deg, r.lat.lo * deg),
                (r.lng.hi * deg, r.lat.hi * deg),
            )))
        }
        _ => None,
    }
}

/// The order an index scan returns documents in: `entries` are each
/// document's keys (document `i` in insertion order), `bounds` one per key
/// field. A document comes where its first key inside the bounds falls,
/// equal keys in insertion order. Documents with no key inside are left out.
pub fn scan_order(entries: &[Vec<Key>], bounds: &[FieldBounds]) -> Vec<usize> {
    let mut inside: Vec<(&Key, usize)> = Vec::new();
    for (doc, keys) in entries.iter().enumerate() {
        for key in keys {
            if key.iter().zip(bounds).all(|(part, b)| b.admits(part)) {
                inside.push((key, doc));
            }
        }
    }
    inside.sort_by(|a, b| match a.0.cmp(b.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });
    let mut seen = vec![false; entries.len()];
    inside
        .into_iter()
        .filter_map(|(_, doc)| (!std::mem::replace(&mut seen[doc], true)).then_some(doc))
        .collect()
}

/// A geohash of a `2d` index (for a test or a probe).
pub fn flat_hash(conv: &Converter, x: f64, y: f64) -> Option<GeoHash> {
    conv.hash(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn sphere_index() -> GeoIndex {
        GeoIndex::from_spec(
            &doc! { "key": { "loc": "2dsphere" }, "name": "g", "2dsphereIndexVersion": 3 },
        )
        .unwrap()
    }

    fn flat_index() -> GeoIndex {
        GeoIndex::from_spec(&doc! { "key": { "loc": "2d" }, "name": "f" }).unwrap()
    }

    #[test]
    fn a_point_query_is_its_leaf_and_the_leafs_ancestors() {
        // mongod 8.2.11's `explain` of `{loc: {$geoIntersects: {$geometry:
        // {type: "Point", coordinates: [1, 1]}}}}` over a 2dsphere index.
        let want: [(i64, i64); 22] = [
            (1153202979583557632, 1153202979583557632),
            (1153273348327735296, 1153273348327735296),
            (1153277746374246400, 1153277746374246400),
            (1153277815093723136, 1153277815093723136),
            (1153277832273592320, 1153277832273592320),
            (1153277836568559616, 1153277836568559616),
            (1153277837642301440, 1153277837642301440),
            (1153277837650690049, 1153277837650722815),
            (1153277837650755584, 1153277837650755584),
            (1153277837650952192, 1153277837650952192),
            (1153277837651738624, 1153277837651738624),
            (1153277837654884352, 1153277837654884352),
            (1153277837659078656, 1153277837659078656),
            (1153277837709410304, 1153277837709410304),
            (1153277837910736896, 1153277837910736896),
            (1153278021252153344, 1153278021252153344),
            (1153278845885874176, 1153278845885874176),
            (1153290940513779712, 1153290940513779712),
            (1154047404513689600, 1154047404513689600),
            (1157425104234217472, 1157425104234217472),
            (1170935903116328960, 1170935903116328960),
            (1224979098644774912, 1224979098644774912),
        ];
        let arg = bson::bson!({ "$geometry": { "type": "Point", "coordinates": [1, 1] } });
        let Some(FieldBounds::Cells(got)) = sphere_index().predicate_bounds("$geoIntersects", &arg)
        else {
            panic!("no bounds");
        };
        assert_eq!(got, want);
    }

    #[test]
    fn a_cap_query_covers_with_twenty_cells() {
        // mongod 8.2.11: 56 intervals for `$centerSphere: [[0, 0], 0.05]`,
        // first and last as below.
        let arg = bson::bson!({ "$centerSphere": [[0, 0], 0.05] });
        let Some(FieldBounds::Cells(got)) = sphere_index().predicate_bounds("$geoWithin", &arg)
        else {
            panic!("no bounds");
        };
        assert_eq!(got.len(), 56);
        assert_eq!(got[3], (381680068419649537, 382243018373070847));
        assert_eq!(got[55], (1945555039024054272, 1945555039024054272));
    }

    #[test]
    fn a_2d_box_query_is_a_geohash_covering() {
        // mongod 8.2.11: `$box: [[0, 0], [3, 3]]` over a default 2d index.
        let arg = bson::bson!({ "$box": [[0, 0], [3, 3]] });
        let Some(FieldBounds::Hashes(got)) = flat_index().predicate_bounds("$geoWithin", &arg)
        else {
            panic!("no bounds");
        };
        assert_eq!(got.len(), 16);
        assert_eq!(got[0], (0x3FFF_FFFF_FFFF_F000, 0x3FFF_FFFF_FFFF_FFFF));
        assert_eq!(got[7], (0xC000_0000_0000_0000, 0xC000_FFFF_FFFF_FFFF));
        assert_eq!(got[15], (0xC00C_0000_0000_0000, 0xC00C_0FFF_FFFF_FFFF));
    }

    #[test]
    fn a_line_is_keyed_by_its_covering() {
        // A point is its leaf; a line its covering at levels 2 to 16, which
        // for this one starts at a cell coarser than the point's leaf.
        let ix = sphere_index();
        let point = ix.geo_keys(&bson::bson!({ "type": "Point", "coordinates": [0, 0] }));
        let line =
            ix.geo_keys(&bson::bson!({ "type": "LineString", "coordinates": [[0, 0], [2, 2]] }));
        assert_eq!(point.len(), 1);
        assert!(line.len() > 1 && line.len() <= 20);
        assert!(line.iter().min() < point.iter().min());
    }

    #[test]
    fn a_scan_returns_a_document_at_its_first_key() {
        let keys = vec![
            vec![vec![KeyPart::Cell(5)], vec![KeyPart::Cell(1)]],
            vec![vec![KeyPart::Cell(3)]],
            vec![vec![KeyPart::Cell(3)]],
            vec![vec![KeyPart::Cell(9)]],
        ];
        let bounds = [FieldBounds::Cells(vec![(2, 6)])];
        // Document 0's key 1 is outside: it comes at 5. Document 3 is not
        // reached.
        assert_eq!(scan_order(&keys, &bounds), [1, 2, 0]);
    }

    #[test]
    fn near_ties_come_in_the_queues_order() {
        // mongod 8.2.11: four points at distance 1 from a 2d `$near` at the
        // origin come back 2, 3, 0, 1.
        let docs: Vec<Document> = [[1, 0], [0, 1], [-1, 0], [0, -1]]
            .iter()
            .enumerate()
            .map(|(i, p)| doc! { "_id": i as i32, "loc": [p[0], p[1]] })
            .collect();
        let near = crate::geoshape::near_condition(&doc! { "$near": [0, 0] })
            .unwrap()
            .unwrap();
        let ix = flat_index();
        let base = vec![FieldBounds::Full];
        let order = near::near_order(&ix, &base, &near, &docs, &[true; 4]).unwrap();
        assert_eq!(order, [2, 3, 0, 1]);
    }
}
