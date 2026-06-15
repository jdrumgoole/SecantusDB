//! `_secantus_storage` — PyO3 bindings exposing SecantusDB's Rust storage layer
//! (`secantus-storage`, Phase 4) to Python.
//!
//! Like the `_secantus_core` operator engines, documents cross the boundary as
//! **BSON bytes** (`secantus.storage`'s "documents are opaque BSON blobs"
//! design), so the seam stays aligned and avoids per-field marshalling. `_id`
//! values are passed wrapped as `bson.encode({"v": <id>})` (the same one-key
//! envelope the sort-key seam uses). Filters / updates / sort specs / options /
//! index key-specs and the results of `explain` / `update` likewise cross as
//! BSON-encoded `Document`s.
//!
//! This extension links the vendored WiredTiger C library (via
//! `secantus-storage` → `secantus-wt`), so unlike `_secantus_core` it does not
//! build in maturin's plain manylinux container — shipping it across the wheel
//! matrix is Phase 4's go/no-go gate.
//!
//! Engine-fallback contract: when the Rust query engine hits a construct it
//! can't evaluate (collation, some `$expr`/regex paths, …) the storage layer
//! returns `StorageError::QueryUnsupported`; that surfaces here as the
//! `EngineFallback` Python exception, which the (future) `secantus.engine`
//! storage adapter catches to re-run the operation on the pure-Python engine.

// PyO3's #[pymethods] expansion inserts an identity `.into()` on PyResult return
// types that clippy flags as a useless conversion; it's a macro artifact, not
// our code (same suppression as secantus-core-py).
#![allow(clippy::useless_conversion)]

use bson::{Bson, Document};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use secantus_storage::{ExplainPlan, Hint, Storage, StorageError};

// Raised when the Rust storage/query engine can't evaluate a construct and the
// caller should re-run the operation on the pure-Python engine.
create_exception!(_secantus_storage, EngineFallback, PyException);

/// Map a storage error to a Python exception. Duplicate-key conditions become
/// `KeyError` (the command layer maps the real E11000 codes); the
/// "defer to Python" signal becomes `EngineFallback`; everything else is a
/// `ValueError`.
fn to_pyerr(e: StorageError) -> PyErr {
    let msg = e.to_string();
    match e {
        StorageError::DuplicateId | StorageError::DuplicateKey(_) => PyKeyError::new_err(msg),
        StorageError::QueryUnsupported | StorageError::UnsupportedValue => {
            EngineFallback::new_err(msg)
        }
        _ => PyValueError::new_err(msg),
    }
}

/// Decode a BSON-encoded `Document` argument.
fn decode_doc(b: &[u8]) -> PyResult<Document> {
    Document::from_reader(&mut std::io::Cursor::new(b))
        .map_err(|e| PyValueError::new_err(format!("invalid BSON document: {e}")))
}

/// Encode a `Document` to bytes (for return values).
fn encode_doc(doc: &Document) -> PyResult<Vec<u8>> {
    let mut buf = Vec::new();
    doc.to_writer(&mut buf)
        .map_err(|e| PyValueError::new_err(format!("BSON encode error: {e}")))?;
    Ok(buf)
}

/// Decode `bson.encode({"v": <id>})` and return the wrapped `_id` value.
fn unwrap_id(id_bytes: &[u8]) -> PyResult<Bson> {
    let doc: Document = decode_doc(id_bytes)?;
    doc.get("v")
        .cloned()
        .ok_or_else(|| PyValueError::new_err("id wrapper document missing key 'v'"))
}

fn bytes(py: Python<'_>, b: Vec<u8>) -> Py<PyBytes> {
    PyBytes::new_bound(py, &b).unbind()
}

fn doc_list(py: Python<'_>, docs: Vec<Vec<u8>>) -> Vec<Py<PyBytes>> {
    docs.into_iter().map(|b| bytes(py, b)).collect()
}

/// Encode an `ExplainPlan` as a `{kind, index_name?, key_pattern?, direction?}`
/// document (the command layer shapes it into MongoDB's `winningPlan`).
fn explain_to_doc(plan: ExplainPlan) -> Document {
    let mut d = Document::new();
    match plan {
        ExplainPlan::CollScan => {
            d.insert("kind", "COLLSCAN");
        }
        ExplainPlan::IxScan {
            index_name,
            key_pattern,
            direction,
        } => {
            d.insert("kind", "IXSCAN");
            d.insert("index_name", index_name);
            d.insert("key_pattern", Bson::Document(key_pattern));
            d.insert("direction", direction);
        }
    }
    d
}

