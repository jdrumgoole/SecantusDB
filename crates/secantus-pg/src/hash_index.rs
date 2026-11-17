//! Hash indexes' buckets, for SERIALIZABLE (RP-254): PostgreSQL 15.19's
//! hash access method keeps a predicate lock on the primary page of each
//! bucket a scan reads (`_hash_first`), checks that page when it inserts
//! (`_hash_doinsert`), and moves its locks to the new bucket when one
//! splits (`_hash_expandtable`). Which bucket a key is in is all that has to
//! be exact, so this keeps each index's metapage: its buckets and masks
//! (`_hash_init_metabuffer`), its tuple count and fill factor, and the
//! split that each insert past the fill factor makes. A bucket's page here
//! is its number plus one, which names it as its block number does.
//!
//! An index is kept from its CREATE INDEX on. One first met later (after a
//! restart) is taken as built over the rows there now.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bson::{Bson, Document};

/// One hash index of a table, when its key's hash is reproduced here.
#[derive(Debug, Clone)]
pub(crate) struct HashShape {
    pub(crate) oid: i64,
    pub(crate) name: String,
    /// Its one key column's stored field, and that column's type.
    pub(crate) field: String,
    pub(crate) pg_type: String,
    /// Its `fillfactor` (75 unless `WITH` gave one), which its builds read.
    pub(crate) fillfactor: u32,
    /// Whether its key's hash is reproduced, so SERIALIZABLE can name its
    /// buckets. One that is not still keeps its metapage, whose tuple
    /// count splits it and so sizes it.
    pub(crate) modelled: bool,
    /// A partial index's WHERE, planned: only a row it holds true for is
    /// indexed. An expression index's is already in its stored field, which
    /// a row lacks when the WHERE does not hold.
    pub(crate) predicate: Option<std::sync::Arc<secantus_pgplan::ColumnExpr>>,
}

impl HashShape {
    /// `_hash_datum2hashkey`: the key's standard hash, or `None` for a NULL
    /// (a hash index holds none).
    pub(crate) fn hashkey(&self, doc: &Document) -> Option<u32> {
        match doc.get(&self.field) {
            None | Some(Bson::Null) => None,
            Some(v) => secantus_pgplan::hashpart::standard_hash(v, &self.pg_type),
        }
    }

    /// [`Self::hashkey`] of a value.
    pub(crate) fn hash_value(&self, v: &Bson) -> Option<u32> {
        secantus_pgplan::hashpart::standard_hash(v, &self.pg_type)
    }
}

const SPLITPOINT_PHASE_BITS: u32 = 2;
const SPLITPOINT_PHASE_MASK: u32 = (1 << SPLITPOINT_PHASE_BITS) - 1;
const SPLITPOINT_GROUPS_WITH_ONE_PHASE: u32 = 10;

/// `pg_ceil_log2_32`.
fn ceil_log2(n: u32) -> u32 {
    if n <= 1 {
        0
    } else {
        32 - (n - 1).leading_zeros()
    }
}

/// `_hash_spareindex`.
fn spare_index(num_bucket: u32) -> u32 {
    let group = ceil_log2(num_bucket);
    if group < SPLITPOINT_GROUPS_WITH_ONE_PHASE {
        return group;
    }
    SPLITPOINT_GROUPS_WITH_ONE_PHASE
        + ((group - SPLITPOINT_GROUPS_WITH_ONE_PHASE) << SPLITPOINT_PHASE_BITS)
        + (((num_bucket - 1) >> (group - (SPLITPOINT_PHASE_BITS + 1))) & SPLITPOINT_PHASE_MASK)
}

/// `_hash_get_totalbuckets`.
fn total_buckets(phase: u32) -> u32 {
    if phase < SPLITPOINT_GROUPS_WITH_ONE_PHASE {
        return 1 << phase;
    }
    let group = SPLITPOINT_GROUPS_WITH_ONE_PHASE
        + ((phase - SPLITPOINT_GROUPS_WITH_ONE_PHASE) >> SPLITPOINT_PHASE_BITS);
    let within = ((phase - SPLITPOINT_GROUPS_WITH_ONE_PHASE) & SPLITPOINT_PHASE_MASK) + 1;
    (1 << (group - 1)) + (((1 << (group - 1)) >> SPLITPOINT_PHASE_BITS) * within)
}

