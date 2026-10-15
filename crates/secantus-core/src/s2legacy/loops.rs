//! `S2Loop` and `S2Polygon`, as far as a covering needs them: containment
//! and intersection tests against a cell, and building a polygon from its
//! loops.

use super::cap::Cap;
use super::cell::Cell;
use super::edge::{
    closest_point, distance_to_edge, wedge_contains, wedge_intersects, EdgeCrosser, RectBounder,
};
use super::interval::{LatLngRect, R1Interval, S1Interval};
use super::point::{ordered_ccw, origin, ortho, turn_angle, Point};
use std::f64::consts::{FRAC_PI_2, PI};

#[derive(Clone, Debug)]
pub struct Loop {
    vertices: Vec<Point>,
    origin_inside: bool,
    pub bound: LatLngRect,
    pub depth: i32,
}

/// What `AreBoundariesCrossing` does with two edges that share a vertex.
enum Wedge {
    Contains { doesnt_contain: bool },
    Intersects { intersects: bool },
}

impl Wedge {
    fn process(&mut self, a0: Point, ab1: Point, a2: Point, b0: Point, b2: Point) -> bool {
        match self {
            Wedge::Contains { doesnt_contain } => {
                *doesnt_contain = !wedge_contains(a0, ab1, a2, b0, b2);
                *doesnt_contain
            }
            Wedge::Intersects { intersects } => {
                *intersects = wedge_intersects(a0, ab1, a2, b0, b2);
                *intersects
            }
        }
    }
}

impl Loop {
    /// `S2Loop(vertices)`.
    pub fn new(vertices: Vec<Point>) -> Loop {
        let mut l = Loop {
            vertices,
            origin_inside: false,
            bound: LatLngRect::full(),
            depth: 0,
        };
        l.init_origin();
        l.init_bound();
        l
    }

    /// `S2Loop(cell)`.
    pub fn from_cell(cell: &Cell) -> Loop {
        let mut l = Loop {
            vertices: (0..4).map(|k| cell.vertex(k)).collect(),
            origin_inside: false,
            bound: cell.rect_bound(),
            depth: 0,
        };
        l.init_origin();
        l.init_bound();
        l
    }

    pub fn num_vertices(&self) -> usize {
        self.vertices.len()
    }

    /// `vertex(i)` for `0 <= i < 2n`.
    pub fn vertex(&self, i: usize) -> Point {
        let n = self.vertices.len();
        if i >= n {
            self.vertices[i - n]
        } else {
            self.vertices[i]
        }
    }

    pub fn sign(&self) -> i32 {
        if self.depth & 1 != 0 {
            -1
        } else {
            1
        }
    }

    fn init_origin(&mut self) {
        self.origin_inside = false;
        let v1_inside = ordered_ccw(
            ortho(self.vertex(1)),
            self.vertex(0),
            self.vertex(2),
            self.vertex(1),
        );
        if v1_inside != self.contains_point(self.vertex(1)) {
            self.origin_inside = true;
        }
    }

    fn init_bound(&mut self) {
        let mut bounder = RectBounder::new();
        for i in 0..=self.num_vertices() {
            bounder.add_point(self.vertex(i));
        }
        let mut b = bounder.bound;
        self.bound = LatLngRect::full();
        if self.contains_point(Point::new(0.0, 0.0, 1.0)) {
            b = LatLngRect::new(R1Interval::new(b.lat.lo, FRAC_PI_2), S1Interval::full());
        }
        if b.lng.is_full() && self.contains_point(Point::new(0.0, 0.0, -1.0)) {
            b.lat.lo = -FRAC_PI_2;
        }
        self.bound = b;
    }

    fn canonical_first_vertex(&self) -> (usize, i64) {
        let n = self.num_vertices();
        let mut first = 0;
        for i in 1..n {
            if self.vertex(i).less(self.vertex(first)) {
                first = i;
            }
        }
        if self.vertex(first + 1).less(self.vertex(first + n - 1)) {
            (first, 1)
        } else {
            (first + n, -1)
        }
    }

