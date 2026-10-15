//! `S2Cap`.

use super::cell::Cell;
use super::interval::{LatLngRect, R1Interval, S1Interval};
use super::point::{latitude, longitude, Point};
use super::{cmax, remainder};
use std::f64::consts::{FRAC_PI_2, PI};

const ROUND_UP: f64 = 1.0 + 1.0 / (1u64 << 52) as f64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cap {
    pub axis: Point,
    pub height: f64,
}

fn height_for_angle(radians: f64) -> f64 {
    if radians >= PI {
        return 2.0;
    }
    let d = (0.5 * radians).sin();
    2.0 * d * d
}

impl Cap {
    pub fn from_axis_height(axis: Point, height: f64) -> Cap {
        Cap { axis, height }
    }
    pub fn from_axis_angle(axis: Point, radians: f64) -> Cap {
        Cap::from_axis_height(axis, height_for_angle(radians))
    }
    pub fn empty() -> Cap {
        Cap::from_axis_height(Point::new(1.0, 0.0, 0.0), -1.0)
    }
    pub fn full() -> Cap {
        Cap::from_axis_height(Point::new(1.0, 0.0, 0.0), 2.0)
    }
    pub fn is_empty(self) -> bool {
        self.height < 0.0
    }
    pub fn is_full(self) -> bool {
        self.height >= 2.0
    }
    pub fn angle(self) -> f64 {
        if self.is_empty() {
            return -1.0;
        }
        2.0 * (0.5 * self.height).sqrt().asin()
    }
    pub fn complement(self) -> Cap {
        let height = if self.is_full() {
            -1.0
        } else {
            2.0 - cmax(self.height, 0.0)
        };
        Cap::from_axis_height(-self.axis, height)
    }
    pub fn add_point(&mut self, p: Point) {
        if self.is_empty() {
            self.axis = p;
            self.height = 0.0;
        } else {
            let dist2 = (self.axis - p).norm2();
            self.height = cmax(self.height, ROUND_UP * 0.5 * dist2);
        }
    }
    pub fn contains_point(self, p: Point) -> bool {
        (self.axis - p).norm2() <= 2.0 * self.height
    }

    pub fn rect_bound(self) -> LatLngRect {
        if self.is_empty() {
            return LatLngRect::empty();
        }
        let (axis_lat, axis_lng) = (latitude(self.axis), longitude(self.axis));
        let cap_angle = self.angle();
        let mut all_longitudes = false;
        let mut lat = [axis_lat - cap_angle, 0.0];
        let mut lng = [-PI, PI];
        if lat[0] <= -FRAC_PI_2 {
            lat[0] = -FRAC_PI_2;
            all_longitudes = true;
        }
        lat[1] = axis_lat + cap_angle;
        if lat[1] >= FRAC_PI_2 {
            lat[1] = FRAC_PI_2;
            all_longitudes = true;
        }
        if !all_longitudes {
            let sin_a = (self.height * (2.0 - self.height)).sqrt();
            let sin_c = axis_lat.cos();
            if sin_a <= sin_c {
                let angle_a = (sin_a / sin_c).asin();
                lng[0] = remainder(axis_lng - angle_a, 2.0 * PI);
                lng[1] = remainder(axis_lng + angle_a, 2.0 * PI);
            }
        }
        LatLngRect::new(
            R1Interval::new(lat[0], lat[1]),
            S1Interval::new(lng[0], lng[1]),
        )
    }

    /// `Intersects(cell, vertices)`: whether the cap meets the cell, given
    /// that it holds none of the cell's vertices.
    fn intersects_cell(self, cell: &Cell, vertices: &[Point; 4]) -> bool {
        if self.height >= 1.0 {
            return false;
        }
        if self.is_empty() {
            return false;
        }
        if cell.contains_point(self.axis) {
            return true;
        }
        let sin2_angle = self.height * (2.0 - self.height);
        for k in 0..4 {
            let edge = cell.edge_raw(k);
            let dot = self.axis.dot(edge);
            if dot > 0.0 {
                continue;
            }
            if dot * dot > sin2_angle * edge.norm2() {
                return false;
            }
            let dir = edge.cross(self.axis);
            if dir.dot(vertices[k]) < 0.0 && dir.dot(vertices[(k + 1) & 3]) > 0.0 {
                return true;
            }
        }
        false
    }

    pub fn contains_cell(self, cell: &Cell) -> bool {
        let mut vertices = [Point::new(0.0, 0.0, 0.0); 4];
        for (k, v) in vertices.iter_mut().enumerate() {
            *v = cell.vertex(k);
            if !self.contains_point(*v) {
                return false;
            }
        }
        !self.complement().intersects_cell(cell, &vertices)
    }

    pub fn may_intersect_cell(self, cell: &Cell) -> bool {
        let mut vertices = [Point::new(0.0, 0.0, 0.0); 4];
        for (k, v) in vertices.iter_mut().enumerate() {
            *v = cell.vertex(k);
            if self.contains_point(*v) {
                return true;
            }
        }
        self.intersects_cell(cell, &vertices)
    }
}
