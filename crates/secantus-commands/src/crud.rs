//! The CRUD write/count command family: `insert`, `update`, `delete`, `count`.
//!
//! Faithful ports of `commands.py::_insert` / `_update` / `_delete` / `_count`,
//! scoped to the paths that map onto the current `secantus-storage` signatures.
//! (`find` lives in the sibling `find` module.)
//!
//! `insert` enforces a collection `validator` (code 121) unless
//! `bypassDocumentValidation` / `validationAction: warn|off`. Pipeline-form `u`
//! applies via `Storage::update_matching_pipeline`; positional operators (`$` /
//! `$[]` / `$[ident]`) + `arrayFilters` via `Storage::update_matching_array_filters`;
//! `let` + `collation` thread through update / delete / count (collation forces a
//! COLLSCAN; non-ASCII / numericOrdering collation → `BadValue`).
//!
//! `writeConcern` validation (malformed `w` / `j` / `wtimeout`) runs in `dispatch`
//! before the handler (`validate_write_concern`), and `writeConcernError`
//! attachment for a satisfiable-but-too-wide `w > 1` is added after
//! (`attach_write_concern_error`). `validator` enforcement runs on `insert` (here)
//! and on `update` / replace (the validator is read here and threaded into the
//! storage update to check the post-apply doc).
//!
//! Direct writes to the synthetic read-only views `local.oplog.rs` /
//! `admin.system.users` are rejected in `dispatch` (`reject_synthetic_view_write`,
//! code 13); reads on a user view (`find` / `aggregate` / `count`) resolve the
//! view's `viewOn` + pipeline in the aggregate/find command layer
//! (`aggregate::resolve_view`).

use bson::{doc, Bson, Document};

/// mongod's per-operator `errInfo.details.reason` wording (best-effort; only a
/// few are byte-pinned by driver tests). Mirrors `commands._VALIDATION_REASON`.
fn validation_reason(op: &str) -> &'static str {
    match op {
        "$type" => "type did not match",
        "$exists" => "field was missing",
        "$regex" => "regular expression did not match",
        "$size" => "array did not match specified size",
        "$mod" => "$mod did not evaluate to the expected remainder",
        "$all" => "array did not contain all specified values",
        "$elemMatch" => "no matching array element found",
        "$in" => "value was not in the set of allowed values",
        "$nin" => "value was in the set of disallowed values",
        _ => "comparison failed",
    }
}

/// Whether `doc` satisfies the single-field clause `{field: spec}`.
fn clause_matches(doc: &Document, field: &str, spec: &Bson) -> bool {
    let mut clause = Document::new();
    clause.insert(field, spec.clone());
    secantus_core::query::matches(doc, &clause, &Document::new(), None).unwrap_or(false)
}

/// mongod-shaped `errInfo.details` for a doc that failed a query-expression
/// validator: walk the clauses, find the first the doc violates, and report the
/// failing operator. Mirrors `commands._validation_failure_details`.
fn validation_failure_details(validator: &Document, doc: &Document) -> Document {
    use secantus_core::{get_path, has_path};
    if validator.contains_key("$jsonSchema") {
        return doc! {"operatorName": "$jsonSchema"};
    }
    for (field, spec) in validator {
        if field.starts_with('$') {
            continue; // document-level logical operator — skip for per-field detail
        }
        if clause_matches(doc, field, spec) {
            continue;
        }
        let mut spec_as = Document::new();
        spec_as.insert(field, spec.clone());
        // Operator-form clause: isolate the specific failing operator; otherwise
        // a bare-equality clause reports as `$eq`.
        let mut detail = match spec {
            Bson::Document(opspec)
                if !opspec.is_empty() && opspec.keys().all(|k| k.starts_with('$')) =>
            {
                let mut op = opspec.keys().next().cloned().unwrap_or_default();
                for cand in opspec.keys() {
                    let single = doc! { cand.clone(): opspec.get(cand).unwrap().clone() };
                    if !clause_matches(doc, field, &Bson::Document(single)) {
                        op = cand.clone();
                        break;
                    }
                }
                let reason = validation_reason(&op);
                doc! { "operatorName": op, "specifiedAs": spec_as, "reason": reason }
            }
            _ => {
                doc! { "operatorName": "$eq", "specifiedAs": spec_as, "reason": "comparison failed" }
            }
        };
        // The value the server considered (and its BSON type), when the field is
        // present — drivers' errorResponse tests read both (mongo-csharp-driver
        // `WriteError_details`, mongo-java-driver `findOneAndUpdate-errorResponse`).
        if has_path(doc, field) {
            if let Some(value) = get_path(doc, field) {
                detail.insert("consideredValue", value.clone());
                detail.insert(
                    "consideredType",
                    secantus_core::query::bson_type_name(value),
                );
            }
        }
        return detail;
    }
    doc! {"operatorName": "validator"}
}

