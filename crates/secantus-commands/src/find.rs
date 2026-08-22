//! The `find` command — the keystone read path, and the producer of the cursors
//! the registry (R3) manages.
//!
//! A port of `commands.py::_find`'s non-tailable path. The Rust storage's
//! `find` returns the full ordered match set (it does not take skip / limit /
//! projection), so this handler does them: fetch (sorted, optionally hinted) →
//! `skip` → `limit` → `projection` (via `secantus_core::projection`) →
//! `_split_into_cursor` (firstBatch + register the remainder).
//!
//! Empty-result filter validation: when nothing matched, the filter is re-run
//! once against an empty document so an invalid / unsupported filter surfaces
//! `BadValue` (as it would against a non-empty collection through the storage
//! scan) instead of silently returning an empty cursor.
//!
//! `tailable: true` on a capped collection opens a tailable cursor: the matched
//! docs seed firstBatch (+ a queued remainder), then [`TailableFindProducer`]
//! polls for docs inserted afterwards (capped rollover → `CappedPositionLost`).
//!
//! **Notes:**
//! * `let` IS applied — command `let` vars (`$$NOW` + evaluated values) are
//!   visible to `$expr` in the filter, threaded through `find_collated`.
//!   `collation` IS applied — filter matching + sort order are collation-aware
//!   (COLLSCAN-forced); a non-ASCII / numericOrdering collation → `BadValue`.

use std::sync::Arc;

use bson::{doc, Bson, Document};

use crate::cursors::{CursorProducer, CursorRegistry, TailableOptions};
use crate::storage::Storage;
use crate::util::{
    as_i64, bool_field, coll_arg, collation_of, command_error, doc_field, docs_to_bson,
    encode_docs, resolve_let_vars,
};
use crate::{
    CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE, MAX_GETMORE_BATCH_BYTES,
};

/// Producer for a tailable cursor on a capped collection (`find` with
/// `tailable: true`). Each `produce` scans the collection for documents whose
/// **RecordId** sorts after the last one returned — insertion order, which is
/// what mongod's tailable cursors follow and what capped FIFO eviction uses
/// (`id_key` order only coincides for monotonic `_id`s). If the cursor's anchor
/// has been evicted by capped rollover (the collection's min RecordId now
/// exceeds it), it surfaces `CappedPositionLost` (136). Mirrors
/// `commands.py::_find_tailable` (whose Python server is still `id_key`-keyed, so
/// there id_key IS insertion order — this RecordId form is the Rust-server fix).
struct TailableFindProducer {
    storage: Arc<dyn Storage>,
    db: String,
    coll: String,
    after: Option<i64>,
    fatal: Option<CommandError>,
}

impl CursorProducer for TailableFindProducer {
    fn produce(&mut self) -> Vec<Vec<u8>> {
        if self.fatal.is_some() {
            return Vec::new();
        }
        // Capped rollover: if the doc we last returned has been evicted, the
        // cursor is lapped — mongod kills it with CappedPositionLost.
        if let Some(after) = self.after {
            match self.storage.collection_min_recordid(&self.db, &self.coll) {
                Ok(Some(min)) if min > after => {
                    self.fatal = Some(capped_position_lost());
                    return Vec::new();
                }
                Ok(None) => {
                    self.fatal = Some(capped_position_lost());
                    return Vec::new();
                }
                _ => {}
            }
        }
        match self
            .storage
            .scan_docs_after_recordid(&self.db, &self.coll, self.after)
        {
            Ok(rows) if !rows.is_empty() => {
                self.after = Some(rows[rows.len() - 1].0);
                rows.into_iter().map(|(_rid, doc)| doc).collect()
            }
            _ => Vec::new(),
        }
    }

    fn position(&self) -> i64 {
        0
    }

    fn invalidated(&self) -> bool {
        false
    }

    fn fatal_error(&self) -> Option<CommandError> {
        self.fatal.clone()
    }
}

fn capped_position_lost() -> CommandError {
    CommandError::new(
        136,
        "CappedPositionLost",
        "CollectionScan died due to position in capped collection being deleted.",
    )
}

