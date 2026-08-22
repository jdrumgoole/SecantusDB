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
//! Some cross-cutting machinery `commands.py` threads through dispatch — metrics,
//! session-TTL touch, profiling, failpoints — is **not** ported yet. (`--auth`
//! gating + RBAC landed with R5; `writeConcernError` for an unsatisfiable `w > 1`
//! is attached in [`dispatch`].) The [`CommandContext`] grows as families land.

pub mod admin;
pub mod aggregate;
pub mod auth;
pub mod changestream;
pub mod crud;
pub mod cursors;
pub mod diagnostics;
pub mod distinct;
pub mod failpoints;
pub mod find;
pub mod findandmodify;
pub mod handshake;
pub mod logbuf;
pub mod mapreduce;
pub mod rbac;
pub mod roles;
pub mod storage;
pub mod transactions;
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
///
/// This is the FIRST-batch default only. mongod's 101-document default applies
/// to `find` / `aggregate` first batches; an unspecified `batchSize` on a
/// `getMore` means "as many documents as fit in [`MAX_GETMORE_BATCH_BYTES`]",
/// so a full scan drains in ~2 round trips, not `count / 101`.
pub const DEFAULT_BATCH_SIZE: i32 = 101;
/// Byte budget for a single cursor batch (mongod's 16MB reply-document cap).
pub const MAX_GETMORE_BATCH_BYTES: usize = 16 * 1024 * 1024;

/// Valid `readConcern.level` values (`commands.py::_VALID_READ_CONCERN_LEVELS`).
const VALID_READ_CONCERN_LEVELS: [&str; 5] =
    ["local", "available", "majority", "linearizable", "snapshot"];

/// Lets `killOp` reach the server's live-connection registry to close a
/// connection by its `conn_id` (our per-connection opid — one in-flight op per
/// connection). Implemented by the server over its connection map; `None` in
/// unit-test / storage-less contexts (where `killOp` reports "no connection
/// registry", matching the Python server).
pub trait ConnectionKiller: Send + Sync {
    /// Close the connection with `conn_id`, returning whether one was found.
    fn kill(&self, conn_id: i64) -> bool;
}

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
    /// The per-server multi-document-transaction registry (drives the
    /// `autocommit: false` envelope + `commitTransaction` / `abortTransaction`).
    /// `None` until the server wires one in (and on standalone-less test paths).
    pub transactions: Option<Arc<transactions::TransactionRegistry>>,
    /// Per-connection authentication state (SCRAM conversation + authenticated
    /// principals), shared across the requests on one socket. `None` until the
    /// server (R4) wires one in; the auth family needs it.
    pub conn_auth: Option<Arc<Mutex<ConnectionAuth>>>,
    /// The verified TLS client certificate's subject DN (RFC 4514), captured
    /// once per connection by the server's accept loop when mTLS is on. `None`
    /// on plaintext connections or when no client cert was presented; the
    /// `MONGODB-X509` mechanism (R5c) reads it.
    pub peer_cert_dn: Option<String>,
    /// Server-wide `configureFailPoint` registry. `None` in unit-test contexts;
    /// the server wires one in so `failCommand` short-circuits in dispatch.
    pub failpoints: Option<Arc<failpoints::FailPointRegistry>>,
    /// Set by a `failCommand` failpoint with `closeConnection: true` — the
    /// server drops the socket after dispatch instead of replying, so the driver
    /// sees a network error (the drivers' retryability / socket-error tests).
    pub close_connection: bool,
    /// The server's live-connection registry, so `killOp` can close a connection
    /// by its `conn_id`. `None` until the server wires one in (and in unit-test
    /// contexts).
    pub conn_killer: Option<Arc<dyn ConnectionKiller>>,
    /// The server's in-memory log ring buffer, read by `getLog`. `None` until the
    /// server wires one in (and in unit-test contexts) — `getLog` then reports an
    /// empty log.
    pub logs: Option<Arc<logbuf::LogBuffer>>,
    /// Live connection counts for `serverStatus.connections`, snapshotted by the
    /// server when it builds the context. `None` off-server (unit tests) ⇒ zeros.
    pub conn_stats: Option<ConnStats>,
    /// Set by a cursor-producing handler (`find` / `getMore`) to hand the
    /// server the reply's document batch as **pre-encoded blobs** instead of an
    /// owned `Bson::Array` inside the reply document. The reply the handler
    /// returns then carries `cursor: { id, ns }` *without* the batch; the server
    /// splices the blobs onto the wire via `secantus_wire::encode_cursor_reply`,
    /// skipping the decode→re-encode round-trip that materialising them into the
    /// reply document would cost (`tasks/rust-perf-findings.md`, Phase 1).
    /// Out-of-band like `close_connection`. `None` for every non-cursor reply.
    pub pending_batch: Option<PendingBatch>,
    /// The raw BSON byte slices of an `insert`'s kind-1 `documents` sequence,
    /// handed in by the server **un-decoded** so the insert handler can store the
    /// client's bytes verbatim (the raw-BSON write path) instead of paying a
    /// merge-decode → re-encode → storage-decode round-trip per document. `None`
    /// unless the request is an `insert` whose documents arrived as an `OP_MSG`
    /// kind-1 sequence (the common driver shape); inline-in-body inserts and every
    /// other command leave it `None` and take the decoded path. Consumed
    /// (`take()`n) by the insert handler.
    pub raw_insert_documents: Option<Vec<Vec<u8>>>,
}

/// A cursor reply's document batch as pre-encoded BSON blobs, plus which field
/// (`"firstBatch"` for `find`, `"nextBatch"` for `getMore`) it splices into
/// `cursor`. See `CommandContext::pending_batch`.
#[derive(Clone)]
pub struct PendingBatch {
    pub batch_field: &'static str,
    pub batch: Vec<Vec<u8>>,
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
            transactions: None,
            conn_auth: None,
            peer_cert_dn: None,
            failpoints: None,
            close_connection: false,
            conn_killer: None,
            logs: None,
            conn_stats: None,
            pending_batch: None,
            raw_insert_documents: None,
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

    /// Attach a transaction registry (builder-style; used by the server and
    /// tests). Drives the multi-document-transaction envelope.
    pub fn with_transactions(
        mut self,
        transactions: Arc<transactions::TransactionRegistry>,
    ) -> Self {
        self.transactions = Some(transactions);
        self
    }

    /// Attach per-connection auth state (builder-style; used by the server and
    /// tests). The auth family (`saslStart` / `saslContinue` / …) reads it.
    pub fn with_conn_auth(mut self, conn_auth: Arc<Mutex<ConnectionAuth>>) -> Self {
        self.conn_auth = Some(conn_auth);
        self
    }

    /// Attach the server's live-connection registry (builder-style). `killOp`
    /// uses it to close a connection by its `conn_id`.
    pub fn with_conn_killer(mut self, conn_killer: Arc<dyn ConnectionKiller>) -> Self {
        self.conn_killer = Some(conn_killer);
        self
    }

    /// Attach the server's in-memory log ring buffer (builder-style). `getLog`
    /// reads it.
    pub fn with_logs(mut self, logs: Arc<logbuf::LogBuffer>) -> Self {
        self.logs = Some(logs);
        self
    }

    /// Attach this instant's connection counts (builder-style).
    pub fn with_conn_stats(mut self, stats: ConnStats) -> Self {
        self.conn_stats = Some(stats);
        self
    }

