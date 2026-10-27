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

/// The literal BSON decoder, for the crates that depend on this one.
pub use secantus_core::bsondoc;

pub mod admin;
mod agg_executor;
mod agg_meta;
mod agg_stream;
pub mod aggregate;
mod apply_ops;
pub mod argtypes;
pub mod auth;
mod authz;
mod authz_pipeline;
mod builtin_roles;
mod bulkwrite;
mod capped_admin;
pub mod changestream;
pub mod cluster_admin;
pub mod cluster_params;
pub mod crud;
pub mod cursors;
mod db_check;
pub mod dbhash;
mod ddl_checks;
pub mod diagnostics;
pub mod distinct;
mod explain;
mod generic;
mod hybrid;
mod index_builds;
mod index_filters;
pub mod names;
mod snapshot;
pub use generic::db_field_problem;
pub mod failpoints;
pub mod find;
pub mod findandmodify;
pub mod handshake;
mod hostinfo;
mod interrupt;
mod list_catalog;
pub mod lock_manager;
pub mod logbuf;
pub mod logical_sessions;
pub mod mapreduce;
mod misc_admin;
pub mod mongod_codes;
mod mongod_commands;
mod near;
mod notablescan;
mod opdebug;
pub mod ops;
pub mod params;
mod privileges;
mod procstats;
pub mod profiler;
pub mod rbac;
mod replset_admin;
mod resume_keystring;
pub mod roles;
mod rwconcern;
pub mod serverinfo;
mod session;
pub mod slowop;
mod stats_admin;
pub mod status;
pub mod stepdown;
pub mod storage;
mod timeseries;
mod ts_bucket_ns;
mod ts_buckets;
pub use ts_buckets::{BUCKETS_PREFIX, BUCKETS_STAGE, VIEWS_STAGE};
pub mod topstats;
pub mod transactions;
mod umc;
mod util;
mod validation_errors;
mod views;

use std::sync::{Arc, Mutex};

pub use auth::ConnectionAuth;
use bson::{doc, Bson, Document};
pub use cursors::{CursorError, CursorRegistry};
use secantus_core::deadline;
pub use secantus_wire::{MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE};
pub use storage::{CollectionSizes, Storage, StorageError, UpdateOutcome};

/// Max wire protocol version advertised in `hello` (mongod 8.2).
///
/// The advertised version is a CAPABILITY CONTRACT, not a label: drivers gate
/// features on it, and the spec suites gate tests on it in both directions
/// (`maxServerVersion: "7.99"` asserts a feature is REJECTED, `minServerVersion:
/// "8.0"` that it works). So it moves only when the 8.0 features it promises
/// exist -- `bulkWrite`, `sort` on updateOne/replaceOne, and change-event
/// `nsType`, all landed alongside this bump. Matches the probed server (8.2.11).
pub const WIRE_VERSION: i32 = 27;
/// `version` reported by `buildInfo` — the MongoDB-compatibility value drivers
/// gate feature flags on (change streams, pre-images, …).
pub const SERVER_VERSION: &str = "8.2.11";
/// `versionArray` companion to [`SERVER_VERSION`].
pub const SERVER_VERSION_ARRAY: [i32; 4] = [8, 2, 11, 0];
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

/// Lets `rotateCertificates` reload the server's TLS certificate and key
/// from their files. Implemented by the server when TLS is on.
pub trait CertificateRotator: Send + Sync {
    /// Re-read the files and use them for new connections; on failure keep
    /// the certificate in use and say why, as mongod's 140 message does.
    fn rotate(&self) -> Result<(), String>;
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
    /// Server-wide store of runtime-changed server parameters, written by
    /// `setParameter` and overlaid onto `getParameter`'s defaults. `None` in
    /// unit-test contexts and before the server wires one in, in which case
    /// `setParameter` still validates and answers but nothing persists.
    pub server_params: Option<Arc<params::ServerParams>>,
    /// Server-wide `replSetStepDown` window. While it is open this node is a
    /// SECONDARY: `hello` says so and writes are refused. `None` off-server.
    pub step_down: Option<Arc<stepdown::StepDownState>>,
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
    /// The server's TLS certificate reloader; `None` when TLS is off.
    pub cert_rotator: Option<Arc<dyn CertificateRotator>>,
    /// The server's running operations (`currentOp`, `killOp`). `None` in
    /// unit-test contexts.
    pub ops: Option<Arc<ops::OpRegistry>>,
    /// The server's in-memory log ring buffer, read by `getLog`. `None` until the
    /// server wires one in (and in unit-test contexts) — `getLog` then reports an
    /// empty log.
    pub logs: Option<Arc<logbuf::LogBuffer>>,
    /// Live connection counts for `serverStatus.connections`, snapshotted by the
    /// server when it builds the context. `None` off-server (unit tests) ⇒ zeros.
    pub conn_stats: Option<ConnStats>,
    /// Server-wide per-namespace operation accounting, read by `top`. `None`
    /// off-server (unit tests) ⇒ `top` reports zeros, as it always used to.
    pub top_stats: Option<Arc<topstats::TopStats>>,
    /// The server-wide slow-operation threshold and sample rate, set by
    /// `profile`. `None` off-server (unit tests) ⇒ `profile` keeps the values
    /// with the database's profiling level, as it always did.
    pub slow_ops: Option<Arc<slowop::SlowOpSettings>>,
    /// Every database's profiling level and filter, shared server-wide. A
    /// context without one gets its own on the first `profile` command.
    pub profiler: Option<Arc<profiler::Profiler>>,
    /// The operation `dispatch` is profiling, from its start to its end.
    pub(crate) profiling: Option<profiler::Pending>,
    /// The request came as a legacy `OP_QUERY`, and is answered `OP_REPLY`.
    pub op_query: bool,
    /// Set when the database has a profile filter: whether the filter
    /// matched, which decides the slow-query log line in place of `slowms`.
    pub slow_log: Option<bool>,
    /// Time this request spent WAITING for data rather than working: an
    /// awaitData `getMore` blocked on the oplog. Left out of the time the
    /// slow-operation log measures, or every idle change stream would log a
    /// slow query a second.
    pub awaited: std::cell::Cell<std::time::Duration>,
    /// Set by a cursor-producing handler (`find` / `getMore`) to hand the
    /// server the reply's document batch as **pre-encoded blobs** instead of an
    /// owned `Bson::Array` inside the reply document. The reply the handler
    /// returns then carries `cursor: { id, ns }` *without* the batch; the server
    /// splices the blobs onto the wire via `secantus_wire::encode_cursor_reply`,
    /// skipping the decode→re-encode round-trip that materialising them into the
    /// reply document would cost (`tasks/rust-perf-findings.md`, Phase 1).
    /// Out-of-band like `close_connection`. `None` for every non-cursor reply.
    pub pending_batch: Option<PendingBatch>,
    /// What a streamed read examined for this command's batch, for its
    /// `Slow query` line (RM-023).
    pub exec_stats: Option<ExecStats>,
    /// What the store counted for this operation, taken when its handler
    /// finished, before its profile entry is written (RM-023).
    pub op_counts: Option<storage::OperationCounts>,
    /// The locks the operation took, as its line prints them, taken with
    /// [`Self::op_counts`].
    pub op_locks: Option<Document>,
    /// The time spent planning the operation's query, for the line's and
    /// profile entry's `planningTimeMicros`.
    pub planning_micros: Option<i64>,
    /// The raw BSON byte slices of an `insert`'s kind-1 `documents` sequence,
    /// handed in by the server **un-decoded** so the insert handler can store the
    /// client's bytes verbatim (the raw-BSON write path) instead of paying a
    /// merge-decode → re-encode → storage-decode round-trip per document. `None`
    /// unless the request is an `insert` whose documents arrived as an `OP_MSG`
    /// kind-1 sequence (the common driver shape); inline-in-body inserts and every
    /// other command leave it `None` and take the decoded path. Consumed
    /// (`take()`n) by the insert handler.
    pub raw_insert_documents: Option<Vec<Vec<u8>>>,
    /// The server-wide lock manager (drives `serverStatus.locks` and the
    /// transaction-held collection locks DDL waits on). `None` in unit-test
    /// contexts and before the server wires one in, like the other services.
    pub locks: Option<Arc<lock_manager::LockManager>>,
    /// Set by the transaction path once it has taken this statement's locks
    /// with the transaction's owner (held to commit/abort): `run_handler`
    /// then skips its own statement-scoped acquisition, so an in-transaction
    /// statement is counted once and its collection intent lock is not
    /// released at the statement's end.
    pub(crate) txn_locks: bool,
}

