//! The `find` command — the keystone read path, and the producer of the cursors
//! the registry (R3) manages.
//!
//! A port of `commands.py::_find`'s non-tailable path. The Rust storage's
//! `find` returns the full ordered match set (it does not take skip / limit /
//! projection), so this handler does them: fetch (sorted, optionally hinted) →
//! `skip` → `limit` → `projection` (via `secantus_core::projection`) →
//! `_split_into_cursor` (firstBatch + register the remainder).
//!
//! **Deferred (documented so parity is honest):**
//! * The up-front empty-collection filter validation (`matches({}, filter)`):
//!   needs the query engine to distinguish a *parse* error from its `Fallback`
//!   "defer" signal. Without it, an invalid filter on an *empty* collection
//!   returns an empty cursor instead of a `BadValue` (non-empty collections
//!   still surface the error through the storage scan).
//! * `tailable: true` (capped-collection poll) — needs the tailable cursor
//!   machinery + `collection_is_capped`; rejected here as unsupported (capped
//!   collections aren't creatable through the ported handlers yet anyway).
//! * `let` IS applied — command `let` vars (`$$NOW` + evaluated values) are
//!   visible to `$expr` in the filter, threaded through `find_collated`.
//!   `collation` IS applied — filter matching + sort order are collation-aware
//!   (COLLSCAN-forced); a non-ASCII / numericOrdering collation → `BadValue`.

use bson::{doc, Bson, Document};

use crate::cursors::CursorRegistry;
use crate::util::{
    as_i64, bool_field, coll_arg, collation_of, command_error, doc_field, docs_to_bson,
    resolve_let_vars,
};
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE};

