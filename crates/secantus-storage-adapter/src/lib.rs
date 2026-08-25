//! `secantus-storage-adapter` — R4b: bridge the WiredTiger-backed
//! `secantus_storage::Storage` to the `secantus_commands::Storage` trait the
//! Rust server (R4a) dispatches against.
//!
//! The command crate stays WiredTiger-free by depending only on the trait; this
//! crate — which *does* link WiredTiger (via `secantus-storage`) and is therefore
//! excluded from the clean workspace and CI-validated only — supplies the
//! concrete implementation. It is a near-identity over the matching method
//! signatures, plus two translations:
//!
//! * **Hints:** the command seam passes the raw `hint` as `Bson`
//!   (`RawHint`); here it becomes a `secantus_storage::Hint`
//!   (`String` ⇒ `Name`, document ⇒ `KeySpec`).
//! * **Errors:** `secantus_storage::StorageError` → `secantus_commands::
//!   StorageError`, so duplicate keys keep their `keyPattern`/`keyValue`, bad
//!   hints / unsupported query constructs become a `BadValue` (2) write error,
//!   and engine/IO faults become `Internal`.

use std::sync::Arc;

use bson::{Bson, Document};
use secantus_commands::storage::{
    ChangeStreamBatch, ChangeStreamOptions, ChangeStreamScope, Collation, DuplicateKey, IdKeyRows,
    RawHint, Storage as CmdStorage, StorageError, UpdateOutcome,
};
use secantus_storage::changestreams::{self, ResumeTokenData, Scope as WtScope};
use secantus_storage::{
    ExplainPlan, Hint, Storage as WtStorage, StorageError as WtError, UserTransactionHandle,
};

/// Wraps a shared WiredTiger-backed `Storage` and presents it as the command
/// layer's `Storage`. Construct with [`StorageAdapter::new`] and hand the
/// resulting `Arc<dyn secantus_commands::Storage>` to `secantus_server::bind`.
pub struct StorageAdapter {
    inner: Arc<WtStorage>,
}

impl StorageAdapter {
    pub fn new(inner: Arc<WtStorage>) -> Self {
        StorageAdapter { inner }
    }
}

impl CmdStorage for StorageAdapter {
    fn in_memory(&self) -> bool {
        self.inner.in_memory()
    }

    fn peek_cluster_time(&self) -> bson::Timestamp {
        // Gossip must never fail a command — fall back to a zero timestamp
        // if the (read-only, mints-once-on-virgin) peek somehow errors.
        self.inner.peek_cluster_time().unwrap_or(bson::Timestamp {
            time: 0,
            increment: 0,
        })
    }

    fn current_cluster_time(&self) -> bson::Timestamp {
        self.inner
            .current_cluster_time()
            .unwrap_or(bson::Timestamp {
                time: 0,
                increment: 0,
            })
    }

    fn begin_user_transaction(&self) -> Result<Box<dyn std::any::Any + Send>, StorageError> {
        let h = self.inner.begin_user_transaction().map_err(map_err)?;
        Ok(Box::new(h))
    }

    fn run_in_user_transaction(
        &self,
        handle: &mut (dyn std::any::Any + Send),
        f: &mut dyn FnMut() -> Document,
    ) -> Result<Document, StorageError> {
        let h = handle
            .downcast_mut::<UserTransactionHandle>()
            .ok_or_else(|| StorageError::Internal("bad transaction handle".into()))?;
        self.inner.with_user_transaction(h, f).map_err(map_err)
    }

    fn commit_user_transaction(
        &self,
        handle: &mut (dyn std::any::Any + Send),
    ) -> Result<(), StorageError> {
        let h = handle
            .downcast_mut::<UserTransactionHandle>()
            .ok_or_else(|| StorageError::Internal("bad transaction handle".into()))?;
        self.inner.commit_user_transaction(h).map_err(map_err)
    }

