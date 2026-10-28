//! `logRotate`, `connPoolStats` and `setFeatureCompatibilityVersion`: the
//! cluster-administration commands a tool sends a single node. Every reply
//! and message is what mongod 8.2.11 answered (2026-10-14,
//! `tools/probes/admin_commands.py`).

use bson::{doc, Bson, Document};

use crate::{CommandContext, CommandError, HandlerResult};

fn type_mismatch(msg: String) -> CommandError {
    CommandError::new(14, "TypeMismatch", msg)
}

fn unknown_field(path: &str, field: &str) -> CommandError {
    CommandError::new(
        40415,
        "IDLUnknownField",
        format!("BSON field '{path}.{field}' is an unknown field."),
    )
}

fn type_name(v: &Bson) -> &'static str {
    secantus_core::query::bson_type_name(v)
}

/// A bool-or-number field, read as mongod's `safeBool`: any number is its
/// truth, null is absent.
fn safe_bool(path: &str, field: &str, value: &Bson) -> Result<Option<bool>, CommandError> {
    match value {
        Bson::Null => Ok(None),
        Bson::Boolean(b) => Ok(Some(*b)),
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
            Ok(Some(crate::util::truthy(value)))
        }
        v => Err(type_mismatch(format!(
            "BSON field '{path}.{field}' is the wrong type '{}', expected types '[decimal, \
             bool, int, double, long]'",
            type_name(v)
        ))),
    }
}

/// `logRotate`: this server writes its log to the stream it was started
/// with and has no file to rotate, which is also what a mongod started
/// without `--logpath` has; both answer `ok`. The value names what to
/// rotate: a bool or number is the server log, and `"server"` is too; any
/// other name is one mongod has no log for.
pub fn log_rotate(doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    for (key, _) in doc {
        match key.as_str() {
            "logRotate" => {}
            k if k.starts_with('$') || crate::argtypes::is_generic_field(k) => {}
            k => return Err(unknown_field("logRotate", k)),
        }
    }
    match doc.get("logRotate") {
        Some(Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))
        | Some(Bson::Decimal128(_)) => {}
        Some(Bson::String(which)) if which == "server" => {}
        Some(Bson::String(_)) => {
            return Err(CommandError::new(
                4,
                "NoSuchKey",
                "Log rotation failed due to one or more errors",
            ))
        }
        Some(v) => {
            return Err(type_mismatch(format!(
                "BSON field 'logRotate.logRotate' is the wrong type '{}', expected types \
                 '[bool, string]'",
                type_name(v)
            )))
        }
        None => {}
    }
    Ok(doc! {"ok": 1.0})
}

/// mongod's histogram of how long a connection took to acquire: 22
/// buckets and their total, all longs.
fn acquisition_wait_times() -> Document {
    let mut out = doc! {"(-inf, 0 ms)": {"count": 0_i64}};
    for lo in (0..1000).step_by(50) {
        out.insert(format!("[{lo} ms, {} ms)", lo + 50), doc! {"count": 0_i64});
    }
    out.insert("[1000 ms, inf)", doc! {"count": 0_i64});
    out.insert("totalCount", 0_i64);
    out
}

/// `connPoolStats`: the pools of connections this node holds to other
/// nodes. There are none -- this server talks to no other node -- so every
/// counter is zero and the per-pool, per-host and per-set maps are empty,
/// in mongod's field order and types. (A replica-set mongod lists a pool
/// to itself here; a standalone lists none.)
pub fn conn_pool_stats(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! {
        "numClientConnections": 0_i32,
        "numAScopedConnections": 0_i32,
        "totalInUse": 0_i32,
        "totalAvailable": 0_i32,
        "totalLeased": 0_i32,
        "totalCreated": 0_i32,
        "totalRefreshing": 0_i32,
        "totalRefreshed": 0_i32,
        "totalWasNeverUsed": 0_i32,
        "totalRejectedRequests": 0_i32,
        "totalPendingRequests": 0_i32,
        "acquisitionWaitTimes": acquisition_wait_times(),
        "pools": {},
        "hosts": {},
        "numReplicaSetMonitorsCreated": 0_i32,
        "replicaSets": {},
        "ok": 1.0,
    })
}

