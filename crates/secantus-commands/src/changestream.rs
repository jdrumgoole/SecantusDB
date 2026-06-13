//! Change-stream watch handling — R3b for the Rust server.
//!
//! `aggregate` with a leading `$changeStream` stage opens a **tailable** cursor
//! over the oplog instead of running a pipeline. This module builds the watch
//! scope + projection options from the request, seeds the oplog position, and
//! registers a tailable cursor whose producer tails the oplog via the WT-free
//! [`Storage::change_stream_poll`](crate::storage::Storage::change_stream_poll)
//! seam (the projector itself lives in the WiredTiger-linked storage crate; the
//! command crate only sees event bytes).
//!
//! **R3b-a (this slice):** create the cursor (always starting at the current
//! oplog tail) and serve a NON-blocking tailable getMore (poll once, return
//! what's available). **Deferred to R3b-b:** `awaitData` blocking on the oplog
//! condvar, resume tokens (`resumeAfter` / `startAfter` / `startAtOperationTime`),
//! the final `invalidate` event's cursor-close semantics, and empty-batch /
//! heartbeat `postBatchResumeToken` advancement.

use std::sync::Arc;

use bson::{doc, Bson, Document};

use crate::cursors::{CursorProducer, TailableOptions};
use crate::storage::{ChangeStreamOptions, ChangeStreamScope, Storage};
use crate::util::as_i64;
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE};

/// The storage-backed producer stored in a change-stream cursor. Each `produce`
/// polls the oplog tail from the current position and advances it.
struct ChangeStreamProducer {
    storage: Arc<dyn Storage>,
    scope: ChangeStreamScope,
    opts: ChangeStreamOptions,
    position: i64,
    invalidated: bool,
    /// Max oplog rows scanned per poll — bounds a single getMore's work.
    limit: usize,
}

impl CursorProducer for ChangeStreamProducer {
    fn produce(&mut self) -> Vec<Vec<u8>> {
        match self
            .storage
            .change_stream_poll(&self.scope, &self.opts, self.position, self.limit)
        {
            Ok(batch) => {
                self.position = batch.new_position;
                if batch.invalidated {
                    self.invalidated = true;
                }
                batch.events
            }
            // A poll failure (decode / projection error) yields nothing this
            // round rather than tearing down the cursor; the next getMore retries.
            Err(_) => Vec::new(),
        }
    }

    fn position(&self) -> i64 {
        self.position
    }

    fn invalidated(&self) -> bool {
        self.invalidated
    }
}

/// Handle an `aggregate` whose first pipeline stage is `$changeStream`.
/// (The caller has already checked the replica-set persona / 40573 gate.)
pub fn open_change_stream(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx
        .storage
        .as_ref()
        .ok_or_else(|| CommandError::new(1, "InternalError", "storage backend not configured"))?
        .clone();

    let cs_spec = first_change_stream_spec(doc).unwrap_or_default();

    // Scope from the aggregate target + allChangesForCluster:
    //   coll.watch()   -> aggregate: "<coll>"            -> Coll
    //   db.watch()     -> aggregate: 1                   -> Db
    //   client.watch() -> aggregate: 1 + allChanges...   -> Cluster
    let scope = match doc.get("aggregate") {
        Some(Bson::String(coll)) => ChangeStreamScope::Coll {
            db: ctx.db_name.clone(),
            coll: coll.clone(),
        },
        Some(_) => {
            if cs_spec.get_bool("allChangesForCluster").unwrap_or(false) {
                ChangeStreamScope::Cluster
            } else {
                ChangeStreamScope::Db(ctx.db_name.clone())
            }
        }
        None => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "$changeStream requires an aggregate target",
            ));
        }
    };

    let opts = ChangeStreamOptions {
        full_document: cs_spec
            .get_str("fullDocument")
            .unwrap_or("default")
            .to_string(),
        full_document_before_change: cs_spec
            .get_str("fullDocumentBeforeChange")
            .unwrap_or("off")
            .to_string(),
        show_expanded_events: cs_spec.get_bool("showExpandedEvents").unwrap_or(false),
    };

    // R3b-a always starts at the current oplog tail (events strictly after
    // creation). Resume support (`resumeAfter` / `startAtOperationTime`) is R3b-b.
    let position = storage.oplog_tail_seq();

    let ns = match doc.get("aggregate") {
        Some(Bson::String(c)) => format!("{}.{}", ctx.db_name, c),
        _ => format!("{}.$cmd.aggregate", ctx.db_name),
    };

    let producer = Box::new(ChangeStreamProducer {
        storage,
        scope,
        opts,
        position,
        invalidated: false,
        limit: 1000,
    });

    let cursors = ctx.cursors()?;
    let cursor_id = cursors
        .register_tailable(
            &ns,
            producer,
            TailableOptions {
                await_data: true,
                no_cursor_timeout: false,
                position_seq: position,
                collection_uuid: None,
                initial_remaining: Vec::new(),
            },
        )
        .map_err(|e| {
            CommandError::new(
                1,
                "InternalError",
                format!("cursor registration failed: {e:?}"),
            )
        })?;

    // An opening change-stream aggregate always returns an empty firstBatch;
    // the client drives the stream with getMore.
    Ok(doc! {
        "cursor": {
            "firstBatch": Bson::Array(vec![]),
            "id": Bson::Int64(cursor_id),
            "ns": ns,
        },
        "ok": 1.0,
    })
}

/// The `$changeStream: {...}` spec document from the first pipeline stage.
fn first_change_stream_spec(doc: &Document) -> Option<Document> {
    let pipeline = doc.get("pipeline")?.as_array()?;
    let first = pipeline.first()?.as_document()?;
    first.get("$changeStream")?.as_document().cloned()
}

/// `cursor.batchSize`, defaulting to the wire default. Unused by R3b-a's
/// empty-firstBatch open but parsed for symmetry with `aggregate`.
#[allow(dead_code)]
fn cursor_batch_size(doc: &Document) -> i64 {
    doc.get("cursor")
        .and_then(Bson::as_document)
        .and_then(|c| c.get("batchSize"))
        .and_then(as_i64)
        .unwrap_or(DEFAULT_BATCH_SIZE as i64)
}