/// The `errInfo` body for a failed document validation: `failingDocumentId`
/// (when the doc has an `_id`) plus the per-operator `details`. Lets drivers'
/// errorResponse tests pick out which doc was rejected. Mirrors
/// `commands._validation_error_info`. Shared with `findandmodify`.
pub(crate) fn validation_error_info(validator: &Document, doc: &Document) -> Document {
    let mut info = Document::new();
    if let Some(id) = doc.get("_id") {
        info.insert("failingDocumentId", id.clone());
    }
    info.insert("details", validation_failure_details(validator, doc));
    info
}

use crate::util::{
    as_i64, bool_field, coll_arg, collation_of, command_error, doc_field, resolve_let_vars,
    write_error,
};
use crate::{CommandContext, CommandError, HandlerResult, StorageError};

/// Build the `insert` reply from storage's outcome: remap storage's
/// surviving-subset indices back to their original `documents` positions and
/// prepend the command-layer pre-check errors. Shared by the decoded and the
/// raw-BSON write paths.
fn insert_reply(
    inserted: usize,
    errors: Vec<Document>,
    pre_errors: Vec<Document>,
    survivor_to_orig: &[usize],
) -> HandlerResult {
    let mut reply = doc! { "n": inserted as i32, "ok": 1.0 };
    if !pre_errors.is_empty() || !errors.is_empty() {
        let mut write_errors: Vec<Bson> = pre_errors.into_iter().map(Bson::Document).collect();
        for mut err in errors {
            if let Some(local) = err.get("index").and_then(as_i64) {
                let orig = survivor_to_orig
                    .get(local as usize)
                    .copied()
                    .unwrap_or(local as usize);
                err.insert("index", orig as i32);
            }
            write_errors.push(Bson::Document(err));
        }
        reply.insert("writeErrors", write_errors);
    }
    Ok(reply)
}

/// The `_id`-may-not-contain-`$`-prefixed-fields pre-check run over raw BSON
/// (no full decode). Mirrors the decoded path exactly: reports the `_id`
/// sub-document's first key when *any* key is `$`-prefixed.
fn raw_id_dollar_key_error(raw: &[u8], index: usize) -> Option<Document> {
    let rd = bson::RawDocument::from_bytes(raw).ok()?;
    let Ok(Some(bson::RawBsonRef::Document(idd))) = rd.get("_id") else {
        return None;
    };
    let mut first_key: Option<String> = None;
    let mut has_dollar = false;
    for elem in idd.into_iter() {
        let Ok((k, _)) = elem else { return None };
        if first_key.is_none() {
            first_key = Some(k.to_string());
        }
        if k.starts_with('$') {
            has_dollar = true;
        }
    }
    if !has_dollar {
        return None;
    }
    let first = first_key.unwrap_or_default();
    Some(doc! {
        "index": index as i32,
        "code": 2,
        "errmsg": format!(
            "_id fields may not contain '$'-prefixed fields: {first} is not valid for storage."
        ),
    })
}

