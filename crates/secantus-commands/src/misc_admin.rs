//! Administrative commands with a short answer on a single node (RM-219).
//! Each reply was measured against mongod 8.2.11 on a one-member replica set
//! (2026-10-20), and each is true of this server, not a stand-in: it keeps
//! no egress connection pool, no storage resources on an idle cursor, and
//! has never rolled back.

use bson::{doc, Bson, Document};
use md5::{Digest, Md5};

use crate::{CommandContext, CommandError, HandlerResult};

fn wrong_type(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
            secantus_core::query::bson_type_name(value)
        ),
    )
}

/// `invalidateUserCache`: every connection's privileges are read again on
/// its next command.
pub fn invalidate_user_cache(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    if let Some(p) = &ctx.server_params {
        p.invalidate_user_cache();
    }
    Ok(doc! {"ok": 1.0})
}

/// `replSetGetRBID`: the rollback id, 1 on a node that has never rolled
/// back -- which a single node never does.
pub fn repl_set_get_rbid(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! {"rbid": 1_i32, "ok": 1.0})
}

/// `replSetFreeze`: a primary cannot be frozen.
pub fn repl_set_freeze(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Err(CommandError::new(
        95,
        "NotSecondary",
        "Cannot freeze node when primary or running for election, state: Primary",
    ))
}

/// `replSetMaintenance`: a primary cannot enter maintenance mode.
pub fn repl_set_maintenance(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Err(CommandError::new(
        95,
        "NotSecondary",
        "primaries can't modify maintenance mode",
    ))
}

/// `dropConnections`: drop the egress connections to the hosts named. This
/// server opens none, so there is nothing to drop once the list is read.
pub fn drop_connections(doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    match doc.get("hostAndPort") {
        Some(Bson::Array(_)) => Ok(doc! {"ok": 1.0}),
        None | Some(Bson::Null) => Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            "BSON field 'dropConnections.hostAndPort' is missing but a required field",
        )),
        Some(other) => Err(wrong_type("dropConnections.hostAndPort", other, "array")),
    }
}

/// `releaseMemory`: let go of what an idle cursor holds of the storage
/// engine. A cursor here holds none between batches, so every live one is
/// released as it is and keeps answering `getMore`.
pub fn release_memory(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let ids = match doc.get("releaseMemory") {
        Some(Bson::Array(ids)) => ids,
        Some(other) => return Err(wrong_type("releaseMemory.releaseMemory", other, "array")),
        None => {
            return Err(wrong_type(
                "releaseMemory.releaseMemory",
                &Bson::Null,
                "array",
            ))
        }
    };
    let mut released = Vec::new();
    let mut not_found = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let Bson::Int64(id) = id else {
            return Err(wrong_type(
                &format!("releaseMemory.releaseMemory.{i}"),
                id,
                "long",
            ));
        };
        let live = ctx.cursors.as_ref().is_some_and(|c| c.owner(*id).is_some());
        if live {
            released.push(Bson::Int64(*id));
        } else {
            not_found.push(Bson::Int64(*id));
        }
    }
    Ok(doc! {
        "cursorsReleased": released,
        "cursorsNotFound": not_found,
        "cursorsCurrentlyPinned": [],
        "cursorsWithErrors": [],
        "ok": 1.0,
    })
}