/// Shape a `BadValue` for a filter the matcher couldn't evaluate, naming the
/// offending operator when there's an unrecognised one (e.g. `$badOperator`) so
/// drivers' error-document tests see the operator in the message, matching
/// mongod / the Python server.
pub(crate) fn query_filter_error(filter: &Document) -> CommandError {
    // An unrecognised aggregation-expression operator inside a `$expr` is
    // mongod's `168 InvalidPipelineOperator` — `Unrecognized expression '$op'` —
    // not a generic query-operator `BadValue`.
    if let Some(op) = unknown_expr_operator_in_filter(filter) {
        return CommandError::new(
            168,
            "InvalidPipelineOperator",
            format!("Unrecognized expression '{op}'"),
        );
    }
    // An invalid `$jsonSchema` keyword carries mongod's own code (9
    // FailedToParse / 14 TypeMismatch), not the generic BadValue.
    if let Some((code, name, msg)) = json_schema_error_in_filter(filter) {
        return CommandError::new(code, name, msg);
    }
    match secantus_core::query::first_unknown_operator(filter) {
        Some(op) => CommandError::new(2, "BadValue", format!("unsupported query operator: {op}")),
        None => CommandError::new(2, "BadValue", "unsupported or invalid query filter"),
    }
}

/// The first invalid `$jsonSchema` keyword violation anywhere in `filter`
/// (recursing through `$and`/`$or`/`$nor`), as mongod's (code, codeName,
/// errmsg). mongod validates schema keywords at parse time, so the command
/// entry points check this up-front — even against an empty collection.
pub(crate) fn json_schema_error_in_filter(
    filter: &Document,
) -> Option<(i32, &'static str, String)> {
    for (k, v) in filter.iter() {
        if k == "$jsonSchema" {
            if let Some(e) = secantus_core::query::json_schema_keyword_error(v) {
                return Some(e);
            }
        } else if (k == "$and" || k == "$or" || k == "$nor") && matches!(v, Bson::Array(_)) {
            if let Bson::Array(arr) = v {
                for sub in arr {
                    if let Bson::Document(d) = sub {
                        if let Some(e) = json_schema_error_in_filter(d) {
                            return Some(e);
                        }
                    }
                }
            }
        }
    }
    None
}

/// The first unrecognised expression operator reachable through a `$expr`
/// anywhere in `filter` (recursing through `$and`/`$or`/`$nor`). `None` when no
/// `$expr` references an unknown operator.
fn unknown_expr_operator_in_filter(filter: &Document) -> Option<String> {
    for (k, v) in filter.iter() {
        if k == "$expr" {
            if let Some(op) = secantus_core::expressions::first_unknown_expr_operator(v) {
                return Some(op);
            }
        } else if (k == "$and" || k == "$or" || k == "$nor") && matches!(v, Bson::Array(_)) {
            if let Bson::Array(arr) = v {
                for sub in arr {
                    if let Bson::Document(d) = sub {
                        if let Some(op) = unknown_expr_operator_in_filter(d) {
                            return Some(op);
                        }
                    }
                }
            }
        }
    }
    None
}

/// `find` — run a query and open a cursor over the results.
/// Build the aggregate command equivalent to a `find` on a view: the find's
/// filter / sort / skip / limit / projection become pipeline stages (in that
/// order), over the view namespace, carrying the collation / let / batchSize.
fn build_view_find_aggregate(doc: &Document, coll: &str) -> Document {
    let mut pipeline: Vec<Bson> = Vec::new();
    if let Some(Bson::Document(f)) = doc.get("filter") {
        if !f.is_empty() {
            pipeline.push(Bson::Document(doc! { "$match": f.clone() }));
        }
    }
    if let Some(Bson::Document(s)) = doc.get("sort") {
        if !s.is_empty() {
            pipeline.push(Bson::Document(doc! { "$sort": s.clone() }));
        }
    }
    if let Some(n) = doc.get("skip").and_then(as_i64) {
        if n > 0 {
            pipeline.push(Bson::Document(doc! { "$skip": n }));
        }
    }
    if let Some(n) = doc.get("limit").and_then(as_i64) {
        if n > 0 {
            pipeline.push(Bson::Document(doc! { "$limit": n }));
        }
    }
    if let Some(Bson::Document(p)) = doc.get("projection") {
        if !p.is_empty() {
            pipeline.push(Bson::Document(doc! { "$project": p.clone() }));
        }
    }
    let batch_size = doc
        .get("batchSize")
        .and_then(as_i64)
        .unwrap_or(DEFAULT_BATCH_SIZE as i64);
    let mut agg = doc! {
        "aggregate": coll,
        "pipeline": pipeline,
        "cursor": { "batchSize": batch_size },
    };
    if let Some(c) = doc.get("collation") {
        agg.insert("collation", c.clone());
    }
    if let Some(l) = doc.get("let") {
        agg.insert("let", l.clone());
    }
    agg
}

