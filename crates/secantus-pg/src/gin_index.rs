//! A GIN index's pages, as PostgreSQL 15.19 lays them out (RP-254), so
//! that SERIALIZABLE takes the predicate locks `ginget.c` takes and an
//! insert checks the pages `gininsert.c` and `ginfast.c` check.
//!
//! Ported function by function: CREATE INDEX (`ginbuild` over the build
//! accumulator, `ginbulk.c`), the entry tree (`ginentrypage.c`), the posting
//! trees and their compressed segments (`gindatapage.c`,
//! `ginpostinglist.c`), the B-tree machinery both share (`ginbtree.c`: the
//! descent, page splits, root splits and the downlinks they insert), the
//! fast-update pending list and its cleanup (`ginfast.c`), and the free
//! space map pages come back from (`GinNewBuffer`). Block numbers are
//! PostgreSQL's: a predicate lock names a page by its number, so which page
//! a key is on, and what number that page has, is the whole point.
//!
//! The model keeps every key and every heap TID, since where an insert
//! lands and when a page splits depend on their sizes. A TID is this
//! server's `ctid` (block * 65536 + offset), whose order is GIN's.
//!
//! What a mutation does to SSI is returned as [`Event`]s, in the order
//! PostgreSQL makes the calls: `CheckForSerializableConflictIn` on a page,
//! and `PredicateLockPageSplit` from one page to another.

use std::cmp::Ordering;

use bson::Bson;

/// `GIN_METAPAGE_BLKNO`, `GIN_ROOT_BLKNO`.
pub(crate) const META_BLKNO: u32 = 0;
pub(crate) const ROOT_BLKNO: u32 = 1;
/// `InvalidBlockNumber`.
pub(crate) const INVALID: u32 = u32::MAX;
const BLCKSZ: usize = 8192;
/// `SizeOfPageHeaderData`.
const PAGE_HEADER: usize = 24;
/// `sizeof(ItemIdData)`.
const LP: usize = 4;
/// `MAXALIGN(sizeof(GinPageOpaqueData))`.
const GIN_OPAQUE: usize = 8;
/// `GinDataPageMaxDataSize`: the page less its header, the right bound
/// and the special space.
const DATA_MAX: usize = BLCKSZ - PAGE_HEADER - 8 - GIN_OPAQUE;
/// `GinMaxItemSize`.
const MAX_ITEM: usize = 2712;
/// `GinPostingListSegmentMaxSize` / `TargetSize` / `MinSize`.
const SEG_MAX: usize = 384;
const SEG_TARGET: usize = 256;
const SEG_MIN: usize = 128;
/// `MinTuplesPerSegment`.
const MIN_TUPLES_PER_SEGMENT: usize = (SEG_MAX - 2) / 6;
/// `sizeof(PostingItem)`.
const POSTING_ITEM: usize = 10;
/// `GinListPageSize`, which is also `GIN_PAGE_FREESIZE`.
const LIST_PAGE_SIZE: usize = BLCKSZ - PAGE_HEADER - GIN_OPAQUE;
/// An entry or list page's line pointers and tuples go between these.
const PAGE_UPPER: usize = BLCKSZ - GIN_OPAQUE;

fn maxalign(n: usize) -> usize {
    (n + 7) & !7
}

fn shortalign(n: usize) -> usize {
    (n + 1) & !1
}

/// A heap TID, as this server spells a `ctid`: block * 65536 + offset.
pub(crate) type Tid = u64;

fn tid_block(t: Tid) -> u64 {
    t >> 16
}

/// `itemptr_to_uint64`.
fn tid_u64(t: Tid) -> u64 {
    (tid_block(t) << 11) | (t & 0xFFFF)
}

/// The bytes `encode_varbyte` writes for `v`.
fn varbyte_len(mut v: u64) -> usize {
    let mut n = 1;
    while v > 0x7F {
        n += 1;
        v >>= 7;
    }
    n
}

/// `ginCompressPostingList`: how many of `items` fit in a posting list of
/// at most `maxsize` bytes, and the list's `SizeOfGinPostingList`.
fn compress(items: &[Tid], maxsize: usize) -> (usize, usize) {
    let maxsize = maxsize & !1;
    let maxbytes = maxsize - 8;
    let mut used = 0;
    let mut packed = 1;
    let mut prev = tid_u64(items[0]);
    while packed < items.len() {
        let val = tid_u64(items[packed]);
        let len = varbyte_len(val - prev);
        if len > maxbytes - used {
            break;
        }
        used += len;
        prev = val;
        packed += 1;
    }
    (packed, 8 + shortalign(used))
}

/// `ginMergeItemPointers`: the union, sorted, without duplicates.
fn merge(a: &[Tid], b: &[Tid]) -> Vec<Tid> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// `GinNullCategory`, `GIN_CAT_EMPTY_QUERY` included (scans only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Cat {
    EmptyQuery = -1,
    Norm = 0,
    NullKey = 1,
    EmptyItem = 2,
    NullItem = 3,
}

/// An entry's key: its category, and its datum when that is `Norm`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Key {
    pub(crate) cat: Cat,
    pub(crate) val: Bson,
}

impl Key {
    pub(crate) fn norm(val: Bson) -> Key {
        Key {
            cat: Cat::Norm,
            val,
        }
    }

    pub(crate) fn cat(cat: Cat) -> Key {
        Key {
            cat,
            val: Bson::Null,
        }
    }
}

/// What the model needs of an index's one key column: its opclass's
/// `compare` support function, and the sizes its keys take.
pub(crate) trait KeyOps {
    /// The opclass's comparison of two normal keys.
    fn cmp(&self, a: &Bson, b: &Bson) -> Ordering;
    /// `index_form_tuple`'s (MAXALIGNed) size for a tuple of this key.
    fn key_size(&self, v: &Bson) -> usize;
    /// The palloc'd size of a by-reference key datum (`datumCopy`), or
    /// `None` for a by-value one.
    fn datum_size(&self, v: &Bson) -> Option<usize>;
}

/// `ginCompareEntries`: by category, then by the opclass.
fn cmp_keys(ops: &dyn KeyOps, a: &Key, b: &Key) -> Ordering {
    if a.cat != b.cat {
        return a.cat.cmp(&b.cat);
    }
    if a.cat != Cat::Norm {
        return Ordering::Equal;
    }
    ops.cmp(&a.val, &b.val)
}

/// The size of a tuple of `key` before its posting list, and the posting
/// list's offset in it (`GinFormTuple`): a null category's tuple has a
/// null bitmap and a category byte after it.
fn key_offset(ops: &dyn KeyOps, key: &Key) -> usize {
    if key.cat == Cat::Norm {
        shortalign(ops.key_size(&key.val))
    } else {
        // `IndexInfoFindDataOffset` with nulls is 16; the category byte
        // follows it.
        shortalign(16 + 1)
    }
}

/// `GinFormTuple` with no posting list: a posting-tree tuple, a pending
/// list tuple.
fn bare_size(ops: &dyn KeyOps, key: &Key) -> usize {
    maxalign(key_offset(ops, key))
}

/// A leaf entry's posting: a list in the tuple, or a posting tree's root.
#[derive(Debug, Clone, PartialEq)]
enum Posting {
    List(Vec<Tid>),
    Tree(u32),
}

/// An entry tree tuple: on a leaf, its posting; on an internal page, its
/// child (and the posting is `Tree(child)` in spirit, kept apart).
#[derive(Debug, Clone)]
struct ETuple {
    key: Key,
    posting: Posting,
    child: u32,
    size: usize,
}

