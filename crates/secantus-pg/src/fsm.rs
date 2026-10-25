//! A relation's free space map, as PostgreSQL 15.19 keeps it
//! (`src/backend/storage/freespace/`): a three-level tree of 8 kB pages,
//! each a binary tree of one-byte categories (`fsmpage.c`), whose leaves at
//! the bottom level are the heap's pages. The bottom level is kept current
//! by every insert that finds a page too full (`RecordAndGetPageWithFreeSpace`)
//! and by VACUUM; the upper levels only by VACUUM (`FreeSpaceMapVacuum`), so
//! a search from the root sees what the last VACUUM saw. Ported function by
//! function, so that the page a search returns -- and so the `ctid` a new
//! row takes -- is PostgreSQL's.

use std::collections::HashMap;

/// `BLCKSZ - MAXALIGN(SizeOfPageHeaderData) - offsetof(FSMPageData, fp_nodes)`.
const NODES_PER_PAGE: usize = 8192 - 24 - 4;
/// `BLCKSZ / 2 - 1`.
const NON_LEAF_NODES_PER_PAGE: usize = 8192 / 2 - 1;
/// `SlotsPerFSMPage`.
pub(crate) const SLOTS_PER_PAGE: u32 = (NODES_PER_PAGE - NON_LEAF_NODES_PER_PAGE) as u32;
/// `FSM_TREE_DEPTH` is 3 for 8 kB pages.
const TREE_DEPTH: u32 = 3;
const ROOT_LEVEL: u32 = TREE_DEPTH - 1;
const BOTTOM_LEVEL: u32 = 0;
/// `BLCKSZ / FSM_CATEGORIES`.
const CAT_STEP: u32 = 32;
/// `MaxHeapTupleSize`.
const MAX_REQUEST: u32 = 8160;

fn leftchild(x: usize) -> usize {
    2 * x + 1
}

fn parentof(x: usize) -> usize {
    (x - 1) / 2
}

fn rightneighbor(x: usize) -> usize {
    let x = x + 1;
    if ((x + 1) & x) == 0 {
        parentof(x)
    } else {
        x
    }
}

/// `fsm_space_avail_to_cat`.
pub(crate) fn avail_to_cat(avail: u32) -> u8 {
    if avail >= MAX_REQUEST {
        return 255;
    }
    (avail / CAT_STEP).min(254) as u8
}

