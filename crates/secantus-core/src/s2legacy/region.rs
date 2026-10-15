//! `S2Polyline` and the regions a covering is asked for: the `S2Region`
//! interface over a cap, a cell, a polygon, a polyline, and the union and
//! intersection of regions.

use super::cap::Cap;
use super::cell::Cell;
use super::edge::{closest_point, distance_to_edge, EdgeCrosser, RectBounder};
use super::interval::LatLngRect;
use super::loops::{Loop, Polygon};
use super::point::Point;

#[derive(Clone, Debug)]
pub struct Polyline {
    pub vertices: Vec<Point>,
}

impl Polyline {
    pub fn rect_bound(&self) -> LatLngRect {
        let mut bounder = RectBounder::new();
        for v in &self.vertices {
            bounder.add_point(*v);
        }
        bounder.bound
    }

    /// `Project`: the point of the line nearest `p`.
    pub fn project(&self, p: Point) -> Point {
        if self.vertices.len() == 1 {
            return self.vertices[0];
        }
        let mut min_distance = 10.0;
        let mut min_index = 0;
        for i in 1..self.vertices.len() {
            let d = distance_to_edge(p, self.vertices[i - 1], self.vertices[i]);
            if d < min_distance {
                min_distance = d;
                min_index = i;
            }
        }
        closest_point(p, self.vertices[min_index - 1], self.vertices[min_index])
    }

    pub fn may_intersect_cell(&self, cell: &Cell) -> bool {
        if self.vertices.is_empty() {
            return false;
        }
        if self.vertices.iter().any(|v| cell.contains_point(*v)) {
            return true;
        }
        let corners: Vec<Point> = (0..4).map(|k| cell.vertex(k)).collect();
        for j in 0..4 {
            let mut crosser = EdgeCrosser::new(corners[j], corners[(j + 1) & 3], self.vertices[0]);
            for v in &self.vertices[1..] {
                if crosser.robust_crossing(*v) >= 0 {
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Clone, Debug)]
pub enum Region {
    Cap(Cap),
    Cell(Cell),
    Polygon(Polygon),
    Polyline(Polyline),
    /// A loop as it was given, not normalised: mongod's `BigSimplePolygon`
    /// (a polygon with the strict-winding `crs`), which may be larger than a
    /// hemisphere.
    Loop(Loop),
    Union(Vec<Region>),
    Intersection(Vec<Region>),
}

impl Region {
    pub fn cap_bound(&self) -> Cap {
        match self {
            Region::Cap(c) => *c,
            Region::Cell(c) => c.cap_bound(),
            Region::Polygon(p) => p.cap_bound(),
            Region::Loop(l) => l.cap_bound(),
            Region::Polyline(_) | Region::Union(_) | Region::Intersection(_) => {
                self.rect_bound().cap_bound()
            }
        }
    }

    pub fn rect_bound(&self) -> LatLngRect {
        match self {
            Region::Cap(c) => c.rect_bound(),
            Region::Cell(c) => c.rect_bound(),
            Region::Polygon(p) => p.bound,
            Region::Loop(l) => l.bound,
            Region::Polyline(l) => l.rect_bound(),
            Region::Union(rs) => rs
                .iter()
                .fold(LatLngRect::empty(), |acc, r| acc.union(r.rect_bound())),
            Region::Intersection(rs) => rs.iter().fold(LatLngRect::full(), |acc, r| {
                acc.intersection(r.rect_bound())
            }),
        }
    }

    pub fn may_intersect(&self, cell: &Cell) -> bool {
        match self {
            Region::Cap(c) => c.may_intersect_cell(cell),
            Region::Cell(c) => c.id.intersects(cell.id),
            Region::Polygon(p) => p.may_intersect_cell(cell),
            Region::Loop(l) => l.may_intersect_cell(cell),
            Region::Polyline(l) => l.may_intersect_cell(cell),
            Region::Union(rs) => rs.iter().any(|r| r.may_intersect(cell)),
            Region::Intersection(rs) => rs.iter().all(|r| r.may_intersect(cell)),
        }
    }

    pub fn contains(&self, cell: &Cell) -> bool {
        match self {
            Region::Cap(c) => c.contains_cell(cell),
            Region::Cell(c) => c.id.contains(cell.id),
            Region::Polygon(p) => p.contains_cell(cell),
            Region::Loop(l) => l.contains_cell(cell),
            Region::Polyline(_) => false,
            Region::Union(rs) => rs.iter().any(|r| r.contains(cell)),
            Region::Intersection(rs) => rs.iter().all(|r| r.contains(cell)),
        }
    }
}