/// One compressed segment of a posting tree leaf: its TIDs, and its
/// `SizeOfGinPostingList` as stored (`None` once it needs recompressing).
#[derive(Debug, Clone)]
struct Seg {
    items: Vec<Tid>,
    size: Option<usize>,
    action: SegAction,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SegAction {
    Unmodified,
    Delete,
    Insert,
    Replace,
    AddItems,
}

/// A pending list tuple: a key and the heap TID it came from.
#[derive(Debug, Clone)]
struct PTuple {
    key: Key,
    tid: Tid,
    size: usize,
}

#[derive(Debug, Clone)]
enum Page {
    Meta,
    EntryLeaf {
        items: Vec<ETuple>,
        right: u32,
    },
    EntryInternal {
        items: Vec<ETuple>,
        right: u32,
    },
    DataLeaf {
        segs: Vec<Seg>,
        right: u32,
        rbound: Tid,
    },
    DataInternal {
        /// `PostingItem`s: a key (the child's right bound) and the child.
        items: Vec<(Tid, u32)>,
        right: u32,
        rbound: Tid,
    },
    List {
        tuples: Vec<PTuple>,
        right: u32,
        /// The opaque `maxoff`: the heap tuples whose last tuple is here.
        heap_tuples: u32,
        full_row: bool,
    },
    Deleted,
}

/// What a mutation does to SSI, in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Event {
    /// `CheckForSerializableConflictIn` on this page.
    ConflictIn(u32),
    /// `PredicateLockPageSplit` from the first page to the second.
    Split(u32, u32),
}

/// `GinStatsData` as the metapage keeps it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Stats {
    pub(crate) n_pending_pages: u32,
    pub(crate) n_total_pages: u32,
    pub(crate) n_entry_pages: u32,
    pub(crate) n_data_pages: u32,
    pub(crate) n_entries: u64,
}

/// The pending list's head and tail as the metapage keeps them.
#[derive(Debug, Clone, Copy)]
struct Pending {
    head: u32,
    tail: u32,
    tail_free: usize,
    n_pages: u32,
    n_heap_tuples: u64,
}

impl Default for Pending {
    fn default() -> Self {
        Pending {
            head: INVALID,
            tail: INVALID,
            tail_free: 0,
            n_pages: 0,
            n_heap_tuples: 0,
        }
    }
}

/// A GIN index.
#[derive(Debug, Clone)]
pub(crate) struct Gin {
    pages: Vec<Page>,
    pending: Pending,
    stats: Stats,
    /// The index's free space map: pages `RecordFreeIndexPage` gave it,
    /// which `GinNewBuffer` takes before extending the relation.
    fsm: Fsm,
}

/// The free space map fork (`fsm.rs`, PostgreSQL's own search).
#[derive(Clone, Default)]
struct Fsm(crate::fsm::Fsm);

impl std::fmt::Debug for Fsm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fsm")
    }
}

impl Fsm {
    /// `RecordFreeIndexPage`.
    fn record_free(&mut self, blk: u32) {
        self.0.record(blk, (BLCKSZ - 1) as u32);
        self.0.touched.clear();
    }

    /// `IndexFreeSpaceMapVacuum`.
    fn vacuum(&mut self) {
        self.0.vacuum_range(0, INVALID);
        self.0.touched.clear();
    }

    /// `GetFreeIndexPage`, and `RecordUsedIndexPage` on what it found.
    fn take(&mut self, nblocks: u32) -> Option<u32> {
        let found = self.0.get_page((BLCKSZ / 2) as u32, nblocks);
        if let Some(b) = found {
            self.0.record(b, 0);
        }
        self.0.touched.clear();
        found
    }
}

/// The build accumulator (`BuildAccumulator`): each key's TIDs, in key
/// order, and the memory `ginbulk.c` counts against
/// `maintenance_work_mem`.
struct Accum {
    entries: Vec<(Key, Vec<Tid>, usize)>,
    allocated: usize,
    eas_used: usize,
    have_allocator: bool,
}

/// `AllocSetContext`'s `GetMemoryChunkSpace` for a `palloc(n)`: a power
/// of two up to `allocChunkLimit`, the MAXALIGNed size past it, and the
/// chunk header either way.
fn chunk_space(n: usize) -> usize {
    const HEADER: usize = 16;
    if n > 8192 {
        maxalign(n) + HEADER
    } else {
        n.max(8).next_power_of_two() + HEADER
    }
}

/// `sizeof(GinEntryAccumulator)`, `DEF_NENTRY`, `DEF_NPTR`.
const ACCUMULATOR: usize = 64;
const DEF_NENTRY: usize = 2048;
const DEF_NPTR: usize = 5;

impl Accum {
    fn new() -> Accum {
        Accum {
            entries: Vec::new(),
            allocated: 0,
            eas_used: 0,
            have_allocator: false,
        }
    }

    /// `ginInsertBAEntry`.
    fn insert(&mut self, ops: &dyn KeyOps, tid: Tid, key: &Key) {
        match self
            .entries
            .binary_search_by(|(k, _, _)| cmp_keys(ops, k, key))
        {
            Ok(i) => {
                let (_, list, maxcount) = &mut self.entries[i];
                if list.len() >= *maxcount {
                    self.allocated -= chunk_space(6 * *maxcount);
                    *maxcount *= 2;
                    self.allocated += chunk_space(6 * *maxcount);
                }
                list.push(tid);
            }
            Err(i) => {
                // `ginAllocEntryAccumulator`: a new chunk of 2048 nodes.
                if !self.have_allocator || self.eas_used >= DEF_NENTRY {
                    self.allocated += chunk_space(ACCUMULATOR * DEF_NENTRY);
                    self.have_allocator = true;
                    self.eas_used = 0;
                }
                self.eas_used += 1;
                if key.cat == Cat::Norm {
                    if let Some(n) = ops.datum_size(&key.val) {
                        self.allocated += chunk_space(n);
                    }
                }
                self.allocated += chunk_space(6 * DEF_NPTR);
                self.entries.insert(i, (key.clone(), vec![tid], DEF_NPTR));
            }
        }
    }

    /// `ginInsertBAEntries`: the keys of one heap tuple, middle first.
    fn insert_all(&mut self, ops: &dyn KeyOps, tid: Tid, keys: &[Key]) {
        let n = keys.len() as u32;
        if n == 0 {
            return;
        }
        let mut step = n;
        step |= step >> 1;
        step |= step >> 2;
        step |= step >> 4;
        step |= step >> 8;
        step |= step >> 16;
        step >>= 1;
        step += 1;
        while step > 0 {
            let mut i = step - 1;
            while i < n {
                self.insert(ops, tid, &keys[i as usize]);
                i += step << 1;
            }
            step >>= 1;
        }
    }

    /// `ginGetBAEntry` throughout: each key and its TIDs, sorted.
    fn drain(&mut self) -> Vec<(Key, Vec<Tid>)> {
        let out = std::mem::take(&mut self.entries)
            .into_iter()
            .map(|(k, mut l, _)| {
                l.sort_unstable();
                (k, l)
            })
            .collect();
        *self = Accum::new();
        out
    }
}

/// A leaf-placement's outcome (`GinPlaceToPageRC`).
enum Placed {
    NoWork,
    Insert,
    Split,
}

/// `ginExtractEntries`' handling of a value's keys: sorted, duplicates
/// gone, a NULL element as one `NullKey`; an empty array an `EmptyItem`,
/// a NULL value a `NullItem`.
pub(crate) fn value_keys(ops: &dyn KeyOps, value: Option<&[Bson]>) -> Vec<Key> {
    let Some(elems) = value else {
        return vec![Key::cat(Cat::NullItem)];
    };
    if elems.is_empty() {
        return vec![Key::cat(Cat::EmptyItem)];
    }
    let mut keys: Vec<Key> = elems
        .iter()
        .map(|e| match e {
            Bson::Null => Key::cat(Cat::NullKey),
            v => Key::norm(v.clone()),
        })
        .collect();
    keys.sort_by(|a, b| cmp_keys(ops, a, b));
    keys.dedup_by(|a, b| cmp_keys(ops, a, b) == Ordering::Equal);
    keys
}

/// A search for one key (`GinScanEntry`): the key, and whether it is the
/// hidden entry of a full scan (`GIN_CAT_EMPTY_QUERY`).
#[derive(Debug, Clone)]
pub(crate) struct ScanEntry {
    pub(crate) key: Key,
}

impl Gin {
    /// An empty index (`ginbuildempty`'s two pages), as TRUNCATE leaves one.
    pub(crate) fn empty() -> Gin {
        Gin {
            pages: vec![
                Page::Meta,
                Page::EntryLeaf {
                    items: Vec::new(),
                    right: INVALID,
                },
            ],
            pending: Pending::default(),
            stats: Stats::default(),
            fsm: Fsm::default(),
        }
    }