/// Raw-BSON write path: store the client's insert documents verbatim (the byte
/// slices the server handed in un-decoded) without the merge-decode → re-encode
/// → storage-decode round-trip. Only the `_id` `$`-key pre-check runs over every
/// document; a collection `validator` is the one case that forces a per-doc
/// decode-and-match before the bytes go to storage.
fn insert_raw_path(
    db: &str,
    coll: &str,
    raw_docs: Vec<Vec<u8>>,
    ordered: bool,
    validator: Option<&Document>,
    storage: &dyn crate::Storage,
) -> HandlerResult {
    if raw_docs.is_empty() {
        return Ok(doc! {
            "ok": 0.0,
            "errmsg": "Write batch sizes must be between 1 and 100000. Got 0 operations.",
            "code": 4,
            "codeName": "InvalidLength",
        });
    }
    let mut pre_errors: Vec<Document> = Vec::new();
    let mut surviving: Vec<Vec<u8>> = Vec::new();
    let mut survivor_to_orig: Vec<usize> = Vec::new();
    for (index, raw) in raw_docs.into_iter().enumerate() {
        if let Some(err) = raw_id_dollar_key_error(&raw, index) {
            pre_errors.push(err);
            if ordered {
                break;
            }
            continue;
        }
        if let Some(v) = validator {
            // The one case the raw path must decode: match against the validator.
            // Wire-validated BSON always decodes; if it somehow doesn't, fall through
            // and let storage surface the error rather than dropping the doc.
            if let Ok(d) = Document::from_reader(&mut &raw[..]) {
                if !secantus_core::query::matches(&d, v, &Document::new(), None).unwrap_or(true) {
                    pre_errors.push(doc! {
                        "index": index as i32,
                        "code": 121,
                        "errmsg": "Document failed validation",
                        "errInfo": validation_error_info(v, &d),
                    });
                    if ordered {
                        break;
                    }
                    continue;
                }
            }
        }
        surviving.push(raw);
        survivor_to_orig.push(index);
    }

    if !pre_errors.is_empty() && ordered {
        return Ok(doc! { "n": 0_i32, "ok": 1.0, "writeErrors": bson_array(pre_errors) });
    }
    if surviving.is_empty() {
        return Ok(doc! { "n": 0_i32, "ok": 1.0, "writeErrors": bson_array(pre_errors) });
    }
    let (inserted, errors) = storage
        .insert(db, coll, surviving, ordered)
        .map_err(command_error)?;
    insert_reply(inserted, errors, pre_errors, &survivor_to_orig)
}

