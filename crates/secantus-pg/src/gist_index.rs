//! A GiST index's pages, as PostgreSQL 15.19 lays them out (RP-254), so
//! that SERIALIZABLE takes the predicate locks `gistget.c` takes (every
//! page a scan reads) and an insert checks every page it changes
//! (`gistinserttuples`).
//!
//! Ported function by function for the two-dimensional opclasses whose
//! keys are boxes -- `point_ops` (a point is stored as a degenerate box)
//! and `box_ops`: an insert's descent by least penalty (`gistchoose`, with
//! its random choice between equal penalties, `gist_box_penalty`), the
//! downlinks it widens on the way (`gistgetadjusted`), page splits by the
//! double-sorting split (`gist_box_picksplit`, through `gistSplit` and
//! `gistSplitByKey`), the downlinks a split inserts (`gistfinishsplit`), a
//! root split; CREATE INDEX as `point_ops` builds it, sorted by Z-order
//! (`gist_indexsortbuild`), and as `box_ops` does, one insert at a time
//! leaving the fill factor free; and a scan's walk of the tree in its
//! queue's order (`gistgettuple`'s pairing heap).
//!
//! `gistchoose` breaks ties between equally good subtrees with
//! `pg_prng_bool`, which PostgreSQL seeds per backend from a random
//! source. The model draws from the same generator (xoroshiro128**) with a
//! fixed seed: where two subtrees tie, which one PostgreSQL takes is not
//! reproducible even on PostgreSQL itself.

use std::cmp::Ordering;

/// `GIST_ROOT_BLKNO`, `InvalidBlockNumber`.
pub(crate) const ROOT_BLKNO: u32 = 0;
pub(crate) const INVALID: u32 = u32::MAX;
/// `BLCKSZ`, `SizeOfPageHeaderData`, `sizeof(ItemIdData)`.
const BLCKSZ: usize = 8192;
const PAGE_HEADER: usize = 24;
const LP: usize = 4;
/// `MAXALIGN(sizeof(GISTPageOpaqueData))`.
const GIST_OPAQUE: usize = 16;
/// `GiSTPageSize`.
const GIST_PAGE_SIZE: usize = BLCKSZ - PAGE_HEADER - GIST_OPAQUE;
/// A box key's tuple (`IndexTupleData` and a `BOX`), and a NULL key's (a
/// header and its null bitmap).
const BOX_TUPLE: usize = 8 + 32;
const NULL_TUPLE: usize = 16;
/// `LIMIT_RATIO`.
const LIMIT_RATIO: f64 = 0.3;
/// `GIST_SORTED_BUILD_PAGE_NUM`.
const SORTED_BUILD_PAGE_NUM: usize = 4;
/// `EPSILON` of the geometric types' fuzzy comparisons.
const EPSILON: f64 = 1.0e-06;

fn maxalign(n: usize) -> usize {
    (n + 7) & !7
}

/// `FPlt`, `FPle`, `FPgt`, `FPge`, `FPeq`.
fn fp_lt(a: f64, b: f64) -> bool {
    a + EPSILON < b
}
fn fp_le(a: f64, b: f64) -> bool {
    a <= b + EPSILON
}
fn fp_gt(a: f64, b: f64) -> bool {
    a > b + EPSILON
}
fn fp_ge(a: f64, b: f64) -> bool {
    a + EPSILON >= b
}
fn fp_eq(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= EPSILON
}

/// `float8_cmp_internal`: NaN above everything, equal to itself.
fn f8_cmp(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}
fn f8_lt(a: f64, b: f64) -> bool {
    f8_cmp(a, b) == Ordering::Less
}
fn f8_le(a: f64, b: f64) -> bool {
    f8_cmp(a, b) != Ordering::Greater
}
fn f8_gt(a: f64, b: f64) -> bool {
    f8_cmp(a, b) == Ordering::Greater
}
fn f8_ge(a: f64, b: f64) -> bool {
    f8_cmp(a, b) != Ordering::Less
}
fn f8_eq(a: f64, b: f64) -> bool {
    f8_cmp(a, b) == Ordering::Equal
}
fn f8_max(a: f64, b: f64) -> f64 {
    if f8_gt(a, b) {
        a
    } else {
        b
    }
}
fn f8_min(a: f64, b: f64) -> f64 {
    if f8_lt(a, b) {
        a
    } else {
        b
    }
}

/// A `BOX`: its high and low corners.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BoxF {
    pub(crate) hx: f64,
    pub(crate) hy: f64,
    pub(crate) lx: f64,
    pub(crate) ly: f64,
}

impl BoxF {
    /// `gist_point_compress`: a point as the box of itself.
    pub(crate) fn point(x: f64, y: f64) -> BoxF {
        BoxF {
            hx: x,
            hy: y,
            lx: x,
            ly: y,
        }
    }
}

/// `rt_box_union`.
fn rt_box_union(a: &BoxF, b: &BoxF) -> BoxF {
    BoxF {
        hx: f8_max(a.hx, b.hx),
        hy: f8_max(a.hy, b.hy),
        lx: f8_min(a.lx, b.lx),
        ly: f8_min(a.ly, b.ly),
    }
}

/// `size_box`.
fn size_box(b: &BoxF) -> f64 {
    if f8_le(b.hx, b.lx) || f8_le(b.hy, b.ly) {
        return 0.0;
    }
    if b.hx.is_nan() || b.hy.is_nan() {
        return f64::INFINITY;
    }
    (b.hx - b.lx) * (b.hy - b.ly)
}

/// `box_penalty`.
fn box_penalty(orig: &BoxF, new: &BoxF) -> f64 {
    size_box(&rt_box_union(orig, new)) - size_box(orig)
}

/// `adjustBox`.
fn adjust_box(b: &mut BoxF, add: &BoxF) {
    if f8_lt(b.hx, add.hx) {
        b.hx = add.hx;
    }
    if f8_gt(b.lx, add.lx) {
        b.lx = add.lx;
    }
    if f8_lt(b.hy, add.hy) {
        b.hy = add.hy;
    }
    if f8_gt(b.ly, add.ly) {
        b.ly = add.ly;
    }
}

/// `gist_box_same`: exact equality.
fn box_same(a: &BoxF, b: &BoxF) -> bool {
    f8_eq(a.lx, b.lx) && f8_eq(a.ly, b.ly) && f8_eq(a.hx, b.hx) && f8_eq(a.hy, b.hy)
}

/// A key: a box, or NULL.
pub(crate) type Key = Option<BoxF>;

/// `gistpenalty` over `gist_box_penalty` (a strict function): a NULL
/// beside a NULL costs nothing, beside a box everything.
fn penalty(orig: &Key, add: &Key) -> f32 {
    match (orig, add) {
        (Some(o), Some(a)) => {
            let p = box_penalty(o, a) as f32;
            if p.is_nan() || p < 0.0 {
                0.0
            } else {
                p
            }
        }
        (None, None) => 0.0,
        _ => f32::INFINITY,
    }
}

/// `gistMakeUnionKey`.
fn union_key(a: &Key, b: &Key) -> Key {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(*x),
        (Some(x), Some(y)) => {
            let mut u = *x;
            adjust_box(&mut u, y);
            Some(u)
        }
    }
}