/// `find` — run a query and open a cursor over the results.
pub fn find(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "find")?;
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;

    let filter = doc_field(doc, "filter");
    let skip = doc.get("skip").and_then(as_i64).unwrap_or(0).max(0) as usize;
    let limit = doc.get("limit").and_then(as_i64).unwrap_or(0).max(0) as usize; // 0 ⇒ no limit
    let sort = doc.get("sort").and_then(Bson::as_document);
    // An empty projection means "no projection" (return full docs).
    let projection = doc
        .get("projection")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());
    let hint = doc.get("hint");
    let collation = collation_of(doc);
    // Command `let` → vars visible to `$expr` in the filter.
    let let_vars = resolve_let_vars(doc.get("let"));
    // `batchSize` is tri-state: absent ⇒ default, 0 ⇒ empty firstBatch + cursor,
    // explicit positive ⇒ that size.
    let batch_size = match doc.get("batchSize") {
        Some(b) => as_i64(b).unwrap_or(DEFAULT_BATCH_SIZE as i64),
        None => DEFAULT_BATCH_SIZE as i64,
    };
    let single_batch = bool_field(doc, "singleBatch", false);
    let ns = format!("{}.{}", ctx.db_name, coll);

    if bool_field(doc, "tailable", false) {
        return Err(CommandError::new(
            1,
            "InternalError",
            "tailable find is not yet supported by the Rust server",
        ));
    }

    let mut docs = storage
        .find_collated(
            &ctx.db_name,
            &coll,
            &filter,
            sort,
            hint,
            collation.as_ref(),
            &let_vars,
        )
        .map_err(command_error)?;

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

    if let Some(spec) = projection {
        docs = project_docs(docs, spec)?;
    }

    let (first_batch, cursor_id) = if single_batch {
        (docs, 0)
    } else {
        split_into_cursor(docs, batch_size, &ns, cursors)?
    };

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
) -> Result<(Vec<Vec<u8>>, i64), CommandError> {
    let take = if batch_size < 0 {
        DEFAULT_BATCH_SIZE as usize
    } else {
        batch_size as usize
    }
    .min(docs.len());
    let remaining = docs.split_off(take);
    if remaining.is_empty() {
        return Ok((docs, 0));
    }
    let cursor_id = cursors
        .register(ns, remaining)
        .map_err(|e| CommandError::new(1, "InternalError", format!("cursor registry: {e:?}")))?;
    Ok((docs, cursor_id))
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

/// Compare `doc`'s key tuple against `bound` over the bound's fields, in the
/// index's per-field direction. `None` ⇒ the doc is missing a bound field (it
/// has no index key, so it's outside any bound). `Some(Ordering)` is the tuple
/// comparison (`Equal` when the doc matches the bound on every bound field).
fn bound_cmp(
    doc: &Document,
    bound: &Document,
    key_spec: &Document,
) -> Result<Option<std::cmp::Ordering>, CommandError> {
    use std::cmp::Ordering;
    for (field, bval) in bound {
        let dir = match key_spec.get(field) {
            Some(Bson::Int32(n)) => *n,
            Some(Bson::Int64(n)) => *n as i32,
            Some(Bson::Double(d)) => *d as i32,
            _ => 1,
        };
        let Some(dval) = secantus_core::get_path(doc, field) else {
            return Ok(None);
        };
        let enc = |v: &Bson| {
            secantus_core::sortkey::encode_value_directed(v, dir, None)
                .map_err(|_| CommandError::new(2, "BadValue", "min/max value not comparable"))
        };
        match enc(dval)?.cmp(&enc(bval)?) {
            Ordering::Equal => continue,
            other => return Ok(Some(other)),
        }
    }
    Ok(Some(Ordering::Equal))
}

/// Filter `docs` (already index-ordered) by the `min` (inclusive) / `max`
/// (exclusive) index bounds. Mirrors `storage._apply_minmax_bounds`.
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
    let hint = hint.ok_or_else(|| {
        CommandError::new(
            51173,
            "Location51173",
            "When using min()/max() a hint of which index to use must be provided",
        )
    })?;
    let key_spec = resolve_index_key(storage, db, coll, hint)?;
    // Each bound's fields must be a leading prefix of the hinted index's key,
    // in the same order — else mongod rejects with 51174 (the "wrong order"
    // case in pymongo's test_min/test_max).
    let index_fields: Vec<&String> = key_spec.keys().collect();
    for bound in [min, max].into_iter().flatten() {
        let bf: Vec<&String> = bound.keys().collect();
        let prefix_match =
            bf.len() <= index_fields.len() && (0..bf.len()).all(|i| bf[i] == index_fields[i]);
        if !prefix_match {
            return Err(CommandError::new(
                51174,
                "Location51174",
                "The field order of the min/max query option does not match the order of the \
                 hinted index's key pattern",
            ));
        }
    }
    let mut out = Vec::with_capacity(docs.len());
    for bytes in docs {
        let d = Document::from_reader(&mut bytes.as_slice()).map_err(|e| {
            CommandError::new(
                1,
                "InternalError",
                format!("failed to decode document: {e}"),
            )
        })?;
        // min is inclusive (doc >= min ⇒ cmp != Less); max is exclusive (doc < max).
        let keep_min = match min {
            Some(m) => matches!(bound_cmp(&d, m, &key_spec)?, Some(o) if o != Ordering::Less),
            None => true,
        };
        let keep_max = match max {
            Some(m) => bound_cmp(&d, m, &key_spec)? == Some(Ordering::Less),
            None => true,
        };
        if keep_min && keep_max {
            out.push(bytes);
        }
    }
    Ok(out)
}

