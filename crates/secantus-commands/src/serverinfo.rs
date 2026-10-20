//! Commands that say what the server and the connection are.
//!
//! `listCommands`, `logout`, `replSetGetConfig`,
//! `shardingState`, `fsyncUnlock`, `connectionStatus`, `whatsmyuri`,
//! `killOp`, `getLog` and `getParameter`, each as mongod 8.2.11 answers it
//! (`tools/probes/diagnostic_commands.py`, 2026-10-13). Before that probe
//! five of these answered `CommandNotFound`, `whatsmyuri` said port 0 to
//! every client, and `getParameter` answered `ok: 1` and nothing else for a
//! parameter it did not have.

use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};
use secantus_core::query::bson_type_name;

use crate::mongod_commands::{self, ADMIN_ONLY, API_V1, REQUIRES_AUTH, SECONDARY_OK};
use crate::{CommandContext, CommandError, HandlerResult};

fn wrong_type(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected {expected}",
            bson_type_name(value)
        ),
    )
}

/// A field where mongod takes a bool or any number: how it reads, `false`
/// when absent or null, and mongod's refusal of any other type.
pub(crate) fn flag(doc: &Document, command: &str, field: &str) -> Result<bool, CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null) => Ok(false),
        Some(v) => crate::util::truthy_number_or_bool(v).ok_or_else(|| {
            wrong_type(
                &format!("{command}.{field}"),
                v,
                "types '[bool, long, int, decimal, double]'",
            )
        }),
    }
}

/// The argument of `usersInfo` / `rolesInfo`, checked as mongod checks it
/// (8.2.11): `1` for all of the database's, a name, a `{user|role, db}`
/// document, a list of those, or -- for users only, `for_all_dbs` -- a
/// `{forAllDBs: v}` document whose `v` is true in BSON's sense (a `0` or
/// `false` there makes it a `UserName` with a field it does not have).
/// `kind` is `UserName` or `RoleName` and `field` the name's own field.
/// Returns whether the request names its principals exactly, which the
/// detail options need.
pub(crate) fn principal_argument(
    arg: Option<&Bson>,
    kind: &str,
    field: &str,
    for_all_dbs: bool,
) -> Result<bool, CommandError> {
    let bad = |message: String| CommandError::new(2, "BadValue", message);
    let one = |value: &Bson| -> Result<(), CommandError> {
        match value {
            Bson::String(_) => Ok(()),
            Bson::Document(name) => {
                // A `UserName` refuses a field it does not have, before it
                // looks for the ones it needs; a `RoleName` ignores one.
                if let Some(unknown) = name
                    .keys()
                    .find(|k| k.as_str() != field && k.as_str() != "db")
                    .filter(|_| kind == "UserName")
                {
                    // mongod's own message closes no quote.
                    return Err(bad(format!(
                        "{kind} contains an unknown field named: '{unknown}"
                    )));
                }
                for required in [field, "db"] {
                    match name.get(required) {
                        Some(Bson::String(_)) => {}
                        None => {
                            return Err(bad(format!(
                                "{kind} must contain a field named: {required}"
                            )))
                        }
                        Some(other) => {
                            return Err(bad(format!(
                                "{kind} must contain a string field named: {required}. But, \
                                 has type {}",
                                bson_type_name(other)
                            )))
                        }
                    }
                }
                Ok(())
            }
            _ => Err(bad(format!("{kind} must be either a string or an object"))),
        }
    };
    match arg {
        Some(Bson::Int32(1)) | Some(Bson::Int64(1)) => Ok(false),
        Some(Bson::Double(d)) if *d == 1.0 => Ok(false),
        Some(Bson::Document(all))
            if for_all_dbs
                && all.get("forAllDBs").is_some_and(|v| match v {
                    Bson::Null | Bson::Undefined => false,
                    v => crate::util::truthy_number_or_bool(v).unwrap_or(true),
                }) =>
        {
            Ok(false)
        }
        Some(Bson::Array(items)) => {
            items.iter().try_for_each(one)?;
            Ok(true)
        }
        Some(other) => one(other).map(|()| true),
        None => Err(bad(format!("{kind} must be either a string or an object"))),
    }
}

