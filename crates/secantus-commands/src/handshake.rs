//! The handshake command family: `hello` / `isMaster`, `ping`, `buildInfo`.
//!
//! Storage-independent static-ish replies that let a driver complete the
//! connection handshake. Faithful ports of `commands.py::_hello` / `_ping` /
//! `_build_info`.
//!
//! `saslSupportedMechs` lists the queried user's own SCRAM mechanisms.
//! **Deferred:** `speculativeAuthenticate` (folding a SCRAM client-first into
//! `hello`), and
//! stashing the driver's `client` metadata into the connection registry for
//! `currentOp`. The non-auth handshake path — the default, and what most
//! conformance suites exercise — is complete here.

use std::sync::OnceLock;

use bson::{doc, oid::ObjectId, Bson, DateTime, Document};

use crate::{
    CommandContext, CommandError, HandlerResult, MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE,
    SERVER_VERSION, SERVER_VERSION_ARRAY, WIRE_VERSION,
};

/// `topologyVersion.processId` identifies the server *process* and is fixed for
/// its lifetime. The SDAM spec compares it across heartbeats; a *changed*
/// processId is read as "the server restarted", making drivers invalidate and
/// clear the connection pool (close + reconnect). Minting a fresh `ObjectId` per
/// hello therefore triggered a spurious pool-clear on nearly every monitoring
/// heartbeat — so pin it once per process. (Java-gauge finding)
pub(crate) fn hello_process_id() -> ObjectId {
    static PROCESS_ID: OnceLock<ObjectId> = OnceLock::new();
    *PROCESS_ID.get_or_init(ObjectId::new)
}

/// When this process first answered anything that asks: the start of the
/// counts it keeps.
pub(crate) fn process_started() -> DateTime {
    static STARTED: OnceLock<DateTime> = OnceLock::new();
    *STARTED.get_or_init(DateTime::now)
}

