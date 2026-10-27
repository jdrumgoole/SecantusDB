//! Each B-tree index's pages (RP-196), kept beside its table's heap: the
//! [`crate::btree_model`] of every index, fed by the heap's writes. A
//! non-HOT tuple version adds an index tuple to each of the table's
//! B-trees (a partial index's when its predicate holds), VACUUM's index
//! pass removes the TIDs its heap pass found dead, and CREATE INDEX, a
//! rewrite or a first touch builds one from the rows as they stand.
//!
//! The pages are what `pg_relation_size` of an index answers, and what
//! CREATE INDEX, a VACUUM that ran its index pass, and ANALYZE record in
//! the index's `pg_class` row.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use bson::{Bson, Document};
use pgwire::error::PgWireResult;

use crate::btree_model::{BTree, Shape};
use crate::PgHandler;

/// The B-tree operator families that are "equal image"
/// (`btequalimage`), which deduplication needs; PostgreSQL 15.19's list
/// by the column types they serve.
fn equal_image(pg_type: &str) -> bool {
    matches!(
        pg_type,
        "int2"
            | "int4"
            | "int8"
            | "oid"
            | "text"
            | "varchar"
            | "bpchar"
            | "name"
            | "bool"
            | "bytea"
            | "char"
            | "date"
            | "timestamp"
            | "timestamptz"
            | "time"
            | "timetz"
            | "uuid"
            | "inet"
            | "cidr"
            | "macaddr"
            | "macaddr8"
            | "money"
            | "bit"
            | "varbit"
            | "tid"
            | "pg_lsn"
            | "xid8"
            | "oidvector"
    ) || secantus_pgplan::is_user_enum(pg_type)
}

/// One B-tree index of a table, as the model needs it.
#[derive(Debug)]
pub(crate) struct IndexShape {
    pub(crate) oid: i64,
    pub(crate) name: String,
    /// The stored fields its tuples hold: its key columns', then its
    /// INCLUDE columns'.
    fields: Vec<String>,
    /// Each field's type, typmod and `(typlen, typalign)`; `None` for an
    /// expression's, sized by its value.
    types: Vec<Option<crate::heap::Laid>>,
    nkey: usize,
    desc: Vec<bool>,
    nulls_first: Vec<bool>,
    unique: bool,
    dedup: bool,
    /// A partial index's predicate, as a storage filter.
    predicate: Option<Document>,
    /// One no storage filter expresses (`a % 2 = 0`), evaluated per row
    /// (RP-236).
    computed: Option<secantus_pgplan::ColumnExpr>,
}

impl Shape for IndexShape {
    fn nkeyatts(&self) -> usize {
        self.nkey
    }

    fn cmp_attr(&self, attr: usize, a: &Bson, b: &Bson) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let (an, bn) = (matches!(a, Bson::Null), matches!(b, Bson::Null));
        let first = self.nulls_first.get(attr).copied().unwrap_or(false);
        let ord = match (an, bn) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, true) => {
                return if first {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            // Integers, the common key, compare as integers.
            _ => match (a, b) {
                (Bson::Int32(x), Bson::Int32(y)) => x.cmp(y),
                (Bson::Int64(x), Bson::Int64(y)) => x.cmp(y),
                (Bson::Int32(x), Bson::Int64(y)) => i64::from(*x).cmp(y),
                (Bson::Int64(x), Bson::Int32(y)) => x.cmp(&i64::from(*y)),
                // As `compare_values` orders them, without its walk through
                // the other kinds (a uuid or text key's every comparison).
                (Bson::String(x), Bson::String(y)) => x.cmp(y),
                (Bson::Binary(x), Bson::Binary(y)) => x.bytes.cmp(&y.bytes),
                _ => crate::compare_values(a, b),
            },
        };
        if self.desc.get(attr).copied().unwrap_or(false) {
            ord.reverse()
        } else {
            ord
        }
    }

    fn tuple_size(&self, attrs: &[Bson]) -> i64 {
        crate::heap::index_tuple_len(&self.types[..attrs.len()], attrs) as i64
    }

    fn dedup(&self) -> bool {
        self.dedup
    }

    fn unique(&self) -> bool {
        self.unique
    }
}

impl IndexShape {
    /// A row's index tuple key, or `None` when a partial index's predicate
    /// leaves the row out.
    /// Its key columns' stored fields.
    /// A UNIQUE (or primary-key) index.
    pub(crate) fn is_unique(&self) -> bool {
        self.unique
    }

    pub(crate) fn key_fields(&self) -> &[String] {
        &self.fields[..self.nkey.min(self.fields.len())]
    }

