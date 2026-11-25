//! Where a database's catalog rows written after `initdb` are (RP-291).
//!
//! PostgreSQL puts a catalog row DDL writes in the catalog's heap as it
//! puts any tuple (`heap_insert`, `RelationGetBufferForTuple`): the
//! backend's target block first, then the free space map, then a new
//! block; and an update of a row DDL just wrote goes beside it, a HOT
//! chain when no indexed column changed (`heap_update`). Each write
//! clears its page's visibility map bit. This server's catalogs are
//! computed, not stored as tuples, so the heaps are modelled here: each
//! database's catalogs start as `initdb` left them
//! ([`crate::catalog_heap::initdb_heap_model`]), and the DDL a statement
//! ran writes its rows into them, with each row's key in every index of
//! its catalog, for a scan that returns the row to read its block
//! (`catalog_btree.rs`).
//!
//! What is placed, and what not yet (both recorded in RP-291): CREATE
//! TABLE's rows in `pg_class`, `pg_type`, `pg_attribute` and `pg_index`
//! (the table, its row type and array type, its TOAST table and that
//! table's index, each index of its constraints), and the HOT update of
//! the table's `pg_class` row a TOAST table makes; DROP TABLE's deletion
//! of them, as dead rows no later scan reads (`_bt_killitems` once the
//! first scan has met them). A row any other DDL writes is not placed,
//! nor are the other catalogs' rows, the index entries in the index
//! trees, the space a deletion leaves for pruning and VACUUM, or a
//! restart: the places are kept in memory.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bson::Bson;

use crate::heap_page::{tid, untid, Heap};

/// One catalog row DDL wrote.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    /// Where its index entries point: the root of its HOT chain.
    pub(crate) ctid: i64,
    /// Where its live version is: the root, or the heap-only tuple a HOT
    /// update put on the root's page, which a heap scan meets in its
    /// place (`heapgettup`).
    at: i64,
    /// Its key in each index of the catalog, by index oid.
    pub(crate) keys: Vec<(i64, Vec<Bson>)>,
    /// Neither deleted by a committed transaction nor written by one that
    /// rolled back.
    pub(crate) live: bool,
    /// The statement's object it belongs to (a table's oid), by which a
    /// DROP finds it.
    group: i64,
}

/// One catalog's heap in one database.
struct Catalog {
    heap: Heap,
    rows: Vec<Row>,
    /// Each row's position by an index and its key's leading column: a
    /// scan finds its rows without walking the rest.
    by_lead: HashMap<(i64, String), Vec<usize>>,
}

/// An index key's leading column, as [`Catalog::by_lead`] files it.
fn lead(key: &[Bson]) -> String {
    format!("{:?}", key.first())
}

/// What an open transaction did, settled when it ends.
enum Pending {
    /// Rows it wrote: dead if it rolls back.
    Created(i64, i64),
    /// A group it deleted: dead once it commits.
    Dropped(i64, i64),
    /// A database it dropped: its heaps go once it commits.
    DroppedDatabase(i64),
}

