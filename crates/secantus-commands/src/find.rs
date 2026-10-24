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

use crate::argtypes;
use crate::cursors::{CursorProducer, CursorRegistry, TailableOptions};
use crate::storage::Storage;
use crate::util::{
    as_i64, bool_field, coll_arg, command_error, doc_field, docs_to_bson, encode_docs_ref,
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
    /// The cursor's query: a document inserted later is returned only if it
    /// matches, and in the cursor's projection. Both were dropped after the
    /// first batch, so a filtered tail returned every new document whole.
    filter: Document,
    projection: Option<Document>,
    let_vars: Document,
    collation: Option<secantus_core::collation::Collation>,
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
                let blobs: Vec<Vec<u8>> = rows.into_iter().map(|(_rid, doc)| doc).collect();
                if self.filter.is_empty() && self.projection.is_none() {
                    return blobs;
                }
                let _active = secantus_core::collation::activate(self.collation.as_ref());
                let mut out = Vec::with_capacity(blobs.len());
                for blob in blobs {
                    let Ok(d) = secantus_core::bsondoc::from_bytes(blob.as_slice()) else {
                        continue;
                    };
                    let keep = self.filter.is_empty()
                        || secantus_core::query::matches(
                            &d,
                            &self.filter,
                            &self.let_vars,
                            self.collation.as_ref(),
                        )
                        .unwrap_or(false);
                    if !keep {
                        continue;
                    }
                    match &self.projection {
                        None => out.push(blob),
                        Some(spec) => {
                            if let Ok(mut projected) = project_to_docs(
                                vec![blob],
                                spec,
                                &self.filter,
                                &self.let_vars,
                                &format!("{}.{}", self.db, self.coll),
                            ) {
                                if let Some(p) = projected.pop() {
                                    if let Ok(mut enc) = crate::util::encode_docs(vec![p]) {
                                        out.append(&mut enc);
                                    }
                                }
                            }
                        }
                    }
                }
                out
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
fn build_view_find_aggregate(doc: &Document, coll: &str, natural_hint: Option<&Bson>) -> Document {
    // Any other hint names an index of the collection beneath the view, and
    // the aggregate reads that collection through it: mongod refuses one
    // that names no index (2) and returns documents in its order.
    let hint = natural_hint.or_else(|| argtypes::given_hint(doc));
    let mut pipeline: Vec<Bson> = Vec::new();
    if let Some(Bson::Document(f)) = doc.get("filter") {
        if !f.is_empty() {
            pipeline.push(Bson::Document(doc! { "$match": f.clone() }));
        }
    }
    // A `$natural` sort is not a `$sort` stage (which would read `$natural`
    // as a field path and fail): it is the direction the BASE collection is
    // scanned in, which the aggregate takes as a hint.
    if let Some(Bson::Document(s)) = doc.get("sort") {
        if !s.is_empty() && natural_hint.is_none() {
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
    if let Some(h) = hint {
        agg.insert("hint", h.clone());
    }
    agg
}

pub fn find(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let reply = find_body(doc, ctx)?;
    // A cursor left open remembers the indexes its plan reads: dropping one
    // kills it (`CursorRegistry::kill_index`), as it kills mongod's plan.
    let open = reply
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get_i64("id").ok())
        .filter(|id| *id != 0);
    if let (Ok(coll), Ok(storage)) = (coll_arg(doc, "find"), ctx.storage()) {
        let filter = doc_field(doc, "filter");
        let collation = crate::util::effective_collation(storage, &ctx.db_name, &coll, doc)
            .ok()
            .flatten();
        let ask = crate::explain::order::Ask {
            db: &ctx.db_name,
            coll: &coll,
            filter: &filter,
            sort: doc.get_document("sort").ok(),
            hint: argtypes::given_hint(doc),
            collation: collation.as_ref(),
            vars: &Document::new(),
            limit: 0,
            asked: Default::default(),
        };
        // The plan ran once: each index it read is used once more.
        crate::explain::order::count_index_use(storage, &ask, crate::explain::order::Uses::Find);
        if let Some(id) = open {
            let indexes = crate::explain::order::plan_indexes(storage, &ask);
            if !indexes.is_empty() {
                if let Ok(cursors) = ctx.cursors() {
                    cursors.set_plan_indexes(id, indexes);
                }
            }
        }
    }
    Ok(reply)
}

fn find_body(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "find")?;
    if coll.starts_with('$') {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Invalid collection name specified '{}.{coll}'", ctx.db_name),
        ));
    }
    if let Some(key) = argtypes::unknown_field(doc, FIND_FIELDS) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'FindCommandRequest.{key}' is an unknown field."),
        ));
    }
    // Argument types, before anything reads them. A wrong-typed slot used to be
    // silently ignored here (`and_then(as_i64)` yields None -> the default), so
    // `find.limit: "x"` returned every document and reported success. mongod
    // reports find's numeric slots under its IDL name, `FindCommandRequest.limit`,
    // NOT `find.limit` -- probed, not guessed. See `crate::argtypes`.
    for field in ["filter", "sort", "projection", "collation"] {
        argtypes::require_object_expected(doc, field)?;
    }
    for field in ["limit", "skip", "batchSize"] {
        argtypes::require_number(doc, field, &format!("FindCommandRequest.{field}"))?;
    }
    argtypes::require_object(doc, "let", "FindCommandRequest.let")?;
    // Negative `skip` / `limit` are refused, not clamped (measured 8.2.11,
    // 2026-10-01: `skip: -1` returned every document).
    for field in ["skip", "limit", "batchSize"] {
        if let Some(n) = doc.get(field).and_then(as_i64) {
            if n < 0 {
                return Err(CommandError::new(
                    2,
                    "BadValue",
                    format!("BSON field '{field}' value must be >= 0, actual value '{n}'"),
                ));
            }
        }
    }
    // `{$natural: ±1}` is storage order, which `find` runs as the `$natural`
    // hint; any other spec is an ordinary sort.
    let natural_hint = match doc.get("sort") {
        Some(Bson::Document(sort)) => {
            let natural = argtypes::natural_sort(sort, doc.get("hint"))?;
            if natural.is_none() {
                argtypes::require_sort_spec(sort)?;
            }
            natural
        }
        _ => None,
    };
    // `min` / `max` are the Expected-field family, which REJECTS an explicit
    // null -- not the BSON-field family, which accepts it. Probed.
    argtypes::require_object_expected(doc, "min")?;
    argtypes::require_object_expected(doc, "max")?;
    argtypes::require_bool_value(doc, "singleBatch")?;
    // The `Field '<name>' should be a boolean value` family, which — unlike the
    // BSON-field family two lines up — REJECTS an explicit null. Five more slots
    // share `singleBatch`'s rule; before this they were accepted and ignored, so
    // `tailable: "x"` silently returned a non-tailable cursor.
    for field in [
        "tailable",
        "awaitData",
        "returnKey",
        "showRecordId",
        "allowDiskUse",
        "noCursorTimeout",
        "allowPartialResults",
        "readOnce",
    ] {
        argtypes::require_bool_value(doc, field)?;
    }
    let tailable_asked = bool_field(doc, "tailable", false);
    if bool_field(doc, "awaitData", false) && !tailable_asked {
        return Err(CommandError::new(
            9,
            "FailedToParse",
            "Cannot set 'awaitData' without also setting 'tailable'",
        ));
    }
    if tailable_asked {
        // A tailable cursor follows storage order and never ends, so it takes
        // no other sort and no single batch.
        let natural_forward = match doc.get("sort") {
            None | Some(Bson::Null) => true,
            Some(Bson::Document(sort)) => {
                sort.is_empty()
                    || (sort.len() == 1 && sort.get("$natural").and_then(as_i64) == Some(1))
            }
            _ => true,
        };
        if !natural_forward {
            return Err(CommandError::new(
                2,
                "BadValue",
                "cannot use tailable option with a sort other than {$natural: 1}",
            ));
        }
        if bool_field(doc, "singleBatch", false) {
            return Err(CommandError::new(
                2,
                "BadValue",
                "cannot use tailable option with the 'singleBatch' option",
            ));
        }
    }
    argtypes::require_let_names(doc)?;
    argtypes::require_object(doc, "readConcern", "FindCommandRequest.readConcern")?;
    argtypes::require_hint(doc, "hint")?;
    // An undefined `$$variable` is a PARSE error (17276), not the storage
    // layer's generic "unsupported construct" BadValue.
    {
        let bound: Vec<String> = match doc.get("let") {
            Some(Bson::Document(d)) => d.keys().cloned().collect(),
            _ => Vec::new(),
        };
        let vars = crate::util::command_let_vars(doc.get("let"), ctx);
        if let Some(Bson::Document(f)) = doc.get("filter") {
            if let Some((code, msg)) = argtypes::expression_problem_in_filter_with(f, &bound, &vars)
            {
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    msg,
                ));
            }
        }
    }

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
        if matches!(doc.get("tailable"), Some(Bson::Boolean(true))) {
            return Ok(CommandError::new(
                168,
                "InvalidPipelineOperator",
                "Tailable cursors are not supported in aggregation.",
            )
            .into_reply());
        }
        let agg = build_view_find_aggregate(doc, &coll, natural_hint.as_ref());
        return crate::aggregate::aggregate(&agg, ctx);
    }
    let storage = ctx.storage()?;
    ctx.cursors()?;
    // mongod answers a `find` on a time-series collection with an
    // aggregation over its buckets, which takes none of these three.
    if storage
        .get_collection_options(&ctx.db_name, &coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        let refused = if matches!(doc.get("tailable"), Some(Bson::Boolean(true))) {
            Some("Tailable cursors are not supported in aggregation.".to_string())
        } else {
            ["min", "max"]
                .into_iter()
                .find(|f| matches!(doc.get(*f), Some(Bson::Document(d)) if !d.is_empty()))
                .map(|f| format!("Option {f} not supported in aggregation."))
        };
        if let Some(message) = refused {
            return Ok(CommandError::new(168, "InvalidPipelineOperator", message).into_reply());
        }
        // A hint naming no index is refused over the buckets
        // (`timeseries::check_hint`; the buckets are clustered, so
        // `{_id: 1}` names none).
        if let Err(e) = crate::timeseries::check_hint(
            storage,
            &ctx.db_name,
            &coll,
            &doc_field(doc, "filter"),
            argtypes::given_hint(doc),
        ) {
            return Ok(e.into_reply());
        }
    }

    let filter = doc_field(doc, "filter");
    // Each `$expr` is parsed as mongod parses it: a malformed expression is
    // refused before any document is read.
    if let Err(f) = crate::aggregate::filter_expr_parse(&filter) {
        return Ok(crate::aggregate::parse_error(None, &f).into_reply());
    }
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
    // `$text` needs a text index, which this server cannot search: mongod's
    // 27 (measured 8.2.11).
    if let Some((code, errmsg)) =
        crate::crud::text_query_problem(ctx.storage()?, &ctx.db_name, &coll, &filter)
    {
        return Ok(
            CommandError::new(code, crate::util::error_code_name(code), errmsg).into_reply(),
        );
    }
    let skip = doc.get("skip").and_then(as_i64).unwrap_or(0).max(0) as usize;
    let limit = doc.get("limit").and_then(as_i64).unwrap_or(0).max(0) as usize; // 0 ⇒ no limit
                                                                                // Validated above; normalised so a decimal or fractional direction reaches
                                                                                // storage as the 1 / -1 mongod reads it as (a decimal -1 sorted ascending).
    let sort_owned = doc
        .get("sort")
        .and_then(Bson::as_document)
        .filter(|_| natural_hint.is_none())
        .map(argtypes::normalise_sort_spec);
    let sort = sort_owned.as_ref();
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
        // mongod's projection parser, in its order: names, collisions,
        // inclusion against exclusion, and every expression inside.
        if let Err(f) = secantus_core::expr_parse::projection_spec(
            spec,
            secantus_core::expr_parse::ProjectionKind::Find,
        ) {
            return Ok(crate::aggregate::parse_error(None, &f).into_reply());
        }
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
        if let Some(e) = projection_expression_error(spec, doc.get("let"), ctx) {
            return Err(e);
        }
        // Projection validation is parse-time in mongod, so an invalid spec
        // errors even when nothing matches.
        //
        // A NAMED error is passed through: the path-collision check answers
        // mongod's own 31249 / 31250, and rewriting it to a generic BadValue
        // threw that away. Only a `Defer` -- the positional cases whose exact
        // Location code (31276 / 31395 / 51246) this engine cannot reproduce --
        // still falls back to the generic message.
        let q = if filter.is_empty() {
            None
        } else {
            Some(&filter)
        };
        if let Err(err) = secantus_core::projection::validate_projection(spec, q) {
            let reply = match err {
                secantus_core::fallback::Fallback::Mongo { code, message, .. } => {
                    CommandError::new(code, crate::util::error_code_name(code), message)
                }
                _ => CommandError::new(2, "BadValue", "invalid positional projection"),
            };
            return Ok(reply.into_reply());
        }
    }
    let hint = natural_hint.as_ref().or_else(|| argtypes::given_hint(doc));
    argtypes::require_natural_hint(hint)?;
    let collation = crate::util::effective_collation(storage, &ctx.db_name, &coll, doc)?;
    let _active = secantus_core::collation::activate(collation.as_ref());
    // Command `let` → vars visible to `$expr` in the filter.
    let let_vars = crate::util::checked_let_vars(doc.get("let"))?;
    // `batchSize` is tri-state: absent ⇒ default, 0 ⇒ empty firstBatch + cursor,
    // explicit positive ⇒ that size. A present-but-non-numeric value (e.g. a
    // string) is a TypeMismatch — mongod rejects it, and the mongo-c-driver
    // find/batchSize test sends `{batchSize: 'foo'}` expecting a server error.
    let batch_size = match doc.get("batchSize") {
        // An explicit null is ACCEPTED and means "absent" (probed): `as_i64`
        // returns None for it, which used to fall into the type error below.
        Some(Bson::Null) | None => DEFAULT_BATCH_SIZE as i64,
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
    };
    let single_batch = bool_field(doc, "singleBatch", false);
    let tailable = bool_field(doc, "tailable", false);
    let await_data = bool_field(doc, "awaitData", false);
    let ns = format!("{}.{}", ctx.db_name, coll);

    // A tailable cursor is only valid on a capped collection (mongod rejects it
    // on a non-capped one with BadValue). Check before the fetch.
    // `local.oplog.rs` is a view of the oplog table with no entry of its own
    // in the catalog; it is there all the same.
    let is_oplog = ctx.db_name == "local" && coll == "oplog.rs";
    let exists = is_oplog
        || storage
            .collection_exists(&ctx.db_name, &coll)
            .unwrap_or(false);
    if tailable && !exists {
        // Nothing to tail: an empty batch and no cursor, not an error.
        return Ok(doc! {
            "cursor": { "firstBatch": [], "id": Bson::Int64(0), "ns": ns },
            "ok": 1.0,
        });
    }
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

    let read = crate::notablescan::Read {
        coll: &coll,
        filter: &filter,
        sort,
        hint,
        collation: collation.as_ref(),
        shown: crate::notablescan::Shown {
            projection: doc.get_document("projection").ok(),
            skip: doc.get("skip").and_then(as_i64),
            limit: doc.get("limit").and_then(as_i64),
            collation: doc.get_document("collation").ok().cloned(),
        },
    };
    read.check_hint(storage, &ctx.db_name)?;
    crate::notablescan::check(ctx, &read)?;

    // A read that streams: its cursor reads on as `getMore` asks, from where
    // the last batch stopped (RM-080). The rest read whole, as before.
    let streamable = !tailable
        && exists
        && collation.is_none()
        && !doc.contains_key("min")
        && !doc.contains_key("max")
        && !bool_field(doc, "returnKey", false)
        && !bool_field(doc, "showRecordId", false)
        && !doc.get_document("projection").is_ok_and(|p| {
            p.values()
                .any(|v| secantus_core::projection::meta_spec(v).is_some())
        })
        && (natural_hint.is_some()
            || !matches!(doc.get("sort"), Some(Bson::Document(given))
                if !given.is_empty() && !crate::explain::order::plain_sort(given)));
    if streamable {
        let asked = StreamAsk {
            coll: &coll,
            filter: &filter,
            sort,
            hint,
            skip,
            limit,
            batch_size,
            single_batch,
            let_vars: &let_vars,
            ns: &ns,
        };
        if let Some(reply) = stream_find(ctx, doc, &asked)? {
            return Ok(reply);
        }
    }
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;

    // `min` and `max` cut the hinted index's scan at a key and fix its
    // direction: that read keeps the order the store gives it. So does a
    // sort by a value that is not 1 or -1 (`-1.5`): mongod sorts by it and
    // does not read it from an index, and by here it has been made -1.
    let bounded = doc.contains_key("min") || doc.contains_key("max");
    let inexact = matches!(doc.get("sort"), Some(Bson::Document(given))
        if !given.is_empty() && !crate::explain::order::plain_sort(given));
    let docs = if bounded || inexact {
        storage.find_collated(
            &ctx.db_name,
            &coll,
            &filter,
            sort,
            hint,
            collation.as_ref(),
            &let_vars,
        )
    } else {
        crate::explain::order::find(
            storage,
            &crate::explain::order::Ask {
                db: &ctx.db_name,
                coll: &coll,
                filter: &filter,
                sort,
                hint,
                collation: collation.as_ref(),
                vars: &let_vars,
                limit: if limit > 0 { (skip + limit) as i64 } else { 0 },
                asked: crate::near::Asked {
                    skip: skip as i64,
                    limit: limit as i64,
                    sort,
                    projection: doc
                        .get_document("projection")
                        .ok()
                        .filter(|p| !p.is_empty()),
                },
            },
        )
    }
    .map_err(|e| crate::util::read_exec_error(e, "find", &format!("{}.{}", ctx.db_name, coll)))?;

    // A time-series collection scans bucket by bucket, and returns each
    // measurement with its time and meta fields first.
    let mut docs = crate::ts_buckets::as_read(storage, &ctx.db_name, &coll, docs, sort.is_some())?;

    // A scan that matched nothing never ran the filter: check it now.
    if docs.is_empty() {
        validate_empty_result(&filter, &let_vars, collation.as_ref())?;
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
        // An empty capped collection gives a tailable cursor no position to
        // hold: mongod answers with a dead cursor (measured 8.2.11).
        if after.is_none() && docs.is_empty() && !is_oplog {
            return Ok(doc! {
                "cursor": { "firstBatch": [], "id": Bson::Int64(0), "ns": ns },
                "ok": 1.0,
            });
        }
        // The projection applies to every batch, this one and the producer's.
        if let Some(spec) = projection {
            docs = crate::util::encode_docs(project_to_docs(docs, spec, &filter, &let_vars, &ns)?)?;
        }
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
            filter: filter.clone(),
            projection: projection.cloned(),
            let_vars: let_vars.clone(),
            collation: collation.clone(),
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
    // `{$meta: "recordId"}` / `{$meta: "indexKey"}` in the projection. A
    // top-level one is added after the projected fields, where mongod puts
    // it; one inside an expression is read from a field the documents are
    // given for the projection (`agg_meta`). A RecordId the store cannot
    // give, or an index key of a plan that reads no single index scan, is
    // missing, as mongod's is (measured 8.2.11).
    let mut nested_spec: Option<Document> = None;
    let mut metas: Vec<(String, &str)> = Vec::new();
    if let (Some(spec), false) = (projection, return_key) {
        let mut rewritten = Document::new();
        let mut nested = false;
        for (k, v) in spec {
            match secantus_core::projection::meta_spec(v) {
                Some(m @ ("recordId" | "indexKey")) => metas.push((k.clone(), m)),
                Some(_) => {}
                None => {
                    let mut v = v.clone();
                    nested |=
                        crate::agg_meta::replace(&mut v, "recordId", crate::agg_meta::RECORD_ID);
                    nested |=
                        crate::agg_meta::replace(&mut v, "indexKey", crate::agg_meta::INDEX_KEY);
                    rewritten.insert(k.clone(), v);
                    continue;
                }
            }
            rewritten.insert(k.clone(), v.clone());
        }
        if nested {
            nested_spec = Some(rewritten);
        }
    }
    let uses = |kind: &str, field: &str| {
        metas.iter().any(|(_, m)| *m == kind)
            || nested_spec
                .as_ref()
                .is_some_and(|s| s.values().any(|v| crate::agg_meta::reads(v, field)))
    };
    let wants_record_id =
        (show_record_id && !return_key) || uses("recordId", crate::agg_meta::RECORD_ID);
    let wants_index_key = uses("indexKey", crate::agg_meta::INDEX_KEY);
    // Each document's RecordId, as the store numbers its records (per
    // collection from 1, as mongod does); `None` when the store cannot say.
    let record_ids: Option<Vec<Option<i64>>> = if wants_record_id {
        let refs: Vec<&[u8]> = docs.iter().map(Vec::as_slice).collect();
        storage
            .record_ids_of(&ctx.db_name, &coll, &refs)
            .map_err(command_error)?
    } else {
        None
    };
    if let (Some(spec), true) = (projection, !metas.is_empty() || nested_spec.is_some()) {
        let mut originals: Vec<Document> = docs
            .iter()
            .map(|b| secantus_core::bsondoc::from_bytes(b.as_slice()))
            .collect::<Result<_, _>>()
            .map_err(|e| CommandError::new(1, "InternalError", e.to_string()))?;
        let index_keys = if wants_index_key {
            crate::explain::order::index_keys(
                storage,
                &crate::explain::order::Ask {
                    db: &ctx.db_name,
                    coll: &coll,
                    filter: &filter,
                    sort,
                    hint,
                    collation: collation.as_ref(),
                    vars: &let_vars,
                    limit: 0,
                    asked: Default::default(),
                },
                &originals,
            )
        } else {
            None
        };
        let mut projected = match &nested_spec {
            Some(rewritten) => {
                crate::agg_meta::attach(
                    &mut originals,
                    record_ids.as_deref(),
                    index_keys.as_deref(),
                );
                let mut out = project_to_docs(
                    crate::util::encode_docs(originals)?,
                    rewritten,
                    &filter,
                    &let_vars,
                    &ns,
                )?;
                for d in out.iter_mut() {
                    d.remove(crate::agg_meta::RECORD_ID);
                    d.remove(crate::agg_meta::INDEX_KEY);
                }
                out
            }
            None => project_to_docs(docs, spec, &filter, &let_vars, &ns)?,
        };
        for (i, out) in projected.iter_mut().enumerate() {
            for (field, meta) in &metas {
                let value = match *meta {
                    "recordId" => record_ids.as_ref().and_then(|r| r[i]).map(Bson::Int64),
                    _ => index_keys.as_ref().map(|k| Bson::Document(k[i].clone())),
                };
                if let Some(v) = value {
                    out.insert(field.clone(), v);
                }
            }
        }
        docs = crate::util::encode_docs(projected)?;
        projection = None;
    }
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
        // `showRecordId` adds to the projected document; only `returnKey`
        // replaces it.
        let projected_first = match projection {
            Some(spec) if !return_key => Some(project_to_docs(
                docs.clone(),
                spec,
                &filter,
                &let_vars,
                &ns,
            )?),
            _ => None,
        };
        let mut rewritten: Vec<Document> = Vec::with_capacity(docs.len());
        for (i, bytes) in docs.iter().enumerate() {
            let d = match &projected_first {
                Some(projected) => projected[i].clone(),
                None => secantus_core::bsondoc::from_bytes(bytes.as_slice())
                    .map_err(|e| CommandError::new(1, "InternalError", e.to_string()))?,
            };
            if return_key {
                let mut o = Document::new();
                for f in &key_fields {
                    if let Some(v) = d.get(f) {
                        o.insert(f.clone(), v.clone());
                    }
                }
                rewritten.push(o);
            } else {
                // The document's RecordId (mongod 8.2.11: after the first of
                // four is deleted, 2, 3, 4 -- not its place in the reply).
                let mut o = d;
                let rid = match &record_ids {
                    Some(ids) => ids[i],
                    None => Some((i + 1) as i64),
                };
                if let Some(rid) = rid {
                    o.insert("$recordId", Bson::Int64(rid));
                }
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
            let projected = project_to_docs(docs, spec, &filter, &let_vars, &ns)?;
            let (first_batch, cursor_id): (Vec<Bson>, i64) = if single_batch {
                let mut projected = projected;
                projected.truncate(batch_size.max(0) as usize);
                (projected.into_iter().map(Bson::Document).collect(), 0)
            } else if batch_size == 0 && exists {
                let rest = crate::util::encode_docs(projected)?;
                (Vec::new(), open_cursor(cursors, &ns, rest, limit > 0)?)
            } else {
                split_docs_into_cursor(projected, batch_size, &ns, cursors, limit > 0)?
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
                docs.truncate(batch_size.max(0) as usize);
                (docs, 0)
            } else if batch_size == 0 && exists {
                // `batchSize: 0` asks for the cursor and no documents, and
                // mongod opens one even when nothing matches.
                (Vec::new(), open_cursor(cursors, &ns, docs, limit > 0)?)
            } else {
                // A positive `limit` bounds the result, so mongod closes the
                // cursor the moment it is reached rather than spending a
                // trailing empty getMore.
                split_into_cursor(docs, batch_size, &ns, cursors, limit > 0)?
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

/// The filter's errors a read that matched nothing never met: mongod parses
/// the whole filter, so an invalid one fails over an empty result too. The
/// matcher run once against an empty document (operator recognition does not
/// depend on the document) surfaces it -- except an error the evaluator
/// raises per document, which the empty document provokes and no stored one
/// did; a constant one is mongod's optimizer failure (measured 8.2.11,
/// 2026-10-16).
fn validate_empty_result(
    filter: &Document,
    let_vars: &Document,
    collation: Option<&secantus_core::collation::Collation>,
) -> Result<(), CommandError> {
    if filter.is_empty() {
        return Ok(());
    }
    if let Err(fault) = secantus_core::query::matches(&Document::new(), filter, let_vars, collation)
    {
        match (fault.folded_verdict(), fault.as_mongo()) {
            (Some(false), _) => {}
            (Some(true), Some((code, errmsg))) => {
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    format!("Failed to optimize expression :: caused by :: {errmsg}"),
                ))
            }
            _ => return Err(query_filter_error(filter)),
        }
    }
    Ok(())
}

/// What a streamed `find` reads.
struct StreamAsk<'a> {
    coll: &'a str,
    filter: &'a Document,
    sort: Option<&'a Document>,
    hint: Option<&'a Bson>,
    skip: usize,
    limit: usize,
    batch_size: i64,
    single_batch: bool,
    let_vars: &'a Document,
    ns: &'a str,
}

/// A `find` cursor that reads on as `getMore` asks: the plan's read
/// (`explain::stream`), each batch projected as it is read.
struct FindStream {
    stream: crate::explain::stream::Stream,
    projection: Option<Document>,
    filter: Document,
    vars: Document,
    ns: String,
    summary: Option<String>,
    command: Document,
}

impl crate::cursors::StreamCursor for FindStream {
    fn next_batch(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        max_bytes: usize,
    ) -> Result<crate::cursors::StreamedBatch, CommandError> {
        let batch = self
            .stream
            .next_batch(storage, want, max_bytes)
            .map_err(|e| crate::util::read_exec_error(e, "find", &self.ns))?;
        let docs = match &self.projection {
            Some(spec) => crate::util::encode_docs(project_to_docs(
                batch.docs,
                spec,
                &self.filter,
                &self.vars,
                &self.ns,
            )?)?,
            None => batch.docs,
        };
        Ok(crate::cursors::StreamedBatch {
            docs,
            exhausted: batch.exhausted,
            keys_examined: batch.keys_examined,
            docs_examined: batch.docs_examined,
        })
    }

    fn plan_summary(&self) -> Option<String> {
        self.summary.clone()
    }

    fn originating_command(&self) -> Document {
        self.command.clone()
    }
}

/// A `find` read through a streaming cursor: its first batch now, the rest
/// as `getMore` asks. `None` when its plan does not stream.
fn stream_find(
    ctx: &mut CommandContext,
    doc: &Document,
    asked: &StreamAsk,
) -> Result<Option<Document>, CommandError> {
    use crate::cursors::StreamCursor;
    let storage = ctx
        .storage
        .clone()
        .ok_or_else(|| CommandError::new(1, "InternalError", "no storage configured"))?;
    let projection = doc
        .get_document("projection")
        .ok()
        .filter(|p| !p.is_empty())
        .cloned();
    let (skip, limit) = (asked.skip as i64, asked.limit as i64);
    let ask = crate::explain::order::Ask {
        db: &ctx.db_name,
        coll: asked.coll,
        filter: asked.filter,
        sort: asked.sort,
        hint: asked.hint,
        collation: None,
        vars: asked.let_vars,
        limit: if limit > 0 { skip + limit } else { 0 },
        asked: crate::near::Asked {
            skip,
            limit,
            sort: asked.sort,
            projection: projection.as_ref(),
        },
    };
    let read_error = |e| crate::util::read_exec_error(e, "find", asked.ns);
    let Some(plan) = crate::explain::order::stream_plan(
        storage.as_ref(),
        &ask,
        skip,
        limit,
        projection.as_ref(),
        doc.contains_key("batchSize"),
    )
    .map_err(read_error)?
    else {
        return Ok(None);
    };
    let Some(stream) = crate::explain::stream::Stream::of_plan(
        storage.as_ref(),
        &plan,
        &ctx.db_name,
        asked.coll,
        asked.filter,
        asked.let_vars,
    )
    .map_err(read_error)?
    else {
        return Ok(None);
    };
    let mut cursor = FindStream {
        stream,
        projection,
        filter: asked.filter.clone(),
        vars: asked.let_vars.clone(),
        ns: asked.ns.to_string(),
        summary: crate::explain::summary::plan_summary(doc, ctx),
        command: doc.clone(),
    };
    // `batchSize: 0` asks for the cursor and no documents.
    let batch = if asked.batch_size == 0 {
        crate::cursors::StreamedBatch {
            docs: Vec::new(),
            exhausted: false,
            keys_examined: 0,
            docs_examined: 0,
        }
    } else {
        cursor.next_batch(
            storage.as_ref(),
            asked.batch_size.max(0) as usize,
            MAX_GETMORE_BATCH_BYTES,
        )?
    };
    if batch.docs.is_empty() && batch.exhausted {
        validate_empty_result(asked.filter, asked.let_vars, None)?;
    }
    let cursor_id = if batch.exhausted || asked.single_batch {
        0
    } else {
        ctx.cursors()?
            .register_stream(asked.ns, Box::new(cursor))
            .map_err(|e| CommandError::new(1, "InternalError", format!("cursor registry: {e:?}")))?
    };
    ctx.exec_stats = Some(crate::ExecStats {
        keys_examined: batch.keys_examined,
        docs_examined: batch.docs_examined,
        cursor_id,
        exhausted: batch.exhausted,
        plan_summary: None,
        originating_command: None,
    });
    ctx.pending_batch = Some(crate::PendingBatch {
        batch_field: "firstBatch",
        batch: batch.docs,
    });
    Ok(Some(doc! {
        "cursor": {
            "id": Bson::Int64(cursor_id),
            "ns": asked.ns,
        },
        "ok": 1.0,
    }))
}

/// Register a cursor over `remaining`, whatever it holds.
pub(crate) fn open_cursor(
    cursors: &CursorRegistry,
    ns: &str,
    remaining: Vec<Vec<u8>>,
    bounded: bool,
) -> Result<i64, CommandError> {
    cursors
        .register_bounded(ns, remaining, bounded)
        .map_err(|e| CommandError::new(1, "InternalError", format!("cursor registry: {e:?}")))
}

/// The fields mongod's `find` takes (8.2.11); any other is 40415.
const FIND_FIELDS: &[&str] = &[
    "find",
    "filter",
    "sort",
    "projection",
    "hint",
    "skip",
    "limit",
    "batchSize",
    "singleBatch",
    "max",
    "min",
    "returnKey",
    "showRecordId",
    "tailable",
    "oplogReplay",
    "noCursorTimeout",
    "awaitData",
    "allowPartialResults",
    "collation",
    "allowDiskUse",
    "let",
    "term",
    "readOnce",
    "maxTimeMSOpOnly",
    "includeQueryStatsMetrics",
    "querySettings",
    "mirrored",
    "sampleId",
    "rawData",
];

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
    bounded: bool,
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
    // A batch that exactly fills the requested size proves nothing about what
    // follows, so an unbounded cursor stays open even with nothing left -- the
    // client spends one more getMore to see the empty batch, exactly as against
    // mongod. Closing early made our round-trip count differ, which drivers
    // observe directly.
    let filled_exactly = batch_size > 0 && docs.len() == batch_size as usize;
    if remaining.is_empty() && (bounded || !filled_exactly) {
        return Ok((docs, 0));
    }
    let cursor_id = cursors
        .register_bounded(ns, remaining, bounded)
        .map_err(|e| CommandError::new(1, "InternalError", format!("cursor registry: {e:?}")))?;
    Ok((docs, cursor_id))
}