/// The index tuples a hash page holds: each is its key's 4-byte hash code
/// after an 8-byte header, 16 bytes aligned, with a 4-byte line pointer;
/// `_hash_doinsert` moves on while `PageGetFreeSpace` is under 16, which
/// leaves 407 in the 8,152 bytes between the page header and the special
/// space.
const TUPLES_PER_PAGE: usize = 407;

/// One bucket: its primary page and its overflow chain, each page's
/// tuples as their hash codes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bucket {
    pages: Vec<Vec<u32>>,
}

impl Bucket {
    fn empty() -> Bucket {
        Bucket {
            pages: vec![Vec::new()],
        }
    }
}

/// The metapage's bucket fields, and, for an index whose keys' hashes are
/// reproduced, every bucket's pages.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Meta {
    pub(crate) maxbucket: u32,
    pub(crate) highmask: u32,
    pub(crate) lowmask: u32,
    pub(crate) ntuples: f64,
    pub(crate) ffactor: u32,
    /// Each bucket's pages; empty when the hashes are not reproduced, which
    /// leaves the overflow pages out.
    buckets: Vec<Bucket>,
    /// The overflow pages allocated (`hashm_spares`, the bitmap page aside),
    /// and those freed for reuse (`_hash_freeovflpage`): the file keeps
    /// both.
    ovfl: u32,
    free_ovfl: u32,
}

impl Meta {
    /// `_hash_init` for `num_tuples` estimated rows at `fillfactor`: a hash
    /// key's tuple is 20 bytes with its line pointer, so at the default 75
    /// 307 of them make a bucket's target (`HashGetTargetPageUsage`).
    pub(crate) fn init(num_tuples: f64, fillfactor: u32) -> Meta {
        let ffactor = ((8192 * fillfactor / 100) / (8 + 8 + 4)).max(10);
        let d = num_tuples / f64::from(ffactor);
        let num_buckets = if d <= 2.0 {
            2
        } else if d >= f64::from(0x4000_0000u32) {
            0x4000_0000
        } else {
            total_buckets(spare_index(d as u32))
        };
        let highmask = (num_buckets + 1).next_power_of_two() - 1;
        Meta {
            maxbucket: num_buckets - 1,
            highmask,
            lowmask: highmask >> 1,
            ntuples: 0.0,
            ffactor,
            buckets: Vec::new(),
            ovfl: 0,
            free_ovfl: 0,
        }
    }

    /// [`Self::init`], keeping each bucket's pages.
    pub(crate) fn init_keyed(num_tuples: f64, fillfactor: u32) -> Meta {
        let mut m = Meta::init(num_tuples, fillfactor);
        m.buckets = (0..=m.maxbucket).map(|_| Bucket::empty()).collect();
        m
    }

    /// `_hash_getovflpage`: a freed overflow page if the bitmap has one,
    /// else a new one at the end of the file.
    fn alloc_ovfl(&mut self) {
        if self.free_ovfl > 0 {
            self.free_ovfl -= 1;
        } else {
            self.ovfl += 1;
        }
    }

    /// A tuple into the first page of bucket `b`'s chain with room for it,
    /// or a new overflow page at its end (`_hash_addovflpage`).
    fn add(&mut self, b: usize, hashkey: u32) {
        if let Some(page) = self.buckets[b]
            .pages
            .iter_mut()
            .find(|p| p.len() < TUPLES_PER_PAGE)
        {
            page.push(hashkey);
            return;
        }
        self.alloc_ovfl();
        self.buckets[b].pages.push(vec![hashkey]);
    }

    /// `_hash_squeezebucket`: the chain's tuples packed toward its start,
    /// each overflow page left empty freed.
    fn squeeze(&mut self, b: usize) {
        let items: Vec<u32> = self.buckets[b].pages.concat();
        let keep = items.len().div_ceil(TUPLES_PER_PAGE).max(1);
        let freed = self.buckets[b].pages.len().saturating_sub(keep);
        self.free_ovfl += freed as u32;
        let mut pages: Vec<Vec<u32>> = items.chunks(TUPLES_PER_PAGE).map(<[u32]>::to_vec).collect();
        if pages.is_empty() {
            pages.push(Vec::new());
        }
        self.buckets[b].pages = pages;
    }