/// `hello` / `isMaster` / `ismaster`. Advertises a single-node `secantus`
/// replica-set primary when a set name is configured (so pymongo's topology
/// machinery accepts change streams), else a plain standalone primary.
///
/// The reply is mongod 8.2.11's, key for key (measured 2026-10-13 on a
/// replica-set member and on a standalone):
///
/// - `hello` answers `isWritablePrimary` and the two older names answer
///   `ismaster`; neither reply carries both.
/// - `helloOk` is echoed first, and only to a request that carried it.
/// - `connectionId` is an int32, `topologyVersion.counter` and
///   `lastWrite.opTime.t` are int64.
/// - A set member leads with `topologyVersion`, `hosts`, `setName`; a
///   standalone leads with the primary flag. Empty `passives` and `arbiters`
///   are not sent.
pub fn hello(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let legacy = doc.keys().next().is_some_and(|name| name != "hello");
    let hello_ok = validate_hello_arguments(doc)?;

    let topology_counter = ctx.step_down.as_ref().map_or(0, |s| s.topology_counter());
    validate_awaitable_arguments(doc, topology_counter)?;

    // The driver's `client` metadata comes with a connection's FIRST hello
    // and with no later one: mongod answers 186 to a second, whether or not
    // the first carried any. `currentOp` shows what was sent.
    let first_hello = ctx
        .conn_auth
        .as_ref()
        .and_then(|a| a.lock().ok().map(|g| !g.hello_seen))
        .unwrap_or(true);
    if let Some(client) = doc.get("client") {
        if !first_hello {
            return Err(CommandError::new(
                186,
                "ClientMetadataCannotBeMutated",
                "The client metadata document may only be sent in the first hello",
            ));
        }
        validate_client_metadata(client, true)?;
        if let (Some(conn_auth), Bson::Document(client)) = (ctx.conn_auth.as_ref(), client) {
            if let Ok(mut guard) = conn_auth.lock() {
                guard.client_metadata = Some(client.clone());
            }
        }
    }

    // `saslSupportedMechs: "<db>.<user>"` — drivers ask which mechanisms to
    // attempt for a principal. mongod 8.2.11 lists the user's own SCRAM
    // mechanisms, OMITS the field for an unknown user, and refuses a name with
    // no `.` (measured 2026-09-30). This always said `["SCRAM-SHA-256"]`.
    let mut mechanisms = None;
    if let Ok(principal) = doc.get_str("saslSupportedMechs") {
        let Some((db, user)) = principal.split_once('.') else {
            return Err(CommandError::new(
                2,
                "BadValue",
                "UserName must contain a '.' separated database.user pair",
            ));
        };
        mechanisms = user_scram_mechanisms(ctx, db, user);
    }

    let now = DateTime::now();
    // Inside a `replSetStepDown` window this node is a SECONDARY. The SDAM spec
    // reads these two fields to place the server in the topology, so they must
    // flip together with the write refusal -- a server that keeps claiming to be
    // primary while rejecting every write is worse than either alone.
    let stepped_down = ctx.step_down.as_ref().is_some_and(|s| s.is_stepped_down());
    let primary_flag = if legacy {
        "ismaster"
    } else {
        "isWritablePrimary"
    };
    // The counter moves when the TOPOLOGY moves, which for a single-node
    // surrogate means a `replSetStepDown`. A driver ignores a "not primary"
    // error whose topologyVersion is not NEWER than the one it already has,
    // so a counter frozen at 0 would make the step-down error look stale.
    let topology = doc! {
        "processId": hello_process_id(),
        "counter": Bson::Int64(topology_counter),
    };

    let mut response = Document::new();
    // A driver puts `helloOk: true` in its handshake to ask whether this server
    // understands the modern `hello`; the server echoes it back to say yes, and
    // the driver then uses `hello` for the life of the connection. Measured on
    // mongod 8.2.11 (2026-09-29): echoed ONLY when the client asked, over both
    // OP_QUERY and OP_MSG.
    //
    // Omitting it is not cosmetic. Without the echo every modern driver decides
    // this server predates `hello` and falls back to the LEGACY `isMaster` on
    // every connection -- which is what mongo-go-driver's SDAM monitor did,
    // taking a different monitoring path from the one it takes against mongod
    // and failing `TestSDAMProse/heartbeats_processed_more_frequently` with 12
    // messages where the formula allows 10.
    if hello_ok {
        response.insert("helloOk", true);
    }

    if let (Some(set_name), Some((host, port))) =
        (ctx.replica_set_name.as_ref(), ctx.server_address.as_ref())
    {
        let addr = format!("{host}:{port}");
        // `lastWrite.opTime.ts` mirrors `commands.py`'s
        // `ctx.storage.current_cluster_time()` — mint the next monotonic cluster
        // time (strictly greater than the last write) so `startAtOperationTime`
        // resumes land just past it. Fall back to the supplied `ctx.cluster_time`
        // when no storage backend is wired (handshake-only fakes).
        let stamp = match ctx.storage.as_ref() {
            Some(s) => s.current_cluster_time(),
            None => ctx.cluster_time,
        };
        let ts = Bson::Timestamp(stamp);
        // mongod reports the write dates to the second: they are the
        // timestamp's own seconds.
        let written = DateTime::from_millis(i64::from(stamp.time) * 1000);
        // Fixed sentinel electionId, matching commands.py.
        let election = ObjectId::parse_str("7fffffff0000000000000001")
            .expect("static electionId hex is valid");
        response.insert("topologyVersion", topology);
        response.insert("hosts", vec![Bson::String(addr.clone())]);
        response.insert("setName", set_name.clone());
        response.insert("setVersion", 1_i32);
        response.insert(primary_flag, !stepped_down);
        // Measured on a single-node replica set (mongod 8.2.11, 2026-09-29):
        // while a node is stepped down it reports `secondary: true` and DROPS
        // both `primary` and `electionId` -- there is no known primary and no
        // election it won. Reporting a primary that is not writable is the
        // combination SDAM cannot make sense of.
        response.insert("secondary", stepped_down);
        if !stepped_down {
            response.insert("primary", addr.clone());
        }
        response.insert("me", addr);
        if !stepped_down {
            response.insert("electionId", election);
        }
        response.insert(
            "lastWrite",
            doc! {
                "opTime": {"ts": ts.clone(), "t": 1_i64},
                "lastWriteDate": written,
                "majorityOpTime": {"ts": ts, "t": 1_i64},
                "majorityWriteDate": written,
            },
        );
    } else {
        response.insert(primary_flag, !stepped_down);
        response.insert("topologyVersion", topology);
    }
    response.insert("maxBsonObjectSize", MAX_BSON_OBJECT_SIZE);
    response.insert("maxMessageSizeBytes", MAX_MESSAGE_SIZE);
    response.insert("maxWriteBatchSize", 100_000_i32);
    response.insert("localTime", now);
    response.insert("logicalSessionTimeoutMinutes", 30_i32);
    // An int32, as mongod sends it; a driver reads either width.
    response.insert("connectionId", ctx.connection_id as i32);
    response.insert("minWireVersion", 0_i32);
    response.insert("maxWireVersion", WIRE_VERSION);
    response.insert("readOnly", false);
    if ctx.require_auth {
        response.insert("accessControlEnabled", true);
    }
    if let Some(mechs) = mechanisms {
        response.insert("saslSupportedMechs", mechs);
    }
    response.insert("ok", 1.0);

    if let Some(conn_auth) = ctx.conn_auth.as_ref() {
        if let Ok(mut guard) = conn_auth.lock() {
            guard.hello_seen = true;
        }
    }
    Ok(response)
}

