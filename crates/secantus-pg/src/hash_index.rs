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

/// The metapage's bucket fields.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Meta {
    pub(crate) maxbucket: u32,
    pub(crate) highmask: u32,
    pub(crate) lowmask: u32,
    pub(crate) ntuples: f64,
    pub(crate) ffactor: u32,
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
        }
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

    /// The pages it occupies: the metapage, the buckets and the first
    /// bitmap page.
    pub(crate) fn pages(&self) -> f64 {
        f64::from(self.maxbucket + 1) + 2.0
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
        for ix in self.index_relations() {
            if ix.primary || ix.exclusion || ix.keys.len() != 1 || ix.keys[0] <= 0 {
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
            if opt("sqlMethod").as_ref().and_then(Bson::as_str) != Some("hash")
                || opt("sqlPredicate").is_some()
            {
                continue;
            }
            let Some(field) = d
                .get_document("key")
                .ok()
                .and_then(|k| k.keys().next().cloned())
            else {
                continue;
            };
            let Some(col) = def.columns.iter().find(|c| c.field() == field) else {
                continue;
            };
            let textual = matches!(
                col.pg_type.as_str(),
                "text" | "varchar" | "name" | "bpchar" | "character varying" | "character" | "char"
            );
            if !secantus_pgplan::hashpart::supported_type(&col.pg_type)
                || (textual && col.extra.contains_key("collation"))
            {
                continue;
            }
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
                    field: col.field(),
                    pg_type: col.pg_type.clone(),
                    fillfactor,
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

    /// `hashbuild` of `shape` over `table` as it is now: estimated as the
    /// planner estimates the table, then each row inserted.
    fn hash_build(&self, table: &str, shape: &HashShape) -> Meta {
        let count = self
            .storage
            .count_matching(self.db(), table, &Document::new(), None)
            .unwrap_or(0);
        let estimate = self
            .planner_table(table, count)
            .map_or(count as f64, |pt| pt.tuples);
        built(estimate, count, shape.fillfactor)
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
        let fresh = (!exists).then(|| self.hash_build(table, shape));
        with_meta(
            store,
            &db,
            oid,
            || fresh.unwrap_or_else(|| Meta::init(0.0, shape.fillfactor)),
            f,
        )
    }

    /// CREATE INDEX, REINDEX or a rewrite: `table`'s hash indexes built
    /// afresh over its rows (those `only` names, or every one).
    pub(crate) fn hash_built(&self, table: &str, only: Option<&[String]>) {
        for shape in self.hash_shapes(table) {
            if only.is_some_and(|names| !names.contains(&shape.name)) {
                continue;
            }
            let meta = self.hash_build(table, &shape);
            metas()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((self.hash_store(), self.db().to_string(), shape.oid), meta);
        }
    }

    /// TRUNCATE: each hash index starts empty, as `hashbuildempty` leaves it.
    pub(crate) fn hash_truncated(&self, table: &str) {
        for shape in self.hash_shapes(table) {
            with_meta(
                self.hash_store(),
                self.db(),
                shape.oid,
                || Meta::init(0.0, shape.fillfactor),
                |m| *m = Meta::init(0.0, shape.fillfactor),
            );
        }
    }

    /// VACUUM's index pass (`hashbulkdelete`): each hash index's tuple count
    /// becomes the tuples left, its rows with a key.
    pub(crate) fn hash_bulk_delete(&self, table: &str) {
        for shape in self.hash_shapes(table) {
            let left = self
                .storage
                .count_matching(
                    self.db(),
                    table,
                    &bson::doc! { shape.field.clone(): { "$ne": Bson::Null } },
                    None,
                )
                .unwrap_or(0);
            self.with_hash_meta(table, &shape, |m| m.ntuples = left as f64);
        }
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
        for shape in self.hash_shapes(table) {
            for (doc, _, _) in versions {
                let Some(hashkey) = shape.hashkey(doc) else {
                    continue;
                };
                let bucket = self.with_hash_meta(table, &shape, |m| m.bucket(hashkey));
                self.ssi_index_insert(shape.oid, bucket_page(bucket))?;
                if let Some((old, new)) = self.with_hash_meta(table, &shape, Meta::insert) {
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
}