    /// Is it partial, or over an expression? (A scan of it the SSI model
    /// does not follow.)
    /// A partial index's predicate as a storage filter, when it is no more
    /// than that (no computed part, and every field typed).
    pub(crate) fn plain_predicate(&self) -> Option<&Document> {
        if self.computed.is_some() || !self.types.iter().all(Option::is_some) {
            return None;
        }
        self.predicate.as_ref()
    }

    /// The order a forward scan of this index reads its keys in, as sort
    /// keys over the stored fields; `None` for an index that is not plain.
    pub(crate) fn order_keys(
        &self,
        def: &secantus_pgcatalog::TableDef,
    ) -> Option<Vec<secantus_pgplan::OrderKey>> {
        if !self.is_plain() {
            return None;
        }
        Some(
            self.key_fields()
                .iter()
                .enumerate()
                .map(|(i, f)| secantus_pgplan::OrderKey {
                    field: f.clone(),
                    ascending: !self.desc.get(i).copied().unwrap_or(false),
                    nulls: if self.nulls_first.get(i).copied().unwrap_or(false) {
                        secantus_pgplan::Nulls::First
                    } else {
                        secantus_pgplan::Nulls::Last
                    },
                    jsonb: def
                        .columns
                        .iter()
                        .any(|c| &c.field() == f && c.pg_type == "jsonb"),
                    expr: None,
                })
                .collect(),
        )
    }

    pub(crate) fn is_plain(&self) -> bool {
        self.predicate.is_none()
            && self.computed.is_none()
            && self.types.iter().all(Option::is_some)
    }

    pub(crate) fn key_of(&self, doc: &Document) -> Option<Vec<Bson>> {
        if let Some(p) = &self.predicate {
            let empty = Document::new();
            if !secantus_core::query::matches(doc, p, &empty, None).unwrap_or(false) {
                return None;
            }
        }
        if let Some(e) = &self.computed {
            if !matches!(
                secantus_pgplan::apply_row_expr(e, doc),
                Ok(Bson::Boolean(true))
            ) {
                return None;
            }
        }
        Some(self.fields.iter().map(|f| field_value(doc, f)).collect())
    }
}

/// A stored field's value, `_id.a` read inside a composite key's `_id`
/// when the row is not flattened.
fn field_value(doc: &Document, field: &str) -> Bson {
    if let Some(v) = doc.get(field) {
        return v.clone();
    }
    if let Some(sub) = field.strip_prefix("_id.") {
        if let Some(Bson::Document(id)) = doc.get("_id") {
            return id.get(sub).cloned().unwrap_or(Bson::Null);
        }
    }
    Bson::Null
}