    fn turning_angle(&self) -> f64 {
        let n = self.num_vertices();
        if n < 3 {
            return 0.0;
        }
        let (start, dir) = self.canonical_first_vertex();
        let at = |i: i64| self.vertex(i as usize);
        let mut i = start as i64;
        let ni = n as i64;
        let mut angle = turn_angle(at((i + ni - dir) % ni), at(i), at((i + dir) % ni));
        let mut left = n;
        while {
            left -= 1;
            left > 0
        } {
            i += dir;
            angle += turn_angle(at(i - dir), at(i), at(i + dir));
        }
        dir as f64 * angle
    }

    fn is_normalized(&self) -> bool {
        if self.bound.lng.length() < PI {
            return true;
        }
        self.turning_angle() >= -1e-14
    }

    /// `Normalize`: a loop larger than a hemisphere is inverted.
    pub fn normalize(&mut self) {
        if !self.is_normalized() {
            self.invert();
        }
    }

    fn invert(&mut self) {
        self.vertices.reverse();
        self.origin_inside ^= true;
        if self.bound.lat.lo > -FRAC_PI_2 && self.bound.lat.hi < FRAC_PI_2 {
            self.bound = LatLngRect::full();
        } else {
            self.init_bound();
        }
    }

    pub fn cap_bound(&self) -> Cap {
        self.bound.cap_bound()
    }

    /// `FindVertex`: the least `i` in `1..=n` with `vertex(i) == p`.
    fn find_vertex(&self, p: Point) -> Option<usize> {
        (1..=self.num_vertices()).find(|&i| self.vertex(i) == p)
    }

    pub fn contains_point(&self, p: Point) -> bool {
        if !self.bound.contains_point(p) {
            return false;
        }
        let mut inside = self.origin_inside;
        let mut crosser = EdgeCrosser::new(origin(), p, self.vertex(0));
        for i in 1..=self.num_vertices() {
            inside ^= crosser.edge_or_vertex_crossing(self.vertex(i));
        }
        inside
    }

    fn boundaries_crossing(&self, b: &Loop, wedge: &mut Wedge) -> bool {
        for j in 0..b.num_vertices() {
            let mut crosser = EdgeCrosser::new(b.vertex(j), b.vertex(j + 1), self.vertex(0));
            let mut previous: i64 = -2;
            for ai in 0..self.num_vertices() {
                if previous != ai as i64 - 1 {
                    crosser.restart_at(self.vertex(ai));
                }
                previous = ai as i64;
                let crossing = crosser.robust_crossing(self.vertex(ai + 1));
                if crossing < 0 {
                    continue;
                }
                if crossing > 0 {
                    return true;
                }
                if self.vertex(ai + 1) == b.vertex(j + 1)
                    && wedge.process(
                        self.vertex(ai),
                        self.vertex(ai + 1),
                        self.vertex(ai + 2),
                        b.vertex(j),
                        b.vertex(j + 2),
                    )
                {
                    return false;
                }
            }
        }
        false
    }

    pub fn contains_loop(&self, b: &Loop) -> bool {
        if !self.bound.contains(b.bound) {
            return false;
        }
        if !self.contains_point(b.vertex(0)) && self.find_vertex(b.vertex(0)).is_none() {
            return false;
        }
        let mut wedge = Wedge::Contains {
            doesnt_contain: false,
        };
        if self.boundaries_crossing(b, &mut wedge)
            || matches!(
                wedge,
                Wedge::Contains {
                    doesnt_contain: true
                }
            )
        {
            return false;
        }
        if self.bound.union(b.bound).is_full()
            && b.contains_point(self.vertex(0))
            && b.find_vertex(self.vertex(0)).is_none()
        {
            return false;
        }
        true
    }

