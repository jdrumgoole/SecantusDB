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
//! 2. Run the remaining pipeline via `apply_pipeline`.
//! 3. Split the result into `firstBatch` + a cursor (`cursor.batchSize`).
//!
//! **Deferred (documented so parity is honest):**
//! * **Storage-backed stages** — `$lookup` / `$out` / `$merge` / `$geoNear` /
//!   `$sample` / `$collStats` / `$indexStats` — the Rust engine returns
//!   `Fallback` for these (they need storage threaded into the pipeline); the
//!   handler surfaces that as a `BadValue`. They land when the pipeline engine
//!   gets a storage context.
//! * **`$changeStream`** — change streams aren't wired into the Rust server yet
//!   (the standalone-rejection, code 40573, is still honoured).
//! * **`let` expression evaluation** (the raw `let` doc is threaded as vars; `$$`
//!   expressions in `let` values aren't pre-evaluated) and **`collation`**.

use bson::{doc, Bson, Document};

use crate::find::split_into_cursor;
use crate::util::{as_i64, command_error, decode_docs, docs_to_bson, encode_docs};
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE};

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
    let vars: Document = doc
        .get("let")
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default();

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
                    .find(&ctx.db_name, c, &filter, None, hint)
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

    let result = secantus_core::aggregate::apply_pipeline(input, &working_pipeline, &vars, None)
        .map_err(|_| {
            CommandError::new(
                2,
                "BadValue",
                "aggregation pipeline uses a stage or operator not supported by the Rust server",
            )
        })?;

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
            _: &Document,
            _: &Document,
            _: bool,
            _: bool,
        ) -> Result<UpdateOutcome, StorageError> {
            Ok(UpdateOutcome::default())
        }
        fn delete_matching(
            &self,
            _: &str,
            _: &str,
            _: &Document,
            _: usize,
        ) -> Result<usize, StorageError> {
            Ok(0)
        }
        fn count_matching(&self, _: &str, _: &str, _: &Document) -> Result<usize, StorageError> {
            Ok(0)
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
        // $lookup is storage-backed → the Rust engine returns Fallback.
        let reply = dispatch(
            &doc! {"aggregate": "c", "pipeline": [
                {"$lookup": {"from": "o", "localField": "_id", "foreignField": "_id", "as": "j"}}
            ], "cursor": {}},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 2);
        assert_eq!(reply.get_str("codeName").unwrap(), "BadValue");
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