/// `gistunion` over a page's keys.
fn union_all<'a>(keys: impl Iterator<Item = &'a Key>) -> Key {
    let mut out: Option<Key> = None;
    for k in keys {
        out = Some(match out {
            None => *k,
            Some(u) => union_key(&u, k),
        });
    }
    out.flatten()
}

/// An index tuple: its key, and the heap TID (a leaf) or child block (an
/// internal page) it points to.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Tuple {
    key: Key,
    ptr: u64,
}

impl Tuple {
    fn size(&self) -> usize {
        if self.key.is_some() {
            BOX_TUPLE
        } else {
            NULL_TUPLE
        }
    }
}

#[derive(Debug, Clone)]
struct Page {
    leaf: bool,
    items: Vec<Tuple>,
    right: u32,
}

impl Page {
    /// `PageGetFreeSpace`.
    fn free(&self) -> usize {
        let used: usize = self.items.iter().map(|t| maxalign(t.size()) + LP).sum();
        (BLCKSZ - GIST_OPAQUE - PAGE_HEADER).saturating_sub(used + LP)
    }
}

/// What a mutation does to SSI, in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Event {
    /// `CheckForSerializableConflictIn` on this page.
    ConflictIn(u32),
    /// `PredicateLockPageSplit` from the first page to the second.
    Split(u32, u32),
}

/// `pg_prng_state`: xoroshiro128**.
#[derive(Debug, Clone)]
struct Prng {
    s0: u64,
    s1: u64,
}

impl Prng {
    /// `pg_prng_seed`: splitmix64 of the seed.
    fn seeded(seed: u64) -> Prng {
        let mut s = seed;
        let mut next = || {
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let s0 = next();
        let s1 = next();
        Prng { s0, s1 }
    }

    fn next(&mut self) -> u64 {
        let s0 = self.s0;
        let sx = self.s1 ^ s0;
        let val = s0.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        self.s0 = s0.rotate_left(24) ^ sx ^ (sx << 16);
        self.s1 = sx.rotate_left(37);
        val
    }

    /// `pg_prng_bool`.
    fn bool(&mut self) -> bool {
        (self.next() >> 63) != 0
    }
}

/// The opclass of the index's one key column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Opclass {
    /// `point_ops`: keys are degenerate boxes; CREATE INDEX sorts.
    Point,
    /// `box_ops`.
    Box,
}

/// A GiST index.
#[derive(Debug, Clone)]
pub(crate) struct Gist {
    pages: Vec<Page>,
    prng: Prng,
}

/// One page of an insert's path from the root (`GISTInsertStack`): the
/// page, the offset of the downlink taken from it, and whether it split
/// under the insert (`retry_from_parent`).
#[derive(Debug, Clone, Copy)]
struct Frame {
    blk: u32,
    off: Option<usize>,
    retry: bool,
}

type Stack = Vec<Frame>;

/// One level of a sorted build (`GistSortedBuildLevelState`).
struct Level {
    pages: Vec<Vec<Tuple>>,
    leaf: bool,
    last_blkno: u32,
}

/// One step of a scan, in the order a plain index scan takes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Step {
    /// The scan reads (and locks) this page.
    Page(u32),
    /// It returns this heap TID.
    Tuple(u64),
}

/// A scan's condition on the key (`gist_point_consistent`,
/// `gist_box_consistent`), by strategy number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Query {
    pub(crate) strategy: u16,
    /// The query value: a point (as its box) or a box.
    pub(crate) arg: BoxF,
    /// The query is a box against a point column (`point <@ box`).
    pub(crate) arg_is_box: bool,
}

impl Gist {
    /// An index of one empty leaf, as CREATE INDEX over an empty table or
    /// TRUNCATE leaves it.
    pub(crate) fn empty() -> Gist {
        Gist {
            pages: vec![Page {
                leaf: true,
                items: Vec::new(),
                right: INVALID,
            }],
            prng: Prng::seeded(0),
        }
    }

    pub(crate) fn pages(&self) -> u32 {
        self.pages.len() as u32
    }

    /// `gistbuild` over `rows` (heap TID and key, in heap order).
    pub(crate) fn build(opclass: Opclass, rows: &[(u64, Key)], fillfactor: u32) -> Gist {
        match opclass {
            Opclass::Point => Self::sorted_build(rows),
            Opclass::Box => {
                let mut g = Gist::empty();
                let freespace = BLCKSZ * (100 - fillfactor as usize) / 100;
                for (tid, key) in rows {
                    let mut ev = Vec::new();
                    g.doinsert(
                        Tuple {
                            key: *key,
                            ptr: *tid,
                        },
                        freespace,
                        &mut ev,
                    );
                }
                g
            }
        }
    }

    /// `gistNewBuffer`: GiST pages are recycled only by VACUUM's page
    /// deletion, which this model does not make, so a new page extends
    /// the relation.
    fn new_buffer(&mut self, leaf: bool) -> u32 {
        self.pages.push(Page {
            leaf,
            items: Vec::new(),
            right: INVALID,
        });
        (self.pages.len() - 1) as u32
    }

    /// `gistinsert`: one heap tuple's key, with no free space held back.
    pub(crate) fn insert(&mut self, tid: u64, key: Key) -> Vec<Event> {
        let mut ev = Vec::new();
        self.doinsert(Tuple { key, ptr: tid }, 0, &mut ev);
        ev
    }

    // ---------------------------------------------------------------
    // Inserts.
    // ---------------------------------------------------------------

    /// `gistchoose`: the downlink of least penalty, ties settled at random.
    fn choose(&mut self, blk: u32, key: &Key) -> usize {
        let items = self.pages[blk as usize].items.clone();
        let mut result = 0;
        let mut best: f32 = -1.0;
        let mut keep_current_best: i32 = -1;
        for (i, t) in items.iter().enumerate() {
            let usize_ = penalty(&t.key, key);
            let mut zero_penalty = usize_ <= 0.0;
            let mut tie = false;
            if best < 0.0 || usize_ < best {
                result = i;
                best = usize_;
                keep_current_best = -1;
            } else if best == usize_ {
                tie = true;
            } else {
                zero_penalty = false;
            }
            if tie && result != i {
                if keep_current_best == -1 {
                    keep_current_best = i32::from(self.prng.bool());
                }
                if keep_current_best == 0 {
                    result = i;
                    keep_current_best = -1;
                }
            }
            if zero_penalty {
                if keep_current_best == -1 {
                    keep_current_best = i32::from(self.prng.bool());
                }
                if keep_current_best == 1 {
                    break;
                }
            }
        }
        result
    }