fn wrong_type(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected {expected}",
            secantus_core::query::bson_type_name(value)
        ),
    )
}

/// The fields of `hello` mongod's parser types, in the order they were sent
/// (measured 8.2.11, 2026-10-13). A `null` is an absent field, the path says
/// `hello.` for the older names too, and a field the parser does not know is
/// accepted. Returns whether the client asked for `helloOk`.
///
/// `compression` is checked and then not answered: this server does not
/// speak `OP_COMPRESSED`, so it names no compressor it shares.
fn validate_hello_arguments(doc: &Document) -> Result<bool, CommandError> {
    let mut hello_ok = false;
    for (key, value) in doc {
        if matches!(value, Bson::Null) {
            continue;
        }
        let path = format!("hello.{key}");
        match (key.as_str(), value) {
            // Echoed for any bool or number, `false` and `0` included: the
            // field being there is the question.
            ("helloOk", v) => match crate::util::truthy_number_or_bool(v) {
                Some(_) => hello_ok = true,
                None => {
                    return Err(wrong_type(
                        &path,
                        v,
                        "types '[bool, long, int, decimal, double]'",
                    ))
                }
            },
            ("compression", Bson::Array(names)) => {
                for (i, name) in names.iter().enumerate() {
                    if !matches!(name, Bson::String(_)) {
                        return Err(wrong_type(&format!("{path}.{i}"), name, "type 'string'"));
                    }
                }
            }
            ("compression", v) => return Err(wrong_type(&path, v, "type 'array'")),
            ("saslSupportedMechs", Bson::String(_) | Bson::Document(_)) => {}
            ("saslSupportedMechs", v) => {
                return Err(wrong_type(&path, v, "types '[object, string]'"))
            }
            ("loadBalanced", Bson::Boolean(_)) => {}
            ("loadBalanced", v) => return Err(wrong_type(&path, v, "type 'bool'")),
            ("internalClient", Bson::Document(_)) => {}
            ("internalClient", v) => return Err(wrong_type(&path, v, "type 'object'")),
            ("speculativeAuthenticate", Bson::Document(spec)) => {
                if spec.is_empty() {
                    return Err(CommandError::new(
                        2,
                        "BadValue",
                        "hello.speculativeAuthenticate must be a non-empty Object",
                    ));
                }
            }
            ("speculativeAuthenticate", v) => return Err(wrong_type(&path, v, "type 'object'")),
            _ => {}
        }
    }
    Ok(hello_ok)
}

