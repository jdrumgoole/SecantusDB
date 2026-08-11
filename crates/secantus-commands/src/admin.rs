//! Collection/index DDL + introspection + db-admin commands: `create` /
//! `collMod` / `explain` / `drop` / `listCollections` / `listIndexes` /
//! `createIndexes` / `dropIndexes` / `dropDatabase` / `renameCollection` /
//! `collStats` / `dbStats` / `serverStatus` / `validate` / `profile`.
//!
//! Ports of the corresponding `commands.py` handlers, scoped to the core paths.
//!
//! `create` persists recognised options (`validator` / `validationLevel` /
//! `validationAction` / `changeStreamPreAndPostImages` / `capped` / `size` /
//! `max`); `collMod` merges the same set into an existing collection (else
//! `NamespaceNotFound`). The `insert` handler enforces `validator` (code 121).
//!
//! `create` with `viewOn` + `pipeline` registers a read-only view; `count`
//! resolves through the view's pipeline and `listCollections` reports it as
//! `type: "view"` (readOnly, no `_id` index).
//!
//! **Deferred (documented so parity is honest):**
//! * `create` unknown-field validation (`Location40415`); capped-size
//!   enforcement; `collMod`'s TTL-index `index: {expireAfterSeconds}` modify.
//! * `validator` enforcement on `findAndModify` (insert / `update` / replace are
//!   enforced — the command layer reads the validator and the storage update
//!   checks the post-apply doc; code 121, bypassable via
//!   `bypassDocumentValidation`).
//! * `listIndexes` `NamespaceNotFound` on a missing collection (returns an empty
//!   cursor instead).
//! * `dropIndexes` by key-spec document (only by name / `"*"`).
//! * `serverStatus` reports a minimal subset; `collStats` / `dbStats` use
//!   `dataSize` for `storageSize` (no separate on-disk accounting).
//! * `writeConcern`, `_reject_oplog_rs_write`.

use bson::{doc, Bson, Document};

use crate::find::split_into_cursor;
use crate::util::{
    as_i64, bool_field, coll_arg, collation_of, command_error, docs_to_bson, encode_docs,
};
use crate::{CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE, SERVER_VERSION};

/// Collection-option keys (from `create` / `collMod`) the Rust server persists.
/// `validator` + `validationLevel`/`validationAction` drive document validation;
/// `changeStreamPreAndPostImages` drives pre-image capture; `capped`/`size`/`max`
/// are reported in stats. (TTL-index `expireAfterSeconds` modification via
/// `collMod`'s `index` option is deferred.)
const STORED_COLL_OPTIONS: [&str; 11] = [
    "validator",
    "validationLevel",
    "validationAction",
    "changeStreamPreAndPostImages",
    "capped",
    "size",
    "max",
    // Persisted so the storage layer can recognise a timeseries collection and
    // relax `_id` uniqueness (mongod buckets by time; `_id` is not a key).
    "timeseries",
    // The collection's default collation — surfaced back in `listCollections`.
    "collation",
    // Round-tripped in `listCollections` so drivers see the options they set on
    // `create` (the rust driver's collection_management asserts both).
    "storageEngine",
    "indexOptionDefaults",
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
    if let Some(unknown) = first_unknown_field(doc, CREATE_KNOWN_OPTIONS) {
        return Ok(CommandError::new(
            40415,
            "Location40415",
            format!("BSON field 'create.{unknown}' is an unknown field"),
        )
        .into_reply());
    }
    // Build the options up front so they ride the `create` oplog entry (carried
    // by create_collection_with_options) — that's what lets PITR replay
    // reconstruct capped / validator / … rather than seeing a bare create.
    let mut opts = collection_option_subset(doc);
    // `viewOn` + `pipeline` makes this a read-only view of another collection
    // (mongod 3.4+). Store the source and the pipeline (under `viewPipeline` so
    // it doesn't collide with an aggregate's `pipeline`); `listCollections`
    // surfaces it as `type: "view"` and `count` resolves through it.
    if let Some(Bson::String(view_on)) = doc.get("viewOn") {
        opts.insert("viewOn", view_on.clone());
        let pipeline = doc
            .get("pipeline")
            .and_then(Bson::as_array)
            .cloned()
            .unwrap_or_default();
        opts.insert("viewPipeline", Bson::Array(pipeline));
    }
    // `clusteredIndex` clusters the collection on `_id` — which is already
    // SecantusDB's doc-table layout (keyed by `_id`), so this is metadata-only.
    // mongod allows it only on `{_id: 1}` with `unique: true`; normalise the
    // stored option (default name `_id_`, add `v: 2`) so listCollections /
    // listIndexes echo mongod's shape. Built before create so an invalid spec
    // rejects without leaving a half-created collection. Mirrors commands.py.
    if let Some(ci) = doc.get("clusteredIndex").and_then(Bson::as_document) {
        let key_ok = ci.get_document("key").is_ok_and(|k| {
            k.len() == 1
                && k.get("_id").is_some_and(|v| {
                    matches!(v, Bson::Int32(1) | Bson::Int64(1)) || v.as_f64() == Some(1.0)
                })
        });
        if !key_ok {
            return Ok(CommandError::new(
                197,
                "InvalidIndexSpecificationOption",
                "The clusteredIndex option is only supported for key: {_id: 1}",
            )
            .into_reply());
        }
        if ci.get_bool("unique") != Ok(true) {
            return Ok(CommandError::new(
                5979700,
                "Location5979700",
                "The clusteredIndex option requires unique: true to be specified",
            )
            .into_reply());
        }
        let name = ci.get_str("name").unwrap_or("_id_").to_string();
        opts.insert(
            "clusteredIndex",
            doc! { "v": 2i32, "key": { "_id": 1i32 }, "name": name, "unique": true },
        );
    }
    let storage = ctx.storage()?;
    let created = storage
        .create_collection_with_options(&ctx.db_name, &coll, &opts)
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
    let mut reply = doc! { "ok": 1.0 };
    // Index modification: `collMod {index: {keyPattern|name, prepareUnique|unique|expireAfterSeconds}}`.
    if let Some(Bson::Document(index_spec)) = doc.get("index") {
        let indexes = storage
            .list_indexes(&ctx.db_name, &coll)
            .map_err(command_error)?;
        // Resolve the target index by name or key pattern.
        let target = if let Ok(want) = index_spec.get_str("name") {
            indexes
                .iter()
                .find(|ix| ix.get_str("name") == Ok(want))
                .cloned()
        } else if let Some(Bson::Document(want_key)) = index_spec.get("keyPattern") {
            indexes
                .iter()
                .find(|ix| {
                    ix.get_document("key")
                        .map(|k| key_patterns_eq(k, want_key))
                        .unwrap_or(false)
                })
                .cloned()
        } else {
            None
        };
        let Some(target) = target else {
            return Ok(CommandError::new(
                27,
                "IndexNotFound",
                format!("cannot find index for ns {}.{}", ctx.db_name, coll),
            )
            .into_reply());
        };
        let target_name = target.get_str("name").unwrap_or("").to_string();
        // `expireAfterSeconds` retunes a TTL index: echo the old/new expiry and
        // persist the new one. Mirrors commands.py::_coll_mod.
        if let Some(new_expiry) = index_spec.get("expireAfterSeconds") {
            reply.insert(
                "expireAfterSeconds_old",
                target
                    .get("expireAfterSeconds")
                    .cloned()
                    .unwrap_or(Bson::Null),
            );
            reply.insert("expireAfterSeconds_new", new_expiry.clone());
            storage
                .set_index_options(
                    &ctx.db_name,
                    &coll,
                    &target_name,
                    &doc! {"expireAfterSeconds": new_expiry.clone()},
                )
                .map_err(command_error)?;
        }
        // `prepareUnique` arms the index: new dup writes are rejected (11000)
        // while pre-existing duplicates are tolerated — the staging step before
        // a `unique: true` conversion.
        if let Some(prep) = index_spec.get("prepareUnique").and_then(Bson::as_bool) {
            storage
                .set_index_options(
                    &ctx.db_name,
                    &coll,
                    &target_name,
                    &doc! {"prepareUnique": prep},
                )
                .map_err(command_error)?;
        }
        // `unique: true` converts the index; if any docs already share a key the
        // conversion is refused with 359 and the offending `_id` groups reported
        // as `violations`. Mirrors commands.py::_coll_mod.
        if index_spec.get("unique").and_then(Bson::as_bool) == Some(true)
            && !target.get_bool("unique").unwrap_or(false)
        {
            let dups = storage
                .find_index_duplicates(&ctx.db_name, &coll, &target_name)
                .map_err(command_error)?;
            if !dups.is_empty() {
                let violations: Vec<Bson> = dups
                    .into_iter()
                    .map(|ids| Bson::Document(doc! {"ids": ids}))
                    .collect();
                let mut reply = CommandError::new(
                    359,
                    "CannotConvertIndexToUnique",
                    format!("Cannot convert index {target_name} to unique: found duplicate values"),
                )
                .into_reply();
                reply.insert("violations", violations);
                return Ok(reply);
            }
            storage
                .set_index_options(
                    &ctx.db_name,
                    &coll,
                    &target_name,
                    &doc! {"unique": true, "prepareUnique": false},
                )
                .map_err(command_error)?;
        }
    }
    let opts = collection_option_subset(doc);
    // `coll_mod` (not `set_collection_options`) so a `showExpandedEvents` change
    // stream sees the resulting `modify` event.
    storage
        .coll_mod(&ctx.db_name, &coll, &opts)
        .map_err(command_error)?;
    Ok(reply)
}

