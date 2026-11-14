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
                Bson::String(s) => {
                    return Err(error(
                        2,
                        format!(
                        "Enumeration value '{s}' for field '.validateMode' is not a valid value."
                    ),
                    ))
                }
                other => {
                    return Err(error(
                        14,
                        format!(
                        "BSON field '.validateMode' is the wrong type '{}', expected type 'string'",
                        secantus_core::query::bson_type_name(other)
                    ),
                    ))
                }
            }
        }
    }
    if doc.get_str("validateMode") == Ok("extraIndexKeysCheck")
        && doc.get_str("secondaryIndex").unwrap_or("").is_empty()
    {
        return Err(error(
            72,
            "When validateMode is set to extraIndexKeysCheck, the secondaryIndex parameter \
             must be set.",
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
            if doc.get_str("validateMode") == Ok("extraIndexKeysCheck") {
                return extra_index_keys(doc, storage.as_ref(), &db, &coll, &ns, &ui, &params);
            }
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
            // A database's views are the documents of `system.views`, which
            // mongod checks as it does any collection.
            let mut colls = crate::views::catalog_collections(storage.as_ref(), &db)?;
            // mongod walks its catalog in its own internal order; this is
            // name order.
            colls.sort();
            for coll in colls {
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
                let raw = crate::bsondoc::to_vec(&d).map_err(|e| error(1, e.to_string()))?;
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
    write_healthlog(storage, record)
}

/// The `dbCheckBatch` warning mongod records when it abandons an
/// `extraIndexKeysCheck`.
fn healthlog_warning(
    storage: &dyn crate::Storage,
    (ns, ui, params): (&String, &Bson, &Document),
    msg: &str,
    err: String,
) -> Result<(), CommandError> {
    let record = doc! {
        "_id": ObjectId::new(),
        "namespace": ns.clone(),
        "collectionUUID": ui.clone(),
        "timestamp": bson::DateTime::now(),
        "severity": "warning",
        "msg": msg,
        "scope": "cluster",
        "operation": "dbCheckBatch",
        "data": {
            "success": false,
            "error": err,
            "context": {},
            "dbCheckParameters": params.clone(),
        },
    };
    write_healthlog(storage, record)
}

fn write_healthlog(storage: &dyn crate::Storage, record: Document) -> Result<(), CommandError> {
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
    let bytes = crate::bsondoc::to_vec(&record).map_err(|e| error(1, e.to_string()))?;
    storage
        .insert("local", HEALTHLOG, vec![bytes], true)
        .map_err(crate::util::command_error)?;
    Ok(())
}

/// One index field: its path, direction, and whether it is hashed.
struct KeyField {
    path: String,
    descending: bool,
    hashed: bool,
}

/// `dbCheck` with `validateMode: "extraIndexKeysCheck"` (RM-416), as
/// mongod 8.2.11's primary runs it: the named index's keys in index order,
/// from `start` to `end` (the first field's MinKey to its MaxKey), in
/// batches of at least `maxDocsPerBatch` keys that end only where the key
/// changes, each an oplog `batch` entry carrying the MD5 of its keys'
/// KeyString bytes. Measured 2026-11-14.
fn extra_index_keys(
    doc: &Document,
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
    ns: &String,
    ui: &Bson,
    params: &Document,
) -> HandlerResult {
    let index_name = doc.get_str("secondaryIndex").unwrap_or("");
    let specs = storage
        .list_indexes(db, coll)
        .map_err(crate::util::command_error)?;
    let spec = specs
        .iter()
        .find(|s| s.get_str("name") == Ok(index_name))
        .cloned();
    let key_pattern = spec
        .as_ref()
        .and_then(|s| s.get_document("key").ok())
        .cloned()
        .unwrap_or_default();
    // What the index's key pattern makes of the check, before anything is
    // written: a kind with no key generator here is refused by name.
    let mut fields = Vec::new();
    let mut unsupported_kind = false;
    for (path, v) in &key_pattern {
        let (descending, hashed) = match v {
            Bson::String(s) if s == "hashed" => (false, true),
            Bson::String(s) if s == "text" => {
                unsupported_kind = true;
                (false, false)
            }
            Bson::String(s) => {
                return Err(error(
                    115,
                    format!("dbCheck extraIndexKeysCheck of a '{s}' index is not supported"),
                ))
            }
            _ if path == "$**" || path.ends_with(".$**") => {
                return Err(error(
                    115,
                    "dbCheck extraIndexKeysCheck of a wildcard index is not supported",
                ))
            }
            Bson::Double(n) => (*n < 0.0, false),
            Bson::Decimal128(n) => (n.to_string().starts_with('-'), false),
            other => (crate::util::as_i64(other).is_some_and(|n| n < 0), false),
        };
        fields.push(KeyField {
            path: path.clone(),
            descending,
            hashed,
        });
    }
    let collation = spec
        .as_ref()
        .and_then(|s| s.get_document("collation").ok())
        .and_then(secantus_core::collation::parse);
    let sparse = spec
        .as_ref()
        .is_some_and(|s| s.get_bool("sparse").unwrap_or(false));
    let partial = spec
        .as_ref()
        .and_then(|s| s.get_document("partialFilterExpression").ok())
        .cloned();

    // Every key, before the first entry is written: a value the encoder
    // cannot write refuses the command rather than half a check.
    let mut keys: Vec<(Vec<u8>, Document)> = Vec::new();
    if spec.is_some() && !unsupported_kind {
        for (_, raw) in sorted_rows(storage, db, coll)? {
            let d = crate::bsondoc::from_bytes(&raw).map_err(|e| error(1, e.to_string()))?;
            doc_keys(
                &d,
                &fields,
                sparse,
                partial.as_ref(),
                collation.as_ref(),
                &mut keys,
            )?;
        }
    }
    keys.sort_by(|a, b| a.0.cmp(&b.0));

    let first = fields
        .first()
        .map(|f| f.path.clone())
        .unwrap_or_else(|| "_id".to_string());
    let start_doc = doc
        .get_document("start")
        .ok()
        .cloned()
        .unwrap_or_else(|| doc! {first.clone(): Bson::MinKey});
    let end_doc = doc
        .get_document("end")
        .ok()
        .cloned()
        .unwrap_or_else(|| doc! {first.clone(): Bson::MaxKey});
    let bound = |d: &Document| -> Result<Vec<u8>, CommandError> {
        let vals: Vec<(&Bson, bool)> = d
            .iter()
            .zip(fields.iter())
            .map(|((_, v), f)| (v, f.descending))
            .collect();
        let mut b = crate::key_string::encode_key(&vals, collation.as_ref())
            .map_err(|u| error(115, format!("dbCheck bound holds {}, not supported", u.0)))?;
        b.pop(); // the bound is a prefix: no kEnd
        Ok(b)
    };
    let lo = bound(&start_doc)?;
    let hi = bound(&end_doc)?;
    keys.retain(|(k, _)| {
        k.as_slice() >= lo.as_slice() && (k.as_slice() <= hi.as_slice() || k.starts_with(&hi))
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
    healthlog(storage, "dbCheckStart", Some((ns, ui, params)))?;
    storage
        .append_command_entry("admin.$cmd", marker("start"))
        .map_err(crate::util::command_error)?;
    let uuid_text = match ui {
        Bson::Binary(b) => {
            bson::uuid::Uuid::from_bytes(b.bytes.clone().try_into().unwrap_or([0; 16])).to_string()
        }
        _ => String::new(),
    };
    if spec.is_none() {
        healthlog_warning(
            storage,
            (ns, ui, params),
            "abandoning dbCheck extra index keys check because index no longer exists",
            format!(
                "IndexNotFound: cannot find index {index_name} for ns {ns} and uuid {uuid_text}"
            ),
        )?;
    } else if unsupported_kind {
        healthlog_warning(
            storage,
            (ns, ui, params),
            "abandoning dbCheck extra index keys check because index type is not supported",
            format!(
                "IndexOptionsConflict: index type is not supported, indexName: {index_name} \
                 for ns {ns} and uuid {uuid_text}"
            ),
        )?;
    } else {
        let mut at = 0usize;
        let mut checked = 0usize;
        let mut batch_start = start_doc.clone();
        loop {
            let begin = at;
            while at < keys.len() {
                at += 1;
                if at - begin >= per_batch && (at >= keys.len() || keys[at].0 != keys[at - 1].0) {
                    break;
                }
            }
            let batch = &keys[begin..at];
            let reached_end = at >= keys.len();
            let last = batch.last().map(|(_, d)| d.clone()).unwrap_or_default();
            let batch_end = if reached_end {
                end_doc.clone()
            } else {
                last.clone()
            };
            let mut md5 = Md5::new();
            for (k, _) in batch {
                md5.update(k);
            }
            let digest: String = md5.finalize().iter().map(|b| format!("{b:02x}")).collect();
            storage
                .append_command_entry(
                    ns,
                    doc! {
                        "dbCheck": ns.clone(),
                        "type": "batch",
                        "md5": digest,
                        "minKey": Bson::MinKey,
                        "maxKey": Bson::MaxKey,
                        "batchStart": batch_start.clone(),
                        "batchEnd": batch_end,
                        "readTimestamp": Bson::Timestamp(storage.current_cluster_time()),
                        "secondaryIndexCheckParameters": params.clone(),
                        "logBatchToHealthLog": false,
                        "batchId": Bson::Binary(Binary {
                            subtype: BinarySubtype::Uuid,
                            bytes: bson::uuid::Uuid::new().bytes().to_vec(),
                        }),
                        "lastKeyChecked": last,
                    },
                )
                .map_err(crate::util::command_error)?;
            checked += batch.len();
            if reached_end || max_count.is_some_and(|m| checked >= m) {
                break;
            }
            batch_start = keys[at].1.clone();
        }
    }
    storage
        .append_command_entry("admin.$cmd", marker("stop"))
        .map_err(crate::util::command_error)?;
    healthlog(storage, "dbCheckStop", Some((ns, ui, params)))?;
    Ok(doc! {"ok": 1.0})
}

/// The keys one document gives the index, each once, as KeyString bytes and
/// as the key document mongod reports (`lastKeyChecked`).
fn doc_keys(
    d: &Document,
    fields: &[KeyField],
    sparse: bool,
    partial: Option<&Document>,
    collation: Option<&secantus_core::collation::Collation>,
    out: &mut Vec<(Vec<u8>, Document)>,
) -> Result<(), CommandError> {
    if let Some(filter) = partial {
        let hit =
            secantus_core::query::matches(d, filter, &Document::new(), None).map_err(|_| {
                error(
                    1,
                    "dbCheck could not evaluate the index's partialFilterExpression",
                )
            })?;
        if !hit {
            return Ok(());
        }
    }
    if sparse
        && fields
            .iter()
            .all(|f| secantus_core::get_path_values(d, &f.path).0.is_empty())
    {
        return Ok(());
    }
    // Each field's values; a multikey document gives their product.
    let per_field: Vec<Vec<Bson>> = fields
        .iter()
        .map(|f| {
            let keys = secantus_core::index_keys::field_keys(d, &f.path, sparse).mongod;
            if f.hashed {
                let v = keys.into_iter().next().unwrap_or(Bson::Null);
                vec![Bson::Int64(secantus_core::hashed::hash64(&v, false))]
            } else {
                keys
            }
        })
        .collect();
    let mut combos: Vec<Vec<&Bson>> = vec![Vec::new()];
    for vals in &per_field {
        let mut next = Vec::with_capacity(combos.len() * vals.len());
        for c in &combos {
            for v in vals {
                let mut c = c.clone();
                c.push(v);
                next.push(c);
            }
        }
        combos = next;
    }
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for combo in combos {
        let vals: Vec<(&Bson, bool)> = combo
            .iter()
            .zip(fields)
            .map(|(v, f)| (*v, f.descending))
            .collect();
        let bytes = crate::key_string::encode_key(&vals, collation).map_err(|u| {
            error(
                115,
                format!(
                    "dbCheck extraIndexKeysCheck of an index key holding {} is not supported",
                    u.0
                ),
            )
        })?;
        if seen.contains(&bytes) {
            continue;
        }
        seen.push(bytes.clone());
        let mut key = Document::new();
        for (v, f) in combo.iter().zip(fields) {
            let shown = match (v, collation) {
                (Bson::String(s), Some(c)) => {
                    let k = secantus_core::collation::sort_key(s, c);
                    let k = k.strip_suffix(&[0]).unwrap_or(&k).to_vec();
                    crate::bsondoc::string_from_bytes(k)
                }
                _ => (*v).clone(),
            };
            key.insert(f.path.clone(), shown);
        }
        out.push((bytes, key));
    }
    Ok(())
}
