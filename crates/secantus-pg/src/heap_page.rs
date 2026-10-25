//! A table's heap pages as PostgreSQL 15.19 manages them, as data: each
//! page's free space and line pointers, the tuples an UPDATE or DELETE has
//! marked, HOT chains, on-access pruning (`pruneheap.c`), the placement of a
//! new tuple (`hio.c`'s `RelationGetBufferForTuple`, through the free space
//! map in [`crate::fsm`]) and lazy VACUUM's passes over the pages
//! (`vacuumlazy.c`). Nothing here reads a row: what a tuple's state is --
//! live, dead, or still being written -- the caller settles from the store
//! and hands in.
//!
//! A page keeps only what the rows cannot say: a line pointer that is
//! unused, dead or a redirect, and a tuple some transaction has deleted or
//! updated (with its length and the deleter's xid), or that is a HOT
//! chain's heap-only member. Every other line pointer up to the page's
//! count is an ordinary tuple.

use std::collections::{BTreeMap, HashMap};

use bson::Bson;

use crate::fsm::Fsm;

pub(crate) const PAGE_SPACE: u32 = 8192 - 24;
pub(crate) const LINE_POINTER: u32 = 4;
/// `MaxHeapTuplesPerPage` for 8 kB pages.
pub(crate) const MAX_TUPLES: u32 = 291;
/// `MaxHeapTupleSize`.
const MAX_HEAP_TUPLE: u32 = 8160;
/// `RelationGetBufferForTuple`'s `nearlyEmptyFreeSpace`.
const NEARLY_EMPTY: u32 = MAX_HEAP_TUPLE - (MAX_TUPLES / 8) * LINE_POINTER;
/// `BLCKSZ / 10`: on-access pruning's floor of free space.
const PRUNE_MINFREE: u32 = 8192 / 10;
/// `BYPASS_THRESHOLD_PAGES`.
const BYPASS_THRESHOLD_PAGES: f64 = 0.02;
/// `REL_TRUNCATE_MINIMUM` / `REL_TRUNCATE_FRACTION`.
const REL_TRUNCATE_MINIMUM: u32 = 1000;
const REL_TRUNCATE_FRACTION: u32 = 16;

/// A deleting or updating transaction, and the row it acted on (its `_id`),
/// by which the caller settles whether it committed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Xmax {
    pub xid: u64,
    pub id: Bson,
    /// Settled: the transaction committed. (One that rolled back clears
    /// the xmax instead.)
    pub committed: bool,
}

/// A tuple the page must remember.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tuple {
    /// Its MAXALIGNed length.
    pub len: u32,
    /// The new version of a HOT update: reached only through its chain.
    pub heap_only: bool,
    /// HOT-updated: the next version's offset on this page.
    pub hot_next: Option<u16>,
    pub xmax: Option<Xmax>,
    /// Its inserting transaction rolled back (a HOT version whose update
    /// was undone): dead whatever the horizon.
    pub xmin_aborted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Lp {
    Unused,
    Dead,
    Redirect(u16),
    Normal(Tuple),
}

impl Lp {
    fn used(&self) -> bool {
        !matches!(self, Lp::Unused)
    }
}

/// `HeapTupleSatisfiesVacuum`'s answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Htsv {
    Live,
    Dead,
    RecentlyDead(u64),
    DeleteInProgress(u64),
    InsertInProgress,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Page {
    /// `pd_upper - pd_lower`.
    pub free: u32,
    /// Line pointers (the page's max offset).
    pub nlp: u16,
    /// `PD_PAGE_FULL`: an UPDATE found no room for its new version here.
    pub full: bool,
    /// `pd_prune_xid`: the oldest xmax a prune has not yet removed.
    pub prune_xid: Option<u64>,
    /// The line pointers that are not ordinary tuples (see the module doc).
    pub lps: BTreeMap<u16, Lp>,
}

impl Page {
    fn new() -> Self {
        Page {
            free: PAGE_SPACE,
            ..Page::default()
        }
    }

    fn lp(&self, off: u16) -> Option<&Lp> {
        self.lps.get(&off)
    }

    fn first_unused(&self) -> Option<u16> {
        self.lps
            .iter()
            .find(|(_, lp)| matches!(lp, Lp::Unused))
            .map(|(o, _)| *o)
    }

