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

/// `(id_key, bson)` document rows, as returned by the collection scans the
/// tailable-find producer polls (`scan_docs_after_id_key`).
pub type IdKeyRows = Vec<(Vec<u8>, Vec<u8>)>;

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
    /// The post-image of a single-doc (`multi == false`) update / upsert,
    /// captured inside the storage write. `findAndModify {new: true}` must
    /// return this rather than re-`find`ing — the re-read races concurrent
    /// writers and can hand two clients the same "new" document. Captured
    /// only when the caller passes `want_post_image` — a plain `update`
    /// never reads it, so it skips the per-doc clone.
    pub post_image: Option<Document>,
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
    /// A write lost a `WT_ROLLBACK` race — mongod's `WriteConflict` (112). The
    /// command layer surfaces it as a command-level error (so the dispatch
    /// transaction envelope attaches the `TransientTransactionError` label).
    WriteConflict,
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
    /// A `$changeStreamSplitLargeEvent` stage was present: every event carries a
    /// `splitEvent: {fragment, of}` envelope (we never actually split, so always
    /// `{fragment: 1, of: 1}`).
    pub split_large_events: bool,
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
    /// A fatal projection error (e.g. `fullDocument: required` with
    /// changeStreamPreAndPostImages disabled) — `(code, errmsg)`. The producer
    /// surfaces it as a getMore-time `ok: 0` reply that ends the stream.
    pub fatal: Option<(i32, String)>,
}

/// The storage operations the command handlers depend on. Bytes at the seam:
/// documents go in/out as `bson::encode` bytes, as they come off the WT cursor.
pub trait Storage: Send + Sync {
    /// True when the store is non-persistent (WiredTiger `in_memory=true`).
    ///
    /// Read by `serverStatus.storageEngine.persistent`. Defaults to `false`
    /// (i.e. persistent) so test fakes, which are not the thing whose
    /// durability anyone is asking about, need not implement it; the
    /// WiredTiger-backed adapter forwards to the real flag.
    fn in_memory(&self) -> bool {
        false
    }

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
            fatal: None,
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

