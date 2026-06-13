//! The handshake command family: `hello` / `isMaster`, `ping`, `buildInfo`.
//!
//! Storage-independent static-ish replies that let a driver complete the
//! connection handshake. Faithful ports of `commands.py::_hello` / `_ping` /
//! `_build_info`.
//!
//! **Deferred to R5 (auth):** `saslSupportedMechs` resolution,
//! `speculativeAuthenticate` (folding a SCRAM client-first into `hello`), and
//! stashing the driver's `client` metadata into the connection registry for
//! `currentOp`. The non-auth handshake path — the default, and what most
//! conformance suites exercise — is complete here.

use bson::{doc, oid::ObjectId, Bson, DateTime, Document};

use crate::{
    CommandContext, HandlerResult, MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE, SERVER_VERSION,
    SERVER_VERSION_ARRAY, WIRE_VERSION,
};

/// `hello` / `isMaster` / `ismaster`. Advertises a single-node `secantus`
/// replica-set primary when a set name is configured (so pymongo's topology
/// machinery accepts change streams), else a plain standalone primary.
///
/// `topologyVersion.counter` and `connectionId` MUST be int64 on the wire — the
/// Go driver rejects the handshake otherwise (see `commands.py::_hello`).
pub fn hello(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let now = DateTime::now();
    let mut response = doc! {
        "isWritablePrimary": true,
        "ismaster": true,
        "topologyVersion": {
            "processId": ObjectId::new(),
            "counter": Bson::Int64(0),
        },
        "maxBsonObjectSize": MAX_BSON_OBJECT_SIZE,
        "maxMessageSizeBytes": MAX_MESSAGE_SIZE,
        "maxWriteBatchSize": 100_000_i32,
        "localTime": now,
        "logicalSessionTimeoutMinutes": 30_i32,
        "connectionId": Bson::Int64(ctx.connection_id),
        "minWireVersion": 0_i32,
        "maxWireVersion": WIRE_VERSION,
        "readOnly": false,
        "ok": 1.0,
    };

    if let (Some(set_name), Some((host, port))) =
        (ctx.replica_set_name.as_ref(), ctx.server_address.as_ref())
    {
        let addr = format!("{host}:{port}");
        // `lastWrite.opTime.ts` mirrors `commands.py`'s
        // `ctx.storage.current_cluster_time()` — mint the next monotonic cluster
        // time (strictly greater than the last write) so `startAtOperationTime`
        // resumes land just past it. Fall back to the supplied `ctx.cluster_time`
        // when no storage backend is wired (handshake-only fakes).
        let ts = Bson::Timestamp(match ctx.storage.as_ref() {
            Some(s) => s.current_cluster_time(),
            None => ctx.cluster_time,
        });
        // Fixed sentinel electionId, matching commands.py.
        let election = ObjectId::parse_str("7fffffff0000000000000001")
            .expect("static electionId hex is valid");
        response.insert("setName", set_name.clone());
        response.insert("setVersion", 1_i32);
        response.insert("hosts", vec![Bson::String(addr.clone())]);
        response.insert("passives", Vec::<Bson>::new());
        response.insert("arbiters", Vec::<Bson>::new());
        response.insert("primary", addr.clone());
        response.insert("me", addr);
        response.insert("electionId", election);
        response.insert(
            "lastWrite",
            doc! {
                "opTime": {"ts": ts.clone(), "t": 1_i32},
                "lastWriteDate": now,
                "majorityOpTime": {"ts": ts, "t": 1_i32},
                "majorityWriteDate": now,
            },
        );
    }

    if ctx.require_auth {
        response.insert("accessControlEnabled", true);
    }

    // `saslSupportedMechs: "<db>.<user>"` — drivers ask which mechanisms to
    // attempt for a principal. We only implement SCRAM-SHA-256, so advertise
    // exactly that (mongod lists whatever the user's credentials carry).
    if doc.get_str("saslSupportedMechs").is_ok() {
        response.insert(
            "saslSupportedMechs",
            vec![Bson::String("SCRAM-SHA-256".to_string())],
        );
    }

    Ok(response)
}

/// `ping` — the trivial liveness probe.
pub fn ping(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "ok": 1.0 })
}

/// `buildInfo` / `buildinfo`. `version` stays at the MongoDB-compatibility value
/// so drivers enable the right feature flags; `secantusVersion` marks the actual
/// build (the crate version here; `commands.py` reads `secantus.__version__`).
pub fn build_info(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! {
        "version": SERVER_VERSION,
        "secantusVersion": env!("CARGO_PKG_VERSION"),
        "gitVersion": "0".repeat(40),
        "versionArray": SERVER_VERSION_ARRAY.iter().map(|n| Bson::Int32(*n)).collect::<Vec<_>>(),
        "bits": 64_i32,
        "debug": false,
        "maxBsonObjectSize": MAX_BSON_OBJECT_SIZE,
        "ok": 1.0,
    })
}