    /// `gistdoinsert`.
    fn doinsert(&mut self, itup: Tuple, freespace: usize, ev: &mut Vec<Event>) {
        let mut stack: Stack = vec![Frame {
            blk: ROOT_BLKNO,
            off: None,
            retry: false,
        }];
        loop {
            // A page split under us: back to its parent to choose again.
            while stack.len() > 1 && stack.last().is_some_and(|f| f.retry) {
                stack.pop();
            }
            let blk = stack.last().expect("a page").blk;
            if !self.pages[blk as usize].leaf {
                let off = self.choose(blk, &itup.key);
                let down = self.pages[blk as usize].items[off];
                let adjusted = union_key(&down.key, &itup.key);
                let need_new = match (&down.key, &itup.key) {
                    (_, None) => false,
                    (None, Some(_)) => true,
                    (Some(o), Some(_)) => !box_same(o, &adjusted.expect("a union")),
                };
                if need_new {
                    let newtup = Tuple {
                        key: adjusted,
                        ptr: down.ptr,
                    };
                    let level = stack.len() - 1;
                    let split =
                        self.insert_tuples(&mut stack, level, &[newtup], Some(off), freespace, ev);
                    if split {
                        // Re-choose from the parent (the root stays where
                        // it is through its own split).
                        if blk != ROOT_BLKNO {
                            stack.pop();
                        }
                        continue;
                    }
                }
                if let Some(last) = stack.last_mut() {
                    last.off = Some(off);
                }
                let child = self.pages[blk as usize].items[off].ptr as u32;
                stack.push(Frame {
                    blk: child,
                    off: None,
                    retry: false,
                });
            } else {
                let level = stack.len() - 1;
                self.insert_tuples(&mut stack, level, &[itup], None, freespace, ev);
                return;
            }
        }
    }

    /// `gistinserttuples` on the page at `stack[level]`: `tuples` in,
    /// replacing the one at `old` if given, the page split if they do not
    /// fit and the split finished in the parent. Returns whether the page
    /// split.
    fn insert_tuples(
        &mut self,
        stack: &mut Stack,
        level: usize,
        tuples: &[Tuple],
        old: Option<usize>,
        freespace: usize,
        ev: &mut Vec<Event>,
    ) -> bool {
        let blk = stack[level].blk;
        ev.push(Event::ConflictIn(blk));
        let (split, halves) = self.place_to_page(blk, tuples, old, freespace, ev);
        if let Some(halves) = halves {
            self.finish_split(stack, level, halves, freespace, ev);
        }
        split
    }

    /// `gistnospace`.
    fn no_space(page: &Page, tuples: &[Tuple], old: Option<usize>, freespace: usize) -> bool {
        let size: usize = freespace + tuples.iter().map(|t| t.size() + LP).sum::<usize>();
        let deleted = old.map_or(0, |o| page.items[o].size() + LP);
        page.free() + deleted < size
    }

    /// `gistplacetopage`: returns whether it split, and for a split that is
    /// not the root's, each half's downlink (left to right) for the parent.
    fn place_to_page(
        &mut self,
        blk: u32,
        tuples: &[Tuple],
        old: Option<usize>,
        freespace: usize,
        ev: &mut Vec<Event>,
    ) -> (bool, Option<Vec<Tuple>>) {
        let page = &self.pages[blk as usize];
        let is_leaf = page.leaf;
        if !Self::no_space(page, tuples, old, freespace) {
            let page = &mut self.pages[blk as usize];
            match (old, tuples.len()) {
                (Some(o), 1) => page.items[o] = tuples[0],
                (Some(o), _) => {
                    page.items.remove(o);
                    page.items.extend_from_slice(tuples);
                }
                (None, _) => page.items.extend_from_slice(tuples),
            }
            return (false, None);
        }
        let mut itvec = page.items.clone();
        if let Some(o) = old {
            itvec.remove(o);
        }
        itvec.extend_from_slice(tuples);
        let dist = self.split(&itvec);
        let is_root = blk == ROOT_BLKNO;
        let oldrlink = page.right;
        let mut blknos = Vec::with_capacity(dist.len());
        let mut start = 0;
        if !is_root {
            blknos.push(blk);
            start = 1;
        }
        for _ in start..dist.len() {
            let nb = self.new_buffer(is_leaf);
            ev.push(Event::Split(blk, nb));
            blknos.push(nb);
        }
        let mut downlinks = Vec::with_capacity(dist.len());
        for (i, (part, key)) in dist.iter().enumerate() {
            let b = blknos[i];
            let right = if i + 1 < dist.len() {
                blknos[i + 1]
            } else {
                oldrlink
            };
            self.pages[b as usize] = Page {
                leaf: is_leaf,
                items: part.clone(),
                right: if is_root && i + 1 == dist.len() {
                    oldrlink
                } else {
                    right
                },
            };
            downlinks.push(Tuple {
                key: *key,
                ptr: u64::from(b),
            });
        }
        if is_root {
            self.pages[ROOT_BLKNO as usize] = Page {
                leaf: false,
                items: downlinks,
                right: INVALID,
            };
            return (true, None);
        }
        (true, Some(downlinks))
    }

    /// `gistfinishsplit`: the halves' downlinks into the parent, right to
    /// left until two are left, then those two in place of the old one;
    /// the split page is then marked for its inserter to choose again from
    /// the parent.
    fn finish_split(
        &mut self,
        stack: &mut Stack,
        level: usize,
        halves: Vec<Tuple>,
        freespace: usize,
        ev: &mut Vec<Event>,
    ) {
        let child = stack[level].blk;
        let mut level = level;
        for pos in (2..halves.len()).rev() {
            level = self.find_correct_parent(stack, level, child);
            if self.insert_tuples(stack, level - 1, &[halves[pos]], None, freespace, ev) {
                stack[level - 1].off = None;
            }
        }
        level = self.find_correct_parent(stack, level, child);
        let off = stack[level - 1].off;
        self.insert_tuples(stack, level - 1, &halves[..2], off, freespace, ev);
        stack[level - 1].off = None;
        stack[level].retry = true;
    }

    /// `gistFindCorrectParent`: the page holding `child`'s downlink -- the
    /// parent on the stack, a page to its right, or (after a root split)
    /// wherever a search from the root finds it (`gistFindPath`). Returns
    /// the child's level in the stack, which a new path can move.
    fn find_correct_parent(&mut self, stack: &mut Stack, level: usize, child: u32) -> usize {
        let Frame {
            blk: pblk,
            off: poff,
            ..
        } = stack[level - 1];
        if let Some(o) = poff {
            if self.pages[pblk as usize]
                .items
                .get(o)
                .is_some_and(|t| t.ptr == u64::from(child))
            {
                return level;
            }
        }
        let mut b = pblk;
        while b != INVALID {
            if let Some(i) = self.pages[b as usize]
                .items
                .iter()
                .position(|t| t.ptr == u64::from(child))
            {
                stack[level - 1].blk = b;
                stack[level - 1].off = Some(i);
                return level;
            }
            b = self.pages[b as usize].right;
        }
        // `gistFindPath`: breadth first from the root.
        let mut fifo: std::collections::VecDeque<(Stack, u32)> =
            std::collections::VecDeque::from([(Vec::new(), ROOT_BLKNO)]);
        while let Some((path, b)) = fifo.pop_front() {
            let page = &self.pages[b as usize];
            if page.leaf {
                break;
            }
            for (i, t) in page.items.iter().enumerate() {
                let mut p = path.clone();
                p.push(Frame {
                    blk: b,
                    off: Some(i),
                    retry: false,
                });
                if t.ptr == u64::from(child) {
                    let tail = stack.split_off(level);
                    *stack = p;
                    let at = stack.len();
                    stack.extend(tail);
                    return at;
                }
                fifo.push_back((p, t.ptr as u32));
            }
        }
        level
    }