    pub fn intersects_loop(&self, b: &Loop) -> bool {
        if b.num_vertices() > self.num_vertices() {
            return b.intersects_loop(self);
        }
        if !self.bound.intersects(b.bound) {
            return false;
        }
        if self.contains_point(b.vertex(0)) && self.find_vertex(b.vertex(0)).is_none() {
            return true;
        }
        let mut wedge = Wedge::Intersects { intersects: false };
        if self.boundaries_crossing(b, &mut wedge)
            || matches!(wedge, Wedge::Intersects { intersects: true })
        {
            return true;
        }
        if b.bound.contains(self.bound)
            && b.contains_point(self.vertex(0))
            && b.find_vertex(self.vertex(0)).is_none()
        {
            return true;
        }
        false
    }

    /// `ContainsNested`: for loops whose boundaries do not cross.
    pub fn contains_nested(&self, b: &Loop) -> bool {
        if !self.bound.contains(b.bound) {
            return false;
        }
        match self.find_vertex(b.vertex(1)) {
            None => self.contains_point(b.vertex(1)),
            Some(m) => wedge_contains(
                self.vertex(m - 1),
                self.vertex(m),
                self.vertex(m + 1),
                b.vertex(0),
                b.vertex(2),
            ),
        }
    }

    pub fn contains_cell(&self, cell: &Cell) -> bool {
        if !self.bound.contains_point(cell.center()) {
            return false;
        }
        self.contains_loop(&Loop::from_cell(cell))
    }

    pub fn may_intersect_cell(&self, cell: &Cell) -> bool {
        if !self.bound.intersects(cell.rect_bound()) {
            return false;
        }
        Loop::from_cell(cell).intersects_loop(self)
    }
}

#[derive(Clone, Debug)]
pub struct Polygon {
    pub loops: Vec<Loop>,
    has_holes: bool,
    pub bound: LatLngRect,
}

impl Polygon {
    /// `S2Polygon::Init(loops)`: the loops nested by containment, holes at
    /// odd depths.
    pub fn new(loops: Vec<Loop>) -> Polygon {
        // The loop map: children of each loop, `None` the root. Loops are
        // named by their index in `loops`.
        let n = loops.len();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
        let root = n;
        for new_loop in 0..n {
            insert_loop(&loops, &mut children, new_loop, root);
        }
        let mut order = Vec::with_capacity(n);
        let mut depths = vec![0; n];
        init_loop(&children, root, -1, &mut order, &mut depths);
        let mut out: Vec<Loop> = order
            .into_iter()
            .map(|i| {
                let mut l = loops[i].clone();
                l.depth = depths[i];
                l
            })
            .collect();
        let mut has_holes = false;
        let mut bound = LatLngRect::empty();
        for l in &mut out {
            if l.sign() < 0 {
                has_holes = true;
            } else {
                bound = bound.union(l.bound);
            }
        }
        Polygon {
            loops: out,
            has_holes,
            bound,
        }
    }

    pub fn cap_bound(&self) -> Cap {
        self.bound.cap_bound()
    }

    fn contains_or_crosses(&self, b: &Loop) -> i32 {
        let mut inside = false;
        for l in &self.loops {
            let result = l.contains_or_crosses(b);
            if result < 0 {
                return -1;
            }
            if result > 0 {
                inside ^= true;
            }
        }
        i32::from(inside)
    }

    fn any_loop_contains(&self, b: &Loop) -> bool {
        self.loops.iter().any(|l| l.contains_loop(b))
    }

    fn contains_all_shells(&self, b: &Polygon) -> bool {
        b.loops
            .iter()
            .filter(|l| l.sign() >= 0)
            .all(|l| self.contains_or_crosses(l) > 0)
    }

    fn excludes_all_holes(&self, b: &Polygon) -> bool {
        b.loops
            .iter()
            .filter(|l| l.sign() <= 0)
            .all(|l| self.contains_or_crosses(l) == 0)
    }

    fn intersects_shell(&self, b: &Loop) -> bool {
        let mut inside = false;
        for l in &self.loops {
            if l.contains_loop(b) {
                inside ^= true;
            } else if !b.contains_loop(l) && l.intersects_loop(b) {
                return true;
            }
        }
        inside
    }

    fn intersects_any_shell(&self, b: &Polygon) -> bool {
        b.loops
            .iter()
            .filter(|l| l.sign() >= 0)
            .any(|l| self.intersects_shell(l))
    }

