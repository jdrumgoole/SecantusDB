//! The storage seam for command handlers.
//!
//! `secantus-commands` stays **WiredTiger-free** (so it builds in the WT-less
//! `rust` CI job and links into the standalone server without pulling the C
//! library) by talking to storage through this trait rather than depending on
//! the `secantus-storage` crate directly. The real `secantus-storage::Storage`
//! satisfies it via a thin adapter in the server crate (R4), which also
//! translates `secantus_storage::StorageError` into the [`StorageError`] here.
//!
//! Signatures mirror `secantus-storage`'s CRUD methods so the adapter is a
//! near-identity. The trait grows one method per command family as they land;
//! this slice covers the write/count surface (`insert` / `update_matching` /
//! `delete_matching` / `count_matching`).

use bson::{Bson, Document};

/// A query hint at the command seam: the raw `hint` value (a string index name,
/// a key-spec document, or a sentinel like `"$natural"` / `"_id_"`). The adapter
/// converts it to `secantus_storage::Hint`; keeping it as `Bson` lets
/// `secantus-commands` stay decoupled from the storage crate's `Hint` type.
pub type RawHint<'a> = &'a Bson;

/// The outcome of an `update` operation (mirrors
/// `secantus_storage::UpdateOutcome`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdateOutcome {
    /// Documents that matched the filter (whether or not they changed).
    pub matched: usize,
    /// Documents actually rewritten (matched *and* the update changed them).
    pub modified: usize,
    /// The `_id` an `upsert` inserted when nothing matched, else `None`.
    pub upserted_id: Option<Bson>,
}

/// An E11000 duplicate-key conflict's payload (boxed in [`StorageError`] to keep
/// the error variant small).
#[derive(Debug, Clone, Default)]
pub struct DuplicateKey {
    pub errmsg: String,
    pub key_pattern: Option<Document>,
    pub key_value: Option<Document>,
}

/// A storage failure, pre-classified by the adapter into the shape command
/// handlers need to build mongod-faithful replies. The adapter maps the real
/// `secantus_storage::StorageError` variants onto these.
#[derive(Debug, Clone)]
pub enum StorageError {
    /// An E11000 duplicate-key conflict — becomes a per-op `writeError` with
    /// `code: 11000` plus `keyPattern` / `keyValue` when known.
    DuplicateKey(Box<DuplicateKey>),
    /// A per-operation failure the adapter has already mapped to a mongod error
    /// `code` (e.g. a bad filter → `2`, an immutable `_id` → `66`). Becomes a
    /// per-op `writeError`; the batch continues (unordered) or stops (ordered).
    WriteError { code: i32, errmsg: String },
    /// An unexpected internal failure — surfaces as a command-level
    /// `InternalError` (`code: 1`), not a per-op write error.
    Internal(String),
}

/// The storage operations the command handlers depend on. Bytes at the seam:
/// documents go in/out as `bson::encode` bytes, as they come off the WT cursor.
pub trait Storage: Send + Sync {
    /// The current cluster time WITHOUT advancing it — for reply gossip
    /// (`$clusterTime` / `operationTime` attached to every reply). The default
    /// returns a zero timestamp (test fakes don't track cluster time); the
    /// WiredTiger-backed adapter forwards to `Storage::peek_cluster_time`.
    fn peek_cluster_time(&self) -> bson::Timestamp {
        bson::Timestamp {
            time: 0,
            increment: 0,
        }
    }

    /// Insert a batch of already-encoded documents. Returns
    /// `(inserted_count, write_errors)` where each write-error document is the
    /// mongod shape `{index, code, errmsg, keyPattern?, keyValue?}` and `index`
    /// refers to a position in `docs`. `ordered` stops at the first error.
    fn insert(
        &self,
        db: &str,
        coll: &str,
        docs: Vec<Vec<u8>>,
        ordered: bool,
    ) -> Result<(usize, Vec<Document>), StorageError>;