    // ---------------------------------------------------------------
    // Splits.
    // ---------------------------------------------------------------

    /// `gistfitpage`.
    fn fits(tuples: &[Tuple]) -> bool {
        tuples.iter().map(|t| t.size() + LP).sum::<usize>() <= GIST_PAGE_SIZE
    }

    /// `gistSplit`: the tuples in pages that each fit, left to right, with
    /// each page's union key.
    fn split(&self, itvec: &[Tuple]) -> Vec<(Vec<Tuple>, Key)> {
        let (left, right, lkey, rkey) = split_by_key(itvec);
        let lvec: Vec<Tuple> = left.iter().map(|i| itvec[*i]).collect();
        let rvec: Vec<Tuple> = right.iter().map(|i| itvec[*i]).collect();
        let mut out = if Self::fits(&lvec) {
            vec![(lvec, lkey)]
        } else {
            self.split(&lvec)
        };
        if Self::fits(&rvec) {
            out.push((rvec, rkey));
        } else {
            out.extend(self.split(&rvec));
        }
        out
    }

    // ---------------------------------------------------------------
    // The sorted build.
    // ---------------------------------------------------------------

    /// `gist_indexsortbuild`: the tuples sorted by their Z-order then TID
    /// (`gist_bbox_zorder_cmp`, `comparetup_index_btree`), packed into
    /// leaves; each run of four pages is split by `gistSplit` into the
    /// pages written; the levels above built the same way from the
    /// downlinks; the root written last, at block 0.
    fn sorted_build(rows: &[(u64, Key)]) -> Gist {
        let mut sorted: Vec<Tuple> = rows
            .iter()
            .map(|(tid, key)| Tuple {
                key: *key,
                ptr: *tid,
            })
            .collect();
        sorted.sort_by(|a, b| match (&a.key, &b.key) {
            (Some(x), Some(y)) => zorder_cmp(x, y).then(a.ptr.cmp(&b.ptr)),
            (None, None) => a.ptr.cmp(&b.ptr),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
        });
        let mut g = Gist::empty();
        // Block 0 is the root's placeholder.
        let mut allocated: u32 = 1;
        let mut written: Vec<Option<Page>> = vec![None];
        let mut levels: Vec<Level> = vec![Level {
            pages: vec![Vec::new()],
            leaf: true,
            last_blkno: 0,
        }];
        for t in sorted {
            Self::level_add(&mut levels, 0, t, &mut allocated, &mut written);
        }
        let mut lvl = 0;
        while lvl + 1 < levels.len() || levels[lvl].pages.len() > 1 {
            Self::level_flush(&mut levels, lvl, &mut allocated, &mut written);
            lvl += 1;
        }
        let top = &levels[lvl];
        g.pages = written
            .into_iter()
            .map(|p| {
                p.unwrap_or(Page {
                    leaf: true,
                    items: Vec::new(),
                    right: INVALID,
                })
            })
            .collect();
        g.pages[0] = Page {
            leaf: top.leaf,
            items: top.pages[0].clone(),
            right: INVALID,
        };
        g
    }

    /// `gist_indexsortbuild_levelstate_add`.
    fn level_add(
        levels: &mut Vec<Level>,
        lvl: usize,
        t: Tuple,
        allocated: &mut u32,
        written: &mut Vec<Option<Page>>,
    ) {
        let cur = levels[lvl].pages.last().expect("a page");
        let free = Page {
            leaf: true,
            items: cur.clone(),
            right: INVALID,
        }
        .free();
        if free < t.size() + LP {
            if levels[lvl].pages.len() == SORTED_BUILD_PAGE_NUM {
                // The run is split into pages written, and the level starts
                // again on its first page.
                Self::level_flush(levels, lvl, allocated, written);
            } else {
                levels[lvl].pages.push(Vec::new());
            }
        }
        levels[lvl].pages.last_mut().expect("a page").push(t);
    }

    /// `gist_indexsortbuild_levelstate_flush`.
    fn level_flush(
        levels: &mut Vec<Level>,
        lvl: usize,
        allocated: &mut u32,
        written: &mut Vec<Option<Page>>,
    ) {
        let leaf = levels[lvl].leaf;
        let pages = std::mem::take(&mut levels[lvl].pages);
        let dist: Vec<(Vec<Tuple>, Key)> = if pages.len() > 1 {
            let all: Vec<Tuple> = pages.into_iter().flatten().collect();
            Gist::empty().split(&all)
        } else {
            let items = pages.into_iter().next().unwrap_or_default();
            let key = union_all(items.iter().map(|t| &t.key));
            vec![(items, key)]
        };
        for (items, key) in dist {
            let blkno = *allocated;
            *allocated += 1;
            let right = if levels[lvl].last_blkno != 0 {
                levels[lvl].last_blkno
            } else {
                INVALID
            };
            levels[lvl].last_blkno = blkno;
            if written.len() <= blkno as usize {
                written.resize(blkno as usize + 1, None);
            }
            written[blkno as usize] = Some(Page { leaf, items, right });
            if levels.len() == lvl + 1 {
                levels.push(Level {
                    pages: vec![Vec::new()],
                    leaf: false,
                    last_blkno: 0,
                });
            }
            Self::level_add(
                levels,
                lvl + 1,
                Tuple {
                    key,
                    ptr: u64::from(blkno),
                },
                allocated,
                written,
            );
        }
        // The level starts again on a fresh first page.
        if levels[lvl].pages.is_empty() {
            levels[lvl].pages.push(Vec::new());
        }
    }

    // ---------------------------------------------------------------
    // Scans and VACUUM.
    // ---------------------------------------------------------------

    /// A plain scan of `opclass` keys for `quals` (all of which a tuple
    /// must pass): each page read and each heap TID returned, in the order
    /// `gistgettuple` takes them -- a page's matching tuples, then the
    /// next page off the queue (a pairing heap whose equal items come off
    /// in its own order). A bitmap scan reads the same pages.
    pub(crate) fn scan(&self, opclass: Opclass, quals: &[Query]) -> Vec<Step> {
        let mut out = Vec::new();
        let mut heap = PairingHeap::default();
        let mut next = Some(ROOT_BLKNO);
        while let Some(blk) = next {
            out.push(Step::Page(blk));
            let page = &self.pages[blk as usize];
            for t in &page.items {
                let Some(key) = &t.key else {
                    continue;
                };
                if !quals.iter().all(|q| consistent(opclass, q, key, page.leaf)) {
                    continue;
                }
                if page.leaf {
                    out.push(Step::Tuple(t.ptr));
                } else {
                    heap.add(t.ptr as u32);
                }
            }
            next = heap.remove_first();
        }
        out
    }

    /// VACUUM's `gistbulkdelete`: the dead TIDs leave the leaves. (An
    /// emptied leaf is deleted by PostgreSQL; this model keeps it.)
    pub(crate) fn vacuum(&mut self, dead: &dyn Fn(u64) -> bool) {
        for p in &mut self.pages {
            if p.leaf {
                p.items.retain(|t| !dead(t.ptr));
            }
        }
    }

    /// Each page as (leaf, items, rightlink), for tests.
    #[cfg(test)]
    fn shape(&self) -> Vec<(bool, usize, u32)> {
        self.pages
            .iter()
            .map(|p| (p.leaf, p.items.len(), p.right))
            .collect()
    }
}

