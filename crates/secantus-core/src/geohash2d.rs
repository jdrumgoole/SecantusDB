//! What a `2d` index holds and how mongod 8.2 scans it.
//!
//! A transcription of mongod's `GeoHash` / `GeoHashConverter`
//! (`src/mongo/db/geo/hash.cpp`), the `R2RegionCoverer` and `R2CellUnion`
//! (`r2_region_coverer.cpp`), and the flat shapes their coverings test
//! (`shapes.cpp`). An index key is a point's geohash at the index's `bits`:
//! x and y interleaved, x the higher bit of each pair, stored as eight
//! big-endian bytes, so the index is in the order of the hash as an unsigned
//! integer.

use crate::heap::Heap;

/// `GeoHash`: `bits` per axis, in the high `2 * bits` bits of `hash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GeoHash {
    pub hash: u64,
    pub bits: u32,
}

impl PartialOrd for GeoHash {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GeoHash {
    /// `GeoHash::operator<`: the hash, then the bits.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.hash.cmp(&other.hash).then(self.bits.cmp(&other.bits))
    }
}

fn interleave_with_zeros(input: u32) -> u64 {
    let mut word = u64::from(input);
    word = (word ^ (word << 16)) & 0x0000_ffff_0000_ffff;
    word = (word ^ (word << 8)) & 0x00ff_00ff_00ff_00ff;
    word = (word ^ (word << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
    word = (word ^ (word << 2)) & 0x3333_3333_3333_3333;
    word = (word ^ (word << 1)) & 0x5555_5555_5555_5555;
    word
}

fn deinterleave_zeros(input: u64) -> u32 {
    let mut word = input & 0x5555_5555_5555_5555;
    word = (word ^ (word >> 1)) & 0x3333_3333_3333_3333;
    word = (word ^ (word >> 2)) & 0x0f0f_0f0f_0f0f_0f0f;
    word = (word ^ (word >> 4)) & 0x00ff_00ff_00ff_00ff;
    word = (word ^ (word >> 8)) & 0x0000_ffff_0000_ffff;
    word = (word ^ (word >> 16)) & 0x0000_0000_ffff_ffff;
    word as u32
}

/// `allX[bits]` / `allY[bits]`: the x (or y) bits of the first `bits` pairs.
fn all_x(bits: u32) -> u64 {
    (0..bits).fold(0, |acc, i| acc | (1u64 << (63 - 2 * i)))
}

fn all_y(bits: u32) -> u64 {
    (0..bits).fold(0, |acc, i| acc | (1u64 << (62 - 2 * i)))
}

impl GeoHash {
    pub fn root() -> GeoHash {
        GeoHash { hash: 0, bits: 0 }
    }

    /// `GeoHash(x, y, bits)`.
    pub fn from_xy(x: u32, y: u32, bits: u32) -> GeoHash {
        let full = (interleave_with_zeros(x) << 1) | interleave_with_zeros(y);
        let mask = if bits == 0 {
            0
        } else {
            u64::MAX << (64 - bits * 2)
        };
        GeoHash {
            hash: full & mask,
            bits,
        }
    }

    /// `GeoHash(hash, bits)`: the unused low bits cleared.
    pub fn new(hash: u64, bits: u32) -> GeoHash {
        let hash = if bits == 0 {
            0
        } else if bits == 32 {
            hash
        } else {
            hash & !((1u64 << (64 - bits * 2)) - 1)
        };
        GeoHash { hash, bits }
    }

    pub fn unhash(self) -> (u32, u32) {
        (
            deinterleave_zeros(self.hash >> 1),
            deinterleave_zeros(self.hash),
        )
    }

    fn has_prefix(self, other: GeoHash) -> bool {
        if other.bits == 0 {
            return true;
        }
        let x = (other.hash ^ self.hash) as i64;
        (x >> (64 - other.bits * 2)) == 0
    }

    pub fn contains(self, other: GeoHash) -> bool {
        self.bits <= other.bits && other.has_prefix(self)
    }

    pub fn parent(self) -> GeoHash {
        GeoHash::new(self.hash, self.bits - 1)
    }

    pub fn parent_at(self, level: u32) -> GeoHash {
        GeoHash::new(self.hash, level)
    }

    pub fn subdivide(self) -> Option<[GeoHash; 4]> {
        if self.bits == 32 {
            return None;
        }
        let c0 = GeoHash::new(self.hash, self.bits + 1);
        let c1 = GeoHash::new(
            c0.hash | (1u64 << (63 - (self.bits * 2 + 1))),
            self.bits + 1,
        );
        let c2 = GeoHash::new(c0.hash | (1u64 << (63 - self.bits * 2)), self.bits + 1);
        let c3 = GeoHash::new(c1.hash | c2.hash, self.bits + 1);
        Some([c0, c1, c2, c3])
    }

    /// `appendHashMin`: the least key in the cell.
    pub fn hash_min(self) -> u64 {
        self.hash
    }

    /// `appendHashMax`: the greatest key in the cell.
    pub fn hash_max(self) -> u64 {
        self.hash | !(all_x(self.bits) | all_y(self.bits))
    }

    fn at_min_x(self) -> bool {
        self.hash & all_x(self.bits) == 0
    }
    fn at_min_y(self) -> bool {
        self.hash & all_y(self.bits) == 0
    }
    fn at_max_x(self) -> bool {
        self.hash & all_x(self.bits) == all_x(self.bits)
    }
    fn at_max_y(self) -> bool {
        self.hash & all_y(self.bits) == all_y(self.bits)
    }

    /// `appendVertexNeighbors`.
    pub fn vertex_neighbors(self, level: u32) -> Vec<GeoHash> {
        let parent = self.parent_at(level);
        let mut out = vec![parent];
        let (px, py) = parent.unhash();
        let parent_bits = parent.bits;
        if parent_bits == 0 {
            return out;
        }
        let pos = (self.hash >> (64 - 2 * (parent_bits + 1))) & 3;
        let m = 1u32.wrapping_shl(32 - parent_bits);
        let at = |x: u32, y: u32| GeoHash::from_xy(x, y, parent_bits);
        if pos & 2 == 0 {
            if !parent.at_min_x() {
                out.push(at(px.wrapping_sub(m), py));
            }
        } else if !parent.at_max_x() {
            out.push(at(px.wrapping_add(m), py));
        }
        if pos & 1 == 0 {
            if !parent.at_min_y() {
                out.push(at(px, py.wrapping_sub(m)));
            }
        } else if !parent.at_max_y() {
            out.push(at(px, py.wrapping_add(m)));
        }
        match pos {
            0 => {
                if !parent.at_min_x() && !parent.at_min_y() {
                    out.push(at(px.wrapping_sub(m), py.wrapping_sub(m)));
                }
            }
            1 => {
                if !parent.at_min_x() && !parent.at_max_y() {
                    out.push(at(px.wrapping_sub(m), py.wrapping_add(m)));
                }
            }
            2 => {
                if !parent.at_max_x() && !parent.at_min_y() {
                    out.push(at(px.wrapping_add(m), py.wrapping_sub(m)));
                }
            }
            _ => {
                if !parent.at_max_x() && !parent.at_max_y() {
                    out.push(at(px.wrapping_add(m), py.wrapping_add(m)));
                }
            }
        }
        out
    }
}

/// `Box`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Box2 {
    pub min: (f64, f64),
    pub max: (f64, f64),
}

fn between(min: f64, max: f64, val: f64, fudge: f64) -> bool {
    val + fudge >= min && val <= max + fudge
}

impl Box2 {
    pub fn new(a: (f64, f64), b: (f64, f64)) -> Box2 {
        Box2 {
            min: (a.0.min(b.0), a.1.min(b.1)),
            max: (a.0.max(b.0), a.1.max(b.1)),
        }
    }
    pub fn intersects(&self, o: &Box2) -> bool {
        let x = between(self.min.0, self.max.0, o.min.0, 0.0)
            || between(self.min.0, self.max.0, o.max.0, 0.0)
            || between(o.min.0, o.max.0, self.min.0, 0.0);
        let y = between(self.min.1, self.max.1, o.min.1, 0.0)
            || between(self.min.1, self.max.1, o.max.1, 0.0)
            || between(o.min.1, o.max.1, self.min.1, 0.0);
        x && y
    }
    pub fn inside(&self, p: (f64, f64)) -> bool {
        between(self.min.0, self.max.0, p.0, 0.0) && between(self.min.1, self.max.1, p.1, 0.0)
    }
    pub fn contains(&self, o: &Box2) -> bool {
        self.inside(o.min) && self.inside(o.max)
    }
    fn center(&self) -> (f64, f64) {
        (
            (self.min.0 + self.max.0) / 2.0,
            (self.min.1 + self.max.1) / 2.0,
        )
    }
}

/// `distanceCompare`.
pub fn distance_compare(p1: (f64, f64), p2: (f64, f64), radius: f64) -> f64 {
    let a = p2.0 - p1.0;
    let b = p2.1 - p1.1;
    if a == 0.0 {
        return if p2.1 > p1.1 {
            p2.1 - (p1.1 + radius)
        } else {
            p1.1 - (p2.1 + radius)
        };
    }
    if b == 0.0 {
        return if p2.0 > p1.0 {
            p2.0 - (p1.0 + radius)
        } else {
            p1.0 - (p2.0 + radius)
        };
    }
    // `sqrt((a * a) + (b * b))`, fused as clang fuses it.
    a.mul_add(a, b * b).sqrt() - radius
}

fn circle_contains_box(center: (f64, f64), r: f64, b: &Box2, boundary: bool) -> bool {
    let corners = [b.min, b.max, (b.min.0, b.max.1), (b.max.0, b.min.1)];
    corners.iter().all(|c| {
        let d = distance_compare(center, *c, r);
        if boundary {
            d <= 0.0
        } else {
            d < 0.0
        }
    })
}

fn circle_intersects_box(center: (f64, f64), r: f64, b: &Box2, boundary: bool) -> bool {
    if r == 0.0 && !boundary {
        return false;
    }
    let bc = b.center();
    let dx = (center.0 - bc.0).abs();
    let dy = (center.1 - bc.1).abs();
    let w = (b.max.0 - b.min.0) / 2.0;
    let h = (b.max.1 - b.min.1) / 2.0;
    if boundary {
        if (dx <= w + r && dy <= h) || (dx <= w && dy <= h + r) {
            return true;
        }
    } else if (dx < w + r && dy < h) || (dx < w && dy < h + r) {
        return true;
    }
    let c = distance_compare((dx, dy), (w, h), r);
    c < 0.0 || (c == 0.0 && boundary)
}

/// `Polygon::contains(p, 0) > 0`: mongod's flat point-in-polygon.
pub fn polygon_contains(points: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = points.len();
    let mut counter = 0;
    let mut p1 = points[0];
    for i in 1..=n {
        let p2 = points[i % n];
        if (p.1 == p1.1 && p.0 == p1.0) || (p.1 == p2.1 && p.0 == p2.0) {
            return true;
        }
        if p1.1 == p2.1 && p.1 == p1.1 && p.0 >= p1.0.min(p2.0) && p.0 <= p1.0.max(p2.0) {
            return true;
        }
        if p.1 > p1.1.min(p2.1) && p.1 <= p1.1.max(p2.1) && p.0 <= p1.0.max(p2.0) && p1.1 != p2.1 {
            let xinters = (p.1 - p1.1) * (p2.0 - p1.0) / (p2.1 - p1.1) + p1.0;
            if p1.0 == p2.0 && p.0 == p1.0 {
                return true;
            } else if p1.0 == p2.0 || p.0 <= xinters {
                counter += 1;
            }
        }
        p1 = p2;
    }
    counter % 2 == 1
}

fn lines_intersect(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> bool {
    let sub = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0, p.1 - q.1);
    let dot = |p: (f64, f64), q: (f64, f64)| p.0 * q.0 + p.1 * q.1;
    // Vector2::Ortho: (-y, x).
    let ortho = |p: (f64, f64)| (-p.1, p.0);
    let normal_ab = ortho(sub(b, a));
    let ac = dot(normal_ab, sub(c, a));
    let ad = dot(normal_ab, sub(d, a));
    if ac * ad > 0.0 {
        return false;
    }
    if ac == 0.0 && ad == 0.0 {
        return dot(sub(c, a), sub(c, b)) <= 0.0 || dot(sub(d, a), sub(d, b)) <= 0.0;
    }
    let normal_cd = ortho(sub(d, c));
    let ca = dot(normal_cd, sub(a, c));
    let cb = dot(normal_cd, sub(b, c));
    ca * cb <= 0.0
}

fn line_intersects_box(a: (f64, f64), b: (f64, f64), bx: &Box2) -> bool {
    let upper_left = (bx.min.0, bx.max.1);
    let lower_right = (bx.max.0, bx.min.1);
    lines_intersect(a, b, upper_left, bx.min)
        || lines_intersect(a, b, bx.min, lower_right)
        || lines_intersect(a, b, lower_right, bx.max)
        || lines_intersect(a, b, bx.max, upper_left)
}

fn polygon_contains_box(points: &[(f64, f64)], b: &Box2) -> bool {
    if !polygon_contains(points, b.min)
        || !polygon_contains(points, b.max)
        || !polygon_contains(points, (b.min.0, b.max.1))
        || !polygon_contains(points, (b.max.0, b.min.1))
    {
        return false;
    }
    let n = points.len();
    for i in 0..n - 1 {
        if line_intersects_box(points[i], points[i + 1], b) {
            return false;
        }
    }
    !line_intersects_box(points[n - 1], points[0], b)
}

/// The flat regions a `2d` covering is asked for.
#[derive(Clone, Debug)]
pub enum R2Region {
    /// `$box`: its own bounds.
    Box(Box2),
    /// `$center`: a circle.
    Circle((f64, f64), f64),
    /// `$polygon`: the vertices, and their bounds.
    Polygon(Vec<(f64, f64)>, Box2),
    /// Any other shape: only its bounds are tested (`R2BoxRegion`).
    Bounds(Box2),
    /// `R2Annulus`: a `$near` interval.
    Annulus((f64, f64), f64, f64),
}

impl R2Region {
    fn bounds(&self) -> Box2 {
        match self {
            R2Region::Box(b) | R2Region::Polygon(_, b) | R2Region::Bounds(b) => *b,
            R2Region::Circle(c, r) => Box2::new((c.0 - r, c.1 - r), (c.0 + r, c.1 + r)),
            R2Region::Annulus(c, _, outer) => Box2 {
                min: (c.0 - outer, c.1 - outer),
                max: (c.0 - outer + 2.0 * outer, c.1 - outer + 2.0 * outer),
            },
        }
    }

    fn fast_contains(&self, other: &Box2) -> bool {
        match self {
            R2Region::Box(b) => b.contains(other),
            R2Region::Circle(c, r) => circle_contains_box(*c, *r, other, true),
            R2Region::Polygon(points, _) => polygon_contains_box(points, other),
            R2Region::Bounds(_) => false,
            R2Region::Annulus(c, inner, outer) => {
                circle_contains_box(*c, *outer, other, true)
                    && !circle_intersects_box(*c, *inner, other, false)
            }
        }
    }

    fn fast_disjoint(&self, other: &Box2) -> bool {
        match self {
            R2Region::Annulus(c, inner, outer) => {
                !circle_intersects_box(*c, *outer, other, true)
                    || circle_contains_box(*c, *inner, other, false)
            }
            _ => !self.bounds().intersects(other),
        }
    }
}

/// `GeoHashConverter`.
#[derive(Clone, Copy, Debug)]
pub struct Converter {
    pub bits: u32,
    pub min: f64,
    pub max: f64,
    scaling: f64,
    error: f64,
    error_unhash_to_box: f64,
}

const NUM_BUCKETS: f64 = 4.0 * 1024.0 * 1024.0 * 1024.0;

impl Converter {
    pub fn new(bits: u32, min: f64, max: f64) -> Converter {
        let scaling = NUM_BUCKETS / (max - min);
        let mut c = Converter {
            bits,
            min,
            max,
            scaling,
            error: 0.0,
            error_unhash_to_box: min.abs().max(max.abs()) * (0.5 * f64::EPSILON) * 8.0,
        };
        let a = GeoHash::from_xy(0, 0, bits);
        let b = c.moved(a);
        let (ax, ay) = c.unhash(a);
        let (bx, by) = c.unhash(b);
        let (dx, dy) = (bx - ax, by - ay);
        c.error = dx.mul_add(dx, dy * dy).sqrt() + 0.001 / scaling;
        c
    }

    /// `a.move(1, 1)` at the converter's bits.
    fn moved(&self, a: GeoHash) -> GeoHash {
        let (x, y) = a.unhash();
        let step = 1u32.wrapping_shl(32 - a.bits);
        GeoHash::from_xy(x.wrapping_add(step), y.wrapping_add(step), a.bits)
    }

    pub fn error(&self) -> f64 {
        self.error
    }

    fn hash_scale(&self, v: f64) -> u32 {
        let ret = ((v - self.min) * self.scaling).min(NUM_BUCKETS - 1.0);
        ret as u32
    }

    fn unhash_scale(&self, v: u32) -> f64 {
        f64::from(v) / self.scaling + self.min
    }

    /// `hash(x, y)`; `None` outside the range (mongod's 16433 / 13027).
    pub fn hash(&self, x: f64, y: f64) -> Option<GeoHash> {
        let inside = |v: f64| v <= self.max && v >= self.min;
        if !(inside(x) && inside(y)) {
            return None;
        }
        Some(GeoHash::from_xy(
            self.hash_scale(x),
            self.hash_scale(y),
            self.bits,
        ))
    }

    pub fn unhash(&self, h: GeoHash) -> (f64, f64) {
        let (a, b) = h.unhash();
        (self.unhash_scale(a), self.unhash_scale(b))
    }

    pub fn size_edge(&self, level: u32) -> f64 {
        (self.max - self.min) * 2f64.powi(-(level as i32))
    }

    fn unhash_to_box_covering(&self, h: GeoHash) -> Box2 {
        if h.bits == 0 {
            return Box2 {
                min: (self.min, self.min),
                max: (self.max, self.max),
            };
        }
        let size = self.size_edge(h.bits);
        let min = self.unhash(h);
        let e = self.error_unhash_to_box;
        Box2 {
            min: (min.0 - e, min.1 - e),
            max: (min.0 + size + e, min.1 + size + e),
        }
    }

    /// `R2RegionCoverer::getCovering` with `max_level` the index's bits.
    pub fn covering(&self, region: &R2Region, max_cells: i32) -> Vec<GeoHash> {
        let mut run = R2Run {
            conv: self,
            region,
            max_level: self.bits,
            max_cells,
            results: Vec::new(),
            queue: Heap::new(),
        };
        let root = run.new_candidate(GeoHash::root());
        run.add_candidate(root);
        while let Some((_, mut candidate)) = run.queue.pop() {
            let n = candidate.children.len() as i32;
            if n == 1 || run.results.len() as i32 + run.queue.len() as i32 + n <= run.max_cells {
                for child in std::mem::take(&mut candidate.children) {
                    run.add_candidate(Some(child));
                }
            } else {
                candidate.is_terminal = true;
                candidate.children.clear();
                run.add_candidate(Some(candidate));
            }
        }
        run.results
    }
}

struct R2Candidate {
    cell: GeoHash,
    is_terminal: bool,
    children: Vec<R2Candidate>,
}

struct R2Run<'a> {
    conv: &'a Converter,
    region: &'a R2Region,
    max_level: u32,
    max_cells: i32,
    results: Vec<GeoHash>,
    queue: Heap<(i32, R2Candidate)>,
}