/// Whether two index key patterns are equal (same fields, same order, same
/// `±1` direction regardless of the numeric BSON type they're encoded as).
fn key_patterns_eq(a: &Document, b: &Document) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ak, av), (bk, bv))| ak == bk && as_i64(av) == as_i64(bv))
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
        let index_name = plan.get_str("indexName").unwrap_or("");
        let mut input_stage = doc! {
            "stage": "IXSCAN",
            "indexName": index_name,
            "keyPattern": plan.get_document("keyPattern").cloned().unwrap_or_default(),
            "direction": plan.get_str("direction").unwrap_or("forward"),
            // mongod always reports whether the scanned index is multikey;
            // planners (Compass, aggregation optimisers) read it to decide
            // what the index can be trusted for.
            "isMultiKey": plan.get_bool("multikey").unwrap_or(false),
        };
        // mongod flags an IXSCAN over a partial index with `isPartial`.
        if !coll.is_empty() {
            let is_partial = storage
                .list_indexes(&ctx.db_name, &coll)
                .map(|ixs| {
                    ixs.iter().any(|ix| {
                        ix.get_str("name").ok() == Some(index_name)
                            && ix.contains_key("partialFilterExpression")
                    })
                })
                .unwrap_or(false);
            if is_partial {
                input_stage.insert("isPartial", true);
            }
        }
        doc! {
            "stage": "FETCH",
            "filter": filter.clone(),
            "inputStage": input_stage,
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
    let mut exec_stats = doc! {
        "executionSuccess": true,
        "nReturned": n_returned,
        "executionTimeMillis": 0_i64,
        "totalKeysExamined": keys_examined,
        "totalDocsExamined": docs_examined,
        "executionStages": execution_stages,
    };
    // `allPlansExecution` verbosity adds per-candidate-plan stats under
    // executionStats. With a single solution (no multi-planning; rejectedPlans
    // is always empty) mongod emits an empty array — drivers' explain helpers
    // (mongo-php-library `ExplainFunctionalTest`) assert the key's presence.
    if verbosity == "allPlansExecution" {
        exec_stats.insert("allPlansExecution", Bson::Array(vec![]));
    }
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
    let ns = format!("{}.{}", ctx.db_name, coll);
    let storage = ctx.storage()?;
    // Kill the collection's cursors BEFORE the storage drop, not after: the
    // drop emits an oplog entry that wakes any awaitData getMore parked on a
    // tailable cursor, and that getMore must observe the tombstone set here so
    // it reports "collection dropped" instead of re-polling a collection that
    // is already gone. Non-tailable cursors are removed outright, so a later
    // getMore is CursorNotFound (mongo-c-driver's error_document/getmore).
    // Mirrors commands.py::_drop.
    if let Ok(cursors) = ctx.cursors() {
        cursors.kill_namespace(&ns);
    }
    let existed = storage
        .drop_collection(&ctx.db_name, &coll)
        .map_err(command_error)?;
    if !existed {
        // Modern mongod treats `drop` of a non-existent collection as an
        // idempotent success (`{ok: 1}`), not a NamespaceNotFound error. The
        // ok:1 shape also lets dispatch attach a `writeConcernError` for an
        // unsatisfiable write concern — pymongo's test_drop_collection drops an
        // already-absent collection with w:50 and asserts a WriteConcernError.
        // Mirrors commands.py::_drop.
        return Ok(doc! { "ok": 1.0 });
    }
    Ok(doc! { "ns": format!("{}.{}", ctx.db_name, coll), "nIndexesWas": 1, "ok": 1.0 })
}

/// `secantusAdmin.backupArchive` — force a checkpoint and tar the WiredTiger home
/// into `outputPath` (a server-side path) for point-in-time recovery. The on-disk
/// and oplog formats match the Python server, so either server's restore tooling
/// reads the result. Mirrors the Python `secantusAdmin.backupArchive` command.
pub fn backup_archive(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let output_path = doc.get_str("outputPath").unwrap_or("");
    if output_path.is_empty() {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "secantusAdmin.backupArchive requires outputPath: <string>",
        ));
    }
    let storage = ctx.storage()?;
    let (path, size_bytes) = storage.create_archive(output_path).map_err(command_error)?;
    Ok(doc! { "path": path, "sizeBytes": size_bytes as i64, "ok": 1.0 })
}

/// `secantusAdmin.pruneOplog` — drop oplog rows past the retention window now,
/// returning `{pruned, ok}`. An operator-driven immediate sweep (the storage
/// engine also prunes opportunistically on every emit). Mirrors the Python
/// `secantusAdmin.pruneOplog` command.
pub fn prune_oplog(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let pruned = storage.prune_oplog().map_err(command_error)?;
    Ok(doc! { "pruned": pruned as i64, "ok": 1.0 })
}

