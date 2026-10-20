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