/// A cursor reply's document batch as pre-encoded BSON blobs, plus which field
/// (`"firstBatch"` for `find`, `"nextBatch"` for `getMore`) it splices into
/// `cursor`. See `CommandContext::pending_batch`.
/// One batch of a streamed read, as its `Slow query` line reports it.
#[derive(Clone, Debug, Default)]
pub struct ExecStats {
    pub keys_examined: i64,
    pub docs_examined: i64,
    /// The cursor still open after the batch (0: none).
    pub cursor_id: i64,
    pub exhausted: bool,
    /// A `getMore`'s plan summary and the command that opened its cursor.
    pub plan_summary: Option<String>,
    pub originating_command: Option<Document>,
}

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
            server_params: None,
            step_down: None,
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
            cert_rotator: None,
            ops: None,
            logs: None,
            conn_stats: None,
            top_stats: None,
            slow_ops: None,
            profiler: None,
            profiling: None,
            op_query: false,
            slow_log: None,
            awaited: std::cell::Cell::new(std::time::Duration::ZERO),
            pending_batch: None,
            exec_stats: None,
            op_counts: None,
            op_locks: None,
            planning_micros: None,
            raw_insert_documents: None,
            locks: None,
            txn_locks: false,
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

    /// Attach the server-wide lock manager (builder-style). Drives
    /// `serverStatus.locks` and the collection locks transactions hold.
    pub fn with_locks(mut self, locks: Arc<lock_manager::LockManager>) -> Self {
        self.locks = Some(locks);
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

    /// Attach the server's TLS certificate reloader (builder-style).
    pub fn with_cert_rotator(mut self, rotator: Option<Arc<dyn CertificateRotator>>) -> Self {
        self.cert_rotator = rotator;
        self
    }

    /// Attach the server's registry of running operations (builder-style).
    pub fn with_ops(mut self, ops: Arc<ops::OpRegistry>) -> Self {
        self.ops = Some(ops);
        self
    }

    /// Attach the server's in-memory log ring buffer (builder-style). `getLog`
    /// reads it.
    pub fn with_logs(mut self, logs: Arc<logbuf::LogBuffer>) -> Self {
        self.logs = Some(logs);
        self
    }

    /// Attach the server-wide slow-operation settings (builder-style).
    pub fn with_slow_ops(mut self, settings: Arc<slowop::SlowOpSettings>) -> Self {
        self.slow_ops = Some(settings);
        self
    }

    /// Attach the server-wide profiler settings (builder-style).
    pub fn with_profiler(mut self, profiler: Arc<profiler::Profiler>) -> Self {
        self.profiler = Some(profiler);
        self
    }

    /// Attach this instant's connection counts (builder-style).
    pub fn with_top_stats(mut self, stats: Arc<topstats::TopStats>) -> Self {
        self.top_stats = Some(stats);
        self
    }

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

    /// [`with_failpoints`] for a server that may not have one — `None` leaves
    /// the context without a registry, which is what makes the test-only
    /// commands report `CommandNotFound`.
    ///
    /// [`with_failpoints`]: Self::with_failpoints
    /// Share the server-wide runtime-parameter store, so a value set on one
    /// connection is visible to every other one (and to `getParameter`).
    pub fn with_server_params(mut self, params: Arc<params::ServerParams>) -> Self {
        self.server_params = Some(params);
        self
    }

    /// Share the server-wide step-down window.
    pub fn with_step_down(mut self, state: Arc<stepdown::StepDownState>) -> Self {
        self.step_down = Some(state);
        self
    }

    pub fn with_failpoints_opt(
        mut self,
        failpoints: Option<Arc<failpoints::FailPointRegistry>>,
    ) -> Self {
        self.failpoints = failpoints;
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

impl From<secantus_core::deadline::MaxTimeMsExpired> for CommandError {
    /// So a polling loop can `?` its way out to the handler boundary without
    /// every intermediate signature learning about time.
    fn from(_: secantus_core::deadline::MaxTimeMsExpired) -> Self {
        CommandError::max_time_expired()
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

    /// `50 MaxTimeMSExpired` — the operation outlived its `maxTimeMS` budget.
    pub fn max_time_expired() -> Self {
        use secantus_core::deadline::MaxTimeMsExpired as E;
        CommandError::new(E::CODE, E::CODE_NAME, E::MESSAGE)
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
/// Commands mongod ships only behind `enableTestCommands`.
///
/// Kept as a list rather than inlined so adding the next one (mongod has
/// several) is a one-line change beside this comment, and so the Python
/// server's mirror has something to be checked against.
pub fn is_test_only_command(name: &str) -> bool {
    matches!(
        name,
        "configureFailPoint" | "refreshLogicalSessionCacheNow" | "reapLogicalSessionCacheNow"
    )
}

/// The commands of this server's own, which mongod does not have.
pub(crate) const OWN_COMMANDS: &[&str] = &[
    "secantusAdmin.archiveBaseSnapshot",
    "secantusAdmin.backupArchive",
    "secantusAdmin.pruneOplog",
    "secantusAdmin.pruneTtl",
    "secantusAdmin.restoreArchive",
];

fn lookup(name: &str) -> Option<Handler> {
    Some(match name {
        "hello" | "isMaster" | "ismaster" => handshake::hello,
        "ping" => handshake::ping,
        "replSetGetStatus" => handshake::repl_set_get_status,
        "buildInfo" | "buildinfo" => handshake::build_info,
        "bulkWrite" => bulkwrite::bulk_write,
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
        "convertToCapped" => capped_admin::convert_to_capped,
        "cloneCollectionAsCapped" => capped_admin::clone_collection_as_capped,
        "compact" => capped_admin::compact,
        "autoCompact" => capped_admin::auto_compact,
        "reIndex" => capped_admin::re_index,
        "explain" => explain::explain,
        "drop" => admin::drop,
        "secantusAdmin.backupArchive" => admin::backup_archive,
        "secantusAdmin.archiveBaseSnapshot" => admin::archive_base_snapshot,
        "secantusAdmin.pruneOplog" => admin::prune_oplog,
        "secantusAdmin.pruneTtl" => admin::prune_ttl,
        "secantusAdmin.restoreArchive" => admin::restore_archive,
        "listCollections" => admin::list_collections,
        "listDatabases" => stats_admin::list_databases,
        "listIndexes" => admin::list_indexes,
        "createIndexes" => admin::create_indexes,
        "createSearchIndexes" | "updateSearchIndex" | "dropSearchIndex" => {
            admin::search_index_not_supported
        }
        "dropIndexes" => admin::drop_indexes,
        "dropDatabase" => admin::drop_database,
        "renameCollection" => admin::rename_collection,
        "collStats" | "collstats" => stats_admin::coll_stats,
        "dataSize" | "datasize" => stats_admin::data_size,
        // mongod accepts the lowercase `dbstats` spelling too (the C driver's
        // command_fully_qualified test sends it over the legacy OP_QUERY path).
        "dbStats" | "dbstats" => stats_admin::db_stats,
        "serverStatus" => admin::server_status,
        "currentOp" => admin::current_op,
        "killOp" => serverinfo::kill_op,
        "validate" => admin::validate,
        "profile" => admin::profile,
        "startSession" => diagnostics::start_session,
        "endSessions" => end_sessions,
        "killSessions" => kill_sessions,
        "killAllSessions" | "killAllSessionsByPattern" => kill_all_sessions,
        "refreshSessions" => refresh_sessions,
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
        "getParameter" => serverinfo::get_parameter,
        "setParameter" => params::set_parameter,
        "replSetStepDown" => stepdown::replset_step_down,
        "getCmdLineOpts" => diagnostics::get_cmd_line_opts,
        "connectionStatus" => serverinfo::connection_status,
        "whatsmyuri" => serverinfo::whatsmyuri,
        "listCommands" => serverinfo::list_commands,
        "logout" => serverinfo::logout,
        "getDefaultRWConcern" => rwconcern::get_default_rw_concern,
        "setDefaultRWConcern" => rwconcern::set_default_rw_concern,
        "logRotate" => cluster_admin::log_rotate,
        "connPoolStats" => cluster_admin::conn_pool_stats,
        "setFeatureCompatibilityVersion" => cluster_admin::set_feature_compatibility_version,
        "dbHash" | "dbhash" => dbhash::db_hash,
        "replSetGetConfig" => serverinfo::repl_set_get_config,
        "replSetReconfig" => replset_admin::repl_set_reconfig,
        "shardingState" => serverinfo::sharding_state,
        "fsyncUnlock" => serverinfo::fsync_unlock,
        "fsync" => diagnostics::fsync,
        "hostInfo" => diagnostics::host_info,
        "top" => diagnostics::top,
        "getLog" => serverinfo::get_log,
        "replSetInitiate" => replset_admin::repl_set_initiate,
        "appendOplogNote" => replset_admin::append_oplog_note,
        "replSetResizeOplog" => replset_admin::repl_set_resize_oplog,
        "lockInfo" => replset_admin::lock_info,
        "features" => replset_admin::features,
        "getClusterParameter" => cluster_params::get_cluster_parameter,
        "setClusterParameter" => cluster_params::set_cluster_parameter,
        "invalidateUserCache" => misc_admin::invalidate_user_cache,
        "replSetGetRBID" => misc_admin::repl_set_get_rbid,
        "replSetFreeze" => misc_admin::repl_set_freeze,
        "replSetMaintenance" => misc_admin::repl_set_maintenance,
        "dropConnections" => misc_admin::drop_connections,
        "setUserWriteBlockMode" => misc_admin::set_user_write_block_mode,
        "applyOps" => apply_ops::apply_ops,
        "rotateCertificates" => misc_admin::rotate_certificates,
        "validateDBMetadata" => misc_admin::validate_db_metadata,
        "dbCheck" => db_check::db_check,
        "releaseMemory" => misc_admin::release_memory,
        "filemd5" => misc_admin::filemd5,
        "planCacheClear" => replset_admin::plan_cache_clear,
        "planCacheClearFilters" => index_filters::clear_filters,
        "planCacheListFilters" => index_filters::list_filters,
        "planCacheSetFilter" => index_filters::set_filter,
        "configureFailPoint" => configure_fail_point,
        "refreshLogicalSessionCacheNow" => refresh_logical_session_cache_now,
        "reapLogicalSessionCacheNow" => reap_logical_session_cache_now,
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
    // A database or collection name that is not UTF-8 is escaped on the way
    // in and written back as its bytes on the way out (RM-222).
    let escaped = match names::from_bytes_named(doc) {
        Ok(escaped) => escaped,
        Err(e) => return e.into_reply(),
    };
    let (doc, carried): (&Document, &[String]) = match &escaped {
        Some((d, carried)) => {
            if let Ok(db) = d.get_str("$db") {
                ctx.db_name = db.to_string();
            }
            (d, carried)
        }
        None => (doc, &[]),
    };
    let mut reply = dispatch_named(doc, ctx);
    names::to_bytes(&mut reply, command_name(doc), carried);
    reply
}

fn dispatch_named(doc: &Document, ctx: &mut CommandContext) -> Document {
    handshake::process_started();
    let started = std::time::Instant::now();
    let stats = ctx.top_stats.clone();
    if let Some(stats) = &stats {
        stats.count_op(doc);
    }
    let name = command_name(doc);
    let _active = stats
        .as_ref()
        .map(|s| s.status.activate(status::OpKind::of(name)));
    // Which of this command's interrupt polls fail (RM-313).
    let _trips = secantus_core::deadline::trip_scope();
    // Listed by `currentOp` and killable by `killOp` while it runs (RM-216).
    let _op = ctx
        .ops
        .clone()
        .map(|registry| ops::begin(&registry, doc, ctx));
    // The write concern a write with none of its own is held to: the stored
    // default, read BEFORE the command runs, so `setDefaultRWConcern`'s own
    // write answers for the default it replaces (mongod's order).
    let client_wc = doc.get("writeConcern").and_then(Bson::as_document);
    let default_wc = if client_wc.is_none() && takes_default_write_concern(name, doc) {
        rwconcern::default_write_concern(ctx)
    } else {
        None
    };
    let dropping = if default_wc.is_some() && name == "dropDatabase" {
        ctx.storage
            .as_ref()
            .and_then(|s| s.list_collections(&ctx.db_name).ok())
            .map_or(0, |names| names.len())
    } else {
        0
    };
    // What the store does for the operation is counted from here (RM-023).
    let counting = ctx.storage.as_ref().and_then(|s| s.begin_operation_counts());
    let planning = explain::planning_scope();
    ctx.profiling = profiler::begin(name, doc, ctx);
    let mut reply = dispatch_inner(doc, ctx);
    // An interrupted operation -- killed, or out of `maxTimeMS` -- answers
    // as mongod does: 11601 for a kill, under the wrapper its command takes,
    // and a write's interrupted write-concern wait (RM-216, RM-313).
    interrupt::finish(name, doc, &mut reply, ctx, client_wc, default_wc.as_ref());
    // A journaled write is acknowledged only once the journal holds it:
    // mongod's `waitUntilDurable`. Before this, `j` was echoed and no sync
    // followed, and a SIGKILL lost writes acknowledged with `j: true`.
    if wants_journal(name, doc, client_wc, default_wc.as_ref()) {
        if let Some(storage) = &ctx.storage {
            if let Err(e) = storage.flush_journal() {
                // The write is not durable: that is the answer, not a
                // footnote to an `ok: 1`.
                reply = crate::util::command_error(e).into_reply();
            }
        }
    }
    // An unacknowledged write's reply is zeroed below; its profile entry
    // records what it did.
    let inner_n = ctx.profiling.as_ref().and_then(|_| match reply.get("n") {
        Some(Bson::Int32(n)) => Some(i64::from(*n)),
        Some(Bson::Int64(n)) => Some(*n),
        _ => None,
    });
    // `dropDatabase` resets the database's profiling settings (measured
    // 8.2.11: the level and filter are gone afterwards).
    if name == "dropDatabase" && reply.get_f64("ok").unwrap_or(0.0) == 1.0 {
        if let Some(p) = &ctx.profiler {
            p.forget(&ctx.db_name);
        }
    }
    // Counted before a `w: 0` write's reply is emptied: mongod counts what
    // the write did, not what it told the client.
    if let Some(stats) = &stats {
        let pending = ctx.pending_batch.as_ref().map_or(0, |b| b.batch.len());
        stats.status.record(doc, &reply, pending, started.elapsed());
        status_authentication(&stats.status, name, doc, &reply, ctx, started);
    }
    // A user or role may have changed: every connection's effective
    // privileges are recomputed on its next command (`rbac::effective`).
    if umc::is_user_or_role_command(name) || name == "secantusAdmin.restoreArchive" {
        if let Some(p) = &ctx.server_params {
            p.invalidate_user_cache();
        }
    }
    unacknowledged_write_reply(doc, &mut reply);
    if let Some(wc) = client_wc {
        attach_write_concern_error(wc, "clientSupplied", &mut reply);
    } else if let Some(wc) = &default_wc {
        attach_write_concern_error(wc, "customDefault", &mut reply);
    }
    if name == "setDefaultRWConcern" {
        fail_on_write_concern_error(&mut reply);
    } else if name == "dropDatabase" {
        let db = ctx.db_name.clone();
        fail_drop_database_on_write_concern_error(&mut reply, &db, dropping, ctx);
    }
    if may_have_written_settings(name, ctx) {
        rwconcern::forget_cached(ctx);
    }
    attach_cluster_time_gossip(doc, &mut reply, ctx);
    order_write_reply(doc, &mut reply);
    // What the operation counted, before its profile entry is written.
    ctx.op_counts = ctx.storage.as_ref().and_then(|s| s.operation_counts());
    ctx.op_locks = Some(lock_manager::operation_locks(false));
    ctx.planning_micros = planning.micros();
    drop(planning);
    drop(counting);
    profiler::finish(name, doc, inner_n, &reply, ctx);
    reply
}

/// Count an authentication for `serverStatus.security` (measured 8.2.11): a
/// `saslStart` or `authenticate` is one received; it succeeds when the
/// conversation ends `done`, and its time runs from the first command to the
/// last.
fn status_authentication(
    counters: &status::StatusCounters,
    name: &str,
    doc: &Document,
    reply: &Document,
    ctx: &CommandContext,
    started: std::time::Instant,
) {
    if matches!(name, "hello" | "isMaster" | "ismaster") {
        if doc.contains_key("saslSupportedMechs") {
            counters.sasl_supported_mechs_received();
        }
        return;
    }
    if !matches!(name, "saslStart" | "saslContinue" | "authenticate") {
        return;
    }
    let ok = reply.get_f64("ok").unwrap_or(0.0) == 1.0;
    let done = !ok || name == "authenticate" || reply.get_bool("done").unwrap_or(false);
    let Some(auth) = ctx.conn_auth.as_ref() else {
        return;
    };
    let mut auth = auth.lock().unwrap_or_else(|e| e.into_inner());
    let begun = if name == "saslContinue" {
        auth.auth_started.take()
    } else {
        let mechanism = doc.get_str("mechanism").unwrap_or("").to_string();
        counters.authentication_received(&mechanism);
        Some((mechanism, started))
    };
    let Some((mechanism, since)) = begun else {
        return;
    };
    if done {
        counters.authentication_ended(&mechanism, ok, since.elapsed());
    } else {
        auth.auth_started = Some((mechanism, since));
    }
}

/// A write sent with `w: 0` asked not to be told what happened, and mongod
/// tells it nothing: `n` (and `nModified`) are 0 and no write error or
/// upserted id is reported, though the write ran (measured 8.2.11).
/// `findAndModify` answers in full.
fn unacknowledged_write_reply(doc: &Document, reply: &mut Document) {
    let name = command_name(doc);
    if !matches!(name, "insert" | "update" | "delete") || !reply.contains_key("n") {
        return;
    }
    let w = doc
        .get_document("writeConcern")
        .ok()
        .and_then(|wc| wc.get("w"));
    if !matches!(w, Some(n @ (Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)))
        if write_concern_w_number(n) == 0)
    {
        return;
    }
    reply.insert("n", 0_i32);
    if name == "update" {
        reply.insert("nModified", 0_i32);
    }
    reply.remove("writeErrors");
    reply.remove("upserted");
    reply.remove("retriedStmtIds");
}

/// Attach a `writeConcernError` when the command carried an integer `w > 1`.
/// SecantusDB advertises as a single-node `secantus` replica set, so a write
/// concern wider than one node can never be satisfied — mongod returns the write
/// result *plus* an `UnsatisfiableWriteConcern` (100) writeConcernError (the
/// write still happened). Only attaches to a successful reply that carried a
/// write concern (reads don't send one).
///
/// The name was `CannotSatisfyWriteConcern` on both servers until 2026-09-28,
/// when a `w: 5` write against a single-node replica-set mongod 8.2.11 was
/// actually run: it answers `UnsatisfiableWriteConcern`, and so does a
/// `failCommand` injecting 100 at the top level. Neither context uses the old
/// name.
///
/// Measured again on 2026-09-30 across insert / update / delete / findAndModify
/// / create / createIndexes / drop, three things this missed:
///
/// - an UNKNOWN tag (`w: "tag"`) is not a pre-flight refusal: the write runs,
///   and the reply carries `79 UnknownReplWriteConcern` here, like `w > 1`;
/// - every `writeConcernError` carries `errInfo.writeConcern` -- the client's
///   `w`, its `j` if given, `wtimeout` (0 when absent) and
///   `provenance: "clientSupplied"`, in that order;
/// - it sits immediately BEFORE `ok`, not after it.
fn attach_write_concern_error(wc: &Document, provenance: &str, reply: &mut Document) {
    if reply.get_f64("ok").unwrap_or(0.0) != 1.0 || reply.contains_key("writeConcernError") {
        return;
    }
    // A null `w` is the empty tag; a tag set echoes its counts as longs.
    let w = match wc.get("w") {
        Some(bson::Bson::Null) => Some(bson::Bson::String(String::new())),
        Some(bson::Bson::Document(tags)) => Some(bson::Bson::Document(
            tags.iter()
                .map(|(k, v)| (k.clone(), bson::Bson::Int64(write_concern_w_number(v))))
                .collect(),
        )),
        // A number is echoed as mongod stores it: truncated, as an int.
        Some(
            n @ (bson::Bson::Int32(_)
            | bson::Bson::Int64(_)
            | bson::Bson::Double(_)
            | bson::Bson::Decimal128(_)),
        ) => {
            let v = write_concern_w_number(n);
            Some(i32::try_from(v).map_or(bson::Bson::Int64(v), bson::Bson::Int32))
        }
        other => other.cloned(),
    };
    let (code, code_name, errmsg) = match &w {
        Some(
            n @ (bson::Bson::Int32(_)
            | bson::Bson::Int64(_)
            | bson::Bson::Double(_)
            | bson::Bson::Decimal128(_)),
        ) if write_concern_w_number(n) > 1 => (
            100,
            "UnsatisfiableWriteConcern",
            "Not enough data-bearing nodes".to_string(),
        ),
        Some(bson::Bson::String(tag)) if tag != "majority" => (
            79,
            "UnknownReplWriteConcern",
            format!("No write concern mode named '{tag}' found in replica set configuration"),
        ),
        Some(bson::Bson::Document(tags)) => match tags.keys().next() {
            Some(key) => (
                4,
                "NoSuchKey",
                format!("No replica set tag key {key} in config"),
            ),
            None => return,
        },
        _ => return,
    };
    let mut echoed = Document::new();
    if let Some(w) = w {
        echoed.insert("w", w);
    }
    if let Some(j) = wc.get("j") {
        let j = match j {
            bson::Bson::Boolean(b) => *b,
            other => util::as_i64(other).is_some_and(|n| n != 0),
        };
        echoed.insert("j", j);
    }
    echoed.insert(
        "wtimeout",
        wc.get("wtimeout")
            .and_then(util::as_i64)
            .map_or(bson::Bson::Int32(0), |n| {
                i32::try_from(n).map_or(bson::Bson::Int64(n), bson::Bson::Int32)
            }),
    );
    echoed.insert("provenance", provenance);
    let mut wce = Document::new();
    wce.insert("code", code);
    wce.insert("codeName", code_name);
    wce.insert("errmsg", errmsg);
    wce.insert("errInfo", doc! {"writeConcern": echoed});
    // Before `ok`: take `ok` out, add the error, put `ok` back last.
    let ok = reply.remove("ok");
    reply.insert("writeConcernError", wce);
    if let Some(ok) = ok {
        reply.insert("ok", ok);
    }
}

/// The commands a stored default write concern applies to when the client
/// sends none: measured on mongod 8.2.11 with a default of `w: 3` on a
/// one-member set (2026-10-14), each of these answered with the
/// `customDefault` write-concern error and a read did not.
/// Whether a command's write concern holds it to the journal before it is
/// acknowledged. `j: true` does; with `j` unset, `w: "majority"` does,
/// because `writeConcernMajorityJournalDefault` is true on this one-member
/// set -- and that is the implicit default, so a write with no write concern
/// at all is journaled, as on mongod 8.2.11. `w: 0` never waits. A statement
/// inside a transaction takes no write concern; its commit does.
fn wants_journal(
    name: &str,
    doc: &Document,
    client_wc: Option<&Document>,
    default_wc: Option<&Document>,
) -> bool {
    let in_transaction = doc.contains_key("autocommit");
    let takes =
        name == "commitTransaction" || (!in_transaction && takes_default_write_concern(name, doc));
    if !takes {
        return false;
    }
    let wc = client_wc.filter(|wc| !wc.is_empty()).or(default_wc);
    let Some(wc) = wc else {
        return true; // the implicit `{w: "majority"}`
    };
    let truthy = |v: &Bson| match v {
        Bson::Boolean(b) => *b,
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Double(d) => *d != 0.0,
        _ => false,
    };
    match (wc.get("w"), wc.get("j")) {
        (Some(w), _) if crate::util::as_i64(w) == Some(0) => false,
        (_, Some(j)) => truthy(j),
        (Some(Bson::String(w)), None) => w == "majority",
        _ => false,
    }
}

fn takes_default_write_concern(name: &str, doc: &Document) -> bool {
    match name {
        "insert"
        | "update"
        | "delete"
        | "findAndModify"
        | "findandmodify"
        | "bulkWrite"
        | "create"
        | "createIndexes"
        | "dropIndexes"
        | "drop"
        | "dropDatabase"
        | "renameCollection"
        | "collMod"
        | "convertToCapped"
        | "cloneCollectionAsCapped"
        | "setFeatureCompatibilityVersion"
        | "setDefaultRWConcern" => true,
        "aggregate" => doc
            .get_array("pipeline")
            .ok()
            .and_then(|p| p.last())
            .and_then(Bson::as_document)
            .is_some_and(|stage| stage.contains_key("$out") || stage.contains_key("$merge")),
        n => umc::is_user_or_role_command(n),
    }
}

/// Whether `name` could have changed `config.settings` other than through
/// `setDefaultRWConcern`, so the cached defaults are read again.
fn may_have_written_settings(name: &str, ctx: &CommandContext) -> bool {
    matches!(
        name,
        "secantusAdmin.restoreArchive" | "renameCollection" | "bulkWrite"
    ) || (ctx.db_name == rwconcern::SETTINGS_DB
        && matches!(
            name,
            "insert"
                | "update"
                | "delete"
                | "findAndModify"
                | "findandmodify"
                | "drop"
                | "dropDatabase"
                | "aggregate"
        ))
}

/// `setDefaultRWConcern` FAILS when its own write cannot satisfy the write
/// concern, where another write reports the error beside its result: the
/// write-concern error becomes the reply's own, with mongod's `Error
/// details` rendering of the concern (measured 8.2.11, 2026-10-14).
fn fail_on_write_concern_error(reply: &mut Document) {
    let Ok(wce) = reply.get_document("writeConcernError").cloned() else {
        return;
    };
    if reply.get_f64("ok") != Ok(1.0) {
        return;
    }
    let details = wce
        .get_document("errInfo")
        .map(mongod_tostring)
        .unwrap_or_default();
    let errmsg = wce.get_str("errmsg").unwrap_or_default();
    let code = wce.get_i32("code").unwrap_or(8);
    *reply = doc! {
        "writeConcernError": wce.clone(),
        "ok": 0.0,
        "errmsg": format!("{errmsg}; Error details: {details}"),
        "code": code,
        "codeName": util::error_code_name(code),
    };
}

/// `dropDatabase` under a default write concern no node can satisfy FAILS
/// too, saying how many collection drops it waited for and the opTime of
/// the last (measured 8.2.11, 2026-10-14). `dropped` is how many
/// collections the database had; with none there was nothing to wait for.
fn fail_drop_database_on_write_concern_error(
    reply: &mut Document,
    db: &str,
    dropped: usize,
    ctx: &CommandContext,
) {
    let Ok(wce) = reply.get_document("writeConcernError").cloned() else {
        return;
    };
    if reply.get_f64("ok") != Ok(1.0) || dropped == 0 {
        return;
    }
    let at = ctx.storage.as_ref().map_or(
        bson::Timestamp {
            time: 0,
            increment: 0,
        },
        |s| s.peek_cluster_time(),
    );
    let errmsg = wce.get_str("errmsg").unwrap_or_default();
    let code = wce.get_i32("code").unwrap_or(8);
    *reply = doc! {
        "writeConcernError": wce.clone(),
        "ok": 0.0,
        "errmsg": format!(
            "dropDatabase {db} failed waiting for {dropped} collection drop(s) (most recent \
             drop optime: {{ ts: Timestamp({}, {}), t: 1 }}) to replicate. :: caused by :: \
             {errmsg}",
            at.time, at.increment
        ),
        "code": code,
        "codeName": util::error_code_name(code),
    };
}

/// A document as mongod's `BSONObj::toString` prints it inside a message.
fn mongod_tostring(doc: &Document) -> String {
    fn value(v: &Bson) -> String {
        match v {
            Bson::String(s) => format!("\"{s}\""),
            Bson::Document(d) => mongod_tostring(d),
            Bson::Boolean(b) => b.to_string(),
            Bson::Int32(n) => n.to_string(),
            Bson::Int64(n) => n.to_string(),
            Bson::Double(d) => format!("{d:?}"),
            other => other.to_string(),
        }
    }
    if doc.is_empty() {
        return "{}".to_string();
    }
    let fields: Vec<String> = doc
        .iter()
        .map(|(k, v)| format!("{k}: {}", value(v)))
        .collect();
    format!("{{ {} }}", fields.join(", "))
}

/// A numeric `w` as mongod reads it: `safeNumberLong` (truncated toward
/// zero, NaN as 0, saturating).
pub(crate) fn write_concern_w_number(w: &bson::Bson) -> i64 {
    match w {
        bson::Bson::Int32(n) => i64::from(*n),
        bson::Bson::Int64(n) => *n,
        bson::Bson::Double(d) if d.is_nan() => 0,
        bson::Bson::Double(d) => *d as i64,
        bson::Bson::Decimal128(d) => {
            d.to_string()
                .parse::<f64>()
                .map_or(0, |f| if f.is_nan() { 0 } else { f as i64 })
        }
        _ => 0,
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
            | "convertToCapped"
            | "cloneCollectionAsCapped"
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
/// bool or non-number/string → `TypeMismatch` (14); an integer `w` outside
/// `[0, 50]` → `FailedToParse` (9). `None` when absent or well-formed. (`w > 1`
/// and an unknown tag still succeed with a `writeConcernError` attached — see
/// `attach_write_concern_error`.)
fn validate_write_concern(doc: &Document, command: &str) -> Option<CommandError> {
    let wc = match doc.get("writeConcern") {
        // An explicit `writeConcern: null` is ACCEPTED — the BSON-field family's
        // null-means-absent rule. This used to reject it.
        None | Some(Bson::Null) => return None,
        Some(Bson::Document(d)) => d,
        Some(v) => {
            // Was a bespoke "writeConcern must be a document"; mongod uses the
            // ordinary BSON-field wording, naming the command and the type.
            return Some(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field '{command}.writeConcern' is the wrong type '{}', \
                     expected type 'object'",
                    secantus_core::query::bson_type_name(v)
                ),
            ));
        }
    };
    const WRITE_CONCERN_FIELDS: [&str; 6] =
        ["w", "j", "wtimeout", "fsync", "getLastError", "provenance"];
    if let Some(unknown) = wc
        .keys()
        .find(|k| !WRITE_CONCERN_FIELDS.contains(&k.as_str()))
    {
        return Some(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'WriteConcernOptions.{unknown}' is an unknown field."),
        ));
    }
    // `j` and `fsync` are a bool or any number, and a wrong type is reported
    // before anything about `w`: the first of the two as the document has
    // them (measured 8.2.11, 2026-10-12).
    for (field, value) in wc.iter().filter(|(k, _)| *k == "j" || *k == "fsync") {
        if !matches!(
            value,
            Bson::Null
                | Bson::Boolean(_)
                | Bson::Int32(_)
                | Bson::Int64(_)
                | Bson::Double(_)
                | Bson::Decimal128(_)
        ) {
            return Some(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'WriteConcernOptions.{field}' is the wrong type '{}', expected \
                     types '[bool, double, long, decimal, int]'",
                    secantus_core::query::bson_type_name(value)
                ),
            ));
        }
    }
    if let Some(w) = wc.get("w") {
        match w {
            // Any number: mongod reads it as `safeNumberLong` and bounds that
            // (measured 8.2.11, 2026-10-06: a double `w: 1.5` is accepted).
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
                let n = write_concern_w_number(w);
                if !(0..=50).contains(&n) {
                    return Some(CommandError::new(
                        9,
                        "FailedToParse",
                        format!(
                            "w has to be a non-negative number and not greater than 50; found: {n}"
                        ),
                    ));
                }
            }
            // Any tag parses, and so does a tag set or a null (the empty
            // tag); an unknown one is reported AFTER the write, as a
            // `writeConcernError` (see `attach_write_concern_error`).
            Bson::Document(tags) if tags.is_empty() => {
                return Some(CommandError::new(
                    9,
                    "FailedToParse",
                    "tagged write concern requires tags",
                ))
            }
            Bson::String(_) | Bson::Document(_) | Bson::Null => {}
            other => {
                return Some(CommandError::new(
                    9,
                    "FailedToParse",
                    format!(
                        "w has to be a number, string, or object; found: {}",
                        secantus_core::query::bson_type_name(other)
                    ),
                ))
            }
        }
    }
    // `j` and `fsync` together are refused when both are set; each alone, or
    // either one false, is fine. `wtimeout` is read as a number whatever its
    // type: mongod accepts a string or a document there (measured 8.2.11,
    // 2026-10-12; this refused both).
    let set = |field: &str| wc.get(field).is_some_and(crate::util::truthy);
    if set("j") && set("fsync") {
        return Some(CommandError::new(
            9,
            "FailedToParse",
            "fsync and j options cannot be used together",
        ));
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
    // An escaped name that is not UTF-8 (`names`) holds NULs of its own; a
    // client's NUL was refused before it was escaped.
    if value.contains('\0') && bsondoc::key_bytes(value).is_none() {
        Some(CommandError::new(
            73,
            "InvalidNamespace",
            // mongod's two wordings (8.2.11): one for a database name, one
            // for anything with a collection in it.
            if kind.starts_with("database") {
                "namespace cannot have embedded null characters".to_string()
            } else {
                "namespaces cannot have embedded null characters".to_string()
            },
        ))
    } else {
        None
    }
}

/// Why the collection a command names cannot be one: mongod reads it before
/// anything else of the command, and says the same thing for every command
/// that takes a collection (measured 8.2.11 across eighteen of them).
fn collection_name_problem(name: &str, doc: &Document, db: &str) -> Option<CommandError> {
    if !matches!(
        name,
        "find"
            | "insert"
            | "update"
            | "delete"
            | "findAndModify"
            | "findandmodify"
            | "count"
            | "distinct"
            | "createIndexes"
            | "drop"
            | "create"
            | "collMod"
            | "dropIndexes"
            | "collStats"
            | "collstats"
            | "validate"
            | "listIndexes"
            | "killCursors"
            | "compact"
    ) {
        return None;
    }
    let invalid = |message: String| Some(CommandError::new(73, "InvalidNamespace", message));
    match doc.get(name) {
        Some(Bson::String(coll)) if coll.is_empty() => invalid(match name {
            "distinct" | "validate" | "compact" => {
                format!("Invalid namespace specified '{db}'")
            }
            _ => format!("Invalid namespace specified: {db}"),
        }),
        Some(Bson::String(_)) => None,
        Some(other) => invalid(format!(
            "Collection name has invalid type {}",
            secantus_core::query::bson_type_name(other)
        )),
        None => None,
    }
}

fn dispatch_inner(doc: &Document, ctx: &mut CommandContext) -> Document {
    let name = command_name(doc);

    // Database-name length limit: mongod rejects any namespace whose database
    // component exceeds 63 bytes with InvalidNamespace before the command runs
    // (libmongoc's `long_namespace/unsupported_long_db` inserts into a
    // 64-character database and expects a server error). Mirrors commands.py.
    if let Some(refused) = ddl_checks::invalid_database_name(&ctx.db_name, doc) {
        return refused.into_reply();
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

    // An unknown command says so before anything about its fields.
    if lookup(name).is_some() {
        // `compact` reads its namespace before its other fields (measured
        // 8.2.11: `{compact: "", bogus: 1}` is InvalidNamespace).
        if name == "compact" {
            if let Some(e) = collection_name_problem(name, doc, &ctx.db_name) {
                return e.into_reply();
            }
        }
        if let Err(e) = generic::check(doc, name) {
            return e.into_reply();
        }
    }
    if let Err(e) = validate_read_concern(doc, name, ctx) {
        return e.into_reply();
    }
    if let Err(e) = validate_api(doc, name) {
        return e.into_reply();
    }
    if let Err(e) = refuse_write_concern(doc, name) {
        return e.into_reply();
    }

    // Test-only commands are gated the way mongod gates them: without
    // `enableTestCommands` they are not merely refused, they DO NOT EXIST.
    // Measured 8.2.11 (2026-09-28) — a mongod started without the parameter
    // answers `configureFailPoint` with `59 CommandNotFound :: no such
    // command: 'configureFailPoint'`, and reports `enableTestCommands: false`
    // from `getParameter`.
    //
    // The registry's absence IS the gate: a server that did not enable test
    // commands wires no `FailPointRegistry`, so there is nothing for the
    // handler to configure and nothing for `failCommand` to match. That keeps
    // the decision at server startup, where the operator made it, rather than
    // duplicating a flag down here.
    if is_test_only_command(name) && ctx.failpoints.is_none() {
        return CommandError::command_not_found(name).into_reply();
    }
    // A command mongod runs only against `admin` says so from any other
    // database before it reads its arguments (`{killOp: 1}` with no `op`
    // answers this, not the missing field), and whether or not this server
    // has the command. The ones excepted parse first or carry their own
    // refusal.
    if ctx.db_name != "admin"
        && mongod_commands::admin_only(name)
        && !matches!(
            name,
            "renameCollection"
                | "bulkWrite"
                | "setParameter"
                | "top"
                | "listDatabases"
                | "abortTransaction"
                | "commitTransaction"
                | "getClusterParameter"
                | "autoCompact"
        )
    {
        return CommandError::new(
            13,
            "Unauthorized",
            format!("{name} may only be run against the admin database."),
        )
        .into_reply();
    }
    match lookup(name) {
        Some(handler) => {
            if let Some(e) = collection_name_problem(name, doc, &ctx.db_name) {
                return e.into_reply();
            }
            // `maxTimeMS` is a generic command field — mongod's IDL validates
            // it on every command, so it belongs here rather than in the three
            // handlers that used to call it (the other 21 took a wrong-typed
            // value silently). Both neighbours were measured against an
            // auth-enabled mongod 8.2.1: CommandNotFound (59) WINS over this
            // check, so it sits inside the `Some(handler)` arm; this check WINS
            // over authorization (13), so it sits above `authorize`.
            if let Err(e) = argtypes::require_max_time_ms(doc, name) {
                return e.into_reply();
            }
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
                ctx.failpoints.as_ref().and_then(|r| {
                    let recorded = ctx
                        .conn_auth
                        .as_ref()
                        .and_then(|a| a.lock().ok())
                        .and_then(|g| g.client_metadata.clone());
                    let app = failpoints::failpoint_app_name(name, doc, recorded.as_ref());
                    r.match_command(name, app.as_deref())
                })
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
                    // A code whose error carries structured extra info cannot be
                    // injected bare: mongod refuses with 40671 rather than send
                    // an error missing the fields a driver would read from it.
                    if crate::mongod_codes::NEEDS_EXTRA_INFO.contains(&code) {
                        return CommandError::new(
                            40671,
                            "Location40671",
                            format!(
                                "Missing required extra info for error code {}",
                                failpoints::fail_code_name(code)
                            ),
                        )
                        .into_reply();
                    }
                    // And on these mongod drops the connection instead of replying.
                    if crate::mongod_codes::CLOSES_CONNECTION.contains(&code) {
                        ctx.close_connection = true;
                        return doc! { "ok": 1.0 };
                    }
                    let mut reply = CommandError::new(
                        code,
                        failpoints::fail_code_name(code),
                        // mongod 8.2.11's wording, measured 2026-09-30; libmongoc's
                        // `/crud/prose_test_9` asserts on it ("due to" failed it).
                        "Failing command via 'failCommand' failpoint",
                    )
                    .into_reply();
                    let labels = failpoint_error_labels(name, doc, code, m);
                    if !labels.is_empty() {
                        reply.insert(
                            "errorLabels",
                            labels.into_iter().map(Bson::String).collect::<Vec<_>>(),
                        );
                    }
                    return reply;
                }
            }
            if max_time_forced_to_expire(name, doc, ctx) {
                return CommandError::new(50, "MaxTimeMSExpired", "operation exceeded time limit")
                    .into_reply();
            }
            // Malformed writeConcern is rejected before a write command runs
            // (mirrors commands.py, which prepends _validate_write_concern to each
            // write handler). Reads don't carry a writeConcern.
            if is_write_concern_command(name) {
                if let Some(e) = validate_write_concern(doc, name) {
                    return e.into_reply();
                }
                // A node inside its `replSetStepDown` window is a SECONDARY, and
                // mongod refuses writes there with 10107 while still serving
                // reads. Gated on the write-command set so a read is unaffected.
                if ctx.step_down.as_ref().is_some_and(|s| s.is_stepped_down()) {
                    return stepdown::not_primary_error(ctx.step_down.as_deref()).into_reply();
                }
            }
            // Refuse a direct insert/update/delete on a synthetic read-only view
            // (local.oplog.rs / admin.system.users), mirroring commands.py.
            if let Some(e) = reject_synthetic_view_write(name, doc, &ctx.db_name) {
                return e.into_reply();
            }
            // A time-series bucket namespace and a view answer here.
            // (A statement of a transaction is refused inside it, in
            // `session`, because the refusal aborts the transaction.)
            if doc.get("autocommit") != Some(&Bson::Boolean(false)) {
                // `system.buckets.<name>` of a time-series collection.
                if let Some(reply) = ts_bucket_ns::intercept(name, doc, ctx) {
                    return reply;
                }
                if let Some(reply) = views::refusal(name, doc, ctx) {
                    return reply;
                }
            }
            profiler::mark_reached(ctx);
            let mut reply = session::run(name, handler, doc, ctx);
            // A failpoint-configured writeConcernError attaches to a successful reply,
            // AND so do the failpoint's errorLabels. The labels are what make the
            // write retryable in the driver's eyes: without `RetryableWriteError`
            // libmongoc never retries, so the two commandStartedEvents that
            // /command_monitoring/unified/writeConcernError expects never happen.
            // Attaching the wce alone left that test red on this server even after
            // the replay fix (#1069) cleared it on the Python server -- caught by
            // running BOTH C lanes rather than assuming they moved together.
            if let Some(m) = &fp {
                if let Some(wce) = &m.write_concern_error {
                    if reply.get_f64("ok").unwrap_or(0.0) == 1.0
                        && !reply.contains_key("writeConcernError")
                    {
                        reply.insert("writeConcernError", Bson::Document(wce.clone()));
                        let supplied = m.error_labels.as_deref().unwrap_or(&[]);
                        if !supplied.is_empty() && !reply.contains_key("errorLabels") {
                            reply.insert(
                                "errorLabels",
                                Bson::Array(
                                    supplied.iter().map(|l| Bson::String(l.clone())).collect(),
                                ),
                            );
                        }
                    }
                }
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
        (Some(reg), Some(lsid), Some(n)) => {
            if let Some(s) = ctx.storage.as_ref() {
                s.take_last_commit_ts();
            }
            match reg.commit(&lsid, n) {
                None => {
                    let mut reply = doc! { "ok": 1.0 };
                    // A transaction that logged its writes leaves its
                    // session's `config.transactions` row, as mongod's does.
                    if let Some(s) = ctx.storage.as_ref() {
                        if let Some(ts) = s.take_last_commit_ts() {
                            session::pin_operation_time(
                                &mut reply,
                                ctx.replica_set_name.is_some(),
                                s.as_ref(),
                            );
                            let session =
                                session::logical_session_id(&lsid, &session::authenticated(ctx));
                            session::record_session_transaction(
                                s.as_ref(),
                                session,
                                n,
                                ts,
                                Some("committed"),
                            )?;
                        }
                    }
                    Ok(reply)
                }
                Some(reply) => Ok(reply),
            }
        }
        _ => Ok(doc! { "ok": 1.0 }),
    }
}

/// `endSessions` -- the listed sessions are no longer needed. mongod checks
/// the list and leaves an open transaction of theirs alone (measured 8.2.11:
/// a statement after `endSessions` still runs, and the commit succeeds); the
/// transaction ends when the client ends it or its lifetime runs out.
///
/// The logical session cache forgets them at once, and its next refresh
/// deletes their records from `config.system.sessions`
/// (`logical_sessions::refresh`).
fn end_sessions(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let ids = session::listed_sessions(
        doc,
        "endSessions",
        "endSessions",
        "endSessions.endSessionsFromClient",
        true,
    )?;
    if let Some(params) = &ctx.server_params {
        let uid = session::user_digest(&session::authenticated(ctx));
        params.forget_sessions(&ids);
        params.logical_sessions().end(ids, &uid);
    }
    Ok(doc! { "ok": 1.0 })
}

/// `refreshSessions` -- checked like `endSessions`. The listed sessions are
/// used now: the logical session cache's next refresh writes their
/// `lastUse` to `config.system.sessions`, as mongod's does.
fn refresh_sessions(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let ids = session::listed_sessions(
        doc,
        "refreshSessions",
        "refreshSessions",
        "refreshSessions.refreshSessionsFromClient",
        true,
    )?;
    if let Some(params) = &ctx.server_params {
        let uid = session::user_digest(&session::authenticated(ctx));
        for id in ids {
            params.note_session(&id, uid.clone());
        }
    }
    Ok(doc! { "ok": 1.0 })
}

/// `refreshLogicalSessionCacheNow` -- a test command (`enableTestCommands`):
/// run the logical session cache's periodic refresh now
/// (`logical_sessions::refresh`). mongod 8.2.11 takes any value, on any
/// database, and ignores other fields.
fn refresh_logical_session_cache_now(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    ctx.storage()?;
    if let (Some(storage), Some(params)) = (ctx.storage.clone(), ctx.server_params.clone()) {
        logical_sessions::refresh(storage.as_ref(), &params, ctx.cursors.as_deref())
            .map_err(crate::util::command_error)?;
    }
    Ok(doc! { "ok": 1.0 })
}

/// `reapLogicalSessionCacheNow` -- a test command: run the transaction
/// reaper now (`logical_sessions::reap`).
fn reap_logical_session_cache_now(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    ctx.storage()?;
    if let (Some(storage), Some(params)) = (ctx.storage.clone(), ctx.server_params.clone()) {
        logical_sessions::reap(storage.as_ref(), &params, ctx.transactions.as_deref())
            .map_err(crate::util::command_error)?;
    }
    Ok(doc! { "ok": 1.0 })
}

/// `killSessions` -- abort each listed session's open transaction and kill
/// the cursors it opened. An empty list is every session.
fn kill_sessions(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let sessions = session::listed_sessions(
        doc,
        "killSessions",
        "KillSessionsCmd",
        "KillSessionsCmd.killSessions",
        false,
    )?;
    if sessions.is_empty() {
        return kill_all_sessions(&doc! {"killAllSessions": []}, ctx);
    }
    // The sessions' running operations are interrupted first: a
    // transaction's statement holds its transaction until it ends, so the
    // abort below waits for it (RM-313).
    if let Some(ops) = &ctx.ops {
        ops.kill_sessions(|lsid, _| sessions.iter().any(|s| s == lsid));
    }
    for lsid in &sessions {
        if let Some(reg) = &ctx.transactions {
            reg.abort_for_session(lsid);
        }
        if let Some(cursors) = &ctx.cursors {
            cursors.kill_session(lsid);
        }
    }
    Ok(doc! { "ok": 1.0 })
}

/// One session a kill command names: its `lsid` (id and owner `uid`), its
/// owner `uid`, or both. A field left out matches anything, so an empty
/// pattern is every session.
#[derive(Default)]
struct KillPattern {
    lsid: Option<(Vec<u8>, Vec<u8>)>,
    uid: Option<Vec<u8>>,
}

impl KillPattern {
    fn matches(&self, lsid: &[u8], owner: &[u8]) -> bool {
        self.lsid
            .as_ref()
            .is_none_or(|(id, uid)| id == lsid && uid == owner)
            && self.uid.as_ref().is_none_or(|uid| uid == owner)
    }
}

/// `killAllSessions` / `killAllSessionsByPattern` — abort the transactions
/// and kill the cursors of the sessions named: by user for the first (an
/// empty user is the no-user digest), by `lsid` and `uid` for the second,
/// whose `users` and `roles` are whom to act AS and select nothing. An empty
/// list is every session. Measured 8.2.11, 2026-10-20 -- every pattern
/// aborted every transaction, and no cursor was killed. Driver test runners
/// send the empty form between tests to clear a leaked transaction.
fn kill_all_sessions(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = command_name(doc);
    let owner = if name == "killAllSessions" {
        "KillAllSessionsCmd"
    } else {
        "KillAllSessionsByPatternCmd"
    };
    let path = format!("{owner}.{name}");
    let wrong = |path: &str, value: &Bson, expected: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
                secantus_core::query::bson_type_name(value)
            ),
        )
    };
    let missing = |path: &str| {
        CommandError::new(
            40414,
            "IDLFailedToParse",
            format!("BSON field '{path}' is missing but a required field"),
        )
    };
    let patterns = match doc.get(name) {
        Some(Bson::Array(patterns)) => patterns.as_slice(),
        None => &[],
        Some(Bson::Null) => return Err(missing(&path)),
        Some(other) => return Err(wrong(&path, other, "array")),
    };
    // Each entry is parsed whether or not any session matches it (mongod
    // 8.2.11): a user is `{user, db}`, both strings, and a pattern has only
    // the fields mongod's parser knows.
    let mut selected: Vec<KillPattern> = Vec::new();
    for (i, pattern) in patterns.iter().enumerate() {
        let Bson::Document(pattern) = pattern else {
            return Err(wrong(&format!("{path}.{i}"), pattern, "object"));
        };
        if name == "killAllSessions" {
            for (key, value) in pattern {
                if matches!(key.as_str(), "user" | "db") && !matches!(value, Bson::String(_)) {
                    return Err(wrong(&format!("{path}.{key}"), value, "string"));
                }
            }
            for required in ["db", "user"] {
                if !pattern.contains_key(required) {
                    return Err(missing(&format!("{path}.{required}")));
                }
            }
            let user = pattern.get_str("user").unwrap_or_default();
            let db = pattern.get_str("db").unwrap_or_default();
            let named = if user.is_empty() {
                Vec::new()
            } else {
                vec![(db.to_string(), user.to_string())]
            };
            selected.push(KillPattern {
                lsid: None,
                uid: Some(session::user_digest(&named)),
            });
        } else {
            if let Some(key) = pattern
                .keys()
                .find(|k| !matches!(k.as_str(), "lsid" | "uid" | "users" | "roles"))
            {
                return Err(CommandError::new(
                    40415,
                    "IDLUnknownField",
                    format!("BSON field '{path}.{key}' is an unknown field."),
                ));
            }
            let mut chosen = KillPattern::default();
            match pattern.get("lsid") {
                None | Some(Bson::Null) => {}
                Some(Bson::Document(lsid)) => {
                    let id = session::session_id(lsid, &format!("{path}.lsid"))?;
                    let Some(Bson::Binary(uid)) = lsid.get("uid") else {
                        return Err(missing(&format!("{path}.lsid.uid")));
                    };
                    chosen.lsid = Some((id, uid.bytes.clone()));
                }
                Some(other) => return Err(wrong(&format!("{path}.lsid"), other, "object")),
            }
            match pattern.get("uid") {
                None | Some(Bson::Null) => {}
                Some(Bson::Binary(uid)) => chosen.uid = Some(uid.bytes.clone()),
                Some(other) => return Err(wrong(&format!("{path}.uid"), other, "binData")),
            }
            for (field, fields) in [("users", ["user", "db"]), ("roles", ["role", "db"])] {
                let entries = match pattern.get(field) {
                    None | Some(Bson::Null) => continue,
                    Some(Bson::Array(entries)) => entries,
                    Some(other) => return Err(wrong(&format!("{path}.{field}"), other, "array")),
                };
                // A misshapen entry is named by index; a field inside one is
                // not (mongod's own paths, measured 8.2.11).
                for (j, entry) in entries.iter().enumerate() {
                    let Bson::Document(entry) = entry else {
                        return Err(wrong(&format!("{path}.{field}.{j}"), entry, "object"));
                    };
                    for key in fields {
                        match entry.get(key) {
                            Some(Bson::String(_)) => {}
                            None | Some(Bson::Null) => {
                                return Err(missing(&format!("{path}.{field}.{key}")))
                            }
                            Some(other) => {
                                return Err(wrong(
                                    &format!("{path}.{field}.{key}"),
                                    other,
                                    "string",
                                ))
                            }
                        }
                    }
                }
            }
            selected.push(chosen);
        }
    }
    if patterns.is_empty() {
        selected.push(KillPattern::default());
    }
    let picks = |lsid: &[u8], owner: &[u8]| selected.iter().any(|p| p.matches(lsid, owner));
    // Their running operations first, as `killSessions` (RM-313).
    if let Some(ops) = &ctx.ops {
        ops.kill_sessions(picks);
    }
    if let Some(reg) = &ctx.transactions {
        reg.abort_matching(&session::user_digest(&[]), picks);
    }
    if let Some(cursors) = &ctx.cursors {
        cursors.kill_matching(picks);
    }
    Ok(doc! { "ok": 1.0 })
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

/// Run `handler`, mapping its `Err` into the standard error reply.
///
/// `maxTimeMS` is armed HERE, around the whole handler, because that is the
/// span mongod bounds: the operation, not any one loop inside it. Until
/// 2026-09-28 the budget was parsed and validated exactly as mongod validates
/// it and then never checked — a `createIndexes` over 100,000 documents with
/// `maxTimeMS: 1` answered `ok` where mongod answers code 50.
fn run_handler(handler: Handler, doc: &Document, ctx: &mut CommandContext) -> Document {
    let budget = match max_time_ms_budget(doc) {
        0 => cluster_params::default_read_budget(doc, ctx),
        b => b,
    };
    let _deadline = deadline::arm((budget > 0).then_some(budget));
    let _write_block_bypass = misc_admin::write_block_bypass(ctx);
    // Expressions are parsed under the feature compatibility version.
    let _fcv = secantus_core::expressions::gate_fcv(
        ctx.server_params.is_some() && cluster_admin::current_fcv(ctx) == "8.0",
    );
    // The statement's lock chain (mongod's hierarchy). A transaction
    // statement's chain was already taken with the transaction's owner,
    // which holds its collection locks to commit/abort — see `session`.
    // A command that ends or is refused by an index build in progress meets
    // it before it waits for its locks (RM-187).
    if !ctx.txn_locks {
        if let Some(reply) = index_builds::before_locks(doc, ctx) {
            return reply;
        }
    }
    let _locks = if ctx.txn_locks {
        None
    } else {
        let facts = lock_manager::StorageFacts {
            storage: ctx.storage.as_ref(),
            db: &ctx.db_name,
        };
        match lock_manager::Guard::statement(ctx.locks.as_ref(), doc, &ctx.db_name, &facts) {
            Some(Ok(guard)) => Some(guard),
            Some(Err(e)) => return e.into_reply(),
            None => None,
        }
    };
    let _current = lock_manager::enter(ctx.locks.as_ref());
    // The command's write statements log themselves when slow, as mongod's do.
    let _slow = slowop::enter(ctx);
    let (result, read_at) = snapshot::run(doc, command_name(doc), ctx, |ctx| handler(doc, ctx));
    match result {
        Ok(mut reply) => {
            // A snapshot read reports the time it read at (a getMore's
            // cursor was opened at it, and says nothing).
            if let Some(at) = read_at.filter(|_| command_name(doc) != "getMore") {
                snapshot::place_at_cluster_time(&mut reply, at);
            }
            // A statement interrupted at a poll is lifted by `dispatch`,
            // after a retryable write has kept the statements before it
            // (`interrupt::finish`); this is for a budget spent elsewhere.
            if budget > 0 && !deadline::tripped() {
                if let Some(e) = expired_write_statement(doc, &reply) {
                    return e.into_reply();
                }
                mark_time_limited_cursor(&reply, ctx);
            }
            reply
        }
        Err(mut e) => {
            if budget > 0 && e.code == deadline::MaxTimeMsExpired::CODE {
                if let Some(prefix) = executor_error_prefix(doc, &ctx.db_name) {
                    e.errmsg = format!("{prefix} :: caused by :: {}", e.errmsg);
                }
            }
            e.into_reply()
        }
    }
}

/// mongod's wrapper for a budget spent while a READ command's plan executor
/// was running. Measured on 8.2.11 (2026-09-30) at 5ms and 20ms budgets over
/// 100,000 documents: `find`, `aggregate`, `distinct` and `count` wrap it, every
/// time; `findAndModify`, `update`, `delete` and `createIndexes` send it bare.
/// mongod sends the bare message for these four too when the budget is gone
/// before execution starts, which on this server is only the
/// `maxTimeAlwaysTimeOut` failpoint, answered before the handler runs.
fn executor_error_prefix(doc: &Document, db: &str) -> Option<String> {
    let (name, target) = doc.iter().next()?;
    let ns = match target {
        Bson::String(coll) => format!("{db}.{coll}"),
        _ => format!("{db}.$cmd.{name}"),
    };
    match name.as_str() {
        "find" => Some(format!("Executor error during find command: {ns}")),
        "aggregate" | "distinct" | "count" => Some(format!(
            "Executor error during {name} command on namespace: {ns}"
        )),
        _ => None,
    }
}

/// A write command whose statement ran out of budget. mongod fails the whole
/// COMMAND with code 50 (`ok: 0`); the write path reports storage faults as a
/// per-statement `writeErrors` entry under `ok: 1`, which is the right shape
/// for a duplicate key and the wrong one for an interrupted operation.
fn expired_write_statement(doc: &Document, reply: &Document) -> Option<CommandError> {
    if !matches!(
        doc.keys().next().map(String::as_str),
        Some("update" | "delete" | "insert")
    ) {
        return None;
    }
    let expired = reply.get_array("writeErrors").ok()?.iter().any(|we| {
        we.as_document()
            .and_then(|d| d.get("code"))
            .and_then(util::as_i64)
            == Some(i64::from(deadline::MaxTimeMsExpired::CODE))
    });
    expired.then(CommandError::max_time_expired)
}

/// The command's `maxTimeMS` as a time limit, or 0 for none. A `getMore`'s own
/// `maxTimeMS` is the awaitData wait, not a limit (`commands.py`'s
/// `_MAX_TIME_MS_NOT_A_DEADLINE`).
fn max_time_ms_budget(doc: &Document) -> i64 {
    if doc.keys().next().map(String::as_str) == Some("getMore") {
        return 0;
    }
    doc.get("maxTimeMS")
        .and_then(util::as_i64)
        .unwrap_or(0)
        .max(0)
}

/// Whether the `maxTimeAlwaysTimeOut` failpoint expires this operation. Mirrors
/// `commands.py::_max_time_forced_to_expire`: an operation with a time limit
/// fails at its first interrupt check; so does a `getMore` on a cursor whose
/// originating `find` / `aggregate` had one, because mongod bounds a
/// non-tailable cursor's getMores by it.
fn max_time_forced_to_expire(name: &str, doc: &Document, ctx: &CommandContext) -> bool {
    let Some(fp) = ctx.failpoints.as_ref() else {
        return false;
    };
    if !fp.max_time_always_armed() {
        return false;
    }
    if max_time_ms_budget(doc) > 0 {
        return fp.consume_max_time_always();
    }
    if name == "getMore" {
        if let (Some(id), Some(cursors)) = (doc.get("getMore").and_then(util::as_i64), &ctx.cursors)
        {
            if cursors.is_time_limited(id) {
                return fp.consume_max_time_always();
            }
        }
    }
    false
}

/// Remember that the cursor a reply opened carries a `maxTimeMS` (read only by
/// `max_time_forced_to_expire`).
fn mark_time_limited_cursor(reply: &Document, ctx: &CommandContext) {
    let id = reply
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get("id"))
        .and_then(util::as_i64)
        .unwrap_or(0);
    if id != 0 {
        if let Some(cursors) = &ctx.cursors {
            cursors.mark_time_limited(id);
        }
    }
}

/// The reply to a write on a capped collection inside a multi-document
/// transaction, which mongod refuses and which aborts the transaction; `None`
/// for anything else. Shapes measured on 8.2.11: `insert` / `update` /
/// `delete` answer `ok: 1` with ONE write error at index 0 whatever the batch
/// holds (263, or 20 for a delete), and `findAndModify` fails outright.
fn capped_write_in_txn(name: &str, doc: &Document, ctx: &CommandContext) -> Option<Document> {
    if !matches!(
        name,
        "insert" | "update" | "delete" | "findAndModify" | "findandmodify"
    ) {
        return None;
    }
    let coll = doc.get_str(name).ok()?;
    let capped = ctx
        .storage
        .as_ref()?
        .collection_is_capped(&ctx.db_name, coll)
        .unwrap_or(false);
    if !capped {
        return None;
    }
    let ns = format!("{}.{coll}", ctx.db_name);
    let refused = format!(
        "Collection '{ns}' is a capped collection. Writes in transactions are not allowed on \
         capped collections."
    );
    let write_error = |code: i32, errmsg: String| {
        let mut reply = doc! { "n": 0_i32 };
        if name == "update" {
            reply.insert("nModified", 0_i32);
        }
        reply.insert(
            "writeErrors",
            vec![doc! { "index": 0_i32, "code": code, "errmsg": errmsg }],
        );
        reply.insert("ok", 1.0);
        reply
    };
    Some(match name {
        "insert" | "update" => write_error(263, refused),
        "delete" => write_error(
            20,
            format!("Cannot remove from a capped collection in a multi-document transaction: {ns}"),
        ),
        _ => CommandError::new(263, "OperationNotSupportedInTransaction", refused).into_reply(),
    })
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

/// The `errorLabels` mongod puts on a `failCommand`-injected error.
///
/// A supplied `errorLabels` -- including an explicit `[]` -- is authoritative:
/// mongod answers with exactly that list and adds nothing (measured
/// 2026-09-28). Otherwise mongod computes them, and the rules below are what a
/// sweep of every code in 1..520 found on 8.2.11 (2026-09-30,
/// `tools/probes/error_labels.py`); the code sets live in `failpoints`:
///
/// * 280 / 286 -> `NonResumableChangeStreamError`, on ANY command;
/// * the aggregate that OPENS a change stream -> `ResumableChangeStreamError`
///   for the resumable set (a `getMore` gets nothing from `failCommand`; its
///   label comes from `failGetMoreAfterCursorCheckout`, `server_injected`);
/// * a statement inside a transaction (`autocommit: false`) ->
///   `TransientTransactionError`; a `commitTransaction` / `abortTransaction`
///   -> `RetryableWriteError` for the retryable-write codes and
///   `TransientTransactionError` for the other transient ones;
/// * a retryable write (a write carrying `txnNumber`, not in a transaction)
///   -> `RetryableWriteError`;
/// * then `SystemOverloadedError`, AFTER any of the above, for its codes.
///
/// Every one of these was missing except the transaction label, so a driver
/// saw a bare code where mongod tells it to retry or resume -- and a
/// transaction label's `autocommit: false` path is what all the drivers'
/// transaction error-label specs exercise, which is why that one existed.
fn failpoint_error_labels(
    name: &str,
    doc: &Document,
    code: i32,
    m: &failpoints::FailPointMatch,
) -> Vec<String> {
    if let Some(supplied) = &m.error_labels {
        return supplied.clone();
    }
    let mut labels: Vec<&str> = Vec::new();
    if failpoints::NON_RESUMABLE_CHANGE_STREAM_CODES.contains(&code) {
        labels.push("NonResumableChangeStreamError");
    } else if m.server_injected {
        if failpoints::is_resumable_change_stream_code(code) {
            labels.push("ResumableChangeStreamError");
        }
    } else if name == "aggregate" && opens_change_stream(doc) {
        if failpoints::CHANGE_STREAM_OPEN_RESUMABLE_CODES.contains(&code) {
            labels.push("ResumableChangeStreamError");
        }
    } else if doc.get("autocommit") == Some(&Bson::Boolean(false)) {
        let ending = name == "commitTransaction" || name == "abortTransaction";
        if ending && failpoints::is_retryable_write_code(code) {
            labels.push(transactions::RETRYABLE_WRITE_LABEL);
        } else if failpoints::is_transient_txn_code(code) {
            labels.push(transactions::TRANSIENT_LABEL);
        }
    } else if doc.contains_key("txnNumber")
        && is_retryable_write_command(name)
        && failpoints::is_retryable_write_code(code)
    {
        labels.push(transactions::RETRYABLE_WRITE_LABEL);
    }
    if failpoints::SYSTEM_OVERLOADED_CODES.contains(&code) {
        labels.push("SystemOverloadedError");
    }
    labels.into_iter().map(String::from).collect()
}

/// Whether an `aggregate` opens a change stream (`$changeStream` first).
fn opens_change_stream(doc: &Document) -> bool {
    matches!(
        doc.get_array("pipeline").ok().and_then(|p| p.first()),
        Some(Bson::Document(stage)) if stage.contains_key("$changeStream")
    )
}

/// The commands a driver retries as retryable writes.
fn is_retryable_write_command(name: &str) -> bool {
    matches!(
        name,
        "insert" | "update" | "delete" | "findAndModify" | "findandmodify" | "bulkWrite"
    )
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
        if failpoints::is_transient_txn_code(code) {
            transactions::add_label_first(result, transactions::TRANSIENT_LABEL);
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
/// mongod's field order for an insert / update / delete reply, standalone and
/// replica set alike (measured 8.2.11, 2026-10-05): `n`, then `electionId` /
/// `opTime` (replica set only), then `upserted` or `writeErrors`, then
/// `nModified`, then `ok` and whatever follows it. This server put `ok` before
/// `writeErrors`. Drivers read by name; this is for a byte-faithful reply.
fn order_write_reply(req: &Document, reply: &mut Document) {
    let Some(cmd) = req.keys().next() else { return };
    if !matches!(cmd.as_str(), "insert" | "update" | "delete") || !reply.contains_key("n") {
        return;
    }
    const LEADING: [&str; 7] = [
        "n",
        "electionId",
        "opTime",
        "writeErrors",
        "retriedStmtIds",
        "upserted",
        "nModified",
    ];
    let mut rest = std::mem::take(reply);
    let mut out = Document::new();
    for key in LEADING {
        if let Some(v) = rest.remove(key) {
            out.insert(key, v);
        }
    }
    for (k, v) in rest {
        out.insert(k, v);
    }
    *reply = out;
}

/// On a replica set, mongod's insert / update / delete reply carries
/// `electionId` and `opTime: {ts, t}` straight after `n` -- on every such
/// reply, one that wrote nothing or carries `writeErrors` included, but not on
/// a command error and not on `findAndModify` (measured 8.2.11, 2026-10-05).
/// `ts` is the reply's `operationTime`; `t` is the term, an int64, and
/// `electionId` the one `hello` reports.
fn attach_write_op_time(req: &Document, reply: &mut Document) {
    let Some(cmd) = req.keys().next() else { return };
    if !matches!(cmd.as_str(), "insert" | "update" | "delete") {
        return;
    }
    // An unacknowledged write (`w: 0`) that is answered at all is answered
    // without them.
    let unacknowledged = req
        .get_document("writeConcern")
        .ok()
        .and_then(|wc| wc.get("w"))
        .and_then(util::as_i64)
        == Some(0);
    if unacknowledged {
        return;
    }
    let ok = matches!(reply.get("ok"), Some(Bson::Double(v)) if *v == 1.0)
        || matches!(reply.get("ok"), Some(Bson::Int32(1)));
    if !ok || !reply.contains_key("n") || reply.contains_key("opTime") {
        return;
    }
    let Some(Bson::Timestamp(ts)) = reply.get("operationTime").cloned() else {
        return;
    };
    let election = bson::oid::ObjectId::parse_str("7fffffff0000000000000001")
        .expect("static electionId hex is valid");
    let mut out = Document::new();
    for (k, v) in std::mem::take(reply) {
        let is_n = k == "n";
        out.insert(k, v);
        if is_n {
            out.insert("electionId", election);
            out.insert("opTime", doc! {"ts": Bson::Timestamp(ts), "t": 1_i64});
        }
    }
    *reply = out;
}

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
    // A handler may have pinned the time of its own write, before writes it
    // keeps beside the oplog moved the clock on (`session::pin_operation_time`);
    // either way it follows `$clusterTime`, as mongod's does.
    let operation_time = reply.remove("operationTime").unwrap_or(Bson::Timestamp(ts));
    reply.insert("operationTime", operation_time);
    attach_write_op_time(req, reply);
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
        == Some("snapshot")
        // A transaction's statements read at its snapshot and report no time.
        && req.get("autocommit") != Some(&Bson::Boolean(false))
        // `explain` reads nothing a client could read again at that time.
        && command_name(req) != "explain";
    let ok = matches!(reply.get("ok"), Some(Bson::Double(d)) if *d != 0.0)
        || matches!(reply.get("ok"), Some(Bson::Int32(n)) if *n != 0)
        || matches!(reply.get("ok"), Some(Bson::Int64(n)) if *n != 0);
    // A store that keeps history read at a timestamp, and `run_handler`
    // reported it (or, as mongod does, chose not to); one that keeps none
    // read the latest data, which is as of now.
    let timestamped = ctx
        .storage
        .as_ref()
        .is_some_and(|s| s.snapshot_clock().is_some());
    if is_snapshot && ok && !timestamped {
        snapshot::place_at_cluster_time(reply, ts);
    }
}

/// mongod's LOCALHOST EXCEPTION: while no user exists, a connection from a
/// loopback address may create the first one, on `admin`. Without it a fresh
/// `--auth` server could never be given a user -- every `createUser` answered
/// 13, where mongod 8.2.11 accepts it (measured 2026-09-30; a second
/// `createUser`, once a user exists, is 13 there too).
fn localhost_exception(name: &str, ctx: &CommandContext) -> bool {
    if name != "createUser" || ctx.db_name != "admin" {
        return false;
    }
    let loopback = ctx
        .conn_auth
        .as_ref()
        .is_some_and(|a| a.lock().unwrap_or_else(|e| e.into_inner()).peer_loopback);
    if !loopback || ctx.server_params.as_ref().is_some_and(|p| p.users_seen()) {
        return false;
    }
    let none_stored = ctx
        .storage
        .as_deref()
        .and_then(|s| s.list_users(None, 0, 1).ok())
        .is_some_and(|users| users.is_empty());
    if !none_stored {
        if let Some(p) = &ctx.server_params {
            p.mark_users_seen();
        }
    }
    none_stored
}

/// `--auth` gating and the privilege check. A no-op when `require_auth` is
/// off. When on: any command outside [`is_pre_auth_command`] needs an
/// authenticated user (`Unauthorized`, 13, or the localhost exception), and
/// an authenticated user needs what mongod checks for the command
/// ([`authz::check`]), over the privileges of every role it holds
/// ([`rbac::effective`]).
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

    if !authenticated {
        if is_pre_auth_command(name) || localhost_exception(name, ctx) {
            return Ok(());
        }
        return Err(CommandError::new(
            13,
            "Unauthorized",
            // mongod's capitalisation (8.2.11).
            format!("Command {name} requires authentication"),
        ));
    }
    let az = rbac::effective(ctx)?;
    authz::check(name, doc, &ctx.db_name, ctx, &az)
}

/// Commands a connection may invoke before authenticating (`--auth` on): the
/// driver handshake plus the SCRAM round-trip itself.
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

/// `readConcern.level` validation (`commands.py::dispatch`): an invalid level is
/// `FailedToParse` (9); `snapshot` is `SnapshotUnavailable` (246) on standalone.
fn validate_read_concern(
    doc: &Document,
    name: &str,
    ctx: &CommandContext,
) -> Result<(), CommandError> {
    let rc = match doc.get("readConcern") {
        Some(Bson::Document(rc)) => rc,
        None | Some(Bson::Null) => return Ok(()),
        // `find` and `aggregate` report it under their own names.
        Some(_) if matches!(name, "find" | "aggregate") || lookup(name).is_none() => return Ok(()),
        Some(other) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field '{name}.readConcern' is the wrong type '{}', expected type \
                     'object'",
                    secantus_core::query::bson_type_name(other)
                ),
            ))
        }
    };
    const FIELDS: [&str; 7] = [
        "level",
        "afterClusterTime",
        "afterOpTime",
        "atClusterTime",
        "provenance",
        "allowTransactionTableSnapshot",
        "waitLastStableRecoveryTimestamp",
    ];
    let level = match rc.get("level") {
        None | Some(Bson::Null) => None,
        Some(Bson::String(l)) => Some(l.as_str()),
        Some(other) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'readConcern.level' is the wrong type '{}', expected type \
                     'string'",
                    secantus_core::query::bson_type_name(other)
                ),
            ))
        }
    };
    if let Some(l) = level {
        if !VALID_READ_CONCERN_LEVELS.contains(&l) {
            return Err(CommandError::new(
                2,
                "BadValue",
                format!(
                    "Enumeration value '{l}' for field 'readConcern.level' is not a valid value."
                ),
            ));
        }
    }
    if let Some(key) = rc.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'readConcern.{key}' is an unknown field."),
        ));
    }
    // The two cluster times and the provenance (mongod 8.2.11). They were
    // read as absent when malformed, and a time this server has not reached
    // yet, or one too old to have a snapshot of, was served as now.
    for field in ["afterClusterTime", "atClusterTime"] {
        if let Some(v) = rc.get(field).filter(|v| !matches!(v, Bson::Timestamp(_))) {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "\"{field}\" had the wrong type. Expected 17, found {}",
                    secantus_core::query::bson_type_name(v)
                ),
            ));
        }
    }
    if let Some(Bson::String(source)) = rc.get("provenance") {
        const SOURCES: [&str; 5] = [
            "clientSupplied",
            "implicitDefault",
            "customDefault",
            "getLastErrorDefaults",
            "internalWriteDefault",
        ];
        if !SOURCES.contains(&source.as_str()) {
            return Err(CommandError::new(
                2,
                "BadValue",
                format!(
                    "Enumeration value '{source}' for field 'readConcern.provenance' is not a \
                     valid value."
                ),
            ));
        }
    }
    let invalid = |message: String| CommandError::new(72, "InvalidOptions", message);
    if let Some(Bson::Timestamp(after)) = rc.get("afterClusterTime") {
        if after.time == 0 && after.increment == 0 {
            return Err(invalid(
                "afterClusterTime cannot be a null timestamp".into(),
            ));
        }
        if let Some(now) = ctx.storage.as_ref().map(|s| s.current_cluster_time()) {
            if (after.time, after.increment) > (now.time, now.increment) {
                return Err(invalid(format!(
                    "readConcern afterClusterTime value must not be greater than the current \
                     clusterTime. Requested clusterTime: {{ ts: Timestamp({}, {}) }}; current \
                     clusterTime: {{ ts: Timestamp({}, {}) }}",
                    after.time, after.increment, now.time, now.increment
                )));
            }
        }
    }
    if let Some(Bson::Timestamp(_)) = rc.get("atClusterTime") {
        if level != Some("snapshot") {
            return Err(invalid(
                "atClusterTime field can be set only if level is equal to snapshot".into(),
            ));
        }
    }
    // A statement of a transaction reads at the transaction's own snapshot,
    // and its read concern is checked with the transaction (`session`).
    if matches!(doc.get("autocommit"), Some(Bson::Boolean(false))) {
        return Ok(());
    }
    let Some(level) = level.filter(|l| *l != "local") else {
        return Ok(());
    };
    if level == "snapshot" && ctx.replica_set_name.is_none() {
        return Err(CommandError::new(
            246,
            "SnapshotUnavailable",
            "Snapshot read concern is not supported on standalone",
        ));
    }
    // Only the read commands take a read concern above `local`, and `count`
    // takes no snapshot (measured 8.2.11).
    let reads = matches!(
        name,
        "find"
            | "aggregate"
            | "distinct"
            | "count"
            | "getMore"
            | "killCursors"
            | "explain"
            | "mapReduce"
            | "mapreduce"
    );
    // mongod keeps the history a snapshot needs for five minutes
    // (`minSnapshotHistoryWindowInSeconds`); this server keeps none older,
    // so a time before that has no snapshot to read. Said last: a command
    // that takes no snapshot at all says that first.
    let too_old = || snapshot::check_at(rc, ctx);
    let reason = if !reads {
        // An unknown command is CommandNotFound, whatever it carries.
        if lookup(name).is_none() {
            return Ok(());
        }
        "read concern not supported"
    } else if level == "snapshot" && name == "count" {
        "read concern snapshot not supported"
    } else {
        return too_old();
    };
    let at = match rc.get("atClusterTime") {
        Some(Bson::Timestamp(t)) => {
            format!("atClusterTime: Timestamp({}, {}), ", t.time, t.increment)
        }
        _ => String::new(),
    };
    Err(CommandError::new(
        72,
        "InvalidOptions",
        format!(
            "Command {name} does not support {{ readConcern: {{ level: \"{level}\", {at}provenance: \
             \"clientSupplied\" }} }} :: caused by :: {reason}"
        ),
    ))
}

