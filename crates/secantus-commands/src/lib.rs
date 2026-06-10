//! `secantus-commands` — the command-dispatch layer of the Rust server.
//!
//! A port of `src/secantus/commands.py` (R2 in `tasks/rust-server-plan.md`),
//! built incrementally **by command family**. This slice lands the dispatch
//! framework and the storage-independent **handshake family**
//! (`hello`/`isMaster`/`ismaster`, `ping`, `buildInfo`/`buildinfo`) — enough for
//! a pymongo client to complete the connection handshake. CRUD, cursors,
//! aggregate, and auth families slot into the same [`dispatch`] / registry in
//! later slices.
//!
//! ## Contract (mirrors `commands.py::dispatch`)
//!
//! * The command name is the **first key** of the request document
//!   ([`command_name`]).
//! * Cross-cutting validation runs before any handler: `readConcern.level`
//!   (`FailedToParse` / `SnapshotUnavailable`) and `apiVersion` / `apiStrict`
//!   (`APIVersionError` / `APIStrictError`).
//! * A handler returns `Result<Document, CommandError>`; an `Err` (and an
//!   unknown command, `59 CommandNotFound`) is shaped into the standard
//!   `{ok: 0, errmsg, code, codeName}` reply so the connection always survives.
//!
//! Unlike Python — which `try/except`es handler exceptions into either a
//! user-facing `TypeMismatch` or a generic `InternalError` — Rust handlers carry
//! their own typed [`CommandError`] (code + codeName + errmsg), so there is no
//! catch-all here; an unexpected panic is the connection loop's concern (R4).
//!
//! ## Deferred to later slices
//!
//! The cross-cutting machinery `commands.py` threads through dispatch — metrics,
//! session-TTL touch, `--auth` gating, RBAC privilege checks, failpoints,
//! profiling, and `writeConcernError` attachment — is **not** in this slice. It
//! lands with the families that need it (auth/RBAC with R5, etc.). The
//! [`CommandContext`] only carries what the handshake reads today; it grows as
//! families are added.

pub mod admin;
pub mod aggregate;
pub mod auth;
pub mod crud;
pub mod cursors;
pub mod diagnostics;
pub mod distinct;
pub mod find;
pub mod findandmodify;
pub mod handshake;
pub mod rbac;
pub mod storage;
mod util;

use std::sync::{Arc, Mutex};

pub use auth::ConnectionAuth;
use bson::{doc, Bson, Document};
pub use cursors::{CursorError, CursorRegistry};
pub use secantus_wire::{MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE};
pub use storage::{Storage, StorageError, UpdateOutcome};

/// Max wire protocol version advertised in `hello` (mongod 7.0).
pub const WIRE_VERSION: i32 = 17;
/// `version` reported by `buildInfo` — the MongoDB-compatibility value drivers
/// gate feature flags on (change streams, pre-images, …).
pub const SERVER_VERSION: &str = "7.0.0";
/// `versionArray` companion to [`SERVER_VERSION`].
pub const SERVER_VERSION_ARRAY: [i32; 4] = [7, 0, 0, 0];
/// Default cursor batch size when the client doesn't specify one.
pub const DEFAULT_BATCH_SIZE: i32 = 101;

/// Valid `readConcern.level` values (`commands.py::_VALID_READ_CONCERN_LEVELS`).
const VALID_READ_CONCERN_LEVELS: [&str; 5] =
    ["local", "available", "majority", "linearizable", "snapshot"];

/// The per-request state a handler reads. Handshake- and CRUD-scoped for this
/// slice; it grows (auth, sessions, cursors, metrics, …) as command families
/// land. The fields mirror the subset of `commands.py::CommandContext` the
/// ported handlers touch.
#[derive(Clone)]
pub struct CommandContext {
    /// Monotonic per-connection id, surfaced as `connectionId` in `hello`.
    pub connection_id: i64,
    /// The `$db` the command targets (defaults to `"admin"`).
    pub db_name: String,
    /// The server's bound `(host, port)`, for the replica-set `hello` fields.
    pub server_address: Option<(String, u16)>,
    /// The advertised replica-set name (`None` ⇒ a plain standalone `hello`).
    pub replica_set_name: Option<String>,
    /// Whether `--auth` is on (drives `accessControlEnabled` in `hello`).
    pub require_auth: bool,
    /// The cluster time to stamp into `hello`'s `lastWrite.opTime`. Supplied by
    /// the caller (R4 mints it from storage via `current_cluster_time`); a zero
    /// timestamp is harmless when no replica-set block is emitted.
    pub cluster_time: bson::Timestamp,
    /// The storage backend the data-bearing commands run against. `None` until
    /// the server (R4) wires one in; the handshake family doesn't need it.
    pub storage: Option<Arc<dyn Storage>>,
    /// The per-server cursor registry (drives `getMore` / `killCursors`). `None`
    /// until the server wires one in.
    pub cursors: Option<Arc<CursorRegistry>>,
    /// Per-connection authentication state (SCRAM conversation + authenticated
    /// principals), shared across the requests on one socket. `None` until the
    /// server (R4) wires one in; the auth family needs it.
    pub conn_auth: Option<Arc<Mutex<ConnectionAuth>>>,
}