/// `gist_point_consistent` / `gist_box_consistent` for one key.
fn consistent(opclass: Opclass, q: &Query, key: &BoxF, leaf: bool) -> bool {
    match opclass {
        Opclass::Point => {
            let mut strategy = q.strategy;
            // `RTOldBelowStrategyNumber`, `RTOldAboveStrategyNumber`.
            if strategy == 29 {
                strategy = 10;
            } else if strategy == 30 {
                strategy = 11;
            }
            if strategy / 20 == 1 || q.arg_is_box {
                // `point <@ box`: an exact overlap test.
                let b = &q.arg;
                return key.hx >= b.lx && key.lx <= b.hx && key.hy >= b.ly && key.ly <= b.hy;
            }
            let (px, py) = (q.arg.hx, q.arg.hy);
            match strategy % 20 {
                1 => fp_lt(key.lx, px),
                5 => fp_gt(key.hx, px),
                11 => fp_gt(key.hy, py),
                10 => fp_lt(key.ly, py),
                6 => {
                    if leaf {
                        fp_eq(key.lx, px) && fp_eq(key.ly, py)
                    } else {
                        fp_le(px, key.hx)
                            && fp_ge(px, key.lx)
                            && fp_le(py, key.hy)
                            && fp_ge(py, key.ly)
                    }
                }
                _ => true,
            }
        }
        Opclass::Box => {
            let b = &q.arg;
            let overlap = || {
                fp_le(key.lx, b.hx)
                    && fp_le(b.lx, key.hx)
                    && fp_le(key.ly, b.hy)
                    && fp_le(b.ly, key.hy)
            };
            let contain = |c: &BoxF, d: &BoxF| {
                fp_ge(c.hx, d.hx) && fp_le(c.lx, d.lx) && fp_ge(c.hy, d.hy) && fp_le(c.ly, d.ly)
            };
            let left = fp_lt(key.hx, b.lx);
            let overleft = fp_le(key.hx, b.hx);
            let right = fp_gt(key.lx, b.hx);
            let overright = fp_ge(key.lx, b.lx);
            let below = fp_lt(key.hy, b.ly);
            let overbelow = fp_le(key.hy, b.hy);
            let above = fp_gt(key.ly, b.hy);
            let overabove = fp_ge(key.ly, b.ly);
            if leaf {
                match q.strategy {
                    1 => left,
                    2 => overleft,
                    3 => overlap(),
                    4 => overright,
                    5 => right,
                    6 => {
                        fp_eq(key.hx, b.hx)
                            && fp_eq(key.hy, b.hy)
                            && fp_eq(key.lx, b.lx)
                            && fp_eq(key.ly, b.ly)
                    }
                    7 => contain(key, b),
                    8 => contain(b, key),
                    9 => overbelow,
                    10 => below,
                    11 => above,
                    12 => overabove,
                    _ => true,
                }
            } else {
                match q.strategy {
                    1 => !overright,
                    2 => !right,
                    3 => overlap(),
                    4 => !left,
                    5 => !overleft,
                    6 | 7 => contain(key, b),
                    8 => overlap(),
                    9 => !above,
                    10 => !overabove,
                    11 => !overbelow,
                    12 => !below,
                    _ => true,
                }
            }
        }
    }
}

/// `pairingheap` holding a scan's queue of pages, all of equal priority
/// (a scan with no ORDER BY): `merge` keeps the first argument as the
/// root on a tie, so the order pages come off is the heap's own.
#[derive(Default)]
struct PairingHeap {
    nodes: Vec<PhNode>,
    root: Option<usize>,
}

struct PhNode {
    blk: u32,
    first_child: Option<usize>,
    next_sibling: Option<usize>,
}

impl PairingHeap {
    fn merge(&mut self, a: Option<usize>, b: Option<usize>) -> Option<usize> {
        let (a, b) = match (a, b) {
            (None, b) => return b,
            (a, None) => return a,
            (Some(a), Some(b)) => (a, b),
        };
        // Equal priority: `a` stays the root, `b` its first child.
        self.nodes[b].next_sibling = self.nodes[a].first_child;
        self.nodes[a].first_child = Some(b);
        Some(a)
    }

    fn add(&mut self, blk: u32) {
        self.nodes.push(PhNode {
            blk,
            first_child: None,
            next_sibling: None,
        });
        let n = self.nodes.len() - 1;
        self.root = self.merge(self.root, Some(n));
        if let Some(r) = self.root {
            self.nodes[r].next_sibling = None;
        }
    }

    fn remove_first(&mut self) -> Option<u32> {
        let r = self.root?;
        let children = self.nodes[r].first_child;
        self.root = self.merge_children(children);
        if let Some(nr) = self.root {
            self.nodes[nr].next_sibling = None;
        }
        Some(self.nodes[r].blk)
    }

    /// `merge_children`: pairs left to right, then the pairs right to
    /// left.
    fn merge_children(&mut self, children: Option<usize>) -> Option<usize> {
        let c = children?;
        if self.nodes[c].next_sibling.is_none() {
            return Some(c);
        }
        let mut next = Some(c);
        let mut pairs: Option<usize> = None;
        while let Some(curr) = next {
            let Some(sib) = self.nodes[curr].next_sibling else {
                self.nodes[curr].next_sibling = pairs;
                pairs = Some(curr);
                break;
            };
            next = self.nodes[sib].next_sibling;
            let m = self.merge(Some(curr), Some(sib)).expect("merged");
            self.nodes[m].next_sibling = pairs;
            pairs = Some(m);
        }
        let mut newroot = pairs;
        let mut next = pairs.and_then(|p| self.nodes[p].next_sibling);
        while let Some(curr) = next {
            next = self.nodes[curr].next_sibling;
            newroot = self.merge(newroot, Some(curr));
        }
        newroot
    }
}

/// `ieee_float32_to_uint32`.
fn f32_order(f: f32) -> u32 {
    if f.is_nan() {
        return 0xFFFF_FFFF;
    }
    let i = f.to_bits();
    if i & 0x8000_0000 != 0 {
        i ^ 0xFFFF_FFFF
    } else {
        i | 0x8000_0000
    }
}

/// `part_bits32_by2`.
fn part_bits(x: u32) -> u64 {
    let mut n = u64::from(x);
    n = (n | (n << 16)) & 0x0000_FFFF_0000_FFFF;
    n = (n | (n << 8)) & 0x00FF_00FF_00FF_00FF;
    n = (n | (n << 4)) & 0x0F0F_0F0F_0F0F_0F0F;
    n = (n | (n << 2)) & 0x3333_3333_3333_3333;
    n = (n | (n << 1)) & 0x5555_5555_5555_5555;
    n
}

/// `point_zorder_internal` (the coordinates as `float4`).
fn zorder(x: f64, y: f64) -> u64 {
    part_bits(f32_order(x as f32)) | (part_bits(f32_order(y as f32)) << 1)
}

/// `gist_bbox_zorder_cmp` on the boxes' low corners.
fn zorder_cmp(a: &BoxF, b: &BoxF) -> Ordering {
    if a.lx == b.lx && a.ly == b.ly {
        return Ordering::Equal;
    }
    zorder(a.lx, a.ly).cmp(&zorder(b.lx, b.ly))
}

