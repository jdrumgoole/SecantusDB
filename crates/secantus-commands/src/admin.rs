//! Collection/index DDL + introspection + db-admin commands: `create` /
//! `collMod` / `explain` / `drop` / `listCollections` / `listIndexes` /
//! `createIndexes` / `dropIndexes` / `dropDatabase` / `renameCollection` /
//! `collStats` / `dbStats` / `serverStatus`.
//!
//! Ports of the corresponding `commands.py` handlers, scoped to the core paths.
//!
//! `create` persists recognised options (`validator` / `validationLevel` /
//! `validationAction` / `changeStreamPreAndPostImages` / `capped` / `size` /
//! `max`); `collMod` merges the same set into an existing collection (else
//! `NamespaceNotFound`). The `insert` handler enforces `validator` (code 121).
//!
//! **Deferred (documented so parity is honest):**
//! * `create` unknown-field validation (`Location40415`); views; capped-size
//!   enforcement; `collMod`'s TTL-index `index: {expireAfterSeconds}` modify.
//! * `validator` enforcement on `update` / replace (needs the post-apply doc in
//!   storage — insert is enforced at the command layer).
//! * `listCollections` name/filter + the richer per-collection `options` /
//!   `idIndex` detail (a minimal, faithful-enough entry is returned).
//! * `listIndexes` `NamespaceNotFound` on a missing collection (returns an empty
//!   cursor instead).
//! * `dropIndexes` by key-spec document (only by name / `"*"`).
//! * `serverStatus` reports a minimal subset; `collStats` / `dbStats` use
//!   `dataSize` for `storageSize` (no separate on-disk accounting).
//! * `writeConcern`, `_reject_oplog_rs_write`.

use bson::{doc, Bson, Document};

use crate::find::split_into_cursor;
use crate::util::{as_i64, coll_arg, collation_of, command_error, docs_to_bson, encode_docs};
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE, SERVER_VERSION};

/// Collection-option keys (from `create` / `collMod`) the Rust server persists.
/// `validator` + `validationLevel`/`validationAction` drive document validation;
/// `changeStreamPreAndPostImages` drives pre-image capture; `capped`/`size`/`max`
/// are reported in stats. (TTL-index `expireAfterSeconds` modification via
/// `collMod`'s `index` option is deferred.)
const STORED_COLL_OPTIONS: [&str; 7] = [
    "validator",
    "validationLevel",
    "validationAction",
    "changeStreamPreAndPostImages",
    "capped",
    "size",
    "max",
];

/// The subset of a command doc that maps to persisted collection options.
fn collection_option_subset(doc: &Document) -> Document {
    let mut out = Document::new();
    for k in STORED_COLL_OPTIONS {
        if let Some(v) = doc.get(k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    out
}

/// `create` — create a collection, persisting recognised options.
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
    let opts = collection_option_subset(doc);
    if !opts.is_empty() {
        storage
            .set_collection_options(&ctx.db_name, &coll, &opts)
            .map_err(command_error)?;
    }
    Ok(doc! { "ok": 1.0 })
}

/// `collMod` — modify a collection's options (`validator` / `validationLevel` /
/// `validationAction` / `changeStreamPreAndPostImages`). Merges the recognised
/// options into the collection's stored blob. Errors `NamespaceNotFound` (26)
/// when the collection doesn't exist. (TTL-index `index` modification deferred.)
pub fn coll_mod(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = match doc.get("collMod").or_else(|| doc.get("collmod")) {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "collMod requires a string collection name",
            ))
        }
    };
    let storage = ctx.storage()?;
    let exists = storage
        .list_collections(&ctx.db_name)
        .map_err(command_error)?
        .iter()
        .any(|c| c == &coll);
    if !exists {
        return Ok(CommandError::new(
            26,
            "NamespaceNotFound",
            format!("ns does not exist: {}.{}", ctx.db_name, coll),
        )
        .into_reply());
    }
    let opts = collection_option_subset(doc);
    storage
        .set_collection_options(&ctx.db_name, &coll, &opts)
        .map_err(command_error)?;
    Ok(doc! { "ok": 1.0 })
}