    /// Apply one update spec. `update` is the document-form update (operators or
    /// a replacement); pipeline-form updates are a later slice.
    fn update_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        update: &Document,
        multi: bool,
        upsert: bool,
    ) -> Result<UpdateOutcome, StorageError>;

    /// Delete up to `limit` documents matching `filter` (`0` ⇒ all).
    fn delete_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        limit: usize,
    ) -> Result<usize, StorageError>;

    /// Count documents matching `filter`.
    fn count_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<usize, StorageError>;

    /// All documents matching `filter`, in `sort` order (or natural order when
    /// `sort` is `None`), optionally index-`hint`ed. Mirrors
    /// `secantus_storage::find_matching_with`; **skip / limit / projection are
    /// applied by the `find` handler**, not here (the storage method returns the
    /// full ordered match set). A bad hint or filter surfaces as
    /// `StorageError::WriteError { code: 2, .. }` (BadValue).
    fn find(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<RawHint<'_>>,
    ) -> Result<Vec<Vec<u8>>, StorageError>;

    // --- DDL / introspection ------------------------------------------------
    //
    // These carry default no-op/empty implementations so test fakes that don't
    // exercise them keep compiling; the real adapter (`secantus-storage-adapter`)
    // overrides every one. A handler that needs a backend the fake didn't
    // override sees the default, which is fine for unrelated unit tests.

    /// Collection names in `db`.
    fn list_collections(&self, _db: &str) -> Result<Vec<String>, StorageError> {
        Ok(Vec::new())
    }

    /// Create a collection. `true` if newly created, `false` if it already existed.
    fn create_collection(&self, _db: &str, _coll: &str) -> Result<bool, StorageError> {
        Ok(true)
    }

    /// Drop a collection. `true` if it existed.
    fn drop_collection(&self, _db: &str, _coll: &str) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// Index definition documents for a collection (mongod's `listIndexes` shape).
    fn list_indexes(&self, _db: &str, _coll: &str) -> Result<Vec<Document>, StorageError> {
        Ok(Vec::new())
    }

    /// Create an index. `true` if newly created (`false` e.g. for `_id_`).
    fn create_index(
        &self,
        _db: &str,
        _coll: &str,
        _name: &str,
        _key: &Document,
        _options: &Document,
    ) -> Result<bool, StorageError> {
        Ok(true)
    }

    /// Drop a named index. `true` if it existed.
    fn drop_index(&self, _db: &str, _coll: &str, _name: &str) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// Drop every non-`_id` index; returns how many were dropped.
    fn drop_all_indexes(&self, _db: &str, _coll: &str) -> Result<usize, StorageError> {
        Ok(0)
    }

    /// Drop an entire database (all its collections + indexes).
    fn drop_database(&self, _db: &str) -> Result<(), StorageError> {
        Ok(())
    }

    /// Rename a collection. Returns `(succeeded, error_message)`; `succeeded ==
    /// false` carries a reason (source missing / target exists).
    fn rename_collection(
        &self,
        _src_db: &str,
        _src_coll: &str,
        _dst_db: &str,
        _dst_coll: &str,
        _drop_target: bool,
    ) -> Result<(bool, Option<String>), StorageError> {
        Ok((true, None))
    }

    /// Whether the collection is capped.
    fn collection_is_capped(&self, _db: &str, _coll: &str) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// Total size in bytes of the collection's documents.
    fn collection_data_size(&self, _db: &str, _coll: &str) -> Result<i64, StorageError> {
        Ok(0)
    }

    /// Per-index sizes in bytes (`{index_name: size}`), for `collStats` /
    /// `dbStats`.
    fn index_sizes(&self, _db: &str, _coll: &str) -> Result<Document, StorageError> {
        Ok(Document::new())
    }

    // --- users (R5 auth) ----------------------------------------------------

    /// Store a user record (opaque BSON). Returns `false` if the user already
    /// exists and `replace` is false.
    fn add_user(
        &self,
        _db: &str,
        _username: &str,
        _record: &[u8],
        _replace: bool,
    ) -> Result<bool, StorageError> {
        Ok(true)
    }

    /// The stored user record bytes for `(db, username)`, or `None`.
    fn get_user(&self, _db: &str, _username: &str) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(None)
    }

    /// Drop a user. `true` if it existed.
    fn drop_user(&self, _db: &str, _username: &str) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// List user records (all dbs when `db` is `None`), paginated.
    fn list_users(
        &self,
        _db: Option<&str>,
        _skip: usize,
        _limit: usize,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        Ok(Vec::new())
    }

    // --- custom roles (R5b-3) -----------------------------------------------

    /// Store a custom-role record (opaque BSON). Returns `false` if the role
    /// already exists and `replace` is false.
    fn add_role(
        &self,
        _db: &str,
        _name: &str,
        _record: &[u8],
        _replace: bool,
    ) -> Result<bool, StorageError> {
        Ok(true)
    }

    /// The stored custom-role record bytes for `(db, name)`, or `None`.
    fn get_role(&self, _db: &str, _name: &str) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(None)
    }

    /// Drop a custom role. `true` if it existed.
    fn drop_role(&self, _db: &str, _name: &str) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// List custom-role records (all dbs when `db` is `None`), paginated.
    fn list_roles(
        &self,
        _db: Option<&str>,
        _skip: usize,
        _limit: usize,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        Ok(Vec::new())
    }
}