/// `gistSplitByKey` for a one-column index: the tuples with NULL keys to
/// the right, the rest to the left; if all are NULL, the page in half;
/// otherwise the opclass's picksplit. Returns the (0-based) tuples of each
/// side and the sides' union keys.
fn split_by_key(itvec: &[Tuple]) -> (Vec<usize>, Vec<usize>, Key, Key) {
    let nulls: Vec<usize> = (0..itvec.len())
        .filter(|i| itvec[*i].key.is_none())
        .collect();
    if nulls.len() == itvec.len() {
        // `gistSplitHalf`.
        let len = itvec.len();
        let mut left = Vec::new();
        let mut right = Vec::new();
        for i in 1..=len {
            if i < len / 2 {
                right.push(i - 1);
            } else {
                left.push(i - 1);
            }
        }
        return (left, right, None, None);
    }
    if !nulls.is_empty() {
        let left: Vec<usize> = (0..itvec.len())
            .filter(|i| itvec[*i].key.is_some())
            .collect();
        let lkey = union_all(left.iter().map(|i| &itvec[*i].key));
        return (left, nulls, lkey, None);
    }
    let boxes: Vec<BoxF> = itvec.iter().map(|t| t.key.expect("not null")).collect();
    let (left, right, lbox, rbox) = box_picksplit(&boxes);
    (left, right, Some(lbox), Some(rbox))
}

/// `ConsiderSplitContext`.
struct Context {
    entries: usize,
    bounding: BoxF,
    first: bool,
    left_upper: f64,
    right_lower: f64,
    ratio: f32,
    overlap: f32,
    dim: usize,
    range: f64,
}

/// `non_negative`.
fn non_negative(v: f32) -> f32 {
    if v >= 0.0 {
        v
    } else {
        0.0
    }
}

/// `g_box_consider_split`.
fn consider_split(
    ctx: &mut Context,
    dim: usize,
    right_lower: f64,
    min_left: usize,
    left_upper: f64,
    max_left: usize,
) {
    let n = ctx.entries;
    let left_count = if min_left >= n.div_ceil(2) {
        min_left
    } else if max_left <= n / 2 {
        max_left
    } else {
        n / 2
    };
    let right_count = n - left_count;
    let ratio = left_count.min(right_count) as f32 / n as f32;
    if f64::from(ratio) > LIMIT_RATIO {
        let range = if dim == 0 {
            ctx.bounding.hx - ctx.bounding.lx
        } else {
            ctx.bounding.hy - ctx.bounding.ly
        };
        let overlap = ((left_upper - right_lower) / range) as f32;
        let select = if ctx.first {
            true
        } else if ctx.dim == dim {
            overlap < ctx.overlap || (overlap == ctx.overlap && ratio > ctx.ratio)
        } else {
            non_negative(overlap) < non_negative(ctx.overlap)
                || (range > ctx.range && non_negative(overlap) <= non_negative(ctx.overlap))
        };
        if select {
            ctx.first = false;
            ctx.ratio = ratio;
            ctx.range = range;
            ctx.overlap = overlap;
            ctx.right_lower = right_lower;
            ctx.left_upper = left_upper;
            ctx.dim = dim;
        }
    }
}

/// `gist_box_picksplit`, the double-sorting split: returns the (0-based)
/// entries of each group and the groups' boxes.
fn box_picksplit(boxes: &[BoxF]) -> (Vec<usize>, Vec<usize>, BoxF, BoxF) {
    let n = boxes.len();
    let mut bounding = boxes[0];
    for b in &boxes[1..] {
        adjust_box(&mut bounding, b);
    }
    let mut ctx = Context {
        entries: n,
        bounding,
        first: true,
        left_upper: 0.0,
        right_lower: 0.0,
        ratio: 0.0,
        overlap: 0.0,
        dim: 0,
        range: 0.0,
    };
    for dim in 0..2 {
        let project = |b: &BoxF| if dim == 0 { (b.lx, b.hx) } else { (b.ly, b.hy) };
        let mut by_lower: Vec<(f64, f64)> = boxes.iter().map(project).collect();
        let mut by_upper = by_lower.clone();
        pg_qsort(&mut by_lower, &|a, b| f8_cmp(a.0, b.0));
        pg_qsort(&mut by_upper, &|a, b| f8_cmp(a.1, b.1));
        // Lower bounds of the right group, with the least upper bound of
        // the left.
        let mut i1 = 0usize;
        let mut i2 = 0usize;
        let mut right_lower = by_lower[i1].0;
        let mut left_upper = by_upper[i2].0;
        loop {
            while i1 < n && f8_eq(right_lower, by_lower[i1].0) {
                if f8_lt(left_upper, by_lower[i1].1) {
                    left_upper = by_lower[i1].1;
                }
                i1 += 1;
            }
            if i1 >= n {
                break;
            }
            right_lower = by_lower[i1].0;
            while i2 < n && f8_le(by_upper[i2].1, left_upper) {
                i2 += 1;
            }
            consider_split(&mut ctx, dim, right_lower, i1, left_upper, i2);
        }
        // Upper bounds of the left group, with the greatest lower bound
        // of the right.
        let mut i1 = n as isize - 1;
        let mut i2 = n as isize - 1;
        let mut right_lower = by_lower[i1 as usize].1;
        let mut left_upper = by_upper[i2 as usize].1;
        loop {
            while i2 >= 0 && f8_eq(left_upper, by_upper[i2 as usize].1) {
                if f8_gt(right_lower, by_upper[i2 as usize].0) {
                    right_lower = by_upper[i2 as usize].0;
                }
                i2 -= 1;
            }
            if i2 < 0 {
                break;
            }
            left_upper = by_upper[i2 as usize].1;
            while i1 >= 0 && f8_ge(by_lower[i1 as usize].0, right_lower) {
                i1 -= 1;
            }
            consider_split(
                &mut ctx,
                dim,
                right_lower,
                (i1 + 1) as usize,
                left_upper,
                (i2 + 1) as usize,
            );
        }
    }
    if ctx.first {
        // `fallbackSplit`: the first half left.
        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut lbox: Option<BoxF> = None;
        let mut rbox: Option<BoxF> = None;
        for (i, b) in boxes.iter().enumerate() {
            if i < n / 2 {
                left.push(i);
                match &mut lbox {
                    Some(u) => adjust_box(u, b),
                    None => lbox = Some(*b),
                }
            } else {
                right.push(i);
                match &mut rbox {
                    Some(u) => adjust_box(u, b),
                    None => rbox = Some(*b),
                }
            }
        }
        return (
            left,
            right,
            lbox.unwrap_or(bounding),
            rbox.unwrap_or(bounding),
        );
    }
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut lbox = BoxF::point(0.0, 0.0);
    let mut rbox = BoxF::point(0.0, 0.0);
    let place = |side: &mut Vec<usize>, sbox: &mut BoxF, b: &BoxF, i: usize| {
        if side.is_empty() {
            *sbox = *b;
        } else {
            adjust_box(sbox, b);
        }
        side.push(i);
    };
    let mut common: Vec<(usize, f64)> = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        let (lower, upper) = if ctx.dim == 0 {
            (b.lx, b.hx)
        } else {
            (b.ly, b.hy)
        };
        if f8_le(upper, ctx.left_upper) {
            if f8_ge(lower, ctx.right_lower) {
                common.push((i, 0.0));
            } else {
                place(&mut left, &mut lbox, b, i);
            }
        } else {
            place(&mut right, &mut rbox, b, i);
        }
    }
    if !common.is_empty() {
        let m = (LIMIT_RATIO * n as f64).ceil() as usize;
        for c in common.iter_mut() {
            let b = &boxes[c.0];
            c.1 = (box_penalty(&lbox, b) - box_penalty(&rbox, b)).abs();
        }
        pg_qsort(&mut common, &|a, b| f8_cmp(a.1, b.1));
        let count = common.len();
        for (k, (i, _)) in common.iter().enumerate() {
            let b = &boxes[*i];
            if left.len() + (count - k) <= m {
                place(&mut left, &mut lbox, b, *i);
            } else if right.len() + (count - k) <= m {
                place(&mut right, &mut rbox, b, *i);
            } else if box_penalty(&lbox, b) < box_penalty(&rbox, b) {
                place(&mut left, &mut lbox, b, *i);
            } else {
                place(&mut right, &mut rbox, b, *i);
            }
        }
    }
    (left, right, lbox, rbox)
}