/// `secantusAdmin.pruneTtl` — run TTL pruning across every collection now,
/// returning `{pruned, ok}` (the docs deleted). Lets callers force a
/// deterministic pass instead of waiting for the background cadence. Mirrors
/// the Python `secantusAdmin.pruneTtl` command.
pub fn prune_ttl(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let pruned = storage.prune_ttl_all().map_err(command_error)?;
    Ok(doc! { "pruned": pruned as i64, "ok": 1.0 })
}

/// `secantusAdmin.restoreArchive` — extract a backup archive (from
/// `backupArchive`) into `targetDir`, a fresh directory the operator then points
/// a *new* server at (the running server's storage is untouched — same
/// side-channel model as the Python command and real mongod's "stop, swap
/// dbpath, start"). Required: `archivePath`, `targetDir`. Optional
/// `allowExisting` (bool, default false) overlays into a non-empty target.
/// Returns `{targetDir, fileCount, archive, ok}`. Mirrors the Python
/// `secantusAdmin.restoreArchive` command.
pub fn restore_archive(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let archive_path = doc.get_str("archivePath").unwrap_or("");
    if archive_path.is_empty() {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "secantusAdmin.restoreArchive requires archivePath: <string>",
        ));
    }
    let target_dir = doc.get_str("targetDir").unwrap_or("");
    if target_dir.is_empty() {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "secantusAdmin.restoreArchive requires targetDir: <string>",
        ));
    }
    let allow_existing = doc.get_bool("allowExisting").unwrap_or(false);
    let storage = ctx.storage()?;
    let (abs_target, abs_archive, file_count) = storage
        .restore_archive(archive_path, target_dir, allow_existing)
        // A failed restore (missing/invalid archive, non-empty target) is a
        // caller error, not an internal fault — mirror the Python handler's
        // IllegalOperation(20) rather than InternalError.
        .map_err(|e| CommandError::new(20, "IllegalOperation", command_error(e).errmsg))?;
    Ok(doc! {
        "targetDir": abs_target,
        "fileCount": file_count as i64,
        "archive": abs_archive,
        "ok": 1.0,
    })
}

/// `secantusAdmin.archiveBaseSnapshot` — take a PITR v2 base snapshot into
/// `archiveDir` (`base-<head>.tar.gz`). Pair with a server started with
/// `--oplog-archive-dir <archiveDir>` so pruned oplog rows are archived as
/// segments there too; recovery then stitches the newest base ≤ T plus the
/// segments. Mirrors the Python `secantusAdmin.archiveBaseSnapshot` command.
pub fn archive_base_snapshot(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let archive_dir = doc.get_str("archiveDir").unwrap_or("");
    if archive_dir.is_empty() {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "secantusAdmin.archiveBaseSnapshot requires archiveDir: <string>",
        ));
    }
    let storage = ctx.storage()?;
    let (path, size_bytes) = storage
        .archive_base_snapshot(archive_dir)
        .map_err(command_error)?;
    Ok(doc! { "path": path, "sizeBytes": size_bytes as i64, "ok": 1.0 })
}

/// `listCollections` — a cursor over the collections in the database, honouring
/// `filter` (a query predicate over each entry) and `nameOnly`. Each entry's
/// `options` reflects the collection's stored options (capped / validator /
/// collation / timeseries / …) so drivers introspecting them see the real
/// values. Mirrors `commands._list_collections`.
pub fn list_collections(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let filter = doc
        .get("filter")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());
    let name_only = bool_field(doc, "nameOnly", false);
    let names = storage
        .list_collections(&ctx.db_name)
        .map_err(command_error)?;
    let mut entries: Vec<Document> = Vec::with_capacity(names.len());
    for n in &names {
        let mut options = storage
            .get_collection_options(&ctx.db_name, n)
            .map_err(command_error)?;
        // A `viewOn` collection is a read-only view: type "view", readOnly true,
        // no `_id` index, and the stored `viewPipeline` surfaces as `pipeline`.
        let is_view = options.contains_key("viewOn");
        if let Some(p) = options.remove("viewPipeline") {
            options.insert("pipeline", p);
        }
        // `uuid` is an internal option (the collection identity) — it's surfaced
        // under `info.uuid`, not as a collection option. Strip it from `options`.
        options.remove("uuid");
        // mongod stores/reports capped `size` / `max` as int32; we may hold the
        // driver-sent int64. Normalise so the round-tripped options match.
        for k in ["size", "max"] {
            if let Some(Bson::Int64(v)) = options.get(k) {
                let v = *v;
                options.insert(k, Bson::Int32(v as i32));
            }
        }
        let coll_type = if is_view {
            "view"
        } else if options.contains_key("timeseries") {
            "timeseries"
        } else {
            "collection"
        };
        // `info.uuid` is BinData(4) — driver CollectionSpecification readers
        // (e.g. the go driver) read it as a Binary, so it must be present and the
        // right type.
        let mut info = doc! { "readOnly": is_view };
        if let Ok(uuid) = storage.collection_uuid(&ctx.db_name, n) {
            if uuid.len() == 16 {
                info.insert(
                    "uuid",
                    Bson::Binary(bson::Binary {
                        subtype: bson::spec::BinarySubtype::Uuid,
                        bytes: uuid,
                    }),
                );
            }
        }
        // Captured before `options` is moved into the entry below.
        let is_clustered = options.contains_key("clusteredIndex");
        let mut entry = doc! {
            "name": n,
            "type": coll_type,
            "options": Bson::Document(options),
            "info": info,
        };
        // A clustered collection has no separate `_id_` index (the clustering
        // key IS the index), so mongod omits `idIndex` for it — same as views.
        if !is_view && !is_clustered {
            entry.insert(
                "idIndex",
                doc! {
                    "v": 2,
                    "key": { "_id": 1 },
                    "name": "_id_",
                    "ns": format!("{}.{}", ctx.db_name, n),
                },
            );
        }
        entries.push(entry);
    }
    // `filter` is evaluated against the full entry (so `{name: …}`,
    // `{type: …}`, `{"options.capped": true}` all work); apply it before the
    // `nameOnly` projection so a filter on `options` still matches.
    if let Some(f) = filter {
        entries.retain(|e| {
            secantus_core::query::matches(e, f, &Document::new(), None).unwrap_or(false)
        });
    }
    if name_only {
        for e in &mut entries {
            let name = e.get_str("name").unwrap_or("").to_string();
            let ty = e.get_str("type").unwrap_or("collection").to_string();
            *e = doc! { "name": name, "type": ty };
        }
    }
    let ns = format!("{}.$cmd.listCollections", ctx.db_name);
    // Honour `cursor: {batchSize: N}` so a client that asks for a small batch
    // gets a real getMore (drivers' "listCollections getMore is monitored"
    // tests force this); absent ⇒ the wire default.
    let batch_size = doc
        .get("cursor")
        .and_then(Bson::as_document)
        .and_then(|c| c.get("batchSize"))
        .and_then(as_i64)
        .unwrap_or(DEFAULT_BATCH_SIZE as i64);
    let (first, cid) = split_into_cursor(encode_docs(entries)?, batch_size, &ns, cursors)?;
    Ok(doc! {
        "cursor": { "id": Bson::Int64(cid), "ns": ns, "firstBatch": docs_to_bson(first)? },
        "ok": 1.0,
    })
}

