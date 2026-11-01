//! The pages a catalog index scan reads (RP-291), counted as
//! `pg_statio_sys_indexes` counts them: each B-tree index of the catalogs
//! is modelled page by page ([`crate::btree_model`]), and a scan reads the
//! metapage on its backend's first use of the index (`_bt_getroot`, which
//! then caches it in `rd_amcache`), the way down from the root, and each
//! leaf it walks right across (`_bt_first`, `_bt_readpage`).
//!
//! A database starts with PostgreSQL 15.19's own pages: `initdb`'s layout
//! of every built-in catalog index, read from a fresh cluster's files by
//! `tools/probes/pg15_system_catalog_dump.py` into
//! `pg15_catalog_indexes.txt` -- which block holds which tuples, each
//! posting list's TID count, every internal page's downlinks. The built-in
//! rows' keys fill it in index order; a row the database already held
//! beyond them when the tree is built (on its first scan) is inserted as
//! `nbtinsert.c` inserts it, splitting pages as PostgreSQL would.
//!
//! Not yet modelled, and recorded in RP-291: a row added after the tree is
//! built (placing later catalog rows is the heap slice's), the heap's own
//! pages (the TIDs here are ordinals, the built-in rows' in index order,
//! later rows' after them), a non-HOT update of a catalog row, and
//! VACUUM's deletion of dead entries.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bson::Bson;

use crate::btree_model::{BTree, LayoutPage, Shape};
use crate::catalog_facts::builtin;
use crate::catcache::{Backend, Key, K};
use crate::PgHandler;

/// `FirstNormalObjectId`: an oid below it is `initdb`'s.
const FIRST_NORMAL_OBJECT_ID: i64 = 16384;

/// One built-in index's recorded layout.
struct Layout {
    root: u32,
    pages: Vec<LayoutPage>,
    /// Its heap TIDs, every leaf's counts summed.
    tids: usize,
}

fn layouts() -> &'static HashMap<i64, Layout> {
    static L: OnceLock<HashMap<i64, Layout>> = OnceLock::new();
    L.get_or_init(|| parse(include_str!("pg15_catalog_indexes.txt")))
}

/// `\index oid root level`, then `block flags next level items` per page,
/// a leaf's items run-length coded (`1x40`).
fn parse(text: &str) -> HashMap<i64, Layout> {
    let mut out: HashMap<i64, Layout> = HashMap::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("\\index ") {
            let f: Vec<i64> = rest.split(' ').filter_map(|v| v.parse().ok()).collect();
            if let [oid, root, _level] = f[..] {
                out.insert(
                    oid,
                    Layout {
                        root: root as u32,
                        pages: Vec::new(),
                        tids: 0,
                    },
                );
                current = Some(oid);
            }
            continue;
        }
        let Some(layout) = current.and_then(|o| out.get_mut(&o)) else {
            continue;
        };
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
        if level == 0 {
            layout.tids += items.iter().map(|&n| n as usize).sum::<usize>();
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
                NAME => size + 64,
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

/// One catalog index of one database (or of the cluster, a shared
/// catalog's).
struct Tree {
    shape: CatalogShape,
    btree: BTree,
}

type Trees = HashMap<(usize, i64, i64), Tree>;

fn trees() -> &'static Mutex<Trees> {
    static T: OnceLock<Mutex<Trees>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The store's trees go with it.
pub(crate) fn forget(store: usize) {
    trees()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(s, _, _), _| *s != store);
}

impl PgHandler {
    /// Count the pages each catalog index scan in `scans` read, in order:
    /// the metapage, which `_bt_metaversion` and then `_bt_getroot` read
    /// and cache in `rd_amcache` on the backend's first scan -- but never
    /// cache while the index has no root, so that both read it on every
    /// scan of an empty index -- then the way down and the leaves.
    /// `_bt_getrootheight` (no key) reads the metapage alone, and caches
    /// it as they do.
    pub(crate) fn catalog_index_pages(&self, be: &mut Backend, scans: Vec<(i64, Option<Key>)>) {
        if scans.is_empty() || !self.tracking() {
            return;
        }
        let stats = self.stats();
        let capacity = self.buffer_pages();
        for (index, key) in scans {
            let Some(pages) = self.catalog_scan_pages(index, key.as_ref()) else {
                continue;
            };
            let mut blocks: Vec<i64> = Vec::new();
            match pages {
                // No root: nothing is cached.
                None => blocks.extend(if key.is_some() {
                    [0, 0].as_slice()
                } else {
                    &[0]
                }),
                Some(pages) => {
                    if be.read_metapage(index) {
                        blocks.push(0);
                    }
                    if key.is_some() {
                        blocks.extend(pages.into_iter().map(i64::from));
                    }
                }
            }
            stats.pages(index, blocks, capacity);
        }
    }

    /// The pages below the metapage a scan of `index` for `key` reads
    /// (none asked without one), `Some(None)` for an index with no root,
    /// `None` for one this does not model.
    ///
    /// A tree is built on its database's first scan of the index, with the
    /// catalog's other indexes' from the same read, and is not brought up
    /// to the catalogs as they change after (RP-291, the heap slice):
    /// reading every row of a catalog again after each change made a new
    /// session's first statement after any DDL slower by hundreds of
    /// milliseconds, reading them inside a transaction that changed the
    /// catalogs left its later lookups blind to its own new objects, and
    /// reading them from a thread of their own lost tables other sessions
    /// had just made.
    fn catalog_scan_pages(&self, index: i64, key: Option<&Key>) -> Option<Option<Vec<u32>>> {
        let ix = builtin().indexes.get(&index)?;
        let table = builtin().relations.get(&ix.table)?;
        let db = if table.shared { 0 } else { self.stats().db };
        let store = std::sync::Arc::as_ptr(&self.storage) as usize;
        let tree_key = (store, db, index);
        let built = trees()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&tree_key);
        if !built {
            if self.marks.catalog_changed() {
                // Not read inside a transaction that changed the catalogs;
                // built once it ends.
                return None;
            }
            self.build_catalog_trees(ix.table, store, db);
        }
        let t = trees().lock().unwrap_or_else(|e| e.into_inner());
        let tree = t.get(&tree_key)?;
        if tree.btree.root.is_none() {
            return Some(None);
        }
        let Some(key) = key else {
            return Some(Some(Vec::new()));
        };
        let prefix: Vec<Bson> = key
            .iter()
            .zip(&tree.shape.types)
            .map(|(k, typ)| from_k(k, *typ))
            .collect();
        Some(Some(tree.btree.prefix_scan_pages(&tree.shape, &prefix)))
    }

    /// Build the trees of every index of catalog `table` this database has
    /// none of yet, from one read of its rows.
    fn build_catalog_trees(&self, table: i64, store: usize, db: i64) {
        let Some(indexes) = builtin().indexes_of.get(&table) else {
            return;
        };
        let Some(rows) = self.catalog_rows(table) else {
            return;
        };
        for &index in indexes {
            let Some(ix) = builtin().indexes.get(&index) else {
                continue;
            };
            if !layouts().contains_key(&index) {
                continue;
            }
            let rows = rows.keys(&ix.keys);
            let mut t = trees().lock().unwrap_or_else(|e| e.into_inner());
            t.entry((store, db, index))
                .or_insert_with(|| Tree::new(index, rows));
        }
    }

    /// Every row of catalog `table`, with whether `initdb` made it.
    fn catalog_rows(&self, table: i64) -> Option<TableRows> {
        let rel = self.cr_catalog(table)?;
        let def = Self::virtual_table(&rel.name)?;
        let field = |name: &str| def.field_of(name).unwrap_or_default();
        // The row's own identity: its oid, a dependency's object, else its
        // first column (the relation an attribute or index belongs to, the
        // object a description is of).
        let identity = if rel.columns.iter().any(|c| c.1 == "oid") {
            field("oid")
        } else if matches!(rel.name.as_str(), "pg_depend" | "pg_shdepend") {
            field("objid")
        } else {
            rel.columns
                .iter()
                .find(|c| c.0 == 1)
                .map_or_else(String::new, |c| field(&c.1))
        };
        let docs = self
            .virtual_rows_uncached(&rel.name, &bson::Document::new())
            .unwrap_or_default();
        let rows = docs
            .into_iter()
            .map(|row| {
                let builtin = row
                    .get(&identity)
                    .and_then(crate::bson_i64)
                    .is_some_and(|o| (0..FIRST_NORMAL_OBJECT_ID).contains(&o));
                (builtin, row)
            })
            .collect();
        let columns = rel
            .columns
            .iter()
            .map(|(att, name, typ)| (*att, field(name), *typ))
            .collect();
        Some(TableRows { columns, rows })
    }
}