pub fn find(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "find")?;
    // A view: translate the find into the equivalent aggregate over the base
    // collection (the find options become pipeline stages after the view's own
    // pipeline) and delegate — the aggregate handler resolves the view. `find` and
    // `aggregate` return the same cursor-reply shape. Mirrors commands._find.
    let is_view = {
        let storage = ctx.storage()?;
        storage
            .get_collection_options(&ctx.db_name, &coll)
            .map(|o| o.contains_key("viewOn"))
            .unwrap_or(false)
    };
    if is_view {
        let agg = build_view_find_aggregate(doc, &coll);
        return crate::aggregate::aggregate(&agg, ctx);
    }
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;

    let filter = doc_field(doc, "filter");
    // An unrecognized `$expr` expression operator is rejected by mongod at parse
    // time (before any scan), so surface `168 InvalidPipelineOperator` regardless
    // of whether the collection is empty or any doc matches. (The per-doc scan
    // path below would otherwise map it to a generic BadValue.)
    if let Some(op) = unknown_expr_operator_in_filter(&filter) {
        return Ok(CommandError::new(
            168,
            "InvalidPipelineOperator",
            format!("Unrecognized expression '{op}'"),
        )
        .into_reply());
    }
    // Same parse-time treatment for an invalid `$jsonSchema` keyword: mongod
    // rejects it (9 FailedToParse / 14 TypeMismatch) before any scan, even on
    // an empty collection — the per-doc path would silently ignore it.
    if let Some((code, name, msg)) = json_schema_error_in_filter(&filter) {
        return Ok(CommandError::new(code, name, msg).into_reply());
    }
    let skip = doc.get("skip").and_then(as_i64).unwrap_or(0).max(0) as usize;
    let limit = doc.get("limit").and_then(as_i64).unwrap_or(0).max(0) as usize; // 0 ⇒ no limit
    let sort = doc.get("sort").and_then(Bson::as_document);
    // An empty projection means "no projection" (return full docs). Mutable
    // because `returnKey` / `showRecordId` rewrite the result set and then
    // suppress any normal projection (mongod ignores `projection` for them).
    let mut projection = doc
        .get("projection")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());
    // A projection may not mix inclusion and exclusion (except `_id`). mongod
    // rejects it at parse with a per-field 31254 / 31253 — the exact wording
    // mongo-node-driver's projection-error tests assert.
    if let Some(spec) = projection {
        if let Some(err) = projection_mix_error(spec) {
            return Ok(err.into_reply());
        }
        // `$meta` projection parse-time validation: an unknown argument is a
        // Location17308, and `{$meta: "textScore"}` without a `$text` query is a
        // Location40218. (Recognized-but-unsupported args validate clean here and
        // are omitted from the result by `apply_projection`.)
        if let Some(err) = projection_meta_error(spec, &filter) {
            return Ok(err.into_reply());
        }
        // Positional (`arr.$`) validation is parse-time in mongod, so an invalid
        // one errors even when nothing matches. The Rust engine can't reproduce
        // the exact Location code (31276 / 31395 / 51246) — a generic BadValue,
        // same as its other deferred error paths.
        let q = if filter.is_empty() {
            None
        } else {
            Some(&filter)
        };
        if secantus_core::projection::validate_projection(spec, q).is_err() {
            return Ok(
                CommandError::new(2, "BadValue", "invalid positional projection").into_reply(),
            );
        }
    }
    let hint = doc.get("hint");
    let collation = collation_of(doc);
    // Command `let` → vars visible to `$expr` in the filter.
    let let_vars = resolve_let_vars(doc.get("let"));
    // `batchSize` is tri-state: absent ⇒ default, 0 ⇒ empty firstBatch + cursor,
    // explicit positive ⇒ that size. A present-but-non-numeric value (e.g. a
    // string) is a TypeMismatch — mongod rejects it, and the mongo-c-driver
    // find/batchSize test sends `{batchSize: 'foo'}` expecting a server error.
    let batch_size = match doc.get("batchSize") {
        Some(b) => match as_i64(b) {
            Some(n) => n,
            None => {
                return Err(CommandError::new(
                    14,
                    "TypeMismatch",
                    "BSON field 'batchSize' is the wrong type, expected a number",
                ))
            }
        },
        None => DEFAULT_BATCH_SIZE as i64,
    };
    let single_batch = bool_field(doc, "singleBatch", false);
    let tailable = bool_field(doc, "tailable", false);
    let await_data = bool_field(doc, "awaitData", false);
    let ns = format!("{}.{}", ctx.db_name, coll);

    // A tailable cursor is only valid on a capped collection (mongod rejects it
    // on a non-capped one with BadValue). Check before the fetch.
    if tailable
        && !storage
            .collection_is_capped(&ctx.db_name, &coll)
            .map_err(command_error)?
    {
        return Ok(CommandError::new(
            2,
            "BadValue",
            format!("error processing query: ns={ns} tailable cursor requested on non capped collection"),
        )
        .into_reply());
    }

    let mut docs = storage
        .find_collated(
            &ctx.db_name,
            &coll,
            &filter,
            sort,
            hint,
            collation.as_ref(),
            &let_vars,
        )
        .map_err(command_error)?;

    // Validate the filter even when nothing matched: against a non-empty
    // collection the storage scan evaluates the filter per doc and an
    // invalid/unsupported one surfaces `BadValue`; on an empty result the scan
    // never runs, so an invalid filter would otherwise return an empty cursor
    // instead of the error. Re-run the matcher once against an empty document
    // (operator recognition is doc-independent) to surface the same `BadValue`.
    if docs.is_empty() && !filter.is_empty() {
        secantus_core::query::matches(&Document::new(), &filter, &let_vars, collation.as_ref())
            .map_err(|_| query_filter_error(&filter))?;
    }

    // Cursor `min` / `max` index bounds (inclusive lower / exclusive upper),
    // evaluated on the hinted index's key — applied to the index-ordered fetch
    // before skip/limit. (pymongo translates the legacy `$min`/`$max`/`$query`
    // filter wrapper into these top-level fields.)
    let min_b = doc
        .get("min")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());
    let max_b = doc
        .get("max")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());
    if min_b.is_some() || max_b.is_some() {
        docs = apply_min_max(docs, min_b, max_b, hint, storage, &ctx.db_name, &coll)?;
    }

    // skip / limit applied after the sorted fetch (the storage returns the full
    // ordered match set).
    if skip > 0 {
        if skip >= docs.len() {
            docs.clear();
        } else {
            docs.drain(..skip);
        }
    }
    if limit > 0 && docs.len() > limit {
        docs.truncate(limit);
    }

    // Tailable cursor: the matched docs seed firstBatch (+ a queued remainder);
    // a producer then polls the collection for docs inserted after this find.
    if tailable {
        let storage_arc = ctx
            .storage
            .as_ref()
            .ok_or_else(|| CommandError::new(1, "InternalError", "storage backend not configured"))?
            .clone();
        // Watermark = the collection's current max RecordId, so the producer
        // follows only docs inserted after this find — insertion order, aligned
        // with capped FIFO eviction. (Was the last matched doc's `id_key`, which
        // only tracks insertion order for monotonic `_id`s; a capped collection
        // with custom non-monotonic `_id`s would then drop follow-up inserts.)
        // `None` for an empty collection.
        let after = storage_arc
            .collection_max_recordid(&ctx.db_name, &coll)
            .map_err(command_error)?;
        let bs = batch_size.max(0) as usize;
        let split = bs.min(docs.len());
        let first_batch = docs[..split].to_vec();
        let initial_remaining = docs[split..].to_vec();
        let producer = Box::new(TailableFindProducer {
            storage: storage_arc,
            db: ctx.db_name.clone(),
            coll: coll.clone(),
            after,
            fatal: None,
        });
        let cursor_id = cursors
            .register_tailable(
                &ns,
                producer,
                TailableOptions {
                    await_data,
                    initial_remaining,
                    ..Default::default()
                },
            )
            .map_err(|e| {
                CommandError::new(
                    1,
                    "InternalError",
                    format!("cursor registration failed: {e:?}"),
                )
            })?;
        return Ok(doc! {
            "cursor": {
                "firstBatch": docs_to_bson(first_batch)?,
                "id": Bson::Int64(cursor_id),
                "ns": ns,
            },
            "ok": 1.0,
        });
    }

    // `returnKey` replaces each result with just the key fields of the index
    // serving the query (the IXSCAN keyPattern, plus the sort fields), and
    // suppresses `showRecordId`. `showRecordId` alone tags each doc with a
    // synthetic `$recordId`. Both ignore `projection` (mongod does too), so we
    // rewrite the docs here and clear `projection`. Mirrors commands.py.
    let return_key = bool_field(doc, "returnKey", false);
    let show_record_id = bool_field(doc, "showRecordId", false);
    if return_key || show_record_id {
        let mut key_fields: Vec<String> = Vec::new();
        if return_key {
            if let Ok(plan) = storage.explain_plan(&ctx.db_name, &coll, &filter, sort, hint) {
                if plan.get_str("kind") == Ok("IXSCAN") {
                    if let Ok(kp) = plan.get_document("keyPattern") {
                        key_fields.extend(kp.keys().cloned());
                    }
                }
            }
            if let Some(s) = sort {
                for k in s.keys() {
                    if !key_fields.iter().any(|f| f == k) {
                        key_fields.push(k.clone());
                    }
                }
            }
        }
        let mut rewritten: Vec<Document> = Vec::with_capacity(docs.len());
        for (i, bytes) in docs.iter().enumerate() {
            let d = Document::from_reader(&mut bytes.as_slice())
                .map_err(|e| CommandError::new(1, "InternalError", e.to_string()))?;
            if return_key {
                let mut o = Document::new();
                for f in &key_fields {
                    if let Some(v) = d.get(f) {
                        o.insert(f.clone(), v.clone());
                    }
                }
                rewritten.push(o);
            } else {
                let mut o = d;
                o.insert("$recordId", Bson::Int64((i + 1) as i64));
                rewritten.push(o);
            }
        }
        docs = crate::util::encode_docs(rewritten)?;
        projection = None;
    }

    // Two reply shapes:
    //   * projection — decode+project every firstBatch doc into an owned
    //     `Bson::Document` (projection inherently materialises); embed directly.
    //   * no projection — the firstBatch stays as pre-encoded storage blobs; hand
    //     them to the server via `ctx.pending_batch` so it splices them onto the
    //     wire (`encode_cursor_reply`) with no decode→re-encode round-trip. The
    //     reply then carries only the cursor envelope (`{ cursor: { id, ns } }`).
    // In both cases only the cursor *remainder* (stored as bytes in the registry)
    // is ever re-encoded.
    match projection {
        Some(spec) => {
            let projected = project_to_docs(docs, spec, &filter)?;
            let (first_batch, cursor_id): (Vec<Bson>, i64) = if single_batch {
                (projected.into_iter().map(Bson::Document).collect(), 0)
            } else {
                split_docs_into_cursor(projected, batch_size, &ns, cursors)?
            };
            Ok(doc! {
                "cursor": {
                    "firstBatch": first_batch,
                    // Cursor `id` MUST be int64 — the Go driver hard-fails int32 here.
                    "id": Bson::Int64(cursor_id),
                    "ns": ns,
                },
                "ok": 1.0,
            })
        }
        None => {
            let (first, cursor_id) = if single_batch {
                (docs, 0)
            } else {
                split_into_cursor(docs, batch_size, &ns, cursors)?
            };
            ctx.pending_batch = Some(crate::PendingBatch {
                batch_field: "firstBatch",
                batch: first,
            });
            Ok(doc! {
                "cursor": {
                    "id": Bson::Int64(cursor_id),
                    "ns": ns,
                },
                "ok": 1.0,
            })
        }
    }
}