/// `listDatabases` — descriptors for every database, honouring `filter` and
/// `nameOnly`. Mirrors `commands._list_databases`: each descriptor is
/// `{name, sizeOnDisk, empty}` (`sizeOnDisk` = summed BSON doc bytes across the
/// db's collections; `empty` = size 0), reduced to `{name}` under `nameOnly`.
/// The `filter` is a query predicate evaluated against each descriptor; it's
/// applied after `totalSize` is accumulated, matching mongod.
pub fn list_databases(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let name_only = bool_field(doc, "nameOnly", false);
    let filter = doc
        .get("filter")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());

    let names = storage.list_databases().map_err(command_error)?;
    let mut descriptors: Vec<Document> = Vec::new();
    let mut total_size: i64 = 0;
    for n in &names {
        if name_only {
            descriptors.push(doc! { "name": n });
            continue;
        }
        let mut size: i64 = 0;
        for coll in storage.list_collections(n).map_err(command_error)? {
            size += storage
                .collection_data_size(n, &coll)
                .map_err(command_error)?;
        }
        total_size += size;
        descriptors.push(doc! { "name": n, "sizeOnDisk": size, "empty": size == 0 });
    }
    if let Some(f) = filter {
        descriptors.retain(|d| {
            secantus_core::query::matches(d, f, &Document::new(), None).unwrap_or(false)
        });
    }
    Ok(doc! {
        "databases": Bson::Array(descriptors.into_iter().map(Bson::Document).collect()),
        "totalSize": total_size,
        "ok": 1.0,
    })
}

/// `listIndexes` — a cursor over the indexes of a collection.
pub fn list_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "listIndexes")?;
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let mut indexes = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    // A collection that exists always has at least the synthesised `_id_` index, so
    // an empty result means the namespace doesn't exist — mongod errors
    // NamespaceNotFound (mongo-ruby-driver `Index::View#each ... collection does not
    // exist`). Mirrors commands.py's `if not indexes`.
    if indexes.is_empty() {
        return Ok(CommandError::new(
            26,
            "NamespaceNotFound",
            format!("ns does not exist: {}.{}", ctx.db_name, coll),
        )
        .into_reply());
    }
    // `multikey` is SecantusDB's internal catalog flag. mongod keeps the
    // equivalent in the durable catalog and never echoes it from `listIndexes`
    // (probed 6.0.16) — drivers see it only as explain's `isMultiKey`. Keep it
    // off the wire. Mirrors commands.py.
    // `entryFormat` is likewise internal — the on-disk index-entry layout
    // version (see `ENTRY_FORMAT_RECORDID`). mongod has no such field.
    for ix in &mut indexes {
        ix.remove("multikey");
        ix.remove("entryFormat");
    }
    // A clustered collection's clustering key IS its index: mongod reports a
    // single entry carrying `clustered: true` (with the user's name) in place of
    // the synthesised `_id_`. Mirrors commands.py.
    if let Some(ci) = storage
        .get_collection_options(&ctx.db_name, &coll)
        .ok()
        .and_then(|o| o.get_document("clusteredIndex").ok().cloned())
    {
        let clustered = doc! {
            "v": ci.get_i32("v").unwrap_or(2),
            "key": { "_id": 1i32 },
            "name": ci.get_str("name").unwrap_or("_id_").to_string(),
            "unique": true,
            "clustered": true,
        };
        let mut rest: Vec<Document> = indexes
            .into_iter()
            .filter(|ix| ix.get_str("name") != Ok("_id_"))
            .collect();
        indexes = std::iter::once(clustered).chain(rest.drain(..)).collect();
    }
    let ns = format!("{}.$cmd.listIndexes.{}", ctx.db_name, coll);
    // Honour `cursor: {batchSize: N}` so a client asking for a small batch gets a
    // real getMore round-trip (the Go driver's
    // `TestIndexView/list/getMore_commands_are_monitored` asserts a getMore fires
    // at batchSize 2); absent ⇒ the wire default. Mirrors `list_collections`.
    let batch_size = doc
        .get("cursor")
        .and_then(Bson::as_document)
        .and_then(|c| c.get("batchSize"))
        .and_then(as_i64)
        .unwrap_or(DEFAULT_BATCH_SIZE as i64);
    // A negative batchSize is rejected, not clamped. mongo-ruby-driver's
    // `Collection#indexes when a session is provided` uses `batch_size: -100`
    // as its deliberately-failing operation and asserts an OperationFailure.
    // Mirrors `commands._list_indexes`.
    if batch_size < 0 {
        return Ok(CommandError::new(
            51024,
            "BadValue",
            format!("BSON field 'batchSize' value must be >= 0, actual value {batch_size}"),
        )
        .into_reply());
    }
    let (first, cid) = split_into_cursor(encode_docs(indexes)?, batch_size, &ns, cursors)?;
    Ok(doc! {
        "cursor": { "id": Bson::Int64(cid), "ns": ns, "firstBatch": docs_to_bson(first)? },
        "ok": 1.0,
    })
}

/// `createIndexes` — create one or more indexes (auto-creating the collection).
/// Field-level query operators a `partialFilterExpression` may use. A `$`-key in
/// a field clause outside this set is an unknown operator (rejected).
const KNOWN_FIELD_OPS: &[&str] = &[
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$exists",
    "$type",
    "$regex",
    "$options",
    "$mod",
    "$size",
    "$all",
    "$elemMatch",
    "$not",
    "$bitsAllSet",
    "$bitsAnySet",
    "$bitsAllClear",
    "$bitsAnyClear",
    "$geoWithin",
    "$geoIntersects",
    "$near",
    "$nearSphere",
    "$comment",
    "$maxDistance",
    "$minDistance",
    "$geometry",
    "$center",
    "$centerSphere",
    "$box",
    "$polygon",
];

/// Document-level query operators accepted in a `partialFilterExpression` (the
/// logical / expression operators; `$and`/`$or`/`$nor` are recursed below).
const KNOWN_DOC_OPS: &[&str] = &["$expr", "$comment", "$text", "$where", "$jsonSchema"];

/// A conservative filter-validity check for `partialFilterExpression`:
/// `Some(reason)` for a clearly-invalid construct (a malformed `$and`/`$or`/`$nor`
/// or an unknown operator), else `None`. Deliberately lenient — it only rejects
/// operators outside the known sets, so a valid filter is never wrongly refused.
fn invalid_partial_filter(filter: &Document) -> Option<String> {
    for (k, v) in filter {
        if let Some(stripped) = k.strip_prefix('$') {
            match k.as_str() {
                "$and" | "$or" | "$nor" => match v {
                    Bson::Array(arr) => {
                        for elem in arr {
                            match elem {
                                Bson::Document(sub) => {
                                    if let Some(r) = invalid_partial_filter(sub) {
                                        return Some(r);
                                    }
                                }
                                _ => return Some(format!("{k} elements must be documents")),
                            }
                        }
                    }
                    _ => return Some(format!("{k} must be an array")),
                },
                _ if KNOWN_DOC_OPS.contains(&k.as_str()) => {}
                _ => return Some(format!("unknown top-level operator ${stripped}")),
            }
        } else if let Bson::Document(opd) = v {
            // A field clause whose value is an operator document: every `$`-key
            // must be a known field operator.
            if let Some(bad) = opd
                .keys()
                .find(|kk| kk.starts_with('$') && !KNOWN_FIELD_OPS.contains(&kk.as_str()))
            {
                return Some(format!("unknown operator {bad}"));
            }
        }
    }
    None
}

