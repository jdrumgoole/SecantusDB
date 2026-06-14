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
// Re-exported so trait implementors (the WT adapter) can name the collation type
// without a direct `secantus-core` dependency.
pub use secantus_core::collation::Collation;

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

/// A change-stream watch scope (mirrors `secantus_storage::changestreams::Scope`;
/// the WiredTiger-backed adapter translates). WT-free so the command crate
/// stays WT-free — the `$changeStream` handler builds it from the pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum ChangeStreamScope {
    /// Whole-cluster change stream (`watch()` on the client).
    Cluster,
    /// A single database (`db.watch()`).
    Db(String),
    /// A single collection (`coll.watch()`).
    Coll { db: String, coll: String },
}

/// Full-document / pre-image projection modes for a change-stream watch, passed
/// through to `secantus_storage::changestreams::project`. Plain strings keep the
/// seam WT-free.
#[derive(Debug, Clone, Default)]
pub struct ChangeStreamOptions {
    /// `"default"` / `"updateLookup"` / `"whenAvailable"` / `"required"`.
    pub full_document: String,
    /// `"off"` / `"whenAvailable"` / `"required"`.
    pub full_document_before_change: String,
    /// `showExpandedEvents: true` surfaces DDL events (create / modify / …).
    pub show_expanded_events: bool,
}

/// One poll of the oplog tail for a change-stream cursor: the projected event
/// documents (as `bson::encode` bytes), the new oplog position consumed (the
/// seq of the last entry read), and whether an invalidating event
/// (drop / rename / dropDatabase on the watched scope) was produced.
#[derive(Debug, Clone, Default)]
pub struct ChangeStreamBatch {
    pub events: Vec<Vec<u8>>,
    pub new_position: i64,
    pub invalidated: bool,
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

    /// The next monotonic cluster time, ADVANCING and persisting it — for
    /// `hello`'s `lastWrite.opTime.ts` (mirrors `Storage.current_cluster_time`).
    /// Minting (rather than peeking) keeps the advertised opTime strictly greater
    /// than the last write, which is what `startAtOperationTime` resumes rely on.
    /// The default forwards to `peek_cluster_time` (fakes don't track time).
    fn current_cluster_time(&self) -> bson::Timestamp {
        self.peek_cluster_time()
    }

    // --- change streams (R3b) ----------------------------------------------
    //
    // The command crate stays WiredTiger-free, but the change-stream projector
    // (`changestreams::project`) needs the concrete `secantus_storage::Storage`
    // for `updateLookup` find / pre-image reads. So the tailable getMore loop
    // drives these trait methods; the adapter implements them by tailing the
    // oplog and projecting. Defaults make test fakes (which don't tail) compile.

    /// Poll the oplog from `after_seq` (exclusive) for up to `limit` change
    /// events matching `scope`, projecting each. Returns the event bytes, the
    /// new position consumed, and whether an invalidating event was produced.
    /// Default: an empty batch at the same position (no oplog to tail).
    fn change_stream_poll(
        &self,
        _scope: &ChangeStreamScope,
        _opts: &ChangeStreamOptions,
        after_seq: i64,
        _limit: usize,
    ) -> Result<ChangeStreamBatch, StorageError> {
        Ok(ChangeStreamBatch {
            events: Vec::new(),
            new_position: after_seq,
            invalidated: false,
        })
    }

    /// Block until the oplog advances past `after_seq` or `timeout_ms` elapses;
    /// return the current tail seq. Used for `awaitData`. Default: `after_seq`.
    fn wait_for_oplog(&self, after_seq: i64, _timeout_ms: u64) -> i64 {
        after_seq
    }

    /// Wake any blocked `wait_for_oplog` waiters without advancing the oplog
    /// (`killCursors` uses this to unblock a tailing getMore). Default: no-op.
    fn notify_oplog_waiters(&self) {}

    /// The current oplog tail seq (`next_seq - 1`) — a fresh watch starts here
    /// (it sees events strictly after it). Default: 0.
    fn oplog_tail_seq(&self) -> i64 {
        0
    }

    /// The smallest oplog seq still retained (retention floor); 0 if empty.
    /// A resume token older than this has fallen off the oplog. Default: 0.
    fn oplog_floor_seq(&self) -> i64 {
        0
    }

    /// The smallest oplog seq whose entry timestamp is `>= ts` (for
    /// `startAtOperationTime`); tail+1 if none qualify. Default: 0.
    fn seq_for_timestamp(&self, _ts: bson::Timestamp) -> i64 {
        0
    }

    /// Decode a change-stream resume token (`{_data: "<hex>"}`) to the oplog
    /// `seq` it points at, for `resumeAfter` / `startAfter`. `None` if the token
    /// is malformed. Kept on the seam so the token format stays in the
    /// WiredTiger-linked `changestreams` module. Default: `None`.
    fn resume_token_seq(&self, _token: &Document) -> Option<i64> {
        None
    }