/// A WiredTiger-backed SecantusDB storage handle (the Rust `Storage`).
#[pyclass(name = "RustStorage")]
struct RustStorage {
    inner: Storage,
}

#[pymethods]
impl RustStorage {
    /// Open (creating if needed) an on-disk database at `home`.
    #[new]
    fn new(home: &str) -> PyResult<Self> {
        Storage::open(home)
            .map(|inner| RustStorage { inner })
            .map_err(to_pyerr)
    }

    /// Open with an explicit WiredTiger config string.
    #[staticmethod]
    fn open_with_config(home: &str, config: &str) -> PyResult<Self> {
        Storage::open_with_config(home, config)
            .map(|inner| RustStorage { inner })
            .map_err(to_pyerr)
    }

    // --- engine configuration ---

    fn set_enable_oplog(&mut self, on: bool) {
        self.inner.set_enable_oplog(on);
    }

    fn set_oplog_retention_seconds(&mut self, secs: i64) {
        self.inner.set_oplog_retention_seconds(secs);
    }

    fn set_oplog_max_entries(&mut self, n: usize) {
        self.inner.set_oplog_max_entries(n);
    }

    // --- CRUD core ---

    /// Insert one BSON-encoded document; returns its `id_key` bytes. Assigns an
    /// `ObjectId` `_id` when absent; a duplicate `_id` raises `KeyError`.
    fn insert_one(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        doc_bytes: &[u8],
    ) -> PyResult<Py<PyBytes>> {
        self.inner
            .insert_one(db, coll, doc_bytes)
            .map(|k| bytes(py, k))
            .map_err(to_pyerr)
    }

    /// Batch insert (`docs` is a list of BSON byte strings). Returns
    /// `(inserted, errors)` where `errors` is a list of BSON-encoded write-error
    /// docs (`{index, code, errmsg, keyPattern?, keyValue?}`). `ordered` stops
    /// at the first error.
    #[pyo3(signature = (db, coll, docs, ordered=true))]
    fn insert(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        docs: Vec<Vec<u8>>,
        ordered: bool,
    ) -> PyResult<(usize, Vec<Py<PyBytes>>)> {
        let (inserted, errors) = self
            .inner
            .insert(db, coll, docs, ordered)
            .map_err(to_pyerr)?;
        let err_bytes = errors
            .iter()
            .map(|e| encode_doc(e))
            .collect::<PyResult<Vec<_>>>()?;
        Ok((
            inserted,
            err_bytes.into_iter().map(|b| bytes(py, b)).collect(),
        ))
    }

    /// Fetch a document by `_id` (wrapped as `{"v": id}`). Returns its BSON
    /// bytes or `None`.
    fn find_by_id(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        id_bytes: &[u8],
    ) -> PyResult<Option<Py<PyBytes>>> {
        let id = unwrap_id(id_bytes)?;
        self.inner
            .find_by_id(db, coll, &id)
            .map(|opt| opt.map(|b| bytes(py, b)))
            .map_err(to_pyerr)
    }

    /// All documents of a collection in natural (`_id`) order, as BSON bytes.
    fn scan_collection(&self, py: Python<'_>, db: &str, coll: &str) -> PyResult<Vec<Py<PyBytes>>> {
        self.inner
            .scan_collection(db, coll)
            .map(|docs| doc_list(py, docs))
            .map_err(to_pyerr)
    }

    /// Replace the document at `_id` with `new_doc_bytes` (whose `_id` is forced
    /// to the given id). Returns `False` if no such document.
    fn replace_by_id(
        &self,
        db: &str,
        coll: &str,
        id_bytes: &[u8],
        new_doc_bytes: &[u8],
    ) -> PyResult<bool> {
        let id = unwrap_id(id_bytes)?;
        self.inner
            .replace_by_id(db, coll, &id, new_doc_bytes)
            .map_err(to_pyerr)
    }

    /// Delete the document with the given `_id`. Returns `False` if absent.
    fn delete_by_id(&self, db: &str, coll: &str, id_bytes: &[u8]) -> PyResult<bool> {
        let id = unwrap_id(id_bytes)?;
        self.inner.delete_by_id(db, coll, &id).map_err(to_pyerr)
    }