/// Split the ordered result into `firstBatch` + a registered cursor for the
/// remainder (`commands.py::_split_into_cursor`). `batchSize == 0` is a real
/// value: an empty `firstBatch` with a live cursor the client drains via
/// `getMore`. Returns `(first_batch, cursor_id)` with `cursor_id == 0` when
/// everything fit. Shared with the `aggregate` handler.
pub(crate) fn split_into_cursor(
    mut docs: Vec<Vec<u8>>,
    batch_size: i64,
    ns: &str,
    cursors: &CursorRegistry,
) -> Result<(Vec<Vec<u8>>, i64), CommandError> {
    let mut take = if batch_size < 0 {
        DEFAULT_BATCH_SIZE as usize
    } else {
        batch_size as usize
    }
    .min(docs.len());
    // Byte-budget the FIRST batch as well as getMore. mongod caps every reply at
    // 16MB and keeps the cursor open for the rest; capping on document count
    // alone made `find` with `batchSize: 25` over 1MB documents return a 25MB
    // reply with an exhausted cursor, where mongod returns 15MB and a live
    // cursor id (measured against 6.0.16). Blob lengths are already known here,
    // so this costs nothing. Always take at least one document, matching
    // `CursorRegistry::next_batch`, so an oversized document still makes
    // progress instead of returning an empty batch forever.
    let mut bytes = 0usize;
    let mut fitted = 0usize;
    for blob in docs.iter().take(take) {
        if fitted > 0 && bytes + blob.len() > MAX_GETMORE_BATCH_BYTES {
            break;
        }
        bytes += blob.len();
        fitted += 1;
    }
    take = fitted;
    let remaining = docs.split_off(take);
    if remaining.is_empty() {
        return Ok((docs, 0));
    }
    let cursor_id = cursors
        .register(ns, remaining)
        .map_err(|e| CommandError::new(1, "InternalError", format!("cursor registry: {e:?}")))?;
    Ok((docs, cursor_id))
}

