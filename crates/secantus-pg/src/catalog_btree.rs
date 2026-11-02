//! The pages a catalog index scan reads (RP-291), counted as
//! `pg_statio_sys_indexes` counts them: each B-tree index of the catalogs
//! is modelled page by page ([`crate::btree_model`]), and a scan reads the
//! metapage on its backend's first use of the index (`_bt_metaversion`,
//! which then caches it in `rd_amcache`), the way down from the root, and
//! each leaf it walks right across (`_bt_first`, `_bt_readpage`).
//!
//! Each index is PostgreSQL 15.19's own: the pages `initdb` left, read from
//! a fresh cluster's files by `tools/probes/pg15_system_catalog_dump.py`
//! into `pg15_catalog_indexes.txt` -- which block holds which tuples, each
//! tuple's key as decoded from its bytes and its heap TIDs, every internal
//! page's downlinks. Nothing is read from this server's catalogs to build
//! it: reading a catalog's rows on a statement's path cost a new session
//! hundreds of milliseconds, inside a transaction that changed the
//! catalogs it blinded the session to its own new objects, and off a
//! statement's path it lost other sessions' new tables.
//!
//! The heap blocks behind a scan are counted too, over `initdb`'s heaps
//! (`catalog_heap.rs`). Not yet modelled, and recorded in RP-291: a row
//! any database adds after `initdb` (placing later catalog rows, and their
//! index entries), and VACUUM's deletion of dead entries.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::OnceLock;

use bson::Bson;

use crate::btree_model::{BTree, LayoutPage, Shape};
use crate::catalog_facts::builtin;
use crate::catalog_heap::initdb_heaps;
use crate::catcache::{Backend, PageScan, K};
use crate::PgHandler;

/// One built-in index as `initdb` left it.
struct Layout {
    root: u32,
    /// Its key columns' types.
    types: Vec<i64>,
    pages: Vec<LayoutPage>,
    /// Each leaf's tuples' keys and heap TIDs, by block, in page order.
    entries: HashMap<u32, Vec<(Vec<Bson>, i64)>>,
}

impl Layout {
    /// Every leaf tuple in index order: the leaves leftmost first, along
    /// their right links.
    fn in_order(&self) -> Vec<(Vec<Bson>, i64)> {
        let by_block: HashMap<u32, &LayoutPage> = self.pages.iter().map(|p| (p.block, p)).collect();
        let mut at = self.root;
        while let Some(p) = by_block.get(&at).filter(|p| p.level > 0) {
            match p.items.first() {
                Some(c) => at = *c,
                None => return Vec::new(),
            }
        }
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        while at != 0 && seen.insert(at) {
            out.extend(self.entries.get(&at).into_iter().flatten().cloned());
            at = by_block.get(&at).map_or(0, |p| p.next);
        }
        out
    }
}

/// `\index oid root level type...`, then per page `block flags next level
/// items` (a leaf's items each tuple's TID count, run-length coded as
/// `1x40`; an internal page's its downlinks), and after a leaf page one
/// `= tid[,tid...]<TAB>key...` line per tuple, a TID as `block.offset`,
/// a key value as its type's text output (escaped as COPY escapes it,
/// `\N` for NULL).
fn parse(text: &str) -> HashMap<i64, Layout> {
    let mut out: HashMap<i64, Layout> = HashMap::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("\\index ") {
            let f: Vec<i64> = rest.split(' ').filter_map(|v| v.parse().ok()).collect();
            if let [oid, root, _level, ref types @ ..] = f[..] {
                out.insert(
                    oid,
                    Layout {
                        root: root as u32,
                        types: types.to_vec(),
                        pages: Vec::new(),
                        entries: HashMap::new(),
                    },
                );
                current = Some(oid);
            }
            continue;
        }
        let Some(layout) = current.and_then(|o| out.get_mut(&o)) else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("= ") {
            let mut cells = rest.split('\t');
            let tids: Vec<i64> = cells
                .next()
                .unwrap_or("")
                .split(',')
                .filter_map(|t| {
                    let (b, o) = t.split_once('.')?;
                    Some(b.parse::<i64>().ok()? * 65536 + o.parse::<i64>().ok()?)
                })
                .collect();
            let key: Vec<Bson> = cells
                .zip(&layout.types)
                .map(|(c, typ)| value(c, *typ))
                .collect();
            let block = layout.pages.last().map_or(0, |p| p.block);
            let leaf = layout.entries.entry(block).or_default();
            for t in tids {
                leaf.push((key.clone(), t));
            }
            continue;
        }
        let mut f = line.splitn(5, ' ');
        let mut num = || f.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        let (block, flags, next, level) = (num(), num(), num(), num());
        let items_text = line.splitn(5, ' ').nth(4).unwrap_or("");
        // A deleted page (`BTP_DELETED`) holds nothing a scan reaches.
        if flags & 4 != 0 {
            continue;
        }
        let mut items = Vec::new();
        for part in items_text.split(',').filter(|p| !p.is_empty()) {
            match part.split_once('x') {
                Some((n, times)) => {
                    let (n, times): (u32, usize) =
                        (n.parse().unwrap_or(1), times.parse().unwrap_or(1));
                    items.extend(std::iter::repeat_n(n, times));
                }
                None => items.push(part.parse().unwrap_or(0)),
            }
        }
        layout.pages.push(LayoutPage {
            block,
            level,
            next,
            items,
        });
    }
    out
}