#[derive(Default)]
struct State {
    /// By store and database oid, each catalog's heap by its oid.
    dbs: HashMap<(usize, i64), HashMap<i64, Catalog>>,
    /// By store and backend pid.
    pending: HashMap<(usize, i32), Vec<Pending>>,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// One catalog row a statement writes, in the order PostgreSQL writes it.
#[derive(Debug, Clone)]
pub(crate) struct Write {
    pub(crate) catalog: i64,
    /// Its MAXALIGNed length (`heap_form_tuple`'s, which `PageAddItem`
    /// aligns).
    pub(crate) len: u32,
    /// Its key in each index of the catalog; none for a row whose object
    /// this server does not keep (a TOAST table's), placed for its space.
    pub(crate) keys: Vec<(i64, Vec<Bson>)>,
    /// An update of the write at this position in the same list, not an
    /// insert: its new version goes beside it, a HOT chain when `hot`.
    pub(crate) update_of: Option<(usize, bool)>,
}

/// The database whose copy of `catalog` a backend of `db` writes: a
/// shared catalog has one heap in the cluster, kept under database 0.
fn home(db: i64, catalog: i64) -> i64 {
    if crate::catalog_facts::is_shared_relation(catalog) {
        0
    } else {
        db
    }
}

/// Write `writes` as backend `pid` of database `db`, for the object
/// `group`, in order.
pub(crate) fn write(store: usize, db: i64, pid: i32, group: i64, writes: &[Write]) {
    let mut st = state();
    let mut placed: Vec<i64> = Vec::with_capacity(writes.len());
    for w in writes {
        let cats = st.dbs.entry((store, home(db, w.catalog))).or_default();
        let Some(cat) = catalog(cats, w.catalog) else {
            placed.push(-1);
            continue;
        };
        let old = w
            .update_of
            .and_then(|(i, hot)| Some((*placed.get(i)?, writes.get(i)?.len, hot)));
        let ctid = match old {
            Some((old, old_len, hot)) if old >= 0 => {
                let new = cat
                    .heap
                    .update(pid, Some(old), old_len, w.len, hot, 0, Bson::Null);
                if hot && untid(new).map(|t| t.0) == untid(old).map(|t| t.0) {
                    // Reached through the chain's root: no entry of its own.
                    if let Some(r) = cat.rows.iter_mut().find(|r| r.ctid == old && r.live) {
                        r.at = new;
                    }
                    cat.heap.accessed.clear();
                    placed.push(old);
                    continue;
                }
                // Out of place: the old version is dead, the new one has
                // entries of its own.
                if let Some(r) = cat.rows.iter_mut().find(|r| r.ctid == old) {
                    r.live = false;
                }
                new
            }
            _ => {
                let (b, off) = cat.heap.place(pid, w.len, true);
                tid(b, off)
            }
        };
        cat.heap.accessed.clear();
        cat.heap.inserting.clear();
        cat.heap.inserting_by_xid.clear();
        placed.push(ctid);
        for (index, key) in &w.keys {
            cat.by_lead
                .entry((*index, lead(key)))
                .or_default()
                .push(cat.rows.len());
        }
        cat.rows.push(Row {
            ctid,
            at: ctid,
            keys: w.keys.clone(),
            live: true,
            group,
        });
    }
    let dbs: std::collections::BTreeSet<i64> = writes.iter().map(|w| home(db, w.catalog)).collect();
    let pending = st.pending.entry((store, pid)).or_default();
    for d in dbs {
        pending.push(Pending::Created(d, group));
    }
}

/// The catalog's heap in `cats`, built from `initdb`'s on first use.
fn catalog(cats: &mut HashMap<i64, Catalog>, oid: i64) -> Option<&mut Catalog> {
    if let std::collections::hash_map::Entry::Vacant(e) = cats.entry(oid) {
        e.insert(Catalog {
            heap: crate::catalog_heap::initdb_heap_model(oid)?,
            rows: Vec::new(),
            by_lead: HashMap::new(),
        });
    }
    cats.get_mut(&oid)
}

/// Backend `pid` deleted the rows of `group` (a DROP) from `catalogs`:
/// dead once its transaction commits.
pub(crate) fn drop_group(store: usize, db: i64, pid: i32, group: i64, catalogs: &[i64]) {
    let homes: std::collections::BTreeSet<i64> = catalogs.iter().map(|c| home(db, *c)).collect();
    let mut st = state();
    let pending = st.pending.entry((store, pid)).or_default();
    for d in homes {
        pending.push(Pending::Dropped(d, group));
    }
}

/// Backend `pid` dropped database `oid`: once it commits, its
/// `pg_database` row is dead and its catalogs' heaps are gone.
pub(crate) fn drop_database(store: usize, pid: i32, oid: i64) {
    let mut st = state();
    let pending = st.pending.entry((store, pid)).or_default();
    pending.push(Pending::Dropped(0, oid));
    pending.push(Pending::DroppedDatabase(oid));
}

/// Backend `pid`'s transaction ended.
pub(crate) fn end_xact(store: usize, pid: i32, commit: bool) {
    let mut st = state();
    let Some(done) = st.pending.remove(&(store, pid)) else {
        return;
    };
    for p in done {
        let (db, group, dies) = match p {
            Pending::Created(db, g) => (db, g, !commit),
            Pending::Dropped(db, g) => (db, g, commit),
            Pending::DroppedDatabase(db) => {
                if commit {
                    st.dbs.remove(&(store, db));
                }
                continue;
            }
        };
        if !dies {
            continue;
        }
        if let Some(cats) = st.dbs.get_mut(&(store, db)) {
            for cat in cats.values_mut() {
                for r in cat.rows.iter_mut().filter(|r| r.group == group) {
                    r.live = false;
                }
            }
        }
    }
}

/// The live rows of `catalog` with a key in `index` whose leading column
/// is `first` (every one with a key in it when `None`): each key and the
/// row's place.
pub(crate) fn rows(
    store: usize,
    db: i64,
    catalog: i64,
    index: i64,
    first: Option<&Bson>,
) -> Vec<(Vec<Bson>, i64)> {
    let st = state();
    let Some(cat) = st
        .dbs
        .get(&(store, home(db, catalog)))
        .and_then(|c| c.get(&catalog))
    else {
        return Vec::new();
    };
    let key_of = |r: &Row| {
        r.keys
            .iter()
            .find(|(i, _)| *i == index)
            .map(|k| (k.1.clone(), r.ctid))
    };
    match first {
        Some(v) => cat
            .by_lead
            .get(&(index, lead(std::slice::from_ref(v))))
            .into_iter()
            .flatten()
            .filter_map(|i| cat.rows.get(*i))
            .filter(|r| r.live)
            .filter_map(key_of)
            .collect(),
        None => cat
            .rows
            .iter()
            .filter(|r| r.live)
            .filter_map(key_of)
            .collect(),
    }
}

/// The live rows of `catalog` with a key in `index`, each key and where
/// its live version is, which a heap scan reads (`heapgettup`), not the
/// root of its HOT chain its index entries point at.
pub(crate) fn live_places(
    store: usize,
    db: i64,
    catalog: i64,
    index: i64,
) -> Vec<(Vec<Bson>, i64)> {
    let st = state();
    let Some(cat) = st
        .dbs
        .get(&(store, home(db, catalog)))
        .and_then(|c| c.get(&catalog))
    else {
        return Vec::new();
    };
    cat.rows
        .iter()
        .filter(|r| r.live)
        .filter_map(|r| {
            let (_, key) = r.keys.iter().find(|(i, _)| *i == index)?;
            Some((key.clone(), r.at))
        })
        .collect()
}

/// The visibility map's all-visible bit of `block` of `catalog` and the
/// heap's length, as the database's writes left them; `None` while no
/// write has reached the catalog.
pub(crate) fn heap_state(store: usize, db: i64, catalog: i64) -> Option<(Vec<bool>, u32)> {
    let st = state();
    let cat = st.dbs.get(&(store, home(db, catalog)))?.get(&catalog)?;
    Some((
        cat.heap.pages.iter().map(|p| p.all_visible).collect(),
        cat.heap.nblocks(),
    ))
}

/// The server's store is gone.
pub(crate) fn forget_store(store: usize) {
    let mut st = state();
    st.dbs.retain(|(s, _), _| *s != store);
    st.pending.retain(|(s, _), _| *s != store);
}

/// `heap_form_tuple`'s length of a `pg_index` row of `natts` columns
/// (no expressions, no predicate), MAXALIGNed: the header and its null
/// bitmap (21 columns) in 32 bytes, then the fixed columns (23 bytes) and
/// the four vectors, each aligned to 4 with its 4-byte varlena header and
/// array header (20 bytes). Measured on PostgreSQL 15: 166 for one
/// column, 176 for two.
pub(crate) fn pg_index_len(natts: u32) -> u32 {
    let align4 = |n: u32| (n + 3) & !3;
    let mut off = 23;
    for elem in [2, 4, 4, 2] {
        off = align4(off) + 24 + elem * natts;
    }
    (32 + off + 7) & !7
}

/// A `pg_class` row with no `relacl`, `reloptions` or `relpartbound`
/// (172 bytes), MAXALIGNed.
pub(crate) const CLASS_LEN: u32 = 176;
/// A table's row type or array type's `pg_type` row (180 bytes).
pub(crate) const TYPE_LEN: u32 = 184;
/// A `pg_database` row with no `daticulocale`, `datcollversion` or
/// `datacl`, its `datcollate` and `datctype` the modelled cluster's `C`
/// (132 bytes: the header and its null bitmap in 32, the fixed columns in
/// 96, each text a 1-byte header and its byte).
pub(crate) const DATABASE_LEN: u32 = 136;
/// A `pg_attribute` row with no `attacl`, `attoptions`, `attfdwoptions`
/// or `attmissingval`. (PostgreSQL 15.20 sets `attoptions` on a table's
/// columns, so its are longer; this server is 15.19's.)
pub(crate) const ATTRIBUTE_LEN: u32 = 144;

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Bson {
        Bson::String(v.into())
    }

