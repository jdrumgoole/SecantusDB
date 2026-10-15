//! An in-memory sort in the order PostgreSQL 15's `tuplesort.c` leaves rows,
//! ties included.
//!
//! SQL leaves the order of rows that tie under ORDER BY unspecified, but a
//! client sees it. PostgreSQL's in-memory sort is its quicksort
//! (`sort_template.h`, `window_order::pg_qsort`), which is not stable from
//! seven rows up: rows `(1,2),(2,null),(3,1),(4,1),(5,1),(6,3),(7,1),(8,2),
//! (9,1),(10,1)` ordered by the second column come out `7, 10, 3, 4, 5, 9, 1,
//! 8, 6, 2` on PostgreSQL 15.19. Under a LIMIT the Limit node tells the Sort
//! how many rows it needs (`ExecSetTupleBound`, OFFSET plus LIMIT; none for
//! `WITH TIES`), and once more than twice that many have arrived the sort
//! keeps only that many in a heap (`make_bounded_heap`), which leaves ties in
//! yet another order. Both are transcribed here.

use std::cmp::Ordering;

use crate::window_order::pg_qsort;

/// Rows put one at a time, sorted by `finish` -- `tuplesort_puttuple` and
/// `tuplesort_performsort` for a sort that fits in memory.
pub(crate) struct TupleSort<T> {
    tuples: Vec<T>,
    bound: Option<usize>,
    /// The tuples are a heap of the `bound` smallest, largest at the root
    /// (`TSS_BOUNDED`).
    bounded: bool,
}

impl<T> TupleSort<T> {
    /// A sort that needs only the first `bound` rows, when it is `Some`.
    pub(crate) fn new(bound: Option<usize>) -> Self {
        TupleSort {
            tuples: Vec::new(),
            bound,
            bounded: false,
        }
    }

    /// `tuplesort_puttuple_common`.
    pub(crate) fn put(&mut self, t: T, cmp: &mut dyn FnMut(&T, &T) -> Ordering) {
        if self.bounded {
            // The heap's comparison is reversed (`reversedirection`), so a
            // row that is not after the root under it is discarded. (A
            // bound of 0 keeps nothing; PostgreSQL's LIMIT 0 never asks.)
            if self.tuples.is_empty() || cmp(&t, &self.tuples[0]).reverse() != Ordering::Greater {
                return;
            }
            heap_replace_top(&mut self.tuples, t, cmp);
            return;
        }
        self.tuples.push(t);
        if let Some(bound) = self.bound {
            if self.tuples.len() > bound.saturating_mul(2) {
                self.make_bounded_heap(bound, cmp);
            }
        }
    }

    /// `make_bounded_heap`: the rows so far, in arrival order, into a heap
    /// of `bound`.
    fn make_bounded_heap(&mut self, bound: usize, cmp: &mut dyn FnMut(&T, &T) -> Ordering) {
        let all = std::mem::take(&mut self.tuples);
        self.bounded = true;
        for t in all {
            if self.tuples.len() < bound {
                heap_insert(&mut self.tuples, t, cmp);
            } else if bound > 0 && cmp(&t, &self.tuples[0]).reverse() == Ordering::Greater {
                heap_replace_top(&mut self.tuples, t, cmp);
            }
        }
    }

    /// `tuplesort_performsort`: the rows in order -- all of them, or under
    /// a bound at most `bound` of them (the caller takes its OFFSET and
    /// LIMIT from these).
    pub(crate) fn finish(mut self, cmp: &mut dyn FnMut(&T, &T) -> Ordering) -> Vec<T> {
        if !self.bounded {
            pg_qsort(&mut self.tuples, cmp);
            return self.tuples;
        }
        // `sort_bounded_heap`: each delete-top removes the largest row,
        // stored in the slot it frees at the end.
        let mut n = self.tuples.len();
        while n > 1 {
            n -= 1;
            self.tuples.swap(0, n);
            sift_down(&mut self.tuples[..n], cmp);
        }
        self.tuples
    }
}

