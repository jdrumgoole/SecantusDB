//! `S2Cell`.

use super::cap::Cap;
use super::cellid::{CellId, POS_TO_IJ, POS_TO_ORIENTATION};
use super::interval::{LatLngRect, R1Interval, S1Interval};
use super::point::{
    face_uv_to_xyz, face_xyz_to_uv, latitude, longitude, st_to_uv, u_axis_z, u_norm, v_axis_z,
    v_norm, Point,
};
use super::MAX_LEVEL;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub face: i32,
    pub level: i32,
    pub orientation: i32,
    pub id: CellId,
    /// `uv_[d][0..2]`: the u (d = 0) and v (d = 1) bounds.
    pub uv: [[f64; 2]; 2],
}

impl Cell {
    /// `S2Cell(id)`, through `Init`.
    pub fn new(id: CellId) -> Cell {
        let (face, i, j, orientation) = id.to_face_ij_orientation();
        let level = id.level();
        let size = CellId::size_ij(level);
        let mut uv = [[0.0; 2]; 2];
        for (d, ij) in [i, j].into_iter().enumerate() {
            let lo = ij & size.wrapping_neg();
            let hi = lo + size;
            let k = 1.0 / f64::from(1i32 << MAX_LEVEL);
            uv[d][0] = st_to_uv(k * f64::from(lo));
            uv[d][1] = st_to_uv(k * f64::from(hi));
        }
        Cell {
            face,
            level,
            orientation,
            id,
            uv,
        }
    }

    /// `S2Cell(point)`: the leaf cell holding the point.
    pub fn from_point(p: Point) -> Cell {
        Cell::new(CellId::from_point(p))
    }

    pub fn vertex_raw(&self, k: usize) -> Point {
        face_uv_to_xyz(
            self.face,
            self.uv[0][(k >> 1) ^ (k & 1)],
            self.uv[1][k >> 1],
        )
    }

    pub fn vertex(&self, k: usize) -> Point {
        self.vertex_raw(k).normalize()
    }

    pub fn edge_raw(&self, k: usize) -> Point {
        match k {
            0 => v_norm(self.face, self.uv[1][0]),
            1 => u_norm(self.face, self.uv[0][1]),
            2 => -v_norm(self.face, self.uv[1][1]),
            _ => -u_norm(self.face, self.uv[0][0]),
        }
    }

    pub fn center(&self) -> Point {
        self.id.to_point_raw().normalize()
    }

    /// `Subdivide`. A leaf has no children.
    pub fn subdivide(&self) -> Option<[Cell; 4]> {
        if self.id.is_leaf() {
            return None;
        }
        let uv_mid = self.id.center_uv();
        let uv_mid = [uv_mid.0, uv_mid.1];
        let mut id = self.id.child_begin();
        let mut out = [*self; 4];
        for (pos, child) in out.iter_mut().enumerate() {
            child.face = self.face;
            child.level = self.level + 1;
            child.orientation = self.orientation ^ POS_TO_ORIENTATION[pos];
            child.id = id;
            let ij = POS_TO_IJ[self.orientation as usize][pos];
            let i = (ij >> 1) as usize;
            let j = (ij & 1) as usize;
            child.uv[0][i] = self.uv[0][i];
            child.uv[0][1 - i] = uv_mid[0];
            child.uv[1][j] = self.uv[1][j];
            child.uv[1][1 - j] = uv_mid[1];
            id = id.next();
        }
        Some(out)
    }

    pub fn cap_bound(&self) -> Cap {
        let u = 0.5 * (self.uv[0][0] + self.uv[0][1]);
        let v = 0.5 * (self.uv[1][0] + self.uv[1][1]);
        let mut cap = Cap::from_axis_height(face_uv_to_xyz(self.face, u, v).normalize(), 0.0);
        for k in 0..4 {
            cap.add_point(self.vertex(k));
        }
        cap
    }

    fn latitude_at(&self, i: usize, j: usize) -> f64 {
        latitude(face_uv_to_xyz(self.face, self.uv[0][i], self.uv[1][j]))
    }

    fn longitude_at(&self, i: usize, j: usize) -> f64 {
        longitude(face_uv_to_xyz(self.face, self.uv[0][i], self.uv[1][j]))
    }

    pub fn rect_bound(&self) -> LatLngRect {
        if self.level > 0 {
            let u = self.uv[0][0] + self.uv[0][1];
            let v = self.uv[1][0] + self.uv[1][1];
            let i = usize::from(if u_axis_z(self.face) == 0.0 {
                u < 0.0
            } else {
                u > 0.0
            });
            let j = usize::from(if v_axis_z(self.face) == 0.0 {
                v < 0.0
            } else {
                v > 0.0
            });
            let max_error = 1.0 / (1u64 << 51) as f64;
            let mut lat =
                R1Interval::from_point_pair(self.latitude_at(i, j), self.latitude_at(1 - i, 1 - j));
            lat = lat.expanded(max_error).intersection(LatLngRect::full_lat());
            if lat.lo == -FRAC_PI_2 || lat.hi == FRAC_PI_2 {
                return LatLngRect::new(lat, S1Interval::full());
            }
            let lng = S1Interval::from_point_pair(
                self.longitude_at(i, 1 - j),
                self.longitude_at(1 - i, j),
            );
            return LatLngRect::new(lat, lng.expanded(max_error));
        }
        let pole_min_lat = (1.0f64 / 3.0).sqrt().asin();
        let quarter = |lo: f64, hi: f64| {
            LatLngRect::new(
                R1Interval::new(-FRAC_PI_4, FRAC_PI_4),
                S1Interval::new(lo, hi),
            )
        };
        match self.face {
            0 => quarter(-FRAC_PI_4, FRAC_PI_4),
            1 => quarter(FRAC_PI_4, 3.0 * FRAC_PI_4),
            2 => LatLngRect::new(
                R1Interval::new(pole_min_lat, FRAC_PI_2),
                S1Interval::new(-PI, PI),
            ),
            3 => quarter(3.0 * FRAC_PI_4, -3.0 * FRAC_PI_4),
            4 => quarter(-3.0 * FRAC_PI_4, -FRAC_PI_4),
            _ => LatLngRect::new(
                R1Interval::new(-FRAC_PI_2, -pole_min_lat),
                S1Interval::new(-PI, PI),
            ),
        }
    }

    pub fn contains_point(&self, p: Point) -> bool {
        let Some((u, v)) = face_xyz_to_uv(self.face, p) else {
            return false;
        };
        u >= self.uv[0][0] && u <= self.uv[0][1] && v >= self.uv[1][0] && v <= self.uv[1][1]
    }
}