    /// `_hash_splitbucket`: each tuple of `old` that now hashes to `new` is
    /// copied there, overflow pages added as `new` fills; then, as nothing
    /// else holds a pin on `old`, `hashbucketcleanup` removes the copied
    /// tuples from it and squeezes it, freeing its emptied overflow pages.
    fn split(&mut self, old: u32, new: u32) {
        let (old, new) = (old as usize, new as usize);
        while self.buckets.len() <= new {
            self.buckets.push(Bucket::empty());
        }
        let moving: Vec<u32> = self.buckets[old]
            .pages
            .iter()
            .flatten()
            .copied()
            .filter(|h| self.bucket(*h) as usize == new)
            .collect();
        if moving.is_empty() {
            return;
        }
        for h in moving {
            self.add(new, h);
        }
        let unmoved = |h: &u32| {
            let b = h & self.highmask;
            let b = if b > self.maxbucket {
                b & self.lowmask
            } else {
                b
            };
            b as usize != new
        };
        let pages: Vec<Vec<u32>> = self.buckets[old]
            .pages
            .iter()
            .map(|p| p.iter().copied().filter(unmoved).collect())
            .collect();
        self.buckets[old].pages = pages;
        self.squeeze(old);
    }

    /// One insert of hash code `hashkey` (`_hash_doinsert`): the tuple
    /// placed in its bucket, then [`Self::insert`]'s count and split, whose
    /// tuples move. The split's old and new bucket.
    pub(crate) fn insert_key(&mut self, hashkey: u32) -> Option<(u32, u32)> {
        if self.buckets.is_empty() {
            return self.insert();
        }
        let b = self.bucket(hashkey) as usize;
        self.add(b, hashkey);
        let split = self.insert();
        if let Some((old, new)) = split {
            self.split(old, new);
        }
        split
    }

    /// `hashbulkdelete`: each bucket keeps the tuples of `live` (hash codes,
    /// one per live tuple) and loses the rest; a bucket that lost one is
    /// squeezed. The count becomes what is left.
    pub(crate) fn bulk_delete(&mut self, live: &[u32]) {
        if self.buckets.is_empty() {
            self.ntuples = live.len() as f64;
            return;
        }
        let mut left: HashMap<u32, usize> = HashMap::new();
        for h in live {
            *left.entry(*h).or_default() += 1;
        }
        for b in 0..self.buckets.len() {
            let mut removed = false;
            for page in &mut self.buckets[b].pages {
                page.retain(|h| {
                    let keep = match left.get_mut(h) {
                        Some(n) if *n > 0 => {
                            *n -= 1;
                            true
                        }
                        _ => false,
                    };
                    removed |= !keep;
                    keep
                });
            }
            if removed {
                self.squeeze(b);
            }
        }
        self.ntuples = live.len() as f64;
    }

    /// `_hash_hashkey2bucket`.
    pub(crate) fn bucket(&self, hashkey: u32) -> u32 {
        let b = hashkey & self.highmask;
        if b > self.maxbucket {
            b & self.lowmask
        } else {
            b
        }
    }

    /// One insert (`_hash_doinsert`): the count goes up, and past the fill
    /// factor a bucket splits (`_hash_expandtable`). The split's old and
    /// new bucket.
    pub(crate) fn insert(&mut self) -> Option<(u32, u32)> {
        self.ntuples += 1.0;
        if self.ntuples <= f64::from(self.ffactor) * f64::from(self.maxbucket + 1)
            || self.maxbucket >= 0x7FFF_FFFE
        {
            return None;
        }
        let new = self.maxbucket + 1;
        let old = new & self.lowmask;
        self.maxbucket = new;
        if new > self.highmask {
            self.lowmask = self.highmask;
            self.highmask = new | self.lowmask;
        }
        Some((old, new))
    }

    /// The pages it occupies (`RelationGetNumberOfBlocks`): the metapage,
    /// the first bitmap page and the buckets of every splitpoint phase
    /// begun, which `_hash_alloc_buckets` allocates whole when its first
    /// bucket splits off, and every overflow page allocated.
    pub(crate) fn pages(&self) -> f64 {
        f64::from(total_buckets(spare_index(self.maxbucket + 1))) + 2.0 + f64::from(self.ovfl)
    }
}

/// A bucket's page, as SSI names it.
pub(crate) fn bucket_page(bucket: u32) -> u32 {
    bucket + 1
}

type Key = (usize, String, i64);