impl CommandContext {
    /// A minimal context for a plaintext standalone connection with no storage
    /// (the handshake family is fully serviceable from this).
    pub fn new(connection_id: i64) -> Self {
        CommandContext {
            connection_id,
            db_name: "admin".to_string(),
            server_address: None,
            replica_set_name: None,
            require_auth: false,
            cluster_time: bson::Timestamp {
                time: 0,
                increment: 0,
            },
            storage: None,
            cursors: None,
            conn_auth: None,
        }
    }

    /// Attach a storage backend (builder-style; used by the server and tests).
    pub fn with_storage(mut self, storage: Arc<dyn Storage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Attach a cursor registry (builder-style; used by the server and tests).
    pub fn with_cursors(mut self, cursors: Arc<CursorRegistry>) -> Self {
        self.cursors = Some(cursors);
        self
    }

    /// Attach per-connection auth state (builder-style; used by the server and
    /// tests). The auth family (`saslStart` / `saslContinue` / …) reads it.
    pub fn with_conn_auth(mut self, conn_auth: Arc<Mutex<ConnectionAuth>>) -> Self {
        self.conn_auth = Some(conn_auth);
        self
    }

    /// The storage backend, or an `InternalError` if none is configured. Data
    /// commands call this; a missing backend is a server-wiring bug, not a
    /// client error.
    pub fn storage(&self) -> Result<&dyn Storage, CommandError> {
        match &self.storage {
            Some(s) => Ok(s.as_ref()),
            None => Err(CommandError::new(
                1,
                "InternalError",
                "storage backend not configured",
            )),
        }
    }

    /// The cursor registry, or an `InternalError` if none is configured.
    pub fn cursors(&self) -> Result<&CursorRegistry, CommandError> {
        match &self.cursors {
            Some(c) => Ok(c.as_ref()),
            None => Err(CommandError::new(
                1,
                "InternalError",
                "cursor registry not configured",
            )),
        }
    }
}

/// A command failure carrying mongod's error triple. Shaped into an `ok: 0`
/// reply by [`CommandError::into_reply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    pub code: i32,
    pub code_name: String,
    pub errmsg: String,
}

impl CommandError {
    pub fn new(code: i32, code_name: impl Into<String>, errmsg: impl Into<String>) -> Self {
        CommandError {
            code,
            code_name: code_name.into(),
            errmsg: errmsg.into(),
        }
    }

    /// `59 CommandNotFound` for an unregistered command name.
    pub fn command_not_found(name: &str) -> Self {
        CommandError::new(59, "CommandNotFound", format!("no such command: '{name}'"))
    }

    /// The standard `{ok: 0, errmsg, code, codeName}` reply document.
    pub fn into_reply(self) -> Document {
        doc! {
            "ok": 0.0,
            "errmsg": self.errmsg,
            "code": self.code,
            "codeName": self.code_name,
        }
    }
}

/// A handler's result: a reply document, or a typed error to be shaped into one.
pub type HandlerResult = Result<Document, CommandError>;

/// A command handler: request document + mutable context → reply.
pub type Handler = fn(&Document, &mut CommandContext) -> HandlerResult;

/// The command name — the first key of the request document
/// (`commands.py::command_name`). Empty string for an empty document.
pub fn command_name(doc: &Document) -> &str {
    doc.keys().next().map(String::as_str).unwrap_or("")
}

