//! `dbCheck` (RM-219), as a one-member replica set's primary runs it.
//!
//! mongod 8.2.11's primary reads the collection in `_id` order, in batches,
//! and writes an oplog command entry per batch carrying the MD5 of the
//! batch's documents (their BSON, concatenated in `_id` order: reproduced
//! exactly), for its secondaries to compare against their own copy. With no
//! secondary there is nothing to compare, so what is left on a single node
//! is those entries and the `dbCheckStart` / `dbCheckStop` records in
//! `local.system.healthlog` (measured 2026-10-20). mongod does this in the
//! background and answers at once; here it is done before the answer.

use bson::{doc, oid::ObjectId, spec::BinarySubtype, Binary, Bson, Document};
use md5::{Digest, Md5};

use crate::{CommandContext, CommandError, HandlerResult};

const HEALTHLOG: &str = "system.healthlog";
/// mongod's default batch size in documents (`maxDocsPerBatch`).
const DEFAULT_DOCS_PER_BATCH: i64 = 5000;
const VALIDATE_MODES: &[&str] = &[
    "dataConsistency",
    "dataConsistencyAndMissingIndexKeysCheck",
    "extraIndexKeysCheck",
];
/// The options a single-collection `dbCheck` takes (mongod's
/// `DbCheckSingleInvocation`).
const OPTIONS: &[&str] = &[
    "minKey",
    "maxKey",
    "start",
    "end",
    "maxCount",
    "maxSize",
    "maxCountPerSecond",
    "maxDocsPerBatch",
    "maxBytesPerBatch",
    "maxBatchTimeMillis",
    "snapshotRead",
    "batchWriteConcern",
    "validateMode",
    "secondaryIndex",
    "skipLookupForExtraKeys",
    "bsonValidateMode",
];

fn error(code: i32, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, crate::util::error_code_name(code), msg)
}