/// `listCommands`: the commands THIS server has, each with mongod's own row
/// for it. A command of this server's own that mongod lacks is listed with
/// what is true of it.
pub fn list_commands(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    let mut commands = Document::new();
    let entry = |help: &str, flags: u8| {
        let mut row = doc! {
            "help": help,
            "requiresAuth": flags & REQUIRES_AUTH != 0,
            "secondaryOk": flags & SECONDARY_OK != 0,
            "adminOnly": flags & ADMIN_ONLY != 0,
        };
        if flags & mongod_commands::SECONDARY_OVERRIDE_OK != 0 {
            row.insert("secondaryOverrideOk", true);
        }
        let versions: Vec<Bson> = if flags & API_V1 != 0 {
            vec![Bson::String("1".into())]
        } else {
            Vec::new()
        };
        row.insert("apiVersions", versions);
        row.insert("deprecatedApiVersions", Vec::<Bson>::new());
        row
    };
    for (name, flags, help) in mongod_commands::COMMANDS {
        if crate::lookup(name).is_some() {
            commands.insert(*name, entry(help, *flags));
        }
    }
    for name in crate::OWN_COMMANDS {
        commands.insert(
            *name,
            entry("A SecantusDB administration command.", REQUIRES_AUTH),
        );
    }
    Ok(doc! { "commands": commands, "ok": 1.0 })
}

/// `logout`: the connection stops being the users it authenticated as on
/// this database. Always `ok: 1`, authenticated or not.
pub fn logout(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    crate::auth::forget_database(ctx);
    Ok(doc! { "ok": 1.0 })
}

/// The one-member set's configuration, in mongod's shape for a
/// configuration nobody has changed.
pub(crate) fn replset_config(
    set_name: &str,
    host_port: &str,
    replica_set_id: bson::oid::ObjectId,
) -> Document {
    doc! {
        "_id": set_name,
        "version": 1_i32,
        "term": 1_i32,
        "members": [{
            "_id": 0_i32,
            "host": host_port,
            "arbiterOnly": false,
            "buildIndexes": true,
            "hidden": false,
            "priority": 1.0,
            "tags": {},
            "secondaryDelaySecs": 0_i64,
            "votes": 1_i32,
        }],
        "protocolVersion": 1_i64,
        "writeConcernMajorityJournalDefault": true,
        "settings": {
            "chainingAllowed": true,
            "heartbeatIntervalMillis": 2000_i32,
            "heartbeatTimeoutSecs": 10_i32,
            "electionTimeoutMillis": 10000_i32,
            "catchUpTimeoutMillis": -1_i32,
            "catchUpTakeoverDelayMillis": 30000_i32,
            "getLastErrorModes": {},
            "getLastErrorDefaults": {"w": 1_i32, "wtimeout": 0_i32},
            "replicaSetId": replica_set_id,
        },
    }
}

/// The configuration stored in `local.system.replset` for `set_name`.
pub(crate) fn stored_replset_config(
    storage: &dyn crate::storage::Storage,
    set_name: &str,
) -> Option<Document> {
    let rows = storage
        .find(
            "local",
            "system.replset",
            &doc! {"_id": set_name},
            None,
            None,
        )
        .ok()?;
    let bytes = rows.first()?;
    secantus_core::bsondoc::from_bytes(bytes).ok()
}

/// The oplog's collection UUID. mongod mints one when the set is initiated
/// and keeps it; here it is derived from the stored `replicaSetId`, so it
/// too is the same after a restart.
pub(crate) fn oplog_uuid(storage: &dyn crate::storage::Storage) -> Vec<u8> {
    use sha2::Digest;
    let id = storage
        .find("local", "system.replset", &doc! {}, None, None)
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|bytes| secantus_core::bsondoc::from_bytes(&bytes).ok())
        .and_then(|d| {
            d.get_document("settings")
                .ok()
                .and_then(|s| s.get_object_id("replicaSetId").ok())
        })
        .unwrap_or_else(crate::handshake::hello_process_id);
    let mut bytes =
        sha2::Sha256::digest([b"local.oplog.rs".as_slice(), &id.bytes()].concat())[..16].to_vec();
    // A version-4 (random) UUID's version and variant bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes
}