/// Whether a BSON value is "falsy" the way mongod treats default index options
/// (`hidden` / `sparse` / `unique`): `false`, `0`, `0.0`, or null.
fn is_falsy(v: &Bson) -> bool {
    match v {
        Bson::Boolean(b) => !b,
        Bson::Int32(i) => *i == 0,
        Bson::Int64(i) => *i == 0,
        Bson::Double(d) => *d == 0.0,
        Bson::Null => true,
        _ => false,
    }
}

pub fn create_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "createIndexes")?;
    let storage = ctx.storage()?;
    let specs: Vec<Bson> = match doc.get("indexes") {
        Some(Bson::Array(a)) => a.clone(),
        _ => Vec::new(),
    };

    // `commitQuorum` (4.4+) accepts an integer, `"majority"`, or `"votingMembers"`;
    // any other string is an unknown write-concern mode (mongo-ruby-driver's
    // `commit_quorum value is not supported` pins this). Mirrors commands.py.
    if let Some(cq) = doc.get("commitQuorum") {
        let ok = matches!(cq, Bson::Int32(_) | Bson::Int64(_))
            || matches!(cq.as_str(), Some("majority") | Some("votingMembers"));
        if !ok {
            let shown = match cq {
                Bson::String(s) => format!("'{s}'"),
                other => format!("{other}"),
            };
            return Ok(CommandError::new(
                79,
                "UnknownReplWriteConcern",
                format!("No write concern mode named {shown} found in replica set configuration"),
            )
            .into_reply());
        }
    }

    let before = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .len();
    // createIndexes implicitly creates the collection if absent.
    let created_coll = storage
        .create_collection(&ctx.db_name, &coll)
        .map_err(command_error)?;

    let mut any_created = false;
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
        // Unknown fields on the spec itself are rejected, not ignored.
        let spec_opts: Document = s
            .iter()
            .filter(|(k, _)| k.as_str() != "key" && k.as_str() != "name")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(unknown) = first_unknown_field(&spec_opts, INDEX_SPEC_KNOWN_OPTIONS) {
            return Ok(CommandError::new(
                40415,
                "Location40415",
                format!("Error in specification {s:?}: the field '{unknown}' is an unknown field"),
            )
            .into_reply());
        }
        // Guard the index name against an embedded NUL before it reaches the
        // WT key encoder (see crate::nul_in_namespace / #139).
        if let Some(e) = crate::nul_in_namespace("index name", &name) {
            return Ok(e.into_reply());
        }
        // `partialFilterExpression` must be a document and a parseable filter —
        // mongod rejects a non-document, unknown operators (`{x: {$asdasd: 3}}`),
        // and malformed logical operators (`{$and: 5}`). (pymongo's
        // `test_index_filter` pins these.)
        if let Some(pfe) = s.get("partialFilterExpression") {
            match pfe {
                Bson::Document(f) => {
                    if let Some(reason) = invalid_partial_filter(f) {
                        return Ok(CommandError::new(
                            2,
                            "BadValue",
                            format!("Error in specification, partialFilterExpression is invalid: {reason}"),
                        )
                        .into_reply());
                    }
                }
                _ => {
                    return Ok(CommandError::new(
                        2,
                        "BadValue",
                        "partialFilterExpression must be a document",
                    )
                    .into_reply())
                }
            }
        }
        // `wildcardProjection` is only valid on a wildcard index ({$**:1} /
        // {f.$**:1}) and must be a non-empty document (mongo-ruby-driver's
        // invalid-wildcard-projection tests). Mirrors commands.py.
        if let Some(wcp) = s.get("wildcardProjection") {
            let nonempty_doc = matches!(wcp, Bson::Document(d) if !d.is_empty());
            if !nonempty_doc {
                return Ok(CommandError::new(
                    67,
                    "CannotCreateIndex",
                    format!(
                        "Error in specification {{ key: {key:?}, wildcardProjection: {wcp:?} }} \
                         :: caused by :: wildcardProjection must be a non-empty object"
                    ),
                )
                .into_reply());
            }
            let is_wildcard = key.keys().any(|k| k == "$**" || k.ends_with(".$**"));
            if !is_wildcard {
                return Ok(CommandError::new(
                    67,
                    "CannotCreateIndex",
                    format!(
                        "Error in specification {{ key: {key:?}, wildcardProjection: {wcp:?} }} \
                         :: caused by :: wildcardProjection is only allowed on wildcard indexes"
                    ),
                )
                .into_reply());
            }
        }
        // mongod stores only the non-default form of hidden / sparse / unique: a
        // falsy value is dropped so it doesn't come back from listIndexes
        // (mongo-ruby-driver `hidden is false` asserts hidden isn't echoed).
        let mut spec = s.clone();
        for opt in ["hidden", "sparse", "unique"] {
            if spec.get(opt).is_some_and(is_falsy) {
                spec.remove(opt);
            }
        }
        let created = storage
            .create_index(&ctx.db_name, &coll, &name, &key, &spec)
            .map_err(command_error)?;
        any_created |= created;
    }

    let after = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .len();
    let mut reply = doc! {
        "createdCollectionAutomatically": created_coll,
        "numIndexesBefore": before as i32,
        "numIndexesAfter": after as i32,
        "ok": 1.0,
    };
    // When every requested index already existed, mongod adds
    // `note: "all indexes already exist"` so drivers report a no-op (mongocxx's
    // `index_view::create_one` returns an empty optional off this).
    if !any_created && !specs.is_empty() {
        reply.insert("note", "all indexes already exist");
    }
    Ok(reply)
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
            if let Some(e) = crate::nul_in_namespace("index name", name) {
                return Ok(e.into_reply());
            }
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
        // A missing source ("source namespace does not exist") is
        // NamespaceNotFound (26); an existing target ("target namespace exists")
        // is NamespaceExists (48). Check the source-missing phrasing first so a
        // bare "exist" substring doesn't misclassify "does not exist" as 48.
        let lower = m.to_lowercase();
        let (code, name) = if lower.contains("does not exist") || lower.contains("not found") {
            (26, "NamespaceNotFound")
        } else if lower.contains("exists") {
            (48, "NamespaceExists")
        } else {
            (26, "NamespaceNotFound")
        };
        return Ok(CommandError::new(code, name, m).into_reply());
    }
    // A rename invalidates cursors open on the source (and the dropped target),
    // same as a drop — a later getMore then fails with CursorNotFound. Mirrors
    // commands.py::_rename_collection.
    if let Ok(cursors) = ctx.cursors() {
        cursors.kill_namespace(&src);
        if drop_target {
            cursors.kill_namespace(&to);
        }
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

/// `serverStatus` — a minimal subset (host / version / process / uptime), plus a
/// live `metrics.cursor.open.total` so drivers can track cursor lifecycle
/// (mongo-php-driver `cursor-destruct-001` opens a batched cursor and asserts the
/// count rises by one, then returns to baseline after `killCursors`).
pub fn server_status(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // `CursorRegistry::len` prunes idle cursors then counts the live ones — the
    // count rises while a batched cursor is open and drops on killCursors.
    let open_cursors = ctx.cursors().map(|c| c.len()).unwrap_or(0) as i64;
    // Real counts when the server supplied them; zeros off-server (unit tests).
    let conns = ctx.conn_stats.unwrap_or_default();
    // Defaults to persistent when there is no storage (unit-test contexts),
    // matching the Python server's fallback rather than erroring the command.
    let persistent = ctx.storage().map(|s| !s.in_memory()).unwrap_or(true);
    Ok(doc! {
        "host": "secantus",
        "version": crate::SERVER_VERSION,
        "process": "mongod",
        "pid": Bson::Int64(0),
        "uptime": 0.0,
        "uptimeMillis": Bson::Int64(0),
        "localTime": bson::DateTime::now(),
        "metrics": {
            "cursor": {
                "open": { "total": open_cursors, "pinned": 0i64, "noTimeout": 0i64 },
            },
        },
        // mongo-c-driver's `/Client/exhaust_cursor/{single,pool}` read
        // `connections.totalCreated` off serverStatus to check the connection
        // pool wasn't cleared. Omitting the section made those fail with
        // "'connections.totalCreated' field not found" — a serverStatus gap
        // that looked like an exhaust-cursor bug. Mirrors the Python server's
        // zeroed block; SecantusDB keeps no pool counters.
        // Int32, not Int64: libmongoc reads these with `bson_lookup_int32`,
        // which type-checks rather than coercing ("'connections.totalCreated'
        // is not a int32"). The Python server emits plain ints, which encode
        // as Int32, so it never hit this.
        "connections": {
            "current": conns.current as i32,
            "available": 0i32,
            "totalCreated": conns.total_created as i32,
        },
        "opcounters": {
            "insert": 0i32, "query": 0i32, "update": 0i32,
            "delete": 0i32, "getmore": 0i32, "command": 0i32,
        },
        // Storage-engine identity. Drivers gate real behaviour on this:
        // mongo-php-library's `skipIfTransactionsNotSupported` reads
        // `storageEngine.name` and throws "Could not determine server storage
        // engine" when the key is absent, turning ~27 transaction tests into
        // ERRORs instead of the clean skip the helper intends. Reporting
        // "wiredTiger" is honest — SecantusDB is WiredTiger-backed, the same
        // engine mongod uses. Kept byte-identical to the Python server's
        // `_storage_engine_section`.
        "storageEngine": {
            "name": "wiredTiger",
            "supportsCommittedReads": true,
            "supportsPendingDrops": true,
            "supportsSnapshotReadConcern": true,
            "readOnly": false,
            "persistent": persistent,
            "backupCursorOpen": false,
        },
        "network": { "numRequests": 0i32, "bytesIn": 0i32, "bytesOut": 0i32 },
        // Categorical self-identification: real mongod never has this key.
        // Tooling (the conformance-gauge tripwire, ad-hoc smoke scripts)
        // checks it to prove it's talking to SecantusDB rather than an
        // accidental real MongoDB on the same address. The Python server
        // reports `server: "python"`.
        "secantus": { "server": "rust", "version": env!("CARGO_PKG_VERSION") },
        "ok": 1.0,
    })
}

/// `currentOp` — mongod's in-flight-operation introspection. SecantusDB runs
/// commands synchronously and keeps no per-op registry, so the only operation
/// "in progress" is the `currentOp` request itself. We emit one synthetic
/// `inprog` entry carrying this connection's driver `clientMetadata` (captured
/// from the handshake `client` doc), which is what the drivers' handshake-
/// metadata tests read back. A client filter (e.g. `command.currentOp` /
/// `$ownOps`) is accepted but not applied — the single self-op already matches
/// the introspection queries the drivers issue.
pub fn current_op(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let client_metadata = ctx
        .conn_auth
        .as_ref()
        .and_then(|a| a.lock().ok())
        .and_then(|g| g.client_metadata.clone());

    let mut op = doc! {
        "type": "op",
        "host": "secantus",
        "desc": "conn",
        "connectionId": Bson::Int64(ctx.connection_id),
        "active": true,
        "op": "command",
        "ns": format!("{}.$cmd", ctx.db_name),
        "command": doc.clone(),
        "opid": Bson::Int64(ctx.connection_id),
        "secs_running": Bson::Int64(0),
        "microsecs_running": Bson::Int64(0),
    };
    if let Some(meta) = client_metadata {
        op.insert("clientMetadata", meta);
    }

    Ok(doc! {
        "inprog": vec![Bson::Document(op)],
        "ok": 1.0,
    })
}

/// `validate` — mongod's collection consistency check. SecantusDB stores
/// documents as opaque BSON and maintains index entries transactionally, so
/// there's nothing to repair: report a clean, mongod-shaped result with real
/// record / index counts. `full` / `background` / `scandata` are accepted and
/// ignored (they only affect how mongod scans, not the verdict). Ports
/// `commands.py::_validate`.
pub fn validate(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = match doc.get("validate") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Ok(
                CommandError::new(14, "TypeMismatch", "validate requires a collection name")
                    .into_reply(),
            )
        }
    };
    let storage = ctx.storage()?;
    if !storage
        .collection_exists(&ctx.db_name, &coll)
        .map_err(command_error)?
    {
        return Ok(CommandError::new(
            26,
            "NamespaceNotFound",
            format!(
                "Collection '{}.{}' does not exist to validate.",
                ctx.db_name, coll
            ),
        )
        .into_reply());
    }
    // mongod rejects full+background together (full needs an exclusive scan).
    if bool_field(doc, "full", false) && bool_field(doc, "background", false) {
        return Ok(CommandError::new(
            72,
            "InvalidOptions",
            "Running the validate command with both { background: true } and { full: true } is \
             not supported.",
        )
        .into_reply());
    }
    let nrecords = storage
        .count_matching(&ctx.db_name, &coll, &Document::new())
        .map_err(command_error)? as i64;
    let indexes = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let mut keys_per_index = Document::new();
    let mut index_details = Document::new();
    let mut n_indexes = 0i32;
    for ix in &indexes {
        if let Ok(name) = ix.get_str("name") {
            keys_per_index.insert(name, nrecords);
            index_details.insert(name, doc! { "valid": true });
            n_indexes += 1;
        }
    }
    Ok(doc! {
        "ns": format!("{}.{}", ctx.db_name, coll),
        "nInvalidDocuments": 0i64,
        "nNonCompliantDocuments": 0i64,
        "nrecords": nrecords,
        "nIndexes": n_indexes,
        "keysPerIndex": keys_per_index,
        "indexDetails": index_details,
        "valid": true,
        "repaired": false,
        "warnings": Bson::Array(vec![]),
        "errors": Bson::Array(vec![]),
        "extraIndexEntries": Bson::Array(vec![]),
        "missingIndexEntries": Bson::Array(vec![]),
        "corruptRecords": Bson::Array(vec![]),
        "ok": 1.0,
    })
}