/// The commands mongod refuses a `writeConcern` on: every one of these
/// answered `72 Command does not support writeConcern` on 8.2.11
/// (2026-10-12), and this server ran them. A command not listed was not
/// measured, and is left alone.
const NO_WRITE_CONCERN: &[&str] = &[
    "find",
    "count",
    "distinct",
    "listCollections",
    "listIndexes",
    "ping",
    "hello",
    "isMaster",
    "ismaster",
    "buildInfo",
    "getMore",
    "killCursors",
    "listDatabases",
    "dbStats",
    "collStats",
    "serverStatus",
    "getParameter",
    "connectionStatus",
    "whatsmyuri",
    "endSessions",
    "validate",
    "dataSize",
    "currentOp",
    "hostInfo",
    "getCmdLineOpts",
    "getLog",
    "startSession",
    "refreshSessions",
    "killSessions",
    "killAllSessions",
    "replSetGetStatus",
    "usersInfo",
    "rolesInfo",
    "top",
    "profile",
    "killOp",
    "fsync",
    "compact",
    "autoCompact",
    "reIndex",
    "mapReduce",
    // Measured 2026-10-14 (`tools/probes/admin_commands.py`).
    "logRotate",
    "connPoolStats",
    "dbHash",
    "dbhash",
    "getDefaultRWConcern",
    // Measured 2026-10-20.
    "setClusterParameter",
];

