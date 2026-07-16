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

use std::sync::OnceLock;

use bson::{doc, oid::ObjectId, Bson, DateTime, Document};

use crate::{
    CommandContext, HandlerResult, MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE, SERVER_VERSION,
    SERVER_VERSION_ARRAY, WIRE_VERSION,
};

/// `topologyVersion.processId` identifies the server *process* and is fixed for
/// its lifetime. The SDAM spec compares it across heartbeats; a *changed*
/// processId is read as "the server restarted", making drivers invalidate and
/// clear the connection pool (close + reconnect). Minting a fresh `ObjectId` per
/// hello therefore triggered a spurious pool-clear on nearly every monitoring
/// heartbeat — so pin it once per process. (Java-gauge finding)
fn hello_process_id() -> ObjectId {
    static PROCESS_ID: OnceLock<ObjectId> = OnceLock::new();
    *PROCESS_ID.get_or_init(ObjectId::new)
}

/// `hello` / `isMaster` / `ismaster`. Advertises a single-node `secantus`
/// replica-set primary when a set name is configured (so pymongo's topology
/// machinery accepts change streams), else a plain standalone primary.
///
/// `topologyVersion.counter` and `connectionId` MUST be int64 on the wire — the
/// Go driver rejects the handshake otherwise (see `commands.py::_hello`).
pub fn hello(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // Capture the driver `client` metadata from the handshake so `currentOp` can
    // surface it as `clientMetadata`. Only the first hello carries it; later
    // helloes (monitoring) omit it, so don't clobber a stored value with None.
    if let Some(client) = doc.get_document("client").ok().cloned() {
        if let Some(conn_auth) = ctx.conn_auth.as_ref() {
            if let Ok(mut guard) = conn_auth.lock() {
                guard.client_metadata = Some(client);
            }
        }
    }

    let now = DateTime::now();
    let mut response = doc! {
        "isWritablePrimary": true,
        "ismaster": true,
        "topologyVersion": {
            "processId": hello_process_id(),
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
pub fn repl_set_get_status(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! {
        "ok": 0.0,
        "errmsg": "not running with --replSet",
        "code": 76_i32,
        "codeName": "NoReplicationEnabled",
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The topologyVersion processId must be identical across calls — a changing
    /// value makes drivers read a server "restart" and clear the connection pool.
    #[test]
    fn hello_process_id_is_stable_across_calls() {
        assert_eq!(hello_process_id(), hello_process_id());
    }
}