/// mongod's check of a driver's `client` metadata (measured 8.2.11,
/// 2026-10-13). The document is at most 512 bytes when it comes with `hello`
/// (`sized`; the internal `$client` field is not measured). Its fields are
/// read in the order sent, each sub-document finished before the next, and
/// only then is a missing `driver` or `os` reported. Unknown fields pass.
pub(crate) fn validate_client_metadata(client: &Bson, sized: bool) -> Result<(), CommandError> {
    const WHERE: &str = "in the client metadata document";
    let Bson::Document(client) = client else {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "The client metadata document must be a document",
        ));
    };
    if sized && bson::to_vec(client).map_or(0, |b| b.len()) > 512 {
        return Err(CommandError::new(
            185,
            "ClientMetadataDocumentTooLarge",
            "The client metadata document must be less than or equal to 512 bytes",
        ));
    }
    let not_document = |field: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!("The '{field}' field is required to be a BSON document {WHERE}"),
        )
    };
    let not_string = |field: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!("The '{field}' field must be a string {WHERE}"),
        )
    };
    let missing = |what: &str, field: &str| {
        CommandError::new(
            183,
            "ClientMetadataMissingField",
            format!("Missing required {what} '{field}' {WHERE}"),
        )
    };
    // Each required string of a sub-document: a wrong type as it is met,
    // then the first one absent.
    let strings = |parent: &str, sub: &Document, required: &[&str]| {
        for (key, value) in sub {
            if required.contains(&key.as_str()) && !matches!(value, Bson::String(_)) {
                return Err(not_string(&format!("{parent}.{key}")));
            }
        }
        match required.iter().find(|name| !sub.contains_key(**name)) {
            Some(name) => Err(missing("field", &format!("{parent}.{name}"))),
            None => Ok(()),
        }
    };
    for (key, value) in client {
        match (key.as_str(), value) {
            ("application", Bson::Document(app)) => match app.get("name") {
                Some(Bson::String(name)) if name.len() > 128 => {
                    return Err(CommandError::new(
                        184,
                        "ClientMetadataAppNameTooLarge",
                        format!(
                            "The 'application.name' field must be less then or equal to 128 \
                             bytes {WHERE}"
                        ),
                    ));
                }
                Some(Bson::String(_)) | None => {}
                Some(_) => return Err(not_string("application.name")),
            },
            ("driver", Bson::Document(driver)) => strings("driver", driver, &["name", "version"])?,
            ("os", Bson::Document(os)) => strings("os", os, &["type"])?,
            ("application" | "driver" | "os", _) => return Err(not_document(key)),
            _ => {}
        }
    }
    for required in ["driver", "os"] {
        if !client.contains_key(required) {
            return Err(missing("sub-document", required));
        }
    }
    Ok(())
}

/// `ping` — the trivial liveness probe.
pub fn ping(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "ok": 1.0 })
}