/// `pg_qsort` (`sort_template.h`): PostgreSQL's quicksort, whose order of
/// equal elements is its own.
pub(crate) fn pg_qsort<T: Copy>(a: &mut [T], cmp: &dyn Fn(&T, &T) -> Ordering) {
    let mut lo = 0usize;
    let mut n = a.len();
    loop {
        if n < 7 {
            for pm in lo + 1..lo + n {
                let mut pl = pm;
                while pl > lo && cmp(&a[pl - 1], &a[pl]) == Ordering::Greater {
                    a.swap(pl, pl - 1);
                    pl -= 1;
                }
            }
            return;
        }
        let presorted = (lo + 1..lo + n).all(|pm| cmp(&a[pm - 1], &a[pm]) != Ordering::Greater);
        if presorted {
            return;
        }
        let med3 = |a: &[T], x: usize, y: usize, z: usize| -> usize {
            if cmp(&a[x], &a[y]) == Ordering::Less {
                if cmp(&a[y], &a[z]) == Ordering::Less {
                    y
                } else if cmp(&a[x], &a[z]) == Ordering::Less {
                    z
                } else {
                    x
                }
            } else if cmp(&a[y], &a[z]) == Ordering::Greater {
                y
            } else if cmp(&a[x], &a[z]) == Ordering::Less {
                x
            } else {
                z
            }
        };
        let mut pm = lo + n / 2;
        if n > 7 {
            let mut pl = lo;
            let mut pn = lo + n - 1;
            if n > 40 {
                let d = n / 8;
                pl = med3(a, pl, pl + d, pl + 2 * d);
                pm = med3(a, pm - d, pm, pm + d);
                pn = med3(a, pn - 2 * d, pn - d, pn);
            }
            pm = med3(a, pl, pm, pn);
        }
        a.swap(lo, pm);
        let mut pa = lo + 1;
        let mut pb = lo + 1;
        let mut pc = lo + n - 1;
        let mut pd = lo + n - 1;
        loop {
            while pb <= pc {
                let r = cmp(&a[pb], &a[lo]);
                if r == Ordering::Greater {
                    break;
                }
                if r == Ordering::Equal {
                    a.swap(pa, pb);
                    pa += 1;
                }
                pb += 1;
            }
            while pb <= pc {
                let r = cmp(&a[pc], &a[lo]);
                if r == Ordering::Less {
                    break;
                }
                if r == Ordering::Equal {
                    a.swap(pc, pd);
                    pd -= 1;
                }
                pc -= 1;
            }
            if pb > pc {
                break;
            }
            a.swap(pb, pc);
            pb += 1;
            pc -= 1;
        }
        let pn = lo + n;
        let d1 = (pa - lo).min(pb - pa);
        for i in 0..d1 {
            a.swap(lo + i, pb - d1 + i);
        }
        let d1 = (pd - pc).min(pn - pd - 1);
        for i in 0..d1 {
            a.swap(pb + i, pn - d1 + i);
        }
        let d1 = pb - pa;
        let d2 = pd - pc;
        if d1 <= d2 {
            if d1 > 1 {
                pg_qsort(&mut a[lo..lo + d1], cmp);
            }
            if d2 > 1 {
                lo = pn - d2;
                n = d2;
                continue;
            }
        } else {
            if d2 > 1 {
                pg_qsort(&mut a[pn - d2..pn], cmp);
            }
            if d1 > 1 {
                n = d1;
                continue;
            }
        }
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_points(g: &mut Gist, n: u64) {
        for i in 1..=n {
            let v = (i * 10) as f64;
            let tid = ((i - 1) / 156) << 16 | ((i - 1) % 156 + 1);
            g.insert(tid, Some(BoxF::point(v, v)));
        }
    }

    /// `predicate-gist`'s setup on PostgreSQL 15.19 (pageinspect on a
    /// private cluster): 1,000 points along the diagonal inserted into an
    /// empty index make a root over leaves 1..10, of 93 tuples each but the
    /// last (163), their boxes in order along the diagonal.
    #[test]
    fn inserts_match_predicate_gist() {
        let mut g = Gist::empty();
        spec_points(&mut g, 1000);
        let shape = g.shape();
        assert_eq!(shape.len(), 11);
        assert_eq!(shape[0], (false, 10, INVALID));
        for (b, s) in shape.iter().enumerate().skip(1) {
            let right = if b == 10 { INVALID } else { b as u32 + 1 };
            let n = if b == 10 { 163 } else { 93 };
            assert_eq!(*s, (true, n, right), "block {b}");
        }
        let keys: Vec<(f64, f64)> = g.pages[0]
            .items
            .iter()
            .map(|t| {
                let k = t.key.expect("box");
                (k.lx, k.hx)
            })
            .collect();
        assert_eq!(keys[0], (10.0, 930.0));
        assert_eq!(keys[2], (1870.0, 2790.0));
        assert_eq!(keys[9], (8380.0, 10000.0));
    }

    #[test]
    fn scans_read_the_pages_their_quals_reach() {
        let mut g = Gist::empty();
        spec_points(&mut g, 1000);
        let pages = |strategy: u16, x: f64| -> Vec<u32> {
            let mut p: Vec<u32> = g
                .scan(
                    Opclass::Point,
                    &[Query {
                        strategy,
                        arg: BoxF::point(x, x),
                        arg_is_box: false,
                    }],
                )
                .into_iter()
                .filter_map(|s| match s {
                    Step::Page(b) => Some(b),
                    Step::Tuple(_) => None,
                })
                .collect();
            p.sort_unstable();
            p
        };
        // `p << point(2500, 2500)` and `p >> point(7500, 7500)`.
        assert_eq!(pages(1, 2500.0), vec![0, 1, 2, 3]);
        assert_eq!(pages(5, 7500.0), vec![0, 9, 10]);
        assert_eq!(pages(5, 6000.0), vec![0, 7, 8, 9, 10]);
        assert_eq!(pages(1, 1000.0), vec![0, 1, 2]);
    }

    #[test]
    fn an_insert_checks_the_leaf_and_any_downlink_it_widens() {
        let mut g = Gist::empty();
        spec_points(&mut g, 1000);
        assert_eq!(
            g.insert(7 << 16, Some(BoxF::point(7500.0, 7500.0))),
            vec![Event::ConflictIn(9)]
        );
        // Past the last box: the root's downlink grows first.
        assert_eq!(
            g.insert(7 << 16 | 1, Some(BoxF::point(20000.0, 20000.0))),
            vec![Event::ConflictIn(0), Event::ConflictIn(10)]
        );
    }

    #[test]
    fn the_scan_queue_is_a_pairing_heap() {
        let mut h = PairingHeap::default();
        for b in 1..=4 {
            h.add(b);
        }
        let order: Vec<u32> = std::iter::from_fn(|| h.remove_first()).collect();
        assert_eq!(order, vec![1, 2, 4, 3]);
    }

    #[test]
    fn pg_qsort_sorts() {
        let mut v: Vec<i32> = (0..200).map(|i| (i * 7919) % 211).collect();
        pg_qsort(&mut v, &|a, b| a.cmp(b));
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }

    /// CREATE INDEX over a table already holding 1,000 diagonal points
    /// and 3,000 scattered ones (each three times), as PostgreSQL 15.19
    /// builds it (pageinspect on a private cluster): 30 leaves written in
    /// Z-order in runs of four pages split by picksplit, each linked to
    /// the one before, under a root at block 0.
    #[test]
    fn sorted_build_matches_postgres() {
        let rows: Vec<(u64, Key)> = (0..4000u64)
            .map(|i| {
                let tid = (i / 157) << 16 | (i % 157 + 1);
                let (x, y) = if i < 1000 {
                    ((i + 1) * 10, (i + 1) * 10)
                } else {
                    let g = i - 999;
                    ((g * 37) % 1000, (g * 91) % 1000)
                };
                (tid, Some(BoxF::point(x as f64, y as f64)))
            })
            .collect();
        let g = Gist::build(Opclass::Point, &rows, 90);
        let counts: Vec<usize> = g.pages.iter().map(|p| p.items.len()).collect();
        assert_eq!(
            counts,
            vec![
                30, 99, 88, 183, 183, 130, 57, 108, 111, 106, 115, 144, 156, 111, 111, 108, 135,
                165, 110, 126, 111, 117, 183, 138, 65, 185, 185, 185, 185, 150, 150
            ]
        );
        let rights: Vec<u32> = g.pages.iter().map(|p| p.right).collect();
        let mut want = vec![INVALID, INVALID];
        want.extend(1..30);
        assert_eq!(rights, want);
        let root: Vec<(u64, BoxF)> = g.pages[0]
            .items
            .iter()
            .map(|t| (t.ptr, t.key.expect("box")))
            .collect();
        assert_eq!(
            root[0],
            (
                1,
                BoxF {
                    hx: 120.0,
                    hy: 214.0,
                    lx: 0.0,
                    ly: 0.0
                }
            )
        );
        assert_eq!(
            root[24],
            (
                25,
                BoxF {
                    hx: 1450.0,
                    hy: 1450.0,
                    lx: 777.0,
                    ly: 769.0
                }
            )
        );
        assert_eq!(
            root[29],
            (
                30,
                BoxF {
                    hx: 10000.0,
                    hy: 10000.0,
                    lx: 8510.0,
                    ly: 8510.0
                }
            )
        );
    }

    fn spec_boxes() -> Vec<(u64, Key)> {
        (1..=3000u64)
            .map(|g| {
                let i = g - 1;
                let (x, y) = (((g * 37) % 1000) as f64, ((g * 91) % 1000) as f64);
                let key = BoxF {
                    hx: x + (g % 7) as f64,
                    hy: y + (g % 5) as f64,
                    lx: x,
                    ly: y,
                };
                ((i / 136) << 16 | (i % 136 + 1), Some(key))
            })
            .collect()
    }

    /// The tuples on each page differ from `want` by at most `slack`:
    /// boxes inside two overlapping subtrees, which `gistchoose` places at
    /// random on PostgreSQL itself.
    fn close_to(got: &[usize], want: &[usize], slack: usize) {
        assert_eq!(got.len(), want.len(), "{got:?}");
        assert_eq!(got.iter().sum::<usize>(), want.iter().sum::<usize>());
        let off: usize = got.iter().zip(want).map(|(a, b)| a.abs_diff(*b)).sum();
        assert!(off <= slack, "{got:?} against {want:?}");
    }

    /// `box_ops` on PostgreSQL 15.19 (pageinspect on a private cluster):
    /// CREATE INDEX over 3,000 overlapping boxes (one insert at a time,
    /// 10% of each page left free), and the same boxes inserted into an
    /// empty index. The pages, their splits and the root's downlinks are
    /// PostgreSQL's; a few boxes that lie inside two subtrees go to either,
    /// as `gistchoose`'s random tie-break sends them on PostgreSQL too.
    #[test]
    fn box_builds_match_postgres() {
        let rows = spec_boxes();
        let built = Gist::build(Opclass::Box, &rows, 90);
        let counts: Vec<usize> = built.pages.iter().map(|p| p.items.len()).collect();
        close_to(
            &counts,
            &[
                27, 96, 114, 123, 120, 90, 60, 144, 146, 105, 111, 132, 111, 141, 96, 96, 138, 153,
                105, 108, 87, 108, 81, 108, 111, 93, 100, 123,
            ],
            8,
        );
        let mut g = Gist::empty();
        for (tid, key) in &rows {
            g.insert(*tid, *key);
        }
        let counts: Vec<usize> = g.pages.iter().map(|p| p.items.len()).collect();
        close_to(
            &counts,
            &[
                25, 96, 105, 93, 121, 117, 123, 123, 118, 93, 126, 138, 108, 84, 90, 171, 177, 177,
                147, 118, 141, 102, 78, 129, 117, 108,
            ],
            8,
        );
        let root: Vec<u64> = g.pages[0].items.iter().map(|t| t.ptr).collect();
        assert_eq!(
            root,
            vec![
                5, 4, 6, 8, 15, 2, 16, 9, 17, 12, 18, 10, 19, 3, 20, 7, 21, 11, 22, 13, 23, 14, 24,
                1, 25
            ]
        );
    }

    #[test]
    fn a_sorted_build_packs_leaves_in_zorder() {
        let rows: Vec<(u64, Key)> = (1..=1000u64)
            .map(|i| {
                let v = (i * 10) as f64;
                (
                    ((i - 1) / 156) << 16 | ((i - 1) % 156 + 1),
                    Some(BoxF::point(v, v)),
                )
            })
            .collect();
        let g = Gist::build(Opclass::Point, &rows, 90);
        // Every tuple is in a leaf, and the root is block 0.
        let leaves: usize = g
            .pages
            .iter()
            .filter(|p| p.leaf)
            .map(|p| p.items.len())
            .sum();
        assert_eq!(leaves, 1000);
        assert!(!g.pages[0].leaf);
    }
}
