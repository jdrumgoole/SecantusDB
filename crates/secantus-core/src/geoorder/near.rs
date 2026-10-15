//! The order of a `$near`: mongod's `GeoNear2DStage` and
//! `GeoNear2DSphereStage` (`src/mongo/db/exec/geo_near.cpp`, `near.cpp`).
//!
//! mongod does not sort a `$near` by distance. It searches rings around the
//! centre, each wider than the last: a density estimate picks the first
//! ring's width from how close the nearest indexed document is, each ring is
//! an index scan of a covering of it, and every document a ring's scan
//! returns goes into a `std::priority_queue` keyed by distance, from which
//! the documents inside the ring are handed out. Nearest first, then; but
//! documents at one distance come out in whatever order the heap's sift
//! steps leave them, which depends on what was pushed, in what order, and
//! what was still in the queue. This replays the search step for step, so
//! the queue sees what mongod's sees.

use super::{sphere_intervals, stored_all, FieldBounds, GeoIndex, Key, Kind};
use crate::geohash2d::{self, distance_compare, R2Region};
use crate::geoshape::{Near, EARTH_RADIUS_METERS};
use crate::heap::{Heap, HeapOrder};
use crate::s2legacy::{
    cell_union_difference, latlng_point, normalize, point_from_radians, Cap, Cell, CellId, Region,
    AVG_EDGE, MIN_WIDTH,
};
use bson::{Bson, Document};
use std::collections::HashSet;
use std::f64::consts::PI;

const MAX_EARTH_DISTANCE: f64 = PI * EARTH_RADIUS_METERS;
const METERS_PER_DEGREE_AT_EQUATOR: f64 = 2.0 * PI * EARTH_RADIUS_METERS / 360.0;

/// `NearStage::SearchResult`: the nearer is the greater, so it is on top.
struct Found {
    distance: f64,
    doc: usize,
}

impl HeapOrder for Found {
    fn heap_less(&self, other: &Self) -> bool {
        self.distance > other.distance
    }
}

/// `R2Annulus`.
#[derive(Clone, Copy)]
struct Annulus {
    centre: (f64, f64),
    inner: f64,
    outer: f64,
}

impl Annulus {
    fn contains(&self, p: (f64, f64)) -> bool {
        distance_compare(p, self.centre, self.inner) >= 0.0
            && distance_compare(p, self.centre, self.outer) <= 0.0
    }
}

/// A document as the search sees it.
struct Doc {
    keys: Vec<Key>,
    /// The rest of the query (all but the `$near`) and, in the plane, the
    /// whole-search annulus: what the ring scans' fetch tests.
    fetched: bool,
    /// `computeGeoNearDistance`; -1 when it holds nothing to measure.
    distance: f64,
}

/// One ring: the covering's key intervals and its distance bounds.
struct Ring {
    geo: FieldBounds,
    min: f64,
    max: f64,
    inclusive_max: bool,
}