    /// `ginbuild` over `rows`, each heap tuple's TID and keys in heap
    /// order, with `maintenance_work_mem` kB for the accumulator.
    pub(crate) fn build(
        ops: &dyn KeyOps,
        rows: &[(Tid, Vec<Key>)],
        maintenance_work_mem_kb: usize,
    ) -> Gin {
        let mut g = Gin::empty();
        g.stats.n_entry_pages = 1;
        let mut accum = Accum::new();
        let mut sink = Vec::new();
        for (tid, keys) in rows {
            accum.insert_all(ops, *tid, keys);
            if accum.allocated >= maintenance_work_mem_kb * 1024 {
                for (key, list) in accum.drain() {
                    g.entry_insert(ops, &key, &list, true, &mut sink);
                }
            }
        }
        for (key, list) in accum.drain() {
            g.entry_insert(ops, &key, &list, true, &mut sink);
        }
        g.stats.n_total_pages = g.pages.len() as u32;
        g
    }

    /// The relation's pages (`RelationGetNumberOfBlocks`).
    pub(crate) fn pages(&self) -> u32 {
        self.pages.len() as u32
    }

    /// The metapage's statistics (`ginGetStats`).
    pub(crate) fn stats(&self) -> Stats {
        Stats {
            n_pending_pages: self.pending.n_pages,
            ..self.stats
        }
    }

    /// `GinNewBuffer`: a recyclable page from the free space map, else a
    /// new one at the end.
    fn new_buffer(&mut self) -> u32 {
        let nblocks = self.pages.len() as u32;
        while let Some(blk) = self.fsm.take(nblocks) {
            if matches!(self.pages[blk as usize], Page::Deleted) {
                return blk;
            }
        }
        self.pages.push(Page::Deleted);
        (self.pages.len() - 1) as u32
    }

    fn rightlink(&self, blk: u32) -> u32 {
        match &self.pages[blk as usize] {
            Page::EntryLeaf { right, .. }
            | Page::EntryInternal { right, .. }
            | Page::DataLeaf { right, .. }
            | Page::DataInternal { right, .. }
            | Page::List { right, .. } => *right,
            _ => INVALID,
        }
    }

    // ---------------------------------------------------------------
    // The entry tree.
    // ---------------------------------------------------------------

    /// `ginFindLeafPage` for an entry: the path from the root, each
    /// internal page with the offset (0-based) of the downlink taken.
    fn find_entry_leaf(&self, ops: &dyn KeyOps, key: &Key) -> Vec<(u32, usize)> {
        let mut stack = Vec::new();
        let mut blk = ROOT_BLKNO;
        loop {
            match &self.pages[blk as usize] {
                Page::EntryLeaf { .. } => {
                    stack.push((blk, 0));
                    return stack;
                }
                Page::EntryInternal { items, right } => {
                    // `entryLocateEntry`: the rightmost tuple of the
                    // rightmost page is "infinity".
                    let maxoff = items.len();
                    let mut low = 1;
                    let mut high = maxoff + 1;
                    let mut found = None;
                    while high > low {
                        let mid = low + (high - low) / 2;
                        let result = if mid == maxoff && *right == INVALID {
                            Ordering::Less
                        } else {
                            cmp_keys(ops, key, &items[mid - 1].key)
                        };
                        match result {
                            Ordering::Equal => {
                                found = Some(mid);
                                break;
                            }
                            Ordering::Greater => low = mid + 1,
                            Ordering::Less => high = mid,
                        }
                    }
                    let off = found.unwrap_or(high).min(maxoff);
                    stack.push((blk, off - 1));
                    blk = items[off - 1].child;
                }
                _ => return stack,
            }
        }
    }

    /// `entryLocateLeafEntry`: whether `key` is on the leaf, and its
    /// (0-based) place.
    fn locate_leaf(&self, ops: &dyn KeyOps, blk: u32, key: &Key) -> (bool, usize) {
        let Page::EntryLeaf { items, .. } = &self.pages[blk as usize] else {
            return (false, 0);
        };
        let mut low = 0;
        let mut high = items.len();
        while high > low {
            let mid = low + (high - low) / 2;
            match cmp_keys(ops, key, &items[mid].key) {
                Ordering::Equal => return (true, mid),
                Ordering::Greater => low = mid + 1,
                Ordering::Less => high = mid,
            }
        }
        (false, high)
    }

    /// A leaf tuple of `key` with its posting list, or `None` when the
    /// list does not fit (`ginCompressPostingList` into `GinMaxItemSize`,
    /// then `GinFormTuple`).
    fn list_tuple(ops: &dyn KeyOps, key: &Key, items: &[Tid]) -> Option<ETuple> {
        let (packed, list) = compress(items, MAX_ITEM);
        let size = maxalign(key_offset(ops, key) + list);
        if size > MAX_ITEM || packed != items.len() {
            return None;
        }
        Some(ETuple {
            key: key.clone(),
            posting: Posting::List(items.to_vec()),
            child: INVALID,
            size,
        })
    }

    fn tree_tuple(ops: &dyn KeyOps, key: &Key, root: u32) -> ETuple {
        ETuple {
            key: key.clone(),
            posting: Posting::Tree(root),
            child: INVALID,
            size: bare_size(ops, key),
        }
    }

    /// `GinFormInteriorTuple`: a downlink to `child` keyed as `t`.
    fn interior(ops: &dyn KeyOps, t: &ETuple, leaf: bool, child: u32) -> ETuple {
        let size = match (&t.posting, leaf) {
            (Posting::List(_), true) => maxalign(key_offset(ops, &t.key)),
            _ => t.size,
        };
        ETuple {
            key: t.key.clone(),
            posting: Posting::Tree(INVALID),
            child,
            size,
        }
    }

    /// `ginEntryInsert`: `items` (sorted) for `key`.
    fn entry_insert(
        &mut self,
        ops: &dyn KeyOps,
        key: &Key,
        items: &[Tid],
        build: bool,
        ev: &mut Vec<Event>,
    ) {
        let stack = self.find_entry_leaf(ops, key);
        let (leaf, _) = *stack.last().expect("a leaf");
        let (found, off) = self.locate_leaf(ops, leaf, key);
        let (tuple, is_delete) = if found {
            let old = match &self.pages[leaf as usize] {
                Page::EntryLeaf { items, .. } => items[off].clone(),
                _ => unreachable!("an entry leaf"),
            };
            match &old.posting {
                Posting::Tree(root) => {
                    self.insert_item_pointers(*root, items, build, ev);
                    return;
                }
                Posting::List(old_items) => {
                    ev.push(Event::ConflictIn(leaf));
                    // `addItemPointersToLeafTuple`.
                    let merged = merge(items, old_items);
                    let t = match Self::list_tuple(ops, key, &merged) {
                        Some(t) => t,
                        None => {
                            let root = self.create_posting_tree(old_items, build, leaf, ev);
                            self.insert_item_pointers(root, items, build, ev);
                            Self::tree_tuple(ops, key, root)
                        }
                    };
                    (t, true)
                }
            }
        } else {
            ev.push(Event::ConflictIn(leaf));
            // `buildFreshLeafTuple`.
            let t = match Self::list_tuple(ops, key, items) {
                Some(t) => t,
                None => {
                    let root = self.create_posting_tree(items, build, leaf, ev);
                    Self::tree_tuple(ops, key, root)
                }
            };
            if build {
                self.stats.n_entries += 1;
            }
            (t, false)
        };
        self.entry_place(
            ops,
            &stack,
            stack.len() - 1,
            tuple,
            is_delete,
            off,
            None,
            build,
            ev,
        );
    }

    /// Free space on an entry or list page (`PageGetFreeSpace`).
    fn entry_free(items: &[ETuple]) -> usize {
        let used: usize = items.iter().map(|t| maxalign(t.size) + LP).sum();
        (PAGE_UPPER - PAGE_HEADER).saturating_sub(used + LP)
    }

