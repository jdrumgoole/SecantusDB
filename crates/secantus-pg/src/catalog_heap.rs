//! Where `initdb` left each built-in catalog's rows (RP-291): every live
//! tuple's `ctid`, by block, and each heap's length in blocks, as
//! PostgreSQL 15.19 wrote them into a fresh database (`createdb` copies
//! `template1`), read by `tools/probes/pg15_system_catalog_dump.py` into
//! `pg15_catalog_heaps.txt`. A row of `pg15_catalog_rows.txt` is its
//! table's n-th tuple here, both being in `ctid` order.
//!
//! An index entry recorded in `pg15_catalog_indexes.txt` points at a HOT
//! chain's root line pointer, not always at the live tuple: `initdb`
//! updates some rows in place (`pg_class`, `pg_proc`, `pg_namespace`,
//! `pg_database`, `pg_index`), and a HOT update keeps its chain on one page,
//! so an index fetch reads the block it names either way.
//!
//! Each page is recorded too, as the heap page model keeps it
//! ([`crate::heap_page`]): its free space, its line pointer count and each
//! line pointer that is not an ordinary tuple (initdb's in-place updates
//! leave redirects and heap-only tuples, VACUUM unused slots), with the
//! free space map's category for each block when the fork exists. From
//! these [`initdb_heap_model`] builds the heap a later catalog row is
//! placed in, filling `initdb`'s free space first as
//! `RelationGetBufferForTuple` does through the map. The visibility map's
//! all-visible bits are recorded as well: an index-only scan fetches the
//! heap for a tuple on a block without one (RP-360).
//!
//! A catalog scan's heap blocks are counted over these
//! (`catalog_btree.rs`); placing every later catalog row into these models
//! as DDL writes it, and its index entries, is RP-291's next slice.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use crate::fsm::Fsm;
use crate::heap_page::{Heap, Lp, Page, Tuple};

/// `NodesPerPage`: an FSM page's node array.
const FSM_NODES_PER_PAGE: usize = 8192 - 24 - 4;

/// One built-in catalog's heap as `initdb` left it.
pub(crate) struct InitdbHeap {
    // Read by the crate's tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) name: String,
    /// `pg_relation_size / BLCKSZ`: a sequential scan reads every block.
    pub(crate) blocks: u32,
    /// Every live tuple's `ctid` as `block * 65536 + offset`, in `ctid` order.
    pub(crate) tids: Vec<i64>,
    #[cfg_attr(not(test), allow(dead_code))]
    /// Each block's page as `heap_pages` in the dump tool recorded it.
    pub(crate) pages: Vec<InitdbPage>,
    #[cfg_attr(not(test), allow(dead_code))]
    /// The free space map fork, as [`crate::fsm::Fsm::from_doc`] reads
    /// it; `None` without one.
    pub(crate) fsm: Option<bson::Document>,
    /// The visibility map's all-visible bit of each block
    /// (`VISIBILITYMAP_ALL_VISIBLE`); a heap without the fork has none set.
    pub(crate) all_visible: Vec<bool>,
}

impl InitdbHeap {
    /// `VM_ALL_VISIBLE`: whether `initdb` left `block` all-visible.
    pub(crate) fn block_all_visible(&self, block: i64) -> bool {
        usize::try_from(block)
            .ok()
            .and_then(|b| self.all_visible.get(b))
            .copied()
            .unwrap_or(false)
    }
}

/// One heap page as `initdb` left it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct InitdbPage {
    /// `pd_upper - pd_lower`.
    pub(crate) free: u32,
    /// Its line pointers.
    pub(crate) nlp: u16,
    /// The line pointers that are not ordinary tuples.
    pub(crate) lps: BTreeMap<u16, Lp>,
}