/// Keep the set's configuration where mongod keeps it, the one document in
/// `local.system.replset`, so its `replicaSetId` is the same after a
/// restart. Called as the server binds: a store that has none gets one, and
/// one whose member is at another address (a server bound to port 0 moves
/// each run) has the address brought up to date, keeping its id.
pub fn record_replset_config(
    storage: &dyn crate::storage::Storage,
    set_name: &str,
    host: &str,
    port: u16,
) -> Result<(), crate::storage::StorageError> {
    let host_port = format!("{host}:{port}");
    match stored_replset_config(storage, set_name) {
        Some(stored) => {
            // What `replSetReconfig` changed stays; only the member's address
            // follows the port this run bound.
            let mut config = stored.clone();
            if config.get_document("settings").is_err() || config.get_array("members").is_err() {
                let id = stored
                    .get_document("settings")
                    .ok()
                    .and_then(|s| s.get_object_id("replicaSetId").ok())
                    .unwrap_or_else(bson::oid::ObjectId::new);
                config = replset_config(set_name, &host_port, id);
            }
            if let Ok(members) = config.get_array_mut("members") {
                if let Some(Bson::Document(m)) = members.first_mut() {
                    m.insert("host", host_port.clone());
                }
            }
            if stored != config {
                storage.update_matching(
                    "local",
                    "system.replset",
                    &doc! {"_id": set_name},
                    &config,
                    false,
                    false,
                )?;
            }
        }
        None => {
            let config = replset_config(set_name, &host_port, bson::oid::ObjectId::new());
            let bytes = bson::to_vec(&config)
                .map_err(|e| crate::storage::StorageError::Internal(e.to_string()))?;
            storage.insert("local", "system.replset", vec![bytes], true)?;
        }
    }
    Ok(())
}

/// `replSetGetConfig`: the one-member set `hello` describes, in the shape
/// of a configuration nobody has changed. A server run without a set name
/// answers as a standalone does.
pub fn repl_set_get_config(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let commitment = match doc.get("commitmentStatus") {
        None => None,
        Some(v) => Some(crate::util::truthy_number_or_bool(v).ok_or_else(|| {
            CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "Expected boolean or number type for field \"commitmentStatus\", found {}",
                    bson_type_name(v)
                ),
            )
        })?),
    };
    let (Some(set_name), Some((host, port))) =
        (ctx.replica_set_name.as_ref(), ctx.server_address.as_ref())
    else {
        return Err(CommandError::new(
            76,
            "NoReplicationEnabled",
            "not running with --replSet",
        ));
    };
    let host_port = format!("{host}:{port}");
    let stored = ctx
        .storage
        .as_ref()
        .and_then(|s| stored_replset_config(s.as_ref(), set_name));
    let config = match stored {
        Some(config) => config,
        // No store to keep it in (a test context): the process's own id
        // stands for the stored one.
        None => replset_config(set_name, &host_port, crate::handshake::hello_process_id()),
    };
    let mut reply = doc! {"config": config};
    if commitment == Some(true) {
        reply.insert("commitmentStatus", true);
    }
    reply.insert("ok", 1.0);
    Ok(reply)
}

/// `shardingState`: this is not a shard.
pub fn sharding_state(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "enabled": false, "ok": 1.0 })
}

/// `fsyncUnlock`: `fsync` never takes the lock here, so there is never one
/// to release, and mongod's answer to that is this refusal.
pub fn fsync_unlock(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Err(CommandError::new(
        20,
        "IllegalOperation",
        "fsyncUnlock called when not locked",
    ))
}