    /// `PageGetHeapFreeSpace`.
    pub(crate) fn heap_free(&self) -> u32 {
        let space = self.free.saturating_sub(LINE_POINTER);
        if space > 0 && u32::from(self.nlp) >= MAX_TUPLES && self.first_unused().is_none() {
            return 0;
        }
        space
    }

    /// `PageAddItem`: the first unused line pointer, else a new one.
    fn add(&mut self, len: u32) -> u16 {
        match self.first_unused() {
            Some(off) => {
                self.lps.remove(&off);
                self.free -= len;
                off
            }
            None => {
                self.nlp += 1;
                self.free -= len + LINE_POINTER;
                self.nlp
            }
        }
    }

    fn set_prunable(&mut self, xid: u64) {
        if self.prune_xid.is_none_or(|p| xid < p) {
            self.prune_xid = Some(xid);
        }
    }

    /// `PageRepairFragmentation`'s line pointer truncation: trailing
    /// unused line pointers are given back.
    fn truncate_unused_tail(&mut self, keep_one: bool) {
        let floor = u16::from(keep_one);
        while self.nlp > floor && matches!(self.lp(self.nlp), Some(Lp::Unused)) {
            self.lps.remove(&self.nlp);
            self.nlp -= 1;
            self.free += LINE_POINTER;
        }
    }

    /// Does the page still hold anything VACUUM's truncation must keep?
    fn has_used(&self) -> bool {
        (1..=self.nlp).any(|o| self.lp(o).is_none_or(Lp::used))
    }
}

/// One table's heap: its pages, free space map and each backend's target
/// block (`RelationGetTargetBlock`), and the tuples transactions still
/// running have inserted.
#[derive(Clone, Default)]
pub(crate) struct Heap {
    pub pages: Vec<Page>,
    /// `RelationGetTargetPageFreeSpace`: what fillfactor leaves free.
    pub reserve: u32,
    pub fsm: Fsm,
    pub targets: HashMap<i32, u32>,
    /// Tuples whose inserting transaction may still run: ctid -> (xid,
    /// length), and the ctids by xid.
    pub inserting: HashMap<i64, (u64, u32)>,
    pub inserting_by_xid: HashMap<u64, Vec<i64>>,
    /// The pages with a prune xid (`pd_prune_xid` set).
    pub prunable: std::collections::BTreeSet<u32>,
    /// The tuples' places no longer follow the rows' stored order (RP-192).
    pub disordered: bool,
    /// Changed since it was last written to the store (RP-193).
    pub dirty: bool,
    /// The buffers the writes since they were last taken read, in order
    /// (RP-239): a heap block as itself, a free space map block `b` as
    /// `-1 - b`, apart from the heap's in the buffer cache.
    pub accessed: Vec<i64>,
    /// Each backend's bulk-insert state (`BulkInsertState`), while a COPY
    /// or a CREATE TABLE AS writes: the page it holds, and whether it
    /// keeps the free space map (CREATE TABLE AS does not).
    pub bulk: HashMap<i32, Bulk>,
}

/// A backend's `BulkInsertState`.
#[derive(Clone, Copy, Default)]
pub(crate) struct Bulk {
    pub use_fsm: bool,
    pub current: Option<u32>,
}

pub(crate) fn tid(block: u32, off: u16) -> i64 {
    i64::from(block) * 65536 + i64::from(off)
}

pub(crate) fn untid(ctid: i64) -> Option<(u32, u16)> {
    if ctid < 0 {
        return None;
    }
    Some((
        u32::try_from(ctid / 65536).ok()?,
        u16::try_from(ctid % 65536).ok()?,
    ))
}

impl Heap {
    pub(crate) fn nblocks(&self) -> u32 {
        self.pages.len() as u32
    }

    /// `RelationGetBufferForTuple` and `PageAddItem` for a tuple of `len`
    /// from backend `pid`, with the page `other` (an UPDATE's old one)
    /// already tried. Returns the new tuple's place.
    pub(crate) fn place(&mut self, pid: i32, len: u32, use_fsm: bool) -> (u32, u16) {
        self.place_beside(pid, len, use_fsm, None)
    }