/// The order mongod returns the documents of a `$near` in, as indexes into
/// `docs` (the collection, in insertion order). `passes[i]` is whether the
/// near stage's own filter lets document `i` through; `base` holds the
/// bounds of the index's other fields (the geo field's entry is ignored).
/// The rest of a query is a filter mongod runs over what the stage returns,
/// so it does not change the order. `None` when the query is one this does
/// not replay.
pub fn near_order(
    index: &GeoIndex,
    base: &[FieldBounds],
    near: &Near,
    docs: &[Document],
    passes: &[bool],
) -> Option<Vec<usize>> {
    let field = index.geo_field();
    let min = near.min.unwrap_or(0.0);
    let max = near.max.unwrap_or(f64::MAX);
    let (centre, spherical) = (near.centre, near.spherical);
    // `geoNearDistanceBounds` / `twoDDistanceBounds`.
    let full = match &index.kind {
        Kind::Flat(conv) if !spherical => Annulus {
            centre,
            inner: min,
            outer: max.min(1.5 * (conv.max - conv.min)),
        },
        Kind::Flat(_) | Kind::Sphere { .. } => {
            if !spherical {
                return None;
            }
            Annulus {
                centre,
                inner: min.min(MAX_EARTH_DISTANCE),
                outer: max.min(MAX_EARTH_DISTANCE),
            }
        }
    };
    let flat = matches!(index.kind, Kind::Flat(_)) && !spherical;
    let docs: Vec<Doc> = docs
        .iter()
        .zip(passes)
        .map(|(doc, &passes)| {
            let value = crate::get_path(doc, field);
            let in_annulus = !flat
                || value
                    .is_some_and(|v| super::flat_points(v).into_iter().any(|p| full.contains(p)));
            Doc {
                keys: index.keys(doc),
                fetched: passes && in_annulus,
                distance: value.and_then(|v| distance(near, flat, v)).unwrap_or(-1.0),
            }
        })
        .collect();
    let geo_at = index.geo_at;
    let bounded = |key: &Key, geo: &FieldBounds, others: bool| {
        key.iter().enumerate().all(|(at, part)| {
            if at == geo_at {
                geo.admits(part)
            } else {
                !others || base.get(at).is_none_or(|b| b.admits(part))
            }
        })
    };
    // Does any entry fall inside these bounds of the geo field? (The
    // density estimator's index scan, which fetches nothing. A `2d`
    // index's estimate bounds only the geo field.)
    let sphere = index.is_sphere();
    let any_entry = |geo: &FieldBounds| {
        docs.iter()
            .any(|d| d.keys.iter().any(|k| bounded(k, geo, sphere)))
    };

    let mut search = match &index.kind {
        Kind::Sphere { coarsest, .. } => {
            Search::Sphere(SphereSearch::new(centre, full, *coarsest, &any_entry))
        }
        Kind::Flat(conv) => {
            Search::Flat(FlatSearch::new(*conv, centre, full, spherical, &any_entry)?)
        }
    };

    let mut heap: Heap<Found> = Heap::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut out = Vec::new();
    let mut last_returned: Option<usize> = None;
    while let Some(ring) = search.next(last_returned) {
        // Buffering: the ring's index scan, its fetch, and the queue.
        let mut entries: Vec<(&Key, usize)> = Vec::new();
        for (i, d) in docs.iter().enumerate() {
            for k in &d.keys {
                if bounded(k, &ring.geo, true) {
                    entries.push((k, i));
                }
            }
        }
        entries.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)));
        let mut scanned = vec![false; docs.len()];
        for (_, i) in entries {
            if std::mem::replace(&mut scanned[i], true) {
                continue;
            }
            if !docs[i].fetched || seen.contains(&i) {
                continue;
            }
            heap.push(Found {
                distance: docs[i].distance,
                doc: i,
            });
            seen.insert(i);
        }
        // Advancing: hand out what is inside the ring.
        let mut returned = 0;
        while let Some(top) = heap.top() {
            if top.distance < ring.min {
                let gone = heap.pop().expect("a top to pop");
                seen.remove(&gone.doc);
                continue;
            }
            let inside = if ring.inclusive_max {
                top.distance <= ring.max
            } else {
                top.distance < ring.max
            };
            if !inside {
                break;
            }
            let found = heap.pop().expect("a top to pop");
            seen.remove(&found.doc);
            out.push(found.doc);
            returned += 1;
        }
        last_returned = Some(returned);
    }
    Some(out)
}

/// `distance()` in the plane.
fn flat_distance(p: (f64, f64), q: (f64, f64)) -> f64 {
    let a = p.0 - q.0;
    let b = p.1 - q.1;
    if a == 0.0 {
        return b.abs();
    }
    if b == 0.0 {
        return a.abs();
    }
    a.mul_add(a, b * b).sqrt()
}

/// `computeGeoNearDistance`: the least distance from the centre to what
/// the field holds, as mongod measures it: in the plane between points, or
/// on the sphere with S2's projection onto the shape, in metres.
fn distance(near: &Near, flat: bool, value: &Bson) -> Option<f64> {
    let centre = near.centre;
    let least = |a: f64, b: f64| if b < a { b } else { a };
    if flat {
        return super::flat_points(value)
            .into_iter()
            .map(|p| flat_distance(p, centre))
            .reduce(least);
    }
    let point = latlng_point(centre.0, centre.1);
    stored_all(value)
        .iter()
        .map(|s| s.distance(point) * EARTH_RADIUS_METERS)
        .reduce(least)
}

