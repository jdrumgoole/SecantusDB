//! Collection/index DDL + introspection commands: `create` / `drop` /
//! `listCollections` / `listIndexes` / `createIndexes` / `dropIndexes`.
//!
//! Ports of the corresponding `commands.py` handlers, scoped to the core paths.
//!
//! **Deferred (documented so parity is honest):**
//! * `create` option validation (unknown-field `Location40415`, `capped`,
//!   `validator`, views) — only the bare create is ported.
//! * `listCollections` name/filter + the richer per-collection `options` /
//!   `idIndex` detail (a minimal, faithful-enough entry is returned).
//! * `listIndexes` `NamespaceNotFound` on a missing collection (returns an empty
//!   cursor instead).
//! * `dropIndexes` by key-spec document (only by name / `"*"`).
//! * `writeConcern`, `_reject_oplog_rs_write`.

use bson::{doc, Bson, Document};

use crate::find::split_into_cursor;
use crate::util::{coll_arg, command_error, docs_to_bson, encode_docs};
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE};

/// `create` — create a collection.
pub fn create(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "create")?;
    let storage = ctx.storage()?;
    let created = storage
        .create_collection(&ctx.db_name, &coll)
        .map_err(command_error)?;
    if !created {
        return Ok(CommandError::new(
            48,
            "NamespaceExists",
            format!("a collection '{}.{}' already exists", ctx.db_name, coll),
        )
        .into_reply());
    }
    Ok(doc! { "ok": 1.0 })
}

/// `drop` — drop a collection.
pub fn drop(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "drop")?;
    let storage = ctx.storage()?;
    let existed = storage
        .drop_collection(&ctx.db_name, &coll)
        .map_err(command_error)?;
    if !existed {
        return Ok(CommandError::new(26, "NamespaceNotFound", "ns not found").into_reply());
    }
    Ok(doc! { "ns": format!("{}.{}", ctx.db_name, coll), "nIndexesWas": 1, "ok": 1.0 })
}

/// `listCollections` — a cursor over the collections in the database.
pub fn list_collections(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let names = storage
        .list_collections(&ctx.db_name)
        .map_err(command_error)?;
    let entries: Vec<Document> = names
        .iter()
        .map(|n| {
            doc! {
                "name": n,
                "type": "collection",
                "options": {},
                "info": { "readOnly": false },
                "idIndex": { "v": 2, "key": { "_id": 1 }, "name": "_id_" },
            }
        })
        .collect();
    let ns = format!("{}.$cmd.listCollections", ctx.db_name);
    let (first, cid) = split_into_cursor(
        encode_docs(entries)?,
        DEFAULT_BATCH_SIZE as i64,
        &ns,
        cursors,
    )?;
    Ok(doc! {
        "cursor": { "id": Bson::Int64(cid), "ns": ns, "firstBatch": docs_to_bson(first)? },
        "ok": 1.0,
    })
}

/// `listIndexes` — a cursor over the indexes of a collection.
pub fn list_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "listIndexes")?;
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let indexes = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let ns = format!("{}.$cmd.listIndexes.{}", ctx.db_name, coll);
    let (first, cid) = split_into_cursor(
        encode_docs(indexes)?,
        DEFAULT_BATCH_SIZE as i64,
        &ns,
        cursors,
    )?;
    Ok(doc! {
        "cursor": { "id": Bson::Int64(cid), "ns": ns, "firstBatch": docs_to_bson(first)? },
        "ok": 1.0,
    })
}

/// `createIndexes` — create one or more indexes (auto-creating the collection).
pub fn create_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "createIndexes")?;
    let storage = ctx.storage()?;
    let specs: Vec<Bson> = match doc.get("indexes") {
        Some(Bson::Array(a)) => a.clone(),
        _ => Vec::new(),
    };

    let before = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .len();
    // createIndexes implicitly creates the collection if absent.
    let created_coll = storage
        .create_collection(&ctx.db_name, &coll)
        .map_err(command_error)?;

    for spec in &specs {
        let Bson::Document(s) = spec else { continue };
        let key = s
            .get("key")
            .and_then(Bson::as_document)
            .cloned()
            .unwrap_or_default();
        let name = s
            .get("name")
            .and_then(Bson::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| default_index_name(&key));
        storage
            .create_index(&ctx.db_name, &coll, &name, &key, s)
            .map_err(command_error)?;
    }

    let after = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .len();
    Ok(doc! {
        "createdCollectionAutomatically": created_coll,
        "numIndexesBefore": before as i32,
        "numIndexesAfter": after as i32,
        "ok": 1.0,
    })
}

