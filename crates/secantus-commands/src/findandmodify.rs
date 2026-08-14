//! The `findAndModify` command — atomically find one document and update/remove
//! it, returning the pre- or post-image.
//!
//! A port of `commands.py::_find_and_modify`, composed at the command layer from
//! the existing `Storage` trait methods: `find` limit-1 + sort picks the
//! target, then the write (`update_matching_*` / `delete_matching_with_let`)
//! is keyed by the matched `_id` *with the original query re-asserted*, in a
//! re-pick loop — a concurrent writer that changes or steals the doc makes
//! the write match 0 and we loop back, mirroring mongod re-evaluating the
//! predicate when it acquires the document write. The `new: true` post-image
//! comes from the write itself (`UpdateOutcome::post_image`, captured under
//! the storage lock), never from a re-`find` — the re-read was a race that
//! could hand two concurrent clients the same "new" document.
//!
//! `let` (`$expr` in `query`) + `collation` apply to the match (via
//! `find_collated`); the update/delete is keyed by the matched `_id`.
//!
//! Collection `validator` is enforced on the update/upsert path (code 121,
//! bypassable via `bypassDocumentValidation`), via `update_matching_array_filters`.
//!
//! **Deferred:** `arrayFilters`; `writeConcern`; `_reject_oplog_rs_write`.

use bson::{doc, Bson, Document};

use crate::util::{bool_field, collation_of, command_error, doc_field, resolve_let_vars};
use crate::{CommandContext, CommandError, HandlerResult, StorageError};