impl R2Run<'_> {
    fn new_candidate(&self, cell: GeoHash) -> Option<R2Candidate> {
        let b = self.conv.unhash_to_box_covering(cell);
        if self.region.fast_disjoint(&b) {
            return None;
        }
        Some(R2Candidate {
            cell,
            is_terminal: cell.bits >= self.max_level || self.region.fast_contains(&b),
            children: Vec::new(),
        })
    }

    fn add_candidate(&mut self, candidate: Option<R2Candidate>) {
        let Some(mut candidate) = candidate else {
            return;
        };
        if candidate.is_terminal {
            self.results.push(candidate.cell);
            return;
        }
        let mut num_terminals = 0;
        for child in candidate
            .cell
            .subdivide()
            .expect("a 2d coverer stops at 32 bits")
        {
            if let Some(c) = self.new_candidate(child) {
                if c.is_terminal {
                    num_terminals += 1;
                }
                candidate.children.push(c);
            }
        }
        if candidate.children.is_empty() {
        } else if num_terminals == 4 {
            candidate.is_terminal = true;
            candidate.children.clear();
            self.add_candidate(Some(candidate));
        } else {
            let n = candidate.children.len() as i32;
            let priority = -((((candidate.cell.bits as i32) << 4) + n) << 4) - num_terminals;
            self.queue.push((priority, candidate));
        }
    }
}