    /// Attach the server-wide failpoint registry (builder-style). `dispatch`
    /// applies matching `failCommand` failpoints; `configureFailPoint` configures.
    pub fn with_failpoints(mut self, failpoints: Arc<failpoints::FailPointRegistry>) -> Self {
        self.failpoints = Some(failpoints);
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
#[derive(Debug, Clone, PartialEq)]
pub struct CommandError {
    pub code: i32,
    pub code_name: String,
    pub errmsg: String,
    /// Extra top-level fields merged into the error reply (e.g. a DuplicateKey's
    /// `keyPattern` / `keyValue`). `None` for the common case — boxed so the
    /// error type stays small in `Result`. (`bson::Document` is `PartialEq` but
    /// not `Eq`, so this type is `PartialEq`-only.)
    pub extra: Option<Box<Document>>,
}

impl CommandError {
    pub fn new(code: i32, code_name: impl Into<String>, errmsg: impl Into<String>) -> Self {
        CommandError {
            code,
            code_name: code_name.into(),
            errmsg: errmsg.into(),
            extra: None,
        }
    }

    /// Attach extra top-level fields to the error reply (merged by `into_reply`).
    pub fn with_extra(mut self, extra: Document) -> Self {
        self.extra = Some(Box::new(extra));
        self
    }

    /// `59 CommandNotFound` for an unregistered command name.
    pub fn command_not_found(name: &str) -> Self {
        CommandError::new(59, "CommandNotFound", format!("no such command: '{name}'"))
    }

    /// The standard `{ok: 0, errmsg, code, codeName}` reply document, plus any
    /// `extra` fields (e.g. `keyPattern` / `keyValue`).
    pub fn into_reply(self) -> Document {
        let mut reply = doc! {
            "ok": 0.0,
            "errmsg": self.errmsg,
            "code": self.code,
            "codeName": self.code_name,
        };
        if let Some(extra) = self.extra {
            for (k, v) in *extra {
                reply.insert(k, v);
            }
        }
        reply
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

/// A snapshot of the server's connection counters for `serverStatus`.
///
/// mongo-c-driver's `/Client/exhaust_cursor/{single,pool}` assert that opening
/// an exhaust cursor *creates a connection* — they read `connections.
/// totalCreated` before and after and require it to rise. Reporting a constant
/// zero fails that just as surely as omitting the field.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnStats {
    /// Connections currently open.
    pub current: i64,
    /// Connections created over the server's lifetime.
    pub total_created: i64,
}

/// Test hook: whether a command name resolves to a handler at all. A name that
/// doesn't is answered with `CommandNotFound`, which some driver suites treat
/// as a different outcome from a registered-but-unsupported command.
#[cfg(test)]
pub(crate) fn lookup_for_test(name: &str) -> Option<Handler> {
    lookup(name)
}

/// Resolve a command name (incl. case aliases) to its handler. `None` ⇒
/// `CommandNotFound`. Families are added here as they are ported.
fn lookup(name: &str) -> Option<Handler> {
    Some(match name {
        "hello" | "isMaster" | "ismaster" => handshake::hello,
        "ping" => handshake::ping,
        "replSetGetStatus" => handshake::repl_set_get_status,
        "buildInfo" | "buildinfo" => handshake::build_info,
        "insert" => crud::insert,
        "update" => crud::update,
        "delete" => crud::delete,
        "count" => crud::count,
        "distinct" => distinct::distinct,
        "mapReduce" | "mapreduce" => mapreduce::map_reduce,
        "findAndModify" | "findandmodify" => findandmodify::find_and_modify,
        "find" => find::find,
        "aggregate" => aggregate::aggregate,
        "getMore" => cursors::get_more,
        "killCursors" => cursors::kill_cursors,
        "create" => admin::create,
        "collMod" => admin::coll_mod,
        "collmod" => admin::coll_mod,
        "explain" => admin::explain,
        "drop" => admin::drop,
        "secantusAdmin.backupArchive" => admin::backup_archive,
        "secantusAdmin.archiveBaseSnapshot" => admin::archive_base_snapshot,
        "secantusAdmin.pruneOplog" => admin::prune_oplog,
        "secantusAdmin.pruneTtl" => admin::prune_ttl,
        "secantusAdmin.restoreArchive" => admin::restore_archive,
        "listCollections" => admin::list_collections,
        "listDatabases" => admin::list_databases,
        "listIndexes" => admin::list_indexes,
        "createIndexes" => admin::create_indexes,
        "createSearchIndexes" | "updateSearchIndex" | "dropSearchIndex" => {
            admin::search_index_not_supported
        }
        "dropIndexes" => admin::drop_indexes,
        "dropDatabase" => admin::drop_database,
        "renameCollection" => admin::rename_collection,
        "collStats" => admin::coll_stats,
        // mongod accepts the lowercase `dbstats` spelling too (the C driver's
        // command_fully_qualified test sends it over the legacy OP_QUERY path).
        "dbStats" | "dbstats" => admin::db_stats,
        "serverStatus" => admin::server_status,
        "currentOp" => admin::current_op,
        "killOp" => diagnostics::kill_op,
        "validate" => admin::validate,
        "profile" => admin::profile,
        "startSession" => diagnostics::start_session,
        "endSessions"
        | "refreshSessions"
        | "killSessions"
        | "killAllSessions"
        | "killAllSessionsByPattern" => diagnostics::ok_session_noop,
        "commitTransaction" => commit_transaction,
        "abortTransaction" => abort_transaction,
        "saslStart" => auth::sasl_start,
        "saslContinue" => auth::sasl_continue,
        "authenticate" => auth::authenticate,
        "createUser" => auth::create_user,
        "updateUser" => auth::update_user,
        "dropUser" => auth::drop_user,
        "grantRolesToUser" => auth::grant_roles_to_user,
        "revokeRolesFromUser" => auth::revoke_roles_from_user,
        "dropAllUsersFromDatabase" => auth::drop_all_users_from_database,
        "usersInfo" => auth::users_info,
        "createRole" => roles::create_role,
        "updateRole" => roles::update_role,
        "dropRole" => roles::drop_role,
        "dropAllRolesFromDatabase" => roles::drop_all_roles_from_database,
        "grantPrivilegesToRole" => roles::grant_privileges_to_role,
        "revokePrivilegesFromRole" => roles::revoke_privileges_from_role,
        "grantRolesToRole" => roles::grant_roles_to_role,
        "revokeRolesFromRole" => roles::revoke_roles_from_role,
        "rolesInfo" => roles::roles_info,
        "getParameter" => diagnostics::get_parameter,
        "getCmdLineOpts" => diagnostics::get_cmd_line_opts,
        "connectionStatus" => diagnostics::connection_status,
        "whatsmyuri" => diagnostics::whatsmyuri,
        "fsync" => diagnostics::fsync,
        "hostInfo" => diagnostics::host_info,
        "top" => diagnostics::top,
        "getLog" => diagnostics::get_log,
        "configureFailPoint" => configure_fail_point,
        _ => return None,
    })
}

/// `configureFailPoint` — install / replace / disable a `failCommand` failpoint
/// (other names are accept-but-ignore). Mirrors `commands._configure_fail_point`.
fn configure_fail_point(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = match doc.get("configureFailPoint") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "configureFailPoint requires a string name",
            ))
        }
    };
    let mode = doc
        .get("mode")
        .cloned()
        .unwrap_or_else(|| Bson::String("off".to_string()));
    let data = doc
        .get("data")
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default();
    if let Some(reg) = &ctx.failpoints {
        reg.configure(&name, &mode, &data);
    }
    Ok(doc! { "ok": 1.0 })
}

/// Dispatch one command to its handler, applying the cross-cutting validation
/// `commands.py::dispatch` runs first. Always returns a reply document (errors
/// are shaped into `ok: 0`), so the connection survives any single command.
pub fn dispatch(doc: &Document, ctx: &mut CommandContext) -> Document {
    let mut reply = dispatch_inner(doc, ctx);
    attach_write_concern_error(doc, &mut reply);
    attach_cluster_time_gossip(doc, &mut reply, ctx);
    reply
}

/// Attach a `writeConcernError` when the command carried an integer `w > 1`.
/// SecantusDB advertises as a single-node `secantus` replica set, so a write
/// concern wider than one node can never be satisfied — mongod returns the write
/// result *plus* a `CannotSatisfyWriteConcern` (100) writeConcernError (the write
/// still happened). Only attaches to a successful reply that carried a write
/// concern (reads don't send one), mirroring `commands.py`.
fn attach_write_concern_error(doc: &Document, reply: &mut Document) {
    if reply.get_f64("ok").unwrap_or(0.0) != 1.0 || reply.contains_key("writeConcernError") {
        return;
    }
    let w = doc
        .get("writeConcern")
        .and_then(bson::Bson::as_document)
        .and_then(|wc| wc.get("w"));
    let unsatisfiable = match w {
        Some(bson::Bson::Int32(n)) => *n > 1,
        Some(bson::Bson::Int64(n)) => *n > 1,
        _ => false,
    };
    if unsatisfiable {
        let mut wce = Document::new();
        wce.insert("code", 100i32);
        wce.insert("codeName", "CannotSatisfyWriteConcern");
        wce.insert("errmsg", "Not enough data-bearing nodes");
        reply.insert("writeConcernError", wce);
    }
}