enum Search {
    Sphere(SphereSearch),
    Flat(FlatSearch),
}

impl Search {
    fn next(&mut self, last_returned: Option<usize>) -> Option<Ring> {
        match self {
            Search::Sphere(s) => s.next(last_returned),
            Search::Flat(s) => s.next(last_returned),
        }
    }
}

/// The ring widths: doubled while a ring returns fewer than 300 documents.
fn adjust(increment: &mut f64, last_returned: Option<usize>) {
    match last_returned {
        Some(n) if n < 300 => *increment *= 2.0,
        Some(n) if n > 600 => *increment /= 2.0,
        _ => {}
    }
}

struct SphereSearch {
    centre: (f64, f64),
    full: Annulus,
    current: (f64, f64),
    increment: f64,
    coarsest: i32,
    scanned: Vec<CellId>,
}

impl SphereSearch {
    fn new(
        centre: (f64, f64),
        full: Annulus,
        coarsest: i32,
        any_entry: &dyn Fn(&FieldBounds) -> bool,
    ) -> SphereSearch {
        // The density estimator: the centre's cell and its vertex
        // neighbours, coarser and coarser, until an index entry is in them.
        let centre_id = CellId::from_point(latlng_point(centre.0, centre.1));
        // `min(kMaxCellLevel - 1, internalQueryS2GeoFinestLevel)`.
        let mut level = 23;
        let estimate = loop {
            let ranges: Vec<(i64, i64)> = centre_id
                .vertex_neighbors(level)
                .iter()
                .map(|c| (c.range_min().key(), c.range_max().key()))
                .collect();
            if any_entry(&FieldBounds::Cells(ranges)) {
                break AVG_EDGE.value(level) * EARTH_RADIUS_METERS;
            }
            let width = MIN_WIDTH.value(level + 1) * EARTH_RADIUS_METERS;
            if full.outer < width {
                break width;
            }
            if level > 0 {
                level -= 1;
                continue;
            }
            break AVG_EDGE.value(level) * EARTH_RADIUS_METERS;
        };
        SphereSearch {
            centre,
            full,
            current: (-1.0, full.inner),
            increment: 3.0 * estimate,
            coarsest,
            scanned: Vec::new(),
        }
    }

    /// `buildS2Region`: the ring between two caps.
    fn region(&self, inner: f64, outer: f64) -> Region {
        let deg = PI / 180.0;
        let point = point_from_radians(self.centre.1 * deg, self.centre.0 * deg);
        let mut regions = Vec::new();
        if inner > 0.0 {
            regions.push(Region::Cap(
                Cap::from_axis_angle(point, inner / EARTH_RADIUS_METERS).complement(),
            ));
        }
        let epsilon = 1e-15;
        if outer * (1.0 + epsilon) < MAX_EARTH_DISTANCE {
            let angle = (outer * (1.0 + epsilon)) / EARTH_RADIUS_METERS;
            let mut cap = Cap::from_axis_angle(point, angle);
            if cap.height < epsilon {
                cap = Cap::from_axis_height(point, epsilon);
            }
            regions.push(Region::Cap(cap));
        }
        if regions.is_empty() {
            regions.push(Region::Cap(Cap::full()));
        }
        Region::Intersection(regions)
    }

    fn next(&mut self, last_returned: Option<usize>) -> Option<Ring> {
        if self.current.0 >= 0.0 && self.current.1 == self.full.outer {
            return None;
        }
        adjust(&mut self.increment, last_returned);
        let next = (
            self.current.1,
            (self.current.1 + self.increment).min(self.full.outer),
        );
        let is_last = next.1 == self.full.outer;
        self.current = next;
        let region = self.region(next.0, next.1);
        let cover = normalize(super::query_covering(&region));
        let cover: Vec<CellId> = cell_union_difference(&cover, &self.scanned)
            .into_iter()
            .filter(|c| region.may_intersect(&Cell::new(*c)))
            .collect();
        let mut all = std::mem::take(&mut self.scanned);
        all.extend(cover.iter().copied());
        self.scanned = normalize(all);
        Some(Ring {
            geo: FieldBounds::Cells(sphere_intervals(&cover, self.coarsest)),
            min: next.0,
            max: next.1,
            inclusive_max: is_last,
        })
    }
}