fn metas() -> &'static Mutex<HashMap<Key, Meta>> {
    static M: OnceLock<Mutex<HashMap<Key, Meta>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The metapage of index `oid` in `(store, db)`, made by `build` when
/// there is none yet.
pub(crate) fn with_meta<T>(
    store: usize,
    db: &str,
    oid: i64,
    build: impl FnOnce() -> Meta,
    f: impl FnOnce(&mut Meta) -> T,
) -> T {
    let mut all = metas().lock().unwrap_or_else(|e| e.into_inner());
    let m = all
        .entry((store, db.to_string(), oid))
        .or_insert_with(build);
    f(m)
}

/// `hashbuild` over `rows` rows, estimated at `estimate` when it starts.
pub(crate) fn built(estimate: f64, rows: usize, fillfactor: u32) -> Meta {
    let mut m = Meta::init(estimate, fillfactor);
    for _ in 0..rows {
        m.insert();
    }
    m
}

/// [`built`] over the rows' hash codes, in heap order, each placed in its
/// bucket's pages.
pub(crate) fn built_keyed(estimate: f64, keys: &[u32], fillfactor: u32) -> Meta {
    let mut m = Meta::init_keyed(estimate, fillfactor);
    for h in keys {
        m.insert_key(*h);
    }
    m
}

impl crate::PgHandler {
    fn hash_store(&self) -> usize {
        std::sync::Arc::as_ptr(&self.storage) as usize
    }

    /// `table`'s hash indexes whose key this models: one column of a type
    /// whose hash is reproduced, compared without a collation of its own,
    /// and no predicate. An index left out is never chosen for a
    /// SERIALIZABLE scan, which then locks the whole table, so no conflict
    /// it would have caught is missed.
    pub(crate) fn hash_shapes(&self, table: &str) -> Vec<HashShape> {
        let mut shapes = self.hash_index_shapes(table);
        shapes.retain(|s| s.modelled);
        shapes
    }