    /// A high-water-mark resume token at oplog `seq` and the current cluster
    /// time, encoded as `bson::encode({_data: "<hex>"})`. Returned as the
    /// `postBatchResumeToken` of an empty change-stream batch so the client can
    /// resume past a quiet stretch. Built via the same encoder as event tokens.
    /// Default: empty (no token).
    fn high_water_mark_token(&self, _seq: i64) -> Vec<u8> {
        Vec::new()
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

    /// Operator-form update with positional operators (`$` / `$[]` / `$[ident]`)
    /// resolved via `array_filters` and the query filter, plus `let_vars` (command
    /// `let`) visible to `$expr` in the filter. The default forwards to
    /// `update_matching` (ignoring `array_filters` / `let_vars`) so fakes / the
    /// no-positional path are unaffected; the WiredTiger adapter routes to the
    /// option-aware `Storage::update_matching`.
    #[allow(clippy::too_many_arguments)]
    fn update_matching_array_filters(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        update: &Document,
        multi: bool,
        upsert: bool,
        _array_filters: &[Document],
        _let_vars: &Document,
        _collation: Option<&Collation>,
    ) -> Result<UpdateOutcome, StorageError> {
        self.update_matching(db, coll, filter, update, multi, upsert)
    }

    /// Pipeline-form update (`u: [ {$set: …}, … ]`): each matched doc is rewritten
    /// by running the aggregation pipeline over it, diff-style in the oplog;
    /// `let_vars` (command `let`) are visible to `$expr` in the filter and to the
    /// pipeline expressions. The default rejects it as a `BadValue` write error
    /// (test fakes don't implement pipeline updates); the WiredTiger adapter
    /// forwards to `Storage::update_matching_pipeline`.
    #[allow(clippy::too_many_arguments)]
    fn update_matching_pipeline(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
        _pipeline: &[Bson],
        _multi: bool,
        _upsert: bool,
        _let_vars: &Document,
        _collation: Option<&Collation>,
    ) -> Result<UpdateOutcome, StorageError> {
        Err(StorageError::WriteError {
            code: 2,
            errmsg: "pipeline-form updates are not supported by this storage backend".into(),
        })
    }

    /// Delete up to `limit` documents matching `filter` (`0` ⇒ all).
    fn delete_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        limit: usize,
    ) -> Result<usize, StorageError>;

    /// Delete with `let_vars` (command `let`) visible to `$expr` in the filter.
    /// The default forwards to `delete_matching` (ignoring `let_vars`) so fakes
    /// are unaffected; the WiredTiger adapter routes to the let-aware
    /// `Storage::delete_matching`.
    #[allow(clippy::too_many_arguments)]
    fn delete_matching_with_let(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        limit: usize,
        _let_vars: &Document,
        _collation: Option<&Collation>,
    ) -> Result<usize, StorageError> {
        self.delete_matching(db, coll, filter, limit)
    }

    /// Count documents matching `filter`.
    fn count_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<usize, StorageError>;

    /// Count with a `collation` applied to string comparison. The default
    /// forwards to `count_matching` (ignoring collation) so fakes are unaffected;
    /// the WiredTiger adapter routes to the collation-aware storage path
    /// (forcing a COLLSCAN). `None` collation = the default path.
    fn count_collated(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        _collation: Option<&Collation>,
    ) -> Result<usize, StorageError> {
        self.count_matching(db, coll, filter)
    }

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

    /// `find` with a `collation` applied to filter comparison + sort order, plus
    /// `let_vars` (command `let`) visible to `$expr` in the filter. The default
    /// forwards to `find` (ignoring both) so fakes are unaffected; the WiredTiger
    /// adapter routes to the collation-/let-aware storage path (collation forces a
    /// COLLSCAN + collation-folded in-memory sort). `None` collation = default.
    #[allow(clippy::too_many_arguments)]
    fn find_collated(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<RawHint<'_>>,
        _collation: Option<&Collation>,
        _let_vars: &Document,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        self.find(db, coll, filter, sort, hint)
    }

    /// The plan `find` would use for these args, without executing — the input to
    /// `explain`'s `winningPlan`. Returns `{kind: "COLLSCAN"}` or
    /// `{kind: "IXSCAN", indexName, keyPattern, direction}`. Default COLLSCAN; the
    /// WT adapter mirrors the storage index router.
    fn explain_plan(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
        _sort: Option<&Document>,
        _hint: Option<RawHint<'_>>,
    ) -> Result<Document, StorageError> {
        let mut d = Document::new();
        d.insert("kind", "COLLSCAN");
        Ok(d)
    }

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

    /// The collection's stored options blob (`validator` / `validationAction` /
    /// `changeStreamPreAndPostImages` / `capped` / …), empty when none/unknown.
    /// Default empty (fakes don't track options); the WT adapter forwards.
    fn get_collection_options(&self, _db: &str, _coll: &str) -> Result<Document, StorageError> {
        Ok(Document::new())
    }

    /// Merge `opts` into the collection's stored options (creating it if needed) —
    /// for `create` with options and `collMod`. Default no-op; WT adapter forwards.
    fn set_collection_options(
        &self,
        _db: &str,
        _coll: &str,
        _opts: &Document,
    ) -> Result<(), StorageError> {
        Ok(())
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
