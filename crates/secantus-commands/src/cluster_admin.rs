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

/// The FCV in force: the stored one, else this server's own version. What
/// `getParameter` reports as `featureCompatibilityVersion.version`.
pub(crate) fn current_fcv(ctx: &CommandContext) -> String {
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
    stored.unwrap_or_else(|| FCV_LATEST.to_string())
}

/// `setFeatureCompatibilityVersion`: record the version in
/// `admin.system.version`, where mongod records it and `getParameter`
/// reads it back. mongod's rules, in its order: the other fields are
/// checked before the version itself; the version must be one this binary
/// knows; `dryRun` is refused; a change needs `confirm`; and the last
/// continuous version cannot be set from either of the other two.
///
/// What this does NOT do is change behaviour: mongod at FCV 8.0 refuses the
/// features 8.1 and 8.2 added, and this server has no such gates
/// (`tasks/backlog.md` 7.084).
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
            &doc! {"_id": FCV_ID, "version": target},
            false,
            true,
        )
        .map_err(crate::util::command_error)?;
    Ok(doc! {"ok": 1.0})
}
