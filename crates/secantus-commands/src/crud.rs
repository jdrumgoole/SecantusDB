//! The CRUD write/count command family: `insert`, `delete`, `count`.
//!
//! Faithful ports of `commands.py::_insert` / `_delete` / `_count`, scoped to
//! the paths that map onto the current `secantus-storage` signatures. `update`
//! is the next slice (its pipeline-form `u`, `arrayFilters`, `let`, `collation`,
//! and `validator` need storage-signature work); `find` lands with the cursor
//! registry (R3) + projection.
//!
//! **Deferred (documented so parity is honest):**
//! * `writeConcern` validation and `writeConcernError` attachment (cross-cutting,
//!   lands with the write-concern slice).
//! * Collection `validator` / `bypassDocumentValidation` (needs
//!   `get_collection_options` + the query engine on the write path).
//! * `_reject_oplog_rs_write` (writes to `local.oplog.rs`).
//! * `let` / `collation` on `delete` (the Rust `delete_matching` takes neither
//!   yet) and view-collection `count` (needs the aggregation engine).

use bson::{doc, Bson, Document};

use crate::util::{as_i64, bool_field, coll_arg, command_error, doc_field, write_error};
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
    for (index, spec) in deletes.iter().enumerate() {
        let Bson::Document(spec) = spec else { continue };
        let filter = doc_field(spec, "q");
        // `limit: 0` ⇒ delete all matches; any positive value ⇒ at most that
        // many (mongod only defines 0 and 1, but we honour the integer).
        let limit = spec.get("limit").and_then(as_i64).unwrap_or(0).max(0) as usize;
        match storage.delete_matching(&ctx.db_name, &coll, &filter, limit) {
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

    let mut n = storage
        .count_matching(&ctx.db_name, &coll, &filter)
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
    }

    impl FakeStorage {
        fn arc() -> Arc<FakeStorage> {
            Arc::new(FakeStorage::default())
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
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _update: &Document,
            _multi: bool,
            _upsert: bool,
        ) -> Result<UpdateOutcome, StorageError> {
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
}