/// `explain` — report the query plan (and, above `queryPlanner` verbosity,
/// execution counts) for a wrapped `find` / `aggregate` / `count` command. Ports
/// `commands.py::_explain`'s core: lifts a leading `$match` for aggregate, rejects
/// a journaled / `w:"majority"` writeConcern (72), validates `verbosity` (2),
/// shapes `queryPlanner.winningPlan` (`FETCH`+`IXSCAN` or `COLLSCAN`) and an
/// `executionStats` block (run via `find` to count). aggregate adds the
/// `stages: [{$cursor: …}, …]` wrapper drivers look for. Collation forces COLLSCAN.
pub fn explain(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let inner = doc
        .get("explain")
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default();
    let cmd_name = inner.keys().next().cloned().unwrap_or_default();
    let coll = match inner.get(&cmd_name) {
        Some(Bson::String(s)) => s.clone(),
        _ => String::new(),
    };
    let mut filter = inner
        .get("filter")
        .or_else(|| inner.get("query"))
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default();
    let sort = inner.get("sort").and_then(Bson::as_document);
    let hint = inner.get("hint");
    let collation = collation_of(&inner);
    // Aggregate lifts a leading $match into the fetch — explain reports the same.
    if cmd_name == "aggregate" && filter.is_empty() {
        if let Some(Bson::Array(p)) = inner.get("pipeline") {
            if let Some(Bson::Document(first)) = p.first() {
                if let Some(Bson::Document(m)) = first.get("$match") {
                    filter = m.clone();
                }
            }
        }
    }
    // explain + a journaled / majority writeConcern is ill-formed (InvalidOptions).
    for wc in [doc.get("writeConcern"), inner.get("writeConcern")] {
        if let Some(Bson::Document(wc)) = wc {
            let journaled = matches!(
                wc.get("j"),
                Some(Bson::Boolean(true)) | Some(Bson::Int32(1))
            );
            if journaled || wc.get_str("w").ok() == Some("majority") {
                return Ok(CommandError::new(
                    72,
                    "InvalidOptions",
                    "Command does not support writeConcern when used with explain",
                )
                .into_reply());
            }
        }
    }
    let verbosity = doc.get_str("verbosity").unwrap_or("executionStats");
    if !["queryPlanner", "executionStats", "allPlansExecution"].contains(&verbosity) {
        return Ok(CommandError::new(
            2,
            "BadValue",
            format!("verbosity {verbosity:?} not recognized"),
        )
        .into_reply());
    }

    let storage = ctx.storage()?;
    let ns = if coll.is_empty() {
        format!("{}.$cmd", ctx.db_name)
    } else {
        format!("{}.{}", ctx.db_name, coll)
    };
    // A collation (or no collection) forces COLLSCAN — the byte-sortable indexes
    // are collation-naive (mirrors `find`'s COLLSCAN-forcing under collation).
    let plan = if coll.is_empty() || collation.is_some() {
        let mut d = Document::new();
        d.insert("kind", "COLLSCAN");
        d
    } else {
        storage
            .explain_plan(&ctx.db_name, &coll, &filter, sort, hint)
            .map_err(command_error)?
    };
    let is_ixscan = plan.get_str("kind").ok() == Some("IXSCAN");

    let (mut n_returned, mut docs_examined, mut keys_examined) = (0i64, 0i64, 0i64);
    if verbosity != "queryPlanner" && !coll.is_empty() {
        let res = storage
            .find_collated(
                &ctx.db_name,
                &coll,
                &filter,
                sort,
                hint,
                collation.as_ref(),
                &Document::new(),
            )
            .map_err(command_error)?;
        n_returned = res.len() as i64;
        if is_ixscan {
            keys_examined = n_returned;
            docs_examined = n_returned;
        } else {
            docs_examined = storage
                .count_collated(&ctx.db_name, &coll, &Document::new(), None)
                .map_err(command_error)? as i64;
        }
    }

    let winning_plan = if is_ixscan {
        doc! {
            "stage": "FETCH",
            "filter": filter.clone(),
            "inputStage": {
                "stage": "IXSCAN",
                "indexName": plan.get_str("indexName").unwrap_or(""),
                "keyPattern": plan.get_document("keyPattern").cloned().unwrap_or_default(),
                "direction": plan.get_str("direction").unwrap_or("forward"),
            },
        }
    } else {
        doc! { "stage": "COLLSCAN", "filter": filter.clone() }
    };
    let query_planner = doc! {
        "namespace": &ns,
        "indexFilterSet": false,
        "parsedQuery": filter.clone(),
        "winningPlan": winning_plan,
        "rejectedPlans": [],
    };
    let execution_stages = if is_ixscan {
        doc! {"stage": "FETCH", "nReturned": n_returned, "inputStage": {"stage": "IXSCAN", "nReturned": n_returned}}
    } else {
        doc! {"stage": "COLLSCAN", "nReturned": n_returned}
    };
    let exec_stats = doc! {
        "executionSuccess": true,
        "nReturned": n_returned,
        "executionTimeMillis": 0_i64,
        "totalKeysExamined": keys_examined,
        "totalDocsExamined": docs_examined,
        "executionStages": execution_stages,
    };
    let server_info = doc! {
        "host": "secantus", "port": 0_i32, "version": SERVER_VERSION, "gitVersion": "0".repeat(40),
    };

    let mut reply = Document::new();
    if cmd_name == "aggregate" {
        let mut cursor = doc! { "queryPlanner": query_planner.clone() };
        if verbosity != "queryPlanner" {
            cursor.insert("executionStats", exec_stats.clone());
        }
        let mut stages = vec![Bson::Document(doc! { "$cursor": cursor })];
        if let Some(Bson::Array(p)) = inner.get("pipeline") {
            for s in p {
                stages.push(s.clone());
            }
        }
        reply.insert("stages", stages);
    }
    reply.insert("queryPlanner", query_planner);
    if verbosity != "queryPlanner" {
        reply.insert("executionStats", exec_stats);
    }
    reply.insert("command", inner);
    reply.insert("serverInfo", server_info);
    reply.insert("ok", 1.0);
    Ok(reply)
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

/// `dropDatabase` — drop the current database.
pub fn drop_database(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    storage.drop_database(&ctx.db_name).map_err(command_error)?;
    Ok(doc! { "dropped": ctx.db_name.clone(), "ok": 1.0 })
}

/// `renameCollection` — rename `renameCollection` (a full `db.coll` ns) to `to`.
pub fn rename_collection(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let src = match doc.get("renameCollection") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "renameCollection requires a string source namespace",
            ))
        }
    };
    let to = match doc.get("to") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Ok(CommandError::new(
                2,
                "BadValue",
                "renameCollection requires a string 'to' namespace",
            )
            .into_reply())
        }
    };
    let drop_target = doc
        .get("dropTarget")
        .and_then(Bson::as_bool)
        .unwrap_or(false);
    let (src_db, src_coll) = split_ns(&src);
    let (dst_db, dst_coll) = split_ns(&to);

    let storage = ctx.storage()?;
    let (ok_, msg) = storage
        .rename_collection(&src_db, &src_coll, &dst_db, &dst_coll, drop_target)
        .map_err(command_error)?;
    if !ok_ {
        let m = msg.unwrap_or_else(|| "rename failed".to_string());
        let (code, name) = if m.to_lowercase().contains("exist") {
            (48, "NamespaceExists")
        } else {
            (26, "NamespaceNotFound")
        };
        return Ok(CommandError::new(code, name, m).into_reply());
    }
    Ok(doc! { "ok": 1.0 })
}

