//! `S2RegionCoverer` and the `S2CellUnion` operations mongod uses.
//!
//! The coverer keeps its candidates in a `std::priority_queue` ordered by
//! priority alone, so which of two candidates of equal priority comes out
//! first is decided by the heap's own sift steps. That decides which cells
//! end up in a covering once it reaches its cell limit, so the queue here is
//! [`crate::heap::Heap`], a transcription of libc++'s heap, the library
//! the reference mongod (8.2.11 on macOS) is built with.

use super::cell::Cell;
use super::cellid::CellId;
use super::region::Region;
use super::{MAX_LEVEL, MIN_WIDTH};
use crate::heap::Heap;

pub struct RegionCoverer {
    pub min_level: i32,
    pub max_level: i32,
    pub level_mod: i32,
    pub max_cells: i32,
}

struct Candidate {
    cell: Cell,
    is_terminal: bool,
    children: Vec<Candidate>,
}

struct Run<'a> {
    c: &'a RegionCoverer,
    region: &'a Region,
    result: Vec<CellId>,
    pq: Heap<(i32, Candidate)>,
}

impl Run<'_> {
    fn max_children_shift(&self) -> i32 {
        2 * self.c.level_mod
    }

    fn new_candidate(&self, cell: Cell) -> Option<Candidate> {
        if !self.region.may_intersect(&cell) {
            return None;
        }
        let mut is_terminal = false;
        if cell.level >= self.c.min_level
            && (cell.level + self.c.level_mod > self.c.max_level || self.region.contains(&cell))
        {
            is_terminal = true;
        }
        Some(Candidate {
            cell,
            is_terminal,
            children: Vec::new(),
        })
    }

    fn expand_children(&self, candidate: &mut Candidate, cell: Cell, mut num_levels: i32) -> i32 {
        num_levels -= 1;
        let children = cell.subdivide().expect("a coverer never subdivides a leaf");
        let mut num_terminals = 0;
        for child in children {
            if num_levels > 0 {
                if self.region.may_intersect(&child) {
                    num_terminals += self.expand_children(candidate, child, num_levels);
                }
                continue;
            }
            if let Some(c) = self.new_candidate(child) {
                if c.is_terminal {
                    num_terminals += 1;
                }
                candidate.children.push(c);
            }
        }
        num_terminals
    }

    fn add_candidate(&mut self, candidate: Option<Candidate>) {
        let Some(mut candidate) = candidate else {
            return;
        };
        if candidate.is_terminal {
            self.result.push(candidate.cell.id);
            return;
        }
        let num_levels = if candidate.cell.level < self.c.min_level {
            1
        } else {
            self.c.level_mod
        };
        let cell = candidate.cell;
        let num_terminals = self.expand_children(&mut candidate, cell, num_levels);
        let shift = self.max_children_shift();
        if candidate.children.is_empty() {
            // Nothing below intersects: dropped.
        } else if num_terminals == 1 << shift && candidate.cell.level >= self.c.min_level {
            candidate.is_terminal = true;
            candidate.children.clear();
            self.add_candidate(Some(candidate));
        } else {
            let n = candidate.children.len() as i32;
            let priority = -((((candidate.cell.level << shift) + n) << shift) + num_terminals);
            self.pq.push((priority, candidate));
        }
    }

    fn initial_candidates(&mut self) {
        if self.c.max_cells >= 4 {
            let cap = self.region.cap_bound();
            let mut level = MIN_WIDTH
                .max_level(2.0 * cap.angle())
                .min(self.c.max_level.min(MAX_LEVEL - 1));
            if self.c.level_mod > 1 && level > self.c.min_level {
                level -= (level - self.c.min_level) % self.c.level_mod;
            }
            if level > 0 {
                let id = CellId::from_point(cap.axis);
                for base in id.vertex_neighbors(level) {
                    let c = self.new_candidate(Cell::new(base));
                    self.add_candidate(c);
                }
                return;
            }
        }
        for face in 0..6 {
            let c = self.new_candidate(Cell::new(CellId::from_face_pos_level(face, 0, 0)));
            self.add_candidate(c);
        }
    }

    fn covering_internal(&mut self) {
        self.initial_candidates();
        while let Some((_, mut candidate)) = self.pq.pop() {
            let n = candidate.children.len() as i32;
            if candidate.cell.level < self.c.min_level
                || n == 1
                || self.result.len() as i32 + self.pq.len() as i32 + n <= self.c.max_cells
            {
                for child in std::mem::take(&mut candidate.children) {
                    self.add_candidate(Some(child));
                }
            } else {
                candidate.is_terminal = true;
                candidate.children.clear();
                self.add_candidate(Some(candidate));
            }
        }
    }
}