/// `connectionStatus`: who this connection has authenticated as. The
/// privileges are listed only when asked for, and `uuid` names the
/// connection for as long as it lasts.
pub fn connection_status(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let privileges = flag(doc, "connectionStatus", "showPrivileges")?;
    let mut users = Vec::new();
    let mut roles = Vec::new();
    let mut uuid = [0_u8; 16];
    if let Some(auth) = ctx.conn_auth.as_ref().and_then(|a| a.lock().ok()) {
        for (db, user) in &auth.authenticated {
            users.push(Bson::Document(
                doc! {"user": user.clone(), "db": db.clone()},
            ));
        }
        for (role, db) in &auth.effective_roles {
            roles.push(Bson::Document(
                doc! {"role": role.clone(), "db": db.clone()},
            ));
        }
        uuid = auth.client_uuid;
    }
    let mut info = doc! {
        "authenticatedUsers": users,
        "authenticatedUserRoles": roles,
    };
    if privileges {
        info.insert("authenticatedUserPrivileges", Vec::<Bson>::new());
    }
    Ok(doc! {
        "authInfo": info,
        "uuid": Bson::Binary(Binary { subtype: BinarySubtype::Uuid, bytes: uuid.to_vec() }),
        "ok": 1.0,
    })
}

/// `whatsmyuri`: the address the client connected from.
pub fn whatsmyuri(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let peer = ctx
        .conn_auth
        .as_ref()
        .and_then(|a| a.lock().ok())
        .and_then(|a| a.peer_address.clone())
        .unwrap_or_else(|| "127.0.0.1:0".to_string());
    Ok(doc! { "you": peer, "ok": 1.0 })
}

/// `killOp` — close a client connection by its `op` (our per-connection
/// `conn_id`; we model one in-flight op per connection, so the opid a caller
/// reads off `hello`'s `connectionId` / `currentOp` is directly killable). Real
/// mongod signals a per-op interrupt flag long-running paths poll; SecantusDB's
/// faithful analogue is "close the socket" — the connection thread's next read
/// returns 0, the loop exits, and the connection unregisters.
///
/// `op` is any number that is a whole 32-bit value, and the answer does not
/// say whether an operation had that id: mongod says "attempting to kill op"
/// to an id nobody holds.
pub fn kill_op(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let op_id = match doc.get("op") {
        None => {
            return Err(CommandError::new(
                4,
                "NoSuchKey",
                "Missing expected field \"op\"",
            ));
        }
        Some(Bson::Int32(n)) => i64::from(*n),
        Some(Bson::Int64(n)) => *n,
        Some(v @ (Bson::Double(_) | Bson::Decimal128(_))) => {
            let number = match v {
                Bson::Double(d) => *d,
                other => other.to_string().parse::<f64>().unwrap_or(f64::NAN),
            };
            if number.fract() != 0.0 || !number.is_finite() {
                return Err(CommandError::new(
                    2,
                    "BadValue",
                    format!(
                        "Expected field \"op\" to have a value exactly representable as a \
                         64-bit integer, but found op: {v}"
                    ),
                ));
            }
            number as i64
        }
        Some(v) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "Expected field \"op\" to have numeric type, but found {}",
                    bson_type_name(v)
                ),
            ));
        }
    };
    if i32::try_from(op_id).is_err() {
        return Err(CommandError::new(
            26823,
            "Location26823",
            format!("invalid op : {op_id}. Op ID cannot be represented with 32 bits"),
        ));
    }
    if let Some(killer) = &ctx.conn_killer {
        killer.kill(op_id);
    }
    Ok(doc! { "info": "attempting to kill op", "ok": 1.0 })
}