/// `dbCheck`.
pub fn db_check(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let single = doc.get_str("dbCheck").ok().map(str::to_string);
    for (k, v) in doc.iter().skip(1) {
        if crate::params::is_generic_arg(k) {
            continue;
        }
        // mongod's parser names no struct here: `BSON field '.x'`.
        if single.is_none() || !OPTIONS.contains(&k.as_str()) {
            return Err(error(
                40415,
                format!("BSON field '.{k}' is an unknown field."),
            ));
        }
        if k == "validateMode" {
            match v {
                Bson::String(s) if VALIDATE_MODES.contains(&s.as_str()) => {}
                Bson::String(s) => return Err(error(
                    2,
                    format!(
                        "Enumeration value '{s}' for field '.validateMode' is not a valid value."
                    ),
                )),
                other => return Err(error(
                    14,
                    format!(
                        "BSON field '.validateMode' is the wrong type '{}', expected type 'string'",
                        secantus_core::query::bson_type_name(other)
                    ),
                )),
            }
        }
    }
    if doc.get_str("validateMode") == Ok("extraIndexKeysCheck") {
        if doc.get_str("secondaryIndex").unwrap_or("").is_empty() {
            return Err(error(
                72,
                "When validateMode is set to extraIndexKeysCheck, the secondaryIndex parameter \
                 must be set.",
            ));
        }
        // That mode walks a secondary index's keys against the documents,
        // which this server does not do.
        return Err(error(
            115,
            "dbCheck with validateMode extraIndexKeysCheck is not supported",
        ));
    }
    let db = ctx.db_name.clone();
    let storage = ctx
        .storage
        .clone()
        .ok_or_else(|| error(1, "storage backend not configured"))?;
    let params = doc! {
        "validateMode": doc.get_str("validateMode").unwrap_or("dataConsistency"),
        "secondaryIndex": doc.get_str("secondaryIndex").unwrap_or(""),
        "skipLookupForExtraKeys": doc.get_bool("skipLookupForExtraKeys").unwrap_or(false),
        "bsonValidateMode": doc.get_str("bsonValidateMode").unwrap_or("kDefault"),
    };
    match single {
        Some(coll) => {
            if crate::views::is_view(storage.as_ref(), &db, &coll) {
                return Err(error(
                    166,
                    format!(
                        "{coll} is a view hence 'dbcheck' is not supported. :: caused by :: \
                         Namespace {db}.{coll} is a view, not a collection"
                    ),
                ));
            }
            if !storage
                .collection_exists(&db, &coll)
                .map_err(crate::util::command_error)?
            {
                return Err(error(26, format!("Collection {coll} not found")));
            }
            let ns = format!("{db}.{coll}");
            let uuid = storage
                .collection_uuid(&db, &coll)
                .map_err(crate::util::command_error)?;
            let ui = Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes: uuid,
            });
            let per_batch = doc
                .get("maxDocsPerBatch")
                .and_then(crate::util::as_i64)
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_DOCS_PER_BATCH) as usize;
            let max_count = doc
                .get("maxCount")
                .and_then(crate::util::as_i64)
                .filter(|n| *n > 0)
                .map(|n| n as usize);
            let marker = |kind: &str| {
                doc! {
                    "dbCheck": ns.clone(),
                    "type": kind,
                    "secondaryIndexCheckParameters": params.clone(),
                    "uuid": ui.clone(),
                }
            };
            healthlog(storage.as_ref(), "dbCheckStart", Some((&ns, &ui, &params)))?;
            storage
                .append_command_entry("admin.$cmd", marker("start"))
                .map_err(crate::util::command_error)?;
            let rows = sorted_rows(storage.as_ref(), &db, &coll)?;
            let in_collection = rows.len();
            let rows = match max_count {
                Some(n) => &rows[..n.min(rows.len())],
                None => &rows[..],
            };
            let total = rows.len();
            let mut start = Bson::MinKey;
            let mut done = 0usize;
            loop {
                let batch = &rows[done..(done + per_batch).min(total)];
                done += batch.len();
                // The batch that reaches the end of the collection ends at
                // MaxKey; one cut short ends at its last `_id`.
                let reached_end = done >= in_collection;
                let end = match (reached_end, batch.last()) {
                    (true, _) | (_, None) => Bson::MaxKey,
                    (false, Some((id, _))) => id.clone(),
                };
                let mut md5 = Md5::new();
                for (_, raw) in batch {
                    md5.update(raw);
                }
                let digest: String = md5.finalize().iter().map(|b| format!("{b:02x}")).collect();
                storage
                    .append_command_entry(
                        &ns,
                        doc! {
                            "dbCheck": ns.clone(),
                            "type": "batch",
                            "md5": digest,
                            "minKey": Bson::MinKey,
                            "maxKey": Bson::MaxKey,
                            "batchStart": {"_id": start.clone()},
                            "batchEnd": {"_id": end.clone()},
                            "readTimestamp": Bson::Timestamp(storage.current_cluster_time()),
                            "secondaryIndexCheckParameters": params.clone(),
                            "logBatchToHealthLog": false,
                            "batchId": Bson::Binary(Binary {
                                subtype: BinarySubtype::Uuid,
                                bytes: bson::uuid::Uuid::new().bytes().to_vec(),
                            }),
                        },
                    )
                    .map_err(crate::util::command_error)?;
                if done >= total {
                    break;
                }
                start = end;
            }
            storage
                .append_command_entry("admin.$cmd", marker("stop"))
                .map_err(crate::util::command_error)?;
            healthlog(storage.as_ref(), "dbCheckStop", Some((&ns, &ui, &params)))?;
        }
        None => {
            healthlog(storage.as_ref(), "dbCheckStart", None)?;
            storage
                .append_command_entry(
                    "admin.$cmd",
                    doc! {"dbCheck": "admin.$cmd", "type": "start"},
                )
                .map_err(crate::util::command_error)?;
            let mut colls = storage
                .list_collections(&db)
                .map_err(crate::util::command_error)?;
            // A database's views are the documents of `system.views`, which
            // mongod checks as it does any collection.
            let has_views = !crate::ts_buckets::view_definitions(storage.as_ref(), &db)?.is_empty();
            if has_views && !colls.iter().any(|c| c == "system.views") {
                colls.push("system.views".to_string());
            }
            // mongod walks its catalog in its own internal order; this is
            // name order.
            colls.sort();
            for coll in colls {
                // `system.views` answers the catalog's question for its views,
                // so it is asked by name.
                if coll != "system.views" && crate::views::is_view(storage.as_ref(), &db, &coll) {
                    continue;
                }
                let ns = format!("{db}.{coll}");
                let mut md5 = Md5::new();
                for (_, raw) in sorted_rows(storage.as_ref(), &db, &coll)? {
                    md5.update(&raw);
                }
                let digest: String = md5.finalize().iter().map(|b| format!("{b:02x}")).collect();
                storage
                    .append_command_entry(
                        &ns,
                        doc! {
                            "dbCheck": ns.clone(),
                            "type": "batch",
                            "md5": digest,
                            "minKey": Bson::MinKey,
                            "maxKey": Bson::MaxKey,
                            "readTimestamp": Bson::Timestamp(storage.current_cluster_time()),
                        },
                    )
                    .map_err(crate::util::command_error)?;
            }
            storage
                .append_command_entry("admin.$cmd", doc! {"dbCheck": "admin.$cmd", "type": "stop"})
                .map_err(crate::util::command_error)?;
            healthlog(storage.as_ref(), "dbCheckStop", None)?;
        }
    }
    Ok(doc! {"ok": 1.0})
}