impl RegionCoverer {
    /// `GetCovering`: the cells, normalised and then denormalised to
    /// `min_level`.
    pub fn covering(&self, region: &Region) -> Vec<CellId> {
        let mut run = Run {
            c: self,
            region,
            result: Vec::new(),
            pq: Heap::new(),
        };
        run.covering_internal();
        let union = normalize(run.result);
        denormalize(&union, self.min_level, self.level_mod)
    }
}

/// `S2CellUnion::Normalize`: sorted, no cell inside another, and four
/// siblings replaced by their parent.
pub fn normalize(mut ids: Vec<CellId>) -> Vec<CellId> {
    ids.sort();
    let mut output: Vec<CellId> = Vec::with_capacity(ids.len());
    for mut id in ids {
        if output.last().is_some_and(|last| last.contains(id)) {
            continue;
        }
        while output.last().is_some_and(|last| id.contains(*last)) {
            output.pop();
        }
        while output.len() >= 3 {
            let n = output.len();
            if (output[n - 3].0 ^ output[n - 2].0 ^ output[n - 1].0) != id.0 {
                break;
            }
            let mut mask = id.lsb() << 1;
            mask = !(mask.wrapping_add(mask << 1));
            let id_masked = id.0 & mask;
            if (output[n - 3].0 & mask) != id_masked
                || (output[n - 2].0 & mask) != id_masked
                || (output[n - 1].0 & mask) != id_masked
                || id.is_face()
            {
                break;
            }
            output.truncate(n - 3);
            id = id.immediate_parent();
        }
        output.push(id);
    }
    output
}

fn denormalize(ids: &[CellId], min_level: i32, level_mod: i32) -> Vec<CellId> {
    let mut output = Vec::with_capacity(ids.len());
    for &id in ids {
        let level = id.level();
        let mut new_level = min_level.max(level);
        if level_mod > 1 {
            new_level += (MAX_LEVEL - (new_level - min_level)) % level_mod;
            new_level = new_level.min(MAX_LEVEL);
        }
        if new_level == level {
            output.push(id);
        } else {
            let end = id.child_end_at(new_level);
            let mut c = id.child_begin_at(new_level);
            while c != end {
                output.push(c);
                c = c.next();
            }
        }
    }
    output
}

/// `S2CellUnion::Contains(id)` over a normalised union.
pub fn union_contains(ids: &[CellId], id: CellId) -> bool {
    let i = ids.partition_point(|c| *c < id);
    if i < ids.len() && ids[i].range_min() <= id {
        return true;
    }
    i > 0 && ids[i - 1].range_max() >= id
}

/// `S2CellUnion::Intersects(id)` over a normalised union.
pub fn union_intersects(ids: &[CellId], id: CellId) -> bool {
    let i = ids.partition_point(|c| *c < id);
    if i < ids.len() && ids[i].range_min() <= id.range_max() {
        return true;
    }
    i > 0 && ids[i - 1].range_max() >= id.range_min()
}

fn difference_internal(cell: CellId, y: &[CellId], out: &mut Vec<CellId>) {
    if !union_intersects(y, cell) {
        out.push(cell);
    } else if !union_contains(y, cell) {
        let mut child = cell.child_begin();
        for i in 0..4 {
            difference_internal(child, y, out);
            if i == 3 {
                break;
            }
            child = child.next();
        }
    }
}

/// `S2CellUnion::GetDifference(x, y)`.
pub fn cell_union_difference(x: &[CellId], y: &[CellId]) -> Vec<CellId> {
    let mut out = Vec::new();
    for &cell in x {
        difference_internal(cell, y, &mut out);
    }
    out
}