/// Apply a projection spec to each result doc via `secantus_core::projection`.
/// A `Fallback` (a projection the Rust engine can't reproduce exactly) surfaces
/// as `BadValue` — the Rust server only ships what the Rust engine supports.
fn project_docs(docs: Vec<Vec<u8>>, spec: &Document) -> Result<Vec<Vec<u8>>, CommandError> {
    docs.iter()
        .map(|bytes| {
            let d = Document::from_reader(&mut bytes.as_slice()).map_err(|e| {
                CommandError::new(
                    1,
                    "InternalError",
                    format!("failed to decode document: {e}"),
                )
            })?;
            let projected =
                secantus_core::projection::apply_projection(&d, spec).map_err(|_| {
                    CommandError::new(
                        2,
                        "BadValue",
                        "projection is not supported by the Rust server",
                    )
                })?;
            let mut out = Vec::new();
            projected.to_writer(&mut out).map_err(|e| {
                CommandError::new(
                    1,
                    "InternalError",
                    format!("failed to encode document: {e}"),
                )
            })?;
            Ok(out)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use crate::storage::{RawHint, Storage, StorageError, UpdateOutcome};
    use crate::CursorRegistry;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn enc(d: &Document) -> Vec<u8> {
        let mut v = Vec::new();
        d.to_writer(&mut v).unwrap();
        v
    }

    fn matches(d: &Document, filter: &Document) -> bool {
        filter.iter().all(|(k, v)| d.get(k) == Some(v))
    }

    /// In-memory storage whose `find` supports the empty / simple-equality
    /// filter and a single numeric-field sort, enough to exercise the handler's
    /// skip / limit / projection / cursor-split plumbing.
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
            _db: &str,
            _coll: &str,
            _docs: Vec<Vec<u8>>,
            _ordered: bool,
        ) -> Result<(usize, Vec<Document>), StorageError> {
            Ok((0, vec![]))
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
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _limit: usize,
        ) -> Result<usize, StorageError> {
            Ok(0)
        }
        fn count_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
        ) -> Result<usize, StorageError> {
            Ok(0)
        }
        fn find(
            &self,
            db: &str,
            coll: &str,
            filter: &Document,
            sort: Option<&Document>,
            _hint: Option<RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, StorageError> {
            let cols = self.cols.lock().unwrap();
            let mut out: Vec<Document> = cols
                .get(&(db.to_string(), coll.to_string()))
                .map(|b| b.iter().filter(|d| matches(d, filter)).cloned().collect())
                .unwrap_or_default();
            if let Some(sort) = sort {
                if let Some((field, dir)) = sort.iter().next() {
                    let dir = dir.as_i32().unwrap_or(1);
                    out.sort_by_key(|d| d.get(field).and_then(|v| v.as_i64()).unwrap_or(0));
                    if dir < 0 {
                        out.reverse();
                    }
                }
            }
            Ok(out.iter().map(enc).collect())
        }
    }

    fn ctx(storage: Arc<FakeStorage>) -> CommandContext {
        CommandContext::new(1)
            .with_storage(storage)
            .with_cursors(Arc::new(CursorRegistry::new()))
    }

    fn batch_ids(cursor: &Document, key: &str) -> Vec<i64> {
        cursor
            .get_array(key)
            .unwrap()
            .iter()
            .map(|b| b.as_document().unwrap().get_i32("_id").unwrap() as i64)
            .collect()
    }

    #[test]
    fn find_all_single_batch() {
        let docs = (0..3).map(|i| doc! {"_id": i}).collect();
        let mut c = ctx(FakeStorage::seed("t", "c", docs));
        c.db_name = "t".into();
        let reply = dispatch(&doc! {"find": "c"}, &mut c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(cur.get_i64("id").unwrap(), 0, "all fit ⇒ no cursor");
        assert_eq!(cur.get_str("ns").unwrap(), "t.c");
        assert_eq!(batch_ids(cur, "firstBatch"), vec![0, 1, 2]);
    }

    #[test]
    fn find_skip_and_limit() {
        let docs = (0..5).map(|i| doc! {"_id": i}).collect();
        let mut c = ctx(FakeStorage::seed("t", "c", docs));
        c.db_name = "t".into();
        let reply = dispatch(&doc! {"find": "c", "skip": 1, "limit": 2}, &mut c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "firstBatch"), vec![1, 2]);
    }

    #[test]
    fn find_sort_descending() {
        let docs = (0..3).map(|i| doc! {"_id": i}).collect();
        let mut c = ctx(FakeStorage::seed("t", "c", docs));
        c.db_name = "t".into();
        let reply = dispatch(&doc! {"find": "c", "sort": {"_id": -1}}, &mut c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "firstBatch"), vec![2, 1, 0]);
    }

    #[test]
    fn find_batched_opens_cursor_and_getmore_drains() {
        let docs = (0..5).map(|i| doc! {"_id": i}).collect();
        let mut c = ctx(FakeStorage::seed("t", "c", docs));
        c.db_name = "t".into();
        let reply = dispatch(&doc! {"find": "c", "batchSize": 2}, &mut c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "firstBatch"), vec![0, 1]);
        let cid = cur.get_i64("id").unwrap();
        assert_ne!(cid, 0, "remaining docs ⇒ live cursor");

        // getMore against the same context drains the rest.
        let reply = dispatch(
            &doc! {"getMore": cid, "collection": "c", "batchSize": 2},
            &mut c,
        );
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "nextBatch"), vec![2, 3]);
        assert_eq!(cur.get_i64("id").unwrap(), cid);

        let reply = dispatch(
            &doc! {"getMore": cid, "collection": "c", "batchSize": 2},
            &mut c,
        );
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "nextBatch"), vec![4]);
        assert_eq!(cur.get_i64("id").unwrap(), 0, "exhausted");
    }

    #[test]
    fn find_batch_size_zero_empty_first_batch() {
        let docs = (0..2).map(|i| doc! {"_id": i}).collect();
        let mut c = ctx(FakeStorage::seed("t", "c", docs));
        c.db_name = "t".into();
        let reply = dispatch(&doc! {"find": "c", "batchSize": 0}, &mut c);
        let cur = reply.get_document("cursor").unwrap();
        assert!(cur.get_array("firstBatch").unwrap().is_empty());
        assert_ne!(cur.get_i64("id").unwrap(), 0);
    }

    #[test]
    fn find_single_batch_never_opens_cursor() {
        let docs = (0..5).map(|i| doc! {"_id": i}).collect();
        let mut c = ctx(FakeStorage::seed("t", "c", docs));
        c.db_name = "t".into();
        let reply = dispatch(
            &doc! {"find": "c", "batchSize": 2, "singleBatch": true},
            &mut c,
        );
        let cur = reply.get_document("cursor").unwrap();
        // singleBatch overrides batchSize splitting: all docs, id 0.
        assert_eq!(batch_ids(cur, "firstBatch").len(), 5);
        assert_eq!(cur.get_i64("id").unwrap(), 0);
    }

    #[test]
    fn find_projection_includes_fields() {
        let docs = vec![doc! {"_id": 1, "a": 10, "b": 20}];
        let mut c = ctx(FakeStorage::seed("t", "c", docs));
        c.db_name = "t".into();
        let reply = dispatch(&doc! {"find": "c", "projection": {"a": 1}}, &mut c);
        let cur = reply.get_document("cursor").unwrap();
        let first = cur.get_array("firstBatch").unwrap()[0]
            .as_document()
            .unwrap();
        assert!(first.get("a").is_some());
        assert!(first.get("b").is_none(), "b excluded by projection");
        assert!(first.get("_id").is_some(), "_id included by default");
    }

    #[test]
    fn find_filter_matches_subset() {
        let docs = vec![
            doc! {"_id": 1, "x": 1},
            doc! {"_id": 2, "x": 2},
            doc! {"_id": 3, "x": 1},
        ];
        let mut c = ctx(FakeStorage::seed("t", "c", docs));
        c.db_name = "t".into();
        let reply = dispatch(&doc! {"find": "c", "filter": {"x": 1}}, &mut c);
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(batch_ids(cur, "firstBatch"), vec![1, 3]);
    }
}