    /// Every hash index of `table`, modelled or not: each one's build
    /// records its size.
    pub(crate) fn hash_index_shapes(&self, table: &str) -> Vec<HashShape> {
        type Memo =
            HashMap<(usize, String), (u64, std::sync::Arc<HashMap<String, Vec<HashShape>>>)>;
        static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
        if table.starts_with("__") {
            return Vec::new();
        }
        let version = crate::catalog_cache()
            .version
            .load(std::sync::atomic::Ordering::SeqCst);
        let mid_ddl = self
            .catalog_statements
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
            || self.block_wrote_catalog();
        let key = (self.hash_store(), self.db().to_string());
        let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
        if !mid_ddl {
            if let Some((v, m)) = memo.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
                if *v == version {
                    return m.get(table).cloned().unwrap_or_default();
                }
            }
        }
        let mut by_table: HashMap<String, Vec<HashShape>> = HashMap::new();
        let mut stored_of: HashMap<String, Vec<Document>> = HashMap::new();
        // While a statement changes the catalog, only this table's (RP-921):
        // nothing is memoised then, and every table's list re-reads the
        // whole catalog.
        let relations = if self.in_catalog_statement() {
            self.table_index_relations_of(table)
        } else {
            self.index_relations()
        };
        for ix in relations {
            if ix.primary || ix.exclusion || ix.keys.len() != 1 {
                continue;
            }
            let def = &ix.table;
            let stored = stored_of.entry(def.name.clone()).or_insert_with(|| {
                self.storage
                    .list_indexes(self.db(), &def.name)
                    .unwrap_or_default()
            });
            let Some(d) = stored
                .iter()
                .find(|d| d.get_str("name") == Ok(ix.name.as_str()))
            else {
                continue;
            };
            let opt = |k: &str| {
                d.get(k).cloned().or_else(|| {
                    d.get_document("options")
                        .ok()
                        .and_then(|o| o.get(k).cloned())
                })
            };
            if opt("sqlMethod").as_ref().and_then(Bson::as_str) != Some("hash") {
                continue;
            }
            let Some(field) = d
                .get_document("key")
                .ok()
                .and_then(|k| k.keys().next().cloned())
            else {
                continue;
            };
            // An expression's key is its stored field (`expr_index`).
            let col = (ix.keys[0] > 0)
                .then(|| def.columns.iter().find(|c| c.field() == field))
                .flatten();
            let predicate = match (col, opt("sqlPredicate")) {
                (Some(_), Some(Bson::String(sql))) => {
                    match secantus_pgplan::plan_check_expression(&sql, def) {
                        Ok(p) => Some(std::sync::Arc::new(p)),
                        Err(_) => continue,
                    }
                }
                _ => None,
            };
            let modelled = col.is_some_and(|col| {
                let textual = matches!(
                    col.pg_type.as_str(),
                    "text"
                        | "varchar"
                        | "name"
                        | "bpchar"
                        | "character varying"
                        | "character"
                        | "char"
                );
                secantus_pgplan::hashpart::supported_type(&col.pg_type)
                    && !(textual && col.extra.contains_key("collation"))
                    && opt("sqlPredicate").is_none()
            });
            let fillfactor = match opt("sqlReloptions") {
                Some(Bson::Array(a)) => a
                    .iter()
                    .filter_map(Bson::as_str)
                    .find_map(|o| o.strip_prefix("fillfactor=")?.trim().parse::<u32>().ok()),
                _ => None,
            }
            .unwrap_or(75);
            by_table
                .entry(def.name.clone())
                .or_default()
                .push(HashShape {
                    oid: ix.oid,
                    name: ix.name.clone(),
                    field,
                    pg_type: col.map(|c| c.pg_type.clone()).unwrap_or_default(),
                    fillfactor,
                    modelled,
                    predicate,
                });
        }
        let m = std::sync::Arc::new(by_table);
        if !mid_ddl {
            memo.lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (version, std::sync::Arc::clone(&m)));
        }
        m.get(table).cloned().unwrap_or_default()
    }

    /// The rows of `table` `shape` indexes: a key that is not NULL (a hash
    /// index holds none), in a row its predicate holds true for.
    fn hash_indexed(&self, table: &str, shape: &HashShape) -> usize {
        let keyed = bson::doc! { shape.field.clone(): { "$ne": Bson::Null } };
        let Some(p) = &shape.predicate else {
            return self
                .storage
                .count_matching(self.db(), table, &keyed, None)
                .unwrap_or(0);
        };
        let empty = Document::new();
        self.table_docs(table)
            .unwrap_or_default()
            .iter()
            .filter(|d| secantus_core::query::matches(d, &keyed, &empty, None).unwrap_or(false))
            .filter(|d| secantus_pgplan::apply_row_expr(p, d).ok() == Some(Bson::Boolean(true)))
            .count()
    }

    /// `hashbuild` of `shape` over `table` as it is now: estimated as the
    /// planner estimates the table (`estimate_rel_size`), then each row it
    /// indexes inserted. Its index tuples, which `index_update_stats`
    /// records as `reltuples`.
    fn hash_build(&self, table: &str, shape: &HashShape) -> (Meta, usize) {
        let count = self
            .storage
            .count_matching(self.db(), table, &Document::new(), None)
            .unwrap_or(0);
        let estimate = self
            .planner_table(table, count)
            .map_or(count as f64, |pt| pt.tuples);
        if shape.modelled {
            let keys = self.hash_keys(table, shape);
            let n = keys.len();
            return (built_keyed(estimate, &keys, shape.fillfactor), n);
        }
        let indexed = self.hash_indexed(table, shape);
        (built(estimate, indexed, shape.fillfactor), indexed)
    }

    /// The hash codes of the rows of `table` a modelled `shape` indexes, in
    /// heap order.
    fn hash_keys(&self, table: &str, shape: &HashShape) -> Vec<u32> {
        self.table_docs(table)
            .unwrap_or_default()
            .iter()
            .filter_map(|d| shape.hashkey(d))
            .collect()
    }

    /// `index`'s metapage, built over the rows now if it has none.
    pub(crate) fn with_hash_meta<T>(
        &self,
        table: &str,
        shape: &HashShape,
        f: impl FnOnce(&mut Meta) -> T,
    ) -> T {
        let oid = shape.oid;
        let store = self.hash_store();
        let db = self.db().to_string();
        let exists = metas()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&(store, db.clone(), oid));
        // Built outside the lock: it reads the table.
        let fresh = (!exists).then(|| self.hash_build(table, shape).0);
        with_meta(
            store,
            &db,
            oid,
            || fresh.unwrap_or_else(|| Self::hash_empty(shape)),
            f,
        )
    }

    /// CREATE INDEX, REINDEX or a rewrite: `table`'s hash indexes built
    /// afresh over its rows (those `only` names, or every one). Each one's
    /// name, pages and index tuples, for `index_update_stats`.
    pub(crate) fn hash_built(
        &self,
        table: &str,
        only: Option<&[String]>,
    ) -> Vec<(String, u32, f64)> {
        let mut sizes = Vec::new();
        for shape in self.hash_index_shapes(table) {
            if only.is_some_and(|names| !names.contains(&shape.name)) {
                continue;
            }
            let (meta, indexed) = self.hash_build(table, &shape);
            sizes.push((shape.name.clone(), meta.pages() as u32, indexed as f64));
            metas()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((self.hash_store(), self.db().to_string(), shape.oid), meta);
        }
        sizes
    }

    /// TRUNCATE: each hash index starts empty, as `hashbuildempty` leaves it.
    pub(crate) fn hash_truncated(&self, table: &str) {
        for shape in self.hash_index_shapes(table) {
            with_meta(
                self.hash_store(),
                self.db(),
                shape.oid,
                || Self::hash_empty(&shape),
                |m| *m = Self::hash_empty(&shape),
            );
        }
    }

    /// `hashbuildempty`'s metapage.
    fn hash_empty(shape: &HashShape) -> Meta {
        if shape.modelled {
            Meta::init_keyed(0.0, shape.fillfactor)
        } else {
            Meta::init(0.0, shape.fillfactor)
        }
    }

    /// VACUUM's index pass (`hashbulkdelete`): each hash index's tuple count
    /// becomes the tuples left, its rows with a key.
    pub(crate) fn hash_bulk_delete(&self, table: &str) {
        for shape in self.hash_index_shapes(table) {
            if shape.modelled {
                let live = self.hash_keys(table, &shape);
                self.with_hash_meta(table, &shape, |m| m.bulk_delete(&live));
            } else {
                let left = self.hash_indexed(table, &shape);
                self.with_hash_meta(table, &shape, |m| m.ntuples = left as f64);
            }
        }
    }

    /// The pages of `table`'s hash index `index` (`RelationGetNumberOfBlocks`),
    /// `None` when it is not a hash index.
    pub(crate) fn hash_pages(&self, table: &str, index: &str) -> Option<u32> {
        let shape = self
            .hash_index_shapes(table)
            .into_iter()
            .find(|s| s.name == index)?;
        Some(self.with_hash_meta(table, &shape, |m| m.pages() as u32))
    }

    /// Each hash index of `table`: its name, pages and index tuples, as
    /// ANALYZE and `hashvacuumcleanup` report them.
    pub(crate) fn hash_sizes(&self, table: &str) -> Vec<(String, u32, f64)> {
        self.hash_index_shapes(table)
            .into_iter()
            .map(|shape| {
                let pages = self.with_hash_meta(table, &shape, |m| m.pages() as u32);
                (
                    shape.name.clone(),
                    pages,
                    self.hash_indexed(table, &shape) as f64,
                )
            })
            .collect()
    }

    /// New tuple versions of `table` that are not heap-only, into each hash
    /// index (`_hash_doinsert`): SSI checks the key's bucket page, the
    /// tuple goes in, and a bucket may split, its locks copied to the new
    /// one.
    pub(crate) fn hash_insert_versions(
        &self,
        table: &str,
        versions: &[(&Document, Option<&Document>, i64)],
    ) -> pgwire::error::PgWireResult<()> {
        if versions.is_empty() {
            return Ok(());
        }
        for shape in self.hash_index_shapes(table) {
            for (doc, _, _) in versions {
                if !shape.modelled {
                    // Its key's hash is not reproduced, so no bucket: only
                    // the count, which is what splits it.
                    let keyed = !matches!(doc.get(&shape.field), None | Some(Bson::Null))
                        && shape.predicate.as_ref().is_none_or(|p| {
                            secantus_pgplan::apply_row_expr(p, doc).ok()
                                == Some(Bson::Boolean(true))
                        });
                    if keyed {
                        self.with_hash_meta(table, &shape, Meta::insert);
                    }
                    continue;
                }
                let Some(hashkey) = shape.hashkey(doc) else {
                    continue;
                };
                let bucket = self.with_hash_meta(table, &shape, |m| m.bucket(hashkey));
                self.ssi_index_insert(shape.oid, bucket_page(bucket))?;
                if let Some((old, new)) =
                    self.with_hash_meta(table, &shape, |m| m.insert_key(hashkey))
                {
                    self.ssi_page_split(shape.oid, bucket_page(old), bucket_page(new));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_follows_hash_init_metabuffer() {
        let m = Meta::init(0.0, 75);
        assert_eq!(
            (m.maxbucket, m.highmask, m.lowmask, m.ffactor),
            (1, 3, 1, 307)
        );
        // 10,000 rows: 32.6 buckets' worth, which `_hash_spareindex` takes
        // as 32 (its argument is a uint32).
        let m = Meta::init(10_000.0, 75);
        assert_eq!((m.maxbucket, m.highmask, m.lowmask), (31, 63, 31));
        // 100,000: 325 buckets, a power of two below 1,024 (512); 1,000,000:
        // 3,257, in the splitpoint phase of 3,584.
        assert_eq!(Meta::init(100_000.0, 75).maxbucket, 511);
        assert_eq!(Meta::init(1_000_000.0, 75).maxbucket, 3583);
    }

    #[test]
    fn a_full_bucket_splits() {
        let mut m = Meta::init(0.0, 75);
        for _ in 0..614 {
            assert_eq!(m.insert(), None);
        }
        assert_eq!(m.insert(), Some((0, 2)));
        assert_eq!((m.maxbucket, m.highmask, m.lowmask), (2, 3, 1));
        // Bucket 2 now takes the keys whose low bits are 2.
        assert_eq!(m.bucket(6), 2);
        assert_eq!(m.bucket(7), 3 & 1);
    }

    #[test]
    fn a_split_allocates_its_whole_splitpoint() {
        // 4 buckets (6 pages, as CREATE INDEX on PostgreSQL 15.19 leaves an
        // empty `(a int, b text)` table's), then a fifth: the phase of
        // buckets 4-7 is allocated, 10 pages.
        let mut m = Meta::init(1000.0, 75);
        assert_eq!((m.maxbucket, m.pages()), (3, 6.0));
        while m.insert().is_none() {}
        assert_eq!((m.maxbucket, m.pages()), (4, 10.0));
        // 3,000 rows into 8 buckets end at 10 buckets, in the phase of 16:
        // 18 pages, as 15.19 measured (2026-11-14).
        let mut m = Meta::init(2000.0, 75);
        assert_eq!(m.pages(), 10.0);
        for _ in 0..3000 {
            m.insert();
        }
        assert_eq!((m.maxbucket, m.pages()), (9, 18.0));
    }

    #[test]
    fn a_full_bucket_chains_overflow_pages() {
        // 1,000 tuples of one key into 8 buckets: 407 a page, so the
        // bucket's primary page and 2 overflow pages; 12 pages, as
        // PostgreSQL 15.19 measured (RP-956).
        let m = built_keyed(2550.0, &[7; 1000], 75);
        assert_eq!((m.maxbucket, m.ovfl, m.pages()), (7, 2, 12.0));
    }

    #[test]
    fn a_split_frees_the_old_buckets_overflow_pages_for_reuse() {
        // Every key in bucket 1 of 2, until a split moves the ones whose
        // next bit is set to bucket 3: the copies take new overflow pages,
        // then the old bucket's emptied ones are freed, and reused.
        let mut m = Meta::init_keyed(0.0, 75);
        let keys: Vec<u32> = (0..614).map(|i| if i % 2 == 0 { 1 } else { 3 }).collect();
        for h in &keys {
            assert_eq!(m.insert_key(*h), None);
        }
        assert_eq!(m.ovfl, 1);
        assert_eq!(m.insert_key(1), Some((0, 2)));
        // The split of bucket 0 moved nothing; bucket 1's turn is next.
        while m.insert_key(1) != Some((1, 3)) {}
        assert_eq!(m.buckets[3].pages.concat().len(), 307);
        assert_eq!(m.free_ovfl, 1);
        let before = m.ovfl;
        for _ in 0..200 {
            m.insert_key(3);
        }
        assert_eq!((m.ovfl, m.free_ovfl), (before, 0));
    }

    #[test]
    fn bulk_delete_squeezes_and_frees() {
        let mut m = built_keyed(2550.0, &[7; 1000], 75);
        m.bulk_delete(&[7; 10]);
        assert_eq!((m.ovfl, m.free_ovfl, m.ntuples), (2, 2, 10.0));
        assert_eq!(m.pages(), 12.0);
    }
}