/// `findAndModify` / `findandmodify`.
pub fn find_and_modify(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = match doc
        .get("findAndModify")
        .or_else(|| doc.get("findandmodify"))
    {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "findAndModify requires a string collection name",
            ))
        }
    };
    // `query` must be a document. mongod rejects a bare value (e.g. an ObjectId
    // passed as a findOneAnd* filter) with TypeMismatch (14) rather than treating
    // it as an empty filter — mongo-node-driver's "object ids as a query
    // predicate" tests assert the error.
    if let Some(q) = doc.get("query") {
        if !matches!(q, Bson::Document(_)) {
            let ty = secantus_core::query::bson_type_name(q);
            return Ok(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'findAndModify.query' is the wrong type '{ty}', \
                     expected type 'object'"
                ),
            )
            .into_reply());
        }
    }
    let query = doc_field(doc, "query");
    let sort = doc.get("sort").and_then(Bson::as_document);
    let fields = doc.get("fields").and_then(Bson::as_document);
    let return_new = doc.get("new").and_then(Bson::as_bool).unwrap_or(false);
    let upsert = doc.get("upsert").and_then(Bson::as_bool).unwrap_or(false);
    let is_remove = doc.get("remove").and_then(Bson::as_bool).unwrap_or(false);
    let update = doc.get("update");

    // Mutually-exclusive arg validation (FailedToParse, code 9).
    if is_remove && update.is_some() {
        return Ok(CommandError::new(
            9,
            "FailedToParse",
            "Cannot specify both update and remove=true",
        )
        .into_reply());
    }
    if !is_remove && update.is_none() {
        return Ok(CommandError::new(
            9,
            "FailedToParse",
            "Either an update or remove=true must be specified",
        )
        .into_reply());
    }

    let storage = ctx.storage()?;
    // Command `let` (visible to `$expr` in `query`) + `collation` apply to the
    // match. The subsequent update/delete is keyed by the matched doc's `_id`.
    let let_vars = resolve_let_vars(doc.get("let"));
    let collation = collation_of(doc);
    // `arrayFilters` ($[ident] identifiers) for an operator-form update.
    let array_filters: Vec<Document> = doc
        .get("arrayFilters")
        .and_then(Bson::as_array)
        .map(|a| a.iter().filter_map(|b| b.as_document().cloned()).collect())
        .unwrap_or_default();
    // Pipeline-form update (`update: [ {$set: …}, … ]`) vs operator/replacement.
    let pipeline = update.and_then(Bson::as_array);

    // Collection validator on the post-apply doc (code 121) unless
    // `bypassDocumentValidation` / `validationAction: warn|off`, mirroring the
    // `update` handler. Threaded into the update via `update_matching_array_filters`.
    // The `findAndModify` command is unusual in that the canonical wire field is
    // the snake_case `bypass_document_validation` (what pymongo's
    // `find_one_and_*` helpers emit); accept the camelCase spelling too for
    // raw-command callers.
    let bypass = bool_field(doc, "bypassDocumentValidation", false)
        || bool_field(doc, "bypass_document_validation", false);
    let val_opts = storage
        .get_collection_options(&ctx.db_name, &coll)
        .map_err(command_error)?;
    // `validationLevel: "moderate"` exempts documents that ALREADY failed the
    // validator from update-time validation; `"off"` disables it entirely.
    // Both were stored by `create` / `collMod` and then never consulted here.
    let validator_moderate = val_opts.get_str("validationLevel").unwrap_or("strict") == "moderate";
    let validator = if bypass {
        None
    } else {
        let action = val_opts.get_str("validationAction").unwrap_or("error");
        let level_off = val_opts.get_str("validationLevel").unwrap_or("strict") == "off";
        if level_off || action == "warn" || action == "off" {
            None
        } else {
            val_opts
                .get("validator")
                .and_then(Bson::as_document)
                .cloned()
        }
    };

    // Find → re-assert → write loop. The write below is keyed by the matched
    // doc's `_id` AND re-asserts the original query, so a concurrent writer
    // that changes (or steals) the doc between our find and our write makes
    // the write match 0 — we loop back and re-pick, mirroring mongod
    // re-evaluating the predicate when it acquires the document write. The
    // `new: true` post-image comes from the write itself
    // (`UpdateOutcome::post_image`), never from a re-`find` a concurrent
    // writer could land in front of.
    //
    // The loop is unbounded (mongod's predicate re-evaluation is too), so a
    // steal storm — many concurrent findAndModify contending for the same
    // job-queue document — is only visible if we say so. Log a warning every
    // few seconds of continuous re-picking, mirroring the storage layer's
    // write-conflict retry telemetry, so a livelock shows up in the server
    // log instead of as silent CPU.
    let repick_started = std::time::Instant::now();
    let mut repick_last_log = repick_started;
    let mut repick_count: u64 = 0;
    loop {
        if repick_count > 0 {
            let now = std::time::Instant::now();
            if now.duration_since(repick_last_log) >= std::time::Duration::from_secs(5) {
                repick_last_log = now;
                eprintln!(
                    "secantus: findAndModify on {}.{} re-picking for {:.1}s \
                     ({repick_count} steals) — concurrent writers keep taking \
                     the matched document",
                    ctx.db_name,
                    coll,
                    now.duration_since(repick_started).as_secs_f64(),
                );
            }
        }
        // Find the target (first match in sort order).
        let candidates = storage
            .find_collated(
                &ctx.db_name,
                &coll,
                &query,
                sort,
                None,
                collation.as_ref(),
                &let_vars,
            )
            .map_err(command_error)?;
        let matched = candidates.into_iter().next();

        // No match.
        let Some(matched_bytes) = matched else {
            if upsert && !is_remove {
                let outcome = match if let Some(stages) = pipeline {
                    storage.update_matching_pipeline(
                        &ctx.db_name,
                        &coll,
                        &query,
                        stages,
                        false,
                        true,
                        &let_vars,
                        collation.as_ref(),
                        validator.as_ref(),
                        validator_moderate,
                        return_new,
                    )
                } else {
                    let upd = update
                        .and_then(Bson::as_document)
                        .cloned()
                        .unwrap_or_default();
                    storage.update_matching_array_filters(
                        &ctx.db_name,
                        &coll,
                        &query,
                        &upd,
                        false,
                        true,
                        &array_filters,
                        &let_vars,
                        collation.as_ref(),
                        validator.as_ref(),
                        validator_moderate,
                        return_new,
                    )
                } {
                    Ok(o) => o,
                    Err(e) => return Ok(storage_err_reply(e)),
                };
                let upserted_id = outcome.upserted_id.unwrap_or(Bson::Null);
                let value = if return_new && upserted_id != Bson::Null {
                    match outcome.post_image {
                        // The upserted doc, from the write itself — a re-`find`
                        // here races concurrent writers to the same doc.
                        Some(post) => project_value(post, fields, &query)?,
                        None => Bson::Null,
                    }
                } else {
                    Bson::Null
                };
                return Ok(doc! {
                    "lastErrorObject": { "n": 1, "updatedExisting": false, "upserted": upserted_id },
                    "value": value,
                    "ok": 1.0,
                });
            }
            return Ok(doc! {
                "lastErrorObject": { "n": 0, "updatedExisting": false },
                "value": Bson::Null,
                "ok": 1.0,
            });
        };

        let matched_doc = decode(&matched_bytes)?;
        let matched_id = matched_doc.get("_id").cloned().unwrap_or(Bson::Null);
        // The write filter: the matched `_id` plus the original query re-asserted,
        // so the write only lands if the doc still satisfies the predicate.
        let mut write_filter = doc! { "_id": matched_id.clone() };
        if !query.is_empty() {
            write_filter.insert("$and", vec![Bson::Document(query.clone())]);
        }

        // remove=true: delete the matched doc, return its (pre-image) value.
        if is_remove {
            let deleted = match storage.delete_matching_with_let(
                &ctx.db_name,
                &coll,
                &write_filter,
                1,
                &let_vars,
                collation.as_ref(),
            ) {
                Ok(n) => n,
                Err(e) => return Ok(storage_err_reply(e)),
            };
            if deleted == 0 {
                // A concurrent writer removed or changed the doc first — the
                // pre-image in hand was never ours to claim. Re-pick.
                repick_count += 1;
                continue;
            }
            let value = project_value(matched_doc, fields, &query)?;
            return Ok(doc! {
                "lastErrorObject": { "n": 1, "updatedExisting": true },
                "value": value,
                "ok": 1.0,
            });
        }

        // Command-layer validator check that produces mongod's `errInfo`
        // (failingDocumentId + per-operator details) — storage enforces the
        // validator too, but its error lacks the detail drivers' errorResponse tests
        // read (mongo-c-driver findOneAndUpdate-errorResponse). Only the
        // operator/replacement form is reconstructible in-memory here; pipeline-form
        // falls back to storage's plain 121. Purely additive: it only short-circuits
        // on a clear post-apply validation failure, otherwise the storage write runs.
        if let Some(v) = &validator {
            if pipeline.is_none() {
                let upd = update
                    .and_then(Bson::as_document)
                    .cloned()
                    .unwrap_or_default();
                if let Ok(post) = secantus_core::update::apply_update(&matched_doc, &upd, false) {
                    if !secantus_core::query::matches(&post, v, &Document::new(), None)
                        .unwrap_or(true)
                    {
                        return Ok(doc! {
                            "ok": 0.0,
                            "errmsg": "Document failed validation",
                            "code": 121,
                            "codeName": "DocumentValidationFailure",
                            "errInfo": crate::crud::validation_error_info(v, &post),
                        });
                    }
                }
            }
        }

        // update: apply it to the matched doc (pipeline-form or operator/replacement).
        let update_result = if let Some(stages) = pipeline {
            storage.update_matching_pipeline(
                &ctx.db_name,
                &coll,
                &write_filter,
                stages,
                false,
                false,
                &let_vars,
                collation.as_ref(),
                validator.as_ref(),
                validator_moderate,
                return_new,
            )
        } else {
            let upd = update
                .and_then(Bson::as_document)
                .cloned()
                .unwrap_or_default();
            storage.update_matching_array_filters(
                &ctx.db_name,
                &coll,
                &write_filter,
                &upd,
                false,
                false,
                &array_filters,
                &let_vars,
                collation.as_ref(),
                validator.as_ref(),
                validator_moderate,
                return_new,
            )
        };
        let outcome = match update_result {
            Ok(o) => o,
            Err(e) => return Ok(storage_err_reply(e)),
        };
        if outcome.matched == 0 {
            // Lost the race — the doc no longer satisfies the query. Re-pick.
            repick_count += 1;
            continue;
        }

        let value = if return_new {
            match outcome.post_image {
                // The post-image the write itself produced, captured under the
                // storage lock — never a re-`find` a concurrent writer can beat.
                Some(post) => project_value(post, fields, &query)?,
                None => Bson::Null,
            }
        } else {
            project_value(matched_doc, fields, &query)?
        };
        return Ok(doc! {
            "lastErrorObject": { "n": 1, "updatedExisting": true },
            "value": value,
            "ok": 1.0,
        });
    }
}

