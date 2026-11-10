//! The C++ standard library's heap and sort algorithms, step for step as
//! libstdc++ implements them (`stl_heap.h`, `stl_algo.h`).
//!
//! mongod's slot-based engine keeps `$topN`, `$bottomN`, `$maxN` and `$minN`
//! in a heap built with `std::push_heap` / `std::pop_heap` and sorts the
//! result with `std::sort`, none of which is stable. Which of several values
//! that compare equal survive, and in what order, is whatever these
//! algorithms do with them. libstdc++ is the library of the Linux build,
//! which is mongod's exemplar (RM-280).
//!
//! mongod's external sorter sorts what it holds with `std::sort` too
//! (`NoLimitSorter::sort`), so the order of documents whose sort keys
//! compare equal is the library's there as well (RM-402).
//!
//! `less(a, b)` is the comparator the C++ code passes: a strict weak order.
//!
//! The heap steps move elements with swaps where libstdc++ moves them into
//! a hole: the hole always holds the value being placed, and every
//! comparison reads it from there, so the two orders are the same.

/// `std::__push_heap`: the value at `hole` rises past each parent less
/// than it, no higher than `top`.
fn push_up<T, F: Fn(&T, &T) -> bool>(v: &mut [T], mut hole: usize, top: usize, less: &F) {
    while hole > top {
        let parent = (hole - 1) / 2;
        if !less(&v[parent], &v[hole]) {
            break;
        }
        v.swap(hole, parent);
        hole = parent;
    }
}

/// `std::__adjust_heap`: the value at `hole` goes down to a leaf, each
/// step to the greater child (the left one when they are equal), then
/// rises as `push_heap` would.
fn adjust<T, F: Fn(&T, &T) -> bool>(v: &mut [T], mut hole: usize, len: usize, less: &F) {
    let top = hole;
    let mut second = hole;
    while len >= 2 && second < (len - 1) / 2 {
        second = 2 * (second + 1);
        if less(&v[second], &v[second - 1]) {
            second -= 1;
        }
        v.swap(hole, second);
        hole = second;
    }
    if len >= 2 && len.is_multiple_of(2) && second == (len - 2) / 2 {
        second = 2 * (second + 1);
        v.swap(hole, second - 1);
        hole = second - 1;
    }
    push_up(v, hole, top, less);
}

/// `std::push_heap(v.begin(), v.end(), less)`: the last element joins the
/// heap in front of it.
pub fn push_heap<T, F: Fn(&T, &T) -> bool>(v: &mut [T], less: &F) {
    let Some(last) = v.len().checked_sub(1) else {
        return;
    };
    push_up(v, last, 0, less);
}

/// `std::__pop_heap(first, last, result)`: the root goes to `result`, and
/// the element that was there is sifted into `v[..last]`.
fn pop_into<T, F: Fn(&T, &T) -> bool>(v: &mut [T], last: usize, result: usize, less: &F) {
    v.swap(0, result);
    adjust(v, 0, last, less);
}

/// `std::pop_heap(v.begin(), v.end(), less)`: the root moves to the end.
pub fn pop_heap<T, F: Fn(&T, &T) -> bool>(v: &mut [T], less: &F) {
    if v.len() > 1 {
        let last = v.len() - 1;
        pop_into(v, last, last, less);
    }
}

fn make_heap<T, F: Fn(&T, &T) -> bool>(v: &mut [T], less: &F) {
    let len = v.len();
    if len < 2 {
        return;
    }
    let mut parent = (len - 2) / 2;
    loop {
        adjust(v, parent, len, less);
        if parent == 0 {
            return;
        }
        parent -= 1;
    }
}

const THRESHOLD: usize = 16;

/// Whether the mongod build this server stands in for is macOS arm64's,
/// whose C++ library is libc++ rather than libstdc++: each platform's
/// mongod is the reference (Joe, 2026-11-07).
pub(crate) const LIBCXX: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

