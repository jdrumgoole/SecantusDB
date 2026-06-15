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
//! **Deferred (documented so parity is honest):**
//! * `writeConcern` validation and `writeConcernError` attachment (cross-cutting,
//!   lands with the write-concern slice).
//! * `validator` enforcement on `update` / replace (needs the post-apply doc in
//!   storage; `insert` is enforced here at the command layer).
//! * `_reject_oplog_rs_write` (writes to `local.oplog.rs`); view-collection
//!   `count`.

use bson::{doc, Bson, Document};

use crate::util::{
    as_i64, bool_field, coll_arg, collation_of, command_error, doc_field, resolve_let_vars,
    write_error,
};
use crate::{CommandContext, CommandError, HandlerResult, StorageError};

/// `insert` — batch document insert.
pub fn insert(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "insert")?;
    let storage = ctx.storage()?;

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
            .get_collection_options(&ctx.db_name, &coll)
            .map_err(command_error)?;
        let action = opts.get_str("validationAction").unwrap_or("error");
        if action == "warn" || action == "off" {
            None
        } else {
            opts.get("validator").and_then(Bson::as_document).cloned()
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
        // Collection validator (code 121, DocumentValidationFailure).
        if let Some(v) = &validator {
            if !secantus_core::query::matches(d, v, &Document::new(), None).unwrap_or(true) {
                pre_errors.push(doc! {
                    "index": index as i32,
                    "code": 121,
                    "errmsg": "Document failed validation",
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
        .insert(&ctx.db_name, &coll, surviving, ordered)
        .map_err(command_error)?;

    let mut reply = doc! { "n": inserted as i32, "ok": 1.0 };
    if !pre_errors.is_empty() || !errors.is_empty() {
        // Remap storage's `index` (into the surviving subset) back to the
        // original `documents` position, then concatenate after the pre-errors.
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

    let mut n = storage
        .count_collated(&ctx.db_name, &coll, &filter, collation.as_ref())
        .map_err(command_error)? as i64;
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
/// **Deferred (tracked in backlog §7):** `validator`, `writeConcern`,
/// `_reject_oplog_rs_write` (none are in the Rust `update_matching` seam yet).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A minimal in-memory storage for exercising the handlers' reply-shaping.
    /// Real matching/indexing is `secantus-storage`'s job (tested there); this
    /// fake supports the empty filter (all docs) and a single `{field: value}`
    /// equality, which is all the handler-plumbing tests need.
    #[derive(Default)]
    struct FakeStorage {
        cols: Mutex<HashMap<(String, String), Vec<Document>>>,
        opts: Mutex<HashMap<(String, String), Document>>,
    }

    impl FakeStorage {
        fn arc() -> Arc<FakeStorage> {
            Arc::new(FakeStorage::default())
        }
        fn seed(&self, db: &str, coll: &str, docs: Vec<Document>) {
            self.cols
                .lock()
                .unwrap()
                .insert((db.to_string(), coll.to_string()), docs);
        }
        fn set_options(&self, db: &str, coll: &str, opts: Document) {
            self.opts
                .lock()
                .unwrap()
                .insert((db.to_string(), coll.to_string()), opts);
        }
    }

    fn matches(d: &Document, filter: &Document) -> bool {
        filter.iter().all(|(k, v)| d.get(k) == Some(v))
    }

    impl crate::Storage for FakeStorage {
        fn insert(
            &self,
            db: &str,
            coll: &str,
            docs: Vec<Vec<u8>>,
            ordered: bool,
        ) -> Result<(usize, Vec<Document>), StorageError> {
            let mut cols = self.cols.lock().unwrap();
            let bucket = cols.entry((db.to_string(), coll.to_string())).or_default();
            let mut inserted = 0;
            let mut errors = Vec::new();
            for (i, bytes) in docs.iter().enumerate() {
                let d = bson::Document::from_reader(&mut bytes.as_slice()).unwrap();
                // Duplicate-`_id` rejection, the canonical insert error path.
                let dup = match d.get("_id") {
                    Some(id) => bucket.iter().any(|e| e.get("_id") == Some(id)),
                    None => false,
                };
                if dup {
                    errors.push(doc! {
                        "index": i as i32,
                        "code": 11000,
                        "errmsg": "E11000 duplicate key error",
                        "keyPattern": { "_id": 1 },
                        "keyValue": { "_id": d.get("_id").cloned().unwrap_or(Bson::Null) },
                    });
                    if ordered {
                        break;
                    }
                    continue;
                }
                bucket.push(d);
                inserted += 1;
            }
            Ok((inserted, errors))
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
            // Minimal `$set`-only semantics, enough to exercise the handler's
            // matched/modified accounting, multi, and upsert.
            let mut cols = self.cols.lock().unwrap();
            let bucket = cols.entry((db.to_string(), coll.to_string())).or_default();
            let set = update.get("$set").and_then(Bson::as_document).cloned();
            let mut matched = 0usize;
            let mut modified = 0usize;
            for d in bucket.iter_mut() {
                if !matches(d, filter) {
                    continue;
                }
                matched += 1;
                if let Some(set) = &set {
                    let mut changed = false;
                    for (k, v) in set.iter() {
                        if d.get(k) != Some(v) {
                            d.insert(k.clone(), v.clone());
                            changed = true;
                        }
                    }
                    if changed {
                        modified += 1;
                    }
                }
                if !multi {
                    break;
                }
            }
            let upserted_id = if matched == 0 && upsert {
                let id = filter.get("_id").cloned().unwrap_or(Bson::Int64(999));
                let mut new_doc = doc! { "_id": id.clone() };
                if let Some(set) = &set {
                    for (k, v) in set.iter() {
                        new_doc.insert(k.clone(), v.clone());
                    }
                }
                bucket.push(new_doc);
                Some(id)
            } else {
                None
            };
            Ok(UpdateOutcome {
                matched,
                modified,
                upserted_id,
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
            _upsert: bool,
            _let_vars: &Document,
            _collation: Option<&crate::storage::Collation>,
        ) -> Result<UpdateOutcome, StorageError> {
            // Apply the pipeline to each matched doc (the real storage path).
            let mut cols = self.cols.lock().unwrap();
            let bucket = cols.entry((db.to_string(), coll.to_string())).or_default();
            let mut matched = 0usize;
            let mut modified = 0usize;
            for d in bucket.iter_mut() {
                if !matches(d, filter) {
                    continue;
                }
                matched += 1;
                let out = secantus_core::aggregate::apply_pipeline(
                    vec![d.clone()],
                    pipeline,
                    &Document::new(),
                    None,
                )
                .map_err(|_| StorageError::WriteError {
                    code: 2,
                    errmsg: "pipeline".into(),
                })?;
                if let Some(new) = out.into_iter().next() {
                    if &new != d {
                        *d = new;
                        modified += 1;
                    }
                }
                if !multi {
                    break;
                }
            }
            Ok(UpdateOutcome {
                matched,
                modified,
                upserted_id: None,
            })
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
            let mut i = 0;
            while i < bucket.len() {
                if matches(&bucket[i], filter) {
                    bucket.remove(i);
                    removed += 1;
                    if limit != 0 && removed >= limit {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
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
        fn get_collection_options(&self, db: &str, coll: &str) -> Result<Document, StorageError> {
            Ok(self
                .opts
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
            _sort: Option<&Document>,
            _hint: Option<crate::storage::RawHint<'_>>,
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

    use crate::UpdateOutcome;

    fn ctx(storage: Arc<FakeStorage>) -> CommandContext {
        CommandContext::new(1).with_storage(storage)
    }

    #[test]
    fn insert_then_count() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}], "$db": "t"},
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert!(reply.get("writeErrors").is_none());

        let mut c = ctx(s);
        c.db_name = "admin".into();
        // db defaults to the ctx db_name (admin); insert went to ctx default
        // too, so count against the same namespace.
        let reply = dispatch(&doc! {"count": "c"}, &mut c);
        assert_eq!(reply.get_i32("n").unwrap(), 2);
    }

    #[test]
    fn insert_rejects_validator_violation() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        let db = c.db_name.clone();
        s.set_options(&db, "c", doc! {"validator": {"a": {"$exists": true}}});
        // doc 0 violates (no `a`), doc 1 passes.
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2, "a": 1}],
            "ordered": false},
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1, "only the valid doc inserts");
        let we = reply.get_array("writeErrors").unwrap();
        assert_eq!(we.len(), 1);
        let e = we[0].as_document().unwrap();
        assert_eq!(e.get_i32("code").unwrap(), 121);
        assert_eq!(e.get_i32("index").unwrap(), 0);
    }

    #[test]
    fn insert_bypass_document_validation_skips_validator() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        let db = c.db_name.clone();
        s.set_options(&db, "c", doc! {"validator": {"a": {"$exists": true}}});
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}],
            "bypassDocumentValidation": true},
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1);
        assert!(reply.get("writeErrors").is_none());
    }

    #[test]
    fn insert_empty_documents_is_invalid_length() {
        let mut c = ctx(FakeStorage::arc());
        let reply = dispatch(&doc! {"insert": "c", "documents": []}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 4);
        assert_eq!(reply.get_str("codeName").unwrap(), "InvalidLength");
    }

    #[test]
    fn insert_id_with_dollar_prefix_rejected() {
        let mut c = ctx(FakeStorage::arc());
        let reply = dispatch(
            &doc! {"insert": "c", "documents": [{"_id": {"$bad": 1}}]},
            &mut c,
        );
        // ordered (default) + a pre-check failure ⇒ nothing inserted, one error.
        assert_eq!(reply.get_i32("n").unwrap(), 0);
        let we = reply.get_array("writeErrors").unwrap();
        assert_eq!(we.len(), 1);
        let e = we[0].as_document().unwrap();
        assert_eq!(e.get_i32("code").unwrap(), 2);
        assert!(e.get_str("errmsg").unwrap().contains("$bad"));
    }

    #[test]
    fn insert_duplicate_key_unordered_continues_and_remaps_index() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        // seed _id 2
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 2}]}, &mut c);
        // unordered batch: [ok(1), dup(2), ok(3)] ⇒ n=2, one writeError at index 1.
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {
                "insert": "c",
                "documents": [{"_id": 1}, {"_id": 2}, {"_id": 3}],
                "ordered": false,
            },
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        let we = reply.get_array("writeErrors").unwrap();
        assert_eq!(we.len(), 1);
        let e = we[0].as_document().unwrap();
        assert_eq!(e.get_i32("index").unwrap(), 1, "index remapped to original");
        assert_eq!(e.get_i32("code").unwrap(), 11000);
    }

    #[test]
    fn insert_pre_error_index_remap_unordered() {
        // [bad-$id(0), ok(1), dup(2)] unordered: the storage error on the dup
        // (original index 2) must remap correctly past the pre-error at 0.
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(&doc! {"insert": "c", "documents": [{"_id": 9}]}, &mut c);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {
                "insert": "c",
                "documents": [{"_id": {"$x": 1}}, {"_id": 5}, {"_id": 9}],
                "ordered": false,
            },
            &mut c,
        );
        // _id 5 inserted; _id 9 duplicate; _id {$x} pre-rejected.
        assert_eq!(reply.get_i32("n").unwrap(), 1);
        let we = reply.get_array("writeErrors").unwrap();
        assert_eq!(we.len(), 2);
        let pre = we[0].as_document().unwrap();
        assert_eq!(pre.get_i32("index").unwrap(), 0);
        assert_eq!(pre.get_i32("code").unwrap(), 2);
        let dup = we[1].as_document().unwrap();
        assert_eq!(dup.get_i32("index").unwrap(), 2);
        assert_eq!(dup.get_i32("code").unwrap(), 11000);
    }

    #[test]
    fn delete_removes_and_counts() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 1}, {"_id": 3, "x": 2}]},
            &mut c,
        );
        let mut c = ctx(s.clone());
        // limit 0 ⇒ delete all matching x:1
        let reply = dispatch(
            &doc! {"delete": "c", "deletes": [{"q": {"x": 1}, "limit": 0}]},
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"count": "c"}, &mut c);
        assert_eq!(reply.get_i32("n").unwrap(), 1);
    }

    #[test]
    fn delete_limit_one() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 1}]},
            &mut c,
        );
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"delete": "c", "deletes": [{"q": {"x": 1}, "limit": 1}]},
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1);
    }

    #[test]
    fn count_skip_and_limit_clamp() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 4}]},
            &mut c,
        );
        let mut c = ctx(s.clone());
        assert_eq!(
            dispatch(&doc! {"count": "c", "skip": 1}, &mut c)
                .get_i32("n")
                .unwrap(),
            3
        );
        let mut c = ctx(s);
        assert_eq!(
            dispatch(&doc! {"count": "c", "limit": 2}, &mut c)
                .get_i32("n")
                .unwrap(),
            2
        );
    }

    #[test]
    fn data_command_without_storage_is_internal_error() {
        let mut c = CommandContext::new(1); // no storage attached
        let reply = dispatch(&doc! {"count": "c"}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 1);
        assert_eq!(reply.get_str("codeName").unwrap(), "InternalError");
    }

    #[test]
    fn update_set_modifies_and_counts() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}]},
            &mut c,
        );
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"x": 2}}}]},
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1);
        assert_eq!(reply.get_i32("nModified").unwrap(), 1);
        assert!(reply.get("upserted").is_none());
        assert!(reply.get("writeErrors").is_none());
    }

    #[test]
    fn update_multi_touches_all_matches() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(
            &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 1}]},
            &mut c,
        );
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"x": 1}, "u": {"$set": {"y": 9}}, "multi": true}]},
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        assert_eq!(reply.get_i32("nModified").unwrap(), 2);
    }

    #[test]
    fn update_upsert_reports_upserted_id() {
        let mut c = ctx(FakeStorage::arc());
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {"_id": 5}, "u": {"$set": {"a": 1}}, "upsert": true}]},
            &mut c,
        );
        assert_eq!(reply.get_i32("n").unwrap(), 1, "upsert counts toward n");
        assert_eq!(reply.get_i32("nModified").unwrap(), 0);
        let up = reply.get_array("upserted").unwrap();
        assert_eq!(up.len(), 1);
        let e = up[0].as_document().unwrap();
        assert_eq!(e.get_i32("index").unwrap(), 0);
        assert_eq!(e.get_i32("_id").unwrap(), 5);
    }

    #[test]
    fn update_sort_option_rejected_pre_8() {
        let mut c = ctx(FakeStorage::arc());
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "sort": {"a": 1}}]},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 9);
        assert_eq!(reply.get_str("codeName").unwrap(), "FailedToParse");
    }

    #[test]
    fn update_pipeline_unknown_stage_is_command_error() {
        let mut c = ctx(FakeStorage::arc());
        let reply = dispatch(
            &doc! {"update": "c", "updates": [{"q": {}, "u": [{"$badStage": {}}]}]},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 168);
        assert_eq!(
            reply.get_str("codeName").unwrap(),
            "InvalidPipelineOperator"
        );
    }

    #[test]
    fn update_valid_pipeline_applies_via_storage() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        let db = c.db_name.clone();
        s.seed(
            &db,
            "c",
            vec![doc! {"_id": 1, "a": 0}, doc! {"_id": 2, "a": 0}],
        );
        let reply = dispatch(
            &doc! {"update": "c", "updates": [
                {"q": {}, "u": [{"$set": {"a": 1}}], "multi": true}
            ]},
            &mut c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert!(reply.get("writeErrors").is_none(), "pipeline now applies");
        assert_eq!(reply.get_i32("n").unwrap(), 2);
        assert_eq!(reply.get_i32("nModified").unwrap(), 2);
        let cols = s.cols.lock().unwrap();
        let bucket = cols.get(&(db, "c".to_string())).unwrap();
        assert!(bucket.iter().all(|d| d.get_i32("a") == Ok(1)));
    }
}