    fn contains_polygon(&self, b: &Polygon) -> bool {
        if self.loops.len() == 1 && b.loops.len() == 1 {
            return self.loops[0].contains_loop(&b.loops[0]);
        }
        if !self.bound.contains(b.bound) && !self.bound.lng.union(b.bound.lng).is_full() {
            return false;
        }
        if !self.has_holes && !b.has_holes {
            return b.loops.iter().all(|l| self.any_loop_contains(l));
        }
        self.contains_all_shells(b) && b.excludes_all_holes(self)
    }

    fn intersects_polygon(&self, b: &Polygon) -> bool {
        if self.loops.len() == 1 && b.loops.len() == 1 {
            return self.loops[0].intersects_loop(&b.loops[0]);
        }
        if !self.bound.intersects(b.bound) {
            return false;
        }
        if !self.has_holes && !b.has_holes {
            return self
                .loops
                .iter()
                .any(|a| b.loops.iter().any(|l| a.intersects_loop(l)));
        }
        self.intersects_any_shell(b) || b.intersects_any_shell(self)
    }

    /// `S2Polygon::Contains(point)`.
    pub fn contains_point(&self, p: Point) -> bool {
        if self.loops.len() == 1 {
            return self.loops[0].contains_point(p);
        }
        if !self.bound.contains_point(p) {
            return false;
        }
        let mut inside = false;
        for l in &self.loops {
            inside ^= l.contains_point(p);
            if inside && !self.has_holes {
                break;
            }
        }
        inside
    }

    /// `Project`: the point of the polygon nearest `p`.
    pub fn project(&self, p: Point) -> Point {
        if self.contains_point(p) {
            return p;
        }
        let mut min_distance = 10.0;
        let (mut min_loop, mut min_vertex) = (0, 0);
        for (li, l) in self.loops.iter().enumerate() {
            for v in 0..l.num_vertices() {
                let d = distance_to_edge(p, l.vertex(v), l.vertex(v + 1));
                if d < min_distance {
                    min_distance = d;
                    min_loop = li;
                    min_vertex = v;
                }
            }
        }
        let l = &self.loops[min_loop];
        closest_point(p, l.vertex(min_vertex), l.vertex(min_vertex + 1))
    }

    pub fn contains_cell(&self, cell: &Cell) -> bool {
        if self.loops.len() == 1 {
            return self.loops[0].contains_cell(cell);
        }
        if !self.bound.contains_point(cell.center()) {
            return false;
        }
        self.contains_polygon(&Polygon::new(vec![Loop::from_cell(cell)]))
    }

    pub fn may_intersect_cell(&self, cell: &Cell) -> bool {
        if self.loops.len() == 1 {
            return self.loops[0].may_intersect_cell(cell);
        }
        if !self.bound.intersects(cell.rect_bound()) {
            return false;
        }
        self.intersects_polygon(&Polygon::new(vec![Loop::from_cell(cell)]))
    }
}

impl Loop {
    /// `ContainsOrCrosses`: 1 contains, 0 neither, -1 the boundaries cross.
    fn contains_or_crosses(&self, b: &Loop) -> i32 {
        if !self.bound.intersects(b.bound) {
            return 0;
        }
        let mut p = CrossesProcessor::default();
        if self.boundaries_crossing_relation(b, &mut p) {
            return -1;
        }
        let result = p.crosses_or_may_contain();
        if result <= 0 {
            return result;
        }
        if !self.bound.contains(b.bound) {
            return 0;
        }
        if !self.contains_point(b.vertex(0)) && self.find_vertex(b.vertex(0)).is_none() {
            return 0;
        }
        1
    }