/// Commands that accept a `writeConcern` and whose malformed value mongod
/// rejects *before* running — the set `commands.py::_validate_write_concern` is
/// prepended to (insert / update / delete / findAndModify / create / collMod /
/// createIndexes / drop / dropIndexes / dropDatabase / renameCollection).
fn is_write_concern_command(name: &str) -> bool {
    matches!(
        name,
        "insert"
            | "update"
            | "delete"
            | "findAndModify"
            | "findandmodify"
            | "create"
            | "collMod"
            | "collmod"
            | "createIndexes"
            | "drop"
            | "dropIndexes"
            | "dropDatabase"
            | "renameCollection"
    )
}

/// Reject a malformed `writeConcern` before a write command runs, mirroring
/// `commands.py::_validate_write_concern`: a non-document `writeConcern` or a
/// non-bool/int `j` / non-number `wtimeout` → `TypeMismatch` (14); a `w` that's a
/// bool or non-number/string → `TypeMismatch` (14); a string `w` other than
/// `"majority"` → `UnknownReplWriteConcern` (79); an integer `w` outside `[0, 50]`
/// → `FailedToParse` (9). `None` when absent or well-formed. (`w > 1` still
/// succeeds with a `writeConcernError` attached — see `attach_write_concern_error`.)
fn validate_write_concern(doc: &Document) -> Option<CommandError> {
    let wc = match doc.get("writeConcern") {
        None => return None,
        Some(Bson::Document(d)) => d,
        Some(_) => {
            return Some(CommandError::new(
                14,
                "TypeMismatch",
                "writeConcern must be a document",
            ))
        }
    };
    if let Some(w) = wc.get("w") {
        match w {
            Bson::Int32(_) | Bson::Int64(_) => {
                let n = if let Bson::Int32(x) = w {
                    *x as i64
                } else if let Bson::Int64(x) = w {
                    *x
                } else {
                    0
                };
                if !(0..=50).contains(&n) {
                    return Some(CommandError::new(
                        9,
                        "FailedToParse",
                        "w has to be a non-negative number and not greater than 50",
                    ));
                }
            }
            Bson::String(s) if s == "majority" => {}
            Bson::String(s) => {
                return Some(CommandError::new(
                    79,
                    "UnknownReplWriteConcern",
                    format!("No write concern mode named '{s}' found in replica set configuration"),
                ))
            }
            _ => {
                return Some(CommandError::new(
                    14,
                    "TypeMismatch",
                    "writeConcern.w must be a number or string",
                ))
            }
        }
    }
    if let Some(j) = wc.get("j") {
        if !matches!(j, Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_)) {
            return Some(CommandError::new(
                14,
                "TypeMismatch",
                "writeConcern.j must be a boolean or integer",
            ));
        }
    }
    if let Some(wt) = wc.get("wtimeout") {
        if !matches!(wt, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)) {
            return Some(CommandError::new(
                14,
                "TypeMismatch",
                "writeConcern.wtimeout must be a number",
            ));
        }
    }
    None
}

/// Refuse a direct `insert` / `update` / `delete` on a synthetic read-only view,
/// mirroring `commands.py::_reject_oplog_rs_write`: `local.oplog.rs` (a view over
/// the oplog WT table, written only via oplog emission) and `admin.system.users`
/// (written only via `createUser` / `updateUser` / `dropUser`). A direct write
/// would land in the wrong table or break the view's invariants, so it's rejected
/// with code 13 (`Unauthorized`), the same code mongod returns for an RBAC denial.
fn reject_synthetic_view_write(name: &str, doc: &Document, db: &str) -> Option<CommandError> {
    let coll = match name {
        "insert" | "update" | "delete" => doc.get_str(name).ok()?,
        _ => return None,
    };
    let detail = match (db, coll) {
        ("local", "oplog.rs") => {
            "local.oplog.rs (synthetic read-only view of the SecantusDB oplog)"
        }
        ("admin", "system.users") => {
            "admin.system.users (synthetic read-only view — use createUser / \
             updateUser / dropUser instead)"
        }
        _ => return None,
    };
    Some(CommandError::new(
        13,
        "Unauthorized",
        format!("not authorized for {name} on {detail}"),
    ))
}

/// Reject a namespace component (database / collection / index name) that
/// carries an embedded NUL byte.
///
/// BSON strings are length-prefixed and may legally contain a NUL, so a
/// client can send `{find: "c\0evil"}` or a `$db` with an interior NUL. Such
/// a value would reach secantus-wt's `cstr` key encoder
/// (`CString::new(..).expect(..)`) and **panic** — and because the storage
/// serialises WT ops under a `std::sync::Mutex`, that panic unwinds while the
/// lock is held and poisons it for *every* connection, turning a per-request
/// fault into a whole-server DoS. Reject it here, before it reaches storage,
/// with the same `InvalidNamespace` error mongod returns. (#139)
pub(crate) fn nul_in_namespace(kind: &str, value: &str) -> Option<CommandError> {
    if value.contains('\0') {
        Some(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Invalid {kind}: names cannot contain a NUL ('\\0') byte."),
        ))
    } else {
        None
    }
}

fn dispatch_inner(doc: &Document, ctx: &mut CommandContext) -> Document {
    let name = command_name(doc);

    // Database-name length limit: mongod rejects any namespace whose database
    // component exceeds 63 bytes with InvalidNamespace before the command runs
    // (libmongoc's `long_namespace/unsupported_long_db` inserts into a
    // 64-character database and expects a server error). Mirrors commands.py.
    if ctx.db_name.len() > 63 {
        return CommandError::new(
            73,
            "InvalidNamespace",
            format!(
                "Invalid database name: '{}'. Database names must be at most 63 characters.",
                ctx.db_name
            ),
        )
        .into_reply();
    }

    // Embedded-NUL guard for the database + collection name. The collection is
    // the command's first-value string for every collection-scoped command
    // (`{find: "coll", ...}`, `{insert: "coll", ...}`, ...); a non-string first
    // value (e.g. `{ping: 1}`) simply isn't checked. Index names are guarded
    // separately inside the index handlers. (#139)
    if let Some(e) = nul_in_namespace("database name", &ctx.db_name) {
        return e.into_reply();
    }
    if let Ok(coll) = doc.get_str(name) {
        if let Some(e) = nul_in_namespace("collection name", coll) {
            return e.into_reply();
        }
    }

    if let Err(e) = validate_read_concern(doc, name, ctx) {
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
            // Failpoint (`failCommand`): a matching failpoint can block, then
            // either short-circuit the command with an injected error or (when
            // it only carries a writeConcernError) let it run and attach the
            // block afterwards. `configureFailPoint` itself is exempt.
            let fp = if name != "configureFailPoint" {
                ctx.failpoints.as_ref().and_then(|r| r.match_command(name))
            } else {
                None
            };
            if let Some(m) = &fp {
                if m.block_time_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(m.block_time_ms as u64));
                }
                // closeConnection: flag the socket for the server to drop after
                // dispatch (the reply is discarded). mongod sends nothing back.
                if m.close_connection {
                    ctx.close_connection = true;
                    return doc! { "ok": 1.0 };
                }
                if let Some(code) = m.error_code {
                    let mut reply = CommandError::new(
                        code,
                        failpoints::fail_code_name(code),
                        "Failing command due to 'failCommand' failpoint",
                    )
                    .into_reply();
                    // `failGetMoreAfterCursorCheckout` is injected inside the
                    // change-stream getMore path, where mongod stamps a
                    // resumable code with `ResumableChangeStreamError`; drivers
                    // on wire >= 9 resume on that label and never on the bare
                    // code. `failCommand` short-circuits earlier and carries
                    // only the labels it was given — which is why the spec has
                    // `failGetMoreAfterCursorCheckout` + code 6 resume while
                    // `failCommand` + code 6 does not.
                    let mut labels = m.error_labels.clone();
                    if m.server_injected
                        && failpoints::is_resumable_change_stream_code(code)
                        && !labels.iter().any(|l| l == "ResumableChangeStreamError")
                    {
                        labels.push("ResumableChangeStreamError".to_string());
                    }
                    if !labels.is_empty() {
                        reply.insert(
                            "errorLabels",
                            labels.into_iter().map(Bson::String).collect::<Vec<_>>(),
                        );
                    }
                    return reply;
                }
            }
            // Malformed writeConcern is rejected before a write command runs
            // (mirrors commands.py, which prepends _validate_write_concern to each
            // write handler). Reads don't carry a writeConcern.
            if is_write_concern_command(name) {
                if let Some(e) = validate_write_concern(doc) {
                    return e.into_reply();
                }
            }
            // Refuse a direct insert/update/delete on a synthetic read-only view
            // (local.oplog.rs / admin.system.users), mirroring commands.py.
            if let Some(e) = reject_synthetic_view_write(name, doc, &ctx.db_name) {
                return e.into_reply();
            }
            // Time profile-eligible commands so dispatch can record a
            // `system.profile` entry when the per-database level requires it.
            let start = profile_eligible(name, doc).then(std::time::Instant::now);
            let mut reply = run_with_txn_envelope(name, handler, doc, ctx);
            // A failpoint-configured writeConcernError attaches to a successful reply.
            if let Some(m) = &fp {
                if let Some(wce) = &m.write_concern_error {
                    if reply.get_f64("ok").unwrap_or(0.0) == 1.0
                        && !reply.contains_key("writeConcernError")
                    {
                        reply.insert("writeConcernError", Bson::Document(wce.clone()));
                    }
                }
            }
            if let Some(start) = start {
                maybe_record_profile(name, doc, &reply, start, ctx);
            }
            reply
        }
        None => CommandError::command_not_found(name).into_reply(),
    }
}

