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

/// `DEFAULT_MIN_GROUP_SIZE`: the rows the full sort mode gathers before it
/// looks for the end of a prefix key group.
const MIN_GROUP_SIZE: usize = 32;

/// `DEFAULT_MAX_FULL_SORT_GROUP_SIZE`: past this many rows of one batch the
/// full sort mode takes it for one large prefix key group.
const MAX_FULL_SORT_GROUP_SIZE: usize = 2 * MIN_GROUP_SIZE;

/// Where `IncrementalSort` is (`IncrementalSortExecutionStatus`:
/// `INCSORT_LOADFULLSORT` and so on).
#[derive(Clone, Copy, PartialEq)]
enum IncStatus {
    LoadFull,
    LoadPrefix,
    ReadFull,
    ReadPrefix,
}

/// PostgreSQL 15's Incremental Sort (`nodeIncrementalSort.c`) over rows
/// already in order by a prefix of the sort keys: rows gathered in batches
/// of at least 32 and sorted by every key (`full`), and a prefix key group
/// that runs past 64 rows moved to a sort by the remaining keys alone
/// (`suffix`), each batch its own `tuplesort` -- bounded, under a LIMIT,
/// by what the Limit still needs. `same_prefix` is `isCurrentGroup`. The
/// rows it returns: all of them, or under a bound at most `bound`.
pub(crate) fn incremental_sort<T: Clone>(
    rows: Vec<T>,
    bound: Option<usize>,
    same_prefix: &mut dyn FnMut(&T, &T) -> bool,
    full: &mut dyn FnMut(&T, &T) -> Ordering,
    suffix: &mut dyn FnMut(&T, &T) -> Ordering,
) -> Vec<T> {
    let mut input = rows.into_iter();
    let mut status = IncStatus::LoadFull;
    let mut outer_done = false;
    let mut bound_done = 0usize;
    let mut group_pivot: Option<T> = None;
    let mut transfer_tuple: Option<T> = None;
    let mut n_fullsort_remaining = 0usize;
    let mut full_out: std::collections::VecDeque<T> = Default::default();
    let mut prefix_out: std::collections::VecDeque<T> = Default::default();
    let mut prefix_sort: Option<TupleSort<T>> = None;
    let mut out = Vec::new();

    // `switchToPresortedPrefixMode`: the first prefix key group of the full
    // sort's remaining rows into a sort by the remaining keys.
    #[allow(clippy::too_many_arguments)]
    fn switch<T: Clone>(
        bound: Option<usize>,
        bound_done: &mut usize,
        n_fullsort_remaining: &mut usize,
        full_out: &mut std::collections::VecDeque<T>,
        transfer_tuple: &mut Option<T>,
        group_pivot: &mut Option<T>,
        prefix_sort: &mut Option<TupleSort<T>>,
        prefix_out: &mut std::collections::VecDeque<T>,
        status: &mut IncStatus,
        same_prefix: &mut dyn FnMut(&T, &T) -> bool,
        suffix: &mut dyn FnMut(&T, &T) -> Ordering,
    ) {
        let mut sort = TupleSort::new(bound.map(|b| b - *bound_done));
        let mut n = 0usize;
        while n < *n_fullsort_remaining {
            if n == 0 && transfer_tuple.is_some() {
                let t = transfer_tuple.clone().expect("checked");
                sort.put(t.clone(), suffix);
                *group_pivot = Some(t);
            } else {
                *transfer_tuple = full_out.pop_front();
                let t = transfer_tuple.as_ref().expect("counted").clone();
                if group_pivot.is_none() {
                    *group_pivot = Some(t.clone());
                }
                if same_prefix(group_pivot.as_ref().expect("set"), &t) {
                    sort.put(t, suffix);
                } else {
                    *group_pivot = None;
                    break;
                }
            }
            n += 1;
        }
        *n_fullsort_remaining -= n;
        if *n_fullsort_remaining == 0 {
            *group_pivot = transfer_tuple.take();
            *prefix_sort = Some(sort);
            *status = IncStatus::LoadPrefix;
        } else {
            *prefix_out = sort.finish(suffix).into();
            if let Some(b) = bound {
                *bound_done = b.min(*bound_done + n);
            }
            *status = IncStatus::ReadPrefix;
        }
    }

    loop {
        if bound.is_some_and(|b| out.len() >= b) {
            return out;
        }
        if matches!(status, IncStatus::ReadFull | IncStatus::ReadPrefix) {
            let q = if status == IncStatus::ReadFull {
                &mut full_out
            } else {
                &mut prefix_out
            };
            if let Some(t) = q.pop_front() {
                out.push(t);
                continue;
            }
            if outer_done {
                return out;
            }
            if n_fullsort_remaining > 0 {
                switch(
                    bound,
                    &mut bound_done,
                    &mut n_fullsort_remaining,
                    &mut full_out,
                    &mut transfer_tuple,
                    &mut group_pivot,
                    &mut prefix_sort,
                    &mut prefix_out,
                    &mut status,
                    same_prefix,
                    suffix,
                );
            } else {
                status = IncStatus::LoadFull;
            }
        }
        // `nTuples`, counted afresh by each call of `ExecIncrementalSort`.
        let mut n_tuples = 0usize;
        if status == IncStatus::LoadFull {
            let (mut sort, min_group) = match bound {
                Some(b) => {
                    let current = b - bound_done;
                    let sort = TupleSort::new((current < MIN_GROUP_SIZE).then_some(current));
                    (sort, MIN_GROUP_SIZE.min(current))
                }
                None => (TupleSort::new(None), MIN_GROUP_SIZE),
            };
            if let Some(p) = group_pivot.take() {
                sort.put(p.clone(), full);
                n_tuples += 1;
                if n_tuples == min_group {
                    group_pivot = Some(p);
                }
            }
            loop {
                let Some(slot) = input.next() else {
                    outer_done = true;
                    full_out = sort.finish(full).into();
                    status = IncStatus::ReadFull;
                    break;
                };
                if n_tuples < min_group {
                    n_tuples += 1;
                    if n_tuples == min_group {
                        group_pivot = Some(slot.clone());
                    }
                    sort.put(slot, full);
                } else if same_prefix(group_pivot.as_ref().expect("pivot"), &slot) {
                    sort.put(slot, full);
                    n_tuples += 1;
                } else {
                    group_pivot = Some(slot);
                    if let Some(b) = bound {
                        bound_done = b.min(bound_done + n_tuples);
                    }
                    full_out = sort.finish(full).into();
                    status = IncStatus::ReadFull;
                    break;
                }
                if n_tuples > MAX_FULL_SORT_GROUP_SIZE {
                    group_pivot = None;
                    full_out = sort.finish(full).into();
                    // A bounded heap keeps only the rows the bound needs
                    // (`tuplesort_used_bound`).
                    n_tuples = n_tuples.min(full_out.len());
                    n_fullsort_remaining = n_tuples;
                    switch(
                        bound,
                        &mut bound_done,
                        &mut n_fullsort_remaining,
                        &mut full_out,
                        &mut transfer_tuple,
                        &mut group_pivot,
                        &mut prefix_sort,
                        &mut prefix_out,
                        &mut status,
                        same_prefix,
                        suffix,
                    );
                    break;
                }
            }
        }
        if status == IncStatus::LoadPrefix {
            let mut sort = prefix_sort.take().expect("switched");
            loop {
                let Some(slot) = input.next() else {
                    outer_done = true;
                    break;
                };
                if same_prefix(group_pivot.as_ref().expect("pivot"), &slot) {
                    sort.put(slot, suffix);
                    n_tuples += 1;
                } else {
                    group_pivot = Some(slot);
                    break;
                }
            }
            prefix_out = sort.finish(suffix).into();
            status = IncStatus::ReadPrefix;
            if let Some(b) = bound {
                bound_done = b.min(bound_done + n_tuples);
            }
        }
        let q = if status == IncStatus::ReadFull {
            &mut full_out
        } else {
            &mut prefix_out
        };
        match q.pop_front() {
            Some(t) => out.push(t),
            None => return out,
        }
    }
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

    /// PostgreSQL 15.19's Incremental Sort over an index scan: `inc_t (i,
    /// a, b)` holding `i, i % m, (i * 7) % 11` for `i` in 1..=n, never
    /// analysed, with an index on `a`; `select i from inc_t order by a, b
    /// limit l` is a Limit over an Incremental Sort (presorted key `a`) over
    /// the index scan, whose rows come in `(a, i)` order. Groups of about
    /// 333 rows go through the presorted prefix mode, groups of 5 and 25
    /// are gathered 32 at a time and sorted by both keys.
    #[test]
    fn incremental_sort_as_postgres_15_leaves_ties() {
        let cases: Vec<(i32, i32, usize, Vec<i32>)> = vec![
            (1000, 3, 7, vec![99, 198, 132, 66, 33, 165, 231]),
            (
                1000,
                3,
                100,
                vec![
                    891, 462, 99, 132, 726, 429, 264, 759, 792, 330, 990, 924, 957, 561, 528, 660,
                    165, 396, 693, 363, 231, 858, 825, 66, 627, 297, 594, 495, 198, 33, 789, 327,
                    360, 723, 393, 63, 756, 657, 294, 822, 954, 558, 525, 162, 987, 921, 30, 426,
                    129, 855, 228, 261, 492, 591, 459, 690, 96, 624, 195, 888, 720, 357, 93, 819,
                    324, 159, 654, 786, 390, 192, 291, 522, 621, 984, 951, 258, 456, 918, 588, 27,
                    225, 126, 687, 852, 885, 489, 555, 60, 423, 753, 354, 321, 816, 90, 552, 288,
                    651, 783, 618, 519,
                ],
            ),
            (
                1000,
                200,
                40,
                vec![
                    800, 200, 1000, 400, 600, 401, 601, 1, 801, 201, 602, 2, 802, 202, 402, 803,
                    203, 403, 603, 3, 404, 604, 4, 804, 204, 605, 5, 805, 205, 405, 206, 406, 606,
                    6, 806, 407, 607, 7, 807, 207,
                ],
            ),
            (
                1000,
                40,
                70,
                vec![
                    880, 440, 800, 360, 720, 280, 200, 640, 560, 1000, 120, 480, 920, 40, 840, 400,
                    320, 760, 680, 240, 160, 600, 520, 80, 960, 561, 121, 921, 481, 41, 401, 841,
                    761, 321, 241, 681, 161, 601, 521, 81, 961, 1, 881, 441, 361, 801, 721, 281,
                    641, 201, 682, 242, 162, 602, 82, 522, 962, 2, 442, 882, 802, 362, 722, 282,
                    642, 202, 122, 562, 922, 42,
                ],
            ),
        ];
        for (n, m, limit, want) in cases {
            let mut rows: Vec<(i32, i32, i32)> =
                (1..=n).map(|i| (i, i % m, (i * 7) % 11)).collect();
            rows.sort_by_key(|r| (r.1, r.0));
            let got: Vec<i32> = incremental_sort(
                rows,
                Some(limit),
                &mut |x, y| x.1 == y.1,
                &mut |x, y| (x.1, x.2).cmp(&(y.1, y.2)),
                &mut |x, y| x.2.cmp(&y.2),
            )
            .into_iter()
            .map(|r| r.0)
            .collect();
            assert_eq!(got, want, "n {n} m {m} limit {limit}");
        }
    }

    #[test]
    fn incremental_sort_orders_every_row() {
        let mut seed = 7u64;
        for (n, groups, bound) in [
            (500usize, 3u32, None),
            (500, 90, None),
            (300, 7, Some(150)),
            (64, 1, None),
            (0, 1, None),
        ] {
            let mut rows: Vec<(u32, u32, usize)> = (0..n)
                .map(|i| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (
                        ((seed >> 33) % u64::from(groups)) as u32,
                        ((seed >> 40) % 5) as u32,
                        i,
                    )
                })
                .collect();
            rows.sort_by_key(|r| r.0);
            let mut want: Vec<(u32, u32)> = rows.iter().map(|r| (r.0, r.1)).collect();
            want.sort();
            if let Some(b) = bound {
                want.truncate(b);
            }
            let got: Vec<(u32, u32)> = incremental_sort(
                rows,
                bound,
                &mut |x, y| x.0 == y.0,
                &mut |x, y| (x.0, x.1).cmp(&(y.0, y.1)),
                &mut |x, y| x.1.cmp(&y.1),
            )
            .into_iter()
            .map(|r| (r.0, r.1))
            .collect();
            assert_eq!(got, want, "{n} {groups} {bound:?}");
        }
    }
}