    /// `AreBoundariesCrossing` with the `ContainsOrCrossesProcessor`.
    fn boundaries_crossing_relation(&self, b: &Loop, p: &mut CrossesProcessor) -> bool {
        for j in 0..b.num_vertices() {
            let mut crosser = EdgeCrosser::new(b.vertex(j), b.vertex(j + 1), self.vertex(0));
            let mut previous: i64 = -2;
            for ai in 0..self.num_vertices() {
                if previous != ai as i64 - 1 {
                    crosser.restart_at(self.vertex(ai));
                }
                previous = ai as i64;
                let crossing = crosser.robust_crossing(self.vertex(ai + 1));
                if crossing < 0 {
                    continue;
                }
                if crossing > 0 {
                    return true;
                }
                if self.vertex(ai + 1) == b.vertex(j + 1)
                    && p.process(
                        self.vertex(ai),
                        self.vertex(ai + 1),
                        self.vertex(ai + 2),
                        b.vertex(j),
                        b.vertex(j + 2),
                    )
                {
                    return false;
                }
            }
        }
        false
    }
}

#[derive(Default)]
struct CrossesProcessor {
    has_boundary_crossing: bool,
    a_super: bool,
    b_super: bool,
    has_disjoint: bool,
}

impl CrossesProcessor {
    fn crosses_or_may_contain(&self) -> i32 {
        if self.has_boundary_crossing {
            return -1;
        }
        if self.has_disjoint || self.b_super {
            return 0;
        }
        1
    }

    fn process(&mut self, a0: Point, ab1: Point, a2: Point, b0: Point, b2: Point) -> bool {
        let relation = wedge_relation(a0, ab1, a2, b0, b2);
        if relation == WedgeRelation::ProperlyOverlaps {
            self.has_boundary_crossing = true;
            return true;
        }
        self.a_super |= relation == WedgeRelation::ProperlyContains;
        self.b_super |= relation == WedgeRelation::IsProperlyContained;
        if self.a_super && self.b_super {
            self.has_boundary_crossing = true;
            return true;
        }
        self.has_disjoint |= relation == WedgeRelation::IsDisjoint;
        false
    }
}

#[derive(PartialEq)]
enum WedgeRelation {
    Equals,
    ProperlyContains,
    IsProperlyContained,
    ProperlyOverlaps,
    IsDisjoint,
}

fn wedge_relation(a0: Point, ab1: Point, a2: Point, b0: Point, b2: Point) -> WedgeRelation {
    if a0 == b0 && a2 == b2 {
        return WedgeRelation::Equals;
    }
    if ordered_ccw(a0, a2, b2, ab1) {
        if ordered_ccw(b2, b0, a0, ab1) {
            return WedgeRelation::ProperlyContains;
        }
        return if a2 == b2 {
            WedgeRelation::IsProperlyContained
        } else {
            WedgeRelation::ProperlyOverlaps
        };
    }
    if ordered_ccw(a0, b0, b2, ab1) {
        return WedgeRelation::IsProperlyContained;
    }
    if ordered_ccw(a0, b0, a2, ab1) {
        WedgeRelation::IsDisjoint
    } else {
        WedgeRelation::ProperlyOverlaps
    }
}

/// `S2Polygon::InsertLoop`.
fn insert_loop(loops: &[Loop], children: &mut [Vec<usize>], new_loop: usize, parent: usize) {
    let n = loops.len();
    for &child in children[parent].clone().iter() {
        if loops[child].contains_nested(&loops[new_loop]) {
            insert_loop(loops, children, new_loop, child);
            return;
        }
    }
    let mut moved = Vec::new();
    let mut i = 0;
    while i < children[parent].len() {
        let child = children[parent][i];
        if child != n && loops[new_loop].contains_nested(&loops[child]) {
            moved.push(child);
            children[parent].remove(i);
        } else {
            i += 1;
        }
    }
    children[new_loop].extend(moved);
    children[parent].push(new_loop);
}

/// `S2Polygon::InitLoop`: depth-first, parents before their children.
fn init_loop(
    children: &[Vec<usize>],
    at: usize,
    depth: i32,
    order: &mut Vec<usize>,
    depths: &mut [i32],
) {
    if at != children.len() - 1 {
        depths[at] = depth;
        order.push(at);
    }
    for &child in &children[at] {
        init_loop(children, child, depth + 1, order, depths);
    }
}
