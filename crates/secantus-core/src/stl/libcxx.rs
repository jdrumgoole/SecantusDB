//! `std::sort` as libc++ implements it (`__algorithm/sort.h`, with the
//! heap sort it falls back on from `partial_sort.h`, `make_heap.h`,
//! `sort_heap.h`, `pop_heap.h`, `push_heap.h` and `sift_down.h`), step for
//! step. libc++ is the library of the macOS build, whose mongod is the
//! reference there (RM-526): macOS mongod 8.2.11 leaves documents whose
//! sort keys tie exactly where this algorithm does.
//!
//! Transcribed from LLVM 21's libc++ (Apple clang 21). The introsort is the
//! one libc++ has used since LLVM 16 (pattern-defeating partitions, Tukey's
//! ninther above 128 elements, insertion sort below 24). Its bitset
//! partition runs only for an arithmetic type under `std::less` /
//! `std::greater`, never for mongod's comparators, so it is not here.
//!
//! `less(a, b)` is the comparator the C++ code passes: a strict weak order.
//! A C++ move is a clone here, and every comparison reads the same values
//! the C++ reads, so the two leave the same order.

/// The insertion-sort limit of `__introsort`.
const LIMIT: usize = 24;
/// Above this length `__introsort` takes Tukey's ninther as the pivot.
const NINTHER_THRESHOLD: usize = 128;

/// `std::__sort3`: orders `v[x]`, `v[y]`, `v[z]`; whether it swapped.
fn sort3<T, F: Fn(&T, &T) -> bool>(v: &mut [T], x: usize, y: usize, z: usize, less: &F) -> bool {
    if !less(&v[y], &v[x]) {
        if !less(&v[z], &v[y]) {
            return false;
        }
        v.swap(y, z);
        if less(&v[y], &v[x]) {
            v.swap(x, y);
        }
        return true;
    }
    if less(&v[z], &v[y]) {
        v.swap(x, z);
        return true;
    }
    v.swap(x, y);
    if less(&v[z], &v[y]) {
        v.swap(y, z);
    }
    true
}

/// `std::__sort4`.
fn sort4<T, F: Fn(&T, &T) -> bool>(v: &mut [T], x: [usize; 4], less: &F) {
    sort3(v, x[0], x[1], x[2], less);
    if less(&v[x[3]], &v[x[2]]) {
        v.swap(x[2], x[3]);
        if less(&v[x[2]], &v[x[1]]) {
            v.swap(x[1], x[2]);
            if less(&v[x[1]], &v[x[0]]) {
                v.swap(x[0], x[1]);
            }
        }
    }
}

/// `std::__sort5`.
fn sort5<T, F: Fn(&T, &T) -> bool>(v: &mut [T], x: [usize; 5], less: &F) {
    sort4(v, [x[0], x[1], x[2], x[3]], less);
    if less(&v[x[4]], &v[x[3]]) {
        v.swap(x[3], x[4]);
        if less(&v[x[3]], &v[x[2]]) {
            v.swap(x[2], x[3]);
            if less(&v[x[2]], &v[x[1]]) {
                v.swap(x[1], x[2]);
                if less(&v[x[1]], &v[x[0]]) {
                    v.swap(x[0], x[1]);
                }
            }
        }
    }
}

/// The cases of five or fewer elements `__introsort` and
/// `__insertion_sort_incomplete` sort by network; `false` when longer.
fn sort_short<T, F: Fn(&T, &T) -> bool>(v: &mut [T], first: usize, last: usize, less: &F) -> bool {
    match last - first {
        0 | 1 => {}
        2 => {
            if less(&v[last - 1], &v[first]) {
                v.swap(first, last - 1);
            }
        }
        3 => {
            sort3(v, first, first + 1, last - 1, less);
        }
        4 => sort4(v, [first, first + 1, first + 2, last - 1], less),
        5 => sort5(v, [first, first + 1, first + 2, first + 3, last - 1], less),
        _ => return false,
    }
    true
}

/// `std::__insertion_sort`: the guarded insertion sort of `v[first..last]`.
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
        if less(&v[i], &v[i - 1]) {
            let t = v[i].clone();
            let mut k = i - 1;
            let mut j = i;
            loop {
                v[j] = v[k].clone();
                j = k;
                if j == first {
                    break;
                }
                k -= 1;
                if !less(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
        }
    }
}

