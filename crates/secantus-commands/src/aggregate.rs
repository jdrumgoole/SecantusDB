//! The `aggregate` command — runs an aggregation pipeline and opens a cursor.
//!
//! A port of `commands.py::_aggregate`'s storage-independent path. The Rust
//! pipeline engine (`secantus_core::aggregate::apply_pipeline`) covers the
//! storage-free stages (`$match` / `$project` / `$group` / `$sort` / `$unwind` /
//! `$bucket` / `$facet` / `$densify` / `$count` / …). The flow:
//!
//! 1. Fetch the input documents from storage (lifting a leading `$match` into
//!    the fetch filter so the index planner can use it, and dropping that stage
//!    from the pipeline — exactly as `_aggregate` does).
//! 2. Run the pipeline via [`run_segmented`], which interleaves the storage-free
//!    core engine with the storage-backed stages handled here.
//! 3. Split the result into `firstBatch` + a cursor (`cursor.batchSize`).
//!
//! **Storage-backed stages handled at this layer** (the core engine is
//! storage-free): `$lookup` (simple + `let`/`pipeline` forms), `$graphLookup`
//! (recursive BFS over a foreign collection — `maxDepth` / `depthField` /
//! `restrictSearchWithMatch`), `$sample`, `$collStats`, `$indexStats`,
//! `$unionWith`, `$out`, `$merge` (including the pipeline-form `whenMatched` with
//! `$$new` bound, and non-`_id` `on`-field unique-index validation). A
//! `run_segmented` flushes the buffered run of storage-free stages through
//! `apply_pipeline`, then applies the storage-backed stage, repeating — and
//! `$facet` sub-pipelines run through `run_segmented` too, so a storage-backed
//! stage (`$lookup` / `$graphLookup`) nested inside a `$facet` works.
//!
//! **Source stages** (`$currentOp` / `$listLocalSessions` / `$listSessions`)
//! also run here: they ignore their input and emit a single synthetic "op" row
//! (port of `aggregate._stage_current_op`), since SecantusDB runs commands
//! synchronously and keeps no per-op registry. They're what makes a
//! database-level `aggregate: 1` pipeline (which has no source collection) work.
//!
//! `$geoNear` runs here too (distance from each doc's `key` to `near` via
//! `secantus_core::geo::point_distance`, min/max filter, ascending sort,
//! `distanceField`/`includeLocs` attach; `key` is explicit or inferred from the
//! lone geo index). A leading **bounded** `$geoNear` (with a `maxDistance` and a
//! matching `2d`/`2dsphere` index) rides that index via a conservative
//! `$geoWithin` candidate fetch (`geo_near_index_filter`) instead of a full
//! COLLSCAN — the stage is kept and re-applies the exact distance filter, so the
//! output is identical; only the fetched set shrinks. Mirrors the Python lift.
//!
//! `$changeStream` is handled separately (`changestream::open_change_stream`);
//! the standalone-rejection (40573) is honoured here.
//!
//! `$lookup`'s simple form drives a per-outer-doc index probe (`Storage::find`)
//! when the foreign collection has a leading-field index on `foreignField` —
//! matching the Python server's result and `as`-array order (index order) — and
//! otherwise materialises the foreign collection and hash-joins (also the
//! `let`/`pipeline` form).
//!
//! **`collation`** is threaded through `$match` / `$sort` (the storage-free core)
//! and the lifted-`$match` fetch (COLLSCAN-forced); a collation the engine can't
//! reproduce (non-ASCII / numericOrdering) surfaces as `BadValue`.

use bson::{doc, Bson, Document};

use crate::find::split_docs_into_cursor;
use crate::util::{
    as_i64, bool_field, collation_of, command_error, decode_docs, decode_docs_minimal, encode_docs,
    resolve_let_vars,
};
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE};
use secantus_core::collation::Collation;