/// Like [`split_into_cursor`], but for already-decoded result `Document`s (the
/// `aggregate` pipeline output, or a projected `find`). The `firstBatch` is
/// returned as `Bson` for direct embedding in the reply — never re-encoded —
/// while only the cursor remainder is encoded to bytes for the registry. Shared
/// with the `aggregate` handler.
pub(crate) fn split_docs_into_cursor(
    mut docs: Vec<Document>,
    batch_size: i64,
    ns: &str,
    cursors: &CursorRegistry,
) -> Result<(Vec<Bson>, i64), CommandError> {
    let take = if batch_size < 0 {
        DEFAULT_BATCH_SIZE as usize
    } else {
        batch_size as usize
    }
    .min(docs.len());
    let remaining = docs.split_off(take);
    let first: Vec<Bson> = docs.into_iter().map(Bson::Document).collect();
    if remaining.is_empty() {
        return Ok((first, 0));
    }
    let cursor_id = cursors
        .register(ns, encode_docs(remaining)?)
        .map_err(|e| CommandError::new(1, "InternalError", format!("cursor registry: {e:?}")))?;
    Ok((first, cursor_id))
}

/// Resolve a `hint` (index-name string or key-spec document) to the index's key
/// pattern (field → direction), for evaluating `min`/`max` bounds.
fn resolve_index_key(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    hint: &Bson,
) -> Result<Document, CommandError> {
    let indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    match hint {
        Bson::String(name) => indexes
            .iter()
            .find(|ix| ix.get_str("name").ok() == Some(name.as_str()))
            .and_then(|ix| ix.get_document("key").ok().cloned())
            .ok_or_else(|| {
                CommandError::new(
                    2,
                    "BadValue",
                    format!("hint {name} does not match an index"),
                )
            }),
        Bson::Document(kd) => indexes
            .iter()
            .find(|ix| ix.get_document("key").ok() == Some(kd))
            .map(|_| kd.clone())
            .ok_or_else(|| CommandError::new(2, "BadValue", "hint does not match an index")),
        _ => Err(CommandError::new(
            2,
            "BadValue",
            "min/max requires a valid hint",
        )),
    }
}