    /// `ginPlaceToPage` on the entry page at `stack[level]`: `tuple` goes
    /// in at `off`, replacing the one there when `is_delete`; on an
    /// internal page the downlink at `off` first moves to `update`. A
    /// split is finished up the stack (`ginFinishSplit`).
    #[allow(clippy::too_many_arguments)]
    fn entry_place(
        &mut self,
        ops: &dyn KeyOps,
        stack: &[(u32, usize)],
        level: usize,
        tuple: ETuple,
        is_delete: bool,
        off: usize,
        update: Option<u32>,
        build: bool,
        ev: &mut Vec<Event>,
    ) {
        let (blk, _) = stack[level];
        let (mut items, right, leaf) = match &self.pages[blk as usize] {
            Page::EntryLeaf { items, right } => (items.clone(), *right, true),
            Page::EntryInternal { items, right } => (items.clone(), *right, false),
            _ => unreachable!("an entry page"),
        };
        // `entryIsEnoughSpace`.
        let released = if is_delete {
            maxalign(items[off].size) + LP
        } else {
            0
        };
        let fits = Self::entry_free(&items) + released >= maxalign(tuple.size) + LP;
        // `entryPreparePage`.
        if is_delete {
            items.remove(off);
        }
        if let Some(u) = update {
            items[off].child = u;
        }
        items.insert(off, tuple);
        if fits {
            self.pages[blk as usize] = if leaf {
                Page::EntryLeaf { items, right }
            } else {
                Page::EntryInternal { items, right }
            };
            return;
        }
        // `entrySplitPage`: equal halves of the data, by size.
        let total: usize = items.iter().map(|t| maxalign(t.size) + LP).sum();
        let mut lsize = 0;
        let mut split = items.len();
        for (i, t) in items.iter().enumerate() {
            if lsize > total / 2 {
                split = i;
                break;
            }
            lsize += maxalign(t.size) + LP;
        }
        let rpart = items.split_off(split);
        let lpart = items;
        let finish = self.split_pages(
            stack,
            level,
            build,
            ev,
            |g, lblk, rblk| {
                let lt = Self::interior(ops, lpart.last().expect("left"), leaf, lblk);
                let rt = Self::interior(ops, rpart.last().expect("right"), leaf, rblk);
                let lpage = |r: u32| {
                    if leaf {
                        Page::EntryLeaf {
                            items: lpart.clone(),
                            right: r,
                        }
                    } else {
                        Page::EntryInternal {
                            items: lpart.clone(),
                            right: r,
                        }
                    }
                };
                let rpage = |r: u32| {
                    if leaf {
                        Page::EntryLeaf {
                            items: rpart.clone(),
                            right: r,
                        }
                    } else {
                        Page::EntryInternal {
                            items: rpart.clone(),
                            right: r,
                        }
                    }
                };
                g.pages[lblk as usize] = lpage(rblk);
                (rpage, lt, rt, leaf)
            },
            right,
            |g, root, lt, rt| {
                g.pages[root as usize] = Page::EntryInternal {
                    items: vec![lt, rt],
                    right: INVALID,
                };
            },
        );
        if let Some((plevel, lt, rblk)) = finish {
            // `ginFinishSplit`: the left page's downlink goes in at the
            // parent's downlink to it, which moves to the right page.
            let (pblk, _) = stack[plevel];
            let child = stack[plevel + 1].0;
            let poff = match &self.pages[pblk as usize] {
                Page::EntryInternal { items, .. } => items
                    .iter()
                    .position(|t| t.child == child)
                    .expect("the parent's downlink"),
                _ => unreachable!("an internal entry page"),
            };
            self.entry_place(ops, stack, plevel, lt, false, poff, Some(rblk), build, ev);
        }
    }

    /// The split half of `ginPlaceToPage`, for either tree: the right page
    /// is allocated first, then (a root split) the left; the predicate
    /// locks of a leaf follow it; a root becomes an internal page over the
    /// two; any other page's split is left to finish in its parent, at
    /// the level returned with the left page's downlink and the right
    /// page.
    #[allow(clippy::too_many_arguments)]
    fn split_pages<T, R>(
        &mut self,
        stack: &[(u32, usize)],
        level: usize,
        build: bool,
        ev: &mut Vec<Event>,
        fill_left: impl FnOnce(&mut Gin, u32, u32) -> (R, T, T, bool),
        old_right: u32,
        fill_root: impl FnOnce(&mut Gin, u32, T, T),
    ) -> Option<(usize, T, u32)>
    where
        R: Fn(u32) -> Page,
    {
        let (blk, _) = stack[level];
        let data = matches!(
            self.pages[blk as usize],
            Page::DataLeaf { .. } | Page::DataInternal { .. }
        );
        let rblk = self.new_buffer();
        let count = |g: &mut Gin| {
            if build {
                if data {
                    g.stats.n_data_pages += 1;
                } else {
                    g.stats.n_entry_pages += 1;
                }
            }
        };
        count(self);
        if level == 0 {
            // A root split.
            let lblk = self.new_buffer();
            count(self);
            let (rpage, lt, rt, leaf) = fill_left(self, lblk, rblk);
            self.pages[rblk as usize] = rpage(INVALID);
            if leaf {
                ev.push(Event::Split(blk, lblk));
                ev.push(Event::Split(blk, rblk));
            }
            fill_root(self, blk, lt, rt);
            return None;
        }
        let (rpage, lt, _rt, leaf) = fill_left(self, blk, rblk);
        self.pages[rblk as usize] = rpage(old_right);
        if leaf {
            ev.push(Event::Split(blk, rblk));
        }
        Some((level - 1, lt, rblk))
    }

    // ---------------------------------------------------------------
    // Posting trees.
    // ---------------------------------------------------------------

    /// `createPostingTree`: as many of `items` as fit on the root, in
    /// segments of at most `GinPostingListSegmentMaxSize`; the rest
    /// inserted. The entry page's locks are copied to the new root.
    fn create_posting_tree(
        &mut self,
        items: &[Tid],
        build: bool,
        entry: u32,
        ev: &mut Vec<Event>,
    ) -> u32 {
        let mut segs = Vec::new();
        let mut n = 0;
        let mut size = 0;
        while n < items.len() {
            let (packed, segsize) = compress(&items[n..], SEG_MAX);
            if size + segsize > DATA_MAX {
                break;
            }
            segs.push(Seg {
                items: items[n..n + packed].to_vec(),
                size: Some(segsize),
                action: SegAction::Unmodified,
            });
            size += segsize;
            n += packed;
        }
        let blk = self.new_buffer();
        ev.push(Event::Split(entry, blk));
        self.pages[blk as usize] = Page::DataLeaf {
            segs,
            right: INVALID,
            rbound: 0,
        };
        if build {
            self.stats.n_data_pages += 1;
        }
        if n < items.len() {
            self.insert_item_pointers(blk, &items[n..], build, ev);
        }
        blk
    }

    /// `ginInsertItemPointers`: each descent checks the root for
    /// conflicts in, and places as many items as the leaf takes.
    fn insert_item_pointers(&mut self, root: u32, items: &[Tid], build: bool, ev: &mut Vec<Event>) {
        let mut cur = 0;
        while cur < items.len() {
            ev.push(Event::ConflictIn(root));
            let stack = self.find_data_leaf(root, items[cur]);
            self.data_leaf_insert(&stack, items, &mut cur, build, ev);
        }
    }

    /// `ginFindLeafPage` in a posting tree (`dataLocateItem`, whose last
    /// item is always "infinity").
    fn find_data_leaf(&self, root: u32, item: Tid) -> Vec<(u32, usize)> {
        let mut stack = Vec::new();
        let mut blk = root;
        loop {
            match &self.pages[blk as usize] {
                Page::DataInternal { items, .. } => {
                    let maxoff = items.len();
                    let mut low = 1;
                    let mut high = maxoff + 1;
                    let mut found = None;
                    while high > low {
                        let mid = low + (high - low) / 2;
                        let result = if mid == maxoff {
                            Ordering::Less
                        } else {
                            item.cmp(&items[mid - 1].0)
                        };
                        match result {
                            Ordering::Equal => {
                                found = Some(mid);
                                break;
                            }
                            Ordering::Greater => low = mid + 1,
                            Ordering::Less => high = mid,
                        }
                    }
                    let off = found.unwrap_or(high).min(maxoff);
                    stack.push((blk, off - 1));
                    blk = items[off - 1].1;
                }
                _ => {
                    stack.push((blk, 0));
                    return stack;
                }
            }
        }
    }