/// `std::__insertion_sort_unguarded`: as [`insertion_sort`], with no bound
/// on the inner loop: `v[first - 1]` is no greater than any element of
/// `v[first..last]`.
fn insertion_sort_unguarded<T: Clone, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    less: &F,
) {
    if first == last {
        return;
    }
    for i in first + 1..last {
        if less(&v[i], &v[i - 1]) {
            let t = v[i].clone();
            let mut k = i - 1;
            let mut j = i;
            loop {
                v[j] = v[k].clone();
                j = k;
                k -= 1;
                if !less(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
        }
    }
}

/// `std::__insertion_sort_incomplete`: insertion-sorts `v[first..last]`
/// until it has moved eight elements; whether the range is sorted.
fn insertion_sort_incomplete<T: Clone, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    less: &F,
) -> bool {
    if sort_short(v, first, last, less) {
        return true;
    }
    let mut j = first + 2;
    sort3(v, first, first + 1, j, less);
    const MOVES: u32 = 8;
    let mut count = 0;
    for i in j + 1..last {
        if less(&v[i], &v[j]) {
            let t = v[i].clone();
            let mut k = j;
            j = i;
            loop {
                v[j] = v[k].clone();
                j = k;
                if j == first {
                    break;
                }
                k -= 1;
                if !less(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
            count += 1;
            if count == MOVES {
                return i + 1 == last;
            }
        }
        j = i;
    }
    true
}

/// `std::__partition_with_equals_on_right`: partitions `v[first..last]`
/// (at least three elements) about `v[first]`, the elements equal to it on
/// its right. The pivot's position, and whether the range was already
/// partitioned.
fn partition_with_equals_on_right<T: Clone, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    less: &F,
) -> (usize, bool) {
    let begin = first;
    let pivot = v[first].clone();
    loop {
        first += 1;
        if !less(&v[first], &pivot) {
            break;
        }
    }
    if begin == first - 1 {
        while first < last {
            last -= 1;
            if less(&v[last], &pivot) {
                break;
            }
        }
    } else {
        loop {
            last -= 1;
            if less(&v[last], &pivot) {
                break;
            }
        }
    }
    let already_partitioned = first >= last;
    while first < last {
        v.swap(first, last);
        loop {
            first += 1;
            if !less(&v[first], &pivot) {
                break;
            }
        }
        loop {
            last -= 1;
            if less(&v[last], &pivot) {
                break;
            }
        }
    }
    let pivot_pos = first - 1;
    if begin != pivot_pos {
        v[begin] = v[pivot_pos].clone();
    }
    v[pivot_pos] = pivot;
    (pivot_pos, already_partitioned)
}

/// `std::__partition_with_equals_on_left`: as on the right, the elements
/// equal to the pivot on its left. Where the right part starts.
fn partition_with_equals_on_left<T: Clone, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    less: &F,
) -> usize {
    let begin = first;
    let pivot = v[first].clone();
    if less(&pivot, &v[last - 1]) {
        loop {
            first += 1;
            if less(&pivot, &v[first]) {
                break;
            }
        }
    } else {
        loop {
            first += 1;
            if !(first < last && !less(&pivot, &v[first])) {
                break;
            }
        }
    }
    if first < last {
        loop {
            last -= 1;
            if !less(&pivot, &v[last]) {
                break;
            }
        }
    }
    while first < last {
        v.swap(first, last);
        loop {
            first += 1;
            if less(&pivot, &v[first]) {
                break;
            }
        }
        loop {
            last -= 1;
            if !less(&pivot, &v[last]) {
                break;
            }
        }
    }
    let pivot_pos = first - 1;
    if begin != pivot_pos {
        v[begin] = v[pivot_pos].clone();
    }
    v[pivot_pos] = pivot;
    first
}

/// `std::__sift_down`: the element at `start` goes down the heap of the
/// first `len` elements of `v`.
fn sift_down<T: Clone, F: Fn(&T, &T) -> bool>(v: &mut [T], len: usize, mut start: usize, less: &F) {
    let mut child = start;
    if len < 2 || (len - 2) / 2 < child {
        return;
    }
    child = 2 * child + 1;
    if child + 1 < len && less(&v[child], &v[child + 1]) {
        child += 1;
    }
    if less(&v[child], &v[start]) {
        return;
    }
    let top = v[start].clone();
    loop {
        v[start] = v[child].clone();
        start = child;
        if (len - 2) / 2 < child {
            break;
        }
        child = 2 * child + 1;
        if child + 1 < len && less(&v[child], &v[child + 1]) {
            child += 1;
        }
        if less(&v[child], &top) {
            break;
        }
    }
    v[start] = top;
}

/// `std::__floyd_sift_down`: the hole at the root goes down to a leaf, each
/// step to the greater child (the left one when they are equal). The leaf.
fn floyd_sift_down<T: Clone, F: Fn(&T, &T) -> bool>(v: &mut [T], len: usize, less: &F) -> usize {
    let mut hole = 0;
    let mut child = 0;
    loop {
        child = 2 * child + 1;
        if child + 1 < len && less(&v[child], &v[child + 1]) {
            child += 1;
        }
        v[hole] = v[child].clone();
        hole = child;
        if child > (len - 2) / 2 {
            return hole;
        }
    }
}

/// `std::__sift_up`: the element before `last` rises in the heap of the
/// `len` elements before it.
fn sift_up<T: Clone, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    mut last: usize,
    mut len: usize,
    less: &F,
) {
    if len > 1 {
        len = (len - 2) / 2;
        let mut ptr = len;
        last -= 1;
        if less(&v[ptr], &v[last]) {
            let t = v[last].clone();
            loop {
                v[last] = v[ptr].clone();
                last = ptr;
                if len == 0 {
                    break;
                }
                len = (len - 1) / 2;
                ptr = len;
                if !less(&v[ptr], &t) {
                    break;
                }
            }
            v[last] = t;
        }
    }
}