thread_local! {
    /// A heap rewrite in progress: its rows are stamped as new tuples, but
    /// the indexes are rebuilt after it, not added to.
    static REWRITING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A rewrite happened in this statement, so its indexes are rebuilt.
    static REWROTE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run a heap rewrite: no index tuples are added while it runs, and the
/// statement reindexes the table after (`ATRewriteTable`, `reindex_relation`).
pub(crate) fn rewriting<T>(f: impl FnOnce() -> T) -> T {
    let was = REWRITING.with(|r| r.replace(true));
    let out = f();
    REWRITING.with(|r| r.set(was));
    REWROTE.with(|r| r.set(true));
    out
}

/// Whether a rewrite happened since the last ask.
pub(crate) fn take_rewrote() -> bool {
    REWROTE.with(|r| r.replace(false))
}

type Models = HashMap<(usize, String, i64), BTree>;

fn models() -> &'static Mutex<Models> {
    static MODELS: OnceLock<Mutex<Models>> = OnceLock::new();
    MODELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// By store, database and -- for a block that changed the catalog, whose
/// shapes are its own -- the backend.
type ShapeMemo =
    HashMap<(usize, String, Option<i32>), (u64, Arc<HashMap<String, Vec<Arc<IndexShape>>>>)>;

fn shape_memo() -> &'static Mutex<ShapeMemo> {
    static MEMO: OnceLock<Mutex<ShapeMemo>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

impl PgHandler {
    pub(crate) fn store_id(&self) -> usize {
        Arc::as_ptr(&self.storage) as usize
    }

    /// Every B-tree of this database's tables, by table, memoised for the
    /// catalog version (the index list reads every table's catalog).
    fn btree_shapes_all(&self) -> Arc<HashMap<String, Vec<Arc<IndexShape>>>> {
        let version = crate::catalog_cache()
            .version
            .load(std::sync::atomic::Ordering::SeqCst);
        // Not while a statement changes the catalog, whose version moves
        // only at its end. A block that changed it keeps a memo of its own:
        // every catalog-changing statement moves the version, its own
        // uncommitted ones too.
        let mid_ddl = self
            .catalog_statements
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0;
        let own = self
            .block_wrote_catalog()
            .then(|| self.backend_pid.load(std::sync::atomic::Ordering::Relaxed));
        let key = (self.store_id(), self.db().to_string(), own);
        if !mid_ddl {
            if let Some((v, m)) = shape_memo()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
            {
                if *v == version {
                    return Arc::clone(m);
                }
            }
        }
        let mut by_table: HashMap<String, Vec<Arc<IndexShape>>> = HashMap::new();
        for ix in self.index_relations() {
            if let Some(shape) = self.btree_shape(&ix) {
                by_table
                    .entry(ix.table.name.clone())
                    .or_default()
                    .push(Arc::new(shape));
            }
        }
        let m = Arc::new(by_table);
        if !mid_ddl {
            shape_memo()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (version, Arc::clone(&m)));
        }
        m
    }

    pub(crate) fn btree_shapes(&self, table: &str) -> Vec<Arc<IndexShape>> {
        if table.starts_with("__") {
            return Vec::new();
        }
        self.btree_shapes_all()
            .get(table)
            .cloned()
            .unwrap_or_default()
    }

    fn btree_shape(&self, ix: &crate::IndexRelation) -> Option<IndexShape> {
        let def = &ix.table;
        let column_type = |field: &str| -> Option<crate::heap::Laid> {
            def.columns
                .iter()
                .find(|c| c.field() == field)
                .map(|c| crate::heap::Laid::of(&c.pg_type, c.typmod))
        };
        let (mut fields, mut desc, mut nulls_first) = (Vec::new(), Vec::new(), Vec::new());
        let mut include: Vec<String> = Vec::new();
        let mut predicate = None;
        let mut computed = None;
        let mut unique = ix.unique;
        if ix.primary {
            for c in secantus_pgplan::ordered_pk_columns(def) {
                fields.push(c.field());
                desc.push(false);
                nulls_first.push(false);
            }
            if let Ok(inc) = def.extra.get_array("pk_include") {
                for c in inc.iter().filter_map(Bson::as_str) {
                    if let Some(f) = def.field_of(c) {
                        include.push(f);
                    }
                }
            }
        } else if ix.exclusion {
            // An EXCLUDE constraint's index is a B-tree only when it says so.
            let u = def.unique_constraints.iter().find(|u| u.name == ix.name)?;
            if u.exclusion_method.as_deref().unwrap_or("gist") != "btree" {
                return None;
            }
            for c in &u.columns {
                fields.push(def.field_of(c)?);
                desc.push(false);
                nulls_first.push(false);
            }
            unique = false;
        } else {
            let stored = self
                .storage
                .list_indexes(self.db(), &def.name)
                .ok()?
                .into_iter()
                .find(|d| d.get_str("name") == Ok(ix.name.as_str()))?;
            let opt = |k: &str| {
                stored.get(k).cloned().or_else(|| {
                    stored
                        .get_document("options")
                        .ok()
                        .and_then(|o| o.get(k).cloned())
                })
            };
            if opt("sqlMethod")
                .as_ref()
                .and_then(Bson::as_str)
                .is_some_and(|m| m != "btree")
            {
                return None;
            }
            let key = stored.get_document("key").ok()?;
            let key_sql: Vec<String> = opt("sqlKeys")
                .and_then(|b| b.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            for (i, (f, dir)) in key.iter().enumerate() {
                let d = matches!(dir, Bson::Int32(n) if *n < 0)
                    || matches!(dir, Bson::Int64(n) if *n < 0);
                let sql = key_sql.get(i).map(|s| s.to_ascii_uppercase());
                let nf = match sql.as_deref() {
                    Some(s) if s.contains(" NULLS FIRST") => true,
                    Some(s) if s.contains(" NULLS LAST") => false,
                    _ => d,
                };
                fields.push(f.clone());
                desc.push(d);
                nulls_first.push(nf);
            }
            if let Some(Bson::Array(inc)) = opt("include") {
                for c in inc.iter().filter_map(Bson::as_str) {
                    include.push(def.field_of(c).unwrap_or_else(|| c.to_string()));
                }
            }
            // The SQL predicate, not the clauses that keep a unique
            // index's NULLs distinct in storage.
            if let (Some(Bson::String(sql)), None) =
                (opt("sqlPredicate"), opt("partialFilterExpression"))
            {
                computed = secantus_pgplan::plan_check_expression(&sql, def).ok();
            }
            if opt("sqlPredicate").is_some() {
                if let Some(Bson::Document(p)) = opt("partialFilterExpression") {
                    let pred = if opt("sqlNullsDistinct").is_some() {
                        p.get_array("$and")
                            .ok()
                            .and_then(|a| a.last())
                            .and_then(Bson::as_document)
                            .cloned()
                            .unwrap_or(p)
                    } else {
                        p
                    };
                    predicate = Some(pred);
                }
            }
        }
        let nkey = fields.len();
        let mut types: Vec<Option<crate::heap::Laid>> =
            fields.iter().map(|f| column_type(f)).collect();
        let dedup = include.is_empty()
            && types
                .iter()
                .all(|t| t.as_ref().is_some_and(|l| equal_image(&l.pg_type)));
        for f in include {
            types.push(column_type(&f));
            fields.push(f);
        }
        Some(IndexShape {
            oid: ix.oid,
            name: ix.name.clone(),
            fields,
            types,
            nkey,
            desc,
            nulls_first,
            unique,
            dedup,
            predicate,
            computed,
        })
    }

    /// The model of `shape`, built from `table`'s rows as CREATE INDEX
    /// would when there is none yet.
    pub(crate) fn with_btree<T>(
        &self,
        table: &str,
        shape: &IndexShape,
        f: impl FnOnce(&mut BTree) -> T,
    ) -> PgWireResult<T> {
        let key = (self.store_id(), self.db().to_string(), shape.oid);
        {
            let mut m = models().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = m.get_mut(&key) {
                return Ok(f(t));
            }
        }
        let model = match self.saved_btree(shape.oid) {
            Some(saved) => self.reconcile_btree(table, shape, saved)?,
            None => BTree::build(shape, self.btree_entries(table, shape)?),
        };
        let mut m = models().lock().unwrap_or_else(|e| e.into_inner());
        let t = m.entry(key).or_insert(model);
        Ok(f(t))
    }

    /// The model the last checkpoint saved of index `oid`.
    fn saved_btree(&self, oid: i64) -> Option<BTree> {
        let docs: Vec<Document> = self
            .storage
            .find_matching(self.db(), COLLECTION, &bson::doc! { "index": oid })
            .ok()?
            .iter()
            .filter_map(|b| crate::decode_doc(b).ok())
            .collect();
        if docs.is_empty() {
            return None;
        }
        BTree::from_docs(&docs)
    }

    /// A saved model made true to the rows: a row placed after the save
    /// (not heap-only) takes the tuple its insert would have; a tuple whose
    /// row went after the save stays, dead, as it would until VACUUM.
    fn reconcile_btree(
        &self,
        table: &str,
        shape: &IndexShape,
        mut t: BTree,
    ) -> PgWireResult<BTree> {
        let have = t.tids();
        let heap_only = self.heap_only_tids(table);
        let mut missing: Vec<(Vec<Bson>, i64)> = self
            .btree_entries(table, shape)?
            .into_iter()
            .filter(|(_, c)| !have.contains(c) && !heap_only.contains(c))
            .collect();
        missing.sort_by_key(|(_, c)| *c);
        for (key, ctid) in missing {
            t.insert(shape, key, ctid);
        }
        Ok(t)
    }

    /// The index tuples CREATE INDEX makes of `table`'s rows: `(key, ctid)`
    /// of each row the predicate admits.
    fn btree_entries(
        &self,
        table: &str,
        shape: &IndexShape,
    ) -> PgWireResult<Vec<(Vec<Bson>, i64)>> {
        Ok(self
            .table_docs(table)?
            .iter()
            .filter_map(|d| Some((shape.key_of(d)?, crate::heap::ctid_of(d)?)))
            .collect())
    }

    /// An index scan of `table` through `shape` for `filter` (an
    /// index-only one too, while the heap is not all-visible): it marks the
    /// entries it passes whose heap versions are dead to all
    /// (`kill_prior_tuple`, `_bt_killitems`), which a later insert into a
    /// full leaf then deletes before it splits (RP-196).
    pub(crate) fn btree_index_scanned(
        &self,
        table: &str,
        shape: &IndexShape,
        filter: &Document,
    ) -> PgWireResult<()> {
        if REWRITING.with(|r| r.get()) {
            return Ok(());
        }
        let dead = self.dead_index_tids(table)?;
        if dead.is_empty() {
            return Ok(());
        }
        let ranges = crate::serializable::index_scan_ranges(shape, filter);
        self.with_btree(table, shape, |t| {
            for (lo, hi) in &ranges {
                t.kill_items(
                    shape,
                    lo.as_ref().map(|(v, i)| (v, *i)),
                    hi.as_ref().map(|(v, i)| (v, *i)),
                    &|x| dead.contains(&x),
                );
            }
        })
    }

    /// [`Self::btree_index_scanned`] for a scan that returns at most one row
    /// -- a lookup by a unique key -- which learns of the versions behind
    /// that key's entries only: reading the whole heap's dead versions for
    /// every primary-key lookup is what kept them unmarked (RP-196).
    pub(crate) fn btree_key_scanned(
        &self,
        table: &str,
        shape: &IndexShape,
        filter: &Document,
        live: &[i64],
    ) -> PgWireResult<()> {
        if REWRITING.with(|r| r.get()) {
            return Ok(());
        }
        let ranges = crate::serializable::index_scan_ranges(shape, filter);
        let tids = self.with_btree(table, shape, |t| {
            ranges
                .iter()
                .flat_map(|(lo, hi)| {
                    t.tids_in(
                        shape,
                        lo.as_ref().map(|(v, i)| (v, *i)),
                        hi.as_ref().map(|(v, i)| (v, *i)),
                    )
                })
                .collect::<Vec<i64>>()
        })?;
        // The entry of the row the lookup returned is live; only the others
        // can name a dead version, and usually there are none.
        let others: Vec<i64> = tids.into_iter().filter(|t| !live.contains(t)).collect();
        if others.is_empty() {
            return Ok(());
        }
        let dead = self.dead_index_tids_among(table, &others)?;
        if dead.is_empty() {
            return Ok(());
        }
        self.with_btree(table, shape, |t| {
            for (lo, hi) in &ranges {
                t.kill_items(
                    shape,
                    lo.as_ref().map(|(v, i)| (v, *i)),
                    hi.as_ref().map(|(v, i)| (v, *i)),
                    &|x| dead.contains(&x),
                );
            }
        })
    }

    /// New tuple versions of `table` that are not heap-only: each B-tree
    /// takes an index tuple for each (`ExecInsertIndexTuples`).
    pub(crate) fn btree_insert_versions(
        &self,
        table: &str,
        versions: &[(&Document, Option<&Document>, i64)],
    ) -> PgWireResult<()> {
        if versions.is_empty() || REWRITING.with(|r| r.get()) {
            return Ok(());
        }
        let shapes = self.btree_shapes(table);
        if shapes.is_empty() {
            return Ok(());
        }
        for shape in shapes {
            // Each new tuple, and whether the UPDATE left this index's key as
            // it was (the executor's `indexUnchanged` hint).
            let keys: Vec<(Vec<Bson>, bool, i64)> = versions
                .iter()
                .filter_map(|(d, old, ctid)| {
                    let key = shape.key_of(d)?;
                    let unchanged = old.and_then(|o| shape.key_of(o)).is_some_and(|k| k == key);
                    Some((key, unchanged, *ctid))
                })
                .collect();
            if keys.is_empty() {
                continue;
            }
            // What the unique check and simple and bottom-up deletion may
            // remove: the heap is read, before the tree is changed and with no
            // B-tree held (RP-196), for the TIDs these inserts can ask about
            // only -- the whole heap read once per command made a run of
            // one-row inserts quadratic (RP-014).
            let pending: i64 = keys
                .iter()
                .map(|(key, _, _)| BTree::key_tuple_size(&*shape, key))
                .sum();
            // When no TID needs the heap's answer -- the common case, a new
            // key on a leaf with room -- the heap is not read (its answer
            // for no TIDs is that none is dead), so a single version goes in
            // under the same hold of the tree, from where the candidates'
            // search already landed (RP-014). Otherwise the tree is let go
            // while the heap is read, as below.
            let mut keys = Some(keys);
            let done = self.with_btree(table, &shape, |t| {
                let all = keys.as_ref().expect("the keys");
                let mut descents = Vec::with_capacity(all.len());
                let mut c: Vec<i64> = Vec::new();
                for (key, _, ctid) in all {
                    let (tids, descent) = t.candidate_tids_descending(&*shape, key, *ctid, pending);
                    c.extend(tids);
                    descents.push(descent);
                }
                c.sort_unstable();
                c.dedup();
                if !c.is_empty() || all.len() != 1 {
                    return Err(c);
                }
                let (key, unchanged, ctid) = keys.take().expect("the keys").pop().expect("one key");
                t.ssi_events.clear();
                t.insert_descended(
                    &*shape,
                    key,
                    ctid,
                    unchanged,
                    &|_| false,
                    descents.pop().flatten(),
                );
                Ok(std::mem::take(&mut t.ssi_events))
            })?;
            let events = match done {
                Ok(events) => events,
                Err(candidates) => {
                    let keys = keys.take().expect("the keys");
                    let dead = self.dead_index_tids_among(table, &candidates)?;
                    // A model built here is of the rows already stored, which
                    // these versions are not yet among.
                    self.with_btree(table, &shape, |t| {
                        t.ssi_events.clear();
                        for (key, unchanged, ctid) in keys {
                            t.insert_deleting(&*shape, key, ctid, unchanged, &|t| {
                                dead.contains(&t)
                            });
                        }
                        std::mem::take(&mut t.ssi_events)
                    })?
                }
            };
            // What SSI hears of the inserts (`_bt_doinsert`, `_bt_split`):
            // a split moves its page's locks to the new half too, whoever
            // splits, and each insert checks the page it landed on.
            for e in events {
                match e {
                    crate::btree_model::SsiEvent::Split(old, new) => {
                        self.ssi_page_split(shape.oid, old, new)
                    }
                    // A unique index checks after its uniqueness is
                    // decided, which a write conflict leaves undecided.
                    crate::btree_model::SsiEvent::Landed(leaf) if shape.unique() => {
                        self.ssi_defer_unique(shape.oid, leaf)
                    }
                    crate::btree_model::SsiEvent::Landed(leaf) => {
                        self.ssi_index_insert(shape.oid, leaf)?
                    }
                }
            }
        }
        Ok(())
    }

    /// `_bt_check_unique` before it raises a duplicate of `row`: an SSI
    /// conflict-in on the leaf each unique index would put its key on, so a
    /// SERIALIZABLE failure is not masked by the 23505. Every unique index
    /// is checked, where PostgreSQL stops at the violated one; the stored
    /// error names the index only by its storage name.
    pub(crate) fn ssi_duplicate(&self, table: &str, row: &Document) -> PgWireResult<()> {
        if !self.ssi_tracking() {
            return Ok(());
        }
        for shape in self.btree_shapes(table) {
            if !shape.unique() {
                continue;
            }
            let Some(key) = shape.key_of(row) else {
                continue;
            };
            if let Some(leaf) = self.with_btree(table, &shape, |t| t.unique_leaf(&*shape, &key))? {
                self.ssi_index_insert(shape.oid, leaf)?;
            }
        }
        Ok(())
    }

    /// The names of `table`'s B-trees.
    pub(crate) fn btree_names(&self, table: &str) -> Vec<String> {
        self.btree_shapes(table)
            .iter()
            .map(|s| s.name.clone())
            .collect()
    }

    /// Build `table`'s B-trees afresh from its rows -- those `only` names,
    /// or every one -- and record each as `index_update_stats` does: its
    /// pages and the heap tuples it indexed.
    fn btree_build_record(&self, table: &str, only: Option<&[String]>) -> PgWireResult<()> {
        for shape in self.btree_shapes(table) {
            if only.is_some_and(|names| !names.contains(&shape.name)) {
                continue;
            }
            let entries = self.btree_entries(table, &shape)?;
            let tuples = entries.len() as f64;
            let built = BTree::build(&*shape, entries);
            let pages = built.pages();
            models()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((self.store_id(), self.db().to_string(), shape.oid), built);
            self.record_index_size(table, &shape.name, pages, tuples)?;
        }
        Ok(())
    }

    /// The B-trees of `table` a statement made (not among `before`), each
    /// built over the rows and recorded: CREATE INDEX, CREATE TABLE's and
    /// ALTER TABLE's constraints.
    pub(crate) fn btree_built(&self, table: &str, before: &[String]) -> PgWireResult<()> {
        let new_hash: Vec<String> = self
            .hash_shapes(table)
            .into_iter()
            .map(|s| s.name)
            .filter(|n| !before.contains(n))
            .collect();
        if !new_hash.is_empty() {
            self.hash_built(table, Some(&new_hash));
        }
        let new_am: Vec<String> = self
            .am_shapes(table)
            .into_iter()
            .map(|s| s.name().to_string())
            .filter(|n| !before.contains(n))
            .collect();
        if !new_am.is_empty() {
            self.am_built(table, Some(&new_am));
        }
        let new: Vec<String> = self
            .btree_names(table)
            .into_iter()
            .filter(|n| !before.contains(n))
            .collect();
        if new.is_empty() {
            return Ok(());
        }
        self.btree_build_record(table, Some(&new))
    }

    /// The indexes of `table` with a page model: its B-trees, then its
    /// hash indexes (`btree_built`'s `before`).
    pub(crate) fn index_model_names(&self, table: &str) -> Vec<String> {
        let mut names = self.btree_names(table);
        names.extend(self.hash_shapes(table).into_iter().map(|s| s.name));
        names.extend(
            self.am_shapes(table)
                .into_iter()
                .map(|s| s.name().to_string()),
        );
        names
    }

    /// Every B-tree of `table` rebuilt and recorded: a rewrite of its heap
    /// (VACUUM FULL, CLUSTER, an ALTER that rewrites) reindexes it.
    pub(crate) fn btree_reindex(&self, table: &str) -> PgWireResult<()> {
        self.hash_built(table, None);
        self.am_built(table, None);
        self.btree_build_record(table, None)
    }

    /// REINDEX INDEX: that B-tree rebuilt and recorded.
    pub(crate) fn btree_reindex_one(&self, table: &str, index: &str) -> PgWireResult<()> {
        self.hash_built(table, Some(&[index.to_string()]));
        self.am_built(table, Some(&[index.to_string()]));
        self.btree_build_record(table, Some(&[index.to_string()]))
    }

    /// Every model of `table`'s B-trees built afresh from its rows,
    /// recording nothing: its heap was rewritten, so the saved pages and the
    /// ones in memory are both of the old one.
    pub(crate) fn btree_rebuild_models(&self, table: &str) -> PgWireResult<()> {
        self.hash_built(table, None);
        self.am_built(table, None);
        for shape in self.btree_shapes(table) {
            let built = BTree::build(&*shape, self.btree_entries(table, &shape)?);
            models()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((self.store_id(), self.db().to_string(), shape.oid), built);
        }
        Ok(())
    }

    /// TRUNCATE: each index starts afresh, and `pg_class` forgets its size
    /// (0 pages, -1 tuples, measured on 15.19).
    pub(crate) fn btree_truncated(&self, table: &str) -> PgWireResult<()> {
        self.btree_rebuild_models(table)?;
        self.hash_truncated(table);
        self.am_truncated(table);
        for name in self.btree_names(table) {
            self.record_index_size(table, &name, 0, -1.0)?;
        }
        Ok(())
    }

    /// ANALYZE: each index's pages as they stand, and the rows it covers
    /// (`totalindexrows`).
    pub(crate) fn btree_analyzed(&self, table: &str) -> PgWireResult<()> {
        let docs = self.table_docs(table)?;
        for shape in self.btree_shapes(table) {
            let rows = docs.iter().filter(|d| shape.key_of(d).is_some()).count();
            let pages = self.with_btree(table, &shape, |t| t.pages())?;
            self.record_index_size(table, &shape.name, pages, rows as f64)?;
        }
        Ok(())
    }

    /// Forget every model of `table`'s indexes, which a DROP ends. Their
    /// saved pages stay -- a DROP rolled back reads them back -- and an oid
    /// is never drawn twice, so nothing else finds them.
    pub(crate) fn btree_forget(&self, table: &str) {
        let oids: Vec<i64> = self.btree_shapes(table).iter().map(|s| s.oid).collect();
        let mut m = models().lock().unwrap_or_else(|e| e.into_inner());
        for oid in oids {
            m.remove(&(self.store_id(), self.db().to_string(), oid));
        }
    }

    /// VACUUM's index pass over `table`'s B-trees: the TIDs its heap pass
    /// found dead leave each. Per index, its pages and the TIDs left, which
    /// `pg_class` records for every index the pass visited.
    pub(crate) fn btree_bulk_delete(
        &self,
        table: &str,
        dead: &std::collections::HashSet<i64>,
    ) -> PgWireResult<Vec<(String, u32, u64)>> {
        let mut out = Vec::new();
        if dead.is_empty() {
            return Ok(out);
        }
        self.hash_bulk_delete(table);
        self.am_bulk_delete(table, dead);
        for shape in self.btree_shapes(table) {
            let (pages, tuples) = self.with_btree(table, &shape, |t| {
                t.bulk_delete(&*shape, &|c| dead.contains(&c));
                (t.pages(), t.tuples())
            })?;
            out.push((shape.name.clone(), pages, tuples));
        }
        Ok(out)
    }

    /// A VACUUM that ran no index pass: each B-tree's cleanup, which scans
    /// it -- and records it -- only when it has enough deleted pages to
    /// recycle.
    pub(crate) fn btree_cleanup(&self, table: &str) -> PgWireResult<Vec<(String, u32, u64)>> {
        let mut out = Vec::new();
        for shape in self.btree_shapes(table) {
            let key = (self.store_id(), self.db().to_string(), shape.oid);
            let mut m = models().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = m.get_mut(&key) {
                if t.cleanup() {
                    out.push((shape.name.clone(), t.pages(), t.tuples()));
                }
            }
        }
        Ok(out)
    }

    /// An index's pages and TIDs, `None` when it is not a B-tree this
    /// server models.
    pub(crate) fn btree_size(
        &self,
        table: &str,
        index: &str,
    ) -> PgWireResult<Option<(u32, u64, u32)>> {
        let Some(shape) = self
            .btree_shapes(table)
            .into_iter()
            .find(|s| s.name == index)
        else {
            return Ok(None);
        };
        self.with_btree(table, &shape, |t| Some((t.pages(), t.tuples(), t.height())))
    }
}

impl PgHandler {
    /// `pg_class`'s key for an index of `table`: its name, qualified by the
    /// table's schema outside `public`, as the index's row is.
    pub(crate) fn index_relstats_key(table: &str, index: &str) -> String {
        let schema = secantus_pgplan::schemas::split_key(table).0;
        if schema == "public" {
            index.to_string()
        } else {
            format!("{schema}.{index}")
        }
    }

    /// Record an index's `relpages` and `reltuples`, as `index_update_stats`
    /// (a build) and `vac_update_relstats` (VACUUM, ANALYZE) write them.
    pub(crate) fn record_index_size(
        &self,
        table: &str,
        index: &str,
        pages: u32,
        tuples: f64,
    ) -> PgWireResult<()> {
        let key = Self::index_relstats_key(table, index);
        let doc = bson::doc! {
            "_id": key.as_str(),
            "relpages": i64::from(pages),
            "reltuples": tuples,
            "index": true,
        };
        self.put_comment_doc(Self::RELSTATS_COLLECTION, &key, Some(doc))
    }
}

/// Each database's saved B-tree pages: a document per page and one per
/// tree, each tagged with its index's oid.
pub(crate) const COLLECTION: &str = "__sql_btree_pages__";

/// One index's checkpoint: its model's key, the documents to write, and
/// the ids they replace (`None`: every document of the index).
type Changes = ((usize, String, i64), Vec<Document>, Option<Vec<String>>);

/// How often the indexes' changed pages are written. They are many more
/// documents than the heaps', each write durable, so less often than the
/// heaps' second -- PostgreSQL's own `checkpoint_timeout` is five minutes.
/// A crash loses at most this much of the indexes' growth: the next touch
/// adds back every live row's tuple (`reconcile_btree`), not the splits
/// in between.
const INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Is it time for `store`'s indexes to be written? (Now, when `force`.)
fn due(store: usize, force: bool) -> bool {
    static LAST: OnceLock<Mutex<HashMap<usize, std::time::Instant>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if !force
        && last
            .get(&store)
            .is_some_and(|t| now.duration_since(*t) < INTERVAL)
    {
        return false;
    }
    last.insert(store, now);
    true
}

/// Write `storage`'s changed models, outside any user transaction, when
/// `due` says so (every one at a clean shutdown). A write that fails is
/// reported and the model left marked, for the next checkpoint.
pub(crate) fn flush_store(storage: &Arc<secantus_storage::Storage>, force: bool) {
    let store = Arc::as_ptr(storage) as usize;
    if !due(store, force) {
        return;
    }
    let changed: Vec<Changes> = {
        let mut all = models().lock().unwrap_or_else(|e| e.into_inner());
        all.iter_mut()
            .filter(|((s, _, _), t)| *s == store && t.dirty)
            .map(|(key, t)| {
                let (docs, ids) = t.take_changes(key.2);
                (key.clone(), docs, ids)
            })
            .collect()
    };
    for ((s, db, oid), docs, ids) in changed {
        // The checkpointer's write, which no snapshot reads: not a commit
        // a REPEATABLE READ transaction must see (`without_commit_count`).
        let written = secantus_storage::without_commit_count(|| {
            storage.outside_user_transaction(|| -> Result<(), String> {
                let encoded = docs
                    .iter()
                    .map(crate::encode_doc)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                let err = |e: secantus_storage::StorageError| e.to_string();
                if !storage.collection_exists(&db, COLLECTION).map_err(err)? {
                    storage.create_collection(&db, COLLECTION).map_err(err)?;
                }
                // The changed pages' documents, or every one of the index's.
                let which = match &ids {
                    None => bson::doc! { "index": oid },
                    Some(ids) => bson::doc! { "_id": { "$in": ids.clone() } },
                };
                storage
                    .delete_matching(&db, COLLECTION, &which, 0, &Document::new(), None)
                    .map_err(err)?;
                storage
                    .insert(&db, COLLECTION, encoded, true)
                    .map_err(err)?;
                Ok(())
            })
        });
        if let Err(e) = written {
            eprintln!("secantusd-pg: could not write the pages of index {oid} in {db}: {e}");
            if let Some(t) = models()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&(s, db, oid))
            {
                t.rewrite_all();
            }
        }
    }
}

/// Forget a dropped database's models.
pub(crate) fn forget_database(storage: &Arc<secantus_storage::Storage>, db: &str) {
    let store = Arc::as_ptr(storage) as usize;
    models()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(s, d, _), _| !(*s == store && d == db));
}