fn median_to_first<T, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    result: usize,
    a: usize,
    b: usize,
    c: usize,
    less: &F,
) {
    let pick = if less(&v[a], &v[b]) {
        if less(&v[b], &v[c]) {
            b
        } else if less(&v[a], &v[c]) {
            c
        } else {
            a
        }
    } else if less(&v[a], &v[c]) {
        a
    } else if less(&v[b], &v[c]) {
        c
    } else {
        b
    };
    v.swap(result, pick);
}

fn partition<T, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    pivot: usize,
    less: &F,
) -> usize {
    loop {
        while less(&v[first], &v[pivot]) {
            first += 1;
        }
        last -= 1;
        while less(&v[pivot], &v[last]) {
            last -= 1;
        }
        if first >= last {
            return first;
        }
        v.swap(first, last);
        first += 1;
    }
}

fn introsort_loop<T: Clone, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    mut last: usize,
    mut depth: u32,
    less: &F,
) {
    while last - first > THRESHOLD {
        if depth == 0 {
            // `std::__partial_sort(first, last, last)`: a heap sort.
            let part = &mut v[first..last];
            make_heap(part, less);
            let mut end = part.len();
            while end > 1 {
                end -= 1;
                pop_into(part, end, end, less);
            }
            return;
        }
        depth -= 1;
        let mid = first + (last - first) / 2;
        median_to_first(v, first, first + 1, mid, last - 1, less);
        let cut = partition(v, first + 1, last, first, less);
        introsort_loop(v, cut, last, depth, less);
        last = cut;
    }
}

fn linear_insert<T: Clone, F: Fn(&T, &T) -> bool>(v: &mut [T], mut last: usize, less: &F) {
    let value = v[last].clone();
    let mut next = last - 1;
    while less(&value, &v[next]) {
        v[last] = v[next].clone();
        last = next;
        next -= 1;
    }
    v[last] = value;
}

fn insertion_sort<T: Clone, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    less: &F,
) {
    if first == last {
        return;
    }
    for i in first + 1..last {
        if less(&v[i], &v[first]) {
            let value = v[i].clone();
            for j in (first..i).rev() {
                v[j + 1] = v[j].clone();
            }
            v[first] = value;
        } else {
            linear_insert(v, i, less);
        }
    }
}

/// `std::sort(v.begin(), v.end(), less)`.
pub fn sort<T: Clone, F: Fn(&T, &T) -> bool>(v: &mut [T], less: &F) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let depth = 2 * (usize::BITS - 1 - n.leading_zeros());
    introsort_loop(v, 0, n, depth, less);
    if n > THRESHOLD {
        insertion_sort(v, 0, THRESHOLD, less);
        for i in THRESHOLD..n {
            linear_insert(v, i, less);
        }
    } else {
        insertion_sort(v, 0, n, less);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_like_a_sort() {
        let mut v: Vec<i32> = (0..200).map(|i| (i * 7919) % 101).collect();
        sort(&mut v, &|a, b| a < b);
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn heap_keeps_its_root() {
        let mut v: Vec<i32> = Vec::new();
        for x in [5, 3, 9, 1, 7, 9, 2] {
            v.push(x);
            push_heap(&mut v, &|a, b| a < b);
        }
        assert_eq!(v[0], 9);
        pop_heap(&mut v, &|a, b| a < b);
        assert_eq!(v.last(), Some(&9));
        assert_eq!(v[0], 9);
    }

    /// Equal keys, told apart by a tag: libstdc++'s insertion sort keeps
    /// equal elements in place.
    #[test]
    fn short_sort_keeps_equal_elements_in_order() {
        let mut v = vec![(1, 'a'), (0, 'b'), (1, 'c'), (0, 'd')];
        sort(&mut v, &|a, b| a.0 < b.0);
        assert_eq!(v, vec![(0, 'b'), (0, 'd'), (1, 'a'), (1, 'c')]);
    }
}
