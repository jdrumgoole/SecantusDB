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
//! **R3b-a:** create the cursor (starting at the current oplog tail) and serve a
//! non-blocking tailable getMore. **R3b-b (this slice):** seed the position from
//! a resume point — resume tokens (`resumeAfter` / `startAfter`) and
//! `startAtOperationTime` — rejecting a resume point that has fallen off the back
//! of the oplog with `ChangeStreamHistoryLost` (286). The `awaitData` blocking
//! getMore and the empty-batch high-water-mark `postBatchResumeToken` live in
//! [`crate::cursors::get_more`]; the final `invalidate` event's cursor-close
//! semantics in [`crate::cursors::CursorRegistry::tailable_next_batch`].

use std::sync::Arc;

use bson::{doc, Bson, Document};

use crate::cursors::{CursorProducer, TailableOptions};
use crate::storage::{ChangeStreamOptions, ChangeStreamScope, Storage};
use crate::util::{as_i64, decode_docs, encode_docs};
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE};

/// Aggregation stages a change-stream pipeline may contain after `$changeStream`
/// (mongod's allow-list). Anything else is rejected when the stream is opened.
const CHANGE_STREAM_PIPELINE_STAGES: &[&str] = &[
    "$addFields",
    "$set",
    "$project",
    "$replaceRoot",
    "$replaceWith",
    "$match",
    "$redact",
    "$unset",
    "$changeStreamSplitLargeEvent",
];

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
    /// User aggregation stages after `$changeStream` (e.g. `$project` / `$match`
    /// / `$addFields`), applied to each event before it reaches the client.
    pipeline: Vec<Bson>,
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
                if self.pipeline.is_empty() {
                    batch.events
                } else {
                    apply_event_pipeline(batch.events, &self.pipeline)
                }
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

    // Start position (events read are strictly after it):
    //   resumeAfter / startAfter — the token's seq (resume just past it)
    //   startAtOperationTime     — one before the first seq at/after the ts
    //   neither                  — the current oplog tail (fresh stream)
    let resume_token = cs_spec
        .get("resumeAfter")
        .or_else(|| cs_spec.get("startAfter"))
        .and_then(Bson::as_document);
    let position = if let Some(tok) = resume_token {
        storage.resume_token_seq(tok).ok_or_else(|| {
            CommandError::new(
                40647,
                "Location40647",
                "resumeAfter / startAfter resume token is not valid",
            )
        })?
    } else if let Some(Bson::Timestamp(ts)) = cs_spec.get("startAtOperationTime") {
        storage.seq_for_timestamp(*ts).saturating_sub(1)
    } else {
        storage.oplog_tail_seq()
    };

    // An explicit resume point that has already fallen off the back of the
    // oplog can't be honoured — mongod's ChangeStreamHistoryLost (286).
    if resume_token.is_some() {
        let floor = storage.oplog_floor_seq();
        if floor > 0 && position + 1 < floor {
            return Err(CommandError::new(
                286,
                "ChangeStreamHistoryLost",
                "Resume of change stream was not possible, as the resume point may \
                 no longer be in the oplog.",
            ));
        }
    }

    let ns = match doc.get("aggregate") {
        Some(Bson::String(c)) => format!("{}.{}", ctx.db_name, c),
        _ => format!("{}.$cmd.aggregate", ctx.db_name),
    };

    // User pipeline stages after `$changeStream` (e.g. `$project` / `$match`),
    // validated against the change-stream allow-list and applied to each event.
    let user_pipeline = extract_change_stream_pipeline(doc)?;

    let producer = Box::new(ChangeStreamProducer {
        storage,
        scope,
        opts,
        position,
        invalidated: false,
        limit: 1000,
        pipeline: user_pipeline,
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

/// Apply the user change-stream pipeline to a batch of projected event bytes:
/// decode → run the storage-free core pipeline → re-encode. On any error (an
/// event a stage can't handle) the raw events pass through, so a one-off
/// unsupported construct never tears the stream down.
fn apply_event_pipeline(events: Vec<Vec<u8>>, pipeline: &[Bson]) -> Vec<Vec<u8>> {
    let raw = events.clone();
    let Ok(decoded) = decode_docs(events) else {
        return raw;
    };
    match secantus_core::aggregate::apply_pipeline(decoded, pipeline, &Document::new(), None) {
        Ok(out) => encode_docs(out).unwrap_or(raw),
        Err(_) => raw,
    }
}

/// Extract + validate the user pipeline stages after `$changeStream`. Each must
/// be in the change-stream allow-list ([`CHANGE_STREAM_PIPELINE_STAGES`]); a
/// disallowed stage or a second `$changeStream` errors, and a stage that strips
/// the event `_id` (the resume token) is a `ChangeStreamFatalError` (280). The
/// `$changeStreamSplitLargeEvent` marker is dropped (handled at projection).
fn extract_change_stream_pipeline(doc: &Document) -> Result<Vec<Bson>, CommandError> {
    let Some(Bson::Array(stages)) = doc.get("pipeline") else {
        return Ok(Vec::new());
    };
    let mut out: Vec<Bson> = Vec::new();
    for stage in stages.iter().skip(1) {
        let Some(s) = stage.as_document() else {
            return Err(CommandError::new(
                2,
                "BadValue",
                "each aggregation stage must be a document",
            ));
        };
        let name = s.keys().next().map(String::as_str).unwrap_or("");
        if name == "$changeStream" {
            return Err(CommandError::new(
                40602,
                "Location40602",
                "$changeStream is only allowed as the first stage in a pipeline",
            ));
        }
        if !CHANGE_STREAM_PIPELINE_STAGES.contains(&name) {
            return Err(CommandError::new(
                2,
                "BadValue",
                format!("{name} is not permitted in a $changeStream pipeline"),
            ));
        }
        if name == "$changeStreamSplitLargeEvent" {
            continue;
        }
        if stage_removes_id(name, s.get(name)) {
            return Err(CommandError::new(
                280,
                "ChangeStreamFatalError",
                "the change stream pipeline may not remove the _id (resume token) field",
            ));
        }
        out.push(stage.clone());
    }
    Ok(out)
}

/// Whether a `$project`/`$unset` stage strips the event's `_id` (resume token).
fn stage_removes_id(stage_name: &str, spec: Option<&Bson>) -> bool {
    match stage_name {
        "$project" => spec
            .and_then(Bson::as_document)
            .and_then(|d| d.get("_id"))
            .is_some_and(|v| {
                matches!(v, Bson::Int32(0) | Bson::Int64(0) | Bson::Boolean(false))
                    || matches!(v, Bson::Double(d) if *d == 0.0)
            }),
        "$unset" => match spec {
            Some(Bson::String(s)) => s == "_id",
            Some(Bson::Array(a)) => a.iter().any(|x| x.as_str() == Some("_id")),
            _ => false,
        },
        _ => false,
    }
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
