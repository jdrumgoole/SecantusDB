//! A B-tree index's pages, as PostgreSQL 15's nbtree lays them out
//! (RP-196): what CREATE INDEX builds (`nbtsort.c`), how each insert
//! fills a leaf, deduplicates it into posting lists and splits it
//! (`nbtinsert.c`, `nbtdedup.c`, `nbtsplitloc.c`), and what VACUUM's bulk
//! delete leaves (`nbtree.c`). The pages are counted, never the bytes
//! stored: `pg_relation_size`, and the `relpages` CREATE INDEX, VACUUM and
//! ANALYZE record, are the model's page count.
//!
//! The model keeps every index tuple's key and heap TIDs, since where an
//! insert lands and where a page splits depend on them; the key's order
//! and its tuples' sizes come from the index (`Shape`).

use std::cmp::Ordering;

use bson::Bson;

/// `BLCKSZ`.
const PAGE: u32 = 8192;
/// A page's space for line pointers and tuples: the page less its header
/// (24) and nbtree's special space (`BTPageOpaqueData`, 16).
const USABLE: i64 = 8192 - 24 - 16;
/// `sizeof(ItemIdData)`.
const LP: i64 = 4;
/// `MAXALIGN(sizeof(ItemPointerData))`: a pivot's heap TID.
const PIVOT_TID: i64 = 8;
/// `sizeof(ItemPointerData)`: one TID of a posting list.
const TID: i64 = 6;
/// `MAXALIGN(sizeof(IndexTupleData))`: a tuple with no attributes, the
/// first item of an internal page ("minus infinity").
const MINUS_INFINITY: i64 = 8;
/// `BTMaxItemSize / 2`: the largest posting list tuple deduplication
/// makes (`BTDedupState.maxpostingsize`).
const MAX_POSTING: i64 = 1352;
/// CREATE INDEX's limit on a posting list tuple (`_bt_load`): a tenth of a
/// page, MAXALIGNed down, less its line pointer -- the space a leaf filled
/// to fillfactor 90 leaves (RP-298).
const BUILD_MAX_POSTING: i64 = (PAGE as i64 * 10 / 100) / 8 * 8 - LP;
/// `BTREE_NONLEAF_FILLFACTOR`.
const NONLEAF_FILLFACTOR: i64 = 70;
/// `BTREE_SINGLEVAL_FILLFACTOR`.
const SINGLEVAL_FILLFACTOR: f64 = 96.0;
/// `LEAF_SPLIT_DISTANCE`, `INTERNAL_SPLIT_DISTANCE`.
const LEAF_SPLIT_DISTANCE: f64 = 0.050;
const INTERNAL_SPLIT_DISTANCE: f64 = 0.075;

fn maxalign(n: i64) -> i64 {
    (n + 7) & !7
}

/// What the model needs to know of an index.
pub(crate) trait Shape {
    /// Its key columns (`indnkeyatts`); the INCLUDE columns follow them in
    /// a leaf tuple and are never in a pivot.
    fn nkeyatts(&self) -> usize;
    /// Two values of key column `attr`, in the index's order (its
    /// direction and NULLS placement applied).
    fn cmp_attr(&self, attr: usize, a: &Bson, b: &Bson) -> Ordering;
    /// `index_form_tuple`'s MAXALIGNed size for these leading attributes.
    fn tuple_size(&self, attrs: &[Bson]) -> i64;
    /// Does deduplication apply: `deduplicate_items` on and every key
    /// column's opclass an "equal image" one (not `numeric`, say).
    fn dedup(&self) -> bool;
    fn unique(&self) -> bool;
    /// The leaf fillfactor (`fillfactor`, 90 by default).
    fn fillfactor(&self) -> i64 {
        90
    }
}

/// A leaf tuple: its key (and INCLUDE) values and its heap TIDs, more than
/// one for a posting list tuple.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    pub(crate) key: Vec<Bson>,
    pub(crate) tids: Vec<i64>,
    /// MAXALIGNed, its line pointer aside.
    pub(crate) size: i64,
    /// `LP_DEAD`: a unique check found every heap version it points at
    /// dead to all, so the next insert that finds the page full deletes it
    /// (RP-196).
    pub(crate) dead: bool,
}

/// A pivot: a page's high key, or a downlink's separator. `key` holds the
/// attributes truncation kept; `tid` is there only when every key column
/// equals its neighbour's.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pivot {
    pub(crate) key: Vec<Bson>,
    pub(crate) tid: Option<i64>,
    pub(crate) size: i64,
}

