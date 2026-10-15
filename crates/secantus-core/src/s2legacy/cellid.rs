//! `S2CellId`.

use super::point::{face_uv_to_xyz, st_to_uv, uv_to_st, xyz_to_face_uv, Point};
use super::MAX_LEVEL;
use std::sync::OnceLock;

const POS_BITS: i32 = 2 * MAX_LEVEL + 1;
const MAX_SIZE: i32 = 1 << MAX_LEVEL;
const LOOKUP_BITS: i32 = 4;
const SWAP_MASK: i32 = 0x01;
const INVERT_MASK: i32 = 0x02;

/// `S2::kPosToIJ`.
pub const POS_TO_IJ: [[i32; 4]; 4] = [[0, 1, 3, 2], [0, 2, 3, 1], [3, 2, 0, 1], [3, 1, 0, 2]];
/// `S2::kPosToOrientation`.
pub const POS_TO_ORIENTATION: [i32; 4] = [SWAP_MASK, 0, 0, INVERT_MASK + SWAP_MASK];

struct Lookup {
    pos: Vec<u16>,
    ij: Vec<u16>,
}

fn init_lookup_cell(
    t: &mut Lookup,
    mut level: i32,
    mut i: i32,
    mut j: i32,
    orig: i32,
    mut pos: i32,
    orientation: i32,
) {
    if level == LOOKUP_BITS {
        let ij = (i << LOOKUP_BITS) + j;
        t.pos[((ij << 2) + orig) as usize] = ((pos << 2) + orientation) as u16;
        t.ij[((pos << 2) + orig) as usize] = ((ij << 2) + orientation) as u16;
    } else {
        level += 1;
        i <<= 1;
        j <<= 1;
        pos <<= 2;
        let r = POS_TO_IJ[orientation as usize];
        for (k, rk) in r.iter().enumerate() {
            init_lookup_cell(
                t,
                level,
                i + (rk >> 1),
                j + (rk & 1),
                orig,
                pos + k as i32,
                orientation ^ POS_TO_ORIENTATION[k],
            );
        }
    }
}