fn refuse_write_concern(doc: &Document, name: &str) -> Result<(), CommandError> {
    if NO_WRITE_CONCERN.contains(&name) && doc.contains_key("writeConcern") {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "Command does not support writeConcern",
        ));
    }
    Ok(())
}

/// `apiVersion` / `apiStrict` validation (`commands.py::dispatch`). An
/// unsupported `apiVersion` is `APIVersionError` (322); under `apiStrict: true`,
/// a command outside API Version 1 is `APIStrictError` (323). The `apiStrict`
/// aggregation-stage gate lands with the aggregate family.
fn validate_api(doc: &Document, name: &str) -> Result<(), CommandError> {
    if let Some(av) = doc.get("apiVersion").filter(|v| !matches!(v, Bson::Null)) {
        if av.as_str() != Some("1") {
            // mongod's two wordings (measured 8.2.11, 2026-10-12): version
            // "2" is named, and anything else is told what is accepted.
            let message = if av.as_str() == Some("2") {
                "Cannot accept API version 2"
            } else {
                "API version must be \"1\""
            };
            return Err(CommandError::new(322, "APIVersionError", message));
        }
    }
    if doc.get("apiStrict").and_then(Bson::as_bool) == Some(true) {
        // Command-name gate. Measured against mongod 8.2.11 (2026-09-27), which
        // has THREE branches, not two:
        //
        //   * a command it HAS, outside v1 (`testVersion2`, `distinct`) -> 323
        //   * a command in v1 (`find`, `count`)                         -> runs
        //   * a command it does NOT have                                -> 59
        //
        // so an unrecognised name deliberately falls through to `lookup`'s
        // CommandNotFound below. This replaced a `name == "distinct"` canary
        // whose message ("Provided command ... is not in API Version 1") was
        // not mongod's either.
        // An older spelling of a command is refused by name, whatever the
        // command it stands for.
        let modern = generic::struct_name(name);
        if modern != name && name != "find" {
            return Err(CommandError::new(
                323,
                "APIStrictError",
                format!(
                    "Command invocation with name '{name}' is not allowed in 'apiStrict' \
                     mode, use '{modern}' instead"
                ),
            ));
        }
        if !api_v1_command(name) && (lookup(name).is_some() || non_v1_mongod_command(name)) {
            return Err(CommandError::new(
                323,
                "APIStrictError",
                format!(
                    "Provided apiStrict:true, but the command {name} is not in API \
                     Version 1. Information on supported commands and migrations in \
                     API Version 1 can be found at \
                     https://dochub.mongodb.org/core/manual-versioned-api."
                ),
            ));
        }
        // The fields of a v1 command that are not themselves in v1, each
        // sent alone to mongod 8.2.11.
        let outside: &[&str] = match name {
            "find" => &["tailable", "awaitData", "min", "max", "oplogReplay"],
            "aggregate" => &["explain"],
            _ => &[],
        };
        if let Some(field) = doc.keys().find(|k| outside.contains(&k.as_str())) {
            return Err(CommandError::new(
                323,
                "APIStrictError",
                format!("BSON field '{field}' is not allowed with apiStrict:true."),
            ));
        }
        // An aggregate whose pipeline uses a stage outside API Version 1 (e.g.
        // `$listLocalSessions`) is rejected — drivers probe with exactly that to
        // land an APIStrictError from inside an allowed command.
        if name == "aggregate" {
            if let Some(Bson::Array(pipeline)) = doc.get("pipeline") {
                if let Some(s) = first_non_v1_stage(pipeline) {
                    return Err(api_strict_stage_error(&s));
                }
            }
        }
    }
    Ok(())
}