/// The feature compatibility versions a mongod of this version accepts,
/// newest first: its own, the last continuous one, and the last LTS.
const FCV_LATEST: &str = "8.2";
const FCV_LAST_CONTINUOUS: &str = "8.1";
const FCV_LAST_LTS: &str = "8.0";

/// The collection and document mongod keeps the FCV in.
const FCV_DB: &str = "admin";
const FCV_COLL: &str = "system.version";
const FCV_ID: &str = "featureCompatibilityVersion";

/// Record the FCV as mongod does when it starts on a store with none:
/// `{_id: "featureCompatibilityVersion", version: "8.2"}` in
/// `admin.system.version`. That document is why mongod always lists the
/// `admin` database. A stored FCV is left as it is.
pub fn record_initial_fcv(
    storage: &dyn crate::storage::Storage,
) -> Result<(), crate::storage::StorageError> {
    let stored = storage.find(FCV_DB, FCV_COLL, &doc! {"_id": FCV_ID}, None, None)?;
    if stored.is_empty() {
        let bytes = crate::bsondoc::to_vec(&doc! {"_id": FCV_ID, "version": FCV_LATEST})
            .map_err(|e| crate::storage::StorageError::Internal(e.to_string()))?;
        storage.insert(FCV_DB, FCV_COLL, vec![bytes], true)?;
    }
    Ok(())
}

/// The collection mongod keeps logical sessions in, and its TTL index.
const SESSIONS_DB: &str = "config";
const SESSIONS_COLL: &str = "system.sessions";
const SESSIONS_TTL_INDEX: &str = "lsidTTLIndex";
/// mongod's `localLogicalSessionTimeoutMinutes` default (30), in seconds.
const SESSIONS_TTL_SECONDS: i32 = 1800;

/// Create `config.system.sessions` as mongod's logical session cache sets
/// it up: the collection, plus `lsidTTLIndex` `{lastUse: 1}` with
/// `expireAfterSeconds: 1800`. A standalone mongod does so as it starts, and
/// that collection is why a fresh standalone lists the `config` database
/// (measured 8.2.11, 2026-10-16: `config` holds `system.sessions` alone); a
/// replica-set member does so on its first refresh, a
/// `logicalSessionRefreshMillis` after it steps up (measured 2026-10-28:
/// 299 s under the default). A collection that is already there is left as
/// it is.
///
/// The logical session cache's refresh sets it up again when it is dropped
/// (`logical_sessions::refresh`), with the TTL the server was started with.
pub fn record_sessions_collection(
    storage: &dyn crate::storage::Storage,
) -> Result<(), crate::storage::StorageError> {
    record_sessions_collection_ttl(storage, SESSIONS_TTL_SECONDS.into())
}

/// [`record_sessions_collection`] with `lsidTTLIndex` expiring records
/// after `ttl_seconds` (`localLogicalSessionTimeoutMinutes`). An index of
/// another TTL is changed to this one, as mongod's `collMod` does.
pub fn record_sessions_collection_ttl(
    storage: &dyn crate::storage::Storage,
    ttl_seconds: i64,
) -> Result<(), crate::storage::StorageError> {
    let ttl = Bson::Int32(i32::try_from(ttl_seconds).unwrap_or(i32::MAX));
    let key = doc! {"lastUse": 1_i32};
    if !storage.collection_exists(SESSIONS_DB, SESSIONS_COLL)? {
        storage.create_collection(SESSIONS_DB, SESSIONS_COLL)?;
    }
    let indexes = storage.list_indexes(SESSIONS_DB, SESSIONS_COLL)?;
    match indexes
        .iter()
        .find(|ix| ix.get_str("name") == Ok(SESSIONS_TTL_INDEX))
    {
        None => {
            let spec = doc! {
                "key": key.clone(),
                "name": SESSIONS_TTL_INDEX,
                "expireAfterSeconds": ttl,
            };
            storage.create_index(SESSIONS_DB, SESSIONS_COLL, SESSIONS_TTL_INDEX, &key, &spec)?;
        }
        Some(ix) => {
            let current = ix.get("expireAfterSeconds").and_then(crate::util::as_i64);
            if current != Some(ttl_seconds) {
                storage.set_index_options(
                    SESSIONS_DB,
                    SESSIONS_COLL,
                    SESSIONS_TTL_INDEX,
                    &doc! {"expireAfterSeconds": ttl},
                )?;
            }
        }
    }
    Ok(())
}

