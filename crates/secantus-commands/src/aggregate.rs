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

use crate::argtypes;
use crate::find::split_docs_into_cursor_checked;
use crate::util::{
    as_i64, bool_field, command_error, decode_docs, decode_docs_minimal, encode_docs,
};
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE};
use secantus_core::collation::Collation;

/// Refuse a pipeline that reads or writes a view whose collation is not the
/// one the command runs under (`crate::views::involved_view_collation_problem`).
/// A malformed collation, or one the command's own view refuses, is left to
/// the check that reports it later, so its error keeps its place.
pub(crate) fn involved_view_collation(
    doc: &Document,
    ctx: &CommandContext,
    coll: Option<&str>,
    pipeline: &[Bson],
) -> Result<(), CommandError> {
    let Ok(storage) = ctx.storage() else {
        return Ok(());
    };
    let collation = match coll {
        Some(c) => crate::util::effective_collation(storage, &ctx.db_name, c, doc),
        None => crate::util::requested_collation(doc).map(|r| r.named().flatten()),
    };
    let Ok(collation) = collation else {
        return Ok(());
    };
    match crate::views::involved_view_collation_problem(
        storage,
        &ctx.db_name,
        pipeline,
        collation.as_ref(),
    ) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// `aggregate` — run a pipeline and return a cursor over the results.
pub fn aggregate(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // `cursor` means "missing or an object" literally: an explicit `cursor: null`
    // is rejected where an absent one is fine. `let` is the BSON-field family.
    argtypes::require_cursor_object(doc)?;
    // `cursor` is required unless the command is an inline explain (measured
    // 8.2.11, 2026-10-01); an aggregate without one used to run and return a
    // cursor anyway.
    if let Some(other) = doc.get("explain") {
        if !matches!(other, Bson::Boolean(_)) {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                "explain must be a boolean",
            ));
        }
    }
    if !doc.contains_key("cursor") && doc.get("explain") != Some(&Bson::Boolean(true)) {
        return Err(CommandError::new(
            9,
            "FailedToParse",
            "The 'cursor' option is required, except for aggregate with the explain argument",
        ));
    }
    argtypes::require_object(doc, "let", "aggregate.let")?;
    argtypes::require_object(doc, "collation", "aggregate.collation")?;
    argtypes::require_object(doc, "readConcern", "aggregate.readConcern")?;
    argtypes::require_hint(doc, "hint")?;
    // The `Field '<name>' should be a boolean value` family, which rejects an
    // explicit null — not the BSON-field family used two lines up.
    argtypes::require_bool_value(doc, "allowDiskUse")?;
    // `pipeline` is checked before it is READ: the read below falls back to an
    // empty pipeline for a non-array, which silently ran the whole collection
    // through no stages and answered ok.
    match doc.get("pipeline") {
        None | Some(Bson::Array(_)) => {}
        Some(_) => {
            return Ok(CommandError::new(
                14,
                "TypeMismatch",
                "A pipeline must be an array of objects",
            )
            .into_reply())
        }
    }
    argtypes::require_cursor_options(doc, "cursor")?;
    const FIELDS: &[&str] = &[
        "aggregate",
        "pipeline",
        "explain",
        "allowDiskUse",
        "cursor",
        "bypassDocumentValidation",
        "collation",
        "hint",
        "let",
        "needsMerge",
        "fromMongos",
        "fromRouter",
        "isMapReduceCommand",
        "collectionUUID",
        "exchange",
        "runtimeConstants",
        "legacyRuntimeConstants",
        "querySettings",
        "passthroughToShard",
        "encryptionInformation",
        "sampleId",
        "use44SortKeys",
        "useNewUpsert",
        "isClusterQueryWithoutShardKeyCmd",
        "includeQueryStatsMetrics",
        "requestReshardingResumeToken",
        "requestResumeToken",
        "resumeAfter",
        "startAt",
        "rawData",
        "translatedForViewlessTimeseries",
        "mirrored",
    ];
    if let Some(key) = argtypes::unknown_field(doc, FIELDS) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'aggregate.{key}' is an unknown field."),
        ));
    }
    // `aggregate: <coll>` (string) or `aggregate: 1` (collectionless).
    let coll = match doc.get("aggregate") {
        Some(Bson::String(s)) if s.is_empty() => {
            return Err(CommandError::new(
                73,
                "InvalidNamespace",
                format!("Invalid namespace specified: {}", ctx.db_name),
            ))
        }
        Some(Bson::String(s)) => Some(s.clone()),
        Some(v) if as_i64(v) == Some(1) && !matches!(v, Bson::Boolean(_)) => None,
        _ => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "Invalid command format: the 'aggregate' field must specify a collection name \
                 or 1",
            ))
        }
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
    // `$score`, `$rankFusion` and `$scoreFusion`, as the stages that run them.
    let pipeline = if crate::hybrid::mentions_hybrid(&pipeline) {
        match crate::hybrid::prepare(pipeline, coll.as_deref()) {
            Ok(p) => p,
            Err(e) => return Ok(e.into_reply()),
        }
    } else {
        pipeline
    };
    if let Err(e) = crate::list_catalog::spec_problem(&pipeline) {
        return Ok(e.into_reply());
    }
    if let Err(e) = source_stage_problem(&pipeline, &ctx.db_name, coll.as_deref()) {
        return Ok(e.into_reply());
    }
    if let Err(e) = crate::list_catalog::place_problem(&pipeline, &ctx.db_name, coll.as_deref()) {
        return Ok(e.into_reply());
    }
    crate::list_catalog::set_member(ctx.replica_set_name.is_some());
    let seen = ctx
        .server_params
        .as_ref()
        .map(|p| p.sessions())
        .unwrap_or_default();
    let caller = crate::session::user_digest(&crate::session::authenticated(ctx));
    SESSIONS.with(|sessions| *sessions.borrow_mut() = (seen, caller));
    // `$where` in a `$match` is refused with the aggregation-context code on
    // every mongod, script engine or not -- and it must be refused before the
    // leading-`$match` lift hands the filter to the query matcher.
    if let Err(e) = validate_no_where_in_match(&pipeline) {
        return Ok(e.into_reply());
    }
    // A missing REQUIRED argument is a parse error too, and mongod names the
    // stage the same way. Must precede the check below, which only knows about
    // unknown operator names.
    if let Err(e) = validate_stage_expr_args(&pipeline) {
        return Ok(e.into_reply());
    }
    // A genuinely unknown expression operator inside a `$project` spec is
    // mongod's stage-specific Location31325, not the generic BadValue the
    // engine fallback produces. Parse-time, like the stage-name check.
    if let Err(e) = validate_project_exprs(&pipeline) {
        return Ok(e.into_reply());
    }
    // Everywhere the projection parser does NOT own, an unknown operator is
    // mongod's 168 with that position's envelope -- not the blanket "not
    // supported by the Rust server", which told the client the server cannot do
    // `$addFields` when the operator inside it was the problem.
    if let Err(e) = validate_unknown_exprs(&pipeline) {
        return Ok(e.into_reply());
    }
    // An undefined `$$variable` is a PARSE error for mongod: it fires on an
    // EMPTY collection, where nothing is ever evaluated, so no amount of engine
    // work could produce it. Checked here, once, before the pipeline runs.
    {
        let bound: Vec<String> = match doc.get("let") {
            Some(Bson::Document(d)) => d.keys().cloned().collect(),
            _ => Vec::new(),
        };
        if let Some((code, msg, stage)) =
            argtypes::expression_problem_in_pipeline(&pipeline, &bound)
        {
            let errmsg = argtypes::wrap_expression_problem(&msg, &stage);
            return Ok(
                CommandError::new(code, crate::util::error_code_name(code), errmsg).into_reply(),
            );
        }
    }
    // A wrong-TYPED stage spec: name it with mongod's own code rather than
    // letting the engine's generic `Fallback` surface as BadValue (2). Seven
    // stages, seven codes -- see `crate::argtypes::stage_spec_error`.
    if let Some((code, errmsg)) = argtypes::stage_spec_error(&pipeline) {
        // `error_code_name` knows 9 is FailedToParse; the rest are mongod's
        // anonymous `Location<n>` codes.
        let code_name = crate::util::error_code_name(code);
        return Ok(CommandError::new(code, code_name, errmsg).into_reply());
    }
    // Every `$sort` spec is valid by now; hand the engine plain 1 / -1, which
    // is all it reads (a decimal or fractional direction was "unsupported").
    if let Some((code, errmsg)) = argtypes::nested_sort_problem(&pipeline) {
        let code_name = crate::util::error_code_name(code);
        return Ok(CommandError::new(code, code_name, errmsg).into_reply());
    }
    let mut pipeline = pipeline;
    argtypes::normalise_pipeline_sorts(&mut pipeline);
    // An operator the query language does not have is a parse error, whether
    // or not the collection has a document to test it on.
    for stage in pipeline.iter().filter_map(Bson::as_document) {
        if let Some(Bson::Document(filter)) = stage.get("$match") {
            if let Some((code, msg)) = argtypes::unknown_operator_in_filter(filter) {
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    msg,
                ));
            }
            // So is a constant `$expr` sub-expression that fails (RM-176).
            if let Some((code, msg)) =
                secantus_core::expr_rewrite::constant_expr_error_in_filter(filter)
            {
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    format!("Failed to optimize pipeline :: caused by :: {msg}"),
                ));
            }
        }
    }

    // Inline `explain: true` on the aggregate command (the legacy flag, distinct
    // from the top-level `explain` wrapper): return the plan instead of running
    // the pipeline, and crucially do NOT execute `$out` / `$merge`. Delegate to
    // the explain handler before any write stage runs; default verbosity is
    // `queryPlanner`. Mirrors `commands.py::_aggregate`.
    if doc.get("explain") == Some(&Bson::Boolean(true)) {
        involved_view_collation(doc, ctx, coll.as_deref(), &pipeline)?;
        let verbosity = doc
            .get_str("verbosity")
            .unwrap_or("queryPlanner")
            .to_string();
        // `$inline` tells the handler the flag came from the aggregate itself.
        let wrapped = doc! { "explain": doc.clone(), "verbosity": verbosity, "$inline": true };
        return crate::explain::explain(&wrapped, ctx);
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

    // `$geoNear` reads the index itself: nothing may come before it.
    if pipeline
        .iter()
        .skip(1)
        .any(|stage| stage_name(stage) == "$geoNear")
    {
        return Err(CommandError::new(
            40603,
            "Location40603",
            "$geoNear was not the first stage in the pipeline after optimization. Is \
             optimization disabled or inhibited?",
        ));
    }
    if let Some(problem) = nested_write_stage(&pipeline) {
        return Ok(problem.into_reply());
    }
    involved_view_collation(doc, ctx, coll.as_deref(), &pipeline)?;

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
            let stage = pipeline.last().map(stage_name).unwrap_or("$out");
            return Ok(CommandError::new(
                72,
                "InvalidOptions",
                format!(
                    "Command aggregate does not support {{ readConcern: {{ level: \
                     \"linearizable\", provenance: \"clientSupplied\" }} }} :: caused by :: \
                     {stage} cannot be used with a 'linearizable' read concern level"
                ),
            )
            .into_reply());
        }
    }

    // `$changeStream` anywhere but first, and the split stage without it.
    for (i, stage) in pipeline.iter().enumerate() {
        let Some(d) = stage.as_document() else {
            continue;
        };
        if let Some(first_only) = ["$collStats", "$indexStats"]
            .iter()
            .find(|name| i > 0 && d.contains_key(**name))
        {
            return Ok(CommandError::new(
                40602,
                "Location40602",
                format!("{first_only} is only valid as the first stage in a pipeline"),
            )
            .into_reply());
        }
        if i > 0 && d.contains_key("$changeStream") {
            return Ok(CommandError::new(
                40602,
                "Location40602",
                "$_internalChangeStreamOplogMatch is only valid as the first stage in a pipeline",
            )
            .into_reply());
        }
        if d.contains_key("$changeStreamSplitLargeEvent")
            && !first_stage_has(&pipeline, "$changeStream")
        {
            return Ok(CommandError::new(
                20,
                "IllegalOperation",
                "$changeStreamSplitLargeEvent can only be used in a $changeStream pipeline",
            )
            .into_reply());
        }
    }
    // A change stream cannot be opened on a time-series collection.
    if first_stage_has(&pipeline, "$changeStream") {
        if let (Some(c), Ok(storage)) = (&coll, ctx.storage()) {
            if storage
                .get_collection_options(&ctx.db_name, c)
                .is_ok_and(|o| o.contains_key("timeseries"))
            {
                return Ok(CommandError::new(
                    166,
                    "CommandNotSupportedOnView",
                    format!(
                        "Cannot run aggregation on timeseries with namespace {}.{c}",
                        ctx.db_name
                    ),
                )
                .into_reply());
            }
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
    // A standalone has no cluster time for `$$CLUSTER_TIME` to read.
    if let Some(e) =
        crate::util::cluster_time_unavailable(ctx.replica_set_name.is_none(), &pipeline)
    {
        return Err(e);
    }
    // A leading `$match` on `$text` needs a text index, which this server
    // cannot search: mongod's 27 (measured 8.2.11).
    if let (Some(c), Some(Bson::Document(m))) = (
        &coll,
        pipeline
            .first()
            .and_then(Bson::as_document)
            .and_then(|d| d.get("$match")),
    ) {
        if let Some((code, errmsg)) = crate::crud::text_query_problem(storage, &ctx.db_name, c, m) {
            return Err(CommandError::new(
                code,
                crate::util::error_code_name(code),
                errmsg,
            ));
        }
    }
    let batch_size = doc
        .get("cursor")
        .and_then(Bson::as_document)
        .and_then(|c| c.get("batchSize"))
        .and_then(as_i64)
        .unwrap_or(DEFAULT_BATCH_SIZE as i64);
    // Command `let` → resolved query vars ($$NOW seeded, values evaluated),
    // visible to `$expr` in `$match` and to pipeline expressions.
    let vars = crate::util::command_let_vars(doc.get("let"), ctx);
    // The collation the command names, else the default of the namespace it
    // names -- a VIEW's own, never its base collection's.
    let collation = match &coll {
        Some(c) => crate::util::effective_collation(storage, &ctx.db_name, c, doc)?,
        None => crate::util::requested_collation(doc)?.named().flatten(),
    };
    let _active = secantus_core::collation::activate(collation.as_ref());
    if let Some(c) = &coll {
        let filter = crate::explain::pipeline_canonical(c, &pipeline).filter;
        crate::timeseries::check_hint(storage, &ctx.db_name, c, &filter, hint)?;
    }

    // `$indexStats` through a view reports the indexes of the collection
    // beneath it (measured 8.2.11).
    let mut index_stats_base: Option<String> = None;
    // The pipeline's first read of its collection itself (not through a
    // view): its filter, sort and limit, for the indexes its plan uses.
    let mut first_read_of_coll: Option<(Document, Option<Document>, i64)> = None;
    // The stages of the pipeline, a view's in front.
    let mut read_pipeline: Vec<Bson> = Vec::new();
    // Fetch the input documents + decide the remaining pipeline.
    let (ns, input, working_pipeline): (String, Vec<Document>, Vec<Bson>) = match &coll {
        Some(c) => {
            let ns = format!("{}.{}", ctx.db_name, c);
            // Resolve a view to its base collection + prepended view pipeline
            // (recursively for a view-on-a-view). The reply ns keeps the queried
            // (view) name; only the fetch reads the base collection. Mirrors
            // commands._resolve_view.
            // `$listCatalog` reads the catalog, not the documents: a
            // namespace this server derives (`system.views`, a time-series
            // collection's buckets) is listed as the collection mongod
            // keeps, not read as the view it is here.
            let derived = first_stage_has(&pipeline, crate::list_catalog::STAGE)
                && (c == "system.views"
                    || crate::ts_buckets::bucketed(storage, &ctx.db_name, c).is_some());
            let asked: &str = c;
            let (base_coll, pipeline) = if derived {
                (c.clone(), pipeline)
            } else {
                resolve_view(storage, &ctx.db_name, c, pipeline)?
            };
            let c = base_coll.as_str();
            // A view's own stages go in front of the caller's, so a
            // `$indexStats` through a view with a pipeline is no longer the
            // first stage: mongod answers 40602 (measured 8.2.11).
            if pipeline
                .iter()
                .skip(1)
                .filter_map(Bson::as_document)
                .any(|d| d.contains_key("$indexStats"))
            {
                return Err(CommandError::new(
                    40602,
                    "Location40602",
                    "$indexStats is only valid as the first stage in a pipeline",
                ));
            }
            if base_coll != asked && first_stage_has(&pipeline, "$indexStats") {
                index_stats_base = Some(base_coll.clone());
            }
            // A view's stages go in front of a `$listCatalog`.
            crate::list_catalog::place_problem(&pipeline, &ctx.db_name, Some(c))?;
            // Stages that generate / read their own input start from no docs.
            if first_stage_has_any(
                &pipeline,
                &[
                    "$collStats",
                    "$indexStats",
                    "$documents",
                    crate::ts_buckets::BUCKETS_STAGE,
                    crate::ts_buckets::VIEWS_STAGE,
                ],
            ) {
                (ns, Vec::new(), pipeline)
            } else if first_stage_has(&pipeline, crate::list_catalog::STAGE) {
                // Through a view, the entry of the collection beneath it.
                let entries = crate::list_catalog::entries(storage, &ctx.db_name, Some(c))?;
                (ns, entries, pipeline[1..].to_vec())
            } else {
                if hint.is_some() || crate::notablescan::enabled(ctx) {
                    let q = crate::explain::pipeline_canonical(c, &pipeline);
                    let read = crate::notablescan::Read {
                        coll: c,
                        filter: &q.filter,
                        sort: q.sort.as_ref(),
                        hint,
                        collation: collation.as_ref(),
                        shown: crate::notablescan::Shown {
                            projection: q.projection.as_ref(),
                            skip: Some(q.skip),
                            limit: Some(q.limit),
                            ..crate::notablescan::Shown::aggregate(collation.as_ref())
                        },
                    };
                    read.check_hint(storage, &ctx.db_name)?;
                    crate::notablescan::check(ctx, &read)?;
                }
                // Lift a leading $match into the fetch filter (dropping it from
                // the pipeline). A leading bounded $geoNear instead rides its geo
                // index via a conservative $geoWithin candidate fetch — the stage
                // is kept (it re-applies the exact distance filter, so the output
                // is identical; only the fetched set shrinks). Mirrors the Python
                // `_geo_near_index_filter` lift.
                // A hint must name an index the leading `$geoNear` reads.
                if let (Some(spec), Some(hint)) = (
                    pipeline
                        .first()
                        .and_then(Bson::as_document)
                        .and_then(|d| d.get("$geoNear")),
                    hint,
                ) {
                    check_geo_near_hint(spec, hint, &ctx.db_name, c, storage)?;
                }
                read_pipeline = pipeline.clone();
                let geonear_candidate = pipeline
                    .first()
                    .and_then(Bson::as_document)
                    .and_then(|d| d.get("$geoNear"))
                    .and_then(|spec| geo_near_index_filter(spec, &ctx.db_name, Some(c), storage));
                let (filter, rest) = match geonear_candidate {
                    Some(cand) => (cand, pipeline),
                    None => lift_leading_match(&crate::explain::hoist::hoist_matches(&pipeline)),
                };
                // A `$sort` at the front is still run by the pipeline, which
                // keeps equal documents in the order they arrive: reading them
                // in the order mongod's plan returns them is what puts its
                // ties in mongod's order. mongod moves a `$match` that follows
                // the `$sort` ahead of it, so that filter is read here too.
                if crate::near::find_near(&filter)
                    .map_err(command_error)?
                    .is_some()
                {
                    return Err(CommandError::new(
                        5626500,
                        "Location5626500",
                        "$geoNear, $near, and $nearSphere are not allowed in this context, as \
                         these operators require sorting geospatial data. If you do not need \
                         sort, consider using $geoWithin instead. Check out \
                         https://dochub.mongodb.org/core/near-sort-operation and \
                         https://dochub.mongodb.org/core/nearSphere-sort-operationfor more \
                         details.",
                    ));
                }
                let lead = leading_sort(&rest, filter.is_empty());
                let read = |sort: Option<&Document>, limit: i64| {
                    let moved = lead.filter.as_ref().filter(|_| sort.is_some());
                    crate::explain::order::find(
                        storage,
                        &crate::explain::order::Ask {
                            db: &ctx.db_name,
                            coll: c,
                            filter: moved.unwrap_or(&filter),
                            sort,
                            hint,
                            collation: collation.as_ref(),
                            vars: &vars,
                            limit,
                            asked: Default::default(),
                        },
                    )
                };
                let bytes = match lead.sort.as_ref() {
                    // A sort or filter the read refuses is the pipeline's
                    // to report: read as if neither had been moved.
                    Some(sort) => read(Some(sort), lead.limit).or_else(|_| read(None, 0)),
                    None => read(None, 0),
                }
                .map_err(|e| {
                    crate::util::read_error_on_namespace(
                        e,
                        "aggregate",
                        &format!("{}.{c}", ctx.db_name),
                    )
                })?;
                // The plan the pipeline's first read runs uses its indexes
                // once (a `$lookup`'s reads count none: measured 8.2.11).
                let first_read = crate::explain::order::Ask {
                    db: &ctx.db_name,
                    coll: c,
                    filter: lead
                        .filter
                        .as_ref()
                        .filter(|_| lead.sort.is_some())
                        .unwrap_or(&filter),
                    sort: lead.sort.as_ref(),
                    hint,
                    collation: collation.as_ref(),
                    vars: &vars,
                    limit: lead.limit,
                    asked: Default::default(),
                };
                crate::explain::order::count_index_use(
                    storage,
                    &first_read,
                    crate::explain::order::Uses::Find,
                );
                // A cursor left open over that read is killed when one of its
                // indexes is dropped, as a `find` cursor is (mongod 8.2.11:
                // 175 on the `getMore`). Only when the cursor's namespace is
                // the collection the index belongs to: through a view the
                // cursor names the view.
                if c == asked {
                    first_read_of_coll = Some((
                        first_read.filter.clone(),
                        first_read.sort.cloned(),
                        first_read.limit,
                    ));
                }
                // A time-series collection scans bucket by bucket.
                let bytes = crate::ts_buckets::as_read(storage, &ctx.db_name, c, bytes, false)?;
                // Reduce the leading $skip/$limit/$match prefix over raw BSON so
                // only the survivors that reach the first heavier stage are
                // decoded (tasks/rust-perf-findings.md).
                let (reduced, remaining) =
                    reduce_raw_prefix(bytes, &rest, &vars, collation.as_ref());
                // `{$meta: "recordId"}` / `{$meta: "indexKey"}` where mongod
                // answers them: read from fields the documents are given here.
                let mut remaining = remaining;
                let metas = crate::agg_meta::rewrite(&read_pipeline, &mut remaining);
                // `{$meta: "sortKey"}` and the `$geoNear` metadata, from the
                // stage that sets them.
                crate::agg_meta::rewrite_sort_metadata(&mut remaining);
                let record_ids = if metas.record_id {
                    let refs: Vec<&[u8]> = reduced.iter().map(Vec::as_slice).collect();
                    storage
                        .record_ids_of(&ctx.db_name, c, &refs)
                        .map_err(command_error)?
                } else {
                    None
                };
                let index_keys = if metas.index_key {
                    let full = decode_docs(reduced.clone())?;
                    crate::explain::order::index_keys(storage, &first_read, &full)
                } else {
                    None
                };
                // If the pipeline reaches a `$group` / `$count` through stages
                // that only pass documents along (`$unwind`, `$sort`, ...),
                // decode only the top-level fields those stages read from each
                // survivor, not the whole (often wide) document
                // (tasks/rust-phase6-streaming-agg-scoping.md, 6a). Any shape
                // the collector can't bound falls back to a full decode.
                let group_fields = secantus_core::pipeline_prefix_fields(&remaining);
                let mut input = match group_fields {
                    Some(fields) => decode_docs_minimal(reduced, &fields)?,
                    None => decode_docs(reduced)?,
                };
                if metas.record_id || metas.index_key {
                    crate::agg_meta::attach(
                        &mut input,
                        record_ids.as_deref(),
                        index_keys.as_deref(),
                    );
                }
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

    // A stage that reads another collection is held to `notablescan` too.
    let _notablescan = crate::notablescan::aggregate(ctx, &ns);
    // The connection's handshake `client` doc, for `$currentOp`'s appName /
    // clientMetadata (read before the borrow of `ctx` below).
    let client_metadata: Option<Document> = ctx
        .conn_auth
        .as_ref()
        .and_then(|a| a.lock().ok())
        .and_then(|g| g.client_metadata.clone());
    let _command = InCommand::enter(&ns);
    let result = run_segmented(
        input,
        &working_pipeline,
        &ctx.db_name,
        index_stats_base.as_deref().or(coll.as_deref()),
        &vars,
        storage,
        collation.as_ref(),
        doc,
        client_metadata.as_ref(),
    )?;

    // The pipeline result is already decoded `Document`s. Send the `firstBatch`
    // straight to the wire as `Bson` and encode only the cursor remainder for the
    // registry — no encode→decode round-trip on the docs the client gets now.
    let prefix = format!("Executor error during aggregate command on namespace: {ns}");
    let (first_batch, mut cursor_id) =
        split_docs_into_cursor_checked(result, batch_size, &ns, cursors, false, Some(&prefix))?;
    // `batchSize: 0` asks for the cursor and no documents, and mongod opens
    // one even over nothing -- a missing collection included.
    if batch_size == 0 && cursor_id == 0 {
        cursor_id = crate::find::open_cursor(cursors, &ns, Vec::new(), false)?;
    }
    // A cursor that still holds the executor of the pipeline's first read
    // ends when an index that read uses is dropped (mongod 8.2.11: 175 on
    // the `getMore`); one whose executor was read to the end does not.
    if let (true, Some((filter, sort, limit)), Some(c)) =
        (cursor_id != 0, first_read_of_coll.as_ref(), coll.as_deref())
    {
        let plan_indexes = crate::explain::order::plan_indexes(
            storage,
            &crate::explain::order::Ask {
                db: &ctx.db_name,
                coll: c,
                filter,
                sort: sort.as_ref(),
                hint,
                collation: collation.as_ref(),
                vars: &vars,
                limit: *limit,
                asked: Default::default(),
            },
        );
        if !plan_indexes.is_empty()
            && crate::agg_executor::holds_executor(ctx, doc, c, &read_pipeline)
        {
            ctx.cursors()?.set_plan_indexes(cursor_id, plan_indexes);
        }
    }
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
            | crate::ts_buckets::BUCKETS_STAGE
            | crate::ts_buckets::VIEWS_STAGE
    )
}

/// Where the stages that read no collection may stand, and what each takes
/// (mongod 8.2.11): `{aggregate: 1}` needs a first stage that makes its own
/// documents; `$currentOp` runs only so, against `admin`; `$listLocalSessions`
/// only so; `$listSessions` only over `config.system.sessions`; each is first
/// or not at all, and takes a document of the options it knows.
fn source_stage_problem(
    pipeline: &[Bson],
    db: &str,
    coll: Option<&str>,
) -> Result<(), CommandError> {
    let namespace = |message: String| CommandError::new(73, "InvalidNamespace", message);
    let first = pipeline
        .first()
        .and_then(Bson::as_document)
        .and_then(|stage| stage.keys().next())
        .map(String::as_str);
    if coll.is_none() {
        match first {
            None => {
                return Err(namespace(
                    "{aggregate: 1} is not valid for an empty pipeline.".into(),
                ))
            }
            Some(
                "$currentOp" | "$listLocalSessions" | "$documents" | "$changeStream"
                | "$listCatalog" | "$queryStats" | "$querySettings",
            ) => {}
            Some(other) => {
                return Err(namespace(format!(
                    "{{aggregate: 1}} is not valid for '{other}'; a collection is required."
                )))
            }
        }
    }
    for (i, stage) in pipeline.iter().enumerate() {
        let Some((name, spec)) = stage.as_document().and_then(|d| d.iter().next()) else {
            continue;
        };
        let known: &[&str] = match name.as_str() {
            "$currentOp" => &[
                "allUsers",
                "idleConnections",
                "idleCursors",
                "idleSessions",
                "localOps",
                "truncateOps",
                "targetAllNodes",
            ],
            "$listLocalSessions" | "$listSessions" => &["allUsers", "users", "$_internalPredicate"],
            _ => continue,
        };
        let Bson::Document(options) = spec else {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "{name} options must be specified in an object, but found: {}",
                    secantus_core::query::bson_type_name(spec)
                ),
            ));
        };
        match (name.as_str(), coll) {
            ("$currentOp", Some(_)) => {
                return Err(namespace(
                    "$currentOp must be run against the 'admin' database with {aggregate: 1}"
                        .into(),
                ))
            }
            ("$currentOp", None) if db != "admin" => {
                return Err(namespace(
                    "$currentOp must be run against the 'admin' database with {aggregate: 1}"
                        .into(),
                ))
            }
            ("$listLocalSessions", Some(_)) => {
                return Err(namespace(
                    "$listLocalSessions must be run against the database with {aggregate: 1}, \
                     not a collection"
                        .into(),
                ))
            }
            ("$listSessions", _) if (db, coll) != ("config", Some("system.sessions")) => {
                return Err(namespace(
                    "$listSessions may only be run against config.system.sessions".into(),
                ))
            }
            _ => {}
        }
        if i > 0 {
            return Err(CommandError::new(
                40602,
                "Location40602",
                format!("{name} is only valid as the first stage in a pipeline"),
            ));
        }
        for (option, value) in options {
            if !known.contains(&option.as_str()) {
                return Err(CommandError::new(
                    40415,
                    "IDLUnknownField",
                    format!("BSON field '{name}.{option}' is an unknown field."),
                ));
            }
            if name == "$currentOp" && !matches!(value, Bson::Boolean(_)) {
                return Err(CommandError::new(
                    14,
                    "TypeMismatch",
                    format!(
                        "The '{option}' parameter of the $currentOp stage must be a boolean \
                         value, but found: {}",
                        secantus_core::query::bson_type_name(value)
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// The sessions the server has seen (id, last use, whose), and the
/// calling user's digest.
type SeenSessions = (Vec<(Vec<u8>, bson::DateTime, Vec<u8>)>, Vec<u8>);

thread_local! {
    /// The sessions the server has seen, for the pipeline this thread runs.
    static SESSIONS: std::cell::RefCell<SeenSessions> =
        const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// What the session stages list: the sessions commands have come in and
/// that have not timed out, as mongod shows one
/// (`{_id: {id, uid}, lastUse}`), each `uid` the SHA-256 of the `user@db`
/// that used it. As on mongod 8.2.11 (an auth-enabled replica set,
/// measured 2026-10-16), `{}` lists the caller's own sessions,
/// `{allUsers: true}` everyone's and `{users: [{user, db}, ...]}` those
/// users'. Without authentication every session is the empty user's.
fn known_sessions(spec: Option<&Bson>) -> Vec<Document> {
    let spec = spec.and_then(Bson::as_document);
    let all = spec.is_some_and(|s| crate::util::truthy(s.get("allUsers").unwrap_or(&Bson::Null)));
    let named: Option<Vec<Vec<u8>>> = spec.and_then(|s| s.get_array("users").ok()).map(|users| {
        users
            .iter()
            .filter_map(Bson::as_document)
            .filter_map(|u| Some((u.get_str("db").ok()?, u.get_str("user").ok()?)))
            .map(|(db, user)| crate::session::user_digest(&[(db.to_string(), user.to_string())]))
            .collect()
    });
    SESSIONS.with(|sessions| {
        let (seen, caller) = &*sessions.borrow();
        seen.iter()
            .filter(|(_, _, uid)| {
                all || named
                    .as_ref()
                    .map_or(uid == caller, |users| users.contains(uid))
            })
            .map(|(id, used, uid)| {
                doc! {
                    "_id": {
                        "id": bson::Binary {
                            subtype: bson::spec::BinarySubtype::Uuid,
                            bytes: id.clone(),
                        },
                        "uid": bson::Binary {
                            subtype: bson::spec::BinarySubtype::Generic,
                            bytes: uid.clone(),
                        },
                    },
                    "lastUse": *used,
                }
            })
            .collect()
    })
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

/// mongod's error when a `pipeline` element is not a BSON document. Code 14
/// (TypeMismatch); libmongoc's `/change_stream/accepts_array` asserts on the
/// wording verbatim. Probed identical on mongod 6.0.16 and 8.3.4.
pub const PIPELINE_ELEMENT_MSG: &str = "Each element of the 'pipeline' array must be an object";

/// mongod's error when a stage *is* a document but isn't a single
/// `{operator: spec}` pair -- both an empty `{}` and a multi-key stage. Note
/// the trailing period, and that it is Location40323, not the generic 14.
pub const STAGE_ARITY_MSG: &str =
    "A pipeline stage specification object must contain exactly one field.";

pub const SEARCH_INDEX_ATLAS_MSG: &str = "Using Atlas Search Database Commands and the \
$listSearchIndexes aggregation stage requires additional configuration. Please connect to Atlas \
or an Atlas-compatible deployment to use this feature.";

/// Whether `name` is a recognised aggregation stage (the set the Python
/// `aggregate._STAGES` registry recognises). Keep in sync with that set.
pub(crate) fn recognized_stage(name: &str) -> bool {
    matches!(
        name,
        "$addFields"
            | "$rankFusion"
            | "$score"
            | "$scoreFusion"
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
            | "$listCatalog"
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
/// Reject an operator argument document that omits a required field, at PARSE
/// time and with mongod's stage wrapper.
///
/// mongod validates a stage's expressions before reading any document, and
/// wraps what it finds as `Invalid $addFields :: caused by :: <message>` --
/// also for `$project` and `$set`. The SAME error inside `$group`, `$match`'s
/// `$expr` or `$redact` is reported BARE (measured 8.2.11, 2026-09-07), which
/// is why only these three stages are walked here; the evaluator carries the
/// same check for every other context and produces the bare form.
fn validate_stage_expr_args(pipeline: &[Bson]) -> Result<(), CommandError> {
    const WRAPPED_STAGES: [&str; 3] = ["$addFields", "$project", "$set"];
    for stage in pipeline {
        let Some(d) = stage.as_document() else {
            continue;
        };
        for name in WRAPPED_STAGES {
            let Some(spec) = d.get(name) else { continue };
            if let Err(f) = secantus_core::expressions::validate_expression_args(spec) {
                if let Some((code, message)) = f.as_mongo() {
                    return Err(CommandError::new(
                        code,
                        crate::util::error_code_name(code),
                        format!("Invalid {name} :: caused by :: {message}"),
                    ));
                }
            }
        }
    }
    Ok(())
}

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
            // TOP LEVEL only. This used to recurse, which gave 31325 to a
            // nested unknown as well -- mongod answers 168 there, because the
            // projection parser owns only the field's own value and anything
            // deeper belongs to the generic expression parser. Measured 8.2.11,
            // 2026-09-17; `validate_unknown_exprs` picks up the nested case.
            if let Some(op) = secantus_core::expressions::top_level_unknown_expr_operator(value) {
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

/// An unknown expression operator anywhere the PROJECTION parser does not own,
/// reported as mongod's `168` with the envelope that position carries.
///
/// Runs after [`validate_project_exprs`], which takes the top-level value of a
/// `$project` field (its own `31325`). Everything else is the generic
/// expression parser's, and mongod's envelope depends on the stage (probed
/// 8.2.11, 2026-09-17):
///
/// ```text
/// $project / $addFields / $set   Invalid $<stage> :: caused by :: Unrecognized expression '$x'
/// $group / $replaceWith / $expr  Unrecognized expression '$x'        -- no envelope at all
/// ```
///
/// Only `$group`'s `_id` is walked. Its other fields are ACCUMULATOR position,
/// where `$push` / `$topN` / `$count` are valid and are absent from
/// `KNOWN_EXPR_OPS` precisely because they are not expressions -- walking them
/// would reject `{$group: {_id: "$g", p: {$push: "$s"}}}`, the plainest valid
/// pipeline there is. The Python server hit exactly that; see
/// `aggregate._expression_problem`.
pub(crate) fn validate_unknown_exprs(pipeline: &[Bson]) -> Result<(), CommandError> {
    const WRAPPED: [&str; 3] = ["$project", "$addFields", "$set"];
    for stage in pipeline {
        let Some(d) = stage.as_document() else {
            continue;
        };

        for name in WRAPPED {
            let Some(spec) = d.get(name).and_then(Bson::as_document) else {
                continue;
            };
            for (_field, value) in spec.iter() {
                if let Some(op) = secantus_core::expressions::first_unknown_expr_operator(value) {
                    return Err(unknown_expr_error(Some(name), &op));
                }
            }
        }

        // Bare positions: the message goes to the client with no wrapper.
        let bare = [
            d.get("$group")
                .and_then(Bson::as_document)
                .and_then(|g| g.get("_id")),
            d.get("$replaceWith"),
            d.get("$match")
                .and_then(Bson::as_document)
                .and_then(|m| m.get("$expr")),
        ];
        for value in bare.into_iter().flatten() {
            if let Some(op) = secantus_core::expressions::first_unknown_expr_operator(value) {
                return Err(unknown_expr_error(None, &op));
            }
        }
    }
    Ok(())
}

/// mongod's unknown-expression error, with `stage`'s envelope when it has one.
fn unknown_expr_error(stage: Option<&str>, op: &str) -> CommandError {
    let message = format!("Unrecognized expression '{op}'");
    let message = match stage {
        Some(name) => format!("Invalid {name} :: caused by :: {message}"),
        None => message,
    };
    CommandError::new(168, crate::util::error_code_name(168), message)
}

/// Refuse `$where` inside any `$match` stage, at aggregate PARSE time.
///
/// This is an aggregation-CONTEXT rule, not a scripting one: mongod answers
/// code 2 `$where is not allowed in this context` whether or not it has a
/// script engine (measured 8.2.11 both ways, 2026-09-29), so it is a DIFFERENT
/// refusal from the one a plain `find` gets (6108304, see `query.rs`) and must
/// not be folded into it.
///
/// Parse-time on purpose, and it has to run before `lift_leading_match`: once
/// the leading `$match` is lifted into the fetch filter it goes through the
/// query matcher, which answers the query-context refusal instead. A
/// non-leading `$match` already answered code 2, so this closes an asymmetry
/// between the first stage and every other one.
fn validate_no_where_in_match(pipeline: &[Bson]) -> Result<(), CommandError> {
    fn scan(value: &Bson) -> bool {
        match value {
            Bson::Document(d) => d.iter().any(|(k, v)| k == "$where" || scan(v)),
            Bson::Array(a) => a.iter().any(scan),
            _ => false,
        }
    }
    for stage in pipeline {
        if let Some(Bson::Document(spec)) = stage.as_document().and_then(|d| d.get("$match")) {
            if scan(&Bson::Document(spec.clone())) {
                return Err(CommandError::new(
                    2,
                    "BadValue",
                    "$where is not allowed in this context",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_stage_names(pipeline: &[Bson]) -> Result<(), CommandError> {
    for stage in pipeline {
        // Both of these used to `continue`, so a malformed element sailed past
        // validation: `pipeline: [42]` reached execution and surfaced as a bare
        // internal error, and a two-key stage was silently accepted.
        let Some(d) = stage.as_document() else {
            return Err(CommandError::new(14, "TypeMismatch", PIPELINE_ELEMENT_MSG));
        };
        if d.len() != 1 {
            return Err(CommandError::new(40323, "Location40323", STAGE_ARITY_MSG));
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
        // Spec-level validation with mongod's own codes. The core engine
        // signals "cannot do this" with a bare `Err(())`, which the adapter
        // reports as a generic `BadValue` (2) -- so a MALFORMED spec was
        // indistinguishable from an unimplemented one, and every case below
        // answered 2 where mongod has a specific code. Validating the spec here,
        // before the engine runs, is the same place `$facet` already does it and
        // avoids widening the engine's error type. All probed against 8.2.11.
        match name {
            "$setWindowFields" => validate_set_window_fields(d.get(name))?,
            "$sample" => validate_sample(d.get(name))?,
            "$unwind" => validate_unwind(d.get(name))?,
            "$bucket" => validate_bucket(d.get(name))?,
            "$densify" => validate_densify(d.get(name))?,
            "$fill" => validate_fill(d.get(name))?,
            _ => {}
        }
    }
    Ok(())
}

/// `$sample`'s `size`: a number, and a positive one.
fn validate_sample(spec: Option<&Bson>) -> Result<(), CommandError> {
    let Some(spec) = spec.and_then(Bson::as_document) else {
        return Ok(());
    };
    let Some(size) = spec.get("size") else {
        return Ok(());
    };
    let n = match size {
        Bson::Int32(v) => f64::from(*v),
        Bson::Int64(v) => *v as f64,
        Bson::Double(v) => *v,
        _ => {
            return Err(CommandError::new(
                28746,
                "Location28746",
                "size argument to $sample must be a number",
            ));
        }
    };
    if n < 0.0 {
        return Err(CommandError::new(
            28747,
            "Location28747",
            "size argument to $sample must be a positive integer",
        ));
    }
    Ok(())
}

/// `$unwind`'s string form must be a field PATH, i.e. `$`-prefixed.
fn validate_unwind(spec: Option<&Bson>) -> Result<(), CommandError> {
    let path = match spec {
        Some(Bson::String(p)) => p.clone(),
        Some(Bson::Document(d)) => match d.get_str("path") {
            Ok(p) => p.to_string(),
            Err(_) => return Ok(()),
        },
        _ => return Ok(()),
    };
    // An empty path is "no path specified", which mongod reports before it
    // objects to the missing `$` prefix (probed 8.2.11).
    if path.is_empty() {
        return Err(CommandError::new(
            28812,
            "Location28812",
            "no path specified to $unwind stage",
        ));
    }
    if !path.starts_with('$') {
        return Err(CommandError::new(
            28818,
            "Location28818",
            format!("path option to $unwind stage should be prefixed with a '$': {path}"),
        ));
    }
    Ok(())
}

/// `$bucket` needs at least two boundaries to define one bucket.
fn validate_bucket(spec: Option<&Bson>) -> Result<(), CommandError> {
    let Some(spec) = spec.and_then(Bson::as_document) else {
        return Ok(());
    };
    if let Some(Bson::Array(b)) = spec.get("boundaries") {
        if b.len() < 2 {
            return Err(CommandError::new(
                40192,
                "Location40192",
                format!(
                    "The $bucket 'boundaries' field must have at least 2 values, \
                     but found {} value(s).",
                    b.len()
                ),
            ));
        }
    }
    Ok(())
}

/// `$densify`'s `range.step` must be strictly positive.
fn validate_densify(spec: Option<&Bson>) -> Result<(), CommandError> {
    let Some(range) = spec
        .and_then(Bson::as_document)
        .and_then(|d| d.get("range"))
        .and_then(Bson::as_document)
    else {
        return Ok(());
    };
    let step = match range.get("step") {
        Some(Bson::Int32(v)) => f64::from(*v),
        Some(Bson::Int64(v)) => *v as f64,
        Some(Bson::Double(v)) => *v,
        Some(_) => f64::NAN,
        None => return Ok(()),
    };
    // NaN spelled out rather than written as `!(step > 0.0)`: that form relies
    // on NaN comparing false, which clippy rejects as a negated comparison on a
    // partially-ordered type. A non-numeric `step` lands here as NaN above.
    if step.is_nan() || step <= 0.0 {
        return Err(CommandError::new(
            5733401,
            "Location5733401",
            "The step parameter in a range statement must be a strictly positive numeric value",
        ));
    }
    Ok(())
}

/// `$fill`'s per-field `method`, when given, is `locf` or `linear`.
fn validate_fill(spec: Option<&Bson>) -> Result<(), CommandError> {
    let Some(output) = spec
        .and_then(Bson::as_document)
        .and_then(|d| d.get("output"))
        .and_then(Bson::as_document)
    else {
        return Ok(());
    };
    for (_field, field_spec) in output.iter() {
        let Some(fs) = field_spec.as_document() else {
            continue;
        };
        if let Some(m) = fs.get("method") {
            let ok = matches!(m.as_str(), Some("locf") | Some("linear"));
            if !ok {
                return Err(CommandError::new(
                    6050202,
                    "Location6050202",
                    "Method must be either locf or linear",
                ));
            }
        }
    }
    Ok(())
}

/// `$setWindowFields`'s own fields. Anything else is REJECTED rather than
/// ignored: an unknown top-level key used to be accepted silently, so a caller
/// who misspelled `partitionBy` -- or put `range` at the top level, where it
/// looks plausible but belongs inside a window -- got a wrong answer instead of
/// an error. Codes and messages probed against mongod 8.2.11. Mirrors the
/// Python server's `_SET_WINDOW_FIELDS_KNOWN`.
const SET_WINDOW_FIELDS_KNOWN: &[&str] = &["partitionBy", "sortBy", "output"];

fn validate_set_window_fields(spec: Option<&Bson>) -> Result<(), CommandError> {
    let Some(spec) = spec.and_then(Bson::as_document) else {
        return Ok(()); // shape errors are the engine's to report
    };
    if let Some(unknown) = spec
        .keys()
        .find(|k| !k.starts_with('$') && !SET_WINDOW_FIELDS_KNOWN.contains(&k.as_str()))
    {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field '$setWindowFields.{unknown}' is an unknown field."),
        ));
    }
    if !spec.contains_key("output") {
        return Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            "BSON field '$setWindowFields.output' is missing but a required field",
        ));
    }
    // The same silent acceptance one level down: an unknown key inside `window`
    // was ignored, so a misspelled `documents` widened the window to the whole
    // partition without saying so.
    if let Some(output) = spec.get("output").and_then(Bson::as_document) {
        for (_field, field_spec) in output.iter() {
            let Some(fs) = field_spec.as_document() else {
                continue;
            };
            let Some(window) = fs.get("window").and_then(Bson::as_document) else {
                continue;
            };
            if window
                .keys()
                .any(|k| !matches!(k.as_str(), "documents" | "range" | "unit"))
            {
                return Err(CommandError::new(
                    9,
                    "FailedToParse",
                    "'window' field can only contain 'documents' as the only argument \
                     or 'range' with an optional 'unit' field",
                ));
            }
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
    // mongod names the namespace in an execution-time aggregate error.
    let ns = format!("{db}.{}", coll.unwrap_or(""));
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
                let _ = core_run(docs, &buffer, vars, collation, Some(&ns))?;
                buffer.clear();
            }
            let spec = stage.as_document().and_then(|d| d.get(name));
            docs = documents_stage(spec, vars, coll)?;
        } else if name == crate::list_catalog::STAGE {
            if !buffer.is_empty() {
                let _ = core_run(docs, &buffer, vars, collation, Some(&ns))?;
                buffer.clear();
            }
            docs = crate::list_catalog::entries(storage, db, coll)?;
        } else if is_source_stage(name) {
            // Source stages (e.g. `$currentOp` / `$listLocalSessions`) ignore the
            // input and emit a synthetic row. Run (and discard) any buffered
            // stages first so a malformed earlier stage still errors in order,
            // matching Python; the source stage replaces the docs regardless.
            if !buffer.is_empty() {
                let _ = core_run(docs, &buffer, vars, collation, Some(&ns))?;
                buffer.clear();
            }
            docs = if name == "$currentOp" {
                // The registry's operations, and its idle cursors when asked
                // (RM-216); the synthetic row only off-server.
                let idle = stage
                    .as_document()
                    .and_then(|d| d.get_document(name).ok())
                    .and_then(|spec| spec.get_bool("idleCursors").ok())
                    .unwrap_or(false);
                crate::ops::current_op_rows(idle)
                    .unwrap_or_else(|| apply_source_stage(name, db, coll, cmd_doc, client_metadata))
            } else {
                known_sessions(stage.as_document().and_then(|d| d.get(name)))
            };
        } else if is_storage_backed(name) {
            if !buffer.is_empty() {
                docs = core_run(docs, &buffer, vars, collation, Some(&ns))?;
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
        docs = core_run(docs, &buffer, vars, collation, Some(&ns))?;
    }
    Ok(docs)
}

/// `$documents: [<expr>, …]` — evaluate each array element (against an empty
/// root) into a document. The array itself may be an expression (e.g. a `$$var`)
/// that resolves to an array of documents.
fn documents_stage(
    spec: Option<&Bson>,
    vars: &Document,
    coll: Option<&str>,
) -> Result<Vec<Document>, CommandError> {
    let spec = spec.ok_or_else(|| bad_value("$documents requires an array"))?;
    // An EMPTY document spec is rejected while the stage is desugared into a
    // projection, so it answers 51270 even against a collection; every other
    // argument gets the namespace error below (probed 8.2.11).
    if matches!(spec, Bson::Document(d) if d.is_empty()) {
        return Err(CommandError::new(
            51270,
            "Location51270",
            "Invalid empty sub-projection: _tempDocumentsField",
        ));
    }
    // The NAMESPACE check comes before the argument is looked at at all:
    // `$documents` is only legal in a collection-less aggregate. This ran
    // happily against a collection and reported an argument error instead.
    if coll.is_some_and(|c| !c.is_empty()) {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            "'$documents' can only be run with {aggregate: 1}",
        ));
    }
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
    ns: Option<&str>,
) -> Result<Vec<Document>, CommandError> {
    // Several stages can fail at RUNTIME with an error mongod names but the
    // engine can only signal as `Fallback` — the input documents are kept for
    // the naming re-check below, which consumes them. `$redact` was the first;
    // `$densify` / `$bucket` / `$switch` were measured divergent on 2026-08-31
    // (the Rust server answered a generic `2 BadValue` for all of them).
    // The gate is deliberately narrow: `saved` is cloned on the SUCCESS path
    // too, and `$project` / `$addFields` / `$set` are ubiquitous, so gating on
    // the stage name alone would copy every input document for most
    // aggregations. `may_name_runtime_error` scans the pipeline SPEC for the
    // shapes that can actually fail this way.
    let nameable = secantus_core::aggregate::may_name_runtime_error(stages);
    let saved = nameable.then(|| docs.clone());
    secantus_core::aggregate::apply_pipeline(docs, stages, vars, collation).map_err(|fault| {
        // The engine can now name the error itself, which is the common case for
        // a bad ARGUMENT (`{$round: ["$n", 1.5]}`, `{$range: [0, 5, 0]}`,
        // `{$ln: 0}`, ...). Those used to land in the generic "not supported"
        // reply below, which told the client the server could not do `$round`
        // when in fact `$round` is fine and 1.5 is not a precision.
        if let Some((code, errmsg)) = fault.as_mongo() {
            // A PARSE error is sent bare here. The projection-style stages get
            // `Invalid $<stage> :: caused by ::` from `validate_stage_expr_args`
            // before execution, so anything reaching this point is inside
            // `$group` / `$expr` / `$redact`, where mongod adds no wrapper.
            let errmsg = if fault.is_bare() {
                errmsg.to_string()
            } else {
                wrap_pipeline_error(errmsg.to_string(), fault.folded(), ns)
            };
            return CommandError::new(code, crate::util::error_code_name(code), errmsg);
        }
        if let Some(docs) = saved {
            if let Some((code, errmsg, folded)) =
                secantus_core::aggregate::runtime_error(&docs, stages, vars, collation)
            {
                // Which of mongod's two prefixes depends on WHEN it failed. A
                // constant expression is folded at optimization time and says
                // so; a document-dependent one fails per document under the
                // executor prefix. This server had no wrapper at all.
                let errmsg = wrap_pipeline_error(errmsg, folded, ns);
                return CommandError::new(code, crate::util::error_code_name(code), errmsg);
            }
        }
        CommandError::new(
            2,
            "BadValue",
            "aggregation pipeline uses a stage or operator not supported by the Rust server",
        )
    })
}

/// A writing stage inside another stage's pipeline, which mongod refuses
/// before it runs anything (measured 8.2.11, 2026-10-13). These RAN, and
/// wrote to the target from inside a `$facet`.
fn nested_write_stage(pipeline: &[Bson]) -> Option<CommandError> {
    fn writer(stages: &[Bson]) -> Option<&str> {
        stages
            .iter()
            .map(stage_name)
            .find(|name| matches!(*name, "$out" | "$merge"))
    }
    for stage in pipeline {
        let Bson::Document(stage) = stage else {
            continue;
        };
        let Some((name, spec)) = stage.iter().next() else {
            continue;
        };
        let inner: Vec<&[Bson]> = match (name.as_str(), spec) {
            ("$facet", Bson::Document(facets)) => facets
                .values()
                .filter_map(|p| p.as_array().map(Vec::as_slice))
                .collect(),
            ("$lookup" | "$unionWith", Bson::Document(spec)) => spec
                .get_array("pipeline")
                .ok()
                .map(|p| vec![p.as_slice()])
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for stages in inner {
            if let Some(found) = writer(stages) {
                let (code, errmsg) = match name.as_str() {
                    "$facet" => (
                        40600,
                        format!("{found} is not allowed to be used within a $facet stage"),
                    ),
                    "$lookup" => (
                        51047,
                        format!("{found} is not allowed within a $lookup's sub-pipeline"),
                    ),
                    _ => (
                        31441,
                        format!("{found} is not allowed within a $unionWith's sub-pipeline"),
                    ),
                };
                return Some(CommandError::new(code, format!("Location{code}"), errmsg));
            }
            if let Some(problem) = nested_write_stage(stages) {
                return Some(problem);
            }
        }
    }
    None
}

/// The kind of stage a sub-pipeline runs under.
#[derive(Clone, Copy, PartialEq)]
enum SubPipeline {
    Lookup,
    UnionWith,
}

thread_local! {
    /// The sub-pipelines being run on this thread, innermost last, each with
    /// the namespace of the `aggregate` command that holds it.
    static SUB_PIPELINE: std::cell::RefCell<Vec<(SubPipeline, Option<String>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The namespace of the `aggregate` command running on this thread.
    static COMMAND_NS: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// While alive, a sub-pipeline of `kind` is running.
struct InSubPipeline;

impl InSubPipeline {
    fn enter(kind: SubPipeline) -> Self {
        let ns = COMMAND_NS.with(|n| n.borrow().clone());
        SUB_PIPELINE.with(|s| s.borrow_mut().push((kind, ns)));
        InSubPipeline
    }
}

impl Drop for InSubPipeline {
    fn drop(&mut self) {
        SUB_PIPELINE.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

/// While alive, the `aggregate` command on `ns` is running.
struct InCommand(Option<String>);

impl InCommand {
    fn enter(ns: &str) -> Self {
        InCommand(COMMAND_NS.with(|n| n.borrow_mut().replace(ns.to_string())))
    }
}

impl Drop for InCommand {
    fn drop(&mut self) {
        let before = self.0.take();
        COMMAND_NS.with(|n| *n.borrow_mut() = before);
    }
}

/// mongod's two wrappers for an error raised inside a pipeline: a
/// constant-folded expression fails at optimization time and says so, while a
/// document-dependent one fails per document under the executor prefix.
fn wrap_pipeline_error(errmsg: String, folded: bool, ns: Option<&str>) -> String {
    if folded {
        return format!("Failed to optimize pipeline :: caused by :: {errmsg}");
    }
    match ns {
        Some(ns) => format!(
            "Executor error during aggregate command on namespace: \
             {ns} :: caused by :: {errmsg}"
        ),
        None => errmsg,
    }
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
        "$indexStats" => apply_index_stats(spec, db, coll, storage),
        // A read of `system.buckets.<name>`: the stage names the collection.
        crate::ts_buckets::VIEWS_STAGE => crate::ts_buckets::view_definitions(storage, db),
        crate::ts_buckets::BUCKETS_STAGE => {
            let name = spec.and_then(Bson::as_str).unwrap_or_default();
            crate::ts_buckets::of_collection(storage, db, name)
        }
        // A document the target's validator refuses is reported under the
        // SOURCE namespace's executor prefix, as mongod reports it.
        "$out" | "$merge" => {
            // `$merge` puts its own write errors under the prefix.
            if name == "$merge" {
                return apply_merge(
                    spec,
                    docs,
                    db,
                    coll,
                    vars,
                    storage,
                    collation,
                    bypass_validation,
                );
            }
            apply_out(spec, docs, db, storage, bypass_validation).map_err(|mut e| {
                if matches!(
                    e.code,
                    121 | 166 | 7406103 | 7268700 | 11000 | 17152 | 20 | 52 | 53
                ) && !e.errmsg.starts_with("Collection names cannot start")
                {
                    let ns = format!("{db}.{}", coll.unwrap_or(""));
                    e.errmsg = wrap_pipeline_error(e.errmsg, false, Some(&ns));
                }
                e
            })
        }
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
    // `from: {db, coll}` is a syntax mongod parses and refuses for an
    // ordinary collection (RM-180, measured 8.2.11): 14 for a part that is
    // not a string, else 9 naming both parts.
    if let Some(Bson::Document(target)) = spec.get("from") {
        let part = |key: &str| -> Result<String, CommandError> {
            match target.get(key) {
                None => Ok(String::new()),
                Some(Bson::String(v)) => Ok(v.clone()),
                Some(other) => Err(CommandError::new(
                    14,
                    "TypeMismatch",
                    format!(
                        "BSON field 'from.{key}' is the wrong type '{}', expected type 'string'",
                        secantus_core::query::bson_type_name(other)
                    ),
                )),
            }
        };
        let (on_db, on_coll) = (part("db")?, part("coll")?);
        return Err(CommandError::new(
            9,
            "FailedToParse",
            format!(
                "$lookup with syntax {{from: {{db:<>, coll:<>}},..}} is not supported for db: \
                 {on_db} and coll: {on_coll}"
            ),
        ));
    }
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

    // The pipeline form runs its pipeline's query on the foreign collection
    // for each outer document (the simple form's join is not checked:
    // whether mongod plans it depends on which engine runs it).
    if let (Some(sub), None, None, false) =
        (sub_pipeline, local_field, foreign_field, docs.is_empty())
    {
        let (filter, sort) = crate::explain::pipeline_query(from, sub);
        crate::notablescan::check_foreign(
            storage,
            db,
            &crate::notablescan::Read {
                coll: from,
                filter: &filter,
                sort: sort.as_ref(),
                hint: None,
                collation,
                shown: Default::default(),
            },
        )?;
    }
    // Materialise the foreign collection once (the whole join's candidate pool).
    let foreign: Vec<Document> =
        collection_documents(storage, db, from, &Document::new(), collation)?;

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
            let _sub = InSubPipeline::enter(SubPipeline::Lookup);
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
    collection_documents(storage, db, coll, &filter, None)
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

    // Each walk queries the foreign collection for `connectToField` equal to
    // the values it chases, within `restrictSearchWithMatch`.
    if let Some(first) = docs.first() {
        if let Ok(seed) = secantus_core::expressions::evaluate(first, start_with, vars) {
            let wanted = match seed {
                Bson::Array(values) => doc! { connect_to: { "$in": values } },
                value => doc! { connect_to: value },
            };
            let filter = match restrict.filter(|r| !r.is_empty()) {
                Some(r) => doc! { "$and": [wanted, r.clone()] },
                None => wanted,
            };
            crate::notablescan::check_foreign(
                storage,
                db,
                &crate::notablescan::Read {
                    coll: from,
                    filter: &filter,
                    sort: None,
                    hint: None,
                    collation: None,
                    shown: Default::default(),
                },
            )?;
        }
    }
    let foreign: Vec<Document> = collection_documents(storage, db, from, &Document::new(), None)?;

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
            // mongod ECHOES the offending spec, which the message omitted.
            let rendered = spec.map(argtypes::render_stage_value).unwrap_or_default();
            CommandError::new(
                40169,
                "Location40169",
                format!(
                    "the $facet specification must be a non-empty object, \
                     but found: $facet: {rendered}"
                ),
            )
        })?;
    let mut out = Document::new();
    for (name, sub) in s.iter() {
        let sub_pipeline = sub.as_array().ok_or_else(|| {
            CommandError::new(
                40170,
                "Location40170",
                // mongod names the TYPE it found rather than restating the rule.
                format!(
                    "arguments to $facet must be arrays, {name} is type {}",
                    secantus_core::query::bson_type_name(sub)
                ),
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
                Bson::Document(d) if d.contains_key("$geoNear") => {
                    return Err(CommandError::new(
                        40600,
                        "Location40600",
                        "$geoNear is not allowed to be used within a $facet stage",
                    ));
                }
                Bson::Document(d) if d.contains_key(crate::list_catalog::STAGE) => {
                    return Err(CommandError::new(
                        40600,
                        "Location40600",
                        "$listCatalog is not allowed to be used within a $facet stage",
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
    let (from, sub_pipeline): (Option<&str>, Option<&Vec<Bson>>) = match spec {
        // An EMPTY collection name is not a namespace. This used to return the
        // outer documents unchanged -- a wrong ANSWER. mongod renders the
        // namespace with the collection half empty, so the database name runs
        // straight into "is" (probed 8.2.11 -- the message really has no space).
        Some(Bson::String(s)) if s.is_empty() => {
            return Err(CommandError::new(
                73,
                "InvalidNamespace",
                format!("Namespace {db}is not a valid collection name"),
            ));
        }
        Some(Bson::String(s)) => (Some(s.as_str()), None),
        Some(Bson::Document(d)) => {
            // No `coll` when the pipeline makes its own documents (RM-179,
            // measured 8.2.11): `argtypes` refuses any other shape.
            let from = match d.get("coll") {
                None => None,
                Some(_) => Some(
                    d.get_str("coll")
                        .map_err(|_| bad_value("$unionWith requires 'coll' (string)"))?,
                ),
            };
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
    let Some(from) = from else {
        let sub = sub_pipeline.ok_or_else(|| bad_value("$unionWith requires 'coll' (string)"))?;
        let _sub = InSubPipeline::enter(SubPipeline::UnionWith);
        let mut made = run_segmented(
            Vec::new(),
            sub,
            db,
            None,
            &Document::new(),
            storage,
            collation,
            &Document::new(),
            None,
        )?;
        docs.append(&mut made);
        return Ok(docs);
    };
    if let Some(sub) = sub_pipeline {
        let (filter, sort) = crate::explain::pipeline_query(from, sub);
        crate::notablescan::check_foreign(
            storage,
            db,
            &crate::notablescan::Read {
                coll: from,
                filter: &filter,
                sort: sort.as_ref(),
                hint: None,
                collation,
                shown: Default::default(),
            },
        )?;
    }
    let mut foreign: Vec<Document> =
        collection_documents(storage, db, from, &Document::new(), collation)?;
    if let Some(sub) = sub_pipeline {
        let _sub = InSubPipeline::enter(SubPipeline::UnionWith);
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

/// `$lookup` equality with mongod's array-aware semantics: array↔array → any
/// element equal; one side array → membership; else equality. A missing field
/// is `Null`.
fn lookup_match(local: Option<&Bson>, foreign: Option<&Bson>) -> bool {
    let local = local.unwrap_or(&Bson::Null);
    let foreign = foreign.unwrap_or(&Bson::Null);
    match (local, foreign) {
        (Bson::Array(la), Bson::Array(fa)) => {
            la.iter().any(|le| fa.iter().any(|fe| lookup_eq(le, fe)))
        }
        (Bson::Array(la), f) => la.iter().any(|le| lookup_eq(le, f)),
        (l, Bson::Array(fa)) => fa.iter().any(|fe| lookup_eq(l, fe)),
        (l, f) => lookup_eq(l, f),
    }
}

/// One pair under mongod's BSON equality. The derived `Bson ==` is
/// structural, so `Int32(2)` and `Double(2.0)` differed, and so did
/// `Decimal128("1.5")` and `Decimal128("1.500")`: a `localField` /
/// `foreignField` join with no index on the foreign field matched none of
/// them, where mongod 8.2.11 joins all three (measured 2026-09-19). The
/// canonical order compares numerics by value and ranks NaN equal to NaN.
fn lookup_eq(a: &Bson, b: &Bson) -> bool {
    use secantus_core::order;
    if order::is_comparable(a) && order::is_comparable(b) {
        order::cmp(a, b) == std::cmp::Ordering::Equal
    } else {
        a == b
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
/// (non-view) collection is reached. Returns the inputs unchanged when `coll`
/// isn't a view. Mirrors `commands._resolve_view`.
///
/// A chain of [`crate::views::MAX_VIEW_DEPTH`] views or more, or a cycle, is
/// mongod's 165 (measured 8.2.11, 2026-10-14: a chain of 19 views resolves
/// and one of 20 does not, though 20 can be created). There was no limit.
pub(crate) fn resolve_view(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    pipeline: Vec<Bson>,
) -> Result<(String, Vec<Bson>), CommandError> {
    let mut coll = coll.to_string();
    let mut combined = pipeline;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    loop {
        if !seen.insert(coll.clone()) || seen.len() > crate::views::MAX_VIEW_DEPTH {
            return Err(CommandError::new(
                165,
                "ViewDepthLimitExceeded",
                format!(
                    "View depth too deep or view cycle detected; maximum depth is {}",
                    crate::views::MAX_VIEW_DEPTH
                ),
            ));
        }
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
        // A namespace read as a view of itself is a derived source
        // (`system.views`, a time-series collection's buckets): its stage
        // produces the documents, and it is where the chain ends. A user's
        // view cannot be defined on itself: `create` refuses the cycle.
        if view_on == coll {
            break;
        }
        coll = view_on.to_string();
    }
    Ok((coll, combined))
}

/// The documents of `db.coll` matching `filter` -- a collection's own, or a
/// view's: its pipeline's output over the collection it is defined on. Every
/// stage that reads ANOTHER namespace (`$lookup`, `$graphLookup`,
/// `$unionWith`) and `distinct` go through here; reading a view's own storage
/// instead found nothing, so a `$lookup` from a view matched no document.
pub(crate) fn collection_documents(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    collation: Option<&Collation>,
) -> Result<Vec<Document>, CommandError> {
    if !crate::views::is_view(storage, db, coll) {
        let found = storage
            .find(db, coll, filter, None, None)
            .map_err(command_error)?;
        return decode_docs(crate::ts_buckets::as_read(storage, db, coll, found, false)?);
    }
    let mut tail: Vec<Bson> = Vec::new();
    if !filter.is_empty() {
        tail.push(Bson::Document(doc! { "$match": filter.clone() }));
    }
    let (base, pipeline) = resolve_view(storage, db, coll, tail)?;
    let input = decode_docs(
        storage
            .find(db, &base, &Document::new(), None, None)
            .map_err(command_error)?,
    )?;
    run_segmented(
        input,
        &pipeline,
        db,
        Some(&base),
        &Document::new(),
        storage,
        collation,
        &Document::new(),
        None,
    )
}

/// The candidate fetch for a leading bounded `$geoNear`: `Some(filter)`
/// holding every document the stage can return (it re-applies the exact
/// distance bounds, so the output is the same; only the documents read
/// shrink), when the stage has a numeric `maxDistance` and a `2d` or
/// `2dsphere` index on `key` of the kind its point reads; else `None` (every
/// document is read).
///
/// The stage measures a line or a polygon by its nearest point, so on the
/// sphere the candidates are what INTERSECTS the disc of the distance, not
/// what lies within it: a `$geoIntersects` with a polygon drawn around the
/// disc, its edges tangent to a slightly larger circle. In the plane a `2d`
/// index holds points only, and a `$center` holds every point inside.
fn geo_near_index_filter(
    spec: &Bson,
    db: &str,
    coll: Option<&str>,
    storage: &dyn crate::storage::Storage,
) -> Option<Document> {
    const EARTH_RADIUS_METERS: f64 = secantus_core::geoshape::EARTH_RADIUS_METERS;
    let spec = spec.as_document()?;
    let max_distance = match spec.get("maxDistance") {
        Some(Bson::Int32(n)) => *n as f64,
        Some(Bson::Int64(n)) => *n as f64,
        Some(Bson::Double(d)) if d.is_finite() && *d >= 0.0 => *d,
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
    // Parse `near` -> (spherical, the distance in radians on the sphere, cx, cy).
    let (spherical, radians, cx, cy) = match spec.get("near") {
        Some(Bson::Document(d)) if d.get_str("type").ok() == Some("Point") => {
            let coords = d.get_array("coordinates").ok()?;
            if coords.len() != 2 {
                return None;
            }
            (
                true,
                max_distance / EARTH_RADIUS_METERS,
                bson_f64(&coords[0])?,
                bson_f64(&coords[1])?,
            )
        }
        Some(Bson::Array(a)) if a.len() == 2 => {
            // A legacy point on the sphere gives its distance in radians.
            let sph = spec.get_bool("spherical").unwrap_or(false);
            (sph, max_distance, bson_f64(&a[0])?, bson_f64(&a[1])?)
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
    let inner = if spherical {
        let ring = disc_polygon(cx, cy, radians)?;
        doc! { "$geoIntersects": { "$geometry": { "type": "Polygon", "coordinates": [ring] } } }
    } else {
        let radius = max_distance * (1.0 + 1e-9); // inflate -> guaranteed superset
        let center = Bson::Array(vec![Bson::Double(cx), Bson::Double(cy)]);
        doc! { "$geoWithin": { "$center": Bson::Array(vec![center, Bson::Double(radius)]) } }
    };
    let mut filter = Document::new();
    filter.insert(key, inner);
    Some(filter)
}

/// A closed ring (`[lng, lat]` pairs) around the disc of `radians` about
/// `(lng, lat)`: a regular polygon whose edges (great circles) stay outside
/// a circle a little larger than the disc. `None` when the disc is too large
/// for a polygon smaller than a hemisphere to hold it.
fn disc_polygon(lng: f64, lat: f64, radians: f64) -> Option<Vec<Bson>> {
    const SIDES: usize = 16;
    // Room for rounding, and for a disc of no size at all.
    let r = (radians * (1.0 + 1e-6)).max(1e-6) + 1e-9;
    // A regular polygon whose sides are tangent to the circle of radius r
    // has its vertices at R with tan(R) = tan(r) / cos(pi / n).
    let tan_vertex = r.tan() / (std::f64::consts::PI / SIDES as f64).cos();
    if !(r < 1.2 && tan_vertex.is_finite() && tan_vertex > 0.0) {
        return None;
    }
    let d = tan_vertex.atan();
    let (lat1, lng1) = (lat.to_radians(), lng.to_radians());
    let mut ring = Vec::with_capacity(SIDES + 1);
    for i in 0..SIDES {
        let bearing = 2.0 * std::f64::consts::PI * i as f64 / SIDES as f64;
        let lat2 = (lat1.sin() * d.cos() + lat1.cos() * d.sin() * bearing.cos())
            .clamp(-1.0, 1.0)
            .asin();
        let lng2 =
            lng1 + (bearing.sin() * d.sin() * lat1.cos()).atan2(d.cos() - lat1.sin() * lat2.sin());
        let mut lng_deg = lng2.to_degrees();
        while lng_deg > 180.0 {
            lng_deg -= 360.0;
        }
        while lng_deg < -180.0 {
            lng_deg += 360.0;
        }
        ring.push(Bson::Array(vec![
            Bson::Double(lng_deg),
            Bson::Double(lat2.to_degrees().clamp(-90.0, 90.0)),
        ]));
    }
    ring.push(ring[0].clone());
    Some(ring)
}

/// One `$geoNear` stage as mongod reads it.
struct GeoNearStage<'a> {
    near: secantus_core::geoshape::Near,
    /// Distances are given and returned in radians: a legacy point on the
    /// sphere.
    radians: bool,
    /// The stage said `spherical: true`.
    asked_spherical: bool,
    distance_field: Option<&'a str>,
    include_locs: Option<&'a str>,
    key: Option<&'a str>,
    query: Option<&'a Document>,
    multiplier: f64,
}

fn geo_near_stage(spec: &Document) -> Result<GeoNearStage<'_>, CommandError> {
    use secantus_core::geoshape::{Near, EARTH_RADIUS_METERS};
    let fail =
        |code: i32, text: String| CommandError::new(code, crate::util::error_code_name(code), text);
    let number = |v: &Bson, wrong: &str, negative: &str| -> Result<f64, CommandError> {
        match bson_f64(v) {
            None => Err(fail(14, wrong.to_string())),
            Some(n) if n < 0.0 => Err(fail(2, negative.to_string())),
            Some(n) => Ok(n),
        }
    };
    let (mut near, mut spherical) = (None, false);
    let (mut min, mut max, mut multiplier) = (None, None, 1.0);
    let (mut distance_field, mut include_locs, mut key, mut query) = (None, None, None, None);
    for (name, v) in spec {
        match name.as_str() {
            "near" => {
                if !matches!(v, Bson::Document(_) | Bson::Array(_)) {
                    return Err(fail(
                        5860401,
                        "$geoNear requires near argument to be a GeoJSON object or a legacy \
                         point(array)"
                            .to_string(),
                    ));
                }
                near = Some(v);
            }
            "distanceField" => match v {
                Bson::String(f) if f.is_empty() => {
                    return Err(fail(
                        40352,
                        "FieldPath cannot be constructed with empty string".to_string(),
                    ))
                }
                Bson::String(f) if f.starts_with('$') => {
                    return Err(fail(
                        16410,
                        format!(
                            "Consider using $getField or $setField for a field path with '.' or \
                             '$'. :: caused by :: FieldPath field names may not start with '$', \
                             given '{f}'."
                        ),
                    ))
                }
                Bson::String(f) => distance_field = Some(f.as_str()),
                _ => {
                    return Err(fail(
                        16606,
                        "$geoNear requires that the 'distanceField' option is a String".to_string(),
                    ))
                }
            },
            "maxDistance" => {
                max = Some(number(
                    v,
                    "$geoNear requires $maxDistance to evaluate to a number",
                    "maxDistance must be nonnegative",
                )?)
            }
            "minDistance" => {
                min = Some(number(
                    v,
                    "$geoNear requires $minDistance to evaluate to a number",
                    "minDistance must be nonnegative",
                )?)
            }
            "distanceMultiplier" => {
                multiplier = number(
                    v,
                    "distanceMultiplier must be a number",
                    "distanceMultiplier must be nonnegative",
                )?
            }
            "spherical" => {
                spherical =
                    !matches!(v, Bson::Boolean(false) | Bson::Null) && bson_f64(v) != Some(0.0)
            }
            "query" => match v {
                Bson::Document(q) => query = Some(q),
                _ => return Err(fail(14, "query must be an object".to_string())),
            },
            "key" => match v {
                Bson::String(k) if k.is_empty() => {
                    return Err(fail(
                        2,
                        "$geoNear parameter 'key' cannot be the empty string".to_string(),
                    ))
                }
                Bson::String(k) => key = Some(k.as_str()),
                other => {
                    return Err(fail(
                        14,
                        format!(
                            "$geoNear parameter 'key' must be of type string but found type: {}",
                            secantus_core::query::bson_type_name(other)
                        ),
                    ))
                }
            },
            "includeLocs" => match v {
                Bson::String(f) => include_locs = Some(f.as_str()),
                _ => {
                    return Err(fail(
                        16607,
                        "$geoNear requires that 'includeLocs' option is a String".to_string(),
                    ))
                }
            },
            "limit" | "num" => {
                return Err(fail(
                    if name == "limit" { 50858 } else { 50857 },
                    format!(
                        "$geoNear no longer supports the '{name}' parameter. Use a $limit stage \
                         instead."
                    ),
                ))
            }
            "uniqueDocs" => {}
            other => return Err(fail(2, format!("Unknown argument to $geoNear: {other}"))),
        }
    }
    let near =
        near.ok_or_else(|| fail(5860400, "$geoNear requires a 'near' argument".to_string()))?;
    // A GeoJSON point is on the sphere whatever `spherical` says.
    let geojson = matches!(near, Bson::Document(d) if d.contains_key("type"));
    let centre = if geojson {
        let d = near.as_document().expect("a GeoJSON object");
        match secantus_core::geoshape::geojson(d) {
            Ok(secantus_core::geoshape::Geom::Point(p)) => p,
            _ => {
                return Err(fail(
                    2,
                    "invalid argument in geo near query: type".to_string(),
                ))
            }
        }
    } else {
        let parts: Vec<&Bson> = match near {
            Bson::Array(a) => a.iter().collect(),
            Bson::Document(d) => d.values().collect(),
            _ => Vec::new(),
        };
        match (
            parts.first().and_then(|v| bson_f64(v)),
            parts.get(1).and_then(|v| bson_f64(v)),
        ) {
            (Some(x), Some(y)) => (x, y),
            _ => return Err(fail(2, "invalid argument in geo near query: 0".to_string())),
        }
    };
    let radians = spherical && !geojson;
    let metres = |d: Option<f64>| d.map(|d| if radians { d * EARTH_RADIUS_METERS } else { d });
    if let Some(q) = query {
        if crate::near::find_near(q).map_err(command_error)?.is_some() {
            return Err(fail(5626500, NEAR_NEEDS_A_SORT.to_string()));
        }
    }
    Ok(GeoNearStage {
        near: Near {
            centre,
            spherical: spherical || geojson,
            geojson,
            min: metres(min),
            max: metres(max),
        },
        radians,
        asked_spherical: spherical,
        distance_field,
        include_locs,
        key,
        query,
        multiplier,
    })
}

const NEAR_NEEDS_A_SORT: &str = "$geoNear, $near, and $nearSphere are not allowed in this \
    context, as these operators require sorting geospatial data. If you do not need sort, \
    consider using $geoWithin instead. Check out \
    https://dochub.mongodb.org/core/near-sort-operation and \
    https://dochub.mongodb.org/core/nearSphere-sort-operationfor more details.";

/// `$geoNear`: the documents nearest first, each with its distance.
///
/// A GeoJSON `near` measures on the sphere in metres and reads a `2dsphere`
/// index. A legacy point measures in the plane and reads a `2d` index, or
/// with `spherical: true` on the sphere in radians and reads either. Every
/// geometry the index holds is measured: a line or a polygon by its nearest
/// point.
fn apply_geo_near(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    coll: Option<&str>,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
) -> Result<Vec<Document>, CommandError> {
    let within = SUB_PIPELINE.with(|s| s.borrow().last().cloned());
    near_stage(spec, docs, db, coll, vars, storage, within.as_ref()).map_err(|mut e| {
        // Run for each document of the outer pipeline, the stage fails under
        // the outer command's executor prefix (measured 8.2.11, 2026-10-15).
        if let Some((_, Some(ns))) = &within {
            if !e.errmsg.starts_with("Executor error during") {
                e.errmsg = wrap_pipeline_error(e.errmsg, false, Some(ns));
            }
        }
        e
    })
}

#[allow(clippy::too_many_arguments)]
fn near_stage(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    coll: Option<&str>,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
    within: Option<&(SubPipeline, Option<String>)>,
) -> Result<Vec<Document>, CommandError> {
    let spec = spec
        .and_then(Bson::as_document)
        .ok_or_else(|| bad_value("$geoNear requires a document spec"))?;
    let stage = geo_near_stage(spec)?;
    if let Some(c) = coll {
        if !storage.collection_exists(db, c).map_err(command_error)? {
            return Err(CommandError::new(
                26,
                "NamespaceNotFound",
                format!("$geoNear requires a geo index to run, but {db}.{c} does not exist"),
            ));
        }
    }
    let key: String = match stage.key {
        Some(k) => k.to_string(),
        None => infer_geo_near_key(db, coll, storage)?,
    };
    let coll = coll.ok_or_else(|| bad_value("$geoNear requires a collection"))?;
    // The index has to be one this kind of point reads.
    let readable = |kind: &str| match kind {
        "2dsphere" => stage.near.spherical,
        "2d" => !stage.near.geojson,
        _ => false,
    };
    let indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    let usable: Vec<(&Document, String)> = indexes
        .iter()
        .filter_map(|ix| {
            let kind = ix.get_document("key").ok()?.get_str(&key).ok()?;
            readable(kind).then(|| (ix, kind.to_string()))
        })
        .collect();
    if !usable.is_empty() {
        // A `2d` index hashes the centre before it reads anything. Inside a
        // `$lookup` mongod meets that while it tries plans, whatever their
        // number.
        let tried = matches!(within, Some((SubPipeline::Lookup, _)));
        crate::near::centre_hashes(&usable, stage.near.centre).map_err(|e| match e {
            crate::storage::StorageError::WriteError { code, errmsg, .. } => {
                let errmsg = if tried && !errmsg.starts_with(crate::near::MULTIPLANNER) {
                    format!("{}{errmsg}", crate::near::MULTIPLANNER)
                } else if errmsg.starts_with(crate::near::MULTIPLANNER) || within.is_some() {
                    errmsg
                } else {
                    wrap_pipeline_error(errmsg, false, Some(&format!("{db}.{coll}")))
                };
                CommandError::new(code, crate::util::error_code_name(code), errmsg)
            }
            other => command_error(other),
        })?;
    }
    if usable.is_empty() {
        // mongod parses the query before it plans, so an unknown top-level
        // operator is its 2, not the 291 (measured 8.2.11, 2026-10-15).
        if let Some(secantus_core::fallback::Fallback::Mongo { code, message, .. }) = stage
            .query
            .and_then(secantus_core::query::unknown_top_level_refusal)
        {
            return Err(CommandError::new(
                code,
                crate::util::error_code_name(code),
                message,
            ));
        }
        return Err(CommandError::new(
            291,
            "NoQueryExecutionPlans",
            geo_near_unplannable(
                &stage,
                spec,
                &key,
                db,
                coll,
                "unable to find index for $geoNear query",
            ),
        ));
    }
    let unit = if stage.radians {
        secantus_core::geoshape::EARTH_RADIUS_METERS
    } else {
        1.0
    };
    let mut scored: Vec<(f64, bool, Document)> = Vec::new();
    for doc in docs {
        if let Some(q) = stage.query {
            if !secantus_core::query::matches(&doc, q, vars, None).map_err(|f| match f {
                secantus_core::fallback::Fallback::Mongo { code, message, .. } => {
                    CommandError::new(code, crate::util::error_code_name(code), message)
                }
                _ => bad_value("query uses a construct the Rust server does not support"),
            })? {
                continue;
            }
        }
        let Some(value) = secantus_core::get_path(&doc, &key).cloned() else {
            continue;
        };
        let Some(d) = stage
            .near
            .distance(&value)
            .filter(|d| stage.near.admits(*d))
        else {
            continue;
        };
        let shaped =
            matches!(&value, Bson::Document(g) if g.get_str("type").is_ok_and(|t| t != "Point"));
        let mut out = doc;
        if let Some(field) = stage.distance_field {
            set_field_path(&mut out, field, Bson::Double(d / unit * stage.multiplier));
        }
        if let Some(field) = stage.include_locs {
            set_field_path(&mut out, field, value);
        }
        scored.push((d, shaped, out));
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    // mongod's own order: its ring search over the index, which leaves
    // documents at one distance as its queue does.
    if let Some(rank) = geo_near_ranks(&stage, &key, db, coll, storage)? {
        let at = |d: &Document| {
            d.get("_id")
                .and_then(|id| secantus_core::sortkey::encode_value(id, None).ok())
                .and_then(|id| rank.get(&id).copied())
                .unwrap_or(usize::MAX)
        };
        scored.sort_by_key(|(_, _, d)| at(d));
    }
    Ok(scored.into_iter().map(|(_, _, d)| d).collect())
}

/// mongod's message for a `$geoNear` it could not plan, for `reason`.
fn geo_near_unplannable(
    stage: &GeoNearStage<'_>,
    spec: &Document,
    key: &str,
    db: &str,
    coll: &str,
    reason: &str,
) -> String {
    let max = match spec.get("maxDistance").and_then(bson_f64) {
        Some(m) => crate::crud::fmt_g(m),
        None => "1.79769e+308".to_string(),
    };
    format!(
        "error processing query: ns={db}.{coll}Tree: {}\nSort: {{}}\nProj: {{}}\nCollation: \
         {{ locale: \"simple\" }}\n planner returned error :: caused by :: {reason}",
        crate::near::tree(
            stage.query,
            &format!(
                "GEONEAR  field={key} maxdist={max} isNearSphere={}",
                i32::from(stage.asked_spherical)
            )
        )
    )
}

/// mongod's refusals of a leading `$geoNear` under the command's `hint`
/// (measured, 8.2.11): a `$natural` hint is 2, a hint naming no index is 2
/// with the query, a hint to an index that cannot answer the stage 291.
fn check_geo_near_hint(
    spec: &Bson,
    hint: &Bson,
    db: &str,
    coll: &str,
    storage: &dyn crate::storage::Storage,
) -> Result<(), CommandError> {
    let Some(spec) = spec.as_document() else {
        return Ok(());
    };
    if matches!(hint, Bson::Document(d) if d.is_empty()) {
        return Ok(());
    }
    let stage = geo_near_stage(spec)?;
    let key: String = match stage.key {
        Some(k) => k.to_string(),
        None => infer_geo_near_key(db, Some(coll), storage)?,
    };
    if matches!(hint, Bson::Document(d) if d.contains_key("$natural")) {
        return Err(CommandError::new(
            2,
            "BadValue",
            "geoNear expression not allowed with $natural hint",
        ));
    }
    let specs = storage.list_indexes(db, coll).map_err(command_error)?;
    let named = specs.iter().find(|s| match hint {
        Bson::String(name) => s.get_str("name") == Ok(name),
        Bson::Document(k) => s.get_document("key").is_ok_and(|key| key == k),
        _ => false,
    });
    let Some(named) = named else {
        return Err(CommandError::new(
            2,
            "BadValue",
            geo_near_unplannable(
                &stage,
                spec,
                &key,
                db,
                coll,
                "hint provided does not correspond to an existing index",
            ),
        ));
    };
    let readable = named
        .get_document("key")
        .ok()
        .and_then(|k| k.get_str(&key).ok())
        .is_some_and(|kind| match kind {
            "2dsphere" => stage.near.spherical,
            "2d" => !stage.near.geojson,
            _ => false,
        });
    if readable {
        return Ok(());
    }
    Err(CommandError::new(
        291,
        "NoQueryExecutionPlans",
        geo_near_unplannable(
            &stage,
            spec,
            &key,
            db,
            coll,
            "unable to find index for $geoNear query",
        ),
    ))
}

/// Where each document of the collection comes in mongod's `$geoNear`, by
/// its `_id`, or `None` when the search is not replayed here.
fn geo_near_ranks(
    stage: &GeoNearStage<'_>,
    key: &str,
    db: &str,
    coll: &str,
    storage: &dyn crate::storage::Storage,
) -> Result<Option<std::collections::HashMap<Vec<u8>, usize>>, CommandError> {
    let Some(index) = crate::explain::geo::near_index(storage, db, coll, key, &stage.near)
        .map_err(command_error)?
    else {
        return Ok(None);
    };
    let Some(all) = storage
        .collection_at_most(db, coll, 2000)
        .map_err(command_error)?
    else {
        return Ok(None);
    };
    let docs: Option<Vec<Document>> = all
        .iter()
        .map(|(_, blob)| secantus_core::bsondoc::from_bytes(blob).ok())
        .collect();
    let Some(docs) = docs else {
        return Ok(None);
    };
    let Some(order) = crate::explain::geo::geo_near_order(&index, &stage.near, stage.query, &docs)
    else {
        return Ok(None);
    };
    Ok(Some(
        order
            .into_iter()
            .enumerate()
            .filter_map(|(rank, i)| {
                let id = docs[i].get("_id")?;
                Some((secantus_core::sortkey::encode_value(id, None).ok()?, rank))
            })
            .collect(),
    ))
}

/// The field `$geoNear` measures when the stage names none: that of the
/// collection's `2d` index when it has one, else of its `2dsphere` index.
/// Two of a kind is mongod's refusal.
fn infer_geo_near_key(
    db: &str,
    coll: Option<&str>,
    storage: &dyn crate::storage::Storage,
) -> Result<String, CommandError> {
    let coll = coll.ok_or_else(|| bad_value("$geoNear requires a collection"))?;
    let indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    let fields = |kind: &str| -> Vec<String> {
        indexes
            .iter()
            .filter_map(|ix| ix.get_document("key").ok())
            .flat_map(|key| key.iter())
            .filter(|(_, v)| matches!(v, Bson::String(s) if s == kind))
            .map(|(field, _)| field.clone())
            .collect()
    };
    let not_found = |text: String| CommandError::new(27, "IndexNotFound", text);
    for kind in ["2d", "2dsphere"] {
        let mut found = fields(kind);
        match found.len() {
            0 => {}
            1 => return Ok(found.remove(0)),
            _ => {
                return Err(not_found(format!(
                    "There is more than one {kind} index on {db}.{coll}; unsure which to use for \
                     $geoNear"
                )))
            }
        }
    }
    Err(not_found(
        "$geoNear requires a 2d or 2dsphere index, but none were found".to_string(),
    ))
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
    let spec = coll_stats_spec(spec)?;
    let spec = Some(&spec);
    let ns_text = format!("{db}.{coll}");
    let missing = |what: &str| {
        CommandError::new(
            26,
            "NamespaceNotFound",
            format!(
                "Executor error during aggregate command on namespace: {ns_text} :: caused by \
                 :: Unable to retrieve {what} in $collStats stage :: caused by :: Collection \
                 [{ns_text}] not found."
            ),
        )
    };
    let exists = storage.collection_exists(db, coll).unwrap_or(false);
    // A time-series collection's statistics are its buckets': their
    // namespace, and how many there are.
    let buckets = if storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        Some(crate::ts_buckets::of_collection(storage, db, coll)?.len() as i64)
    } else {
        None
    };
    let count = match buckets {
        Some(n) => n,
        None => storage
            .count_matching(db, coll, &Document::new())
            .map_err(command_error)? as i64,
    };
    let ns = match buckets {
        Some(_) => format!("{db}.{}{coll}", crate::ts_buckets::BUCKETS_PREFIX),
        None => format!("{db}.{coll}"),
    };
    let mut out = doc! {
        "ns": ns,
        "host": "secantus",
        "localTime": bson::DateTime::now(),
    };
    if let Some(Bson::Document(options)) = spec.and_then(|s| s.get("storageStats")) {
        if !exists {
            return Err(missing("storageStats"));
        }
        let scale = options
            .get("scale")
            .and_then(crate::util::as_i64)
            .unwrap_or(1);
        let mut storage_stats = crate::stats_admin::storage_stats(storage, db, coll, scale)?;
        if let Some(n) = buckets {
            storage_stats.insert("count", crate::admin::int_bson(n));
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
        if !exists {
            return Err(missing("count"));
        }
        out.insert("count", crate::admin::int_bson(count));
    }
    Ok(vec![out])
}

/// A `$collStats` specification, checked as mongod's parser checks it
/// (8.2.11): a document of the sections it knows, each a document, with a
/// `storageStats.scale` of 1 or more and a boolean `histograms`.
fn coll_stats_spec(spec: Option<&Bson>) -> Result<Document, CommandError> {
    let wrong = |path: &str, value: &Bson, expected: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{path}' is the wrong type '{}', expected {expected}",
                secantus_core::query::bson_type_name(value)
            ),
        )
    };
    let spec = match spec {
        Some(Bson::Document(spec)) => spec,
        other => {
            let shown =
                other.map_or_else(|| "null".to_string(), secantus_core::query::bson_value_repr);
            return Err(CommandError::new(
                5447000,
                "Location5447000",
                format!("$collStats must take a nested object but found: $collStats: {shown}"),
            ));
        }
    };
    for (key, value) in spec {
        let path = format!("$collStats.{key}");
        match (key.as_str(), value) {
            (
                "latencyStats" | "storageStats" | "count" | "queryExecStats" | "operationStats",
                Bson::Document(section),
            ) => match (
                key.as_str(),
                section.get("scale"),
                section.get("histograms"),
            ) {
                ("storageStats", Some(scale), _) if !matches!(scale, Bson::Null) => {
                    let Some(n) = crate::util::as_i64(scale).filter(|_| {
                        matches!(
                            scale,
                            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
                        )
                    }) else {
                        return Err(wrong(
                            &format!("{path}.scale"),
                            scale,
                            "types '[decimal, double, int, long]'",
                        ));
                    };
                    if n < 1 {
                        return Err(bad_value(format!(
                            "BSON field 'scale' value must be >= 1, actual value '{n}'"
                        )));
                    }
                }
                ("latencyStats", _, Some(flag)) if !matches!(flag, Bson::Boolean(_)) => {
                    return Err(CommandError::new(
                        14,
                        "TypeMismatch",
                        format!(
                            "Field 'histograms' should be a boolean value, but found: {}",
                            secantus_core::query::bson_type_name(flag)
                        ),
                    ));
                }
                _ => {}
            },
            (
                "latencyStats" | "storageStats" | "count" | "queryExecStats" | "operationStats",
                Bson::Null,
            ) => {}
            (
                "latencyStats" | "storageStats" | "count" | "queryExecStats" | "operationStats",
                v,
            ) => return Err(wrong(&path, v, "type 'object'")),
            ("targetAllNodes" | "$_requestOnTimeseriesView", _) => {}
            _ => {
                return Err(CommandError::new(
                    40415,
                    "IDLUnknownField",
                    format!("BSON field '{path}' is an unknown field."),
                ));
            }
        }
    }
    Ok(spec.clone())
}

/// `$indexStats` — one doc per index (mirrors `aggregate._stage_index_stats`).
fn apply_index_stats(
    spec: Option<&Bson>,
    db: &str,
    coll: Option<&str>,
    storage: &dyn crate::storage::Storage,
) -> Result<Vec<Document>, CommandError> {
    if !matches!(spec, Some(Bson::Document(d)) if d.is_empty()) {
        return Err(CommandError::new(
            28803,
            "Location28803",
            "The $indexStats stage specification must be an empty object",
        ));
    }
    let coll = coll.ok_or_else(|| bad_value("$indexStats requires a collection"))?;
    let mut indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    // A time-series collection has no `_id` index.
    if storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        indexes.retain(|ix| ix.get_str("name") != Ok("_id_"));
    }
    Ok(indexes
        .into_iter()
        .map(|ix| {
            // The index as `listIndexes` shows it, less what only the
            // catalog keeps.
            let mut spec = doc! {
                "v": 2_i32,
                "key": ix.get_document("key").cloned().unwrap_or_default(),
                "name": ix.get_str("name").unwrap_or("").to_string(),
            };
            for (option, value) in &ix {
                if !matches!(option.as_str(), "v" | "key" | "name" | "ns")
                    && !crate::admin::INDEX_CATALOG_ONLY.contains(&option.as_str())
                {
                    spec.insert(option.clone(), value.clone());
                }
            }
            let name = ix.get_str("name").unwrap_or("").to_string();
            // Each plan that read the index, since it was made or this
            // process began, whichever was later.
            let (ops, since) = storage.index_use(db, coll, &name).unwrap_or((0, None));
            doc! {
                "name": name,
                "key": ix.get_document("key").cloned().unwrap_or_default(),
                "host": "secantus",
                "accesses": {
                    "ops": ops,
                    "since": since.unwrap_or_else(crate::handshake::process_started),
                },
                "spec": spec,
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
            // The `errInfo` an insert carries.
            let mut e = CommandError::new(
                121,
                "DocumentValidationFailure",
                "Document failed validation",
            );
            let mut extra = Document::new();
            extra.insert("errInfo", crate::crud::validation_error_info(&validator, d));
            e.extra = Some(Box::new(extra));
            return Err(e);
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
    // What an insert refuses about a document, `$out` refuses too, and
    // leaves the target as it was.
    if let Some((code, errmsg)) = docs.iter().find_map(crate::crud::insert_document_error) {
        return Err(CommandError::new(
            code,
            crate::util::error_code_name(code),
            errmsg,
        ));
    }
    enforce_target_validator(storage, &out_db, &out_coll, &docs, bypass_validation)?;
    // `$out` replaces the target's DOCUMENTS. Its options and indexes stay:
    // mongod builds the replacement with both and swaps it in (measured
    // 8.2.11, 2026-10-11). Until then the target was dropped and created
    // bare, so a validator and every index on it were silently lost.
    let existed = storage
        .collection_exists(&out_db, &out_coll)
        .map_err(command_error)?;
    let mut options = if existed {
        storage
            .get_collection_options(&out_db, &out_coll)
            .map_err(command_error)?
    } else {
        Document::new()
    };
    options.remove("uuid");
    if options.get_bool("capped").unwrap_or(false) {
        return Err(CommandError::new(
            17152,
            "Location17152",
            format!("namespace '{out_db}.{out_coll}' is capped so it can't be used for $out"),
        ));
    }
    let indexes: Vec<Document> = if existed {
        storage
            .list_indexes(&out_db, &out_coll)
            .map_err(command_error)?
            .into_iter()
            .filter(|ix| ix.get_str("name") != Ok("_id_"))
            .collect()
    } else {
        Vec::new()
    };
    // `{db, coll, timeseries}` writes into a time-series collection: a new
    // one with that definition, or an existing one that has it.
    let ns = format!("{out_db}.{out_coll}");
    let wanted = match spec {
        Some(Bson::Document(d)) if d.contains_key("timeseries") => crate::timeseries::definition(
            &doc! { "timeseries": d.get("timeseries").cloned() },
            &ns,
        )?,
        _ => None,
    };
    let mut default_index = None;
    if wanted.is_some() && existed && !options.contains_key("timeseries") {
        return Err(CommandError::new(
            7268700,
            "Location7268700",
            "Cannot create a time-series collection from a non time-series collection or view.",
        ));
    }
    if let Some(wanted) = wanted {
        match options.get_document("timeseries") {
            Ok(found) if found == &wanted => {}
            Ok(found) => {
                return Err(CommandError::new(
                    7406103,
                    "Location7406103",
                    format!(
                        "Time-series options inputted must match the existing time-series \
                         collection. Received: {}Found: timeseries: {}",
                        // The definition as the stage gave it, then as stored.
                        argtypes::render_stage_value(
                            spec.and_then(Bson::as_document)
                                .and_then(|d| d.get("timeseries"))
                                .unwrap_or(&Bson::Null)
                        ),
                        argtypes::render_stage_value(&Bson::Document(found.clone())),
                    ),
                ));
            }
            Err(_) => {
                options.insert("timeseries", wanted);
                default_index =
                    crate::timeseries::Fields::of(&options).and_then(|f| f.default_index());
            }
        }
    }
    // The replacement is built beside the target and swapped in, so a
    // document or index build it refuses (a duplicate `_id`, a unique index
    // the output breaks) leaves the target as it was. It used to be dropped
    // first, and a failed `$out` destroyed what it was meant to replace.
    let tmp = format!("tmp.agg_out.{}", bson::oid::ObjectId::new().to_hex());
    let build = (|| {
        storage
            .create_collection_with_options(&out_db, &tmp, &options)
            .map_err(command_error)?;
        if !docs.is_empty() {
            insert_or_fail(storage, &out_db, &tmp, encode_docs(docs)?)?;
        }
        for ix in &indexes {
            let (Ok(name), Ok(key)) = (ix.get_str("name"), ix.get_document("key")) else {
                continue;
            };
            storage
                .create_index(&out_db, &tmp, name, key, ix)
                .map_err(command_error)?;
        }
        if let Some((name, key)) = default_index {
            let index = doc! { "key": key.clone(), "name": name.clone() };
            storage
                .create_index(&out_db, &tmp, &name, &key, &index)
                .map_err(command_error)?;
        }
        Ok(())
    })();
    if let Err(e) = build {
        storage
            .drop_collection(&out_db, &tmp)
            .map_err(command_error)?;
        return Err(e);
    }
    let (renamed, why) = storage
        .rename_collection(&out_db, &tmp, &out_db, &out_coll, true)
        .map_err(command_error)?;
    if !renamed {
        storage
            .drop_collection(&out_db, &tmp)
            .map_err(command_error)?;
        return Err(CommandError::new(
            8,
            "UnknownError",
            format!(
                "$out could not replace {out_db}.{out_coll}: {}",
                why.unwrap_or_default()
            ),
        ));
    }
    Ok(Vec::new())
}

/// Resolve `$out`'s target. Accepts `"coll"` or `{db, coll}`.
pub(crate) fn out_target(spec: Option<&Bson>, db: &str) -> Result<(String, String), CommandError> {
    match spec {
        // An EMPTY collection name is not a namespace. It used to be accepted
        // and the stage wrote to a nameless collection, reporting success --
        // a `$out` that silently did nothing. mongod renders the namespace with
        // the collection half empty, so the database name stands alone (73).
        Some(Bson::String(c)) if c.is_empty() => Err(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Invalid $out target namespace, {db}"),
        )),
        Some(Bson::String(c)) => out_name(db.to_string(), c.clone()),
        Some(Bson::Document(d)) => {
            let wrong_type = |field: &str, value: &Bson, expected: &str| {
                CommandError::new(
                    14,
                    "TypeMismatch",
                    format!(
                        "BSON field '$out.{field}' is the wrong type '{}', expected type \
                         '{expected}'",
                        secantus_core::query::bson_type_name(value)
                    ),
                )
            };
            let (mut out_db, mut out_coll) = (None, None);
            for (key, value) in d {
                match (key.as_str(), value) {
                    ("db", Bson::String(s)) => out_db = Some(s.clone()),
                    ("coll", Bson::String(s)) => out_coll = Some(s.clone()),
                    ("db" | "coll", Bson::Null) => {}
                    ("db" | "coll", other) => return Err(wrong_type(key, other, "string")),
                    ("timeseries", Bson::Document(_) | Bson::Null) => {}
                    ("timeseries", other) => return Err(wrong_type(key, other, "object")),
                    _ => {}
                }
            }
            let coll = out_coll.ok_or_else(|| bad_value("$out target document requires 'coll'"))?;
            // The document form names its database: it is not optional
            // (measured 8.2.11, 2026-10-13; the current one was assumed).
            let Some(out_db) = out_db else {
                return Err(CommandError::new(
                    40414,
                    "IDLFailedToParse",
                    "BSON field '$out.db' is missing but a required field",
                ));
            };
            if out_db.is_empty() {
                return Err(CommandError::new(
                    73,
                    "InvalidNamespace",
                    format!("Invalid $out target namespace, .{coll}"),
                ));
            }
            if coll.is_empty() {
                return Err(CommandError::new(
                    73,
                    "InvalidNamespace",
                    format!("Invalid $out target namespace, {out_db}"),
                ));
            }
            out_name(out_db, coll)
        }
        _ => Err(bad_value("$out requires a collection name or {db, coll}")),
    }
}

/// A `$out` target mongod will not write: a name no collection can have, or
/// one of the system's own. These were written.
fn out_name(db: String, coll: String) -> Result<(String, String), CommandError> {
    if coll.starts_with('.') {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Collection names cannot start with '.': {coll}"),
        ));
    }
    if coll.starts_with("system.") {
        return Err(CommandError::new(
            17385,
            "Location17385",
            format!("Can't $out to special collection: {coll}"),
        ));
    }
    let refused = if coll.contains('$') {
        Some(format!("Invalid collection name: {coll}"))
    } else {
        crate::admin::invalid_write_namespace(&db, &coll).map(|e| e.errmsg)
    };
    match refused {
        Some(reason) => Err(CommandError::new(
            20,
            "IllegalOperation",
            format!("error with target namespace: {reason}"),
        )),
        None => Ok((db, coll)),
    }
}

/// `$merge` — write the pipeline's documents into the target collection.
///
/// mongod writes them with the commands a client would use, and this does
/// the same, so the stage stores what those commands store (measured 8.2.11,
/// 2026-10-13). For a document and the `on` fields' values `q`:
///
/// | `whenMatched`  | the write                                         |
/// | -------------- | ------------------------------------------------- |
/// | `merge`        | `update {q, u: {$set: doc}}`                      |
/// | `replace`      | `update {q, u: doc}`                              |
/// | `keepExisting` | `update {q, u: {$setOnInsert: doc}, upsert: true}` |
/// | a pipeline     | `update {q, u: pipeline, c: {new: doc}}`          |
/// | `fail`         | `insert doc`                                      |
///
/// `whenNotMatched: "insert"` makes the update an upsert. So `merge` sets
/// each top-level field WHOLE (a sub-document is replaced, not merged into),
/// a field name with a dot in it is a path, and new fields land in the
/// order `$set` gives them. This used to merge sub-documents field by field
/// and stored a different document.
///
/// The writes of a batch are all attempted and the first failure is what the
/// stage reports, as an unordered batch: the documents after a refused one
/// are still written. It used to check every document against the target's
/// validator first and write none, and to stop at the first refused write.
#[allow(clippy::too_many_arguments)]
fn apply_merge(
    spec: Option<&Bson>,
    docs: Vec<Document>,
    db: &str,
    source: Option<&str>,
    vars: &Document,
    storage: &dyn crate::storage::Storage,
    collation: Option<&Collation>,
    bypass_validation: bool,
) -> Result<Vec<Document>, CommandError> {
    let merge = merge_spec(spec, db)?;
    // A `let` that names a variable nobody defined.
    if let Some(bindings) = &merge.let_vars {
        let bound: Vec<String> = vars.keys().cloned().collect();
        for expr in bindings.values() {
            if let Some((code, errmsg)) = argtypes::expression_problem(expr, &bound) {
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    errmsg,
                ));
            }
        }
    }
    let source_ns = format!("{db}.{}", source.unwrap_or(""));
    let during = |mut e: CommandError| {
        e.errmsg = wrap_pipeline_error(e.errmsg, false, Some(&source_ns));
        e
    };
    // `$merge` writes to a collection, and mongod presents a time-series
    // collection as a view.
    if storage
        .get_collection_options(&merge.db, &merge.coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        return Err(during(crate::timeseries::is_a_view(&format!(
            "{}.{}",
            merge.db, merge.coll
        ))));
    }
    // mongod requires a unique index covering a non-`_id` `on` so the match is
    // guaranteed to hit at most one target doc.
    merge_validate_on(storage, &merge.db, &merge.coll, &merge.on, collation)?;
    if merge.coll.starts_with('.') {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Collection names cannot start with '.': {}", merge.coll),
        ));
    }
    if merge.coll.starts_with("system.") {
        return Err(CommandError::new(
            31319,
            "Location31319",
            format!("Cannot $merge to special collection: {}", merge.coll),
        ));
    }
    if docs.is_empty() {
        return Ok(Vec::new());
    }
    // The names a write refuses are refused when the first document is
    // written, so a pipeline that produces none says nothing about them.
    if merge.coll.contains('$') {
        return Err(during(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Invalid collection name: {}", merge.coll),
        )));
    }
    if let Some(e) = crate::admin::invalid_write_namespace(&merge.db, &merge.coll) {
        return Err(during(e));
    }

    // Each document with the values of its `on` fields.
    let mut writes: Vec<(Document, Document)> = Vec::with_capacity(docs.len());
    for mut doc in docs {
        if merge.on.iter().any(|f| f == "_id") && !doc.contains_key("_id") {
            let mut with_id = doc! { "_id": secantus_core::oid::new() };
            with_id.extend(doc);
            doc = with_id;
        }
        let mut q = Document::new();
        for field in &merge.on {
            let value = match secantus_core::get_path(&doc, field) {
                Some(Bson::Array(_)) => {
                    return Err(during(CommandError::new(
                        51185,
                        "Location51185",
                        format!("$merge write error: 'on' field '{field}' is an array"),
                    )))
                }
                Some(v) => v.clone(),
                None => Bson::Null,
            };
            q.insert(field.clone(), value);
        }
        writes.push((q, doc));
    }

    // Writing into a target that is not there creates it: two write rounds
    // more on mongod 8.2.11, as an insert that creates its collection takes.
    if !writes.is_empty()
        && !storage
            .collection_exists(&merge.db, &merge.coll)
            .unwrap_or(true)
    {
        drop(crate::lock_manager::write_rounds(&merge.db, &merge.coll, 2));
    }
    let upsert = merge.not_matched == "insert";
    let mut first: Option<CommandError> = None;
    let note = |e: CommandError, first: &mut Option<CommandError>| {
        if first.is_none() {
            *first = Some(e);
        }
    };
    if matches!(&merge.matched, WhenMatched::Mode(m) if m == "fail") {
        // Plain inserts, unordered: what is already there is the failure.
        for chunk in writes.chunks(MERGE_BATCH) {
            let mut encoded = Vec::with_capacity(chunk.len());
            for (_, doc) in chunk {
                if let Some((code, errmsg)) = crate::crud::insert_document_error(doc) {
                    note(
                        CommandError::new(code, crate::util::error_code_name(code), errmsg),
                        &mut first,
                    );
                    continue;
                }
                if let Err(e) = enforce_target_validator(
                    storage,
                    &merge.db,
                    &merge.coll,
                    std::slice::from_ref(doc),
                    bypass_validation,
                ) {
                    note(e, &mut first);
                    continue;
                }
                encoded.extend(encode_docs(vec![doc.clone()])?);
            }
            if encoded.is_empty() {
                continue;
            }
            // One round for the batch (mongod 8.2.11, when it inserts
            // cleanly; its retries after a duplicate are not reproduced).
            let _round = crate::lock_manager::write_rounds(&merge.db, &merge.coll, 1);
            let (_, errors) = storage
                .insert(&merge.db, &merge.coll, encoded, false)
                .map_err(command_error)?;
            if let Some(error) = errors.into_iter().next() {
                let mut e = write_error_of(&error);
                if e.code == 11000 {
                    e.errmsg = format!(
                        "$merge with whenMatched: fail found an existing document with the same \
                         values for the 'on' fields :: caused by :: {}",
                        e.errmsg
                    );
                }
                note(e, &mut first);
            }
            if first.is_some() {
                break;
            }
        }
        return match first {
            Some(e) => Err(during(e)),
            None => Ok(Vec::new()),
        };
    }

    // The variables a `whenMatched` pipeline sees beside `$$new`: the
    // command's own, and the stage's `let`, read from each source document.
    let statement = |q: &Document, doc: &Document| -> Result<Document, CommandError> {
        let mut st = doc! { "q": q.clone() };
        match &merge.matched {
            WhenMatched::Pipeline(pipeline) => {
                let mut constants = vars.clone();
                if let Some(bindings) = &merge.let_vars {
                    for (name, expr) in bindings {
                        let value = secantus_core::expressions::evaluate(doc, expr, vars)
                            .map_err(|_| bad_value("$merge could not evaluate a 'let' variable"))?;
                        constants.insert(name.clone(), value);
                    }
                }
                constants.insert("new", doc.clone());
                st.insert("u", pipeline.clone());
                st.insert("c", constants);
                st.insert("upsert", upsert);
                st.insert("upsertSupplied", upsert);
            }
            WhenMatched::Mode(mode) => match mode.as_str() {
                "replace" => {
                    st.insert("u", doc.clone());
                    st.insert("upsert", upsert);
                }
                "keepExisting" => {
                    st.insert("u", doc! { "$setOnInsert": doc.clone() });
                    st.insert("upsert", true);
                }
                _ => {
                    st.insert("u", doc! { "$set": doc.clone() });
                    st.insert("upsert", upsert);
                }
            },
        }
        st.insert("multi", false);
        Ok(st)
    };
    // `whenNotMatched: "fail"` needs each statement's own count, so those go
    // one to a command.
    let per_command = if merge.not_matched == "fail" {
        1
    } else {
        MERGE_BATCH
    };
    for chunk in writes.chunks(per_command) {
        let mut statements: Vec<Bson> = Vec::with_capacity(chunk.len());
        for (q, doc) in chunk {
            statements.push(Bson::Document(statement(q, doc)?));
        }
        let mut command = doc! {
            "update": merge.coll.clone(),
            "updates": statements,
            "ordered": false,
        };
        if bypass_validation {
            command.insert("bypassDocumentValidation", true);
        }
        // A write round per document, as mongod takes them.
        let _rounds = crate::lock_manager::write_rounds(&merge.db, &merge.coll, chunk.len());
        let reply =
            match crate::crud::update_in(&command, &merge.db, storage, false, None, Err(false)) {
                Ok(reply) => reply,
                Err(e) => return Err(during(e)),
            };
        if reply.get_f64("ok").unwrap_or(0.0) != 1.0 {
            return Err(during(write_error_of(&reply)));
        }
        if let Some(error) = reply
            .get_array("writeErrors")
            .ok()
            .and_then(|errors| errors.first())
            .and_then(Bson::as_document)
        {
            let mut e = write_error_of(error);
            let prefix = match e.code {
                11000 => "$merge failed due to a DuplicateKey error :: caused by :: ",
                66 => {
                    "$merge failed to update the matching document, did you attempt to modify \
                     the _id or the shard key? :: caused by :: "
                }
                _ => "",
            };
            e.errmsg = format!("{prefix}{}", e.errmsg);
            note(e, &mut first);
        } else if merge.not_matched == "fail" && crate::util::as_i64_field(&reply, "n") == 0 {
            note(
                CommandError::new(
                    13113,
                    "MergeStageNoMatchingDocument",
                    "$merge could not find a matching document in the target collection for at \
                     least one document in the source collection",
                ),
                &mut first,
            );
        }
        if first.is_some() && per_command > 1 {
            break;
        }
    }
    match first {
        Some(e) => Err(during(e)),
        None => Ok(Vec::new()),
    }
}

/// The most statements one write command takes, which is as many as mongod
/// puts in one of the stage's batches.
const MERGE_BATCH: usize = 100_000;

/// A write error, or a failed reply, as the error a stage raises: its code
/// and message, and whatever else it carried (`keyPattern`, `errInfo`).
fn write_error_of(error: &Document) -> CommandError {
    let code = crate::util::as_i64_field(error, "code") as i32;
    let errmsg = error.get_str("errmsg").unwrap_or("write failed");
    let mut extra = error.clone();
    for key in [
        "index",
        "code",
        "codeName",
        "errmsg",
        "ok",
        "n",
        "nModified",
    ] {
        extra.remove(key);
    }
    let e = CommandError::new(code, crate::util::error_code_name(code), errmsg);
    if extra.is_empty() {
        e
    } else {
        e.with_extra(extra)
    }
}

/// Insert into a stage's target, failing on the first document the storage
/// refuses. `Storage::insert` REPORTS a refused document in its result rather
/// than failing, and dropping that result lost the document without a word.
pub(crate) fn insert_or_fail(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    docs: Vec<Vec<u8>>,
) -> Result<(), CommandError> {
    let (_, errors) = storage
        .insert(db, coll, docs, true)
        .map_err(command_error)?;
    match errors.into_iter().next() {
        Some(first) => Err(write_error_of(&first)),
        None => Ok(()),
    }
}

/// `$merge` `whenMatched`: either a named mode or an inline aggregation pipeline
/// applied to each matched document (with the incoming doc bound to `$$new`).
enum WhenMatched {
    Mode(String),
    Pipeline(Vec<Bson>),
}

/// A `$merge` stage as given: where it writes, the fields that pair a
/// document with one already there, what to do with each kind, and the
/// stage's own variables.
struct MergeSpec {
    db: String,
    coll: String,
    on: Vec<String>,
    matched: WhenMatched,
    not_matched: String,
    let_vars: Option<Document>,
}

/// Parse `$merge`'s spec, refusing what mongod refuses in the order it
/// refuses it (measured 8.2.11, 2026-10-13): each field as the document has
/// it, then `whenNotMatched`'s value, the pair of modes, and `let`.
fn merge_spec(spec: Option<&Bson>, db: &str) -> Result<MergeSpec, CommandError> {
    let located = |code: i32, msg: String| CommandError::new(code, format!("Location{code}"), msg);
    let wrong_type = |path: &str, value: &Bson, expected: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
                secantus_core::query::bson_type_name(value)
            ),
        )
    };
    let fields = match spec {
        // Empty target: not a namespace. mongod QUOTES it here and does not in
        // `$out` -- the two messages really are shaped differently.
        Some(Bson::String(c)) if c.is_empty() => {
            return Err(CommandError::new(
                73,
                "InvalidNamespace",
                format!("Invalid $merge target namespace: '{db}'"),
            ));
        }
        Some(Bson::String(c)) => {
            return Ok(MergeSpec {
                db: db.to_string(),
                coll: c.clone(),
                on: vec!["_id".to_string()],
                matched: WhenMatched::Mode("merge".to_string()),
                not_matched: "insert".to_string(),
                let_vars: None,
            })
        }
        Some(Bson::Document(d)) => d,
        _ => return Err(bad_value("$merge requires a string or document spec")),
    };
    let mut target: Option<(String, String)> = None;
    let mut on = vec!["_id".to_string()];
    let mut matched = WhenMatched::Mode("merge".to_string());
    let mut not_matched = "insert".to_string();
    let mut let_vars: Option<Document> = None;
    for (key, value) in fields {
        match key.as_str() {
            "into" => match value {
                Bson::String(c) if c.is_empty() => {
                    return Err(located(
                        5786800,
                        "$merge 'into' field cannot be an empty string".to_string(),
                    ))
                }
                Bson::String(c) => target = Some((db.to_string(), c.clone())),
                Bson::Document(into) => {
                    let (mut out_db, mut out_coll) = (None, None);
                    for (name, part) in into {
                        let slot = match name.as_str() {
                            "db" => &mut out_db,
                            "coll" => &mut out_coll,
                            other => {
                                return Err(CommandError::new(
                                    40415,
                                    "IDLUnknownField",
                                    format!("BSON field 'into.{other}' is an unknown field."),
                                ))
                            }
                        };
                        match part {
                            Bson::String(s) => *slot = Some(s.clone()),
                            Bson::Null => {}
                            other => {
                                return Err(wrong_type(&format!("into.{name}"), other, "string"))
                            }
                        }
                    }
                    let Some(out_coll) = out_coll.filter(|c| !c.is_empty()) else {
                        return Err(located(
                            5786801,
                            "$merge 'into' field must specify a 'coll' that is not empty, null \
                             or undefined"
                                .to_string(),
                        ));
                    };
                    let out_db = out_db.filter(|d| !d.is_empty());
                    target = Some((out_db.unwrap_or_else(|| db.to_string()), out_coll));
                }
                other => {
                    return Err(located(
                        51178,
                        format!(
                            "$merge 'into' field  must be either a string or an object, but \
                             found {}",
                            secantus_core::query::bson_type_name(other)
                        ),
                    ))
                }
            },
            "on" => {
                on = match value {
                    Bson::String(s) => vec![s.clone()],
                    Bson::Array(items) => {
                        let mut names = Vec::with_capacity(items.len());
                        for item in items {
                            match item {
                                Bson::String(s) => names.push(s.clone()),
                                other => {
                                    return Err(located(
                                        51134,
                                        format!(
                                            "$merge 'on' array elements must be strings, but \
                                             found {}",
                                            secantus_core::query::bson_type_name(other)
                                        ),
                                    ))
                                }
                            }
                        }
                        if names.is_empty() {
                            return Err(located(
                                51187,
                                "If explicitly specifying $merge 'on', must include at least \
                                 one field"
                                    .to_string(),
                            ));
                        }
                        names
                    }
                    other => {
                        return Err(located(
                            51186,
                            format!(
                                "$merge 'on' field  must be either a string or an array of \
                                 strings, but found {}",
                                secantus_core::query::bson_type_name(other)
                            ),
                        ))
                    }
                };
                for (index, name) in on.iter().enumerate() {
                    if name.is_empty() {
                        return Err(located(
                            40352,
                            "FieldPath cannot be constructed with empty string".to_string(),
                        ));
                    }
                    if on[..index].contains(name) {
                        return Err(located(31465, format!("Found a duplicate field '{name}'")));
                    }
                }
            }
            "whenMatched" => {
                matched = match value {
                    Bson::Array(stages) => {
                        for stage in stages {
                            let Bson::Document(stage) = stage else {
                                return Err(CommandError::new(
                                    14,
                                    "TypeMismatch",
                                    "Each element of the 'pipeline' array must be an object",
                                ));
                            };
                            let name = stage.keys().next().map(String::as_str).unwrap_or("");
                            if !recognized_stage(name) {
                                return Err(located(
                                    40324,
                                    format!("Unrecognized pipeline stage name: '{name}'"),
                                ));
                            }
                        }
                        WhenMatched::Pipeline(stages.clone())
                    }
                    Bson::String(mode) => {
                        const MODES: &[&str] = &["merge", "replace", "keepExisting", "fail"];
                        if !MODES.contains(&mode.as_str()) {
                            return Err(bad_value(format!(
                                "Enumeration value '{mode}' for field 'whenMatched' is not a \
                                 valid value."
                            )));
                        }
                        WhenMatched::Mode(mode.clone())
                    }
                    other => {
                        return Err(located(
                            51191,
                            format!(
                                "$merge 'whenMatched' field  must be either a string or an \
                                 array, but found {}",
                                secantus_core::query::bson_type_name(other)
                            ),
                        ))
                    }
                }
            }
            "whenNotMatched" => match value {
                Bson::String(mode) if !["insert", "discard", "fail"].contains(&mode.as_str()) => {
                    return Err(bad_value(format!(
                        "Enumeration value '{mode}' for field '$merge.whenNotMatched' is not a \
                         valid value."
                    )))
                }
                Bson::String(mode) => not_matched = mode.clone(),
                Bson::Null => {}
                other => return Err(wrong_type("$merge.whenNotMatched", other, "string")),
            },
            "let" => match value {
                Bson::Document(bindings) => let_vars = Some(bindings.clone()),
                Bson::Null => {}
                other => return Err(wrong_type("$merge.let", other, "object")),
            },
            other => {
                return Err(CommandError::new(
                    40415,
                    "IDLUnknownField",
                    format!("BSON field '$merge.{other}' is an unknown field."),
                ))
            }
        }
    }
    let Some((out_db, out_coll)) = target else {
        return Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            "BSON field '$merge.into' is missing but a required field",
        ));
    };
    let mode_name = match &matched {
        WhenMatched::Mode(mode) => Some(mode.as_str()),
        WhenMatched::Pipeline(_) => None,
    };
    // `keepExisting` and `fail` say what to do with a document that is
    // already there, and only make sense when the others are inserted.
    if matches!(mode_name, Some("keepExisting" | "fail")) && not_matched != "insert" {
        return Err(located(
            51181,
            format!(
                "Combination of $merge modes 'whenMatched: {}' and 'whenNotMatched: \
                 {not_matched}' is not supported",
                mode_name.unwrap_or_default()
            ),
        ));
    }
    if let Some(bindings) = &let_vars {
        if let Some(mode) = mode_name {
            return Err(located(
                51199,
                format!("Cannot use 'let' variables with 'whenMatched: {mode}' mode"),
            ));
        }
        for (name, expr) in bindings {
            let starts = name.chars().next();
            if !starts.is_some_and(|c| c.is_ascii_lowercase() || !c.is_ascii()) {
                return Err(CommandError::new(
                    9,
                    "FailedToParse",
                    format!("'{name}' starts with an invalid character for a user variable name"),
                ));
            }
            if name == "new" && expr.as_str() != Some("$$ROOT") {
                return Err(located(
                    51273,
                    "'let' may not define a value for the reserved 'new' variable other than \
                     '$$ROOT'"
                        .to_string(),
                ));
            }
            if let Some(op) = secantus_core::expressions::first_unknown_expr_operator(expr) {
                return Err(CommandError::new(
                    168,
                    "InvalidPipelineOperator",
                    format!("Unrecognized expression '{op}'"),
                ));
            }
        }
    }
    Ok(MergeSpec {
        db: out_db,
        coll: out_coll,
        on,
        matched,
        not_matched,
        let_vars,
    })
}

/// Validate that a non-`_id` `$merge` `on` is backed by a unique index on the
/// target whose key fields are exactly the `on` fields (mongod code 51183). The
/// default `on: ["_id"]` is always satisfied (the `_id` index is unique).
///
/// The index holds every document (a partial one does not count) and
/// compares as the pipeline does: one built under another collation, or on
/// a collection whose default is another, does not count either.
fn merge_validate_on(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    on: &[String],
    collation: Option<&Collation>,
) -> Result<(), CommandError> {
    if on.len() == 1 && on[0] == "_id" {
        return Ok(());
    }
    let simple = |spec: Option<&Document>| -> Option<(String, i64)> {
        let spec = spec?;
        let locale = spec.get_str("locale").unwrap_or("simple");
        if locale == "simple" {
            return None;
        }
        let strength = spec
            .get("strength")
            .and_then(crate::util::as_i64)
            .unwrap_or(3);
        Some((locale.to_string(), strength))
    };
    let pipeline_collation = simple(collation.map(|c| c.spec()));
    let options = storage.get_collection_options(db, coll).unwrap_or_default();
    let default_collation = options.get_document("collation").ok();
    let mut want: Vec<&str> = on.iter().map(String::as_str).collect();
    want.sort_unstable();
    let indexes = storage.list_indexes(db, coll).unwrap_or_default();
    for idx in &indexes {
        if !idx.get_bool("unique").unwrap_or(false) || idx.contains_key("partialFilterExpression") {
            continue;
        }
        let index_collation = simple(idx.get_document("collation").ok().or(default_collation));
        if index_collation != pipeline_collation {
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
        "Cannot find index to verify that join fields will be unique",
    ))
}

/// Whether the first pipeline stage has `key`.
fn first_stage_has(pipeline: &[Bson], key: &str) -> bool {
    matches!(pipeline.first(), Some(Bson::Document(d)) if d.contains_key(key))
}

/// Whether the first pipeline stage has any of `keys`.
fn first_stage_has_any(pipeline: &[Bson], keys: &[&str]) -> bool {
    matches!(pipeline.first(), Some(Bson::Document(d)) if keys.iter().any(|k| d.contains_key(*k)))
}

/// What the front of a pipeline, past its lifted `$match`, asks of the read.
#[derive(Default)]
struct LeadingSort {
    /// A plain `$sort` (every key a field, every value 1 or -1).
    sort: Option<Document>,
    /// The `$match` right after that `$sort`, when nothing was lifted.
    filter: Option<Document>,
    /// How many documents the stages after the `$sort` take; 0 is all.
    limit: i64,
}

fn leading_sort(rest: &[Bson], nothing_lifted: bool) -> LeadingSort {
    let stage = |i: usize, name: &str| {
        rest.get(i)
            .and_then(Bson::as_document)
            .filter(|d| d.len() == 1)
            .and_then(|d| d.get(name))
    };
    let Some(Bson::Document(spec)) = stage(0, "$sort") else {
        return LeadingSort::default();
    };
    if !crate::explain::order::plain_sort(spec) {
        return LeadingSort::default();
    }
    let mut out = LeadingSort {
        sort: Some(spec.clone()),
        ..LeadingSort::default()
    };
    let mut at = 1;
    if nothing_lifted {
        if let Some(Bson::Document(m)) = stage(1, "$match") {
            out.filter = Some(m.clone());
            at = 2;
        }
    }
    let whole = |v: &Bson| as_i64(v).filter(|n| *n >= 0);
    let skip = stage(at, "$skip").and_then(whole);
    if skip.is_some() {
        at += 1;
    }
    if let Some(limit) = stage(at, "$limit").and_then(whole).filter(|n| *n > 0) {
        out.limit = limit + skip.unwrap_or(0);
    }
    out
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

#[cfg(test)]
mod malformed_pipeline_tests {
    use super::*;
    use bson::doc;

    /// Values probed against real mongod (6.0.16 and 8.3.4 agree): every
    /// non-document element of `pipeline` is 14 TypeMismatch with one wording.
    #[test]
    fn non_document_element_is_type_mismatch() {
        for bad in [
            Bson::Int32(42),
            Bson::String("stage".into()),
            Bson::Array(vec![Bson::String("nested".into())]),
            Bson::Null,
            Bson::Double(3.5),
            Bson::Boolean(true),
        ] {
            let err = validate_stage_names(std::slice::from_ref(&bad))
                .expect_err(&format!("{bad:?} should be rejected"));
            assert_eq!(err.code, 14, "{bad:?}");
            assert_eq!(err.code_name, "TypeMismatch", "{bad:?}");
            assert_eq!(err.errmsg, PIPELINE_ELEMENT_MSG, "{bad:?}");
        }
    }

    /// A document of the wrong arity is a *different* error from the wrong
    /// type: Location40323, with a trailing period.
    #[test]
    fn wrong_arity_stage_is_location_40323() {
        for bad in [
            doc! {},
            doc! {"$match": {}, "$count": "n"},
            doc! {"$limit": 1, "$count": "n"},
        ] {
            let err = validate_stage_names(&[Bson::Document(bad.clone())])
                .expect_err(&format!("{bad:?} should be rejected"));
            assert_eq!(err.code, 40323, "{bad:?}");
            assert_eq!(err.code_name, "Location40323", "{bad:?}");
            assert_eq!(err.errmsg, STAGE_ARITY_MSG, "{bad:?}");
        }
    }

    #[test]
    fn well_formed_pipeline_still_validates() {
        validate_stage_names(&[
            Bson::Document(doc! {"$match": {"x": 1}}),
            Bson::Document(doc! {"$count": "n"}),
        ])
        .expect("a valid pipeline must not be rejected");
    }
}

#[cfg(test)]
mod geo_near_candidate_tests {
    use super::*;

    fn unit(lng: f64, lat: f64) -> [f64; 3] {
        let (lng, lat) = (lng.to_radians(), lat.to_radians());
        [lat.cos() * lng.cos(), lat.cos() * lng.sin(), lat.sin()]
    }

    fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    /// The angle from `p` to the great circle through `a` and `b`.
    fn to_great_circle(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
        let n = cross(a, b);
        (dot(p, n).abs() / dot(n, n).sqrt()).asin()
    }

    /// Every side of the candidate polygon passes outside the disc, so a
    /// shape that reaches into the disc meets the polygon.
    #[test]
    fn the_candidate_polygon_holds_the_disc() {
        for (lng, lat, r) in [
            (0.0, 0.0, 0.0157),
            (179.9, 10.0, 0.3),
            (-73.9, 40.7, 1e-7),
            (20.0, 89.0, 0.05),
            (0.0, -60.0, 1.0),
        ] {
            let ring = disc_polygon(lng, lat, r).expect("a polygon");
            assert_eq!(ring.len(), 17);
            let centre = unit(lng, lat);
            let pts: Vec<[f64; 3]> = ring
                .iter()
                .map(|p| {
                    let p = p.as_array().unwrap();
                    unit(bson_f64(&p[0]).unwrap(), bson_f64(&p[1]).unwrap())
                })
                .collect();
            for w in pts.windows(2) {
                let d = to_great_circle(centre, w[0], w[1]);
                assert!(d > r, "a side at {d} from a disc of {r}");
            }
        }
        // Too large for a polygon smaller than a hemisphere.
        assert!(disc_polygon(0.0, 0.0, 1.3).is_none());
    }
}