/// `std::__pop_heap(first, last, comp, len)`: the root moves to the end of
/// the heap of the first `len` elements.
fn pop_heap<T: Clone, F: Fn(&T, &T) -> bool>(v: &mut [T], len: usize, less: &F) {
    if len > 1 {
        let top = v[0].clone();
        let mut hole = floyd_sift_down(v, len, less);
        let last = len - 1;
        if hole == last {
            v[hole] = top;
        } else {
            v[hole] = v[last].clone();
            hole += 1;
            v[last] = top;
            sift_up(v, hole, hole, less);
        }
    }
}

/// `std::__partial_sort(first, last, last)`: a heap sort, `__introsort`'s
/// fallback when its depth runs out.
pub(super) fn heap_sort<T: Clone, F: Fn(&T, &T) -> bool>(v: &mut [T], less: &F) {
    let n = v.len();
    if n == 0 {
        return;
    }
    if n > 1 {
        for start in (0..=(n - 2) / 2).rev() {
            sift_down(v, n, start, less);
        }
    }
    let mut len = n;
    while len > 1 {
        pop_heap(v, len, less);
        len -= 1;
    }
}

/// `std::__introsort`.
fn introsort<T: Clone, F: Fn(&T, &T) -> bool>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    less: &F,
    mut depth: u32,
    mut leftmost: bool,
) {
    loop {
        let len = last - first;
        if sort_short(v, first, last, less) {
            return;
        }
        if len < LIMIT {
            if leftmost {
                insertion_sort(v, first, last, less);
            } else {
                insertion_sort_unguarded(v, first, last, less);
            }
            return;
        }
        if depth == 0 {
            heap_sort(&mut v[first..last], less);
            return;
        }
        depth -= 1;
        let half = len / 2;
        if len > NINTHER_THRESHOLD {
            sort3(v, first, first + half, last - 1, less);
            sort3(v, first + 1, first + half - 1, last - 2, less);
            sort3(v, first + 2, first + half + 1, last - 3, less);
            sort3(v, first + half - 1, first + half, first + half + 1, less);
            v.swap(first, first + half);
        } else {
            sort3(v, first + half, first, last - 1, less);
        }
        // Not the leftmost part, and the pivot no greater than the element
        // before the range: every element equal to it goes on its left, and
        // that side needs no sorting.
        if !leftmost && !less(&v[first - 1], &v[first]) {
            first = partition_with_equals_on_left(v, first, last, less);
            continue;
        }
        let (i, already_partitioned) = partition_with_equals_on_right(v, first, last, less);
        if already_partitioned {
            let left_sorted = insertion_sort_incomplete(v, first, i, less);
            if insertion_sort_incomplete(v, i + 1, last, less) {
                if left_sorted {
                    return;
                }
                last = i;
                continue;
            } else if left_sorted {
                first = i + 1;
                continue;
            }
        }
        introsort(v, first, i, less, depth, leftmost);
        leftmost = false;
        first = i + 1;
    }
}