    /// `ginInsertValue` on a posting tree leaf.
    fn data_leaf_insert(
        &mut self,
        stack: &[(u32, usize)],
        items: &[Tid],
        cur: &mut usize,
        build: bool,
        ev: &mut Vec<Event>,
    ) {
        let level = stack.len() - 1;
        let (blk, _) = stack[level];
        let Page::DataLeaf {
            segs,
            right,
            rbound,
        } = self.pages[blk as usize].clone()
        else {
            unreachable!("a posting tree leaf")
        };
        match self.data_leaf_place(segs, right, rbound, items, cur, build) {
            (Placed::NoWork, _) => {}
            (Placed::Insert, (lsegs, _, _)) => {
                self.pages[blk as usize] = Page::DataLeaf {
                    segs: lsegs,
                    right,
                    rbound,
                };
            }
            (Placed::Split, (lsegs, rsegs, lbound)) => {
                let finish = self.split_pages(
                    stack,
                    level,
                    build,
                    ev,
                    |g, lblk, rblk| {
                        g.pages[lblk as usize] = Page::DataLeaf {
                            segs: lsegs,
                            right: rblk,
                            rbound: lbound,
                        };
                        let rpage = move |r: u32| Page::DataLeaf {
                            segs: rsegs.clone(),
                            right: r,
                            rbound,
                        };
                        ((rpage), (lbound, lblk), (rbound, rblk), true)
                    },
                    right,
                    |g, root, lt, rt| {
                        g.pages[root as usize] = Page::DataInternal {
                            items: vec![lt, rt],
                            right: INVALID,
                            rbound: 0,
                        };
                    },
                );
                if let Some((plevel, lt, rblk)) = finish {
                    self.data_finish_split(stack, plevel, lt, rblk, build, ev);
                }
            }
        }
    }

    /// `ginFinishSplit` in a posting tree: the left page's downlink (its
    /// right bound) goes in at the parent's downlink to it.
    fn data_finish_split(
        &mut self,
        stack: &[(u32, usize)],
        level: usize,
        downlink: (Tid, u32),
        rblk: u32,
        build: bool,
        ev: &mut Vec<Event>,
    ) {
        let (pblk, _) = stack[level];
        let child = stack[level + 1].0;
        let Page::DataInternal {
            mut items,
            right,
            rbound,
        } = self.pages[pblk as usize].clone()
        else {
            unreachable!("an internal posting tree page")
        };
        let off = items
            .iter()
            .position(|(_, c)| *c == child)
            .expect("the parent's downlink");
        // `dataBeginPlaceToPageInternal`.
        if DATA_MAX - items.len() * POSTING_ITEM >= POSTING_ITEM {
            items[off].1 = rblk;
            items.insert(off, downlink);
            self.pages[pblk as usize] = Page::DataInternal {
                items,
                right,
                rbound,
            };
            return;
        }
        // `dataSplitPageInternal`.
        items[off].1 = rblk;
        items.insert(off, downlink);
        let separator = if build && right == INVALID {
            DATA_MAX / POSTING_ITEM
        } else {
            items.len() / 2
        };
        let ritems = items.split_off(separator);
        let litems = items;
        let lbound = litems.last().expect("left").0;
        let finish = self.split_pages(
            stack,
            level,
            build,
            ev,
            |g, lblk, rb| {
                g.pages[lblk as usize] = Page::DataInternal {
                    items: litems,
                    right: rb,
                    rbound: lbound,
                };
                let rpage = move |r: u32| Page::DataInternal {
                    items: ritems.clone(),
                    right: r,
                    rbound,
                };
                (rpage, (lbound, lblk), (rbound, rb), false)
            },
            right,
            |g, root, lt, rt| {
                g.pages[root as usize] = Page::DataInternal {
                    items: vec![lt, rt],
                    right: INVALID,
                    rbound: 0,
                };
            },
        );
        if let Some((plevel, lt, rb)) = finish {
            self.data_finish_split(stack, plevel, lt, rb, build, ev);
        }
    }

    /// `dataBeginPlaceToPageLeaf`: the leaf's segments with as many of
    /// `items[cur..]` as belong and fit, `cur` advanced past them. On a
    /// split, the left and right segments and the left's new right bound.
    #[allow(clippy::type_complexity)]
    fn data_leaf_place(
        &self,
        segs: Vec<Seg>,
        right: u32,
        rbound: Tid,
        items: &[Tid],
        cur: &mut usize,
        build: bool,
    ) -> (Placed, (Vec<Seg>, Vec<Seg>, Tid)) {
        let new = &items[*cur..];
        let mut maxitems = new.len();
        if right != INVALID {
            maxitems = new.iter().take_while(|t| **t <= rbound).count();
        }
        let mut leaf: Vec<Seg> = segs
            .into_iter()
            .map(|s| Seg {
                action: SegAction::Unmodified,
                ..s
            })
            .collect();
        let (append, max_old) = match leaf.last() {
            Some(last) => {
                let m = *last.items.last().expect("a segment's items");
                (new[0] >= m, m)
            }
            None => (true, 0),
        };
        let used: usize = leaf.iter().filter_map(|s| s.size).sum();
        let freespace = DATA_MAX - used;
        if append {
            maxitems = maxitems.min(freespace + DATA_MAX);
        } else {
            let nnew = freespace / SEG_MAX + DATA_MAX / SEG_MAX;
            maxitems = maxitems.min(nnew * MIN_TUPLES_PER_SEGMENT);
        }
        if !add_items_to_leaf(&mut leaf, &new[..maxitems]) {
            *cur += maxitems;
            return (Placed::NoWork, (Vec::new(), Vec::new(), 0));
        }
        let (needsplit, remaining, mut lastleft, mut lsize, mut rsize) = leaf_repack(&mut leaf);
        if let Some(rem) = remaining {
            debug_assert!(append && max_old < rem);
            maxitems = new[..maxitems].iter().take_while(|t| **t < rem).count();
        }
        *cur += maxitems;
        if !needsplit {
            leaf.retain(|s| s.action != SegAction::Delete);
            return (Placed::Insert, (leaf, Vec::new(), 0));
        }
        if !build {
            // Even halves, or a left page three quarters full when
            // appending.
            while lastleft > 0 {
                let info = &leaf[lastleft];
                if info.action != SegAction::Delete {
                    let segsize = info.size.expect("packed") as i64;
                    if (lsize - segsize) - (rsize + segsize) < 0 {
                        break;
                    }
                    if append && (lsize - segsize) < (BLCKSZ as i64 * 3) / 4 {
                        break;
                    }
                    lsize -= segsize;
                    rsize += segsize;
                }
                lastleft -= 1;
            }
        }
        let lbound = leaf[..=lastleft]
            .iter()
            .rev()
            .find(|s| s.action != SegAction::Delete)
            .and_then(|s| s.items.last().copied())
            .unwrap_or(0);
        let rsegs: Vec<Seg> = leaf
            .split_off(lastleft + 1)
            .into_iter()
            .filter(|s| s.action != SegAction::Delete)
            .collect();
        leaf.retain(|s| s.action != SegAction::Delete);
        (Placed::Split, (leaf, rsegs, lbound))
    }

    // ---------------------------------------------------------------
    // Inserts.
    // ---------------------------------------------------------------

    /// `gininsert` of one heap tuple's keys: into the pending list with
    /// `fastupdate` (`ginHeapTupleFastInsert`, cleaned up once it passes
    /// `pending_limit_kb`, with `work_mem_kb` for the cleanup), else each
    /// into the tree (`ginHeapTupleInsert`).
    pub(crate) fn insert(
        &mut self,
        ops: &dyn KeyOps,
        tid: Tid,
        keys: &[Key],
        fastupdate: bool,
        pending_limit_kb: usize,
        work_mem_kb: usize,
    ) -> Vec<Event> {
        let mut ev = Vec::new();
        if fastupdate {
            self.fast_insert(ops, tid, keys, &mut ev);
            if self.pending.n_pages as usize * LIST_PAGE_SIZE > pending_limit_kb * 1024 {
                self.insert_cleanup(ops, false, true, work_mem_kb, &mut ev);
            }
        } else {
            for key in keys {
                self.entry_insert(ops, key, &[tid], false, &mut ev);
            }
        }
        ev
    }