/// A catalog's rows, read once for all its indexes.
struct TableRows {
    /// `(attnum, the row's field, type)`.
    columns: Vec<(i64, String, i64)>,
    rows: Vec<(bool, bson::Document)>,
}

impl TableRows {
    /// Each row's values of an index's key columns (`attnums`).
    fn keys(&self, attnums: &[i64]) -> CatalogRows {
        let cols: Vec<(String, i64)> = attnums
            .iter()
            .map(|a| {
                self.columns
                    .iter()
                    .find(|c| c.0 == *a)
                    .map_or((String::new(), 0), |c| (c.1.clone(), c.2))
            })
            .collect();
        CatalogRows {
            types: cols.iter().map(|c| c.1).collect(),
            rows: self
                .rows
                .iter()
                .map(|(b, row)| {
                    let key = cols
                        .iter()
                        .map(|(f, typ)| normal(row.get(f).unwrap_or(&Bson::Null), *typ))
                        .collect();
                    (*b, key)
                })
                .collect(),
        }
    }
}

#[derive(Default)]
struct CatalogRows {
    types: Vec<i64>,
    rows: Vec<(bool, Vec<Bson>)>,
}

impl Tree {
    /// `initdb`'s pages holding the built-in rows, then every later row
    /// inserted. Without a recorded layout, or when the built-in rows do
    /// not fill it, the built-in rows are loaded as CREATE INDEX loads
    /// them.
    fn new(index: i64, rows: CatalogRows) -> Tree {
        let unique = builtin().indexes.get(&index).is_some_and(|i| i.unique);
        let shape = CatalogShape {
            types: rows.types,
            unique,
        };
        let (initdb, later): (Vec<_>, Vec<_>) = rows.rows.into_iter().partition(|(b, _)| *b);
        let initdb: Vec<(Vec<Bson>, i64)> = initdb
            .into_iter()
            .enumerate()
            .map(|(i, (_, k))| (k, i as i64))
            .collect();
        let first_later = initdb.len() as i64;
        let mut btree = layouts()
            .get(&index)
            .filter(|l| l.tids == initdb.len())
            .and_then(|l| BTree::from_layout(&shape, l.root, &l.pages, initdb.clone()))
            .unwrap_or_else(|| BTree::build(&shape, initdb));
        for (tid, (_, k)) in (first_later..).zip(later) {
            btree.insert(&shape, k, tid);
        }
        Tree { shape, btree }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layouts_parse() {
        let l = layouts();
        // pg_class_relname_nsp_index: a root over three leaves.
        let c = &l[&2663];
        assert_eq!(c.root, 3);
        assert_eq!(c.tids, 136 + 120 + 154);
        assert_eq!(c.pages.len(), 4);
        assert!(l.len() > 150);
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