/// `getLog` — the server's in-memory log ring buffer. mongod returns the log as
/// a list of pre-formatted strings (`"<ts> <level> <component> <msg>"`); we
/// mirror that format so tooling that grep-parses the response keeps working.
/// Without a buffer wired in (unit tests), reports an empty log.
///
/// `"*"` lists the logs there are; `startupWarnings` is the warnings among
/// the lines kept; any other name is refused.
pub fn get_log(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let which = match doc.get("getLog") {
        Some(Bson::String(name)) => name.as_str(),
        None | Some(Bson::Null) => {
            return Err(CommandError::new(
                40414,
                "IDLFailedToParse",
                "BSON field 'getLog.getLog' is missing but a required field",
            ));
        }
        Some(v) => return Err(wrong_type("getLog.getLog", v, "type 'string'")),
    };
    let warnings_only = match which {
        "*" => {
            return Ok(doc! { "names": ["global", "startupWarnings"], "ok": 1.0 });
        }
        "global" => false,
        "startupWarnings" => true,
        other => {
            return Err(CommandError::new(
                96,
                "OperationFailed",
                format!("No log named '{other}'"),
            ));
        }
    };
    let entries = ctx.logs.as_ref().map(|l| l.tail(None)).unwrap_or_default();
    let formatted: Vec<Bson> = entries
        .iter()
        .filter(|e| !warnings_only || e.level == "W")
        .map(|e| {
            let ts = e.ts.try_to_rfc3339_string().unwrap_or_default();
            Bson::String(format!("{ts} {} {} {}", e.level, e.component, e.msg))
        })
        .collect();
    Ok(doc! {
        "totalLinesWritten": formatted.len() as i32,
        "log": formatted,
        "ok": 1.0,
    })
}

/// A server parameter this server reports: its name, whether mongod lets it
/// be given at startup, and its value when nobody has changed it.
///
/// Only what is TRUE of this server is listed. Each value here is a limit or
/// a setting the server really has (the cursor and transaction lifetimes,
/// the SCRAM iteration counts it stores, the session timeout `hello` gives);
/// a mongod parameter with nothing behind it is not reported, so a client
/// cannot read a setting that changes nothing.
fn parameters(test_commands: bool) -> Vec<(&'static str, bool, Bson)> {
    let version = crate::SERVER_VERSION_ARRAY;
    vec![
        (
            "authenticationMechanisms",
            true,
            Bson::Array(vec![
                Bson::String("MONGODB-X509".into()),
                Bson::String("SCRAM-SHA-1".into()),
                Bson::String("SCRAM-SHA-256".into()),
            ]),
        ),
        (
            "cursorTimeoutMillis",
            true,
            Bson::Int64((crate::cursors::DEFAULT_IDLE_TTL_SECONDS * 1000.0) as i64),
        ),
        ("enableLocalhostAuthBypass", true, Bson::Boolean(true)),
        // The REAL value, not a constant. Drivers gate on this: while it said
        // false, pymongo's harness skipped ~1,080 unified-spec failpoint tests
        // (measured 2026-09-25). But hardcoding `true` would have made a server
        // that refuses `configureFailPoint` claim to accept it, so the two now
        // come from the same fact -- whether a FailPointRegistry was wired.
        ("enableTestCommands", true, Bson::Boolean(test_commands)),
        // `<major>.<minor>` of the version this server says it is; a literal
        // here once read "7.0" beside a `buildInfo` of 8.2.11.
        (
            "featureCompatibilityVersion",
            false,
            Bson::Document(doc! {"version": format!("{}.{}", version[0], version[1])}),
        ),
        // The connection-establishment rate limiter (`params::IngressBucket`),
        // with mongod 8.2.11's defaults: off, and no limit when on.
        (
            "ingressConnectionEstablishmentBurstCapacitySecs",
            true,
            Bson::Double(f64::MAX),
        ),
        (
            "ingressConnectionEstablishmentMaxQueueDepth",
            true,
            Bson::Int32(0),
        ),
        (
            "ingressConnectionEstablishmentRateLimiterEnabled",
            true,
            Bson::Boolean(false),
        ),
        (
            "ingressConnectionEstablishmentRatePerSec",
            true,
            Bson::Int32(i32::MAX),
        ),
        ("localLogicalSessionTimeoutMinutes", true, Bson::Int32(30)),
        ("logLevel", true, Bson::Int32(0)),
        (
            "maxTransactionLockRequestTimeoutMillis",
            true,
            Bson::Int32(5),
        ),
        // Honoured: a query the planner would answer with a collection scan
        // is refused (`crate::notablescan`).
        ("notablescan", true, Bson::Boolean(false)),
        ("quiet", true, Bson::Boolean(false)),
        ("scramIterationCount", true, Bson::Int32(10_000)),
        (
            "scramSHA256IterationCount",
            true,
            Bson::Int32(secantus_auth::DEFAULT_ITERATIONS as i32),
        ),
        ("transactionLifetimeLimitSeconds", true, Bson::Int32(60)),
        ("ttlMonitorEnabled", true, Bson::Boolean(true)),
        ("ttlMonitorSleepSecs", true, Bson::Int32(60)),
    ]
}