/// Resolve a command name (incl. case aliases) to its handler. `None` ⇒
/// `CommandNotFound`. Families are added here as they are ported.
fn lookup(name: &str) -> Option<Handler> {
    Some(match name {
        "hello" | "isMaster" | "ismaster" => handshake::hello,
        "ping" => handshake::ping,
        "buildInfo" | "buildinfo" => handshake::build_info,
        "insert" => crud::insert,
        "update" => crud::update,
        "delete" => crud::delete,
        "count" => crud::count,
        "distinct" => distinct::distinct,
        "findAndModify" | "findandmodify" => findandmodify::find_and_modify,
        "find" => find::find,
        "aggregate" => aggregate::aggregate,
        "getMore" => cursors::get_more,
        "killCursors" => cursors::kill_cursors,
        "create" => admin::create,
        "drop" => admin::drop,
        "listCollections" => admin::list_collections,
        "listIndexes" => admin::list_indexes,
        "createIndexes" => admin::create_indexes,
        "dropIndexes" => admin::drop_indexes,
        "dropDatabase" => admin::drop_database,
        "renameCollection" => admin::rename_collection,
        "collStats" => admin::coll_stats,
        "dbStats" => admin::db_stats,
        "serverStatus" => admin::server_status,
        "startSession" => diagnostics::start_session,
        "endSessions"
        | "refreshSessions"
        | "killSessions"
        | "killAllSessions"
        | "killAllSessionsByPattern" => diagnostics::ok_session_noop,
        "commitTransaction" | "abortTransaction" => diagnostics::ok_transaction,
        "saslStart" => auth::sasl_start,
        "saslContinue" => auth::sasl_continue,
        "createUser" => auth::create_user,
        "dropUser" => auth::drop_user,
        "usersInfo" => auth::users_info,
        "getParameter" => diagnostics::get_parameter,
        "getCmdLineOpts" => diagnostics::get_cmd_line_opts,
        "connectionStatus" => diagnostics::connection_status,
        "whatsmyuri" => diagnostics::whatsmyuri,
        "hostInfo" => diagnostics::host_info,
        "getLog" => diagnostics::get_log,
        _ => return None,
    })
}

/// Dispatch one command to its handler, applying the cross-cutting validation
/// `commands.py::dispatch` runs first. Always returns a reply document (errors
/// are shaped into `ok: 0`), so the connection survives any single command.
pub fn dispatch(doc: &Document, ctx: &mut CommandContext) -> Document {
    let name = command_name(doc);

    if let Err(e) = validate_read_concern(doc) {
        return e.into_reply();
    }
    if let Err(e) = validate_api(doc, name) {
        return e.into_reply();
    }

    match lookup(name) {
        Some(handler) => {
            // Auth gating + RBAC run after CommandNotFound but before the
            // handler (mirrors `commands.py::dispatch`), so an unknown command
            // is still `59` rather than `13` even under `--auth`.
            if let Err(e) = authorize(name, doc, ctx) {
                return e.into_reply();
            }
            match handler(doc, ctx) {
                Ok(reply) => reply,
                Err(e) => e.into_reply(),
            }
        }
        None => CommandError::command_not_found(name).into_reply(),
    }
}