/// `insert` — batch document insert.
pub fn insert(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "insert")?;
    // Take the raw-BSON write side-channel first, releasing the `&mut` borrow
    // before the immutable `storage()` / `db_name` borrows below.
    let raw_docs = ctx.raw_insert_documents.take();
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    let ordered = bool_field(doc, "ordered", true);

    // Document validation: a collection `validator` rejects non-matching inserts
    // with code 121 unless `validationAction` is "warn"/"off" or
    // `bypassDocumentValidation` is set. A validator the query engine can't
    // evaluate is treated as passing (lenient) rather than wrongly rejecting.
    let bypass = bool_field(doc, "bypassDocumentValidation", false);
    let validator = if bypass {
        None
    } else {
        let opts = storage
            .get_collection_options(&db, &coll)
            .map_err(command_error)?;
        let action = opts.get_str("validationAction").unwrap_or("error");
        // `validationLevel: "off"` disables validation entirely, whatever the
        // action says — mongod checks the level first. The level was stored by
        // `create` / `collMod` and then never consulted, so a collection
        // explicitly opted OUT of validation still had it enforced.
        let level_off = opts.get_str("validationLevel").unwrap_or("strict") == "off";
        if level_off || action == "warn" || action == "off" {
            None
        } else {
            opts.get("validator").and_then(Bson::as_document).cloned()
        }
    };

    // Raw-BSON write fast path: documents arrived as an un-decoded kind-1 sequence.
    if let Some(raw_docs) = raw_docs {
        return insert_raw_path(&db, &coll, raw_docs, ordered, validator.as_ref(), storage);
    }

    // Decoded path: documents inline in the command body.
    let documents = match doc.get("documents") {
        Some(Bson::Array(a)) if !a.is_empty() => a,
        // mongod rejects an empty/absent `documents` array with InvalidLength
        // (4) — drivers gate command-error tests on this exact code/codeName.
        _ => {
            return Ok(doc! {
                "ok": 0.0,
                "errmsg": "Write batch sizes must be between 1 and 100000. Got 0 operations.",
                "code": 4,
                "codeName": "InvalidLength",
            })
        }
    };

    // Per-doc pre-checks (no storage): an `_id` may not carry top-level
    // `$`-prefixed keys. A failure is a per-doc writeError keyed by the doc's
    // position in the original `documents` array.
    let mut pre_errors: Vec<Document> = Vec::new();
    let mut surviving: Vec<Vec<u8>> = Vec::new();
    let mut survivor_to_orig: Vec<usize> = Vec::new();
    for (index, entry) in documents.iter().enumerate() {
        let Bson::Document(d) = entry else {
            // Non-document batch entries are malformed input; mongod rejects
            // them. Surface a per-doc BadValue rather than choking the batch.
            pre_errors.push(doc! {
                "index": index as i32,
                "code": 2,
                "errmsg": "insert document must be an object",
            });
            if ordered {
                break;
            }
            continue;
        };
        if let Some(Bson::Document(id_value)) = d.get("_id") {
            if id_value.keys().any(|k| k.starts_with('$')) {
                let first = id_value.keys().next().map(String::as_str).unwrap_or("");
                pre_errors.push(doc! {
                    "index": index as i32,
                    "code": 2,
                    "errmsg": format!(
                        "_id fields may not contain '$'-prefixed fields: {first} is not valid for storage."
                    ),
                });
                if ordered {
                    break;
                }
                continue;
            }
        }
        // Collection validator (code 121, DocumentValidationFailure). mongod
        // attaches an `errInfo` (failingDocumentId + per-operator details) that
        // drivers' errorResponse tests read; mirror it.
        if let Some(v) = &validator {
            if !secantus_core::query::matches(d, v, &Document::new(), None).unwrap_or(true) {
                pre_errors.push(doc! {
                    "index": index as i32,
                    "code": 121,
                    "errmsg": "Document failed validation",
                    "errInfo": validation_error_info(v, d),
                });
                if ordered {
                    break;
                }
                continue;
            }
        }
        surviving.push(encode_doc(d)?);
        survivor_to_orig.push(index);
    }

    // Ordered + a pre-check failure aborts the whole batch (matching the Python
    // server): nothing is inserted, the first bad doc is reported.
    if !pre_errors.is_empty() && ordered {
        return Ok(doc! { "n": 0_i32, "ok": 1.0, "writeErrors": bson_array(pre_errors) });
    }
    if surviving.is_empty() {
        return Ok(doc! { "n": 0_i32, "ok": 1.0, "writeErrors": bson_array(pre_errors) });
    }

    let (inserted, errors) = storage
        .insert(&db, &coll, surviving, ordered)
        .map_err(command_error)?;
    insert_reply(inserted, errors, pre_errors, &survivor_to_orig)
}