    /// `ginHeapTupleFastInsert`.
    fn fast_insert(&mut self, ops: &dyn KeyOps, tid: Tid, keys: &[Key], ev: &mut Vec<Event>) {
        if keys.is_empty() {
            return;
        }
        let tuples: Vec<PTuple> = keys
            .iter()
            .map(|k| PTuple {
                key: k.clone(),
                tid,
                size: bare_size(ops, k),
            })
            .collect();
        let sumsize: usize = tuples.iter().map(|t| t.size).sum();
        let need = sumsize + tuples.len() * LP;
        let separate =
            need > LIST_PAGE_SIZE || self.pending.head == INVALID || need > self.pending.tail_free;
        if separate {
            // `makeSublist`.
            let mut pages: Vec<(u32, Vec<PTuple>)> = Vec::new();
            let mut size = 0;
            for t in tuples {
                let tupsize = maxalign(t.size) + LP;
                if pages.is_empty() || size + tupsize > LIST_PAGE_SIZE {
                    let blk = self.new_buffer();
                    pages.push((blk, Vec::new()));
                    size = 0;
                }
                size += tupsize;
                pages.last_mut().expect("a page").1.push(t);
            }
            let n = pages.len();
            let head = pages[0].0;
            let mut tail_free = 0;
            for i in 0..n {
                let blk = pages[i].0;
                let right = if i + 1 < n { pages[i + 1].0 } else { INVALID };
                let tuples = std::mem::take(&mut pages[i].1);
                let last = right == INVALID;
                if last {
                    tail_free = LIST_PAGE_SIZE
                        - tuples.iter().map(|t| maxalign(t.size) + LP).sum::<usize>();
                }
                self.pages[blk as usize] = Page::List {
                    tuples,
                    right,
                    heap_tuples: u32::from(last),
                    full_row: last,
                };
            }
            ev.push(Event::ConflictIn(META_BLKNO));
            let tail = pages[n - 1].0;
            if self.pending.head == INVALID {
                self.pending = Pending {
                    head,
                    tail,
                    tail_free,
                    n_pages: n as u32,
                    n_heap_tuples: 1,
                };
            } else {
                if let Page::List { right, .. } = &mut self.pages[self.pending.tail as usize] {
                    *right = head;
                }
                self.pending.tail = tail;
                self.pending.tail_free = tail_free;
                self.pending.n_pages += n as u32;
                self.pending.n_heap_tuples += 1;
            }
        } else {
            ev.push(Event::ConflictIn(META_BLKNO));
            let tail = self.pending.tail as usize;
            if let Page::List {
                tuples: page,
                heap_tuples,
                ..
            } = &mut self.pages[tail]
            {
                *heap_tuples += 1;
                page.extend(tuples);
                self.pending.tail_free =
                    LIST_PAGE_SIZE - page.iter().map(|t| maxalign(t.size) + LP).sum::<usize>();
            }
            self.pending.n_heap_tuples += 1;
        }
    }

    /// `ginInsertCleanup`: the pending list's tuples moved into the tree a
    /// collection at a time (each `work_mem_kb`, or to the list's end),
    /// and their pages deleted (`shiftList`), into the free space map with
    /// `fill_fsm`.
    pub(crate) fn insert_cleanup(
        &mut self,
        ops: &dyn KeyOps,
        full_clean: bool,
        fill_fsm: bool,
        work_mem_kb: usize,
        ev: &mut Vec<Event>,
    ) {
        if self.pending.head == INVALID {
            return;
        }
        let finish = self.pending.tail;
        let mut blk = self.pending.head;
        let mut accum = Accum::new();
        let mut flushed = false;
        loop {
            let cleanup_finish = blk == finish && !full_clean;
            let (tuples, right, full_row) = match &self.pages[blk as usize] {
                Page::List {
                    tuples,
                    right,
                    full_row,
                    ..
                } => (tuples.clone(), *right, *full_row),
                _ => break,
            };
            // `processPendingPage`: each heap tuple's keys together.
            let mut i = 0;
            while i < tuples.len() {
                let tid = tuples[i].tid;
                let mut keys = Vec::new();
                while i < tuples.len() && tuples[i].tid == tid {
                    keys.push(tuples[i].key.clone());
                    i += 1;
                }
                accum.insert_all(ops, tid, &keys);
            }
            if right == INVALID || (full_row && accum.allocated >= work_mem_kb * 1024) {
                for (key, list) in accum.drain() {
                    self.entry_insert(ops, &key, &list, false, ev);
                }
                // `shiftList` up to the next page.
                let mut b = self.pending.head;
                let mut deleted = Vec::new();
                while b != right {
                    let (r, ht) = match &self.pages[b as usize] {
                        Page::List {
                            right, heap_tuples, ..
                        } => (*right, *heap_tuples),
                        _ => break,
                    };
                    self.pending.n_pages -= 1;
                    self.pending.n_heap_tuples =
                        self.pending.n_heap_tuples.saturating_sub(u64::from(ht));
                    self.pages[b as usize] = Page::Deleted;
                    deleted.push(b);
                    b = r;
                }
                self.pending.head = right;
                if right == INVALID {
                    self.pending = Pending::default();
                }
                if fill_fsm {
                    for b in deleted {
                        self.fsm.record_free(b);
                    }
                    flushed = true;
                }
                if right == INVALID || cleanup_finish {
                    break;
                }
            }
            blk = right;
        }
        if flushed && fill_fsm {
            self.fsm.vacuum();
        }
    }