/// `collStats` — per-collection size / count / index statistics.
pub fn coll_stats(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "collStats")?;
    let storage = ctx.storage()?;
    let count = storage
        .count_matching(&ctx.db_name, &coll, &Document::new())
        .map_err(command_error)? as i64;
    let size = storage
        .collection_data_size(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let index_sizes = storage
        .index_sizes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let capped = storage
        .collection_is_capped(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let total_index_size: i64 = index_sizes.values().filter_map(as_i64).sum();
    let avg_obj_size = if count > 0 { size / count } else { 0 };
    Ok(doc! {
        "ns": format!("{}.{}", ctx.db_name, coll),
        "count": count as i32,
        "size": size,
        "avgObjSize": avg_obj_size,
        "storageSize": size,
        "nindexes": index_sizes.len() as i32,
        "totalIndexSize": total_index_size,
        "indexSizes": index_sizes,
        "capped": capped,
        "ok": 1.0,
    })
}

/// `dbStats` — database-wide totals aggregated across collections.
pub fn db_stats(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let colls = storage
        .list_collections(&ctx.db_name)
        .map_err(command_error)?;
    let mut objects = 0i64;
    let mut data_size = 0i64;
    let mut indexes = 0i64;
    let mut index_size = 0i64;
    for c in &colls {
        objects += storage
            .count_matching(&ctx.db_name, c, &Document::new())
            .map_err(command_error)? as i64;
        data_size += storage
            .collection_data_size(&ctx.db_name, c)
            .map_err(command_error)?;
        let isz = storage
            .index_sizes(&ctx.db_name, c)
            .map_err(command_error)?;
        indexes += isz.len() as i64;
        index_size += isz.values().filter_map(as_i64).sum::<i64>();
    }
    Ok(doc! {
        "db": ctx.db_name.clone(),
        "collections": colls.len() as i32,
        "objects": objects,
        "dataSize": data_size,
        "storageSize": data_size,
        "indexes": indexes,
        "indexSize": index_size,
        "ok": 1.0,
    })
}

/// `serverStatus` — a minimal subset (host / version / process / uptime).
pub fn server_status(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! {
        "host": "secantus",
        "version": crate::SERVER_VERSION,
        "process": "mongod",
        "pid": Bson::Int64(0),
        "uptime": 0.0,
        "uptimeMillis": Bson::Int64(0),
        "localTime": bson::DateTime::now(),
        // Categorical self-identification: real mongod never has this key.
        // Tooling (the conformance-gauge tripwire, ad-hoc smoke scripts)
        // checks it to prove it's talking to SecantusDB rather than an
        // accidental real MongoDB on the same address. The Python server
        // reports `server: "python"`.
        "secantus": { "server": "rust", "version": env!("CARGO_PKG_VERSION") },
        "ok": 1.0,
    })
}