/// `profile` — get / set per-database profiling level. `{profile: -1}` reads;
/// `{profile: 0|1|2, slowms, sampleRate}` updates. The reply carries the
/// PREVIOUS values under `was` / `slowms` / `sampleRate`. Ports
/// `commands.py::_profile`. (SecantusDB records the level but does no actual
/// slow-op profiling — `system.profile` stays a faithful stub.)
pub fn profile(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    let prev = storage.get_profile(&db).map_err(command_error)?;
    let prev_level = prev.get_i32("level").unwrap_or(0);
    let prev_slowms = prev.get_i32("slowms").unwrap_or(100);
    let prev_rate = prev.get_f64("sampleRate").unwrap_or(1.0);
    let was = doc! {
        "was": prev_level,
        "slowms": prev_slowms,
        "sampleRate": prev_rate,
        "ok": 1.0,
    };
    let arg = match doc.get("profile") {
        Some(Bson::Int32(n)) => Some(*n),
        Some(Bson::Int64(n)) => Some(*n as i32),
        Some(Bson::Double(d)) if d.fract() == 0.0 => Some(*d as i32),
        _ => None,
    };
    match arg {
        Some(-1) => Ok(was),
        Some(level) if (0..=2).contains(&level) => {
            let slowms = doc
                .get("slowms")
                .and_then(as_i64)
                .map(|n| n as i32)
                .unwrap_or(prev_slowms);
            let rate = doc
                .get("sampleRate")
                .and_then(Bson::as_f64)
                .unwrap_or(prev_rate);
            storage
                .set_profile(&db, level, slowms, rate)
                .map_err(command_error)?;
            Ok(was)
        }
        _ => Ok(
            CommandError::new(14, "TypeMismatch", "profile must be -1, 0, 1, or 2").into_reply(),
        ),
    }
}

