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
    /// Set when the user pipeline strips the resume-token `_id` from an event —
    /// mongod surfaces this as a getMore-time `ChangeStreamFatalError` (280),
    /// NOT at stream open, so it's detected here during projection.
    fatal_error: Option<CommandError>,
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
                    let (events, stripped_id) = apply_event_pipeline(batch.events, &self.pipeline);
                    if stripped_id {
                        // mongod tags this fatal change-stream error
                        // NonResumableChangeStreamError so drivers don't retry it.
                        self.fatal_error = Some(
                            CommandError::new(
                                280,
                                "ChangeStreamFatalError",
                                "the change stream pipeline may not remove the _id \
                                 (resume token) field",
                            )
                            .with_extra(doc! {
                                "errorLabels": ["NonResumableChangeStreamError"],
                            }),
                        );
                    }
                    events
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

    fn fatal_error(&self) -> Option<CommandError> {
        self.fatal_error.clone()
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

    // The high-water-mark resume token at the start position — mongod returns it
    // as the open reply's `postBatchResumeToken` so a client that sees an empty
    // first batch still has a token to resume from before any event arrives.
    let open_pbrt = {
        let bytes = storage.high_water_mark_token(position);
        if bytes.is_empty() {
            None
        } else {
            Document::from_reader(&mut bytes.as_slice())
                .ok()
                .map(Bson::Document)
        }
    };

    let producer = Box::new(ChangeStreamProducer {
        storage,
        scope,
        opts,
        position,
        invalidated: false,
        limit: 1000,
        pipeline: user_pipeline,
        fatal_error: None,
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
    let mut cursor_doc = doc! {
        "firstBatch": Bson::Array(vec![]),
        "id": Bson::Int64(cursor_id),
        "ns": ns,
    };
    if let Some(tok) = open_pbrt {
        cursor_doc.insert("postBatchResumeToken", tok);
    }
    Ok(doc! {
        "cursor": cursor_doc,
        "ok": 1.0,
    })
}

/// Apply the user change-stream pipeline to a batch of projected event bytes:
/// decode → run the storage-free core pipeline → re-encode. On any error (an
/// event a stage can't handle) the raw events pass through, so a one-off
/// unsupported construct never tears the stream down.
fn apply_event_pipeline(events: Vec<Vec<u8>>, pipeline: &[Bson]) -> (Vec<Vec<u8>>, bool) {
    let raw = events.clone();
    let Ok(decoded) = decode_docs(events) else {
        return (raw, false);
    };
    match secantus_core::aggregate::apply_pipeline(decoded, pipeline, &Document::new(), None) {
        Ok(out) => {
            // mongod treats a pipeline that drops a delivered event's `_id`
            // (the resume token) as a fatal change-stream error. An event
            // filtered out entirely (e.g. by `$match`) is fine — only a
            // surviving event missing `_id` trips it.
            let stripped_id = out.iter().any(|d| !d.contains_key("_id"));
            (encode_docs(out).unwrap_or(raw), stripped_id)
        }
        Err(_) => (raw, false),
    }
}

/// Extract + validate the user pipeline stages after `$changeStream`. Each must
/// be in the change-stream allow-list ([`CHANGE_STREAM_PIPELINE_STAGES`]); a
/// disallowed stage or a second `$changeStream` errors. A stage that strips the
/// event `_id` (the resume token) is NOT rejected here — mongod accepts the
/// pipeline at open and only fails (code 280) at getMore when a delivered event
/// loses its `_id`, so that's detected during projection (see
/// [`apply_event_pipeline`]). The `$changeStreamSplitLargeEvent` marker is
/// dropped (handled at projection).
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
            // mongod reports an unrecognised stage in a change-stream pipeline as
            // Location40324 "Unrecognized pipeline stage name", not a generic
            // BadValue (the drivers' "invalid aggregation stage" test asserts it).
            return Err(CommandError::new(
                40324,
                "Location40324",
                format!("Unrecognized pipeline stage name: '{name}'"),
            ));
        }
        if name == "$changeStreamSplitLargeEvent" {
            continue;
        }
        out.push(stage.clone());
    }
    Ok(out)
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