/// `commitTransaction` — commit the session's transaction via the registry
/// (idempotent; an envelope-less call is a tolerated no-op). Mirrors
/// `commands.py::_commit_transaction`.
fn commit_transaction(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let (lsid, txn_number) = txn_envelope(doc);
    match (&ctx.transactions, lsid, txn_number) {
        (Some(reg), Some(lsid), Some(n)) => match reg.commit(&lsid, n) {
            None => Ok(doc! { "ok": 1.0 }),
            Some(reply) => Ok(reply),
        },
        _ => Ok(doc! { "ok": 1.0 }),
    }
}

/// `abortTransaction` — roll back the session's transaction via the registry.
/// Mirrors `commands.py::_abort_transaction`.
fn abort_transaction(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let (lsid, txn_number) = txn_envelope(doc);
    match (&ctx.transactions, lsid, txn_number) {
        (Some(reg), Some(lsid), Some(n)) => match reg.abort(&lsid, n) {
            None => Ok(doc! { "ok": 1.0 }),
            Some(reply) => Ok(reply),
        },
        _ => Ok(doc! { "ok": 1.0 }),
    }
}

/// Commands dispatch never profiles (handshake / auth / session / cursor
/// framing + `profile` itself), mirroring `commands.py::_PROFILE_SKIP_COMMANDS`.
fn profile_skip_command(name: &str) -> bool {
    matches!(
        name,
        "hello"
            | "isMaster"
            | "ismaster"
            | "ping"
            | "buildInfo"
            | "buildinfo"
            | "whatsmyuri"
            | "saslStart"
            | "saslContinue"
            | "logout"
            | "connectionStatus"
            | "startSession"
            | "endSessions"
            | "refreshSessions"
            | "getMore"
            | "killCursors"
            | "profile"
    )
}

/// Whether dispatch should time + maybe record this command in `system.profile`.
/// Excludes framing commands and any op against `system.profile` itself (a
/// recursion / unbounded-growth guard). Mirrors `_profile_eligible_command`.
fn profile_eligible(name: &str, doc: &Document) -> bool {
    if profile_skip_command(name) {
        return false;
    }
    !matches!(doc.get(name), Some(Bson::String(s)) if s == "system.profile")
}

/// mongod's profiler `op` bucket for a command (`find` → `query`, writes →
/// `insert`/`update`/`remove`, everything else → `command`).
fn profile_op_label(name: &str) -> &'static str {
    match name {
        "find" => "query",
        "insert" => "insert",
        "update" => "update",
        "delete" => "remove",
        _ => "command",
    }
}

/// The mongod-shaped `system.profile` entry for a finished command, recorded
/// when the per-database profiling level requires it. Ports
/// `commands.py::_maybe_record_profile` (sample-rate skipping omitted — the
/// gauge uses the default rate of 1.0). Failures are swallowed: the command
/// already produced its reply.
fn maybe_record_profile(
    name: &str,
    doc: &Document,
    reply: &Document,
    start: std::time::Instant,
    ctx: &CommandContext,
) {
    let Some(storage) = ctx.storage.clone() else {
        return;
    };
    let db = ctx.db_name.clone();
    let Ok(settings) = storage.get_profile(&db) else {
        return;
    };
    let level = settings.get_i32("level").unwrap_or(0);
    if level == 0 {
        return;
    }
    let duration_ms = start.elapsed().as_millis() as i64;
    let slowms = settings.get_i32("slowms").unwrap_or(100) as i64;
    if level == 1 && duration_ms < slowms {
        return;
    }
    // `ns`: db.coll when the command names a collection, else db.
    let ns = match doc.get(name) {
        Some(Bson::String(c)) if !c.is_empty() => format!("{db}.{c}"),
        _ => db.clone(),
    };
    // `command`: the request minus framing fields.
    let mut command = doc.clone();
    for k in ["$db", "$clusterTime", "lsid", "$readPreference"] {
        command.remove(k);
    }
    let ok = reply.get_f64("ok").unwrap_or(0.0) == 1.0;
    let mut entry = doc! {
        "ts": bson::DateTime::now(),
        "op": profile_op_label(name),
        "ns": ns,
        "command": command,
        "millis": duration_ms as i32,
        "ok": if ok { 1.0 } else { 0.0 },
        "client": "",
    };
    if !ok {
        if let Ok(em) = reply.get_str("errmsg") {
            entry.insert("errMsg", em.to_string());
        }
    }
    let mut bytes = Vec::new();
    if entry.to_writer(&mut bytes).is_ok() {
        let _ = storage.insert(&db, "system.profile", vec![bytes], true);
    }
}

/// Run `handler`, mapping its `Err` into the standard error reply.
fn run_handler(handler: Handler, doc: &Document, ctx: &mut CommandContext) -> Document {
    match handler(doc, ctx) {
        Ok(reply) => reply,
        Err(e) => e.into_reply(),
    }
}

/// Commands permitted as statements inside a multi-document transaction
/// (`commands.py::_TXN_ALLOWED_COMMANDS`). `commitTransaction` / `abortTransaction`
/// are the controls, handled by their own handlers, not the statement path.
fn txn_allowed_command(name: &str) -> bool {
    matches!(
        name,
        "insert"
            | "update"
            | "delete"
            | "findAndModify"
            | "findandmodify"
            | "find"
            | "getMore"
            | "killCursors"
            | "aggregate"
            | "distinct"
            | "bulkWrite"
            | "create"
            | "createIndexes"
    )
}

/// Aggregation stages mongod refuses inside a transaction.
fn txn_blocked_agg_stage(stage: &str) -> bool {
    matches!(
        stage,
        "$out"
            | "$merge"
            | "$changeStream"
            | "$collStats"
            | "$currentOp"
            | "$indexStats"
            | "$listLocalSessions"
            | "$listSessions"
    )
}

/// Error codes that earn the `TransientTransactionError` label when a statement
/// inside a transaction fails (`commands.py::_TRANSIENT_TXN_CODES`). Notably NOT
/// 11000 duplicate key — it aborts the transaction but retrying wouldn't help.
fn is_transient_txn_code(code: i32) -> bool {
    matches!(
        code,
        112 | 246 | 251 | 24 | 6 | 7 | 89 | 91 | 189 | 9001 | 10107 | 11600 | 11602 | 13435 | 13436
    )
}

/// The mongod-shaped reason a statement can't run in a transaction, or `None`.
fn txn_unsupported_reason(name: &str, doc: &Document) -> Option<String> {
    if !txn_allowed_command(name) {
        return Some(format!(
            "Cannot run '{name}' in a multi-document transaction."
        ));
    }
    if name == "aggregate" {
        if let Some(Bson::Array(pipeline)) = doc.get("pipeline") {
            for stage in pipeline {
                if let Some(s) = stage.as_document().and_then(|d| d.keys().next()) {
                    if txn_blocked_agg_stage(s) {
                        return Some(format!(
                            "Operation not permitted in transaction :: caused by :: Aggregation \
                             stage {s} cannot run within a multi-document transaction."
                        ));
                    }
                }
            }
        }
    }
    None
}

/// The 16-byte UUID payload from a `lsid` argument (`{id: BinData(4, <uuid>)}`),
/// or `None` for an unrecognised shape.
fn lsid_bytes_from_arg(entry: Option<&Bson>) -> Option<Vec<u8>> {
    let d = entry?.as_document()?;
    match d.get("id") {
        Some(Bson::Binary(b)) => Some(b.bytes.clone()),
        _ => None,
    }
}