    #[test]
    fn a_pg_index_rows_length_is_postgresqls() {
        assert_eq!(pg_index_len(1), 168);
        assert_eq!(pg_index_len(2), 176);
    }

    #[test]
    fn create_tables_pg_class_rows_go_where_postgresql_puts_them() {
        // `create table rpc_t (id int primary key, v text)` in a fresh
        // database, measured on PostgreSQL 15.20 (whose initdb pg_class is
        // 15.19's): the table at (0,2), its TOAST table and index at (0,3)
        // and (0,4), the table's row again at (0,5) as the heap-only end of
        // a HOT chain from (0,2) (`reltoastrelid`), the primary key at (0,6).
        let store = 0x5eed_0001;
        let class = |k: &[(i64, Vec<Bson>)]| Write {
            catalog: 1259,
            len: CLASS_LEN,
            keys: k.to_vec(),
            update_of: None,
        };
        let t = vec![
            (2662, vec![Bson::Int64(16385)]),
            (2663, vec![s("rpc_t"), Bson::Int64(2200)]),
        ];
        let mut writes = vec![class(&t), class(&[]), class(&[])];
        writes.push(Write {
            update_of: Some((0, true)),
            ..class(&t)
        });
        writes.push(class(&[(2662, vec![Bson::Int64(16390)])]));
        write(store, 5, 1, 16385, &writes);
        let mut by_oid = rows(store, 5, 1259, 2662, None);
        by_oid.sort_by_key(|r| r.1);
        assert_eq!(
            by_oid,
            vec![(vec![Bson::Int64(16385)], 2), (vec![Bson::Int64(16390)], 6)]
        );
        let (visible, blocks) = heap_state(store, 5, 1259).expect("written");
        assert_eq!(blocks, 14);
        // Block 0's bit is cleared; the others keep initdb's.
        assert!(!visible[0]);
        assert_eq!(
            visible[1..],
            crate::catalog_heap::initdb_heaps()[&1259].all_visible[1..]
        );
        // Another database's pg_class is initdb's still.
        assert!(heap_state(store, 6, 1259).is_none());
        // A rolled-back CREATE leaves its rows dead; a committed DROP too.
        end_xact(store, 1, false);
        assert!(rows(store, 5, 1259, 2662, None).is_empty());
        forget_store(store);
    }