struct FlatSearch {
    conv: geohash2d::Converter,
    full: Annulus,
    spherical: bool,
    current: (f64, f64),
    increment: f64,
    scanned: Vec<geohash2d::GeoHash>,
}

impl FlatSearch {
    fn new(
        conv: geohash2d::Converter,
        centre: (f64, f64),
        full: Annulus,
        spherical: bool,
        any_entry: &dyn Fn(&FieldBounds) -> bool,
    ) -> Option<FlatSearch> {
        // A centre outside the index's range is mongod's 16433, which the
        // caller reports.
        let centre_cell = conv.hash(centre.0, centre.1)?;
        let mut level = conv.bits.saturating_sub(1);
        let estimate = loop {
            let ranges = centre_cell
                .vertex_neighbors(level)
                .iter()
                .map(|c| (c.hash_min(), c.hash_max()))
                .collect();
            if any_entry(&FieldBounds::Hashes(ranges)) {
                break conv.size_edge(level);
            }
            if full.outer < 0.5 * conv.size_edge(level) {
                break 0.5 * conv.size_edge(level);
            }
            if level > 0 {
                level -= 1;
                continue;
            }
            break conv.size_edge(level);
        };
        let increment = if spherical {
            ((estimate * EARTH_RADIUS_METERS * 3.0) * (PI / 180.0)).min(MAX_EARTH_DISTANCE / 1000.0)
        } else {
            3.0 * estimate
        };
        Some(FlatSearch {
            conv,
            full,
            spherical,
            current: (-1.0, full.inner),
            increment,
            scanned: Vec::new(),
        })
    }

    fn next(&mut self, last_returned: Option<usize>) -> Option<Ring> {
        if self.current.0 >= 0.0 && self.current.1 == self.full.outer {
            return None;
        }
        adjust(&mut self.increment, last_returned);
        let min_increment = if self.spherical {
            self.conv.error() / 2.0 * METERS_PER_DEGREE_AT_EQUATOR
        } else {
            self.conv.error() / 2.0
        };
        self.increment = self.increment.max(min_increment);
        let mut next = (
            self.current.1,
            (self.current.1 + self.increment).min(self.full.outer),
        );
        let is_last = next.1 == self.full.outer;
        self.current = next;
        let centre = self.full.centre;
        let region = if self.spherical {
            // `projectBoundsToTwoDDegrees`.
            let rad2deg = |r: f64| r * (180.0 / PI);
            let outer = rad2deg(next.1 / EARTH_RADIUS_METERS);
            let inner = rad2deg(next.0 / EARTH_RADIUS_METERS);
            let error = x_scan_distance(centre.1, outer);
            R2Region::Annulus(centre, (inner - error).max(0.0), outer + error)
        } else {
            let epsilon = f64::EPSILON * (centre.0.abs().max(centre.1.abs()) + self.full.outer);
            if next.0 > 0.0 && next.0 == self.full.inner {
                next.0 = (next.0 - epsilon).max(0.0);
            }
            if next.1 > 0.0 && next.1 == self.full.outer {
                next.1 += epsilon;
            }
            R2Region::Annulus(centre, (next.0 - epsilon).max(0.0), next.1 + epsilon)
        };
        let covering = self.conv.covering(&region, 16);
        let covering = geohash2d::difference(&geohash2d::normalize(covering), &self.scanned);
        let mut all = std::mem::take(&mut self.scanned);
        all.extend(covering.iter().copied());
        self.scanned = geohash2d::normalize(all);
        let mut cells = covering;
        cells.sort();
        cells.dedup();
        Some(Ring {
            geo: FieldBounds::Hashes(cells.iter().map(|c| (c.hash_min(), c.hash_max())).collect()),
            min: next.0,
            max: next.1,
            inclusive_max: is_last,
        })
    }
}

/// `computeXScanDistance`.
fn x_scan_distance(y: f64, max_dist_degrees: f64) -> f64 {
    let deg2rad = |d: f64| d * (PI / 180.0);
    max_dist_degrees
        / deg2rad((y + max_dist_degrees).min(89.0))
            .cos()
            .min(deg2rad((y - max_dist_degrees).max(-89.0)).cos())
}