/// `filemd5`: the MD5 of a GridFS file's chunks, read in `n` order through
/// an index, as mongod reads them (it runs this query with `notablescan`).
pub fn filemd5(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let files_id = doc.get("filemd5").cloned().unwrap_or(Bson::Null);
    let root = match doc.get("root") {
        None => "fs".to_string(),
        Some(Bson::String(s)) => s.clone(),
        Some(_) => {
            return Err(CommandError::new(
                73,
                "InvalidNamespace",
                "'root' must be of type String",
            ))
        }
    };
    // `partialOk` answers with mongod's MD5 implementation's internal state
    // (`md5state`), which no other implementation reproduces: refused by
    // name rather than answered without it.
    if doc
        .get("partialOk")
        .is_some_and(|v| !matches!(v, Bson::Boolean(false) | Bson::Null))
    {
        return Err(CommandError::new(
            115,
            "CommandNotSupported",
            "filemd5 with partialOk is not supported: its md5state is mongod's own MD5 state",
        ));
    }
    let chunks = format!("{root}.chunks");
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    // An index whose leading field is `files_id` or `n` gives the query a
    // plan; without one, mongod's `notablescan` refuses it.
    let indexed = storage
        .list_indexes(&db, &chunks)
        .map_err(crate::util::command_error)?
        .iter()
        .filter_map(|ix| ix.get_document("key").ok())
        .any(|key| {
            matches!(
                key.keys().next().map(String::as_str),
                Some("files_id" | "n")
            )
        });
    if !indexed {
        return Err(CommandError::new(
            291,
            "NoQueryExecutionPlans",
            format!(
                "error processing query: ns={db}.{chunks}Tree: $and\n    files_id $eq {}\n    n \
                 $gte 0\nSort: {{ files_id: 1, n: 1 }}\nProj: {{}}\n planner returned error :: \
                 caused by :: No indexed plans available, and running with 'notablescan' 2",
                crate::argtypes::render_stage_value(&files_id)
            ),
        ));
    }
    let filter = doc! {"files_id": files_id.clone(), "n": {"$gte": 0_i32}};
    let sort = doc! {"files_id": 1_i32, "n": 1_i32};
    let rows = storage
        .find(&db, &chunks, &filter, Some(&sort), None)
        .map_err(crate::util::command_error)?;
    let executor = |code: i32, why: String| {
        CommandError::new(
            code,
            crate::util::error_code_name(code),
            format!("Executor error during filemd5 command :: caused by :: {why}"),
        )
    };
    let mut md5 = Md5::new();
    let mut count: i32 = 0;
    for raw in rows {
        let chunk = secantus_core::bsondoc::from_bytes(&raw)
            .map_err(|e| CommandError::new(1, "InternalError", e.to_string()))?;
        let n = chunk.get("n").and_then(crate::util::as_i64).unwrap_or(-1);
        if n != i64::from(count) {
            return Err(executor(10040, "chunks out of order".to_string()));
        }
        match chunk.get("data") {
            Some(Bson::Binary(b)) => md5.update(&b.bytes),
            // mongod's own spelling, "misisng" included.
            None => {
                return Err(executor(
                    50848,
                    "The element that calls binDataClean() must be type of BinData, but type \
                     of misisng found. Field name is required"
                        .to_string(),
                ))
            }
            Some(other) => {
                return Err(executor(
                    50849,
                    format!(
                        "The element that calls binDataClean() must be type of BinData, but \
                         type of {} found.",
                        secantus_core::query::bson_type_name(other)
                    ),
                ))
            }
        }
        count += 1;
    }
    let digest: String = md5.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(doc! {"numChunks": count, "md5": digest, "ok": 1.0})
}

/// The reasons `setUserWriteBlockMode` takes, as mongod 8.2.11 names them.
const WRITE_BLOCK_REASONS: &[&str] = &[
    "Unspecified",
    "ClusterToClusterMigrationInProgress",
    "DiskUseThresholdExceeded",
];