    #[test]
    fn a_drop_kills_its_groups_rows_at_commit() {
        let store = 0x5eed_0002;
        let w = Write {
            catalog: 2610,
            len: pg_index_len(1),
            keys: vec![(2679, vec![Bson::Int64(16390)])],
            update_of: None,
        };
        write(store, 5, 1, 16385, std::slice::from_ref(&w));
        end_xact(store, 1, true);
        assert_eq!(rows(store, 5, 2610, 2679, None).len(), 1);
        drop_group(store, 5, 2, 16385, &[2610]);
        end_xact(store, 2, false);
        assert_eq!(rows(store, 5, 2610, 2679, None).len(), 1);
        drop_group(store, 5, 2, 16385, &[2610]);
        end_xact(store, 2, true);
        assert!(rows(store, 5, 2610, 2679, None).is_empty());
        forget_store(store);
    }

    #[test]
    fn a_shared_catalogs_rows_are_every_databases() {
        // CREATE DATABASE from database 5: its pg_database row is in the
        // cluster's one pg_database heap, block 0 (measured on 15.20, whose
        // initdb pg_database is 15.19's).
        let store = 0x5eed_0003;
        let w = Write {
            catalog: 1262,
            len: DATABASE_LEN,
            keys: vec![(2671, vec![s("d2")]), (2672, vec![Bson::Int64(70000)])],
            update_of: None,
        };
        write(store, 5, 1, 70000, std::slice::from_ref(&w));
        end_xact(store, 1, true);
        let seen = |db: i64| rows(store, db, 1262, 2671, Some(&s("d2")));
        assert_eq!(seen(5), seen(70000));
        assert_eq!(seen(9).first().map(|r| r.1 / 65536), Some(0));
        drop_database(store, 2, 70000);
        end_xact(store, 2, true);
        assert!(seen(5).is_empty());
        forget_store(store);
    }
}
