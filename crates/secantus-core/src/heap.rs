//! `std::priority_queue` as libstdc++ builds it.
//!
//! mongod keeps queues whose order among equal entries reaches the client:
//! the S2 and 2d region coverers' candidates (equal priority), the `$near`
//! result buffer (equal distance) and a SORT_MERGE's heads (equal sort
//! keys). A `std::priority_queue` compares entries and nothing else, so of
//! two equal entries the one that comes out first is whichever the heap's
//! sift steps leave on top, and the C++ libraries sift differently: libc++
//! (the macOS build) and libstdc++ (the Linux build) can hand the same
//! equal entries back in different orders. The Linux build is the exemplar
//! where builds differ (Joe, 2026-10-21), so this is libstdc++'s
//! `push_heap` and `pop_heap` ([`crate::stl`]).

pub trait HeapOrder {
    /// The queue's comparator: `true` when `self` belongs BELOW `other`
    /// (the top is the greatest).
    fn heap_less(&self, other: &Self) -> bool;
}

impl<T> HeapOrder for (i32, T) {
    fn heap_less(&self, other: &Self) -> bool {
        self.0 < other.0
    }
}

pub struct Heap<T: HeapOrder> {
    c: Vec<T>,
}

impl<T: HeapOrder> Default for Heap<T> {
    fn default() -> Self {
        Heap::new()
    }
}

impl<T: HeapOrder> Heap<T> {
    pub fn new() -> Heap<T> {
        Heap { c: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.c.len()
    }

    pub fn is_empty(&self) -> bool {
        self.c.is_empty()
    }

    pub fn top(&self) -> Option<&T> {
        self.c.first()
    }

    /// Every entry, in the heap's own layout, to change in ways that do not
    /// move them (the order the comparator sees must stay as it is).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.c.iter_mut()
    }

    pub fn push(&mut self, value: T) {
        self.c.push(value);
        crate::stl::push_heap(&mut self.c, &T::heap_less);
    }

    pub fn pop(&mut self) -> Option<T> {
        crate::stl::pop_heap(&mut self.c, &T::heap_less);
        self.c.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct D(f64, u32);
    impl HeapOrder for D {
        // `NearStage::SearchResult::operator<`: the nearer is greater.
        fn heap_less(&self, other: &Self) -> bool {
            self.0 > other.0
        }
    }

    #[test]
    fn pops_in_order_of_priority() {
        let mut h = Heap::new();
        for p in [3, 1, 4, 1, 5, 9, 2, 6] {
            h.push((p, ()));
        }
        let out: Vec<i32> = std::iter::from_fn(|| h.pop().map(|(p, _)| p)).collect();
        assert_eq!(out, vec![9, 6, 5, 4, 3, 2, 1, 1]);
    }

    #[test]
    fn equal_entries_leave_in_heap_order() {
        // Four equal distances pushed in order 0..4: libstdc++'s sift
        // steps hand them back 0, 2, 1, 3 (libc++'s: 0, 1, 3, 2).
        let mut h = Heap::new();
        for i in 0..4 {
            h.push(D(1.0, i));
        }
        let out: Vec<u32> = std::iter::from_fn(|| h.pop().map(|d| d.1)).collect();
        assert_eq!(out, vec![0, 2, 1, 3]);
    }
}