/// `std::sort(v.begin(), v.end(), less)`.
pub fn sort<T: Clone, F: Fn(&T, &T) -> bool>(v: &mut [T], less: &F) {
    let n = v.len();
    if n == 0 {
        return;
    }
    // `2 * std::__bit_log2(n)`.
    let depth = 2 * (usize::BITS - 1 - n.leading_zeros());
    introsort(v, 0, n, less, depth, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pseudo-random keys the reference checksums were taken over: a
    /// 64-bit LCG (Knuth's MMIX constants), its top 32 bits modulo the
    /// number of distinct keys.
    fn keys(seed: u64, n: usize, distinct: u64) -> Vec<u64> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (s >> 32) % distinct
            })
            .collect()
    }

    /// FNV-1a over every position of every order, as the C++ harness folds
    /// them.
    fn fold(h: &mut u64, order: &[usize]) {
        for &p in order {
            *h ^= p as u64;
            *h = h.wrapping_mul(0x100000001b3);
        }
    }

    /// The cases the checksums cover: every length 0..=300 and some up to
    /// 5000, each with few distinct keys (many ties), many, and all equal.
    fn cases() -> Vec<(u64, usize, u64)> {
        let mut out = Vec::new();
        let lengths = (0..=300).chain([511, 512, 513, 1000, 1024, 2047, 5000]);
        for (seed, n) in lengths.enumerate() {
            for distinct in [1, 3, 7, 50, 1_000_000] {
                out.push((seed as u64 * 31 + distinct, n, distinct));
            }
        }
        out
    }

    /// The order `std::sort` leaves each case's positions in, sorted by key
    /// alone, folded into one checksum. Reference: Apple clang 21 /
    /// libc++ 21.1 (`_LIBCPP_VERSION` 210106) on macOS, `-O2`, 2026-11-13.
    #[test]
    fn sort_leaves_every_order_libcxx_does() {
        let mut h = 0xcbf29ce484222325;
        for (seed, n, distinct) in cases() {
            let k = keys(seed, n, distinct);
            let mut order: Vec<usize> = (0..n).collect();
            sort(&mut order, &|a: &usize, b: &usize| k[*a] < k[*b]);
            assert!(order.windows(2).all(|w| k[w[0]] <= k[w[1]]));
            fold(&mut h, &order);
        }
        assert_eq!(h, SORT_CHECKSUM);
    }

    /// The heap sort `__introsort` falls back on, against
    /// `std::partial_sort(first, last, last)` over the same cases.
    #[test]
    fn heap_sort_leaves_every_order_libcxx_does() {
        let mut h = 0xcbf29ce484222325;
        for (seed, n, distinct) in cases() {
            let k = keys(seed, n, distinct);
            let mut order: Vec<usize> = (0..n).collect();
            heap_sort(&mut order, &|a: &usize, b: &usize| k[*a] < k[*b]);
            assert!(order.windows(2).all(|w| k[w[0]] <= k[w[1]]));
            fold(&mut h, &order);
        }
        assert_eq!(h, HEAP_SORT_CHECKSUM);
    }

    /// macOS mongod 8.2.11's orders (RM-526): 40 documents with `k: _id %
    /// 3` sorted by `{k: 1}`, and the least key's twenty of RM-312's 200.
    #[test]
    fn sort_leaves_ties_where_macos_mongod_does() {
        let mut v: Vec<usize> = (0..40).collect();
        sort(&mut v, &|a: &usize, b: &usize| a % 3 < b % 3);
        assert_eq!(
            v[..4],
            [39, 3, 27, 9],
            "mongod 8.2.11 on macOS returns 39, 3, 27, 9, ..."
        );
        let key = |i: usize| format!("{}{}", if i.is_multiple_of(2) { "a" } else { "A" }, i % 5);
        let mut v: Vec<usize> = (1..=200).collect();
        sort(&mut v, &|a: &usize, b: &usize| key(*a) < key(*b));
        assert_eq!(v[..5], [25, 105, 95, 115, 5]);
    }

    const SORT_CHECKSUM: u64 = 0x8849_f586_d661_fc71;
    const HEAP_SORT_CHECKSUM: u64 = 0x43dd_694f_f65b_9777;
}