/// `delete` — batch delete, one entry per `{q, limit}` spec.
pub fn delete(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "delete")?;
    let storage = ctx.storage()?;
    let deletes = array_field(doc, "deletes");
    let ordered = bool_field(doc, "ordered", true);

    let mut n = 0_i32;
    let mut write_errors: Vec<Bson> = Vec::new();
    // Command-level `let` → vars visible to `$expr` in each spec's `q` filter.
    let let_vars = resolve_let_vars(doc.get("let"));
    for (index, spec) in deletes.iter().enumerate() {
        let Bson::Document(spec) = spec else { continue };
        let filter = doc_field(spec, "q");
        // `collation` is per-delete-statement (inside each `deletes[]` entry).
        let collation = collation_of(spec);
        // `limit: 0` ⇒ delete all matches; any positive value ⇒ at most that
        // many (mongod only defines 0 and 1, but we honour the integer).
        let limit = spec.get("limit").and_then(as_i64).unwrap_or(0).max(0) as usize;
        match storage.delete_matching_with_let(
            &ctx.db_name,
            &coll,
            &filter,
            limit,
            &let_vars,
            collation.as_ref(),
        ) {
            Ok(deleted) => n += deleted as i32,
            Err(StorageError::Internal(msg)) => {
                return Ok(CommandError::new(1, "InternalError", msg).into_reply())
            }
            // A write conflict fails the whole statement command-level (mongod
            // semantics), so the dispatch txn envelope attaches the transient
            // label and drivers retry the transaction.
            Err(e @ StorageError::WriteConflict) => return Ok(command_error(e).into_reply()),
            Err(e) => {
                write_errors.push(Bson::Document(write_error(index, e)));
                if ordered {
                    break;
                }
            }
        }
    }

    let mut reply = doc! { "n": n, "ok": 1.0 };
    if !write_errors.is_empty() {
        reply.insert("writeErrors", write_errors);
    }
    Ok(reply)
}

/// `count` — count matching documents, honouring `skip` / `limit` clamping the
/// way mongod's `count` command (and the legacy `cursor.count()`) does.
pub fn count(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "count")?;
    let storage = ctx.storage()?;
    let filter = doc_field(doc, "query");
    let collation = collation_of(doc);

    // View support: a collection created with `viewOn` is a read-only view; its
    // count runs the view's pipeline (plus the count's query as a trailing
    // `$match`) through the aggregation engine. estimatedDocumentCount (the
    // drivers' "estimatedDocumentCount works correctly on views" tests) leans on
    // this. Mirrors `commands.py::_count`.
    let view_on = storage
        .get_collection_options(&ctx.db_name, &coll)
        .ok()
        .and_then(|o| o.get_str("viewOn").ok().map(String::from));
    let mut n = if let Some(view_on) = view_on {
        let opts = storage
            .get_collection_options(&ctx.db_name, &coll)
            .map_err(command_error)?;
        let mut pipeline: Vec<Bson> = opts
            .get_array("viewPipeline")
            .ok()
            .cloned()
            .unwrap_or_default();
        if !filter.is_empty() {
            pipeline.push(Bson::Document(doc! { "$match": filter.clone() }));
        }
        pipeline.push(Bson::Document(doc! { "$count": "n" }));
        let agg = doc! { "aggregate": view_on, "pipeline": pipeline, "cursor": {} };
        let reply = crate::aggregate::aggregate(&agg, ctx)?;
        reply
            .get_document("cursor")
            .ok()
            .and_then(|c| c.get_array("firstBatch").ok())
            .and_then(|b| b.first())
            .and_then(Bson::as_document)
            .and_then(|d| d.get("n").and_then(as_i64))
            .unwrap_or(0)
    } else if let Some(hint) = doc.get("hint") {
        // A `hint` forces a specific index; for an empty filter this still walks
        // that index, so hinting a sparse index counts only the docs present in
        // it (php-lib Count `testHintOption`). Count via the hinted find, which
        // honours the hint + sparse semantics. Mirrors `commands.py::_count`.
        storage
            .find_collated(
                &ctx.db_name,
                &coll,
                &filter,
                None,
                Some(hint),
                collation.as_ref(),
                &Document::new(),
            )
            .map_err(command_error)?
            .len() as i64
    } else {
        storage
            .count_collated(&ctx.db_name, &coll, &filter, collation.as_ref())
            .map_err(command_error)? as i64
    };
    let skip = doc.get("skip").and_then(as_i64).unwrap_or(0);
    if skip > 0 {
        n = (n - skip).max(0);
    }
    let limit = doc.get("limit").and_then(as_i64).unwrap_or(0);
    if limit > 0 {
        n = n.min(limit);
    }
    Ok(doc! { "n": n as i32, "ok": 1.0 })
}

