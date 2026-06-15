//! The `findAndModify` command — atomically find one document and update/remove
//! it, returning the pre- or post-image.
//!
//! A port of `commands.py::_find_and_modify`, composed at the command layer from
//! the existing `Storage` trait methods (`find` limit-1 + sort, then
//! `update_matching` / `delete_matching`, then a re-`find` for the new image).
//!
//! **Caveat — not atomic across calls.** The Python server uses storage-internal
//! atomicity; here the find + modify are separate storage calls, so a concurrent
//! writer could interleave. Acceptable for the single-node test surrogate;
//! tracked for a future find-and-modify storage primitive.
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

    // Collection validator on the post-apply doc (code 121) unless
    // `bypassDocumentValidation` / `validationAction: warn|off`, mirroring the
    // `update` handler. Threaded into the update via `update_matching_array_filters`.
    // The `findAndModify` command is unusual in that the canonical wire field is
    // the snake_case `bypass_document_validation` (what pymongo's
    // `find_one_and_*` helpers emit); accept the camelCase spelling too for
    // raw-command callers.
    let bypass = bool_field(doc, "bypassDocumentValidation", false)
        || bool_field(doc, "bypass_document_validation", false);
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
            let upd = update
                .and_then(Bson::as_document)
                .cloned()
                .unwrap_or_default();
            let outcome = match storage.update_matching_array_filters(
                &ctx.db_name,
                &coll,
                &query,
                &upd,
                false,
                true,
                &[],
                &let_vars,
                collation.as_ref(),
                validator.as_ref(),
            ) {
                Ok(o) => o,
                Err(e) => return Ok(storage_err_reply(e)),
            };
            let upserted_id = outcome.upserted_id.unwrap_or(Bson::Null);
            let value = if return_new && upserted_id != Bson::Null {
                fetch_projected(storage, &ctx.db_name, &coll, &upserted_id, fields)?
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
    let id_filter = doc! { "_id": matched_id.clone() };

    // remove=true: delete the matched doc, return its (pre-image) value.
    if is_remove {
        if let Err(e) = storage.delete_matching(&ctx.db_name, &coll, &id_filter, 1) {
            return Ok(storage_err_reply(e));
        }
        let value = project_value(matched_doc, fields)?;
        return Ok(doc! {
            "lastErrorObject": { "n": 1, "updatedExisting": true },
            "value": value,
            "ok": 1.0,
        });
    }

    // update: apply it to the matched doc.
    let upd = update
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default();
    if let Err(e) = storage.update_matching_array_filters(
        &ctx.db_name,
        &coll,
        &id_filter,
        &upd,
        false,
        false,
        &[],
        &let_vars,
        collation.as_ref(),
        validator.as_ref(),
    ) {
        return Ok(storage_err_reply(e));
    }

    let value = if return_new {
        fetch_projected(storage, &ctx.db_name, &coll, &matched_id, fields)?
    } else {
        project_value(matched_doc, fields)?
    };
    Ok(doc! {
        "lastErrorObject": { "n": 1, "updatedExisting": true },
        "value": value,
        "ok": 1.0,
    })
}

// --- helpers -------------------------------------------------------------

fn decode(bytes: &[u8]) -> Result<Document, CommandError> {
    Document::from_reader(&mut &bytes[..])
        .map_err(|e| CommandError::new(1, "InternalError", format!("decode: {e}")))
}

/// Re-fetch the doc with `_id == id` and apply the projection — used for the
/// post-image (`new: true`) and the upsert result.
fn fetch_projected(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
    id: &Bson,
    fields: Option<&Document>,
) -> Result<Bson, CommandError> {
    let filter = doc! { "_id": id.clone() };
    let found = storage
        .find(db, coll, &filter, None, None)
        .map_err(command_error)?;
    match found.into_iter().next() {
        Some(b) => project_value(decode(&b)?, fields),
        None => Ok(Bson::Null),
    }
}