/// `(lsid_bytes, txnNumber)` from a command's transaction envelope; either is
/// `None` when absent or malformed (a boolean `txnNumber` is rejected).
fn txn_envelope(doc: &Document) -> (Option<Vec<u8>>, Option<i64>) {
    let lsid = lsid_bytes_from_arg(doc.get("lsid"));
    let txn_number = match doc.get("txnNumber") {
        Some(Bson::Int32(n)) => Some(*n as i64),
        Some(Bson::Int64(n)) => Some(*n),
        _ => None,
    };
    (lsid, txn_number)
}

/// The multi-document-transaction envelope around a statement (port of the
/// `commands.py::dispatch` transaction block). When the command carries an
/// `autocommit: false` + `lsid` + `txnNumber` envelope it resolves / runs inside
/// the transaction; a bare `txnNumber` (retryable write) just advances the
/// session sequence; everything else runs the handler directly.
/// Commands mongod records a retryable-write result for. Drivers only attach a
/// `txnNumber` to genuinely retryable operations (single-document writes) —
/// `updateMany` / `deleteMany` are excluded by the spec and arrive without one
/// — so the envelope alone is nearly sufficient. Naming the commands anyway
/// keeps a stray `txnNumber` on a read from being cached and replayed.
const RETRYABLE_WRITE_COMMANDS: &[&str] = &["insert", "update", "delete", "findAndModify"];

/// Envelope fields a driver legitimately varies between the original attempt
/// and its retry (gossip, routing, the session envelope itself). Excluded from
/// the identity digest so a genuine retry still matches.
const RETRY_IDENTITY_IGNORED: &[&str] = &[
    "lsid",
    "txnNumber",
    "$clusterTime",
    "$db",
    "$readPreference",
    "readConcern",
    "writeConcern",
    "apiVersion",
    "apiStrict",
    "apiDeprecationErrors",
    "comment",
];

/// A stable digest of the write this command represents.
///
/// Two attempts of the same retryable write are byte-identical apart from the
/// envelope fields above, so this matches on a genuine retry and differs when
/// the `(lsid, txnNumber)` key has been reused for another write. Mirrors the
/// Python server's `_retry_identity`.
fn retry_identity(doc: &Document) -> [u8; 20] {
    use sha2::{Digest, Sha256};

    let mut body = Document::new();
    for (k, v) in doc.iter() {
        if !RETRY_IDENTITY_IGNORED.contains(&k.as_str()) {
            body.insert(k.clone(), v.clone());
        }
    }
    let mut buf: Vec<u8> = Vec::new();
    let payload = match body.to_writer(&mut buf) {
        Ok(()) => buf.as_slice(),
        // Unencodable body: fall back to a debug rendering. It only has to be
        // consistent for a given command, not canonical.
        Err(_) => {
            buf = format!("{body:?}").into_bytes();
            buf.as_slice()
        }
    };
    let full = Sha256::digest(payload);
    let mut out = [0u8; 20];
    out.copy_from_slice(&full[..20]);
    out
}

fn run_with_txn_envelope(
    name: &str,
    handler: Handler,
    doc: &Document,
    ctx: &mut CommandContext,
) -> Document {
    let registry = match (&ctx.transactions, doc.contains_key("txnNumber")) {
        (Some(r), true) => r.clone(),
        _ => return run_handler(handler, doc, ctx),
    };
    let (Some(lsid), Some(txn_number)) = txn_envelope(doc) else {
        return run_handler(handler, doc, ctx);
    };
    let autocommit_false = matches!(doc.get("autocommit"), Some(Bson::Boolean(false)));
    if !autocommit_false {
        // Retryable write: consumes the session's txnNumber sequence (and aborts
        // an older in-progress transaction), then runs normally.
        registry.on_retryable_write(&lsid, txn_number);
        if !RETRYABLE_WRITE_COMMANDS.contains(&name) {
            return run_handler(handler, doc, ctx);
        }
        // If this exact (lsid, txnNumber) already ran THIS command, replay the
        // stored reply instead of executing the write again. Without it a
        // retried `{$inc: {n: 1}}` applies twice while both replies claim
        // `nModified: 1` — silent corruption on a path every driver exercises
        // automatically after a network blip.
        let identity = retry_identity(doc);
        if let Some(cached) = registry.retryable_reply(&lsid, txn_number, &identity) {
            return cached;
        }
        let result = run_handler(handler, doc, ctx);
        registry.record_retryable(&lsid, txn_number, identity, &result);
        return result;
    }
    // commit/abort carry the same envelope but are controls (own handlers).
    if name == "commitTransaction" || name == "abortTransaction" {
        return run_handler(handler, doc, ctx);
    }
    // A transaction's concerns are fixed when it starts: `readConcern` may ride
    // only the FIRST statement, and `writeConcern` belongs on commit/abort,
    // never on a statement. mongod rejects both with InvalidOptions (72); we
    // accepted and silently ignored them, so a caller could believe a statement
    // ran at a concern it did not. Drivers guard this client-side (the
    // transactions spec marks these `isClientError: true`), so no gauge covers
    // it — it matters for raw-command callers. Messages are mongod's verbatim,
    // from the spec corpus the drivers vendor.
    if doc.get("writeConcern").is_some() {
        return CommandError::new(
            72,
            "InvalidOptions",
            "Cannot set write concern after starting a transaction",
        )
        .into_reply();
    }
    let starting = matches!(
        doc.get("startTransaction"),
        Some(Bson::Boolean(true)) | Some(Bson::Int32(1)) | Some(Bson::Int64(1))
    );
    if !starting && doc.get("readConcern").is_some() {
        return CommandError::new(
            72,
            "InvalidOptions",
            "Cannot set read concern after starting a transaction",
        )
        .into_reply();
    }
    let start = matches!(
        doc.get("startTransaction"),
        Some(Bson::Boolean(true)) | Some(Bson::Int32(1)) | Some(Bson::Int64(1))
    );
    let txn = match registry.for_statement(&lsid, txn_number, start) {
        Ok(t) => t,
        Err(reply) => return reply,
    };
    // A disallowed statement aborts the transaction, then errors (263).
    if let Some(reason) = txn_unsupported_reason(name, doc) {
        registry.abort_in_progress(&txn);
        return CommandError::new(263, "OperationNotSupportedInTransaction", reason).into_reply();
    }
    let Some(storage) = ctx.storage.clone() else {
        return run_handler(handler, doc, ctx);
    };
    // Create the WT transaction handle lazily at the first statement (the
    // snapshot pins here).
    {
        let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
        if t.handle.is_none() {
            match storage.begin_user_transaction() {
                Ok(h) => t.handle = Some(h),
                Err(e) => {
                    return CommandError::new(1, "InternalError", format!("{e:?}")).into_reply()
                }
            }
        }
    }
    let mut result = {
        let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
        let handle = t.handle.as_mut().expect("handle created above").as_mut();
        match storage.run_in_user_transaction(handle, &mut || run_handler(handler, doc, ctx)) {
            Ok(r) => r,
            Err(e) => CommandError::new(1, "InternalError", format!("{e:?}")).into_reply(),
        }
    };
    finish_txn_statement(&registry, &txn, &mut result);
    result
}

/// mongod parity: any failed statement aborts the transaction server-side. Only
/// transient-class codes earn the `TransientTransactionError` label (E11000
/// aborts unlabeled). Mirrors `commands.py::_finish_txn_statement`.
fn finish_txn_statement(
    registry: &transactions::TransactionRegistry,
    txn: &std::sync::Arc<std::sync::Mutex<transactions::Transaction>>,
    result: &mut Document,
) {
    let ok = result.get_f64("ok").unwrap_or(0.0) == 1.0;
    let failed = !ok || result.contains_key("writeErrors");
    if !failed {
        return;
    }
    registry.abort_in_progress(txn);
    if !ok {
        let code = result.get_i32("code").unwrap_or(0);
        if is_transient_txn_code(code) {
            let labels = match result.get_array_mut("errorLabels") {
                Ok(arr) => arr,
                Err(_) => {
                    result.insert("errorLabels", Bson::Array(Vec::new()));
                    result.get_array_mut("errorLabels").unwrap()
                }
            };
            if !labels
                .iter()
                .any(|l| l.as_str() == Some(transactions::TRANSIENT_LABEL))
            {
                labels.push(Bson::String(transactions::TRANSIENT_LABEL.to_string()));
            }
        }
    }
}