/// Compare `doc`'s key tuple against `bound` over the bound's fields, in the
/// index's per-field direction. `None` ⇒ the doc is missing a bound field (it
/// has no index key, so it's outside any bound). `Some(Ordering)` is the tuple
/// comparison (`Equal` when the doc matches the bound on every bound field).
fn bound_cmp(
    doc: &Document,
    bound: &Document,
    key_spec: &Document,
) -> Result<Option<std::cmp::Ordering>, CommandError> {
    use std::cmp::Ordering;
    for (field, bval) in bound {
        let dir = match key_spec.get(field) {
            Some(Bson::Int32(n)) => *n,
            Some(Bson::Int64(n)) => *n as i32,
            Some(Bson::Double(d)) => *d as i32,
            _ => 1,
        };
        let Some(dval) = secantus_core::get_path(doc, field) else {
            return Ok(None);
        };
        let enc = |v: &Bson| {
            secantus_core::sortkey::encode_value_directed(v, dir, None)
                .map_err(|_| CommandError::new(2, "BadValue", "min/max value not comparable"))
        };
        match enc(dval)?.cmp(&enc(bval)?) {
            Ordering::Equal => continue,
            other => return Ok(Some(other)),
        }
    }
    Ok(Some(Ordering::Equal))
}

/// Filter `docs` (already index-ordered) by the `min` (inclusive) / `max`
/// (exclusive) index bounds. Mirrors `storage._apply_minmax_bounds`.
fn apply_min_max(
    docs: Vec<Vec<u8>>,
    min: Option<&Document>,
    max: Option<&Document>,
    hint: Option<&Bson>,
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> Result<Vec<Vec<u8>>, CommandError> {
    use std::cmp::Ordering;
    let hint = hint.ok_or_else(|| {
        CommandError::new(
            51173,
            "Location51173",
            "When using min()/max() a hint of which index to use must be provided",
        )
    })?;
    let key_spec = resolve_index_key(storage, db, coll, hint)?;
    // Each bound's fields must be a leading prefix of the hinted index's key,
    // in the same order — else mongod rejects with 51174 (the "wrong order"
    // case in pymongo's test_min/test_max).
    let index_fields: Vec<&String> = key_spec.keys().collect();
    for bound in [min, max].into_iter().flatten() {
        let bf: Vec<&String> = bound.keys().collect();
        let prefix_match =
            bf.len() <= index_fields.len() && (0..bf.len()).all(|i| bf[i] == index_fields[i]);
        if !prefix_match {
            return Err(CommandError::new(
                51174,
                "Location51174",
                "The field order of the min/max query option does not match the order of the \
                 hinted index's key pattern",
            ));
        }
    }
    let mut out = Vec::with_capacity(docs.len());
    for bytes in docs {
        let d = Document::from_reader(&mut bytes.as_slice()).map_err(|e| {
            CommandError::new(
                1,
                "InternalError",
                format!("failed to decode document: {e}"),
            )
        })?;
        // min is inclusive (doc >= min ⇒ cmp != Less); max is exclusive (doc < max).
        let keep_min = match min {
            Some(m) => matches!(bound_cmp(&d, m, &key_spec)?, Some(o) if o != Ordering::Less),
            None => true,
        };
        let keep_max = match max {
            Some(m) => bound_cmp(&d, m, &key_spec)? == Some(Ordering::Less),
            None => true,
        };
        if keep_min && keep_max {
            out.push(bytes);
        }
    }
    Ok(out)
}

/// mongod's mixed-inclusion/exclusion projection error, if the spec mixes the
/// two modes. `_id` is exempt (it may be excluded in an inclusion projection and
/// included in an exclusion one). The first non-`_id` field sets the mode; the
/// first field that contradicts it loses, with mongod's per-field wording
/// (`Cannot do exclusion on field X in inclusion projection`, 31254 — or the
/// inclusion-in-exclusion mirror, 31253). Operator specs (`$slice` / `$elemMatch`)
/// are neutral here (handled by `apply_projection`). Mirrors mongod's parse-time
/// validation that mongo-node-driver's projection-error tests assert.
fn projection_mix_error(spec: &Document) -> Option<CommandError> {
    let field_inclusion = |v: &Bson| -> Option<bool> {
        match v {
            Bson::Int32(0) | Bson::Int64(0) | Bson::Boolean(false) => Some(false),
            Bson::Int32(_) | Bson::Int64(_) | Bson::Boolean(true) => Some(true),
            Bson::Double(d) => Some(*d != 0.0),
            _ => None, // operator spec ($slice / $elemMatch) — neutral
        }
    };
    let mut mode: Option<bool> = None;
    for (field, v) in spec {
        if field == "_id" {
            continue;
        }
        let Some(incl) = field_inclusion(v) else {
            continue;
        };
        match mode {
            None => mode = Some(incl),
            Some(m) if m != incl => {
                return Some(if m {
                    CommandError::new(
                        31254,
                        "Location31254",
                        format!("Cannot do exclusion on field {field} in inclusion projection"),
                    )
                } else {
                    CommandError::new(
                        31253,
                        "Location31253",
                        format!("Cannot do inclusion on field {field} in exclusion projection"),
                    )
                });
            }
            _ => {}
        }
    }
    None
}

/// Whether `query` carries a `$text` clause (top-level or nested inside a
/// `$and` / `$or` / `$nor` array). mongod requires a `$text` predicate before a
/// `{$meta: "textScore"}` projection is legal. Mirrors `projection._query_has_text`.
fn query_has_text(query: &Document) -> bool {
    for (key, val) in query {
        if key == "$text" {
            return true;
        }
        if matches!(key.as_str(), "$and" | "$or" | "$nor") {
            if let Bson::Array(arr) = val {
                if arr.iter().filter_map(Bson::as_document).any(query_has_text) {
                    return true;
                }
            }
        }
    }
    false
}

/// mongod's parse-time `$meta` projection errors, if the spec carries a faulty
/// `{$meta: ...}` value. Oracle-pinned against mongod 6.0: an unrecognized
/// argument is a Location17308 (`Unsupported argument to $meta: <arg>`), and
/// `{$meta: "textScore"}` without a `$text` query is a Location40218 (`query
/// requires text score metadata, but it is not available`). Recognized-but-
/// unsupported args (`indexKey` / `recordId` / `sortKey` / …) validate clean and
/// are omitted from the result by `secantus_core::projection::apply_projection`.
fn projection_meta_error(spec: &Document, filter: &Document) -> Option<CommandError> {
    for v in spec.values() {
        let Some(arg) = secantus_core::projection::meta_spec(v) else {
            continue;
        };
        if !secantus_core::projection::META_KEYWORDS.contains(&arg) {
            return Some(CommandError::new(
                17308,
                "Location17308",
                format!("Unsupported argument to $meta: {arg}"),
            ));
        }
        if arg == "textScore" && !query_has_text(filter) {
            return Some(CommandError::new(
                40218,
                "Location40218",
                "query requires text score metadata, but it is not available",
            ));
        }
    }
    None
}

/// Apply a projection spec to each result doc via `secantus_core::projection`.
/// A `Fallback` (a projection the Rust engine can't reproduce exactly) surfaces
/// as `BadValue` — the Rust server only ships what the Rust engine supports.
fn project_to_docs(
    docs: Vec<Vec<u8>>,
    spec: &Document,
    filter: &Document,
) -> Result<Vec<Document>, CommandError> {
    let query = if filter.is_empty() {
        None
    } else {
        Some(filter)
    };
    docs.iter()
        .map(|bytes| {
            // Fast path: a pure top-level inclusion spec projects straight off
            // the raw BSON, decoding only the included fields (not the whole
            // document). Anything else — exclusion, dotted, operators,
            // positional — falls back to the full decode + `apply_projection`.
            if let Ok(raw) = bson::RawDocument::from_bytes(bytes) {
                if let Some(projected) = secantus_core::projection::apply_projection_raw(raw, spec)
                {
                    return Ok(projected);
                }
            }
            let d = Document::from_reader(&mut bytes.as_slice()).map_err(|e| {
                CommandError::new(
                    1,
                    "InternalError",
                    format!("failed to decode document: {e}"),
                )
            })?;
            secantus_core::projection::apply_projection(&d, spec, query).map_err(|_| {
                CommandError::new(
                    2,
                    "BadValue",
                    "projection is not supported by the Rust server",
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod first_batch_byte_cap_tests {
    use super::split_into_cursor;
    use crate::cursors::CursorRegistry;
    use crate::MAX_GETMORE_BATCH_BYTES;

    /// A blob of `n` bytes standing in for an encoded document.
    fn blob(n: usize) -> Vec<u8> {
        vec![0u8; n]
    }

    #[test]
    fn first_batch_stops_under_the_reply_cap() {
        // 25 x 1MiB with batchSize 25: the count cap alone would return all 25
        // and exhaust the cursor. mongod 6.0.16, measured, returns 15 documents
        // (15.0 MiB) and a live cursor id.
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..25).map(|_| blob(1024 * 1024)).collect();
        let (first, cursor_id) = split_into_cursor(docs, 25, "t.big", &reg).unwrap();

        let bytes: usize = first.iter().map(|b| b.len()).sum();
        assert!(
            bytes <= MAX_GETMORE_BATCH_BYTES,
            "reply exceeded the 16MB cap"
        );
        assert!(
            first.len() < 25,
            "the count cap alone would have taken all 25"
        );
        assert_ne!(cursor_id, 0, "the remainder must stay behind a live cursor");
    }

    #[test]
    fn a_single_oversized_document_still_makes_progress() {
        // Never hand back an empty batch with documents pending — that hangs a
        // client. Matches CursorRegistry::next_batch's "at least one" rule.
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..2).map(|_| blob(12 * 1024 * 1024)).collect();
        let (first, cursor_id) = split_into_cursor(docs, 2, "t.huge", &reg).unwrap();
        assert_eq!(first.len(), 1);
        assert_ne!(cursor_id, 0);
    }

    #[test]
    fn small_documents_are_governed_by_the_count_cap() {
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..500).map(|_| blob(64)).collect();
        let (first, cursor_id) = split_into_cursor(docs, 101, "t.small", &reg).unwrap();
        assert_eq!(first.len(), 101, "the common path must not change");
        assert_ne!(cursor_id, 0);
    }

    #[test]
    fn everything_fitting_exhausts_the_cursor() {
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..5).map(|_| blob(64)).collect();
        let (first, cursor_id) = split_into_cursor(docs, 101, "t.small", &reg).unwrap();
        assert_eq!(first.len(), 5);
        assert_eq!(cursor_id, 0);
    }
}