    // --- query / write / count ---

    /// Documents matching `filter` (BSON), as a list of BSON-encoded docs.
    fn find_matching(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        filter_bytes: &[u8],
    ) -> PyResult<Vec<Py<PyBytes>>> {
        let filter = decode_doc(filter_bytes)?;
        self.inner
            .find_matching(db, coll, &filter)
            .map(|docs| doc_list(py, docs))
            .map_err(to_pyerr)
    }

    /// `find_matching` with an optional `sort` (BSON) and `hint` (an index name
    /// or a BSON key-spec). Results come back ordered / index-routed per
    /// `find_matching_with`.
    #[pyo3(signature = (db, coll, filter_bytes, sort_bytes=None, hint_name=None, hint_key_spec=None))]
    fn find_matching_with(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        filter_bytes: &[u8],
        sort_bytes: Option<&[u8]>,
        hint_name: Option<String>,
        hint_key_spec: Option<&[u8]>,
    ) -> PyResult<Vec<Py<PyBytes>>> {
        let filter = decode_doc(filter_bytes)?;
        let sort = match sort_bytes {
            Some(b) => Some(decode_doc(b)?),
            None => None,
        };
        let hint = if let Some(name) = hint_name {
            Some(Hint::Name(name))
        } else if let Some(ks) = hint_key_spec {
            Some(Hint::KeySpec(decode_doc(ks)?))
        } else {
            None
        };
        self.inner
            .find_matching_with(
                db,
                coll,
                &filter,
                sort.as_ref(),
                hint.as_ref(),
                None,
                &Document::new(),
            )
            .map(|docs| doc_list(py, docs))
            .map_err(to_pyerr)
    }

    /// The plan `find_matching` would use for `filter`, as a
    /// `{kind, index_name?, key_pattern?, direction?}` BSON document.
    fn explain_plan(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        filter_bytes: &[u8],
    ) -> PyResult<Py<PyBytes>> {
        let filter = decode_doc(filter_bytes)?;
        let plan = self
            .inner
            .explain_plan(db, coll, &filter)
            .map_err(to_pyerr)?;
        Ok(bytes(py, encode_doc(&explain_to_doc(plan))?))
    }

    /// Count documents matching `filter` (the whole collection when empty).
    fn count_matching(&self, db: &str, coll: &str, filter_bytes: &[u8]) -> PyResult<usize> {
        let filter = decode_doc(filter_bytes)?;
        self.inner
            .count_matching(db, coll, &filter, None)
            .map_err(to_pyerr)
    }

    /// Apply `update` (BSON) to documents matching `filter`. Returns a
    /// `{matched, modified, upserted_id?}` BSON document (`upserted_id` present
    /// only when an `upsert` inserted a doc).
    fn update_matching(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        filter_bytes: &[u8],
        update_bytes: &[u8],
        multi: bool,
        upsert: bool,
    ) -> PyResult<Py<PyBytes>> {
        let filter = decode_doc(filter_bytes)?;
        let update = decode_doc(update_bytes)?;
        let out = self
            .inner
            .update_matching(
                db,
                coll,
                &filter,
                &update,
                multi,
                upsert,
                &[],
                &Document::new(),
                None,
                None,
            )
            .map_err(to_pyerr)?;
        let mut d = Document::new();
        d.insert("matched", out.matched as i64);
        d.insert("modified", out.modified as i64);
        if let Some(id) = out.upserted_id {
            d.insert("upserted_id", id);
        }
        Ok(bytes(py, encode_doc(&d)?))
    }

    /// Delete documents matching `filter`, returning how many were removed.
    /// `limit > 0` caps the count (1 for `deleteOne`; 0 = all matches).
    fn delete_matching(
        &self,
        db: &str,
        coll: &str,
        filter_bytes: &[u8],
        limit: usize,
    ) -> PyResult<usize> {
        let filter = decode_doc(filter_bytes)?;
        self.inner
            .delete_matching(db, coll, &filter, limit, &Document::new(), None)
            .map_err(to_pyerr)
    }

    // --- indexes ---