    /// `place`, with `pinned` (an UPDATE's old page) already held: reading
    /// it again takes no buffer. Each page read is noted in `accessed`.
    pub(crate) fn place_beside(
        &mut self,
        pid: i32,
        len: u32,
        use_fsm: bool,
        pinned: Option<u32>,
    ) -> (u32, u16) {
        let target_free = if len + self.reserve > NEARLY_EMPTY {
            len.max(NEARLY_EMPTY)
        } else {
            len + self.reserve
        };
        // What a VACUUM read of the map was counted with it.
        self.fsm.touched.clear();
        let bulk = self.bulk.get(&pid).copied();
        let use_fsm = use_fsm && bulk.is_none_or(|b| b.use_fsm);
        let nblocks = self.nblocks();
        // A bulk insert goes on with the page it holds.
        let mut target = bulk
            .and_then(|b| b.current)
            .or_else(|| self.targets.get(&pid).copied())
            .filter(|b| *b < nblocks);
        if target.is_none() && use_fsm {
            target = self.fsm.get_page(target_free, nblocks);
            self.take_fsm_reads();
        }
        if target.is_none() && nblocks > 0 {
            target = Some(nblocks - 1);
        }
        while let Some(b) = target {
            // `ReadBufferBI`: the page held is not read again.
            let held = pinned == Some(b) || bulk.and_then(|x| x.current) == Some(b);
            if !held {
                self.accessed.push(i64::from(b));
            }
            self.hold(pid, b);
            let page_free = self.pages[b as usize].heap_free();
            if target_free <= page_free {
                self.targets.insert(pid, b);
                let off = self.pages[b as usize].add(len);
                return (b, off);
            }
            if !use_fsm {
                break;
            }
            target = self.fsm.record_and_get(b, page_free, target_free, nblocks);
            self.take_fsm_reads();
        }
        self.pages.push(Page::new());
        let b = self.nblocks() - 1;
        // The new page, read as it is added (`ReadBuffer(P_NEW)`).
        self.accessed.push(i64::from(b));
        self.hold(pid, b);
        self.targets.insert(pid, b);
        let off = self.pages[b as usize].add(len);
        (b, off)
    }

    /// The free space map's pages read, as buffer accesses.
    fn take_fsm_reads(&mut self) {
        for b in std::mem::take(&mut self.fsm.touched) {
            self.accessed.push(-1 - i64::from(b));
        }
    }

    /// A bulk insert now holds page `b`.
    fn hold(&mut self, pid: i32, b: u32) {
        if let Some(bulk) = self.bulk.get_mut(&pid) {
            bulk.current = Some(b);
        }
    }

    /// A new tuple of a transaction `xid`: placed, and remembered as being
    /// inserted until `xid` ends.
    pub(crate) fn insert(&mut self, pid: i32, xid: u64, len: u32) -> i64 {
        let (b, off) = self.place(pid, len, true);
        let ctid = tid(b, off);
        self.note_inserting(xid, ctid, len);
        ctid
    }

    fn note_inserting(&mut self, xid: u64, ctid: i64, len: u32) {
        self.inserting.insert(ctid, (xid, len));
        self.inserting_by_xid.entry(xid).or_default().push(ctid);
    }

    /// Give back a place [`Self::insert`] took for a tuple never written.
    pub(crate) fn release(&mut self, ctid: i64, len: u32) {
        let Some((b, off)) = untid(ctid) else {
            return;
        };
        if let Some((xid, _)) = self.inserting.remove(&ctid) {
            if let Some(v) = self.inserting_by_xid.get_mut(&xid) {
                v.retain(|c| *c != ctid);
            }
        }
        let Some(page) = self.pages.get_mut(b as usize) else {
            return;
        };
        if off == page.nlp && !page.lps.contains_key(&off) {
            page.nlp -= 1;
            page.free += len + LINE_POINTER;
            if page.nlp == 0 && b as usize + 1 == self.pages.len() {
                self.pages.pop();
            }
        } else if off < page.nlp && !page.lps.contains_key(&off) {
            page.lps.insert(off, Lp::Unused);
            page.free += len;
        }
    }

