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
struct Found<P> {
    distance: f64,
    rid: i64,
    payload: P,
}

impl<P> HeapOrder for Found<P> {
    fn heap_less(&self, other: &Self) -> bool {
        self.distance > other.distance
    }
}

/// `R2Annulus`.
#[derive(Clone, Copy, Debug)]
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

/// A document as the search sees it: its RecordId, its index keys, whether
/// the ring scans' fetch lets it through, its distance and what the caller
/// keeps of it (the document, as the queue buffers it).
pub struct Candidate<P> {
    pub rid: i64,
    keys: Vec<Key>,
    /// The near stage's own filter and, in the plane, the whole-search
    /// annulus: what the ring scans' fetch tests.
    fetched: bool,
    /// `computeGeoNearDistance`; -1 when it holds nothing to measure.
    distance: f64,
    pub payload: P,
}

/// The collection as the search reads it, each time it reads it: the
/// density estimate's scan and each ring's.
pub trait Source<P> {
    type Error;
    /// Every document of the collection as it is now, in RecordId order.
    fn candidates(&mut self) -> Result<&[Candidate<P>], Self::Error>;
}

/// How a search measures a document: the index, the query, and the
/// annulus the whole search covers (`geoNearDistanceBounds` /
/// `twoDDistanceBounds`).
#[derive(Clone, Debug)]
pub struct Measure {
    index: GeoIndex,
    near: Near,
    full: Annulus,
    flat: bool,
}

impl Measure {
    /// `None` when the query is one this does not replay, or a `2d`
    /// index's range does not hold the centre (mongod's 16433, which the
    /// caller reports).
    pub fn new(index: &GeoIndex, near: &Near) -> Option<Measure> {
        let min = near.min.unwrap_or(0.0);
        let max = near.max.unwrap_or(f64::MAX);
        let (centre, spherical) = (near.centre, near.spherical);
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
        if let Kind::Flat(conv) = &index.kind {
            conv.hash(centre.0, centre.1)?;
        }
        Some(Measure {
            index: index.clone(),
            near: near.clone(),
            full,
            flat: matches!(index.kind, Kind::Flat(_)) && !spherical,
        })
    }

    /// `doc` (RecordId `rid`) as the search sees it; `passes`: the near
    /// stage's own filter lets it through.
    pub fn candidate<P>(&self, rid: i64, doc: &Document, passes: bool, payload: P) -> Candidate<P> {
        let value = crate::get_path(doc, self.index.geo_field());
        let in_annulus = !self.flat
            || value.is_some_and(|v| {
                super::flat_points(v)
                    .into_iter()
                    .any(|p| self.full.contains(p))
            });
        Candidate {
            rid,
            keys: self.index.keys(doc),
            fetched: passes && in_annulus,
            distance: value
                .and_then(|v| distance(&self.near, self.flat, v))
                .unwrap_or(-1.0),
            payload,
        }
    }
}

/// One ring: the covering's key intervals and its distance bounds.
struct Ring {
    geo: FieldBounds,
    min: f64,
    max: f64,
    inclusive_max: bool,
}

/// mongod's `NearStage` as it runs: the search's rings, its queue and its
/// increment, kept between calls, so a cursor reads it ring by ring as
/// `getMore` asks. Each ring scans the collection as it is when the search
/// reaches it; the queue holds what earlier rings buffered, as they
/// buffered it. Nearest first, then; but documents at one distance come out
/// in whatever order the heap's sift steps leave them, which depends on what
/// was pushed, in what order, and what was still in the queue.
pub struct NearSearch<P> {
    measure: Measure,
    /// The bounds of the index's other fields (the geo field's entry is
    /// ignored).
    base: Vec<FieldBounds>,
    /// `None` until the first call runs the density estimate.
    search: Option<Search>,
    heap: Heap<Found<P>>,
    seen: HashSet<i64>,
    /// The ring being handed out, and how many it has returned.
    ring: Option<Ring>,
    returned: usize,
    last_returned: Option<usize>,
    done: bool,
    /// Index entries the ring scans read, and documents they fetched.
    pub keys_examined: i64,
    pub docs_examined: i64,
}

impl<P: Clone> NearSearch<P> {
    pub fn new(measure: Measure, base: Vec<FieldBounds>) -> NearSearch<P> {
        NearSearch {
            measure,
            base,
            search: None,
            heap: Heap::new(),
            seen: HashSet::new(),
            ring: None,
            returned: 0,
            last_returned: None,
            done: false,
            keys_examined: 0,
            docs_examined: 0,
        }
    }

    pub fn measure(&self) -> &Measure {
        &self.measure
    }

    /// The search has handed out its last document.
    pub fn is_eof(&self) -> bool {
        self.done
    }

    fn bounded(&self, key: &Key, geo: &FieldBounds, others: bool) -> bool {
        let geo_at = self.measure.index.geo_at;
        key.iter().enumerate().all(|(at, part)| {
            if at == geo_at {
                geo.admits(part)
            } else {
                !others || self.base.get(at).is_none_or(|b| b.admits(part))
            }
        })
    }