/// Stages allowed in a pipeline-form (`u: [...]`) update
/// (`update.py::_PIPELINE_UPDATE_STAGES`).
const PIPELINE_UPDATE_STAGES: [&str; 6] = [
    "$set",
    "$addFields",
    "$unset",
    "$project",
    "$replaceRoot",
    "$replaceWith",
];

/// Update modifiers mongod accepts (`secantus.update._KNOWN_UPDATE_OPS`). A
/// top-level `$`-key outside this set is an "Unknown modifier" parse error.
const KNOWN_UPDATE_OPS: [&str; 15] = [
    "$set",
    "$setOnInsert",
    "$unset",
    "$currentDate",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$push",
    "$addToSet",
    "$pull",
    "$pullAll",
    "$pop",
    "$rename",
    "$bit",
];

/// Parse-time check of an operator-form update document, mirroring
/// `secantus.update.validate_update_doc`: `Some(errmsg)` for an unknown modifier
/// or a mix of operators and replacement fields, else `None` (a pure replacement
/// or a valid operator update). Surfaces as a `FailedToParse` (9) write error.
fn unsupported_update_modifier(u: &Document) -> Option<String> {
    if !u.keys().any(|k| k.starts_with('$')) {
        return None; // replacement-style update
    }
    if !u.keys().all(|k| k.starts_with('$')) {
        return Some("update document cannot mix operators with replacement fields".to_string());
    }
    u.keys()
        .find(|k| !KNOWN_UPDATE_OPS.contains(&k.as_str()))
        .map(|k| {
            format!(
                "Unknown modifier: {k}. Expected a valid update modifier (e.g. $set, $unset, $inc, ...)"
            )
        })
}

/// `update` — batch update, one entry per `{q, u, multi?, upsert?}` spec. Ports
/// `commands.py::_update`. Document-, replacement-, and pipeline-form `u` all
/// apply (pipeline via `Storage::update_matching_pipeline`, diff-style oplog so
/// change streams see `operationType: "update"`). A malformed pipeline still
/// returns the faithful command-level `FailedToParse` (9) / `InvalidPipelineOperator`
/// (168).
///
/// Positional update operators (`$` / `$[]` / `$[ident]`) + `arrayFilters` apply
/// via `Storage::update_matching_array_filters` (`$` resolved from the query
/// filter, `$[ident]` from the per-statement `arrayFilters`); `let` + `collation`
/// thread through (collation forces a COLLSCAN match).
///
/// The collection `validator` is enforced on the post-apply document (read once
/// here, threaded into `Storage::update_matching`); malformed `writeConcern` is
/// rejected in `dispatch` before this handler. **Deferred:**
/// `_reject_oplog_rs_write` (writes to `local.oplog.rs`).
///
/// Whether a timeseries update's `u` touches only the metaField (mongod 7.0's
/// rule): an operator-form update whose every modifier targets a path rooted at
/// `meta`. Replacement / pipeline updates and an empty `meta` (no metaField
/// declared) never qualify.
fn ts_update_is_meta_only(u: Option<&Bson>, meta: &str) -> bool {
    if meta.is_empty() {
        return false;
    }
    let Some(Bson::Document(ud)) = u else {
        return false;
    };
    if ud.is_empty() || !ud.keys().all(|k| k.starts_with('$')) {
        return false;
    }
    for (_op, arg) in ud {
        let Some(fields) = arg.as_document() else {
            return false;
        };
        for fpath in fields.keys() {
            if fpath.split('.').next().unwrap_or("") != meta {
                return false;
            }
        }
    }
    true
}