/// Sort `v` as `TupleSort` does, all rows arriving in their current order.
pub(crate) fn sort<T>(
    v: Vec<T>,
    bound: Option<usize>,
    cmp: &mut dyn FnMut(&T, &T) -> Ordering,
) -> Vec<T> {
    let mut s = TupleSort::new(bound);
    for t in v {
        s.put(t, cmp);
    }
    s.finish(cmp)
}

/// `tuplesort_heap_insert`, under the reversed comparison: sift the new
/// entry up from the end.
fn heap_insert<T>(h: &mut Vec<T>, t: T, cmp: &mut dyn FnMut(&T, &T) -> Ordering) {
    h.push(t);
    let mut j = h.len() - 1;
    while j > 0 {
        let i = (j - 1) / 2;
        if cmp(&h[j], &h[i]).reverse() != Ordering::Less {
            break;
        }
        h.swap(i, j);
        j = i;
    }
}

/// `tuplesort_heap_replace_top`: `t` replaces the root and sinks.
fn heap_replace_top<T>(h: &mut [T], t: T, cmp: &mut dyn FnMut(&T, &T) -> Ordering) {
    h[0] = t;
    sift_down(h, cmp);
}

/// The sift-down of `tuplesort_heap_replace_top`, for the entry at the root:
/// it moves past a child only while it compares after it (a tie stops it),
/// taking the right child only when the left compares after the right.
fn sift_down<T>(h: &mut [T], cmp: &mut dyn FnMut(&T, &T) -> Ordering) {
    let n = h.len();
    let mut i = 0usize;
    loop {
        let mut j = 2 * i + 1;
        if j >= n {
            break;
        }
        if j + 1 < n && cmp(&h[j], &h[j + 1]).reverse() == Ordering::Greater {
            j += 1;
        }
        if cmp(&h[i], &h[j]).reverse() != Ordering::Greater {
            break;
        }
        h.swap(i, j);
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows of the RP-028 measurement: (id, v), v NULL as `None`,
    /// ordered by v ASC NULLS LAST.
    fn rp028() -> Vec<(i32, Option<i32>)> {
        vec![
            (1, Some(2)),
            (2, None),
            (3, Some(1)),
            (4, Some(1)),
            (5, Some(1)),
            (6, Some(3)),
            (7, Some(1)),
            (8, Some(2)),
            (9, Some(1)),
            (10, Some(1)),
        ]
    }

    fn by_v(a: &(i32, Option<i32>), b: &(i32, Option<i32>)) -> Ordering {
        match (a.1, b.1) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(x), Some(y)) => x.cmp(&y),
        }
    }

    fn ids(v: Vec<(i32, Option<i32>)>) -> Vec<i32> {
        v.into_iter().map(|r| r.0).collect()
    }

    #[test]
    fn unbounded_ties_as_postgres_15_leaves_them() {
        let got = ids(sort(rp028(), None, &mut by_v));
        assert_eq!(got, vec![7, 10, 3, 4, 5, 9, 1, 8, 6, 2]);
    }

    #[test]
    fn a_bound_at_least_half_the_rows_is_the_quicksort() {
        // 10 rows, bound 5: not more than twice the bound, so no heap.
        let got = ids(sort(rp028(), Some(5), &mut by_v));
        assert_eq!(&got[..5], &[7, 10, 3, 4, 5]);
    }

    #[test]
    fn a_bounded_heap_sorts_its_rows() {
        let mut seed = 99u64;
        for (n, bound) in [(10usize, 1usize), (10, 4), (100, 3), (1000, 49), (50, 0)] {
            let v: Vec<u32> = (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    ((seed >> 33) % 13) as u32
                })
                .collect();
            let mut want = v.clone();
            want.sort();
            want.truncate(bound);
            let got = sort(v, Some(bound), &mut |a, b| a.cmp(b));
            assert_eq!(got, want, "{n} {bound}");
        }
    }
}