    /// VACUUM's index pass (`ginbulkdelete`, `ginvacuumcleanup`): the
    /// pending list moved into the tree (its pages not yet in the free
    /// space map), the dead TIDs taken from the entry tuples and posting
    /// trees, the deleted pages recorded free, and the statistics
    /// counted afresh (`ginUpdateStats`).
    pub(crate) fn vacuum(
        &mut self,
        ops: &dyn KeyOps,
        dead: &dyn Fn(Tid) -> bool,
        work_mem_kb: usize,
    ) {
        let mut ev = Vec::new();
        let pending_pages: Vec<u32> = self.list_pages();
        self.insert_cleanup(ops, true, false, work_mem_kb, &mut ev);
        for page in &mut self.pages {
            match page {
                Page::EntryLeaf { items, .. } => {
                    for t in items.iter_mut() {
                        if let Posting::List(l) = &mut t.posting {
                            l.retain(|x| !dead(*x));
                            let (_, list) = if l.is_empty() {
                                (0, 0)
                            } else {
                                compress(l, MAX_ITEM)
                            };
                            t.size = maxalign(key_offset(ops, &t.key) + list);
                        }
                    }
                }
                Page::DataLeaf { segs, .. } => {
                    for s in segs.iter_mut() {
                        let before = s.items.len();
                        s.items.retain(|x| !dead(*x));
                        if s.items.len() != before {
                            s.size = (!s.items.is_empty()).then(|| compress(&s.items, BLCKSZ).1);
                        }
                    }
                    segs.retain(|s| !s.items.is_empty());
                }
                _ => {}
            }
        }
        // `ginvacuumcleanup`: every deleted page is recyclable.
        let _ = pending_pages;
        let deleted: Vec<u32> = self
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p, Page::Deleted))
            .map(|(i, _)| i as u32)
            .collect();
        for b in deleted {
            self.fsm.record_free(b);
        }
        self.fsm.vacuum();
        let mut s = Stats {
            n_total_pages: self.pages.len() as u32,
            ..Stats::default()
        };
        for p in &self.pages {
            match p {
                Page::EntryLeaf { items, .. } => {
                    s.n_entry_pages += 1;
                    s.n_entries += items.len() as u64;
                }
                Page::EntryInternal { .. } => s.n_entry_pages += 1,
                Page::DataLeaf { .. } | Page::DataInternal { .. } => s.n_data_pages += 1,
                _ => {}
            }
        }
        self.stats = s;
    }

    fn list_pages(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut b = self.pending.head;
        while b != INVALID {
            out.push(b);
            b = self.rightlink(b);
        }
        out
    }

    // ---------------------------------------------------------------
    // Scans.
    // ---------------------------------------------------------------

    /// The pages a bitmap scan for `entries` locks (`gingetbitmap`): the
    /// metapage (`scanPendingInsert`), then for each entry the leaf its
    /// key is on or would be on -- or, when the key has a posting tree,
    /// that tree's root (`startScanEntry`). A full-scan entry walks the
    /// leaves from the first, locking each, to the first NULL item
    /// (`collectMatchBitmap`), with each posting tree root it meets.
    pub(crate) fn scan_locks(&self, ops: &dyn KeyOps, entries: &[ScanEntry]) -> Vec<u32> {
        let mut locks = vec![META_BLKNO];
        for e in entries {
            let stack = self.find_entry_leaf(ops, &e.key);
            let Some(&(leaf, _)) = stack.last() else {
                continue;
            };
            if e.key.cat == Cat::EmptyQuery {
                locks.push(leaf);
                let (_, mut off) = self.locate_leaf(ops, leaf, &e.key);
                let mut blk = leaf;
                'walk: while let Page::EntryLeaf { items, right } = &self.pages[blk as usize] {
                    while off < items.len() {
                        let t = &items[off];
                        if t.key.cat == Cat::NullItem {
                            break 'walk;
                        }
                        if let Posting::Tree(root) = t.posting {
                            locks.push(root);
                        }
                        off += 1;
                    }
                    if *right == INVALID {
                        break;
                    }
                    blk = *right;
                    off = 0;
                    locks.push(blk);
                }
                continue;
            }
            let (found, off) = self.locate_leaf(ops, leaf, &e.key);
            match &self.pages[leaf as usize] {
                Page::EntryLeaf { items, .. } if found => match items[off].posting {
                    Posting::Tree(root) => locks.push(root),
                    Posting::List(_) => locks.push(leaf),
                },
                _ => locks.push(leaf),
            }
        }
        locks
    }

    /// Every TID the index holds for `key`, in the tree and in the
    /// pending list.
    pub(crate) fn key_tids(&self, ops: &dyn KeyOps, key: &Key) -> Vec<Tid> {
        let mut out = Vec::new();
        let stack = self.find_entry_leaf(ops, key);
        if let Some(&(leaf, _)) = stack.last() {
            let (found, off) = self.locate_leaf(ops, leaf, key);
            if let Page::EntryLeaf { items, .. } = &self.pages[leaf as usize] {
                if found {
                    match &items[off].posting {
                        Posting::List(l) => out.extend_from_slice(l),
                        Posting::Tree(root) => self.tree_tids(*root, &mut out),
                    }
                }
            }
        }
        for b in self.list_pages() {
            if let Page::List { tuples, .. } = &self.pages[b as usize] {
                out.extend(
                    tuples
                        .iter()
                        .filter(|t| cmp_keys(ops, &t.key, key) == Ordering::Equal)
                        .map(|t| t.tid),
                );
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Every TID a full scan (`GIN_SEARCH_MODE_ALL`) returns: those of
    /// every entry before the NULL items, and the pending list's.
    pub(crate) fn all_tids(&self, ops: &dyn KeyOps) -> Vec<Tid> {
        let _ = ops;
        let mut out = Vec::new();
        let mut blk = ROOT_BLKNO;
        while let Page::EntryInternal { items, .. } = &self.pages[blk as usize] {
            blk = items[0].child;
        }
        while blk != INVALID {
            let Page::EntryLeaf { items, right } = &self.pages[blk as usize] else {
                break;
            };
            for t in items {
                if t.key.cat == Cat::NullItem {
                    continue;
                }
                match &t.posting {
                    Posting::List(l) => out.extend_from_slice(l),
                    Posting::Tree(root) => self.tree_tids(*root, &mut out),
                }
            }
            blk = *right;
        }
        for b in self.list_pages() {
            if let Page::List { tuples, .. } = &self.pages[b as usize] {
                out.extend(
                    tuples
                        .iter()
                        .filter(|t| t.key.cat != Cat::NullItem)
                        .map(|t| t.tid),
                );
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn tree_tids(&self, blk: u32, out: &mut Vec<Tid>) {
        match &self.pages[blk as usize] {
            Page::DataLeaf { segs, .. } => {
                for s in segs {
                    out.extend_from_slice(&s.items);
                }
            }
            Page::DataInternal { items, .. } => {
                for (_, c) in items {
                    self.tree_tids(*c, out);
                }
            }
            _ => {}
        }
    }

    /// The pages of each kind, by block, for tests: `M` metapage, `E`/`e`
    /// entry internal/leaf, `D`/`d` posting tree internal/leaf, `L`
    /// pending list, `-` deleted.
    #[cfg(test)]
    fn layout(&self) -> String {
        self.pages
            .iter()
            .map(|p| match p {
                Page::Meta => 'M',
                Page::EntryInternal { .. } => 'E',
                Page::EntryLeaf { .. } => 'e',
                Page::DataInternal { .. } => 'D',
                Page::DataLeaf { .. } => 'd',
                Page::List { .. } => 'L',
                Page::Deleted => '-',
            })
            .collect()
    }
}

/// `addItemsToLeaf`: each new item into the segment it belongs in; when
/// appending past a segment already `GinPostingListSegmentTargetSize`
/// long, into a new segment. `false` when every item was already there.
fn add_items_to_leaf(leaf: &mut Vec<Seg>, new: &[Tid]) -> bool {
    if leaf.is_empty() {
        leaf.push(Seg {
            items: new.to_vec(),
            size: None,
            action: SegAction::Insert,
        });
        return true;
    }
    let mut next = 0;
    let mut modified = false;
    let mut i = 0;
    while i < leaf.len() {
        let left = new.len() - next;
        let nthis = if i + 1 == leaf.len() {
            left
        } else {
            let first = leaf[i + 1].items[0];
            new[next..].iter().take_while(|t| **t < first).count()
        };
        if nthis == 0 {
            i += 1;
            continue;
        }
        let cur = &leaf[i];
        if i + 1 == leaf.len()
            && *cur.items.last().expect("items") < new[next]
            && cur.size.is_some_and(|s| s >= SEG_TARGET)
        {
            leaf.push(Seg {
                items: new[next..next + nthis].to_vec(),
                size: None,
                action: SegAction::Insert,
            });
            modified = true;
            break;
        }
        let merged = merge(&cur.items, &new[next..next + nthis]);
        if merged.len() != cur.items.len() {
            let cur = &mut leaf[i];
            cur.action =
                if merged.len() == nthis + cur.items.len() && cur.action == SegAction::Unmodified {
                    SegAction::AddItems
                } else {
                    SegAction::Replace
                };
            cur.items = merged;
            cur.size = None;
            modified = true;
        }
        next += nthis;
        if next == new.len() {
            break;
        }
        i += 1;
    }
    modified
}

/// `leafRepackItems`: each modified segment compressed (into pieces of
/// `GinPostingListSegmentTargetSize` when it outgrows the maximum), each
/// small one merged into the next, and the segments laid onto one page or
/// two. Returns whether it splits, the first item that fit on neither
/// page, the last segment on the left, and the two pages' sizes.
fn leaf_repack(leaf: &mut Vec<Seg>) -> (bool, Option<Tid>, usize, i64, i64) {
    let mut pgused = 0usize;
    let mut needsplit = false;
    let mut lastleft = 0usize;
    let mut lsize = 0usize;
    let mut remaining = None;
    let mut i = 0;
    while i < leaf.len() {
        if leaf[i].action != SegAction::Delete {
            if leaf[i].size.is_none() {
                let nitems = leaf[i].items.len();
                let npacked = if nitems > SEG_MAX {
                    0
                } else {
                    let (np, size) = compress(&leaf[i].items, SEG_MAX);
                    leaf[i].size = Some(size);
                    np
                };
                if npacked != nitems {
                    let (np, size) = compress(&leaf[i].items, SEG_TARGET);
                    let tail = leaf[i].items.split_off(np);
                    leaf[i].size = Some(size);
                    if leaf[i].action != SegAction::Insert {
                        leaf[i].action = SegAction::Replace;
                    }
                    leaf.insert(
                        i + 1,
                        Seg {
                            items: tail,
                            size: None,
                            action: SegAction::Insert,
                        },
                    );
                }
            }
            if leaf[i].size.expect("packed") < SEG_MIN && i + 1 < leaf.len() {
                let merged = merge(&leaf[i].items, &leaf[i + 1].items);
                leaf[i + 1].items = merged;
                leaf[i + 1].size = None;
                leaf[i + 1].action = SegAction::Replace;
                if leaf[i].action == SegAction::Insert {
                    leaf.remove(i);
                    continue;
                }
                leaf[i].action = SegAction::Delete;
                leaf[i].size = None;
            }
        }
        if leaf[i].action == SegAction::Delete {
            i += 1;
            continue;
        }
        let segsize = leaf[i].size.expect("packed");
        if pgused + segsize > DATA_MAX {
            if !needsplit {
                lastleft = i - 1;
                needsplit = true;
                lsize = pgused;
                pgused = 0;
            } else {
                remaining = Some(leaf[i].items[0]);
                leaf.truncate(i);
                break;
            }
        }
        pgused += segsize;
        i += 1;
    }
    let (lsize, rsize) = if needsplit {
        (lsize, pgused)
    } else {
        (pgused, 0)
    };
    (needsplit, remaining, lastleft, lsize as i64, rsize as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `int4`'s `array_ops`.
    struct Int4;

    impl KeyOps for Int4 {
        fn cmp(&self, a: &Bson, b: &Bson) -> Ordering {
            a.as_i32().cmp(&b.as_i32())
        }
        fn key_size(&self, _: &Bson) -> usize {
            16
        }
        fn datum_size(&self, _: &Bson) -> Option<usize> {
            None
        }
    }

    fn tid(blk: u64, off: u64) -> Tid {
        (blk << 16) | off
    }

    /// The heap of `predicate-gin`'s setup: 8,192 rows of `{1}` then
    /// `{2}` .. `{800}`, 136 to a page.
    fn spec_rows() -> Vec<(Tid, Vec<Key>)> {
        (0..8991u64)
            .map(|i| {
                let t = tid(i / 136, i % 136 + 1);
                let v = if i < 8192 { 1 } else { (i - 8192 + 2) as i32 };
                (t, vec![Key::norm(Bson::Int32(v))])
            })
            .collect()
    }

    #[test]
    fn compression_is_varbyte_deltas() {
        // Consecutive offsets take a byte each; a block step takes two.
        assert_eq!(compress(&[tid(0, 1)], 2712), (1, 8));
        assert_eq!(compress(&[tid(0, 1), tid(0, 2), tid(0, 3)], 2712), (3, 10));
        assert_eq!(compress(&[tid(0, 136), tid(1, 1)], 2712), (2, 10));
    }

    /// The pages PostgreSQL 15.19's `ginbuild` makes for `predicate-gin`
    /// (pageinspect on a private cluster): the entry root at 1, the
    /// posting tree of `{1}` at 2 over leaves 4 and 3, and entry leaves
    /// 6, 5, 7, 8 and 9 of 147, 147, 147, 147 and 212 keys.
    #[test]
    fn build_matches_predicate_gin() {
        let g = Gin::build(&Int4, &spec_rows(), 65536);
        assert_eq!(g.layout(), "MEDddeeeee");
        let Page::EntryInternal { items, .. } = &g.pages[1] else {
            panic!("root")
        };
        let children: Vec<u32> = items.iter().map(|t| t.child).collect();
        assert_eq!(children, vec![6, 5, 7, 8, 9]);
        let sizes: Vec<usize> = [6u32, 5, 7, 8, 9]
            .iter()
            .map(|b| match &g.pages[*b as usize] {
                Page::EntryLeaf { items, .. } => items.len(),
                _ => 0,
            })
            .collect();
        assert_eq!(sizes, vec![147, 147, 147, 147, 212]);
        let Page::DataInternal { items, .. } = &g.pages[2] else {
            panic!("posting root")
        };
        assert_eq!(items.iter().map(|i| i.1).collect::<Vec<_>>(), vec![4, 3]);
        assert_eq!(
            g.stats(),
            Stats {
                n_pending_pages: 0,
                n_total_pages: 10,
                n_entry_pages: 6,
                n_data_pages: 3,
                n_entries: 800,
            }
        );
    }

    /// A larger build on PostgreSQL 15.19: 100,000 rows of three keys,
    /// 37 of them in posting trees. The metapage's statistics and every
    /// page's kind, by block, are PostgreSQL's.
    #[test]
    fn build_matches_a_larger_index() {
        let g = Gin::build(&Int4, &three_key_rows(100_000), 65536);
        assert_eq!(
            g.stats(),
            Stats {
                n_pending_pages: 0,
                n_total_pages: 204,
                n_entry_pages: 166,
                n_data_pages: 37,
                n_entries: 5000,
            }
        );
        let want = format!("ME{}{}", "d".repeat(37), "e".repeat(165));
        assert_eq!(g.layout(), want);
    }

    fn three_key_rows(n: u64) -> Vec<(Tid, Vec<Key>)> {
        (1..=n)
            .map(|g| {
                let i = g - 1;
                let vals = [g % 1000, g % 37, (g * 7) % 5000];
                let elems: Vec<Bson> = vals.iter().map(|v| Bson::Int32(*v as i32)).collect();
                (tid(i / 120, i % 120 + 1), value_keys(&Int4, Some(&elems)))
            })
            .collect()
    }

    /// 30,000 rows inserted one at a time into an empty index, on
    /// PostgreSQL 15.19: with `fastupdate = off`, 67 pages of entry tree;
    /// with it on, every row in a pending list of 221 pages (blocks 2 to
    /// 222), 4,100 bytes free on the tail.
    #[test]
    fn inserts_match_postgres() {
        let rows = three_key_rows(30_000);
        let mut off = Gin::empty();
        let mut on = Gin::empty();
        for (t, keys) in &rows {
            off.insert(&Int4, *t, keys, false, 4096, 4096);
            on.insert(&Int4, *t, keys, true, 4096, 4096);
        }
        assert_eq!(off.layout(), format!("ME{}", "e".repeat(65)));
        assert_eq!(on.layout(), format!("Me{}", "L".repeat(221)));
        assert_eq!(
            (on.pending.head, on.pending.tail, on.pending.tail_free),
            (2, 222, 4100)
        );
        assert_eq!(
            (on.pending.n_pages, on.pending.n_heap_tuples),
            (221, 30_000)
        );
    }

    #[test]
    fn scans_lock_the_leaf_or_the_posting_root() {
        let g = Gin::build(&Int4, &spec_rows(), 65536);
        let locks = |v: i32| {
            g.scan_locks(
                &Int4,
                &[ScanEntry {
                    key: Key::norm(Bson::Int32(v)),
                }],
            )
        };
        assert_eq!(locks(1), vec![0, 2]);
        assert_eq!(locks(2), vec![0, 6]);
        assert_eq!(locks(800), vec![0, 9]);
        assert_eq!(locks(2000), vec![0, 9]);
    }

    #[test]
    fn inserts_check_the_page_they_change() {
        let mut g = Gin::build(&Int4, &spec_rows(), 65536);
        let t = tid(66, 16);
        let ins = |g: &mut Gin, v: i32, fast: bool| {
            g.insert(&Int4, t, &[Key::norm(Bson::Int32(v))], fast, 4096, 4096)
        };
        // Into the posting tree: its root.
        assert_eq!(ins(&mut g, 1, false), vec![Event::ConflictIn(2)]);
        // Into a posting list, and as a new key.
        assert_eq!(ins(&mut g, 2, false), vec![Event::ConflictIn(6)]);
        assert_eq!(ins(&mut g, 2000, false), vec![Event::ConflictIn(9)]);
        // With fastupdate, into the pending list: the metapage.
        assert_eq!(ins(&mut g, 3, true), vec![Event::ConflictIn(0)]);
        assert_eq!(g.layout(), "MEDddeeeeeL");
        assert_eq!(g.stats().n_pending_pages, 1);
        assert_eq!(
            g.key_tids(&Int4, &Key::norm(Bson::Int32(3))),
            vec![tid(60, 34), t]
        );
    }

    #[test]
    fn pending_cleanup_moves_the_list_and_frees_its_pages() {
        let mut g = Gin::build(&Int4, &spec_rows(), 65536);
        let mut ev = Vec::new();
        for i in 0..3u64 {
            g.fast_insert(&Int4, tid(70, i + 1), &[Key::norm(Bson::Int32(5))], &mut ev);
        }
        g.insert_cleanup(&Int4, false, true, 4096, &mut ev);
        assert_eq!(g.layout(), "MEDddeeeee-");
        assert_eq!(g.key_tids(&Int4, &Key::norm(Bson::Int32(5))).len(), 4);
        // The next new page reuses it.
        assert_eq!(g.new_buffer(), 10);
    }
}