/// Whether `name` is in MongoDB's **Stable API Version 1**: mongod's own
/// answer, from the `apiVersions` of its `listCommands` (8.2.11).
///
/// A list kept by hand stood here, measured by sending each command under
/// `apiStrict: true` and recording which were refused. Ten of its names were
/// wrong, because a command whose arguments did not parse answered the parse
/// error first and was counted as accepted (`createUser`, `grantRolesToUser`
/// and the like), and `collStats` was simply mistaken. An older spelling
/// (`findandmodify`) is not in the table and gets its own refusal below.
fn api_v1_command(name: &str) -> bool {
    mongod_commands::api_v1(name)
}

/// Commands a real `mongod` HAS but this server does not implement, which sit
/// outside API Version 1. Without these, the driver specs' `testVersion2` case
/// would answer 59 -- correct for a command nobody has, wrong for one mongod has
/// outside the API. Mirrors `commands.py::_NON_V1_MONGOD_COMMANDS`.
fn non_v1_mongod_command(name: &str) -> bool {
    matches!(name, "testVersion2" | "testDeprecationInVersion2")
        || mongod_commands::row(name).is_some()
}

/// Aggregation stages inside API Version 1 (`commands.py::_API_V1_AGG_STAGES`).
/// A pipeline stage outside this set under `apiStrict: true` is an
/// `APIStrictError`. Deliberately excludes `$listLocalSessions` / `$listSessions`
/// / `$currentOp` — the stages driver tests probe with.
/// mongod's refusal of a stage outside API Version 1 under `apiStrict`.
pub(crate) fn api_strict_stage_error(stage: &str) -> CommandError {
    CommandError::new(
        323,
        "APIStrictError",
        format!("{stage} is not allowed with 'apiStrict: true' in API Version 1"),
    )
}