/// `off:u`, `off:r>to`, `off:h<len>`.
fn special_lp(text: &str) -> Option<(u16, Lp)> {
    let (off, what) = text.split_once(':')?;
    let off: u16 = off.parse().ok()?;
    let lp = if what == "u" {
        Lp::Unused
    } else if let Some(to) = what.strip_prefix("r>") {
        Lp::Redirect(to.parse().ok()?)
    } else {
        Lp::Normal(Tuple {
            len: what.strip_prefix('h')?.parse().ok()?,
            heap_only: true,
            hot_next: None,
            xmax: None,
            xmin_aborted: false,
        })
    };
    Some((off, lp))
}

fn parse(text: &str) -> HashMap<i64, InitdbHeap> {
    let mut out = HashMap::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("\\heap ") {
            let f: Vec<&str> = rest.split(' ').collect();
            if let [oid, name, blocks] = f[..] {
                let oid: i64 = oid.parse().unwrap_or(0);
                out.insert(
                    oid,
                    InitdbHeap {
                        name: name.to_string(),
                        blocks: blocks.parse().unwrap_or(0),
                        tids: Vec::new(),
                        pages: Vec::new(),
                        fsm: None,
                        all_visible: Vec::new(),
                    },
                );
                current = Some(oid);
            }
            continue;
        }
        let Some(heap) = current.and_then(|o| out.get_mut(&o)) else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("\\page ") {
            let mut f = rest.split(' ');
            let _block = f.next();
            let free = f.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let nlp = f.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            heap.pages.push(InitdbPage {
                free,
                nlp,
                lps: f.filter_map(special_lp).collect(),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("\\fsm ") {
            let nblocks: i64 = rest.parse().unwrap_or(0);
            heap.fsm = Some(bson::doc! { "nblocks": nblocks, "pages": [] });
            continue;
        }
        if let Some(rest) = line.strip_prefix("\\vm") {
            heap.all_visible = vec![false; heap.blocks as usize];
            for part in rest.trim().split(',').filter(|p| !p.is_empty()) {
                let (a, b) = part.split_once('-').unwrap_or((part, part));
                let (a, b): (usize, usize) = (a.parse().unwrap_or(0), b.parse().unwrap_or(0));
                for blk in a..=b {
                    if let Some(v) = heap.all_visible.get_mut(blk) {
                        *v = true;
                    }
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("\\fsmpage ") {
            let mut f = rest.splitn(3, ' ');
            let block: i64 = f.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let next: i32 = f.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let mut nodes = vec![0u8; FSM_NODES_PER_PAGE];
            for cell in f.next().unwrap_or("").split(',') {
                if let Some((n, v)) = cell.split_once(':') {
                    if let (Ok(n), Ok(v)) = (n.parse::<usize>(), v.parse::<u8>()) {
                        if let Some(slot) = nodes.get_mut(n) {
                            *slot = v;
                        }
                    }
                }
            }
            if let Some(Ok(pages)) = heap.fsm.as_mut().map(|d| d.get_array_mut("pages")) {
                pages.push(bson::Bson::Document(bson::doc! {
                    "block": block,
                    "next": next,
                    "nodes": bson::Binary {
                        subtype: bson::spec::BinarySubtype::Generic,
                        bytes: nodes,
                    },
                }));
            }
            continue;
        }
        let Some((block, offsets)) = line.split_once(' ') else {
            continue;
        };
        let block: i64 = block.parse().unwrap_or(0);
        for part in offsets.split(',') {
            let (a, b) = part.split_once('-').unwrap_or((part, part));
            let (a, b): (i64, i64) = (a.parse().unwrap_or(0), b.parse().unwrap_or(0));
            heap.tids.extend((a..=b).map(|o| block * 65536 + o));
        }
    }
    out
}

/// Every built-in catalog's `initdb` heap, by table oid.
pub(crate) fn initdb_heaps() -> &'static HashMap<i64, InitdbHeap> {
    static H: OnceLock<HashMap<i64, InitdbHeap>> = OnceLock::new();
    H.get_or_init(|| parse(include_str!("pg15_catalog_heaps.txt")))
}

/// The heap page model of catalog `table` as `initdb` left it: its pages
/// and its free space map. `None` for a table that is not a built-in
/// catalog. (The next slice places DDL's catalog rows in it.)
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn initdb_heap_model(table: i64) -> Option<Heap> {
    let h = initdb_heaps().get(&table)?;
    let mut heap = Heap::default();
    for p in &h.pages {
        heap.pages.push(Page {
            free: p.free,
            nlp: p.nlp,
            lps: p.lps.clone(),
            ..Page::default()
        });
    }
    if let Some(d) = &h.fsm {
        heap.fsm = Fsm::from_doc(d);
    }
    Some(heap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_catalog_has_its_initdb_heap() {
        let h = initdb_heaps();
        assert_eq!(h.len(), 64);
        let class = &h[&1259];
        assert_eq!((class.name.as_str(), class.blocks), ("pg_class", 14));
        assert_eq!(class.tids.len(), 410);
        assert_eq!(class.tids[0], 46);
        // In ctid order, every tuple on a block the heap has.
        for heap in h.values() {
            assert!(heap.tids.windows(2).all(|w| w[0] < w[1]), "{}", heap.name);
            assert!(heap.tids.iter().all(|t| t / 65536 < i64::from(heap.blocks)));
        }
        // Every block has its page, and every live tuple is an ordinary
        // line pointer or a heap-only tuple of its block.
        for heap in h.values() {
            assert_eq!(heap.pages.len() as u32, heap.blocks, "{}", heap.name);
            for (b, p) in heap.pages.iter().enumerate() {
                let live = heap.tids.iter().filter(|t| *t / 65536 == b as i64).count();
                let other = p
                    .lps
                    .values()
                    .filter(|lp| !matches!(lp, Lp::Normal(_)))
                    .count();
                assert_eq!(live + other, usize::from(p.nlp), "{} block {b}", heap.name);
            }
        }
        // An empty catalog has no block at all.
        assert_eq!((h[&826].blocks, h[&826].tids.len()), (0, 0));
    }

    #[test]
    fn the_rows_file_is_each_heap_in_ctid_order() {
        let text = include_str!("pg15_catalog_rows.txt");
        let by_name: HashMap<&str, &InitdbHeap> = initdb_heaps()
            .values()
            .map(|h| (h.name.as_str(), h))
            .collect();
        let mut checked = 0;
        for section in text.split("\\relation ").skip(1) {
            let (rel, rows) = section.split_once('\n').unwrap_or((section, ""));
            let Some(heap) = rel.strip_prefix("pg_catalog.").and_then(|n| by_name.get(n)) else {
                continue;
            };
            assert_eq!(rows.lines().count(), heap.tids.len(), "{rel}");
            checked += 1;
        }
        assert_eq!(checked, 12);
    }

    #[test]
    fn a_new_tables_catalog_rows_go_where_postgresql_puts_them() {
        // Measured on 15.19 in a fresh database: `create table rpc_n (a
        // int)` -- no toast table, no index, no later update of its rows --
        // puts its pg_class row (172 bytes, 176 MAXALIGNed) in block 0's
        // first unused line pointer, its two pg_type rows (180) on the
        // last block, and its seven pg_attribute rows (144) on block 17,
        // each block found through the free space map's `fp_next_slot`.
        let place = |table: i64, len: u32, n: usize| {
            let mut heap = initdb_heap_model(table).expect("a catalog");
            (0..n).map(|_| heap.place(1, len, true)).collect::<Vec<_>>()
        };
        assert_eq!(place(1259, 176, 1), vec![(0, 2)]);
        assert_eq!(place(1247, 184, 2), vec![(14, 10), (14, 11)]);
        assert_eq!(
            place(1249, 144, 7),
            (8..=14).map(|o| (17, o)).collect::<Vec<_>>()
        );
        // The free space map was read as PostgreSQL keeps it.
        assert!(initdb_heaps()[&1259].fsm.is_some());
        let class = initdb_heap_model(1259).expect("pg_class");
        assert_eq!(class.nblocks(), 14);
        assert!(initdb_heap_model(16384).is_none());
    }
}