/// A key value from its text output, as the shape compares it.
fn value(text: &str, typ: i64) -> Bson {
    if text == "\\N" {
        return Bson::Null;
    }
    let text = text
        .replace("\\t", "\t")
        .replace("\\n", "\n")
        .replace("\\\\", "\\");
    match typ {
        NAME | TEXT | CHAR => Bson::String(text),
        BOOL => Bson::Boolean(text == "t"),
        OIDVECTOR | INT2VECTOR => normal(&Bson::String(text), typ),
        _ => text.parse::<i64>().map_or(Bson::String(text), Bson::Int64),
    }
}

/// The trees, one per built-in index, built on first use.
fn trees() -> &'static HashMap<i64, OnceLock<Option<Tree>>> {
    static T: OnceLock<HashMap<i64, OnceLock<Option<Tree>>>> = OnceLock::new();
    T.get_or_init(|| layouts().keys().map(|k| (*k, OnceLock::new())).collect())
}

fn layouts() -> &'static HashMap<i64, Layout> {
    static L: OnceLock<HashMap<i64, Layout>> = OnceLock::new();
    L.get_or_init(|| parse(include_str!("pg15_catalog_indexes.txt")))
}

fn tree(index: i64) -> Option<&'static Tree> {
    trees()
        .get(&index)?
        .get_or_init(|| {
            let l = layouts().get(&index)?;
            let unique = builtin().indexes.get(&index).is_some_and(|i| i.unique);
            let shape = CatalogShape {
                types: l.types.clone(),
                unique,
            };
            let btree = if l.root == 0 {
                BTree::build(&shape, Vec::new())
            } else {
                BTree::from_layout(&shape, l.root, &l.pages, l.in_order())?
            };
            Some(Tree { shape, btree })
        })
        .as_ref()
}

/// A catalog index's size as the planner reads it for `IndexOptInfo`:
/// its blocks (`RelationGetNumberOfBlocks`, the metapage among them) and
/// its root's level (`_bt_getrootheight`), from `initdb`'s pages.
pub(crate) fn initdb_index_size(index: i64) -> Option<(u32, u32)> {
    let t = tree(index)?;
    Some((t.btree.pages(), t.btree.height()))
}

/// A catalog index's key columns, by type: how PostgreSQL's default
/// operator class orders each (a catalog's `name` and `text` columns
/// under the "C" collation) and lays it out in an index tuple.
pub(crate) struct CatalogShape {
    types: Vec<i64>,
    unique: bool,
}

const BOOL: i64 = 16;
const CHAR: i64 = 18;
const NAME: i64 = 19;
const INT8: i64 = 20;
const INT2: i64 = 21;
const INT2VECTOR: i64 = 22;
const INT4: i64 = 23;
const TEXT: i64 = 25;
const OIDVECTOR: i64 = 30;
const FLOAT4: i64 = 700;
const FLOAT8: i64 = 701;