/// `R2CellUnion::normalize`.
pub fn normalize(mut cells: Vec<GeoHash>) -> Vec<GeoHash> {
    cells.sort();
    let mut output: Vec<GeoHash> = Vec::with_capacity(cells.len());
    for mut id in cells {
        if output.last().is_some_and(|last| last.contains(id)) {
            continue;
        }
        while output.len() >= 3 {
            let n = output.len();
            if (output[n - 3].hash ^ output[n - 2].hash ^ output[n - 1].hash) != id.hash {
                break;
            }
            let parent = id.parent();
            if parent != output[n - 3].parent()
                || parent != output[n - 2].parent()
                || parent != output[n - 1].parent()
            {
                break;
            }
            output.truncate(n - 3);
            id = parent;
        }
        output.push(id);
    }
    output
}

fn union_contains(cells: &[GeoHash], id: GeoHash) -> bool {
    let i = cells.partition_point(|c| *c <= id);
    i > 0 && cells[i - 1].contains(id)
}

fn union_intersects(cells: &[GeoHash], id: GeoHash) -> bool {
    let i = cells.partition_point(|c| *c < id);
    if i < cells.len() && id.contains(cells[i]) {
        return true;
    }
    i > 0 && cells[i - 1].contains(id)
}

fn difference_internal(id: GeoHash, union: &[GeoHash], out: &mut Vec<GeoHash>) {
    if !union_intersects(union, id) {
        out.push(id);
    } else if !union_contains(union, id) {
        if let Some(children) = id.subdivide() {
            for c in children {
                difference_internal(c, union, out);
            }
        }
    }
}

/// `R2CellUnion::getDifference`: the cells of `cells` (normalised) not in
/// `union` (normalised).
pub fn difference(cells: &[GeoHash], union: &[GeoHash]) -> Vec<GeoHash> {
    let mut out = Vec::new();
    for &c in cells {
        difference_internal(c, union, &mut out);
    }
    out
}