/// Cluster-time gossip: real replica-set mongod attaches `$clusterTime` and
/// `operationTime` to EVERY reply — successes and errors — when the node is a
/// replica-set member; standalones don't gossip (neither do we when the
/// replica-set persona is off). Drivers and pymongo read `operationTime` for
/// causal consistency and `startAtOperationTime`. `contains_key` guards
/// preserve a handler that already attached a more specific value (e.g. the
/// change-stream `aggregate` reply). The keyless signature (20 zero bytes,
/// keyId 0) is what auth-less replica sets send. Mirrors `commands.py::dispatch`.
fn attach_cluster_time_gossip(req: &Document, reply: &mut Document, ctx: &CommandContext) {
    if ctx.replica_set_name.is_none() {
        return;
    }
    let ts = ctx
        .storage
        .as_ref()
        .map(|s| s.peek_cluster_time())
        .unwrap_or(bson::Timestamp {
            time: 0,
            increment: 0,
        });
    if !reply.contains_key("$clusterTime") {
        reply.insert(
            "$clusterTime",
            doc! {
                "clusterTime": Bson::Timestamp(ts),
                "signature": doc! {
                    "hash": Bson::Binary(bson::Binary {
                        subtype: bson::spec::BinarySubtype::Generic,
                        bytes: vec![0u8; 20],
                    }),
                    "keyId": 0i64,
                },
            },
        );
    }
    if !reply.contains_key("operationTime") {
        reply.insert("operationTime", Bson::Timestamp(ts));
    }
    // Snapshot sessions: pymongo pins the session's read timestamp from the
    // FIRST snapshot read's reply — `cursor.atClusterTime` for cursor commands,
    // top-level `atClusterTime` otherwise — and echoes it as
    // `readConcern.atClusterTime` on subsequent reads. Reads aren't actually
    // pinned (single node, accept-and-record), but the wire contract is met.
    // Mirrors `commands.py::dispatch`.
    let is_snapshot = req
        .get_document("readConcern")
        .ok()
        .and_then(|rc| rc.get_str("level").ok())
        == Some("snapshot");
    let ok = matches!(reply.get("ok"), Some(Bson::Double(d)) if *d != 0.0)
        || matches!(reply.get("ok"), Some(Bson::Int32(n)) if *n != 0)
        || matches!(reply.get("ok"), Some(Bson::Int64(n)) if *n != 0);
    if is_snapshot && ok {
        match reply.get_mut("cursor") {
            Some(Bson::Document(cur)) => {
                if !cur.contains_key("atClusterTime") {
                    cur.insert("atClusterTime", Bson::Timestamp(ts));
                }
            }
            _ => {
                if !reply.contains_key("atClusterTime") {
                    reply.insert("atClusterTime", Bson::Timestamp(ts));
                }
            }
        }
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
                .unwrap_or_else(|e| e.into_inner())
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
                        .unwrap_or_else(|e| e.into_inner())
                        .effective_roles
                        .clone()
                })
                .unwrap_or_default();
            // Custom roles are expanded through a storage-backed resolver
            // (`Storage::get_role`); built-in roles short-circuit without it.
            let storage = ctx.storage.as_deref();
            let resolver = |db: &str, role: &str| -> Option<Document> {
                let bytes = storage?.get_role(db, role).ok()??;
                Document::from_reader(&mut bytes.as_slice()).ok()
            };
            if !rbac::check_privilege_resolved(
                &roles,
                action,
                target_db.as_deref(),
                cluster,
                Some(&resolver),
            ) {
                return Err(CommandError::new(
                    13,
                    "Unauthorized",
                    format!(
                        "not authorized on {} to execute command (action: {action})",
                        target_db.as_deref().unwrap_or("cluster")
                    ),
                ));
            }
            // A pipeline's secondary namespaces carry their own privilege
            // requirements ($out/$merge write, $lookup-family read) — the
            // primary (find, primary-collection) grant alone must not
            // authorize writes to or reads from other namespaces. Mirrors
            // commands.py's _pipeline_secondary_requirements check.
            if name == "aggregate" {
                for (extra_action, extra_db) in pipeline_secondary_requirements(doc, &ctx.db_name) {
                    if !rbac::check_privilege_resolved(
                        &roles,
                        extra_action,
                        Some(&extra_db),
                        false,
                        Some(&resolver),
                    ) {
                        return Err(CommandError::new(
                            13,
                            "Unauthorized",
                            format!(
                                "not authorized on {extra_db} to execute \
                                 aggregation stage (action: {extra_action})"
                            ),
                        ));
                    }
                }
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
            | "authenticate"
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
            | "authenticate"
            | "connectionStatus"
            | "abortTransaction"
            | "commitTransaction"
            | "replSetGetStatus"
    )
}