/// `setUserWriteBlockMode`: block every write to a database other than
/// `admin`, `config` and `local` (371 UserWritesBlocked) until it is lifted
/// with the reason it was set with. The block is refused at the write
/// itself, so a command that would change nothing still succeeds; it
/// persists in `config.user_writes_critical_sections`, as mongod's does, and
/// a user holding `bypassWriteBlockingMode` writes past it. Measured on
/// mongod 8.2.11 (2026-10-20), which refuses it on a standalone.
pub fn set_user_write_block_mode(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let mut global = None;
    let mut reason = "Unspecified".to_string();
    for (k, v) in doc.iter().skip(1) {
        match k.as_str() {
            "global" => match v {
                Bson::Boolean(b) => global = Some(*b),
                other => return Err(wrong_type("setUserWriteBlockMode.global", other, "bool")),
            },
            "reason" => match v {
                Bson::String(s) if WRITE_BLOCK_REASONS.contains(&s.as_str()) => reason = s.clone(),
                Bson::String(s) => {
                    return Err(CommandError::new(
                        2,
                        "BadValue",
                        format!(
                            "Enumeration value '{s}' for field 'setUserWriteBlockMode.reason' is \
                             not a valid value."
                        ),
                    ))
                }
                other => return Err(wrong_type("setUserWriteBlockMode.reason", other, "string")),
            },
            k if crate::params::is_generic_arg(k) => {}
            k => {
                return Err(CommandError::new(
                    40415,
                    "IDLUnknownField",
                    format!("BSON field 'setUserWriteBlockMode.{k}' is an unknown field."),
                ))
            }
        }
    }
    let Some(global) = global else {
        return Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            "BSON field 'setUserWriteBlockMode.global' is missing but a required field",
        ));
    };
    if ctx.replica_set_name.is_none() {
        return Err(CommandError::new(
            20,
            "IllegalOperation",
            "setUserWriteBlockMode cannot be run on standalones",
        ));
    }
    let storage = ctx.storage()?;
    let current = storage.user_writes_blocked();
    match (global, current) {
        (true, Some(current)) if current != reason => Err(CommandError::new(
            20,
            "IllegalOperation",
            format!(
                "Cannot acquire user writes critical section with different options than the \
                 already existing one. reason: {reason}, current: {current}"
            ),
        )),
        (true, Some(_)) => Ok(doc! {"ok": 1.0}),
        (true, None) => {
            storage
                .set_user_write_block(Some(&reason))
                .map_err(crate::util::command_error)?;
            Ok(doc! {"ok": 1.0})
        }
        (false, Some(current)) if current != reason => Err(CommandError::new(
            20,
            "IllegalOperation",
            format!(
                "Cannot release user writes critical section with different reason than the \
                 already existing one. reason: {reason}, current: {current}"
            ),
        )),
        (false, Some(_)) => {
            storage
                .set_user_write_block(None)
                .map_err(crate::util::command_error)?;
            Ok(doc! {"ok": 1.0})
        }
        (false, None) => Ok(doc! {"ok": 1.0}),
    }
}

/// A guard that lets this command's writes pass a user write block: for an
/// authenticated user holding `bypassWriteBlockingMode`. With authorization
/// off no user holds it, and mongod blocks every client (measured 8.2.11).
pub(crate) fn write_block_bypass(ctx: &CommandContext) -> Option<Box<dyn std::any::Any>> {
    if !ctx.require_auth {
        return None;
    }
    let storage = ctx.storage.as_ref()?;
    storage.user_writes_blocked()?;
    let authenticated = ctx.conn_auth.as_ref().is_some_and(|a| {
        a.lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_authenticated()
    });
    if !authenticated {
        return None;
    }
    let az = crate::rbac::effective(ctx).ok()?;
    az.is_authorized(
        &crate::privileges::Resource::Cluster,
        &["bypassWriteBlockingMode"],
    )
    .then(|| storage.bypass_user_write_block())
    .flatten()
}

/// `rotateCertificates`: re-read the TLS certificate, key and CA files and
/// use them for new connections. With TLS off there is nothing to rotate and
/// mongod 8.2.11 answers `ok`; a file that does not load is 140
/// InvalidSSLConfiguration and the certificate in use stays.
pub fn rotate_certificates(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    for (k, v) in doc.iter().skip(1) {
        match k.as_str() {
            "message" => {
                if !matches!(v, Bson::String(_)) {
                    return Err(wrong_type("rotateCertificates.message", v, "string"));
                }
            }
            k if crate::params::is_generic_arg(k) => {}
            k => {
                return Err(CommandError::new(
                    40415,
                    "IDLUnknownField",
                    format!("BSON field 'rotateCertificates.{k}' is an unknown field."),
                ))
            }
        }
    }
    if let Some(rotator) = &ctx.cert_rotator {
        rotator
            .rotate()
            .map_err(|why| CommandError::new(140, "InvalidSSLConfiguration", why))?;
    }
    Ok(doc! {"ok": 1.0})
}