    /// The next document, with its RecordId: what the queue holds inside
    /// the ring being handed out, else the next ring's, read from `source`
    /// as it is now.
    pub fn next<S: Source<P>>(&mut self, source: &mut S) -> Result<Option<(i64, P)>, S::Error> {
        loop {
            if self.done {
                return Ok(None);
            }
            if self.search.is_none() {
                // The density estimator's index scan, which fetches nothing.
                // A `2d` index's estimate bounds only the geo field.
                let docs = source.candidates()?;
                let sphere = self.measure.index.is_sphere();
                let any_entry = |geo: &FieldBounds| {
                    docs.iter()
                        .any(|d| d.keys.iter().any(|k| self.bounded(k, geo, sphere)))
                };
                let (centre, full) = (self.measure.near.centre, self.measure.full);
                let search = match &self.measure.index.kind {
                    Kind::Sphere { coarsest, .. } => {
                        Search::Sphere(SphereSearch::new(centre, full, *coarsest, &any_entry))
                    }
                    Kind::Flat(conv) => Search::Flat(
                        FlatSearch::new(
                            *conv,
                            centre,
                            full,
                            self.measure.near.spherical,
                            &any_entry,
                        )
                        .expect("a centre Measure::new hashed"),
                    ),
                };
                self.search = Some(search);
            }
            // Advancing: hand out what is inside the ring.
            if let Some(ring) = &self.ring {
                while let Some(top) = self.heap.top() {
                    if top.distance < ring.min {
                        let gone = self.heap.pop().expect("a top to pop");
                        self.seen.remove(&gone.rid);
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
                    let found = self.heap.pop().expect("a top to pop");
                    self.seen.remove(&found.rid);
                    self.returned += 1;
                    return Ok(Some((found.rid, found.payload)));
                }
                self.last_returned = Some(self.returned);
                self.ring = None;
            }
            let next = self
                .search
                .as_mut()
                .expect("a search")
                .next(self.last_returned);
            let Some(ring) = next else {
                self.done = true;
                return Ok(None);
            };
            // Buffering: the ring's index scan, its fetch, and the queue.
            let docs = source.candidates()?;
            let mut entries: Vec<(&Key, usize)> = Vec::new();
            for (i, d) in docs.iter().enumerate() {
                for k in &d.keys {
                    if self.bounded(k, &ring.geo, true) {
                        entries.push((k, i));
                    }
                }
            }
            entries.sort_by(|a, b| a.0.cmp(b.0).then(docs[a.1].rid.cmp(&docs[b.1].rid)));
            self.keys_examined += entries.len() as i64;
            let mut scanned: HashSet<usize> = HashSet::new();
            for (_, i) in entries {
                if !scanned.insert(i) {
                    continue;
                }
                let d = &docs[i];
                self.docs_examined += 1;
                if !d.fetched || self.seen.contains(&d.rid) {
                    continue;
                }
                self.heap.push(Found {
                    distance: d.distance,
                    rid: d.rid,
                    payload: d.payload.clone(),
                });
                self.seen.insert(d.rid);
            }
            self.ring = Some(ring);
            self.returned = 0;
        }
    }
}

/// A collection that does not change while it is read.
struct Fixed<P>(Vec<Candidate<P>>);

impl<P> Source<P> for Fixed<P> {
    type Error = std::convert::Infallible;
    fn candidates(&mut self) -> Result<&[Candidate<P>], Self::Error> {
        Ok(&self.0)
    }
}

/// The order mongod returns the documents of a `$near` in, read at once
/// from a collection that does not change, as indexes into `docs` (the
/// collection, in insertion order). `passes[i]` is whether the near stage's
/// own filter lets document `i` through; `base` holds the bounds of the
/// index's other fields (the geo field's entry is ignored). The rest of a
/// query is a filter mongod runs over what the stage returns, so it does
/// not change the order. `None` when the query is one this does not replay.
pub fn near_order(
    index: &GeoIndex,
    base: &[FieldBounds],
    near: &Near,
    docs: &[Document],
    passes: &[bool],
) -> Option<Vec<usize>> {
    let measure = Measure::new(index, near)?;
    let mut source = Fixed(
        docs.iter()
            .zip(passes)
            .enumerate()
            .map(|(i, (doc, &passes))| measure.candidate(i as i64, doc, passes, ()))
            .collect(),
    );
    let mut search = NearSearch::new(measure, base.to_vec());
    let mut out = Vec::new();
    while let Some((i, ())) = search.next(&mut source).unwrap_or_else(|e| match e {}) {
        out.push(i as usize);
    }
    Some(out)
}

/// `distance()` in the plane (`shapes.cpp`).
pub(crate) fn flat_distance(p: (f64, f64), q: (f64, f64)) -> f64 {
    let a = p.0 - q.0;
    let b = p.1 - q.1;
    if a == 0.0 {
        return b.abs();
    }
    if b == 0.0 {
        return a.abs();
    }
    // Rounded after each step, as the Linux build computes it (the macOS
    // build fuses the add, RM-231).
    (a * a + b * b).sqrt()
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