/// `replSetGetStatus`. SecantusDB advertises a single-node `secantus` replica
/// set in `hello` (so pymongo's change-stream topology accepts it) but is not a
/// real replica set with a member roster. Return exactly what a standalone
/// mongod returns — `NoReplicationEnabled` (76) with the canonical "not running
/// with --replSet" message. Drivers and their harnesses special-case this
/// message to mean "standalone, skip replica-set-only behaviour" (e.g.
/// libmongoc's `test_framework_replset_member_count`), whereas a bare
/// CommandNotFound (59) is an unexpected error that aborts the harness — which
/// truncated the entire C-driver gauge after the first suite. Mirrors
/// `commands.py::_repl_set_get_status`.
pub fn repl_set_get_status(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // When a set name is configured, `hello` already advertises this node as a
    // single-node replica-set primary (that is what makes drivers accept change
    // streams). Report a matching one-member roster here rather than the
    // standalone error, so the two answers agree.
    //
    // Driver test harnesses read the roster to decide whether replica-set-only
    // behaviour is available: libmongoc's `test_framework_replset_member_count`
    // counts `members`, and with zero it skips every `/change_stream` suite as
    // "standalone" — which is why those suites were excluded from the C gauge
    // entirely. One live member makes them run.
    //
    // With no set name (`--replica-set-name` off) this is a genuine standalone
    // and the `NoReplicationEnabled` error is still the honest answer; harnesses
    // special-case that message to mean "skip replica-set-only behaviour",
    // whereas a bare CommandNotFound aborts them.
    let (Some(set_name), Some((host, port))) =
        (ctx.replica_set_name.as_ref(), ctx.server_address.as_ref())
    else {
        return Ok(doc! {
            "ok": 0.0,
            "errmsg": "not running with --replSet",
            "code": 76_i32,
            "codeName": "NoReplicationEnabled",
        });
    };
    let addr = format!("{host}:{port}");
    let ts = Bson::Timestamp(match ctx.storage.as_ref() {
        Some(s) => s.current_cluster_time(),
        None => ctx.cluster_time,
    });
    let now = bson::DateTime::now();
    let optime = doc! { "ts": ts.clone(), "t": 1_i64 };
    Ok(doc! {
        "set": set_name.clone(),
        "date": now,
        "myState": 1_i32,
        "term": 1_i64,
        "syncSourceHost": "",
        "syncSourceId": -1_i32,
        "heartbeatIntervalMillis": 2000_i64,
        "majorityVoteCount": 1_i32,
        "writeMajorityCount": 1_i32,
        "votingMembersCount": 1_i32,
        "writableVotingMembersCount": 1_i32,
        "optimes": {
            "lastCommittedOpTime": optime.clone(),
            "lastCommittedWallTime": now,
            "readConcernMajorityOpTime": optime.clone(),
            "appliedOpTime": optime.clone(),
            "durableOpTime": optime.clone(),
            "lastAppliedWallTime": now,
            "lastDurableWallTime": now,
        },
        "lastStableRecoveryTimestamp": ts,
        "members": [
            {
                "_id": 0_i32,
                "name": addr,
                "health": 1.0,
                "state": 1_i32,
                "stateStr": "PRIMARY",
                "uptime": 0_i32,
                "optime": optime.clone(),
                "optimeDate": now,
                "lastAppliedWallTime": now,
                "lastDurableWallTime": now,
                "syncSourceHost": "",
                "syncSourceId": -1_i32,
                "infoMessage": "",
                "electionTime": Bson::Timestamp(ctx.cluster_time),
                "electionDate": now,
                "configVersion": 1_i32,
                "configTerm": 1_i64,
                "self": true,
            }
        ],
        "ok": 1.0,
    })
}

/// `buildInfo` / `buildinfo`. `version` stays at the MongoDB-compatibility value
/// so drivers enable the right feature flags; `secantusVersion` marks the actual
/// build (the crate version here; `commands.py` reads `secantus.__version__`).
pub fn build_info(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    // mongod's keys in mongod's order (8.2.11), each saying what is true of
    // THIS build: no source modules, no script engine, a Rust compiler, and
    // WiredTiger as the one storage engine. `maxBsonObjectSize` is an int64
    // here and an int32 in `hello`, as on mongod.
    Ok(doc! {
        "version": SERVER_VERSION,
        "secantusVersion": env!("CARGO_PKG_VERSION"),
        "versionArray": SERVER_VERSION_ARRAY.iter().map(|n| Bson::Int32(*n)).collect::<Vec<_>>(),
        "gitVersion": "0".repeat(40),
        "modules": Vec::<Bson>::new(),
        "allocator": "mimalloc",
        "javascriptEngine": "none",
        "sysinfo": "deprecated",
        "openssl": {"running": "rustls"},
        "buildEnvironment": {
            "distmod": "",
            "distarch": std::env::consts::ARCH,
            "cc": "rustc",
            "ccflags": "",
            "cxx": "",
            "cxxflags": "",
            "linkflags": "",
            "target_arch": std::env::consts::ARCH,
            "target_os": std::env::consts::OS,
            "cppdefines": "",
        },
        "bits": 64_i32,
        "debug": cfg!(debug_assertions),
        "maxBsonObjectSize": Bson::Int64(i64::from(MAX_BSON_OBJECT_SIZE)),
        "storageEngines": ["wiredTiger"],
        "ok": 1.0,
    })
}