/// The `(action, scope)` a command needs, or `None` for "no privilege required"
/// (a permissive default — an authenticated user with any role can invoke it).
/// Mirrors `commands.py::_COMMAND_ACTIONS`.
fn command_action(name: &str) -> Option<(&'static str, &'static str)> {
    use rbac::*;
    Some(match name {
        "find" | "count" | "distinct" | "aggregate" | "mapReduce" | "mapreduce" => {
            (A_FIND, SCOPE_COLLECTION)
        }
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
        "listDatabases" => (A_LIST_DATABASES, SCOPE_CLUSTER),
        "dbStats" | "dbstats" => (A_DB_STATS, SCOPE_DATABASE),
        "collStats" => (A_COLL_STATS, SCOPE_COLLECTION),
        "createUser" => (A_CREATE_USER, SCOPE_DATABASE),
        "updateUser" => (A_CHANGE_PASSWORD, SCOPE_DATABASE),
        "dropUser" => (A_DROP_USER, SCOPE_DATABASE),
        "grantRolesToUser" => (A_GRANT_ROLE, SCOPE_DATABASE),
        "revokeRolesFromUser" => (A_REVOKE_ROLE, SCOPE_DATABASE),
        "dropAllUsersFromDatabase" => (A_DROP_USER, SCOPE_DATABASE),
        "usersInfo" => (A_VIEW_USER, SCOPE_DATABASE),
        "createRole" => (A_CREATE_ROLE, SCOPE_DATABASE),
        "updateRole" => (A_GRANT_ROLE, SCOPE_DATABASE),
        "dropRole" => (A_DROP_ROLE, SCOPE_DATABASE),
        "dropAllRolesFromDatabase" => (A_DROP_ROLE, SCOPE_DATABASE),
        "grantPrivilegesToRole" => (A_GRANT_ROLE, SCOPE_DATABASE),
        "revokePrivilegesFromRole" => (A_REVOKE_ROLE, SCOPE_DATABASE),
        "grantRolesToRole" => (A_GRANT_ROLE, SCOPE_DATABASE),
        "revokeRolesFromRole" => (A_REVOKE_ROLE, SCOPE_DATABASE),
        "rolesInfo" => (A_VIEW_ROLE, SCOPE_DATABASE),
        "serverStatus" => (A_SERVER_STATUS, SCOPE_CLUSTER),
        "currentOp" => (A_INPROG, SCOPE_CLUSTER),
        "killOp" => (A_KILLOP, SCOPE_CLUSTER),
        "hostInfo" => (A_HOST_INFO, SCOPE_CLUSTER),
        "top" => (A_TOP, SCOPE_CLUSTER),
        "getCmdLineOpts" => (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
        "getParameter" => (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
        "getLog" => (A_GET_LOG, SCOPE_CLUSTER),
        // Fault injection is a server-wide DoS lever (e.g. closeConnection on
        // every find); require an explicit cluster-admin grant under --auth.
        "configureFailPoint" => (A_CONFIGURE_FAIL_POINT, SCOPE_CLUSTER),
        _ => return None,
    })
}

/// The `(action, db)` grants a pipeline needs beyond the primary `find`:
/// `$out` insert+remove on its target, `$merge` insert+update, and the
/// read-side stages (`$lookup` / `$graphLookup` / `$unionWith`) find on the
/// foreign namespace. Sub-pipelines (`$lookup.pipeline`,
/// `$unionWith.pipeline`, `$facet` branches) are walked recursively. RBAC is
/// db-granular, so requirements resolve to `(action, db)` pairs. Mirrors
/// `commands.py::_pipeline_secondary_requirements`.
fn pipeline_secondary_requirements(
    doc: &Document,
    default_db: &str,
) -> Vec<(&'static str, String)> {
    let mut reqs: Vec<(&'static str, String)> = Vec::new();

    fn walk(pipeline: &Bson, default_db: &str, reqs: &mut Vec<(&'static str, String)>) {
        use rbac::{A_FIND, A_INSERT, A_REMOVE, A_UPDATE};
        let Bson::Array(stages) = pipeline else {
            return;
        };
        for stage in stages {
            let Bson::Document(stage) = stage else {
                continue;
            };
            for (op, spec) in stage.iter() {
                match op.as_str() {
                    "$out" => {
                        let db = match spec {
                            Bson::Document(d) => d.get_str("db").unwrap_or(default_db).to_string(),
                            _ => default_db.to_string(),
                        };
                        reqs.push((A_INSERT, db.clone()));
                        reqs.push((A_REMOVE, db));
                    }
                    "$merge" => {
                        let into = match spec {
                            Bson::Document(d) => d.get("into"),
                            _ => None,
                        };
                        let db = match into {
                            Some(Bson::Document(d)) => {
                                d.get_str("db").unwrap_or(default_db).to_string()
                            }
                            _ => default_db.to_string(),
                        };
                        reqs.push((A_INSERT, db.clone()));
                        reqs.push((A_UPDATE, db));
                    }
                    "$lookup" | "$graphLookup" | "$unionWith" => {
                        reqs.push((A_FIND, default_db.to_string()));
                        if let Bson::Document(d) = spec {
                            if let Some(sub) = d.get("pipeline") {
                                walk(sub, default_db, reqs);
                            }
                        }
                    }
                    "$facet" => {
                        if let Bson::Document(d) = spec {
                            for (_, sub) in d.iter() {
                                walk(sub, default_db, reqs);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(pipeline) = doc.get("pipeline") {
        walk(pipeline, default_db, &mut reqs);
    }
    reqs.sort();
    reqs.dedup();
    reqs
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
fn validate_read_concern(
    doc: &Document,
    name: &str,
    ctx: &CommandContext,
) -> Result<(), CommandError> {
    let Some(Bson::Document(rc)) = doc.get("readConcern") else {
        return Ok(());
    };
    let Some(level) = rc.get("level") else {
        return Ok(());
    };
    match level.as_str() {
        Some(l) if VALID_READ_CONCERN_LEVELS.contains(&l) => {
            if l == "snapshot" {
                // Inside a multi-document transaction (`autocommit: false`) every
                // read runs against the txn's pinned WT snapshot — exactly what
                // `snapshot` asks for on a single node — so it's accepted. Outside
                // a txn, a replica-set member accepts `snapshot` on the snapshot-
                // session reads (find / aggregate / distinct / getMore /
                // killCursors); everything else is rejected like a standalone.
                let in_txn = matches!(doc.get("autocommit"), Some(Bson::Boolean(false)));
                let snapshot_readable = matches!(
                    name,
                    "find" | "aggregate" | "distinct" | "getMore" | "killCursors"
                ) && ctx.replica_set_name.is_some();
                if !in_txn && !snapshot_readable {
                    return Err(CommandError::new(
                        246,
                        "SnapshotUnavailable",
                        "Snapshot read concern is not supported on standalone",
                    ));
                }
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
    if doc.get("apiStrict").and_then(Bson::as_bool) == Some(true) {
        // `distinct` is the canary command rejected under apiStrict (mirrors
        // commands.py's narrow `_API_V1_REJECTED_BY_NAME`).
        if name == "distinct" {
            return Err(CommandError::new(
                323,
                "APIStrictError",
                format!("Provided command {name} is not in API Version 1"),
            ));
        }
        // An aggregate whose pipeline uses a stage outside API Version 1 (e.g.
        // `$listLocalSessions`) is rejected — drivers probe with exactly that to
        // land an APIStrictError from inside an allowed command.
        if name == "aggregate" {
            if let Some(Bson::Array(pipeline)) = doc.get("pipeline") {
                for stage in pipeline {
                    if let Some(s) = stage.as_document().and_then(|d| d.keys().next()) {
                        if !api_v1_agg_stage(s) {
                            return Err(CommandError::new(
                                323,
                                "APIStrictError",
                                format!(
                                    "Provided aggregation pipeline stage {s} is not in API Version 1"
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Aggregation stages inside API Version 1 (`commands.py::_API_V1_AGG_STAGES`).
/// A pipeline stage outside this set under `apiStrict: true` is an
/// `APIStrictError`. Deliberately excludes `$listLocalSessions` / `$listSessions`
/// / `$currentOp` — the stages driver tests probe with.
fn api_v1_agg_stage(stage: &str) -> bool {
    matches!(
        stage,
        "$addFields"
            | "$bucket"
            | "$bucketAuto"
            | "$changeStream"
            | "$collStats"
            | "$count"
            | "$densify"
            | "$documents"
            | "$facet"
            | "$fill"
            | "$geoNear"
            | "$graphLookup"
            | "$group"
            | "$indexStats"
            | "$limit"
            | "$lookup"
            | "$match"
            | "$merge"
            | "$out"
            | "$project"
            | "$redact"
            | "$replaceRoot"
            | "$replaceWith"
            | "$sample"
            | "$set"
            | "$setWindowFields"
            | "$skip"
            | "$sort"
            | "$sortByCount"
            | "$unionWith"
            | "$unset"
            | "$unwind"
    )
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
    fn pipeline_secondary_requirements_walks_all_stage_shapes() {
        let doc = doc! {"aggregate": "orders", "pipeline": [
            {"$match": {"v": 1}},
            {"$out": {"db": "warehouse", "coll": "t"}},
            {"$merge": {"into": "t2"}},
            {"$unionWith": {"coll": "c2", "pipeline": [
                {"$merge": {"into": {"db": "x", "coll": "y"}}},
            ]}},
            {"$facet": {"branch": [
                {"$lookup": {"from": "f", "localField": "a", "foreignField": "b", "as": "z"}},
            ]}},
        ]};
        let reqs = pipeline_secondary_requirements(&doc, "shop");
        for want in [
            (rbac::A_INSERT, "warehouse"),
            (rbac::A_REMOVE, "warehouse"),
            (rbac::A_INSERT, "shop"),
            (rbac::A_UPDATE, "shop"),
            (rbac::A_INSERT, "x"),
            (rbac::A_UPDATE, "x"),
            (rbac::A_FIND, "shop"),
        ] {
            assert!(
                reqs.contains(&(want.0, want.1.to_string())),
                "missing {want:?} in {reqs:?}"
            );
        }
        // A plain read pipeline demands nothing extra.
        assert!(pipeline_secondary_requirements(
            &doc! {"aggregate": "orders", "pipeline": [{"$match": {}}]},
            "shop"
        )
        .is_empty());
    }

    #[test]
    fn nul_in_namespace_rejects_interior_nul() {
        assert!(nul_in_namespace("collection name", "fine").is_none());
        let e = nul_in_namespace("collection name", "c\0x").expect("should reject");
        assert_eq!(
            e.into_reply().get_str("codeName").unwrap(),
            "InvalidNamespace"
        );
    }

    #[test]
    fn dispatch_rejects_nul_collection_name() {
        // A well-formed BSON command whose collection name carries an interior
        // NUL must return InvalidNamespace, not panic — a panic here would
        // unwind while the storage lock is held and poison it (#139).
        let reply = dispatch(&doc! {"find": "c\0evil"}, &mut ctx());
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 73);
        assert_eq!(reply.get_str("codeName").unwrap(), "InvalidNamespace");
    }

    #[test]
    fn dispatch_rejects_nul_database_name() {
        let mut c = ctx();
        c.db_name = "te\0st".to_string();
        let reply = dispatch(&doc! {"find": "coll"}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 73);
        assert_eq!(reply.get_str("codeName").unwrap(), "InvalidNamespace");
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
    fn write_concern_error_for_unsatisfiable_w() {
        // w > 1 can't be satisfied by the single-node "secantus" RS → 100.
        let reply = dispatch(&doc! {"ping": 1, "writeConcern": {"w": 2}}, &mut ctx());
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let wce = reply.get_document("writeConcernError").unwrap();
        assert_eq!(wce.get_i32("code").unwrap(), 100);
        assert_eq!(
            wce.get_str("codeName").unwrap(),
            "CannotSatisfyWriteConcern"
        );
    }

    #[test]
    fn no_write_concern_error_for_satisfiable_w() {
        for wc in [doc! {"w": 1}, doc! {"w": 0}, doc! {"w": "majority"}] {
            let reply = dispatch(&doc! {"ping": 1, "writeConcern": wc}, &mut ctx());
            assert!(reply.get("writeConcernError").is_none());
        }
        // A read without a writeConcern never gets one.
        let reply = dispatch(&doc! {"ping": 1}, &mut ctx());
        assert!(reply.get("writeConcernError").is_none());
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
    fn hello_advertises_sasl_supported_mechs() {
        // Without the field, hello doesn't volunteer mechanisms.
        let plain = dispatch(&doc! {"hello": 1}, &mut ctx());
        assert!(plain.get("saslSupportedMechs").is_none());
        // With saslSupportedMechs: "<db>.<user>", advertise SCRAM-SHA-256.
        let reply = dispatch(
            &doc! {"hello": 1, "saslSupportedMechs": "admin.alice"},
            &mut ctx(),
        );
        let mechs = reply.get_array("saslSupportedMechs").unwrap();
        assert_eq!(mechs, &vec![Bson::String("SCRAM-SHA-256".into())]);
    }

    #[test]
    fn replica_set_replies_gossip_cluster_time() {
        // Replica-set persona on: every reply (success or error) carries
        // $clusterTime + operationTime, exactly like a real replica-set mongod.
        let mut c = ctx();
        c.replica_set_name = Some("secantus".into());
        for req in [doc! {"ping": 1}, doc! {"nonsuchcommand": 1}] {
            let reply = dispatch(&req, &mut c);
            let ct = reply.get_document("$clusterTime").unwrap();
            assert!(matches!(ct.get("clusterTime"), Some(Bson::Timestamp(_))));
            let sig = ct.get_document("signature").unwrap();
            assert_eq!(sig.get_i64("keyId").unwrap(), 0);
            assert!(matches!(
                reply.get("operationTime"),
                Some(Bson::Timestamp(_))
            ));
        }
    }

    #[test]
    fn long_database_name_is_invalid_namespace() {
        // A database component over 63 bytes is rejected with InvalidNamespace
        // (73) before the command runs (libmongoc long_namespace test).
        let mut c = ctx();
        c.db_name = "d".repeat(64);
        let reply = dispatch(&doc! {"ping": 1}, &mut c);
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 73);
        assert_eq!(reply.get_str("codeName").unwrap(), "InvalidNamespace");
        // Exactly 63 is allowed.
        let mut c = ctx();
        c.db_name = "d".repeat(63);
        assert_eq!(
            dispatch(&doc! {"ping": 1}, &mut c).get_f64("ok").unwrap(),
            1.0
        );
    }

    #[test]
    fn snapshot_read_concern_pins_at_cluster_time() {
        // A successful snapshot-level read echoes `atClusterTime` so pymongo can
        // pin the session timestamp: nested under `cursor` for cursor replies,
        // top-level otherwise. Tests the gossip helper directly (the read-concern
        // validation that gates which commands may use snapshot is separate, and
        // the end-to-end find/aggregate/distinct paths are covered by the gauge).
        let mut c = ctx();
        c.replica_set_name = Some("secantus".into());
        // Non-cursor reply (e.g. distinct) -> top-level atClusterTime.
        let req = doc! {"distinct": "t", "readConcern": {"level": "snapshot"}};
        let mut reply = doc! {"ok": 1.0, "values": []};
        attach_cluster_time_gossip(&req, &mut reply, &c);
        assert!(matches!(
            reply.get("atClusterTime"),
            Some(Bson::Timestamp(_))
        ));
        // Cursor reply (e.g. find) -> nested under cursor.atClusterTime.
        let req2 = doc! {"find": "t", "readConcern": {"level": "snapshot"}};
        let mut reply2 = doc! {"ok": 1.0, "cursor": {"id": 0i64, "ns": "d.t", "firstBatch": []}};
        attach_cluster_time_gossip(&req2, &mut reply2, &c);
        let cur = reply2.get_document("cursor").unwrap();
        assert!(matches!(cur.get("atClusterTime"), Some(Bson::Timestamp(_))));
        // No snapshot readConcern -> no atClusterTime.
        let mut reply3 = doc! {"ok": 1.0};
        attach_cluster_time_gossip(&doc! {"find": "t"}, &mut reply3, &c);
        assert!(reply3.get("atClusterTime").is_none());
        // A failed reply (ok:0) doesn't pin, even with snapshot readConcern.
        let mut reply4 = doc! {"ok": 0.0};
        attach_cluster_time_gossip(&req, &mut reply4, &c);
        assert!(reply4.get("atClusterTime").is_none());
        // Standalone (no persona) never gossips.
        let mut reply5 = doc! {"ok": 1.0};
        attach_cluster_time_gossip(&req, &mut reply5, &ctx());
        assert!(reply5.get("atClusterTime").is_none());
    }

    #[test]
    fn standalone_replies_do_not_gossip_cluster_time() {
        // No replica-set persona: no gossip (matches standalone mongod).
        let reply = dispatch(&doc! {"ping": 1}, &mut ctx());
        assert!(reply.get("$clusterTime").is_none());
        assert!(reply.get("operationTime").is_none());
    }

    #[test]
    fn api_strict_rejects_non_v1_aggregation_stage() {
        // `$listLocalSessions` is outside API Version 1 → APIStrictError (323),
        // surfaced by validate_api before the handler ever needs storage.
        let reply = dispatch(
            &doc! {
                "aggregate": 1,
                "pipeline": [{"$listLocalSessions": {}}, {"$limit": 1}],
                "apiVersion": "1",
                "apiStrict": true,
            },
            &mut ctx(),
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 323);
        // `distinct` is the rejected canary command.
        let d = dispatch(
            &doc! {"distinct": "c", "key": "x", "apiVersion": "1", "apiStrict": true},
            &mut ctx(),
        );
        assert_eq!(d.get_i32("code").unwrap(), 323);
        // A v1 stage is not rejected by the api gate (it may fail later for lack
        // of storage, but not with 323).
        let ok = dispatch(
            &doc! {"aggregate": 1, "pipeline": [{"$match": {}}],
            "apiVersion": "1", "apiStrict": true},
            &mut ctx(),
        );
        assert_ne!(ok.get_i32("code").ok(), Some(323));
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
    fn hello_optime_is_minted_from_storage_when_present() {
        // With a storage backend wired, `hello`'s lastWrite.opTime.ts comes from
        // `current_cluster_time()` (minting), not the static `ctx.cluster_time`.
        // This is what `startAtOperationTime` resumes depend on — the advertised
        // opTime must be strictly past the last write.
        use crate::storage::{RawHint, Storage, StorageError, UpdateOutcome};
        use std::sync::Arc;
        struct ClockStorage;
        impl Storage for ClockStorage {
            fn current_cluster_time(&self) -> bson::Timestamp {
                bson::Timestamp {
                    time: 555,
                    increment: 9,
                }
            }
            fn insert(
                &self,
                _db: &str,
                _coll: &str,
                _docs: Vec<Vec<u8>>,
                _ordered: bool,
            ) -> Result<(usize, Vec<Document>), StorageError> {
                Ok((0, Vec::new()))
            }
            fn update_matching(
                &self,
                _db: &str,
                _coll: &str,
                _filter: &Document,
                _update: &Document,
                _multi: bool,
                _upsert: bool,
            ) -> Result<UpdateOutcome, StorageError> {
                Ok(UpdateOutcome::default())
            }
            fn delete_matching(
                &self,
                _db: &str,
                _coll: &str,
                _filter: &Document,
                _limit: usize,
            ) -> Result<usize, StorageError> {
                Ok(0)
            }
            fn count_matching(
                &self,
                _db: &str,
                _coll: &str,
                _filter: &Document,
            ) -> Result<usize, StorageError> {
                Ok(0)
            }
            fn find(
                &self,
                _db: &str,
                _coll: &str,
                _filter: &Document,
                _sort: Option<&Document>,
                _hint: Option<RawHint<'_>>,
            ) -> Result<Vec<Vec<u8>>, StorageError> {
                Ok(Vec::new())
            }
        }
        let mut c = ctx();
        c.server_address = Some(("127.0.0.1".into(), 27017));
        c.replica_set_name = Some("secantus".into());
        c.cluster_time = bson::Timestamp {
            time: 100,
            increment: 1,
        };
        c.storage = Some(Arc::new(ClockStorage));
        let reply = dispatch(&doc! {"hello": 1}, &mut c);
        let ts = reply
            .get_document("lastWrite")
            .unwrap()
            .get_document("opTime")
            .unwrap()
            .get("ts");
        assert_eq!(
            ts,
            Some(&Bson::Timestamp(bson::Timestamp {
                time: 555,
                increment: 9,
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