/// `fsm_space_needed_to_cat`.
pub(crate) fn needed_to_cat(needed: u32) -> u8 {
    if needed == 0 {
        return 1;
    }
    needed.div_ceil(CAT_STEP).min(255) as u8
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Addr {
    level: u32,
    logpageno: u32,
}

const ROOT: Addr = Addr {
    level: ROOT_LEVEL,
    logpageno: 0,
};

/// One FSM page: its node tree and `fp_next_slot`.
#[derive(Clone)]
struct FsmPage {
    nodes: Vec<u8>,
    next_slot: i32,
}

impl FsmPage {
    fn new() -> Self {
        FsmPage {
            nodes: vec![0; NODES_PER_PAGE],
            next_slot: 0,
        }
    }

    /// `fsm_set_avail`.
    fn set_avail(&mut self, slot: usize, value: u8) -> bool {
        let mut nodeno = NON_LEAF_NODES_PER_PAGE + slot;
        let old = self.nodes[nodeno];
        if old == value && value <= self.nodes[0] {
            return false;
        }
        self.nodes[nodeno] = value;
        loop {
            nodeno = parentof(nodeno);
            let l = leftchild(nodeno);
            let r = l + 1;
            let mut new = self.nodes[l];
            if r < NODES_PER_PAGE {
                new = new.max(self.nodes[r]);
            }
            if self.nodes[nodeno] == new {
                break;
            }
            self.nodes[nodeno] = new;
            if nodeno == 0 {
                break;
            }
        }
        if value > self.nodes[0] {
            self.rebuild();
        }
        true
    }

    fn get_avail(&self, slot: usize) -> u8 {
        self.nodes[NON_LEAF_NODES_PER_PAGE + slot]
    }

    /// `fsm_search_avail`.
    fn search_avail(&mut self, minvalue: u8, advancenext: bool) -> Option<usize> {
        loop {
            if self.nodes[0] < minvalue {
                return None;
            }
            let mut target = self.next_slot;
            if target < 0 || target as u32 >= SLOTS_PER_PAGE {
                target = 0;
            }
            let mut nodeno = target as usize + NON_LEAF_NODES_PER_PAGE;
            while nodeno > 0 {
                if self.nodes[nodeno] >= minvalue {
                    break;
                }
                nodeno = parentof(rightneighbor(nodeno));
            }
            let mut corrupt = false;
            while nodeno < NON_LEAF_NODES_PER_PAGE {
                let child = leftchild(nodeno);
                if child < NODES_PER_PAGE && self.nodes[child] >= minvalue {
                    nodeno = child;
                    continue;
                }
                let child = child + 1;
                if child < NODES_PER_PAGE && self.nodes[child] >= minvalue {
                    nodeno = child;
                } else {
                    corrupt = true;
                    break;
                }
            }
            if corrupt {
                self.rebuild();
                continue;
            }
            let slot = nodeno - NON_LEAF_NODES_PER_PAGE;
            self.next_slot = slot as i32 + i32::from(advancenext);
            return Some(slot);
        }
    }

    /// `fsm_truncate_avail`.
    fn truncate_avail(&mut self, nslots: usize) {
        let mut changed = false;
        for n in &mut self.nodes[NON_LEAF_NODES_PER_PAGE + nslots..] {
            changed |= *n != 0;
            *n = 0;
        }
        if changed {
            self.rebuild();
        }
    }

    /// `fsm_rebuild_page`.
    fn rebuild(&mut self) {
        for nodeno in (0..NON_LEAF_NODES_PER_PAGE).rev() {
            let l = leftchild(nodeno);
            let r = l + 1;
            let mut new = 0;
            if l < NODES_PER_PAGE {
                new = self.nodes[l];
            }
            if r < NODES_PER_PAGE {
                new = new.max(self.nodes[r]);
            }
            self.nodes[nodeno] = new;
        }
    }
}

/// A relation's FSM fork: its pages by physical block number. A page below
/// `nblocks` that was never written reads as empty, as PostgreSQL's
/// zero-filled extension does.
#[derive(Clone, Default)]
pub(crate) struct Fsm {
    pages: HashMap<u32, FsmPage>,
    nblocks: u32,
    /// The pages read since the heap last took them (`fsm_readbuf`), each a
    /// buffer access PostgreSQL counts against the relation (RP-239).
    pub(crate) touched: Vec<u32>,
}

impl Fsm {
    /// The fork as a document, for a heap kept across a restart (RP-193).
    pub(crate) fn to_doc(&self) -> bson::Document {
        let mut pages: Vec<(&u32, &FsmPage)> = self.pages.iter().collect();
        pages.sort_by_key(|(b, _)| **b);
        bson::doc! {
            "nblocks": i64::from(self.nblocks),
            "pages": pages
                .into_iter()
                .map(|(b, p)| {
                    bson::Bson::Document(bson::doc! {
                        "block": i64::from(*b),
                        "next": p.next_slot,
                        "nodes": bson::Binary {
                            subtype: bson::spec::BinarySubtype::Generic,
                            bytes: p.nodes.clone(),
                        },
                    })
                })
                .collect::<Vec<_>>(),
        }
    }

    /// The fork [`Fsm::to_doc`] wrote; a page of the wrong size is left out
    /// (it reads as empty).
    pub(crate) fn from_doc(d: &bson::Document) -> Fsm {
        let mut fsm = Fsm {
            pages: HashMap::new(),
            nblocks: d.get_i64("nblocks").unwrap_or(0).try_into().unwrap_or(0),
            touched: Vec::new(),
        };
        for p in d.get_array("pages").map(|a| a.as_slice()).unwrap_or(&[]) {
            let bson::Bson::Document(p) = p else { continue };
            let Ok(nodes) = p.get_binary_generic("nodes") else {
                continue;
            };
            if nodes.len() != NODES_PER_PAGE {
                continue;
            }
            let block: u32 = p.get_i64("block").unwrap_or(0).try_into().unwrap_or(0);
            fsm.pages.insert(
                block,
                FsmPage {
                    nodes: nodes.clone(),
                    next_slot: p.get_i32("next").unwrap_or(0),
                },
            );
        }
        fsm
    }
}

/// `fsm_logical_to_physical`.
fn physical(addr: Addr) -> u32 {
    let mut leafno = addr.logpageno;
    for _ in 0..addr.level {
        leafno *= SLOTS_PER_PAGE;
    }
    let mut pages = 0;
    for _ in 0..TREE_DEPTH {
        pages += leafno + 1;
        leafno /= SLOTS_PER_PAGE;
    }
    pages -= addr.level;
    pages - 1
}

fn location(heapblk: u32) -> (Addr, usize) {
    (
        Addr {
            level: BOTTOM_LEVEL,
            logpageno: heapblk / SLOTS_PER_PAGE,
        },
        (heapblk % SLOTS_PER_PAGE) as usize,
    )
}

fn heap_blk(addr: Addr, slot: usize) -> u32 {
    addr.logpageno * SLOTS_PER_PAGE + slot as u32
}

fn parent(child: Addr) -> (Addr, usize) {
    (
        Addr {
            level: child.level + 1,
            logpageno: child.logpageno / SLOTS_PER_PAGE,
        },
        (child.logpageno % SLOTS_PER_PAGE) as usize,
    )
}

fn child(parent: Addr, slot: usize) -> Addr {
    Addr {
        level: parent.level - 1,
        logpageno: parent.logpageno * SLOTS_PER_PAGE + slot as u32,
    }
}

impl Fsm {
    /// `fsm_readbuf`: the page, if the fork reaches it (or `extend`).
    fn page(&mut self, addr: Addr, extend: bool) -> Option<&mut FsmPage> {
        let blkno = physical(addr);
        if blkno >= self.nblocks {
            if !extend {
                return None;
            }
            self.nblocks = blkno + 1;
        }
        self.touched.push(blkno);
        Some(self.pages.entry(blkno).or_insert_with(FsmPage::new))
    }

    /// `fsm_set_and_search`.
    fn set_and_search(&mut self, addr: Addr, slot: usize, value: u8, min: u8) -> Option<usize> {
        let page = self.page(addr, true).expect("extended");
        page.set_avail(slot, value);
        if min != 0 {
            return page.search_avail(min, addr.level == BOTTOM_LEVEL);
        }
        None
    }

    /// `fsm_search`: a heap block of `heap_blocks` with at least `min`.
    fn search(&mut self, min: u8, heap_blocks: u32) -> Option<u32> {
        let mut addr = ROOT;
        let mut restarts = 0;
        loop {
            let (slot, max_avail) = match self.page(addr, false) {
                Some(page) => {
                    let slot = page.search_avail(min, addr.level == BOTTOM_LEVEL);
                    (slot, page.nodes[0])
                }
                None => (None, 0),
            };
            match slot {
                Some(slot) if addr.level == BOTTOM_LEVEL => {
                    let blkno = heap_blk(addr, slot);
                    if blkno < heap_blocks {
                        return Some(blkno);
                    }
                    // A block past the relation's end: forget it, retry.
                    if let Some(page) = self.page(addr, false) {
                        page.set_avail(slot, 0);
                    }
                    restarts += 1;
                    if restarts > 10000 {
                        return None;
                    }
                    addr = ROOT;
                }
                Some(slot) => addr = child(addr, slot),
                None if addr.level == ROOT_LEVEL => return None,
                None => {
                    // The parent promised more than this page has: correct
                    // the parent and start again from the root.
                    let (p, pslot) = parent(addr);
                    self.set_and_search(p, pslot, max_avail, 0);
                    restarts += 1;
                    if restarts > 10000 {
                        return None;
                    }
                    addr = ROOT;
                }
            }
        }
    }

    /// `GetPageWithFreeSpace`.
    pub(crate) fn get_page(&mut self, needed: u32, heap_blocks: u32) -> Option<u32> {
        self.search(needed_to_cat(needed), heap_blocks)
    }

    /// `RecordAndGetPageWithFreeSpace`.
    pub(crate) fn record_and_get(
        &mut self,
        old_page: u32,
        old_avail: u32,
        needed: u32,
        heap_blocks: u32,
    ) -> Option<u32> {
        let (addr, slot) = location(old_page);
        let search_cat = needed_to_cat(needed);
        if let Some(found) = self.set_and_search(addr, slot, avail_to_cat(old_avail), search_cat) {
            let blkno = heap_blk(addr, found);
            if blkno < heap_blocks {
                return Some(blkno);
            }
        }
        self.search(search_cat, heap_blocks)
    }

    /// `RecordPageWithFreeSpace`.
    pub(crate) fn record(&mut self, heap_blk: u32, avail: u32) {
        let (addr, slot) = location(heap_blk);
        self.set_and_search(addr, slot, avail_to_cat(avail), 0);
    }

    /// `FreeSpaceMapPrepareTruncateRel` and the fork's truncation: the
    /// slots of blocks from `nblocks` on are cleared, the pages past them
    /// dropped.
    pub(crate) fn truncate(&mut self, nblocks: u32) {
        let (addr, slot) = location(nblocks);
        let keep = if slot > 0 {
            if let Some(page) = self.page(addr, false) {
                page.truncate_avail(slot);
            }
            physical(addr) + 1
        } else {
            physical(addr)
        };
        if keep < self.nblocks {
            self.nblocks = keep;
            self.pages.retain(|b, _| *b < keep);
        }
    }

    /// `FreeSpaceMapVacuumRange(start, end)`: the upper levels brought up
    /// to date from the bottom, every visited page's next slot reset.
    pub(crate) fn vacuum_range(&mut self, start: u32, end: u32) {
        if end > start {
            let mut eof = false;
            self.vacuum_page(ROOT, start, end, &mut eof);
        }
    }

    /// `fsm_vacuum_page`.
    fn vacuum_page(&mut self, addr: Addr, start: u32, end: u32, eof: &mut bool) -> u8 {
        if self.page(addr, false).is_none() {
            *eof = true;
            return 0;
        }
        if addr.level > BOTTOM_LEVEL {
            let (mut fs, mut fs_slot) = location(start);
            let (mut fe, mut fe_slot) = location(end - 1);
            while fs.level < addr.level {
                (fs, fs_slot) = parent(fs);
                (fe, fe_slot) = parent(fe);
            }
            let start_slot = match fs.logpageno.cmp(&addr.logpageno) {
                std::cmp::Ordering::Equal => fs_slot as i64,
                std::cmp::Ordering::Greater => i64::from(SLOTS_PER_PAGE),
                std::cmp::Ordering::Less => 0,
            };
            let end_slot = match fe.logpageno.cmp(&addr.logpageno) {
                std::cmp::Ordering::Equal => fe_slot as i64,
                std::cmp::Ordering::Greater => i64::from(SLOTS_PER_PAGE) - 1,
                std::cmp::Ordering::Less => -1,
            };
            let mut child_eof = false;
            let mut slot = start_slot;
            while slot <= end_slot {
                let s = slot as usize;
                let avail = if child_eof {
                    0
                } else {
                    self.vacuum_page(child(addr, s), start, end, &mut child_eof)
                };
                let page = self.page(addr, false).expect("read above");
                if page.get_avail(s) != avail {
                    page.set_avail(s, avail);
                }
                slot += 1;
            }
        }
        let page = self.page(addr, false).expect("read above");
        page.next_slot = 0;
        page.nodes[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page recorded by an insert that overflowed it is not found from
    /// the root until a VACUUM brings the upper levels up to date.
    #[test]
    fn the_upper_levels_wait_for_vacuum() {
        let mut fsm = Fsm::default();
        assert_eq!(fsm.record_and_get(0, 44, 140, 1), None);
        assert_eq!(fsm.get_page(32, 2), None);
        fsm.vacuum_range(0, 2);
        assert_eq!(fsm.get_page(32, 2), Some(0));
    }

    /// A search starts after the slot the last one returned.
    #[test]
    fn a_search_moves_on() {
        let mut fsm = Fsm::default();
        for b in 0..4 {
            fsm.record(b, 188);
        }
        fsm.vacuum_range(0, 4);
        assert_eq!(fsm.get_page(32, 4), Some(0));
        assert_eq!(fsm.get_page(32, 4), Some(1));
        fsm.vacuum_range(0, 4);
        assert_eq!(fsm.get_page(32, 4), Some(0));
    }

    #[test]
    fn categories() {
        assert_eq!(avail_to_cat(48), 1);
        assert_eq!(avail_to_cat(8160), 255);
        assert_eq!(needed_to_cat(32), 1);
        assert_eq!(needed_to_cat(33), 2);
        assert_eq!(needed_to_cat(0), 1);
    }
}