impl Pivot {
    fn minus_infinity() -> Self {
        Pivot {
            key: Vec::new(),
            tid: None,
            size: MINUS_INFINITY,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Downlink {
    pub(crate) pivot: Pivot,
    pub(crate) child: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Page {
    Meta,
    Leaf {
        items: Vec<Item>,
        high: Option<Pivot>,
        right: Option<u32>,
    },
    Internal {
        items: Vec<Downlink>,
        high: Option<Pivot>,
        right: Option<u32>,
    },
    /// Deleted by VACUUM; reusable once VACUUM has finished with it.
    Deleted,
}

/// One index's pages, block 0 its metapage.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct BTree {
    pub(crate) pages: Vec<Page>,
    pub(crate) root: Option<u32>,
    /// Deleted pages the free space map offers a split.
    pub(crate) free: Vec<u32>,
    /// Pages the last VACUUM deleted, recycled only by a later index scan
    /// (their safe xid is not yet past every snapshot when it ends).
    pub(crate) pending: Vec<u32>,
    /// Changed since it was last saved.
    pub(crate) dirty: bool,
    /// Every page changed since (a build, a VACUUM); else only `touched`
    /// and the tree's own document are written.
    full: bool,
    touched: std::collections::BTreeSet<u32>,
    /// Each leaf's bytes in use (tuples and line pointers), by block, as
    /// the insert path keeps it; -1 or absent when it must be recounted.
    used: Vec<i64>,
    /// What the inserts since it was last taken told SSI (RP-061): the
    /// leaf each landed on, and each split `(page, its new right half)`.
    pub(crate) ssi_events: Vec<SsiEvent>,
}

/// An insert's report to SSI (`_bt_doinsert`, `_bt_split`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SsiEvent {
    Landed(u32),
    Split(u32, u32),
}

/// An item's key against a TID-bearing search key, PostgreSQL's
/// `_bt_compare`: attributes a truncated pivot lacks are minus infinity.
fn cmp_pivot(shape: &dyn Shape, key: &[Bson], tid: i64, pivot: &Pivot) -> Ordering {
    let n = shape.nkeyatts();
    for (attr, k) in key.iter().enumerate().take(n) {
        let Some(p) = pivot.key.get(attr) else {
            // Truncated away: the search key is greater.
            return Ordering::Greater;
        };
        match shape.cmp_attr(attr, k, p) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    match pivot.tid {
        Some(t) => tid.cmp(&t),
        None => Ordering::Greater,
    }
}

fn cmp_keys(shape: &dyn Shape, a: &[Bson], b: &[Bson]) -> Ordering {
    for attr in 0..shape.nkeyatts() {
        match shape.cmp_attr(attr, &a[attr], &b[attr]) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    Ordering::Equal
}

/// `_bt_keep_natts_fast`: the leading key attributes a pivot between
/// `left` and `right` must keep, `nkeyatts + 1` when they are all equal
/// (a heap TID is needed too).
fn keep_natts(shape: &dyn Shape, left: &[Bson], right: &[Bson]) -> usize {
    let n = shape.nkeyatts();
    let mut keep = 1;
    for attr in 0..n {
        if shape.cmp_attr(attr, &left[attr], &right[attr]) != Ordering::Equal {
            break;
        }
        keep += 1;
    }
    keep
}

/// `_bt_truncate`: the high key between `lastleft` and `firstright`.
fn truncate(shape: &dyn Shape, lastleft: &Item, firstright: &Item) -> Pivot {
    let n = shape.nkeyatts();
    let keep = keep_natts(shape, &lastleft.key, &firstright.key);
    if keep <= n {
        let key = firstright.key[..keep].to_vec();
        let size = shape.tuple_size(&key);
        Pivot {
            key,
            tid: None,
            size,
        }
    } else {
        // Every key attribute is equal: the pivot keeps them all and
        // lastleft's highest heap TID.
        let key = firstright.key[..n].to_vec();
        let size = maxalign(shape.tuple_size(&key)) + PIVOT_TID;
        Pivot {
            key,
            tid: lastleft.tids.last().copied(),
            size,
        }
    }
}

/// A leaf tuple for `key` and `tids`: a plain one for a single TID, else a
/// posting list tuple (`BTreeFormPostingTuple`).
fn item(shape: &dyn Shape, key: Vec<Bson>, tids: Vec<i64>) -> Item {
    let base = shape.tuple_size(&key);
    let size = if tids.len() > 1 {
        maxalign(base + TID * tids.len() as i64)
    } else {
        base
    };
    Item {
        key,
        tids,
        size,
        dead: false,
    }
}

/// `_bt_simpledel_pass`: the LP_DEAD tuples go, and so does every other TID
/// in one of their heap blocks whose HOT chain is dead (`dead`), which the
/// heap checks while it visits those blocks anyway.
fn simple_pass(shape: &dyn Shape, items: &mut Vec<Item>, newtid: i64, dead: &dyn Fn(i64) -> bool) {
    // `_bt_deadblocks`: the new tuple's heap block first, the one block
    // checked that holds no LP_DEAD tuple's version.
    let mut blocks: std::collections::HashSet<i64> = items
        .iter()
        .filter(|i| i.dead)
        .flat_map(|i| i.tids.iter().map(|t| t / 65536))
        .collect();
    blocks.insert(newtid / 65536);
    let mut kept = Vec::with_capacity(items.len());
    for it in std::mem::take(items) {
        if it.dead {
            continue;
        }
        let left: Vec<i64> = it
            .tids
            .iter()
            .copied()
            .filter(|t| !(blocks.contains(&(t / 65536)) && dead(*t)))
            .collect();
        if left.len() == it.tids.len() {
            kept.push(it);
        } else if !left.is_empty() {
            kept.push(item(shape, it.key, left));
        }
    }
    *items = kept;
}

/// `_bt_bottomupdel_pass` and the heap's half of it,
/// `heap_index_delete_tuples` for a bottom-up caller (PostgreSQL 15.19): the
/// leaf's TIDs are offered to the heap, those in runs of equal keys marked
/// promising (a posting list's first or last TID, when its heap block
/// predominates); the heap visits at most six of their blocks, the most
/// promising first, and every TID whose HOT chain is dead (`dead`) goes.
/// True when no deduplication pass should follow: the page now has room
/// for a while.
fn bottomup_pass(
    shape: &dyn Shape,
    items: &mut Vec<Item>,
    high: &Option<Pivot>,
    newitemsz: i64,
    dead: &dyn Fn(i64) -> bool,
) -> bool {
    /// `BOTTOMUP_MAX_NBLOCKS`, `BOTTOMUP_TOLERANCE_NBLOCKS`.
    const MAX_NBLOCKS: usize = 6;
    const TOLERANCE: i64 = 3;
    struct Deltid {
        tid: i64,
        item: usize,
        promising: bool,
        freespace: i64,
    }
    let mut deltids: Vec<Deltid> = Vec::new();
    let mut nintervals = 0;
    let mut start = 0;
    while start < items.len() {
        let mut end = start + 1;
        while end < items.len()
            && cmp_keys(shape, &items[start].key, &items[end].key) == Ordering::Equal
        {
            end += 1;
        }
        let dupinterval = end - start > 1;
        if dupinterval {
            nintervals += 1;
        }
        for (i, it) in items.iter().enumerate().take(end).skip(start) {
            if it.tids.len() == 1 {
                deltids.push(Deltid {
                    tid: it.tids[0],
                    item: i,
                    promising: dupinterval,
                    freespace: it.size + LP,
                });
                continue;
            }
            let n = it.tids.len();
            let (mut first, mut last) = (false, false);
            if dupinterval {
                let block = |t: i64| t / 65536;
                let (min, mid, max) = (
                    block(it.tids[0]),
                    block(it.tids[n / 2]),
                    block(it.tids[n - 1]),
                );
                first = min == mid;
                last = !first && mid == max;
            }
            for (p, t) in it.tids.iter().enumerate() {
                deltids.push(Deltid {
                    tid: *t,
                    item: i,
                    promising: (first && p == 0) || (last && p == n - 1),
                    freespace: TID,
                });
            }
        }
        start = end;
    }
    if nintervals == 0 {
        return false;
    }
    // `index_delete_sort`, then `bottomup_sort_and_shrink`'s block groups.
    deltids.sort_by_key(|d| d.tid);
    struct Group {
        first: usize,
        ntids: usize,
        promising: u32,
    }
    let mut groups: Vec<Group> = Vec::new();
    for (i, d) in deltids.iter().enumerate() {
        match groups.last_mut() {
            Some(g) if deltids[g.first].tid / 65536 == d.tid / 65536 => {
                g.ntids += 1;
                g.promising += u32::from(d.promising);
            }
            _ => groups.push(Group {
                first: i,
                ntids: 1,
                promising: u32::from(d.promising),
            }),
        }
    }
    for g in &mut groups {
        g.promising = if g.promising <= 4 {
            4
        } else {
            g.promising.next_power_of_two()
        };
    }
    groups.sort_by(|a, b| {
        b.promising
            .cmp(&a.promising)
            .then_with(|| {
                if a.ntids == b.ntids {
                    Ordering::Equal
                } else {
                    (b.ntids as u32)
                        .next_power_of_two()
                        .cmp(&(a.ntids as u32).next_power_of_two())
                }
            })
            .then(a.first.cmp(&b.first))
    });
    groups.truncate(MAX_NBLOCKS);
    // `bottomup_nblocksfavorable`: the leading blocks near one another.
    let mut favorable = 0usize;
    let mut lastblock = -1i64;
    for g in &groups {
        let block = deltids[g.first].tid / 65536;
        if lastblock != -1 && (block < lastblock - TOLERANCE || block > lastblock + TOLERANCE) {
            break;
        }
        favorable += 1;
        lastblock = block;
    }
    let mut target = (8192 / 16).max(newitemsz);
    let (mut actual, mut last) = (0i64, 0i64);
    let mut final_block = false;
    let mut deletable: Vec<(usize, i64)> = Vec::new();
    for (accessed, g) in groups.iter().enumerate() {
        if final_block {
            break;
        }
        if accessed >= 1 && actual == last {
            break;
        }
        last = actual;
        if favorable > 0 {
            favorable -= 1;
        } else {
            target /= 2;
        }
        for d in &deltids[g.first..g.first + g.ntids] {
            if dead(d.tid) {
                deletable.push((d.item, d.tid));
                actual += d.freespace;
                if actual >= target {
                    final_block = true;
                }
            }
        }
    }
    if !deletable.is_empty() {
        // `_bt_delitems_delete`: a plain tuple goes, a posting list loses
        // its dead TIDs (and goes with the last).
        let mut gone: std::collections::HashMap<usize, Vec<i64>> = Default::default();
        for (i, t) in deletable {
            gone.entry(i).or_default().push(t);
        }
        let mut kept = Vec::with_capacity(items.len());
        for (i, it) in std::mem::take(items).into_iter().enumerate() {
            match gone.get(&i) {
                None => kept.push(it),
                Some(ts) => {
                    let left: Vec<i64> = it
                        .tids
                        .iter()
                        .copied()
                        .filter(|t| !ts.contains(t))
                        .collect();
                    if !left.is_empty() {
                        kept.push(item(shape, it.key, left));
                    }
                }
            }
        }
        *items = kept;
    }
    // `PageGetExactFreeSpace >= Max(BLCKSZ / 24, newitemsz)`.
    let exact = leaf_free(items, high) + LP;
    exact >= (8192 / 24).max(newitemsz)
}

fn leaf_free(items: &[Item], high: &Option<Pivot>) -> i64 {
    // `PageGetFreeSpace`: what is left, less one more line pointer.
    USABLE
        - items.iter().map(|i| i.size + LP).sum::<i64>()
        - high.as_ref().map_or(0, |h| h.size + LP)
        - LP
}

fn internal_free(items: &[Downlink], high: &Option<Pivot>) -> i64 {
    USABLE
        - items.iter().map(|d| d.pivot.size + LP).sum::<i64>()
        - high.as_ref().map_or(0, |h| h.size + LP)
        - LP
}

/// One tuple of a page being split, as `_bt_findsplitloc` sees it.
#[derive(Clone)]
struct Tuple<'a> {
    key: &'a [Bson],
    /// MAXALIGNed size plus its line pointer.
    size: i64,
    /// A posting list's TIDs' bytes, which truncation removes.
    posting: i64,
}

#[derive(Clone, Copy, Debug)]
struct Split {
    /// Tuples on the left page (`firstright` is tuple `left`).
    left: usize,
    leftfree: i64,
    rightfree: i64,
    delta: i16,
}

/// `_bt_findsplitloc` over `tuples` (the page's data tuples with the new
/// one at `new_at`): how many go to the left page.
fn find_split(
    shape: &dyn Shape,
    tuples: &[Tuple<'_>],
    new_at: usize,
    leaf: bool,
    rightmost: bool,
    high_size: Option<i64>,
    after_ok: bool,
) -> usize {
    let leftspace = USABLE;
    let rightspace = USABLE - high_size.map_or(0, |h| h + LP);
    let total: i64 = tuples
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != new_at)
        .map(|(_, t)| t.size)
        .sum();
    let newsz = tuples[new_at].size;
    let mut splits: Vec<Split> = Vec::new();
    let mut toleft = 0i64; // old data items to the left
    for p in 1..tuples.len() {
        let prev = p - 1;
        if prev != new_at {
            toleft += tuples[prev].size;
        }
        let first = &tuples[p];
        let new_on_left = new_at < p;
        let mut leftfree = leftspace - toleft;
        let mut rightfree = rightspace - (total - toleft);
        let firstsz = first.size;
        let posting = if leaf && p != new_at && firstsz > 64 {
            first.posting
        } else {
            0
        };
        if leaf {
            leftfree -= firstsz + PIVOT_TID - posting;
        } else {
            leftfree -= firstsz;
        }
        if new_on_left {
            leftfree -= newsz;
        } else {
            rightfree -= newsz;
        }
        if !leaf {
            rightfree += firstsz - (MINUS_INFINITY + LP);
        }
        if leftfree >= 0 && rightfree >= 0 {
            splits.push(Split {
                left: p,
                leftfree,
                rightfree,
                delta: 0,
            });
        }
    }
    if splits.is_empty() {
        return tuples.len() / 2;
    }
    let ff = shape.fillfactor();
    let (mut usemult, mut mult) = if !leaf {
        (
            rightmost,
            if rightmost {
                NONLEAF_FILLFACTOR as f64 / 100.0
            } else {
                0.5
            },
        )
    } else if rightmost {
        (true, ff as f64 / 100.0)
    } else if let Some(after) = after_ok
        .then(|| after_new_item(shape, tuples, new_at, ff, high_size.is_some()))
        .flatten()
    {
        match after {
            AfterNew::Fillfactor => (true, ff as f64 / 100.0),
            AfterNew::Exact => {
                if let Some(s) = splits.iter().find(|s| s.left == new_at + 1) {
                    return s.left;
                }
                (false, 0.5)
            }
        }
    } else {
        (false, 0.5)
    };
    let leftmost_split = splits[0];
    let rightmost_split = splits[splits.len() - 1];
    delta_sort(&mut splits, mult, usemult);
    let tolerance = (total as f64
        * if leaf {
            LEAF_SPLIT_DISTANCE
        } else {
            INTERNAL_SPLIT_DISTANCE
        }) as i64;
    let mut interval = default_interval(&splits, tolerance);
    let lastleft = |s: &Split| tuples[s.left - 1].key;
    let firstright = |s: &Split| tuples[s.left].key;
    let n = shape.nkeyatts();
    // `_bt_strategy`.
    let mut strategy = Strategy::Default;
    let perfect: i64 = if !leaf {
        splits
            .iter()
            .map(|s| tuples[s.left].size)
            .min()
            .unwrap_or(0)
    } else {
        let (lo, hi) = interval_edges(&splits[..interval]);
        let p = keep_natts(shape, lastleft(&lo), firstright(&hi)) as i64;
        if p <= n as i64 {
            p
        } else {
            let p = keep_natts(
                shape,
                lastleft(&leftmost_split),
                firstright(&rightmost_split),
            );
            if p <= n {
                strategy = Strategy::ManyDuplicates;
                n as i64
            } else {
                strategy = Strategy::SingleValue;
                p as i64
            }
        }
    };
    // A single-value split that is not the rightmost of its value's pages
    // falls back to the default strategy (`_bt_strategy`'s high key test).
    if strategy == Strategy::SingleValue && !rightmost {
        let new_key = tuples[new_at].key;
        let high_equal = high_size.is_some() && {
            // The high key's attributes are the new item's when the page
            // holds only that value; the caller passed the high key's size
            // only, so compare with the page's last tuple instead.
            let last = tuples[tuples.len() - 1].key;
            keep_natts(shape, last, new_key) > n
        };
        if !high_equal {
            strategy = Strategy::Default;
        }
    }
    match strategy {
        Strategy::Default => {}
        Strategy::ManyDuplicates => interval = splits.len(),
        Strategy::SingleValue => {
            usemult = true;
            mult = SINGLEVAL_FILLFACTOR / 100.0;
            delta_sort(&mut splits, mult, usemult);
            interval = 1;
        }
    }
    // `_bt_bestsplitloc`.
    let mut best = i64::MAX;
    let mut at = 0;
    for (i, s) in splits.iter().enumerate().take(interval.min(splits.len())) {
        let penalty = if leaf {
            keep_natts(shape, lastleft(s), firstright(s)) as i64
        } else {
            tuples[s.left].size
        };
        if penalty < best {
            best = penalty;
            at = i;
        }
        if penalty <= perfect {
            break;
        }
    }
    let mut chosen = splits[at];
    let new_on_left = new_at < chosen.left;
    if strategy == Strategy::ManyDuplicates
        && !rightmost
        && !new_on_left
        && chosen.left >= new_at
        && chosen.left < new_at + 9
    {
        chosen = splits[0];
    }
    chosen.left
}

#[derive(PartialEq, Clone, Copy)]
enum Strategy {
    Default,
    ManyDuplicates,
    SingleValue,
}

enum AfterNew {
    Fillfactor,
    Exact,
}

/// `_bt_afternewitemoff`: a new item inserted at the end of a localized
/// ascending run of a composite index.
fn after_new_item(
    shape: &dyn Shape,
    tuples: &[Tuple<'_>],
    new_at: usize,
    ff: i64,
    has_high: bool,
) -> Option<AfterNew> {
    let n = shape.nkeyatts();
    if n == 1 || new_at == 0 {
        return None;
    }
    let newsz = tuples[new_at].size;
    if tuples
        .iter()
        .enumerate()
        .any(|(i, t)| i != new_at && t.size != newsz)
    {
        return None;
    }
    if newsz > maxalign(8 + 16) + LP {
        return None;
    }
    let keep = keep_natts(shape, tuples[new_at - 1].key, tuples[new_at].key);
    if !(keep > 1 && keep <= n) {
        return None;
    }
    if new_at == tuples.len() - 1 {
        return Some(AfterNew::Fillfactor);
    }
    // The mid-page case needs the heap TIDs to be adjacent; the caller
    // checked that before offering it (see `insert`). Its interpolation is
    // over the page's offsets, the high key's among them.
    let h = usize::from(has_high);
    let maxoff = (tuples.len() - 1 + h) as f64;
    let newitemoff = (new_at + 1 + h) as f64;
    let interp = newitemoff / (maxoff + 1.0);
    if interp > ff as f64 / 100.0 {
        Some(AfterNew::Fillfactor)
    } else {
        Some(AfterNew::Exact)
    }
}

fn delta_sort(splits: &mut [Split], mult: f64, usemult: bool) {
    for s in splits.iter_mut() {
        let d = if usemult {
            mult * s.leftfree as f64 - (1.0 - mult) * s.rightfree as f64
        } else {
            (s.leftfree - s.rightfree) as f64
        };
        s.delta = d.abs() as i16;
    }
    crate::window_order::pg_qsort(splits, &mut |a: &Split, b: &Split| a.delta.cmp(&b.delta));
}

fn default_interval(splits: &[Split], tolerance: i64) -> usize {
    let best = splits[0];
    for (i, s) in splits.iter().enumerate().skip(1) {
        if s.leftfree < best.leftfree - tolerance
            || s.rightfree < best.rightfree - tolerance
            || s.leftfree > best.leftfree + tolerance
            || s.rightfree > best.rightfree + tolerance
        {
            return i;
        }
    }
    splits.len()
}

/// `_bt_interval_edges`: the leftmost and rightmost splits of an interval.
fn interval_edges(splits: &[Split]) -> (Split, Split) {
    let mut lo = splits[0];
    let mut hi = splits[0];
    for s in splits {
        if s.left < lo.left {
            lo = *s;
        }
        if s.left > hi.left {
            hi = *s;
        }
    }
    (lo, hi)
}

impl BTree {
    /// The leaf pages an index scan reads (`_bt_first`, `_bt_readpage`,
    /// `_bt_readnextpage`), each of which it predicate-locks: from where
    /// the lower bound on the leading column puts it (the leftmost leaf
    /// without one), rightward until an item or a page's high key is past
    /// the upper bound. `None` for an empty index, which a scan locks
    /// whole.
    pub(crate) fn scan_pages(
        &self,
        shape: &dyn Shape,
        lower: Option<(&Bson, bool)>,
        upper: Option<(&Bson, bool)>,
    ) -> Option<Vec<u32>> {
        let root = self.root?;
        let past = |k: &Bson| match upper {
            Some((u, inclusive)) => match shape.cmp_attr(0, k, u) {
                Ordering::Greater => true,
                Ordering::Equal => !inclusive,
                Ordering::Less => false,
            },
            None => false,
        };
        let (mut at, start_key) = match lower {
            Some((k, inclusive)) => {
                let tid = if inclusive { i64::MIN } else { i64::MAX };
                (
                    self.descend(shape, std::slice::from_ref(k), tid).0,
                    Some((k, inclusive)),
                )
            }
            None => {
                let mut at = root;
                while let Page::Internal { items, .. } = &self.pages[at as usize] {
                    at = items[0].child;
                }
                (at, None)
            }
        };
        let mut pages = Vec::new();
        let mut first = true;
        loop {
            match &self.pages[at as usize] {
                Page::Leaf { items, high, right } => {
                    pages.push(at);
                    let mut stop = false;
                    for it in items {
                        let Some(k) = it.key.first() else { continue };
                        if first {
                            // `_bt_binsrch`: the scan starts at the first
                            // item past the lower bound.
                            if let Some((lk, inclusive)) = start_key {
                                match shape.cmp_attr(0, k, lk) {
                                    Ordering::Less => continue,
                                    Ordering::Equal if !inclusive => continue,
                                    _ => {}
                                }
                            }
                        }
                        if past(k) {
                            stop = true;
                            break;
                        }
                    }
                    first = false;
                    if stop {
                        return Some(pages);
                    }
                    let Some(next) = right else {
                        return Some(pages);
                    };
                    // The high key says whether the next page can hold more.
                    if let Some(h) = high.as_ref().and_then(|h| h.key.first()) {
                        if past(h) {
                            return Some(pages);
                        }
                    }
                    at = *next;
                }
                Page::Deleted => {
                    // A deleted page passes its scan right; it has no link
                    // here, so the scan ends.
                    return Some(pages);
                }
                _ => return Some(pages),
            }
        }
    }

    /// `_bt_killitems`: an index scan between `lower` and `upper` on the
    /// leading column marks LP_DEAD each item it returned whose every heap
    /// version it found dead to all (`kill_prior_tuple`; a posting list
    /// only when each of its TIDs is). Whether any item was marked.
    pub(crate) fn kill_items(
        &mut self,
        shape: &dyn Shape,
        lower: Option<(&Bson, bool)>,
        upper: Option<(&Bson, bool)>,
        dead: &dyn Fn(i64) -> bool,
    ) -> bool {
        let Some(pages) = self.scan_pages(shape, lower, upper) else {
            return false;
        };
        let within = |k: &Bson| {
            let above = lower.is_none_or(|(l, inclusive)| match shape.cmp_attr(0, k, l) {
                Ordering::Greater => true,
                Ordering::Equal => inclusive,
                Ordering::Less => false,
            });
            let below = upper.is_none_or(|(u, inclusive)| match shape.cmp_attr(0, k, u) {
                Ordering::Less => true,
                Ordering::Equal => inclusive,
                Ordering::Greater => false,
            });
            above && below
        };
        let mut any = false;
        for p in pages {
            let Page::Leaf { items, .. } = &mut self.pages[p as usize] else {
                continue;
            };
            let mut marked = false;
            for it in items.iter_mut() {
                if it.dead || it.tids.is_empty() || !it.key.first().is_some_and(within) {
                    continue;
                }
                if it.tids.iter().all(|t| dead(*t)) {
                    it.dead = true;
                    marked = true;
                }
            }
            if marked {
                self.touched.insert(p);
                any = true;
            }
        }
        any
    }

    /// The heap TIDs of the entries between `lower` and `upper` not yet
    /// marked dead: what an index scan of that range passes (RP-196).
    pub(crate) fn tids_in(
        &self,
        shape: &dyn Shape,
        lower: Option<(&Bson, bool)>,
        upper: Option<(&Bson, bool)>,
    ) -> Vec<i64> {
        let Some(pages) = self.scan_pages(shape, lower, upper) else {
            return Vec::new();
        };
        let within = |k: &Bson| {
            let above = lower.is_none_or(|(l, inclusive)| match shape.cmp_attr(0, k, l) {
                Ordering::Greater => true,
                Ordering::Equal => inclusive,
                Ordering::Less => false,
            });
            let below = upper.is_none_or(|(u, inclusive)| match shape.cmp_attr(0, k, u) {
                Ordering::Less => true,
                Ordering::Equal => inclusive,
                Ordering::Greater => false,
            });
            above && below
        };
        let mut out = Vec::new();
        for p in pages {
            let Page::Leaf { items, .. } = &self.pages[p as usize] else {
                continue;
            };
            for it in items {
                if !it.dead && it.key.first().is_some_and(within) {
                    out.extend(it.tids.iter().copied());
                }
            }
        }
        out
    }

    /// The pages it occupies, the metapage and deleted pages among them.
    pub(crate) fn pages(&self) -> u32 {
        self.pages.len().max(1) as u32
    }

    /// Its heap TIDs, what VACUUM records as `reltuples`.
    pub(crate) fn tuples(&self) -> u64 {
        self.pages
            .iter()
            .map(|p| match p {
                Page::Leaf { items, .. } => items.iter().map(|i| i.tids.len() as u64).sum(),
                _ => 0,
            })
            .sum()
    }

    /// The root's level (0 for a leaf root; 0 with no root).
    pub(crate) fn height(&self) -> u32 {
        let mut level = 0;
        let mut at = match self.root {
            Some(r) => r,
            None => return 0,
        };
        while let Page::Internal { items, .. } = &self.pages[at as usize] {
            level += 1;
            at = items[0].child;
        }
        level
    }

    /// CREATE INDEX over `entries`, `(key, tid)` each (`_bt_load`): sorted,
    /// deduplicated when that applies, and loaded into pages filled to the
    /// fillfactor bottom up.
    pub(crate) fn build(shape: &dyn Shape, mut entries: Vec<(Vec<Bson>, i64)>) -> BTree {
        entries.sort_by(|a, b| cmp_keys(shape, &a.0, &b.0).then(a.1.cmp(&b.1)));
        let mut tree = BTree {
            pages: vec![Page::Meta],
            root: None,
            free: Vec::new(),
            pending: Vec::new(),
            dirty: true,
            full: true,
            touched: Default::default(),
            used: Vec::new(),
            ssi_events: Vec::new(),
        };
        if entries.is_empty() {
            return tree;
        }
        let dedup = shape.dedup() && !shape.unique();
        // Merge equal keys into posting lists of at most BUILD_MAX_POSTING
        // bytes (`_bt_sort_dedup_finish_pending`).
        let mut items: Vec<Item> = Vec::new();
        for (key, tid) in entries {
            if dedup {
                if let Some(last) = items.last_mut() {
                    if keep_natts(shape, &last.key, &key) > shape.nkeyatts() {
                        let base = shape.tuple_size(&last.key);
                        let merged = maxalign(base + TID * (last.tids.len() as i64 + 1));
                        if merged <= BUILD_MAX_POSTING {
                            last.tids.push(tid);
                            continue;
                        }
                    }
                }
            }
            items.push(Item {
                key,
                tids: vec![tid],
                size: 0,
                dead: false,
            });
        }
        for i in items.iter_mut() {
            *i = item(
                shape,
                std::mem::take(&mut i.key),
                std::mem::take(&mut i.tids),
            );
        }
        let ff = shape.fillfactor();
        let mut b = Builder {
            tree: &mut tree,
            levels: Vec::new(),
            leaf_full: (PAGE as i64) * (100 - ff) / 100,
            internal_full: (PAGE as i64) * (100 - NONLEAF_FILLFACTOR) / 100,
        };
        for it in items {
            b.add_leaf(shape, it);
        }
        b.finish();
        tree
    }

    /// The leaf `_bt_search` puts a unique index's key on without its heap
    /// TID (the first page the key could be on), which `_bt_check_unique`
    /// checks for an SSI conflict before it raises a duplicate. `None` for
    /// an empty index.
    pub(crate) fn unique_leaf(&self, shape: &dyn Shape, key: &[Bson]) -> Option<u32> {
        self.root?;
        Some(self.descend(shape, key, i64::MIN).0)
    }

    /// The leaf an item of `key` and `tid` belongs on, and the internal
    /// pages above it, root first.
    fn descend(&self, shape: &dyn Shape, key: &[Bson], tid: i64) -> (u32, Vec<u32>) {
        let mut path = Vec::new();
        let mut at = self.root.expect("a root");
        loop {
            match &self.pages[at as usize] {
                Page::Internal { items, .. } => {
                    path.push(at);
                    // The last downlink whose separator is at or below the
                    // key (the first is minus infinity).
                    let after = items[1..].partition_point(|d| {
                        cmp_pivot(shape, key, tid, &d.pivot) != Ordering::Less
                    });
                    at = items[after].child;
                }
                _ => return (at, path),
            }
        }
    }

    /// [`Self::descend`] without the heap TID (`i64::MIN`, `_bt_search`
    /// with no `scantid`, as a unique insert first searches), and whether
    /// the TID could have taken any other way down: only a pivot whose
    /// every key attribute equals `key` and which keeps a heap TID compares
    /// differently for different TIDs, and such pivots sort together right
    /// where the TID-less search stops on each page. When no page on the way
    /// has one there, a search WITH any TID lands on the same leaf by the
    /// same path -- what `_bt_findinsertloc` finds by stepping right from
    /// the leaf the unique check began at, without a second descent
    /// (RP-014).
    fn descend_tidless(&self, shape: &dyn Shape, key: &[Bson]) -> (u32, Vec<u32>, bool) {
        let n = shape.nkeyatts();
        let mut path = Vec::new();
        let mut tid_free = true;
        let mut at = self.root.expect("a root");
        loop {
            match &self.pages[at as usize] {
                Page::Internal { items, .. } => {
                    path.push(at);
                    let after = items[1..].partition_point(|d| {
                        cmp_pivot(shape, key, i64::MIN, &d.pivot) != Ordering::Less
                    });
                    if let Some(next) = items.get(after + 1) {
                        let p = &next.pivot;
                        if p.tid.is_some()
                            && p.key.len() >= n
                            && key.len() >= n
                            && (0..n).all(|a| shape.cmp_attr(a, &key[a], &p.key[a]) == Ordering::Equal)
                        {
                            tid_free = false;
                        }
                    }
                    at = items[after].child;
                }
                _ => return (at, path, tid_free),
            }
        }
    }

    /// Every heap TID inserting `key` (with heap `tid`) can ask the heap
    /// about: the equal tuples `_bt_check_unique` passes (a unique index),
    /// and every tuple on the leaf the insert lands on, which simple and
    /// bottom-up deletion read. A superset of what `insert_deleting` asks,
    /// for the heap to be read for these alone, before the tree is changed
    /// (RP-014: reading the whole heap for each inserted row made a run of
    /// inserts quadratic).
    ///
    /// The leaf's tuples are asked about only when the leaf cannot take the
    /// new tuple (`free < need`), and `pending` -- the size of every tuple
    /// the same command inserts, which may fill the leaf first -- is added
    /// to the need so the set stays a superset.
    pub(crate) fn candidate_tids(
        &self,
        shape: &dyn Shape,
        key: &[Bson],
        tid: i64,
        pending: i64,
    ) -> Vec<i64> {
        let mut out = Vec::new();
        if self.root.is_none() {
            return out;
        }
        // The leaf the insert lands on, when the TID-less search already
        // found it (`descend_tidless`).
        let mut landed = None;
        if shape.unique() {
            let (first, _, tid_free) = self.descend_tidless(shape, key);
            if tid_free {
                landed = Some(first);
            }
            let mut page = first;
            while let Page::Leaf { items, right, .. } = &self.pages[page as usize] {
                let mut last_equal = false;
                // The items are in key order: the equal run starts where the
                // smaller keys end.
                let from =
                    items.partition_point(|it| cmp_keys(shape, &it.key, key) == Ordering::Less);
                for it in &items[from..] {
                    match cmp_keys(shape, &it.key, key) {
                        Ordering::Less => continue,
                        Ordering::Greater => {
                            last_equal = false;
                            break;
                        }
                        Ordering::Equal => {
                            last_equal = true;
                            out.extend(it.tids.iter().copied());
                        }
                    }
                }
                match right {
                    Some(r) if last_equal => page = *r,
                    _ => break,
                }
            }
        }
        let leaf = landed.unwrap_or_else(|| self.descend(shape, key, tid).0);
        let cached = self.used.get(leaf as usize).copied().filter(|u| *u >= 0);
        if let Page::Leaf { items, high, .. } = &self.pages[leaf as usize] {
            let free = match cached {
                Some(used) => USABLE - used - high.as_ref().map_or(0, |h| h.size + LP) - LP,
                None => leaf_free(items, high),
            };
            if free < pending {
                for it in items {
                    out.extend(it.tids.iter().copied());
                }
            }
        }
        out
    }

    /// The size an index tuple for `key` takes (`insertstate->itemsz`).
    pub(crate) fn tuple_size(shape: &dyn Shape, key: Vec<Bson>, tid: i64) -> i64 {
        item(shape, key, vec![tid]).size
    }

    /// `_bt_check_unique`'s pass over the key's equal tuples, from the
    /// leftmost leaf it can be on (a search without the heap TID) rightward
    /// while they go on: each whose every heap version is dead to all is
    /// marked LP_DEAD as the check passes it. Returns that leftmost leaf:
    /// marking changes no page's place, so it is where a search without the
    /// heap TID still lands.
    ///
    /// With it, the TID-less search's path and whether a search with any
    /// heap TID takes the same one (`descend_tidless`): marking changes no
    /// page's place, so that leaf and path are where the insert goes.
    fn check_unique(
        &mut self,
        shape: &dyn Shape,
        key: &[Bson],
        dead: &dyn Fn(i64) -> bool,
    ) -> (u32, Vec<u32>, bool) {
        let (first, path, tid_free) = self.descend_tidless(shape, key);
        let done = (first, path, tid_free);
        let mut page = first;
        loop {
            let Page::Leaf { items, right, .. } = &mut self.pages[page as usize] else {
                return done;
            };
            let mut marked = false;
            let mut last_equal = false;
            // The items are in key order: the equal run starts where the
            // smaller keys end.
            let from = items.partition_point(|it| cmp_keys(shape, &it.key, key) == Ordering::Less);
            for it in items[from..].iter_mut() {
                match cmp_keys(shape, &it.key, key) {
                    Ordering::Less => continue,
                    Ordering::Greater => {
                        last_equal = false;
                        break;
                    }
                    Ordering::Equal => {
                        last_equal = true;
                        if !it.dead && it.tids.iter().all(|t| dead(*t)) {
                            it.dead = true;
                            marked = true;
                        }
                    }
                }
            }
            let next = *right;
            if marked {
                self.touched.insert(page);
            }
            match next {
                Some(r) if last_equal => page = r,
                _ => return done,
            }
        }
    }

    fn new_page(&mut self, page: Page) -> u32 {
        let b = if let Some(b) = self.free.pop() {
            self.pages[b as usize] = page;
            b
        } else {
            self.pages.push(page);
            (self.pages.len() - 1) as u32
        };
        if let Some(u) = self.used.get_mut(b as usize) {
            *u = -1;
        }
        self.touched.insert(b);
        b
    }

    /// An index tuple for `key` and heap `tid`, as `_bt_doinsert` adds one.
    /// A unique index deduplicates a page only when an equal key is already
    /// there (an older version of the row).
    pub(crate) fn insert(&mut self, shape: &dyn Shape, key: Vec<Bson>, tid: i64) {
        self.insert_deleting(shape, key, tid, false, &|_| false);
    }

    /// `insert`, with the executor's hint that the UPDATE left this index's
    /// key unchanged (`indexUnchanged`) and the heap's answer to which TIDs
    /// are dead to every snapshot (`dead`), for bottom-up deletion (RP-196).
    pub(crate) fn insert_deleting(
        &mut self,
        shape: &dyn Shape,
        key: Vec<Bson>,
        tid: i64,
        unchanged: bool,
        dead: &dyn Fn(i64) -> bool,
    ) {
        self.dirty = true;
        if self.root.is_none() {
            let new = item(shape, key, vec![tid]);
            let root = self.new_page(Page::Leaf {
                items: vec![new],
                high: None,
                right: None,
            });
            self.root = Some(root);
            // `_bt_getroot` made the root the insert lands on.
            self.ssi_events.push(SsiEvent::Landed(root));
            return;
        }
        let (unique_first, reuse) = match shape.unique().then(|| self.check_unique(shape, &key, dead)) {
            Some((first, path, tid_free)) => (Some(first), tid_free.then_some((first, path))),
            None => (None, None),
        };
        let (leaf, path) = match reuse {
            Some(found) => found,
            None => self.descend(shape, &key, tid),
        };
        // The page `_bt_search` lands on, which SSI checks: without the heap
        // TID for a unique index (the first the key could be on) -- the
        // leaf the unique check began at, which its marks did not move.
        let landed = unique_first.unwrap_or(leaf);
        self.ssi_events.push(SsiEvent::Landed(landed));
        self.touched.insert(leaf);
        let cached = self.used.get(leaf as usize).copied().filter(|u| *u >= 0);
        let Page::Leaf { items, high, right } = &mut self.pages[leaf as usize] else {
            return;
        };
        let new = item(shape, key, vec![tid]);
        // `insertstate->itemsz`: the tuple's MAXALIGNed size, its line
        // pointer aside -- `PageGetFreeSpace` (`leaf_free`) has already
        // set one aside.
        let need = new.size;
        let mut free = match cached {
            Some(used) => USABLE - used - high.as_ref().map_or(0, |h| h.size + LP) - LP,
            None => leaf_free(items, high),
        };
        // `_bt_delete_or_dedup_one_page`, for a leaf too full for the new
        // tuple. A unique index goes on only around an equal key
        // (`uniquedup`); then bottom-up deletion when the UPDATE left the key
        // unchanged or the key is already there, and deduplication when that
        // did not free enough.
        // `_bt_simpledel_pass` first: the LP_DEAD tuples, and any other
        // dead TID in one of their heap blocks.
        let mut simple_ran = false;
        if free < need && items.iter().any(|i| i.dead) {
            simple_ran = true;
            simple_pass(shape, items, tid, dead);
            free = leaf_free(items, high);
        }
        if free < need {
            // `_bt_findinsertloc` starts from the executor's hint
            // (`uniquedup = indexUnchanged`).
            let uniquedup = shape.unique()
                && (unchanged || simple_ran || {
                    let at = items
                        .partition_point(|i| cmp_keys(shape, &i.key, &new.key) == Ordering::Less);
                    items
                        .get(at)
                        .is_some_and(|i| cmp_keys(shape, &i.key, &new.key) == Ordering::Equal)
                });
            if !shape.unique() || uniquedup {
                let mut deduplicate = shape.dedup();
                if unchanged || uniquedup {
                    let done = bottomup_pass(shape, items, high, new.size, dead);
                    free = leaf_free(items, high);
                    // `_bt_bottomupdel_pass`' "success": no deduplication.
                    deduplicate = deduplicate && !done;
                }
                if deduplicate && free < need {
                    let rightmost = right.is_none();
                    dedup_pass(shape, items, high, &new, rightmost);
                    free = leaf_free(items, high);
                }
            }
        }
        let Page::Leaf { items, high, .. } = &mut self.pages[leaf as usize] else {
            return;
        };
        // The position after deduplication. A TID inside a posting list's
        // range splits it (`_bt_swap_posting`): the list keeps its size and
        // the new tuple carries its highest TID.
        let mut new = new;
        let mut pos = items.partition_point(|i| match cmp_keys(shape, &i.key, &new.key) {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => i.tids.first().is_some_and(|t| *t < tid),
        });
        if pos > 0 {
            let prev = &mut items[pos - 1];
            if prev.tids.len() > 1
                && cmp_keys(shape, &prev.key, &new.key) == Ordering::Equal
                && prev.tids.last().is_some_and(|t| *t > tid)
            {
                let top = prev.tids.pop().unwrap_or(tid);
                let at = prev.tids.partition_point(|t| *t < tid);
                prev.tids.insert(at, tid);
                new.tids = vec![top];
                pos = items.partition_point(|i| match cmp_keys(shape, &i.key, &new.key) {
                    Ordering::Less => true,
                    Ordering::Greater => false,
                    Ordering::Equal => i.tids.first().is_some_and(|t| *t < top),
                });
            }
        }
        // A posting list split keeps every size, so the space is as it was.
        if free >= need {
            let grown = new.size + LP;
            items.insert(pos, new);
            let used = USABLE - free - high.as_ref().map_or(0, |h| h.size + LP) - LP + grown;
            let b = leaf as usize;
            if self.used.len() <= b {
                self.used.resize(b + 1, -1);
            }
            self.used[b] = used;
            return;
        }
        if let Some(u) = self.used.get_mut(leaf as usize) {
            *u = -1;
        }
        let Page::Leaf { items, high, right } = &mut self.pages[leaf as usize] else {
            return;
        };
        let rightmost = right.is_none();
        let high_size = high.as_ref().map(|h| h.size);
        let mut all: Vec<Item> = std::mem::take(items);
        all.insert(pos, new);
        let tuples: Vec<Tuple<'_>> = all
            .iter()
            .map(|i| Tuple {
                key: &i.key,
                size: i.size + LP,
                posting: if i.tids.len() > 1 {
                    i.size - shape.tuple_size(&i.key)
                } else {
                    0
                },
            })
            .collect();
        let mut offer_after = true;
        if pos > 0 && pos + 1 < all.len() && shape.nkeyatts() > 1 {
            // `_bt_adjacenthtid`: the mid-page "split after new item" case
            // needs the new TID adjacent to its left neighbour's.
            // A posting list's TID field is not a heap TID.
            let prev = &all[pos - 1];
            offer_after = prev.tids.len() == 1 && {
                let (lo, hi) = (prev.tids[0] / 65536, tid / 65536);
                lo == hi || (lo + 1 == hi && tid % 65536 == 1)
            };
        }
        let left = find_split(shape, &tuples, pos, true, rightmost, high_size, offer_after);
        drop(tuples);
        let right_items = all.split_off(left);
        let pivot = truncate(shape, &all[all.len() - 1], &right_items[0]);
        let (old_high, old_right) = match &mut self.pages[leaf as usize] {
            Page::Leaf { high, right, .. } => (high.take(), *right),
            _ => (None, None),
        };
        let new_block = self.new_page(Page::Leaf {
            items: right_items,
            high: old_high,
            right: old_right,
        });
        self.ssi_events.push(SsiEvent::Split(leaf, new_block));
        if let Page::Leaf { items, high, right } = &mut self.pages[leaf as usize] {
            *items = all;
            *high = Some(pivot.clone());
            *right = Some(new_block);
        }
        self.insert_downlink(shape, path, leaf, pivot, new_block);
    }

    /// Link a new right sibling into the parent (`_bt_insert_parent`),
    /// splitting it when it is full, up to a new root.
    fn insert_downlink(
        &mut self,
        shape: &dyn Shape,
        mut path: Vec<u32>,
        left: u32,
        pivot: Pivot,
        right_block: u32,
    ) {
        let Some(parent) = path.pop() else {
            // The root split: a new root over the two.
            let root = self.new_page(Page::Internal {
                items: vec![
                    Downlink {
                        pivot: Pivot::minus_infinity(),
                        child: left,
                    },
                    Downlink {
                        pivot,
                        child: right_block,
                    },
                ],
                high: None,
                right: None,
            });
            self.root = Some(root);
            return;
        };
        self.touched.insert(parent);
        let Page::Internal { items, high, right } = &mut self.pages[parent as usize] else {
            return;
        };
        let at = items
            .iter()
            .position(|d| d.child == left)
            .map_or(items.len(), |i| i + 1);
        let link = Downlink {
            pivot,
            child: right_block,
        };
        // `PageGetFreeSpace(page) < itemsz`, as for a leaf.
        if internal_free(items, high) >= link.pivot.size {
            items.insert(at, link);
            return;
        }
        let rightmost = right.is_none();
        let high_size = high.as_ref().map(|h| h.size);
        let mut all = std::mem::take(items);
        all.insert(at, link);
        let keys: Vec<Vec<Bson>> = all.iter().map(|d| d.pivot.key.clone()).collect();
        let tuples: Vec<Tuple<'_>> = all
            .iter()
            .zip(&keys)
            .map(|(d, k)| Tuple {
                key: k,
                size: d.pivot.size + LP,
                posting: 0,
            })
            .collect();
        let split = find_split(shape, &tuples, at, false, rightmost, high_size, false);
        drop(tuples);
        let mut right_items = all.split_off(split);
        // The right page's first downlink keeps its child; its separator
        // moves up, and the page keeps minus infinity in its place.
        let up = std::mem::replace(&mut right_items[0].pivot, Pivot::minus_infinity());
        let (old_high, old_right) = match &mut self.pages[parent as usize] {
            Page::Internal { high, right, .. } => (high.take(), *right),
            _ => (None, None),
        };
        let new_block = self.new_page(Page::Internal {
            items: right_items,
            high: old_high,
            right: old_right,
        });
        if let Page::Internal { items, high, right } = &mut self.pages[parent as usize] {
            *items = all;
            *high = Some(up.clone());
            *right = Some(new_block);
        }
        self.insert_downlink(shape, path, parent, up, new_block);
    }

    /// VACUUM's bulk delete (`btbulkdelete`): every TID in `dead` leaves
    /// its tuple (a posting list shrinks), and a leaf left empty is deleted
    /// when it is not the rightmost of its level, its downlink removed.
    /// Whether any tuple went.
    pub(crate) fn bulk_delete(&mut self, shape: &dyn Shape, dead: &dyn Fn(i64) -> bool) -> bool {
        // The scan recycles what the last VACUUM deleted.
        self.dirty = true;
        self.full = true;
        self.used.clear();
        self.free.append(&mut self.pending);
        let mut removed = false;
        let mut emptied: Vec<u32> = Vec::new();
        for (b, page) in self.pages.iter_mut().enumerate() {
            let Page::Leaf { items, right, .. } = page else {
                continue;
            };
            let before: usize = items.iter().map(|i| i.tids.len()).sum();
            for it in items.iter_mut() {
                it.tids.retain(|t| !dead(*t));
            }
            items.retain(|i| !i.tids.is_empty());
            for it in items.iter_mut() {
                let key = std::mem::take(&mut it.key);
                let tids = std::mem::take(&mut it.tids);
                *it = item(shape, key, tids);
            }
            let after: usize = items.iter().map(|i| i.tids.len()).sum();
            removed |= after != before;
            if items.is_empty() && right.is_some() && Some(b as u32) != self.root {
                emptied.push(b as u32);
            }
        }
        for b in emptied {
            self.delete_leaf(b);
        }
        removed
    }

    /// `_bt_pagedel` of an empty leaf: its parent loses the downlink (and
    /// the leaf's left sibling takes its key space); a parent left with no
    /// downlink goes the same way.
    fn delete_leaf(&mut self, block: u32) {
        let parent = self.pages.iter().position(|p| match p {
            Page::Internal { items, .. } => items.iter().any(|d| d.child == block),
            _ => false,
        });
        let Some(parent) = parent else { return };
        let mut parent_empty = false;
        if let Page::Internal { items, right, .. } = &mut self.pages[parent] {
            // The only child of its parent is deleted only with the parent,
            // which the rightmost parent never is.
            if items.len() == 1 && right.is_none() {
                return;
            }
            let at = items.iter().position(|d| d.child == block).unwrap_or(0);
            items.remove(at);
            if at == 0 {
                if let Some(first) = items.first_mut() {
                    first.pivot = Pivot::minus_infinity();
                }
            }
            parent_empty = items.is_empty();
        }
        // The left sibling links past it, and takes its high key.
        let (high, right) = match &self.pages[block as usize] {
            Page::Leaf { high, right, .. } => (high.clone(), *right),
            _ => (None, None),
        };
        for p in self.pages.iter_mut() {
            if let Page::Leaf {
                right: r, high: h, ..
            } = p
            {
                if *r == Some(block) {
                    *r = right;
                    *h = high.clone();
                }
            }
        }
        self.pages[block as usize] = Page::Deleted;
        self.pending.push(block);
        if parent_empty {
            self.pages[parent] = Page::Deleted;
            self.pending.push(parent as u32);
        }
    }

    /// `btvacuumcleanup` after a VACUUM with no index pass: it scans the
    /// index -- recycling the pages the last one deleted -- only when they
    /// are more than 5% of it (`_bt_vacuum_needs_cleanup`). Whether it did.
    pub(crate) fn cleanup(&mut self) -> bool {
        let pending = self.pending.len() as u32;
        if pending > 0 && pending > self.pages() / 20 {
            self.dirty = true;
            self.free.append(&mut self.pending);
            return true;
        }
        false
    }
}

/// `_bt_dedup_pass`: merge each run of equal keys on a full leaf into
/// posting lists, the single value strategy's limits applied when the
/// page holds only the new item's value.
fn dedup_pass(
    shape: &dyn Shape,
    items: &mut Vec<Item>,
    _high: &Option<Pivot>,
    new: &Item,
    _rightmost: bool,
) {
    let n = shape.nkeyatts();
    let single = match (items.first(), items.last()) {
        (Some(f), Some(l)) => {
            keep_natts(shape, &f.key, &new.key) > n && keep_natts(shape, &l.key, &new.key) > n
        }
        _ => false,
    };
    let mut maxposting = MAX_POSTING;
    let mut nmax = 0;
    let mut merging = true;
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    let mut i = 0;
    while i < items.len() {
        if !merging {
            out.push(items[i].clone());
            i += 1;
            continue;
        }
        // The run of tuples equal to this one, merged while they fit.
        let base_key = items[i].key.clone();
        let base = shape.tuple_size(&base_key);
        let mut tids = items[i].tids.clone();
        let mut j = i + 1;
        let start_max = items[i].size >= maxposting;
        while j < items.len() && keep_natts(shape, &items[j].key, &base_key) > n {
            let more = items[j].tids.len() as i64;
            let merged = maxalign(base + TID * (tids.len() as i64 + more));
            if merged > maxposting {
                break;
            }
            tids.extend_from_slice(&items[j].tids);
            j += 1;
        }
        let merged = item(shape, base_key, tids);
        let at_max = merged.size + TID > maxposting || start_max;
        out.push(merged);
        i = j;
        if single && at_max {
            nmax += 1;
            if nmax == 5 {
                // `_bt_singleval_fillfactor`.
                let leftfree = USABLE - (new.size + LP) - PIVOT_TID;
                let reduction = (leftfree as f64 * ((100.0 - SINGLEVAL_FILLFACTOR) / 100.0)) as i64;
                maxposting = (maxposting - reduction).max(0);
            } else if nmax == 6 {
                merging = false;
            }
        }
    }
    *items = out;
}

struct Builder<'a> {
    tree: &'a mut BTree,
    /// Per level, the page being filled: its block, items (leaf) or
    /// downlinks, and its low key.
    levels: Vec<BuildLevel>,
    leaf_full: i64,
    internal_full: i64,
}

struct BuildLevel {
    block: u32,
    leaf_items: Vec<Item>,
    links: Vec<Downlink>,
    low: Pivot,
}

impl Builder<'_> {
    fn new_level_page(&mut self, leaf: bool) -> u32 {
        self.tree.pages.push(if leaf {
            Page::Leaf {
                items: Vec::new(),
                high: None,
                right: None,
            }
        } else {
            Page::Internal {
                items: Vec::new(),
                high: None,
                right: None,
            }
        });
        (self.tree.pages.len() - 1) as u32
    }

    fn add_leaf(&mut self, shape: &dyn Shape, it: Item) {
        if self.levels.is_empty() {
            let block = self.new_level_page(true);
            self.levels.push(BuildLevel {
                block,
                leaf_items: Vec::new(),
                links: Vec::new(),
                low: Pivot::minus_infinity(),
            });
        }
        let lvl = &self.levels[0];
        // `_bt_blnewpage` reserves the high key's line pointer.
        let free = USABLE - LP - lvl.leaf_items.iter().map(|i| i.size + LP).sum::<i64>() - LP;
        // `_bt_buildadd`: room for the item and for a heap TID suffix
        // truncation may add to the high key; and the soft fillfactor
        // limit counts the posting list the last item's high key would
        // shed (`btps_lastextra`, RP-298).
        let last_extra = lvl.leaf_items.last().map_or(0, |l| {
            if l.tids.len() > 1 {
                l.size - shape.tuple_size(&l.key)
            } else {
                0
            }
        });
        if free < it.size + PIVOT_TID
            || (free + last_extra < self.leaf_full && lvl.leaf_items.len() >= 2)
        {
            // Finish the page: its last item starts the next one, and the
            // high key between them closes it.
            let lvl = &mut self.levels[0];
            let last = lvl.leaf_items.pop().expect("an item");
            let high = truncate(shape, lvl.leaf_items.last().expect("an item"), &last);
            let block = self.new_level_page(true);
            let lvl = &mut self.levels[0];
            let old = lvl.block;
            let items = std::mem::take(&mut lvl.leaf_items);
            self.tree.pages[old as usize] = Page::Leaf {
                items,
                high: Some(high.clone()),
                right: Some(block),
            };
            let low = std::mem::replace(&mut lvl.low, high);
            lvl.block = block;
            lvl.leaf_items.push(last);
            self.add_link(
                1,
                Downlink {
                    pivot: low,
                    child: old,
                },
            );
        }
        self.levels[0].leaf_items.push(it);
    }

    fn add_link(&mut self, level: usize, link: Downlink) {
        if self.levels.len() <= level {
            let block = self.new_level_page(false);
            self.levels.push(BuildLevel {
                block,
                leaf_items: Vec::new(),
                links: Vec::new(),
                low: Pivot::minus_infinity(),
            });
        }
        // The first downlink of a page is stored as minus infinity.
        let size_of = |links: &[Downlink], d: &Downlink| -> i64 {
            if links.is_empty() {
                MINUS_INFINITY
            } else {
                d.pivot.size
            }
        };
        let lvl = &self.levels[level];
        let used: i64 = lvl
            .links
            .iter()
            .enumerate()
            .map(|(i, d)| if i == 0 { MINUS_INFINITY } else { d.pivot.size } + LP)
            .sum();
        let free = USABLE - LP - used - LP;
        let itsz = size_of(&lvl.links, &link);
        if free < itsz || (free < self.internal_full && lvl.links.len() >= 2) {
            let lvl = &mut self.levels[level];
            let mut last = lvl.links.pop().expect("a downlink");
            let high = last.pivot.clone();
            let block = self.new_level_page(false);
            let lvl = &mut self.levels[level];
            let old = lvl.block;
            let mut links = std::mem::take(&mut lvl.links);
            if let Some(f) = links.first_mut() {
                f.pivot = Pivot::minus_infinity();
            }
            self.tree.pages[old as usize] = Page::Internal {
                items: links,
                high: Some(high.clone()),
                right: Some(block),
            };
            let low = std::mem::replace(&mut lvl.low, high);
            lvl.block = block;
            last.pivot = Pivot::minus_infinity();
            lvl.links.push(last);
            self.add_link(
                level + 1,
                Downlink {
                    pivot: low,
                    child: old,
                },
            );
        }
        self.levels[level].links.push(link);
    }

    /// `_bt_uppershutdown`: each level's last page is its rightmost, linked
    /// into the level above; the top level's page is the root.
    fn finish(&mut self) {
        let mut level = 0;
        while level < self.levels.len() {
            let block = self.levels[level].block;
            let top = level + 1 == self.levels.len();
            if level == 0 {
                let items = std::mem::take(&mut self.levels[0].leaf_items);
                self.tree.pages[block as usize] = Page::Leaf {
                    items,
                    high: None,
                    right: None,
                };
            } else {
                let mut links = std::mem::take(&mut self.levels[level].links);
                if let Some(f) = links.first_mut() {
                    f.pivot = Pivot::minus_infinity();
                }
                self.tree.pages[block as usize] = Page::Internal {
                    items: links,
                    high: None,
                    right: None,
                };
            }
            if top {
                self.tree.root = Some(block);
            } else {
                let low = std::mem::replace(&mut self.levels[level].low, Pivot::minus_infinity());
                self.add_link(
                    level + 1,
                    Downlink {
                        pivot: low,
                        child: block,
                    },
                );
            }
            level += 1;
        }
    }
}

fn ints(v: &[u32]) -> Bson {
    Bson::Array(v.iter().map(|b| Bson::Int64(i64::from(*b))).collect())
}

fn from_ints(v: Option<&Bson>) -> Vec<u32> {
    v.and_then(Bson::as_array)
        .map(|a| {
            a.iter()
                .filter_map(crate::bson_i64)
                .filter_map(|b| u32::try_from(b).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn pivot_doc(p: &Pivot) -> Bson {
    Bson::Array(vec![
        Bson::Array(p.key.clone()),
        p.tid.map_or(Bson::Null, Bson::Int64),
        Bson::Int64(p.size),
    ])
}

fn pivot_from(b: &Bson) -> Option<Pivot> {
    let a = b.as_array()?;
    Some(Pivot {
        key: a.first()?.as_array()?.clone(),
        tid: a.get(1).and_then(crate::bson_i64),
        size: a.get(2).and_then(crate::bson_i64)?,
    })
}

impl BTree {
    /// The model as documents, one a page (`_id` "oid:block") and one for
    /// the tree (`_id` "oid"), each tagged with the index's oid.
    pub(crate) fn to_docs(&self, oid: i64) -> Vec<bson::Document> {
        let mut out = vec![self.tree_doc(oid)];
        out.extend((0..self.pages.len()).map(|b| self.page_doc(oid, b)));
        out
    }

    fn tree_doc(&self, oid: i64) -> bson::Document {
        bson::doc! {
            "_id": oid.to_string(),
            "index": oid,
            "root": self.root.map_or(Bson::Null, |r| Bson::Int64(i64::from(r))),
            "free": ints(&self.free),
            "pending": ints(&self.pending),
            "npages": self.pages.len() as i64,
        }
    }

    fn page_doc(&self, oid: i64, b: usize) -> bson::Document {
        let page = &self.pages[b];
        {
            let mut d = bson::doc! { "_id": format!("{oid}:{b}"), "index": oid, "block": b as i64 };
            let link = |r: &Option<u32>| r.map_or(Bson::Null, |r| Bson::Int64(i64::from(r)));
            match page {
                Page::Meta => d.insert("kind", "m"),
                Page::Deleted => d.insert("kind", "d"),
                Page::Leaf { items, high, right } => {
                    d.insert("kind", "l");
                    d.insert(
                        "items",
                        items
                            .iter()
                            .map(|i| {
                                let mut a = vec![
                                    Bson::Array(i.key.clone()),
                                    Bson::Array(i.tids.iter().map(|t| Bson::Int64(*t)).collect()),
                                    Bson::Int64(i.size),
                                ];
                                if i.dead {
                                    a.push(Bson::Boolean(true));
                                }
                                Bson::Array(a)
                            })
                            .collect::<Vec<_>>(),
                    );
                    d.insert("high", high.as_ref().map_or(Bson::Null, pivot_doc));
                    d.insert("right", link(right))
                }
                Page::Internal { items, high, right } => {
                    d.insert("kind", "i");
                    d.insert(
                        "items",
                        items
                            .iter()
                            .map(|l| {
                                Bson::Array(vec![
                                    pivot_doc(&l.pivot),
                                    Bson::Int64(i64::from(l.child)),
                                ])
                            })
                            .collect::<Vec<_>>(),
                    );
                    d.insert("high", high.as_ref().map_or(Bson::Null, pivot_doc));
                    d.insert("right", link(right))
                }
            };
            d
        }
    }

    /// What a checkpoint writes: the documents of the pages changed since
    /// the last, and the tree's; with the ids to replace, or `None` to
    /// replace every document of the index.
    pub(crate) fn take_changes(&mut self, oid: i64) -> (Vec<bson::Document>, Option<Vec<String>>) {
        self.dirty = false;
        if std::mem::take(&mut self.full) {
            self.touched.clear();
            return (self.to_docs(oid), None);
        }
        let touched = std::mem::take(&mut self.touched);
        let mut docs = vec![self.tree_doc(oid)];
        docs.extend(
            touched
                .iter()
                .map(|b| *b as usize)
                .filter(|b| *b < self.pages.len())
                .map(|b| self.page_doc(oid, b)),
        );
        let ids = docs
            .iter()
            .filter_map(|d| d.get_str("_id").ok().map(str::to_string))
            .collect();
        (docs, Some(ids))
    }

    /// A write that failed: everything is written again next time.
    pub(crate) fn rewrite_all(&mut self) {
        self.dirty = true;
        self.full = true;
    }

    /// The model [`Self::to_docs`] wrote, or `None` when a page is missing.
    pub(crate) fn from_docs(docs: &[bson::Document]) -> Option<BTree> {
        let tree = docs
            .iter()
            .find(|d| d.get_str("_id").is_ok_and(|i| !i.contains(':')))?;
        let n = usize::try_from(tree.get("npages").and_then(crate::bson_i64)?).ok()?;
        let mut pages: Vec<Option<Page>> = vec![None; n];
        for d in docs {
            let Some(b) = d.get("block").and_then(crate::bson_i64) else {
                continue;
            };
            let b = usize::try_from(b).ok()?;
            let right = d
                .get("right")
                .and_then(crate::bson_i64)
                .and_then(|r| u32::try_from(r).ok());
            let high = d.get("high").and_then(pivot_from);
            let items = d.get_array("items").map(|a| a.as_slice()).unwrap_or(&[]);
            let page = match d.get_str("kind").ok()? {
                "m" => Page::Meta,
                "d" => Page::Deleted,
                "l" => Page::Leaf {
                    items: items
                        .iter()
                        .map(|i| {
                            let a = i.as_array()?;
                            Some(Item {
                                key: a.first()?.as_array()?.clone(),
                                tids: a
                                    .get(1)?
                                    .as_array()?
                                    .iter()
                                    .filter_map(crate::bson_i64)
                                    .collect(),
                                size: a.get(2).and_then(crate::bson_i64)?,
                                dead: a.get(3).and_then(Bson::as_bool).unwrap_or(false),
                            })
                        })
                        .collect::<Option<Vec<_>>>()?,
                    high,
                    right,
                },
                "i" => Page::Internal {
                    items: items
                        .iter()
                        .map(|l| {
                            let a = l.as_array()?;
                            Some(Downlink {
                                pivot: pivot_from(a.first()?)?,
                                child: u32::try_from(a.get(1).and_then(crate::bson_i64)?).ok()?,
                            })
                        })
                        .collect::<Option<Vec<_>>>()?,
                    high,
                    right,
                },
                _ => return None,
            };
            *pages.get_mut(b)? = Some(page);
        }
        Some(BTree {
            pages: pages.into_iter().collect::<Option<Vec<_>>>()?,
            root: tree
                .get("root")
                .and_then(crate::bson_i64)
                .and_then(|r| u32::try_from(r).ok()),
            free: from_ints(tree.get("free")),
            pending: from_ints(tree.get("pending")),
            dirty: false,
            full: false,
            touched: Default::default(),
            used: Vec::new(),
            ssi_events: Vec::new(),
        })
    }

    /// Every heap TID it holds.
    pub(crate) fn tids(&self) -> std::collections::HashSet<i64> {
        self.pages
            .iter()
            .flat_map(|p| match p {
                Page::Leaf { items, .. } => items.iter().flat_map(|i| i.tids.clone()).collect(),
                _ => Vec::new(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An int4 column: a 16-byte tuple, distinct keys in integer order.
    struct Int {
        unique: bool,
    }
    impl Shape for Int {
        fn nkeyatts(&self) -> usize {
            1
        }
        fn cmp_attr(&self, _: usize, a: &Bson, b: &Bson) -> Ordering {
            a.as_i32().cmp(&b.as_i32())
        }
        fn tuple_size(&self, attrs: &[Bson]) -> i64 {
            maxalign(8 + 4 * attrs.len() as i64)
        }
        fn dedup(&self) -> bool {
            true
        }
        fn unique(&self) -> bool {
            self.unique
        }
    }

    /// The ctid of the i-th row of a one-int4-column table: 226 a page.
    fn tid(i: usize) -> i64 {
        ((i / 226) as i64) * 65536 + (i % 226) as i64 + 1
    }

    fn keys(vals: impl Iterator<Item = i32>) -> Vec<(Vec<Bson>, i64)> {
        vals.enumerate()
            .map(|(i, v)| (vec![Bson::Int32(v)], tid(i)))
            .collect()
    }

    #[test]
    fn a_built_index_has_postgres_pages() {
        // PostgreSQL 15.19's relpages after CREATE INDEX on 1..n.
        let shape = Int { unique: false };
        for (n, pages) in [
            (0, 1),
            (1, 2),
            (200, 2),
            (367, 2),
            (368, 4),
            (733, 4),
            (734, 5),
            (1000, 5),
            (1101, 6),
            (10000, 30),
            (100000, 276),
        ] {
            let t = BTree::build(&shape, keys(1..=n));
            assert_eq!(t.pages(), pages, "{n} keys");
            assert_eq!(t.tuples(), n as u64);
        }
    }

    #[test]
    fn duplicates_build_posting_lists() {
        let shape = Int { unique: false };
        for (n, d, pages) in [
            (1000, 10, 2),
            (1000, 1, 2),
            (10000, 100, 11),
            (10000, 5000, 27),
        ] {
            let t = BTree::build(&shape, keys((1..=n).map(|g| g % d)));
            assert_eq!(t.pages(), pages, "{n} rows of {d} values");
        }
    }

    #[test]
    fn a_tidless_descent_knows_when_a_tid_would_go_elsewhere() {
        // Many duplicates, so pivots keep heap TIDs; built and grown.
        let shape = Int { unique: false };
        let built = BTree::build(&shape, keys((0..20000).map(|i| i % 7)));
        let grown_dups = grown(&shape, (0..6000).map(|i| (i * 7919) % 13));
        let distinct = grown(&shape, (0..6000).map(|i| (i * 7919) % 6007));
        let mut sensitive = 0;
        for t in [&built, &grown_dups, &distinct] {
            for k in -1..15 {
                let key = [Bson::Int32(k)];
                let (leaf, path, tid_free) = t.descend_tidless(&shape, &key);
                assert_eq!((leaf, path.clone()), t.descend(&shape, &key, i64::MIN));
                if !tid_free {
                    sensitive += 1;
                    continue;
                }
                for i in [0usize, 1, 500, 5000, 19999, 1_000_000] {
                    assert_eq!(
                        t.descend(&shape, &key, tid(i)),
                        (leaf, path.clone()),
                        "key {k}, tid {i}"
                    );
                }
            }
        }
        assert!(sensitive > 0, "no descent met a TID-keeping pivot");
    }

    fn grown(shape: &dyn Shape, vals: impl Iterator<Item = i32>) -> BTree {
        let mut t = BTree::build(shape, Vec::new());
        for (i, v) in vals.enumerate() {
            t.insert(shape, vec![Bson::Int32(v)], tid(i));
        }
        t
    }

    #[test]
    fn inserts_split_pages_as_postgres_does() {
        // pg_relation_size of an index made empty, then filled.
        let shape = Int { unique: false };
        assert_eq!(grown(&shape, 1..=1000).pages(), 5);
        assert_eq!(grown(&shape, 1..=10000).pages(), 30);
        assert_eq!(
            grown(&shape, (1..=1000).map(|g| (g * 7919) % 10007)).pages(),
            7
        );
        assert_eq!(
            grown(&shape, (1..=10000).map(|g| (g * 7919) % 10007)).pages(),
            39
        );
    }

    #[test]
    fn duplicate_inserts_deduplicate() {
        let shape = Int { unique: false };
        for (n, d, pages) in [
            (1000, 10, 2),
            (1000, 1, 2),
            (10000, 100, 11),
            (10000, 5000, 30),
        ] {
            assert_eq!(
                grown(&shape, (1..=n).map(|g| g % d)).pages(),
                pages,
                "{n}/{d}"
            );
        }
    }

    #[test]
    fn vacuum_keeps_the_pages_and_counts_what_is_left() {
        let shape = Int { unique: false };
        let mut t = BTree::build(&shape, keys(1..=1000));
        // Delete the even keys: rows 1, 3, 5... (0-based) hold them.
        let dead: Vec<i64> = (0..1000).filter(|i| (i + 1) % 2 == 0).map(tid).collect();
        assert!(t.bulk_delete(&shape, &|x| dead.contains(&x)));
        assert_eq!((t.pages(), t.tuples()), (5, 500));
    }

    #[test]
    fn a_saved_model_round_trips() {
        let shape = Int { unique: false };
        let t = grown(&shape, (1..=3000).map(|g| (g * 7919) % 10007));
        let back = BTree::from_docs(&t.to_docs(16384)).expect("a model");
        assert_eq!((back.pages, back.root), (t.pages.clone(), t.root));
    }

    #[test]
    fn checkpoints_of_changed_pages_add_up_to_the_model() {
        // A store of the saved documents, written as `flush_store` does.
        let shape = Int { unique: false };
        let mut t = BTree::build(&shape, keys(1..=500));
        let mut store: std::collections::BTreeMap<String, bson::Document> = Default::default();
        let save =
            |t: &mut BTree, store: &mut std::collections::BTreeMap<String, bson::Document>| {
                let (docs, ids) = t.take_changes(7);
                match ids {
                    None => store.clear(),
                    Some(ids) => ids.iter().for_each(|i| {
                        store.remove(i);
                    }),
                }
                for d in docs {
                    store.insert(d.get_str("_id").unwrap().to_string(), d);
                }
            };
        save(&mut t, &mut store);
        for round in 0..3 {
            for i in 0..2000 {
                let k = ((i * 7919 + round * 13) % 10007) as i32;
                t.insert(&shape, vec![Bson::Int32(k)], tid(500 + round * 2000 + i));
            }
            save(&mut t, &mut store);
            let back =
                BTree::from_docs(&store.values().cloned().collect::<Vec<_>>()).expect("a model");
            assert_eq!((back.pages, back.root), (t.pages.clone(), t.root));
        }
    }

    #[test]
    fn a_non_hot_update_grows_the_index() {
        // 1..1000 built, then 1001..2000 inserted (the updated rows' new
        // versions): 8 pages, as on PostgreSQL 15.19.
        let shape = Int { unique: false };
        let mut t = BTree::build(&shape, keys(1..=1000));
        for i in 0..1000 {
            t.insert(&shape, vec![Bson::Int32(1001 + i as i32)], tid(1000 + i));
        }
        assert_eq!(t.pages(), 8);
    }
}