    fn rollback_user_transaction(
        &self,
        handle: &mut (dyn std::any::Any + Send),
    ) -> Result<(), StorageError> {
        let h = handle
            .downcast_mut::<UserTransactionHandle>()
            .ok_or_else(|| StorageError::Internal("bad transaction handle".into()))?;
        self.inner.rollback_user_transaction(h).map_err(map_err)
    }

    fn change_stream_poll(
        &self,
        scope: &ChangeStreamScope,
        opts: &ChangeStreamOptions,
        after_seq: i64,
        limit: usize,
    ) -> Result<ChangeStreamBatch, StorageError> {
        let wt_scope = to_wt_scope(scope);
        // read_oplog's start_seq is inclusive; after_seq is the last consumed.
        let rows = self
            .inner
            .read_oplog(after_seq + 1, limit)
            .map_err(map_err)?;
        let mut events: Vec<Vec<u8>> = Vec::new();
        let mut new_position = after_seq;
        let mut invalidated = false;
        let mut fatal: Option<(i32, String)> = None;
        for (seq, blob) in rows {
            // Always advance the position past a scanned entry, even when it
            // projects to nothing (noop heartbeats / other-scope writes) — that
            // keeps the resume token moving and the next poll cheap.
            new_position = seq;
            let entry = Document::from_reader(&mut blob.as_slice())
                .map_err(|e| StorageError::Internal(format!("oplog decode: {e}")))?;
            let projected = changestreams::project(
                seq,
                &entry,
                &self.inner,
                &opts.full_document,
                &opts.full_document_before_change,
                &wt_scope,
                opts.show_expanded_events,
            );
            // A fatal projection error (e.g. fullDocument: required with
            // changeStreamPreAndPostImages disabled) ends the stream with an
            // ok: 0 reply rather than tearing down the poll — surface it via the
            // batch so the producer/getMore can report it (code 280).
            let (event, invalidates) = match projected {
                Ok(v) => v,
                Err(WtError::ChangeStreamFatal(m)) => {
                    fatal = Some((280, m));
                    break;
                }
                Err(e) => return Err(map_err(e)),
            };
            if let Some(ev) = event {
                push_event(&mut events, ev, opts.split_large_events)?;
                if invalidates {
                    // An invalidating event (drop / rename / dropDatabase on the
                    // watched scope) is followed by a synthesized terminal
                    // `invalidate` event, then the cursor closes — mirroring
                    // `commands.py`'s producer (project → invalidate_event → break).
                    let inv = changestreams::invalidate_event(seq, &entry).map_err(map_err)?;
                    push_event(&mut events, inv, opts.split_large_events)?;
                    invalidated = true;
                    break;
                }
            }
        }
        Ok(ChangeStreamBatch {
            events,
            new_position,
            invalidated,
            fatal,
        })
    }

    fn wait_for_oplog(&self, after_seq: i64, timeout_ms: u64) -> i64 {
        self.inner.wait_for_oplog(after_seq, timeout_ms)
    }

    fn notify_oplog_waiters(&self) {
        self.inner.notify_oplog_waiters();
    }

    fn oplog_tail_seq(&self) -> i64 {
        // The change-stream OPEN position (this trait method's only caller
        // is the open-seeding path): sync mode, the visible tail — the
        // minted tail can sit past an entry whose transaction has not
        // committed yet, and a watch opened there would permanently miss the
        // entry when it commits. Async mode, `oplog_open_seq` additionally
        // waits for the drainer to reach the minted tail so writes acked
        // before the open can't surface as post-open events.
        self.inner.oplog_open_seq()
    }

    fn oplog_floor_seq(&self) -> i64 {
        self.inner.oplog_floor_seq().unwrap_or(0)
    }

    fn seq_for_timestamp(&self, ts: bson::Timestamp) -> i64 {
        self.inner.find_seq_for_ts(ts).unwrap_or(0)
    }

    fn resume_token_seq(&self, token: &Document) -> Option<i64> {
        changestreams::parse_resume_token(token).ok().map(|d| d.seq)
    }