/// The collection's documents in `_id` order, each with its `_id`.
fn sorted_rows(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
) -> Result<Vec<(Bson, Vec<u8>)>, CommandError> {
    // The view definitions are catalog rows here, read as mongod stores them.
    if coll == "system.views" {
        let mut defs = crate::ts_buckets::view_definitions(storage, db)?;
        defs.sort_by(|a, b| {
            a.get_str("_id")
                .unwrap_or("")
                .cmp(b.get_str("_id").unwrap_or(""))
        });
        return defs
            .into_iter()
            .map(|d| {
                let raw = bson::to_vec(&d).map_err(|e| error(1, e.to_string()))?;
                Ok((d.get("_id").cloned().unwrap_or(Bson::Null), raw))
            })
            .collect();
    }
    let rows = storage
        .find(db, coll, &Document::new(), Some(&doc! {"_id": 1}), None)
        .map_err(crate::util::command_error)?;
    rows.into_iter()
        .map(|raw| {
            let d = crate::bsondoc::from_bytes(&raw).map_err(|e| error(1, e.to_string()))?;
            Ok((d.get("_id").cloned().unwrap_or(Bson::Null), raw))
        })
        .collect()
}

/// A record in `local.system.healthlog`, created as mongod creates it.
fn healthlog(
    storage: &dyn crate::Storage,
    operation: &str,
    collection: Option<(&String, &Bson, &Document)>,
) -> Result<(), CommandError> {
    if !storage
        .collection_exists("local", HEALTHLOG)
        .map_err(crate::util::command_error)?
    {
        storage
            .create_collection_with_options(
                "local",
                HEALTHLOG,
                &doc! {"capped": true, "size": 100_000_000_i64, "autoIndexId": false},
            )
            .map_err(crate::util::command_error)?;
    }
    let mut record = doc! {"_id": ObjectId::new()};
    if let Some((ns, ui, _)) = collection {
        record.insert("namespace", ns.clone());
        record.insert("collectionUUID", ui.clone());
    }
    record.insert("timestamp", bson::DateTime::now());
    record.insert("severity", "info");
    record.insert("msg", "");
    record.insert("scope", "cluster");
    record.insert("operation", operation);
    if let Some((_, _, params)) = collection {
        record.insert("data", doc! {"dbCheckParameters": params.clone()});
    }
    let bytes = bson::to_vec(&record).map_err(|e| error(1, e.to_string()))?;
    storage
        .insert("local", HEALTHLOG, vec![bytes], true)
        .map_err(crate::util::command_error)?;
    Ok(())
}