/// `dropIndexes` — drop a named index, or all of them with `"*"`.
pub fn drop_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "dropIndexes")?;
    let storage = ctx.storage()?;
    let before = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .len();

    match doc.get("index") {
        Some(Bson::String(s)) if s == "*" => {
            storage
                .drop_all_indexes(&ctx.db_name, &coll)
                .map_err(command_error)?;
        }
        Some(Bson::String(name)) => {
            let existed = storage
                .drop_index(&ctx.db_name, &coll, name)
                .map_err(command_error)?;
            if !existed {
                return Ok(CommandError::new(
                    27,
                    "IndexNotFound",
                    format!("index not found with name [{name}]"),
                )
                .into_reply());
            }
        }
        Some(Bson::Document(_)) => {
            return Err(CommandError::new(
                1,
                "InternalError",
                "dropIndexes by key spec is not yet supported by the Rust server",
            ));
        }
        _ => {
            return Ok(CommandError::new(
                2,
                "BadValue",
                "dropIndexes requires an index name or '*'",
            )
            .into_reply())
        }
    }
    Ok(doc! { "nIndexesWas": before as i32, "ok": 1.0 })
}

/// The default index name mongod derives from a key spec, e.g. `{a:1, b:-1}` →
/// `"a_1_b_-1"`, `{loc:"2dsphere"}` → `"loc_2dsphere"`.
fn default_index_name(key: &Document) -> String {
    key.iter()
        .map(|(k, v)| {
            let vs = match v {
                Bson::Int32(i) => i.to_string(),
                Bson::Int64(i) => i.to_string(),
                Bson::Double(d) => (*d as i64).to_string(),
                Bson::String(s) => s.clone(),
                _ => "1".to_string(),
            };
            format!("{k}_{vs}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use crate::storage::{RawHint, Storage, StorageError, UpdateOutcome};
    use crate::CursorRegistry;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// An in-memory storage tracking collections + per-collection index names.
    #[derive(Default)]
    struct FakeStorage {
        // db -> set of collection names
        cols: Mutex<HashMap<String, Vec<String>>>,
        // (db, coll) -> index names (excluding the implicit _id_)
        idx: Mutex<HashMap<(String, String), Vec<String>>>,
    }

    impl FakeStorage {
        fn arc() -> Arc<FakeStorage> {
            Arc::new(FakeStorage::default())
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
            _: &str,
            _: &str,
            _: &Document,
            _: Option<&Document>,
            _: Option<RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, StorageError> {
            Ok(vec![])
        }

        fn list_collections(&self, db: &str) -> Result<Vec<String>, StorageError> {
            Ok(self
                .cols
                .lock()
                .unwrap()
                .get(db)
                .cloned()
                .unwrap_or_default())
        }
        fn create_collection(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
            let mut cols = self.cols.lock().unwrap();
            let v = cols.entry(db.to_string()).or_default();
            if v.iter().any(|c| c == coll) {
                Ok(false)
            } else {
                v.push(coll.to_string());
                Ok(true)
            }
        }
        fn drop_collection(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
            let mut cols = self.cols.lock().unwrap();
            let v = cols.entry(db.to_string()).or_default();
            let before = v.len();
            v.retain(|c| c != coll);
            Ok(v.len() != before)
        }
        fn list_indexes(&self, db: &str, coll: &str) -> Result<Vec<Document>, StorageError> {
            // Only report indexes for an existing collection (its implicit _id_
            // plus any created secondary indexes).
            let exists = self
                .cols
                .lock()
                .unwrap()
                .get(db)
                .map(|v| v.iter().any(|c| c == coll))
                .unwrap_or(false);
            if !exists {
                return Ok(vec![]);
            }
            let mut out = vec![doc! {"v": 2, "key": {"_id": 1}, "name": "_id_"}];
            if let Some(names) = self.idx.lock().unwrap().get(&(db.into(), coll.into())) {
                for n in names {
                    out.push(doc! {"v": 2, "key": {n.clone(): 1}, "name": n.clone()});
                }
            }
            Ok(out)
        }
        fn create_index(
            &self,
            db: &str,
            coll: &str,
            name: &str,
            _key: &Document,
            _options: &Document,
        ) -> Result<bool, StorageError> {
            let mut idx = self.idx.lock().unwrap();
            let v = idx.entry((db.into(), coll.into())).or_default();
            if v.iter().any(|n| n == name) {
                Ok(false)
            } else {
                v.push(name.to_string());
                Ok(true)
            }
        }
        fn drop_index(&self, db: &str, coll: &str, name: &str) -> Result<bool, StorageError> {
            let mut idx = self.idx.lock().unwrap();
            let v = idx.entry((db.into(), coll.into())).or_default();
            let before = v.len();
            v.retain(|n| n != name);
            Ok(v.len() != before)
        }
        fn drop_all_indexes(&self, db: &str, coll: &str) -> Result<usize, StorageError> {
            let mut idx = self.idx.lock().unwrap();
            let v = idx.entry((db.into(), coll.into())).or_default();
            let n = v.len();
            v.clear();
            Ok(n)
        }
    }

    fn ctx(storage: Arc<FakeStorage>) -> CommandContext {
        let mut c = CommandContext::new(1)
            .with_storage(storage)
            .with_cursors(Arc::new(CursorRegistry::new()));
        c.db_name = "t".into();
        c
    }

    #[test]
    fn create_then_drop_collection() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        assert_eq!(
            dispatch(&doc! {"create": "c"}, &mut c)
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        // re-create ⇒ NamespaceExists
        let mut c = ctx(s.clone());
        let reply = dispatch(&doc! {"create": "c"}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 48);

        let mut c = ctx(s.clone());
        let reply = dispatch(&doc! {"drop": "c"}, &mut c);
        assert_eq!(reply.get_str("ns").unwrap(), "t.c");
        // drop again ⇒ NamespaceNotFound
        let mut c = ctx(s);
        assert_eq!(
            dispatch(&doc! {"drop": "c"}, &mut c)
                .get_i32("code")
                .unwrap(),
            26
        );
    }

    #[test]
    fn list_collections_returns_created() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(&doc! {"create": "a"}, &mut c);
        let mut c = ctx(s.clone());
        dispatch(&doc! {"create": "b"}, &mut c);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"listCollections": 1}, &mut c);
        let names: Vec<String> = reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .iter()
            .map(|b| {
                b.as_document()
                    .unwrap()
                    .get_str("name")
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn create_indexes_and_list() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        let reply = dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"a": 1}, "name": "a_1"},
                {"key": {"b": -1}},  // name auto-derived ⇒ b_-1
            ]},
            &mut c,
        );
        assert!(reply.get_bool("createdCollectionAutomatically").unwrap());
        assert_eq!(reply.get_i32("numIndexesBefore").unwrap(), 0);
        // _id_ + a_1 + b_-1
        assert_eq!(reply.get_i32("numIndexesAfter").unwrap(), 3);

        let mut c = ctx(s);
        let reply = dispatch(&doc! {"listIndexes": "c"}, &mut c);
        let names: Vec<String> = reply
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .iter()
            .map(|b| {
                b.as_document()
                    .unwrap()
                    .get_str("name")
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["_id_", "a_1", "b_-1"]);
    }

    #[test]
    fn drop_indexes_by_name_and_star() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(
            &doc! {"createIndexes": "c", "indexes": [
                {"key": {"a": 1}, "name": "a_1"}, {"key": {"b": 1}, "name": "b_1"}
            ]},
            &mut c,
        );
        let mut c = ctx(s.clone());
        let reply = dispatch(&doc! {"dropIndexes": "c", "index": "a_1"}, &mut c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        // unknown index ⇒ IndexNotFound
        let mut c = ctx(s.clone());
        assert_eq!(
            dispatch(&doc! {"dropIndexes": "c", "index": "zzz"}, &mut c)
                .get_i32("code")
                .unwrap(),
            27
        );
        // "*" drops the rest
        let mut c = ctx(s.clone());
        dispatch(&doc! {"dropIndexes": "c", "index": "*"}, &mut c);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"listIndexes": "c"}, &mut c);
        // only _id_ remains
        assert_eq!(
            reply
                .get_document("cursor")
                .unwrap()
                .get_array("firstBatch")
                .unwrap()
                .len(),
            1
        );
    }
}