    fn resume_token_from_invalidate(&self, token: &Document) -> bool {
        changestreams::parse_resume_token(token)
            .map(|d| d.from_invalidate)
            .unwrap_or(false)
    }

    fn high_water_mark_token(&self, seq: i64) -> Vec<u8> {
        let ts = self.inner.peek_cluster_time().unwrap_or(bson::Timestamp {
            time: 0,
            increment: 0,
        });
        let data = ResumeTokenData {
            seq,
            ts,
            ns: String::new(),
            document_key: Document::new(),
            from_invalidate: false,
        };
        match changestreams::make_resume_token(&data) {
            Ok(doc) => {
                let mut buf = Vec::new();
                if doc.to_writer(&mut buf).is_ok() {
                    buf
                } else {
                    Vec::new()
                }
            }
            Err(_) => Vec::new(),
        }
    }

    fn insert(
        &self,
        db: &str,
        coll: &str,
        docs: Vec<Vec<u8>>,
        ordered: bool,
    ) -> Result<(usize, Vec<Document>), StorageError> {
        // Same return shape (inserted_count, write_error docs) on both sides.
        self.inner.insert(db, coll, docs, ordered).map_err(map_err)
    }

    fn update_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        update: &Document,
        multi: bool,
        upsert: bool,
    ) -> Result<UpdateOutcome, StorageError> {
        let o = self
            .inner
            .update_matching(
                db,
                coll,
                filter,
                update,
                multi,
                upsert,
                &[],
                &Document::new(),
                None,
                None,
                false,
            )
            .map_err(map_err)?;
        Ok(UpdateOutcome {
            matched: o.matched,
            modified: o.modified,
            upserted_id: o.upserted_id,
            post_image: o.post_image,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn update_matching_array_filters(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        update: &Document,
        multi: bool,
        upsert: bool,
        array_filters: &[Document],
        let_vars: &Document,
        collation: Option<&Collation>,
        validator: Option<&Document>,
        validator_moderate: bool,
        want_post_image: bool,
    ) -> Result<UpdateOutcome, StorageError> {
        let o = self
            .inner
            .update_matching_leveled(
                db,
                coll,
                filter,
                update,
                multi,
                upsert,
                array_filters,
                let_vars,
                collation,
                validator,
                validator_moderate,
                want_post_image,
            )
            .map_err(map_err)?;
        Ok(UpdateOutcome {
            matched: o.matched,
            modified: o.modified,
            upserted_id: o.upserted_id,
            post_image: o.post_image,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn update_matching_pipeline(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        pipeline: &[Bson],
        multi: bool,
        upsert: bool,
        let_vars: &Document,
        collation: Option<&Collation>,
        validator: Option<&Document>,
        validator_moderate: bool,
        want_post_image: bool,
    ) -> Result<UpdateOutcome, StorageError> {
        let o = self
            .inner
            .update_matching_pipeline(
                db,
                coll,
                filter,
                pipeline,
                multi,
                upsert,
                let_vars,
                collation,
                validator,
                validator_moderate,
                want_post_image,
            )
            .map_err(map_err)?;
        Ok(UpdateOutcome {
            matched: o.matched,
            modified: o.modified,
            upserted_id: o.upserted_id,
            post_image: o.post_image,
        })
    }

    fn delete_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        limit: usize,
    ) -> Result<usize, StorageError> {
        self.inner
            .delete_matching(db, coll, filter, limit, &Document::new(), None)
            .map_err(map_err)
    }

    #[allow(clippy::too_many_arguments)]
    fn delete_matching_with_let(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        limit: usize,
        let_vars: &Document,
        collation: Option<&Collation>,
    ) -> Result<usize, StorageError> {
        self.inner
            .delete_matching(db, coll, filter, limit, let_vars, collation)
            .map_err(map_err)
    }

    fn count_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<usize, StorageError> {
        self.inner
            .count_matching(db, coll, filter, None)
            .map_err(map_err)
    }

    fn count_collated(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        collation: Option<&Collation>,
    ) -> Result<usize, StorageError> {
        self.inner
            .count_matching(db, coll, filter, collation)
            .map_err(map_err)
    }

    fn find(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<RawHint<'_>>,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let resolved = hint.map(to_hint);
        self.inner
            .find_matching_with(
                db,
                coll,
                filter,
                sort,
                resolved.as_ref(),
                None,
                &Document::new(),
            )
            .map_err(map_err)
    }

    #[allow(clippy::too_many_arguments)]
    fn find_collated(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<RawHint<'_>>,
        collation: Option<&Collation>,
        let_vars: &Document,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let resolved = hint.map(to_hint);
        self.inner
            .find_matching_with(
                db,
                coll,
                filter,
                sort,
                resolved.as_ref(),
                collation,
                let_vars,
            )
            .map_err(map_err)
    }

    fn list_collections(&self, db: &str) -> Result<Vec<String>, StorageError> {
        self.inner.list_collections(db).map_err(map_err)
    }

    fn list_databases(&self) -> Result<Vec<String>, StorageError> {
        self.inner.list_databases().map_err(map_err)
    }

    fn create_collection(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
        self.inner.create_collection(db, coll).map_err(map_err)
    }

    fn create_collection_with_options(
        &self,
        db: &str,
        coll: &str,
        options: &Document,
    ) -> Result<bool, StorageError> {
        self.inner
            .create_collection_with_options(db, coll, options)
            .map_err(map_err)
    }

    fn get_collection_options(&self, db: &str, coll: &str) -> Result<Document, StorageError> {
        self.inner.get_collection_options(db, coll).map_err(map_err)
    }

    fn explain_plan(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<RawHint<'_>>,
    ) -> Result<Document, StorageError> {
        let resolved = hint.map(to_hint);
        let plan = self
            .inner
            .explain_plan_with(db, coll, filter, sort, resolved.as_ref())
            .map_err(map_err)?;
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
                d.insert(
                    "multikey",
                    self.inner.index_is_multikey(db, coll, &index_name),
                );
                d.insert("indexName", index_name);
                d.insert("keyPattern", Bson::Document(key_pattern));
                d.insert("direction", direction);
            }
        }
        Ok(d)
    }

    fn set_collection_options(
        &self,
        db: &str,
        coll: &str,
        opts: &Document,
    ) -> Result<(), StorageError> {
        self.inner
            .set_collection_options(db, coll, opts)
            .map_err(map_err)
    }

    fn coll_mod(&self, db: &str, coll: &str, opts: &Document) -> Result<(), StorageError> {
        self.inner.coll_mod(db, coll, opts).map_err(map_err)
    }

    fn drop_collection(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
        self.inner.drop_collection(db, coll).map_err(map_err)
    }

    fn list_indexes(&self, db: &str, coll: &str) -> Result<Vec<Document>, StorageError> {
        self.inner.list_indexes(db, coll).map_err(map_err)
    }

    fn collection_exists(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
        self.inner.collection_exists(db, coll).map_err(map_err)
    }

    fn get_profile(&self, db: &str) -> Result<Document, StorageError> {
        self.inner.get_profile(db).map_err(map_err)
    }

    fn set_profile(
        &self,
        db: &str,
        level: i32,
        slowms: i32,
        sample_rate: f64,
    ) -> Result<(), StorageError> {
        self.inner
            .set_profile(db, level, slowms, sample_rate)
            .map_err(map_err)
    }

    fn create_index(
        &self,
        db: &str,
        coll: &str,
        name: &str,
        key: &Document,
        options: &Document,
    ) -> Result<bool, StorageError> {
        self.inner
            .create_index(db, coll, name, key, options)
            .map_err(map_err)
    }

    fn drop_index(&self, db: &str, coll: &str, name: &str) -> Result<bool, StorageError> {
        self.inner.drop_index(db, coll, name).map_err(map_err)
    }

    fn drop_all_indexes(&self, db: &str, coll: &str) -> Result<usize, StorageError> {
        self.inner.drop_all_indexes(db, coll).map_err(map_err)
    }

    fn set_index_options(
        &self,
        db: &str,
        coll: &str,
        name: &str,
        opts: &Document,
    ) -> Result<bool, StorageError> {
        self.inner
            .set_index_options(db, coll, name, opts)
            .map_err(map_err)
    }

    fn find_index_duplicates(
        &self,
        db: &str,
        coll: &str,
        name: &str,
    ) -> Result<Vec<Vec<Bson>>, StorageError> {
        self.inner
            .find_index_duplicates(db, coll, name)
            .map_err(map_err)
    }

    fn drop_database(&self, db: &str) -> Result<(), StorageError> {
        self.inner.drop_database(db).map_err(map_err)
    }

    fn create_archive(&self, output_path: &str) -> Result<(String, u64), StorageError> {
        self.inner
            .create_archive(output_path)
            .map(|info| (info.path, info.size_bytes))
            .map_err(map_err)
    }

    fn archive_base_snapshot(&self, archive_dir: &str) -> Result<(String, u64), StorageError> {
        self.inner
            .archive_base_snapshot(archive_dir)
            .map(|info| (info.path, info.size_bytes))
            .map_err(map_err)
    }

    fn prune_oplog(&self) -> Result<usize, StorageError> {
        self.inner.prune_oplog(None).map_err(map_err)
    }

    fn prune_ttl_all(&self) -> Result<usize, StorageError> {
        self.inner
            .prune_ttl_all_collections(bson::DateTime::now())
            .map_err(map_err)
    }

    fn restore_archive(
        &self,
        archive_path: &str,
        target_dir: &str,
        allow_existing: bool,
    ) -> Result<(String, String, u64), StorageError> {
        // Free function on the storage crate — no live handle needed; it rebuilds
        // a fresh on-disk directory the operator points a new server at.
        secantus_storage::extract_backup_archive_ex(archive_path, target_dir, allow_existing)
            .map_err(map_err)
    }

    fn rename_collection(
        &self,
        src_db: &str,
        src_coll: &str,
        dst_db: &str,
        dst_coll: &str,
        drop_target: bool,
    ) -> Result<(bool, Option<String>), StorageError> {
        self.inner
            .rename_collection(src_db, src_coll, dst_db, dst_coll, drop_target)
            .map_err(map_err)
    }

    fn collection_is_capped(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
        self.inner.collection_is_capped(db, coll).map_err(map_err)
    }

    fn collection_uuid(&self, db: &str, coll: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.collection_uuid(db, coll).map_err(map_err)
    }

    fn scan_docs_after_id_key(
        &self,
        db: &str,
        coll: &str,
        after: Option<&[u8]>,
    ) -> Result<IdKeyRows, StorageError> {
        self.inner
            .scan_docs_after_id_key(db, coll, after)
            .map_err(map_err)
    }

    fn collection_min_id_key(&self, db: &str, coll: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.inner.collection_min_id_key(db, coll).map_err(map_err)
    }

    fn scan_docs_after_recordid(
        &self,
        db: &str,
        coll: &str,
        after: Option<i64>,
    ) -> Result<Vec<(i64, Vec<u8>)>, StorageError> {
        self.inner
            .scan_docs_after_recordid(db, coll, after)
            .map_err(map_err)
    }

    fn collection_min_recordid(&self, db: &str, coll: &str) -> Result<Option<i64>, StorageError> {
        self.inner
            .collection_min_recordid(db, coll)
            .map_err(map_err)
    }

    fn collection_max_recordid(&self, db: &str, coll: &str) -> Result<Option<i64>, StorageError> {
        self.inner
            .collection_max_recordid(db, coll)
            .map_err(map_err)
    }

    fn collection_data_size(&self, db: &str, coll: &str) -> Result<i64, StorageError> {
        self.inner.collection_data_size(db, coll).map_err(map_err)
    }

    fn index_sizes(&self, db: &str, coll: &str) -> Result<Document, StorageError> {
        self.inner.index_sizes(db, coll).map_err(map_err)
    }

    fn add_user(
        &self,
        db: &str,
        username: &str,
        record: &[u8],
        replace: bool,
    ) -> Result<bool, StorageError> {
        self.inner
            .add_user(db, username, record, replace)
            .map_err(map_err)
    }

    fn get_user(&self, db: &str, username: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.inner.get_user(db, username).map_err(map_err)
    }

    fn drop_user(&self, db: &str, username: &str) -> Result<bool, StorageError> {
        self.inner.drop_user(db, username).map_err(map_err)
    }

    fn list_users(
        &self,
        db: Option<&str>,
        skip: usize,
        limit: usize,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        self.inner.list_users(db, skip, limit).map_err(map_err)
    }

    fn add_role(
        &self,
        db: &str,
        name: &str,
        record: &[u8],
        replace: bool,
    ) -> Result<bool, StorageError> {
        self.inner
            .add_role(db, name, record, replace)
            .map_err(map_err)
    }

    fn get_role(&self, db: &str, name: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.inner.get_role(db, name).map_err(map_err)
    }

    fn drop_role(&self, db: &str, name: &str) -> Result<bool, StorageError> {
        self.inner.drop_role(db, name).map_err(map_err)
    }

    fn list_roles(
        &self,
        db: Option<&str>,
        skip: usize,
        limit: usize,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        self.inner.list_roles(db, skip, limit).map_err(map_err)
    }
}

/// Translate the WT-free command-layer scope into the storage projector's
/// `Scope`. Identity-shaped; the split exists only to keep `secantus-commands`
/// free of the WiredTiger-linked `secantus-storage` crate.
/// Encode a projected change event into the batch, splitting it into fragments
/// first when the user opted into `splitLargeChangeStreamEvents` /
/// `$changeStreamSplitLargeEvent` (one over-16MB event → several fragments, each
/// a valid event tagged `splitEvent: {fragment, of}`). Mirrors `commands.py`'s
/// producer applying `stamp_split_event` to every projected / invalidate event.
fn push_event(
    events: &mut Vec<Vec<u8>>,
    ev: Document,
    split_large_events: bool,
) -> Result<(), StorageError> {
    let fragments = if split_large_events {
        changestreams::stamp_split_event(ev).map_err(map_err)?
    } else {
        vec![ev]
    };
    for frag in fragments {
        let mut buf = Vec::new();
        frag.to_writer(&mut buf)
            .map_err(|e| StorageError::Internal(format!("event encode: {e}")))?;
        events.push(buf);
    }
    Ok(())
}

fn to_wt_scope(scope: &ChangeStreamScope) -> WtScope {
    match scope {
        ChangeStreamScope::Cluster => WtScope::Cluster,
        ChangeStreamScope::Db(db) => WtScope::Db(db.clone()),
        ChangeStreamScope::Coll { db, coll } => WtScope::Coll {
            db: db.clone(),
            coll: coll.clone(),
        },
    }
}

/// Convert a raw `hint` value into the storage `Hint`. A string is an index
/// name (or `"$natural"` / `"_id_"`); a document is a key spec. Anything else
/// falls through to an empty name, which `resolve_hint` rejects as `BadHint`
/// (→ `BadValue` at the command layer), matching mongod.
fn to_hint(b: RawHint<'_>) -> Hint {
    match b {
        Bson::String(s) => Hint::Name(s.clone()),
        Bson::Document(d) => Hint::KeySpec(d.clone()),
        _ => Hint::Name(String::new()),
    }
}

/// Translate a storage error into the command layer's pre-classified error.
fn map_err(e: WtError) -> StorageError {
    match e {
        WtError::DuplicateKey(conflict) => StorageError::DuplicateKey(Box::new(DuplicateKey {
            errmsg: format!(
                "E11000 duplicate key error index: {} dup key: {:?}",
                conflict.index, conflict.key_value
            ),
            key_pattern: Some(conflict.key_pattern),
            key_value: Some(conflict.key_value),
        })),
        WtError::DuplicateId => StorageError::WriteError {
            code: 11000,
            errmsg: "E11000 duplicate key error".to_string(),
        },
        // A lost WT_ROLLBACK race → mongod's WriteConflict (112). Routed
        // command-level by the write handlers so the txn envelope labels it.
        WtError::WriteConflict => StorageError::WriteConflict,
        // An oversized multi-document transaction → mongod's
        // TransactionTooLargeForCache (313). Deliberately NOT in the
        // transient-label set: retrying the same transaction hits the same
        // wall.
        WtError::TransactionTooLargeForCache => StorageError::WriteError {
            code: 313,
            errmsg: "Transaction is too large and will not fit in the storage engine cache"
                .to_string(),
        },
        // An over-limit document → mongod's BSONObjectTooLarge (10334).
        WtError::DocumentTooLarge(size) => StorageError::WriteError {
            code: 10334,
            errmsg: format!(
                "object to insert too large. size in bytes: {size}, max size: 16777216"
            ),
        },
        // Post-apply validator failure → mongod's DocumentValidationFailure (121).
        WtError::DocumentValidationFailure => StorageError::WriteError {
            code: 121,
            errmsg: "Document failed validation".to_string(),
        },
        // An update that would change `_id` → mongod's ImmutableField (66).
        WtError::ImmutableField => StorageError::WriteError {
            code: 66,
            errmsg: "Performing an update on the path '_id' would modify the immutable field '_id'"
                .to_string(),
        },
        // Bad hint / unsupported query construct → BadValue (2), the same code
        // the Python server surfaces for these at the command layer.
        WtError::BadHint(m) => StorageError::WriteError { code: 2, errmsg: m },
        // A named refusal (non-numeric $inc/$mul) → mongod's TypeMismatch (14),
        // not the generic BadValue the plain defer would produce.
        WtError::UpdateTypeMismatch(m) => StorageError::WriteError {
            code: 14,
            errmsg: m,
        },
        WtError::QueryUnsupported => StorageError::WriteError {
            code: 2,
            errmsg: "query uses a construct the Rust server does not support".to_string(),
        },
        WtError::UnsupportedId => StorageError::WriteError {
            code: 2,
            errmsg: "_id is of a type the Rust server does not support".to_string(),
        },
        WtError::UnsupportedValue => StorageError::WriteError {
            code: 2,
            errmsg: "an indexed value is of a type the Rust server does not support".to_string(),
        },
        // Index-create / change-stream faults don't arise on the CRUD path, but
        // map them to a command-level internal error if they ever surface here.
        // Index re-create conflicts → mongod's IndexOptionsConflict (85) /
        // IndexKeySpecsConflict (86); an unsupported index type (text/hashed) →
        // CannotCreateIndex (67). These reach the command layer via createIndexes.
        WtError::IndexOptionsConflict(m) => StorageError::WriteError {
            code: 85,
            errmsg: m,
        },
        WtError::IndexKeySpecsConflict(m) => StorageError::WriteError {
            code: 86,
            errmsg: m,
        },
        WtError::CreateIndexUnsupported(m) => StorageError::WriteError {
            code: 67,
            errmsg: m,
        },
        // Change-stream faults don't arise on the CRUD path; map to internal.
        WtError::ChangeStreamFatal(m) | WtError::Internal(m) => StorageError::Internal(m),
        WtError::Wt(err) => StorageError::Internal(format!("WiredTiger error: {err:?}")),
        WtError::Bson(m) => StorageError::Internal(format!("BSON error: {m}")),
    }
}