/// The query sampler's collections mongod 8.2.11 creates as it steps up,
/// each with a TTL index on `expireAt` that expires a document at that time
/// (measured 2026-10-28, RM-366).
const STEP_UP_TTL_COLLECTIONS: [(&str, &str); 3] = [
    (
        "analyzeShardKeySplitPoints",
        "AnalyzeShardKeySplitPointsTTLIndex",
    ),
    ("sampledQueries", "SampledQueriesTTLIndex"),
    ("sampledQueriesDiff", "SampledQueriesDiffTTLIndex"),
];

/// Create the collections a replica-set primary keeps its sessions' state
/// in, as mongod 8.2.11 creates them when it becomes primary:
/// `config.transactions` (with its `parent_lsid` index) and
/// `config.image_collection`; and the others it creates then (RM-366):
/// `config.system.indexBuilds`, where it records the index builds it would
/// resume, and the query sampler's three, each with its `expireAt` TTL
/// index. Here that is when the server starts on a store that lacks them;
/// one already there is left as it is. (`config.system.preimages`, which
/// mongod also creates, is where it keeps change stream pre-images; this
/// server keeps them elsewhere, and RM-366 moves them there.)
pub fn record_session_state_collections(
    storage: &dyn crate::storage::Storage,
) -> Result<(), crate::storage::StorageError> {
    if !storage.collection_exists(SESSIONS_DB, "transactions")? {
        storage.create_collection(SESSIONS_DB, "transactions")?;
        let key = doc! {"parentLsid": 1_i32, "_id.txnNumber": 1_i32, "_id": 1_i32};
        let spec = doc! {
            "key": key.clone(),
            "name": "parent_lsid",
            "partialFilterExpression": {"parentLsid": {"$exists": true}},
        };
        storage.create_index(SESSIONS_DB, "transactions", "parent_lsid", &key, &spec)?;
    }
    if !storage.collection_exists(SESSIONS_DB, "image_collection")? {
        storage.create_collection(SESSIONS_DB, "image_collection")?;
    }
    if !storage.collection_exists(SESSIONS_DB, "system.indexBuilds")? {
        storage.create_collection(SESSIONS_DB, "system.indexBuilds")?;
    }
    for (coll, index) in STEP_UP_TTL_COLLECTIONS {
        if storage.collection_exists(SESSIONS_DB, coll)? {
            continue;
        }
        storage.create_collection(SESSIONS_DB, coll)?;
        let key = doc! {"expireAt": 1_i32};
        let spec = doc! {"key": key.clone(), "name": index, "expireAfterSeconds": 0_i32};
        storage.create_index(SESSIONS_DB, coll, index, &key, &spec)?;
    }
    Ok(())
}

/// The FCV in force: the stored one, else this server's own version. What
/// `getParameter` reports as `featureCompatibilityVersion.version`.
pub(crate) fn current_fcv(ctx: &CommandContext) -> String {
    if let Some(known) = ctx.server_params.as_ref().and_then(|p| p.fcv()) {
        return known;
    }
    let stored = ctx.storage().ok().and_then(|storage| {
        storage
            .find(FCV_DB, FCV_COLL, &doc! {"_id": FCV_ID}, None, None)
            .ok()?
            .first()
            .and_then(|bytes| crate::bsondoc::from_bytes(bytes).ok())?
            .get_str("version")
            .ok()
            .map(str::to_string)
    });
    let version = stored.unwrap_or_else(|| FCV_LATEST.to_string());
    if let Some(p) = &ctx.server_params {
        p.set_fcv(&version);
    }
    version
}