/// Apply the optional projection to a value document, returning it as `Bson`.
fn project_value(value: Document, fields: Option<&Document>) -> Result<Bson, CommandError> {
    match fields {
        Some(spec) if !spec.is_empty() => secantus_core::projection::apply_projection(&value, spec)
            .map(Bson::Document)
            .map_err(|_| {
                CommandError::new(
                    2,
                    "BadValue",
                    "projection is not supported by the Rust server",
                )
            }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use crate::storage::{RawHint, Storage, UpdateOutcome};
    use std::sync::{Arc, Mutex};

    fn matches(d: &Document, filter: &Document) -> bool {
        filter.iter().all(|(k, v)| d.get(k) == Some(v))
    }

    #[derive(Default)]
    struct FakeStorage {
        docs: Mutex<Vec<Document>>,
        next_id: Mutex<i32>,
    }

    impl FakeStorage {
        fn with(docs: Vec<Document>) -> Arc<FakeStorage> {
            Arc::new(FakeStorage {
                docs: Mutex::new(docs),
                next_id: Mutex::new(100),
            })
        }
    }

    impl Storage for FakeStorage {
        fn insert(
            &self,
            _: &str,
            _: &str,
            _: Vec<Vec<u8>>,
            _: bool,
        ) -> Result<(usize, Vec<Document>), StorageError> {
            Ok((0, vec![]))
        }
        fn update_matching(
            &self,
            _: &str,
            _: &str,
            filter: &Document,
            update: &Document,
            _multi: bool,
            upsert: bool,
        ) -> Result<UpdateOutcome, StorageError> {
            let mut docs = self.docs.lock().unwrap();
            let set = update.get("$set").and_then(Bson::as_document).cloned();
            let mut matched = 0;
            let mut modified = 0;
            for d in docs.iter_mut() {
                if matches(d, filter) {
                    matched += 1;
                    if let Some(set) = &set {
                        for (k, v) in set.iter() {
                            d.insert(k.clone(), v.clone());
                        }
                        modified += 1;
                    }
                    break;
                }
            }
            let upserted_id = if matched == 0 && upsert {
                let id = filter.get("_id").cloned().unwrap_or_else(|| {
                    let mut n = self.next_id.lock().unwrap();
                    *n += 1;
                    Bson::Int32(*n)
                });
                let mut new_doc = doc! { "_id": id.clone() };
                for (k, v) in filter.iter() {
                    if k != "_id" {
                        new_doc.insert(k.clone(), v.clone());
                    }
                }
                if let Some(set) = &set {
                    for (k, v) in set.iter() {
                        new_doc.insert(k.clone(), v.clone());
                    }
                }
                docs.push(new_doc);
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
        fn delete_matching(
            &self,
            _: &str,
            _: &str,
            filter: &Document,
            limit: usize,
        ) -> Result<usize, StorageError> {
            let mut docs = self.docs.lock().unwrap();
            let mut removed = 0;
            let mut i = 0;
            while i < docs.len() {
                if matches(&docs[i], filter) {
                    docs.remove(i);
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
        fn count_matching(&self, _: &str, _: &str, _: &Document) -> Result<usize, StorageError> {
            Ok(0)
        }
        fn find(
            &self,
            _: &str,
            _: &str,
            filter: &Document,
            sort: Option<&Document>,
            _: Option<RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, StorageError> {
            let docs = self.docs.lock().unwrap();
            let mut out: Vec<Document> = docs
                .iter()
                .filter(|d| matches(d, filter))
                .cloned()
                .collect();
            if let Some(s) = sort {
                if let Some((field, dir)) = s.iter().next() {
                    out.sort_by_key(|d| match d.get(field) {
                        Some(Bson::Int32(i)) => *i as i64,
                        Some(Bson::Int64(i)) => *i,
                        _ => 0,
                    });
                    if dir.as_i32().unwrap_or(1) < 0 {
                        out.reverse();
                    }
                }
            }
            Ok(out
                .iter()
                .map(|d| {
                    let mut v = Vec::new();
                    d.to_writer(&mut v).unwrap();
                    v
                })
                .collect())
        }
    }

    fn ctx(storage: Arc<FakeStorage>) -> CommandContext {
        let mut c = CommandContext::new(1).with_storage(storage);
        c.db_name = "t".into();
        c
    }

    #[test]
    fn update_returns_old_by_default() {
        let s = FakeStorage::with(vec![doc! {"_id": 1, "x": 1}]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"x": 9}}},
            &mut c,
        );
        let value = reply.get_document("value").unwrap();
        assert_eq!(value.get_i32("x").unwrap(), 1, "old image");
        let leo = reply.get_document("lastErrorObject").unwrap();
        assert_eq!(leo.get_i32("n").unwrap(), 1);
        assert!(leo.get_bool("updatedExisting").unwrap());
    }

    #[test]
    fn update_returns_new_when_requested() {
        let s = FakeStorage::with(vec![doc! {"_id": 1, "x": 1}]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"x": 9}}, "new": true},
            &mut c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("x").unwrap(),
            9,
            "new image"
        );
    }

    #[test]
    fn remove_returns_deleted_doc() {
        let s = FakeStorage::with(vec![doc! {"_id": 1, "x": 1}, doc! {"_id": 2, "x": 2}]);
        let mut c = ctx(s.clone());
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 1}, "remove": true},
            &mut c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("_id").unwrap(),
            1
        );
        assert_eq!(s.docs.lock().unwrap().len(), 1, "doc removed");
    }

    #[test]
    fn no_match_returns_null() {
        let s = FakeStorage::with(vec![]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 9}, "update": {"$set": {"x": 1}}},
            &mut c,
        );
        assert_eq!(reply.get("value"), Some(&Bson::Null));
        assert_eq!(
            reply
                .get_document("lastErrorObject")
                .unwrap()
                .get_i32("n")
                .unwrap(),
            0
        );
    }

    #[test]
    fn upsert_inserts_and_reports_upserted() {
        let s = FakeStorage::with(vec![]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"_id": 5}, "update": {"$set": {"x": 1}}, "upsert": true, "new": true},
            &mut c,
        );
        let leo = reply.get_document("lastErrorObject").unwrap();
        assert_eq!(leo.get_i32("upserted").unwrap(), 5);
        assert!(!leo.get_bool("updatedExisting").unwrap());
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("x").unwrap(),
            1
        );
    }

    #[test]
    fn sort_picks_first() {
        let s = FakeStorage::with(vec![
            doc! {"_id": 1, "g": "a", "p": 3},
            doc! {"_id": 2, "g": "a", "p": 1},
            doc! {"_id": 3, "g": "a", "p": 2},
        ]);
        let mut c = ctx(s);
        // sort by p asc ⇒ _id 2 is the target
        let reply = dispatch(
            &doc! {"findAndModify": "c", "query": {"g": "a"}, "sort": {"p": 1}, "remove": true},
            &mut c,
        );
        assert_eq!(
            reply.get_document("value").unwrap().get_i32("_id").unwrap(),
            2
        );
    }

    #[test]
    fn remove_and_update_together_is_failed_to_parse() {
        let s = FakeStorage::with(vec![]);
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"findAndModify": "c", "remove": true, "update": {"$set": {"x": 1}}},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 9);
        assert_eq!(reply.get_str("codeName").unwrap(), "FailedToParse");
    }
}
