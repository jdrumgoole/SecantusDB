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
pub mod find;
pub mod findandmodify;
pub mod handshake;
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
        "collMod" => admin::coll_mod,
        "collmod" => admin::coll_mod,
        "explain" => admin::explain,
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
        "hostInfo" => diagnostics::host_info,
        "getLog" => diagnostics::get_log,
        _ => return None,
    })
}

/// Dispatch one command to its handler, applying the cross-cutting validation
/// `commands.py::dispatch` runs first. Always returns a reply document (errors
/// are shaped into `ok: 0`), so the connection survives any single command.
pub fn dispatch(doc: &Document, ctx: &mut CommandContext) -> Document {
    let mut reply = dispatch_inner(doc, ctx);
    attach_write_concern_error(doc, &mut reply);
    attach_cluster_time_gossip(&mut reply, ctx);
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

fn dispatch_inner(doc: &Document, ctx: &mut CommandContext) -> Document {
    let name = command_name(doc);

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
            // Time profile-eligible commands so dispatch can record a
            // `system.profile` entry when the per-database level requires it.
            let start = profile_eligible(name, doc).then(std::time::Instant::now);
            let reply = run_with_txn_envelope(name, handler, doc, ctx);
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
        return run_handler(handler, doc, ctx);
    }
    // commit/abort carry the same envelope but are controls (own handlers).
    if name == "commitTransaction" || name == "abortTransaction" {
        return run_handler(handler, doc, ctx);
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
        let mut t = txn.lock().unwrap();
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
        let mut t = txn.lock().unwrap();
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
fn attach_cluster_time_gossip(reply: &mut Document, ctx: &CommandContext) {
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
        "updateUser" => (A_CHANGE_PASSWORD, SCOPE_DATABASE),
        "dropUser" => (A_DROP_USER, SCOPE_DATABASE),
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
    fn standalone_replies_do_not_gossip_cluster_time() {
        // No replica-set persona: no gossip (matches standalone mongod).
        let reply = dispatch(&doc! {"ping": 1}, &mut ctx());
        assert!(reply.get("$clusterTime").is_none());
        assert!(reply.get("operationTime").is_none());
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