/// A value as the shape compares it: integers as int64, `oidvector` and
/// `int2vector` as arrays of them, `name`, `text` and `"char"` as strings.
fn normal(v: &Bson, typ: i64) -> Bson {
    match (v, typ) {
        (Bson::Null, _) => Bson::Null,
        (Bson::String(s), OIDVECTOR | INT2VECTOR) => Bson::Array(
            s.split_whitespace()
                .filter_map(|x| x.parse::<i64>().ok())
                .map(Bson::Int64)
                .collect(),
        ),
        (Bson::Array(a), _) => Bson::Array(
            a.iter()
                .map(|x| crate::bson_i64(x).map_or(Bson::Null, Bson::Int64))
                .collect(),
        ),
        (Bson::String(_), _) | (Bson::Boolean(_), _) => v.clone(),
        _ => crate::bson_i64(v).map_or_else(|| v.clone(), Bson::Int64),
    }
}

fn from_k(k: &K, typ: i64) -> Bson {
    match k {
        K::O(v) => Bson::Int64(*v),
        K::N(s) => normal(&Bson::String(s.clone()), typ),
    }
}

/// `oid`, `regproc` and the other unsigned 32-bit types compare unsigned.
fn unsigned(typ: i64) -> bool {
    !matches!(typ, INT2 | INT4 | INT8)
}

impl Shape for CatalogShape {
    fn nkeyatts(&self) -> usize {
        self.types.len()
    }