/// Like [`split_into_cursor`], but for already-decoded result `Document`s (the
/// `aggregate` pipeline output, or a projected `find`). The `firstBatch` is
/// returned as `Bson` for direct embedding in the reply — never re-encoded —
/// while only the cursor remainder is encoded to bytes for the registry. Shared
/// with the `aggregate` handler.
pub(crate) fn split_docs_into_cursor(
    docs: Vec<Document>,
    batch_size: i64,
    ns: &str,
    cursors: &CursorRegistry,
    bounded: bool,
) -> Result<(Vec<Bson>, i64), CommandError> {
    split_docs_into_cursor_checked(docs, batch_size, ns, cursors, bounded, None)
}

/// mongod's internal BSON ceiling (16MB + 16KB): a result document past it
/// cannot be serialised into a reply at all.
const BSON_OBJ_MAX_INTERNAL_SIZE: usize = 16 * 1024 * 1024 + 16 * 1024;

/// [`split_docs_into_cursor`], byte-budgeted like [`split_into_cursor`] and,
/// when `oversize_prefix` is given, refusing a document too large to send.
///
/// Both halves were missing for decoded results. Without the budget a first
/// batch of large projected / aggregated documents went out as one reply of
/// any size; without the refusal a single result over 16MB -- `{$range: [0,
/// 6553590]}` is 84MB -- was sent as an 84MB MESSAGE, past the 48MB wire
/// maximum, and the driver dropped the connection with a ProtocolError.
/// mongod answers 10334 `Serializing Document failed` under its executor
/// prefix (measured 8.2.11, 2026-09-30). Every result is encoded once here: the
/// size decides both, and the cursor remainder needs the bytes anyway.
pub(crate) fn split_docs_into_cursor_checked(
    mut docs: Vec<Document>,
    batch_size: i64,
    ns: &str,
    cursors: &CursorRegistry,
    bounded: bool,
    oversize_prefix: Option<&str>,
) -> Result<(Vec<Bson>, i64), CommandError> {
    let take = if batch_size < 0 {
        DEFAULT_BATCH_SIZE as usize
    } else {
        batch_size as usize
    }
    .min(docs.len());
    let mut encoded = encode_docs_ref(&docs)?;
    if let Some(prefix) = oversize_prefix {
        if let Some(big) = encoded
            .iter()
            .find(|b| b.len() > BSON_OBJ_MAX_INTERNAL_SIZE)
        {
            return Err(CommandError::new(
                10334,
                "BSONObjectTooLarge",
                format!(
                    "{prefix} :: caused by :: Serializing Document failed :: caused by :: \
                     Size {} exceeds maximum {BSON_OBJ_MAX_INTERNAL_SIZE}",
                    big.len()
                ),
            ));
        }
    }
    // Same rule as the blob path: stop before the batch passes 16MB, but always
    // take one document so an oversized one still makes progress.
    let mut bytes = 0usize;
    let mut fitted = 0usize;
    for blob in encoded.iter().take(take) {
        if fitted > 0 && bytes + blob.len() > MAX_GETMORE_BATCH_BYTES {
            break;
        }
        bytes += blob.len();
        fitted += 1;
    }
    docs.truncate(fitted);
    let remaining = encoded.split_off(fitted);
    let filled_exactly = batch_size > 0 && docs.len() == batch_size as usize;
    let first: Vec<Bson> = docs.into_iter().map(Bson::Document).collect();
    if remaining.is_empty() && (bounded || !filled_exactly) {
        return Ok((first, 0));
    }
    let cursor_id = cursors
        .register_bounded(ns, remaining, bounded)
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

/// The index keys `doc` has under the leading `fields` of an index: one tuple
/// per combination of array elements, with a missing field as null. This is
/// what `min` / `max` bound -- index KEYS, not documents -- so a document whose
/// array holds one value inside the bounds is returned, and so is one that
/// lacks the field when null is inside them.
fn index_key_tuples(doc: &Document, fields: &[&String]) -> Vec<Vec<Bson>> {
    let mut tuples: Vec<Vec<Bson>> = vec![Vec::new()];
    for field in fields {
        let (found, _) = secantus_core::get_path_values(doc, field);
        let mut values: Vec<Bson> = Vec::new();
        for v in found {
            match v {
                Bson::Array(items) if !items.is_empty() => values.extend(items.iter().cloned()),
                Bson::Array(_) => values.push(Bson::Undefined),
                other => values.push(other.clone()),
            }
        }
        if values.is_empty() {
            values.push(Bson::Null);
        }
        tuples = tuples
            .into_iter()
            .flat_map(|t| {
                values.iter().map(move |v| {
                    let mut next = t.clone();
                    next.push(v.clone());
                    next
                })
            })
            .collect();
    }
    tuples
}

/// Compare a key tuple with a bound, column by column in the index's
/// directions.
fn tuple_cmp(
    key: &[Bson],
    bound: &Document,
    key_spec: &Document,
) -> Result<std::cmp::Ordering, CommandError> {
    use std::cmp::Ordering;
    for (value, (field, bval)) in key.iter().zip(bound) {
        let descending = key_spec.get(field).and_then(as_i64).is_some_and(|d| d < 0);
        // ASCENDING encodings compared and then negated, never the INVERTED
        // bytes: inversion does not reverse a PREFIX relationship, so `""`
        // (whose key is a strict prefix of every string's) compared as the
        // LARGEST value on a descending column. See `storage::sort_key`.
        let enc = |v: &Bson| {
            secantus_core::sortkey::encode_value(v, None)
                .map_err(|_| CommandError::new(2, "BadValue", "min/max value not comparable"))
        };
        let ord = enc(value)?.cmp(&enc(bval)?);
        match if descending { ord.reverse() } else { ord } {
            Ordering::Equal => continue,
            other => return Ok(other),
        }
    }
    Ok(Ordering::Equal)
}

/// A `min` / `max` refusal, under the prefix mongod's planner gives it.
fn min_max_error(code: i32, ns: &str, reason: &str) -> CommandError {
    CommandError::new(
        code,
        crate::util::error_code_name(code),
        format!("error processing query: ns={ns} planner returned error :: caused by :: {reason}"),
    )
}

/// Filter `docs` (already index-ordered) by the `min` (inclusive) / `max`
/// (exclusive) bounds on the hinted index's keys.
#[allow(clippy::too_many_arguments)]
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
    let ns = format!("{db}.{coll}");
    let hint = hint.ok_or_else(|| {
        min_max_error(
            291,
            &ns,
            "When using min()/max() a hint of which index to use must be provided",
        )
    })?;
    if matches!(hint, Bson::Document(h) if h.contains_key("$natural")) {
        return Err(min_max_error(
            291,
            &ns,
            "min and max are incompatible with $natural",
        ));
    }
    let key_spec = resolve_index_key(storage, db, coll, hint)?;
    // Each bound's fields must be a leading prefix of the hinted index's key,
    // in the same order.
    let index_fields: Vec<&String> = key_spec.keys().collect();
    for bound in [min, max].into_iter().flatten() {
        let bf: Vec<&String> = bound.keys().collect();
        let prefix_match =
            bf.len() <= index_fields.len() && (0..bf.len()).all(|i| bf[i] == index_fields[i]);
        if !prefix_match {
            return Err(min_max_error(
                51174,
                &ns,
                "The index chosen is not compatible with min/max",
            ));
        }
    }
    if let (Some(lo), Some(hi)) = (min, max) {
        let low: Vec<Bson> = lo.values().cloned().collect();
        if lo.len() == hi.len() && tuple_cmp(&low, hi, &key_spec)? != Ordering::Less {
            return Err(min_max_error(
                51175,
                &ns,
                "The value provided for min() does not come before the value provided for \
                 max() in the hinted index",
            ));
        }
    }
    let width = min
        .map_or(0, Document::len)
        .max(max.map_or(0, Document::len));
    let fields = &index_fields[..width.min(index_fields.len())];
    let mut out = Vec::with_capacity(docs.len());
    for bytes in docs {
        let d = secantus_core::bsondoc::from_bytes(bytes.as_slice()).map_err(|e| {
            CommandError::new(
                1,
                "InternalError",
                format!("failed to decode document: {e}"),
            )
        })?;
        // min is inclusive, max exclusive; one key inside both keeps the
        // document.
        let mut keep = false;
        for key in index_key_tuples(&d, fields) {
            let above = match min {
                Some(m) => tuple_cmp(&key, m, &key_spec)? != Ordering::Less,
                None => true,
            };
            let below = match max {
                Some(m) => tuple_cmp(&key, m, &key_spec)? == Ordering::Less,
                None => true,
            };
            if above && below {
                keep = true;
                break;
            }
        }
        if keep {
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
/// What mongod refuses about a projection's field names and operators when it
/// parses it: a field that starts with `$`, and an operator it does not know.
/// Measured on `findAndModify`'s `fields`, 8.2.11, 2026-10-12.
pub(crate) fn projection_name_error(spec: &Document) -> Option<CommandError> {
    for (field, value) in spec {
        if field.starts_with('$') {
            return Some(CommandError::new(
                16410,
                "Location16410",
                format!(
                    "Consider using $getField or $setField for a field path with '.' or '$'. \
                     :: caused by :: FieldPath field names may not start with '$', given \
                     '{field}'."
                ),
            ));
        }
        if let Bson::Document(d) = value {
            if let Some(op) = d.keys().next().filter(|k| k.starts_with('$')) {
                let known = matches!(op.as_str(), "$slice" | "$elemMatch" | "$meta")
                    || secantus_core::expressions::KNOWN_EXPR_OPS.contains(&op.as_str());
                if !known {
                    return Some(CommandError::new(
                        31325,
                        "Location31325",
                        format!("Unknown expression {op}"),
                    ));
                }
            }
        }
    }
    None
}

pub(crate) fn projection_mix_error(spec: &Document) -> Option<CommandError> {
    let field_inclusion = |v: &Bson| -> Option<bool> {
        match v {
            Bson::Int32(0) | Bson::Int64(0) | Bson::Boolean(false) => Some(false),
            Bson::Int32(_) | Bson::Int64(_) | Bson::Boolean(true) => Some(true),
            Bson::Double(d) => Some(*d != 0.0),
            // A COMPUTED field (`"$a"`, `{$add: [...]}`, a literal constant)
            // forces INCLUSION mode, so a companion `b: 0` is the 31254 mix.
            // Measured against mongod 8.2.11 (2026-09-07):
            //   {x: {$add: ["$a", 1]}, b: 0}
            //     -> Cannot do exclusion on field b in inclusion projection
            // The three projection operators stay neutral.
            _ if secantus_core::projection::is_computed_spec(v) => Some(true),
            _ => None, // operator spec ($slice / $elemMatch / $meta) — neutral
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
                format!("Unsupported $meta field: {arg}"),
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

/// What mongod's projection parser refuses inside a computed field, for
/// `find` and `findAndModify`: an undefined `$$variable` (17276), then a
/// constant sub-expression that fails, folded with the command's `let`
/// substituted -- bare, over an empty collection too (mongod 8.2.11,
/// 2026-10-22 and 2026-10-24; RM-251). find's own operators are not
/// expressions; a `$slice` that is not find's `[skip, limit]` is one.
pub(crate) fn projection_expression_error(
    spec: &Document,
    let_field: Option<&Bson>,
    ctx: &CommandContext,
) -> Option<CommandError> {
    let is_find_operator = |v: &&Bson| match v {
        Bson::Document(d) if d.len() == 1 => match d.iter().next() {
            Some((k, arg)) if k == "$slice" => {
                secantus_core::projection::find_slice_problem(arg).is_none()
            }
            Some((k, _)) => matches!(k.as_str(), "$elemMatch" | "$meta"),
            None => false,
        },
        _ => false,
    };
    let bound: Vec<String> = match let_field {
        Some(Bson::Document(d)) => d.keys().cloned().collect(),
        _ => Vec::new(),
    };
    if let Some((code, msg)) = spec
        .values()
        .filter(|v| !is_find_operator(v))
        .find_map(|v| argtypes::expression_problem(v, &bound))
    {
        return Some(CommandError::new(
            code,
            crate::util::error_code_name(code),
            msg,
        ));
    }
    let fold_vars = crate::util::command_let_vars(let_field, ctx);
    let fault = spec
        .values()
        .filter(|v| !is_find_operator(v))
        .filter_map(|v| secantus_core::expr_rewrite::constant_expr_fault_with(v, &fold_vars))
        .find(|f| f.folded_verdict() != Some(false))?;
    let (code, msg) = fault.as_mongo()?;
    Some(CommandError::new(
        code,
        crate::util::error_code_name(code),
        msg.to_string(),
    ))
}

/// Apply a projection spec to each result doc via `secantus_core::projection`.
/// A `Fallback` (a projection the Rust engine can't reproduce exactly) surfaces
/// as `BadValue` — the Rust server only ships what the Rust engine supports.
fn project_to_docs(
    docs: Vec<Vec<u8>>,
    spec: &Document,
    filter: &Document,
    vars: &Document,
    ns: &str,
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
            let d = secantus_core::bsondoc::from_bytes(bytes.as_slice()).map_err(|e| {
                CommandError::new(
                    1,
                    "InternalError",
                    format!("failed to decode document: {e}"),
                )
            })?;
            secantus_core::projection::apply_projection_with(&d, spec, query, vars).map_err(|f| {
                // A Fallback carrying a mongod code is a real server error and
                // must be surfaced with it. The spec was parsed before any
                // document was read, so what fails here fails on a document:
                // mongod's executor error (measured 8.2.11, 2026-10-24).
                // Only a bare Defer, which has no Python behind it here,
                // becomes the generic refusal.
                match f.as_mongo() {
                    Some((code, msg)) => CommandError::new(
                        code,
                        crate::util::error_code_name(code),
                        format!("Executor error during find command: {ns} :: caused by :: {msg}"),
                    ),
                    None => CommandError::new(
                        2,
                        "BadValue",
                        "projection is not supported by the Rust server",
                    ),
                }
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
        let (first, cursor_id) = split_into_cursor(docs, 25, "t.big", &reg, false).unwrap();

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
        let (first, cursor_id) = split_into_cursor(docs, 2, "t.huge", &reg, false).unwrap();
        assert_eq!(first.len(), 1);
        assert_ne!(cursor_id, 0);
    }

    #[test]
    fn small_documents_are_governed_by_the_count_cap() {
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..500).map(|_| blob(64)).collect();
        let (first, cursor_id) = split_into_cursor(docs, 101, "t.small", &reg, false).unwrap();
        assert_eq!(first.len(), 101, "the common path must not change");
        assert_ne!(cursor_id, 0);
    }

    #[test]
    fn everything_fitting_exhausts_the_cursor() {
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..5).map(|_| blob(64)).collect();
        let (first, cursor_id) = split_into_cursor(docs, 101, "t.small", &reg, false).unwrap();
        assert_eq!(first.len(), 5);
        assert_eq!(cursor_id, 0);
    }
}