/// `validateDBMetadata`: what in the catalog API Version 1 does not allow,
/// with `apiParameters: {version: "1", strict: true}` (any other API
/// parameters find nothing). Measured on mongod 8.2.11: a `sparse` or `text`
/// index is listed as 323; a view whose pipeline, nested ones included, uses
/// a stage outside the API fails the command with that stage's 323; other
/// index types, options, validators and collection kinds pass. With no `db`
/// every database is read, in name order, and a database or collection that
/// does not exist has nothing to report.
pub fn validate_db_metadata(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let mut api = None;
    let mut db_arg = None;
    let mut coll_arg = None;
    for (k, v) in doc.iter().skip(1) {
        match k.as_str() {
            "apiParameters" => match v {
                Bson::Document(d) => api = Some(d),
                other => {
                    return Err(wrong_type(
                        "validateDBMetadata.apiParameters",
                        other,
                        "object",
                    ))
                }
            },
            "db" => match v {
                Bson::String(s) => db_arg = Some(s.clone()),
                other => return Err(wrong_type("validateDBMetadata.db", other, "string")),
            },
            "collection" => match v {
                Bson::String(s) => coll_arg = Some(s.clone()),
                other => return Err(wrong_type("validateDBMetadata.collection", other, "string")),
            },
            k if crate::params::is_generic_arg(k) => {}
            k => {
                return Err(CommandError::new(
                    40415,
                    "IDLUnknownField",
                    format!("BSON field 'validateDBMetadata.{k}' is an unknown field."),
                ))
            }
        }
    }
    let Some(api) = api else {
        return Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            "BSON field 'validateDBMetadata.apiParameters' is missing but a required field",
        ));
    };
    let strict_v1 = api.get_str("version") == Ok("1") && api.get_bool("strict") == Ok(true);
    let mut errors: Vec<Bson> = Vec::new();
    if strict_v1 {
        let storage = ctx.storage()?;
        let mut dbs = match db_arg {
            Some(db) => vec![db],
            None => storage
                .list_databases()
                .map_err(crate::util::command_error)?,
        };
        dbs.sort();
        for db in dbs {
            let mut colls = storage
                .list_collections(&db)
                .map_err(crate::util::command_error)?;
            colls.sort();
            for coll in colls {
                if coll_arg.as_ref().is_some_and(|c| *c != coll) {
                    continue;
                }
                let options = storage
                    .get_collection_options(&db, &coll)
                    .unwrap_or_default();
                if options.contains_key("viewOn") {
                    // The catalog keeps a view's pipeline as `viewPipeline`.
                    if let Ok(pipeline) = options.get_array("viewPipeline") {
                        if let Some(stage) = crate::first_non_v1_stage(pipeline) {
                            return Err(crate::api_strict_stage_error(&stage));
                        }
                    }
                    continue;
                }
                for ix in storage.list_indexes(&db, &coll).unwrap_or_default() {
                    let sparse = ix.get_bool("sparse").unwrap_or(false);
                    let text = ix
                        .get_document("key")
                        .is_ok_and(|k| k.values().any(|v| v.as_str() == Some("text")));
                    if sparse || text {
                        errors.push(Bson::Document(doc! {
                            "ns": format!("{db}.{coll}"),
                            "code": 323,
                            "codeName": "APIStrictError",
                            "errmsg": format!(
                                "The index with name {} is not allowed in API version 1.",
                                ix.get_str("name").unwrap_or_default()
                            ),
                        }));
                    }
                }
            }
        }
    }
    Ok(doc! {"apiVersionErrors": errors, "ok": 1.0})
}