/// Options mongod's index-spec IDL accepts, plus the legacy / deprecated forms
/// drivers still emit. Anything outside this set is an unknown field.
/// Mirrors `commands._INDEX_SPEC_KNOWN_OPTIONS` — keep the two in step.
const INDEX_SPEC_KNOWN_OPTIONS: &[&str] = &[
    // Geometric / vector indexes.
    "2dsphereIndexVersion",
    "bits",
    "min",
    "max",
    // Wildcard.
    "wildcardProjection",
    // Standard knobs.
    "unique",
    "sparse",
    "hidden",
    "background",
    "expireAfterSeconds",
    "partialFilterExpression",
    "collation",
    "storageEngine",
    // Text — accepted on the wire even though text indexes are unsupported
    // (storage rejects them with CreateIndexUnsupported).
    "weights",
    "default_language",
    "language_override",
    "textIndexVersion",
    // Index format version + namespace (legacy drivers).
    "v",
    "ns",
    // Haystack (deprecated).
    "bucketSize",
    // Removed in MongoDB 3.0; modern mongod accepts and silently ignores it,
    // so a unique index over duplicate data still fails on the duplicate
    // rather than on an unknown-field error.
    "dropDups",
];

/// Top-level options the `create` command accepts, plus the wire-envelope
/// fields a driver may attach. Mirrors `commands._CREATE_KNOWN_OPTIONS`.
const CREATE_KNOWN_OPTIONS: &[&str] = &[
    "create",
    "capped",
    "size",
    "max",
    "validator",
    "validationAction",
    "validationLevel",
    "viewOn",
    "pipeline",
    "collation",
    "expireAfterSeconds",
    "timeseries",
    "clusteredIndex",
    "changeStreamPreAndPostImages",
    "storageEngine",
    "indexOptionDefaults",
    "writeConcern",
    "comment",
    "maxTimeMS",
    // mongorestore sends the source collection's full `_id_` spec.
    "idIndex",
    // Legacy / deprecated but tolerated.
    "autoIndexId",
    "flags",
    // Non-`$`-prefixed envelope fields ( `$`-prefixed keys are accepted
    // unconditionally by the caller).
    "lsid",
    "txnNumber",
    "autocommit",
    "startTransaction",
    "readConcern",
    "apiVersion",
    "apiStrict",
    "apiDeprecationErrors",
];