    /// `heap_delete`: the tuple at `ctid` marked by `xid`.
    pub(crate) fn delete(&mut self, ctid: i64, len: u32, xid: u64, id: Bson) {
        let Some((b, off)) = untid(ctid) else {
            return;
        };
        let Some(page) = self.pages.get_mut(b as usize) else {
            return;
        };
        if off == 0 || off > page.nlp {
            return;
        }
        let xmax = Some(Xmax {
            xid,
            id,
            committed: false,
        });
        match page.lps.get_mut(&off) {
            Some(Lp::Normal(t)) => t.xmax = xmax,
            Some(_) => return,
            None => {
                page.lps.insert(
                    off,
                    Lp::Normal(Tuple {
                        len,
                        heap_only: false,
                        hot_next: None,
                        xmax,
                        xmin_aborted: false,
                    }),
                );
            }
        }
        page.set_prunable(xid);
        self.prunable.insert(b);
    }

    /// `heap_update`: the tuple at `old` (of `old_len`) marked by `xid`,
    /// and its new version of `len` placed -- on the old page while it fits
    /// there, a heap-only tuple of a HOT chain when `hot` (no indexed
    /// column changed), else where an INSERT's would go.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        pid: i32,
        old: Option<i64>,
        old_len: u32,
        len: u32,
        hot: bool,
        xid: u64,
        id: Bson,
    ) -> i64 {
        self.disordered = true;
        let Some((ob, ooff)) = old.and_then(untid).filter(|(b, o)| {
            self.pages
                .get(*b as usize)
                .is_some_and(|p| *o >= 1 && *o <= p.nlp)
        }) else {
            return self.insert(pid, xid, len);
        };
        self.delete(tid(ob, ooff), old_len, xid, id);
        // The old version fetched for the new one to be formed from
        // (`table_tuple_fetch_row_version`), then its page read by
        // `heap_update`.
        self.accessed.push(i64::from(ob));
        self.accessed.push(i64::from(ob));
        let same_page = len <= self.pages[ob as usize].heap_free();
        let (nb, noff) = if same_page {
            let off = self.pages[ob as usize].add(len);
            (ob, off)
        } else {
            self.pages[ob as usize].full = true;
            self.place_beside(pid, len, true, Some(ob))
        };
        let ctid = tid(nb, noff);
        if same_page && hot {
            let page = &mut self.pages[ob as usize];
            if let Some(Lp::Normal(t)) = page.lps.get_mut(&ooff) {
                t.hot_next = Some(noff);
            }
            page.lps.insert(
                noff,
                Lp::Normal(Tuple {
                    len,
                    heap_only: true,
                    hot_next: None,
                    xmax: None,
                    xmin_aborted: false,
                }),
            );
        }
        self.note_inserting(xid, ctid, len);
        ctid
    }

    /// Forget the inserts of transactions that have ended.
    pub(crate) fn forget_ended(&mut self, running: &dyn Fn(u64) -> bool) {
        let ended: Vec<u64> = self
            .inserting_by_xid
            .keys()
            .copied()
            .filter(|x| !running(*x))
            .collect();
        for x in ended {
            for c in self.inserting_by_xid.remove(&x).unwrap_or_default() {
                self.inserting.remove(&c);
            }
        }
    }

    /// The pages a prune could find work on.
    pub(crate) fn prunable_blocks(&self) -> Vec<u32> {
        self.prunable.iter().copied().collect()
    }

    /// Would [`Self::prune_opt`] act on `block`, its deleters settled?
    pub(crate) fn may_prune(&self, block: u32, horizon: u64) -> bool {
        let Some(page) = self.pages.get(block as usize) else {
            return false;
        };
        page.prune_xid.is_some_and(|p| p < horizon)
            && (page.full || page.heap_free() < self.reserve.max(PRUNE_MINFREE))
    }

    /// Settle every ended deleter at once, from the visible rows' places
    /// (`present`): VACUUM's scan has read them all.
    pub(crate) fn settle_all(
        &mut self,
        present: &dyn Fn(i64) -> bool,
        running: &dyn Fn(u64) -> bool,
    ) {
        for block in 0..self.nblocks() {
            let outcome: Vec<(u16, bool)> = self
                .unsettled(block, u64::MAX, running)
                .into_iter()
                .map(|(off, _)| (off, !present(tid(block, off))))
                .collect();
            if !outcome.is_empty() {
                self.settle(block, &outcome);
            }
        }
    }

    /// The TIDs an index may point at whose whole HOT chain is dead to
    /// every snapshot (`heap_hot_search_buffer` under
    /// `SnapshotNonVacuumable` finding nothing), as bottom-up index
    /// deletion asks the heap (RP-196). Only settled deleters count.
    pub(crate) fn vacuumable_tids(
        &self,
        horizon: u64,
        running: &dyn Fn(u64) -> bool,
    ) -> std::collections::HashSet<i64> {
        let mut out = std::collections::HashSet::new();
        for (b, page) in self.pages.iter().enumerate() {
            let b = b as u32;
            for (&o, lp) in &page.lps {
                let root = match lp {
                    Lp::Normal(t) => !t.heap_only,
                    _ => true,
                };
                if root && self.chain_vacuumable(page, b, o, horizon, running) {
                    out.insert(tid(b, o));
                }
            }
        }
        out
    }

    fn chain_vacuumable(
        &self,
        page: &Page,
        block: u32,
        mut off: u16,
        horizon: u64,
        running: &dyn Fn(u64) -> bool,
    ) -> bool {
        for _ in 0..=page.nlp {
            match page.lps.get(&off) {
                // An ordinary live tuple, kept out of the map.
                None => return false,
                Some(Lp::Dead | Lp::Unused) => return true,
                Some(Lp::Redirect(n)) => off = *n,
                Some(lp @ Lp::Normal(t)) => {
                    let state = self.htsv(tid(block, off), Some(lp), horizon, running, &|_| false);
                    if state != Htsv::Dead {
                        return false;
                    }
                    match t.hot_next {
                        Some(n) => off = n,
                        None => return true,
                    }
                }
            }
        }
        false
    }

    /// The marked tuples on `block` whose deleter ended before `horizon`
    /// but is not yet settled: `(offset, _id)`, for the caller to look up.
    pub(crate) fn unsettled(
        &self,
        block: u32,
        horizon: u64,
        running: &dyn Fn(u64) -> bool,
    ) -> Vec<(u16, Bson)> {
        let Some(page) = self.pages.get(block as usize) else {
            return Vec::new();
        };
        page.lps
            .iter()
            .filter_map(|(o, lp)| match lp {
                Lp::Normal(Tuple { xmax: Some(x), .. })
                    if !x.committed && x.xid < horizon && !running(x.xid) =>
                {
                    Some((*o, x.id.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Record what the store says of the tuples [`Self::unsettled`] named:
    /// `committed[i]` for the `i`th, whose deleter committed (the row has
    /// gone, or moved) or rolled back (the row is still there, here).
    pub(crate) fn settle(&mut self, block: u32, outcome: &[(u16, bool)]) {
        let Some(page) = self.pages.get_mut(block as usize) else {
            return;
        };
        let mut orphans: Vec<u16> = Vec::new();
        for (off, committed) in outcome {
            let plain = match page.lps.get_mut(off) {
                Some(Lp::Normal(t)) => {
                    if *committed {
                        if let Some(x) = t.xmax.as_mut() {
                            x.committed = true;
                        }
                        false
                    } else {
                        // Rolled back: the tuple is live again, its chain
                        // (if the update was HOT) ends here, and the
                        // version the update made is dead.
                        t.xmax = None;
                        if let Some(next) = t.hot_next.take() {
                            orphans.push(next);
                        }
                        !t.heap_only
                    }
                }
                _ => false,
            };
            if plain {
                page.lps.remove(off);
            }
        }
        for o in orphans {
            if let Some(Lp::Normal(t)) = page.lps.get_mut(&o) {
                t.xmin_aborted = true;
            }
        }
    }

    /// A tuple's state, as `HeapTupleSatisfiesVacuum` would give it, to a
    /// prune whose removal horizon is `horizon`. `absent` says a plain
    /// tuple is not among the visible rows (VACUUM's scan knows; a prune
    /// on access does not, and takes it as live).
    fn htsv(
        &self,
        ctid: i64,
        lp: Option<&Lp>,
        horizon: u64,
        running: &dyn Fn(u64) -> bool,
        absent: &dyn Fn(i64) -> bool,
    ) -> Htsv {
        if self.inserting.get(&ctid).is_some_and(|(x, _)| running(*x)) {
            return Htsv::InsertInProgress;
        }
        match lp {
            Some(Lp::Normal(Tuple {
                xmin_aborted: true, ..
            })) => Htsv::Dead,
            Some(Lp::Normal(Tuple { xmax: Some(x), .. })) => {
                if running(x.xid) || !x.committed {
                    Htsv::DeleteInProgress(x.xid)
                } else if x.xid < horizon {
                    Htsv::Dead
                } else {
                    Htsv::RecentlyDead(x.xid)
                }
            }
            _ if absent(ctid) => Htsv::Dead,
            _ => Htsv::Live,
        }
    }

    /// `heap_page_prune_opt`: prune `block` if its prune xid is past the
    /// horizon and it is full or short of space.
    pub(crate) fn prune_opt(&mut self, block: u32, horizon: u64, running: &dyn Fn(u64) -> bool) {
        let Some(page) = self.pages.get(block as usize) else {
            return;
        };
        let Some(prune_xid) = page.prune_xid else {
            return;
        };
        if prune_xid >= horizon {
            return;
        }
        let minfree = self.reserve.max(PRUNE_MINFREE);
        if page.full || page.heap_free() < minfree {
            self.prune(block, horizon, running, &|_| false);
        }
    }

    /// `heap_page_prune`: each HOT chain's dead members removed -- the root
    /// redirected to the first survivor, or dead with none -- and every
    /// other dead tuple's line pointer made dead, its space given back.
    /// Returns how many line pointers it made dead.
    pub(crate) fn prune(
        &mut self,
        block: u32,
        horizon: u64,
        running: &dyn Fn(u64) -> bool,
        absent: &dyn Fn(i64) -> bool,
    ) -> usize {
        let b = block as usize;
        let nlp = self.pages[b].nlp;
        let states: Vec<Option<Htsv>> = (0..=nlp)
            .map(|o| {
                let page = &self.pages[b];
                match (o, page.lp(o)) {
                    (0, _) => None,
                    (_, Some(Lp::Unused | Lp::Dead | Lp::Redirect(_))) => None,
                    (_, lp) => Some(self.htsv(tid(block, o), lp, horizon, running, absent)),
                }
            })
            .collect();
        let page = &self.pages[b];
        let mut marked = vec![false; usize::from(nlp) + 1];
        let mut redirected: Vec<(u16, u16)> = Vec::new();
        let mut now_dead: Vec<u16> = Vec::new();
        let mut now_unused: Vec<u16> = Vec::new();
        let mut new_prune_xid: Option<u64> = None;
        let heap_only = |o: u16| matches!(page.lp(o), Some(Lp::Normal(t)) if t.heap_only);
        let hot_next = |o: u16| match page.lp(o) {
            Some(Lp::Normal(t)) => t.hot_next,
            _ => None,
        };
        for root in 1..=nlp {
            if marked[usize::from(root)] {
                continue;
            }
            let rootlp = page.lp(root);
            if matches!(rootlp, Some(Lp::Unused | Lp::Dead)) {
                continue;
            }
            // heap_prune_chain
            if !matches!(rootlp, Some(Lp::Redirect(_))) && heap_only(root) {
                if states[usize::from(root)] == Some(Htsv::Dead) && hot_next(root).is_none() {
                    now_unused.push(root);
                    marked[usize::from(root)] = true;
                }
                continue;
            }
            let mut chain: Vec<u16> = Vec::new();
            let mut latestdead: Option<u16> = None;
            let mut off = root;
            loop {
                if off < 1 || off > nlp || marked[usize::from(off)] {
                    break;
                }
                match page.lp(off) {
                    Some(Lp::Unused) | Some(Lp::Dead) => break,
                    Some(Lp::Redirect(to)) => {
                        if !chain.is_empty() {
                            break;
                        }
                        chain.push(off);
                        off = *to;
                        continue;
                    }
                    _ => {}
                }
                chain.push(off);
                let (dead, recent) = match states[usize::from(off)] {
                    Some(Htsv::Dead) => (true, false),
                    Some(Htsv::RecentlyDead(x)) => {
                        new_prune_xid = Some(new_prune_xid.map_or(x, |p| p.min(x)));
                        (false, true)
                    }
                    Some(Htsv::DeleteInProgress(x)) => {
                        new_prune_xid = Some(new_prune_xid.map_or(x, |p| p.min(x)));
                        (false, false)
                    }
                    _ => (false, false),
                };
                if dead {
                    latestdead = Some(off);
                } else if !recent {
                    break;
                }
                match hot_next(off) {
                    Some(next) => off = next,
                    None => break,
                }
            }
            if let Some(ld) = latestdead {
                let mut i = 1;
                while i < chain.len() && chain[i - 1] != ld {
                    now_unused.push(chain[i]);
                    marked[usize::from(chain[i])] = true;
                    i += 1;
                }
                if i >= chain.len() {
                    now_dead.push(root);
                    marked[usize::from(root)] = true;
                } else {
                    redirected.push((root, chain[i]));
                    marked[usize::from(root)] = true;
                    marked[usize::from(chain[i])] = true;
                }
            } else if chain.len() < 2 && matches!(rootlp, Some(Lp::Redirect(_))) {
                now_dead.push(root);
                marked[usize::from(root)] = true;
            }
        }
        let page = &mut self.pages[b];
        let changed = !(redirected.is_empty() && now_dead.is_empty() && now_unused.is_empty());
        let storage = |page: &Page, o: u16| match page.lp(o) {
            Some(Lp::Normal(t)) => Some(t.len),
            Some(_) => Some(0),
            None => None,
        };
        // Space a removed tuple held. A plain tuple's length is not kept;
        // only VACUUM removes those, and it recounts the page.
        let mut freed = 0u32;
        for (from, to) in &redirected {
            freed += storage(page, *from).unwrap_or(0);
            page.lps.insert(*from, Lp::Redirect(*to));
        }
        for o in &now_dead {
            freed += storage(page, *o).unwrap_or(0);
            page.lps.insert(*o, Lp::Dead);
        }
        for o in &now_unused {
            freed += storage(page, *o).unwrap_or(0);
            page.lps.insert(*o, Lp::Unused);
        }
        page.free += freed;
        if changed {
            page.truncate_unused_tail(false);
        }
        page.prune_xid = new_prune_xid;
        page.full = false;
        if new_prune_xid.is_none() {
            self.prunable.remove(&block);
        }
        now_dead.len()
    }

    /// Lazy VACUUM (`heap_vacuum_rel`) over every page: each pruned, its
    /// dead line pointers made unused -- at once without indexes, after the
    /// index pass with them, unless too few pages have any to be worth it
    /// (the bypass) -- the free space recorded, trailing empty pages
    /// truncated away when `truncate` allows. `live` gives each visible
    /// row's length by ctid; every page's free space is recounted from it.
    /// Returns the TIDs the index pass removes from the indexes, `None`
    /// when no index pass ran (no dead tuples, no indexes, or the bypass).
    pub(crate) fn vacuum(
        &mut self,
        horizon: u64,
        running: &dyn Fn(u64) -> bool,
        live: &HashMap<i64, u32>,
        has_indexes: bool,
        truncate: bool,
    ) -> Option<Vec<i64>> {
        let absent = |c: i64| !live.contains_key(&c);
        let rel_pages = self.nblocks();
        let mut deferred: Vec<u32> = Vec::new();
        let mut lpdead_items = 0usize;
        let mut nonempty_pages = 0u32;
        for block in 0..rel_pages {
            self.prune(block, horizon, running, &absent);
            self.recount(block, live);
            let page = &self.pages[block as usize];
            let dead: usize = page.lps.values().filter(|l| matches!(l, Lp::Dead)).count();
            let hastup = (1..=page.nlp).any(|o| !matches!(page.lp(o), Some(Lp::Unused | Lp::Dead)));
            if hastup {
                nonempty_pages = block + 1;
            }
            if dead > 0 && has_indexes {
                lpdead_items += dead;
                deferred.push(block);
            } else {
                if dead > 0 {
                    self.vacuum_page(block);
                }
                let free = self.pages[block as usize].heap_free();
                self.fsm.record(block, free);
            }
        }
        let mut index_pass = None;
        if !deferred.is_empty() {
            let threshold = (f64::from(rel_pages) * BYPASS_THRESHOLD_PAGES) as usize;
            let bypass = deferred.len() < threshold && lpdead_items < 32 * 1024 * 1024 / 6;
            if !bypass {
                let mut dead: Vec<i64> = Vec::new();
                for block in &deferred {
                    let page = &self.pages[*block as usize];
                    dead.extend(
                        page.lps
                            .iter()
                            .filter(|(_, l)| matches!(l, Lp::Dead))
                            .map(|(o, _)| tid(*block, *o)),
                    );
                }
                index_pass = Some(dead);
                for block in deferred {
                    self.vacuum_page(block);
                    let free = self.pages[block as usize].heap_free();
                    self.fsm.record(block, free);
                }
            }
        }
        self.fsm.vacuum_range(0, rel_pages);
        let possibly_freeable = rel_pages - nonempty_pages;
        if truncate
            && possibly_freeable > 0
            && (possibly_freeable >= REL_TRUNCATE_MINIMUM
                || possibly_freeable >= rel_pages / REL_TRUNCATE_FRACTION)
        {
            // count_nondeletable_pages: back from the end while a page
            // holds no used line pointer.
            let mut new_pages = rel_pages;
            while new_pages > nonempty_pages && !self.pages[new_pages as usize - 1].has_used() {
                new_pages -= 1;
            }
            if new_pages < rel_pages {
                self.pages.truncate(new_pages as usize);
                self.prunable.retain(|b| *b < new_pages);
                self.fsm.truncate(new_pages);
                self.fsm.vacuum_range(new_pages, u32::MAX);
            }
        }
        // The relation's statistics are updated in place, which invalidates
        // every backend's cached target block.
        self.targets.clear();
        index_pass
    }

    /// `lazy_vacuum_heap_page`: dead line pointers made unused, the
    /// array's unused tail given back.
    fn vacuum_page(&mut self, block: u32) {
        let page = &mut self.pages[block as usize];
        for lp in page.lps.values_mut() {
            if matches!(lp, Lp::Dead) {
                *lp = Lp::Unused;
            }
        }
        page.truncate_unused_tail(true);
    }

    /// A page's free space from what it holds: the visible rows' tuples
    /// (`live`), the marked ones still kept, and its line pointers.
    fn recount(&mut self, block: u32, live: &HashMap<i64, u32>) {
        let page = &self.pages[block as usize];
        let mut used = u32::from(page.nlp) * LINE_POINTER;
        for o in 1..=page.nlp {
            used += match page.lp(o) {
                Some(Lp::Normal(t)) => live.get(&tid(block, o)).copied().unwrap_or(t.len),
                Some(_) => 0,
                None => live
                    .get(&tid(block, o))
                    .copied()
                    .or_else(|| self.inserting.get(&tid(block, o)).map(|(_, l)| *l))
                    .unwrap_or(0),
            };
        }
        self.pages[block as usize].free = PAGE_SPACE.saturating_sub(used);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: u64) -> bool {
        false
    }

    /// Three 140-byte tuples, the second updated with no index: a HOT
    /// chain, and VACUUM leaves its root a redirect (measured on 15.19).
    #[test]
    fn vacuum_leaves_a_hot_root_redirected() {
        let mut h = Heap::default();
        for _ in 0..3 {
            h.insert(1, 800, 136);
        }
        let new = h.update(1, Some(tid(0, 2)), 136, 136, true, 801, Bson::Int32(2));
        assert_eq!(new, tid(0, 4));
        h.settle(0, &[(2, true)]);
        h.forget_ended(&none);
        let live: HashMap<i64, u32> = [tid(0, 1), tid(0, 3), tid(0, 4)]
            .into_iter()
            .map(|c| (c, 136))
            .collect();
        h.vacuum(900, &none, &live, false, true);
        assert_eq!(h.pages[0].lp(2), Some(&Lp::Redirect(4)));
        assert_eq!(h.insert(2, 802, 136), tid(0, 5));
    }

    /// The same with the key indexed and changed: the old tuple dies and
    /// its line pointer is reused.
    #[test]
    fn vacuum_frees_a_non_hot_version() {
        let mut h = Heap::default();
        for _ in 0..3 {
            h.insert(1, 800, 136);
        }
        h.update(1, Some(tid(0, 2)), 136, 136, false, 801, Bson::Int32(2));
        h.settle(0, &[(2, true)]);
        h.forget_ended(&none);
        let live: HashMap<i64, u32> = [tid(0, 1), tid(0, 3), tid(0, 4)]
            .into_iter()
            .map(|c| (c, 136))
            .collect();
        h.vacuum(900, &none, &live, true, true);
        assert_eq!(h.pages[0].lp(2), Some(&Lp::Unused));
        assert_eq!(h.insert(2, 802, 136), tid(0, 2));
        assert_eq!(h.insert(2, 803, 136), tid(0, 5));
    }
}