/// `maxAwaitTimeMS` as mongod reads it: any number, truncated toward zero.
/// `None` when absent, `null` or not a number (the handler has already refused
/// a non-number by the time the server asks).
pub fn max_await_time_ms(doc: &Document) -> Option<i64> {
    match doc.get("maxAwaitTimeMS")? {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        Bson::Double(d) => Some(d.trunc() as i64),
        Bson::Decimal128(d) => d.to_string().parse::<f64>().ok().map(|f| f.trunc() as i64),
        _ => None,
    }
}

/// True when an awaitable `hello` names the topology this server is in NOW --
/// same `processId`, same counter -- which is the only case mongod HOLDS the
/// reply for `maxAwaitTimeMS`. A different process (the client last saw
/// another server, or this one before a restart) or an older counter means the
/// client is out of date, and mongod answers at once so it catches up.
/// Measured 8.2.11 (2026-09-30), streamed and not.
pub fn awaitable_topology_is_current(doc: &Document, counter: i64) -> bool {
    let Ok(tv) = doc.get_document("topologyVersion") else {
        return false;
    };
    tv.get_object_id("processId").ok() == Some(hello_process_id())
        && tv.get_i64("counter").ok() == Some(counter)
}

/// mongod's parse of the awaitable-hello arguments, in its order (measured
/// 8.2.11, 2026-09-30). Every shape below was ACCEPTED here before, and a
/// malformed or newer-than-ours `topologyVersion` was simply waited on.
///
/// 1. IDL parse, field by field in document order: `topologyVersion` must be an
///    object whose `processId` is an ObjectId and whose `counter` is a LONG (an
///    int32 counter is a type error), with no other field, and with `counter`
///    reported missing before `processId`; `maxAwaitTimeMS` must be a number
///    and, truncated toward zero, not negative. `null` is absent for both. The
///    path says `hello.` for `isMaster` too.
/// 2. The pair: either one without the other is 31368.
/// 3. The counter: negative is 31372; for THIS process, newer than ours is
///    31382 (a counter from another process is merely stale, not an error).
fn validate_awaitable_arguments(doc: &Document, counter: i64) -> Result<(), CommandError> {
    let type_mismatch = |path: &str, v: &Bson, expected: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{path}' is the wrong type '{}', expected {expected}",
                secantus_core::query::bson_type_name(v)
            ),
        )
    };
    let mut topology: Option<&Document> = None;
    let mut max_await = false;
    for (key, value) in doc {
        match (key.as_str(), value) {
            ("topologyVersion", Bson::Null) | ("maxAwaitTimeMS", Bson::Null) => {}
            ("topologyVersion", Bson::Document(tv)) => {
                for (field, v) in tv {
                    let path = format!("hello.topologyVersion.{field}");
                    match (field.as_str(), v) {
                        ("processId", Bson::ObjectId(_)) | ("counter", Bson::Int64(_)) => {}
                        ("processId", v) => {
                            return Err(type_mismatch(&path, v, "type 'objectId'"));
                        }
                        ("counter", v) => return Err(type_mismatch(&path, v, "type 'long'")),
                        _ => {
                            return Err(CommandError::new(
                                40415,
                                "IDLUnknownField",
                                format!("BSON field '{path}' is an unknown field."),
                            ))
                        }
                    }
                }
                for required in ["counter", "processId"] {
                    if !tv.contains_key(required) {
                        return Err(CommandError::new(
                            40414,
                            "IDLFailedToParse",
                            format!(
                                "BSON field 'hello.topologyVersion.{required}' is missing but a \
                                 required field"
                            ),
                        ));
                    }
                }
                topology = Some(tv);
            }
            ("topologyVersion", v) => {
                return Err(type_mismatch("hello.topologyVersion", v, "type 'object'"));
            }
            ("maxAwaitTimeMS", v) => {
                let Some(ms) = max_await_time_ms(doc) else {
                    return Err(type_mismatch(
                        "hello.maxAwaitTimeMS",
                        v,
                        "types '[int, decimal, long, double]'",
                    ));
                };
                if ms < 0 {
                    return Err(CommandError::new(
                        2,
                        "BadValue",
                        format!(
                            "BSON field 'maxAwaitTimeMS' value must be >= 0, actual value '{ms}'"
                        ),
                    ));
                }
                max_await = true;
            }
            _ => {}
        }
    }
    match (topology, max_await) {
        (None, true) => Err(CommandError::new(
            31368,
            "Location31368",
            "A request with 'maxAwaitTimeMS' must include a 'topologyVersion'",
        )),
        (Some(_), false) => Err(CommandError::new(
            31368,
            "Location31368",
            "A request with a 'topologyVersion' must include 'maxAwaitTimeMS'",
        )),
        (Some(tv), true) => {
            let theirs = tv.get_i64("counter").unwrap_or(0);
            if theirs < 0 {
                return Err(CommandError::new(
                    31372,
                    "Location31372",
                    "topologyVersion must have a non-negative counter",
                ));
            }
            if tv.get_object_id("processId").ok() == Some(hello_process_id()) && theirs > counter {
                return Err(CommandError::new(
                    31382,
                    "Location31382",
                    format!(
                        "Received a topology version with counter: {theirs} which is greater \
                         than the server topology version counter: {counter}"
                    ),
                ));
            }
            Ok(())
        }
        (None, false) => Ok(()),
    }
}