    /// Whether a resume token is for an `invalidate` event. `resumeAfter` on such
    /// a token is rejected (the stream it came from is over) — mongod requires
    /// `startAfter` instead (`InvalidResumeToken`, 260). Default: `false`.
    fn resume_token_from_invalidate(&self, _token: &Document) -> bool {
        false
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
        _validator: Option<&Document>,
        // `validationLevel: "moderate"` — exempt docs that ALREADY failed the
        // validator from update-time validation (inserts stay validated).
        _validator_moderate: bool,
        _want_post_image: bool,
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
        _validator: Option<&Document>,
        // `validationLevel: "moderate"` — exempt docs that ALREADY failed the
        // validator from update-time validation (inserts stay validated).
        _validator_moderate: bool,
        _want_post_image: bool,
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
    /// `{kind: "IXSCAN", indexName, keyPattern, direction, multikey}`. Default COLLSCAN; the
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

    /// Names of all databases that hold at least one collection (for
    /// `listDatabases`). Default empty; the WT adapter forwards.
    fn list_databases(&self) -> Result<Vec<String>, StorageError> {
        Ok(Vec::new())
    }

    /// Create a collection. `true` if newly created, `false` if it already existed.
    fn create_collection(&self, _db: &str, _coll: &str) -> Result<bool, StorageError> {
        Ok(true)
    }

    /// Create a collection, persisting `options` (`capped` / `validator` / …) AND
    /// carrying them in the `create` oplog entry so PITR replay reconstructs them.
    /// Default: create then a plain (oplog-silent) option write; the WT adapter
    /// forwards to `Storage::create_collection_with_options`.
    fn create_collection_with_options(
        &self,
        db: &str,
        coll: &str,
        options: &Document,
    ) -> Result<bool, StorageError> {
        let created = self.create_collection(db, coll)?;
        if created && !options.is_empty() {
            self.set_collection_options(db, coll, options)?;
        }
        Ok(created)
    }

    /// The collection's stored options blob (`validator` / `validationAction` /
    /// `changeStreamPreAndPostImages` / `capped` / …), empty when none/unknown.
    /// Default empty (fakes don't track options); the WT adapter forwards.
    fn get_collection_options(&self, _db: &str, _coll: &str) -> Result<Document, StorageError> {
        Ok(Document::new())
    }

    /// Merge `opts` into the collection's stored options (creating it if needed) —
    /// for `create` with options. Default no-op; WT adapter forwards.
    fn set_collection_options(
        &self,
        _db: &str,
        _coll: &str,
        _opts: &Document,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    /// `collMod`: like [`Storage::set_collection_options`], but also emits a DDL
    /// `op: "c"` `collMod` oplog entry so a `showExpandedEvents` change stream
    /// surfaces a `modify` event. Default falls back to the silent option write.
    fn coll_mod(&self, db: &str, coll: &str, opts: &Document) -> Result<(), StorageError> {
        self.set_collection_options(db, coll, opts)
    }

    /// Drop a collection. `true` if it existed.
    fn drop_collection(&self, _db: &str, _coll: &str) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// Index definition documents for a collection (mongod's `listIndexes` shape).
    fn list_indexes(&self, _db: &str, _coll: &str) -> Result<Vec<Document>, StorageError> {
        Ok(Vec::new())
    }

    /// Whether `(db, coll)` exists. Default `false` (test fakes track no
    /// collections); the WiredTiger adapter forwards to the registry.
    fn collection_exists(&self, _db: &str, _coll: &str) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// Per-database profiling state `{level, slowms, sampleRate}` (mongod's
    /// `profile` shape). Default: profiling off.
    fn get_profile(&self, _db: &str) -> Result<Document, StorageError> {
        Ok(bson::doc! { "level": 0i32, "slowms": 100i32, "sampleRate": 1.0 })
    }

    /// Set per-database profiling state. Default no-op (fakes don't persist it).
    fn set_profile(
        &self,
        _db: &str,
        _level: i32,
        _slowms: i32,
        _sample_rate: f64,
    ) -> Result<(), StorageError> {
        Ok(())
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

    /// Merge `opts` into an existing index's stored options (e.g. `prepareUnique`
    /// / `unique`). Backs `collMod {index: {keyPattern|name, ...}}`. `true` if the
    /// index existed. Default no-op for test fakes; the WT adapter forwards to
    /// `Storage::set_index_options`.
    fn set_index_options(
        &self,
        _db: &str,
        _coll: &str,
        _name: &str,
        _opts: &Document,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// Group the `_id`s of docs sharing a key on index `name` (groups of >= 2).
    /// A non-empty result blocks a `collMod {index: {unique: true}}` conversion
    /// (code 359 + `violations`). Default empty; the WT adapter forwards to
    /// `Storage::find_index_duplicates`.
    fn find_index_duplicates(
        &self,
        _db: &str,
        _coll: &str,
        _name: &str,
    ) -> Result<Vec<Vec<Bson>>, StorageError> {
        Ok(Vec::new())
    }

    /// Drop an entire database (all its collections + indexes).
    fn drop_database(&self, _db: &str) -> Result<(), StorageError> {
        Ok(())
    }

    /// Force a checkpoint and write a backup `.tar.gz` of the WiredTiger home to
    /// `output_path`, returning `(path, size_bytes)`. Backs `secantusAdmin.backupArchive`
    /// (PITR). Default: unsupported (test fakes have no on-disk state); the WT
    /// adapter forwards to `Storage::create_archive`.
    fn create_archive(&self, _output_path: &str) -> Result<(String, u64), StorageError> {
        Err(StorageError::Internal(
            "backupArchive: this storage backend has no on-disk state to archive".into(),
        ))
    }

    /// Take a PITR v2 base snapshot into `archive_dir` (`base-<head>.tar.gz`),
    /// returning `(path, size_bytes)`. Backs `secantusAdmin.archiveBaseSnapshot`.
    /// Default: unsupported; the WT adapter forwards to
    /// `Storage::archive_base_snapshot`.
    fn archive_base_snapshot(&self, _archive_dir: &str) -> Result<(String, u64), StorageError> {
        Err(StorageError::Internal(
            "archiveBaseSnapshot: this storage backend has no on-disk state to archive".into(),
        ))
    }

    /// Drop oplog rows past the retention window, returning the number pruned.
    /// Backs `secantusAdmin.pruneOplog` — an operator-driven immediate sweep
    /// (the WT backend also prunes opportunistically on every emit). Default:
    /// unsupported; the WT adapter forwards to `Storage::prune_oplog`.
    fn prune_oplog(&self) -> Result<usize, StorageError> {
        Err(StorageError::Internal(
            "pruneOplog: this storage backend has no oplog to prune".into(),
        ))
    }

    /// Run TTL pruning across every collection, returning the number of docs
    /// deleted. Backs `secantusAdmin.pruneTtl` — an immediate pass (the WT
    /// backend also sweeps on a background cadence). Default: unsupported; the
    /// WT adapter forwards to `Storage::prune_ttl_all_collections`.
    fn prune_ttl_all(&self) -> Result<usize, StorageError> {
        Err(StorageError::Internal(
            "pruneTtl: this storage backend has no TTL indexes to prune".into(),
        ))
    }

    /// Extract a backup archive (from `create_archive`) into `target_dir`,
    /// returning `(abs_target, abs_archive, file_count)`. Backs
    /// `secantusAdmin.restoreArchive` — a side-channel restore into a fresh
    /// directory the operator then points a new server at; the running server's
    /// storage is untouched. Rejects a non-empty target unless `allow_existing`,
    /// and rejects an archive with no `WiredTiger` metadata. Default:
    /// unsupported; the WT adapter forwards to
    /// `secantus_storage::extract_backup_archive_ex`.
    fn restore_archive(
        &self,
        _archive_path: &str,
        _target_dir: &str,
        _allow_existing: bool,
    ) -> Result<(String, String, u64), StorageError> {
        Err(StorageError::Internal(
            "restoreArchive: this storage backend has no on-disk archive support".into(),
        ))
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

    /// The collection's 16-byte UUID (mongod's collection identity, surfaced as
    /// `info.uuid` BinData(4) in `listCollections` and `ui` in the oplog).
    fn collection_uuid(&self, _db: &str, _coll: &str) -> Result<Vec<u8>, StorageError> {
        Ok(Vec::new())
    }

    /// Documents whose `id_key` sorts strictly after `after` (all of them when
    /// `after` is `None`), as `(id_key, bson)` pairs — the tailable-find producer
    /// polls this for docs inserted since it last returned.
    fn scan_docs_after_id_key(
        &self,
        _db: &str,
        _coll: &str,
        _after: Option<&[u8]>,
    ) -> Result<IdKeyRows, StorageError> {
        Ok(Vec::new())
    }

    /// The smallest `id_key` currently in the collection (`None` if empty) — a
    /// tailable cursor uses it to detect capped rollover (`CappedPositionLost`).
    fn collection_min_id_key(
        &self,
        _db: &str,
        _coll: &str,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(None)
    }

    /// Documents whose **RecordId** is strictly greater than `after` (all when
    /// `None`), as `(recordid, bson)` pairs in insertion order — the tailable-find
    /// producer polls this. RecordId order is mongod's tailable (insertion) order,
    /// unlike `scan_docs_after_id_key`, which only matches it for monotonic `_id`s.
    fn scan_docs_after_recordid(
        &self,
        _db: &str,
        _coll: &str,
        _after: Option<i64>,
    ) -> Result<Vec<(i64, Vec<u8>)>, StorageError> {
        Ok(Vec::new())
    }

    /// The smallest **RecordId** currently in the collection (`None` if empty) —
    /// the tailable cursor uses it to detect capped rollover (`CappedPositionLost`),
    /// aligned with the FIFO (RecordId-order) eviction.
    fn collection_min_recordid(&self, _db: &str, _coll: &str) -> Result<Option<i64>, StorageError> {
        Ok(None)
    }

    /// The largest **RecordId** in the collection (`None` if empty) — the
    /// tailable cursor seeds its watermark with this (position at end of the
    /// initial scan, mongod's tailable start point).
    fn collection_max_recordid(&self, _db: &str, _coll: &str) -> Result<Option<i64>, StorageError> {
        Ok(None)
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

    // --- multi-document transactions (T2) ----------------------------------
    //
    // The opaque handle is the storage `UserTransactionHandle`, boxed as
    // `dyn Any + Send` so the command crate (and its test fakes) stay WT-free.
    // The WiredTiger adapter downcasts it. Defaults make the registry's state
    // machine run WITHOUT real isolation (writes apply immediately; abort can't
    // roll back) — enough for the lifecycle / error-label tests; the adapter
    // override adds real WT isolation.

    /// Open a new multi-document transaction, returning an opaque handle. Default
    /// returns a unit handle (no real WT transaction).
    fn begin_user_transaction(&self) -> Result<Box<dyn std::any::Any + Send>, StorageError> {
        Ok(Box::new(()))
    }

    /// Run one in-transaction statement `f` with `handle`'s WT session installed,
    /// so the statement's reads/writes execute inside the transaction. Default
    /// just runs `f` (no installation). Returns the statement's reply document.
    fn run_in_user_transaction(
        &self,
        _handle: &mut (dyn std::any::Any + Send),
        f: &mut dyn FnMut() -> Document,
    ) -> Result<Document, StorageError> {
        Ok(f())
    }

    /// Commit the transaction's WT session. Default no-op.
    fn commit_user_transaction(
        &self,
        _handle: &mut (dyn std::any::Any + Send),
    ) -> Result<(), StorageError> {
        Ok(())
    }

    /// Roll back the transaction's WT session. Default no-op.
    fn rollback_user_transaction(
        &self,
        _handle: &mut (dyn std::any::Any + Send),
    ) -> Result<(), StorageError> {
        Ok(())
    }
}
