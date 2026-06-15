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
//! storage-free): `$lookup` (simple + `let`/`pipeline` forms), `$sample`,
//! `$collStats`, `$indexStats`, `$out`, `$merge`. A `run_segmented` flushes the
//! buffered run of storage-free stages through `apply_pipeline`, then applies the
//! storage-backed stage, repeating.
//!
//! **Source stages** (`$currentOp` / `$listLocalSessions` / `$listSessions`)
//! also run here: they ignore their input and emit a single synthetic "op" row
//! (port of `aggregate._stage_current_op`), since SecantusDB runs commands
//! synchronously and keeps no per-op registry. They're what makes a
//! database-level `aggregate: 1` pipeline (which has no source collection) work.
//!
//! `$geoNear` runs here too (brute-force COLLSCAN: distance from each doc's `key`
//! to `near` via `secantus_core::geo::point_distance`, min/max filter, ascending
//! sort, `distanceField`/`includeLocs` attach) — `key` must be explicit
//! (geo-index inference deferred).
//!
//! **Deferred (documented so parity is honest):**
//! * **`$graphLookup`** — not ported; surfaces as `BadValue`.
//! * **`$lookup` inside `$facet`** — `$facet` sub-pipelines run inside the
//!   storage-free core, so a storage-backed stage nested in a facet still
//!   Fallbacks. **`$merge` pipeline-form `whenMatched`** and **`on`-field
//!   unique-index validation** are also deferred.
//! * **`$changeStream`** — handled separately (`changestream::open_change_stream`);
//!   the standalone-rejection (40573) is honoured here.
//!
//! **`collation`** is threaded through `$match` / `$sort` (the storage-free core)
//! and the lifted-`$match` fetch (COLLSCAN-forced); a collation the engine can't
//! reproduce (non-ASCII / numericOrdering) surfaces as `BadValue`.

use bson::{doc, Bson, Document};

use crate::find::split_into_cursor;
use crate::util::{
    as_i64, collation_of, command_error, decode_docs, docs_to_bson, encode_docs, resolve_let_vars,
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
            // Stages that generate / read their own input start from no docs.
            if first_stage_has_any(&pipeline, &["$collStats", "$indexStats", "$documents"]) {
                (ns, Vec::new(), pipeline)
            } else {
                // Lift a leading $match into the fetch filter; drop it from the
                // pipeline so we don't apply it twice.
                let (filter, rest) = lift_leading_match(&pipeline);
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
                (ns, decode_docs(bytes)?, rest)
            }
        }
        // Collectionless aggregate (e.g. `{aggregate: 1, pipeline: [{$documents: …}]}`).
        None => (
            format!("{}.$cmd.aggregate", ctx.db_name),
            Vec::new(),
            pipeline,
        ),
    };

    let result = run_segmented(
        input,
        &working_pipeline,
        &ctx.db_name,
        coll.as_deref(),
        &vars,
        storage,
        collation.as_ref(),
        doc,
    )?;

    let (first_batch, cursor_id) =
        split_into_cursor(encode_docs(result)?, batch_size, &ns, cursors)?;
    Ok(doc! {
        "cursor": {
            "firstBatch": docs_to_bson(first_batch)?,
            // Cursor `id` MUST be int64 — the Go driver hard-fails int32 here.
            "id": Bson::Int64(cursor_id),
            "ns": ns,
        },
        "ok": 1.0,
    })
}