/// The first field of `doc` outside `known`, ignoring `$`-prefixed envelope
/// keys. mongod surfaces an unknown field as `Location40415` (IDLUnknownField)
/// rather than ignoring it, and driver suites rely on that: mongo-ruby-driver's
/// "a failed operation using a session" shared specs provoke it deliberately by
/// passing `invalid: true` and asserting an `OperationFailure`.
fn first_unknown_field(doc: &Document, known: &[&str]) -> Option<String> {
    doc.keys()
        .find(|k| !k.starts_with('$') && !known.contains(&k.as_str()))
        .cloned()
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

/// `createSearchIndexes` / `updateSearchIndex` / `dropSearchIndex` — Atlas Search
/// index management, an Atlas-only feature.
///
/// A real non-Atlas mongod *registers* these commands and fails them at
/// execution with a message naming Atlas; the driver index-management spec
/// tests assert only that the error mentions Atlas. Leaving them unregistered
/// returns `CommandNotFound` (59) instead, which is what
/// mongo-c-driver's `/index-management/{update,drop}SearchIndex` caught. The
/// message is shared with the `$listSearchIndexes` stage so the two stay in
/// lockstep. Mirrors `commands._search_index_not_supported`.
pub fn search_index_not_supported(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(CommandError::new(
        115,
        "CommandNotSupported",
        crate::aggregate::SEARCH_INDEX_ATLAS_MSG,
    )
    .into_reply())
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use bson::doc;

    fn ctx() -> CommandContext {
        let mut c = CommandContext::new(1);
        c.db_name = "testdb".to_string();
        c
    }

    fn err_of(reply: &Document) -> (i32, String, String) {
        (
            reply.get_i32("code").unwrap_or_default(),
            reply.get_str("codeName").unwrap_or_default().to_string(),
            reply.get_str("errmsg").unwrap_or_default().to_string(),
        )
    }

    /// mongo-c-driver's `/index-management/{update,drop}SearchIndex` assert the
    /// error names Atlas. Leaving the commands unregistered returned
    /// `CommandNotFound` (59) instead.
    #[test]
    fn search_index_commands_report_atlas_not_command_not_found() {
        for name in [
            "createSearchIndexes",
            "updateSearchIndex",
            "dropSearchIndex",
        ] {
            assert!(
                crate::lookup_for_test(name).is_some(),
                "{name} must be registered, not CommandNotFound"
            );
        }
        let reply = search_index_not_supported(&doc! {"dropSearchIndex": "c"}, &mut ctx()).unwrap();
        let (code, name, msg) = err_of(&reply);
        assert_eq!((code, name.as_str()), (115, "CommandNotSupported"));
        assert!(
            msg.contains("Atlas"),
            "the driver specs assert on 'Atlas': {msg}"
        );
    }

    /// mongo-ruby-driver's "a failed operation using a session" shared specs
    /// pass `invalid: true` and assert an `OperationFailure`; silently
    /// accepting the unknown field made them fail.
    #[test]
    fn create_rejects_an_unknown_top_level_option() {
        let reply = create(&doc! {"create": "c", "invalid": true}, &mut ctx()).unwrap();
        let (code, name, msg) = err_of(&reply);
        assert_eq!((code, name.as_str()), (40415, "Location40415"));
        assert!(msg.contains("create.invalid"), "{msg}");
    }

    #[test]
    fn create_still_accepts_every_known_option_and_the_wire_envelope() {
        // A `$`-prefixed envelope key and the non-`$` ones must pass through.
        let d = doc! {
            "create": "c", "capped": true, "size": 4096_i64, "max": 512_i64,
            "lsid": {"id": "x"}, "$db": "testdb", "writeConcern": {"w": 1},
        };
        assert!(
            first_unknown_field(&d, CREATE_KNOWN_OPTIONS).is_none(),
            "known options must not trip the unknown-field check"
        );
    }

    /// End to end through `createIndexes`, which is what the Ruby spec drives:
    /// `view.create_one({random: 1}, invalid: true)`.
    #[test]
    fn create_indexes_rejects_an_unknown_spec_option() {
        let mut c = ctx();
        c = c.with_storage(std::sync::Arc::new(FakeStorage));
        let reply = create_indexes(
            &doc! {
                "createIndexes": "specs",
                "indexes": [{"key": {"random": 1}, "name": "random_1", "invalid": true}],
            },
            &mut c,
        )
        .unwrap();
        let (code, name, msg) = err_of(&reply);
        assert_eq!((code, name.as_str()), (40415, "Location40415"));
        assert!(msg.contains("invalid"), "{msg}");
    }

    #[test]
    fn create_indexes_accepts_a_valid_spec() {
        let mut c = ctx();
        c = c.with_storage(std::sync::Arc::new(FakeStorage));
        let reply = create_indexes(
            &doc! {
                "createIndexes": "specs",
                "indexes": [{"key": {"a": 1}, "name": "a_1", "unique": true}],
            },
            &mut c,
        )
        .unwrap();
        assert_eq!(reply.get_f64("ok").unwrap_or(0.0), 1.0, "{reply:?}");
    }

    /// mongo-php-library's `skipIfTransactionsNotSupported` reads
    /// `storageEngine.name`, and throws "Could not determine server storage
    /// engine" when it is absent — erroring ~27 transaction tests rather than
    /// skipping them. Must stay byte-identical to the Python server's
    /// `_storage_engine_section`.
    #[test]
    fn server_status_reports_wiredtiger_storage_engine() {
        let mut c = ctx();
        c = c.with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()));
        let reply = server_status(&doc! {"serverStatus": 1}, &mut c).unwrap();
        let engine = reply.get_document("storageEngine").expect("storageEngine");
        assert_eq!(engine.get_str("name").unwrap(), "wiredTiger");
        assert!(engine.get_bool("supportsCommittedReads").unwrap());
        assert!(engine.get_bool("supportsSnapshotReadConcern").unwrap());
        assert!(!engine.get_bool("readOnly").unwrap());
        // No storage attached (unit context) falls back to persistent, matching
        // the Python server rather than failing the command.
        assert!(engine.get_bool("persistent").unwrap());
    }

    /// mongo-c-driver's `/Client/exhaust_cursor/{single,pool}` read
    /// `connections.totalCreated` to check the pool wasn't cleared. Its absence
    /// failed them with "field not found", which read as an exhaust-cursor bug.
    #[test]
    fn server_status_carries_the_sections_drivers_read() {
        let mut c = ctx();
        c = c.with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()));
        let reply = server_status(&doc! {"serverStatus": 1}, &mut c).unwrap();
        let conns = reply.get_document("connections").expect("connections");
        // Int32 specifically — libmongoc type-checks with bson_lookup_int32
        // rather than coercing, so an Int64 zero fails just as hard as a
        // missing field.
        for f in ["totalCreated", "current", "available"] {
            assert!(
                matches!(conns.get(f), Some(bson::Bson::Int32(_))),
                "connections.{f} must be Int32: {conns:?}"
            );
        }

        // With real counters attached, the reported values are those — not
        // zeros. The exhaust tests read `totalCreated` before and after opening
        // a cursor and require it to have risen, so a constant is not enough.
        let mut c2 = ctx();
        c2 = c2
            .with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()))
            .with_conn_stats(crate::ConnStats {
                current: 3,
                total_created: 7,
            });
        let reply2 = server_status(&doc! {"serverStatus": 1}, &mut c2).unwrap();
        let conns2 = reply2.get_document("connections").unwrap();
        assert_eq!(conns2.get_i32("totalCreated").unwrap(), 7);
        assert_eq!(conns2.get_i32("current").unwrap(), 3);
        assert!(reply.get_document("opcounters").is_ok());
        assert!(reply.get_document("network").is_ok());
    }

    /// mongo-ruby-driver's `Collection#indexes when a session is provided` uses
    /// `batch_size: -100` as its deliberately-failing operation.
    #[test]
    fn list_indexes_rejects_a_negative_batch_size() {
        let mut c = ctx();
        c = c
            .with_storage(std::sync::Arc::new(FakeStorage))
            .with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()));
        let reply = list_indexes(
            &doc! {"listIndexes": "specs", "cursor": {"batchSize": -100_i32}},
            &mut c,
        )
        .unwrap();
        let (code, name, msg) = err_of(&reply);
        assert_eq!((code, name.as_str()), (51024, "BadValue"));
        assert!(msg.contains("must be >= 0"), "{msg}");
    }

    #[test]
    fn list_indexes_still_accepts_a_zero_or_positive_batch_size() {
        for bs in [0_i32, 2_i32] {
            let mut c = ctx();
            c = c
                .with_storage(std::sync::Arc::new(FakeStorage))
                .with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()));
            let reply = list_indexes(
                &doc! {"listIndexes": "specs", "cursor": {"batchSize": bs}},
                &mut c,
            )
            .unwrap();
            assert_eq!(
                reply.get_f64("ok").unwrap_or(0.0),
                1.0,
                "batchSize {bs}: {reply:?}"
            );
        }
    }

    /// Minimal in-memory `Storage`: only the methods without a default impl.
    struct FakeStorage;

    impl crate::Storage for FakeStorage {
        fn insert(
            &self,
            _db: &str,
            _coll: &str,
            _docs: Vec<Vec<u8>>,
            _ordered: bool,
        ) -> Result<(usize, Vec<Document>), crate::StorageError> {
            Ok((0, Vec::new()))
        }
        fn update_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _update: &Document,
            _multi: bool,
            _upsert: bool,
        ) -> Result<crate::UpdateOutcome, crate::StorageError> {
            Ok(crate::UpdateOutcome::default())
        }
        fn delete_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _limit: usize,
        ) -> Result<usize, crate::StorageError> {
            Ok(0)
        }
        fn count_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
        ) -> Result<usize, crate::StorageError> {
            Ok(0)
        }
        fn find(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _sort: Option<&Document>,
            _hint: Option<crate::storage::RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, crate::StorageError> {
            Ok(Vec::new())
        }
        /// A non-empty result is what marks the namespace as existing — an
        /// empty one is how `list_indexes` detects NamespaceNotFound.
        fn list_indexes(
            &self,
            _db: &str,
            _coll: &str,
        ) -> Result<Vec<Document>, crate::StorageError> {
            Ok(vec![doc! {"v": 2, "key": {"_id": 1}, "name": "_id_"}])
        }
    }

    #[test]
    fn index_spec_accepts_the_documented_options() {
        for k in [
            "unique",
            "sparse",
            "hidden",
            "background",
            "expireAfterSeconds",
            "partialFilterExpression",
            "collation",
            "storageEngine",
            "weights",
            "v",
            "ns",
            "bucketSize",
            "dropDups",
            "2dsphereIndexVersion",
            "bits",
            "min",
            "max",
            "wildcardProjection",
        ] {
            let d = doc! { k: 1 };
            assert!(
                first_unknown_field(&d, INDEX_SPEC_KNOWN_OPTIONS).is_none(),
                "{k} is a real mongod index option and must be accepted"
            );
        }
    }
}