    fn cmp_attr(&self, attr: usize, a: &Bson, b: &Bson) -> Ordering {
        let typ = self.types.get(attr).copied().unwrap_or(0);
        let int = |v: i64| if unsigned(typ) { v as u32 as i64 } else { v };
        match (a, b) {
            // NULLS LAST, the default.
            (Bson::Null, Bson::Null) => Ordering::Equal,
            (Bson::Null, _) => Ordering::Greater,
            (_, Bson::Null) => Ordering::Less,
            (Bson::String(x), Bson::String(y)) => x.as_bytes().cmp(y.as_bytes()),
            (Bson::Boolean(x), Bson::Boolean(y)) => x.cmp(y),
            // `btoidvectorcmp`, `btint2vectorcmp`: the shorter first, then
            // element by element.
            (Bson::Array(x), Bson::Array(y)) => x.len().cmp(&y.len()).then_with(|| {
                for (p, q) in x.iter().zip(y) {
                    let (p, q) = (crate::bson_i64(p), crate::bson_i64(q));
                    let o = p.map(|v| v as u32).cmp(&q.map(|v| v as u32));
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            }),
            _ => match (crate::bson_i64(a), crate::bson_i64(b)) {
                (Some(x), Some(y)) => int(x).cmp(&int(y)),
                _ => Ordering::Equal,
            },
        }
    }

    /// `index_form_tuple`: the 8-byte header (16 with a null bitmap), each
    /// value at its type's alignment, MAXALIGNed.
    fn tuple_size(&self, attrs: &[Bson]) -> i64 {
        let align = |n: i64, a: i64| (n + a - 1) / a * a;
        let nulls = attrs.iter().any(|v| matches!(v, Bson::Null));
        let mut size: i64 = if nulls { 16 } else { 8 };
        for (v, typ) in attrs.iter().zip(&self.types) {
            if matches!(v, Bson::Null) {
                continue;
            }
            size = match *typ {
                BOOL | CHAR => size + 1,
                // `name_ops` keeps a `cstring`: its bytes and a NUL.
                NAME => {
                    size + match v {
                        Bson::String(s) => s.len() as i64 + 1,
                        _ => 1,
                    }
                }
                INT2 => align(size, 2) + 2,
                INT8 | FLOAT8 => align(size, 8) + 8,
                // Plain storage keeps the 4-byte header: `vl_len_`, `ndim`,
                // `dataoffset`, `elemtype`, `dim1`, `lbound1`, the values.
                OIDVECTOR | INT2VECTOR => {
                    let n = match v {
                        Bson::Array(a) => a.len() as i64,
                        _ => 0,
                    };
                    let elem = if *typ == OIDVECTOR { 4 } else { 2 };
                    align(size, 4) + 24 + elem * n
                }
                // A short value takes a 1-byte header, unaligned.
                TEXT => {
                    let n = match v {
                        Bson::String(s) => s.len() as i64,
                        _ => 0,
                    };
                    if n < 127 {
                        size + 1 + n
                    } else {
                        align(size, 4) + 4 + n
                    }
                }
                FLOAT4 | INT4 => align(size, 4) + 4,
                _ => align(size, 4) + 4,
            };
        }
        (size + 7) & !7
    }

    fn dedup(&self) -> bool {
        true
    }

    fn unique(&self) -> bool {
        self.unique
    }
}

/// One built-in catalog index.
struct Tree {
    shape: CatalogShape,
    btree: BTree,
}

impl PgHandler {
    /// Count the pages each catalog scan in `scans` read, in order.
    ///
    /// An index scan reads the metapage, which `_bt_metaversion` and then
    /// `_bt_getroot` read and cache in `rd_amcache` on the backend's first
    /// scan -- but never cache while the index has no root, so that both
    /// read it on every scan of an empty index -- then the way down and
    /// the leaves; and then the heap block of each tuple it returns, in
    /// index order, a block again only when the one before was another
    /// (`heapam_index_fetch_tuple` keeps its buffer pinned, and
    /// `ReleaseAndReadBuffer` reads nothing for the block it holds).
    /// `_bt_getrootheight` (no key) reads the metapage alone, and caches
    /// it as they do. A sequential scan reads every block of the heap.
    ///
    /// The heap is `initdb`'s (`catalog_heap.rs`): a row added after it is
    /// not yet placed (RP-291).
    pub(crate) fn catalog_index_pages(&self, be: &mut Backend, scans: Vec<PageScan>) {
        if scans.is_empty() || !self.tracking() {
            return;
        }
        let stats = self.stats();
        let capacity = self.buffer_pages();
        let heaps = initdb_heaps();
        for scan in scans {
            let (index, key) = match scan {
                PageScan::Seq(table) => {
                    if let Some(h) = heaps.get(&table) {
                        stats.pages(table, 0..i64::from(h.blocks), capacity);
                    }
                    continue;
                }
                PageScan::Index(index, key) => (index, key),
            };
            let Some(t) = tree(index) else {
                continue;
            };
            let mut blocks: Vec<i64> = Vec::new();
            let mut heap: Vec<i64> = Vec::new();
            if t.btree.root.is_none() {
                // No root: nothing is cached.
                blocks.push(0);
                if key.is_some() {
                    blocks.push(0);
                }
            } else {
                if be.read_metapage(index) {
                    blocks.push(0);
                }
                if let Some(key) = key {
                    let prefix: Vec<Bson> = key
                        .iter()
                        .zip(&t.shape.types)
                        .map(|(k, typ)| from_k(k, *typ))
                        .collect();
                    let (pages, tids) = t.btree.prefix_scan(&t.shape, &prefix);
                    blocks.extend(pages.into_iter().map(i64::from));
                    for tid in tids {
                        let block = tid / 65536;
                        if heap.last() != Some(&block) {
                            heap.push(block);
                        }
                    }
                }
            }
            stats.pages(index, blocks, capacity);
            if !heap.is_empty() {
                if let Some(table) = builtin().indexes.get(&index).map(|i| i.table) {
                    stats.pages(table, heap, capacity);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recorded_index_builds() {
        assert!(layouts().len() > 150);
        for (oid, l) in layouts() {
            let t = tree(*oid).unwrap_or_else(|| panic!("index {oid} does not fill its layout"));
            assert_eq!(t.btree.root.is_none(), l.root == 0, "index {oid}");
        }
        // PostgreSQL's order is the shape's: every index's tuples are
        // already sorted as this model compares them.
        for (oid, l) in layouts() {
            let shape = &tree(*oid).expect("built").shape;
            for w in l.in_order().windows(2) {
                let o = (0..shape.nkeyatts())
                    .map(|a| shape.cmp_attr(a, &w[0].0[a], &w[1].0[a]))
                    .find(|o| *o != Ordering::Equal)
                    .unwrap_or(Ordering::Equal);
                assert_ne!(
                    o,
                    Ordering::Greater,
                    "index {oid}: {:?} before {:?}",
                    w[0],
                    w[1]
                );
            }
        }
        // pg_class_relname_nsp_index: a root over three leaves.
        let c = &layouts()[&2663];
        assert_eq!((c.root, c.in_order().len()), (3, 410));
        assert_eq!(c.types, vec![NAME, 26]);
    }

    #[test]
    fn every_index_entry_names_a_block_of_its_initdb_heap() {
        // Each index's entries against its table's recorded heap: one per
        // tuple, as many on each heap block (an entry names a HOT chain's
        // root, which is on its live tuple's page).
        let heaps = crate::catalog_heap::initdb_heaps();
        let per_block = |tids: &mut dyn Iterator<Item = i64>| {
            let mut m: HashMap<i64, usize> = HashMap::new();
            for t in tids {
                *m.entry(t / 65536).or_default() += 1;
            }
            m
        };
        for (index, table) in [
            (2662, 1259),
            (2663, 1259),
            (3455, 1259),
            (2658, 1249),
            (2659, 1249),
            (2690, 1255),
            (2691, 1255),
            (2684, 2615),
            (2685, 2615),
            (2678, 2610),
            (2679, 2610),
            (2703, 1247),
            (2704, 1247),
        ] {
            let entries = layouts()[&index].in_order();
            let heap = &heaps[&table];
            assert_eq!(
                per_block(&mut entries.iter().map(|e| e.1)),
                per_block(&mut heap.tids.iter().copied()),
                "index {index} over {}",
                heap.name
            );
        }
    }

    #[test]
    fn a_scan_returns_its_rows_heap_tids() {
        // pg_class_relname_nsp_index: `pg_class` in `pg_catalog` is one
        // row, on a block of pg_class's initdb heap.
        let t = tree(2663).expect("pg_class_relname_nsp_index");
        let (_, tids) = t.btree.prefix_scan(
            &t.shape,
            &[Bson::String("pg_class".into()), Bson::Int64(11)],
        );
        assert_eq!(tids.len(), 1);
        assert!(tids[0] / 65536 < 14);
        // pg_attribute_relid_attnum_index by pg_class's oid: its every
        // column, user and system.
        let t = tree(2659).expect("pg_attribute_relid_attnum_index");
        let (_, tids) = t.btree.prefix_scan(&t.shape, &[Bson::Int64(1259)]);
        assert_eq!(tids.len(), 33 + 6);
    }

    #[test]
    fn a_name_lookup_reads_the_way_down_to_its_leaf() {
        let t = tree(2663).expect("pg_class_relname_nsp_index");
        let pages = |name: &str| {
            t.btree
                .prefix_scan_pages(&t.shape, &[Bson::String(name.into()), Bson::Int64(11)])
        };
        // The root, then the one leaf the name is on.
        assert_eq!(pages("pg_class").len(), 2);
        assert_eq!(pages("pg_class")[0], 3);
    }

    #[test]
    fn a_scan_reads_the_way_down_and_the_leaves_it_crosses() {
        let shape = CatalogShape {
            types: vec![26, INT2],
            unique: true,
        };
        // Two leaves under a root, as PostgreSQL recorded them.
        let pages = vec![
            LayoutPage {
                block: 1,
                level: 0,
                next: 2,
                items: vec![1; 4],
            },
            LayoutPage {
                block: 2,
                level: 0,
                next: 0,
                items: vec![1; 4],
            },
            LayoutPage {
                block: 3,
                level: 1,
                next: 0,
                items: vec![1, 2],
            },
        ];
        // Relation 10's attributes 1-3, relation 11's 1-3, relation 12's 1-2.
        let mut entries = Vec::new();
        for (rel, n) in [(10, 3), (11, 3), (12, 2)] {
            for att in 1..=n {
                entries.push((
                    vec![Bson::Int64(rel), Bson::Int64(att)],
                    entries.len() as i64,
                ));
            }
        }
        let t = BTree::from_layout(&shape, 3, &pages, entries).expect("fills the layout");
        let scan = |k: &[i64]| {
            let k: Vec<Bson> = k.iter().map(|v| Bson::Int64(*v)).collect();
            t.prefix_scan_pages(&shape, &k)
        };
        assert_eq!(scan(&[10]), vec![3, 1]);
        // Relation 11's rows straddle the two leaves.
        assert_eq!(scan(&[11]), vec![3, 1, 2]);
        assert_eq!(scan(&[11, 2]), vec![3, 2]);
        assert_eq!(scan(&[12]), vec![3, 2]);
        // Too few rows for the layout: no tree.
        assert!(BTree::from_layout(&shape, 3, &pages, Vec::new()).is_none());
    }
}