/// The SCRAM mechanisms a stored user can authenticate with, in mongod's
/// order, or `None` when there is no such user.
fn user_scram_mechanisms(ctx: &CommandContext, db: &str, user: &str) -> Option<Vec<Bson>> {
    let bytes = ctx.storage.as_deref()?.get_user(db, user).ok()??;
    let record = secantus_core::bsondoc::from_bytes(bytes.as_slice()).ok()?;
    let creds = record.get_document("credentials").ok();
    Some(
        ["SCRAM-SHA-1", "SCRAM-SHA-256"]
            .into_iter()
            .filter(|m| creds.is_some_and(|c| c.contains_key(*m)))
            .map(|m| Bson::String(m.to_string()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(doc: Document) -> Option<i32> {
        validate_awaitable_arguments(&doc, 3).err().map(|e| e.code)
    }

    /// mongod's parse of the awaitable-hello arguments, measured 8.2.11.
    #[test]
    fn awaitable_arguments_are_parsed_as_mongod_does() {
        let pid = hello_process_id();
        let tv = doc! {"processId": pid, "counter": 3_i64};
        assert_eq!(
            code_of(doc! {"hello": 1, "topologyVersion": tv.clone(), "maxAwaitTimeMS": 5}),
            None
        );
        assert_eq!(code_of(doc! {"hello": 1}), None);
        assert_eq!(
            code_of(doc! {"hello": 1, "topologyVersion": Bson::Null, "maxAwaitTimeMS": Bson::Null}),
            None
        );
        // An int32 counter is a type error: the field is a long.
        assert_eq!(
            code_of(
                doc! {"hello": 1, "topologyVersion": {"processId": pid, "counter": 3_i32}, "maxAwaitTimeMS": 5}
            ),
            Some(14)
        );
        assert_eq!(
            code_of(doc! {"hello": 1, "topologyVersion": 1, "maxAwaitTimeMS": 5}),
            Some(14)
        );
        assert_eq!(
            code_of(doc! {"hello": 1, "topologyVersion": {"processId": pid}, "maxAwaitTimeMS": 5}),
            Some(40414)
        );
        assert_eq!(
            code_of(
                doc! {"hello": 1, "topologyVersion": {"processId": pid, "counter": 3_i64, "z": 1}, "maxAwaitTimeMS": 5}
            ),
            Some(40415)
        );
        assert_eq!(
            code_of(doc! {"hello": 1, "topologyVersion": tv.clone(), "maxAwaitTimeMS": "x"}),
            Some(14)
        );
        assert_eq!(
            code_of(doc! {"hello": 1, "topologyVersion": tv.clone(), "maxAwaitTimeMS": -1}),
            Some(2)
        );
        // -0.5 truncates to 0, which is allowed.
        assert_eq!(
            code_of(doc! {"hello": 1, "topologyVersion": tv.clone(), "maxAwaitTimeMS": -0.5}),
            None
        );
        assert_eq!(
            code_of(doc! {"hello": 1, "topologyVersion": tv.clone()}),
            Some(31368)
        );
        assert_eq!(code_of(doc! {"hello": 1, "maxAwaitTimeMS": 5}), Some(31368));
        assert_eq!(
            code_of(
                doc! {"hello": 1, "topologyVersion": {"processId": pid, "counter": -1_i64}, "maxAwaitTimeMS": 5}
            ),
            Some(31372)
        );
        // Newer than ours is an error for THIS process, merely stale for another.
        assert_eq!(
            code_of(
                doc! {"hello": 1, "topologyVersion": {"processId": pid, "counter": 4_i64}, "maxAwaitTimeMS": 5}
            ),
            Some(31382)
        );
        assert_eq!(
            code_of(
                doc! {"hello": 1, "topologyVersion": {"processId": ObjectId::new(), "counter": 4_i64}, "maxAwaitTimeMS": 5}
            ),
            None
        );
    }

    #[test]
    fn only_the_current_topology_is_held() {
        let pid = hello_process_id();
        let current = doc! {"topologyVersion": {"processId": pid, "counter": 3_i64}};
        assert!(awaitable_topology_is_current(&current, 3));
        assert!(!awaitable_topology_is_current(&current, 4));
        let other = doc! {"topologyVersion": {"processId": ObjectId::new(), "counter": 3_i64}};
        assert!(!awaitable_topology_is_current(&other, 3));
    }

    /// With a set name configured, `hello` already claims to be a replica-set
    /// primary; `replSetGetStatus` has to agree. Driver harnesses count the
    /// `members` array to decide whether replica-set behaviour is available —
    /// libmongoc's `test_framework_replset_member_count` skipped every
    /// `/change_stream` suite while this reported zero.
    #[test]
    fn repl_set_get_status_reports_one_live_member_when_a_set_is_configured() {
        let mut ctx = CommandContext::new(1);
        ctx.replica_set_name = Some("secantus".to_string());
        ctx.server_address = Some(("127.0.0.1".to_string(), 27017));
        let r = repl_set_get_status(&doc! {"replSetGetStatus": 1}, &mut ctx).unwrap();
        assert_eq!(r.get_f64("ok").unwrap(), 1.0, "{r:?}");
        assert_eq!(r.get_str("set").unwrap(), "secantus");
        assert_eq!(r.get_i32("myState").unwrap(), 1);
        let members = r.get_array("members").unwrap();
        assert_eq!(members.len(), 1, "one live member: {r:?}");
        let m = members[0].as_document().unwrap();
        assert_eq!(m.get_str("stateStr").unwrap(), "PRIMARY");
        assert_eq!(m.get_str("name").unwrap(), "127.0.0.1:27017");
        assert_eq!(m.get_f64("health").unwrap(), 1.0);
        assert!(m.get_bool("self").unwrap());
    }

    /// Without a set name this really is a standalone, and the
    /// `NoReplicationEnabled` error is the honest answer — harnesses read that
    /// message as "skip replica-set-only behaviour", where a bare
    /// CommandNotFound aborts them.
    #[test]
    fn repl_set_get_status_still_reports_standalone_without_a_set_name() {
        let mut ctx = CommandContext::new(1);
        let r = repl_set_get_status(&doc! {"replSetGetStatus": 1}, &mut ctx).unwrap();
        assert_eq!(r.get_f64("ok").unwrap(), 0.0);
        assert_eq!(r.get_str("codeName").unwrap(), "NoReplicationEnabled");
        assert!(r.get("members").is_none(), "no roster for a standalone");
    }

    /// The topologyVersion processId must be identical across calls — a changing
    /// value makes drivers read a server "restart" and clear the connection pool.
    #[test]
    fn hello_process_id_is_stable_across_calls() {
        assert_eq!(hello_process_id(), hello_process_id());
    }
}
