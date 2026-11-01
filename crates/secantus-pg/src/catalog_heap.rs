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
//! This is the first of RP-291's heap slices: the next places every later
//! catalog row with the heap page model and counts each scan's heap pages.

use std::collections::HashMap;
use std::sync::OnceLock;

/// One built-in catalog's heap as `initdb` left it.
// Read by the crate's tests until the counting slices of RP-291 land.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct InitdbHeap {
    pub(crate) name: String,
    /// `pg_relation_size / BLCKSZ`: a sequential scan reads every block.
    pub(crate) blocks: u32,
    /// Every live tuple's `ctid` as `block * 65536 + offset`, in `ctid` order.
    pub(crate) tids: Vec<i64>,
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
                    },
                );
                current = Some(oid);
            }
            continue;
        }
        let Some(heap) = current.and_then(|o| out.get_mut(&o)) else {
            continue;
        };
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
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn initdb_heaps() -> &'static HashMap<i64, InitdbHeap> {
    static H: OnceLock<HashMap<i64, InitdbHeap>> = OnceLock::new();
    H.get_or_init(|| parse(include_str!("pg15_catalog_heaps.txt")))
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
}