/// A TTL monitor setting, read from the monitor itself: the cadence and the
/// switch the server was started with until a client sets them, as mongod
/// reports a `--setParameter` startup value. `None` for any other parameter,
/// or a backend with no monitor.
pub(crate) fn ttl_monitor_param(ctx: &CommandContext, name: &str) -> Option<Bson> {
    let (secs, enabled, _, _) = ctx.storage().ok()?.ttl_monitor()?;
    match name {
        "ttlMonitorSleepSecs" if secs > 0 => {
            Some(Bson::Int32(i32::try_from(secs).unwrap_or(i32::MAX)))
        }
        "ttlMonitorEnabled" => Some(Bson::Boolean(enabled)),
        _ => None,
    }
}

/// The parameters and their defaults, as one document (`setParameter` reads
/// the previous value from it).
pub(crate) fn known_params(test_commands: bool) -> Document {
    parameters(test_commands)
        .into_iter()
        .map(|(name, _, value)| (name.to_string(), value))
        .collect()
}

/// `getParameter`, as mongod answers it (8.2.11):
///
/// - the parameters asked for come back in NAME order, whatever order they
///   were asked in and whatever value was put beside each name;
/// - a name the server does not have is left out, and a request that names
///   nothing the server has is `72 no option found to get`;
/// - `"*"` or `{allParameters: true}` is every parameter;
/// - `{showDetails: true}` wraps each value with whether it can be set at
///   runtime and at startup, and `{setAt: ...}` keeps only those that can.
pub fn get_parameter(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let mut details = false;
    let mut everything = false;
    let mut set_at: Option<&str> = None;
    match doc.get("getParameter") {
        Some(Bson::String(s)) if s == "*" => everything = true,
        Some(Bson::Document(options)) => {
            for (key, value) in options {
                match (key.as_str(), value) {
                    ("showDetails", _) => details = flag(options, "getParameter", key)?,
                    ("allParameters", _) => everything = flag(options, "getParameter", key)?,
                    ("setAt", Bson::String(when)) if when == "startup" || when == "runtime" => {
                        set_at = Some(when);
                    }
                    ("setAt", Bson::String(when)) => {
                        return Err(CommandError::new(
                            2,
                            "BadValue",
                            format!(
                                "Enumeration value '{when}' for field 'getParameter.setAt' is \
                                 not a valid value."
                            ),
                        ));
                    }
                    ("setAt", v) => {
                        return Err(wrong_type("getParameter.setAt", v, "type 'string'"))
                    }
                    _ => {
                        return Err(CommandError::new(
                            40415,
                            "IDLUnknownField",
                            format!("BSON field 'getParameter.{key}' is an unknown field."),
                        ));
                    }
                }
            }
        }
        _ => {}
    }
    let current = ctx.server_params.clone();
    let mut out = Document::new();
    for (name, at_startup, default) in parameters(ctx.failpoints.is_some()) {
        if !everything && !doc.contains_key(name) {
            continue;
        }
        let at_runtime = crate::params::settable(name);
        match set_at {
            Some("startup") if !at_startup => continue,
            Some("runtime") if !at_runtime => continue,
            _ => {}
        }
        // A value `setParameter` changed wins over the default, so the two
        // commands cannot disagree about what the server holds.
        let value = current
            .as_ref()
            .and_then(|s| s.get(name))
            .unwrap_or(default);
        let value = ttl_monitor_param(ctx, name).unwrap_or_else(|| value.clone());
        // The FCV is what `setFeatureCompatibilityVersion` recorded, when
        // it recorded one.
        let value = if name == "featureCompatibilityVersion" {
            Bson::Document(doc! {"version": crate::cluster_admin::current_fcv(ctx)})
        } else {
            value
        };
        if details {
            out.insert(
                name,
                doc! {
                    "value": value,
                    "settableAtRuntime": at_runtime,
                    "settableAtStartup": at_startup,
                },
            );
        } else {
            out.insert(name, value);
        }
    }
    if out.is_empty() {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "no option found to get",
        ));
    }
    out.insert("ok", 1.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CommandContext {
        let mut ctx = CommandContext::new(1);
        ctx.db_name = "admin".to_string();
        ctx
    }

    #[test]
    fn get_parameter_answers_in_name_order_and_refuses_when_it_has_none() {
        let reply = get_parameter(
            &doc! {"getParameter": 1, "quiet": 1, "logLevel": 0},
            &mut ctx(),
        )
        .unwrap();
        assert_eq!(
            reply.keys().map(String::as_str).collect::<Vec<_>>(),
            ["logLevel", "quiet", "ok"]
        );
        for request in [
            doc! {"getParameter": 1},
            doc! {"getParameter": 1, "bogus": 1},
        ] {
            let e = get_parameter(&request, &mut ctx()).unwrap_err();
            assert_eq!((e.code, e.errmsg.as_str()), (72, "no option found to get"));
        }
        let all = get_parameter(&doc! {"getParameter": "*"}, &mut ctx()).unwrap();
        assert!(all.contains_key("featureCompatibilityVersion"));
    }

    #[test]
    fn show_details_says_when_a_parameter_can_be_set() {
        let request = doc! {
            "getParameter": {"showDetails": true},
            "quiet": 1,
            "featureCompatibilityVersion": 1,
        };
        let reply = get_parameter(&request, &mut ctx()).unwrap();
        let quiet = reply.get_document("quiet").unwrap();
        assert_eq!(
            quiet,
            &doc! {"value": false, "settableAtRuntime": true, "settableAtStartup": true}
        );
        let fcv = reply.get_document("featureCompatibilityVersion").unwrap();
        assert_eq!(fcv.get_bool("settableAtStartup"), Ok(false));
        let runtime = doc! {"getParameter": {"setAt": "runtime"}, "enableTestCommands": 1};
        assert_eq!(get_parameter(&runtime, &mut ctx()).unwrap_err().code, 72);
    }

    #[test]
    fn kill_op_reads_any_whole_number_and_never_says_what_it_found() {
        for op in [Bson::Int32(7), Bson::Double(7.0), Bson::Int64(-1)] {
            let reply = kill_op(&doc! {"killOp": 1, "op": op}, &mut ctx()).unwrap();
            assert_eq!(reply.get_str("info"), Ok("attempting to kill op"));
        }
        let code = |op: Bson| {
            kill_op(&doc! {"killOp": 1, "op": op}, &mut ctx())
                .unwrap_err()
                .code
        };
        assert_eq!(code(Bson::String("x".into())), 14);
        assert_eq!(code(Bson::Double(2.5)), 2);
        assert_eq!(code(Bson::Int64(1 << 40)), 26823);
        assert_eq!(
            kill_op(&doc! {"killOp": 1}, &mut ctx()).unwrap_err().code,
            4
        );
    }

    #[test]
    fn get_log_names_its_logs_and_refuses_another() {
        let names = get_log(&doc! {"getLog": "*"}, &mut ctx()).unwrap();
        assert_eq!(names.get_array("names").unwrap().len(), 2);
        assert_eq!(
            get_log(&doc! {"getLog": "bogus"}, &mut ctx())
                .unwrap_err()
                .code,
            96
        );
        assert_eq!(
            get_log(&doc! {"getLog": 1}, &mut ctx()).unwrap_err().code,
            14
        );
    }
}
