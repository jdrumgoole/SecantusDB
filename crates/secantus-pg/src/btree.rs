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
    fn key_of(&self, doc: &Document) -> Option<Vec<Bson>> {
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
    fn store_id(&self) -> usize {
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
    fn with_btree<T>(
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
        // What bottom-up deletion may remove: read from the heap now, before
        // any B-tree is held (RP-196), once per command -- within one, only
        // deleters that ended before it are dead to every snapshot, and the
        // command's row-by-row writes would otherwise read the heap per row.
        let dead = self.command_dead_tids(table)?;
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
            // A model built here is of the rows already stored, which these
            // versions are not yet among.
            self.with_btree(table, &shape, |t| {
                for (key, unchanged, ctid) in keys {
                    t.insert_deleting(&*shape, key, ctid, unchanged, &|t| dead.contains(&t));
                }
            })?;
        }
        Ok(())
    }

    /// [`Self::dead_index_tids`] of `table`, kept for the running command.
    fn command_dead_tids(
        &self,
        table: &str,
    ) -> PgWireResult<std::sync::Arc<std::collections::HashSet<i64>>> {
        type Key = (usize, String, String, u64, i64);
        thread_local! {
            static MEMO: std::cell::RefCell<Option<(Key, std::sync::Arc<std::collections::HashSet<i64>>)>> =
                const { std::cell::RefCell::new(None) };
        }
        let (xid, cid) = self.command_writer();
        let key: Key = (
            self.store_id(),
            self.db().to_string(),
            table.to_string(),
            xid,
            cid,
        );
        if let Some(hit) = MEMO.with(|m| {
            m.borrow()
                .as_ref()
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        }) {
            return Ok(hit);
        }
        let dead = std::sync::Arc::new(self.dead_index_tids(table)?);
        MEMO.with(|m| *m.borrow_mut() = Some((key, dead.clone())));
        Ok(dead)
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

    /// Every B-tree of `table` rebuilt and recorded: a rewrite of its heap
    /// (VACUUM FULL, CLUSTER, an ALTER that rewrites) reindexes it.
    pub(crate) fn btree_reindex(&self, table: &str) -> PgWireResult<()> {
        self.btree_build_record(table, None)
    }

    /// REINDEX INDEX: that B-tree rebuilt and recorded.
    pub(crate) fn btree_reindex_one(&self, table: &str, index: &str) -> PgWireResult<()> {
        self.btree_build_record(table, Some(&[index.to_string()]))
    }

    /// Every model of `table`'s B-trees built afresh from its rows,
    /// recording nothing: its heap was rewritten, so the saved pages and the
    /// ones in memory are both of the old one.
    pub(crate) fn btree_rebuild_models(&self, table: &str) -> PgWireResult<()> {
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
        let written = storage.outside_user_transaction(|| -> Result<(), String> {
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