/// The first stage outside API Version 1 in `pipeline`, looking inside the
/// sub-pipelines of `$facet`, `$lookup` and `$unionWith` as mongod does
/// (measured 8.2.11: each refuses a nested `$planCacheStats` with 323).
pub(crate) fn first_non_v1_stage(pipeline: &[Bson]) -> Option<String> {
    for stage in pipeline {
        let Some(d) = stage.as_document() else {
            continue;
        };
        let Some((name, spec)) = d.iter().next() else {
            continue;
        };
        if !api_v1_agg_stage(name) {
            return Some(name.clone());
        }
        let nested: Vec<&Vec<Bson>> = match (name.as_str(), spec) {
            ("$facet", Bson::Document(f)) => f.values().filter_map(Bson::as_array).collect(),
            ("$lookup" | "$unionWith", Bson::Document(l)) => {
                l.get_array("pipeline").ok().into_iter().collect()
            }
            _ => Vec::new(),
        };
        if let Some(found) = nested.into_iter().find_map(|p| first_non_v1_stage(p)) {
            return Some(found);
        }
    }
    None
}

/// Whether `stage` is in API Version 1 (measured 8.2.11 under `apiStrict`:
/// `$fill` and `$indexStats` are refused although they run otherwise).
pub(crate) fn api_v1_agg_stage(stage: &str) -> bool {
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
            | "$geoNear"
            | "$graphLookup"
            | "$group"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn injected(code: i32) -> failpoints::FailPointMatch {
        failpoints::FailPointMatch {
            error_code: Some(code),
            server_injected: false,
            error_labels: None,
            write_concern_error: None,
            close_connection: false,
            block_time_ms: 0,
        }
    }

    /// The label rules measured on mongod 8.2.11 (2026-09-30).
    #[test]
    fn failpoint_error_labels_follow_the_mongod_sweep() {
        let find = doc! {"find": "c"};
        let stream = doc! {"aggregate": "c", "pipeline": [{"$changeStream": {}}]};
        let retryable = doc! {"insert": "c", "txnNumber": 1_i64};
        let in_txn = doc! {"insert": "c", "txnNumber": 1_i64, "autocommit": false};
        let commit = doc! {"commitTransaction": 1, "txnNumber": 1_i64, "autocommit": false};
        let labels = |name: &str, doc: &Document, code: i32| {
            failpoint_error_labels(name, doc, code, &injected(code))
        };
        // 280 on anything, a plain find cursor included (php bug1419-001).
        assert_eq!(
            labels("find", &find, 280),
            ["NonResumableChangeStreamError"]
        );
        assert_eq!(
            labels("insert", &retryable, 286),
            ["NonResumableChangeStreamError"]
        );
        // Opening a change stream vs a plain read.
        assert_eq!(
            labels("aggregate", &stream, 6),
            ["ResumableChangeStreamError"]
        );
        assert!(labels("find", &find, 6).is_empty());
        // A retryable write, a transaction statement, a commit.
        assert_eq!(labels("insert", &retryable, 9001), ["RetryableWriteError"]);
        assert!(labels("insert", &doc! {"insert": "c"}, 9001).is_empty());
        assert_eq!(
            labels("insert", &in_txn, 150),
            ["TransientTransactionError"]
        );
        assert_eq!(
            labels("commitTransaction", &commit, 358),
            ["RetryableWriteError"]
        );
        assert!(labels("insert", &in_txn, 358).is_empty());
        assert_eq!(
            labels("commitTransaction", &commit, 112),
            ["TransientTransactionError"]
        );
        // SystemOverloadedError comes after the context label.
        assert_eq!(
            labels("insert", &retryable, 462),
            ["RetryableWriteError", "SystemOverloadedError"]
        );
        assert_eq!(labels("find", &find, 433), ["SystemOverloadedError"]);
        // A supplied list is authoritative, an explicit [] included.
        let mut m = injected(280);
        m.error_labels = Some(vec![]);
        assert!(failpoint_error_labels("find", &find, 280, &m).is_empty());
    }
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
    fn nul_in_namespace_rejects_interior_nul() {
        assert!(nul_in_namespace("collection name", "fine").is_none());
        let e = nul_in_namespace("collection name", "c\0x").expect("should reject");
        assert_eq!(
            e.into_reply().get_str("codeName").unwrap(),
            "InvalidNamespace"
        );
    }

    /// The adapter inlines 50 / "operation exceeded time limit" because it does
    /// not depend on secantus-core. Two copies of a constant is how they drift,
    /// so this pins them: if the core ever changes the code or the wording, this
    /// fails rather than the two halves quietly disagreeing over the wire.
    /// mongod 8.2.11 (2026-09-30), a budget spent mid-execution: the four read
    /// commands carry their executor prefix, everything else is bare.
    #[test]
    fn max_time_expiry_prefix_is_per_command() {
        let p = |d: Document| executor_error_prefix(&d, "db");
        assert_eq!(
            p(doc! {"find": "c"}).as_deref(),
            Some("Executor error during find command: db.c")
        );
        for cmd in ["aggregate", "distinct", "count"] {
            assert_eq!(
                p(doc! {cmd: "c"}),
                Some(format!(
                    "Executor error during {cmd} command on namespace: db.c"
                ))
            );
        }
        for cmd in ["findAndModify", "update", "delete", "createIndexes"] {
            assert_eq!(p(doc! {cmd: "c"}), None, "{cmd}");
        }
    }

    /// An interrupted write fails the COMMAND; a duplicate key does not.
    #[test]
    fn expired_write_statement_fails_the_command() {
        let reply =
            doc! {"n": 0, "writeErrors": [{"index": 0, "code": 50, "errmsg": "x"}], "ok": 1.0};
        let err = expired_write_statement(&doc! {"update": "c"}, &reply).expect("lifted");
        assert_eq!(
            (err.code, err.errmsg.as_str()),
            (50, "operation exceeded time limit")
        );
        let dup =
            doc! {"n": 0, "writeErrors": [{"index": 0, "code": 11000, "errmsg": "x"}], "ok": 1.0};
        assert!(expired_write_statement(&doc! {"insert": "c"}, &dup).is_none());
        assert!(expired_write_statement(&doc! {"find": "c"}, &reply).is_none());
    }

    #[test]
    fn max_time_expired_matches_core() {
        use secantus_core::deadline::MaxTimeMsExpired as E;
        assert_eq!(E::CODE, 50);
        assert_eq!(E::MESSAGE, "operation exceeded time limit");
        let err = CommandError::max_time_expired();
        assert_eq!(err.code, E::CODE);
        assert_eq!(err.code_name, E::CODE_NAME);
        assert_eq!(err.errmsg, E::MESSAGE);
        // and the name must render, not fall through to `Location50`
        let reply = err.into_reply();
        assert_eq!(reply.get_str("codeName").unwrap(), "MaxTimeMSExpired");
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
        // `ping` takes no write concern (72 on mongod), so the error is
        // attached here to the reply of a write that succeeded.
        let refused = dispatch(&doc! {"ping": 1, "writeConcern": {"w": 2}}, &mut ctx());
        assert_eq!(refused.get_i32("code").unwrap(), 72);
        let mut reply = doc! {"n": 1, "ok": 1.0};
        attach_write_concern_error(&doc! {"w": 2}, "clientSupplied", &mut reply);
        let wce = reply.get_document("writeConcernError").unwrap();
        assert_eq!(wce.get_i32("code").unwrap(), 100);
        assert_eq!(
            wce.get_str("codeName").unwrap(),
            "UnsatisfiableWriteConcern"
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
        // `hello` answers the modern name only, as mongod does.
        assert!(!reply.contains_key("ismaster"));
        assert_eq!(reply.get_i32("maxWireVersion").unwrap(), WIRE_VERSION);
        assert_eq!(
            reply.get_i32("maxBsonObjectSize").unwrap(),
            MAX_BSON_OBJECT_SIZE
        );
        // mongod's widths: an int32 connectionId, an int64 counter.
        assert!(matches!(reply.get("connectionId"), Some(Bson::Int32(7))));
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
        // For a user that does not exist mongod OMITS the field (8.2.11); a
        // stored user's own mechanisms are pinned in `auth::tests`.
        let reply = dispatch(
            &doc! {"hello": 1, "saslSupportedMechs": "admin.alice"},
            &mut ctx(),
        );
        assert!(reply.get("saslSupportedMechs").is_none());
        // A name with no `.` is refused.
        let bad = dispatch(
            &doc! {"hello": 1, "saslSupportedMechs": "alice"},
            &mut ctx(),
        );
        assert_eq!(bad.get_i32("code").unwrap(), 2);
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
        // The older names answer `ismaster` and not the modern flag.
        let reply2 = dispatch(&doc! {"ismaster": 1}, &mut ctx());
        assert!(reply2.get_bool("ismaster").unwrap());
        assert!(!reply2.contains_key("isWritablePrimary"));
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
    fn an_invalid_read_concern_is_refused_as_mongod_refuses_it() {
        for (rc, code, errmsg) in [
            (
                doc! {"level": "bogus"},
                2,
                "Enumeration value 'bogus' for field 'readConcern.level' is not a valid value.",
            ),
            (
                doc! {"level": 1},
                14,
                "BSON field 'readConcern.level' is the wrong type 'int', expected type 'string'",
            ),
            (
                doc! {"level": "local", "bogus": 1},
                40415,
                "BSON field 'readConcern.bogus' is an unknown field.",
            ),
        ] {
            let reply = dispatch(&doc! {"find": "c", "readConcern": rc}, &mut ctx());
            assert_eq!(reply.get_i32("code").unwrap(), code);
            assert_eq!(reply.get_str("errmsg").unwrap(), errmsg);
        }
        // A command that is not a read takes `local` and nothing above it.
        let reply = dispatch(
            &doc! {"ping": 1, "readConcern": {"level": "majority"}},
            &mut ctx(),
        );
        assert_eq!(reply.get_i32("code").unwrap(), 72);
        assert_eq!(
            reply.get_str("errmsg").unwrap(),
            "Command ping does not support { readConcern: { level: \"majority\", provenance: \
             \"clientSupplied\" } } :: caused by :: read concern not supported"
        );
        let reply = dispatch(
            &doc! {"ping": 1, "readConcern": {"level": "local"}},
            &mut ctx(),
        );
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
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

#[cfg(test)]
mod write_concern_w_tests {
    //! `writeConcern.w` as mongod parses it (measured 8.2.11 on a replica set,
    //! 2026-10-06).
    use super::validate_write_concern;
    use bson::{doc, Bson, Document};

    fn refusal(w: Bson) -> Option<(i32, String)> {
        let cmd = doc! {"insert": "c", "writeConcern": {"w": w}};
        validate_write_concern(&cmd, "insert").map(|e| (e.code, e.errmsg))
    }

    #[test]
    fn accepts_any_number_a_tag_a_tag_set_and_null() {
        for w in [
            Bson::Int32(1),
            Bson::Double(1.5),
            Bson::String("majority".into()),
            Bson::Document(doc! {"dc1": 1}),
            Bson::Null,
        ] {
            assert_eq!(refusal(w.clone()), None, "{w:?}");
        }
    }

    #[test]
    fn refuses_what_mongod_refuses() {
        assert_eq!(
            refusal(Bson::Boolean(true)),
            Some((
                9,
                "w has to be a number, string, or object; found: bool".into()
            ))
        );
        assert_eq!(
            refusal(Bson::Array(vec![])),
            Some((
                9,
                "w has to be a number, string, or object; found: array".into()
            ))
        );
        assert_eq!(
            refusal(Bson::Double(-1.5)),
            Some((
                9,
                "w has to be a non-negative number and not greater than 50; found: -1".into()
            ))
        );
        assert_eq!(
            refusal(Bson::Document(Document::new())),
            Some((9, "tagged write concern requires tags".into()))
        );
    }
}