pub fn update(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "update")?;
    let storage = ctx.storage()?;
    let updates = array_field(doc, "updates");
    let ordered = bool_field(doc, "ordered", true);

    let mut n = 0_i32;
    let mut n_modified = 0_i32;
    let mut upserted: Vec<Bson> = Vec::new();
    let mut write_errors: Vec<Bson> = Vec::new();

    // Command-level `let` → vars visible to `$expr` in each spec's `q` filter
    // (and to pipeline-form `u` expressions). Resolved once for the batch.
    let let_vars = resolve_let_vars(doc.get("let"));

    // Collection validator on the POST-APPLY document (code 121) unless
    // `bypassDocumentValidation` or `validationAction: warn|off`. The validator
    // is read once here (no storage lock held) and threaded into the storage
    // update so it can check the rewritten doc before writing.
    let opts = storage
        .get_collection_options(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let validator = if bool_field(doc, "bypassDocumentValidation", false) {
        None
    } else {
        let action = opts.get_str("validationAction").unwrap_or("error");
        // `validationLevel: "off"` disables validation entirely, whatever the
        // action says — mongod checks the level first. The level was stored by
        // `create` / `collMod` and then never consulted, so a collection
        // explicitly opted OUT of validation still had it enforced.
        let level_off = opts.get_str("validationLevel").unwrap_or("strict") == "off";
        if level_off || action == "warn" || action == "off" {
            None
        } else {
            opts.get("validator").and_then(Bson::as_document).cloned()
        }
    };
    // `validationLevel: "moderate"` applies the validator to inserts and to
    // updates of documents that ALREADY satisfy it, but exempts documents that
    // were already invalid when the validator was introduced — the level exists
    // so a validator can be added to a collection with legacy rows without
    // freezing them.
    let validator_moderate = opts.get_str("validationLevel").unwrap_or("strict") == "moderate";

    // mongod 7.0 restricts updates on a timeseries collection to the metaField
    // only. `ts_meta` is `Some(metaField)` for a timeseries collection (an empty
    // string when no metaField is declared — then nothing is updatable).
    let ts_meta: Option<String> = opts
        .get_document("timeseries")
        .ok()
        .map(|t| t.get_str("metaField").unwrap_or("").to_string());

    for (index, spec) in updates.iter().enumerate() {
        let Bson::Document(spec) = spec else { continue };

        // MongoDB 8.0 added a per-spec `sort`; pre-8.0 (we advertise 7.0) it's a
        // command-level FailedToParse. Drivers' `*-sort` tests with
        // `maxServerVersion: "7.99"` assert this.
        if spec.contains_key("sort") {
            return Ok(CommandError::new(
                9,
                "FailedToParse",
                "The 'sort' option is not supported on update commands before MongoDB 8.0",
            )
            .into_reply());
        }

        // `collation` is per-update-statement (inside each `updates[]` entry),
        // not a command-level field — collation-aware filter matching (COLLSCAN).
        let collation = collation_of(spec);
        let q = doc_field(spec, "q");
        let multi = bool_field(spec, "multi", false);
        let upsert = bool_field(spec, "upsert", false);

        // Timeseries (mongod 7.0): an update may only modify the metaField, via
        // operator-form modifiers — no replacement / pipeline / non-meta paths.
        if let Some(meta) = &ts_meta {
            if !ts_update_is_meta_only(spec.get("u"), meta) {
                write_errors.push(Bson::Document(doc! {
                    "index": index as i32,
                    "code": 2,
                    "errmsg": "Cannot perform an update on a time-series collection \
                               that modifies a field other than the metaField",
                }));
                if ordered {
                    break;
                }
                continue;
            }
        }

        // Parse-time validation of an operator-form `u`: an unknown modifier (or
        // mixing operators with replacement fields) is rejected at parse time —
        // mongod errors even against an empty / no-match collection, where the
        // apply-time engine would never run. Pipeline-form `u` is validated below.
        if !matches!(spec.get("u"), Some(Bson::Array(_))) {
            if let Some(errmsg) = unsupported_update_modifier(&doc_field(spec, "u")) {
                write_errors.push(Bson::Document(doc! {
                    "index": index as i32, "code": 9, "errmsg": errmsg,
                }));
                if ordered {
                    break;
                }
                continue;
            }
        }

        // Pipeline-form `u` (`[...]`) vs operator/replacement-form (`{...}`).
        let outcome = if let Some(Bson::Array(stages)) = spec.get("u") {
            // Validate the shape up-front — real mongod returns a command-level
            // error for a malformed/unknown stage, not a per-op writeError.
            for stage in stages {
                match stage {
                    Bson::Document(s) if s.len() == 1 => {
                        let name = s.keys().next().map(String::as_str).unwrap_or("");
                        if !PIPELINE_UPDATE_STAGES.contains(&name) {
                            return Ok(CommandError::new(
                                168,
                                "InvalidPipelineOperator",
                                format!("stage {name} not allowed in pipeline updates"),
                            )
                            .into_reply());
                        }
                    }
                    _ => {
                        return Ok(CommandError::new(
                            9,
                            "FailedToParse",
                            "each pipeline stage must be a single-key document",
                        )
                        .into_reply())
                    }
                }
            }
            storage.update_matching_pipeline(
                &ctx.db_name,
                &coll,
                &q,
                stages,
                multi,
                upsert,
                &let_vars,
                collation.as_ref(),
                validator.as_ref(),
                validator_moderate,
                false,
            )
        } else {
            let u = doc_field(spec, "u");
            // arrayFilters (`$[ident]`) is per-update-statement. The `$` / `$[]`
            // positional operators resolve in storage regardless; `$[ident]`
            // needs these filter docs.
            let array_filters: Vec<Document> = spec
                .get("arrayFilters")
                .and_then(Bson::as_array)
                .map(|a| a.iter().filter_map(|b| b.as_document().cloned()).collect())
                .unwrap_or_default();
            storage.update_matching_array_filters(
                &ctx.db_name,
                &coll,
                &q,
                &u,
                multi,
                upsert,
                &array_filters,
                &let_vars,
                collation.as_ref(),
                validator.as_ref(),
                validator_moderate,
                false,
            )
        };

        match outcome {
            Ok(outcome) => {
                n += outcome.matched as i32;
                n_modified += outcome.modified as i32;
                if let Some(id) = outcome.upserted_id {
                    upserted.push(Bson::Document(doc! { "index": index as i32, "_id": id }));
                    n += 1;
                }
            }
            Err(StorageError::Internal(msg)) => {
                return Ok(CommandError::new(1, "InternalError", msg).into_reply())
            }
            // A write conflict fails the whole statement command-level (mongod
            // semantics), so the dispatch txn envelope attaches the transient
            // label and drivers retry the transaction.
            Err(e @ StorageError::WriteConflict) => return Ok(command_error(e).into_reply()),
            Err(e) => {
                write_errors.push(Bson::Document(write_error(index, e)));
                if ordered {
                    break;
                }
            }
        }
    }

    let mut reply = doc! { "n": n, "nModified": n_modified, "ok": 1.0 };
    if !upserted.is_empty() {
        reply.insert("upserted", upserted);
    }
    if !write_errors.is_empty() {
        reply.insert("writeErrors", write_errors);
    }
    Ok(reply)
}

// --- helpers -------------------------------------------------------------

/// An array-valued field as a slice, or empty.
fn array_field<'a>(doc: &'a Document, key: &str) -> &'a [Bson] {
    match doc.get(key) {
        Some(Bson::Array(a)) => a,
        _ => &[],
    }
}

/// Encode a document to BSON bytes for the storage seam.
fn encode_doc(d: &Document) -> Result<Vec<u8>, CommandError> {
    let mut bytes = Vec::new();
    d.to_writer(&mut bytes).map_err(|e| {
        CommandError::new(
            1,
            "InternalError",
            format!("failed to encode document: {e}"),
        )
    })?;
    Ok(bytes)
}

fn bson_array(docs: Vec<Document>) -> Vec<Bson> {
    docs.into_iter().map(Bson::Document).collect()
}
