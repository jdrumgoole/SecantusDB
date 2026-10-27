//! `std::priority_queue` as libc++ builds it.
//!
//! mongod keeps two queues whose order among equal entries reaches the
//! client: the S2 region coverer's candidates (equal priority), and the
//! `$near` result buffer (equal distance). A `std::priority_queue` compares
//! entries and nothing else, so of two equal entries the one that comes out
//! first is whichever the heap's sift steps leave on top. This is libc++'s
//! `push_heap` (`__sift_up`) and `pop_heap` (Floyd's sift-down to a leaf,
//! then `__sift_up`), which the reference mongod 8.2.11 on macOS is built
//! with. libstdc++ (a Linux build) sifts differently, and can leave equal
//! entries in another order.

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
        let len = self.c.len();
        sift_up(&mut self.c, len);
    }

    pub fn pop(&mut self) -> Option<T> {
        let len = self.c.len();
        if len == 0 {
            return None;
        }
        if len > 1 {
            // Floyd: move the hole at the top down to a leaf, always to the
            // greater child (the right one only when it is strictly greater).
            let mut hole = 0usize;
            let mut child = 0usize;
            loop {
                let mut child_i = 2 * child + 1;
                child = child_i;
                if child + 1 < len && self.c[child_i].heap_less(&self.c[child_i + 1]) {
                    child_i += 1;
                    child += 1;
                }
                self.c.swap(hole, child_i);
                hole = child_i;
                if child > (len - 2) / 2 {
                    break;
                }
            }
            // The old top now sits at `hole`.
            let last = len - 1;
            if hole != last {
                // The last element fills the hole and sifts up; the old top
                // goes to the end.
                self.c.swap(hole, last);
                sift_up(&mut self.c[..last], hole + 1);
            }
        }
        self.c.pop()
    }
}

/// libc++'s `__sift_up(first, first + len)`: the element at `len - 1`
/// rises while its parent is less than it.
fn sift_up<T: HeapOrder>(c: &mut [T], len: usize) {
    if len <= 1 {
        return;
    }
    let mut last = len - 1;
    let mut parent = (len - 2) / 2;
    if !c[parent].heap_less(&c[last]) {
        return;
    }
    loop {
        c.swap(parent, last);
        last = parent;
        if parent == 0 {
            break;
        }
        parent = (parent - 1) / 2;
        if !c[parent].heap_less(&c[last]) {
            break;
        }
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
        // Four equal distances pushed in order 0..4: libc++'s sift steps
        // hand them back 0, 1, 3, 2.
        let mut h = Heap::new();
        for i in 0..4 {
            h.push(D(1.0, i));
        }
        let out: Vec<u32> = std::iter::from_fn(|| h.pop().map(|d| d.1)).collect();
        assert_eq!(out, vec![0, 1, 3, 2]);
    }
}