    /// Create index `name` over `key_spec` (BSON) with `options` (BSON).
    fn create_index(
        &self,
        db: &str,
        coll: &str,
        name: &str,
        key_spec_bytes: &[u8],
        options_bytes: &[u8],
    ) -> PyResult<bool> {
        let key_spec = decode_doc(key_spec_bytes)?;
        let options = decode_doc(options_bytes)?;
        self.inner
            .create_index(db, coll, name, &key_spec, &options)
            .map_err(to_pyerr)
    }

    /// All indexes on `(db, coll)` in `listIndexes` shape, as BSON docs.
    fn list_indexes(&self, py: Python<'_>, db: &str, coll: &str) -> PyResult<Vec<Py<PyBytes>>> {
        self.inner
            .list_indexes(db, coll)
            .and_then(|docs| {
                docs.iter()
                    .map(|d| {
                        let mut buf = Vec::new();
                        d.to_writer(&mut buf)
                            .map_err(|e| StorageError::Bson(e.to_string()))?;
                        Ok(buf)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .map(|docs| doc_list(py, docs))
            .map_err(to_pyerr)
    }

    fn drop_index(&self, db: &str, coll: &str, name: &str) -> PyResult<bool> {
        self.inner.drop_index(db, coll, name).map_err(to_pyerr)
    }

    fn drop_all_indexes(&self, db: &str, coll: &str) -> PyResult<usize> {
        self.inner.drop_all_indexes(db, coll).map_err(to_pyerr)
    }

    /// Prune TTL-expired docs (`now` is epoch milliseconds). Returns the count
    /// deleted.
    fn prune_ttl(&self, db: &str, coll: &str, now_millis: i64) -> PyResult<usize> {
        self.inner
            .prune_ttl(db, coll, bson::DateTime::from_millis(now_millis))
            .map_err(to_pyerr)
    }

    /// Run TTL pruning across every collection (`now` epoch milliseconds);
    /// returns the total pruned.
    fn prune_ttl_all_collections(&self, now_millis: i64) -> PyResult<usize> {
        self.inner
            .prune_ttl_all_collections(bson::DateTime::from_millis(now_millis))
            .map_err(to_pyerr)
    }

    // --- collection / database lifecycle ---

    fn collection_exists(&self, db: &str, coll: &str) -> PyResult<bool> {
        self.inner.collection_exists(db, coll).map_err(to_pyerr)
    }

    fn create_collection(&self, db: &str, coll: &str) -> PyResult<bool> {
        self.inner.create_collection(db, coll).map_err(to_pyerr)
    }

    fn drop_collection(&self, db: &str, coll: &str) -> PyResult<bool> {
        self.inner.drop_collection(db, coll).map_err(to_pyerr)
    }

    fn drop_database(&self, db: &str) -> PyResult<()> {
        self.inner.drop_database(db).map_err(to_pyerr)
    }

    /// Rename `src_db.src_coll` to `dst_db.dst_coll`. Returns `(ok, error_msg)`
    /// — `error_msg` is `None` on success.
    fn rename_collection(
        &self,
        src_db: &str,
        src_coll: &str,
        dst_db: &str,
        dst_coll: &str,
        drop_target: bool,
    ) -> PyResult<(bool, Option<String>)> {
        self.inner
            .rename_collection(src_db, src_coll, dst_db, dst_coll, drop_target)
            .map_err(to_pyerr)
    }

    fn list_collections(&self, db: &str) -> PyResult<Vec<String>> {
        self.inner.list_collections(db).map_err(to_pyerr)
    }

    fn list_databases(&self) -> PyResult<Vec<String>> {
        self.inner.list_databases().map_err(to_pyerr)
    }

    // --- collection options / stats ---

    /// The collection's options blob as a BSON document (`{}` when absent).
    fn get_collection_options(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
    ) -> PyResult<Py<PyBytes>> {
        let opts = self
            .inner
            .get_collection_options(db, coll)
            .map_err(to_pyerr)?;
        Ok(bytes(py, encode_doc(&opts)?))
    }

    /// Merge `opts` (BSON) into the collection's options blob.
    fn set_collection_options(&self, db: &str, coll: &str, opts_bytes: &[u8]) -> PyResult<()> {
        let opts = decode_doc(opts_bytes)?;
        self.inner
            .set_collection_options(db, coll, &opts)
            .map_err(to_pyerr)
    }

    fn collection_is_capped(&self, db: &str, coll: &str) -> PyResult<bool> {
        self.inner.collection_is_capped(db, coll).map_err(to_pyerr)
    }

    fn collection_data_size(&self, db: &str, coll: &str) -> PyResult<i64> {
        self.inner.collection_data_size(db, coll).map_err(to_pyerr)
    }

    /// Per-index byte sizes as a `{name: bytes}` BSON document.
    fn index_sizes(&self, py: Python<'_>, db: &str, coll: &str) -> PyResult<Py<PyBytes>> {
        let sizes = self.inner.index_sizes(db, coll).map_err(to_pyerr)?;
        Ok(bytes(py, encode_doc(&sizes)?))
    }

    /// Natural-order `(id_key, doc)` byte pairs whose `id_key` is strictly
    /// greater than `after` (the whole collection when `after` is `None`).
    #[pyo3(signature = (db, coll, after=None))]
    fn scan_docs_after_id_key(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        after: Option<&[u8]>,
    ) -> PyResult<Vec<(Py<PyBytes>, Py<PyBytes>)>> {
        self.inner
            .scan_docs_after_id_key(db, coll, after)
            .map(|rows| {
                rows.into_iter()
                    .map(|(k, v)| (bytes(py, k), bytes(py, v)))
                    .collect()
            })
            .map_err(to_pyerr)
    }

    // --- oplog / cluster time / change-stream support ---

    /// The collection's 16-byte UUID (minted + persisted on first use).
    fn collection_uuid(&self, py: Python<'_>, db: &str, coll: &str) -> PyResult<Py<PyBytes>> {
        self.inner
            .collection_uuid(db, coll)
            .map(|u| bytes(py, u))
            .map_err(to_pyerr)
    }

    /// The next monotonic cluster `Timestamp`, as `(seconds, increment)`.
    fn current_cluster_time(&self) -> PyResult<(u32, u32)> {
        self.inner
            .current_cluster_time()
            .map(|t| (t.time, t.increment))
            .map_err(to_pyerr)
    }

    /// Oplog entries from `start_seq` (inclusive), up to `limit`, as
    /// `(seq, entry_bytes)` pairs.
    fn read_oplog(
        &self,
        py: Python<'_>,
        start_seq: i64,
        limit: usize,
    ) -> PyResult<Vec<(i64, Py<PyBytes>)>> {
        self.inner
            .read_oplog(start_seq, limit)
            .map(|rows| {
                rows.into_iter()
                    .map(|(seq, blob)| (seq, bytes(py, blob)))
                    .collect()
            })
            .map_err(to_pyerr)
    }

    fn oplog_floor_seq(&self) -> PyResult<i64> {
        self.inner.oplog_floor_seq().map_err(to_pyerr)
    }

    fn oplog_tail_seq(&self) -> i64 {
        self.inner.oplog_tail_seq()
    }

    /// Block (releasing the GIL) until the oplog tail seq exceeds `after_seq`, a
    /// `notify_oplog_waiters` fires, or `timeout_ms` elapses; returns the current
    /// tail seq. The change-stream tailable `getMore` waits here instead of on a
    /// Python `threading.Condition`.
    fn wait_for_oplog(&self, py: Python<'_>, after_seq: i64, timeout_ms: u64) -> i64 {
        py.allow_threads(|| self.inner.wait_for_oplog(after_seq, timeout_ms))
    }

    /// Wake every `wait_for_oplog` waiter without advancing the oplog (e.g. on
    /// `killCursors`).
    fn notify_oplog_waiters(&self) {
        self.inner.notify_oplog_waiters();
    }

    /// The pre-image doc stored for oplog `seq` (BSON), or `None`.
    fn read_preimage(&self, py: Python<'_>, seq: i64) -> PyResult<Option<Py<PyBytes>>> {
        self.inner
            .read_preimage(seq)
            .map(|opt| opt.map(|b| bytes(py, b)))
            .map_err(to_pyerr)
    }

    /// Prune oplog entries past retention / the entry cap (`now` epoch seconds,
    /// `None` = use the wall clock). Returns the count removed.
    #[pyo3(signature = (now=None))]
    fn prune_oplog(&self, now: Option<i64>) -> PyResult<usize> {
        self.inner.prune_oplog(now).map_err(to_pyerr)
    }

    /// Emit a no-op heartbeat oplog entry; returns its seq.
    fn emit_noop_heartbeat(&self) -> PyResult<i64> {
        self.inner.emit_noop_heartbeat().map_err(to_pyerr)
    }

    /// The first oplog seq whose timestamp is `>= (secs, inc)` (for
    /// `startAtOperationTime`).
    fn find_seq_for_ts(&self, secs: u32, inc: u32) -> PyResult<i64> {
        self.inner
            .find_seq_for_ts(bson::Timestamp {
                time: secs,
                increment: inc,
            })
            .map_err(to_pyerr)
    }

    // --- users / roles / profiling ---

    /// Persist a user record (BSON blob). `False` if it existed and `replace`
    /// is false.
    fn add_user(&self, db: &str, username: &str, record: &[u8], replace: bool) -> PyResult<bool> {
        self.inner
            .add_user(db, username, record, replace)
            .map_err(to_pyerr)
    }

    /// The user record (BSON) or `None`.
    fn get_user(&self, py: Python<'_>, db: &str, username: &str) -> PyResult<Option<Py<PyBytes>>> {
        self.inner
            .get_user(db, username)
            .map(|opt| opt.map(|b| bytes(py, b)))
            .map_err(to_pyerr)
    }

    fn drop_user(&self, db: &str, username: &str) -> PyResult<bool> {
        self.inner.drop_user(db, username).map_err(to_pyerr)
    }

    /// Paginated user records (BSON); `db=None` spans every database.
    #[pyo3(signature = (db=None, skip=0, limit=100))]
    fn list_users(
        &self,
        py: Python<'_>,
        db: Option<&str>,
        skip: usize,
        limit: usize,
    ) -> PyResult<Vec<Py<PyBytes>>> {
        self.inner
            .list_users(db, skip, limit)
            .map(|docs| doc_list(py, docs))
            .map_err(to_pyerr)
    }

    fn add_role(&self, db: &str, name: &str, record: &[u8], replace: bool) -> PyResult<bool> {
        self.inner
            .add_role(db, name, record, replace)
            .map_err(to_pyerr)
    }

    fn get_role(&self, py: Python<'_>, db: &str, name: &str) -> PyResult<Option<Py<PyBytes>>> {
        self.inner
            .get_role(db, name)
            .map(|opt| opt.map(|b| bytes(py, b)))
            .map_err(to_pyerr)
    }

    fn drop_role(&self, db: &str, name: &str) -> PyResult<bool> {
        self.inner.drop_role(db, name).map_err(to_pyerr)
    }

    #[pyo3(signature = (db=None, skip=0, limit=100))]
    fn list_roles(
        &self,
        py: Python<'_>,
        db: Option<&str>,
        skip: usize,
        limit: usize,
    ) -> PyResult<Vec<Py<PyBytes>>> {
        self.inner
            .list_roles(db, skip, limit)
            .map(|docs| doc_list(py, docs))
            .map_err(to_pyerr)
    }

    /// The active `{level, slowms, sampleRate}` profile settings for `db` (BSON).
    fn get_profile(&self, py: Python<'_>, db: &str) -> PyResult<Py<PyBytes>> {
        let doc = self.inner.get_profile(db).map_err(to_pyerr)?;
        Ok(bytes(py, encode_doc(&doc)?))
    }

    /// Persist profile settings (`level` 0/1/2, `slowms` >= 0, `sample_rate`
    /// in `[0,1]`).
    fn set_profile(&self, db: &str, level: i32, slowms: i32, sample_rate: f64) -> PyResult<()> {
        self.inner
            .set_profile(db, level, slowms, sample_rate)
            .map_err(to_pyerr)
    }

    /// Ensure `<db>.system.profile` exists as a capped collection.
    #[pyo3(signature = (db, size_bytes=10485760))]
    fn ensure_profile_collection(&self, db: &str, size_bytes: i64) -> PyResult<()> {
        self.inner
            .ensure_profile_collection(db, size_bytes)
            .map_err(to_pyerr)
    }
}

#[pymodule]
fn _secantus_storage(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add(
        "__doc__",
        "Rust storage layer for SecantusDB (Phase 4): WiredTiger-backed \
         collections / documents / indexes / oplog, behind the BSON byte seam.",
    )?;
    m.add_class::<RustStorage>()?;
    m.add("EngineFallback", m.py().get_type_bound::<EngineFallback>())?;
    Ok(())
}