/// Stages handled at the command layer (they read/write storage or need a
/// brute-force scan), rather than by the storage-free `secantus_core` pipeline
/// engine. Everything else flows through `apply_pipeline`. `$graphLookup` is
/// absent — not ported, so it Fallbacks to `BadValue`.
fn is_storage_backed(name: &str) -> bool {
    matches!(
        name,
        "$lookup" | "$sample" | "$collStats" | "$indexStats" | "$out" | "$merge" | "$geoNear"
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
    vec![doc! {
        "type": "op",
        "host": "secantus",
        "desc": "$currentOp",
        "active": false,
        "currentOpTime": "",
        "command": command_doc,
        "ns": ns,
        "op": "command",
    }]
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
) -> Result<Vec<Document>, CommandError> {
    let mut docs = input;
    let mut buffer: Vec<Bson> = Vec::new();
    for stage in pipeline {
        let name = stage_name(stage);
        if is_source_stage(name) {
            // Source stages (e.g. `$currentOp` / `$listLocalSessions`) ignore the
            // input and emit a synthetic row. Run (and discard) any buffered
            // stages first so a malformed earlier stage still errors in order,
            // matching Python; the source stage replaces the docs regardless.
            if !buffer.is_empty() {
                let _ = core_run(docs, &buffer, vars, collation)?;
                buffer.clear();
            }
            docs = apply_source_stage(name, db, coll, cmd_doc);
        } else if is_storage_backed(name) {
            if !buffer.is_empty() {
                docs = core_run(docs, &buffer, vars, collation)?;
                buffer.clear();
            }
            let spec = stage.as_document().and_then(|d| d.get(name)).cloned();
            docs = apply_storage_stage(
                name,
                spec.as_ref(),
                docs,
                db,
                coll,
                vars,
                storage,
                collation,
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
) -> Result<Vec<Document>, CommandError> {
    match name {
        "$lookup" => apply_lookup(spec, docs, db, vars, storage, collation),
        "$sample" => apply_sample(spec, docs),
        "$collStats" => apply_coll_stats(spec, db, coll, storage),
        "$indexStats" => apply_index_stats(db, coll, storage),
        "$out" => apply_out(spec, docs, db, storage),
        "$merge" => apply_merge(spec, docs, db, storage),
        "$geoNear" => apply_geo_near(spec, docs, vars),
        _ => Err(bad_value(format!(
            "unsupported storage-backed stage {name}"
        ))),
    }
}

/// `$lookup` — join a foreign collection in. Mirrors `aggregate._stage_lookup`:
/// simple `localField`/`foreignField` equality join (array-aware), or the
/// `let` + `pipeline` form (each outer doc binds `let` vars, then runs the
/// sub-pipeline over the candidate foreign docs). We materialise the foreign
/// collection and hash-join in Rust — correctness-identical to the Python
/// index-driven path; index acceleration is a follow-up.
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

/// `$geoNear` — proximity search with attached distances. Mirrors
/// `aggregate._stage_geo_near`: optional `query` pre-filter, distance from each
/// doc's `key` field to `near` (`secantus_core::geo::point_distance`), drop docs
/// outside `[minDistance, maxDistance]`, attach the distance (× `distanceMultiplier`)
/// under `distanceField`, optionally echo the raw geometry under `includeLocs`,
/// and return ascending by distance. A GeoJSON Point `near` is spherical; a
/// legacy `[x, y]` is planar unless `spherical: true`.
///
/// **Deferred:** `key`-inference from a geo index (we require an explicit `key`,
/// erroring otherwise — real mongod infers it from the sole geo index).
fn apply_geo_near(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    vars: &Document,
) -> Result<Vec<Document>, CommandError> {
    let spec = spec
        .and_then(Bson::as_document)
        .ok_or_else(|| bad_value("$geoNear requires a document spec"))?;
    let distance_field = spec
        .get_str("distanceField")
        .map_err(|_| bad_value("$geoNear requires a string `distanceField`"))?;
    let key = spec.get_str("key").map_err(|_| {
        bad_value("$geoNear requires a string `key` (geo-index inference deferred)")
    })?;
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
            storage_stats.insert("capped", true);
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

/// `$out` — replace the target collection with the pipeline result. Mirrors
/// `aggregate._stage_out`'s default (same-db) behaviour: drop the target, insert
/// every result doc, and emit nothing downstream.
fn apply_out(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    storage: &dyn crate::storage::Storage,
) -> Result<Vec<Document>, CommandError> {
    let (out_db, out_coll) = out_target(spec, db)?;
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
/// `keepExisting` / `delete` / `fail`) or `whenNotMatched` (`insert` default /
/// `discard` / `fail`). The pipeline-array `whenMatched` form is deferred
/// (surfaced as `BadValue`); `on`-field unique-index validation is skipped.
fn apply_merge(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    storage: &dyn crate::storage::Storage,
) -> Result<Vec<Document>, CommandError> {
    let (out_db, out_coll, on, when_matched, when_not_matched) = merge_spec(spec, db)?;
    storage
        .create_collection(&out_db, &out_coll)
        .map_err(command_error)?;
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
                match when_matched.as_str() {
                    "keepExisting" => {}
                    "fail" => {
                        return Err(CommandError::new(
                            11000,
                            "DuplicateKey",
                            "$merge whenMatched=fail matched an existing document",
                        ));
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
fn merge_spec(
    spec: Option<&Bson>,
    db: &str,
) -> Result<(String, String, Vec<String>, String, String), CommandError> {
    const VALID_MATCHED: &[&str] = &["merge", "replace", "keepExisting", "fail", "delete"];
    const VALID_NOT_MATCHED: &[&str] = &["insert", "discard", "fail"];
    let (out_db, out_coll, on, when_matched, when_not_matched) = match spec {
        Some(Bson::String(c)) => (
            db.to_string(),
            c.clone(),
            vec!["_id".to_string()],
            "merge".to_string(),
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
            if matches!(d.get("whenMatched"), Some(Bson::Array(_))) {
                return Err(bad_value(
                    "$merge whenMatched pipeline form is not supported by the Rust server",
                ));
            }
            let wm = d.get_str("whenMatched").unwrap_or("merge").to_string();
            let wnm = d.get_str("whenNotMatched").unwrap_or("insert").to_string();
            (odb, ocoll, on, wm, wnm)
        }
        _ => return Err(bad_value("$merge requires a string or document spec")),
    };
    if !VALID_MATCHED.contains(&when_matched.as_str()) {
        return Err(bad_value(format!(
            "$merge whenMatched must be one of {VALID_MATCHED:?} or a pipeline array"
        )));
    }
    if !VALID_NOT_MATCHED.contains(&when_not_matched.as_str()) {
        return Err(bad_value(format!(
            "$merge whenNotMatched must be one of {VALID_NOT_MATCHED:?}"
        )));
    }
    Ok((out_db, out_coll, on, when_matched, when_not_matched))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use crate::storage::{RawHint, Storage, StorageError, UpdateOutcome};
    use crate::CursorRegistry;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn matches(d: &Document, filter: &Document) -> bool {
        filter.iter().all(|(k, v)| d.get(k) == Some(v))
    }

    #[derive(Default)]
    struct FakeStorage {
        cols: Mutex<HashMap<(String, String), Vec<Document>>>,
        indexes: Mutex<HashMap<(String, String), Vec<Document>>>,
    }

    impl FakeStorage {
        fn seed(db: &str, coll: &str, docs: Vec<Document>) -> Arc<FakeStorage> {
            let s = FakeStorage::default();
            s.cols
                .lock()
                .unwrap()
                .insert((db.to_string(), coll.to_string()), docs);
            Arc::new(s)
        }
        /// Seed an additional collection on an existing fake.
        fn add(&self, db: &str, coll: &str, docs: Vec<Document>) {
            self.cols
                .lock()
                .unwrap()
                .insert((db.to_string(), coll.to_string()), docs);
        }
        /// Seed an index descriptor (`{name, key}`) for a collection.
        fn add_index(&self, db: &str, coll: &str, index: Document) {
            self.indexes
                .lock()
                .unwrap()
                .entry((db.to_string(), coll.to_string()))
                .or_default()
                .push(index);
        }
    }

    impl Storage for FakeStorage {
        fn insert(
            &self,
            db: &str,
            coll: &str,
            docs: Vec<Vec<u8>>,
            _: bool,
        ) -> Result<(usize, Vec<Document>), StorageError> {
            let mut cols = self.cols.lock().unwrap();
            let bucket = cols.entry((db.to_string(), coll.to_string())).or_default();
            let n = docs.len();
            for bytes in docs {
                bucket.push(Document::from_reader(&mut bytes.as_slice()).unwrap());
            }
            Ok((n, vec![]))
        }
        fn update_matching(
            &self,
            db: &str,
            coll: &str,
            filter: &Document,
            update: &Document,
            _: bool,
            upsert: bool,
        ) -> Result<UpdateOutcome, StorageError> {
            // Only the replacement-document form is exercised by $merge tests.
            let mut cols = self.cols.lock().unwrap();
            let bucket = cols.entry((db.to_string(), coll.to_string())).or_default();
            if let Some(d) = bucket.iter_mut().find(|d| matches(d, filter)) {
                *d = update.clone();
                return Ok(UpdateOutcome {
                    matched: 1,
                    modified: 1,
                    upserted_id: None,
                });
            }
            if upsert {
                bucket.push(update.clone());
                return Ok(UpdateOutcome {
                    matched: 0,
                    modified: 0,
                    upserted_id: update.get("_id").cloned(),
                });
            }
            Ok(UpdateOutcome::default())
        }
        fn delete_matching(
            &self,
            db: &str,
            coll: &str,
            filter: &Document,
            limit: usize,
        ) -> Result<usize, StorageError> {
            let mut cols = self.cols.lock().unwrap();
            let bucket = cols.entry((db.to_string(), coll.to_string())).or_default();
            let mut removed = 0;
            bucket.retain(|d| {
                if (limit == 0 || removed < limit) && matches(d, filter) {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
            Ok(removed)
        }
        fn count_matching(
            &self,
            db: &str,
            coll: &str,
            filter: &Document,
        ) -> Result<usize, StorageError> {
            let cols = self.cols.lock().unwrap();
            Ok(cols
                .get(&(db.to_string(), coll.to_string()))
                .map(|b| b.iter().filter(|d| matches(d, filter)).count())
                .unwrap_or(0))
        }
        fn create_collection(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
            let mut cols = self.cols.lock().unwrap();
            cols.entry((db.to_string(), coll.to_string())).or_default();
            Ok(true)
        }
        fn drop_collection(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
            let mut cols = self.cols.lock().unwrap();
            Ok(cols.remove(&(db.to_string(), coll.to_string())).is_some())
        }
        fn list_indexes(&self, db: &str, coll: &str) -> Result<Vec<Document>, StorageError> {
            Ok(self
                .indexes
                .lock()
                .unwrap()
                .get(&(db.to_string(), coll.to_string()))
                .cloned()
                .unwrap_or_default())
        }
        fn find(
            &self,
            db: &str,
            coll: &str,
            filter: &Document,
            _: Option<&Document>,
            _: Option<RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, StorageError> {
            let cols = self.cols.lock().unwrap();
            Ok(cols
                .get(&(db.to_string(), coll.to_string()))
                .map(|b| {
                    b.iter()
                        .filter(|d| matches(d, filter))
                        .map(|d| {
                            let mut v = Vec::new();
                            d.to_writer(&mut v).unwrap();
                            v
                        })
                        .collect()
                })
                .unwrap_or_default())
        }
    }

    fn ctx(storage: Arc<FakeStorage>) -> CommandContext {
        let mut c = CommandContext::new(1)
            .with_storage(storage)
            .with_cursors(Arc::new(CursorRegistry::new()));
        c.db_name = "t".into();
        c
    }

    fn first_batch(reply: &Document) -> &Vec<Bson> {
        reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
    }

    #[test]
    fn aggregate_match_then_count() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![
                doc! {"_id": 1, "x": 1},
                doc! {"_id": 2, "x": 2},
                doc! {"_id": 3, "x": 1},
            ],
        );
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$match": {"x": 1}}, {"$count": "n"}], "cursor": {}},
            &mut c,
        );
        let fb = first_batch(&reply);
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].as_document().unwrap().get_i32("n").unwrap(), 2);
        assert_eq!(
            reply.get_document("cursor").unwrap().get_str("ns").unwrap(),
            "t.c"
        );
    }

    #[test]
    fn aggregate_group_sum() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![
                doc! {"_id": 1, "v": 10},
                doc! {"_id": 2, "v": 20},
                doc! {"_id": 3, "v": 30},
            ],
        );
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$group": {"_id": Bson::Null, "total": {"$sum": "$v"}}}
            ], "cursor": {}},
            &mut c,
        );
        let fb = first_batch(&reply);
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].as_document().unwrap().get_i32("total").unwrap(), 60);
    }

    #[test]
    fn aggregate_sort_then_limit() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![
                doc! {"_id": 1},
                doc! {"_id": 2},
                doc! {"_id": 3},
                doc! {"_id": 4},
            ],
        );
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$sort": {"_id": -1}}, {"$limit": 2}], "cursor": {}},
            &mut c,
        );
        let ids: Vec<i32> = first_batch(&reply)
            .iter()
            .map(|b| b.as_document().unwrap().get_i32("_id").unwrap())
            .collect();
        assert_eq!(ids, vec![4, 3]);
    }

    #[test]
    fn aggregate_unsupported_stage_is_bad_value() {
        let s = FakeStorage::seed("t", "c", vec![doc! {"_id": 1}]);
        let mut c = ctx(s);
        // $graphLookup isn't ported → the Rust engine Fallbacks to BadValue.
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$graphLookup": {"from": "o", "startWith": "$x", "connectFromField": "x", "connectToField": "y", "as": "g"}}
            ], "cursor": {}},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 2);
        assert_eq!(reply.get_str("codeName").unwrap(), "BadValue");
    }

    #[test]
    fn aggregate_list_local_sessions_source_stage() {
        // Database-level aggregate over a source stage (the test_database.py
        // shape): $listLocalSessions emits one synthetic doc, then the rest of
        // the pipeline reduces it to {dummy: "dummy field"}.
        let s = FakeStorage::seed("t", "c", vec![]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": 1, "pipeline": [
                {"$listLocalSessions": {}},
                {"$limit": 1},
                {"$addFields": {"dummy": "dummy field"}},
                {"$project": {"_id": 0, "dummy": 1}},
            ], "cursor": {}},
            &mut c,
        );
        let fb = first_batch(&reply);
        assert_eq!(fb.len(), 1);
        assert_eq!(
            fb[0].as_document().unwrap().get_str("dummy").unwrap(),
            "dummy field"
        );
    }

    #[test]
    fn aggregate_current_op_synthetic_shape() {
        let s = FakeStorage::seed("t", "c", vec![]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": 1, "pipeline": [{"$currentOp": {}}], "cursor": {}},
            &mut c,
        );
        let fb = first_batch(&reply);
        assert_eq!(fb.len(), 1);
        let row = fb[0].as_document().unwrap();
        assert_eq!(row.get_str("type").unwrap(), "op");
        assert_eq!(row.get_str("op").unwrap(), "command");
        // `command` echoes the aggregate request, with `$db` defaulted.
        let cmd = row.get_document("command").unwrap();
        assert!(cmd.contains_key("aggregate"));
        assert_eq!(cmd.get_str("$db").unwrap(), "t");
    }

    #[test]
    fn geo_near_sorts_by_distance_and_attaches_field() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![
                doc! {"_id": 1, "loc": [0.0, 0.0]},
                doc! {"_id": 2, "loc": [3.0, 4.0]},
                doc! {"_id": 3, "loc": [1.0, 0.0]},
            ],
        );
        let mut c = ctx(s);
        // Planar near at origin; ascending by distance → 1 (0), 3 (1), 2 (5).
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$geoNear": {"near": [0.0, 0.0], "key": "loc", "distanceField": "d"}}
            ], "cursor": {}},
            &mut c,
        );
        let out = docs_of(&reply);
        let ids: Vec<i32> = out.iter().map(|d| d.get_i32("_id").unwrap()).collect();
        assert_eq!(ids, vec![1, 3, 2]);
        let dists: Vec<f64> = out.iter().map(|d| d.get_f64("d").unwrap()).collect();
        assert_eq!(dists, vec![0.0, 1.0, 5.0]);
    }

    #[test]
    fn geo_near_max_distance_and_multiplier() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![
                doc! {"_id": 1, "loc": [0.0, 0.0]},
                doc! {"_id": 2, "loc": [3.0, 4.0]},
            ],
        );
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$geoNear": {"near": [0.0, 0.0], "key": "loc", "distanceField": "d",
                              "maxDistance": 2.0, "distanceMultiplier": 10.0}}
            ], "cursor": {}},
            &mut c,
        );
        let out = docs_of(&reply);
        // only _id:1 within maxDistance 2; distance 0 * 10 = 0.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get_i32("_id").unwrap(), 1);
        assert_eq!(out[0].get_f64("d").unwrap(), 0.0);
    }

    #[test]
    fn aggregate_changestream_standalone_rejected() {
        let s = FakeStorage::seed("t", "c", vec![]);
        let mut c = ctx(s); // replica_set_name is None ⇒ standalone
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$changeStream": {}}], "cursor": {}},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 40573);
        assert_eq!(reply.get_str("codeName").unwrap(), "IllegalOperation");
    }

    fn docs_of(reply: &Document) -> Vec<Document> {
        first_batch(reply)
            .iter()
            .map(|b| b.as_document().unwrap().clone())
            .collect()
    }

    #[test]
    fn lookup_simple_form_joins_foreign_docs() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![doc! {"_id": 1, "k": 10}, doc! {"_id": 2, "k": 20}],
        );
        s.add(
            "t",
            "o",
            vec![
                doc! {"_id": 100, "fk": 10, "v": "a"},
                doc! {"_id": 101, "fk": 10, "v": "b"},
                doc! {"_id": 102, "fk": 20, "v": "c"},
            ],
        );
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$lookup": {"from": "o", "localField": "k", "foreignField": "fk", "as": "j"}},
                {"$sort": {"_id": 1}}
            ], "cursor": {}},
            &mut c,
        );
        let out = docs_of(&reply);
        assert_eq!(out[0].get_array("j").unwrap().len(), 2);
        assert_eq!(out[1].get_array("j").unwrap().len(), 1);
        assert_eq!(
            out[1].get_array("j").unwrap()[0]
                .as_document()
                .unwrap()
                .get_str("v")
                .unwrap(),
            "c"
        );
    }

    #[test]
    fn lookup_pipeline_form_with_let_binding() {
        let s = FakeStorage::seed("t", "c", vec![doc! {"_id": 1, "k": 5}]);
        s.add(
            "t",
            "o",
            vec![doc! {"_id": 1, "n": 3}, doc! {"_id": 2, "n": 9}],
        );
        let mut c = ctx(s);
        // Inner pipeline keeps foreign docs whose n > the outer doc's k (=5).
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$lookup": {
                    "from": "o",
                    "let": {"kk": "$k"},
                    "pipeline": [{"$match": {"$expr": {"$gt": ["$n", "$$kk"]}}}],
                    "as": "j"
                }}
            ], "cursor": {}},
            &mut c,
        );
        let out = docs_of(&reply);
        let j = out[0].get_array("j").unwrap();
        assert_eq!(j.len(), 1);
        assert_eq!(j[0].as_document().unwrap().get_i32("n").unwrap(), 9);
    }

    #[test]
    fn sample_returns_requested_size_subset() {
        let docs: Vec<Document> = (0..10).map(|i| doc! {"_id": i}).collect();
        let s = FakeStorage::seed("t", "c", docs);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$sample": {"size": 3}}], "cursor": {}},
            &mut c,
        );
        let out = docs_of(&reply);
        assert_eq!(out.len(), 3);
        // every sampled _id is a real one (0..10), no dupes
        let mut ids: Vec<i32> = out.iter().map(|d| d.get_i32("_id").unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn sample_size_ge_len_returns_all() {
        let s = FakeStorage::seed("t", "c", vec![doc! {"_id": 1}, doc! {"_id": 2}]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$sample": {"size": 50}}], "cursor": {}},
            &mut c,
        );
        assert_eq!(docs_of(&reply).len(), 2);
    }

    #[test]
    fn coll_stats_reports_count_and_index_sizes() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![doc! {"_id": 1}, doc! {"_id": 2}, doc! {"_id": 3}],
        );
        s.add_index("t", "c", doc! {"name": "_id_", "key": {"_id": 1}});
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$collStats": {"storageStats": {}, "count": {}}}], "cursor": {}},
            &mut c,
        );
        let out = docs_of(&reply);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get_str("ns").unwrap(), "t.c");
        let ss = out[0].get_document("storageStats").unwrap();
        assert_eq!(ss.get_i64("count").unwrap(), 3);
        assert_eq!(ss.get_i32("nindexes").unwrap(), 1);
        assert!(ss.get_document("indexSizes").unwrap().contains_key("_id_"));
        assert_eq!(out[0].get_i64("count").unwrap(), 3);
    }

    #[test]
    fn index_stats_one_doc_per_index() {
        let s = FakeStorage::seed("t", "c", vec![doc! {"_id": 1}]);
        s.add_index("t", "c", doc! {"name": "_id_", "key": {"_id": 1}});
        s.add_index("t", "c", doc! {"name": "x_1", "key": {"x": 1}});
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$indexStats": {}}], "cursor": {}},
            &mut c,
        );
        let out = docs_of(&reply);
        assert_eq!(out.len(), 2);
        let names: Vec<&str> = out.iter().map(|d| d.get_str("name").unwrap()).collect();
        assert!(names.contains(&"_id_") && names.contains(&"x_1"));
    }

    #[test]
    fn out_replaces_target_collection() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![doc! {"_id": 1, "v": 1}, doc! {"_id": 2, "v": 2}],
        );
        // Pre-existing junk in the target must be wiped by $out.
        s.add("t", "dst", vec![doc! {"_id": 99, "stale": true}]);
        let mut c = ctx(s.clone());
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$out": "dst"}], "cursor": {}},
            &mut c,
        );
        // $out emits nothing downstream.
        assert_eq!(docs_of(&reply).len(), 0);
        let dst = s
            .cols
            .lock()
            .unwrap()
            .get(&("t".into(), "dst".into()))
            .unwrap()
            .clone();
        let mut ids: Vec<i32> = dst.iter().map(|d| d.get_i32("_id").unwrap()).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn merge_deep_merges_matched_and_inserts_unmatched() {
        let s = FakeStorage::seed(
            "t",
            "c",
            vec![
                doc! {"_id": 1, "a": {"x": 1}, "new": "f"},
                doc! {"_id": 2, "fresh": true},
            ],
        );
        s.add(
            "t",
            "dst",
            vec![doc! {"_id": 1, "a": {"y": 2}, "keep": "g"}],
        );
        let mut c = ctx(s.clone());
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$merge": {"into": "dst"}}], "cursor": {}},
            &mut c,
        );
        assert_eq!(docs_of(&reply).len(), 0);
        let dst = s
            .cols
            .lock()
            .unwrap()
            .get(&("t".into(), "dst".into()))
            .unwrap()
            .clone();
        let merged = dst.iter().find(|d| d.get_i32("_id") == Ok(1)).unwrap();
        // deep merge: existing a.y kept, new a.x added, keep retained, new added.
        let a = merged.get_document("a").unwrap();
        assert_eq!(a.get_i32("x").unwrap(), 1);
        assert_eq!(a.get_i32("y").unwrap(), 2);
        assert_eq!(merged.get_str("keep").unwrap(), "g");
        assert_eq!(merged.get_str("new").unwrap(), "f");
        // unmatched _id:2 inserted.
        assert!(dst.iter().any(|d| d.get_i32("_id") == Ok(2)));
    }

    #[test]
    fn merge_keep_existing_skips_matched() {
        let s = FakeStorage::seed("t", "c", vec![doc! {"_id": 1, "v": "new"}]);
        s.add("t", "dst", vec![doc! {"_id": 1, "v": "old"}]);
        let mut c = ctx(s.clone());
        dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$merge": {"into": "dst", "whenMatched": "keepExisting"}}
            ], "cursor": {}},
            &mut c,
        );
        let dst = s
            .cols
            .lock()
            .unwrap()
            .get(&("t".into(), "dst".into()))
            .unwrap()
            .clone();
        assert_eq!(dst[0].get_str("v").unwrap(), "old");
    }

    #[test]
    fn aggregate_batches_into_cursor() {
        let docs: Vec<Document> = (0..5).map(|i| doc! {"_id": i}).collect();
        let s = FakeStorage::seed("t", "c", docs);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [{"$sort": {"_id": 1}}], "cursor": {"batchSize": 2}},
            &mut c,
        );
        let cursor = reply.get_document("cursor").unwrap();
        assert_eq!(cursor.get_array("firstBatch").unwrap().len(), 2);
        assert_ne!(cursor.get_i64("id").unwrap(), 0, "remaining ⇒ live cursor");
    }
}