// --- helpers -------------------------------------------------------------

fn decode(bytes: &[u8]) -> Result<Document, CommandError> {
    Document::from_reader(&mut &bytes[..])
        .map_err(|e| CommandError::new(1, "InternalError", format!("decode: {e}")))
}

/// Apply the optional projection to a value document, returning it as `Bson`.
fn project_value(
    value: Document,
    fields: Option<&Document>,
    query: &Document,
) -> Result<Bson, CommandError> {
    let q = if query.is_empty() { None } else { Some(query) };
    match fields {
        Some(spec) if !spec.is_empty() => {
            secantus_core::projection::apply_projection(&value, spec, q)
                .map(Bson::Document)
                .map_err(|_| {
                    CommandError::new(
                        2,
                        "BadValue",
                        "projection is not supported by the Rust server",
                    )
                })
        }
        _ => Ok(Bson::Document(value)),
    }
}

/// Shape a storage error into an `ok: 0` reply (findAndModify is single-doc, so
/// errors are command-level, not per-op `writeErrors`). Preserves the E11000
/// `keyPattern` / `keyValue`.
fn storage_err_reply(e: StorageError) -> Document {
    match e {
        StorageError::DuplicateKey(info) => {
            let mut r = doc! {
                "ok": 0.0, "errmsg": info.errmsg, "code": 11000, "codeName": "DuplicateKey",
            };
            if let Some(kp) = info.key_pattern {
                r.insert("keyPattern", kp);
            }
            if let Some(kv) = info.key_value {
                r.insert("keyValue", kv);
            }
            r
        }
        other => command_error(other).into_reply(),
    }
}