/// `setFeatureCompatibilityVersion`: record the version in
/// `admin.system.version`, where mongod records it and `getParameter`
/// reads it back. mongod's rules, in its order: the other fields are
/// checked before the version itself; the version must be one this binary
/// knows; `dryRun` is refused; a change needs `confirm`; and the last
/// continuous version cannot be set from either of the other two.
///
/// At 8.0 the expressions 8.2 added are refused, as mongod refuses them
/// (`secantus_core::expressions::ADDED_IN_82`, gated around every command in
/// `run_handler`).
pub fn set_feature_compatibility_version(
    doc: &Document,
    ctx: &mut CommandContext,
) -> HandlerResult {
    const PATH: &str = "setFeatureCompatibilityVersion";
    let mut confirm = None;
    let mut dry_run = None;
    for (key, value) in doc {
        match key.as_str() {
            PATH => {}
            "confirm" => confirm = safe_bool(PATH, key, value)?,
            "dryRun" => dry_run = safe_bool(PATH, key, value)?,
            "fromConfigServer" => {
                safe_bool(PATH, key, value)?;
            }
            k if k.starts_with('$') || crate::argtypes::is_generic_field(k) => {}
            k => return Err(unknown_field(PATH, k)),
        }
    }
    let target = match doc.get(PATH) {
        Some(Bson::String(s)) => s.clone(),
        None | Some(Bson::Null) => {
            return Err(CommandError::new(
                40414,
                "IDLFailedToParse",
                format!("BSON field '{PATH}.{PATH}' is missing but a required field"),
            ))
        }
        Some(v) => {
            return Err(type_mismatch(format!(
                "BSON field '{PATH}.{PATH}' is the wrong type '{}', expected type 'string'",
                type_name(v)
            )))
        }
    };
    if !matches!(
        target.as_str(),
        FCV_LATEST | FCV_LAST_CONTINUOUS | FCV_LAST_LTS
    ) {
        // The doubled quote before the first version is mongod's own.
        return Err(CommandError::new(
            4926900,
            "Location4926900",
            format!(
                "Invalid feature compatibility version value '{target}'. Expected one of the \
                 following versions: ''{FCV_LATEST}', '{FCV_LAST_CONTINUOUS}', '{FCV_LAST_LTS}'. \
                 See https://docs.mongodb.com/master/release-notes/8.0-compatibility/\
                 #feature-compatibility."
            ),
        ));
    }
    if dry_run.is_some() {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "dry-run mode is not enabled",
        ));
    }
    let current = current_fcv(ctx);
    if confirm != Some(true) {
        let msg = if target.as_str() > current.as_str() {
            format!(
                "Once you have upgraded to {target}, you will not be able to downgrade FCV and \
                 binary version without support assistance. Please re-run this command with \
                 'confirm: true' to acknowledge this and continue with the FCV upgrade."
            )
        } else {
            "Once you have downgraded the FCV, if you choose to downgrade the binary version, \
             it will require support assistance. Please re-run this command with 'confirm: \
             true' to acknowledge this and continue with the FCV downgrade."
                .to_string()
        };
        return Err(CommandError::new(7369100, "Location7369100", msg));
    }
    if target == FCV_LAST_CONTINUOUS && current != FCV_LAST_CONTINUOUS {
        return Err(CommandError::new(
            5147403,
            "Location5147403",
            format!(
                "cannot set featureCompatibilityVersion to '{target}' while \
                 featureCompatibilityVersion is '{current}'"
            ),
        ));
    }
    ctx.storage()?
        .update_matching(
            FCV_DB,
            FCV_COLL,
            &doc! {"_id": FCV_ID},
            &doc! {"_id": FCV_ID, "version": target.clone()},
            false,
            true,
        )
        .map_err(crate::util::command_error)?;
    if let Some(p) = &ctx.server_params {
        p.set_fcv(&target);
    }
    Ok(doc! {"ok": 1.0})
}