/// Split a `db.coll` namespace into `(db, coll)`.
fn split_ns(ns: &str) -> (String, String) {
    match ns.split_once('.') {
        Some((d, c)) => (d.to_string(), c.to_string()),
        None => (String::new(), ns.to_string()),
    }
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
        // (db, coll) -> stored options blob
        opts: Mutex<HashMap<(String, String), Document>>,
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
        fn get_collection_options(&self, db: &str, coll: &str) -> Result<Document, StorageError> {
            Ok(self
                .opts
                .lock()
                .unwrap()
                .get(&(db.to_string(), coll.to_string()))
                .cloned()
                .unwrap_or_default())
        }
        fn set_collection_options(
            &self,
            db: &str,
            coll: &str,
            opts: &Document,
        ) -> Result<(), StorageError> {
            let mut store = self.opts.lock().unwrap();
            let cur = store.entry((db.to_string(), coll.to_string())).or_default();
            for (k, v) in opts {
                cur.insert(k.clone(), v.clone());
            }
            Ok(())
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
    fn create_stores_validator() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        let reply = dispatch(
            &doc! {"create": "c", "validator": {"a": {"$exists": true}}},
            &mut c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let stored = s
            .opts
            .lock()
            .unwrap()
            .get(&("t".to_string(), "c".to_string()))
            .cloned()
            .unwrap();
        assert!(stored.contains_key("validator"));
    }

    #[test]
    fn collmod_sets_validator() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(&doc! {"create": "c"}, &mut c);
        let mut c = ctx(s.clone());
        let reply = dispatch(
            &doc! {"collMod": "c", "validator": {"n": {"$gt": 0}},
            "changeStreamPreAndPostImages": {"enabled": true}},
            &mut c,
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let stored = s
            .opts
            .lock()
            .unwrap()
            .get(&("t".to_string(), "c".to_string()))
            .cloned()
            .unwrap();
        assert!(stored.contains_key("validator"));
        assert!(stored.contains_key("changeStreamPreAndPostImages"));
    }

    #[test]
    fn collmod_missing_ns_is_namespace_not_found() {
        let s = FakeStorage::arc();
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"collMod": "nope", "validator": {}}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 26);
        assert_eq!(reply.get_str("codeName").unwrap(), "NamespaceNotFound");
    }

    #[test]
    fn explain_find_collscan_shape() {
        let s = FakeStorage::arc();
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"explain": {"find": "c", "filter": {"x": 1}}}, &mut c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let qp = reply.get_document("queryPlanner").unwrap();
        assert_eq!(qp.get_str("namespace").unwrap(), "t.c");
        let wp = qp.get_document("winningPlan").unwrap();
        assert_eq!(wp.get_str("stage").unwrap(), "COLLSCAN");
        // default verbosity (executionStats) includes the stats block.
        assert!(reply.get_document("executionStats").is_ok());
    }

    #[test]
    fn explain_query_planner_verbosity_omits_exec_stats() {
        let s = FakeStorage::arc();
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"explain": {"find": "c"}, "verbosity": "queryPlanner"},
            &mut c,
        );
        assert!(reply.get_document("queryPlanner").is_ok());
        assert!(reply.get("executionStats").is_none());
    }

    #[test]
    fn explain_invalid_verbosity_is_bad_value() {
        let s = FakeStorage::arc();
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"explain": {"find": "c"}, "verbosity": "bogus"},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 2);
        assert_eq!(reply.get_str("codeName").unwrap(), "BadValue");
    }

    #[test]
    fn explain_with_majority_write_concern_rejected() {
        let s = FakeStorage::arc();
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"explain": {"find": "c"}, "writeConcern": {"w": "majority"}},
            &mut c,
        );
        assert_eq!(reply.get_i32("code").unwrap(), 72);
        assert_eq!(reply.get_str("codeName").unwrap(), "InvalidOptions");
    }

    #[test]
    fn explain_aggregate_has_cursor_stages() {
        let s = FakeStorage::arc();
        let mut c = ctx(s);
        let reply = dispatch(
            &doc! {"explain": {"aggregate": "c", "pipeline": [{"$match": {"x": 1}}]}},
            &mut c,
        );
        let stages = reply.get_array("stages").unwrap();
        assert!(stages[0].as_document().unwrap().contains_key("$cursor"));
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

    #[test]
    fn server_status_minimal_shape() {
        let mut c = ctx(FakeStorage::arc());
        let reply = dispatch(&doc! {"serverStatus": 1}, &mut c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(reply.get_str("version").unwrap(), crate::SERVER_VERSION);
        assert_eq!(reply.get_str("process").unwrap(), "mongod");
        // The categorical SecantusDB marker — real mongod never has it.
        let marker = reply.get_document("secantus").unwrap();
        assert_eq!(marker.get_str("server").unwrap(), "rust");
        assert!(!marker.get_str("version").unwrap().is_empty());
    }

    #[test]
    fn drop_database_reports_dropped() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(&doc! {"create": "c"}, &mut c);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"dropDatabase": 1}, &mut c);
        assert_eq!(reply.get_str("dropped").unwrap(), "t");
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    }

    #[test]
    fn db_stats_counts_collections() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(&doc! {"create": "a"}, &mut c);
        let mut c = ctx(s.clone());
        dispatch(&doc! {"create": "b"}, &mut c);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"dbStats": 1}, &mut c);
        assert_eq!(reply.get_str("db").unwrap(), "t");
        assert_eq!(reply.get_i32("collections").unwrap(), 2);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    }

    #[test]
    fn coll_stats_shape() {
        let s = FakeStorage::arc();
        let mut c = ctx(s.clone());
        dispatch(&doc! {"create": "c"}, &mut c);
        let mut c = ctx(s);
        let reply = dispatch(&doc! {"collStats": "c"}, &mut c);
        assert_eq!(reply.get_str("ns").unwrap(), "t.c");
        assert!(reply.get("indexSizes").is_some());
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    }

    #[test]
    fn rename_collection_default_ok() {
        // The fake uses the trait default (succeeds); validates dispatch + ns parse.
        let mut c = ctx(FakeStorage::arc());
        let reply = dispatch(&doc! {"renameCollection": "t.a", "to": "t.b"}, &mut c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    }
}