/// `--auth` gating + RBAC privilege check (`commands.py::dispatch`). A no-op when
/// `require_auth` is off (default-allow). When on: any command outside
/// [`is_pre_auth_command`] requires an authenticated principal (`Unauthorized`,
/// 13); an authenticated principal must then hold the command's action grant
/// (also `Unauthorized`). Pre-auth and explicitly exempt commands
/// ([`is_no_privilege_command`]) bypass the privilege check.
fn authorize(name: &str, doc: &Document, ctx: &CommandContext) -> Result<(), CommandError> {
    if !ctx.require_auth {
        return Ok(());
    }
    let authenticated = ctx
        .conn_auth
        .as_ref()
        .map(|a| {
            a.lock()
                .expect("conn auth mutex poisoned")
                .is_authenticated()
        })
        .unwrap_or(false);

    if !is_pre_auth_command(name) && !authenticated {
        return Err(CommandError::new(
            13,
            "Unauthorized",
            format!("command {name} requires authentication"),
        ));
    }

    if authenticated && !is_pre_auth_command(name) && !is_no_privilege_command(name) {
        if let Some((action, scope)) = command_action(name) {
            let (target_db, cluster) = resource_for_command(name, doc, scope, &ctx.db_name);
            let roles = ctx
                .conn_auth
                .as_ref()
                .map(|a| {
                    a.lock()
                        .expect("conn auth mutex poisoned")
                        .effective_roles
                        .clone()
                })
                .unwrap_or_default();
            if !rbac::check_privilege(&roles, action, target_db.as_deref(), cluster) {
                return Err(CommandError::new(
                    13,
                    "Unauthorized",
                    format!(
                        "not authorized on {} to execute command (action: {action})",
                        target_db.as_deref().unwrap_or("cluster")
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Commands a connection may invoke before authenticating (`--auth` on): the
/// driver handshake plus the SCRAM round-trip itself. Mirrors
/// `commands.py::_PRE_AUTH_COMMANDS`.
fn is_pre_auth_command(name: &str) -> bool {
    matches!(
        name,
        "hello"
            | "isMaster"
            | "ismaster"
            | "ping"
            | "buildInfo"
            | "buildinfo"
            | "saslStart"
            | "saslContinue"
            | "endSessions"
            | "whatsmyuri"
    )
}

/// Commands that skip the RBAC privilege check even when authenticated: cursor
/// continuation (authorized at find/aggregate time), session administrivia, and
/// metadata the driver depends on. Mirrors `commands.py::_NO_PRIVILEGE_COMMANDS`.
fn is_no_privilege_command(name: &str) -> bool {
    matches!(
        name,
        "getMore"
            | "endSessions"
            | "startSession"
            | "refreshSessions"
            | "ping"
            | "ismaster"
            | "isMaster"
            | "hello"
            | "buildInfo"
            | "buildinfo"
            | "whatsmyuri"
            | "saslStart"
            | "saslContinue"
            | "connectionStatus"
            | "abortTransaction"
            | "commitTransaction"
    )
}

/// The `(action, scope)` a command needs, or `None` for "no privilege required"
/// (a permissive default — an authenticated user with any role can invoke it).
/// Mirrors `commands.py::_COMMAND_ACTIONS`.
fn command_action(name: &str) -> Option<(&'static str, &'static str)> {
    use rbac::*;
    Some(match name {
        "find" | "count" | "distinct" | "aggregate" => (A_FIND, SCOPE_COLLECTION),
        "insert" => (A_INSERT, SCOPE_COLLECTION),
        "update" => (A_UPDATE, SCOPE_COLLECTION),
        "delete" => (A_REMOVE, SCOPE_COLLECTION),
        "findAndModify" | "findandmodify" => (A_UPDATE, SCOPE_COLLECTION),
        "killCursors" => (A_KILL_CURSORS, SCOPE_COLLECTION),
        "listIndexes" => (A_LIST_INDEXES, SCOPE_COLLECTION),
        "createIndexes" => (A_CREATE_INDEX, SCOPE_COLLECTION),
        "dropIndexes" => (A_DROP_INDEX, SCOPE_COLLECTION),
        "create" => (A_CREATE_COLLECTION, SCOPE_DATABASE),
        "drop" => (A_DROP_COLLECTION, SCOPE_COLLECTION),
        "dropDatabase" => (A_DROP_DATABASE, SCOPE_DATABASE),
        "renameCollection" => (A_RENAME_COLL_SAME_DB, SCOPE_COLLECTION),
        "listCollections" => (A_LIST_COLLECTIONS, SCOPE_DATABASE),
        "dbStats" | "dbstats" => (A_DB_STATS, SCOPE_DATABASE),
        "collStats" => (A_COLL_STATS, SCOPE_COLLECTION),
        "createUser" => (A_CREATE_USER, SCOPE_DATABASE),
        "dropUser" => (A_DROP_USER, SCOPE_DATABASE),
        "usersInfo" => (A_VIEW_USER, SCOPE_DATABASE),
        "serverStatus" => (A_SERVER_STATUS, SCOPE_CLUSTER),
        "hostInfo" => (A_HOST_INFO, SCOPE_CLUSTER),
        "getCmdLineOpts" => (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
        "getParameter" => (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
        "getLog" => (A_GET_LOG, SCOPE_CLUSTER),
        _ => return None,
    })
}

/// Resolve the `(target_db, cluster_flag)` an action operates on. Cluster-scoped
/// commands return `(None, true)`; collection/database commands use the
/// connection's `$db`, except `renameCollection` whose source db is its
/// namespace prefix. Mirrors `commands.py::_resource_for_command`.
fn resource_for_command(
    name: &str,
    doc: &Document,
    scope: &str,
    default_db: &str,
) -> (Option<String>, bool) {
    if scope == rbac::SCOPE_CLUSTER {
        return (None, true);
    }
    if name == "renameCollection" {
        if let Ok(ns) = doc.get_str("renameCollection") {
            if let Some((db, _)) = ns.split_once('.') {
                return (Some(db.to_string()), false);
            }
        }
    }
    (Some(default_db.to_string()), false)
}

/// `readConcern.level` validation (`commands.py::dispatch`): an invalid level is
/// `FailedToParse` (9); `snapshot` is `SnapshotUnavailable` (246) on standalone.
fn validate_read_concern(doc: &Document) -> Result<(), CommandError> {
    let Some(Bson::Document(rc)) = doc.get("readConcern") else {
        return Ok(());
    };
    let Some(level) = rc.get("level") else {
        return Ok(());
    };
    match level.as_str() {
        Some(l) if VALID_READ_CONCERN_LEVELS.contains(&l) => {
            if l == "snapshot" {
                return Err(CommandError::new(
                    246,
                    "SnapshotUnavailable",
                    "Snapshot read concern is not supported on standalone",
                ));
            }
            Ok(())
        }
        _ => Err(CommandError::new(
            9,
            "FailedToParse",
            format!(
                "Specified readConcern level {} is not valid",
                py_repr(level)
            ),
        )),
    }
}

/// `apiVersion` / `apiStrict` validation (`commands.py::dispatch`). An
/// unsupported `apiVersion` is `APIVersionError` (322); under `apiStrict: true`,
/// a command outside API Version 1 is `APIStrictError` (323). The `apiStrict`
/// aggregation-stage gate lands with the aggregate family.
fn validate_api(doc: &Document, name: &str) -> Result<(), CommandError> {
    if let Some(av) = doc.get("apiVersion") {
        if av.as_str() != Some("1") {
            return Err(CommandError::new(
                322,
                "APIVersionError",
                format!(
                    "Provided apiVersion {} is not supported. Supported versions: [\"1\"]",
                    py_repr(av)
                ),
            ));
        }
    }
    if doc.get("apiStrict").and_then(Bson::as_bool) == Some(true) && name == "distinct" {
        return Err(CommandError::new(
            323,
            "APIStrictError",
            format!("Provided command {name} is not in API Version 1"),
        ));
    }
    Ok(())
}

/// Render a BSON value the way Python's `{!r}` does for the messages above:
/// strings single-quoted, everything else via its `Display`. (Drivers gate on
/// the integer `code`; this only shapes the human-readable `errmsg`.)
fn py_repr(v: &Bson) -> String {
    match v {
        Bson::String(s) => format!("'{s}'"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn ctx() -> CommandContext {
        CommandContext::new(7)
    }

    #[test]
    fn command_name_is_first_key() {
        assert_eq!(command_name(&doc! {"ping": 1, "$db": "admin"}), "ping");
        assert_eq!(command_name(&Document::new()), "");
    }

    #[test]
    fn unknown_command_is_command_not_found() {
        let reply = dispatch(&doc! {"bogusCommand": 1}, &mut ctx());
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 59);
        assert_eq!(reply.get_str("codeName").unwrap(), "CommandNotFound");
        assert!(reply.get_str("errmsg").unwrap().contains("bogusCommand"));
    }

    #[test]
    fn ping_ok() {
        let reply = dispatch(&doc! {"ping": 1}, &mut ctx());
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    }

    #[test]
    fn hello_standalone_shape() {
        let reply = dispatch(&doc! {"hello": 1}, &mut ctx());
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert!(reply.get_bool("isWritablePrimary").unwrap());
        assert!(reply.get_bool("ismaster").unwrap());
        assert_eq!(reply.get_i32("maxWireVersion").unwrap(), WIRE_VERSION);
        assert_eq!(
            reply.get_i32("maxBsonObjectSize").unwrap(),
            MAX_BSON_OBJECT_SIZE
        );
        // connectionId and topologyVersion.counter must be int64 on the wire.
        assert!(matches!(reply.get("connectionId"), Some(Bson::Int64(7))));
        let tv = reply.get_document("topologyVersion").unwrap();
        assert!(matches!(tv.get("counter"), Some(Bson::Int64(0))));
        // No replica-set fields without a configured set name.
        assert!(reply.get("setName").is_none());
        assert!(reply.get("accessControlEnabled").is_none());
    }

    #[test]
    fn ismaster_alias_routes_to_hello() {
        let reply = dispatch(&doc! {"isMaster": 1}, &mut ctx());
        assert!(reply.get_bool("ismaster").unwrap());
        let reply2 = dispatch(&doc! {"ismaster": 1}, &mut ctx());
        assert!(reply2.get_bool("isWritablePrimary").unwrap());
    }

    #[test]
    fn hello_replica_set_block() {
        let mut c = ctx();
        c.server_address = Some(("127.0.0.1".into(), 27017));
        c.replica_set_name = Some("secantus".into());
        c.require_auth = true;
        c.cluster_time = bson::Timestamp {
            time: 100,
            increment: 1,
        };
        let reply = dispatch(&doc! {"hello": 1}, &mut c);
        assert_eq!(reply.get_str("setName").unwrap(), "secantus");
        assert_eq!(reply.get_str("primary").unwrap(), "127.0.0.1:27017");
        assert_eq!(reply.get_str("me").unwrap(), "127.0.0.1:27017");
        let hosts = reply.get_array("hosts").unwrap();
        assert_eq!(hosts, &vec![Bson::String("127.0.0.1:27017".into())]);
        assert!(reply.get_bool("accessControlEnabled").unwrap());
        let last_write = reply.get_document("lastWrite").unwrap();
        let op_time = last_write.get_document("opTime").unwrap();
        assert_eq!(
            op_time.get("ts"),
            Some(&Bson::Timestamp(bson::Timestamp {
                time: 100,
                increment: 1
            }))
        );
    }

    #[test]
    fn build_info_shape() {
        let reply = dispatch(&doc! {"buildInfo": 1}, &mut ctx());
        assert_eq!(reply.get_str("version").unwrap(), SERVER_VERSION);
        assert_eq!(reply.get_i32("bits").unwrap(), 64);
        assert_eq!(reply.get_array("versionArray").unwrap().len(), 4);
        assert_eq!(reply.get_str("gitVersion").unwrap().len(), 40);
        // alias
        assert_eq!(
            dispatch(&doc! {"buildinfo": 1}, &mut ctx())
                .get_str("version")
                .unwrap(),
            SERVER_VERSION
        );
    }

    #[test]
    fn invalid_read_concern_level_is_failed_to_parse() {
        let reply = dispatch(
            &doc! {"find": "c", "readConcern": {"level": "bogus"}},
            &mut ctx(),
        );
        assert_eq!(reply.get_i32("code").unwrap(), 9);
        assert_eq!(reply.get_str("codeName").unwrap(), "FailedToParse");
        assert!(reply.get_str("errmsg").unwrap().contains("'bogus'"));
    }

    #[test]
    fn snapshot_read_concern_is_unavailable_on_standalone() {
        let reply = dispatch(
            &doc! {"find": "c", "readConcern": {"level": "snapshot"}},
            &mut ctx(),
        );
        assert_eq!(reply.get_i32("code").unwrap(), 246);
        assert_eq!(reply.get_str("codeName").unwrap(), "SnapshotUnavailable");
    }

    #[test]
    fn valid_read_concern_passes_through() {
        // 'majority' is valid; the command itself is unknown here, so we land on
        // CommandNotFound — proving readConcern validation didn't short-circuit.
        let reply = dispatch(
            &doc! {"unknownCmd": 1, "readConcern": {"level": "majority"}},
            &mut ctx(),
        );
        assert_eq!(reply.get_i32("code").unwrap(), 59);
    }

    #[test]
    fn unsupported_api_version_is_rejected() {
        let reply = dispatch(&doc! {"ping": 1, "apiVersion": "2"}, &mut ctx());
        assert_eq!(reply.get_i32("code").unwrap(), 322);
        assert_eq!(reply.get_str("codeName").unwrap(), "APIVersionError");
    }

    #[test]
    fn api_strict_rejects_distinct_by_name() {
        let reply = dispatch(
            &doc! {"distinct": "c", "apiVersion": "1", "apiStrict": true},
            &mut ctx(),
        );
        assert_eq!(reply.get_i32("code").unwrap(), 323);
        assert_eq!(reply.get_str("codeName").unwrap(), "APIStrictError");
    }

    #[test]
    fn api_strict_allows_listed_command() {
        // ping is in API V1; apiStrict shouldn't reject it.
        let reply = dispatch(
            &doc! {"ping": 1, "apiVersion": "1", "apiStrict": true},
            &mut ctx(),
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    }
}