fn lookup() -> &'static Lookup {
    static TABLE: OnceLock<Lookup> = OnceLock::new();
    TABLE.get_or_init(|| {
        let size = 1 << (2 * LOOKUP_BITS + 2);
        let mut t = Lookup {
            pos: vec![0; size],
            ij: vec![0; size],
        };
        for o in [0, SWAP_MASK, INVERT_MASK, SWAP_MASK | INVERT_MASK] {
            init_lookup_cell(&mut t, 0, 0, 0, o, 0, o);
        }
        t
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellId(pub u64);

impl CellId {
    pub fn lsb_for_level(level: i32) -> u64 {
        1u64 << (2 * (MAX_LEVEL - level))
    }
    pub fn size_ij(level: i32) -> i32 {
        1 << (MAX_LEVEL - level)
    }
    pub fn lsb(self) -> u64 {
        self.0 & self.0.wrapping_neg()
    }
    pub fn face(self) -> i32 {
        (self.0 >> POS_BITS) as i32
    }
    pub fn is_leaf(self) -> bool {
        self.0 & 1 != 0
    }
    pub fn is_face(self) -> bool {
        self.0 & (CellId::lsb_for_level(0) - 1) == 0
    }
    pub fn level(self) -> i32 {
        if self.is_leaf() {
            return MAX_LEVEL;
        }
        let mut x = self.0 as u32;
        let mut level = -1;
        if x != 0 {
            level += 16;
        } else {
            x = (self.0 >> 32) as u32;
        }
        x &= x.wrapping_neg();
        if x & 0x0000_5555 != 0 {
            level += 8;
        }
        if x & 0x0055_0055 != 0 {
            level += 4;
        }
        if x & 0x0505_0505 != 0 {
            level += 2;
        }
        if x & 0x1111_1111 != 0 {
            level += 1;
        }
        level
    }
    pub fn parent(self, level: i32) -> CellId {
        let new_lsb = CellId::lsb_for_level(level);
        CellId((self.0 & new_lsb.wrapping_neg()) | new_lsb)
    }
    pub fn immediate_parent(self) -> CellId {
        let new_lsb = self.lsb() << 2;
        CellId((self.0 & new_lsb.wrapping_neg()) | new_lsb)
    }
    pub fn child_begin(self) -> CellId {
        let old = self.lsb();
        CellId(self.0.wrapping_sub(old).wrapping_add(old >> 2))
    }
    pub fn child_begin_at(self, level: i32) -> CellId {
        CellId(
            self.0
                .wrapping_sub(self.lsb())
                .wrapping_add(CellId::lsb_for_level(level)),
        )
    }
    pub fn child_end_at(self, level: i32) -> CellId {
        CellId(
            self.0
                .wrapping_add(self.lsb())
                .wrapping_add(CellId::lsb_for_level(level)),
        )
    }
    pub fn next(self) -> CellId {
        CellId(self.0.wrapping_add(self.lsb() << 1))
    }
    pub fn range_min(self) -> CellId {
        CellId(self.0 - (self.lsb() - 1))
    }
    pub fn range_max(self) -> CellId {
        CellId(self.0 + (self.lsb() - 1))
    }
    pub fn contains(self, other: CellId) -> bool {
        other >= self.range_min() && other <= self.range_max()
    }
    pub fn intersects(self, other: CellId) -> bool {
        other.range_min() <= self.range_max() && other.range_max() >= self.range_min()
    }
    pub fn from_face_pos_level(face: i32, pos: u64, level: i32) -> CellId {
        CellId(((face as u64) << POS_BITS) + (pos | 1)).parent(level)
    }

    fn st_to_ij(s: f64) -> i32 {
        // MathUtil::FastIntRound is rint(): ties to even.
        let r = (f64::from(MAX_SIZE) * s - 0.5).round_ties_even() as i32;
        r.clamp(0, MAX_SIZE - 1)
    }

    pub fn from_face_ij(face: i32, i: i32, j: i32) -> CellId {
        let t = lookup();
        let mut n = [0u32, (face as u32) << (POS_BITS - 33)];
        let mut bits = face & SWAP_MASK;
        for k in (0..8).rev() {
            let mask = (1 << LOOKUP_BITS) - 1;
            bits += ((i >> (k * LOOKUP_BITS)) & mask) << (LOOKUP_BITS + 2);
            bits += ((j >> (k * LOOKUP_BITS)) & mask) << 2;
            bits = i32::from(t.pos[bits as usize]);
            n[(k >> 2) as usize] |= ((bits >> 2) as u32) << ((k & 3) * 2 * LOOKUP_BITS);
            bits &= SWAP_MASK | INVERT_MASK;
        }
        CellId(((u64::from(n[1]) << 32) + u64::from(n[0])) * 2 + 1)
    }

    pub fn from_point(p: Point) -> CellId {
        let (face, u, v) = xyz_to_face_uv(p);
        let i = CellId::st_to_ij(uv_to_st(u));
        let j = CellId::st_to_ij(uv_to_st(v));
        CellId::from_face_ij(face, i, j)
    }

    /// `ToFaceIJOrientation`: `(face, i, j, orientation)`.
    pub fn to_face_ij_orientation(self) -> (i32, i32, i32, i32) {
        let t = lookup();
        let (mut i, mut j) = (0i32, 0i32);
        let face = self.face();
        let mut bits = face & SWAP_MASK;
        for k in (0..8).rev() {
            let nbits = if k == 7 {
                MAX_LEVEL - 7 * LOOKUP_BITS
            } else {
                LOOKUP_BITS
            };
            bits +=
                (((self.0 >> (k * 2 * LOOKUP_BITS + 1)) as i32) & ((1 << (2 * nbits)) - 1)) << 2;
            bits = i32::from(t.ij[bits as usize]);
            i += (bits >> (LOOKUP_BITS + 2)) << (k * LOOKUP_BITS);
            j += ((bits >> 2) & ((1 << LOOKUP_BITS) - 1)) << (k * LOOKUP_BITS);
            bits &= SWAP_MASK | INVERT_MASK;
        }
        if self.lsb() & 0x1111_1111_1111_1110 != 0 {
            bits ^= SWAP_MASK;
        }
        (face, i, j, bits)
    }

    fn center_si_ti(self) -> (i32, i32, i32) {
        let (face, i, j, _) = self.to_face_ij_orientation();
        let delta = if self.is_leaf() {
            1
        } else if (i ^ ((self.0 as i32) >> 2)) & 1 != 0 {
            2
        } else {
            0
        };
        (face, 2 * i + delta, 2 * j + delta)
    }

    pub fn to_point_raw(self) -> Point {
        let (face, si, ti) = self.center_si_ti();
        let k = 0.5 / f64::from(MAX_SIZE);
        face_uv_to_xyz(
            face,
            st_to_uv(k * f64::from(si)),
            st_to_uv(k * f64::from(ti)),
        )
    }

    pub fn center_uv(self) -> (f64, f64) {
        let (_, si, ti) = self.center_si_ti();
        let k = 0.5 / f64::from(MAX_SIZE);
        (st_to_uv(k * f64::from(si)), st_to_uv(k * f64::from(ti)))
    }

    fn from_face_ij_wrap(face: i32, i: i32, j: i32) -> CellId {
        let i = i.clamp(-1, MAX_SIZE);
        let j = j.clamp(-1, MAX_SIZE);
        let scale = 1.0 / f64::from(MAX_SIZE);
        // In 64 bits: `i << 1` overflows an int at the far edge, where the
        // C++ wraps back to this same value.
        let u = scale * ((i64::from(i) << 1) + 1 - i64::from(MAX_SIZE)) as f64;
        let v = scale * ((i64::from(j) << 1) + 1 - i64::from(MAX_SIZE)) as f64;
        let (face, u, v) = xyz_to_face_uv(face_uv_to_xyz(face, u, v));
        CellId::from_face_ij(
            face,
            CellId::st_to_ij(0.5 * (u + 1.0)),
            CellId::st_to_ij(0.5 * (v + 1.0)),
        )
    }

    fn from_face_ij_same(face: i32, i: i32, j: i32, same_face: bool) -> CellId {
        if same_face {
            CellId::from_face_ij(face, i, j)
        } else {
            CellId::from_face_ij_wrap(face, i, j)
        }
    }

    /// `AppendVertexNeighbors`.
    pub fn vertex_neighbors(self, level: i32) -> Vec<CellId> {
        let (face, i, j, _) = self.to_face_ij_orientation();
        let halfsize = CellId::size_ij(level + 1);
        let size = halfsize << 1;
        let (ioffset, isame) = if i & halfsize != 0 {
            (size, (i + size) < MAX_SIZE)
        } else {
            (-size, (i - size) >= 0)
        };
        let (joffset, jsame) = if j & halfsize != 0 {
            (size, (j + size) < MAX_SIZE)
        } else {
            (-size, (j - size) >= 0)
        };
        let mut out = vec![
            self.parent(level),
            CellId::from_face_ij_same(face, i + ioffset, j, isame).parent(level),
            CellId::from_face_ij_same(face, i, j + joffset, jsame).parent(level),
        ];
        if isame || jsame {
            out.push(
                CellId::from_face_ij_same(face, i + ioffset, j + joffset, isame && jsame)
                    .parent(level),
            );
        }
        out
    }

    /// The key a `2dsphere` (version 3) index stores: the id as a signed
    /// 64-bit integer, so faces 4 and 5 sort below faces 0 to 3.
    pub fn key(self) -> i64 {
        self.0 as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s2legacy::latlng_point;

    #[test]
    fn leaf_of_a_point_round_trips() {
        let id = CellId::from_point(latlng_point(1.0, 1.0));
        assert_eq!(id.level(), 30);
        assert!(id.is_leaf());
        let parent = id.parent(10);
        assert_eq!(parent.level(), 10);
        assert!(parent.contains(id));
        let p = id.to_point_raw().normalize();
        assert_eq!(CellId::from_point(p), id);
    }

    #[test]
    fn face_bits() {
        let f = CellId::from_face_pos_level(5, 0, 0);
        assert_eq!(f.face(), 5);
        assert!(f.is_face());
        assert!(f.key() < 0);
    }
}