/// `aggregate` — run a pipeline and return a cursor over the results.
pub fn aggregate(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // `aggregate: <coll>` (string) or `aggregate: 1` (collectionless).
    let coll = match doc.get("aggregate") {
        Some(Bson::String(s)) => Some(s.clone()),
        _ => None,
    };
    let pipeline: Vec<Bson> = match doc.get("pipeline") {
        Some(Bson::Array(a)) => a.clone(),
        _ => Vec::new(),
    };

    // Validate stage names up-front (mongod parses before running): an
    // unrecognised stage is Location40324, an Atlas-only stage is
    // CommandNotSupported (115) — not the generic "unsupported" BadValue.
    if let Err(e) = validate_stage_names(&pipeline) {
        return Ok(e.into_reply());
    }
    // A genuinely unknown expression operator inside a `$project` spec is
    // mongod's stage-specific Location31325, not the generic BadValue the
    // engine fallback produces. Parse-time, like the stage-name check.
    if let Err(e) = validate_project_exprs(&pipeline) {
        return Ok(e.into_reply());
    }

    // Inline `explain: true` on the aggregate command (the legacy flag, distinct
    // from the top-level `explain` wrapper): return the plan instead of running
    // the pipeline, and crucially do NOT execute `$out` / `$merge`. Delegate to
    // the explain handler before any write stage runs; default verbosity is
    // `queryPlanner`. Mirrors `commands.py::_aggregate`.
    if doc.get("explain") == Some(&Bson::Boolean(true)) {
        let mut inner = doc.clone();
        inner.remove("explain");
        let verbosity = doc
            .get_str("verbosity")
            .unwrap_or("queryPlanner")
            .to_string();
        let wrapped = doc! { "explain": inner, "verbosity": verbosity };
        return crate::admin::explain(&wrapped, ctx);
    }

    // `$out` / `$merge` may only be the final pipeline stage — mongod rejects a
    // non-terminal write stage with Location40601 before executing anything
    // (mongo-cxx-driver "out fails when not last").
    if !pipeline.is_empty() {
        let last = pipeline.len() - 1;
        for (i, s) in pipeline.iter().enumerate() {
            let name = stage_name(s);
            if (name == "$out" || name == "$merge") && i != last {
                return Ok(CommandError::new(
                    40601,
                    "Location40601",
                    format!("{name} can only be the final stage in the pipeline"),
                )
                .into_reply());
            }
        }
    }

    // `$out` / `$merge` are incompatible with readConcern level "linearizable"
    // (the only level mongod rejects here — local / available / majority all
    // run an output-stage aggregate fine). InvalidOptions (72).
    if let Some(level) = doc
        .get("readConcern")
        .and_then(Bson::as_document)
        .and_then(|rc| rc.get_str("level").ok())
    {
        let has_output_stage = pipeline
            .iter()
            .any(|s| matches!(stage_name(s), "$out" | "$merge"));
        if level == "linearizable" && has_output_stage {
            return Ok(CommandError::new(
                72,
                "InvalidOptions",
                "Aggregation stage $out/$merge cannot run with a readConcern \
                 level of 'linearizable'",
            )
            .into_reply());
        }
    }

    // $changeStream gate (must come before storage access). On a standalone
    // (no replica-set name) mongod rejects with IllegalOperation (40573).
    if first_stage_has(&pipeline, "$changeStream") {
        if ctx.replica_set_name.is_none() {
            return Ok(CommandError::new(
                40573,
                "IllegalOperation",
                "The $changeStream stage is only supported on replica sets",
            )
            .into_reply());
        }
        return crate::changestream::open_change_stream(doc, ctx);
    }

    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let hint = doc.get("hint");
    let batch_size = doc
        .get("cursor")
        .and_then(Bson::as_document)
        .and_then(|c| c.get("batchSize"))
        .and_then(as_i64)
        .unwrap_or(DEFAULT_BATCH_SIZE as i64);
    // Command `let` → resolved query vars ($$NOW seeded, values evaluated),
    // visible to `$expr` in `$match` and to pipeline expressions.
    let vars = resolve_let_vars(doc.get("let"));
    let collation = collation_of(doc);

    // Fetch the input documents + decide the remaining pipeline.
    let (ns, input, working_pipeline): (String, Vec<Document>, Vec<Bson>) = match &coll {
        Some(c) => {
            let ns = format!("{}.{}", ctx.db_name, c);
            // Resolve a view to its base collection + prepended view pipeline
            // (recursively for a view-on-a-view). The reply ns keeps the queried
            // (view) name; only the fetch reads the base collection. Mirrors
            // commands._resolve_view.
            let (base_coll, pipeline) = resolve_view(storage, &ctx.db_name, c, pipeline);
            let c = base_coll.as_str();
            // Stages that generate / read their own input start from no docs.
            if first_stage_has_any(&pipeline, &["$collStats", "$indexStats", "$documents"]) {
                (ns, Vec::new(), pipeline)
            } else {
                // Lift a leading $match into the fetch filter (dropping it from
                // the pipeline). A leading bounded $geoNear instead rides its geo
                // index via a conservative $geoWithin candidate fetch — the stage
                // is kept (it re-applies the exact distance filter, so the output
                // is identical; only the fetched set shrinks). Mirrors the Python
                // `_geo_near_index_filter` lift.
                let geonear_candidate = pipeline
                    .first()
                    .and_then(Bson::as_document)
                    .and_then(|d| d.get("$geoNear"))
                    .and_then(|spec| geo_near_index_filter(spec, &ctx.db_name, Some(c), storage));
                let (filter, rest) = match geonear_candidate {
                    Some(cand) => (cand, pipeline),
                    None => lift_leading_match(&pipeline),
                };
                let bytes = storage
                    .find_collated(
                        &ctx.db_name,
                        c,
                        &filter,
                        None,
                        hint,
                        collation.as_ref(),
                        &vars,
                    )
                    .map_err(command_error)?;
                // Reduce the leading $skip/$limit/$match prefix over raw BSON so
                // only the survivors that reach the first heavier stage are
                // decoded (tasks/rust-perf-findings.md).
                let (reduced, remaining) =
                    reduce_raw_prefix(bytes, &rest, &vars, collation.as_ref());
                // If the first heavier stage is a `$group`, decode only the
                // top-level fields its `_id` + accumulators read from each
                // survivor, not the whole (often wide) document
                // (tasks/rust-phase6-streaming-agg-scoping.md, 6a). Any group
                // shape the collector can't bound falls back to a full decode.
                let group_fields = remaining
                    .first()
                    .and_then(Bson::as_document)
                    .filter(|d| d.len() == 1)
                    .and_then(|d| d.get("$group"))
                    .and_then(secantus_core::referenced_top_level_fields);
                let input = match group_fields {
                    Some(fields) => decode_docs_minimal(reduced, &fields)?,
                    None => decode_docs(reduced)?,
                };
                (ns, input, remaining)
            }
        }
        // Collectionless aggregate (e.g. `{aggregate: 1, pipeline: [{$documents: …}]}`).
        None => (
            format!("{}.$cmd.aggregate", ctx.db_name),
            Vec::new(),
            pipeline,
        ),
    };

    // The connection's handshake `client` doc, for `$currentOp`'s appName /
    // clientMetadata (read before the borrow of `ctx` below).
    let client_metadata: Option<Document> = ctx
        .conn_auth
        .as_ref()
        .and_then(|a| a.lock().ok())
        .and_then(|g| g.client_metadata.clone());
    let result = run_segmented(
        input,
        &working_pipeline,
        &ctx.db_name,
        coll.as_deref(),
        &vars,
        storage,
        collation.as_ref(),
        doc,
        client_metadata.as_ref(),
    )?;

    // The pipeline result is already decoded `Document`s. Send the `firstBatch`
    // straight to the wire as `Bson` and encode only the cursor remainder for the
    // registry — no encode→decode round-trip on the docs the client gets now.
    let (first_batch, cursor_id) = split_docs_into_cursor(result, batch_size, &ns, cursors)?;
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

/// Stages handled at the command layer (they read/write storage or need a
/// brute-force scan), rather than by the storage-free `secantus_core` pipeline
/// engine. Everything else flows through `apply_pipeline`.
fn is_storage_backed(name: &str) -> bool {
    matches!(
        name,
        "$lookup"
            | "$graphLookup"
            | "$facet"
            | "$sample"
            | "$collStats"
            | "$indexStats"
            | "$out"
            | "$merge"
            | "$geoNear"
            | "$unionWith"
    )
}

/// Source stages that ignore their input and emit a synthetic row, mirroring
/// `aggregate._stage_current_op` (reused for `$listLocalSessions` /
/// `$listSessions`). They need the command/db/coll context, so they're handled
/// at this layer rather than in the storage-free core engine.
fn is_source_stage(name: &str) -> bool {
    matches!(name, "$currentOp" | "$listLocalSessions" | "$listSessions")
}

/// `$currentOp` / `$listLocalSessions` / `$listSessions` — emit one synthetic
/// "op" document (SecantusDB runs commands synchronously and keeps no per-op
/// registry). Port of `aggregate._stage_current_op`: the `command` field echoes
/// the actual aggregate request (with `$db` / `cursor` defaulted) so callers
/// that introspect it see a faithful row; the `ns` / `host` / `op` fields match
/// the Python stub.
fn apply_source_stage(
    _name: &str,
    db: &str,
    coll: Option<&str>,
    cmd_doc: &Document,
    client_metadata: Option<&Document>,
) -> Vec<Document> {
    let command_doc = if cmd_doc.contains_key("aggregate") {
        let mut c = cmd_doc.clone();
        if !c.contains_key("$db") {
            c.insert("$db", db.to_string());
        }
        if !c.contains_key("cursor") {
            c.insert("cursor", Document::new());
        }
        c
    } else {
        doc! { "aggregate": 1 }
    };
    let ns = format!("{}.{}", db, coll.unwrap_or("$cmd.aggregate"));
    let mut row = doc! {
        "type": "op",
        "host": "secantus",
        "desc": "$currentOp",
        "active": false,
        "currentOpTime": "",
        "command": command_doc,
        "ns": ns,
        "op": "command",
    };
    // Surface the connection's driver handshake metadata the way mongod's
    // `$currentOp` does: the whole `clientMetadata` document plus a top-level
    // `appName` lifted out of `application.name`. mongocxx's "client metadata
    // handshake feature" test connects with `?appName=…`, scans
    // `db.aggregate([{$currentOp: {}}])` for a row whose `appName` matches, and
    // only then checks `clientMetadata.{application,driver,os}` — with neither
    // field present its scan matched nothing and the test failed on a missing
    // op rather than on any of the metadata it meant to verify. Mirrors
    // `aggregate._stage_current_op`.
    if let Some(meta) = client_metadata {
        if let Some(name) = meta
            .get_document("application")
            .ok()
            .and_then(|a| a.get_str("name").ok())
        {
            row.insert("appName", name.to_string());
        }
        row.insert("clientMetadata", meta.clone());
    }
    vec![row]
}

/// Atlas-only aggregation stages SecantusDB can't provide — rejected with the
/// Atlas message (`CommandNotSupported`, 115) rather than the generic
/// unrecognized-stage error. Mirrors `aggregate._ATLAS_ONLY_STAGES`.
const ATLAS_STAGES: &[&str] = &[
    "$listSearchIndexes",
    "$search",
    "$searchMeta",
    "$vectorSearch",
];

pub const SEARCH_INDEX_ATLAS_MSG: &str = "Using Atlas Search Database Commands and the \
$listSearchIndexes aggregation stage requires additional configuration. Please connect to Atlas \
or an Atlas-compatible deployment to use this feature.";

/// Whether `name` is a recognised aggregation stage (the set the Python
/// `aggregate._STAGES` registry recognises). Keep in sync with that set.
fn recognized_stage(name: &str) -> bool {
    matches!(
        name,
        "$addFields"
            | "$bucket"
            | "$bucketAuto"
            | "$changeStream"
            | "$changeStreamSplitLargeEvent"
            | "$collStats"
            | "$count"
            | "$currentOp"
            | "$densify"
            | "$documents"
            | "$facet"
            | "$fill"
            | "$geoNear"
            | "$graphLookup"
            | "$group"
            | "$indexStats"
            | "$limit"
            | "$listLocalSessions"
            | "$listSessions"
            | "$lookup"
            | "$match"
            | "$merge"
            | "$out"
            | "$project"
            | "$redact"
            | "$replaceRoot"
            | "$replaceWith"
            | "$sample"
            | "$set"
            | "$setWindowFields"
            | "$skip"
            | "$sort"
            | "$sortByCount"
            | "$unionWith"
            | "$unset"
            | "$unwind"
    )
}

/// Up-front pipeline stage-name validation (mongod validates at parse time,
/// before any document flows). An Atlas-only stage → `CommandNotSupported` (115);
/// an unrecognised single-key stage → `Location40324`. Malformed stage shapes
/// (non-document / empty / multi-key) are left to the engine path. Mirrors
/// `aggregate.validate_stage_names`.
/// Parse-time check for a genuinely unknown expression operator inside a
/// `$project` stage — mongod reports `Location31325` ("Invalid $project ::
/// caused by :: Unknown expression $op") where the engine-fallback path would
/// give a generic `2 BadValue`. Mirrors `aggregate._stage_project`'s
/// `UnknownExpressionOperatorError` wrap on the Python server.
///
/// Only values that would route to the expression evaluator are scanned: a
/// single-key document whose key is a projection-only operator (`$slice` /
/// `$elemMatch` / `$meta` — valid inside `$project`, NOT expression operators)
/// is skipped, so it is never mislabeled. `first_unknown_expr_operator`
/// recurses through nested documents/arrays and flags only a truly-unknown
/// `$`-operator — a recognised-but-deferred operator still defers to Python.
fn validate_project_exprs(pipeline: &[Bson]) -> Result<(), CommandError> {
    const PROJECTION_ONLY_OPS: [&str; 3] = ["$slice", "$elemMatch", "$meta"];
    for stage in pipeline {
        let Some(spec) = stage
            .as_document()
            .and_then(|d| d.get("$project"))
            .and_then(Bson::as_document)
        else {
            continue;
        };
        for (_field, value) in spec.iter() {
            if let Bson::Document(d) = value {
                if d.len() == 1 {
                    let key = d.keys().next().map(String::as_str).unwrap_or_default();
                    if PROJECTION_ONLY_OPS.contains(&key) {
                        continue;
                    }
                }
            }
            if let Some(op) = secantus_core::expressions::first_unknown_expr_operator(value) {
                return Err(CommandError::new(
                    31325,
                    "Location31325",
                    format!("Invalid $project :: caused by :: Unknown expression {op}"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_stage_names(pipeline: &[Bson]) -> Result<(), CommandError> {
    for stage in pipeline {
        let Some(d) = stage.as_document() else {
            continue;
        };
        if d.len() != 1 {
            continue;
        }
        let name = stage_name(stage);
        if ATLAS_STAGES.contains(&name) {
            return Err(CommandError::new(
                115,
                "CommandNotSupported",
                SEARCH_INDEX_ATLAS_MSG,
            ));
        }
        if !recognized_stage(name) {
            return Err(CommandError::new(
                40324,
                "Location40324",
                format!("Unrecognized pipeline stage name: '{name}'"),
            ));
        }
    }
    Ok(())
}

/// The stage operator name (the single key of a stage document), or `""`.
fn stage_name(stage: &Bson) -> &str {
    stage
        .as_document()
        .and_then(|d| d.keys().next())
        .map(String::as_str)
        .unwrap_or("")
}

/// Run a pipeline that may interleave storage-free stages (handled in one shot by
/// `secantus_core::apply_pipeline`) with storage-backed stages (`$lookup` etc.,
/// handled here). Consecutive storage-free stages are buffered and flushed
/// through the core engine; each storage-backed stage is applied in between. This
/// is the Rust analogue of `commands.py`'s `_aggregate`, where every stage —
/// storage-free or not — dispatches through the same `apply_pipeline`.
#[allow(clippy::too_many_arguments)]
fn run_segmented(
    input: Vec<Document>,
    pipeline: &[Bson],
    db: &str,
    coll: Option<&str>,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
    collation: Option<&Collation>,
    cmd_doc: &Document,
    client_metadata: Option<&Document>,
) -> Result<Vec<Document>, CommandError> {
    let mut docs = input;
    let mut buffer: Vec<Bson> = Vec::new();
    for stage in pipeline {
        let name = stage_name(stage);
        if name == "$documents" {
            // `$documents: [<expr>, …]` is a source stage: it ignores its input
            // and emits the (evaluated) array as the new document stream. mongod
            // only allows it first in a collectionless aggregate; run any buffered
            // stages first so ordering errors still surface in order.
            if !buffer.is_empty() {
                let _ = core_run(docs, &buffer, vars, collation)?;
                buffer.clear();
            }
            let spec = stage.as_document().and_then(|d| d.get(name));
            docs = documents_stage(spec, vars)?;
        } else if is_source_stage(name) {
            // Source stages (e.g. `$currentOp` / `$listLocalSessions`) ignore the
            // input and emit a synthetic row. Run (and discard) any buffered
            // stages first so a malformed earlier stage still errors in order,
            // matching Python; the source stage replaces the docs regardless.
            if !buffer.is_empty() {
                let _ = core_run(docs, &buffer, vars, collation)?;
                buffer.clear();
            }
            docs = apply_source_stage(name, db, coll, cmd_doc, client_metadata);
        } else if is_storage_backed(name) {
            if !buffer.is_empty() {
                docs = core_run(docs, &buffer, vars, collation)?;
                buffer.clear();
            }
            let spec = stage.as_document().and_then(|d| d.get(name)).cloned();
            let bypass = bool_field(cmd_doc, "bypassDocumentValidation", false);
            docs = apply_storage_stage(
                name,
                spec.as_ref(),
                docs,
                db,
                coll,
                vars,
                storage,
                collation,
                bypass,
            )?;
        } else {
            buffer.push(stage.clone());
        }
    }
    if !buffer.is_empty() {
        docs = core_run(docs, &buffer, vars, collation)?;
    }
    Ok(docs)
}

/// `$documents: [<expr>, …]` — evaluate each array element (against an empty
/// root) into a document. The array itself may be an expression (e.g. a `$$var`)
/// that resolves to an array of documents.
fn documents_stage(spec: Option<&Bson>, vars: &Document) -> Result<Vec<Document>, CommandError> {
    let spec = spec.ok_or_else(|| bad_value("$documents requires an array"))?;
    let empty = Document::new();
    let value = secantus_core::expressions::evaluate(&empty, spec, vars)
        .map_err(|_| bad_value("$documents expression not supported"))?;
    let arr = match value {
        Bson::Array(a) => a,
        _ => return Err(bad_value("$documents argument must evaluate to an array")),
    };
    let mut out = Vec::with_capacity(arr.len());
    for elem in arr {
        match elem {
            Bson::Document(d) => out.push(d),
            _ => return Err(bad_value("each $documents element must be an object")),
        }
    }
    Ok(out)
}

/// Run a run of storage-free stages through the core engine, mapping the engine's
/// `Fallback` to the `BadValue` the client sees for unsupported constructs.
/// `collation` applies to `$match` string comparison + `$sort` order.
fn core_run(
    docs: Vec<Document>,
    stages: &[Bson],
    vars: &Document,
    collation: Option<&Collation>,
) -> Result<Vec<Document>, CommandError> {
    secantus_core::aggregate::apply_pipeline(docs, stages, vars, collation).map_err(|_| {
        CommandError::new(
            2,
            "BadValue",
            "aggregation pipeline uses a stage or operator not supported by the Rust server",
        )
    })
}

fn bad_value(msg: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", msg)
}

/// Dispatch one storage-backed stage.
#[allow(clippy::too_many_arguments)]
fn apply_storage_stage(
    name: &str,
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    coll: Option<&str>,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
    collation: Option<&Collation>,
    bypass_validation: bool,
) -> Result<Vec<Document>, CommandError> {
    match name {
        "$lookup" => apply_lookup(spec, docs, db, vars, storage, collation),
        "$graphLookup" => apply_graph_lookup(spec, docs, db, vars, storage),
        "$facet" => apply_facet(spec, docs, db, coll, vars, storage, collation),
        "$sample" => apply_sample(spec, docs),
        "$collStats" => apply_coll_stats(spec, db, coll, storage),
        "$indexStats" => apply_index_stats(db, coll, storage),
        "$out" => apply_out(spec, docs, db, storage, bypass_validation),
        "$merge" => apply_merge(spec, docs, db, storage, bypass_validation),
        "$geoNear" => apply_geo_near(spec, docs, db, coll, vars, storage),
        "$unionWith" => apply_union_with(spec, docs, db, storage, collation),
        _ => Err(bad_value(format!(
            "unsupported storage-backed stage {name}"
        ))),
    }
}

/// `$lookup` — join a foreign collection in. Mirrors `aggregate._stage_lookup`:
/// simple `localField`/`foreignField` equality join (array-aware), or the
/// `let` + `pipeline` form (each outer doc binds `let` vars, then runs the
/// sub-pipeline over the candidate foreign docs). The simple form drives a
/// per-outer-doc index probe (`Storage::find`) when the foreign collection has a
/// leading-field index on `foreignField` — matching the Python server's result
/// *and order* — and otherwise materialises the foreign collection and hash-joins
/// (also the pipeline-form path).
fn apply_lookup(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
    collation: Option<&Collation>,
) -> Result<Vec<Document>, CommandError> {
    let spec = spec
        .and_then(Bson::as_document)
        .ok_or_else(|| bad_value("$lookup requires a document spec"))?;
    let from = spec
        .get_str("from")
        .map_err(|_| bad_value("$lookup requires 'from' (string)"))?;
    let as_field = spec
        .get_str("as")
        .map_err(|_| bad_value("$lookup requires 'as' (string)"))?;
    let local_field = spec.get_str("localField").ok();
    let foreign_field = spec.get_str("foreignField").ok();
    let sub_pipeline = spec.get("pipeline").and_then(Bson::as_array);
    let let_spec = spec.get("let").and_then(Bson::as_document);

    // Index-driven simple form: when there's no sub-pipeline, both fields are
    // present, and the foreign collection has a leading-field index on
    // `foreignField`, probe that index per outer doc via `Storage::find` (the
    // picker lands it as an IXSCAN) instead of materialising the whole foreign
    // collection and hash-joining. This mirrors the Python `_stage_lookup` path —
    // and, crucially, makes the `as` array order match the Python server's (index
    // order, not foreign-scan order), fixing a two-server divergence.
    if sub_pipeline.is_none() {
        if let (Some(lf), Some(ff)) = (local_field, foreign_field) {
            if foreign_field_has_leading_index(storage, db, from, ff)? {
                let mut out = Vec::with_capacity(docs.len());
                for doc in docs {
                    let lv = secantus_core::get_path(&doc, lf).cloned();
                    let joined = index_join_lookup(storage, db, from, ff, lv.as_ref())?;
                    let mut new = doc;
                    set_field_path(
                        &mut new,
                        as_field,
                        Bson::Array(joined.into_iter().map(Bson::Document).collect()),
                    );
                    out.push(new);
                }
                return Ok(out);
            }
        }
    }

    // Materialise the foreign collection once (the whole join's candidate pool).
    let foreign: Vec<Document> = decode_docs(
        storage
            .find(db, from, &Document::new(), None, None)
            .map_err(command_error)?,
    )?;

    let mut out = Vec::with_capacity(docs.len());
    for doc in docs {
        let joined: Vec<Document> = if let Some(sub) = sub_pipeline {
            // Pipeline form: bind `let` vars against the outer doc, pick the
            // candidate foreign docs (pre-filtered by localField/foreignField if
            // both are present, else the whole collection), then run the inner
            // pipeline over them.
            let mut sub_vars = vars.clone();
            if let Some(ls) = let_spec {
                for (k, expr) in ls.iter() {
                    let v = secantus_core::expressions::evaluate(&doc, expr, vars)
                        .map_err(|_| bad_value("$lookup let expression not supported"))?;
                    sub_vars.insert(k.clone(), v);
                }
            }
            let candidates: Vec<Document> = match (local_field, foreign_field) {
                (Some(lf), Some(ff)) => {
                    let lv = secantus_core::get_path(&doc, lf).cloned();
                    foreign
                        .iter()
                        .filter(|fd| lookup_match(lv.as_ref(), secantus_core::get_path(fd, ff)))
                        .cloned()
                        .collect()
                }
                _ => foreign.clone(),
            };
            run_segmented(
                candidates,
                sub,
                db,
                Some(from),
                &sub_vars,
                storage,
                collation,
                &Document::new(),
                None,
            )?
        } else {
            // Simple form: localField == foreignField (array-aware).
            let lf = local_field
                .ok_or_else(|| bad_value("$lookup requires localField+foreignField or pipeline"))?;
            let ff = foreign_field
                .ok_or_else(|| bad_value("$lookup requires localField+foreignField or pipeline"))?;
            let lv = secantus_core::get_path(&doc, lf).cloned();
            foreign
                .iter()
                .filter(|fd| lookup_match(lv.as_ref(), secantus_core::get_path(fd, ff)))
                .cloned()
                .collect()
        };
        let mut new = doc;
        let arr = Bson::Array(joined.into_iter().map(Bson::Document).collect());
        set_field_path(&mut new, as_field, arr);
        out.push(new);
    }
    Ok(out)
}

/// Whether the foreign collection has an index whose *leading* column is `field`
/// and all columns are ASC/DESC (`1`/`-1`) — the shape `$lookup` can drive through
/// `Storage::find` (single-field, compound-prefix, or multikey all light up at
/// IXSCAN). Geo / hashed / text indexes are excluded (their direction values are
/// strings). Mirrors `aggregate._foreign_field_has_simple_index`.
fn foreign_field_has_leading_index(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    field: &str,
) -> Result<bool, CommandError> {
    let is_dir = |v: &Bson| matches!(v, Bson::Int32(1 | -1) | Bson::Int64(1 | -1));
    let indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    for ix in &indexes {
        if let Ok(key) = ix.get_document("key") {
            let mut it = key.iter();
            if let Some((first, _)) = it.next() {
                if first == field && key.values().all(is_dir) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// The foreign docs whose `foreign_field` matches `local_value`, via
/// `Storage::find` (so the index picker decides IXSCAN vs COLLSCAN and returns
/// them in index order — matching the Python `_index_join_lookup`). An array
/// local value uses `$in` (empty array → no matches, short-circuited).
fn index_join_lookup(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    foreign_field: &str,
    local_value: Option<&Bson>,
) -> Result<Vec<Document>, CommandError> {
    let filter = match local_value {
        Some(Bson::Array(a)) => {
            if a.is_empty() {
                return Ok(Vec::new());
            }
            doc! { foreign_field: { "$in": a.clone() } }
        }
        other => doc! { foreign_field: other.cloned().unwrap_or(Bson::Null) },
    };
    decode_docs(
        storage
            .find(db, coll, &filter, None, None)
            .map_err(command_error)?,
    )
}

/// `$graphLookup` — recursive graph traversal of a foreign collection. Mirrors
/// `aggregate._stage_graph_lookup`: for each input doc, evaluate `startWith`,
/// then breadth-first walk the foreign collection matching `connectFromField`
/// (the value to chase) against `connectToField`, collecting matched docs into
/// `as`. `maxDepth` (default 100, matching mongod) bounds recursion; `depthField`
/// (when set) records the BFS depth as a `NumberLong`; `restrictSearchWithMatch`
/// (a query filter) limits which docs are traversed. We materialise the foreign
/// collection and walk it in Rust — correctness-identical to the Python path.
fn apply_graph_lookup(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
) -> Result<Vec<Document>, CommandError> {
    let spec = spec
        .and_then(Bson::as_document)
        .ok_or_else(|| bad_value("$graphLookup requires a document spec"))?;
    let strs_err =
        || bad_value("$graphLookup requires from/connectFromField/connectToField/as as strings");
    let from = spec.get_str("from").map_err(|_| strs_err())?;
    let connect_from = spec.get_str("connectFromField").map_err(|_| strs_err())?;
    let connect_to = spec.get_str("connectToField").map_err(|_| strs_err())?;
    let as_field = spec.get_str("as").map_err(|_| strs_err())?;
    let start_with = spec
        .get("startWith")
        .ok_or_else(|| bad_value("$graphLookup requires 'startWith'"))?;
    // Default maxDepth=100 (mongod's behaviour) so a self-referencing collection
    // can't blow the frontier up to O(N^2).
    let max_depth: i64 = match spec.get("maxDepth") {
        None => 100,
        Some(b) => as_i64(b).ok_or_else(|| bad_value("$graphLookup maxDepth must be numeric"))?,
    };
    let depth_field = spec.get_str("depthField").ok();
    let restrict = spec
        .get("restrictSearchWithMatch")
        .and_then(Bson::as_document);

    let foreign: Vec<Document> = decode_docs(
        storage
            .find(db, from, &Document::new(), None, None)
            .map_err(command_error)?,
    )?;

    let mut out = Vec::with_capacity(docs.len());
    for doc in docs {
        let seed = secantus_core::expressions::evaluate(&doc, start_with, vars)
            .map_err(|_| bad_value("$graphLookup startWith expression not supported"))?;
        let walked = graph_walk(
            &foreign,
            &seed,
            connect_from,
            connect_to,
            max_depth,
            depth_field,
            restrict,
        )?;
        let mut new = doc;
        set_field_path(
            &mut new,
            as_field,
            Bson::Array(walked.into_iter().map(Bson::Document).collect()),
        );
        out.push(new);
    }
    Ok(out)
}

/// `$facet` at the command layer: run each named sub-pipeline through
/// `run_segmented` (not the storage-free core engine) so storage-backed stages
/// like `$lookup` / `$graphLookup` work inside a facet. Each sub-pipeline sees a
/// copy of the same input docs; results collect into one output doc. Mirrors the
/// Python pipeline, where every facet sub-pipeline dispatches through the same
/// `apply_pipeline` as the top level.
#[allow(clippy::too_many_arguments)]
fn apply_facet(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    coll: Option<&str>,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
    collation: Option<&Collation>,
) -> Result<Vec<Document>, CommandError> {
    // mongod validates the spec before running: a non-empty object (40169), each
    // value an array (40170), each stage a non-empty object (40171), and no nested
    // $facet (40600).
    let s = spec
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty())
        .ok_or_else(|| {
            CommandError::new(
                40169,
                "Location40169",
                "the $facet specification must be a non-empty object",
            )
        })?;
    let mut out = Document::new();
    for (name, sub) in s.iter() {
        let sub_pipeline = sub.as_array().ok_or_else(|| {
            CommandError::new(
                40170,
                "Location40170",
                format!("arguments to $facet must be arrays, {name} is not an array"),
            )
        })?;
        for stage in sub_pipeline {
            match stage {
                Bson::Document(d) if d.contains_key("$facet") => {
                    return Err(CommandError::new(
                        40600,
                        "Location40600",
                        "$facet is not allowed to be used within a $facet stage",
                    ));
                }
                Bson::Document(d) if !d.is_empty() => {}
                _ => {
                    return Err(CommandError::new(
                        40171,
                        "Location40171",
                        format!(
                            "elements of arrays in $facet spec must be non-empty objects, \
                             {name} argument contained an invalid element"
                        ),
                    ));
                }
            }
        }
        // Each sub-pipeline runs over its own copy of the input docs.
        let res = run_segmented(
            docs.clone(),
            sub_pipeline,
            db,
            coll,
            vars,
            storage,
            collation,
            &Document::new(),
            None,
        )?;
        out.insert(
            name.clone(),
            Bson::Array(res.into_iter().map(Bson::Document).collect()),
        );
    }
    Ok(vec![out])
}

/// `$unionWith` — concatenate docs from another collection, optionally after a
/// sub-pipeline. The spec is a bare collection name (`{$unionWith: "coll"}`) or
/// `{coll, pipeline}`. The sub-pipeline runs in a *fresh* context (outer `let` /
/// vars are not visible — mongod's `$unionWith` has no `let`), and the union docs
/// are appended after the input docs (no dedup). Mirrors
/// `aggregate._stage_union_with`.
fn apply_union_with(
    spec: Option<&Bson>,
    mut docs: Vec<Document>,
    db: &str,
    storage: &dyn crate::storage::Storage,
    collation: Option<&Collation>,
) -> Result<Vec<Document>, CommandError> {
    let (from, sub_pipeline): (&str, Option<&Vec<Bson>>) = match spec {
        Some(Bson::String(s)) => (s.as_str(), None),
        Some(Bson::Document(d)) => {
            let from = d
                .get_str("coll")
                .map_err(|_| bad_value("$unionWith requires 'coll' (string)"))?;
            let sub = match d.get("pipeline") {
                None => None,
                Some(Bson::Array(a)) if !a.is_empty() => Some(a),
                Some(Bson::Array(_)) => None, // empty pipeline is a no-op
                Some(_) => return Err(bad_value("$unionWith 'pipeline' must be an array")),
            };
            (from, sub)
        }
        _ => {
            return Err(bad_value(
                "$unionWith requires a collection name or {coll, pipeline} doc",
            ))
        }
    };
    let mut foreign: Vec<Document> = decode_docs(
        storage
            .find(db, from, &Document::new(), None, None)
            .map_err(command_error)?,
    )?;
    if let Some(sub) = sub_pipeline {
        foreign = run_segmented(
            foreign,
            sub,
            db,
            Some(from),
            &Document::new(),
            storage,
            collation,
            &Document::new(),
            None,
        )?;
    }
    docs.append(&mut foreign);
    Ok(docs)
}

/// BFS over `foreign` from `seed`, chasing `connect_from` → `connect_to`.
/// Dedups by `_id` (a doc is collected at most once, at its shallowest depth).
#[allow(clippy::too_many_arguments)]
fn graph_walk(
    foreign: &[Document],
    seed: &Bson,
    connect_from: &str,
    connect_to: &str,
    max_depth: i64,
    depth_field: Option<&str>,
    restrict: Option<&Document>,
) -> Result<Vec<Document>, CommandError> {
    let mut seen: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    let mut out: Vec<Document> = Vec::new();
    let mut frontier: std::collections::VecDeque<(Bson, i64)> = std::collections::VecDeque::new();
    frontier.push_back((seed.clone(), 0));
    while let Some((value, depth)) = frontier.pop_front() {
        if depth > max_depth {
            continue;
        }
        for fdoc in foreign {
            // restrictSearchWithMatch (mongod): only traverse matching docs.
            if let Some(r) = restrict {
                if !secantus_core::query::matches(fdoc, r, &Document::new(), None).unwrap_or(false)
                {
                    continue;
                }
            }
            let id_key = graph_id_key(fdoc.get("_id"));
            if seen.contains(&id_key) {
                continue;
            }
            let target = secantus_core::get_path(fdoc, connect_to);
            if graph_values_match(&value, target) {
                seen.insert(id_key);
                let mut nd = fdoc.clone();
                if let Some(df) = depth_field {
                    nd.insert(df, Bson::Int64(depth));
                }
                out.push(nd);
                if let Some(next_value) = secantus_core::get_path(fdoc, connect_from) {
                    frontier.push_back((next_value.clone(), depth + 1));
                }
            }
        }
    }
    Ok(out)
}

/// A hashable dedup key for a `_id` value (BSON-encoded; `None` for a missing
/// `_id`, mirroring Python's `set` of `_id`s where a missing `_id` is `None`).
fn graph_id_key(id: Option<&Bson>) -> Vec<u8> {
    match id {
        None => Vec::new(),
        Some(v) => bson::to_vec(&doc! { "k": v.clone() }).unwrap_or_default(),
    }
}

/// `$graphLookup` connect-value equality, mirroring `aggregate._values_match`:
/// array↔array (any common element), array↔scalar membership, else Python `==`
/// (numeric-aware via `expressions::py_eq`). A missing target never matches.
fn graph_values_match(a: &Bson, target: Option<&Bson>) -> bool {
    let Some(b) = target else { return false };
    let eq = |x: &Bson, y: &Bson| secantus_core::expressions::py_eq(x, y).unwrap_or(false);
    match (a, b) {
        (Bson::Array(xs), Bson::Array(ys)) => xs.iter().any(|x| ys.iter().any(|y| eq(x, y))),
        (Bson::Array(xs), _) => xs.iter().any(|x| eq(x, b)),
        (_, Bson::Array(ys)) => ys.iter().any(|y| eq(a, y)),
        _ => eq(a, b),
    }
}

/// Set `path` (dotted) in `doc` to `value`, creating intermediate documents —
/// for `$lookup`'s `as` target. `set_path` in `secantus-core` is deliberately
/// private (its `Result<_, ()>` defer signal isn't public API), so this is a
/// small local setter; `as` is almost always a top-level field.
fn set_field_path(doc: &mut Document, path: &str, value: Bson) {
    match path.split_once('.') {
        None => {
            doc.insert(path, value);
        }
        Some((head, rest)) => {
            let child = match doc.get_mut(head) {
                Some(Bson::Document(d)) => d,
                _ => {
                    doc.insert(head, Document::new());
                    doc.get_document_mut(head).expect("just inserted")
                }
            };
            set_field_path(child, rest, value);
        }
    }
}

/// `$lookup` equality with mongod's array-aware semantics (mirrors
/// `aggregate._lookup_match`): array↔array → any element equal; one side array →
/// membership; else plain equality. A missing field is `Null`.
fn lookup_match(local: Option<&Bson>, foreign: Option<&Bson>) -> bool {
    let local = local.unwrap_or(&Bson::Null);
    let foreign = foreign.unwrap_or(&Bson::Null);
    match (local, foreign) {
        (Bson::Array(la), Bson::Array(fa)) => la.iter().any(|le| fa.iter().any(|fe| le == fe)),
        (Bson::Array(la), f) => la.iter().any(|le| le == f),
        (l, Bson::Array(fa)) => fa.iter().any(|fe| fe == l),
        (l, f) => l == f,
    }
}

/// `$sample` — `{size: N}`. Random N-doc subset (whole set when N ≥ len).
/// `SECANTUS_SAMPLE_SEED` pins the RNG for deterministic tests, mirroring
/// `aggregate._SAMPLE_RNG`.
fn apply_sample(spec: Option<&Bson>, docs: Vec<Document>) -> Result<Vec<Document>, CommandError> {
    use rand::seq::SliceRandom;
    let size = spec
        .and_then(Bson::as_document)
        .and_then(|d| d.get("size"))
        .and_then(as_i64)
        .ok_or_else(|| bad_value("$sample requires {size: N}"))?;
    if size < 0 {
        return Err(bad_value("$sample size must be non-negative"));
    }
    let size = size as usize;
    if size >= docs.len() {
        return Ok(docs);
    }
    let mut docs = docs;
    match sample_seed() {
        Some(seed) => {
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            docs.shuffle(&mut rng);
        }
        None => docs.shuffle(&mut rand::rng()),
    }
    docs.truncate(size);
    Ok(docs)
}

fn sample_seed() -> Option<u64> {
    std::env::var("SECANTUS_SAMPLE_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
}

/// A BSON number (int32 / int64 / double) as `f64`.
fn bson_f64(b: &Bson) -> Option<f64> {
    match b {
        Bson::Double(d) => Some(*d),
        Bson::Int32(i) => Some(*i as f64),
        Bson::Int64(i) => Some(*i as f64),
        _ => None,
    }
}

/// Resolve a view namespace to its base collection + combined pipeline, following
/// a `viewOn` chain (a view may be defined on another view) and prepending each
/// view's stored `viewPipeline` ahead of the caller's pipeline until a base
/// (non-view) collection is reached. A cycle is broken defensively. Returns the
/// inputs unchanged when `coll` isn't a view. Mirrors `commands._resolve_view`.
pub(crate) fn resolve_view(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    pipeline: Vec<Bson>,
) -> (String, Vec<Bson>) {
    let mut coll = coll.to_string();
    let mut combined = pipeline;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    while seen.insert(coll.clone()) {
        let Ok(opts) = storage.get_collection_options(db, &coll) else {
            break;
        };
        let Ok(view_on) = opts.get_str("viewOn") else {
            break;
        };
        let mut prepended: Vec<Bson> = opts
            .get_array("viewPipeline")
            .map(|a| a.to_vec())
            .unwrap_or_default();
        prepended.extend(combined);
        combined = prepended;
        coll = view_on.to_string();
    }
    (coll, combined)
}

/// Conservative `$geoWithin` candidate filter for a leading bounded `$geoNear`
/// (Rust mirror of `aggregate._geo_near_index_filter`): `Some(filter)` when the
/// stage has a numeric `maxDistance` and a matching `2d`/`2dsphere` index on
/// `key`, else `None` (fall back to the full scan). The radius is inflated by a
/// tiny epsilon so the candidate set is a strict superset of the exact
/// within-`maxDistance` set — the `$geoNear` stage then re-applies the exact
/// filter, so the output is byte-for-byte identical, only the fetched set shrinks.
/// The candidate shape (`$centerSphere`/`$center`) must match the index type.
fn geo_near_index_filter(
    spec: &Bson,
    db: &str,
    coll: Option<&str>,
    storage: &dyn crate::storage::Storage,
) -> Option<Document> {
    const EARTH_RADIUS_METERS: f64 = 6_378_100.0; // matches secantus_core::geo
    let spec = spec.as_document()?;
    let max_distance = match spec.get("maxDistance") {
        Some(Bson::Int32(n)) => *n as f64,
        Some(Bson::Int64(n)) => *n as f64,
        Some(Bson::Double(d)) => *d,
        _ => return None, // no numeric maxDistance -> no optimization
    };
    let coll = coll?;
    let indexes = storage.list_indexes(db, coll).ok()?;
    // Geo-indexed fields (field -> "2dsphere"/"2d"), in list_indexes order.
    let mut geo_fields: Vec<(String, String)> = Vec::new();
    for idx in &indexes {
        if let Ok(key) = idx.get_document("key") {
            for (field, v) in key {
                if let Bson::String(t) = v {
                    if (t == "2dsphere" || t == "2d") && !geo_fields.iter().any(|(f, _)| f == field)
                    {
                        geo_fields.push((field.clone(), t.clone()));
                    }
                }
            }
        }
    }
    if geo_fields.is_empty() {
        return None;
    }
    let key: String = match spec.get_str("key") {
        Ok(k) if !k.is_empty() => k.to_string(),
        _ => geo_fields[0].0.clone(), // infer: first geo index (mongod's behaviour)
    };
    let idx_type = geo_fields
        .iter()
        .find(|(f, _)| *f == key)
        .map(|(_, t)| t.as_str())?;
    // Parse `near` -> (spherical, cx, cy).
    let (spherical, cx, cy) = match spec.get("near") {
        Some(Bson::Document(d)) if d.get_str("type").ok() == Some("Point") => {
            let coords = d.get_array("coordinates").ok()?;
            if coords.len() != 2 {
                return None;
            }
            (true, bson_f64(&coords[0])?, bson_f64(&coords[1])?)
        }
        Some(Bson::Array(a)) if a.len() == 2 => {
            let sph = spec.get_bool("spherical").unwrap_or(false);
            (sph, bson_f64(&a[0])?, bson_f64(&a[1])?)
        }
        _ => return None,
    };
    // The candidate shape must match the index type.
    if spherical && idx_type != "2dsphere" {
        return None;
    }
    if !spherical && idx_type != "2d" {
        return None;
    }
    let radius = max_distance * (1.0 + 1e-9); // inflate -> guaranteed superset
    let center = Bson::Array(vec![Bson::Double(cx), Bson::Double(cy)]);
    let inner = if spherical {
        doc! { "$centerSphere": Bson::Array(vec![center, Bson::Double(radius / EARTH_RADIUS_METERS)]) }
    } else {
        doc! { "$center": Bson::Array(vec![center, Bson::Double(radius)]) }
    };
    let mut filter = Document::new();
    filter.insert(key, doc! { "$geoWithin": inner });
    Some(filter)
}

/// `$geoNear` — proximity search with attached distances. Mirrors
/// `aggregate._stage_geo_near`: optional `query` pre-filter, distance from each
/// doc's `key` field to `near` (`secantus_core::geo::point_distance`), drop docs
/// outside `[minDistance, maxDistance]`, attach the distance (× `distanceMultiplier`)
/// under `distanceField`, optionally echo the raw geometry under `includeLocs`,
/// and return ascending by distance. A GeoJSON Point `near` is spherical; a
/// legacy `[x, y]` is planar unless `spherical: true`. `key` is explicit or
/// inferred from the collection's lone geo index (`infer_geo_near_key`).
fn apply_geo_near(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    coll: Option<&str>,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
) -> Result<Vec<Document>, CommandError> {
    let spec = spec
        .and_then(Bson::as_document)
        .ok_or_else(|| bad_value("$geoNear requires a document spec"))?;
    let distance_field = spec
        .get_str("distanceField")
        .map_err(|_| bad_value("$geoNear requires a string `distanceField`"))?;
    // `key` is optional: when absent, infer it from the collection's lone geo
    // index (mongod does the same; ambiguous when there's more than one).
    let key: String = match spec.get_str("key") {
        Ok(k) => k.to_string(),
        Err(_) => infer_geo_near_key(db, coll, storage)?,
    };
    let key = key.as_str();
    let (spherical, center) = match spec.get("near") {
        Some(Bson::Document(d)) if d.get_str("type").ok() == Some("Point") => {
            let coords = d
                .get_array("coordinates")
                .map_err(|_| bad_value("$geoNear `near` Point needs coordinates"))?;
            if coords.len() != 2 {
                return Err(bad_value("$geoNear `near` Point needs [lng, lat]"));
            }
            let (x, y) = (bson_f64(&coords[0]), bson_f64(&coords[1]));
            (
                true,
                (
                    x.ok_or_else(|| bad_value("$geoNear `near` coords must be numbers"))?,
                    y.ok_or_else(|| bad_value("$geoNear `near` coords must be numbers"))?,
                ),
            )
        }
        Some(Bson::Array(a)) if a.len() == 2 => {
            let sph = spec.get_bool("spherical").unwrap_or(false);
            let (x, y) = (bson_f64(&a[0]), bson_f64(&a[1]));
            (
                sph,
                (
                    x.ok_or_else(|| bad_value("$geoNear `near` coords must be numbers"))?,
                    y.ok_or_else(|| bad_value("$geoNear `near` coords must be numbers"))?,
                ),
            )
        }
        _ => {
            return Err(bad_value(
                "$geoNear `near` must be a GeoJSON Point or [x, y]",
            ))
        }
    };
    let pre_filter = spec.get("query").and_then(Bson::as_document);
    let multiplier = spec
        .get("distanceMultiplier")
        .and_then(bson_f64)
        .unwrap_or(1.0);
    let max_distance = spec.get("maxDistance").and_then(bson_f64);
    let min_distance = spec.get("minDistance").and_then(bson_f64);
    let include_locs = spec.get_str("includeLocs").ok();

    let mut scored: Vec<(f64, Document)> = Vec::new();
    for doc in docs {
        if let Some(pf) = pre_filter {
            if !secantus_core::query::matches(&doc, pf, vars, None).unwrap_or(false) {
                continue;
            }
        }
        let Some(value) = secantus_core::get_path(&doc, key).cloned() else {
            continue;
        };
        let Some(d) = secantus_core::geo::point_distance(center, &value, spherical) else {
            continue;
        };
        if max_distance.is_some_and(|mx| d > mx) || min_distance.is_some_and(|mn| d < mn) {
            continue;
        }
        let mut out = doc;
        set_field_path(&mut out, distance_field, Bson::Double(d * multiplier));
        if let Some(loc_field) = include_locs {
            set_field_path(&mut out, loc_field, value);
        }
        scored.push((d, out));
    }
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored.into_iter().map(|(_, d)| d).collect())
}

/// Infer `$geoNear`'s `key` from the collection's geo index when the stage
/// doesn't name one: the field of the lone `2d` / `2dsphere` index. Errors if
/// there's no geo index or more than one (mongod's behaviour — ambiguous).
fn infer_geo_near_key(
    db: &str,
    coll: Option<&str>,
    storage: &dyn crate::storage::Storage,
) -> Result<String, CommandError> {
    let coll = coll.ok_or_else(|| bad_value("$geoNear requires a collection"))?;
    let indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    let mut geo_fields: Vec<String> = Vec::new();
    for idx in &indexes {
        if let Ok(key) = idx.get_document("key") {
            for (field, v) in key {
                if matches!(v, Bson::String(s) if s == "2dsphere" || s == "2d") {
                    geo_fields.push(field.clone());
                }
            }
        }
    }
    match geo_fields.len() {
        1 => Ok(geo_fields.remove(0)),
        0 => Err(bad_value(
            "$geoNear requires a geo index; no `key` given and the collection has no 2d/2dsphere index",
        )),
        _ => Err(bad_value(
            "$geoNear: more than one geo index — specify `key` to disambiguate",
        )),
    }
}

/// `$collStats` — first-stage collection statistics. Minimal but driver-faithful
/// shape (mirrors `aggregate._stage_coll_stats`): `ns` / `host` / `localTime`,
/// plus `storageStats` / `latencyStats` / `count` sub-docs when requested.
fn apply_coll_stats(
    spec: Option<&Bson>,
    db: &str,
    coll: Option<&str>,
    storage: &dyn crate::storage::Storage,
) -> Result<Vec<Document>, CommandError> {
    let coll = coll.ok_or_else(|| bad_value("$collStats requires a collection"))?;
    let spec = spec.and_then(Bson::as_document);
    let count = storage
        .count_matching(db, coll, &Document::new())
        .map_err(command_error)? as i64;
    let indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    let mut out = doc! {
        "ns": format!("{db}.{coll}"),
        "host": "secantus",
        "localTime": bson::DateTime::now(),
    };
    if spec.is_some_and(|s| s.contains_key("storageStats")) {
        let mut index_sizes = Document::new();
        for ix in &indexes {
            if let Ok(name) = ix.get_str("name") {
                index_sizes.insert(name.to_string(), 0_i32);
            }
        }
        let mut storage_stats = doc! {
            "size": 0_i32,
            "count": count,
            "avgObjSize": 0_i32,
            "storageSize": 0_i32,
            "indexSizes": index_sizes,
            "totalIndexSize": 0_i32,
            "scaleFactor": 1_i32,
            "nindexes": indexes.len() as i32,
        };
        if storage.collection_is_capped(db, coll).unwrap_or(false) {
            // mongod renames the user-set `size` to `maxSize` here, so a caller
            // can tell the cap from the current data size. mongo-ruby-driver's
            // `Collection#create ... applies the options` capped spec reads
            // `storageStats.{capped, max, maxSize}` directly, so all three have
            // to be present. Mirrors `aggregate._coll_stats`.
            storage_stats.insert("capped", true);
            // `as_i64` (the crate helper), not `Bson::as_i64` — the latter
            // matches Int64 only, and drivers send these as Int32
            // (mongo-ruby-driver sends `size: 4096, max: 512`), so both fields
            // silently vanished from the reply.
            let opts = storage.get_collection_options(db, coll).unwrap_or_default();
            if let Some(size) = opts.get("size").and_then(as_i64) {
                storage_stats.insert("maxSize", size);
            }
            if let Some(max) = opts.get("max").and_then(as_i64) {
                storage_stats.insert("max", max);
            }
        }
        out.insert("storageStats", storage_stats);
    }
    if spec.is_some_and(|s| s.contains_key("latencyStats")) {
        out.insert(
            "latencyStats",
            doc! {
                "reads": {"latency": 0_i64, "ops": 0_i64},
                "writes": {"latency": 0_i64, "ops": 0_i64},
                "commands": {"latency": 0_i64, "ops": 0_i64},
            },
        );
    }
    if spec.is_some_and(|s| s.contains_key("count")) {
        out.insert("count", count);
    }
    Ok(vec![out])
}

/// `$indexStats` — one doc per index (mirrors `aggregate._stage_index_stats`).
fn apply_index_stats(
    db: &str,
    coll: Option<&str>,
    storage: &dyn crate::storage::Storage,
) -> Result<Vec<Document>, CommandError> {
    let coll = coll.ok_or_else(|| bad_value("$indexStats requires a collection"))?;
    let indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    Ok(indexes
        .into_iter()
        .map(|ix| {
            doc! {
                "name": ix.get_str("name").unwrap_or("").to_string(),
                "key": ix.get_document("key").cloned().unwrap_or_default(),
                "host": "secantus",
                "accesses": {"ops": 0_i64, "since": Bson::Null},
            }
        })
        .collect())
}

/// Enforce the destination collection's `validator` on a `$out`/`$merge` write
/// unless `bypass` (the command's `bypassDocumentValidation`) is set: a doc that
/// fails the validator aborts with `DocumentValidationFailure` (121) when
/// `validationAction` is `"error"` (the default; `"warn"` is a no-op here).
/// Mirrors `aggregate._enforce_target_validator`.
fn enforce_target_validator(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    docs: &[Document],
    bypass: bool,
) -> Result<(), CommandError> {
    if bypass {
        return Ok(());
    }
    let opts = storage
        .get_collection_options(db, coll)
        .map_err(command_error)?;
    let validator = match opts.get_document("validator") {
        Ok(v) if !v.is_empty() => v.clone(),
        _ => return Ok(()),
    };
    if opts.get_str("validationAction").unwrap_or("error") != "error" {
        return Ok(());
    }
    for d in docs {
        if !secantus_core::query::matches(d, &validator, &Document::new(), None).unwrap_or(false) {
            return Err(CommandError::new(
                121,
                "DocumentValidationFailure",
                "Document failed validation",
            ));
        }
    }
    Ok(())
}

/// `$out` — replace the target collection with the pipeline result. Mirrors
/// `aggregate._stage_out`'s default (same-db) behaviour: drop the target, insert
/// every result doc, and emit nothing downstream.
fn apply_out(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    storage: &dyn crate::storage::Storage,
    bypass_validation: bool,
) -> Result<Vec<Document>, CommandError> {
    let (out_db, out_coll) = out_target(spec, db)?;
    enforce_target_validator(storage, &out_db, &out_coll, &docs, bypass_validation)?;
    storage
        .drop_collection(&out_db, &out_coll)
        .map_err(command_error)?;
    storage
        .create_collection(&out_db, &out_coll)
        .map_err(command_error)?;
    if !docs.is_empty() {
        let encoded = encode_docs(docs)?;
        storage
            .insert(&out_db, &out_coll, encoded, true)
            .map_err(command_error)?;
    }
    Ok(Vec::new())
}

/// Resolve `$out`'s target. Accepts `"coll"` or `{db, coll}`.
fn out_target(spec: Option<&Bson>, db: &str) -> Result<(String, String), CommandError> {
    match spec {
        Some(Bson::String(c)) => Ok((db.to_string(), c.clone())),
        Some(Bson::Document(d)) => {
            let coll = d
                .get_str("coll")
                .map_err(|_| bad_value("$out target document requires 'coll'"))?;
            let out_db = d.get_str("db").unwrap_or(db);
            Ok((out_db.to_string(), coll.to_string()))
        }
        _ => Err(bad_value("$out requires a collection name or {db, coll}")),
    }
}

/// `$merge` — merge the pipeline result into the target collection. Mirrors
/// `aggregate._stage_merge`: per result doc, find the target doc(s) matching the
/// `on` key, then apply `whenMatched` (`merge` deep-merge default / `replace` /
/// `keepExisting` / `delete` / `fail`, or an inline pipeline with `$$new` bound
/// to the incoming doc) or `whenNotMatched` (`insert` default / `discard` /
/// `fail`). A non-`_id` `on` requires a matching unique index on the target.
fn apply_merge(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    storage: &dyn crate::storage::Storage,
    bypass_validation: bool,
) -> Result<Vec<Document>, CommandError> {
    let (out_db, out_coll, on, when_matched, when_not_matched) = merge_spec(spec, db)?;
    enforce_target_validator(storage, &out_db, &out_coll, &docs, bypass_validation)?;
    storage
        .create_collection(&out_db, &out_coll)
        .map_err(command_error)?;
    // mongod requires a unique index covering a non-`_id` `on` so the match is
    // guaranteed to hit at most one target doc.
    merge_validate_on(storage, &out_db, &out_coll, &on)?;
    for d in docs {
        let mut filter = Document::new();
        for field in &on {
            let v = secantus_core::get_path(&d, field)
                .cloned()
                .unwrap_or(Bson::Null);
            filter.insert(field.clone(), v);
        }
        let existing = decode_docs(
            storage
                .find(&out_db, &out_coll, &filter, None, None)
                .map_err(command_error)?,
        )?
        .into_iter()
        .next();

        match existing {
            Some(existing) => {
                let existing_id = existing.get("_id").cloned().unwrap_or(Bson::Null);
                let id_filter = doc! {"_id": existing_id.clone()};
                let mode = match &when_matched {
                    WhenMatched::Mode(m) => m.as_str(),
                    WhenMatched::Pipeline(pipeline) => {
                        // Run the inline pipeline over the existing doc with the
                        // incoming doc bound to `$$new`; the result replaces it
                        // (existing `_id` preserved).
                        let pvars = doc! { "new": Bson::Document(d.clone()) };
                        let result = core_run(vec![existing.clone()], pipeline, &pvars, None)?;
                        let mut newdoc = result.into_iter().next().unwrap_or(existing);
                        newdoc.insert("_id", existing_id);
                        storage
                            .update_matching(&out_db, &out_coll, &id_filter, &newdoc, false, false)
                            .map_err(command_error)?;
                        continue;
                    }
                };
                match mode {
                    "keepExisting" => {}
                    "fail" => {
                        // mongod's DuplicateKey carries keyPattern/keyValue so the
                        // driver can inspect which key collided (the crud-spec
                        // "$merge DuplicateKey error is accessible" test asserts it).
                        return Err(CommandError::new(
                            11000,
                            "DuplicateKey",
                            "$merge whenMatched=fail matched an existing document",
                        )
                        .with_extra(doc! {
                            "keyPattern": { "_id": 1 },
                            "keyValue": { "_id": existing_id.clone() },
                        }));
                    }
                    "delete" => {
                        storage
                            .delete_matching(&out_db, &out_coll, &id_filter, 1)
                            .map_err(command_error)?;
                    }
                    "replace" => {
                        storage
                            .delete_matching(&out_db, &out_coll, &id_filter, 1)
                            .map_err(command_error)?;
                        let mut new = d;
                        new.entry("_id".to_string()).or_insert(existing_id);
                        storage
                            .insert(&out_db, &out_coll, encode_docs(vec![new])?, true)
                            .map_err(command_error)?;
                    }
                    // default: "merge" — deep-merge the result doc over the
                    // existing one (existing `_id` preserved), then replace.
                    _ => {
                        let mut merged = deep_merge_docs(&existing, &d);
                        merged.insert("_id", existing_id);
                        storage
                            .update_matching(&out_db, &out_coll, &id_filter, &merged, false, false)
                            .map_err(command_error)?;
                    }
                }
            }
            None => match when_not_matched.as_str() {
                "discard" => {}
                "fail" => {
                    return Err(bad_value(
                        "$merge whenNotMatched=fail and no matching document exists",
                    ));
                }
                _ => {
                    storage
                        .insert(&out_db, &out_coll, encode_docs(vec![d])?, true)
                        .map_err(command_error)?;
                }
            },
        }
    }
    Ok(Vec::new())
}

/// Parse `$merge`'s spec into `(db, coll, on, whenMatched, whenNotMatched)`.
/// Accepts the string short-form (`$merge: "coll"`) or the document form.
/// `$merge` `whenMatched`: either a named mode or an inline aggregation pipeline
/// applied to each matched document (with the incoming doc bound to `$$new`).
enum WhenMatched {
    Mode(String),
    Pipeline(Vec<Bson>),
}

fn merge_spec(
    spec: Option<&Bson>,
    db: &str,
) -> Result<(String, String, Vec<String>, WhenMatched, String), CommandError> {
    const VALID_MATCHED: &[&str] = &["merge", "replace", "keepExisting", "fail", "delete"];
    const VALID_NOT_MATCHED: &[&str] = &["insert", "discard", "fail"];
    let (out_db, out_coll, on, when_matched, when_not_matched) = match spec {
        Some(Bson::String(c)) => (
            db.to_string(),
            c.clone(),
            vec!["_id".to_string()],
            WhenMatched::Mode("merge".to_string()),
            "insert".to_string(),
        ),
        Some(Bson::Document(d)) => {
            let (odb, ocoll) = match d.get("into") {
                Some(Bson::String(c)) => (db.to_string(), c.clone()),
                Some(Bson::Document(into)) => {
                    let coll = into
                        .get_str("coll")
                        .map_err(|_| bad_value("$merge into.coll must be a string"))?;
                    (
                        into.get_str("db").unwrap_or(db).to_string(),
                        coll.to_string(),
                    )
                }
                _ => return Err(bad_value("$merge requires 'into'")),
            };
            let on = match d.get("on") {
                Some(Bson::String(s)) => vec![s.clone()],
                Some(Bson::Array(a)) => a
                    .iter()
                    .filter_map(|b| b.as_str().map(String::from))
                    .collect(),
                None => vec!["_id".to_string()],
                _ => {
                    return Err(bad_value(
                        "$merge 'on' must be a string or array of strings",
                    ))
                }
            };
            let wm = match d.get("whenMatched") {
                Some(Bson::Array(p)) => WhenMatched::Pipeline(p.clone()),
                Some(Bson::String(s)) => WhenMatched::Mode(s.clone()),
                None => WhenMatched::Mode("merge".to_string()),
                _ => {
                    return Err(bad_value(
                        "$merge whenMatched must be a mode string or a pipeline array",
                    ))
                }
            };
            let wnm = d.get_str("whenNotMatched").unwrap_or("insert").to_string();
            (odb, ocoll, on, wm, wnm)
        }
        _ => return Err(bad_value("$merge requires a string or document spec")),
    };
    if let WhenMatched::Mode(m) = &when_matched {
        if !VALID_MATCHED.contains(&m.as_str()) {
            return Err(bad_value(format!(
                "$merge whenMatched must be one of {VALID_MATCHED:?} or a pipeline array"
            )));
        }
    }
    if !VALID_NOT_MATCHED.contains(&when_not_matched.as_str()) {
        return Err(bad_value(format!(
            "$merge whenNotMatched must be one of {VALID_NOT_MATCHED:?}"
        )));
    }
    Ok((out_db, out_coll, on, when_matched, when_not_matched))
}

/// Validate that a non-`_id` `$merge` `on` is backed by a unique index on the
/// target whose key fields are exactly the `on` fields (mongod code 51183). The
/// default `on: ["_id"]` is always satisfied (the `_id` index is unique).
fn merge_validate_on(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    on: &[String],
) -> Result<(), CommandError> {
    if on.len() == 1 && on[0] == "_id" {
        return Ok(());
    }
    let mut want: Vec<&str> = on.iter().map(String::as_str).collect();
    want.sort_unstable();
    let indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    for idx in &indexes {
        if !idx.get_bool("unique").unwrap_or(false) {
            continue;
        }
        if let Ok(key) = idx.get_document("key") {
            let mut have: Vec<&str> = key.keys().map(String::as_str).collect();
            have.sort_unstable();
            if have == want {
                return Ok(());
            }
        }
    }
    Err(CommandError::new(
        51183,
        "Location51183",
        format!("$merge: no unique index on the target collection matches the 'on' fields {on:?}"),
    ))
}

/// Recursive document merge for `$merge whenMatched: "merge"` (mirrors
/// `aggregate._deep_merge_docs`): overlapping sub-documents merge recursively;
/// arrays / scalars take the new value; non-overlapping keys from both survive.
fn deep_merge_docs(existing: &Document, new: &Document) -> Document {
    let mut result = existing.clone();
    for (k, v) in new.iter() {
        match (result.get(k), v) {
            (Some(Bson::Document(e)), Bson::Document(n)) => {
                result.insert(k.clone(), Bson::Document(deep_merge_docs(e, n)));
            }
            _ => {
                result.insert(k.clone(), v.clone());
            }
        }
    }
    result
}

/// Whether the first pipeline stage has `key`.
fn first_stage_has(pipeline: &[Bson], key: &str) -> bool {
    matches!(pipeline.first(), Some(Bson::Document(d)) if d.contains_key(key))
}

/// Whether the first pipeline stage has any of `keys`.
fn first_stage_has_any(pipeline: &[Bson], keys: &[&str]) -> bool {
    matches!(pipeline.first(), Some(Bson::Document(d)) if keys.iter().any(|k| d.contains_key(*k)))
}

/// If the pipeline starts with `{$match: {...}}`, return its filter and the
/// remaining stages; otherwise an empty filter and the whole pipeline.
fn lift_leading_match(pipeline: &[Bson]) -> (Document, Vec<Bson>) {
    if let Some(Bson::Document(d)) = pipeline.first() {
        if let Some(Bson::Document(m)) = d.get("$match") {
            return (m.clone(), pipeline[1..].to_vec());
        }
    }
    (Document::new(), pipeline.to_vec())
}

/// Process the leading **pass-through prefix** of a pipeline over the fetched
/// RAW blobs, before `decode_docs` materialises them. `$skip` / `$limit` drop or
/// truncate whole documents and a (non-leading) `$match` filters via
/// `query::matches_raw` (decoding only the filter's fields) — none of these
/// inspect a document beyond the filter's fields or its position, so a limiting
/// or selective prefix shrinks the set that the heavier stages (`$group` /
/// `$sort` / computed `$project` / `$unwind` / …) must decode. Order-preserving,
/// so the reduced-then-decoded input is identical to decoding everything and
/// running the same stages through `apply_pipeline`. Stops at the first stage it
/// doesn't handle (or a `$match` whose filter the raw matcher defers on),
/// returning the reduced blobs and the remaining pipeline for the normal
/// decode + run path.
fn reduce_raw_prefix(
    mut bytes: Vec<Vec<u8>>,
    pipeline: &[Bson],
    vars: &Document,
    collation: Option<&Collation>,
) -> (Vec<Vec<u8>>, Vec<Bson>) {
    let mut consumed = 0;
    for stage in pipeline {
        let Some(sd) = stage.as_document() else { break };
        if sd.len() != 1 {
            break;
        }
        let Some((name, arg)) = sd.iter().next() else {
            break;
        };
        match name.as_str() {
            // Only fast-path a clearly-valid INTEGER argument. A whole double
            // (`$limit: 2.0`) is valid but a fractional / bool / negative /
            // zero-`$limit` argument must raise — deferring every non-integer
            // arg to the full engine keeps its validation intact (it accepts the
            // whole double and computes it; it raises on the rest).
            "$skip" => match arg {
                Bson::Int32(n) if *n >= 0 => {
                    let n = (*n as usize).min(bytes.len());
                    bytes.drain(..n);
                }
                Bson::Int64(n) if *n >= 0 => {
                    let n = (*n as usize).min(bytes.len());
                    bytes.drain(..n);
                }
                _ => break,
            },
            "$limit" => match arg {
                Bson::Int32(n) if *n > 0 => bytes.truncate(*n as usize),
                Bson::Int64(n) if *n > 0 => bytes.truncate(*n as usize),
                _ => break,
            },
            "$match" => {
                let Some(filter) = arg.as_document() else {
                    break;
                };
                let mut kept = Vec::with_capacity(bytes.len());
                let mut deferred = false;
                for blob in &bytes {
                    let Ok(raw) = bson::RawDocument::from_bytes(blob) else {
                        deferred = true;
                        break;
                    };
                    match secantus_core::query::matches_raw(raw, filter, vars, collation) {
                        Ok(true) => kept.push(blob.clone()),
                        Ok(false) => {}
                        Err(_) => {
                            deferred = true;
                            break;
                        }
                    }
                }
                if deferred {
                    break; // a filter the raw matcher can't do -> full engine
                }
                bytes = kept;
            }
            _ => break, // first heavier stage — stop
        }
        consumed += 1;
    }
    (bytes, pipeline[consumed..].to_vec())
}

#[cfg(test)]
mod current_op_metadata_tests {
    use super::*;

    fn meta() -> Document {
        doc! {
            "application": {"name": "my-app"},
            "driver": {"name": "mongoc / mongocxx", "version": "1.2.3"},
            "os": {"type": "Darwin"},
        }
    }

    /// mongocxx's client-metadata handshake test scans `$currentOp` for a row
    /// whose top-level `appName` matches the one it connected with, and only
    /// then inspects `clientMetadata`. With neither field the scan matched
    /// nothing, so the test failed on a missing op rather than on the metadata
    /// it meant to check.
    #[test]
    fn current_op_surfaces_app_name_and_client_metadata() {
        let rows = apply_source_stage("$currentOp", "db", None, &doc! {}, Some(&meta()));
        let row = &rows[0];
        assert_eq!(row.get_str("appName").unwrap(), "my-app");
        let cm = row.get_document("clientMetadata").unwrap();
        assert_eq!(
            cm.get_document("driver").unwrap().get_str("name").unwrap(),
            "mongoc / mongocxx"
        );
        assert!(
            cm.get_document("os").is_ok(),
            "os is asserted by the test too"
        );
    }

    /// No handshake metadata (an internal caller) ⇒ neither field, rather than
    /// an empty `appName` that a scan could match by accident.
    #[test]
    fn current_op_omits_the_fields_without_metadata() {
        let rows = apply_source_stage("$currentOp", "db", None, &doc! {}, None);
        assert!(rows[0].get("appName").is_none());
        assert!(rows[0].get("clientMetadata").is_none());
    }

    /// Metadata without `application` (a driver that sent no appName) still
    /// surfaces `clientMetadata`, just no `appName` to match on.
    #[test]
    fn metadata_without_an_application_yields_no_app_name() {
        let m = doc! {"driver": {"name": "d", "version": "1"}, "os": {"type": "Linux"}};
        let rows = apply_source_stage("$currentOp", "db", None, &doc! {}, Some(&m));
        assert!(rows[0].get("appName").is_none());
        assert!(rows[0].get_document("clientMetadata").is_ok());
    }
}
