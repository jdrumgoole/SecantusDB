//! The SecantusDB PostgreSQL server -- P1 vertical slice.
//!
//! psql -> `pgwire` -> `secantus-pgplan` (libpg_query) -> MQL ->
//! `secantus-storage` (WiredTiger). No Python anywhere in that path, and no
//! fallback into it: a construct the planner cannot lower becomes a real
//! PostgreSQL SQLSTATE, never a wrong row.
//!
//! Scope is deliberately thin -- CREATE TABLE, INSERT, single-table SELECT --
//! because the point of P1 is to prove the SEAM end to end on real storage,
//! including the shared on-disk catalog format. Breadth is P5's problem.

mod do_block;
mod encoding;
mod plpgsql_do;

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32};
use std::sync::{Arc, Mutex, OnceLock};

use encoding::ClientEncoding;

use async_trait::async_trait;
use bson::{Bson, Document};
use bytes::{BufMut, Bytes, BytesMut};
use futures::{stream, Sink, SinkExt, StreamExt, TryStreamExt};
use pgwire::api::auth::{DefaultServerParameterProvider, StartupHandler};
use pgwire::api::copy::CopyHandler;
use pgwire::api::portal::{Format, Portal};
use pgwire::api::query::{send_describe_response, ExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::results::{CopyResponse, DescribePortalResponse, DescribeStatementResponse};
use pgwire::api::results::{DataRowEncoder, FieldFormat, FieldInfo, QueryResponse, Response, Tag};
use pgwire::api::stmt::{QueryParser, StoredStatement};
use pgwire::api::{ClientInfo, ClientPortalStore, PgWireServerHandlers, Type, DEFAULT_NAME};
use pgwire::api::{PidSecretKeyGenerator, RandomPidSecretKeyGenerator};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::copy::{CopyData, CopyDone, CopyFail};
use pgwire::messages::data::DataRow;
use pgwire::messages::extendedquery::{Describe, Parse, Sync as PgSync, TARGET_TYPE_BYTE_PORTAL};
use pgwire::messages::response::{CommandComplete, NotificationResponse, ReadyForQuery};
use pgwire::messages::simplequery::Query;
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use pgwire::types::format::FormatOptions;
use pgwire::types::ToSqlText;
use postgres_types::{to_sql_checked, IsNull, ToSql};
use secantus_pgcatalog::{TableDef, CATALOG_COLLECTION, SEQUENCE_COLLECTION};
use secantus_pgplan::{
    companion_field, render_array_element_text, render_timestamp, AggFunc, AggItem, ConstCol,
    Error as PlanError, Nulls, OrderKey, OutputCol, Statement, TransactionControl,
    TransactionModes,
};
use secantus_storage::{Storage, UserTransactionHandle};

/// One live backend as the OTHER backends -- and a `CancelRequest` -- see it.
///
/// `pg_terminate_backend(pid)` on another connection sets `terminate`; the
/// target notices at the top of its next statement and ends with a `57P01`,
/// exactly as a real backend torn down by an administrator would. A
/// `CancelRequest` carrying this backend's PID and secret sets `cancel`; the
/// running statement's cancellation points (`pg_sleep`, the row scan, the
/// COPY OUT stream) notice it and answer `57014`. `activity` is the row
/// `pg_stat_activity` shows for this backend.
pub struct BackendEntry {
    /// The `BackendKeyData` secret, set once startup has assigned it. A
    /// `CancelRequest` with the wrong secret is ignored, as PostgreSQL's is.
    secret: OnceLock<Bytes>,
    terminate: AtomicBool,
    cancel: AtomicBool,
    /// A result streamed AFTER its statement answered (COPY OUT) failed --
    /// a cancel mid-stream. The next statement sees it and poisons the block
    /// the way the failed statement itself would have.
    stream_failed: AtomicBool,
    activity: Mutex<BackendActivity>,
    /// The channels this backend LISTENs on -- read by every other backend's
    /// NOTIFY, which is why they live here and not on the handler.
    listening: Mutex<Vec<String>>,
    /// Notifications addressed to this backend and not yet sent: the
    /// `NotificationResponse`s it delivers before its next `ReadyForQuery`,
    /// or straight away when it is idle.
    inbox: Mutex<VecDeque<Notification>>,
    /// Wakes the connection's idle wait when the inbox fills or `terminate`
    /// is set, so an idle client hears without sending anything.
    wake: tokio::sync::Notify,
}

/// One queued `NotificationResponse`.
#[derive(Clone, Debug)]
struct Notification {
    pid: i32,
    channel: String,
    payload: String,
}

/// A LISTEN / UNLISTEN waiting for its transaction to commit.
#[derive(Clone, Debug)]
enum ListenOp {
    Listen(String),
    Unlisten(String),
    UnlistenAll,
}

/// What `pg_stat_activity` reports for one backend.
#[derive(Clone)]
struct BackendActivity {
    datname: String,
    usename: String,
    application_name: String,
    backend_start: bson::DateTime,
    query_start: Option<bson::DateTime>,
    state_change: Option<bson::DateTime>,
    /// `active`, `idle`, `idle in transaction`, `idle in transaction (aborted)`.
    state: &'static str,
    query: String,
}

impl BackendEntry {
    fn new(datname: &str) -> Self {
        Self {
            secret: OnceLock::new(),
            terminate: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            stream_failed: AtomicBool::new(false),
            listening: Mutex::new(Vec::new()),
            inbox: Mutex::new(VecDeque::new()),
            wake: tokio::sync::Notify::new(),
            activity: Mutex::new(BackendActivity {
                datname: datname.to_string(),
                usename: String::new(),
                application_name: String::new(),
                backend_start: bson::DateTime::now(),
                query_start: None,
                state_change: None,
                state: "idle",
                query: String::new(),
            }),
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Process-wide map of every live backend's PID to its entry. One handler
/// per connection registers here at startup and deregisters on drop, so a
/// stale PID is never signalled.
fn backend_registry() -> &'static Mutex<HashMap<i32, Arc<BackendEntry>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<i32, Arc<BackendEntry>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Process-wide cache of the COMMITTED rows of each type-catalog collection,
/// keyed by `(storage, db, collection)`. Every statement re-read and
/// re-decoded every catalog collection -- once per describe and once per
/// result column -- and that decode was the whole cost of a statement: a
/// `select 1` round trip took 0.86 ms against PostgreSQL 16's 0.034 ms, and
/// an `executemany` of 20,000 rows 15 s against 0.25 s, until psycopg's
/// `test_type_error_shadow` (which does exactly that) ran past its 20 s
/// budget (2026-09-09). The catalog changes only under DDL, so the decoded
/// rows are kept and re-read only when `CATALOG_VERSION` has moved.
///
/// Invalidation is deliberately COARSE: `bump_catalog_version` runs after
/// every statement that is not a plain read or a row write (see
/// `Statement::may_change_catalog`), after every transaction-control
/// statement (a COMMIT publishes a block's DDL to other connections), and
/// after every rollback to a savepoint. An extra bump costs one re-read; a
/// missed one is a stale catalog, so the classification errs on bumping.
///
/// A session fills the cache from its own read only when nothing can have
/// moved the catalog since its transaction's snapshot: the version now must
/// equal the one captured when its transaction handle opened (autocommit
/// statements in the extended protocol run under a handle too). Otherwise
/// the read is served but not kept -- a snapshot taken before another
/// connection's `CREATE TABLE` committed must not be published as current,
/// and a block's own uncommitted `CREATE TYPE` must not be seen by others.
/// A cache map keyed on `(storage, db, name)`, each value tagged with the
/// catalog version it was read under.
type VersionedMap<K, V> = Mutex<HashMap<(usize, String, K), (u64, V)>>;

struct CatalogCache {
    version: std::sync::atomic::AtomicU64,
    /// `(storage, db, collection)` -> the decoded rows of one type-catalog
    /// collection, `_id`-sorted.
    entries: VersionedMap<&'static str, Arc<Vec<Document>>>,
    /// `(storage, db, table)` -> its decoded catalog entry; `None` records
    /// that the table does not exist.
    tables: VersionedMap<String, Option<TableDef>>,
}

thread_local! {
    /// The `(storage, db, catalog version)` whose user types this thread's
    /// planner tables hold, or `None` when they hold something that must
    /// not be reused: a session's uncommitted overlay, or a read taken
    /// under a snapshot the catalog has since moved past.
    static INSTALLED_USER_TYPES: std::cell::RefCell<Option<(usize, String, u64)>> =
        const { std::cell::RefCell::new(None) };
}

fn catalog_cache() -> &'static CatalogCache {
    static CACHE: OnceLock<CatalogCache> = OnceLock::new();
    CACHE.get_or_init(|| CatalogCache {
        version: std::sync::atomic::AtomicU64::new(0),
        entries: Mutex::new(HashMap::new()),
        tables: Mutex::new(HashMap::new()),
    })
}

/// Declare the committed type catalog changed (or possibly changed): every
/// cached collection is re-read on its next use, on every connection.
fn bump_catalog_version() {
    catalog_cache()
        .version
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// The `CancelRequest` handler: a cancel connection names a `(pid, secret)`,
/// and the matching live backend's `cancel` flag is raised. An unknown PID
/// or a wrong secret is silently ignored, as PostgreSQL ignores it -- the
/// cancel connection gets no answer either way.
pub struct CancelBackend;

#[async_trait]
impl pgwire::api::cancel::CancelHandler for CancelBackend {
    async fn on_cancel_request(&self, request: pgwire::messages::cancel::CancelRequest) {
        let entry = backend_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&request.pid)
            .cloned();
        if let Some(entry) = entry {
            if entry.secret.get() == Some(&request.secret_key.to_bytes()) {
                entry
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

/// A composite type's fields: `(field name, field type name)`, in order.
type CompositeFields = Vec<(String, String)>;
/// One enum as `(schema, bare_name, oid, labels)`.
type EnumWithSchema = (String, String, i64, Vec<String>);

/// The session settings a row encoder needs, captured once per statement:
/// pgwire may encode DataRows lazily on another worker thread, where the
/// session state is not visible.
struct RowEnv {
    tz: secantus_pgplan::TimeZoneSetting,
    ds: secantus_pgplan::DateStyle,
    cenc: ClientEncoding,
}

/// An integer sequence field, whichever BSON width it was written at.
fn bson_i64(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        _ => None,
    }
}

/// One row of `pg_database`: a database this server will accept a connection to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseInfo {
    pub oid: i64,
    pub name: String,
    pub is_template: bool,
    pub allow_conn: bool,
}

/// The databases this server has, which is what a startup packet's `database`
/// is checked against (an unknown name is FATAL `3D000`, as PostgreSQL's is).
///
/// Three sources, in `pg_database` order: the builtin `template1` /
/// `template0` / `postgres` (oids 1 / 4 / 5, as `initdb` numbers them), the
/// names the daemon was started with (`--database`, so a harness can serve a
/// name without a `CREATE DATABASE` first), and the ones `CREATE DATABASE`
/// recorded -- persisted in a namespace of their own so that dropping any
/// user database, the default one included, cannot take the registry with it.
pub struct DatabaseRegistry {
    /// The storage namespace a connection lands in when it names no database
    /// the registry knows a namespace for -- every `dbname=postgres` gauge.
    default: String,
    /// `--database` names. A `Mutex` because `DROP DATABASE` removes one.
    configured: Mutex<Vec<String>>,
}

impl DatabaseRegistry {
    /// The namespace and collection the `CREATE DATABASE` records live in.
    const NAMESPACE: &'static str = "__secantus_pg__";
    const COLLECTION: &'static str = "databases";
    /// Where `initdb` starts user oids.
    const FIRST_USER_OID: i64 = 16384;

    pub fn new(default: &str, configured: impl IntoIterator<Item = String>) -> Self {
        Self {
            default: default.to_string(),
            configured: Mutex::new(configured.into_iter().collect()),
        }
    }

    pub fn default_db(&self) -> &str {
        &self.default
    }

    fn builtin() -> [DatabaseInfo; 3] {
        let info = |oid, name: &str, is_template, allow_conn| DatabaseInfo {
            oid,
            name: name.to_string(),
            is_template,
            allow_conn,
        };
        [
            info(1, "template1", true, true),
            info(4, "template0", true, false),
            info(5, "postgres", false, true),
        ]
    }

    /// The `CREATE DATABASE` records, in oid order.
    fn persisted(&self, storage: &Storage) -> PgWireResult<Vec<DatabaseInfo>> {
        let exists = storage
            .collection_exists(Self::NAMESPACE, Self::COLLECTION)
            .map_err(|e| PgHandler::storage_err("could not read the databases", e))?;
        if !exists {
            return Ok(Vec::new());
        }
        let mut out: Vec<DatabaseInfo> = storage
            .find_matching(Self::NAMESPACE, Self::COLLECTION, &Document::new())
            .map_err(|e| PgHandler::storage_err("could not read the databases", e))?
            .iter()
            .filter_map(|bytes| bson::from_slice::<Document>(bytes).ok())
            .filter_map(|d| {
                Some(DatabaseInfo {
                    oid: d.get("oid").and_then(bson_i64)?,
                    name: d.get_str("_id").ok()?.to_string(),
                    is_template: false,
                    allow_conn: true,
                })
            })
            .collect();
        out.sort_by_key(|d| d.oid);
        Ok(out)
    }

    /// Every database, in `pg_database` order.
    pub fn all(&self, storage: &Storage) -> PgWireResult<Vec<DatabaseInfo>> {
        let mut out: Vec<DatabaseInfo> = Self::builtin().to_vec();
        let configured = self
            .configured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut next_oid = Self::FIRST_USER_OID;
        for name in configured {
            if out.iter().any(|d| d.name == name) {
                continue;
            }
            next_oid += 1;
            out.push(DatabaseInfo {
                oid: next_oid,
                name,
                is_template: false,
                allow_conn: true,
            });
        }
        for info in self.persisted(storage)? {
            if !out.iter().any(|d| d.name == info.name) {
                out.push(info);
            }
        }
        Ok(out)
    }

    pub fn lookup(&self, storage: &Storage, name: &str) -> PgWireResult<Option<DatabaseInfo>> {
        Ok(self.all(storage)?.into_iter().find(|d| d.name == name))
    }

    /// Records a `CREATE DATABASE`. The caller has checked the name is new.
    fn create(&self, storage: &Storage, name: &str) -> PgWireResult<()> {
        let exists = storage
            .collection_exists(Self::NAMESPACE, Self::COLLECTION)
            .map_err(|e| PgHandler::storage_err("could not record the database", e))?;
        if !exists {
            storage
                .create_collection(Self::NAMESPACE, Self::COLLECTION)
                .map_err(|e| PgHandler::storage_err("could not record the database", e))?;
        }
        // Past every oid in use, so a dropped-and-recreated name gets a new
        // one as PostgreSQL's does.
        let oid = self
            .all(storage)?
            .iter()
            .map(|d| d.oid)
            .max()
            .unwrap_or(Self::FIRST_USER_OID)
            .max(Self::FIRST_USER_OID)
            + 1;
        let doc = bson::doc! {"_id": name, "oid": oid};
        let bytes = bson::to_vec(&doc)
            .map_err(|e| PgHandler::storage_err("could not encode the database", e))?;
        storage
            .insert(Self::NAMESPACE, Self::COLLECTION, vec![bytes], true)
            .map_err(|e| PgHandler::storage_err("could not record the database", e))?;
        Ok(())
    }

    /// Forgets a database and drops its data. The caller has checked it exists
    /// and may be dropped.
    fn remove(&self, storage: &Storage, name: &str) -> PgWireResult<()> {
        self.configured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|n| n != name);
        let exists = storage
            .collection_exists(Self::NAMESPACE, Self::COLLECTION)
            .map_err(|e| PgHandler::storage_err("could not drop the database", e))?;
        if exists {
            storage
                .delete_matching(
                    Self::NAMESPACE,
                    Self::COLLECTION,
                    &bson::doc! {"_id": name},
                    0,
                    &Document::new(),
                    None,
                )
                .map_err(|e| PgHandler::storage_err("could not drop the database", e))?;
        }
        storage
            .drop_database(name)
            .map_err(|e| PgHandler::storage_err("could not drop the database", e))?;
        Ok(())
    }
}

/// One database's worth of SQL over a shared `Storage`.
pub struct PgHandler {
    storage: Arc<Storage>,
    /// The database the startup packet named, once it has been checked
    /// against the registry; the registry's default until then.
    db: OnceLock<String>,
    databases: Arc<DatabaseRegistry>,
    /// The open explicit transaction, if any.
    ///
    /// One handler per connection, so this is per-session state exactly as
    /// PostgreSQL's is. Held as a real `UserTransactionHandle` rather than a
    /// flag: a `ROLLBACK` that did not actually roll back would be a silent
    /// wrong answer, which is worse than refusing `BEGIN` outright.
    txn: Mutex<Option<UserTransactionHandle>>,
    /// Session settings (GUCs), per connection as PostgreSQL's are.
    settings: Arc<Mutex<HashMap<String, String>>>,
    /// NoticeResponses raised by the statement in flight (a `DO` block's
    /// `RAISE NOTICE` / `WARNING` / `INFO`), sent to the client by the query
    /// handlers before the statement's own result or error.
    pending_notices: Mutex<Vec<ErrorInfo>>,
    /// GUC changes to report to the client via `ParameterStatus` after the
    /// current query, for the variables PostgreSQL marks GUC_REPORT (TimeZone,
    /// DateStyle, ...). libpq / psycopg track the session `TimeZone` from these
    /// and re-express a `timestamptz` in it -- without the report a stored
    /// instant displays in the client's stale (startup) zone.
    pending_params: Mutex<Vec<(String, String)>>,
    /// The in-progress `COPY ... FROM STDIN`, if any: target plus the bytes
    /// received so far. Per connection, like PostgreSQL's.
    copy_in: Mutex<Option<CopyInState>>,
    /// Open cursors, by name. Per connection, as PostgreSQL's are.
    cursors: Mutex<HashMap<String, CursorState>>,
    /// The NAMED prepared statements of this connection, in the order they
    /// were prepared -- what `pg_prepared_statements` lists. Filled by
    /// `Parse` with a non-empty name, emptied by protocol `Close`,
    /// `DEALLOCATE <name>` and `DEALLOCATE ALL`. Per connection, as
    /// PostgreSQL's are: another session sees none of them. The wire layer's
    /// own statement store is not consulted because it also holds the
    /// unnamed statement, which PostgreSQL never lists.
    prepared: Mutex<Vec<PreparedRecord>>,
    /// Tables created (`Some`) or dropped (`None`) in the open transaction and
    /// not yet committed. Cleared when the transaction ends, whichever way.
    uncommitted: Mutex<HashMap<String, Option<TableDef>>>,
    /// User TYPES created or dropped in the open transaction but not yet
    /// committed -- the type analogue of `uncommitted`. Planning reads the
    /// type catalog (`to_regtype`, a column's declared type), and an
    /// uncommitted `CREATE TYPE` is invisible to a plain read, so
    /// `CREATE TYPE t ...; SELECT 't'::regtype` in one transaction failed.
    /// Wrapping the catalog read in `with_user_transaction` deadlocks COPY
    /// (see `plan_with_session_types`), so this mirrors tables: an overlay
    /// consulted on top of the committed catalog, cleared when the
    /// transaction ends and snapshotted by savepoints. Keyed by the catalog
    /// collection and the doc's `_id`; a `None` value is a drop tombstone.
    uncommitted_types: Mutex<UncommittedTypes>,
    /// Whether a transaction is open, as a LOCK-FREE flag.
    ///
    /// `txn` cannot answer this from inside `execute`: `run` holds that mutex
    /// for the whole of execution and it is not reentrant, so asking there
    /// deadlocks the connection. The flag is set and cleared alongside the
    /// handle itself.
    in_transaction: std::sync::atomic::AtomicBool,
    /// Whether the statement being answered asked for BINARY result columns.
    ///
    /// The format is a property of the `Bind` that started the statement, but
    /// it is needed where the row DESCRIPTION is built, which is several
    /// layers down and reached from both `Describe` and `Execute`. A
    /// connection answers one statement at a time, so a flag set at the top of
    /// each is the whole of the state -- and it is atomic because, like
    /// `in_transaction`, it is read from inside `execute` while `run` holds
    /// the mutexes.
    binary_results: std::sync::atomic::AtomicBool,
    /// Whether the open transaction has FAILED.
    ///
    /// PostgreSQL refuses every statement after an error inside a transaction
    /// block until the block ends -- `25P02`, "current transaction is aborted"
    /// -- and turns a `COMMIT` there into a rollback. Without that, a client
    /// that shrugged off a mid-transaction error went on writing and COMMITTED
    /// work PostgreSQL would have discarded, which is a wrong answer rather
    /// than a missing feature. Lock-free for the same reason as
    /// `in_transaction`.
    txn_failed: std::sync::atomic::AtomicBool,
    /// The catalog version when this session's transaction handle opened;
    /// a catalog read may fill the process-wide cache only while the version
    /// still equals it (see `CatalogCache`).
    txn_catalog_version: std::sync::atomic::AtomicU64,
    /// The open savepoints, oldest first.
    ///
    /// WiredTiger has no savepoint of its own, so one is a set of PRE-IMAGES:
    /// before a statement writes a table, every open savepoint that has not
    /// yet captured that table captures it, and `ROLLBACK TO` puts the
    /// captured contents back. Capturing lazily is what keeps it affordable --
    /// a savepoint nobody writes through costs nothing.
    savepoints: Mutex<Vec<Savepoint>>,
    /// Typed row capture for a `DECLARE CURSOR`, armed only around the inner
    /// query's execution.
    ///
    /// A server cursor materialises its rows once, at DECLARE, encoded in TEXT
    /// (the DECLARE arrives over the simple-query protocol, which is always
    /// text). But a later `FETCH` may ask for BINARY -- psycopg's binary server
    /// cursor requests it on the FETCH's `Bind`, not on the DECLARE -- and the
    /// frozen text bytes cannot be turned back into binary. So the cursor also
    /// keeps the resolved per-column values, captured here as the inner query's
    /// rows stream is drained, and re-encodes them in the FETCH's format. `None`
    /// except during a DECLARE, so a plain SELECT pays only a cheap flag check.
    cursor_capture: std::sync::Arc<Mutex<Option<CapturedRows>>>,
    /// This connection's backend PID, as pgwire assigned it during startup.
    ///
    /// `pg_backend_pid()` returns it, and `pg_terminate_backend(pid)` compares
    /// against it to recognise a self-termination. Set once in `post_startup`;
    /// `0` before that, which no real PID collides with.
    backend_pid: AtomicI32,
    /// The role the client connected as (the startup packet's `user`), which
    /// `current_user` / `session_user` / `user` / `current_role` answer.
    /// PostgreSQL reports the real role; a fixed name was a wrong answer for
    /// every client not connecting as that name.
    session_user: Mutex<String>,
    /// This connection's entry in [`backend_registry`]: what another
    /// backend's `pg_terminate_backend`, a `CancelRequest`, and every
    /// `pg_stat_activity` read see of it.
    backend: Arc<BackendEntry>,
    /// `(table, constraint name)` of every INITIALLY DEFERRED foreign key a
    /// write in the open transaction touched; re-checked at COMMIT.
    deferred_fks: Mutex<Vec<(String, String)>>,
    /// Set when a COMMIT failed its deferred checks and rolled back: the
    /// error goes out, and the `ReadyForQuery` after it must say IDLE (the
    /// transaction is over), where pgwire's error path would say failed.
    commit_failed: AtomicBool,
    /// An extended-protocol STATEMENT GROUP is open: the transaction handle
    /// in `txn` was opened by the first `Execute` since the last `Sync`, not
    /// by a `BEGIN`. PostgreSQL runs every statement between two `Sync`s in
    /// one transaction (`TBLOCK_STARTED`) that commits at the `Sync` -- so an
    /// error rolls back the group's earlier statements too, which is what a
    /// pipelining client relies on. It is NOT a transaction block:
    /// `in_transaction` stays false, so `DECLARE` and `SAVEPOINT` still
    /// answer `25P01`, and a `BEGIN` inside the group turns it into one.
    implicit_extended: AtomicBool,
    /// A statement in the open group failed: the group rolls back at `Sync`.
    group_failed: AtomicBool,
    /// NOTIFYs of the open transaction, `(channel, payload)` in first-issue
    /// order with duplicates dropped, as PostgreSQL queues them: delivered
    /// at commit, discarded at rollback.
    pending_notifies: Mutex<Vec<(String, String)>>,
    /// LISTEN / UNLISTENs of the open transaction, applied at commit.
    pending_listens: Mutex<Vec<ListenOp>>,
}

/// A user type created (`Some(doc)`) or dropped (`None`) in the open
/// transaction, overlaid on the committed catalog. Keyed by
/// `(catalog collection, doc _id)`.
type UncommittedTypes = HashMap<(&'static str, String), Option<Document>>;

/// A materialised result as resolved per-column values: one inner `Vec` per
/// row, one `Option<Bson>` per output column (`None` is SQL NULL). Kept by a
/// server cursor so a BINARY `FETCH` can re-encode rows frozen in text at
/// DECLARE.
type CapturedRows = Vec<Vec<Option<Bson>>>;

/// One open savepoint and the table contents it can put back.
struct Savepoint {
    name: String,
    /// Table -> its documents when this savepoint was established, or `None`
    /// when the table did not exist then (so rolling back DROPS it).
    tables: HashMap<String, Option<Vec<Vec<u8>>>>,
    /// The uncommitted-DDL map as it was, so a table created after this
    /// savepoint stops being visible when it is rolled back.
    uncommitted: HashMap<String, Option<TableDef>>,
    /// The uncommitted-TYPE overlay as it was, so a type created after this
    /// savepoint stops being visible when it is rolled back.
    uncommitted_types: UncommittedTypes,
}

/// A declared cursor's materialised result.
///
/// The rows are collected at DECLARE rather than streamed, because a
/// PostgreSQL cursor is SCROLLABLE -- `MOVE BACKWARD` and `FETCH ABSOLUTE` both
/// have to work, and a forward-only stream cannot answer either. The cost is
/// holding the result in memory, which is the trade this server already makes
/// everywhere else.
struct CursorState {
    schema: Arc<Vec<FieldInfo>>,
    rows: Vec<DataRow>,
    /// PostgreSQL's own cursor position: the 1-based row the cursor sits ON,
    /// with 0 meaning "before the first row" and `len + 1` meaning "after the
    /// last". Those two extra positions are not decoration -- fetching past
    /// the end leaves the cursor at `len + 1`, so a later `MOVE BACKWARD 2`
    /// lands on the LAST row rather than the second-to-last.
    pos: i64,
    /// The `pg_cursors` catalog columns for this open cursor. Reported by a
    /// `SELECT ... FROM pg_cursors`, which psycopg's server cursor issues to
    /// check whether a cursor it is about to close still exists.
    statement: String,
    is_holdable: bool,
    is_binary: bool,
    is_scrollable: bool,
    creation_time: bson::DateTime,
    /// The resolved per-column values behind each text row in `rows`, kept so a
    /// BINARY `FETCH` can re-encode them (see `cursor_capture`). `None` when the
    /// cursor's source is not re-encodable (only a plain `SELECT` is captured);
    /// a binary FETCH of such a cursor falls back to the text bytes.
    typed_rows: Option<CapturedRows>,
    /// The session time zone in force at DECLARE, for re-encoding `typed_rows`.
    tz: secantus_pgplan::TimeZoneSetting,
}

/// One row of `pg_prepared_statements`.
struct PreparedRecord {
    name: String,
    /// The query text exactly as it was parsed.
    statement: String,
    prepare_time: bson::DateTime,
    /// Display names (`smallint`, `character varying`), as a `regtype[]`
    /// renders them.
    parameter_types: Vec<String>,
    /// The result columns' display names, or `None` for a statement that
    /// returns no rows -- PostgreSQL reports NULL there, not an empty array.
    result_types: Option<Vec<String>>,
}

/// A base type as the `__sql_base_types__` catalog holds it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BaseType {
    name: String,
    schema: String,
    oid: i64,
    /// False while the type is a SHELL (`CREATE TYPE name` with no body).
    defined: bool,
    input: Option<String>,
    output: Option<String>,
}

/// A user function as the `__sql_functions__` catalog holds it -- the fields
/// this server reads, which is the signature and the language.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UserFunction {
    name: String,
    param_types: Vec<String>,
    return_type: String,
    language: String,
}

/// What a type name resolves to when a function declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeKind {
    Unknown,
    Shell,
    Defined,
}

struct CopyInState {
    format: secantus_pgplan::CopyFormat,
    table: String,
    /// Stored field per target column, in the order the data supplies them.
    fields: Vec<String>,
    /// The declared PostgreSQL type of each target column, for parsing.
    types: Vec<String>,
    buffer: Vec<u8>,
}

impl PgHandler {
    pub fn new(storage: Arc<Storage>, databases: Arc<DatabaseRegistry>) -> Self {
        Self {
            storage,
            db: OnceLock::new(),
            databases,
            txn: Mutex::new(None),
            settings: Arc::new(Mutex::new(default_settings())),
            pending_notices: Mutex::new(Vec::new()),
            pending_params: Mutex::new(Vec::new()),
            copy_in: Mutex::new(None),
            cursors: Mutex::new(HashMap::new()),
            prepared: Mutex::new(Vec::new()),
            uncommitted: Mutex::new(HashMap::new()),
            uncommitted_types: Mutex::new(HashMap::new()),
            in_transaction: std::sync::atomic::AtomicBool::new(false),
            binary_results: std::sync::atomic::AtomicBool::new(false),
            txn_failed: std::sync::atomic::AtomicBool::new(false),
            txn_catalog_version: std::sync::atomic::AtomicU64::new(0),
            savepoints: Mutex::new(Vec::new()),
            cursor_capture: std::sync::Arc::new(Mutex::new(None)),
            backend_pid: AtomicI32::new(0),
            session_user: Mutex::new(String::new()),
            backend: Arc::new(BackendEntry::new("")),
            deferred_fks: Mutex::new(Vec::new()),
            commit_failed: AtomicBool::new(false),
            implicit_extended: AtomicBool::new(false),
            group_failed: AtomicBool::new(false),
            pending_notifies: Mutex::new(Vec::new()),
            pending_listens: Mutex::new(Vec::new()),
        }
    }

    /// The storage namespace this connection reads and writes.
    fn db(&self) -> &str {
        self.db
            .get()
            .map(String::as_str)
            .unwrap_or_else(|| self.databases.default_db())
    }

    /// One output column, in the format the current statement asked for.
    ///
    /// Every row description goes through here rather than naming a format at
    /// the call site: the format is per statement, and the DESCRIPTION and the
    /// ROWS are built in two different places that have to agree, or the
    /// client decodes binary bytes as text.
    fn field(&self, name: String, ty: Type) -> FieldInfo {
        self.field_mod(name, ty, -1)
    }

    /// Like [`Self::field`], but carrying a declared type-modifier
    /// (`atttypmod`) such as the `(10, 2)` of `numeric(10,2)` or the `(42)` of
    /// `varchar(42)`.
    ///
    /// The modifier and the type's fixed byte width (`typlen`) travel in the
    /// `RowDescription` so clients can report `precision` / `scale` /
    /// `display_size` / `internal_size`. They describe the column, never the
    /// value bytes, so this is description metadata only.
    fn field_mod(&self, name: String, ty: Type, type_modifier: i32) -> FieldInfo {
        let binary = self
            .binary_results
            .load(std::sync::atomic::Ordering::Relaxed)
            && binary_encodable(&ty);
        let format = if binary {
            FieldFormat::Binary
        } else {
            FieldFormat::Text
        };
        // The RowDescription carries column names in the client encoding, so
        // a LATIN1 / LATIN9 session gets the name's transcoded bytes. A name
        // with a character the encoding cannot represent keeps its UTF-8 bytes
        // (PostgreSQL raises 22P05 there; `field_mod` is infallible and the
        // case needs a non-Latin alias under a Latin client encoding).
        let name_raw = transcoded_name(self.client_encoding(), &name);
        FieldInfo::new(name, None, None, ty.clone(), format)
            .with_type_size(type_size(&ty))
            .with_type_modifier(type_modifier)
            .with_name_raw(name_raw)
    }

    /// Remember the result format a `Bind` asked for.
    ///
    /// A MIXED request -- some columns binary, some text -- is answered
    /// entirely in text. PostgreSQL honours it column by column; no client
    /// this server is measured against sends one, and quietly answering half
    /// of it in the wrong format would be worse than uniformly answering the
    /// format the `RowDescription` then reports.
    fn note_result_format(&self, format: &Format) {
        let binary = match format {
            Format::UnifiedText => false,
            Format::UnifiedBinary => true,
            Format::Individual(codes) => !codes.is_empty() && codes.iter().all(|c| *c == 1),
        };
        self.binary_results
            .store(binary, std::sync::atomic::Ordering::Relaxed);
    }

    /// Plan a statement, resolving table names against the catalog PLUS any
    /// tables created in the open transaction but not yet committed.
    ///
    /// Planning reads the catalog, and the catalog is an ordinary table -- so
    /// an uncommitted `CREATE TABLE` is invisible to a plain read, and this
    /// failed:
    ///
    /// ```text
    /// BEGIN;
    /// CREATE TABLE t (...);
    /// SELECT * FROM t;      -- relation "t" does not exist
    /// ```
    ///
    /// EXECUTION already ran inside the transaction, so selecting from a
    /// pre-existing table worked and hid this. Any client that creates a table
    /// and uses it before committing hit it -- the ordinary shape of a test
    /// fixture, and 195 psycopg failures.
    ///
    /// The fix is a per-connection map of what this transaction has created or
    /// dropped, consulted before the catalog. Wrapping the PLAN in a second
    /// `with_user_transaction` also worked for plain statements and DEADLOCKED
    /// COPY, which opens its own transaction context: nesting that call is not
    /// safe, and this needs no nesting.
    /// The enum catalog, in the PYTHON SERVER'S representation -- the two
    /// servers share one store, so these collection names, doc shapes and the
    /// oid-minting rule are a CONTRACT, not an implementation choice.
    /// (`src/secantus/sql/catalog.py`: `__sql_enums__` docs
    /// `{_id, enum, labels, oid}`; `__sql_enum_meta__` carries the monotonic
    /// counter; `typarray` is DERIVED as `oid + 100_000`, never stored.)
    const SCHEMA_COLLECTION: &'static str = "__sql_schemas__";
    const COMPOSITE_COLLECTION: &'static str = "__sql_composites__";
    const COMPOSITE_TYPE_OID_BASE: i64 = 67_000;
    const ENUM_COLLECTION: &'static str = "__sql_enums__";
    const ENUM_META_COLLECTION: &'static str = "__sql_enum_meta__";
    const ENUM_TYPE_OID_BASE: i64 = 65_000;
    const RANGE_COLLECTION: &'static str = "__sql_ranges__";
    const RANGE_TYPE_OID_BASE: i64 = 69_000;
    /// Base types (`CREATE TYPE name` shells and the `(input = ..., output =
    /// ...)` types that complete them): a doc `{_id, base, schema, oid,
    /// defined, input, output}` per type. `_id` is the resolution name, `base`
    /// the bare name (`pg_type.typname`), `defined` false while it is a shell,
    /// `input` / `output` the I/O function names once defined. Rust-server
    /// only: the Python server has no base-type support and does not read
    /// this collection. Oids in their own band; `typarray` derived as
    /// `oid + 100_000` like every other user type, and 0 while a shell
    /// (PostgreSQL mints the array type only at completion).
    const BASE_TYPE_COLLECTION: &'static str = "__sql_base_types__";
    const BASE_TYPE_OID_BASE: i64 = 71_000;
    /// User functions, in the PYTHON server's `__sql_functions__` shape (a
    /// shared-store contract): `_id: "name/nargs"`, `name`, `nargs`, `params`
    /// (declared names, `null` when unnamed), `param_types` (type tags),
    /// `return_tag`, `is_table`, `body`, `language`, `returns_trigger`. The
    /// Rust server writes only `language: "internal"` wrappers here (a base
    /// type's I/O functions) and never runs them.
    const FUNCTION_COLLECTION: &'static str = "__sql_functions__";
    const USER_TYPE_ARRAY_OID_OFFSET: i64 = secantus_pgplan::USER_TYPE_ARRAY_OID_OFFSET;
    /// A custom range's auto-created multirange type gets `range_oid + this`,
    /// and the multirange's own array type `multirange_oid + array offset`.
    /// Range oids live in the 69_000 band, their arrays at +100_000, so
    /// +200_000 (multirange) and +300_000 (multirange array) never collide.
    const MULTIRANGE_TYPE_OID_OFFSET: i64 = 200_000;

    /// Hand the planner this database's user types, at the current catalog
    /// version. The planner's type tables are thread-local, so this runs
    /// before every plan -- but it publishes only when the calling thread
    /// does not already hold this `(storage, db, version)`, or when this
    /// session has an uncommitted type overlay (a per-session view, which
    /// must be re-published every statement and never recorded as held).
    /// Before the skip, every statement rebuilt every table's row type
    /// from BSON, and a used store made `select 1` twice as slow.
    fn install_user_types(&self) {
        secantus_pgplan::set_session_user(Some(
            self.session_user
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        ));
        let overlay_empty = self
            .uncommitted_types
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty();
        let version = catalog_cache()
            .version
            .load(std::sync::atomic::Ordering::SeqCst);
        let held = (
            Arc::as_ptr(&self.storage) as usize,
            self.db().to_string(),
            version,
        );
        if overlay_empty && INSTALLED_USER_TYPES.with(|c| c.borrow().as_ref() == Some(&held)) {
            return;
        }
        self.publish_user_types();
        // A read under a transaction snapshot that predates a catalog bump
        // is not the committed truth at `version` (see
        // `may_fill_catalog_cache`): publish it, but do not record it.
        let record = overlay_empty && self.may_fill_catalog_cache(version);
        INSTALLED_USER_TYPES.with(|c| *c.borrow_mut() = record.then_some(held));
    }

    /// Build the planner's user-type tables from the catalog and publish
    /// them to this thread. `install_user_types` is the gate in front.
    fn publish_user_types(&self) {
        // Enums resolve by name too. A `public` enum resolves by its bare name
        // (public is on the default search_path); a schema-qualified one
        // resolves only as `schema.name`, exactly like composites and ranges.
        let mut types: Vec<(String, i64, Vec<String>)> = self
            .enums_with_schema()
            .unwrap_or_default()
            .into_iter()
            .map(|(schema, name, oid, labels)| (Self::type_resolution(&schema, &name), oid, labels))
            .collect();
        // Composites resolve by name too; they have no labels, so an empty
        // label list stands in. A `public` composite resolves by its bare name
        // (public is on the default search_path); a schema-qualified one
        // resolves only as `schema.name`, so `to_regtype('testschema.t')` finds
        // it while `to_regtype('t')` does not (matching PostgreSQL).
        let mut composites: Vec<secantus_pgplan::CompositeType> = Vec::new();
        for (schema, name, oid, fields) in self.composites_with_schema().unwrap_or_default() {
            let resolution = Self::type_resolution(&schema, &name);
            types.push((resolution.clone(), oid, Vec::new()));
            // The field metadata rides its own channel so a composite VALUE cast
            // (`'(1,x)'::testcomp`) resolves each field's type; the empty label
            // list above keeps it OUT of the enum arm.
            composites.push((resolution, oid, fields));
        }
        secantus_pgplan::set_user_types(types);
        secantus_pgplan::set_user_composites(composites);
        // Custom ranges resolve their subtype at cast time and their oid for
        // regtype -- but they are NOT enums, so they stay OUT of set_user_types.
        // A schema-qualified range resolves as `schema.name`, so
        // `to_regtype('testschema.testrange')` reaches it and a bare
        // `to_regtype('testrange')` reaches only the public one.
        let ranges_with_schema = self.ranges_with_schema().unwrap_or_default();
        let ranges: Vec<(String, String, i64)> = ranges_with_schema
            .iter()
            .map(|(schema, name, oid, subtype)| {
                (Self::type_resolution(schema, name), subtype.clone(), *oid)
            })
            .collect();
        secantus_pgplan::set_user_ranges(ranges);
        // Every custom range carries an auto-created multirange companion.
        // Its resolution name is the range's multirange name (bare in public,
        // else schema-qualified), so `to_regtype('testmultirange')` and the
        // schema-qualified form both reach it -- exactly like the range.
        let multiranges: Vec<(String, i64, String)> = ranges_with_schema
            .iter()
            .map(|(schema, name, oid, _)| {
                let mr_name = secantus_pgplan::range::multirange_name_for(name);
                (
                    Self::type_resolution(schema, &mr_name),
                    oid + Self::MULTIRANGE_TYPE_OID_OFFSET,
                    Self::type_resolution(schema, name),
                )
            })
            .collect();
        secantus_pgplan::set_user_multiranges(multiranges);
        // Base types (shell or defined) resolve by name for casts and regtype;
        // the planner refuses a cast to a shell and hides it from to_regtype.
        let base_types: Vec<(String, i64, bool)> = self
            .base_types()
            .unwrap_or_default()
            .into_iter()
            .map(|b| (Self::type_resolution(&b.schema, &b.name), b.oid, b.defined))
            .collect();
        secantus_pgplan::set_user_base_types(base_types);
    }

    /// The name a user type resolves under: its bare name in `public` (on the
    /// default search_path), else `schema.name`.
    fn type_resolution(schema: &str, name: &str) -> String {
        if schema == "public" {
            name.to_string()
        } else {
            format!("{schema}.{name}")
        }
    }

    /// The wire type for a column whose type is a USER enum: pgwire's `Type`
    /// takes a custom oid, and psycopg matches it against what EnumInfo
    /// registered.
    fn user_wire_type(&self, pg_type: &str) -> Option<Type> {
        let enums = self.enums().ok()?;
        // An enum ARRAY: `inttestenum[]` reports the type's typarray oid
        // (derived, oid + 100_000), so a client that registered the array
        // decodes it rather than reading varchar. A `[]` suffix that matches
        // no enum falls through (composite arrays are handled below), so this
        // must NOT `?`-short-circuit the whole function on a miss.
        if let Some(element) = pg_type.strip_suffix("[]") {
            if let Some((name, oid, _)) = enums.iter().find(|(n, _, _)| n == element) {
                return Some(Type::new(
                    format!("_{name}"),
                    u32::try_from(*oid + Self::USER_TYPE_ARRAY_OID_OFFSET).ok()?,
                    postgres_types::Kind::Array(Type::new(
                        name.clone(),
                        u32::try_from(*oid).ok()?,
                        postgres_types::Kind::Enum(Vec::new()),
                        "public".to_string(),
                    )),
                    "public".to_string(),
                ));
            }
        }
        if let Some((name, oid, _)) = enums.iter().find(|(n, _, _)| n == pg_type) {
            return Some(Type::new(
                name.clone(),
                u32::try_from(*oid).ok()?,
                postgres_types::Kind::Enum(Vec::new()),
                "public".to_string(),
            ));
        }
        // A custom range (`create type testrange as range (...)`), its
        // auto-created multirange, or an ARRAY of either: its own oid, so a
        // client that ran `register_range` / `register_multirange` fires its
        // loader instead of reading text. The value goes out as range TEXT in
        // either cursor format, as a builtin range does.
        if let Some(ty) = self.user_range_wire_type(pg_type) {
            return Some(ty);
        }
        // A base type reports its own oid (and its array the derived typarray
        // oid, `Kind::Array(element)`), so a client that registered a dumper /
        // loader on `TypeInfo.fetch`'s oid sees it come back. The value goes
        // out as its text form in either cursor format.
        if let Some(ty) = self.user_base_wire_type(pg_type) {
            return Some(ty);
        }
        // A composite type reports its own oid so a client that ran
        // `register_composite` fires its loader. The type carries
        // `Kind::Composite` (its declared fields, resolved to their own wire
        // types), so the value goes out as PostgreSQL composite TEXT `(...)`
        // in a text cursor and as the binary record format in a binary one.
        // A composite ARRAY: `testcomp[]` reports the derived typarray oid with
        // `Kind::Array(composite)` so a client that registered the array
        // decodes each element as the composite rather than reading varchar.
        if let Some(element) = pg_type.strip_suffix("[]") {
            let (schema, bare, oid, fields) = self.composite_with_schema_named(element).ok()??;
            let elem_ty = self.composite_type(&schema, &bare, oid, &fields)?;
            return Some(Type::new(
                format!("_{bare}"),
                u32::try_from(oid + Self::USER_TYPE_ARRAY_OID_OFFSET).ok()?,
                postgres_types::Kind::Array(elem_ty),
                schema,
            ));
        }
        let (schema, bare, oid, fields) = self.composite_with_schema_named(pg_type).ok()??;
        self.composite_type(&schema, &bare, oid, &fields)
    }

    /// The wire `Type` for a DEFINED base type or its array, by resolution
    /// name (`a-b`, `a-b[]`). A shell has no values, so it has no wire type.
    fn user_base_wire_type(&self, pg_type: &str) -> Option<Type> {
        let (element, is_array) = match pg_type.strip_suffix("[]") {
            Some(e) => (e, true),
            None => (pg_type, false),
        };
        let base = self
            .base_types()
            .ok()?
            .into_iter()
            .find(|b| b.defined && Self::type_resolution(&b.schema, &b.name) == element)?;
        let scalar = Type::new(
            base.name.clone(),
            u32::try_from(base.oid).ok()?,
            postgres_types::Kind::Simple,
            base.schema.clone(),
        );
        if !is_array {
            return Some(scalar);
        }
        Some(Type::new(
            format!("_{}", base.name),
            u32::try_from(base.oid + Self::USER_TYPE_ARRAY_OID_OFFSET).ok()?,
            postgres_types::Kind::Array(scalar),
            base.schema,
        ))
    }

    /// The wire `Type` for a custom range, its multirange companion, or an
    /// array of either, by resolution name (`testrange`, `testmultirange`,
    /// `testschema.testrange[]`). `Kind::Range(subtype)` / `Kind::Multirange`
    /// carry the subtype the way pgwire's builtin range types do.
    fn user_range_wire_type(&self, pg_type: &str) -> Option<Type> {
        let (element, is_array) = match pg_type.strip_suffix("[]") {
            Some(e) => (e, true),
            None => (pg_type, false),
        };
        let ranges = self.ranges_with_schema().ok()?;
        let scalar = ranges.iter().find_map(|(schema, name, oid, subtype)| {
            // Match the name BEFORE resolving the subtype: resolving it
            // re-enters `user_wire_type`, which asks this function again
            // for every non-enum name (`text`), so resolving eagerly for
            // every registered range recursed without bound.
            let is_range = Self::type_resolution(schema, name) == element;
            let mr_name = secantus_pgplan::range::multirange_name_for(name);
            if !is_range && Self::type_resolution(schema, &mr_name) != element {
                return None;
            }
            let sub = if subtype == element {
                None
            } else {
                self.user_wire_type(subtype)
            }
            .unwrap_or_else(|| wire_type(subtype));
            let range_ty = Type::new(
                name.clone(),
                u32::try_from(*oid).ok()?,
                postgres_types::Kind::Range(sub),
                schema.clone(),
            );
            if is_range {
                return Some(range_ty);
            }
            Some(Type::new(
                mr_name,
                u32::try_from(oid + Self::MULTIRANGE_TYPE_OID_OFFSET).ok()?,
                postgres_types::Kind::Multirange(range_ty),
                schema.clone(),
            ))
        })?;
        if !is_array {
            return Some(scalar);
        }
        Some(Type::new(
            format!("_{}", scalar.name()),
            u32::try_from(i64::from(scalar.oid()) + Self::USER_TYPE_ARRAY_OID_OFFSET).ok()?,
            postgres_types::Kind::Array(scalar.clone()),
            scalar.schema().to_string(),
        ))
    }

    /// The wire `Type` for a composite from its `(schema, bare, oid, fields)`.
    ///
    /// Carries `Kind::Composite` with one `Field` per declared column, each
    /// field's type resolved through the same door (so a nested composite
    /// field resolves recursively) and falling back to `wire_type` for a
    /// built-in field type. The field oids are what the binary record encoder
    /// stamps into each field header.
    fn composite_type(
        &self,
        schema: &str,
        bare: &str,
        oid: i64,
        fields: &CompositeFields,
    ) -> Option<Type> {
        let field_types = fields
            .iter()
            .map(|(fname, ftype)| {
                let ty = self
                    .user_wire_type(ftype)
                    .unwrap_or_else(|| wire_type(ftype));
                postgres_types::Field::new(fname.clone(), ty)
            })
            .collect();
        Some(Type::new(
            bare.to_string(),
            u32::try_from(oid).ok()?,
            postgres_types::Kind::Composite(field_types),
            schema.to_string(),
        ))
    }

    /// The name a RAW parameter oid resolves under, when the oid names a USER
    /// type (composite / enum, and their arrays). pgwire's `Type::from_oid`
    /// only knows builtins, so a user type's oid arrives as `None` in
    /// `parameter_types` -- this recovers the planner-facing name from the raw
    /// oid the (patched) `parameter_oids` preserved, so a bound composite /
    /// enum parameter gets a declared type instead of `could not determine
    /// data type of parameter $1`.
    fn user_type_name_for_oid(&self, oid: u32) -> Option<String> {
        let oid_i = i64::from(oid);
        if let Ok(cs) = self.composites_with_schema() {
            if let Some((schema, name, _, _)) = cs.iter().find(|(_, _, o, _)| *o == oid_i) {
                return Some(Self::type_resolution(schema, name));
            }
            if let Some((schema, name, _, _)) = cs
                .iter()
                .find(|(_, _, o, _)| *o + Self::USER_TYPE_ARRAY_OID_OFFSET == oid_i)
            {
                return Some(format!("{}[]", Self::type_resolution(schema, name)));
            }
        }
        if let Ok(es) = self.enums_with_schema() {
            if let Some((schema, name, _, _)) = es.iter().find(|(_, _, o, _)| *o == oid_i) {
                return Some(Self::type_resolution(schema, name));
            }
            if let Some((schema, name, _, _)) = es
                .iter()
                .find(|(_, _, o, _)| *o + Self::USER_TYPE_ARRAY_OID_OFFSET == oid_i)
            {
                return Some(format!("{}[]", Self::type_resolution(schema, name)));
            }
        }
        if let Ok(bs) = self.base_types() {
            for b in &bs {
                let resolution = Self::type_resolution(&b.schema, &b.name);
                if b.oid == oid_i {
                    return Some(resolution);
                }
                if b.oid + Self::USER_TYPE_ARRAY_OID_OFFSET == oid_i {
                    return Some(format!("{resolution}[]"));
                }
            }
        }
        // A custom range, its multirange companion, and their arrays: the
        // oid psycopg's `register_range` / `register_multirange` dumpers
        // stamp on a bound parameter.
        if let Ok(rs) = self.ranges_with_schema() {
            for (schema, name, oid, _) in &rs {
                let mr_name = secantus_pgplan::range::multirange_name_for(name);
                let named = [
                    (*oid, Self::type_resolution(schema, name)),
                    (
                        *oid + Self::MULTIRANGE_TYPE_OID_OFFSET,
                        Self::type_resolution(schema, &mr_name),
                    ),
                ];
                for (base, resolution) in named {
                    if base == oid_i {
                        return Some(resolution);
                    }
                    if base + Self::USER_TYPE_ARRAY_OID_OFFSET == oid_i {
                        return Some(format!("{resolution}[]"));
                    }
                }
            }
        }
        None
    }

    /// The declared PLANNER type name for each parameter of a prepared
    /// statement, in order: the builtin name for a mapped `Type`, else the
    /// user-type name recovered from the raw Parse oid (see
    /// `user_type_name_for_oid`), else `None` (the client left it to us).
    ///
    /// Sized by the SQL's own `$n` references when the client listed fewer
    /// oids than that (none at all is the usual case -- libpq's `PQprepare`
    /// with `nParams = 0`): the slots past the list are `None`, the same as an
    /// oid of 0. Sized from the oid list alone, `select $1::uuid` prepared
    /// with no oids described as "there is no parameter $1".
    fn param_type_names(&self, stmt: &StoredStatement<ParsedStatement>) -> Vec<Option<String>> {
        let n = stmt
            .parameter_types
            .len()
            .max(secantus_pgplan::max_param_number(&stmt.statement.sql));
        (0..n)
            .map(|i| {
                stmt.parameter_types
                    .get(i)
                    .and_then(|t| t.as_ref())
                    .and_then(internal_type_name)
                    .or_else(|| {
                        stmt.parameter_oids
                            .get(i)
                            .copied()
                            .filter(|o| *o != 0)
                            .and_then(|oid| self.user_type_name_for_oid(oid))
                    })
            })
            .collect()
    }

    /// The `pg_prepared_statements` row for a freshly parsed NAMED statement.
    ///
    /// Parameter types are the client's declarations completed from the SQL
    /// (`catalog_param_types`), in display spelling; result types come from
    /// the same describe pass a `Describe` would run, and a statement that
    /// cannot be described (a bad query, a table that does not exist yet)
    /// records no result types rather than failing the Parse -- PostgreSQL
    /// reports the parse error at Parse time, and this server reports it at
    /// Describe, which is where psycopg sees it either way.
    fn prepared_record(&self, stmt: &StoredStatement<ParsedStatement>) -> PreparedRecord {
        let sql = stmt.statement.sql.clone();
        let declared = self.param_type_names(stmt);
        let column_type = |table: &str, column: secantus_pgplan::ColumnRef<'_>| {
            self.lookup(table).and_then(|def| {
                let col = match column {
                    secantus_pgplan::ColumnRef::Name(name) => def.column(name),
                    secantus_pgplan::ColumnRef::Position(pos) => def.columns.get(pos),
                };
                col.map(|c| c.pg_type.clone())
            })
        };
        let parameter_types = secantus_pgplan::catalog_param_types(&sql, &declared, &column_type)
            .into_iter()
            .map(|t| secantus_pgplan::display_type(&t))
            .collect();
        let result_types = self
            .describe_fields(&sql, declared.len(), &declared)
            .ok()
            .flatten()
            .map(|fields| {
                fields
                    .iter()
                    .map(|f| {
                        let ty = f.datatype();
                        internal_type_name(ty)
                            .map(|n| secantus_pgplan::display_type(&n))
                            .unwrap_or_else(|| ty.name().to_string())
                    })
                    .collect()
            });
        PreparedRecord {
            name: stmt.id.clone(),
            statement: sql,
            prepare_time: bson::DateTime::now(),
            parameter_types,
            result_types,
        }
    }

    /// The wire `Type` a RAW parameter oid names, when it is a user type. Used
    /// to report a composite / enum parameter's real oid in
    /// `ParameterDescription` rather than `unknown`.
    fn user_wire_type_for_oid(&self, oid: u32) -> Option<Type> {
        let name = self.user_type_name_for_oid(oid)?;
        self.user_wire_type(&name)
    }

    /// Decode a bound PARAMETER whose raw oid names a user COMPOSITE into the
    /// same record BSON a `'(..)'::comp` literal produces. `Ok(None)` when the
    /// oid is not a known composite, so the caller falls back to the ordinary
    /// per-type decoder. Handles both the TEXT `(a,b,..)` form and the binary
    /// RECORD form, and recurses for a composite-typed field.
    fn decode_composite_param(
        &self,
        oid: u32,
        raw: Option<&Bytes>,
        binary: bool,
        tz: &secantus_pgplan::TimeZoneSetting,
    ) -> PgWireResult<Option<Bson>> {
        let oid_i = i64::from(oid);
        let composites = self.composites_with_schema()?;
        let Some((schema, name, _, fields)) =
            composites.into_iter().find(|(_, _, o, _)| *o == oid_i)
        else {
            return Ok(None);
        };
        let resolution = Self::type_resolution(&schema, &name);
        let Some(bytes) = raw else {
            return Ok(Some(Bson::Null));
        };
        // The planner coerces the assembled value through its user-composite
        // table, so it must be installed first.
        self.install_user_types();
        if !binary {
            let text = String::from_utf8_lossy(bytes);
            return secantus_pgplan::cast_text_to(&text, &resolution, tz)
                .map(Some)
                .map_err(|e| Self::err(&e));
        }
        let vals = self.decode_binary_record(bytes, &fields, tz)?;
        let mut d = Document::new();
        d.insert(secantus_pgplan::RECORD_KEY, Bson::Array(vals));
        secantus_pgplan::cast_value_with_tz(Bson::Document(d), &resolution, tz)
            .map(Some)
            .map_err(|e| Self::err(&e))
    }

    /// Decode PostgreSQL's binary RECORD datum into one BSON per field. Layout:
    /// `i32 ncols`, then per field `u32 field_oid`, `i32 len` (`-1` = NULL),
    /// `len` bytes in the field's binary format. Each field decodes through the
    /// same per-type decoder its own binary parameter would, recursing into
    /// `decode_composite_param` when the field is itself a composite.
    fn decode_binary_record(
        &self,
        bytes: &[u8],
        fields: &CompositeFields,
        tz: &secantus_pgplan::TimeZoneSetting,
    ) -> PgWireResult<Vec<Bson>> {
        let malformed = || {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "22P03".into(), // invalid_binary_representation
                "malformed binary record parameter".into(),
            )))
        };
        if bytes.len() < 4 {
            return Err(malformed());
        }
        let ncols = i32::from_be_bytes(bytes[..4].try_into().expect("checked")).max(0) as usize;
        let mut pos = 4usize;
        let mut out = Vec::with_capacity(ncols);
        for i in 0..ncols {
            if pos + 8 > bytes.len() {
                return Err(malformed());
            }
            let field_oid = u32::from_be_bytes(bytes[pos..pos + 4].try_into().expect("checked"));
            pos += 4;
            let len = i32::from_be_bytes(bytes[pos..pos + 4].try_into().expect("checked"));
            pos += 4;
            if len < 0 {
                out.push(Bson::Null);
                continue;
            }
            let n = len as usize;
            if pos + n > bytes.len() {
                return Err(malformed());
            }
            let field_bytes = Bytes::copy_from_slice(&bytes[pos..pos + n]);
            pos += n;
            // A composite-typed field is another record: recurse on its oid.
            if let Some(rec) =
                self.decode_composite_param(field_oid, Some(&field_bytes), true, tz)?
            {
                out.push(rec);
                continue;
            }
            let ftype = fields.get(i).map(|(_, t)| t.as_str());
            let field_ty = ftype
                .and_then(|t| self.user_wire_type(t))
                .or_else(|| ftype.map(wire_type));
            // A text-family field inside a binary record carries client-encoded
            // bytes, exactly like a top-level text parameter.
            out.push(decode_parameter(
                Some(&field_bytes),
                field_ty.as_ref(),
                true,
                tz,
                self.client_encoding(),
            )?);
        }
        Ok(out)
    }

    /// One table's rows as documents, virtual or stored, unfiltered.
    fn table_docs(&self, table: &str) -> PgWireResult<Vec<Document>> {
        if let Some(docs) = self.virtual_rows(table, &Document::new()) {
            return Ok(docs);
        }
        let raw = self
            .storage
            .find_matching(self.db(), table, &Document::new())
            .map_err(|e| Self::storage_err("could not read", e))?;
        raw.iter()
            .map(|b| {
                bson::from_slice(b).map_err(|e| Self::storage_err("could not decode a row", e))
            })
            .collect()
    }

    /// Materialise a joined subquery's rows, keyed by its OUTPUT names.
    ///
    /// Nested-loop over two materialised sides -- the catalog tables this
    /// exists for are dozens of rows. The ON and WHERE equalities compare
    /// NUMERICALLY across int widths and unwrap a regtype to its oid, because
    /// `t.oid = to_regtype(...)` is the shape every caller sends.
    /// The grouped, ordered aggregate result: (group key, computed values) per
    /// group, positional. Shared by the Aggregate response arm (which encodes
    /// positionally) and `aggregate_rows` (which keys by output name for a join
    /// subquery side).
    #[allow(clippy::type_complexity)]
    fn aggregate_groups(
        &self,
        agg: &secantus_pgplan::Aggregate,
        max_rows: usize,
    ) -> PgWireResult<Vec<(Vec<Option<Bson>>, Vec<Bson>)>> {
        let empty = Document::new();
        let docs: Vec<Document> = match &agg.series {
            Some(series) => series
                .values()
                .into_iter()
                .map(|v| {
                    let mut d = Document::new();
                    d.insert(series.column.clone(), Bson::Int32(v as i32));
                    d
                })
                .filter(|d| {
                    agg.filter.is_empty()
                        || secantus_core::query::matches(d, &agg.filter, &empty, None)
                            .unwrap_or(false)
                })
                .collect(),
            // A virtual table's rows are computed, not read: without
            // this arm `count(*) from pg_type` fell through to storage,
            // found no such collection, and answered 0 -- the right
            // shape and the wrong number, which no error would flag.
            // A joined subquery's rows, already keyed by output name.
            _ if agg.join.is_some() => self.join_docs(agg.join.as_ref().expect("checked"))?,
            None if Self::virtual_table(&agg.table).is_some() => {
                self.virtual_rows(&agg.table, &agg.filter).expect("checked")
            }
            None => {
                let raw = self
                    .storage
                    .find_matching(self.db(), &agg.table, &agg.filter)
                    .map_err(|e| Self::storage_err("could not read", e))?;
                raw.iter()
                    .map(|b| bson::from_slice(b))
                    .collect::<Result<_, _>>()
                    .map_err(|e| Self::storage_err("could not decode a row", e))?
            }
        };

        // An aggregate over an expression reads a hidden per-row slot
        // (`__aggN`), filled here so the aggregate itself only ever sees a
        // field.
        let mut docs = docs;
        for item in &agg.items {
            let (Some(expr), Some(slot)) = (item.expr.as_ref(), item.field.as_deref()) else {
                continue;
            };
            for d in docs.iter_mut() {
                let v = secantus_pgplan::apply_row_expr(expr, d).map_err(|e| Self::err(&e))?;
                d.insert(slot, v);
            }
        }
        // Group, preserving first-seen order so output is deterministic
        // even with no ORDER BY.
        let mut keys: Vec<Vec<Option<Bson>>> = Vec::new();
        let mut buckets: Vec<Vec<Document>> = Vec::new();
        if agg.group_by.is_empty() {
            keys.push(Vec::new());
            buckets.push(docs);
        } else {
            for d in docs {
                // NULL forms its OWN group in PostgreSQL, so a missing
                // or null key is a real key rather than a skip.
                let mut key: Vec<Option<Bson>> = Vec::with_capacity(agg.group_by.len());
                for k in &agg.group_by {
                    let v = match &k.expr {
                        Some(expr) => {
                            secantus_pgplan::apply_row_expr(expr, &d).map_err(|e| Self::err(&e))?
                        }
                        None => d.get(&k.field).cloned().unwrap_or(Bson::Null),
                    };
                    key.push(match v {
                        Bson::Null => None,
                        v => Some(v),
                    });
                }
                match keys.iter().position(|k| *k == key) {
                    Some(i) => buckets[i].push(d),
                    None => {
                        keys.push(key);
                        buckets.push(vec![d]);
                    }
                }
            }
        }

        // (group key, computed aggregates) per group. Kept POSITIONAL:
        // `SELECT count(*), count(n)` yields two columns both named
        // `count`, so a name-keyed row silently drops one.
        let mut groups: Vec<(Vec<Option<Bson>>, Vec<Bson>)> = keys
            .iter()
            .zip(buckets.iter())
            .map(|(k, bucket)| {
                let vals = agg
                    .items
                    .iter()
                    .map(|item| compute_aggregate(item, bucket))
                    .collect();
                (k.clone(), vals)
            })
            .collect();

        // Sort on the GROUP KEY, by index -- so `GROUP BY s ORDER BY s`
        // works even when `s` is not projected.
        if !agg.order.is_empty() {
            groups.sort_by(|a, b| {
                for key in &agg.order {
                    let (l, r) = (&a.0[key.group_index], &b.0[key.group_index]);
                    let ord = match (l, r) {
                        (None, None) => Ordering::Equal,
                        (None, Some(_)) => match key.nulls {
                            Nulls::First => Ordering::Less,
                            Nulls::Last => Ordering::Greater,
                        },
                        (Some(_), None) => match key.nulls {
                            Nulls::First => Ordering::Greater,
                            Nulls::Last => Ordering::Less,
                        },
                        (Some(x), Some(y)) => {
                            let c = compare_values(x, y);
                            if key.ascending {
                                c
                            } else {
                                c.reverse()
                            }
                        }
                    };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                Ordering::Equal
            });
        }
        if agg.offset > 0 {
            let skip = usize::try_from(agg.offset).unwrap_or(usize::MAX);
            groups = groups.into_iter().skip(skip).collect();
        }
        if let Some(limit) = agg.limit {
            groups.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
        }
        if max_rows > 0 {
            groups.truncate(max_rows);
        }
        Ok(groups)
    }

    /// An aggregate's output rows as documents keyed by OUTPUT column name, for
    /// materialising a join subquery side. (Distinct output names are assumed --
    /// true for the catalog introspection queries this serves.)
    fn aggregate_rows(&self, agg: &secantus_pgplan::Aggregate) -> PgWireResult<Vec<Document>> {
        use secantus_pgplan::OutputCol;
        let groups = self.aggregate_groups(agg, 0)?;
        let mut out = Vec::with_capacity(groups.len());
        for (key, vals) in groups {
            let mut doc = Document::new();
            for (name, col) in &agg.select {
                let v = match col {
                    OutputCol::Group(i) => key[*i].clone().unwrap_or(Bson::Null),
                    OutputCol::Agg(i) => vals[*i].clone(),
                };
                doc.insert(name.clone(), v);
            }
            out.push(doc);
        }
        Ok(out)
    }

    /// Materialise a JOIN subquery side (`... JOIN (SELECT ...) a`) to rows.
    /// Only the shapes the planner emits as a `*_sub` are reachable here (an
    /// aggregate subquery today); anything else is a planner/executor mismatch.
    fn sub_plan_rows(&self, stmt: &Statement) -> PgWireResult<Vec<Document>> {
        match stmt {
            Statement::Aggregate(agg) => self.aggregate_rows(agg),
            _ => Err(Self::err(&PlanError::Unsupported(
                "this JOIN subquery shape".into(),
            ))),
        }
    }

    fn join_docs(&self, join: &secantus_pgplan::JoinSelect) -> PgWireResult<Vec<Document>> {
        self.join_docs_with(join, None, None)
    }

    /// `join_docs` with optional pre-materialised rows for a side that is a
    /// SUBQUERY (`... JOIN (SELECT ...) a`) rather than a table -- those rows
    /// are keyed by the sub-plan's OUTPUT names, so the side's "field" is the
    /// column name itself. A table side (rows `None`) reads from `table_docs`.
    fn join_docs_with(
        &self,
        join: &secantus_pgplan::JoinSelect,
        left_rows: Option<Vec<Document>>,
        right_rows: Option<Vec<Document>>,
    ) -> PgWireResult<Vec<Document>> {
        let eq = |a: &Bson, b: &Bson| -> bool {
            let num = |v: &Bson| -> Option<i64> {
                secantus_pgplan::regtype_oid(v).or(match v {
                    Bson::Int32(x) => Some(i64::from(*x)),
                    Bson::Int64(x) => Some(*x),
                    _ => None,
                })
            };
            match (num(a), num(b)) {
                (Some(x), Some(y)) => x == y,
                _ => a == b,
            }
        };
        let field_of = |table: &str, col: &str| -> PgWireResult<String> {
            self.lookup(table)
                .and_then(|def| def.field_of(col))
                .ok_or_else(|| Self::err(&PlanError::UndefinedColumn(col.to_string())))
        };

        let left_is_sub = join.left_sub.is_some();
        let right_is_sub = join.right_sub.is_some();
        let lfield = |col: &str| -> PgWireResult<String> {
            if left_is_sub {
                Ok(col.to_string())
            } else {
                field_of(&join.left.0, col)
            }
        };
        let rfield = |col: &str| -> PgWireResult<String> {
            if right_is_sub {
                Ok(col.to_string())
            } else {
                field_of(&join.right.0, col)
            }
        };
        let mut left_rows = match left_rows {
            Some(r) => r,
            None => self.table_docs(&join.left.0)?,
        };
        let mut right_rows = match right_rows {
            Some(r) => r,
            None => self.table_docs(&join.right.0)?,
        };

        // WHERE predicates bind to whichever side each alias names; applying
        // them before the join is correct and keeps the nested loop trivial. A
        // left-side predicate filters the left rows; a right-side one filters
        // the right rows -- safe under an INNER join, and under a LEFT join too
        // for these catalog queries (an unmatched left row still surfaces as a
        // NULL-extended miss, which is what a LEFT JOIN means). Ops beyond `=`
        // (`>`/`>=`/`<`/`<=`) compare numerically; `NotTrue` keeps rows whose
        // boolean column is not true.
        use secantus_pgplan::JoinOp;
        for pred in &join.filter {
            let on_left = pred.alias == join.left.1;
            let field = if on_left {
                lfield(&pred.col)?
            } else {
                rfield(&pred.col)?
            };
            let value = pred.value.clone();
            let op = pred.op.clone();
            let keep = move |d: &Document| -> bool {
                let cell = d.get(&field);
                match op {
                    JoinOp::NotTrue => !matches!(cell, Some(Bson::Boolean(true))),
                    JoinOp::Eq => cell.map(|v| eq(v, &value)).unwrap_or(false),
                    _ => {
                        let ord = cell.and_then(|v| secantus_pgplan::compare_values(v, &value));
                        match ord {
                            None => false,
                            Some(o) => match op {
                                JoinOp::Gt => o == std::cmp::Ordering::Greater,
                                JoinOp::Ge => o != std::cmp::Ordering::Less,
                                JoinOp::Lt => o == std::cmp::Ordering::Less,
                                JoinOp::Le => o != std::cmp::Ordering::Greater,
                                _ => false,
                            },
                        }
                    }
                }
            };
            if on_left {
                left_rows.retain(keep);
            } else {
                right_rows.retain(keep);
            }
        }

        // The ON columns, resolved to each side's stored field.
        let (l_on, r_on) = {
            let (a, b) = (&join.on.0, &join.on.1);
            if a.0 == join.left.1 {
                (lfield(&a.1)?, rfield(&b.1)?)
            } else {
                (lfield(&b.1)?, rfield(&a.1)?)
            }
        };

        let tz = self.session_timezone();
        let mut out = Vec::new();
        for l in &left_rows {
            let matches: Vec<&Document> = right_rows
                .iter()
                .filter(|r| match (l.get(&l_on), r.get(&r_on)) {
                    (Some(a), Some(b)) => eq(a, b),
                    _ => false,
                })
                .collect();
            let rights: Vec<Option<&Document>> = if matches.is_empty() {
                if join.left_join {
                    vec![None]
                } else {
                    Vec::new()
                }
            } else {
                matches.into_iter().map(Some).collect()
            };
            for r in rights {
                let mut doc = Document::new();
                for (i, (out_name, alias, col)) in join.columns.iter().enumerate() {
                    let on_left = if *alias == join.left.1 {
                        true
                    } else if *alias == join.right.1 || left_is_sub {
                        // A named right alias, or an unaliased column when the
                        // left side is a subquery (whose columns we cannot probe
                        // by name), resolves to the right.
                        false
                    } else {
                        self.lookup(&join.left.0)
                            .is_some_and(|d| d.column(col).is_some())
                    };
                    let value = if on_left {
                        let f = lfield(col)?;
                        l.get(&f).cloned().unwrap_or(Bson::Null)
                    } else {
                        match r {
                            Some(r) => {
                                let f = rfield(col)?;
                                r.get(&f).cloned().unwrap_or(Bson::Null)
                            }
                            None => Bson::Null,
                        }
                    };
                    // Most column exprs (cast chains, scalar calls) no-op on a
                    // NULL and are skipped; COALESCE is the exception -- a
                    // LEFT-JOIN miss is exactly the NULL it must replace.
                    let value = match join.exprs.get(i).and_then(|e| e.as_ref()) {
                        Some(expr)
                            if value != Bson::Null
                                || matches!(expr, secantus_pgplan::ColumnExpr::Coalesce { .. }) =>
                        {
                            secantus_pgplan::apply_column_expr(expr, value, &tz)
                                .map_err(|e| PgHandler::err(&e))?
                        }
                        _ => value,
                    };
                    doc.insert(out_name.clone(), value);
                }
                // The ORDER BY column rides along under a reserved name even
                // when not projected -- `ORDER BY e.enumsortorder` sorts a
                // projection that does not include it.
                if let Some((alias, col, _)) = &join.order {
                    let value = if *alias == join.left.1 {
                        let f = lfield(col)?;
                        l.get(&f).cloned().unwrap_or(Bson::Null)
                    } else {
                        match r {
                            Some(r) => {
                                let f = rfield(col)?;
                                r.get(&f).cloned().unwrap_or(Bson::Null)
                            }
                            None => Bson::Null,
                        }
                    };
                    doc.insert("__join_order", value);
                }
                out.push(doc);
            }
        }

        if let Some((_, _, ascending)) = &join.order {
            // PostgreSQL sorts NULLS LAST ascending / FIRST descending, which
            // the LEFT-JOIN misses rely on.
            let key = |d: &Document| d.get("__join_order").cloned().unwrap_or(Bson::Null);
            out.sort_by(|a, b| {
                let (ka, kb) = (key(a), key(b));
                let ord = match (&ka, &kb) {
                    (Bson::Null, Bson::Null) => std::cmp::Ordering::Equal,
                    (Bson::Null, _) => std::cmp::Ordering::Greater,
                    (_, Bson::Null) => std::cmp::Ordering::Less,
                    _ => secantus_pgplan::compare_values(&ka, &kb)
                        .unwrap_or(std::cmp::Ordering::Equal),
                };
                if *ascending {
                    ord
                } else {
                    ord.reverse()
                }
            });
            for d in &mut out {
                d.remove("__join_order");
            }
        }
        Ok(out)
    }

    /// Every `__sql_*` catalog collection, created (empty) before a
    /// transaction handle opens, so their registry rows are committed on
    /// their own rather than as part of the user's block.
    ///
    /// Lazily creating one inside the block was a real bug (2026-09-09): the
    /// first `CREATE TABLE` in a store registered `__sql_catalog__` in
    /// connection 1's uncommitted transaction, and a `CREATE TABLE` of a
    /// DIFFERENT table on connection 2 then wrote the same registry row and
    /// failed with a WiredTiger `WriteConflict` -- where PostgreSQL runs the
    /// two independently. Creating them outside the block but per statement
    /// was not enough either: a WiredTiger transaction reads its snapshot,
    /// so a block that began before the registry row landed could not see
    /// it, tried to register the collection again, and hit the same
    /// conflict (`create schema s; create type e as enum (...)` on a fresh
    /// store did exactly that). The rows are server bookkeeping, not
    /// something a `ROLLBACK` should undo, so they belong before the block.
    const CATALOG_COLLECTIONS: [&'static str; 7] = [
        CATALOG_COLLECTION,
        SEQUENCE_COLLECTION,
        Self::SCHEMA_COLLECTION,
        Self::COMPOSITE_COLLECTION,
        Self::ENUM_COLLECTION,
        Self::ENUM_META_COLLECTION,
        Self::RANGE_COLLECTION,
    ];

    /// Open a transaction handle, with every catalog collection registered
    /// first (see `CATALOG_COLLECTIONS`). The WiredTiger transaction itself
    /// begins lazily on the first statement, so the rows are committed and
    /// visible before its snapshot is taken.
    fn open_transaction_handle(&self) -> PgWireResult<UserTransactionHandle> {
        for coll in Self::CATALOG_COLLECTIONS {
            self.ensure_collection(coll)?;
        }
        self.txn_catalog_version.store(
            catalog_cache()
                .version
                .load(std::sync::atomic::Ordering::SeqCst),
            std::sync::atomic::Ordering::SeqCst,
        );
        self.storage
            .begin_user_transaction()
            .map_err(|e| Self::storage_err("could not begin a transaction", e))
    }

    /// Make sure a catalog collection exists before writing to it: a delete or
    /// insert against a collection nobody created yet is a WiredTiger ENOENT,
    /// not a no-op. Reads tolerate the absence; writes must not.
    fn ensure_collection(&self, coll: &str) -> PgWireResult<()> {
        let exists = self
            .storage
            .collection_exists(self.db(), coll)
            .map_err(|e| Self::storage_err("could not check a catalog collection", e))?;
        if !exists {
            self.storage
                .create_collection(self.db(), coll)
                .map_err(|e| Self::storage_err("could not create a catalog collection", e))?;
        }
        Ok(())
    }

    /// Every doc in one type-catalog collection, with this transaction's
    /// uncommitted creates/drops overlaid on the committed rows. Keyed by
    /// `_id`, so an uncommitted create shadows (and a tombstone hides) the
    /// committed row of the same name. This is what makes `CREATE TYPE t;
    /// SELECT 't'::regtype` in one transaction resolve `t`, since planning
    /// reads the catalog OUTSIDE the transaction and a plain read misses the
    /// uncommitted write.
    ///
    /// Shared, not copied: with no overlay this is the cache's own `Arc`, so
    /// a statement that consults the catalog several times (the planner
    /// install, then one wire-type lookup per described column) decodes and
    /// clones nothing. The per-statement cost used to grow with every table
    /// the session had ever created -- each one leaves a row type here --
    /// until a plain `select 1` ran twice as slowly on a used store.
    fn type_catalog_docs(&self, collection: &'static str) -> PgWireResult<Arc<Vec<Document>>> {
        let committed = self.committed_type_catalog_docs(collection)?;
        let overlay = self
            .uncommitted_types
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if overlay.is_empty() {
            return Ok(committed);
        }
        let mut by_id: std::collections::BTreeMap<String, Document> = committed
            .iter()
            .map(|d| (d.get_str("_id").unwrap_or_default().to_string(), d.clone()))
            .collect();
        for ((coll, id), doc) in overlay.iter() {
            if *coll != collection {
                continue;
            }
            match doc {
                Some(d) => {
                    by_id.insert(id.clone(), d.clone());
                }
                None => {
                    by_id.remove(id);
                }
            }
        }
        Ok(Arc::new(by_id.into_values().collect()))
    }

    /// The COMMITTED rows of one type-catalog collection, `_id`-sorted, from
    /// the process-wide cache when its version is current and from storage
    /// (decoding once for everyone) when it is not. See `CatalogCache`.
    fn committed_type_catalog_docs(
        &self,
        collection: &'static str,
    ) -> PgWireResult<Arc<Vec<Document>>> {
        let cache = catalog_cache();
        let version = cache.version.load(std::sync::atomic::Ordering::SeqCst);
        let key = (
            Arc::as_ptr(&self.storage) as usize,
            self.db().to_string(),
            collection,
        );
        if let Some((v, docs)) = cache
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            if *v == version {
                return Ok(Arc::clone(docs));
            }
        }
        let raw = self
            .storage
            .find_matching(self.db(), collection, &Document::new())
            .map_err(|e| Self::storage_err("could not read the type catalog", e))?;
        let mut by_id: std::collections::BTreeMap<String, Document> =
            std::collections::BTreeMap::new();
        for bytes in raw {
            let d: Document = bson::from_slice(&bytes)
                .map_err(|e| Self::storage_err("could not decode a type", e))?;
            let id = d.get_str("_id").unwrap_or_default().to_string();
            by_id.insert(id, d);
        }
        let docs: Arc<Vec<Document>> = Arc::new(by_id.into_values().collect());
        // A read that was in flight while the version moved must not be
        // recorded as current: store it under the version it was read AT,
        // so a bump during the read still forces the next reader to storage.
        if self.may_fill_catalog_cache(version) {
            cache
                .entries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (version, Arc::clone(&docs)));
        }
        Ok(docs)
    }

    /// May a catalog row this session just read at `version` be published
    /// as the committed truth? Outside a transaction handle, always; under
    /// one, only while the catalog has not moved since the handle opened --
    /// by this block (its own uncommitted DDL) or by anyone else (a commit
    /// this block's snapshot may predate). See `CatalogCache`.
    fn may_fill_catalog_cache(&self, version: u64) -> bool {
        !self.transaction_handle_open()
            || self
                .txn_catalog_version
                .load(std::sync::atomic::Ordering::SeqCst)
                == version
    }

    /// Record that the open transaction created (`Some(doc)`) or dropped
    /// (`None`) a user type in `collection`, so later statements in the SAME
    /// transaction see it before it is committed. A no-op outside a
    /// transaction: an autocommit statement's write is committed at once and
    /// a plain read already finds it.
    fn note_uncommitted_type(&self, collection: &'static str, id: &str, doc: Option<Document>) {
        if !self.transaction_handle_open() {
            return;
        }
        self.uncommitted_types
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((collection, id.to_string()), doc);
    }

    /// Every composite type: `(name, oid, [(field, type_name)])`, name-sorted.
    /// The Python server's `__sql_composites__` shape: a doc `{composite, oid,
    /// fields: [[name, tag, sub], ...]}`, where `tag` is the field's SQL type
    /// name and `sub` is a nested composite's fields (unused here).
    fn composites(&self) -> PgWireResult<Vec<(String, i64, CompositeFields)>> {
        let mut out = Vec::new();
        for d in self.type_catalog_docs(Self::COMPOSITE_COLLECTION)?.iter() {
            let name = d.get_str("composite").unwrap_or_default().to_string();
            let oid = d
                .get_i64("oid")
                .or_else(|_| d.get_i32("oid").map(i64::from))
                .unwrap_or(0);
            let fields = Self::composite_fields(d);
            out.push((name, oid, fields));
        }
        out.sort();
        Ok(out)
    }

    /// A composite catalog doc's `fields`: `[[name, type, sub], ...]` as
    /// `(name, type)` pairs.
    fn composite_fields(d: &Document) -> CompositeFields {
        d.get_array("fields")
            .map(|items| {
                items
                    .iter()
                    .filter_map(|f| match f {
                        Bson::Array(pair) => {
                            let n = pair.first()?.as_str()?.to_string();
                            let t = pair.get(1)?.as_str()?.to_string();
                            Some((n, t))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One composite by its resolution name (`name` in public, else
    /// `schema.name`), as `composites_with_schema` would list it -- decoding
    /// only that one, since a described column asks for exactly one type.
    fn composite_with_schema_named(
        &self,
        resolution: &str,
    ) -> PgWireResult<Option<(String, String, i64, CompositeFields)>> {
        for d in self.type_catalog_docs(Self::COMPOSITE_COLLECTION)?.iter() {
            let name = d.get_str("composite").unwrap_or_default();
            let schema = d.get_str("schema").unwrap_or("public");
            if Self::type_resolution(schema, name) != resolution {
                continue;
            }
            let oid = d
                .get_i64("oid")
                .or_else(|_| d.get_i32("oid").map(i64::from))
                .unwrap_or(0);
            return Ok(Some((
                schema.to_string(),
                name.to_string(),
                oid,
                Self::composite_fields(d),
            )));
        }
        Ok(None)
    }

    /// Is `name` the ROW TYPE of a table (a composite recorded by CREATE
    /// TABLE, `relation: true`) rather than a CREATE TYPE composite?
    fn row_type_of_table(&self, name: &str) -> PgWireResult<bool> {
        Ok(self
            .type_catalog_docs(Self::COMPOSITE_COLLECTION)?
            .iter()
            .any(|d| {
                d.get_str("_id").unwrap_or_default() == name
                    && d.get_bool("relation").unwrap_or(false)
            }))
    }

    /// Every composite as `(schema, bare_name, oid, fields)`. `schema` defaults
    /// to `public` for a composite stored before schema qualification (no
    /// `schema` field), so the bare-name resolution is unchanged for those.
    /// `pg_type.typname` and `pg_attribute` still use the BARE name; only
    /// duplicate-checking and `to_regtype` resolution consult the schema.
    fn composites_with_schema(&self) -> PgWireResult<Vec<(String, String, i64, CompositeFields)>> {
        let mut out = Vec::new();
        for d in self.type_catalog_docs(Self::COMPOSITE_COLLECTION)?.iter() {
            let name = d.get_str("composite").unwrap_or_default().to_string();
            let schema = d.get_str("schema").unwrap_or("public").to_string();
            let oid = d
                .get_i64("oid")
                .or_else(|_| d.get_i32("oid").map(i64::from))
                .unwrap_or(0);
            let fields = Self::composite_fields(d);
            out.push((schema, name, oid, fields));
        }
        out.sort();
        Ok(out)
    }

    /// The `__sql_ranges__` catalog: a doc `{range, oid, subtype}` per custom
    /// range type. `subtype` is the element type name (e.g. `int4`).
    fn ranges(&self) -> PgWireResult<Vec<(String, i64, String)>> {
        let mut out = Vec::new();
        for d in self.type_catalog_docs(Self::RANGE_COLLECTION)?.iter() {
            let name = d.get_str("range").unwrap_or_default().to_string();
            let oid = d
                .get_i64("oid")
                .or_else(|_| d.get_i32("oid").map(i64::from))
                .unwrap_or(0);
            let subtype = d.get_str("subtype").unwrap_or_default().to_string();
            out.push((name, oid, subtype));
        }
        out.sort();
        Ok(out)
    }

    /// Every custom range as `(schema, bare_name, oid, subtype)`. `schema`
    /// defaults to `public` for a range stored before schema qualification, so
    /// the bare-name resolution is unchanged for those. `pg_type.typname` still
    /// uses the BARE name (PostgreSQL keeps a range's typname unqualified); only
    /// duplicate-checking and `to_regtype` resolution consult the schema.
    fn ranges_with_schema(&self) -> PgWireResult<Vec<(String, String, i64, String)>> {
        let mut out = Vec::new();
        for d in self.type_catalog_docs(Self::RANGE_COLLECTION)?.iter() {
            let name = d.get_str("range").unwrap_or_default().to_string();
            let schema = d.get_str("schema").unwrap_or("public").to_string();
            let oid = d
                .get_i64("oid")
                .or_else(|_| d.get_i32("oid").map(i64::from))
                .unwrap_or(0);
            let subtype = d.get_str("subtype").unwrap_or_default().to_string();
            out.push((schema, name, oid, subtype));
        }
        out.sort();
        Ok(out)
    }

    /// The `__sql_base_types__` catalog, name-sorted. See the constant.
    fn base_types(&self) -> PgWireResult<Vec<BaseType>> {
        let mut out = Vec::new();
        for d in self.type_catalog_docs(Self::BASE_TYPE_COLLECTION)?.iter() {
            out.push(BaseType {
                name: d.get_str("base").unwrap_or_default().to_string(),
                schema: d.get_str("schema").unwrap_or("public").to_string(),
                oid: d
                    .get_i64("oid")
                    .or_else(|_| d.get_i32("oid").map(i64::from))
                    .unwrap_or(0),
                defined: d.get_bool("defined").unwrap_or(false),
                input: d.get_str("input").ok().map(str::to_string),
                output: d.get_str("output").ok().map(str::to_string),
            });
        }
        out.sort();
        Ok(out)
    }

    /// The `__sql_functions__` catalog (the Python server's shape, see the
    /// constant), name-sorted.
    fn functions(&self) -> PgWireResult<Vec<UserFunction>> {
        let mut out = Vec::new();
        for d in self.type_catalog_docs(Self::FUNCTION_COLLECTION)?.iter() {
            let strings = |key: &str| -> Vec<String> {
                d.get_array(key)
                    .map(|items| {
                        items
                            .iter()
                            .map(|v| v.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            out.push(UserFunction {
                name: d.get_str("name").unwrap_or_default().to_string(),
                param_types: strings("param_types"),
                return_type: d.get_str("return_tag").unwrap_or_default().to_string(),
                language: d.get_str("language").unwrap_or_default().to_string(),
            });
        }
        out.sort();
        Ok(out)
    }

    /// A function's signature as PostgreSQL prints it in messages:
    /// `invin(cstring)`, `invout("a-b")` -- each argument type by its display
    /// name, a user type quoted as an identifier.
    fn function_signature(&self, f: &UserFunction) -> String {
        let args: Vec<String> = f
            .param_types
            .iter()
            .map(|t| self.display_type_name(t))
            .collect();
        format!("{}({})", f.name, args.join(", "))
    }

    /// A type name as PostgreSQL displays it: a builtin by its display name
    /// (`integer`), a user type quoted when its spelling needs it (`"a-b"`).
    fn display_type_name(&self, name: &str) -> String {
        if secantus_pgplan::pgtypes::oid_of_name(name).is_some()
            || name == "cstring"
            || name.ends_with("[]")
        {
            return secantus_pgplan::display_type(name);
        }
        secantus_pgplan::scalar::quote_identifier(name)
    }

    /// A type name's oid, resolving BUILTINS first, then user types
    /// (composites, enums, ranges). A composite field whose type is itself a
    /// user type -- `CREATE TYPE t AS (sub other_composite)` -- resolves here;
    /// the builtin-only lookup dropped it from `pg_attribute`.
    fn type_oid_by_name(&self, name: &str) -> Option<i64> {
        if let Some(oid) = secantus_pgplan::pgtypes::oid_of_name(name) {
            return Some(oid);
        }
        if let Ok(cs) = self.composites() {
            if let Some((_, oid, _)) = cs.iter().find(|(n, _, _)| n == name) {
                return Some(*oid);
            }
        }
        if let Ok(es) = self.enums() {
            if let Some((_, oid, _)) = es.iter().find(|(n, _, _)| n == name) {
                return Some(*oid);
            }
        }
        if let Ok(rs) = self.ranges() {
            if let Some((_, oid, _)) = rs.iter().find(|(n, _, _)| n == name) {
                return Some(*oid);
            }
        }
        if let Ok(bs) = self.base_types() {
            if let Some(b) = bs.iter().find(|b| b.name == name) {
                return Some(b.oid);
            }
        }
        None
    }

    /// Mint the next base-type oid -- the range minting rule, base 71000.
    fn mint_base_type_oid(&self) -> PgWireResult<i64> {
        self.mint_oid_outside_transaction(|| {
            let existing = self.base_types()?;
            Ok(match existing.iter().map(|b| b.oid).max() {
                Some(taken) => {
                    (Self::BASE_TYPE_OID_BASE + existing.len() as i64 - 1).max(taken) + 1
                }
                None => Self::BASE_TYPE_OID_BASE,
            })
        })
    }

    /// Is `name` taken by ANY type in the default search_path -- an enum, a
    /// composite, a range, a base type (shell or not), or a builtin? The
    /// 42710 `type "x" already exists` gate every CREATE TYPE shares.
    fn type_name_taken(&self, name: &str) -> PgWireResult<bool> {
        Ok(self.composites()?.iter().any(|(n, _, _)| n == name)
            || self.enums()?.iter().any(|(n, _, _)| n == name)
            || self.ranges()?.iter().any(|(n, _, _)| n == name)
            || self.base_types()?.iter().any(|b| b.name == name)
            || secantus_pgplan::pgtypes::oid_of_name(name).is_some())
    }

    /// Queue a NOTICE for the statement in flight.
    fn notice(&self, sqlstate: &str, message: String, detail: Option<String>) {
        let mut info = ErrorInfo::new("NOTICE".into(), sqlstate.into(), message);
        info.detail = detail;
        self.pending_notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(info);
    }

    /// PostgreSQL's CASCADE notice: one dependent is named in the message
    /// (`drop cascades to function shin(cstring)`); two or more are counted
    /// there and listed one per DETAIL line (measured on 16).
    fn cascade_notice(&self, descs: &[String]) {
        match descs {
            [] => {}
            [one] => self.notice("00000", format!("drop cascades to {one}"), None),
            many => {
                let lines: Vec<String> = many
                    .iter()
                    .map(|d| format!("drop cascades to {d}"))
                    .collect();
                self.notice(
                    "00000",
                    format!("drop cascades to {} other objects", many.len()),
                    Some(lines.join("\n")),
                );
            }
        }
    }

    /// The base type (shell or defined) resolving under `key`
    /// (`type_resolution` form), if any.
    fn base_type_named(&self, key: &str) -> PgWireResult<Option<BaseType>> {
        Ok(self
            .base_types()?
            .into_iter()
            .find(|b| Self::type_resolution(&b.schema, &b.name) == key))
    }

    /// The catalog key a function's declared parameter / return type is
    /// recorded under: the planner's canonical spelling, with an array
    /// suffix kept as written.
    fn function_type_key(&self, declared: &str) -> PgWireResult<String> {
        Ok(declared.to_string())
    }

    /// Whether a type name is defined, a shell, or nothing at all. An array
    /// (`t[]`) is as defined as its element type.
    fn type_kind(&self, key: &str) -> PgWireResult<TypeKind> {
        let element = key.strip_suffix("[]").unwrap_or(key);
        if element == "cstring"
            || element == "void"
            || element == "trigger"
            || element == "record"
            || secantus_pgplan::pgtypes::oid_of_name(element).is_some()
        {
            return Ok(TypeKind::Defined);
        }
        if let Some(b) = self.base_type_named(element)? {
            return Ok(if b.defined {
                TypeKind::Defined
            } else {
                TypeKind::Shell
            });
        }
        if self.composites()?.iter().any(|(n, _, _)| n == element)
            || self.enums()?.iter().any(|(n, _, _)| n == element)
            || self.ranges()?.iter().any(|(n, _, _)| n == element)
        {
            return Ok(TypeKind::Defined);
        }
        Ok(TypeKind::Unknown)
    }

    /// The built-ins a `LANGUAGE internal` wrapper may name: every builtin
    /// type's `<name>in` / `<name>out` pair plus the array pair. Anything
    /// else is PostgreSQL's 42883 `there is no built-in function named`.
    fn is_builtin_function(body: &str) -> bool {
        if body == "array_in" || body == "array_out" {
            return true;
        }
        secantus_pgplan::pgtypes::BUILTIN_TYPES
            .iter()
            .any(|(name, _, _)| body == format!("{name}in") || body == format!("{name}out"))
    }

    /// Insert one catalog doc and note it for the open transaction.
    fn insert_type_doc(
        &self,
        collection: &'static str,
        id: &str,
        doc: Document,
    ) -> PgWireResult<()> {
        let bytes =
            bson::to_vec(&doc).map_err(|e| Self::storage_err("could not encode the catalog", e))?;
        self.storage
            .insert(self.db(), collection, vec![bytes], true)
            .map_err(|e| Self::storage_err("could not record the catalog", e))?;
        self.note_uncommitted_type(collection, id, Some(doc));
        Ok(())
    }

    /// Delete one catalog doc by `_id` and tombstone it for the open
    /// transaction.
    fn delete_type_doc(&self, collection: &'static str, id: &str) -> PgWireResult<()> {
        self.ensure_collection(collection)?;
        let filter = bson::doc! {"_id": id};
        self.storage
            .delete_matching(self.db(), collection, &filter, 0, &Document::new(), None)
            .map_err(|e| Self::storage_err("could not drop the catalog row", e))?;
        self.note_uncommitted_type(collection, id, None);
        Ok(())
    }

    /// Mint an enum / composite oid the way PostgreSQL mints an OID: from a
    /// counter OUTSIDE the user's transaction, under one process-wide lock.
    /// Advanced inside the block, the counter row was one key that two open
    /// `CREATE TYPE` blocks both rewrote, and the second failed with a
    /// WiredTiger write conflict where PostgreSQL runs them independently
    /// (2026-09-09). An oid minted by a block that rolls back is simply
    /// skipped, as PostgreSQL skips one. The lock makes the read-rewrite of
    /// the counter atomic across connections.
    fn mint_oid_outside_transaction(
        &self,
        mint: impl FnOnce() -> PgWireResult<i64>,
    ) -> PgWireResult<i64> {
        static OID_MINT: Mutex<()> = Mutex::new(());
        let _serial = OID_MINT.lock().unwrap_or_else(|e| e.into_inner());
        self.storage.outside_user_transaction(mint)
    }

    fn mint_enum_oid(&self) -> PgWireResult<i64> {
        self.mint_oid_outside_transaction(|| self.mint_enum_oid_in_session())
    }

    fn mint_composite_oid(&self) -> PgWireResult<i64> {
        self.mint_oid_outside_transaction(|| self.mint_composite_oid_in_session())
    }

    /// Mint the next range-type oid -- the enum minting rule, base 69000.
    fn mint_range_oid(&self) -> PgWireResult<i64> {
        let existing = self.ranges()?;
        let oid = match existing.iter().map(|(_, o, _)| *o).max() {
            Some(taken) => (Self::RANGE_TYPE_OID_BASE + existing.len() as i64 - 1).max(taken) + 1,
            None => Self::RANGE_TYPE_OID_BASE,
        };
        Ok(oid)
    }

    /// Mint the next composite oid -- the enum minting rule, own counter and
    /// base 67000, monotonic and never reused.
    fn mint_composite_oid_in_session(&self) -> PgWireResult<i64> {
        let key = "composite_oid_counter";
        let counter = self
            .storage
            .find_matching(
                self.db(),
                Self::ENUM_META_COLLECTION,
                &bson::doc! {"_id": key},
            )
            .map_err(|e| Self::storage_err("could not read the oid counter", e))?;
        let oid = if let Some(bytes) = counter.first() {
            let d: Document = bson::from_slice(bytes)
                .map_err(|e| Self::storage_err("could not decode the oid counter", e))?;
            d.get_i64("next")
                .or_else(|_| d.get_i32("next").map(i64::from))
                .unwrap_or(Self::COMPOSITE_TYPE_OID_BASE)
        } else {
            let existing = self.composites()?;
            match existing.iter().map(|(_, o, _)| *o).max() {
                Some(taken) => {
                    (Self::COMPOSITE_TYPE_OID_BASE + existing.len() as i64 - 1).max(taken) + 1
                }
                None => Self::COMPOSITE_TYPE_OID_BASE,
            }
        };
        self.storage
            .delete_matching(
                self.db(),
                Self::ENUM_META_COLLECTION,
                &bson::doc! {"_id": key},
                0,
                &Document::new(),
                None,
            )
            .map_err(|e| Self::storage_err("could not advance the oid counter", e))?;
        let doc = bson::doc! {"_id": key, "next": oid + 1};
        let bytes = bson::to_vec(&doc)
            .map_err(|e| Self::storage_err("could not encode the oid counter", e))?;
        self.storage
            .insert(self.db(), Self::ENUM_META_COLLECTION, vec![bytes], true)
            .map_err(|e| Self::storage_err("could not advance the oid counter", e))?;
        Ok(oid)
    }

    /// Every enum type: `(name, oid, labels)`, name-sorted for stable output.
    fn enums(&self) -> PgWireResult<Vec<(String, i64, Vec<String>)>> {
        let mut out = Vec::new();
        for d in self.type_catalog_docs(Self::ENUM_COLLECTION)?.iter() {
            let name = d.get_str("enum").unwrap_or_default().to_string();
            let oid = d
                .get_i64("oid")
                .or_else(|_| d.get_i32("oid").map(i64::from))
                .unwrap_or(0);
            let labels = d
                .get_array("labels")
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            out.push((name, oid, labels));
        }
        out.sort();
        Ok(out)
    }

    /// Every enum as `(schema, bare_name, oid, labels)`. `schema` defaults to
    /// `public` for an enum stored before schema qualification, so the bare-name
    /// resolution is unchanged for those. `pg_type.typname` still uses the BARE
    /// name; only duplicate-checking and `to_regtype` resolution consult the
    /// schema.
    fn enums_with_schema(&self) -> PgWireResult<Vec<EnumWithSchema>> {
        let mut out = Vec::new();
        for d in self.type_catalog_docs(Self::ENUM_COLLECTION)?.iter() {
            let name = d.get_str("enum").unwrap_or_default().to_string();
            let schema = d.get_str("schema").unwrap_or("public").to_string();
            let oid = d
                .get_i64("oid")
                .or_else(|_| d.get_i32("oid").map(i64::from))
                .unwrap_or(0);
            let labels = d
                .get_array("labels")
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            out.push((schema, name, oid, labels));
        }
        out.sort();
        Ok(out)
    }

    /// Mint the next enum oid, exactly as the Python server does: read the
    /// counter, else start above everything already taken; rewrite the counter
    /// as `next = oid + 1`. Monotonic and never reused -- positional minting
    /// would renumber types under a client that registered loaders by oid.
    fn mint_enum_oid_in_session(&self) -> PgWireResult<i64> {
        let counter = self
            .storage
            .find_matching(
                self.db(),
                Self::ENUM_META_COLLECTION,
                &bson::doc! {"_id": "oid_counter"},
            )
            .map_err(|e| Self::storage_err("could not read the oid counter", e))?;
        let oid = if let Some(bytes) = counter.first() {
            let d: Document = bson::from_slice(bytes)
                .map_err(|e| Self::storage_err("could not decode the oid counter", e))?;
            d.get_i64("next")
                .or_else(|_| d.get_i32("next").map(i64::from))
                .unwrap_or(Self::ENUM_TYPE_OID_BASE)
        } else {
            let existing = self.enums()?;
            match existing.iter().map(|(_, o, _)| *o).max() {
                Some(taken) => {
                    (Self::ENUM_TYPE_OID_BASE + existing.len() as i64 - 1).max(taken) + 1
                }
                None => Self::ENUM_TYPE_OID_BASE,
            }
        };
        self.storage
            .delete_matching(
                self.db(),
                Self::ENUM_META_COLLECTION,
                &bson::doc! {"_id": "oid_counter"},
                0,
                &Document::new(),
                None,
            )
            .map_err(|e| Self::storage_err("could not advance the oid counter", e))?;
        let doc = bson::doc! {"_id": "oid_counter", "next": oid + 1};
        let bytes = bson::to_vec(&doc)
            .map_err(|e| Self::storage_err("could not encode the oid counter", e))?;
        self.storage
            .insert(self.db(), Self::ENUM_META_COLLECTION, vec![bytes], true)
            .map_err(|e| Self::storage_err("could not advance the oid counter", e))?;
        Ok(oid)
    }

    /// The virtual catalog tables: a definition and rows COMPUTED on read,
    /// nothing stored. `pg_type` is what psycopg's `TypeInfo.fetch` reads to
    /// learn a type's oid and array oid; `pg_prepared_statements` is what its
    /// pipeline tests count (empty here -- the statement store lives in the
    /// wire layer and holds nothing a client named).
    fn virtual_table(name: &str) -> Option<TableDef> {
        match name {
            "pg_type" => Some(TableDef::new(
                "pg_type",
                vec![
                    secantus_pgcatalog::Column::new("typname", "name", false),
                    secantus_pgcatalog::Column::new("oid", "int8", false),
                    secantus_pgcatalog::Column::new("typarray", "int8", false),
                    secantus_pgcatalog::Column::new("typdelim", "text", false),
                    // A composite's row type: its own oid here, 0 otherwise.
                    // `pg_attribute` keys a composite's fields on it.
                    secantus_pgcatalog::Column::new("typrelid", "oid", false),
                ],
            )),
            "pg_attribute" => Some(TableDef::new(
                "pg_attribute",
                vec![
                    secantus_pgcatalog::Column::new("attrelid", "oid", false),
                    secantus_pgcatalog::Column::new("attname", "name", false),
                    secantus_pgcatalog::Column::new("atttypid", "oid", false),
                    secantus_pgcatalog::Column::new("attnum", "int2", false),
                    secantus_pgcatalog::Column::new("attisdropped", "bool", false),
                ],
            )),
            "pg_range" => Some(TableDef::new(
                "pg_range",
                vec![
                    secantus_pgcatalog::Column::new("rngtypid", "oid", false),
                    secantus_pgcatalog::Column::new("rngsubtype", "oid", false),
                    // The oid of the range's auto-created multirange companion.
                    // psycopg's `MultirangeInfo.fetch` joins pg_type to pg_range
                    // ON `t.oid = r.rngmultitypid`, so this must be present and
                    // point at the multirange's `pg_type` row.
                    secantus_pgcatalog::Column::new("rngmultitypid", "oid", false),
                ],
            )),
            "pg_enum" => Some(TableDef::new(
                "pg_enum",
                vec![
                    secantus_pgcatalog::Column::new("enumtypid", "oid", false),
                    secantus_pgcatalog::Column::new("enumsortorder", "float4", false),
                    secantus_pgcatalog::Column::new("enumlabel", "name", false),
                ],
            )),
            // PostgreSQL 16's column set and types, measured: `parameter_types`
            // and `result_types` are `regtype[]` (oid 2211) of DISPLAY names,
            // `from_sql` is false for a protocol-prepared statement, and the
            // plan counters are `int8` -- a statement with parameters has had
            // one custom plan and no generic one, a statement without has the
            // reverse.
            "pg_prepared_statements" => Some(TableDef::new(
                "pg_prepared_statements",
                vec![
                    secantus_pgcatalog::Column::new("name", "text", false),
                    secantus_pgcatalog::Column::new("statement", "text", false),
                    secantus_pgcatalog::Column::new("prepare_time", "timestamptz", false),
                    secantus_pgcatalog::Column::new("parameter_types", "regtype[]", false),
                    secantus_pgcatalog::Column::new("result_types", "regtype[]", true),
                    secantus_pgcatalog::Column::new("from_sql", "bool", false),
                    secantus_pgcatalog::Column::new("generic_plans", "int8", false),
                    secantus_pgcatalog::Column::new("custom_plans", "int8", false),
                ],
            )),
            // The open cursors of THIS connection, in PostgreSQL's column order.
            // psycopg's server cursor reads it (`SELECT 1 FROM pg_cursors WHERE
            // name = ...`) to check a cursor exists before closing one it did
            // not declare, and the suite queries it directly to prove a cursor
            // is gone after close.
            // Every live backend of this server, PostgreSQL 16's column set
            // in its order; the columns a single-node server has no value
            // for (client address, wait events, xids, query_id) are NULL.
            "pg_database" => Some(TableDef::new(
                "pg_database",
                vec![
                    secantus_pgcatalog::Column::new("oid", "oid", false),
                    secantus_pgcatalog::Column::new("datname", "name", false),
                    secantus_pgcatalog::Column::new("datdba", "oid", false),
                    secantus_pgcatalog::Column::new("encoding", "int4", false),
                    secantus_pgcatalog::Column::new("datlocprovider", "char", false),
                    secantus_pgcatalog::Column::new("datistemplate", "bool", false),
                    secantus_pgcatalog::Column::new("datallowconn", "bool", false),
                    secantus_pgcatalog::Column::new("datconnlimit", "int4", false),
                    secantus_pgcatalog::Column::new("datfrozenxid", "xid", false),
                    secantus_pgcatalog::Column::new("datminmxid", "xid", false),
                    secantus_pgcatalog::Column::new("dattablespace", "oid", false),
                    secantus_pgcatalog::Column::new("datcollate", "text", false),
                    secantus_pgcatalog::Column::new("datctype", "text", false),
                    secantus_pgcatalog::Column::new("daticulocale", "text", false),
                    secantus_pgcatalog::Column::new("daticurules", "text", false),
                    secantus_pgcatalog::Column::new("datcollversion", "text", false),
                    secantus_pgcatalog::Column::new("datacl", "text", false),
                ],
            )),
            "pg_stat_activity" => Some(TableDef::new(
                "pg_stat_activity",
                vec![
                    secantus_pgcatalog::Column::new("datid", "oid", false),
                    secantus_pgcatalog::Column::new("datname", "name", false),
                    secantus_pgcatalog::Column::new("pid", "int4", false),
                    secantus_pgcatalog::Column::new("leader_pid", "int4", false),
                    secantus_pgcatalog::Column::new("usesysid", "oid", false),
                    secantus_pgcatalog::Column::new("usename", "name", false),
                    secantus_pgcatalog::Column::new("application_name", "text", false),
                    secantus_pgcatalog::Column::new("client_addr", "inet", false),
                    secantus_pgcatalog::Column::new("client_hostname", "text", false),
                    secantus_pgcatalog::Column::new("client_port", "int4", false),
                    secantus_pgcatalog::Column::new("backend_start", "timestamptz", false),
                    secantus_pgcatalog::Column::new("xact_start", "timestamptz", false),
                    secantus_pgcatalog::Column::new("query_start", "timestamptz", false),
                    secantus_pgcatalog::Column::new("state_change", "timestamptz", false),
                    secantus_pgcatalog::Column::new("wait_event_type", "text", false),
                    secantus_pgcatalog::Column::new("wait_event", "text", false),
                    secantus_pgcatalog::Column::new("state", "text", false),
                    secantus_pgcatalog::Column::new("backend_xid", "xid", false),
                    secantus_pgcatalog::Column::new("backend_xmin", "xid", false),
                    secantus_pgcatalog::Column::new("query_id", "int8", false),
                    secantus_pgcatalog::Column::new("query", "text", false),
                    secantus_pgcatalog::Column::new("backend_type", "text", false),
                ],
            )),
            // PostgreSQL 16's `pg_tables` view: every user table (and the
            // catalog's own, so `where schemaname = 'pg_catalog'` finds
            // `pg_class`). No index / rule / trigger / row-security support
            // here, so those flags are false; `tablespace` is NULL as it is
            // for a table in the default tablespace.
            "pg_tables" => Some(TableDef::new(
                "pg_tables",
                vec![
                    secantus_pgcatalog::Column::new("schemaname", "name", false),
                    secantus_pgcatalog::Column::new("tablename", "name", false),
                    secantus_pgcatalog::Column::new("tableowner", "name", false),
                    secantus_pgcatalog::Column::new("tablespace", "name", false),
                    secantus_pgcatalog::Column::new("hasindexes", "bool", false),
                    secantus_pgcatalog::Column::new("hasrules", "bool", false),
                    secantus_pgcatalog::Column::new("hastriggers", "bool", false),
                    secantus_pgcatalog::Column::new("rowsecurity", "bool", false),
                ],
            )),
            "pg_cursors" => Some(TableDef::new(
                "pg_cursors",
                vec![
                    secantus_pgcatalog::Column::new("name", "text", false),
                    secantus_pgcatalog::Column::new("statement", "text", false),
                    secantus_pgcatalog::Column::new("is_holdable", "bool", false),
                    secantus_pgcatalog::Column::new("is_binary", "bool", false),
                    secantus_pgcatalog::Column::new("is_scrollable", "bool", false),
                    secantus_pgcatalog::Column::new("creation_time", "timestamptz", false),
                ],
            )),
            _ => None,
        }
    }

    /// The rows of one virtual table, already filtered.
    fn virtual_rows(&self, name: &str, filter: &Document) -> Option<Vec<Document>> {
        let def = Self::virtual_table(name)?;
        let rows: Vec<Document> = match name {
            "pg_type" => {
                let mut rows: Vec<Document> = secantus_pgplan::pgtypes::BUILTIN_TYPES
                    .iter()
                    .map(|(typname, oid, typarray)| {
                        let mut d = Document::new();
                        d.insert(def.field_of("typname").expect("column"), *typname);
                        d.insert(def.field_of("oid").expect("column"), Bson::Int64(*oid));
                        d.insert(
                            def.field_of("typarray").expect("column"),
                            Bson::Int64(*typarray),
                        );
                        d.insert(
                            def.field_of("typdelim").expect("column"),
                            secantus_pgplan::pgtypes::typdelim(typname).to_string(),
                        );
                        d.insert(def.field_of("typrelid").expect("column"), Bson::Int64(0));
                        d
                    })
                    .collect();
                // User enums ride along, their typarray DERIVED as
                // oid + 100_000 -- the shared-store rule, never stored.
                for (name, oid, _) in self.enums().ok()? {
                    let mut d = Document::new();
                    d.insert(def.field_of("typname").expect("column"), name);
                    d.insert(def.field_of("oid").expect("column"), Bson::Int64(oid));
                    d.insert(
                        def.field_of("typarray").expect("column"),
                        Bson::Int64(oid + Self::USER_TYPE_ARRAY_OID_OFFSET),
                    );
                    d.insert(def.field_of("typdelim").expect("column"), ",");
                    d.insert(def.field_of("typrelid").expect("column"), Bson::Int64(0));
                    rows.push(d);
                }
                // Composites likewise. `typrelid` is the composite's OWN oid
                // here -- pg_attribute keys its fields on it, and this query
                // needs no separate pg_class row.
                for (name, oid, _) in self.composites().ok()? {
                    let mut d = Document::new();
                    d.insert(def.field_of("typname").expect("column"), name);
                    d.insert(def.field_of("oid").expect("column"), Bson::Int64(oid));
                    d.insert(
                        def.field_of("typarray").expect("column"),
                        Bson::Int64(oid + Self::USER_TYPE_ARRAY_OID_OFFSET),
                    );
                    d.insert(def.field_of("typdelim").expect("column"), ",");
                    if let Some(f) = def.field_of("typrelid") {
                        d.insert(f, Bson::Int64(oid));
                    }
                    rows.push(d);
                }
                // Custom range types: their own oid, typarray derived, typrelid 0.
                // Each also has an auto-created MULTIRANGE companion row: its
                // typname is the multirange name (bare, like the range's), its
                // oid is range_oid + offset, its typarray derived from that.
                // psycopg's MultirangeInfo.fetch reads this row after resolving
                // `to_regtype('<mrname>')` and joining pg_range.rngmultitypid.
                for (name, oid, _) in self.ranges().ok()? {
                    let mut d = Document::new();
                    d.insert(def.field_of("typname").expect("column"), name.clone());
                    d.insert(def.field_of("oid").expect("column"), Bson::Int64(oid));
                    d.insert(
                        def.field_of("typarray").expect("column"),
                        Bson::Int64(oid + Self::USER_TYPE_ARRAY_OID_OFFSET),
                    );
                    d.insert(def.field_of("typdelim").expect("column"), ",");
                    d.insert(def.field_of("typrelid").expect("column"), Bson::Int64(0));
                    rows.push(d);

                    let mr_oid = oid + Self::MULTIRANGE_TYPE_OID_OFFSET;
                    let mut mr = Document::new();
                    mr.insert(
                        def.field_of("typname").expect("column"),
                        secantus_pgplan::range::multirange_name_for(&name),
                    );
                    mr.insert(def.field_of("oid").expect("column"), Bson::Int64(mr_oid));
                    mr.insert(
                        def.field_of("typarray").expect("column"),
                        Bson::Int64(mr_oid + Self::USER_TYPE_ARRAY_OID_OFFSET),
                    );
                    mr.insert(def.field_of("typdelim").expect("column"), ",");
                    mr.insert(def.field_of("typrelid").expect("column"), Bson::Int64(0));
                    rows.push(mr);
                }
                // Base types: `typarray` is 0 while the type is a SHELL --
                // PostgreSQL mints the array type only when the full CREATE
                // TYPE completes it (measured on 16) -- and derived after.
                for b in self.base_types().ok()? {
                    let mut d = Document::new();
                    d.insert(def.field_of("typname").expect("column"), b.name);
                    d.insert(def.field_of("oid").expect("column"), Bson::Int64(b.oid));
                    let typarray = if b.defined {
                        b.oid + Self::USER_TYPE_ARRAY_OID_OFFSET
                    } else {
                        0
                    };
                    d.insert(
                        def.field_of("typarray").expect("column"),
                        Bson::Int64(typarray),
                    );
                    d.insert(def.field_of("typdelim").expect("column"), ",");
                    d.insert(def.field_of("typrelid").expect("column"), Bson::Int64(0));
                    rows.push(d);
                }
                rows
            }
            // One row per builtin range type: (range oid, element oid). The
            // element name comes from `range_element`, so this and the range
            // casts cannot disagree about what a range is OVER.
            "pg_range" => {
                let ranges = [
                    "int4range",
                    "int8range",
                    "numrange",
                    "daterange",
                    "tsrange",
                    "tstzrange",
                ];
                let mut rows = Vec::new();
                for name in ranges {
                    let Some(rngtypid) = secantus_pgplan::pgtypes::oid_of_name(name) else {
                        continue;
                    };
                    let element = secantus_pgplan::range::range_element(name)
                        .map(|(e, _)| e)
                        .unwrap_or_default();
                    let Some(rngsubtype) = secantus_pgplan::pgtypes::oid_of_name(&element) else {
                        continue;
                    };
                    // The builtin multirange companion (int4range ->
                    // int4multirange, etc.), so a client can join here for it.
                    let rngmultitypid = secantus_pgplan::pgtypes::oid_of_name(
                        &secantus_pgplan::range::multirange_name_for(name),
                    )
                    .unwrap_or(0);
                    let mut d = Document::new();
                    d.insert(
                        def.field_of("rngtypid").expect("column"),
                        Bson::Int64(rngtypid),
                    );
                    d.insert(
                        def.field_of("rngsubtype").expect("column"),
                        Bson::Int64(rngsubtype),
                    );
                    d.insert(
                        def.field_of("rngmultitypid").expect("column"),
                        Bson::Int64(rngmultitypid),
                    );
                    rows.push(d);
                }
                // Custom range types: (range oid, subtype oid, multirange oid).
                for (_, oid, subtype) in self.ranges().ok()? {
                    let Some(rngsubtype) = secantus_pgplan::pgtypes::oid_of_name(&subtype) else {
                        continue;
                    };
                    let mut d = Document::new();
                    d.insert(def.field_of("rngtypid").expect("column"), Bson::Int64(oid));
                    d.insert(
                        def.field_of("rngsubtype").expect("column"),
                        Bson::Int64(rngsubtype),
                    );
                    d.insert(
                        def.field_of("rngmultitypid").expect("column"),
                        Bson::Int64(oid + Self::MULTIRANGE_TYPE_OID_OFFSET),
                    );
                    rows.push(d);
                }
                rows
            }
            // One row per composite field: attrelid = the composite's oid,
            // attnum 1-based, atttypid = the field type's oid.
            "pg_attribute" => {
                let mut rows = Vec::new();
                for (_, oid, fields) in self.composites().ok()? {
                    for (i, (fname, ftype)) in fields.iter().enumerate() {
                        let Some(atttypid) = self.type_oid_by_name(ftype) else {
                            continue;
                        };
                        let mut d = Document::new();
                        d.insert(def.field_of("attrelid").expect("column"), Bson::Int64(oid));
                        d.insert(def.field_of("attname").expect("column"), fname.as_str());
                        d.insert(
                            def.field_of("atttypid").expect("column"),
                            Bson::Int64(atttypid),
                        );
                        d.insert(
                            def.field_of("attnum").expect("column"),
                            Bson::Int32((i + 1) as i32),
                        );
                        d.insert(
                            def.field_of("attisdropped").expect("column"),
                            Bson::Boolean(false),
                        );
                        rows.push(d);
                    }
                }
                rows
            }
            // One row per label, in declared order; sortorder starts at 1.
            "pg_enum" => {
                let mut rows = Vec::new();
                for (_, oid, labels) in self.enums().ok()? {
                    for (i, label) in labels.iter().enumerate() {
                        let mut d = Document::new();
                        d.insert(def.field_of("enumtypid").expect("column"), Bson::Int64(oid));
                        d.insert(
                            def.field_of("enumsortorder").expect("column"),
                            Bson::Double((i + 1) as f64),
                        );
                        d.insert(def.field_of("enumlabel").expect("column"), label.as_str());
                        rows.push(d);
                    }
                }
                rows
            }
            // One row per NAMED prepared statement on this connection, in
            // the order they were prepared -- psycopg's suite sorts by
            // `prepare_time`, and two statements prepared within the same
            // millisecond must keep their order under that (stable) sort.
            "pg_prepared_statements" => {
                let prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
                prepared
                    .iter()
                    .map(|rec| {
                        let names = |v: &[String]| {
                            Bson::Array(v.iter().map(|t| Bson::String(t.clone())).collect())
                        };
                        let mut d = Document::new();
                        d.insert(def.field_of("name").expect("column"), rec.name.as_str());
                        d.insert(
                            def.field_of("statement").expect("column"),
                            rec.statement.as_str(),
                        );
                        d.insert(
                            def.field_of("prepare_time").expect("column"),
                            Bson::DateTime(rec.prepare_time),
                        );
                        d.insert(
                            def.field_of("parameter_types").expect("column"),
                            names(&rec.parameter_types),
                        );
                        d.insert(
                            def.field_of("result_types").expect("column"),
                            rec.result_types.as_deref().map_or(Bson::Null, names),
                        );
                        d.insert(
                            def.field_of("from_sql").expect("column"),
                            Bson::Boolean(false),
                        );
                        let has_params = !rec.parameter_types.is_empty();
                        d.insert(
                            def.field_of("generic_plans").expect("column"),
                            Bson::Int64(i64::from(!has_params)),
                        );
                        d.insert(
                            def.field_of("custom_plans").expect("column"),
                            Bson::Int64(i64::from(has_params)),
                        );
                        d
                    })
                    .collect()
            }
            // One row per database the server accepts a connection to. The
            // constant columns are what `initdb` writes (probed PG 16).
            "pg_database" => {
                let field = |name: &str| def.field_of(name).expect("column");
                self.databases
                    .all(&self.storage)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|info| {
                        let mut d = Document::new();
                        d.insert(field("oid"), Bson::Int64(info.oid));
                        d.insert(field("datname"), info.name);
                        d.insert(field("datdba"), Bson::Int64(10));
                        d.insert(field("encoding"), Bson::Int32(6));
                        d.insert(field("datlocprovider"), "c");
                        d.insert(field("datistemplate"), info.is_template);
                        d.insert(field("datallowconn"), info.allow_conn);
                        d.insert(field("datconnlimit"), Bson::Int32(-1));
                        d.insert(field("datfrozenxid"), Bson::Int64(722));
                        d.insert(field("datminmxid"), Bson::Int64(1));
                        d.insert(field("dattablespace"), Bson::Int64(1663));
                        d.insert(field("datcollate"), "C");
                        d.insert(field("datctype"), "C");
                        d.insert(field("daticulocale"), Bson::Null);
                        d.insert(field("daticurules"), Bson::Null);
                        d.insert(field("datcollversion"), Bson::Null);
                        d.insert(field("datacl"), Bson::Null);
                        d
                    })
                    .collect()
            }
            "pg_stat_activity" => {
                let backends: Vec<(i32, BackendActivity)> = backend_registry()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .map(|(pid, entry)| {
                        let activity = entry.activity.lock().unwrap_or_else(|e| e.into_inner());
                        (*pid, activity.clone())
                    })
                    .collect();
                let field = |name: &str| def.field_of(name).expect("column");
                let opt_time = |t: Option<bson::DateTime>| t.map_or(Bson::Null, Bson::DateTime);
                backends
                    .into_iter()
                    .map(|(pid, a)| {
                        let mut d = Document::new();
                        d.insert(field("datid"), Bson::Int64(0));
                        d.insert(field("datname"), a.datname);
                        d.insert(field("pid"), Bson::Int32(pid));
                        d.insert(field("leader_pid"), Bson::Null);
                        d.insert(field("usesysid"), Bson::Int64(10));
                        d.insert(field("usename"), a.usename);
                        d.insert(field("application_name"), a.application_name);
                        d.insert(field("client_addr"), Bson::Null);
                        d.insert(field("client_hostname"), Bson::Null);
                        d.insert(field("client_port"), Bson::Null);
                        d.insert(field("backend_start"), Bson::DateTime(a.backend_start));
                        d.insert(field("xact_start"), Bson::Null);
                        d.insert(field("query_start"), opt_time(a.query_start));
                        d.insert(field("state_change"), opt_time(a.state_change));
                        d.insert(field("wait_event_type"), Bson::Null);
                        d.insert(field("wait_event"), Bson::Null);
                        d.insert(field("state"), a.state);
                        d.insert(field("backend_xid"), Bson::Null);
                        d.insert(field("backend_xmin"), Bson::Null);
                        d.insert(field("query_id"), Bson::Null);
                        d.insert(field("query"), a.query);
                        d.insert(field("backend_type"), "client backend");
                        d
                    })
                    .collect()
            }
            "pg_tables" => {
                let field = |name: &str| def.field_of(name).expect("column");
                let owner = self
                    .session_user
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let mut rows: Vec<Document> = self
                    .all_table_defs()
                    .ok()?
                    .into_iter()
                    .map(|t| {
                        let mut d = Document::new();
                        d.insert(field("schemaname"), Self::schema_of(&t));
                        d.insert(field("tablename"), t.name.as_str());
                        d.insert(field("tableowner"), owner.as_str());
                        d.insert(field("tablespace"), Bson::Null);
                        d.insert(field("hasindexes"), t.columns.iter().any(|c| c.pk));
                        d.insert(field("hasrules"), false);
                        d.insert(field("hastriggers"), false);
                        d.insert(field("rowsecurity"), false);
                        d
                    })
                    .collect();
                // The catalog relations this server answers for, as
                // PostgreSQL lists its own: owned by the bootstrap
                // superuser, indexed.
                for name in [
                    "pg_type",
                    "pg_attribute",
                    "pg_range",
                    "pg_enum",
                    "pg_database",
                    "pg_class",
                    "pg_namespace",
                ] {
                    let mut d = Document::new();
                    d.insert(field("schemaname"), "pg_catalog");
                    d.insert(field("tablename"), name);
                    d.insert(field("tableowner"), "postgres");
                    d.insert(field("tablespace"), Bson::Null);
                    d.insert(field("hasindexes"), true);
                    d.insert(field("hasrules"), false);
                    d.insert(field("hastriggers"), false);
                    d.insert(field("rowsecurity"), false);
                    rows.push(d);
                }
                rows
            }
            "pg_cursors" => {
                let cursors = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
                cursors
                    .iter()
                    .map(|(name, c)| {
                        let mut d = Document::new();
                        d.insert(def.field_of("name").expect("column"), name.as_str());
                        d.insert(
                            def.field_of("statement").expect("column"),
                            c.statement.as_str(),
                        );
                        d.insert(
                            def.field_of("is_holdable").expect("column"),
                            Bson::Boolean(c.is_holdable),
                        );
                        d.insert(
                            def.field_of("is_binary").expect("column"),
                            Bson::Boolean(c.is_binary),
                        );
                        d.insert(
                            def.field_of("is_scrollable").expect("column"),
                            Bson::Boolean(c.is_scrollable),
                        );
                        d.insert(
                            def.field_of("creation_time").expect("column"),
                            Bson::DateTime(c.creation_time),
                        );
                        d
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let empty = Document::new();
        Some(
            rows.into_iter()
                .filter(|d| secantus_core::query::matches(d, filter, &empty, None).unwrap_or(false))
                .collect(),
        )
    }

    /// Read one table's catalog entry: this transaction's pending creates and
    /// drops first, then the process-wide cache of committed entries, then
    /// storage (see `CatalogCache` for when a read fills the cache). The
    /// store is this process's alone -- WiredTiger locks the directory -- so
    /// every write to the catalog passes through `run` and bumps the version.
    fn lookup(&self, name: &str) -> Option<TableDef> {
        if let Some(def) = Self::virtual_table(name) {
            return Some(def);
        }
        // What this transaction has created or dropped, before what is
        // committed. A `None` here is a tombstone: the table was dropped in
        // this transaction and must not be found even though its catalog row
        // is still there.
        if let Some(pending) = self
            .uncommitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            return pending.clone();
        }
        let cache = catalog_cache();
        let version = cache.version.load(std::sync::atomic::Ordering::SeqCst);
        let key = (
            Arc::as_ptr(&self.storage) as usize,
            self.db().to_string(),
            name.to_string(),
        );
        if let Some((v, def)) = cache
            .tables
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            if *v == version {
                return def.clone();
            }
        }
        let filter = bson::doc! { "_id": name };
        // A storage error is a transient `None`, not a recorded absence.
        let rows = self
            .storage
            .find_matching(self.db(), CATALOG_COLLECTION, &filter)
            .ok()?;
        let def = rows.first().and_then(|raw| {
            let d: Document = bson::from_slice(raw).ok()?;
            TableDef::from_document(&d)
        });
        if self.may_fill_catalog_cache(version) {
            cache
                .tables
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (version, def.clone()));
        }
        def
    }

    fn err(e: &PlanError) -> PgWireError {
        // A planner message may carry PostgreSQL's DETAIL / HINT line after
        // the primary one; each travels in its own field, not the message.
        let text = e.to_string();
        let (message, hint) = match text.split_once("\nHint: ") {
            Some((m, h)) => (m.to_string(), Some(h.to_string())),
            None => (text, None),
        };
        let (message, detail) = match message.split_once("\nDetail: ") {
            Some((m, d)) => (m.to_string(), Some(d.to_string())),
            None => (message, None),
        };
        let mut info = ErrorInfo::new("ERROR".into(), e.sqlstate().into(), message);
        info.detail = detail;
        info.hint = hint.or_else(|| e.hint().map(str::to_string));
        PgWireError::UserError(Box::new(info))
    }

    /// `err`, with the statement text to point the error's `P` field at the
    /// token at fault, for the errors PostgreSQL positions: an unknown
    /// relation points at its first mention (`LINE 1: select * from wat`).
    fn err_in(e: &PlanError, sql: &str) -> PgWireError {
        let mut out = Self::err(e);
        if let (PlanError::UndefinedTable(name), PgWireError::UserError(info)) = (e, &mut out) {
            if let Some(pos) = secantus_pgplan::identifier_position(sql, name) {
                info.position = Some(pos.to_string());
            }
        }
        out
    }

    /// A storage write error, rendered as PostgreSQL renders it.
    ///
    /// The storage layer speaks MongoDB: a duplicate `_id` comes back as
    /// `E11000 duplicate key error collection: postgres.t index: _id_ ...`.
    /// Letting that reach a psql client would leak the Mongo persona straight
    /// through the PostgreSQL one. Real PostgreSQL 14 answers (probed
    /// 2026-08-31):
    ///
    /// ```text
    /// ERROR:  duplicate key value violates unique constraint "dupt_pkey"
    /// DETAIL:  Key (id)=(1) already exists.
    /// ```
    fn write_error(table: &str, def: &TableDef, err: &Document) -> PgWireError {
        let msg = err.get_str("errmsg").unwrap_or_default();
        if err.get_i32("code").unwrap_or(0) == 11000 || msg.starts_with("E11000") {
            let pk = def.columns.iter().find(|c| c.pk);
            let detail = pk.map(|c| {
                // `keyValue` carries the offending value under the STORED field.
                let v = err
                    .get_document("keyValue")
                    .ok()
                    .and_then(|kv| kv.get(c.field()).cloned());
                match v {
                    Some(Bson::String(s)) => format!("Key ({})=({}) already exists.", c.name, s),
                    Some(ref b) if secantus_pgplan::is_numeric(b) => format!(
                        "Key ({})=({}) already exists.",
                        c.name,
                        secantus_pgplan::numeric_text(b).unwrap_or_default()
                    ),
                    Some(b) => format!(
                        "Key ({})=({}) already exists.",
                        c.name,
                        b.to_string().trim_matches('"')
                    ),
                    None => format!("Key ({}) already exists.", c.name),
                }
            });
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "23505".into(),
                format!("duplicate key value violates unique constraint \"{table}_pkey\""),
            );
            info.detail = detail;
            // pgwire 0.39 added the protocol's schema/table/column/constraint
            // fields (they were absent in 0.31, which is why this used to be a
            // recorded limitation). Real PostgreSQL sends them on a 23505, and
            // pgjdbc surfaces them as
            // `PSQLException.getServerErrorMessage().getConstraint()`.
            info.table = Some(table.to_string());
            info.schema = Some("public".to_string());
            info.constraint = Some(format!("{table}_pkey"));
            // `column` stays UNSET: PostgreSQL identifies the offending column
            // through the constraint on a 23505, not through this field
            // (probed 14 -- it sends None). Populating it looked more helpful
            // and was simply wrong.
            return PgWireError::UserError(Box::new(info));
        }
        Self::storage_err("could not insert", msg)
    }

    /// The first row whose numeric PRIMARY KEY is wider than Decimal128 and
    /// equal in VALUE to a key already in the table (or earlier in the same
    /// batch), or `None`.
    ///
    /// The `_id` index keys a Decimal128 by value, so `1.5` and `1.50` collide
    /// there as PostgreSQL requires. A wide numeric is a document carrying its
    /// display scale, so the index sees `1e40` and `1e40.0` as two keys; this
    /// lookup applies the value-equality the index cannot.
    fn wide_numeric_pk_conflict(
        &self,
        table: &str,
        def: &TableDef,
        rows: &[Document],
    ) -> PgWireResult<Option<Bson>> {
        let Some(pk) = def.columns.iter().find(|c| c.pk) else {
            return Ok(None);
        };
        if !matches!(pk.pg_type.as_str(), "numeric" | "decimal") {
            return Ok(None);
        }
        let mut seen: Vec<String> = Vec::new();
        for row in rows {
            let Some(id) = row.get(pk.field()) else {
                continue;
            };
            if !secantus_pgplan::is_numeric(id) {
                continue;
            }
            let text = secantus_pgplan::numeric_text(id).unwrap_or_default();
            let wide = secantus_pgplan::is_wide_numeric(id);
            let clash = seen.iter().any(|prev| {
                secantus_pgplan::compare_decimal_text(prev, &text) == Some(Ordering::Equal)
            });
            if clash && wide {
                return Ok(Some(id.clone()));
            }
            if wide {
                let Some(filter) = secantus_pgplan::numeric::numeric_filter(&pk.field(), "$eq", id)
                else {
                    continue;
                };
                let hit = self
                    .storage
                    .find_matching(self.db(), table, &filter)
                    .map_err(|e| Self::storage_err("could not check the primary key", e))?;
                if !hit.is_empty() {
                    return Ok(Some(id.clone()));
                }
            }
            seen.push(text);
        }
        Ok(None)
    }

    fn storage_err(context: &str, e: impl std::fmt::Display) -> PgWireError {
        // A storage failure is never dressed up as a SQL-level error: this is a
        // database, and an internal error must read as one.
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "XX000".into(),
            format!("{context}: {e}"),
        )))
    }
}

/// PostgreSQL's canonical spelling for a setting name.
///
/// `SHOW datestyle` answers a column called `DateStyle` -- lookups are
/// case-insensitive but the reported name is not, and clients match on it.
fn canonical_setting(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "datestyle" => "DateStyle".to_string(),
        "timezone" => "TimeZone".to_string(),
        "intervalstyle" => "IntervalStyle".to_string(),
        other => other.to_string(),
    }
}

/// The two session-idle GUCs this server enforces (PostgreSQL 16 semantics:
/// milliseconds, `0` disables), with the FATAL error each one ends the
/// connection with.
const IDLE_TIMEOUT_GUCS: [(&str, &str, &str); 2] = [
    (
        "idle_in_transaction_session_timeout",
        "25P03",
        "terminating connection due to idle-in-transaction timeout",
    ),
    (
        "idle_session_timeout",
        "57P05",
        "terminating connection due to idle-session timeout",
    ),
];

/// Parse a millisecond GUC the way PostgreSQL does: a number (integer, or a
/// float such as `1.5s` / `1e3`) with an optional unit (`us`, `ms`, `s`,
/// `min`, `h`, `d`), whitespace allowed between the two, rounded to the
/// nearest millisecond. `None` when the text is not a duration at all.
fn parse_ms_guc(text: &str) -> Option<i64> {
    let text = text.trim();
    // The unit starts at a letter -- but so does the exponent of `1e3`, so
    // try every letter as the split and take the first that parses.
    let splits = text
        .char_indices()
        .filter(|(_, c)| c.is_ascii_alphabetic())
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()));
    for at in splits {
        let (number, unit) = text.split_at(at);
        let factor: f64 = match unit.trim() {
            "" | "ms" => 1.0,
            "us" => 0.001,
            "s" => 1_000.0,
            "min" => 60_000.0,
            "h" => 3_600_000.0,
            "d" => 86_400_000.0,
            _ => continue,
        };
        let number = number.trim();
        if let Ok(n) = number.parse::<i64>() {
            return if factor >= 1.0 {
                n.checked_mul(factor as i64)
            } else {
                Some((n as f64 * factor).round_ties_even() as i64)
            };
        }
        if let Ok(f) = number.parse::<f64>() {
            let ms = (f * factor).round_ties_even();
            return ms.is_finite().then_some(ms as i64);
        }
    }
    None
}

/// Render a millisecond GUC as `SHOW` does: the largest unit that divides
/// the value evenly (`1500` -> `1500ms`, `60000` -> `1min`), and a bare `0`.
fn render_ms_guc(ms: i64) -> String {
    if ms == 0 {
        return "0".to_string();
    }
    for (unit, factor) in [
        ("d", 86_400_000),
        ("h", 3_600_000),
        ("min", 60_000),
        ("s", 1_000),
    ] {
        if ms % factor == 0 {
            return format!("{}{unit}", ms / factor);
        }
    }
    format!("{ms}ms")
}

/// Validate and canonicalise a `SET <ms-guc>` value, answering PostgreSQL's
/// two `22023` refusals: text that is not a duration (or exceeds the integer
/// range) is an invalid value, and a negative one is out of range.
fn canonical_ms_guc(name: &str, value: &str) -> PgWireResult<String> {
    let invalid = |msg: String| {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "22023".into(),
            msg,
        )))
    };
    match parse_ms_guc(value) {
        Some(ms) if ms < 0 => Err(invalid(format!(
            "{ms} ms is outside the valid range for parameter \"{name}\" (0 .. 2147483647)"
        ))),
        Some(ms) if ms <= i32::MAX as i64 => Ok(render_ms_guc(ms)),
        _ => Err(invalid(format!(
            "invalid value for parameter \"{name}\": \"{value}\""
        ))),
    }
}

/// The boolean GUCs whose value this server OBEYS, so `SET` validates them as
/// PostgreSQL does (`22023 parameter "x" requires a Boolean value`) and
/// stores the canonical `on` / `off` a client reads back.
const BOOL_GUCS: [&str; 2] = ["standard_conforming_strings", "escape_string_warning"];

/// PostgreSQL's `parse_bool`: `on` / `off` / `true` / `false` / `yes` / `no`
/// / `1` / `0`, case-insensitively, and any unambiguous prefix of the words
/// (`t`, `of`, `n`; measured on 16).
fn canonical_bool_guc(name: &str, value: &str) -> PgWireResult<String> {
    let v = value.trim().to_ascii_lowercase();
    let prefix_of = |word: &str| !v.is_empty() && word.starts_with(&v);
    let parsed = if v == "1" || prefix_of("true") || prefix_of("yes") || v == "on" {
        Some("on")
    } else if v == "0"
        || prefix_of("false")
        || prefix_of("no")
        || (v.len() >= 2 && prefix_of("off"))
    {
        Some("off")
    } else {
        None
    };
    parsed.map(str::to_string).ok_or_else(|| {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "22023".into(),
            format!("parameter \"{name}\" requires a Boolean value"),
        )))
    })
}

/// The settings a fresh connection starts with, matching what a client expects
/// to read back before it has set anything.
fn default_settings() -> HashMap<String, String> {
    [
        ("client_encoding", "UTF8"),
        ("DateStyle", "ISO, MDY"),
        ("TimeZone", "UTC"),
        ("IntervalStyle", "postgres"),
        ("standard_conforming_strings", "on"),
        ("escape_string_warning", "on"),
        ("integer_datetimes", "on"),
        ("transaction_read_only", "off"),
        // Transaction GUCs psycopg reads to learn the connection's defaults.
        // This server runs one un-prepared transaction at a time at READ
        // COMMITTED, so these are the fixed values a real single-node server
        // reports; `max_prepared_transactions` is 0 because 2PC is not offered.
        ("max_prepared_transactions", "0"),
        ("transaction_isolation", "read committed"),
        ("default_transaction_isolation", "read committed"),
        ("transaction_deferrable", "off"),
        ("default_transaction_read_only", "off"),
        ("default_transaction_deferrable", "off"),
        ("search_path", "\"$user\", public"),
        ("idle_in_transaction_session_timeout", "0"),
        ("idle_session_timeout", "0"),
        ("application_name", ""),
        ("server_encoding", "UTF8"),
        ("server_version", "15.0"),
        // Read by a client before `ALTER USER ... PASSWORD` to pick the hash
        // (libpq's `PQchangePassword`); PostgreSQL 16's default.
        ("password_encryption", "scram-sha-256"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The PostgreSQL type a column's declared type maps onto over the wire.
/// The planner's INTERNAL name for a wire type, which is the inverse of
/// `wire_type` for the types a parameter can be declared as.
///
/// `None` for a type this server has no name for: the planner then falls back
/// to the value, which is what it did for every parameter before declared
/// types reached it.
fn internal_type_name(ty: &Type) -> Option<String> {
    Some(
        match ty.oid() {
            21 => "int2",
            23 => "int4",
            20 => "int8",
            700 => "float4",
            701 => "float8",
            1700 => "numeric",
            16 => "bool",
            25 => "text",
            17 => "bytea",
            869 => "inet",
            650 => "cidr",
            1033 => "aclitem",
            603 => "box",
            1043 => "varchar",
            1042 => "bpchar",
            19 => "name",
            1082 => "date",
            1083 => "time",
            1114 => "timestamp",
            1184 => "timestamptz",
            1266 => "timetz",
            2950 => "uuid",
            26 => "oid",
            1028 => "oid[]",
            2249 => "record",
            1186 => "interval",
            114 => "json",
            3802 => "jsonb",
            1005 => "int2[]",
            1007 => "int4[]",
            1016 => "int8[]",
            1021 => "float4[]",
            1022 => "float8[]",
            1231 => "numeric[]",
            1000 => "bool[]",
            1009 => "text[]",
            1015 => "varchar[]",
            1001 => "bytea[]",
            1034 => "aclitem[]",
            1041 => "inet[]",
            651 => "cidr[]",
            1020 => "box[]",
            2951 => "uuid[]",
            oid => {
                return secantus_pgplan::range::range_oid_name(oid)
                    .or_else(|| secantus_pgplan::range::multirange_oid_name(oid))
                    .map(str::to_string)
                    // An ARRAY of ranges / multiranges (`_int4range`, oid
                    // 3905, and friends). Without a declared name the planner
                    // typed the parameter from its decoded value -- `text[]`
                    // -- so an untyped literal beside it was never coerced
                    // and `'{empty,"(,)"}' = $1` was "text = text[]".
                    .or_else(|| {
                        element_of_array_oid(oid)
                            .filter(|e| {
                                secantus_pgplan::range::is_range_type(e)
                                    || secantus_pgplan::range::is_multirange_type(e)
                            })
                            .map(|e| format!("{e}[]"))
                    });
            }
        }
        .to_string(),
    )
}

/// `pg_type.typlen` for a wire type: the fixed byte width PostgreSQL reports as
/// a column's `type_size`, or -1 for a variable-length (varlena) type. Clients
/// read it back as `internal_size`. Fixed-width types name themselves; every
/// varlena type (text, numeric, bit, arrays, ranges, json, ...) is -1.
fn type_size(ty: &Type) -> i16 {
    match *ty {
        Type::BOOL | Type::CHAR => 1,
        Type::INT2 => 2,
        Type::INT4 | Type::FLOAT4 | Type::DATE | Type::OID | Type::REGTYPE => 4,
        Type::INT8 | Type::FLOAT8 | Type::TIME | Type::TIMESTAMP | Type::TIMESTAMPTZ => 8,
        Type::TIMETZ => 12,
        Type::INTERVAL | Type::UUID => 16,
        Type::BOX => 32,
        Type::NAME => 64,
        _ => -1,
    }
}

fn wire_type(pg_type: &str) -> Type {
    match pg_type {
        // Their own oids (1560 / 1562): a bit string is not a varchar, and a
        // declared `bit(n)` / `varbit(n)` carries a length modifier clients
        // read as `display_size`.
        "bit" => Type::BIT,
        "varbit" | "bit varying" => Type::VARBIT,
        "int2" => Type::INT2,
        "int4" | "integer" | "int" => Type::INT4,
        "int8" | "bigint" => Type::INT8,
        "float4" => Type::FLOAT4,
        "float8" => Type::FLOAT8,
        "bool" | "boolean" => Type::BOOL,
        "void" => Type::VOID,
        // `text` is oid 25, NOT varchar (1043). PostgreSQL distinguishes them
        // and clients read the oid: psycopg decodes both to `str` so a value
        // comparison never notices, but pgjdbc and pgx do.
        "text" => Type::TEXT,
        "bytea" => Type::BYTEA,
        "inet" => Type::INET,
        "cidr" => Type::CIDR,
        "aclitem" => Type::ACLITEM,
        "box" => Type::BOX,
        "varchar" | "character varying" => Type::VARCHAR,
        "bpchar" | "char" | "character" => Type::BPCHAR,
        "name" => Type::NAME,
        // Stored as canonical TEXT but reported with their real oids: a client
        // reading 1082/1083 parses the value into a date/time object, whereas
        // varchar hands it back as a string. Same shape as the text-vs-varchar
        // bug -- psycopg would not notice, pgjdbc and pgx would.
        "date" => Type::DATE,
        "time" => Type::TIME,
        "timestamp" => Type::TIMESTAMP,
        // Their own oids: a client reading 1184 builds an aware datetime,
        // where 1114 builds a naive one from the same characters.
        "timestamptz" => Type::TIMESTAMPTZ,
        "timetz" => Type::TIMETZ,
        // Its own oid (2950): a client reading it builds a UUID object rather
        // than handing back the canonical text.
        "uuid" => Type::UUID,
        // An anonymous record (`ROW(...)`): oid 2249, rendered as `(...)` text.
        "record" => Type::RECORD,
        "interval" => Type::INTERVAL,
        "json" => Type::JSON,
        "jsonb" => Type::JSONB,
        // Range types carry their own oids, so a client builds a Range object
        // rather than handing back the text.
        "int4range" => Type::INT4_RANGE,
        "int8range" => Type::INT8_RANGE,
        "numrange" => Type::NUM_RANGE,
        "daterange" => Type::DATE_RANGE,
        "tsrange" => Type::TS_RANGE,
        "tstzrange" => Type::TSTZ_RANGE,
        "int4multirange" => Type::INT4MULTI_RANGE,
        "int8multirange" => Type::INT8MULTI_RANGE,
        "nummultirange" => Type::NUMMULTI_RANGE,
        "datemultirange" => Type::DATEMULTI_RANGE,
        "tsmultirange" => Type::TSMULTI_RANGE,
        "tstzmultirange" => Type::TSTZMULTI_RANGE,
        "numeric" | "decimal" => Type::NUMERIC,
        // `pg_typeof` answers a `regtype` (2206), not text: a client reading
        // 25 would print the same characters but compare unequal to a regtype.
        "regtype" => Type::REGTYPE,
        // A real oid column type: psycopg's numeric tests read the oid back
        // and check `ftype(0) == 26`.
        "oid" => Type::OID,
        // Array oids are their own types (int4[] is 1007, not 23).
        "int4[]" | "int[]" | "integer[]" => Type::INT4_ARRAY,
        "int8[]" | "bigint[]" => Type::INT8_ARRAY,
        "int2[]" | "smallint[]" => Type::INT2_ARRAY,
        "float8[]" | "double[]" => Type::FLOAT8_ARRAY,
        "float4[]" | "real[]" => Type::FLOAT4_ARRAY,
        "bool[]" | "boolean[]" => Type::BOOL_ARRAY,
        "numeric[]" | "decimal[]" => Type::NUMERIC_ARRAY,
        "text[]" => Type::TEXT_ARRAY,
        "varchar[]" => Type::VARCHAR_ARRAY,
        // The rest of the array types this server can NAME. Without these an
        // array of dates was described as `varchar`, so a client read back
        // strings where PostgreSQL hands it dates.
        "date[]" => Type::DATE_ARRAY,
        "int4range[]" => Type::INT4_RANGE_ARRAY,
        "int8range[]" => Type::INT8_RANGE_ARRAY,
        "numrange[]" => Type::NUM_RANGE_ARRAY,
        "daterange[]" => Type::DATE_RANGE_ARRAY,
        "tsrange[]" => Type::TS_RANGE_ARRAY,
        "tstzrange[]" => Type::TSTZ_RANGE_ARRAY,
        // Multirange arrays. Without these an `int4multirange[]` fell through to
        // varchar, which is binary_encodable -- so a binary result stayed binary
        // and encode_binary refused the array value, and a text result carried
        // the varchar oid so the client handed back a string instead of parsing
        // it into Multirange objects. Their own array oids keep them, like range
        // arrays, on the text-format path the row description already downgrades
        // a non-binary-encodable type onto.
        "int4multirange[]" => Type::INT4MULTI_RANGE_ARRAY,
        "int8multirange[]" => Type::INT8MULTI_RANGE_ARRAY,
        "nummultirange[]" => Type::NUMMULTI_RANGE_ARRAY,
        "datemultirange[]" => Type::DATEMULTI_RANGE_ARRAY,
        "tsmultirange[]" => Type::TSMULTI_RANGE_ARRAY,
        "tstzmultirange[]" => Type::TSTZMULTI_RANGE_ARRAY,
        "time[]" => Type::TIME_ARRAY,
        "timestamp[]" => Type::TIMESTAMP_ARRAY,
        "timestamptz[]" => Type::TIMESTAMPTZ_ARRAY,
        "timetz[]" => Type::TIMETZ_ARRAY,
        "interval[]" => Type::INTERVAL_ARRAY,
        "json[]" => Type::JSON_ARRAY,
        "jsonb[]" => Type::JSONB_ARRAY,
        // Arrays of the text-stored types. Without these they fell through to
        // varchar -- a binary array result then hit the binary-varchar encoder
        // ("cannot send this value as a binary varchar"), and a text result
        // carried the varchar oid so the client returned strings instead of
        // parsing bytes / addresses / UUIDs. Their own array oids keep them on
        // the text array path the row description downgrades non-binary types to.
        "bytea[]" => Type::BYTEA_ARRAY,
        "inet[]" => Type::INET_ARRAY,
        "cidr[]" => Type::CIDR_ARRAY,
        "aclitem[]" => Type::ACLITEM_ARRAY,
        "box[]" => Type::BOX_ARRAY,
        "uuid[]" => Type::UUID_ARRAY,
        "bpchar[]" | "char[]" | "character[]" => Type::BPCHAR_ARRAY,
        "name[]" => Type::NAME_ARRAY,
        // `array_agg(atttypid)` is an `oid[]` -- without this arm it fell to
        // varchar, so psycopg's `CompositeInfo.fetch` read `field_types` back as
        // the raw string `"{23,25}"` instead of a list of oids.
        "oid[]" => Type::OID_ARRAY,
        // `pg_prepared_statements.parameter_types`: a client reads 2211 back as
        // a list of type names.
        "regtype[]" => Type::REGTYPE_ARRAY,
        // Everything else renders as text for now; P4 owns the real type map.
        _ => Type::VARCHAR,
    }
}

static PID_GENERATOR: std::sync::LazyLock<RandomPidSecretKeyGenerator> =
    std::sync::LazyLock::new(RandomPidSecretKeyGenerator::default);

#[async_trait]
impl StartupHandler for PgHandler {
    /// pgwire's no-authentication startup, with the `database` check in the
    /// place PostgreSQL makes it: BEFORE `AuthenticationOk`. An unknown name
    /// (or `template0`, which never accepts connections) is a FATAL
    /// `ErrorResponse` and the socket closes, with no `ReadyForQuery` -- what
    /// libpq reads as a failed connect rather than a failed first query.
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let PgWireFrontendMessage::Startup(ref startup) = message else {
            return Ok(());
        };
        pgwire::api::auth::protocol_negotiation(client, startup).await?;
        pgwire::api::auth::save_startup_parameters_to_metadata(client, startup);

        // libpq defaults `database` to the user name; a startup packet with
        // neither gets the server's default (PostgreSQL would look for a
        // database named after the user, which is not this server's model).
        let requested = client
            .metadata()
            .get("database")
            .cloned()
            .unwrap_or_else(|| self.databases.default_db().to_string());
        if let Err(e) = self.select_database(&requested) {
            let info: ErrorInfo = e.into();
            client
                .send(PgWireBackendMessage::ErrorResponse(info.into()))
                .await?;
            client.close().await?;
            return Ok(());
        }

        let (pid, secret_key) = PID_GENERATOR.generate(client);
        client.set_pid_and_secret_key(pid, secret_key);
        // The startup ParameterStatus values are the SESSION's settings, so
        // what a client reads from `parameter_status("TimeZone")` is what
        // `SHOW timezone` answers -- pgwire's own defaults (`Etc/UTC`, `ISO,
        // YMD`) disagreed with the session's `UTC` / `ISO, MDY`.
        let mut provider = DefaultServerParameterProvider::default();
        let defaults = default_settings();
        if let Some(tz) = defaults.get("TimeZone") {
            provider.time_zone = tz.clone();
        }
        if let Some(ds) = defaults.get("DateStyle") {
            provider.date_style = ds.clone();
        }
        pgwire::api::auth::finish_authentication0(client, &provider).await?;
        self.post_startup(client).await?;
        client
            .send(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                pgwire::messages::response::TransactionStatus::Idle,
            )))
            .await?;
        client.set_state(pgwire::api::PgWireConnectionState::ReadyForQuery);
        Ok(())
    }
}

impl PgHandler {
    /// Binds this connection to the database the startup packet named.
    fn select_database(&self, name: &str) -> PgWireResult<()> {
        let fatal = |code: &str, message: String| {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "FATAL".into(),
                code.into(),
                message,
            )))
        };
        let Some(info) = self.databases.lookup(&self.storage, name)? else {
            return Err(fatal(
                "3D000", // invalid_catalog_name
                format!("database \"{name}\" does not exist"),
            ));
        };
        if !info.allow_conn {
            return Err(fatal(
                "55000", // object_not_in_prerequisite_state
                format!("database \"{name}\" is not currently accepting connections"),
            ));
        }
        let _ = self.db.set(info.name.clone());
        self.backend
            .activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .datname = info.name;
        Ok(())
    }

    async fn post_startup<C>(&self, _c: &mut C) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        // pgwire has assigned and already sent the BackendKeyData PID by now;
        // record it so `pg_backend_pid()` / `pg_terminate_backend()` can see
        // it, and register this connection so another backend can terminate it.
        let (pid, secret) = _c.pid_and_secret_key();
        self.backend_pid
            .store(pid, std::sync::atomic::Ordering::Relaxed);
        let _ = self.backend.secret.set(secret.to_bytes());
        {
            let mut activity = self
                .backend
                .activity
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            activity.usename = _c.metadata().get("user").cloned().unwrap_or_default();
            activity.application_name = _c
                .metadata()
                .get("application_name")
                .cloned()
                .unwrap_or_default();
        }
        backend_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pid, self.backend.clone());
        if let Some(user) = _c.metadata().get("user") {
            *self.session_user.lock().unwrap_or_else(|e| e.into_inner()) = user.clone();
        }
        // ErrorResponse / NoticeResponse fields leave in the session's
        // `client_encoding`, like every other text the server sends: a LATIN9
        // client reading an `ERROR: ... \u{20ac}` expects the single byte 0xA4,
        // not UTF-8. The transcoder reads the setting at send time, so a later
        // SET takes effect.
        let settings = Arc::clone(&self.settings);
        _c.session_extensions()
            .insert(pgwire::api::BackendMessageTranscoder(Arc::new(move |s| {
                let enc = settings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get("client_encoding")
                    .map(|name| encoding::client_encoding(name))
                    .unwrap_or(ClientEncoding::Utf8);
                if enc.transcodes() {
                    encoding::encode(enc, s.as_bytes()).ok()
                } else {
                    None
                }
            })));
        // A `client_encoding` in the startup packet (libpq's PGCLIENTENCODING /
        // psycopg's `client_encoding=` connection option) is a SET before the
        // first query. pgwire has already echoed the client's raw spelling in a
        // ParameterStatus; apply it so the session actually transcodes, and
        // re-report the CANONICAL name -- the later report wins in libpq, and
        // it is the spelling psycopg matches on (`utf-8` -> `UTF8`). An invalid
        // name fails the connection, as PostgreSQL's does (22023).
        if let Some(requested) = _c.metadata().get("client_encoding").cloned() {
            self.apply_client_encoding(&requested)
                .map_err(|e| match e {
                    PgWireError::UserError(info) => PgWireError::UserError(Box::new(
                        ErrorInfo::new("FATAL".into(), info.code, info.message),
                    )),
                    other => other,
                })?;
            self.report_pending_params(_c).await?;
        }
        Ok(())
    }
}

impl Drop for PgHandler {
    fn drop(&mut self) {
        // Deregister so the map never signals a PID this connection has left
        // behind. `0` means startup never ran, so there is nothing to remove.
        let pid = self.backend_pid.load(std::sync::atomic::Ordering::Relaxed);
        if pid != 0 {
            backend_registry()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&pid);
        }
    }
}

#[async_trait]
impl SimpleQueryHandler for PgHandler {
    async fn before_ready_for_query<C>(&self, client: &mut C) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        self.flush_notifications(client).await
    }

    /// The query text arrives in the client's `client_encoding`, not
    /// necessarily UTF-8: decode the raw wire bytes from the session's
    /// encoding (a LATIN9 `select '\u{20ac}'` is the single byte 0xA4, which
    /// the lossy UTF-8 default would have turned into U+FFFD before we saw it).
    fn decode_query_text<C>(&self, _c: &C, query: &Query) -> PgWireResult<String>
    where
        C: ClientInfo,
    {
        Ok(encoding::decode(self.client_encoding(), &query.query_raw))
    }

    async fn do_query<C>(&self, _c: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        // The simple protocol takes any number of commands separated by
        // semicolons and answers with one result each. The extended protocol
        // does not, and still refuses -- see `Error::MultipleCommands`.
        // The simple protocol carries no `Bind`, so its results are always text.
        self.binary_results
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // A syntax error caught while splitting (`meh`) is where a simple-query
        // statement fails BEFORE `run_typed` ever sees it -- and inside a
        // transaction PostgreSQL poisons the block on it exactly as on any
        // other error, so the next statement gets `25P02`. Recording it here
        // is the simple-protocol twin of the `Describe` note in
        // `describe_fields`.
        // The text is read under the session's string syntax first (see
        // `apply_string_syntax`); an error there is a syntax error like any
        // other, and the scanner's warnings precede the error they led up to.
        let query = match self.apply_string_syntax(query) {
            Ok(query) => query,
            Err(e) => {
                self.note_failure();
                self.flush_notices(_c).await?;
                return Err(e);
            }
        };
        let query = query.as_str();
        let stmts = match secantus_pgplan::split_statements(query) {
            Ok(stmts) => stmts,
            Err(e) => {
                self.note_failure();
                self.flush_notices(_c).await?;
                return Err(Self::err(&e));
            }
        };
        let out = if stmts.len() <= 1 {
            self.run(query, &[], 0).await
        } else {
            self.run_batch(&stmts).await
        };
        // A simple query inside an extended-protocol statement group runs in
        // the group's transaction and ends it, as PostgreSQL's does
        // (`exec_simple_query` finishes the transaction command).
        let out = match self.close_extended_group(out.is_err()) {
            Ok(()) => out,
            Err(e) => out.and(Err(e)),
        };
        self.flush_notices(_c).await?;
        // Report any GUC change (TimeZone, ...) so the client tracks it.
        self.report_pending_params(_c).await?;
        self.settle_failed_commit(_c);
        // A FATAL error (`pg_terminate_backend` on this backend) ends the
        // connection. pgwire's own error path would send a `ReadyForQuery`
        // after the `ErrorResponse` and leave the socket open, so the client
        // never learns it is gone. A real backend sends the `ErrorResponse`
        // and closes, with NO `ReadyForQuery` -- do that here, in the simple
        // protocol, where the client would otherwise think the connection is
        // still usable. (The extended protocol's `Sync` handshake already
        // surfaces the close, so it needs nothing.)
        if matches!(&out, Err(PgWireError::UserError(info)) if info.severity == "FATAL") {
            if let Err(out) = out {
                let info: ErrorInfo = out.into();
                let _ = _c
                    .send(PgWireBackendMessage::ErrorResponse(info.into()))
                    .await;
                let _ = SinkExt::close(_c).await;
                return Ok(vec![]);
            }
            unreachable!("matched Err above");
        }
        out
    }
}

impl PgHandler {
    /// Run a multi-command simple query as PostgreSQL does: every command in
    /// order, one result each, the whole batch in an IMPLICIT TRANSACTION.
    ///
    /// The transaction is the part that is easy to miss and impossible to fake
    /// afterwards. Measured against PostgreSQL 14:
    ///
    /// * `insert into t values (1); select * from nosuchtable` leaves **no**
    ///   row in `t` -- the failure rolls the earlier write back.
    /// * `begin; insert into t values (2); commit; select * from nosuchtable`
    ///   leaves the row -- an explicit COMMIT inside the batch ends the
    ///   transaction, and what it committed survives the later failure.
    ///
    /// Both fall out of reusing the session's own transaction slot rather than
    /// tracking a second one: `BEGIN` inside an open transaction is already a
    /// no-op here (as it is a warning in PostgreSQL), and `COMMIT` already
    /// takes the handle. After a mid-batch COMMIT a fresh implicit transaction
    /// is opened for the commands that follow, which is what PostgreSQL does.
    async fn run_batch(&self, stmts: &[String]) -> PgWireResult<Vec<Response>> {
        let mut implicit = self.txn.lock().unwrap_or_else(|e| e.into_inner()).is_none();
        if implicit {
            self.begin_implicit()?;
        }

        let mut out = Vec::with_capacity(stmts.len());
        for (i, sql) in stmts.iter().enumerate() {
            match self.run(sql, &[], 0).await {
                Ok(responses) => {
                    // A BEGIN inside the batch makes the implicit transaction
                    // the BLOCK: it stays open past the batch's end, and so
                    // does a cursor declared after it. Measured on 16:
                    // `begin; declare cur cursor for select 1;` leaves the
                    // session in a transaction with `cur` fetchable.
                    // Committing it here closed the cursor and left the
                    // client believing in a block the server had ended.
                    if responses
                        .iter()
                        .any(|r| matches!(r, Response::TransactionStart(_)))
                    {
                        implicit = false;
                    }
                    out.extend(responses);
                }
                Err(e) => {
                    if implicit {
                        // Roll back whatever this batch opened. A failure to
                        // roll back must not mask the error that caused it.
                        let _ = self.rollback_implicit();
                    }
                    return Err(e);
                }
            }
            // An explicit COMMIT or ROLLBACK inside the batch closed the
            // transaction; PostgreSQL starts another for what follows.
            if i + 1 < stmts.len() && self.txn.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
                implicit = true;
                self.begin_implicit()?;
            }
        }

        if implicit {
            self.commit_implicit()?;
        }
        Ok(out)
    }

    /// `FETCH` and `MOVE`, which differ only in whether the rows are returned.
    ///
    /// Positions follow PostgreSQL's model exactly: the cursor sits ON a
    /// 1-based row, with 0 before the first and `len + 1` after the last. Two
    /// consequences that a simpler "next index" model gets wrong:
    ///
    /// * fetching past the end leaves the cursor at `len + 1`, so
    ///   `MOVE BACKWARD 2` afterwards lands on the LAST row;
    /// * a BACKWARD fetch returns its rows in reverse order, nearest first.
    fn fetch(
        &self,
        name: &str,
        direction: secantus_pgplan::FetchDirection,
        count: i64,
        is_move: bool,
    ) -> PgWireResult<Vec<Response>> {
        use secantus_pgplan::FetchDirection as Fd;
        let mut cursors = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
        let Some(cursor) = cursors.get_mut(name) else {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "34000".into(), // invalid_cursor_name
                format!("cursor \"{name}\" does not exist"),
            ))));
        };
        let len = cursor.rows.len() as i64;
        let pos = cursor.pos;

        // `FETCH ALL` arrives as a count of i64::MAX, so every step saturates.
        // RELATIVE and ABSOLUTE fetch ONE row -- the n'th from here, or the
        // n'th from the start -- where FORWARD and BACKWARD fetch a RUN of
        // them. Treating RELATIVE as a forward run returned every row up to
        // the target instead of just the target.
        let single = matches!(direction, Fd::Absolute | Fd::Relative);
        let (mut wanted, backward) = match direction {
            Fd::Forward => (count, false),
            Fd::Backward => (count, true),
            Fd::Absolute | Fd::Relative => (0, false),
        };
        // A negative count reverses the direction it was asked in.
        let backward = if wanted < 0 {
            wanted = wanted.saturating_neg();
            !backward
        } else {
            backward
        };

        // A `NO SCROLL` cursor may only scan forward. Any FETCH/MOVE that would
        // step to an earlier position is rejected, exactly as PostgreSQL does
        // (`cursor can only scan forward`, SQLSTATE 55000) -- a materialised
        // cursor could serve it, but matching the server's contract is what a
        // client's `ServerCursor` relies on.
        let is_backward = if single {
            let target = match direction {
                Fd::Relative => pos.saturating_add(count),
                _ if count > 0 => count,
                _ if count < 0 => len.saturating_add(count).saturating_add(1),
                _ => 0,
            }
            .clamp(0, len + 1);
            target < pos
        } else {
            backward && wanted > 0
        };
        if is_backward && !cursor.is_scrollable {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "55000".into(), // object_not_in_prerequisite_state
                "cursor can only scan forward".into(),
            ))));
        }

        // The 0-based indices of the rows to return, in output order. Kept as
        // indices rather than cloned rows so a BINARY fetch can re-encode from
        // `typed_rows` at the same positions.
        let (indices, new_pos): (Vec<usize>, i64) = match direction {
            _ if single => {
                let target = match direction {
                    Fd::Relative => pos.saturating_add(count),
                    // ABSOLUTE counts from the start, and from the END when
                    // negative: -1 is the last row.
                    _ if count > 0 => count,
                    _ if count < 0 => len.saturating_add(count).saturating_add(1),
                    _ => 0,
                };
                let target = target.clamp(0, len + 1);
                let idx = if (1..=len).contains(&target) {
                    vec![(target - 1) as usize]
                } else {
                    Vec::new()
                };
                (idx, target)
            }
            _ if backward => {
                // Rows below the cursor, nearest first.
                let first = pos - 1;
                let last = pos.saturating_sub(wanted).max(1);
                let mut out = Vec::new();
                let mut i = first.min(len);
                while i >= last && i >= 1 {
                    out.push((i - 1) as usize);
                    i -= 1;
                }
                (out, pos.saturating_sub(wanted).max(0))
            }
            _ => {
                let first = pos.saturating_add(1);
                let last = pos.saturating_add(wanted).min(len);
                let mut out = Vec::new();
                let mut i = first.max(1);
                while i <= last {
                    out.push((i - 1) as usize);
                    i += 1;
                }
                (out, pos.saturating_add(wanted).min(len + 1))
            }
        };

        cursor.pos = new_pos.clamp(0, len + 1);
        let n = indices.len();
        if is_move {
            return Ok(vec![Response::Execution(Tag::new(&format!("MOVE {n}")))]);
        }
        // A BINARY fetch re-encodes the captured typed values in binary; every
        // other fetch reuses the text rows frozen at DECLARE. `binary_results`
        // carries the format this FETCH's `Bind` asked for.
        let want_binary = self
            .binary_results
            .load(std::sync::atomic::Ordering::Relaxed);
        let (schema, rows): (Arc<Vec<FieldInfo>>, Vec<DataRow>) = match &cursor.typed_rows {
            Some(values) if want_binary => {
                let bin_schema = Arc::new(
                    cursor
                        .schema
                        .iter()
                        .map(|f| rebind_field_format(f, true))
                        .collect::<Vec<_>>(),
                );
                // A binary fetch is DateStyle-independent (the encoder takes the
                // binary branch before any text rendering), so the style passed
                // here is immaterial; use the session's for consistency.
                let fetch_ds = self.session_datestyle();
                let cenc = self.client_encoding();
                let mut out = Vec::with_capacity(indices.len());
                for &i in &indices {
                    out.push(encode_typed_row(
                        &bin_schema,
                        &values[i],
                        &cursor.tz,
                        &fetch_ds,
                        cenc,
                    )?);
                }
                (bin_schema, out)
            }
            _ => (
                cursor.schema.clone(),
                indices.iter().map(|&i| cursor.rows[i].clone()).collect(),
            ),
        };
        drop(cursors);
        let mut response = QueryResponse::new(schema, stream::iter(rows.into_iter().map(Ok)));
        // The tag is just `FETCH`: the wire layer appends the row count, so
        // building `FETCH 2` here produced `FETCH 2 2` on the wire.
        response.command_tag = "FETCH".to_string();
        Ok(vec![Response::Query(response)])
    }

    /// The output columns of a `COPY (query) TO STDOUT`, from the same code
    /// that describes the query anywhere else.
    fn copy_query_fields(&self, inner: &Statement) -> PgWireResult<Vec<FieldInfo>> {
        match inner {
            // `COPY (SELECT 1) TO STDOUT` -- a query with no FROM at all.
            Statement::SelectConstant(sc) => Ok(sc
                .columns
                .iter()
                .map(|(name, _, ty, _)| {
                    FieldInfo::new(name.clone(), None, None, wire_type(ty), FieldFormat::Text)
                })
                .collect()),
            Statement::ValuesConstant(vc) => Ok(vc
                .names
                .iter()
                .zip(&vc.types)
                .map(|(name, ty)| {
                    let wire = self.user_wire_type(ty).unwrap_or_else(|| wire_type(ty));
                    FieldInfo::new(name.clone(), None, None, wire, FieldFormat::Text)
                })
                .collect()),
            // The query's own description, in TEXT: COPY output is text (or
            // its own binary framing) whatever the session's result format.
            // A computed column (`id + 1`, `id::text`) types by its expression
            // here exactly as it does for a SELECT.
            Statement::Select(sel) => Ok(self
                .row_schema(&self.select_def(sel)?, &sel.columns, &sel.casts)
                .into_iter()
                .map(|f| {
                    FieldInfo::new(
                        f.name().to_string(),
                        None,
                        None,
                        f.datatype().clone(),
                        FieldFormat::Text,
                    )
                })
                .collect()),
            _ => Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "0A000".into(),
                "COPY over this statement is not supported yet".into(),
            )))),
        }
    }

    /// Parse a binary COPY payload: an 11-byte signature, flags and a header
    /// extension, then per row a field count and each field length-prefixed in
    /// its own binary format, then a `-1` count as the trailer.
    ///
    /// The per-field decoding is the same code that decodes a bound binary
    /// parameter -- the bytes on the wire are the same, so a second
    /// implementation could only drift from the first.
    fn parse_binary_copy(
        &self,
        buffer: &[u8],
        types: &[String],
    ) -> PgWireResult<Vec<Vec<Option<Bson>>>> {
        const SIGNATURE: &[u8] = b"PGCOPY\n\xff\r\n\0";
        let bad = || {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "22P04".into(), // bad_copy_file_format
                "invalid binary COPY data".into(),
            )))
        };
        if buffer.len() < 19 || &buffer[..SIGNATURE.len()] != SIGNATURE {
            return Err(bad());
        }
        let be32 = |b: &[u8], i: usize| -> Option<i32> {
            b.get(i..i + 4)
                .map(|s| i32::from_be_bytes(s.try_into().expect("4 bytes")))
        };
        // Skip the signature, the flags, and any header extension.
        let ext = be32(buffer, 15).ok_or_else(bad)?;
        let mut pos = 19 + usize::try_from(ext.max(0)).unwrap_or(0);
        let tz = self.session_timezone();
        let mut rows = Vec::new();
        loop {
            let count = buffer
                .get(pos..pos + 2)
                .map(|s| i16::from_be_bytes(s.try_into().expect("2 bytes")))
                .ok_or_else(bad)?;
            pos += 2;
            // `-1` is the trailer; anything after it is ignored, as PostgreSQL
            // ignores it.
            if count < 0 {
                break;
            }
            let mut row = Vec::with_capacity(count as usize);
            for i in 0..count as usize {
                let len = be32(buffer, pos).ok_or_else(bad)?;
                pos += 4;
                if len < 0 {
                    row.push(None);
                    continue;
                }
                let n = len as usize;
                let raw = buffer.get(pos..pos + n).ok_or_else(bad)?;
                pos += n;
                let ty = types.get(i).map(String::as_str).unwrap_or("text");
                let value = decode_parameter(
                    Some(&Bytes::copy_from_slice(raw)),
                    Some(&wire_type(ty)),
                    true,
                    &tz,
                    self.client_encoding(),
                )?;
                row.push(Some(value));
            }
            rows.push(row);
        }
        Ok(rows)
    }

    /// Record a table this transaction created (`Some`) or dropped (`None`).
    ///
    /// Only while a transaction is open: outside one the write is already
    /// committed and the catalog is the truth.
    /// The tables a statement is about to write, so the open savepoints can
    /// capture them first.
    ///
    /// DDL adds the CATALOG collection: a `CREATE TABLE` inside a savepoint
    /// writes a catalog row, and putting the table's contents back without the
    /// catalog would leave a table the server still believes in.
    fn written_tables(stmt: &Statement) -> Vec<String> {
        let mut out = match stmt {
            // A serial column's INSERT moves its sequence too.
            Statement::Insert(i) => vec![i.table.clone(), SEQUENCE_COLLECTION.to_string()],
            Statement::Update(u) => vec![u.table.clone()],
            Statement::Delete(d) => vec![d.table.clone()],
            Statement::CopyFrom(c) => vec![c.table.clone()],
            // A table's ROW TYPE is a composite, so its catalog moves too.
            Statement::CreateTable(def, _) => {
                vec![
                    def.name.clone(),
                    CATALOG_COLLECTION.to_string(),
                    SEQUENCE_COLLECTION.to_string(),
                    Self::COMPOSITE_COLLECTION.to_string(),
                ]
            }
            Statement::CreateTableAs { table, .. } => {
                vec![
                    table.clone(),
                    CATALOG_COLLECTION.to_string(),
                    SEQUENCE_COLLECTION.to_string(),
                    Self::COMPOSITE_COLLECTION.to_string(),
                ]
            }
            Statement::DropTable(d) => {
                let mut v = d.tables.clone();
                v.push(CATALOG_COLLECTION.to_string());
                v.push(SEQUENCE_COLLECTION.to_string());
                v.push(Self::COMPOSITE_COLLECTION.to_string());
                v
            }
            // CREATE/DROP TYPE writes a type-catalog row; a `ROLLBACK TO`
            // before it has to put that catalog back, exactly as a table's
            // does. The overlay hides the type from planning; the pre-image
            // is what stops a later COMMIT from resurrecting it.
            Statement::CreateComposite { .. } => vec![Self::COMPOSITE_COLLECTION.to_string()],
            Statement::CreateEnum { .. } => vec![Self::ENUM_COLLECTION.to_string()],
            Statement::CreateRange { .. } => vec![Self::RANGE_COLLECTION.to_string()],
            Statement::CreateShellType { .. } | Statement::CreateBaseType { .. } => {
                vec![Self::BASE_TYPE_COLLECTION.to_string()]
            }
            Statement::CreateFunction { .. } => vec![Self::FUNCTION_COLLECTION.to_string()],
            Statement::DropFunction { .. } => vec![
                Self::FUNCTION_COLLECTION.to_string(),
                Self::BASE_TYPE_COLLECTION.to_string(),
            ],
            Statement::DropType { .. } => vec![
                Self::ENUM_COLLECTION.to_string(),
                Self::COMPOSITE_COLLECTION.to_string(),
                Self::RANGE_COLLECTION.to_string(),
                Self::BASE_TYPE_COLLECTION.to_string(),
                Self::FUNCTION_COLLECTION.to_string(),
            ],
            _ => Vec::new(),
        };
        out.sort();
        out.dedup();
        out
    }

    /// Capture, into every open savepoint that has not got it yet, the contents
    /// of each table a statement is about to write.
    ///
    /// "Has not got it yet" is what makes a savepoint's view the state at its
    /// ESTABLISHMENT: the first write to a table after the savepoint captures
    /// the table as it was before that write, and later writes find the entry
    /// already there and leave it alone.
    fn capture_for_savepoints(&self, stmt: &Statement) -> PgWireResult<()> {
        let tables = {
            let savepoints = self.savepoints.lock().unwrap_or_else(|e| e.into_inner());
            if savepoints.is_empty() {
                return Ok(());
            }
            let wanted = Self::written_tables(stmt);
            // Read what is missing WITHOUT holding the savepoint lock, because
            // reading goes back through storage.
            wanted
                .into_iter()
                .filter(|t| savepoints.iter().any(|sp| !sp.tables.contains_key(t)))
                .collect::<Vec<_>>()
        };
        let mut captured: Vec<(String, Option<Vec<Vec<u8>>>)> = Vec::new();
        self.in_open_transaction(|| {
            for table in tables {
                // A table that does not exist is captured as `None`, which is not
                // the same as an empty one: rolling back has to DROP it.
                let docs = match self.storage.collection_exists(self.db(), &table) {
                    Ok(true) => Some(
                        self.storage
                            .find_matching(self.db(), &table, &Document::new())
                            .map_err(|e| Self::storage_err("could not read for a savepoint", e))?,
                    ),
                    Ok(false) => None,
                    Err(e) => return Err(Self::storage_err("could not read for a savepoint", e)),
                };
                captured.push((table, docs));
            }
            Ok(())
        })?;
        let mut savepoints = self.savepoints.lock().unwrap_or_else(|e| e.into_inner());
        for (table, docs) in captured {
            for sp in savepoints.iter_mut() {
                sp.tables
                    .entry(table.clone())
                    .or_insert_with(|| docs.clone());
            }
        }
        Ok(())
    }

    /// Put one table back to a captured state.
    fn restore_table(&self, table: &str, docs: Option<&Vec<Vec<u8>>>) -> PgWireResult<()> {
        let exists = self
            .storage
            .collection_exists(self.db(), table)
            .map_err(|e| Self::storage_err("could not check a table", e))?;
        match docs {
            // It did not exist at the savepoint, so rolling back drops it.
            None => {
                if exists {
                    self.storage
                        .drop_collection(self.db(), table)
                        .map_err(|e| Self::storage_err("could not drop the table", e))?;
                }
            }
            Some(docs) => {
                if !exists {
                    self.storage
                        .create_collection(self.db(), table)
                        .map_err(|e| Self::storage_err("could not create the table", e))?;
                }
                self.storage
                    .delete_matching(
                        self.db(),
                        table,
                        &Document::new(),
                        0,
                        &Document::new(),
                        None,
                    )
                    .map_err(|e| Self::storage_err("could not clear the table", e))?;
                if !docs.is_empty() {
                    self.storage
                        .insert(self.db(), table, docs.clone(), true)
                        .map_err(|e| Self::storage_err("could not restore the table", e))?;
                }
            }
        }
        Ok(())
    }

    fn note_uncommitted(&self, name: &str, def: Option<TableDef>) {
        if !self.transaction_handle_open() {
            return;
        }
        self.uncommitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), def);
    }

    /// Queue a `ParameterStatus` report if this GUC is one PostgreSQL reports
    /// (GUC_REPORT). Sent to the client after the query completes.
    fn note_reportable_guc(&self, key: &str, value: &str) {
        // Report ONLY the GUCs whose change this server actually HONOURS in
        // output -- reporting one we ignore makes the client switch its parser
        // to a style our output never uses (that is how a past DateStyle report,
        // without matching output, broke ~160 datetime tests, issue #1370).
        //  - TimeZone: a timestamptz renders in it.
        //  - DateStyle: date / timestamp / timestamptz text now renders in it
        //    (see `encode_field_value`), so it is finally safe to report. The
        //    reported value is the CANONICAL spelling psycopg matches on
        //    (`ISO, MDY`, `German, DMY`, ...), not the raw SET text.
        //  - client_encoding: result text is transcoded to it (and query
        //    text / parameters decoded from it), see `encoding.rs`.
        let (report, value) = match key {
            "TimeZone" => (true, value.to_string()),
            "DateStyle" => (true, secantus_pgplan::DateStyle::parse(value).canonical()),
            "client_encoding" => (true, value.to_string()),
            //  - standard_conforming_strings: the statement text is read under
            //    it (`apply_string_syntax`), so libpq's `PQescapeString` may
            //    follow the report -- it switches its own escaping on it.
            "standard_conforming_strings" => (true, value.to_string()),
            _ => (false, String::new()),
        };
        if report {
            self.pending_params
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((key.to_string(), value));
        }
    }

    /// Send a `ParameterStatus` for each GUC change queued by
    /// `note_reportable_guc`. PostgreSQL reports these (TimeZone, DateStyle,
    /// ...) so the client can interpret values -- psycopg re-expresses a
    /// timestamptz in the session `TimeZone` it learns here.
    /// Send the NoticeResponses the statement in flight queued up.
    async fn flush_notices<C>(&self, client: &mut C) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let notices: Vec<ErrorInfo> = std::mem::take(
            &mut *self
                .pending_notices
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for info in notices {
            client
                .send(PgWireBackendMessage::NoticeResponse(info.into()))
                .await?;
        }
        Ok(())
    }

    async fn report_pending_params<C>(&self, client: &mut C) -> PgWireResult<()>
    where
        C: Sink<PgWireBackendMessage> + Unpin,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let pending: Vec<(String, String)> = std::mem::take(
            &mut self
                .pending_params
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for (name, value) in pending {
            client
                .feed(PgWireBackendMessage::ParameterStatus(
                    pgwire::messages::startup::ParameterStatus::new(name, value),
                ))
                .await?;
        }
        Ok(())
    }

    /// Close the cursors a transaction boundary invalidates.
    ///
    /// A COMMIT closes every non-holdable cursor (`WITH HOLD` cursors survive,
    /// their rows already materialised); a ROLLBACK closes ALL of them,
    /// holdable included. After this a `FETCH` of a closed cursor answers
    /// `34000 cursor does not exist`, which is what a client's `ServerCursor`
    /// checks for after `conn.commit()`.
    fn close_cursors_on_txn_end(&self, keep_holdable: bool) {
        let mut cursors = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
        if keep_holdable {
            cursors.retain(|_, c| c.is_holdable);
        } else {
            cursors.clear();
        }
    }

    /// The session's `TimeZone` GUC, resolved.
    fn session_timezone(&self) -> secantus_pgplan::TimeZoneSetting {
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings
            .get("TimeZone")
            .map(|v| secantus_pgplan::TimeZoneSetting::parse(v))
            .unwrap_or_default()
    }

    /// The session's `DateStyle` GUC, resolved. Drives how date / timestamp /
    /// timestamptz TEXT output is rendered (binary output is style-independent).
    fn session_datestyle(&self) -> secantus_pgplan::DateStyle {
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings
            .get("DateStyle")
            .map(|v| secantus_pgplan::DateStyle::parse(v))
            .unwrap_or_default()
    }

    /// The session's `client_encoding` GUC, resolved to its transcoding
    /// behaviour. The stored value is always a canonical PostgreSQL name (the
    /// SET / `set_config` paths refuse anything else), so a bare lookup is
    /// enough; an absent value is the UTF8 default.
    fn client_encoding(&self) -> ClientEncoding {
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings
            .get("client_encoding")
            .map(|v| encoding::client_encoding(v))
            .unwrap_or(ClientEncoding::Utf8)
    }

    /// Set `client_encoding` from a user-supplied name, mirroring PostgreSQL:
    /// an unknown name is `22023`, `MULE_INTERNAL` (a real encoding with no
    /// client conversion) is `0A000`, and a valid name is stored in its
    /// canonical spelling and queued for a `ParameterStatus` report -- which is
    /// safe precisely because output is now transcoded to it.
    fn apply_client_encoding(&self, requested: &str) -> PgWireResult<()> {
        match encoding::canonical_name(requested) {
            Ok(canonical) => {
                self.settings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert("client_encoding".to_string(), canonical.to_string());
                self.note_reportable_guc("client_encoding", canonical);
                Ok(())
            }
            Err(encoding::EncodingError::Invalid) => {
                Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "22023".into(), // invalid_parameter_value
                    format!("invalid value for parameter \"client_encoding\": \"{requested}\""),
                ))))
            }
            Err(encoding::EncodingError::Unconvertible(name)) => {
                Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "0A000".into(), // feature_not_supported
                    format!("conversion between {name} and UTF8 is not supported"),
                ))))
            }
        }
    }

    fn begin_implicit(&self) -> PgWireResult<()> {
        let handle = self.open_transaction_handle()?;
        *self.txn.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        self.in_transaction
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// A transaction handle is open -- a block, or an extended-protocol
    /// statement group. Catalog writes inside either are invisible to the
    /// planner's committed-catalog reads until the handle commits, so the
    /// `uncommitted` overlays must record them for both.
    fn transaction_handle_open(&self) -> bool {
        self.in_transaction
            .load(std::sync::atomic::Ordering::Relaxed)
            || self
                .implicit_extended
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Open the extended-protocol statement group if no transaction handle is
    /// open: the first `Execute` since the last `Sync` outside a block.
    fn open_extended_group(&self) -> PgWireResult<()> {
        let mut guard = self.txn.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return Ok(());
        }
        let handle = self.open_transaction_handle()?;
        *guard = Some(handle);
        self.group_failed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.implicit_extended
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// End the extended-protocol statement group, if one is open: commit it
    /// (deferred constraints checked first, as at any commit) unless a
    /// statement in it failed, in which case roll the whole group back.
    /// No-op when the handle belongs to a block -- a `BEGIN` in the group
    /// made it one, and only `COMMIT` / `ROLLBACK` end that.
    fn close_extended_group(&self, failed: bool) -> PgWireResult<()> {
        if !self
            .implicit_extended
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(());
        }
        let failed = failed
            | self
                .group_failed
                .swap(false, std::sync::atomic::Ordering::Relaxed);
        if !failed {
            return self.commit_implicit();
        }
        self.settle_notifies(false);
        // Not `rollback_implicit`: that closes every cursor, holdable ones
        // included, and a `WITH HOLD` cursor from an earlier, committed
        // transaction survives a failed statement in PostgreSQL. The group
        // itself declared none -- `DECLARE` is refused outside a block.
        self.uncommitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.uncommitted_types
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.deferred_fks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        if let Some(mut handle) = self.txn.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.storage
                .rollback_user_transaction(&mut handle)
                .map_err(|e| Self::storage_err("could not roll back a transaction", e))?;
        }
        Ok(())
    }

    fn commit_implicit(&self) -> PgWireResult<()> {
        self.close_cursors_on_txn_end(true);
        self.savepoints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // The block is over either way, so the failed-transaction gate
        // lifts with it. Left set it would refuse every later statement on
        // this connection, forever.
        self.txn_failed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.uncommitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.uncommitted_types
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.in_transaction
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(mut handle) = self.txn.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let checked = self
                .storage
                .with_user_transaction(&mut handle, || self.run_deferred_checks())
                .map_err(|e| Self::storage_err("transaction failed", e))
                .and_then(|r| r);
            if let Err(e) = checked {
                self.storage
                    .rollback_user_transaction(&mut handle)
                    .map_err(|e| Self::storage_err("could not roll back a transaction", e))?;
                self.settle_notifies(false);
                return Err(e);
            }
            self.storage
                .commit_user_transaction(&mut handle)
                .map_err(|e| Self::storage_err("could not commit a transaction", e))?;
        }
        self.settle_notifies(true);
        Ok(())
    }

    fn rollback_implicit(&self) -> PgWireResult<()> {
        self.settle_notifies(false);
        self.close_cursors_on_txn_end(false);
        self.savepoints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // The block is over either way, so the failed-transaction gate
        // lifts with it. Left set it would refuse every later statement on
        // this connection, forever.
        self.txn_failed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.uncommitted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.uncommitted_types
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.in_transaction
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.deferred_fks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        if let Some(mut handle) = self.txn.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.storage
                .rollback_user_transaction(&mut handle)
                .map_err(|e| Self::storage_err("could not roll back a transaction", e))?;
        }
        Ok(())
    }

    /// Execute one statement. Shared by BOTH protocols so they cannot drift:
    /// the simple path passes no parameters, the extended path passes the
    /// portal's bound values and `Execute`'s row limit.
    async fn run(
        &self,
        query: &str,
        params: &[Bson],
        max_rows: usize,
    ) -> PgWireResult<Vec<Response>> {
        self.run_typed(query, params, &[], max_rows).await
    }

    /// As `run`, and told what type the client DECLARED for each parameter.
    ///
    /// The declared type is not recoverable from the decoded value -- psycopg
    /// sends a small integer as `int2` and `pg_typeof` has to say `smallint` --
    /// so the extended protocol passes it down and the simple protocol, which
    /// has no `Bind` and therefore no declared types, passes nothing.
    async fn run_typed(
        &self,
        query: &str,
        params: &[Bson],
        param_types: &[Option<String>],
        max_rows: usize,
    ) -> PgWireResult<Vec<Response>> {
        // Another backend may have terminated this one while it was idle. Real
        // PostgreSQL tears the connection down asynchronously; here the target
        // notices at its next statement -- COMMIT and ROLLBACK included, since
        // a terminated backend cannot honour them either -- and ends with a
        // FATAL 57P01 that closes the socket.
        if self
            .backend
            .terminate
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(Self::admin_shutdown());
        }
        // A cancel that arrived while this backend was idle is dropped, as
        // PostgreSQL drops one: it targets the statement that is running,
        // and none was.
        self.backend
            .cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // A COPY OUT cancelled mid-stream failed after its statement had
        // answered; the block is poisoned from here, as it is on PostgreSQL.
        if self
            .backend
            .stream_failed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.note_failure();
        }
        self.note_activity("active", Some(query));
        let result = self
            .run_typed_inner(query, params, param_types, max_rows)
            .await;
        // Outside a block the statement was its own transaction: its NOTIFYs
        // go out now, or nowhere if it failed. Inside a block (or an
        // extended-protocol statement group) they wait for the COMMIT.
        if !self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed)
            && !self
                .implicit_extended
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.settle_notifies(result.is_ok());
        }
        self.note_activity(
            if self
                .in_transaction
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                if self.txn_failed.load(std::sync::atomic::Ordering::Relaxed) || result.is_err() {
                    "idle in transaction (aborted)"
                } else {
                    "idle in transaction"
                }
            } else {
                "idle"
            },
            None,
        );
        result
    }

    /// Record what `pg_stat_activity` shows for this backend: the state, and
    /// -- at a statement's start -- its text and `query_start`.
    fn note_activity(&self, state: &'static str, query: Option<&str>) {
        let mut activity = self
            .backend
            .activity
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = bson::DateTime::now();
        activity.state = state;
        activity.state_change = Some(now);
        if let Some(query) = query {
            activity.query = query.to_string();
            activity.query_start = Some(now);
        }
    }

    /// The deadline for the next wait for a frontend message, per PostgreSQL's
    /// idle timeouts: `idle_in_transaction_session_timeout` applies while a
    /// transaction block is open (aborted or not), `idle_session_timeout`
    /// otherwise; `0` (the default) disables each. The wire loop sends the
    /// FATAL error and closes the connection when the deadline passes.
    pub fn idle_timeout(&self) -> Option<(std::time::Duration, ErrorInfo)> {
        let in_txn = self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed);
        let (guc, code, message) = IDLE_TIMEOUT_GUCS[if in_txn { 0 } else { 1 }];
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        let ms = settings.get(guc).and_then(|v| parse_ms_guc(v)).unwrap_or(0);
        if ms <= 0 {
            return None;
        }
        Some((
            std::time::Duration::from_millis(ms as u64),
            ErrorInfo::new("FATAL".into(), code.into(), message.into()),
        ))
    }

    /// PostgreSQL's answer to a statement interrupted by a `CancelRequest`.
    fn query_canceled() -> PgWireError {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "57014".into(), // query_canceled
            "canceling statement due to user request".into(),
        )))
    }

    /// A cancellation point: `57014` if a `CancelRequest` for this backend
    /// has arrived since the running statement started.
    fn check_cancel(&self) -> PgWireResult<()> {
        // A `pg_terminate_backend` aimed at a RUNNING statement ends it, and
        // the session, at the statement's next cancellation point -- the
        // client sees the FATAL within the wait, not after it.
        if self
            .backend
            .terminate
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(Self::admin_shutdown());
        }
        if self.backend.cancelled() {
            return Err(Self::query_canceled());
        }
        Ok(())
    }

    async fn run_typed_inner(
        &self,
        query: &str,
        params: &[Bson],
        param_types: &[Option<String>],
        max_rows: usize,
    ) -> PgWireResult<Vec<Response>> {
        let sql = query.trim().trim_end_matches(';').trim();
        if sql.is_empty() {
            return Ok(vec![Response::EmptyQuery]);
        }

        // The failed-block gate comes BEFORE the planner's answer, because
        // PostgreSQL's does: in an aborted block `select nosuchcolumn` is
        // `25P02`, not `42703`. A SYNTAX error is the exception -- the parser
        // runs first there too, so `selct 1` still answers `42601`.
        let tz = self.session_timezone();
        self.install_user_types();
        let planned = secantus_pgplan::plan_with_session_types(
            sql,
            &|n| self.lookup(n),
            params,
            param_types,
            &tz,
        );
        self.collect_planner_warnings();
        if self.txn_failed.load(std::sync::atomic::Ordering::Relaxed) {
            let ends_the_block = matches!(
                &planned,
                Ok(Statement::Transaction(
                    TransactionControl::Commit { .. }
                        | TransactionControl::Rollback { .. }
                        | TransactionControl::RollbackTo(_)
                ))
            );
            let syntax_error = matches!(&planned, Err(e) if e.sqlstate() == "42601");
            if !ends_the_block && !syntax_error {
                return Err(Self::in_failed_transaction());
            }
        }
        let stmt = planned
            .map_err(|e| Self::err_in(&e, sql))
            .inspect_err(|_| self.note_failure())?;

        // An inline code block runs its statements back through this same
        // path one at a time, so it is neither a storage operation nor a
        // single transaction-control step.
        if let Statement::Do { language, body } = &stmt {
            let out = self.run_do(language, body, sql).await;
            if out.is_err() {
                self.note_failure();
            }
            return out;
        }

        // Transaction control is session state, not a storage operation.
        // ROLLBACK TO is exempt from the failed-block gate below, exactly as
        // COMMIT and ROLLBACK are: rolling back to a savepoint is how a client
        // RECOVERS from the error that poisoned the block.
        if let Statement::Transaction(control) = &stmt {
            let out = match control {
                TransactionControl::Savepoint(_)
                | TransactionControl::Release(_)
                | TransactionControl::RollbackTo(_) => {
                    // A savepoint statement that FAILS poisons the block like
                    // any other: PostgreSQL's `3B001` for an unknown name
                    // leaves the block aborted, and the next statement gets
                    // `25P02` rather than its own error.
                    self.savepoint_control(control)
                        .inspect_err(|_| self.note_failure())
                }
                other => self.transaction_control(other.clone()),
            };
            // A COMMIT publishes the block's DDL to every other connection;
            // a ROLLBACK (to a savepoint or of the block) restores rows the
            // cache may have read past. See `CatalogCache`.
            bump_catalog_version();
            return out;
        }

        // Before anything writes, the open savepoints capture what it is about
        // to change -- there is no savepoint in WiredTiger to do it for us.
        self.capture_for_savepoints(&stmt)
            .inspect_err(|_| self.note_failure())?;

        // DECLARE runs its query NOW and keeps the rows, so the cursor can be
        // scrolled in both directions later. Collecting a row stream needs to
        // await, so it happens here in the async path rather than inside
        // `execute` -- blocking on it there stalled the runtime and hung the
        // connection outright.
        if let Statement::DeclareCursor {
            name,
            query,
            statement,
            scrollable,
            holdable,
            binary: is_binary_cursor,
        } = stmt
        {
            // The BLOCK, not the handle: an extended-protocol statement group
            // holds a handle too, and PostgreSQL refuses a cursor there.
            if !self
                .in_transaction
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "25P01".into(), // no_active_sql_transaction
                    "DECLARE CURSOR can only be used in transaction blocks".into(),
                ))));
            }
            // A cursor's rows are encoded ONCE, at DECLARE, in TEXT: the
            // DECLARE arrives over the simple-query protocol, which is always
            // text, and every client can read a text row whatever format it
            // later asks for. But a BINARY `FETCH` needs binary bytes, which the
            // frozen text cannot supply -- so for a plain SELECT source the
            // resolved values are also captured (`cursor_capture`), to be
            // re-encoded per FETCH.
            let tz = self.session_timezone();
            let capture_typed = matches!(&*query, Statement::Select(_));
            if capture_typed {
                *self
                    .cursor_capture
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
            }
            let binary = self
                .binary_results
                .swap(false, std::sync::atomic::Ordering::Relaxed);
            let responses = self.execute(*query, 0);
            self.binary_results
                .store(binary, std::sync::atomic::Ordering::Relaxed);
            let responses = match responses {
                Ok(r) => r,
                Err(e) => {
                    // Disarm capture on the error path so the buffer does not
                    // leak into an unrelated later statement.
                    self.cursor_capture
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take();
                    return Err(e);
                }
            };
            let Some(Response::Query(q)) = responses.into_iter().next() else {
                self.cursor_capture
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "0A000".into(),
                    "DECLARE CURSOR over this statement is not supported yet".into(),
                ))));
            };
            let schema = q.row_schema.clone();
            let rows = q.data_rows.try_collect::<Vec<_>>().await?;
            let typed_rows = self
                .cursor_capture
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            self.cursors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    name,
                    CursorState {
                        schema,
                        rows,
                        pos: 0,
                        statement,
                        is_holdable: holdable,
                        is_binary: is_binary_cursor,
                        is_scrollable: scrollable,
                        creation_time: bson::DateTime::now(),
                        typed_rows,
                        tz,
                    },
                );
            return Ok(vec![Response::Execution(Tag::new("DECLARE CURSOR"))]);
        }

        // Everything else runs INSIDE the open transaction when there is one,
        // so a later ROLLBACK really discards it.
        //
        // The statement runs synchronously, so it runs under
        // `block_in_place`: a worker that blocks in a long statement
        // (`pg_sleep`, a big scan) would otherwise take the runtime's I/O
        // driver down with it — no other connection is served, and the
        // `CancelRequest` meant to interrupt the statement never arrives.
        let mut guard = self.txn.lock().unwrap_or_else(|e| e.into_inner());
        let out = tokio::task::block_in_place(|| match guard.as_mut() {
            Some(handle) => self
                .storage
                .with_user_transaction(handle, || self.execute(stmt, max_rows))
                .map_err(|e| Self::storage_err("transaction failed", e))
                .and_then(|r| r),
            None => self.execute(stmt, max_rows),
        });
        self.collect_planner_warnings();
        if out.is_err() {
            self.note_failure();
        }
        out
    }

    /// Queue the WARNINGs the planner raised on this thread (an `aclitem`
    /// with no grantor) as NoticeResponses for the statement in flight.
    /// Read statement text the way the session's `standard_conforming_strings`
    /// says to. The parser only knows the setting ON, so when it is off every
    /// plain literal is rewritten to the `E'...'` it means, and the scanner's
    /// `escape_string_warning` notices are queued (see
    /// `secantus_pgplan::escape_strings`). This runs where the text ARRIVES
    /// -- the simple `Query` and the extended `Parse` -- because that is when
    /// PostgreSQL's scanner reads it: a statement prepared under one setting
    /// keeps its meaning when the setting later changes.
    fn apply_string_syntax(&self, sql: &str) -> PgWireResult<String> {
        let (conforming, warn) = {
            let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            (
                settings
                    .get("standard_conforming_strings")
                    .is_none_or(|v| v != "off"),
                settings
                    .get("escape_string_warning")
                    .is_none_or(|v| v != "off"),
            )
        };
        if conforming {
            return Ok(sql.to_string());
        }
        let (rewritten, warnings) = secantus_pgplan::escape_strings::rewrite(sql, warn);
        if !warnings.is_empty() {
            let mut pending = self
                .pending_notices
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for w in warnings {
                let mut info = ErrorInfo::new(
                    "WARNING".into(),
                    secantus_pgplan::escape_strings::SQLSTATE.into(),
                    w.message,
                );
                info.hint = Some(w.hint);
                info.position = Some(w.position.to_string());
                pending.push(info);
            }
        }
        rewritten.map_err(|e| Self::err(&e))
    }

    fn collect_planner_warnings(&self) {
        let warnings = secantus_pgplan::take_warnings();
        if warnings.is_empty() {
            return;
        }
        let mut pending = self
            .pending_notices
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (sqlstate, message) in warnings {
            pending.push(ErrorInfo::new("WARNING".into(), sqlstate, message));
        }
    }

    /// Mark the open transaction failed, if there is one.
    ///
    /// Outside a transaction an error changes nothing: PostgreSQL's own
    /// transition preserves the idle state, and the next statement runs.
    fn note_failure(&self) {
        if self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.txn_failed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // An error anywhere in an extended-protocol statement group -- a
        // `Describe` that cannot resolve the statement as much as an
        // `Execute` -- rolls the group back at `Sync`.
        if self
            .implicit_extended
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.group_failed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Resolve a FROM-less SELECT column that needs connection state.
    fn resolve_const_col(&self, col: &ConstCol) -> PgWireResult<Bson> {
        match col {
            ConstCol::Value(v) => Ok(v.clone()),
            ConstCol::CurrentSetting { name, missing_ok } => {
                let key = canonical_setting(name);
                let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                match settings.get(&key) {
                    Some(v) => Ok(Bson::String(v.clone())),
                    // `current_setting(x)` errors on an unknown name;
                    // `current_setting(x, true)` answers NULL (probed PG 14).
                    None if *missing_ok => Ok(Bson::Null),
                    None => Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42704".into(),
                        format!("unrecognized configuration parameter \"{name}\""),
                    )))),
                }
            }
            ConstCol::SetConfig { name, value, .. } => {
                // `is_local` is accepted and ignored: this server has no
                // statement-scoped settings, and the difference is only
                // observable across a rollback.
                let text = match value {
                    Bson::String(s) => s.clone(),
                    Bson::Null => String::new(),
                    other => format!("{other}"),
                };
                let key = canonical_setting(name);
                if key == "client_encoding" {
                    // Same validation / canonicalisation / report as `SET`, and
                    // the value returned to the caller is the canonical spelling
                    // now stored.
                    self.apply_client_encoding(&text)?;
                    let stored = self
                        .settings
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get("client_encoding")
                        .cloned()
                        .unwrap_or(text);
                    Ok(Bson::String(stored))
                } else {
                    // The same canonicalisation and ParameterStatus report as
                    // `SET`: PostgreSQL reports `set_config('TimeZone', ...)`
                    // exactly as it reports `SET TimeZone` (measured on 16 --
                    // psycopg builds a timestamptz's tzinfo from that report,
                    // so without it a session-zone change was invisible to
                    // the client's loader).
                    let text = if key == "DateStyle" {
                        secantus_pgplan::DateStyle::parse(&text).canonical()
                    } else if BOOL_GUCS.contains(&key.as_str()) {
                        canonical_bool_guc(&key, &text)?
                    } else {
                        text
                    };
                    let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                    settings.insert(key.clone(), text.clone());
                    drop(settings);
                    self.note_reportable_guc(&key, &text);
                    Ok(Bson::String(text))
                }
            }
            ConstCol::BackendPid => Ok(Bson::Int32(
                self.backend_pid.load(std::sync::atomic::Ordering::Relaxed),
            )),
            ConstCol::SessionUser => Ok(Bson::String(
                self.session_user
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            )),
            ConstCol::CurrentDatabase => Ok(Bson::String(self.db().to_string())),
            // `pg_sleep(NULL)` is NULL (strict); zero or negative seconds
            // return at once; otherwise the wait is the given fraction of a
            // second. The `void` result renders as `''` (probed PG 16).
            ConstCol::Sleep(seconds) => {
                let secs = match seconds {
                    Bson::Null => return Ok(Bson::Null),
                    Bson::Double(d) => *d,
                    Bson::Int32(i) => f64::from(*i),
                    Bson::Int64(i) => *i as f64,
                    _ => 0.0,
                };
                if secs > 0.0 && secs.is_finite() {
                    // In slices, so a `CancelRequest` interrupts the sleep:
                    // `pg_sleep` is the statement every cancel test cancels.
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
                    loop {
                        self.check_cancel()?;
                        let left = deadline.saturating_duration_since(std::time::Instant::now());
                        if left.is_zero() {
                            break;
                        }
                        std::thread::sleep(left.min(std::time::Duration::from_millis(5)));
                    }
                }
                Ok(Bson::String(String::new()))
            }
            ConstCol::PgNotify { channel, payload } => {
                // `pg_notify(NULL, ...)` and `pg_notify('', ...)` are the same
                // 22023; a NULL payload is the empty string. Probed PG 16.
                let channel = match channel {
                    Bson::String(c) => c.clone(),
                    _ => String::new(),
                };
                let payload = match payload {
                    Bson::String(p) => p.clone(),
                    Bson::Null => String::new(),
                    other => other.to_string(),
                };
                self.queue_notify(&channel, &payload)?;
                Ok(Bson::String(String::new()))
            }
            ConstCol::ListeningChannels => Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "0A000".into(), // feature_not_supported
                "pg_listening_channels() beside other columns is not supported yet".into(),
            )))),
            // Read from the source row by `const_rows`; never resolved alone.
            ConstCol::FromColumn => Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "XX000".into(), // internal_error
                "a FROM column outside its source".into(),
            )))),
            ConstCol::TerminateBackend(inner) | ConstCol::CancelBackend(inner) => {
                let terminate = matches!(col, ConstCol::TerminateBackend(_));
                let name = if terminate {
                    "pg_terminate_backend"
                } else {
                    "pg_cancel_backend"
                };
                let target = match self.resolve_const_col(inner)? {
                    Bson::Int32(i) => i64::from(i),
                    Bson::Int64(i) => i,
                    Bson::Double(d) => d as i64,
                    Bson::Null => {
                        // `pg_terminate_backend(NULL)` is NULL in PostgreSQL.
                        return Ok(Bson::Null);
                    }
                    other => {
                        return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                            "ERROR".into(),
                            "22023".into(), // invalid_parameter_value
                            format!("{name}() PID must be an integer, not {other}"),
                        ))));
                    }
                };
                let my_pid = i64::from(self.backend_pid.load(std::sync::atomic::Ordering::Relaxed));
                if target == my_pid {
                    // Our own backend: the CURRENT statement is the one that
                    // dies (or is cancelled), so raise it now rather than
                    // arming a flag for a next statement that will never come.
                    return Err(if terminate {
                        Self::admin_shutdown()
                    } else {
                        Self::query_canceled()
                    });
                }
                // Another backend: arm its flag if it is live. PostgreSQL
                // returns true when the signal was sent, false -- with a
                // WARNING -- when no such backend exists (probed 16).
                let target = i32::try_from(target).unwrap_or(0);
                let armed = backend_registry()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&target)
                    .map(|entry| {
                        if terminate {
                            entry
                                .terminate
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        } else {
                            entry
                                .cancel
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        // An idle target hears a terminate from its idle
                        // wait; an active one from its next cancellation
                        // point (a cancel to an idle backend is dropped, as
                        // PostgreSQL drops one).
                        entry.wake.notify_one();
                    })
                    .is_some();
                if !armed {
                    let mut info = ErrorInfo::new(
                        "WARNING".into(),
                        "01000".into(), // warning
                        format!("PID {target} is not a PostgreSQL backend process"),
                    );
                    info.routine = Some("pg_signal_backend".into());
                    self.pending_notices
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(info);
                }
                Ok(Bson::Boolean(armed))
            }
        }
    }

    /// Run `f` inside the open user transaction, if there is one.
    ///
    /// A savepoint's captures and restores MUST go through the transaction's
    /// own session: a read outside it cannot see the block's uncommitted rows,
    /// and a write outside it lands in a different snapshot -- which is why the
    /// first version of this restored nothing at all while reporting success.
    fn in_open_transaction<T>(&self, f: impl FnOnce() -> PgWireResult<T>) -> PgWireResult<T> {
        let mut guard = self.txn.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(handle) => self
                .storage
                .with_user_transaction(handle, f)
                .map_err(|e| Self::storage_err("transaction failed", e))?,
            None => f(),
        }
    }

    /// `25001` for the statements PostgreSQL refuses inside a block
    /// (`CREATE DATABASE`, `DROP DATABASE`, ...).
    fn refuse_in_transaction_block(&self, what: &str) -> PgWireResult<()> {
        if self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "25001".into(), // active_sql_transaction
                format!("{what} cannot run inside a transaction block"),
            ))));
        }
        Ok(())
    }

    /// PostgreSQL's answer to any statement in a block that has already failed.
    fn in_failed_transaction() -> PgWireError {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "25P02".into(), // in_failed_sql_transaction
            "current transaction is aborted, commands ignored until end of \
             transaction block"
                .into(),
        )))
    }

    /// The `57P01` a backend sends as it is torn down by `pg_terminate_backend`.
    ///
    /// Severity is `FATAL`, which is what makes pgwire close the socket after
    /// the `ErrorResponse` (see `is_fatal`) -- so the client sees the same
    /// `AdminShutdown` + disconnect a real terminated backend produces.
    fn admin_shutdown() -> PgWireError {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "FATAL".into(),
            "57P01".into(), // admin_shutdown
            "terminating connection due to administrator command".into(),
        )))
    }

    /// Queue a NOTIFY for the open transaction.
    ///
    /// PostgreSQL's rules, probed on 16: the channel must be non-empty
    /// (22023), the payload is capped at 7999 bytes (22023), and a
    /// `(channel, payload)` pair already queued in this transaction is not
    /// queued again -- the listener gets it once, in first-issue order.
    fn queue_notify(&self, channel: &str, payload: &str) -> PgWireResult<()> {
        if channel.is_empty() {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "22023".into(), // invalid_parameter_value
                "channel name cannot be empty".into(),
            ))));
        }
        if payload.len() >= 8000 {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "22023".into(), // invalid_parameter_value
                "payload string too long".into(),
            ))));
        }
        let mut pending = self
            .pending_notifies
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !pending.iter().any(|(c, p)| c == channel && p == payload) {
            pending.push((channel.to_string(), payload.to_string()));
        }
        Ok(())
    }

    /// The transaction ended: apply its LISTEN / UNLISTENs and deliver its
    /// NOTIFYs to every listening backend's inbox (this one's included) on
    /// commit; drop both on rollback.
    fn settle_notifies(&self, commit: bool) {
        let listens = std::mem::take(
            &mut *self
                .pending_listens
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        let notifies = std::mem::take(
            &mut *self
                .pending_notifies
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        if !commit {
            return;
        }
        if !listens.is_empty() {
            let mut listening = self
                .backend
                .listening
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for op in listens {
                match op {
                    ListenOp::Listen(c) => {
                        if !listening.contains(&c) {
                            listening.push(c);
                        }
                    }
                    ListenOp::Unlisten(c) => listening.retain(|l| *l != c),
                    ListenOp::UnlistenAll => listening.clear(),
                }
            }
        }
        if notifies.is_empty() {
            return;
        }
        let pid = self.backend_pid.load(std::sync::atomic::Ordering::Relaxed);
        let registry = backend_registry().lock().unwrap_or_else(|e| e.into_inner());
        for entry in registry.values() {
            let mut delivered = false;
            {
                let listening = entry.listening.lock().unwrap_or_else(|e| e.into_inner());
                let mut inbox = entry.inbox.lock().unwrap_or_else(|e| e.into_inner());
                for (channel, payload) in &notifies {
                    if listening.contains(channel) {
                        inbox.push_back(Notification {
                            pid,
                            channel: channel.clone(),
                            payload: payload.clone(),
                        });
                        delivered = true;
                    }
                }
            }
            if delivered {
                entry.wake.notify_one();
            }
        }
    }

    /// The `NotificationResponse`s this backend owes its client right now:
    /// everything in the inbox, unless a transaction block is open --
    /// PostgreSQL holds them until the block ends (probed 16: a listener
    /// idle in a block hears nothing until its COMMIT).
    fn drain_notifications(&self) -> Vec<PgWireBackendMessage> {
        if self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Vec::new();
        }
        let mut inbox = self.backend.inbox.lock().unwrap_or_else(|e| e.into_inner());
        inbox
            .drain(..)
            .map(|n| {
                PgWireBackendMessage::NotificationResponse(NotificationResponse::new(
                    n.pid, n.channel, n.payload,
                ))
            })
            .collect()
    }

    /// The rows of a FROM-less SELECT: one, of its resolved columns -- or
    /// none under a false WHERE (and nothing resolved, so a `pg_sleep()`
    /// behind it does not wait) -- or, for `SELECT pg_listening_channels()`,
    /// one per channel in LISTEN order, the set-returning shape.
    fn const_rows(&self, sc: &secantus_pgplan::SelectConstant) -> PgWireResult<Vec<Vec<Bson>>> {
        if !sc.where_true {
            return Ok(Vec::new());
        }
        let channels = || -> Vec<Bson> {
            self.backend
                .listening
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|c| Bson::String(c.clone()))
                .collect()
        };
        if let [(_, ConstCol::ListeningChannels, _, _)] = sc.columns.as_slice() {
            return Ok(channels().into_iter().map(|c| vec![c]).collect());
        }
        // `FROM function(...)`: the function is the row source -- one row
        // per result of a set-returning one, one row otherwise -- and is
        // evaluated exactly once, before the select list.
        let source_rows: Vec<Option<Bson>> = match sc.source.as_deref() {
            None => vec![None],
            Some(ConstCol::ListeningChannels) => channels().into_iter().map(Some).collect(),
            Some(col) => vec![Some(self.resolve_const_col(col)?)],
        };
        source_rows
            .into_iter()
            .map(|source| {
                sc.columns
                    .iter()
                    .map(|(_, c, _, _)| match c {
                        ConstCol::FromColumn => Ok(source.clone().unwrap_or(Bson::Null)),
                        other => self.resolve_const_col(other),
                    })
                    .collect::<PgWireResult<Vec<_>>>()
            })
            .collect()
    }

    /// Send the owed `NotificationResponse`s, before a `ReadyForQuery`.
    async fn flush_notifications<C>(&self, client: &mut C) -> PgWireResult<()>
    where
        C: Sink<PgWireBackendMessage> + Unpin + Send,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        for message in self.drain_notifications() {
            client.feed(message).await?;
        }
        Ok(())
    }

    /// SAVEPOINT / RELEASE / ROLLBACK TO.
    ///
    /// All three need an open block: PostgreSQL answers `25P01` outside one,
    /// and a name that is not open is `3B001`. A repeated name SHADOWS rather
    /// than replaces, so the search is from the innermost outwards.
    fn savepoint_control(&self, control: &TransactionControl) -> PgWireResult<Vec<Response>> {
        let word = match control {
            TransactionControl::Savepoint(_) => "SAVEPOINT",
            TransactionControl::Release(_) => "RELEASE SAVEPOINT",
            _ => "ROLLBACK TO SAVEPOINT",
        };
        if !self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "25P01".into(), // no_active_sql_transaction
                format!("{word} can only be used in transaction blocks"),
            ))));
        }
        let name = match control {
            TransactionControl::Savepoint(n)
            | TransactionControl::Release(n)
            | TransactionControl::RollbackTo(n) => n.clone(),
            _ => unreachable!("not a savepoint statement"),
        };

        // The innermost savepoint of that name, since a repeated name shadows.
        let index = |savepoints: &Vec<Savepoint>| -> Option<usize> {
            savepoints.iter().rposition(|sp| sp.name == name)
        };
        let missing = || -> PgWireError {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "3B001".into(), // invalid_savepoint_specification
                format!("savepoint \"{name}\" does not exist"),
            )))
        };

        match control {
            TransactionControl::Savepoint(_) => {
                let uncommitted = self
                    .uncommitted
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let uncommitted_types = self
                    .uncommitted_types
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                self.savepoints
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(Savepoint {
                        name,
                        tables: HashMap::new(),
                        uncommitted,
                        uncommitted_types,
                    });
                Ok(vec![Response::Execution(Tag::new("SAVEPOINT"))])
            }
            // RELEASE keeps the writes, so nothing is restored -- but the
            // pre-images captured by the savepoints being destroyed have to
            // MERGE DOWN into the enclosing one, or an outer ROLLBACK TO would
            // no longer be able to undo them. The OLDEST capture wins, which is
            // the one nearest the enclosing savepoint.
            TransactionControl::Release(_) => {
                let mut savepoints = self.savepoints.lock().unwrap_or_else(|e| e.into_inner());
                let idx = index(&savepoints).ok_or_else(missing)?;
                let dropped: Vec<Savepoint> = savepoints.split_off(idx);
                if let Some(outer) = savepoints.last_mut() {
                    for sp in dropped {
                        for (table, docs) in sp.tables {
                            outer.tables.entry(table).or_insert(docs);
                        }
                    }
                }
                Ok(vec![Response::Execution(Tag::new("RELEASE"))])
            }
            _ => {
                // Restore from the OLDEST capture of each table among this
                // savepoint and the ones nested inside it: that is the state at
                // the named savepoint, whichever frame happened to capture it.
                let (restore, uncommitted, uncommitted_types) = {
                    let mut savepoints = self.savepoints.lock().unwrap_or_else(|e| e.into_inner());
                    let idx = index(&savepoints).ok_or_else(missing)?;
                    let uncommitted = savepoints[idx].uncommitted.clone();
                    let uncommitted_types = savepoints[idx].uncommitted_types.clone();
                    let dropped: Vec<Savepoint> = savepoints.split_off(idx + 1);
                    let mut restore: HashMap<String, Option<Vec<Vec<u8>>>> =
                        savepoints[idx].tables.clone();
                    for sp in &dropped {
                        for (table, docs) in &sp.tables {
                            restore.entry(table.clone()).or_insert_with(|| docs.clone());
                        }
                    }
                    // The savepoint itself stays open, and starts capturing
                    // again from the state just restored.
                    savepoints[idx].tables.clear();
                    (restore, uncommitted, uncommitted_types)
                };
                self.in_open_transaction(|| {
                    for (table, docs) in &restore {
                        self.restore_table(table, docs.as_ref())?;
                    }
                    Ok(())
                })?;
                *self.uncommitted.lock().unwrap_or_else(|e| e.into_inner()) = uncommitted;
                *self
                    .uncommitted_types
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = uncommitted_types;
                // Rolling back to a savepoint UN-POISONS the block: PostgreSQL
                // lets the session carry on from there, which is the whole
                // point of the nested-block pattern that uses it.
                self.txn_failed
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                Ok(vec![Response::Execution(Tag::new("ROLLBACK"))])
            }
        }
    }

    /// BEGIN / START TRANSACTION / COMMIT / ROLLBACK, with `AND CHAIN`.
    /// Apply the transaction characteristics of a `BEGIN` / `START TRANSACTION`
    /// to the `transaction_*` GUCs for the life of the block.
    ///
    /// PostgreSQL resets `transaction_*` to the session `default_transaction_*`
    /// when a block opens, then overlays whatever modes the statement named. So
    /// a bare `BEGIN` reflects the defaults, and `BEGIN ISOLATION LEVEL
    /// SERIALIZABLE` reflects `serializable` for the isolation and the defaults
    /// for the rest. This server is single-node and does not enforce isolation;
    /// it only reflects what was requested so a client reading
    /// `current_setting('transaction_isolation')` sees its own choice.
    fn apply_transaction_modes_on_begin(&self, modes: &TransactionModes) {
        let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        let default_of = |settings: &HashMap<String, String>, key: &str| -> String {
            settings.get(key).cloned().unwrap_or_default()
        };
        let isolation = modes
            .isolation
            .clone()
            .unwrap_or_else(|| default_of(&settings, "default_transaction_isolation"));
        let read_only = match modes.read_only {
            Some(v) => if v { "on" } else { "off" }.to_string(),
            None => default_of(&settings, "default_transaction_read_only"),
        };
        let deferrable = match modes.deferrable {
            Some(v) => if v { "on" } else { "off" }.to_string(),
            None => default_of(&settings, "default_transaction_deferrable"),
        };
        settings.insert("transaction_isolation".into(), isolation);
        settings.insert("transaction_read_only".into(), read_only);
        settings.insert("transaction_deferrable".into(), deferrable);
    }

    /// Reset the `transaction_*` GUCs to the session `default_transaction_*`
    /// when a block ends, so a subsequent standalone `current_setting` reads the
    /// defaults rather than the last block's overrides -- exactly what real
    /// PostgreSQL reports after a `COMMIT` / `ROLLBACK`.
    fn reset_transaction_gucs(&self) {
        let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        for (tx, def) in [
            ("transaction_isolation", "default_transaction_isolation"),
            ("transaction_read_only", "default_transaction_read_only"),
            ("transaction_deferrable", "default_transaction_deferrable"),
        ] {
            let value = settings.get(def).cloned().unwrap_or_default();
            settings.insert(tx.into(), value);
        }
    }

    fn transaction_control(&self, control: TransactionControl) -> PgWireResult<Vec<Response>> {
        let opens = matches!(
            control,
            TransactionControl::Begin(_) | TransactionControl::Start(_)
        );
        // Whatever the transaction did to the catalog is either committed or
        // discarded once it ends, so the pending map stops being the truth.
        if !opens {
            self.uncommitted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            self.uncommitted_types
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        // Every savepoint belongs to the block that is starting or ending.
        self.savepoints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let mut guard = self.txn.lock().unwrap_or_else(|e| e.into_inner());
        // A COMMIT of a FAILED transaction is a rollback, and PostgreSQL says
        // so in the command tag: `ROLLBACK`, not `COMMIT`. A client that
        // believed a `COMMIT` tag would think discarded work had landed.
        let failed = self
            .txn_failed
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        let control = match control {
            TransactionControl::Commit { chain } if failed => {
                TransactionControl::Rollback { chain }
            }
            other => other,
        };

        let begin = |guard: &mut Option<UserTransactionHandle>| -> PgWireResult<()> {
            // A BEGIN inside an extended-protocol statement group makes the
            // group's transaction the block: same handle, now explicit.
            if self
                .implicit_extended
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                self.in_transaction
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.txn_failed
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            }
            if guard.is_none() {
                let handle = self.open_transaction_handle()?;
                *guard = Some(handle);
                self.in_transaction
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.txn_failed
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        };

        let (tag, chain) = match control {
            // A BEGIN inside a transaction is a WARNING in PostgreSQL, not an
            // error, and the existing transaction continues.
            TransactionControl::Begin(modes) => {
                begin(&mut guard)?;
                self.apply_transaction_modes_on_begin(&modes);
                ("BEGIN", false)
            }
            // Same statement, different word: the tag is what a client reads
            // back, and `START TRANSACTION` answers with its own.
            TransactionControl::Start(modes) => {
                begin(&mut guard)?;
                self.apply_transaction_modes_on_begin(&modes);
                ("START TRANSACTION", false)
            }
            TransactionControl::Commit { chain } => {
                self.reset_transaction_gucs();
                // A COMMIT with no BEGIN commits the statement group so far;
                // what follows before the Sync starts a new one.
                self.implicit_extended
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                // A COMMIT closes every non-holdable cursor; `WITH HOLD`
                // survives with its rows already materialised.
                self.close_cursors_on_txn_end(true);
                if let Some(mut handle) = guard.take() {
                    self.in_transaction
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    // INITIALLY DEFERRED constraints are checked now, inside
                    // the transaction; a violation rolls it back and the
                    // error is the COMMIT's answer, with the connection IDLE.
                    let checked = self
                        .storage
                        .with_user_transaction(&mut handle, || self.run_deferred_checks())
                        .map_err(|e| Self::storage_err("transaction failed", e))
                        .and_then(|r| r);
                    if let Err(e) = checked {
                        self.storage
                            .rollback_user_transaction(&mut handle)
                            .map_err(|e| Self::storage_err("could not roll back", e))?;
                        self.commit_failed
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        return Err(e);
                    }
                    self.storage
                        .commit_user_transaction(&mut handle)
                        .map_err(|e| Self::storage_err("could not commit", e))?;
                }
                self.settle_notifies(true);
                ("COMMIT", chain)
            }
            TransactionControl::Rollback { chain } => {
                self.reset_transaction_gucs();
                self.implicit_extended
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                // A ROLLBACK closes ALL cursors, holdable included.
                self.close_cursors_on_txn_end(false);
                self.deferred_fks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                if let Some(mut handle) = guard.take() {
                    self.in_transaction
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    self.storage
                        .rollback_user_transaction(&mut handle)
                        .map_err(|e| Self::storage_err("could not roll back", e))?;
                }
                self.settle_notifies(false);
                ("ROLLBACK", chain)
            }
            // Handled before this, in `run`: they are statements INSIDE a
            // block rather than ways of starting or ending one.
            TransactionControl::Savepoint(_)
            | TransactionControl::Release(_)
            | TransactionControl::RollbackTo(_) => {
                unreachable!("savepoint statements are handled by savepoint_control")
            }
        };
        // `AND CHAIN` ends the block and opens another one immediately, so the
        // connection is still in a transaction when the answer arrives. A
        // client that chained and was left IDLE would have its next statements
        // autocommitted one by one.
        if chain {
            begin(&mut guard)?;
        }

        // Not `Execution`: pgwire tracks the transaction status that rides on
        // every `ReadyForQuery` from these two responses, and a plain
        // execution tag left every connection reporting IDLE -- inside a
        // transaction, and after an error inside one.
        Ok(vec![if opens || chain {
            Response::TransactionStart(Tag::new(tag))
        } else {
            Response::TransactionEnd(Tag::new(tag))
        }])
    }

    /// Draw the next `count` values of sequence `name` and move it past them.
    ///
    /// The first draw returns `last_value` as it stands (`is_called` false);
    /// every later one adds the increment. Exhausting `max_value` is 2200H,
    /// as PostgreSQL's `nextval` reports it. The move is a storage write, so
    /// it rolls back with the transaction -- PostgreSQL never re-issues a
    /// value; this server can.
    fn nextval(&self, name: &str, count: usize) -> PgWireResult<Vec<i64>> {
        let filter = bson::doc! { "_id": name };
        let raw = self
            .storage
            .find_matching(self.db(), SEQUENCE_COLLECTION, &filter)
            .map_err(|e| Self::storage_err("could not read the sequence", e))?;
        let Some(raw) = raw.first() else {
            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "42P01".into(), // undefined_table
                format!("relation \"{name}\" does not exist"),
            ))));
        };
        let seq: Document = bson::from_slice(raw)
            .map_err(|e| Self::storage_err("could not decode the sequence", e))?;
        let int = |key: &str| seq.get(key).and_then(bson_i64);
        let mut last = int("last_value").unwrap_or(1);
        let increment = int("increment").unwrap_or(1);
        let max_value = int("max_value").unwrap_or(i64::MAX);
        let mut called = seq.get_bool("is_called").unwrap_or(false);
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            let next = if called {
                last.checked_add(increment).filter(|v| *v <= max_value)
            } else {
                Some(last)
            };
            let Some(next) = next else {
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "2200H".into(), // sequence_generator_limit_exceeded
                    format!("nextval: reached maximum value of sequence \"{name}\" ({max_value})"),
                ))));
            };
            values.push(next);
            last = next;
            called = true;
        }
        if !values.is_empty() {
            self.storage
                .update_matching(
                    self.db(),
                    SEQUENCE_COLLECTION,
                    &filter,
                    &bson::doc! { "$set": { "last_value": last, "is_called": true } },
                    false,
                    false,
                    &[],
                    &Document::new(),
                    None,
                    None,
                    false,
                )
                .map_err(|e| Self::storage_err("could not advance the sequence", e))?;
        }
        Ok(values)
    }

    /// Fill each row's omitted `serial` columns from their sequences, as the
    /// column default PostgreSQL attaches. A column the row names -- even as
    /// NULL -- keeps what it was given; only an ABSENT one draws a value.
    fn apply_serial_defaults(&self, def: &TableDef, rows: &mut [Document]) -> PgWireResult<()> {
        for column in &def.columns {
            let Some(sequence) = column.sequence.as_deref() else {
                continue;
            };
            let field = column.field();
            let missing: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, d)| !d.contains_key(&field))
                .map(|(i, _)| i)
                .collect();
            let values = self.nextval(sequence, missing.len())?;
            for (i, v) in missing.into_iter().zip(values) {
                let value = match column.pg_type.as_str() {
                    "int8" => Bson::Int64(v),
                    _ => i32::try_from(v).map(Bson::Int32).unwrap_or(Bson::Int64(v)),
                };
                rows[i].insert(field.clone(), value);
            }
        }
        Ok(())
    }

    /// The RowDescription for a projection over one table's rows: a computed
    /// column's TYPE comes from its expression (the last cast, or the call's
    /// fixed result type), a plain one from the catalog. The describe pass and
    /// the executor share this rule or the client decodes rows against the
    /// wrong oid.
    fn row_schema(
        &self,
        def: &TableDef,
        columns: &[(String, String)],
        casts: &[Option<secantus_pgplan::ColumnExpr>],
    ) -> Vec<FieldInfo> {
        columns
            .iter()
            .enumerate()
            .map(|(i, (out, field))| {
                let ty = match casts.get(i).and_then(|c| c.as_ref()) {
                    Some(expr) => wire_type(secantus_pgplan::column_expr_type(expr)),
                    None => def
                        .column(field)
                        .or_else(|| def.column(out))
                        .map(|c| {
                            self.user_wire_type(&c.pg_type)
                                .unwrap_or_else(|| wire_type(&c.pg_type))
                        })
                        .unwrap_or(Type::VARCHAR),
                };
                self.field(out.clone(), ty)
            })
            .collect()
    }

    /// The table definition a SELECT's projection reads through: the join's
    /// output columns, the series' one int4 column, the virtual table, or the
    /// catalog entry.
    fn select_def(&self, sel: &secantus_pgplan::Select) -> PgWireResult<TableDef> {
        if let Some(join) = &sel.join {
            return secantus_pgplan::join_output_def(join, &|n| self.lookup(n))
                .map_err(|e| Self::err(&e));
        }
        if let Some(series) = &sel.series {
            return Ok(series_table_def(series));
        }
        if let Some(def) = Self::virtual_table(&sel.table) {
            return Ok(def);
        }
        self.lookup(&sel.table)
            .ok_or_else(|| Self::err(&PlanError::UndefinedTable(sel.table.clone())))
    }

    /// The documents a SELECT reads, after its source (table, series, join or
    /// virtual table), ORDER BY, OFFSET, LIMIT and the protocol row cap, with
    /// the table definition the projection reads them through. Everything
    /// that consumes a SELECT -- the row encoder, `COPY (query)`,
    /// `INSERT ... SELECT` -- starts here, so there is one reader to be wrong.
    fn select_docs(
        &self,
        sel: &secantus_pgplan::Select,
        max_rows: usize,
    ) -> PgWireResult<(Vec<Document>, TableDef)> {
        // A generated source stands in for the table. Everything after
        // this point -- ORDER BY, OFFSET, LIMIT, the encoder -- works
        // on documents and does not care where they came from, which is
        // why the series is a SOURCE rather than its own statement.
        let (mut docs, def): (Vec<Document>, TableDef) = match (&sel.series, &sel.join) {
            // A top-level JOIN source: materialise it, treat its
            // output columns as the table.
            (_, Some(join)) => {
                // A subquery join side is materialised first (its rows
                // keyed by the sub-plan's output names); a table side
                // stays `None` and `join_docs_with` reads it itself.
                let left_rows = match &join.left_sub {
                    Some(stmt) => Some(self.sub_plan_rows(stmt)?),
                    None => None,
                };
                let right_rows = match &join.right_sub {
                    Some(stmt) => Some(self.sub_plan_rows(stmt)?),
                    None => None,
                };
                let docs = self.join_docs_with(join, left_rows, right_rows)?;
                let def = secantus_pgplan::join_output_def(join, &|n| self.lookup(n))
                    .map_err(|e| Self::err(&e))?;
                (docs, def)
            }
            (Some(series), _) => {
                let column = series.column.clone();
                let empty = Document::new();
                let docs = series
                    .values()
                    .into_iter()
                    .map(|v| {
                        let mut d = Document::new();
                        d.insert(column.clone(), Bson::Int32(v as i32));
                        d
                    })
                    // The WHERE clause, over the one generated column.
                    .filter(|d| {
                        sel.filter.is_empty()
                            || secantus_core::query::matches(d, &sel.filter, &empty, None)
                                .unwrap_or(false)
                    })
                    .collect();
                (docs, series_table_def(series))
            }
            (None, _) if Self::virtual_table(&sel.table).is_some() => {
                let docs = self.virtual_rows(&sel.table, &sel.filter).expect("checked");
                (docs, Self::virtual_table(&sel.table).expect("checked"))
            }
            (None, _) => {
                let raw = self
                    .storage
                    .find_matching(self.db(), &sel.table, &sel.filter)
                    .map_err(|e| Self::storage_err("could not read", e))?;
                let def = self
                    .lookup(&sel.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(sel.table.clone())))?;
                // Decode once: ORDER BY, OFFSET and LIMIT all need the
                // values, and re-decoding per comparison is quadratic.
                let docs: Vec<Document> = raw
                    .iter()
                    .map(|b| bson::from_slice(b))
                    .collect::<Result<_, _>>()
                    .map_err(|e| Self::storage_err("could not decode a row", e))?;
                (docs, def)
            }
        };
        // A cancellation point between the scan and the sort: cooperative,
        // like the storage layer's own `maxTimeMS` polling.
        self.check_cancel()?;

        if !sel.order.is_empty() {
            sort_rows(&mut docs, &sel.order);
        }
        // OFFSET is applied before LIMIT, as PostgreSQL does.
        if sel.offset > 0 {
            let skip = usize::try_from(sel.offset).unwrap_or(usize::MAX);
            docs = docs.into_iter().skip(skip).collect();
        }
        if let Some(limit) = sel.limit {
            // A negative LIMIT is a PostgreSQL error, but the parser
            // hands it through; clamp rather than panic on the cast.
            let take = usize::try_from(limit.max(0)).unwrap_or(usize::MAX);
            docs.truncate(take);
        }
        // `Execute` may cap rows independently of any SQL LIMIT; 0 means
        // "no cap" in the protocol, not "no rows".
        if max_rows > 0 {
            docs.truncate(max_rows);
        }
        Ok((docs, def))
    }

    /// Every row of a row-producing statement as resolved values, in output
    /// column order. This is `COPY (query) TO STDOUT` and `INSERT ... SELECT`
    /// reading a query the same way the wire encoder does, casts and
    /// expressions included -- `copy (select id + 1 from t) to stdout` used to
    /// read the bare column and write `id`.
    fn query_rows(&self, inner: &Statement) -> PgWireResult<Vec<Vec<Option<Bson>>>> {
        match inner {
            Statement::SelectConstant(sc) => Ok(self
                .const_rows(sc)?
                .into_iter()
                .map(|row| row.into_iter().map(Some).collect())
                .collect()),
            Statement::ValuesConstant(vc) => Ok(vc
                .rows
                .iter()
                .map(|r| r.iter().map(|v| Some(v.clone())).collect())
                .collect()),
            Statement::Select(sel) => {
                let (docs, def) = self.select_docs(sel, 0)?;
                let schema = self.row_schema(&def, &sel.columns, &sel.casts);
                let tz = self.session_timezone();
                docs.iter()
                    .map(|d| {
                        sel.columns
                            .iter()
                            .enumerate()
                            .map(|(i, (_, field))| {
                                resolve_cell(
                                    d,
                                    field,
                                    sel.casts.get(i).and_then(|c| c.as_ref()),
                                    schema[i].datatype(),
                                    &tz,
                                )
                            })
                            .collect()
                    })
                    .collect()
            }
            _ => Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "0A000".into(),
                "reading rows from this statement is not supported yet".into(),
            )))),
        }
    }

    /// Encode `docs` through a projection -- a SELECT's select list or an
    /// INSERT's RETURNING list -- as a query response tagged `SELECT`.
    fn project_rows(
        &self,
        docs: Vec<Document>,
        def: &TableDef,
        columns: &[(String, String)],
        casts: &[Option<secantus_pgplan::ColumnExpr>],
        env: &RowEnv,
    ) -> PgWireResult<QueryResponse> {
        let schema = Arc::new(self.row_schema(def, columns, casts));
        let fields: Vec<String> = columns.iter().map(|(_, f)| f.clone()).collect();
        let casts = casts.to_vec();
        let (row_tz, row_ds, row_cenc) = (env.tz.clone(), env.ds, env.cenc);
        let tz = self.session_timezone();
        let schema_ref = schema.clone();
        // A `DECLARE CURSOR` over this SELECT arms row capture (see
        // `cursor_capture`); a plain SELECT leaves it disarmed and pays
        // only the `Option` check below -- no lock, no extra clone.
        let capture = {
            let armed = self
                .cursor_capture
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            armed.then(|| self.cursor_capture.clone())
        };
        let rows = stream::iter(docs).map(move |d| {
            let mut enc = DataRowEncoder::new(schema_ref.clone());
            let mut captured: Option<Vec<Option<Bson>>> =
                capture.as_ref().map(|_| Vec::with_capacity(fields.len()));
            for (i, f) in fields.iter().enumerate() {
                // A computed column is applied per row -- the cast
                // chain or the scalar call the planner recorded.
                if let Some(expr) = casts.get(i).and_then(|c| c.as_ref()) {
                    let v = if matches!(expr, secantus_pgplan::ColumnExpr::Row { .. }) {
                        // An expression over the row sees every
                        // column, not just the one it is filed under.
                        secantus_pgplan::apply_row_expr(expr, &d)
                    } else {
                        let v = d.get(f).cloned().unwrap_or(Bson::Null);
                        secantus_pgplan::apply_column_expr(expr, v, &tz)
                    }
                    .map_err(|e| PgHandler::err(&e))?;
                    encode_field_value(
                        &mut enc,
                        &schema_ref[i],
                        Some(&v),
                        &row_tz,
                        &row_ds,
                        row_cenc,
                    )?;
                    if let Some(row) = captured.as_mut() {
                        row.push(Some(v));
                    }
                    continue;
                }
                // A stored timestamp/timestamptz is reassembled from its
                // date plus the hidden `__us_` companion. Both are a UTC
                // instant; a `timestamp` renders naively, a `timestamptz`
                // renders in the SESSION zone. A special value (infinity)
                // is a String and falls to encode_field_value.
                // In BINARY format the text is not wanted: the instant is
                // reassembled as the composite carrier (`copy_reassemble`) and
                // encoded as an i64, the way a constant or a COPY row already
                // is. Emitting the text into a binary column made psycopg
                // read `2020-01-01 00:00:00` as an integer ("timestamp too
                // large").
                let reassembled = if schema_ref[i].format() == FieldFormat::Binary {
                    None
                } else if *schema_ref[i].datatype() == Type::TIMESTAMPTZ {
                    timestamptz_text(&d, f, &row_tz)
                } else {
                    timestamp_text(&d, f)
                };
                match reassembled {
                    Some(text) => {
                        enc.encode_field(&Some(text.as_str()))?;
                        // The reassembled text re-encodes as a String,
                        // which `encode_field_value` passes through for a
                        // timestamp column (text) unchanged.
                        if let Some(row) = captured.as_mut() {
                            row.push(Some(Bson::String(text)));
                        }
                    }
                    None => {
                        let cell = copy_reassemble(&d, f, schema_ref[i].datatype());
                        encode_field_value(
                            &mut enc,
                            &schema_ref[i],
                            cell.as_ref(),
                            &row_tz,
                            &row_ds,
                            row_cenc,
                        )?;
                        if let Some(row) = captured.as_mut() {
                            row.push(cell);
                        }
                    }
                }
            }
            if let (Some(cap), Some(row)) = (&capture, captured) {
                if let Some(buf) = cap.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    buf.push(row);
                }
            }
            Ok(enc.take_row())
        });
        Ok(QueryResponse::new(schema, rows))
    }

    /// Execute one planned statement against storage.
    /// Can this statement change the type catalog? Reads, row writes to user
    /// tables, cursor and session-state statements cannot; everything else
    /// (every DDL, type, function, schema and database statement, and the
    /// forms this list does not name) is taken to. Errs on `true`: an extra
    /// catalog re-read is cheap, a stale catalog is a wrong answer.
    fn may_change_catalog(stmt: &Statement) -> bool {
        !matches!(
            stmt,
            Statement::Select(_)
                | Statement::SelectConstant(_)
                | Statement::ValuesConstant(_)
                | Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_)
                | Statement::Aggregate(_)
                | Statement::CopyFrom(_)
                | Statement::CopyTo(_)
                | Statement::Show(_)
                | Statement::Set { .. }
                | Statement::Reset(_)
                | Statement::SetTransaction(_)
                | Statement::SetSessionCharacteristics(_)
                | Statement::Fetch { .. }
                | Statement::CloseCursor(_)
                | Statement::Deallocate(_)
                | Statement::DeallocateAll
                | Statement::Notify { .. }
                | Statement::Listen(_)
                | Statement::Unlisten(_)
        )
    }

    /// Run one planned statement, and declare the catalog changed afterwards
    /// when the statement is one that can change it. After, not before: a
    /// bump before would let the statement's own reads re-fill the cache
    /// with the rows it is about to change. On failure too -- an autocommit
    /// DDL that failed halfway is rolled back, and the cache may have read
    /// the half. Here rather than in `run` because a statement can run
    /// another: `CREATE TABLE AS` creates its table and then INSERTs into
    /// it, and that INSERT's lookup must not find the "no such table" the
    /// CTAS itself cached a moment earlier (see `CatalogCache`).
    fn execute(&self, stmt: Statement, max_rows: usize) -> PgWireResult<Vec<Response>> {
        let may_change_catalog = Self::may_change_catalog(&stmt);
        let out = self.execute_inner(stmt, max_rows);
        if may_change_catalog {
            bump_catalog_version();
        }
        out
    }

    fn execute_inner(&self, stmt: Statement, max_rows: usize) -> PgWireResult<Vec<Response>> {
        // A timestamptz renders in the SESSION zone; capture it once here and
        // thread it into the row encoder explicitly. A thread-local does not
        // work: pgwire may encode the DataRows lazily on another async worker
        // thread, where a thread-local set here would not be visible.
        let row_tz = self.session_timezone();
        // Date / timestamp / timestamptz text renders in the session DateStyle,
        // captured here for the same reason as the zone above.
        let row_ds = self.session_datestyle();
        // The client encoding is captured for the same reason as `row_tz`:
        // pgwire may encode the DataRows lazily on another worker thread, so it
        // is threaded into the row encoder explicitly rather than read from
        // session state at encode time. `ClientEncoding` is `Copy`.
        let row_cenc = self.client_encoding();
        match stmt {
            Statement::Transaction(_) => unreachable!("handled before execute"),
            // Handled in `run`, which can await the row stream.
            Statement::DeclareCursor { .. } => unreachable!("handled before execute"),
            Statement::Do { .. } => unreachable!("handled before execute"),
            Statement::CreateTable(mut def, if_not_exists) => {
                if self.lookup(&def.name).is_some() {
                    // `IF NOT EXISTS` is a NO-OP on an existing table, tag and
                    // all -- PostgreSQL only adds a notice. Raising here made
                    // the idiomatic "create it if it isn't there" fixture fail
                    // the second time a session ran it.
                    if if_not_exists {
                        return Ok(vec![Response::Execution(Tag::new("CREATE TABLE"))]);
                    }
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P07".into(), // duplicate_table
                        format!("relation \"{}\" already exists", def.name),
                    ))));
                }
                // The table's name is also its ROW TYPE's, so it must be free
                // as a type. Measured on 16: over a composite (which is a
                // relation too) it is 42P07; over an enum, a range or a
                // builtin it is 42710 with the hint.
                if self.composites()?.iter().any(|(n, _, _)| *n == def.name) {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P07".into(),
                        format!("relation \"{}\" already exists", def.name),
                    ))));
                }
                if self.enums()?.iter().any(|(n, _, _)| *n == def.name)
                    || self.ranges()?.iter().any(|(n, _, _)| *n == def.name)
                    || secantus_pgplan::pgtypes::oid_of_name(&def.name).is_some()
                {
                    let mut info = ErrorInfo::new(
                        "ERROR".into(),
                        "42710".into(),
                        format!("type \"{}\" already exists", def.name),
                    );
                    info.hint = Some(
                        "A relation has an associated type of the same name, so you must \
                         use a name that doesn't conflict with any existing type."
                            .into(),
                    );
                    return Err(PgWireError::UserError(Box::new(info)));
                }
                // A column may not be typed as a SHELL: 42704 `is only a
                // shell` (measured on 16), the same refusal a cast gets.
                for col in &def.columns {
                    let element = col.pg_type.strip_suffix("[]").unwrap_or(&col.pg_type);
                    if let Some(base) = self.base_type_named(element)? {
                        if !base.defined {
                            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "42704".into(), // undefined_object
                                format!("type \"{}\" is only a shell", base.name),
                            ))));
                        }
                    }
                }
                // A FOREIGN KEY to another table names its PRIMARY KEY (the
                // planner settled self-references, which need no lookup).
                for fk in &mut def.foreign_keys {
                    if fk.ref_table == def.name {
                        continue;
                    }
                    let target = self.lookup(&fk.ref_table).ok_or_else(|| {
                        Self::err(&PlanError::UndefinedTable(fk.ref_table.clone()))
                    })?;
                    secantus_pgplan::resolve_fk_target(fk, &target).map_err(|e| Self::err(&e))?;
                }
                self.storage
                    .create_collection(self.db(), &def.name)
                    .map_err(|e| Self::storage_err("could not create the table", e))?;
                let bytes = bson::to_vec(&def.to_document())
                    .map_err(|e| Self::storage_err("could not encode the catalog entry", e))?;
                self.storage
                    .insert(self.db(), CATALOG_COLLECTION, vec![bytes], true)
                    .map_err(|e| Self::storage_err("could not record the table", e))?;
                // Each serial column's sequence, owned by the column so the
                // table's DROP takes it along.
                let sequences = def
                    .columns
                    .iter()
                    .filter_map(|c| c.sequence.as_deref().map(|seq| (c, seq)))
                    .map(|(c, seq)| {
                        let max_value = match c.pg_type.as_str() {
                            "int2" => i64::from(i16::MAX),
                            "int8" => i64::MAX,
                            _ => i64::from(i32::MAX),
                        };
                        let owned_by = format!("{}.{}", def.name, c.name);
                        bson::to_vec(&secantus_pgcatalog::sequence_document(
                            seq, &owned_by, max_value,
                        ))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| Self::storage_err("could not encode a sequence", e))?;
                if !sequences.is_empty() {
                    self.ensure_collection(SEQUENCE_COLLECTION)?;
                    // A sequence left behind by a dropped table of the same
                    // name (a drop that did not know about sequences) would
                    // otherwise collide on `_id`.
                    for seq in def.columns.iter().filter_map(|c| c.sequence.as_deref()) {
                        self.storage
                            .delete_matching(
                                self.db(),
                                SEQUENCE_COLLECTION,
                                &bson::doc! { "_id": seq },
                                0,
                                &Document::new(),
                                None,
                            )
                            .map_err(|e| Self::storage_err("could not reset a sequence", e))?;
                    }
                    self.storage
                        .insert(self.db(), SEQUENCE_COLLECTION, sequences, true)
                        .map_err(|e| Self::storage_err("could not record a sequence", e))?;
                }
                // The table's ROW TYPE: a composite of its columns, in the
                // type catalog like any other so `'(foo)'::mytype`,
                // `mytype[]`, `pg_type` and `to_regtype` all find it. Marked
                // `relation` so DROP TYPE refuses it and DROP TABLE takes it.
                self.ensure_collection(Self::COMPOSITE_COLLECTION)?;
                self.ensure_collection(Self::ENUM_META_COLLECTION)?;
                let oid = self.mint_composite_oid()?;
                let field_docs: Vec<Bson> = def
                    .columns
                    .iter()
                    .map(|c| {
                        Bson::Array(vec![
                            Bson::String(c.name.clone()),
                            Bson::String(c.pg_type.clone()),
                            Bson::Null,
                        ])
                    })
                    .collect();
                let row_type = bson::doc! {
                    "_id": &def.name,
                    "composite": &def.name,
                    "schema": "public",
                    "fields": field_docs,
                    "oid": oid,
                    "relation": true,
                };
                let bytes = bson::to_vec(&row_type)
                    .map_err(|e| Self::storage_err("could not encode the row type", e))?;
                self.storage
                    .insert(self.db(), Self::COMPOSITE_COLLECTION, vec![bytes], true)
                    .map_err(|e| Self::storage_err("could not record the row type", e))?;
                self.note_uncommitted_type(Self::COMPOSITE_COLLECTION, &def.name, Some(row_type));
                // Remember it for the rest of this transaction: the catalog row
                // above is not committed yet, so a plain read cannot see it.
                self.note_uncommitted(&def.name, Some(def.clone()));
                Ok(vec![Response::Execution(Tag::new("CREATE TABLE"))])
            }

            // `CREATE TABLE t AS query`: the table takes the query's output
            // columns (renamed by an explicit column list), then the query's
            // rows are written as an `INSERT ... SELECT` would write them.
            // Measured on 16: the tag is `SELECT n` when rows are copied and
            // `CREATE TABLE AS` for `WITH NO DATA` or an `IF NOT EXISTS`
            // that found the table.
            Statement::CreateTableAs {
                table,
                if_not_exists,
                temp,
                column_names,
                query,
                with_data,
            } => {
                if self.lookup(&table).is_some() {
                    if if_not_exists {
                        return Ok(vec![Response::Execution(Tag::new("CREATE TABLE AS"))]);
                    }
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P07".into(), // duplicate_table
                        format!("relation \"{table}\" already exists"),
                    ))));
                }
                let fields = self.copy_query_fields(&query)?;
                if column_names.len() > fields.len() {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42601".into(), // syntax_error
                        "too many column names were specified".into(),
                    ))));
                }
                let mut seen: Vec<String> = Vec::new();
                let columns = fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let name = column_names
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| f.name().to_string());
                        if seen.contains(&name) {
                            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "42701".into(), // duplicate_column
                                format!("column \"{name}\" specified more than once"),
                            ))));
                        }
                        seen.push(name.clone());
                        let pg_type = internal_type_name(f.datatype()).ok_or_else(|| {
                            PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "0A000".into(), // feature_not_supported
                                format!(
                                    "CREATE TABLE AS over a column of type {} is not supported yet",
                                    f.datatype().name()
                                ),
                            )))
                        })?;
                        Ok(secantus_pgcatalog::Column::new(&name, &pg_type, false))
                    })
                    .collect::<PgWireResult<Vec<_>>>()?;
                let mut def = TableDef::new(&table, columns);
                def.temp = temp;
                let targets: Vec<String> = def.columns.iter().map(|c| c.name.clone()).collect();
                // The rows are read BEFORE the table exists so a query that
                // fails leaves nothing behind, as PostgreSQL's single
                // transaction would.
                let rows = if with_data {
                    self.query_rows(&query)?
                        .into_iter()
                        .map(|values| {
                            let values = values
                                .into_iter()
                                .map(|v| v.unwrap_or(Bson::Null))
                                .collect();
                            secantus_pgplan::insert_row(&def, &targets, false, values)
                                .map_err(|e| Self::err(&e))
                        })
                        .collect::<PgWireResult<Vec<_>>>()?
                } else {
                    Vec::new()
                };
                self.execute(Statement::CreateTable(def, false), max_rows)?;
                if !with_data {
                    return Ok(vec![Response::Execution(Tag::new("CREATE TABLE AS"))]);
                }
                let written = rows.len();
                if written > 0 {
                    self.execute(
                        Statement::Insert(secantus_pgplan::Insert {
                            table: table.clone(),
                            rows,
                            returning: None,
                            source: None,
                            targets,
                            explicit_columns: false,
                        }),
                        max_rows,
                    )?;
                }
                Ok(vec![Response::Execution(
                    Tag::new("SELECT").with_rows(written),
                )])
            }

            Statement::Insert(mut ins) => {
                let def = self
                    .lookup(&ins.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(ins.table.clone())))?;
                // `INSERT ... SELECT`: read the query's rows first, then write
                // them exactly as a VALUES list would be written.
                if let Some(source) = ins.source.take() {
                    for values in self.query_rows(&source)? {
                        let values = values
                            .into_iter()
                            .map(|v| v.unwrap_or(Bson::Null))
                            .collect();
                        let row = secantus_pgplan::insert_row(
                            &def,
                            &ins.targets,
                            ins.explicit_columns,
                            values,
                        )
                        .map_err(|e| Self::err(&e))?;
                        ins.rows.push(row);
                    }
                }
                self.apply_serial_defaults(&def, &mut ins.rows)?;
                apply_column_defaults(&def, &mut ins.rows);
                // Every constraint is checked BEFORE the first write, so a
                // violation on any row leaves none of them inserted.
                for row in &ins.rows {
                    self.check_row_constraints(&def, row)?;
                }
                if let Some(dup) = self.wide_numeric_pk_conflict(&ins.table, &def, &ins.rows)? {
                    return Err(Self::write_error(
                        &ins.table,
                        &def,
                        &bson::doc! { "code": 11000, "keyValue": { "_id": dup } },
                    ));
                }
                self.check_foreign_keys(&def, &ins.rows)?;
                let n = ins.rows.len();
                let docs = ins
                    .rows
                    .iter()
                    .map(bson::to_vec)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| Self::storage_err("could not encode a row", e))?;
                let (written, errors) = self
                    .storage
                    .insert(self.db(), &ins.table, docs, true)
                    .map_err(|e| Self::storage_err("could not insert", e))?;
                if let Some(first) = errors.first() {
                    return Err(Self::write_error(&ins.table, &def, first));
                }
                debug_assert_eq!(written, n);
                match ins.returning {
                    None => Ok(vec![Response::Execution(
                        Tag::new("INSERT").with_oid(0).with_rows(written),
                    )]),
                    // `RETURNING` projects the rows as written -- serial
                    // defaults included -- and tags the response `INSERT 0 n`
                    // with the row count pgwire appends.
                    Some(returning) => {
                        let mut response = self.project_rows(
                            ins.rows,
                            &def,
                            &returning.columns,
                            &returning.casts,
                            &RowEnv {
                                tz: row_tz,
                                ds: row_ds,
                                cenc: row_cenc,
                            },
                        )?;
                        response.set_command_tag("INSERT 0");
                        Ok(vec![Response::Query(response)])
                    }
                }
            }

            Statement::Select(sel) => {
                let (docs, def) = self.select_docs(&sel, max_rows)?;
                Ok(vec![Response::Query(self.project_rows(
                    docs,
                    &def,
                    &sel.columns,
                    &sel.casts,
                    &RowEnv {
                        tz: row_tz,
                        ds: row_ds,
                        cenc: row_cenc,
                    },
                )?)])
            }

            Statement::CreateDatabase { name } => {
                self.refuse_in_transaction_block("CREATE DATABASE")?;
                if self.databases.lookup(&self.storage, &name)?.is_some() {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P04".into(), // duplicate_database
                        format!("database \"{name}\" already exists"),
                    ))));
                }
                self.databases.create(&self.storage, &name)?;
                Ok(vec![Response::Execution(Tag::new("CREATE DATABASE"))])
            }

            Statement::DropDatabase { name, if_exists } => {
                self.refuse_in_transaction_block("DROP DATABASE")?;
                let Some(info) = self.databases.lookup(&self.storage, &name)? else {
                    if if_exists {
                        self.pending_notices
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(ErrorInfo::new(
                                "NOTICE".into(),
                                "00000".into(),
                                format!("database \"{name}\" does not exist, skipping"),
                            ));
                        return Ok(vec![Response::Execution(Tag::new("DROP DATABASE"))]);
                    }
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "3D000".into(), // invalid_catalog_name
                        format!("database \"{name}\" does not exist"),
                    ))));
                };
                if info.is_template {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42809".into(), // wrong_object_type
                        "cannot drop a template database".into(),
                    ))));
                }
                if info.name == self.db() {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "55006".into(), // object_in_use
                        "cannot drop the currently open database".into(),
                    ))));
                }
                self.databases.remove(&self.storage, &info.name)?;
                Ok(vec![Response::Execution(Tag::new("DROP DATABASE"))])
            }

            Statement::CreateSchema {
                name,
                if_not_exists,
            } => {
                self.ensure_collection(Self::SCHEMA_COLLECTION)?;
                let exists = !self
                    .storage
                    .find_matching(
                        self.db(),
                        Self::SCHEMA_COLLECTION,
                        &bson::doc! {"_id": &name},
                    )
                    .map_err(|e| Self::storage_err("could not read schemas", e))?
                    .is_empty();
                if exists {
                    // `IF NOT EXISTS` is a no-op; a bare CREATE is 42P06.
                    if if_not_exists {
                        return Ok(vec![Response::Execution(Tag::new("CREATE SCHEMA"))]);
                    }
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P06".into(), // duplicate_schema
                        format!("schema \"{name}\" already exists"),
                    ))));
                }
                let doc = bson::doc! {"_id": &name, "schema": &name};
                let bytes = bson::to_vec(&doc)
                    .map_err(|e| Self::storage_err("could not encode the schema", e))?;
                self.storage
                    .insert(self.db(), Self::SCHEMA_COLLECTION, vec![bytes], true)
                    .map_err(|e| Self::storage_err("could not record the schema", e))?;
                Ok(vec![Response::Execution(Tag::new("CREATE SCHEMA"))])
            }

            Statement::DropSchema {
                names,
                if_exists,
                cascade: _,
            } => {
                // CASCADE would drop the schema's contents; this server does
                // not track which tables/types belong to a schema (names carry
                // no schema), so it accepts the keyword and drops only the
                // schema record. The test corpus creates a schema, uses it,
                // and drops it CASCADE at teardown -- the objects are dropped
                // by name elsewhere, so nothing is orphaned in practice.
                self.ensure_collection(Self::SCHEMA_COLLECTION)?;
                for name in &names {
                    let removed = self
                        .storage
                        .delete_matching(
                            self.db(),
                            Self::SCHEMA_COLLECTION,
                            &bson::doc! {"_id": name},
                            0,
                            &Document::new(),
                            None,
                        )
                        .map_err(|e| Self::storage_err("could not drop the schema", e))?;
                    if removed == 0 && !if_exists {
                        return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                            "ERROR".into(),
                            "3F000".into(), // invalid_schema_name
                            format!("schema \"{name}\" does not exist"),
                        ))));
                    }
                }
                Ok(vec![Response::Execution(Tag::new("DROP SCHEMA"))])
            }

            Statement::CreateComposite {
                name,
                schema,
                fields,
            } => {
                // A schema-qualified name (`CREATE TYPE s.t`) is a distinct type
                // from a bare `t`; unqualified lands in `public`. A duplicate is
                // 42710, checked per (schema, name): a composite collides only
                // with another composite in the same schema, and an unqualified
                // name additionally collides with an enum or builtin (those live
                // in the default search_path).
                let schema_name = schema.clone().unwrap_or_else(|| "public".to_string());
                let composite_dup = self
                    .composites_with_schema()?
                    .iter()
                    .any(|(s, n, _, _)| *s == schema_name && *n == name);
                let unqualified = schema.is_none();
                let taken = composite_dup
                    || (unqualified
                        && (self.enums()?.iter().any(|(n, _, _)| *n == name)
                            || secantus_pgplan::pgtypes::oid_of_name(&name).is_some()));
                if taken {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42710".into(),
                        format!("type \"{name}\" already exists"),
                    ))));
                }
                self.ensure_collection(Self::COMPOSITE_COLLECTION)?;
                self.ensure_collection(Self::ENUM_META_COLLECTION)?;
                let oid = self.mint_composite_oid()?;
                // Fields as the Python server writes them: [name, tag, null].
                let field_docs: Vec<Bson> = fields
                    .iter()
                    .map(|(n, t)| {
                        Bson::Array(vec![
                            Bson::String(n.clone()),
                            Bson::String(t.clone()),
                            Bson::Null,
                        ])
                    })
                    .collect();
                // `_id` is keyed on (schema, name) so a bare `t` and a
                // `schema.t` do not collide; `composite` stays the BARE name
                // (pg_type.typname is unqualified, as in PostgreSQL).
                let id_key = if unqualified {
                    name.clone()
                } else {
                    format!("{schema_name}.{name}")
                };
                let doc = bson::doc! {
                    "_id": &id_key,
                    "composite": &name,
                    "schema": &schema_name,
                    "fields": field_docs,
                    "oid": oid,
                };
                let bytes = bson::to_vec(&doc)
                    .map_err(|e| Self::storage_err("could not encode the type", e))?;
                self.storage
                    .insert(self.db(), Self::COMPOSITE_COLLECTION, vec![bytes], true)
                    .map_err(|e| Self::storage_err("could not record the type", e))?;
                self.note_uncommitted_type(Self::COMPOSITE_COLLECTION, &id_key, Some(doc));
                Ok(vec![Response::Execution(Tag::new("CREATE TYPE"))])
            }

            Statement::CreateRange {
                name,
                schema,
                subtype,
            } => {
                // A schema-qualified name (`CREATE TYPE s.t AS RANGE`) is a
                // distinct type from a bare `t`; unqualified lands in `public`.
                // A duplicate is 42710, checked per (schema, name): a range
                // collides with another range in the same schema, and an
                // unqualified name additionally collides with a composite, enum
                // or builtin (those live in the default search_path).
                let schema_name = schema.clone().unwrap_or_else(|| "public".to_string());
                let unqualified = schema.is_none();
                let range_dup = self
                    .ranges_with_schema()?
                    .iter()
                    .any(|(s, n, _, _)| *s == schema_name && *n == name);
                let taken = range_dup
                    || (unqualified
                        && (self.composites()?.iter().any(|(n2, _, _)| *n2 == name)
                            || self.enums()?.iter().any(|(n2, _, _)| *n2 == name)
                            || secantus_pgplan::pgtypes::oid_of_name(&name).is_some()));
                if taken {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42710".into(),
                        format!("type \"{name}\" already exists"),
                    ))));
                }
                self.ensure_collection(Self::RANGE_COLLECTION)?;
                let oid = self.mint_range_oid()?;
                // `_id` is keyed on (schema, name) so a bare `t` and a `schema.t`
                // do not collide; `range` stays the BARE name (pg_type.typname is
                // unqualified, as in PostgreSQL).
                let id_key = Self::type_resolution(&schema_name, &name);
                let doc = bson::doc! {
                    "_id": &id_key,
                    "range": &name,
                    "schema": &schema_name,
                    "subtype": &subtype,
                    "oid": oid,
                };
                let bytes = bson::to_vec(&doc)
                    .map_err(|e| Self::storage_err("could not encode the type", e))?;
                self.storage
                    .insert(self.db(), Self::RANGE_COLLECTION, vec![bytes], true)
                    .map_err(|e| Self::storage_err("could not record the type", e))?;
                self.note_uncommitted_type(Self::RANGE_COLLECTION, &id_key, Some(doc));
                Ok(vec![Response::Execution(Tag::new("CREATE TYPE"))])
            }

            Statement::CreateShellType { name, schema } => {
                // `CREATE TYPE "a-b";` -- a shell: a pg_type row with
                // typisdefined false, typtype `p`, typarray 0 (measured on
                // 16). It exists so `CREATE FUNCTION ... RETURNS "a-b"` can
                // name it; a value cannot be cast to it until the full
                // CREATE TYPE completes it.
                let schema_name = schema.clone().unwrap_or_else(|| "public".to_string());
                let id_key = Self::type_resolution(&schema_name, &name);
                let taken = self.base_type_named(&id_key)?.is_some()
                    || (schema.is_none() && self.type_name_taken(&name)?);
                if taken {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42710".into(), // duplicate_object
                        format!("type \"{name}\" already exists"),
                    ))));
                }
                self.ensure_collection(Self::BASE_TYPE_COLLECTION)?;
                let oid = self.mint_base_type_oid()?;
                let doc = bson::doc! {
                    "_id": &id_key,
                    "base": &name,
                    "schema": &schema_name,
                    "oid": oid,
                    "defined": false,
                };
                self.insert_type_doc(Self::BASE_TYPE_COLLECTION, &id_key, doc)?;
                Ok(vec![Response::Execution(Tag::new("CREATE TYPE"))])
            }

            Statement::CreateBaseType {
                name,
                schema,
                input,
                output,
            } => {
                // The full form completes a shell. Every refusal below is
                // PostgreSQL 16's, in its order: the shell must exist
                // (42710 + hint), both I/O functions must be named (42P17),
                // each must exist with the exact signature (42883), and each
                // must return the right type (42P17).
                let schema_name = schema.clone().unwrap_or_else(|| "public".to_string());
                let id_key = Self::type_resolution(&schema_name, &name);
                let quoted = secantus_pgplan::scalar::quote_identifier(&name);
                let shell = match self.base_type_named(&id_key)? {
                    Some(b) if b.defined => {
                        return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                            "ERROR".into(),
                            "42710".into(),
                            format!("type \"{name}\" already exists"),
                        ))));
                    }
                    Some(b) => b,
                    None => {
                        if schema.is_none() && self.type_name_taken(&name)? {
                            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "42710".into(),
                                format!("type \"{name}\" already exists"),
                            ))));
                        }
                        // 42710 (not 42704) with a hint: measured on 16.
                        let mut info = ErrorInfo::new(
                            "ERROR".into(),
                            "42710".into(),
                            format!("type \"{name}\" does not exist"),
                        );
                        info.hint = Some(
                            "Create the type as a shell type, then create its I/O \
                             functions, then do a full CREATE TYPE."
                                .to_string(),
                        );
                        return Err(PgWireError::UserError(Box::new(info)));
                    }
                };
                let Some(input) = input else {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P17".into(), // invalid_object_definition
                        "type input function must be specified".into(),
                    ))));
                };
                let Some(output) = output else {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P17".into(),
                        "type output function must be specified".into(),
                    ))));
                };
                let functions = self.functions()?;
                let Some(input_fn) = functions
                    .iter()
                    .find(|f| f.name == input && f.param_types == ["cstring"])
                else {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42883".into(), // undefined_function
                        format!("function {input}(cstring) does not exist"),
                    ))));
                };
                if input_fn.return_type != id_key {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P17".into(),
                        format!("type input function {input} must return type {quoted}"),
                    ))));
                }
                let Some(output_fn) = functions
                    .iter()
                    .find(|f| f.name == output && f.param_types == [id_key.clone()])
                else {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42883".into(),
                        format!("function {output}({quoted}) does not exist"),
                    ))));
                };
                if output_fn.return_type != "cstring" {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P17".into(),
                        format!("type output function {output} must return type cstring"),
                    ))));
                }
                let doc = bson::doc! {
                    "_id": &id_key,
                    "base": &name,
                    "schema": &schema_name,
                    "oid": shell.oid,
                    "defined": true,
                    "input": &input,
                    "output": &output,
                };
                self.delete_type_doc(Self::BASE_TYPE_COLLECTION, &id_key)?;
                self.insert_type_doc(Self::BASE_TYPE_COLLECTION, &id_key, doc)?;
                Ok(vec![Response::Execution(Tag::new("CREATE TYPE"))])
            }

            Statement::CreateFunction {
                name,
                replace,
                arg_types,
                return_type,
                body,
                volatility: _,
            } => {
                // A `LANGUAGE internal` wrapper over a built-in: a catalog
                // row only, which is all a base type's `input = ` / `output =`
                // options resolve against. Nothing here ever CALLS it -- a
                // real PostgreSQL 16 crashed its backend when a wrapper
                // declared over the wrong C signature was called, so
                // callability is not something to imitate.
                if !Self::is_builtin_function(&body) {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42883".into(), // undefined_function
                        format!("there is no built-in function named \"{body}\""),
                    ))));
                }
                // The return type: an unknown name becomes a new shell with
                // a notice (how PostgreSQL lets `RETURNS newtype` precede
                // `CREATE TYPE newtype`); a shell is accepted with a notice.
                // Both notices name the type UNQUOTED (TypeNameToString on
                // 16), unlike the 42704 message which quotes it.
                let return_key = self.function_type_key(&return_type)?;
                match self.type_kind(&return_key)? {
                    TypeKind::Unknown => {
                        self.ensure_collection(Self::BASE_TYPE_COLLECTION)?;
                        let oid = self.mint_base_type_oid()?;
                        let (schema_name, bare) = match return_type.split_once('.') {
                            Some((s, n)) => (s.to_string(), n.to_string()),
                            None => ("public".to_string(), return_type.clone()),
                        };
                        let doc = bson::doc! {
                            "_id": &return_key,
                            "base": &bare,
                            "schema": &schema_name,
                            "oid": oid,
                            "defined": false,
                        };
                        self.insert_type_doc(Self::BASE_TYPE_COLLECTION, &return_key, doc)?;
                        self.notice(
                            "42704",
                            format!("type \"{return_type}\" is not yet defined"),
                            Some("Creating a shell type definition.".to_string()),
                        );
                    }
                    TypeKind::Shell => self.notice(
                        "42809",
                        format!("return type {return_type} is only a shell"),
                        None,
                    ),
                    TypeKind::Defined => {}
                }
                let mut param_types = Vec::with_capacity(arg_types.len());
                for t in &arg_types {
                    let key = self.function_type_key(t)?;
                    match self.type_kind(&key)? {
                        TypeKind::Unknown => {
                            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "42704".into(), // undefined_object
                                format!("type {t} does not exist"),
                            ))));
                        }
                        TypeKind::Shell => {
                            self.notice("42809", format!("argument type {t} is only a shell"), None)
                        }
                        TypeKind::Defined => {}
                    }
                    param_types.push(key);
                }
                let id_key = format!("{name}/{}", param_types.len());
                let functions = self.functions()?;
                if let Some(existing) = functions.iter().find(|f| f.name == name) {
                    if existing.param_types == param_types {
                        if replace {
                            self.delete_type_doc(Self::FUNCTION_COLLECTION, &id_key)?;
                        } else {
                            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "42723".into(), // duplicate_function
                                format!(
                                    "function \"{name}\" already exists with same argument types"
                                ),
                            ))));
                        }
                    } else if existing.param_types.len() == param_types.len() {
                        // The shared catalog keys a function on name/arity
                        // (the Python server's shape), so two overloads at one
                        // arity cannot both be recorded.
                        return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                            "ERROR".into(),
                            "0A000".into(), // feature_not_supported
                            format!(
                                "overloading function \"{name}\" at the same number of \
                                 arguments is not supported yet"
                            ),
                        ))));
                    }
                }
                self.ensure_collection(Self::FUNCTION_COLLECTION)?;
                let nargs = param_types.len() as i64;
                let doc = bson::doc! {
                    "_id": &id_key,
                    "name": &name,
                    "nargs": nargs,
                    "params": vec![Bson::Null; param_types.len()],
                    "param_types": &param_types,
                    "return_tag": &return_key,
                    "is_table": false,
                    "body": &body,
                    "language": "internal",
                    "returns_trigger": false,
                };
                self.insert_type_doc(Self::FUNCTION_COLLECTION, &id_key, doc)?;
                Ok(vec![Response::Execution(Tag::new("CREATE FUNCTION"))])
            }

            Statement::DropFunction {
                name,
                arg_types,
                if_exists,
                cascade,
            } => {
                let tag = || Ok(vec![Response::Execution(Tag::new("DROP FUNCTION"))]);
                // Every named argument type must resolve first (measured on
                // 16: `drop function invout("a-b")` after the type is gone is
                // 42704 on the TYPE, not 42883 on the function).
                let mut wanted = None;
                if let Some(types) = &arg_types {
                    let mut keys = Vec::with_capacity(types.len());
                    for t in types {
                        let key = self.function_type_key(t)?;
                        if self.type_kind(&key)? == TypeKind::Unknown {
                            if if_exists {
                                self.notice(
                                    "00000",
                                    format!("type \"{t}\" does not exist, skipping"),
                                    None,
                                );
                                return tag();
                            }
                            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "42704".into(),
                                format!("type \"{t}\" does not exist"),
                            ))));
                        }
                        keys.push(key);
                    }
                    wanted = Some(keys);
                }
                let functions = self.functions()?;
                let candidates: Vec<&UserFunction> =
                    functions.iter().filter(|f| f.name == name).collect();
                let target = match &wanted {
                    Some(keys) => {
                        let Some(f) = candidates.iter().find(|f| f.param_types == *keys) else {
                            let probe = UserFunction {
                                name: name.clone(),
                                param_types: keys.clone(),
                                return_type: String::new(),
                                language: String::new(),
                            };
                            let sig = self.function_signature(&probe);
                            if if_exists {
                                self.notice(
                                    "00000",
                                    format!("function {sig} does not exist, skipping"),
                                    None,
                                );
                                return tag();
                            }
                            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "42883".into(),
                                format!("function {sig} does not exist"),
                            ))));
                        };
                        (*f).clone()
                    }
                    None => match candidates.as_slice() {
                        [] => {
                            if if_exists {
                                self.notice(
                                    "00000",
                                    format!("function {name}() does not exist, skipping"),
                                    None,
                                );
                                return tag();
                            }
                            return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                                "ERROR".into(),
                                "42883".into(),
                                format!("could not find a function named \"{name}\""),
                            ))));
                        }
                        [one] => (*one).clone(),
                        _ => {
                            let mut info = ErrorInfo::new(
                                "ERROR".into(),
                                "42725".into(), // ambiguous_function
                                format!("function name \"{name}\" is not unique"),
                            );
                            info.hint = Some(
                                "Specify the argument list to select the function \
                                 unambiguously."
                                    .to_string(),
                            );
                            return Err(PgWireError::UserError(Box::new(info)));
                        }
                    },
                };
                let sig = self.function_signature(&target);
                // A DEFINED base type depends on its I/O functions; and the
                // type's other I/O function depends on the type, so it goes
                // too. (A shell has no such dependency: dropping the function
                // that returns it is plain.)
                let dependents: Vec<BaseType> = self
                    .base_types()?
                    .into_iter()
                    .filter(|b| {
                        b.defined
                            && (b.input.as_deref() == Some(&name)
                                || b.output.as_deref() == Some(&name))
                    })
                    .collect();
                let mut cascade_descs: Vec<String> = Vec::new();
                let mut cascade_functions: Vec<String> = Vec::new();
                for b in &dependents {
                    let key = Self::type_resolution(&b.schema, &b.name);
                    let quoted = secantus_pgplan::scalar::quote_identifier(&b.name);
                    cascade_descs.push(format!("type {quoted}"));
                    for f in functions.iter().filter(|f| {
                        f.name != name && (f.param_types.contains(&key) || f.return_type == key)
                    }) {
                        cascade_descs.push(format!("function {}", self.function_signature(f)));
                        cascade_functions.push(format!("{}/{}", f.name, f.param_types.len()));
                    }
                }
                if !dependents.is_empty() && !cascade {
                    let mut lines = Vec::new();
                    for b in &dependents {
                        let key = Self::type_resolution(&b.schema, &b.name);
                        let quoted = secantus_pgplan::scalar::quote_identifier(&b.name);
                        lines.push(format!("type {quoted} depends on function {sig}"));
                        for f in functions.iter().filter(|f| {
                            f.name != name && (f.param_types.contains(&key) || f.return_type == key)
                        }) {
                            lines.push(format!(
                                "function {} depends on type {quoted}",
                                self.function_signature(f)
                            ));
                        }
                    }
                    let mut info = ErrorInfo::new(
                        "ERROR".into(),
                        "2BP01".into(), // dependent_objects_still_exist
                        format!("cannot drop function {sig} because other objects depend on it"),
                    );
                    info.detail = Some(lines.join("\n"));
                    info.hint =
                        Some("Use DROP ... CASCADE to drop the dependent objects too.".to_string());
                    return Err(PgWireError::UserError(Box::new(info)));
                }
                if !cascade_descs.is_empty() {
                    self.cascade_notice(&cascade_descs);
                    for b in &dependents {
                        let key = Self::type_resolution(&b.schema, &b.name);
                        self.delete_type_doc(Self::BASE_TYPE_COLLECTION, &key)?;
                    }
                    for id in &cascade_functions {
                        self.delete_type_doc(Self::FUNCTION_COLLECTION, id)?;
                    }
                }
                let id_key = format!("{}/{}", target.name, target.param_types.len());
                self.delete_type_doc(Self::FUNCTION_COLLECTION, &id_key)?;
                tag()
            }

            Statement::CreateEnum {
                name,
                schema,
                labels,
            } => {
                // A schema-qualified name (`CREATE TYPE s.t AS ENUM`) is a
                // distinct type from a bare `t`; unqualified lands in `public`.
                // A duplicate is 42710, distinct from a table's 42P07, checked
                // per (schema, name): an enum collides with another enum in the
                // same schema, and an unqualified name additionally collides with
                // a builtin (`create type text as enum ...` is the same refusal).
                let schema_name = schema.clone().unwrap_or_else(|| "public".to_string());
                let unqualified = schema.is_none();
                let enum_dup = self
                    .enums_with_schema()?
                    .iter()
                    .any(|(s, n, _, _)| *s == schema_name && *n == name);
                let exists = enum_dup
                    || (unqualified
                        && (secantus_pgplan::pgtypes::oid_of_name(&name).is_some()
                            || self.composites()?.iter().any(|(n, _, _)| *n == name)
                            || self.ranges()?.iter().any(|(n, _, _)| *n == name)));
                if exists {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42710".into(), // duplicate_object
                        format!("type \"{name}\" already exists"),
                    ))));
                }
                self.ensure_collection(Self::ENUM_COLLECTION)?;
                self.ensure_collection(Self::ENUM_META_COLLECTION)?;
                let oid = self.mint_enum_oid()?;
                let id_key = Self::type_resolution(&schema_name, &name);
                let doc = bson::doc! {
                    "_id": &id_key,
                    "enum": &name,
                    "schema": &schema_name,
                    "labels": labels,
                    "oid": oid,
                };
                let bytes = bson::to_vec(&doc)
                    .map_err(|e| Self::storage_err("could not encode the type", e))?;
                self.storage
                    .insert(self.db(), Self::ENUM_COLLECTION, vec![bytes], true)
                    .map_err(|e| Self::storage_err("could not record the type", e))?;
                self.note_uncommitted_type(Self::ENUM_COLLECTION, &id_key, Some(doc));
                Ok(vec![Response::Execution(Tag::new("CREATE TYPE"))])
            }

            Statement::DropType {
                names,
                if_exists,
                cascade,
            } => {
                self.ensure_collection(Self::ENUM_COLLECTION)?;
                self.ensure_collection(Self::COMPOSITE_COLLECTION)?;
                self.ensure_collection(Self::RANGE_COLLECTION)?;
                for name in &names {
                    // A base type (shell or defined): its I/O functions depend
                    // on it, so RESTRICT refuses with 2BP01 while any exist
                    // and CASCADE drops them with a notice (measured on 16).
                    if let Some(base) = self.base_type_named(name)? {
                        let quoted = secantus_pgplan::scalar::quote_identifier(&base.name);
                        let functions = self.functions()?;
                        let dependents: Vec<&UserFunction> = functions
                            .iter()
                            .filter(|f| {
                                f.param_types.iter().any(|t| t == name) || f.return_type == *name
                            })
                            .collect();
                        if !dependents.is_empty() && !cascade {
                            let lines: Vec<String> = dependents
                                .iter()
                                .map(|f| {
                                    format!(
                                        "function {} depends on type {quoted}",
                                        self.function_signature(f)
                                    )
                                })
                                .collect();
                            let mut info = ErrorInfo::new(
                                "ERROR".into(),
                                "2BP01".into(), // dependent_objects_still_exist
                                format!(
                                    "cannot drop type {quoted} because other objects depend on it"
                                ),
                            );
                            info.detail = Some(lines.join("\n"));
                            info.hint = Some(
                                "Use DROP ... CASCADE to drop the dependent objects too."
                                    .to_string(),
                            );
                            return Err(PgWireError::UserError(Box::new(info)));
                        }
                        if !dependents.is_empty() {
                            let descs: Vec<String> = dependents
                                .iter()
                                .map(|f| format!("function {}", self.function_signature(f)))
                                .collect();
                            self.cascade_notice(&descs);
                            for f in &dependents {
                                let id = format!("{}/{}", f.name, f.param_types.len());
                                self.delete_type_doc(Self::FUNCTION_COLLECTION, &id)?;
                            }
                        }
                        self.delete_type_doc(Self::BASE_TYPE_COLLECTION, name)?;
                        continue;
                    }
                    // A table's row type goes with the table, not on its own
                    // (measured on 16).
                    if self.row_type_of_table(name)? {
                        let mut info = ErrorInfo::new(
                            "ERROR".into(),
                            "2BP01".into(), // dependent_objects_still_exist
                            format!("cannot drop type {name} because table {name} requires it"),
                        );
                        info.hint = Some(format!("You can drop table {name} instead."));
                        return Err(PgWireError::UserError(Box::new(info)));
                    }
                    let filter = bson::doc! {"_id": name};
                    let from_enum = self
                        .storage
                        .delete_matching(
                            self.db(),
                            Self::ENUM_COLLECTION,
                            &filter,
                            0,
                            &Document::new(),
                            None,
                        )
                        .map_err(|e| Self::storage_err("could not drop the type", e))?;
                    let from_comp = self
                        .storage
                        .delete_matching(
                            self.db(),
                            Self::COMPOSITE_COLLECTION,
                            &filter,
                            0,
                            &Document::new(),
                            None,
                        )
                        .map_err(|e| Self::storage_err("could not drop the type", e))?;
                    let from_range = self
                        .storage
                        .delete_matching(
                            self.db(),
                            Self::RANGE_COLLECTION,
                            &filter,
                            0,
                            &Document::new(),
                            None,
                        )
                        .map_err(|e| Self::storage_err("could not drop the type", e))?;
                    // Hide the dropped type from later statements in this same
                    // transaction: the deletes above landed in the transaction
                    // session, but planning reads the catalog OUTSIDE it and
                    // would still find a committed row. A tombstone per
                    // collection that had a row removed does for DROP what the
                    // create notes do for CREATE.
                    if from_enum > 0 {
                        self.note_uncommitted_type(Self::ENUM_COLLECTION, name, None);
                    }
                    if from_comp > 0 {
                        self.note_uncommitted_type(Self::COMPOSITE_COLLECTION, name, None);
                    }
                    if from_range > 0 {
                        self.note_uncommitted_type(Self::RANGE_COLLECTION, name, None);
                    }
                    let removed = from_enum + from_comp + from_range;
                    if removed == 0 {
                        if if_exists {
                            self.notice(
                                "00000",
                                format!("type \"{name}\" does not exist, skipping"),
                                None,
                            );
                            continue;
                        }
                        return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                            "ERROR".into(),
                            "42704".into(), // undefined_object
                            format!("type \"{name}\" does not exist"),
                        ))));
                    }
                }
                Ok(vec![Response::Execution(Tag::new("DROP TYPE"))])
            }

            Statement::DropTable(drop) => {
                for table in &drop.tables {
                    let Some(def) = self.lookup(table) else {
                        if drop.if_exists {
                            continue;
                        }
                        return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                            "ERROR".into(),
                            "42P01".into(), // undefined_table
                            format!("table \"{table}\" does not exist"),
                        ))));
                    };
                    // The sequences its serial columns own go with it.
                    if def.columns.iter().any(|c| c.sequence.is_some()) {
                        self.ensure_collection(SEQUENCE_COLLECTION)?;
                    }
                    for seq in def.columns.iter().filter_map(|c| c.sequence.as_deref()) {
                        self.storage
                            .delete_matching(
                                self.db(),
                                SEQUENCE_COLLECTION,
                                &bson::doc! { "_id": seq },
                                0,
                                &Document::new(),
                                None,
                            )
                            .map_err(|e| Self::storage_err("could not drop a sequence", e))?;
                    }
                    // Both halves, and the CATALOG entry last: if the drop
                    // fails midway, a table whose catalog row survived is
                    // recoverable, whereas a catalog row pointing at a
                    // collection that no longer exists is not.
                    self.storage
                        .drop_collection(self.db(), table)
                        .map_err(|e| Self::storage_err("could not drop the table", e))?;
                    self.storage
                        .delete_matching(
                            self.db(),
                            CATALOG_COLLECTION,
                            &bson::doc! { "_id": table },
                            0,
                            &Document::new(),
                            None,
                        )
                        .map_err(|e| Self::storage_err("could not drop the catalog entry", e))?;
                    // And its ROW TYPE (a table created before row types were
                    // recorded has none to drop).
                    self.ensure_collection(Self::COMPOSITE_COLLECTION)?;
                    let dropped = self
                        .storage
                        .delete_matching(
                            self.db(),
                            Self::COMPOSITE_COLLECTION,
                            &bson::doc! { "_id": table, "relation": true },
                            0,
                            &Document::new(),
                            None,
                        )
                        .map_err(|e| Self::storage_err("could not drop the row type", e))?;
                    if dropped > 0 {
                        self.note_uncommitted_type(Self::COMPOSITE_COLLECTION, table, None);
                    }
                    // A tombstone: the catalog row is deleted but not
                    // committed, so a plain read would still find it.
                    self.note_uncommitted(table, None);
                }
                Ok(vec![Response::Execution(Tag::new("DROP TABLE"))])
            }

            Statement::CopyFrom(cf) => {
                let def = self
                    .lookup(&cf.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(cf.table.clone())))?;
                let cols: Vec<&secantus_pgcatalog::Column> = if cf.columns.is_empty() {
                    def.columns.iter().collect()
                } else {
                    cf.columns
                        .iter()
                        .map(|n| def.column(n).expect("planner checked"))
                        .collect()
                };
                let n = cols.len();
                *self.copy_in.lock().unwrap_or_else(|e| e.into_inner()) = Some(CopyInState {
                    format: cf.format,
                    table: cf.table.clone(),
                    fields: cols.iter().map(|c| c.field()).collect(),
                    types: cols.iter().map(|c| c.pg_type.clone()).collect(),
                    buffer: Vec::new(),
                });
                // The format code must match what the client will send: 1 for
                // binary, 0 for the textual formats.
                let code = if cf.format == secantus_pgplan::CopyFormat::Binary {
                    1
                } else {
                    0
                };
                Ok(vec![Response::CopyIn(CopyResponse::new(
                    code,
                    n,
                    futures::stream::empty(),
                ))])
            }

            Statement::CopyTo(ct) => {
                use secantus_pgplan::CopyFormat;
                // A query source reuses the ordinary SELECT path: run it, take
                // its schema and rows, and encode those. Rebuilding the read
                // here would be a second implementation of SELECT that could
                // disagree with the first.
                let (schema, rows): (Arc<Vec<FieldInfo>>, Vec<Vec<Option<Bson>>>) =
                    match ct.query.as_deref() {
                        Some(inner) => {
                            let fields = self.copy_query_fields(inner)?;
                            let values = self.query_rows(inner)?;
                            (Arc::new(fields), values)
                        }
                        None => {
                            let def = self.lookup(&ct.table).ok_or_else(|| {
                                Self::err(&PlanError::UndefinedTable(ct.table.clone()))
                            })?;
                            let cols: Vec<&secantus_pgcatalog::Column> = if ct.columns.is_empty() {
                                def.columns.iter().collect()
                            } else {
                                ct.columns
                                    .iter()
                                    .map(|n| def.column(n).expect("planner checked"))
                                    .collect()
                            };
                            let schema = Arc::new(
                                cols.iter()
                                    .map(|c| {
                                        FieldInfo::new(
                                            c.name.clone(),
                                            None,
                                            None,
                                            self.user_wire_type(&c.pg_type)
                                                .unwrap_or_else(|| wire_type(&c.pg_type)),
                                            FieldFormat::Text,
                                        )
                                    })
                                    .collect::<Vec<_>>(),
                            );
                            let fields: Vec<String> = cols.iter().map(|c| c.field()).collect();
                            let raw = self
                                .storage
                                .find_matching(self.db(), &ct.table, &Document::new())
                                .map_err(|e| Self::storage_err("could not read", e))?;
                            let docs: Vec<Document> = raw
                                .iter()
                                .map(|b| bson::from_slice(b))
                                .collect::<Result<_, _>>()
                                .map_err(|e| Self::storage_err("could not decode a row", e))?;
                            let types: Vec<Type> =
                                schema.iter().map(|fi| fi.datatype().clone()).collect();
                            let values = docs
                                .iter()
                                .map(|d| {
                                    fields
                                        .iter()
                                        .zip(types.iter())
                                        .map(|(f, ty)| copy_reassemble(d, f, ty))
                                        .collect::<Vec<_>>()
                                })
                                .collect();
                            (schema, values)
                        }
                    };

                let n = schema.len();
                let format = ct.format;
                // The binary format encodes each field through the SAME codec
                // the SELECT binary path uses (`encode_binary`), so the two can
                // never drift: a `DataRowEncoder` over a binary-format schema
                // produces exactly the `[i32 len][bytes]` layout a binary COPY
                // field wants. The header (fixed signature + flags + extension)
                // rides the first row; the field count precedes every row; the
                // trailer is appended by `CopyResponse::new` for format 1.
                let bin_schema: Arc<Vec<FieldInfo>> = if format == CopyFormat::Binary {
                    Arc::new(
                        schema
                            .iter()
                            .map(|f| {
                                FieldInfo::new(
                                    f.name().to_string(),
                                    None,
                                    None,
                                    f.datatype().clone(),
                                    FieldFormat::Binary,
                                )
                            })
                            .collect(),
                    )
                } else {
                    schema.clone()
                };
                let mut header_written = false;
                let backend = self.backend.clone();
                let data = stream::iter(rows).map(move |row| {
                    // The rows stream out AFTER this statement has answered,
                    // so a `CancelRequest` mid-COPY is noticed here: the
                    // stream ends in the `57014` and the block is poisoned
                    // for the next statement.
                    if backend.cancelled() {
                        backend
                            .stream_failed
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        return Err(Self::query_canceled());
                    }
                    // The two TEXTUAL formats are written here rather than
                    // through an encoder. Their null handling asks the value
                    // whether it is null, and an `Option` of the wrong type
                    // answered "not null" with no bytes -- so every NULL came
                    // out as an empty field instead of `\N`, which is exactly
                    // the distinction COPY text exists to preserve. The rules
                    // are short and were measured; binary goes through the
                    // shared codec, where the per-type byte layout is the hard
                    // part.
                    match format {
                        CopyFormat::Binary => {
                            let mut enc = DataRowEncoder::new(bin_schema.clone());
                            for (i, v) in row.iter().enumerate() {
                                // A text-family column's binary bytes are its
                                // string in the client encoding, so it goes
                                // through the same transcoding wrapper the live
                                // query path uses; everything else is unchanged.
                                let field = &bin_schema[i];
                                let v = v.as_ref();
                                transcoding_field(&mut enc, field, row_cenc, |e| {
                                    encode_binary(e, field.datatype(), v)
                                })?;
                            }
                            let dr = enc.take_row();
                            let mut buf = BytesMut::with_capacity(dr.data.len() + 21);
                            if !header_written {
                                buf.put_slice(b"PGCOPY\n\xff\r\n\x00");
                                buf.put_i32(0); // flags (no OIDs)
                                buf.put_i32(0); // header extension length
                                header_written = true;
                            }
                            buf.put_i16(n as i16);
                            buf.extend_from_slice(&dr.data);
                            Ok(CopyData::new(buf.freeze()))
                        }
                        _ => {
                            let line = copy_text_row(&row, format, &bin_schema);
                            // COPY text is line-structured with ASCII delimiters
                            // and escapes, so the whole line transcodes as one
                            // blob to the client encoding (only the field
                            // content bytes move); an untranslatable character
                            // is the same 22P05 the row path raises.
                            if row_cenc.transcodes() {
                                let bytes = encoding::encode(row_cenc, &line)
                                    .map_err(|ch| untranslatable_char(ch, row_cenc))?;
                                Ok(CopyData::new(Bytes::from(bytes)))
                            } else {
                                Ok(CopyData::new(line))
                            }
                        }
                    }
                });
                // The response's format code must match: 1 for binary, 0 for
                // the two textual ones.
                let code = if ct.format == CopyFormat::Binary {
                    1
                } else {
                    0
                };
                Ok(vec![Response::CopyOut(CopyResponse::new(code, n, data))])
            }

            Statement::AlterRole(role) => {
                let me = self
                    .session_user
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if role != me {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42704".into(), // undefined_object
                        format!("role \"{role}\" does not exist"),
                    ))));
                }
                Ok(vec![Response::Execution(Tag::new("ALTER ROLE"))])
            }

            Statement::Show(name) => {
                let key = canonical_setting(&name);
                // Release the settings lock BEFORE building the field:
                // `field` reads `client_encoding` from the same (non-reentrant)
                // mutex to transcode the column name.
                let value = self
                    .settings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| {
                        PgWireError::UserError(Box::new(ErrorInfo::new(
                            "ERROR".into(),
                            "42704".into(), // undefined_object
                            format!("unrecognized configuration parameter \"{name}\""),
                        )))
                    })?;
                let schema = Arc::new(vec![self.field(key, Type::TEXT)]);
                let schema_ref = schema.clone();
                let rows = stream::iter(std::iter::once(value)).map(move |v| {
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    enc.encode_field(&Some(v.as_str()))?;
                    Ok(enc.take_row())
                });
                let mut response = QueryResponse::new(schema, rows);
                // PostgreSQL's tag is a bare `SHOW` -- one row, no count.
                response.set_bare_command_tag("SHOW");
                Ok(vec![Response::Query(response)])
            }

            // The wire layer owns the prepared-statement store, so there is
            // nothing here to free: psycopg issues this to reset its own cache
            // and then re-prepares under fresh names. The TAG is the part that
            // matters, and PostgreSQL's is `DEALLOCATE ALL`, not `DEALLOCATE`.
            Statement::Fetch {
                name,
                direction,
                count,
                is_move,
            } => self.fetch(&name, direction, count, is_move),

            Statement::CloseCursor(name) => {
                let mut cursors = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
                if !name.is_empty() && cursors.remove(&name).is_none() {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "34000".into(), // invalid_cursor_name
                        format!("cursor \"{name}\" does not exist"),
                    ))));
                }
                // `CLOSE ALL` carries an empty name.
                if name.is_empty() {
                    cursors.clear();
                }
                Ok(vec![Response::Execution(Tag::new("CLOSE CURSOR"))])
            }

            // The wire layer's own statement store keeps its entry: psycopg
            // never reuses a deallocated name (its counter only climbs), and
            // the store is the connection's, dropped with it.
            Statement::DeallocateAll => {
                self.prepared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                Ok(vec![Response::Execution(Tag::new("DEALLOCATE ALL"))])
            }
            Statement::Deallocate(name) => {
                let mut prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
                let Some(idx) = prepared.iter().position(|r| r.name == name) else {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "26000".into(), // invalid_sql_statement_name
                        format!("prepared statement \"{name}\" does not exist"),
                    ))));
                };
                prepared.remove(idx);
                Ok(vec![Response::Execution(Tag::new("DEALLOCATE"))])
            }
            Statement::Notify { channel, payload } => {
                self.queue_notify(&channel, &payload)?;
                Ok(vec![Response::Execution(Tag::new("NOTIFY"))])
            }
            Statement::Listen(channel) => {
                self.pending_listens
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(ListenOp::Listen(channel));
                Ok(vec![Response::Execution(Tag::new("LISTEN"))])
            }
            Statement::Unlisten(channel) => {
                self.pending_listens
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(match channel {
                        Some(channel) => ListenOp::Unlisten(channel),
                        None => ListenOp::UnlistenAll,
                    });
                Ok(vec![Response::Execution(Tag::new("UNLISTEN"))])
            }

            Statement::Set { name, value } => {
                let key = canonical_setting(&name);
                if key == "client_encoding" {
                    // Validated, canonicalised, and reported separately: an
                    // invalid name must be refused (not stored), and the stored
                    // value must be the canonical spelling the client reads back.
                    self.apply_client_encoding(&value)?;
                } else {
                    // DateStyle is stored in its canonical spelling so `SHOW
                    // datestyle` answers what PostgreSQL does (`ISO, MDY`), and so
                    // the stored value and the reported ParameterStatus agree.
                    let value = if key == "DateStyle" {
                        secantus_pgplan::DateStyle::parse(&value).canonical()
                    } else if IDLE_TIMEOUT_GUCS.iter().any(|(guc, _, _)| *guc == key) {
                        canonical_ms_guc(&key, &value)?
                    } else if BOOL_GUCS.contains(&key.as_str()) {
                        canonical_bool_guc(&key, &value)?
                    } else {
                        value
                    };
                    let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                    settings.insert(key.clone(), value.clone());
                    drop(settings);
                    self.note_reportable_guc(&key, &value);
                }
                Ok(vec![Response::Execution(Tag::new("SET"))])
            }

            // `SET TRANSACTION <modes>` sets the CURRENT block's
            // characteristics. Outside an explicit block PostgreSQL warns and
            // does nothing, so we only apply the modes when a transaction is
            // open; either way the command tag is `SET`.
            Statement::SetTransaction(modes) => {
                if self
                    .in_transaction
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(iso) = &modes.isolation {
                        settings.insert("transaction_isolation".into(), iso.clone());
                    }
                    if let Some(ro) = modes.read_only {
                        settings.insert(
                            "transaction_read_only".into(),
                            if ro { "on" } else { "off" }.into(),
                        );
                    }
                    if let Some(df) = modes.deferrable {
                        settings.insert(
                            "transaction_deferrable".into(),
                            if df { "on" } else { "off" }.into(),
                        );
                    }
                }
                Ok(vec![Response::Execution(Tag::new("SET"))])
            }

            // `SET SESSION CHARACTERISTICS AS TRANSACTION <modes>` sets the
            // session DEFAULT. When no explicit block is open, the next
            // implicit statement inherits it, so the `transaction_*` GUCs move
            // in lockstep -- which is what a client reads back immediately.
            Statement::SetSessionCharacteristics(modes) => {
                let in_txn = self
                    .in_transaction
                    .load(std::sync::atomic::Ordering::Relaxed);
                let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(iso) = &modes.isolation {
                    settings.insert("default_transaction_isolation".into(), iso.clone());
                    if !in_txn {
                        settings.insert("transaction_isolation".into(), iso.clone());
                    }
                }
                if let Some(ro) = modes.read_only {
                    let v = if ro { "on" } else { "off" };
                    settings.insert("default_transaction_read_only".into(), v.into());
                    if !in_txn {
                        settings.insert("transaction_read_only".into(), v.into());
                    }
                }
                if let Some(df) = modes.deferrable {
                    let v = if df { "on" } else { "off" };
                    settings.insert("default_transaction_deferrable".into(), v.into());
                    if !in_txn {
                        settings.insert("transaction_deferrable".into(), v.into());
                    }
                }
                Ok(vec![Response::Execution(Tag::new("SET"))])
            }

            Statement::Reset(name) => {
                let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                if name.is_empty() {
                    *settings = default_settings();
                } else {
                    let key = canonical_setting(&name);
                    // RESET restores the DEFAULT, which is not the same as
                    // removing the setting: a client reading it back afterwards
                    // must see the default, not an error.
                    match default_settings().get(&key) {
                        Some(d) => {
                            settings.insert(key.clone(), d.clone());
                            let d = d.clone();
                            drop(settings);
                            self.note_reportable_guc(&key, &d);
                        }
                        None => {
                            settings.remove(&key);
                        }
                    }
                }
                Ok(vec![Response::Execution(Tag::new("RESET"))])
            }

            Statement::SelectConstant(sc) => {
                // One row, no storage touched.
                let schema = Arc::new(
                    sc.columns
                        .iter()
                        .map(|(name, _, ty, typmod)| {
                            let wire = self.user_wire_type(ty).unwrap_or_else(|| wire_type(ty));
                            self.field_mod(name.clone(), wire, *typmod)
                        })
                        .collect::<Vec<_>>(),
                );
                // A false WHERE means no row -- and nothing to resolve, so a
                // `pg_sleep()` behind it does not wait either.
                let values = self.const_rows(&sc)?;
                let schema_ref = schema.clone();
                let rows = stream::iter(values).map(move |vals| {
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    for (i, v) in vals.iter().enumerate() {
                        encode_field_value(
                            &mut enc,
                            &schema_ref[i],
                            Some(v),
                            &row_tz,
                            &row_ds,
                            row_cenc,
                        )?;
                    }
                    Ok(enc.take_row())
                });
                Ok(vec![Response::Query(QueryResponse::new(schema, rows))])
            }

            Statement::ValuesConstant(vc) => {
                // A fixed set of literal rows, no storage touched.
                let schema = Arc::new(
                    vc.names
                        .iter()
                        .zip(&vc.types)
                        .map(|(name, ty)| {
                            let wire = self.user_wire_type(ty).unwrap_or_else(|| wire_type(ty));
                            self.field(name.clone(), wire)
                        })
                        .collect::<Vec<_>>(),
                );
                let schema_ref = schema.clone();
                let rows = stream::iter(vc.rows).map(move |vals| {
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    for (i, v) in vals.iter().enumerate() {
                        encode_field_value(
                            &mut enc,
                            &schema_ref[i],
                            Some(v),
                            &row_tz,
                            &row_ds,
                            row_cenc,
                        )?;
                    }
                    Ok(enc.take_row())
                });
                Ok(vec![Response::Query(QueryResponse::new(schema, rows))])
            }

            Statement::Aggregate(agg) => {
                // A generated source, as for a plain SELECT: the grouping and
                // accumulation below work on documents and do not care where
                // they came from.
                let groups = self.aggregate_groups(&agg, max_rows)?;
                let schema = Arc::new(
                    agg.select
                        .iter()
                        .map(|(name, col)| {
                            let ty = match col {
                                OutputCol::Group(i) => {
                                    let t = &agg.group_by[*i].pg_type;
                                    self.user_wire_type(t).unwrap_or_else(|| wire_type(t))
                                }
                                OutputCol::Agg(i) => aggregate_wire_type(&agg.items[*i]),
                            };
                            self.field(name.clone(), ty)
                        })
                        .collect::<Vec<_>>(),
                );

                let select = agg.select.clone();
                let schema_ref = schema.clone();
                let rows = stream::iter(groups).map(move |(key, vals)| {
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    for (n, (_, col)) in select.iter().enumerate() {
                        let v = match col {
                            OutputCol::Group(i) => key[*i].clone().unwrap_or(Bson::Null),
                            OutputCol::Agg(i) => vals[*i].clone(),
                        };
                        encode_field_value(
                            &mut enc,
                            &schema_ref[n],
                            Some(&v),
                            &row_tz,
                            &row_ds,
                            row_cenc,
                        )?;
                    }
                    Ok(enc.take_row())
                });
                Ok(vec![Response::Query(QueryResponse::new(schema, rows))])
            }

            Statement::Update(upd) => {
                let def = self.lookup(&upd.table);
                let constrained = def.as_ref().is_some_and(table_has_row_constraints);
                let matched = if upd.set_exprs.is_empty() && !constrained {
                    self.update_rows(&upd.table, &upd.filter, &upd.set, &upd.unset)?
                } else {
                    // A SET list that reads the row (`num = num * 2`) is
                    // evaluated over each matched row and written by `_id`.
                    // Every row is evaluated BEFORE the first write, so an
                    // expression that fails on one row leaves none updated.
                    let raw = self
                        .storage
                        .find_matching(self.db(), &upd.table, &upd.filter)
                        .map_err(|e| Self::storage_err("could not read", e))?;
                    let mut writes = Vec::with_capacity(raw.len());
                    let mut new_rows = Vec::with_capacity(raw.len());
                    for bytes in &raw {
                        let row: Document = bson::from_slice(bytes)
                            .map_err(|e| Self::storage_err("could not decode a row", e))?;
                        let (set, unset) = if upd.set_exprs.is_empty() {
                            (upd.set.clone(), upd.unset.clone())
                        } else {
                            secantus_pgplan::update_row_sets(&upd, &row)
                                .map_err(|e| Self::err(&e))?
                        };
                        if let Some(def) = def.as_ref() {
                            let mut after = row.clone();
                            for (k, v) in &set {
                                after.insert(k.clone(), v.clone());
                            }
                            for k in &unset {
                                after.remove(k);
                            }
                            self.check_row_constraints(def, &after)?;
                            new_rows.push(after);
                        }
                        let id = row.get("_id").cloned().unwrap_or(Bson::Null);
                        writes.push((id, set, unset));
                    }
                    if let Some(def) = def.as_ref() {
                        self.check_foreign_keys(def, &new_rows)?;
                    }
                    let mut matched = 0usize;
                    for (id, set, unset) in writes {
                        matched +=
                            self.update_rows(&upd.table, &bson::doc! {"_id": id}, &set, &unset)?;
                    }
                    matched
                };
                // PostgreSQL's UPDATE tag counts rows MATCHED, not rows whose
                // value actually changed: `UPDATE t SET n = n` reports every
                // row. `modified` would under-report a no-op assignment.
                Ok(vec![Response::Execution(
                    Tag::new("UPDATE").with_rows(matched),
                )])
            }

            Statement::Delete(del) => {
                if let Some(def) = self.lookup(&del.table) {
                    self.check_referencing_rows(&def, &del.filter)?;
                }
                let deleted = self
                    .storage
                    .delete_matching(
                        self.db(),
                        &del.table,
                        &del.filter,
                        0,
                        &Document::new(),
                        None,
                    )
                    .map_err(|e| Self::storage_err("could not delete", e))?;
                Ok(vec![Response::Execution(
                    Tag::new("DELETE").with_rows(deleted),
                )])
            }
        }
    }
}

impl PgHandler {
    /// Apply one `$set` / `$unset` pair to every row matching `filter`,
    /// returning the number of rows matched.
    fn update_rows(
        &self,
        table: &str,
        filter: &Document,
        set: &Document,
        unset: &[String],
    ) -> PgWireResult<usize> {
        let mut ops = bson::doc! { "$set": set.clone() };
        if !unset.is_empty() {
            let mut u = Document::new();
            for f in unset {
                u.insert(f.clone(), "");
            }
            ops.insert("$unset", u);
        }
        let outcome = self
            .storage
            .update_matching(
                self.db(),
                table,
                filter,
                &ops,
                true,  // multi: SQL UPDATE has no single-row default
                false, // upsert: never; PostgreSQL UPDATE does not insert
                &[],
                &Document::new(),
                None,
                None,
                false,
            )
            .map_err(|e| Self::storage_err("could not update", e))?;
        Ok(outcome.matched)
    }
}

/// Whether an UPDATE of `def` must look at each row it writes: a NOT NULL
/// column, a CHECK, or a FOREIGN KEY can all be violated by the new value.
fn table_has_row_constraints(def: &TableDef) -> bool {
    def.columns.iter().any(|c| !c.nullable && !c.pk)
        || !def.check_constraints.is_empty()
        || !def.foreign_keys.is_empty()
}

/// Two stored values equal as PostgreSQL compares a key: numerics by value
/// across the integer / float widths, everything else structurally.
fn key_values_equal(a: &Bson, b: &Bson) -> bool {
    fn num(v: &Bson) -> Option<f64> {
        match v {
            Bson::Int32(i) => Some(f64::from(*i)),
            Bson::Int64(i) => Some(*i as f64),
            Bson::Double(d) => Some(*d),
            _ => None,
        }
    }
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

impl PgHandler {
    /// The schema a table's error diagnostics name: a TEMP table lives in the
    /// session's `pg_temp_N` namespace.
    fn schema_of(def: &TableDef) -> String {
        if def.temp {
            "pg_temp_1".to_string()
        } else {
            "public".to_string()
        }
    }

    /// PostgreSQL's `Failing row contains (...)` detail: every column in
    /// declaration order, each as its text output, NULL as `null`.
    fn failing_row_detail(def: &TableDef, row: &Document) -> String {
        let cells: Vec<String> = def
            .columns
            .iter()
            .map(|c| match row.get(c.field()) {
                None | Some(Bson::Null) => "null".to_string(),
                Some(v) => secantus_pgplan::value_text(v),
            })
            .collect();
        format!("Failing row contains ({}).", cells.join(", "))
    }

    /// An integrity-constraint error (class 23) with the diagnostic fields
    /// PostgreSQL attaches: schema and table always, the constraint or the
    /// column as the kind of violation names it.
    fn constraint_error(
        code: &str,
        message: String,
        detail: String,
        def: &TableDef,
        constraint: Option<&str>,
        column: Option<&str>,
    ) -> PgWireError {
        let mut info = ErrorInfo::new("ERROR".into(), code.into(), message);
        info.detail = Some(detail);
        info.schema = Some(Self::schema_of(def));
        info.table = Some(def.name.clone());
        info.constraint = constraint.map(str::to_string);
        info.column = column.map(str::to_string);
        PgWireError::UserError(Box::new(info))
    }

    /// NOT NULL and CHECK over one row about to be written, in PostgreSQL's
    /// order: every NOT NULL column first, then the CHECK constraints by
    /// name. A CHECK that evaluates to NULL passes (SQL's rule).
    fn check_row_constraints(&self, def: &TableDef, row: &Document) -> PgWireResult<()> {
        for c in &def.columns {
            if c.nullable || c.pk {
                continue;
            }
            if matches!(row.get(c.field()), None | Some(Bson::Null)) {
                return Err(Self::constraint_error(
                    "23502",
                    format!(
                        "null value in column \"{}\" of relation \"{}\" violates not-null constraint",
                        c.name, def.name
                    ),
                    Self::failing_row_detail(def, row),
                    def,
                    None,
                    Some(&c.name),
                ));
            }
        }
        for check in &def.check_constraints {
            let expr = secantus_pgplan::plan_check_expression(&check.expression, def)
                .map_err(|e| Self::err(&e))?;
            let verdict = secantus_pgplan::apply_row_expr(&expr, row).map_err(|e| Self::err(&e))?;
            if verdict == Bson::Boolean(false) {
                return Err(Self::constraint_error(
                    "23514",
                    format!(
                        "new row for relation \"{}\" violates check constraint \"{}\"",
                        def.name, check.name
                    ),
                    Self::failing_row_detail(def, row),
                    def,
                    Some(&check.name),
                    None,
                ));
            }
        }
        Ok(())
    }

    /// Whether `value` is present in the referenced column of `parent` --
    /// among its stored rows, or among `pending` rows the same statement is
    /// about to write when the key refers back to its own table.
    fn referenced_key_exists(
        &self,
        parent: &TableDef,
        ref_field: &str,
        value: &Bson,
        pending: &[Document],
    ) -> PgWireResult<bool> {
        if pending
            .iter()
            .any(|r| r.get(ref_field).is_some_and(|v| key_values_equal(v, value)))
        {
            return Ok(true);
        }
        let found = self
            .storage
            .find_matching(
                self.db(),
                &parent.name,
                &bson::doc! { ref_field: value.clone() },
            )
            .map_err(|e| Self::storage_err("could not read", e))?;
        Ok(!found.is_empty())
    }

    /// The child side of every FOREIGN KEY on `def`, over the rows a
    /// statement is about to write. An INITIALLY DEFERRED key inside a
    /// transaction is queued for COMMIT instead.
    fn check_foreign_keys(&self, def: &TableDef, rows: &[Document]) -> PgWireResult<()> {
        for fk in &def.foreign_keys {
            if fk.initially_deferred
                && self
                    .in_transaction
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                self.defer_fk(&def.name, &fk.name);
                continue;
            }
            self.check_fk_child_side(def, fk, rows)?;
        }
        Ok(())
    }

    fn defer_fk(&self, table: &str, name: &str) {
        let mut q = self.deferred_fks.lock().unwrap_or_else(|e| e.into_inner());
        let key = (table.to_string(), name.to_string());
        if !q.contains(&key) {
            q.push(key);
        }
    }

    /// `rows` of `def` (the referencing table) must each find their key in
    /// the referenced table; a NULL key passes. When the key refers back to
    /// `def` itself, the rows being written count as present -- PostgreSQL
    /// checks at the end of the statement, so `(1, 2), (2, 1)` inserts.
    fn check_fk_child_side(
        &self,
        def: &TableDef,
        fk: &secantus_pgcatalog::ForeignKey,
        rows: &[Document],
    ) -> PgWireResult<()> {
        let (Some(col), Some(ref_col)) = (fk.columns.first(), fk.ref_columns.first()) else {
            return Ok(());
        };
        let Some(field) = def.field_of(col) else {
            return Ok(());
        };
        let parent = if fk.ref_table == def.name {
            def.clone()
        } else {
            self.lookup(&fk.ref_table)
                .ok_or_else(|| Self::err(&PlanError::UndefinedTable(fk.ref_table.clone())))?
        };
        let Some(ref_field) = parent.field_of(ref_col) else {
            return Ok(());
        };
        let pending: &[Document] = if fk.ref_table == def.name { rows } else { &[] };
        for row in rows {
            let value = match row.get(&field) {
                None | Some(Bson::Null) => continue,
                Some(v) => v,
            };
            if !self.referenced_key_exists(&parent, &ref_field, value, pending)? {
                return Err(Self::constraint_error(
                    "23503",
                    format!(
                        "insert or update on table \"{}\" violates foreign key constraint \"{}\"",
                        def.name, fk.name
                    ),
                    format!(
                        "Key ({})=({}) is not present in table \"{}\".",
                        col,
                        secantus_pgplan::value_text(value),
                        fk.ref_table
                    ),
                    def,
                    Some(&fk.name),
                    None,
                ));
            }
        }
        Ok(())
    }

    /// Every table whose FOREIGN KEYs reference `parent`, with those keys.
    /// Every user table this session can see: the committed catalog with
    /// what this transaction created or dropped overlaid on it.
    fn all_table_defs(&self) -> PgWireResult<Vec<TableDef>> {
        let raw = self
            .storage
            .find_matching(self.db(), CATALOG_COLLECTION, &Document::new())
            .map_err(|e| Self::storage_err("could not read the catalog", e))?;
        let mut defs: Vec<TableDef> = raw
            .iter()
            .filter_map(|b| bson::from_slice::<Document>(b).ok())
            .filter_map(|d| TableDef::from_document(&d))
            .collect();
        {
            let pending = self.uncommitted.lock().unwrap_or_else(|e| e.into_inner());
            defs.retain(|d| !pending.contains_key(&d.name));
            defs.extend(pending.values().filter_map(|d| d.clone()));
        }
        Ok(defs)
    }

    fn referencing_keys(
        &self,
        parent: &str,
    ) -> PgWireResult<Vec<(TableDef, secantus_pgcatalog::ForeignKey)>> {
        let defs = self.all_table_defs()?;
        Ok(defs
            .into_iter()
            .flat_map(|d| {
                let fks: Vec<_> = d
                    .foreign_keys
                    .iter()
                    .filter(|fk| fk.ref_table == parent)
                    .cloned()
                    .collect();
                fks.into_iter().map(move |fk| (d.clone(), fk))
            })
            .collect())
    }

    /// The parent side of a DELETE from `def`: a row another table's row
    /// still references cannot go (NO ACTION / RESTRICT), is followed by its
    /// dependants (CASCADE), or leaves them keyless (SET NULL). Applied
    /// BEFORE the delete, so a refusal leaves the table untouched.
    fn check_referencing_rows(&self, def: &TableDef, filter: &Document) -> PgWireResult<()> {
        let referencing = self.referencing_keys(&def.name)?;
        if referencing.is_empty() {
            return Ok(());
        }
        let raw = self
            .storage
            .find_matching(self.db(), &def.name, filter)
            .map_err(|e| Self::storage_err("could not read", e))?;
        let going: Vec<Document> = raw
            .iter()
            .filter_map(|b| bson::from_slice::<Document>(b).ok())
            .collect();
        for (child, fk) in referencing {
            let (Some(col), Some(ref_col)) = (fk.columns.first(), fk.ref_columns.first()) else {
                continue;
            };
            let (Some(field), Some(ref_field)) = (child.field_of(col), def.field_of(ref_col))
            else {
                continue;
            };
            for row in &going {
                let Some(key) = row.get(&ref_field).filter(|v| **v != Bson::Null) else {
                    continue;
                };
                // A self-referencing row that is itself going does not hold
                // its own parent; only rows that STAY count.
                let mut child_filter = bson::doc! { &field: key.clone() };
                if child.name == def.name {
                    let going_ids: Vec<Bson> =
                        going.iter().filter_map(|r| r.get("_id").cloned()).collect();
                    child_filter.insert("_id", bson::doc! { "$nin": going_ids });
                }
                let dependants = self
                    .storage
                    .find_matching(self.db(), &child.name, &child_filter)
                    .map_err(|e| Self::storage_err("could not read", e))?;
                if dependants.is_empty() {
                    continue;
                }
                match fk.on_delete.as_deref() {
                    Some("CASCADE") => {
                        self.check_referencing_rows(&child, &child_filter)?;
                        self.storage
                            .delete_matching(
                                self.db(),
                                &child.name,
                                &child_filter,
                                0,
                                &Document::new(),
                                None,
                            )
                            .map_err(|e| Self::storage_err("could not delete", e))?;
                    }
                    Some("SET NULL") => {
                        let mut after = Vec::new();
                        for b in &dependants {
                            let mut r: Document = bson::from_slice(b)
                                .map_err(|e| Self::storage_err("could not decode a row", e))?;
                            r.insert(field.clone(), Bson::Null);
                            self.check_row_constraints(&child, &r)?;
                            after.push(r);
                        }
                        self.update_rows(
                            &child.name,
                            &child_filter,
                            &bson::doc! { &field: Bson::Null },
                            &[],
                        )?;
                    }
                    _ if fk.initially_deferred
                        && self
                            .in_transaction
                            .load(std::sync::atomic::Ordering::Relaxed) =>
                    {
                        self.defer_fk(&child.name, &fk.name);
                    }
                    _ => {
                        return Err(Self::constraint_error(
                            "23503",
                            format!(
                                "update or delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{}\"",
                                def.name, fk.name, child.name
                            ),
                            format!(
                                "Key ({})=({}) is still referenced from table \"{}\".",
                                ref_col,
                                secantus_pgplan::value_text(key),
                                child.name
                            ),
                            &child,
                            Some(&fk.name),
                            None,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Re-check every deferred FOREIGN KEY over the whole referencing table,
    /// as COMMIT does. Runs inside the transaction, so it sees its writes.
    fn run_deferred_checks(&self) -> PgWireResult<()> {
        let queued: Vec<(String, String)> =
            std::mem::take(&mut *self.deferred_fks.lock().unwrap_or_else(|e| e.into_inner()));
        for (table, name) in queued {
            let Some(def) = self.lookup(&table) else {
                continue;
            };
            let Some(fk) = def.foreign_keys.iter().find(|f| f.name == name) else {
                continue;
            };
            let raw = self
                .storage
                .find_matching(self.db(), &table, &Document::new())
                .map_err(|e| Self::storage_err("could not read", e))?;
            let rows: Vec<Document> = raw
                .iter()
                .filter_map(|b| bson::from_slice::<Document>(b).ok())
                .collect();
            self.check_fk_child_side(&def, fk, &rows)?;
        }
        Ok(())
    }

    /// After a COMMIT whose deferred checks failed, the transaction is
    /// already rolled back: the `ReadyForQuery` that follows the error must
    /// say IDLE, not "in a failed transaction".
    fn settle_failed_commit<C: ClientInfo>(&self, client: &mut C) {
        if self
            .commit_failed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            client.set_transaction_status(pgwire::messages::response::TransactionStatus::Idle);
        }
    }
}

/// Fill every column an INSERT omitted with its literal DEFAULT. Sequence
/// defaults are applied first (`apply_serial_defaults`); a column with
/// neither stays absent, which reads as NULL.
fn apply_column_defaults(def: &TableDef, rows: &mut [Document]) {
    for column in &def.columns {
        let Some(default) = column.default.as_ref() else {
            continue;
        };
        let field = column.field();
        for row in rows.iter_mut() {
            if !row.contains_key(&field) {
                row.insert(field.clone(), default.clone());
            }
        }
    }
}

/// A stored timestamp, with its hidden companion added back.
///
/// The remainder is validated rather than trusted: a value outside 0-999, or
/// not an integer at all, is ignored. A hand-edited or foreign document must
/// not be able to produce a time that was never written -- the same
/// defensiveness `subms.py::merge` applies on the Python side.
/// The value one projected cell carries: a computed expression applied to
/// the row, a timestamp column reassembled from its hidden sub-millisecond
/// companion, or the stored value itself. `None` is SQL NULL. The same three
/// cases the row encoder (`project_rows`) walks, as a value rather than as
/// wire bytes.
fn resolve_cell(
    doc: &Document,
    field: &str,
    expr: Option<&secantus_pgplan::ColumnExpr>,
    datatype: &Type,
    tz: &secantus_pgplan::TimeZoneSetting,
) -> PgWireResult<Option<Bson>> {
    if let Some(expr) = expr {
        let v = if matches!(expr, secantus_pgplan::ColumnExpr::Row { .. }) {
            secantus_pgplan::apply_row_expr(expr, doc)
        } else {
            let v = doc.get(field).cloned().unwrap_or(Bson::Null);
            secantus_pgplan::apply_column_expr(expr, v, tz)
        }
        .map_err(|e| PgHandler::err(&e))?;
        return Ok(Some(v));
    }
    let reassembled = if *datatype == Type::TIMESTAMPTZ {
        timestamptz_text(doc, field, tz)
    } else {
        timestamp_text(doc, field)
    };
    Ok(match reassembled {
        Some(text) => Some(Bson::String(text)),
        None => doc.get(field).cloned(),
    })
}

fn timestamp_text(doc: &Document, field: &str) -> Option<String> {
    let ms = match doc.get(field) {
        Some(Bson::DateTime(d)) => d.timestamp_millis(),
        _ => return None,
    };
    let rem = match doc.get(companion_field(field)) {
        Some(Bson::Int32(v)) if (1..1000).contains(v) => i64::from(*v),
        Some(Bson::Int64(v)) if (1..1000).contains(v) => *v,
        _ => 0,
    };
    Some(render_timestamp(ms * 1000 + rem))
}

/// A stored `timestamptz` COLUMN value: the same date + `__us_` companion a
/// `timestamp` uses (the stored form is a UTC INSTANT), but rendered in the
/// SESSION zone rather than naively. A special value (infinity / wide / BC) is
/// stored as a String and is not handled here -- it falls to
/// `encode_field_value`, which passes the String through verbatim.
fn timestamptz_text(
    doc: &Document,
    field: &str,
    tz: &secantus_pgplan::TimeZoneSetting,
) -> Option<String> {
    let ms = match doc.get(field) {
        Some(Bson::DateTime(d)) => d.timestamp_millis(),
        _ => return None,
    };
    let rem = match doc.get(companion_field(field)) {
        Some(Bson::Int32(v)) if (1..1000).contains(v) => i64::from(*v),
        Some(Bson::Int64(v)) if (1..1000).contains(v) => *v,
        _ => 0,
    };
    Some(secantus_pgplan::render_timestamptz(ms * 1000 + rem, tz))
}

/// A stored value for a COPY row, with a timestamp/timestamptz column's hidden
/// sub-millisecond companion (`__us_<field>`) folded back into the composite
/// carrier the encoders understand.
///
/// The SELECT row path reassembles the same way (`timestamp_text` /
/// `timestamptz_text`); COPY reads the raw document, so without this it would
/// hand the encoder a millisecond-truncated `DateTime` and lose the last three
/// digits of a `.ffffff` timestamp. Every other column is its value verbatim.
fn copy_reassemble(d: &Document, field: &str, ty: &Type) -> Option<Bson> {
    if matches!(*ty, Type::TIMESTAMP | Type::TIMESTAMPTZ) {
        if let Some(Bson::DateTime(dt)) = d.get(field) {
            let rem = match d.get(companion_field(field)) {
                Some(Bson::Int32(v)) if (1..1000).contains(v) => i64::from(*v),
                Some(Bson::Int64(v)) if (1..1000).contains(v) => *v,
                _ => 0,
            };
            if rem != 0 {
                let mut doc = Document::new();
                doc.insert(secantus_pgplan::COMPOSITE_DATE, Bson::DateTime(*dt));
                doc.insert(secantus_pgplan::COMPOSITE_US, Bson::Int32(rem as i32));
                return Some(Bson::Document(doc));
            }
        }
    }
    d.get(field).cloned()
}

/// Encode one stored value as a SQL datum. Absent and explicit null are both
/// SQL NULL.
/// The types this server can put on the wire in PostgreSQL's BINARY format.
///
/// A column outside this list stays TEXT even when the client asked for
/// binary. The format travels per column in the `RowDescription`, so the
/// client still decodes it correctly -- but PostgreSQL honours the request for
/// every type, and the gap is recorded in `tasks/backlog.md` rather than
/// hidden.
fn binary_encodable(ty: &Type) -> bool {
    const OK: [Type; 35] = [
        Type::OID,
        Type::JSON_ARRAY,
        Type::JSONB_ARRAY,
        Type::OID_ARRAY,
        Type::BYTEA,
        Type::UUID,
        Type::INET,
        Type::CIDR,
        Type::BYTEA_ARRAY,
        Type::UUID_ARRAY,
        Type::INET_ARRAY,
        Type::CIDR_ARRAY,
        Type::VOID,
        Type::JSON,
        Type::JSONB,
        Type::BOOL,
        Type::INT2,
        Type::INT4,
        Type::INT8,
        Type::FLOAT4,
        Type::FLOAT8,
        Type::TEXT,
        Type::VARCHAR,
        Type::BPCHAR,
        Type::NAME,
        Type::CHAR,
        Type::NUMERIC,
        Type::BOOL_ARRAY,
        Type::INT2_ARRAY,
        Type::INT4_ARRAY,
        Type::INT8_ARRAY,
        Type::FLOAT4_ARRAY,
        Type::FLOAT8_ARRAY,
        Type::TEXT_ARRAY,
        Type::NUMERIC_ARRAY,
    ];
    if matches!(ty.kind(), postgres_types::Kind::Enum(_)) {
        return true;
    }
    // A user COMPOSITE has a binary record wire format, and a composite ARRAY
    // does too (the array encoder length-prefixes each element's binary bytes).
    // So does an ANONYMOUS record (`ROW(...)`, oid 2249): its field oids are
    // the expression types (`'x'` is `unknown`, `'x'::text` is `text`), which
    // the planner now records beside the fields (`RECORD_TYPES_KEY`).
    if *ty == Type::RECORD || matches!(ty.kind(), postgres_types::Kind::Composite(_)) {
        return true;
    }
    // A user ENUM ARRAY too: each element's binary form is its label bytes,
    // so the array encoder length-prefixes those (PostgreSQL's `array_send`
    // over `enum_send`).
    if let postgres_types::Kind::Array(inner) = ty.kind() {
        if matches!(
            inner.kind(),
            postgres_types::Kind::Composite(_) | postgres_types::Kind::Enum(_)
        ) {
            return true;
        }
    }
    // The datetime family, every range / multirange (builtin or user-defined),
    // and arrays of any of them: PostgreSQL sends them all in binary when
    // asked, and so must this server. psycopg reads EVERY column of a result
    // in the format of column 0 (`Transformer.set_pgresult` looks at
    // `PQfformat(res, 0)` only, because PostgreSQL never mixes formats within
    // one request), so a single text column in an otherwise-binary row made
    // the client decode binary bytes with text loaders -- garbage dates,
    // `could not convert string to float`, `UnicodeDecodeError` -- or vice
    // versa. Measured on 16: `select date, float4` with a binary result comes
    // back with both columns in format 1.
    if datetime_or_range_kind(ty) {
        return true;
    }
    OK.contains(ty)
}

/// A date / time / timetz / timestamp / timestamptz / interval, a range or
/// multirange (builtin or user-defined), or an ARRAY of one of those.
fn datetime_or_range_kind(ty: &Type) -> bool {
    let scalar = |t: &Type| {
        matches!(
            *t,
            Type::DATE
                | Type::TIME
                | Type::TIMETZ
                | Type::TIMESTAMP
                | Type::TIMESTAMPTZ
                | Type::INTERVAL
        ) || matches!(
            t.kind(),
            postgres_types::Kind::Range(_) | postgres_types::Kind::Multirange(_)
        )
    };
    match ty.kind() {
        postgres_types::Kind::Array(inner) => scalar(inner),
        _ => scalar(ty),
    }
}

/// A `numeric` in both wire formats.
///
/// The text half is the rendering the rest of the server already produces --
/// `1.50`, scale and all. The binary half is PostgreSQL's own layout, built
/// from that same text, because a numeric carries more digits than any float
/// this could pass through on the way.
#[derive(Debug)]
struct PgNumeric(String);

impl ToSqlText for PgNumeric {
    fn to_sql_text(
        &self,
        ty: &Type,
        out: &mut BytesMut,
        options: &FormatOptions,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        self.0.as_str().to_sql_text(ty, out, options)
    }
}

impl ToSql for PgNumeric {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        let bytes = numeric_binary(&self.0).ok_or_else(
            || -> Box<dyn std::error::Error + Sync + Send> {
                format!("cannot render {} as a binary numeric", self.0).into()
            },
        )?;
        out.put_slice(&bytes);
        Ok(IsNull::No)
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::NUMERIC
    }

    to_sql_checked!();
}

/// A field value whose wire bytes are already final -- written verbatim in
/// either format. Used to emit a text value that has been transcoded to the
/// client encoding, where the bytes are no longer valid UTF-8 and so cannot
/// travel as a Rust `String`. A text-family value has the same bytes in text
/// and binary format, so one wrapper serves both.
#[derive(Debug)]
struct RawEncoded(Vec<u8>);

impl ToSqlText for RawEncoded {
    fn to_sql_text(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
        _options: &FormatOptions,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        out.put_slice(&self.0);
        Ok(IsNull::No)
    }
}

impl ToSql for RawEncoded {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        out.put_slice(&self.0);
        Ok(IsNull::No)
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

/// PostgreSQL's binary `numeric`: `ndigits`, `weight`, `sign`, `dscale`, then
/// `ndigits` base-10000 groups, most significant first.
///
/// The groups are aligned on the decimal point rather than on the digit
/// string, which is why both halves are padded to a multiple of four before
/// being cut up: `0.00001` is one group of `1000` at weight `-2`, not a group
/// that straddles the point.
fn numeric_binary(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let header = |ndigits: i16, weight: i16, sign: u16, dscale: u16, out: &mut Vec<u8>| {
        out.extend_from_slice(&ndigits.to_be_bytes());
        out.extend_from_slice(&weight.to_be_bytes());
        out.extend_from_slice(&sign.to_be_bytes());
        out.extend_from_slice(&dscale.to_be_bytes());
    };
    let t = text.trim();
    // The three non-finite values are a sign word and nothing else.
    let special = match t {
        "NaN" | "nan" | "NAN" => Some(0xC000u16),
        "Infinity" | "inf" | "Inf" => Some(0xD000),
        "-Infinity" | "-inf" | "-Inf" => Some(0xF000),
        _ => None,
    };
    if let Some(sign) = special {
        header(0, 0, sign, 0, &mut out);
        return Some(out);
    }

    let plain = secantus_pgplan::plain_numeric_text(t);
    let (sign, body) = match plain.strip_prefix('-') {
        Some(rest) => (0x4000u16, rest.to_string()),
        None => (
            0x0000,
            plain.strip_prefix('+').unwrap_or(&plain).to_string(),
        ),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body.as_str(), ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part
        .bytes()
        .chain(frac_part.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let dscale = u16::try_from(frac_part.len()).ok()?;

    let mut int_padded = String::new();
    int_padded.push_str(&"0".repeat((4 - int_part.len() % 4) % 4));
    int_padded.push_str(int_part);
    let mut frac_padded = String::from(frac_part);
    frac_padded.push_str(&"0".repeat((4 - frac_part.len() % 4) % 4));

    let mut digits: Vec<i16> = Vec::new();
    for chunk in int_padded
        .as_bytes()
        .chunks(4)
        .chain(frac_padded.as_bytes().chunks(4))
    {
        digits.push(std::str::from_utf8(chunk).ok()?.parse::<i16>().ok()?);
    }
    // The weight counts groups BEFORE the point, so it moves as leading empty
    // groups are dropped; trailing ones just disappear.
    let mut weight = (int_padded.len() / 4) as i32 - 1;
    while digits.first() == Some(&0) {
        digits.remove(0);
        weight -= 1;
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
    if digits.is_empty() {
        weight = 0;
    }

    header(
        i16::try_from(digits.len()).ok()?,
        i16::try_from(weight).ok()?,
        sign,
        dscale,
        &mut out,
    );
    for d in digits {
        out.extend_from_slice(&d.to_be_bytes());
    }
    Some(out)
}

/// One value in the BINARY format, encoded against the column's DECLARED type
/// rather than against the BSON type it happens to be stored as.
///
/// That distinction is the whole point: an `int8` column holding a BSON
/// `Int32` renders as `1` either way in text, but in binary it would put four
/// bytes where the client reads eight. Text encoding is left exactly as it
/// was.
fn encode_binary(enc: &mut DataRowEncoder, ty: &Type, v: Option<&Bson>) -> PgWireResult<()> {
    let v = match v {
        None | Some(Bson::Null) => return enc.encode_field(&None::<i32>),
        Some(v) => v,
    };
    let bad = |what: &str| -> PgWireError {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".to_owned(),
            "22P03".to_owned(), // invalid_binary_representation
            format!("cannot send {what} as a binary {}", ty.name()),
        )))
    };
    let as_i64 = |v: &Bson| -> Option<i64> {
        match v {
            Bson::Int32(x) => Some(i64::from(*x)),
            Bson::Int64(x) => Some(*x),
            Bson::Double(x) if x.fract() == 0.0 => Some(*x as i64),
            _ => None,
        }
    };
    let as_f64 = |v: &Bson| -> Option<f64> {
        match v {
            Bson::Int32(x) => Some(f64::from(*x)),
            Bson::Int64(x) => Some(*x as f64),
            Bson::Double(x) => Some(*x),
            v if secantus_pgplan::is_numeric(v) => {
                secantus_pgplan::numeric::numeric_text_to_f64(&secantus_pgplan::numeric_text(v)?)
            }
            _ => None,
        }
    };
    let as_numeric = |v: &Bson| -> Option<PgNumeric> {
        match v {
            // Decimal128 and the wide-numeric document alike render to their
            // canonical text; the binary codec is built from that text.
            v if secantus_pgplan::is_numeric(v) => {
                Some(PgNumeric(secantus_pgplan::numeric_text(v)?))
            }
            Bson::Int32(x) => Some(PgNumeric(x.to_string())),
            Bson::Int64(x) => Some(PgNumeric(x.to_string())),
            Bson::Double(x) => Some(PgNumeric(x.to_string())),
            _ => None,
        }
    };
    let as_text = |v: &Bson| -> Option<String> {
        match v {
            Bson::String(x) => Some(x.clone()),
            _ => None,
        }
    };

    // A scalar COMPOSITE or anonymous RECORD: the binary record format, built
    // by `element_binary` (field oids from the composite's declared types, or
    // inferred per value for an oid-2249 record).
    if *ty == Type::RECORD || matches!(ty.kind(), postgres_types::Kind::Composite(_)) {
        let binary = element_binary(v, ty).ok_or_else(|| bad("this value"))?;
        let text = secantus_pgplan::value_text(v);
        return enc.encode_field(&RawField { binary, text });
    }
    // json's binary form is its text verbatim; jsonb's is a one-byte format
    // version (`1`) followed by the same text (PostgreSQL 16 `jsonb_send`).
    if *ty == Type::JSON || *ty == Type::JSONB {
        let text = as_text(v).ok_or_else(|| bad("this value"))?;
        let binary = json_binary(&text, ty == &Type::JSONB);
        return enc.encode_field(&RawField { binary, text });
    }
    if *ty == Type::BYTEA {
        let Bson::Binary(b) = v else {
            return Err(bad("this value"));
        };
        return enc.encode_field(&b.bytes);
    }
    // A uuid's binary form is its 16 raw bytes (`uuid_send`).
    if *ty == Type::UUID {
        let wire = as_text(v)
            .and_then(|t| secantus_pgplan::uuid_to_wire(&t))
            .ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&wire);
    }
    if *ty == Type::INET || *ty == Type::CIDR {
        let Bson::String(text) = v else {
            return Err(bad("this value"));
        };
        let wire = secantus_pgplan::net::to_wire(text, *ty == Type::CIDR)
            .ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&wire);
    }
    if *ty == Type::BOOL {
        let Bson::Boolean(b) = v else {
            return Err(bad("this value"));
        };
        return enc.encode_field(&Some(*b));
    }
    if *ty == Type::INT2 {
        let x = as_i64(v)
            .and_then(|x| i16::try_from(x).ok())
            .ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(x));
    }
    if *ty == Type::INT4 {
        let x = as_i64(v)
            .and_then(|x| i32::try_from(x).ok())
            .ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(x));
    }
    if *ty == Type::INT8 {
        let x = as_i64(v).ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(x));
    }
    if *ty == Type::OID {
        // `u32` IS the oid type in rust-postgres's encoder.
        let x = as_i64(v)
            .and_then(|x| u32::try_from(x).ok())
            .ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(x));
    }
    if *ty == Type::FLOAT4 {
        let x = as_f64(v).ok_or_else(|| bad("this value"))? as f32;
        return enc.encode_field(&Some(x));
    }
    if *ty == Type::FLOAT8 {
        let x = as_f64(v).ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(x));
    }
    if *ty == Type::NUMERIC {
        let x = as_numeric(v).ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(x));
    }
    // date / time / timestamp binary are fixed-width integers: `date` is an i32
    // day count, `time`/`timestamp`/`timestamptz` an i64 microsecond count.
    // `i32`/`i64`'s `to_sql` writes those big-endian ignoring the column type,
    // so the integer IS the field -- the conversion from the stored value is
    // the whole job, and it lives in `secantus_pgplan` beside the inverse
    // render functions the FROM-side decoder already uses.
    if *ty == Type::DATE {
        let text = as_text(v).ok_or_else(|| bad("this value"))?;
        let days = secantus_pgplan::date_to_pg_days(&text).ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(days));
    }
    if *ty == Type::TIME {
        let text = as_text(v).ok_or_else(|| bad("this value"))?;
        let micros = secantus_pgplan::time_to_pg_micros(&text).ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(micros));
    }
    if *ty == Type::TIMESTAMP || *ty == Type::TIMESTAMPTZ {
        let micros = timestamp_pg_micros(v).ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(micros));
    }
    // timetz, interval, and every range / multirange: hand-built layouts
    // (`element_binary` holds them, shared with the array encoder), emitted
    // verbatim through RawField.
    if *ty == Type::TIMETZ
        || *ty == Type::INTERVAL
        || matches!(
            ty.kind(),
            postgres_types::Kind::Range(_) | postgres_types::Kind::Multirange(_)
        )
    {
        let binary = element_binary(v, ty).ok_or_else(|| bad("this value"))?;
        let text = secantus_pgplan::value_text(v);
        return enc.encode_field(&RawField { binary, text });
    }
    // A user ENUM's binary format is its label's UTF-8 -- the same bytes as
    // text -- so it rides the text-family arm. So does `void`, whose binary
    // form is the same zero bytes as its text form.
    if [
        Type::TEXT,
        Type::VARCHAR,
        Type::BPCHAR,
        Type::NAME,
        Type::CHAR,
        Type::VOID,
    ]
    .contains(ty)
        || matches!(ty.kind(), postgres_types::Kind::Enum(_))
    {
        let x = as_text(v).ok_or_else(|| bad("this value"))?;
        return enc.encode_field(&Some(x));
    }

    // Arrays: one element type for the whole array, so the element conversion
    // is chosen once rather than per element.
    let Bson::Array(items) = v else {
        return Err(bad("this value"));
    };
    // An EMPTY array: `array_send` writes it with ZERO dimensions (no
    // dimension pair at all), where postgres_types' `Vec<T>` encoder writes
    // one dimension of length 0. psycopg reads both, but the bytes are what a
    // binary COPY or a byte-comparing client sees -- so it takes the
    // hand-built path, which knows the zero-dimension form.
    // A multidimensional array: hand-build its binary wire form (postgres_types
    // has no ToSql for one) and emit it verbatim through RawField.
    if items.is_empty() || items.iter().any(|x| matches!(x, Bson::Array(_))) {
        let elem = match ty.kind() {
            postgres_types::Kind::Array(inner) => inner.clone(),
            _ => wire_type(element_of_array_oid(ty.oid()).ok_or_else(|| bad("this value"))?),
        };
        let binary = array_binary(items, &elem).ok_or_else(|| bad("this value"))?;
        let text = secantus_pgplan::value_text(v);
        return enc.encode_field(&RawField { binary, text });
    }
    // An ARRAY of COMPOSITE / anonymous RECORD: the element type is carried on
    // the array's own `Kind::Array`, and `array_binary` length-prefixes each
    // element's binary record bytes (via `element_binary`).
    // An ARRAY of a user ENUM takes the same door: the element oid on the
    // wire is the enum's own, and each element is its label's bytes.
    if let postgres_types::Kind::Array(inner) = ty.kind() {
        if *inner == Type::RECORD
            || matches!(
                inner.kind(),
                postgres_types::Kind::Composite(_) | postgres_types::Kind::Enum(_)
            )
        {
            let binary = array_binary(items, inner).ok_or_else(|| bad("this value"))?;
            let text = secantus_pgplan::value_text(v);
            return enc.encode_field(&RawField { binary, text });
        }
    }
    if *ty == Type::BOOL_ARRAY {
        let v: Vec<Option<bool>> = items.iter().map(|x| x.as_bool()).collect();
        return enc.encode_field(&v);
    }
    if *ty == Type::INT2_ARRAY {
        let v: Vec<Option<i16>> = items
            .iter()
            .map(|x| as_i64(x).and_then(|n| i16::try_from(n).ok()))
            .collect();
        return enc.encode_field(&v);
    }
    if *ty == Type::INT4_ARRAY {
        let v: Vec<Option<i32>> = items
            .iter()
            .map(|x| as_i64(x).and_then(|n| i32::try_from(n).ok()))
            .collect();
        return enc.encode_field(&v);
    }
    if *ty == Type::INT8_ARRAY {
        let v: Vec<Option<i64>> = items.iter().map(&as_i64).collect();
        return enc.encode_field(&v);
    }
    if *ty == Type::OID_ARRAY {
        let v: Vec<Option<u32>> = items
            .iter()
            .map(|x| as_i64(x).and_then(|n| u32::try_from(n).ok()))
            .collect();
        return enc.encode_field(&v);
    }
    if *ty == Type::FLOAT4_ARRAY {
        let v: Vec<Option<f32>> = items.iter().map(|x| as_f64(x).map(|n| n as f32)).collect();
        return enc.encode_field(&v);
    }
    if *ty == Type::FLOAT8_ARRAY {
        let v: Vec<Option<f64>> = items.iter().map(&as_f64).collect();
        return enc.encode_field(&v);
    }
    if *ty == Type::NUMERIC_ARRAY {
        let v: Vec<Option<PgNumeric>> = items.iter().map(&as_numeric).collect();
        return enc.encode_field(&v);
    }
    if *ty == Type::TEXT_ARRAY {
        let v: Vec<Option<String>> = items.iter().map(&as_text).collect();
        return enc.encode_field(&v);
    }
    // bytea[] / uuid[] / inet[] / cidr[] / json[] / jsonb[]: elements through `element_binary`,
    // the array framing hand-built (postgres_types has no ToSql for a
    // `Vec<Option<Vec<u8>>>` typed as any of them).
    // The same door serves a datetime / interval / range / multirange array
    // (`datetime_or_range_kind`), whose elements `element_binary` also knows.
    if let Some(elem) = match *ty {
        Type::BYTEA_ARRAY => Some(Type::BYTEA),
        Type::UUID_ARRAY => Some(Type::UUID),
        Type::INET_ARRAY => Some(Type::INET),
        Type::CIDR_ARRAY => Some(Type::CIDR),
        Type::JSON_ARRAY => Some(Type::JSON),
        Type::JSONB_ARRAY => Some(Type::JSONB),
        _ => match ty.kind() {
            postgres_types::Kind::Array(inner) if datetime_or_range_kind(inner) => {
                Some(inner.clone())
            }
            _ => None,
        },
    } {
        let binary = array_binary(items, &elem).ok_or_else(|| bad("this value"))?;
        let text = secantus_pgplan::value_text(v);
        return enc.encode_field(&RawField { binary, text });
    }
    Err(bad("this value"))
}

/// A timestamp / timestamptz value's binary form: the stored instant carrier
/// through `timestamp_bson_to_pg_micros`, or -- for a value kept as TEXT
/// (`infinity`, a BC or wide-year timestamp) -- the canonical text through
/// `timestamp_text_to_pg_micros`, which knows the wire sentinels and counts a
/// BC year back through the proleptic calendar as `timestamp_send` does.
fn timestamp_pg_micros(v: &Bson) -> Option<i64> {
    match v {
        Bson::String(s) => secantus_pgplan::timestamp_text_to_pg_micros(s),
        other => secantus_pgplan::timestamp_bson_to_pg_micros(other),
    }
}

/// A range's binary form (`range_send`): a flags byte, then each PRESENT
/// bound as `[i32 len][element binary]`. The flag bits are PostgreSQL's:
/// 0x01 empty, 0x02 lower inclusive, 0x04 upper inclusive, 0x08 lower
/// infinite, 0x10 upper infinite. An `infinity` timestamp / date bound is a
/// PRESENT bound holding the wire sentinel, not an infinite-bound flag --
/// measured on 16: `'[-infinity,infinity]'::tsrange` sends flags 0x06 with
/// two 8-byte bounds.
fn range_binary(text: &str, type_name: &str) -> Option<Vec<u8>> {
    let r = secantus_pgplan::range::parse_stored(text).ok()?;
    if r.empty {
        return Some(vec![0x01]);
    }
    let (element, _) = secantus_pgplan::range::range_element(type_name)?;
    let mut flags = 0u8;
    if r.lower_inc {
        flags |= 0x02;
    }
    if r.upper_inc {
        flags |= 0x04;
    }
    if r.lower.is_none() {
        flags |= 0x08;
    }
    if r.upper.is_none() {
        flags |= 0x10;
    }
    let mut out = vec![flags];
    for bound in [&r.lower, &r.upper].into_iter().flatten() {
        let bytes = range_bound_binary(bound, &element)?;
        out.extend_from_slice(&(i32::try_from(bytes.len()).ok()?).to_be_bytes());
        out.extend_from_slice(&bytes);
    }
    Some(out)
}

/// One stored range bound (the element's canonical text) in the element's
/// binary form. A timestamp bound is read as the naive UTC text it is stored
/// as -- a `tstzrange` keeps its bounds in UTC, so no session zone applies;
/// everything else is cast back to a value and encoded as a scalar would be.
fn range_bound_binary(text: &str, element: &str) -> Option<Vec<u8>> {
    match element {
        "timestamp" | "timestamptz" => Some(
            secantus_pgplan::timestamp_text_to_pg_micros(text)?
                .to_be_bytes()
                .to_vec(),
        ),
        "numeric" => numeric_binary(text),
        "int4" => Some(text.trim().parse::<i32>().ok()?.to_be_bytes().to_vec()),
        "int8" => Some(text.trim().parse::<i64>().ok()?.to_be_bytes().to_vec()),
        "date" => Some(
            secantus_pgplan::date_to_pg_days(text)?
                .to_be_bytes()
                .to_vec(),
        ),
        other => {
            let value =
                secantus_pgplan::cast_text_to(text, other, &secantus_pgplan::TimeZoneSetting::Utc)
                    .ok()?;
            element_binary(&value, &wire_type(other))
        }
    }
}

/// A multirange's binary form (`multirange_send`): an i32 member count, then
/// each member range as `[i32 len][range binary]`.
fn multirange_binary(text: &str, type_name: &str) -> Option<Vec<u8>> {
    let member = secantus_pgplan::range::multirange_member(type_name)?;
    let members = secantus_pgplan::range::split_members(text).ok()?;
    let mut out = (i32::try_from(members.len()).ok()?).to_be_bytes().to_vec();
    for m in &members {
        let bytes = range_binary(m, &member)?;
        out.extend_from_slice(&(i32::try_from(bytes.len()).ok()?).to_be_bytes());
        out.extend_from_slice(&bytes);
    }
    Some(out)
}

/// One value in whichever format the column was described in.
/// The same column, described in the requested wire format.
///
/// A server cursor's schema is built in TEXT at DECLARE; a BINARY `FETCH`
/// re-describes each column as binary (where the type has a binary encoding at
/// all -- otherwise it stays text, exactly as `field_mod` decides for a live
/// query).
fn rebind_field_format(field: &FieldInfo, binary: bool) -> FieldInfo {
    let format = if binary && binary_encodable(field.datatype()) {
        FieldFormat::Binary
    } else {
        FieldFormat::Text
    };
    FieldInfo::new(
        field.name().to_string(),
        None,
        None,
        field.datatype().clone(),
        format,
    )
    .with_type_size(field.type_size())
    .with_type_modifier(field.type_modifier())
    .with_name_raw(field.name_raw().cloned())
}

/// A column name's `RowDescription` bytes under the client encoding, or `None`
/// when they are the name's own UTF-8 (the UTF8 / passthrough encodings, and a
/// name the target encoding cannot represent).
fn transcoded_name(cenc: ClientEncoding, name: &str) -> Option<Bytes> {
    if !cenc.transcodes() || name.is_ascii() {
        return None;
    }
    encoding::encode(cenc, name.as_bytes())
        .ok()
        .map(Bytes::from)
}

/// Encode one captured cursor row against a (re-formatted) schema.
///
/// The values are the ones captured at DECLARE, so this reuses the very same
/// `encode_field_value` the live query path uses -- binary via `encode_binary`,
/// text otherwise -- and a BINARY fetch of a server cursor produces bytes
/// identical to a binary live query.
fn encode_typed_row(
    schema: &Arc<Vec<FieldInfo>>,
    values: &[Option<Bson>],
    tz: &secantus_pgplan::TimeZoneSetting,
    ds: &secantus_pgplan::DateStyle,
    cenc: ClientEncoding,
) -> PgWireResult<DataRow> {
    let mut enc = DataRowEncoder::new(schema.clone());
    for (i, field) in schema.iter().enumerate() {
        let v = values.get(i).and_then(|c| c.as_ref());
        encode_field_value(&mut enc, field, v, tz, ds, cenc)?;
    }
    Ok(enc.take_row())
}

/// Whether a field's rendered bytes may carry non-ASCII characters that need
/// transcoding to the client encoding.
///
/// In TEXT format every value is safe to transcode as one blob: the structural
/// bytes (array braces, separators, escapes) are all ASCII and unchanged across
/// LATIN1 / LATIN9, so only the character content moves. In BINARY format that
/// is true only for a scalar text-family value, whose whole payload IS the
/// string bytes -- json too, and jsonb, whose only non-text byte is the `1`
/// version prefix that a Latin transcode leaves alone; a binary array or record
/// interleaves big-endian length words that a blanket transcode would corrupt,
/// so those keep the internal UTF-8 bytes (correct for ASCII; non-ASCII in a
/// binary array under LATIN1 / LATIN9 is deferred -- see `tasks/backlog.md`).
fn field_may_carry_text(field: &FieldInfo) -> bool {
    match field.format() {
        FieldFormat::Text => true,
        FieldFormat::Binary => {
            let ty = field.datatype();
            matches!(
                *ty,
                Type::TEXT
                    | Type::VARCHAR
                    | Type::BPCHAR
                    | Type::NAME
                    | Type::CHAR
                    | Type::JSON
                    | Type::JSONB
            ) || matches!(ty.kind(), postgres_types::Kind::Enum(_))
        }
    }
}

/// The single field written into a fresh `DataRowEncoder`, split back into
/// `None` (SQL NULL) or its raw payload bytes. The buffer is `[i32-BE len]` then
/// `len` payload bytes, with `len == -1` for NULL.
fn split_single_field(row: &DataRow) -> Option<Vec<u8>> {
    let data = &row.data;
    if data.len() < 4 {
        return None;
    }
    let len = i32::from_be_bytes(data[..4].try_into().expect("4 bytes"));
    if len < 0 {
        return None;
    }
    Some(data[4..4 + len as usize].to_vec())
}

/// Encode one live-query field, transcoding to the client encoding when needed.
fn encode_field_value(
    enc: &mut DataRowEncoder,
    field: &FieldInfo,
    v: Option<&Bson>,
    tz: &secantus_pgplan::TimeZoneSetting,
    ds: &secantus_pgplan::DateStyle,
    cenc: ClientEncoding,
) -> PgWireResult<()> {
    transcoding_field(enc, field, cenc, |tmp| {
        encode_field_value_inner(tmp, field, v, tz, ds)
    })
}

/// Encode one field with `encode_once`, transcoding its rendered bytes to the
/// client encoding when that encoding differs from the internal UTF-8 form and
/// the field can carry text.
///
/// For UTF8 / SQL_ASCII / any accepted-but-untranscoded encoding this is
/// byte-for-byte `encode_once(enc)` -- the fast path is untouched. For LATIN1 /
/// LATIN9 the value is rendered once into a throwaway single-column encoder, its
/// payload transcoded, and the result re-emitted; a character with no
/// representation in the target encoding becomes PostgreSQL's `22P05`
/// untranslatable-character error. Shared by the live-query encoder and the
/// COPY-OUT binary path, whose text-family fields carry the same character
/// bytes.
fn transcoding_field<F>(
    enc: &mut DataRowEncoder,
    field: &FieldInfo,
    cenc: ClientEncoding,
    encode_once: F,
) -> PgWireResult<()>
where
    F: FnOnce(&mut DataRowEncoder) -> PgWireResult<()>,
{
    if !cenc.transcodes() || !field_may_carry_text(field) {
        return encode_once(enc);
    }
    let schema = Arc::new(vec![field.clone()]);
    let mut tmp = DataRowEncoder::new(schema);
    encode_once(&mut tmp)?;
    let row = tmp.take_row();
    match split_single_field(&row) {
        None => enc.encode_field(&None::<&str>),
        Some(utf8) => {
            let bytes =
                encoding::encode(cenc, &utf8).map_err(|ch| untranslatable_char(ch, cenc))?;
            enc.encode_field_with_type_and_format(
                &RawEncoded(bytes),
                field.datatype(),
                field.format(),
                &FormatOptions::default(),
            )
        }
    }
}

/// PostgreSQL's `22P05`: a character the server holds in UTF-8 has no
/// representation in the client encoding.
fn untranslatable_char(ch: char, cenc: ClientEncoding) -> PgWireError {
    let hex: Vec<String> = ch
        .to_string()
        .as_bytes()
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect();
    let target = match cenc {
        ClientEncoding::Latin1 => "LATIN1",
        ClientEncoding::Latin9 => "LATIN9",
        // Only the transcoding encodings ever reach here.
        _ => "UTF8",
    };
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        "22P05".into(), // untranslatable_character
        format!(
            "character with byte sequence {} in encoding \"UTF8\" has no equivalent in encoding \"{target}\"",
            hex.join(" "),
        ),
    )))
}

fn encode_field_value_inner(
    enc: &mut DataRowEncoder,
    field: &FieldInfo,
    v: Option<&Bson>,
    tz: &secantus_pgplan::TimeZoneSetting,
    ds: &secantus_pgplan::DateStyle,
) -> PgWireResult<()> {
    if field.format() == FieldFormat::Binary {
        // Binary datetime output is DateStyle-INDEPENDENT (it is a fixed-width
        // integer, not text), so `ds` is deliberately unused on this path.
        return encode_binary(enc, field.datatype(), v);
    }
    // An inet/cidr COLUMN in TEXT format uses inet_out/cidr_out: inet drops a
    // full-host mask (`/32`, `/128`), cidr keeps it. (A `::text` cast is type
    // `text`, not 869/650, so it never reaches here -- it keeps its mask.)
    if matches!(field.datatype().oid(), 869 | 650) {
        if let Some(Bson::String(text)) = v {
            let out = secantus_pgplan::net::text_out(text, field.datatype().oid() == 650);
            return enc.encode_field(&Some(out));
        }
    }
    // A COMPOSITE / anonymous-RECORD ARRAY in TEXT format: render each element
    // as its composite `(...)` text and let the text-array encoder escape it
    // once. The element oid is a user oid `element_of_array_oid` does not know,
    // so the generic array branch below misses it and the catch-all renders the
    // whole `Bson::Array` through a second escaping pass (double-escaped).
    if let (Some(Bson::Array(items)), postgres_types::Kind::Array(inner)) =
        (v, field.datatype().kind())
    {
        if *inner == Type::RECORD || matches!(inner.kind(), postgres_types::Kind::Composite(_)) {
            let rendered: Vec<Option<String>> = items
                .iter()
                .map(|x| match x {
                    Bson::Null => None,
                    other => Some(secantus_pgplan::value_text(other)),
                })
                .collect();
            return enc.encode_field(&rendered);
        }
    }
    // An ARRAY goes through the typed encoder in text too, because the element
    // conversion has to come from the COLUMN's type rather than from the first
    // element's: `array[1, 1.5]` is a `numeric[]` whose first element is an
    // integer, and reading the type off that element turned the decimal into a
    // NULL.
    if let (Some(Bson::Array(items)), Some(element)) =
        (v, element_of_array_oid(field.datatype().oid()))
    {
        // A `float8[]` in text is `float8out` per element (`{1e+20,2}`),
        // which the typed encoder's ryu rendering (`{1e20,2.0}`) is not.
        if element == "float8" && !items.iter().any(|x| matches!(x, Bson::Array(_))) {
            let rendered: Vec<Option<String>> = items
                .iter()
                .map(|x| match x {
                    Bson::Null => None,
                    other => Some(secantus_pgplan::value_text(other)),
                })
                .collect();
            return enc.encode_field(&rendered);
        }
        // An `inet[]` / `cidr[]` in text is `inet_out` per element: a host
        // address drops its full mask (`{::1}`, not `{::1/128}`), same as the
        // scalar arm above.
        if matches!(element, "inet" | "cidr") {
            let rendered: Vec<Option<String>> = items
                .iter()
                .map(|x| match x {
                    Bson::Null => None,
                    Bson::String(t) => Some(secantus_pgplan::net::text_out(t, element == "cidr")),
                    other => Some(secantus_pgplan::value_text(other)),
                })
                .collect();
            return enc.encode_field(&rendered);
        }
        // A datetime / range array in TEXT keeps the per-element text
        // rendering below (a timestamptz / tstzrange element in the SESSION
        // zone); everything else the typed encoder knows goes through it.
        if binary_encodable(field.datatype()) && !datetime_or_range_kind(field.datatype()) {
            return encode_binary(enc, field.datatype(), v);
        }
        // A `timestamptz[]` / `tstzrange[]` / `tstzmultirange[]` element is a
        // stored UTC instant (or UTC bounds); PostgreSQL renders each in the
        // session zone with its offset: `{"2020-01-01 01:00:00+01"}`.
        if matches!(element, "timestamptz" | "tstzrange" | "tstzmultirange") {
            let rendered: Vec<Option<String>> = items
                .iter()
                .map(|x| match x {
                    Bson::Null => None,
                    other => Some(session_zone_text(other, element, tz, ds)),
                })
                .collect();
            return enc.encode_field(&rendered);
        }
        // A date, timestamp or interval array: those are carried as canonical
        // TEXT here, so the elements go out as the strings they already are.
        // A `box[]` joins its elements with `;`, which the element-wise
        // encoder cannot do; its whole text is rendered here instead.
        // Sent as plain TEXT: the `&str` encoder quotes a value that holds
        // braces when the field is array-typed, and this is the whole array.
        if element == "box" {
            let text = secantus_pgplan::value_text(&Bson::Array(items.clone()));
            let options = field.format_options().clone();
            return enc.encode_field_with_type_and_format(
                &Some(text),
                &Type::TEXT,
                field.format(),
                options.as_ref(),
            );
        }
        let rendered: Vec<Option<String>> = items
            .iter()
            .map(|x| match x {
                Bson::Null => None,
                other => Some(secantus_pgplan::value_text(other)),
            })
            .collect();
        return enc.encode_field(&rendered);
    }
    // A timestamptz value is a stored UTC INSTANT; it renders in the SESSION
    // zone, which the type-blind `encode_value` (naive `timestamp_value_text`)
    // cannot do -- so render it here, where the FIELD type is known. A special
    // value (infinity / wide / BC) arrives as a String and goes out verbatim.
    if *field.datatype() == Type::TIMESTAMPTZ {
        match v {
            None | Some(Bson::Null) => return enc.encode_field(&None::<&str>),
            // A special value (infinity / wide / BC) arrives as text. Under a
            // non-ISO DateStyle psycopg's timestamptz loader refuses to parse
            // ANY value (it cannot read zone names) and raises
            // `NotImplementedError`, so the exact text here is not parsed back;
            // restyle the shape it recognises and pass the rest through.
            Some(Bson::String(s)) => {
                let out = secantus_pgplan::render_timestamp_styled(s, ds);
                return enc.encode_field(&Some(out.as_str()));
            }
            Some(value) => {
                if let Some(text) = secantus_pgplan::timestamptz_value_text_styled(value, tz, ds) {
                    return enc.encode_field(&Some(text.as_str()));
                }
            }
        }
    }
    // A tstzrange / tstzmultirange is stored with its bounds as naive UTC
    // text; PostgreSQL renders each bound in the session zone with its offset
    // (`["2020-01-01 01:00:00+01","2020-06-01 12:00:00+02")` under
    // Europe/Rome). A bound that will not parse goes out as stored.
    if matches!(*field.datatype(), Type::TSTZ_RANGE | Type::TSTZMULTI_RANGE) {
        if let Some(Bson::String(text)) = v {
            let name = if *field.datatype() == Type::TSTZ_RANGE {
                "tstzrange"
            } else {
                "tstzmultirange"
            };
            let out = session_zone_text(&Bson::String(text.clone()), name, tz, ds);
            return enc.encode_field(&Some(out.as_str()));
        }
    }
    // A DATE is stored as its canonical ISO text; restyle it for the session
    // DateStyle. ISO passes through unchanged.
    if *field.datatype() == Type::DATE {
        if let Some(Bson::String(s)) = v {
            let out = secantus_pgplan::render_date_styled(s, ds);
            return enc.encode_field(&Some(out.as_str()));
        }
    }
    // A TIMESTAMP (without zone) is a stored instant (or a special text value);
    // restyle it for the session DateStyle. ISO passes through unchanged.
    if *field.datatype() == Type::TIMESTAMP {
        if let Some(value) = v {
            if let Some(text) = secantus_pgplan::timestamp_value_text_styled(value, ds) {
                return enc.encode_field(&Some(text.as_str()));
            }
        }
    }
    encode_value(enc, v)
}

/// The TEXT rendering of one stored timestamptz / tstzrange / tstzmultirange
/// value in the session zone. A value this cannot re-render (a bound that
/// will not parse) goes out as the text it is stored as.
fn session_zone_text(
    v: &Bson,
    type_name: &str,
    tz: &secantus_pgplan::TimeZoneSetting,
    ds: &secantus_pgplan::DateStyle,
) -> String {
    match (type_name, v) {
        ("tstzrange", Bson::String(text)) => {
            secantus_pgplan::range::render_in_zone(text, tz).unwrap_or_else(|_| text.clone())
        }
        ("tstzmultirange", Bson::String(text)) => {
            secantus_pgplan::range::render_multirange_in_zone(text, tz)
                .unwrap_or_else(|_| text.clone())
        }
        ("timestamptz", Bson::String(text)) => secantus_pgplan::render_timestamp_styled(text, ds),
        ("timestamptz", value) => secantus_pgplan::timestamptz_value_text_styled(value, tz, ds)
            .unwrap_or_else(|| secantus_pgplan::value_text(value)),
        (_, value) => secantus_pgplan::value_text(value),
    }
}

fn encode_value(enc: &mut DataRowEncoder, v: Option<&Bson>) -> PgWireResult<()> {
    // A timestamp CONSTANT never passes through a row, so it arrives here as a
    // BSON date or as the sub-millisecond composite rather than as something
    // `timestamp_text` reassembled. Without this arm it fell through to the
    // catch-all and `select '2026-01-01 12:00'::timestamp` answered NULL.
    if let Some(value) = v {
        if let Some(text) = secantus_pgplan::timestamp_value_text(value) {
            return enc.encode_field(&Some(text.as_str()));
        }
        // An interval is three parts in a document; the wire wants its text.
        if let Some(text) = secantus_pgplan::interval_value_text(value) {
            return enc.encode_field(&Some(text.as_str()));
        }
        // A regtype is an oid in a document; the wire wants its display name.
        if let Some(oid) = secantus_pgplan::regtype_oid(value) {
            return enc.encode_field(&Some(secantus_pgplan::regtype_text(oid).as_str()));
        }
        // A record is a tagged field list; the wire wants its `(...)` text.
        if let Some(text) = secantus_pgplan::record_value_text(value) {
            return enc.encode_field(&Some(text.as_str()));
        }
        // A box is four corners in a document; the wire wants `(h),(l)`.
        if let Some(coords) = secantus_pgplan::geo::box_coords(value) {
            return enc.encode_field(&Some(secantus_pgplan::geo::box_text(&coords).as_str()));
        }
    }
    match v {
        Some(Bson::Int32(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::Int64(x)) => enc.encode_field(&Some(*x)),
        // `float8out`, not Rust's `Display`: `1e+20` and `Infinity`, where
        // Rust writes `1e20` and `inf` -- text a client's float parser may
        // still read, but not what PostgreSQL sends.
        Some(Bson::Double(x)) => {
            enc.encode_field(&Some(secantus_pgplan::geo::float8_text(*x).as_str()))
        }
        Some(Bson::Boolean(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::String(x)) => enc.encode_field(&Some(x.as_str())),
        // Decimal128's rendering already carries the scale (`1.50`, not `1.5`),
        // which is part of a PostgreSQL `numeric` value rather than formatting.
        Some(Bson::Decimal128(x)) => enc.encode_field(&Some(
            secantus_pgplan::plain_numeric_text(&x.to_string()).as_str(),
        )),
        // A numeric wider than Decimal128 is stored as its canonical text
        // inside a marker document (`secantus_pgplan::numeric`).
        Some(v) if secantus_pgplan::is_wide_numeric(v) => enc.encode_field(&Some(
            secantus_pgplan::numeric_text(v)
                .unwrap_or_default()
                .as_str(),
        )),
        // An array must be handed over as a TYPED vector, not as pre-rendered
        // text: `encode_field` encodes against the column's declared type, so
        // giving it a `&str` for an `int4[]` field wraps the whole literal as a
        // single element (`{1,2,3}` came out as `"{1,2,3}"`).
        // A nested array is REFUSED rather than flattened. rust-postgres
        // encodes only one dimension, so the typed path below silently turned
        // `{{1,2},{3,4}}` into two elements whose text was `{1,2}` and
        // `{3,4}` -- a wrong answer that a client cannot tell from a real one.
        // Guessing the client's format code to smuggle the literal through as
        // text would be the same trade in a less visible place.
        // A multidimensional array in the TEXT format: `value_text` already
        // renders the nesting as `{{1,2},{3,4}}`, which the client parses.
        Some(Bson::Array(items)) if items.iter().any(|x| matches!(x, Bson::Array(_))) => {
            let text = secantus_pgplan::value_text(&Bson::Array(items.clone()));
            enc.encode_field(&Some(text))
        }
        Some(Bson::Array(items)) => match items.first() {
            Some(Bson::Int32(_)) => {
                let v: Vec<Option<i32>> = items.iter().map(|x| x.as_i32()).collect();
                enc.encode_field(&v)
            }
            Some(Bson::Int64(_)) => {
                let v: Vec<Option<i64>> = items.iter().map(|x| x.as_i64()).collect();
                enc.encode_field(&v)
            }
            // As `float8out` text, not pgwire's ryu (`1e20`, `inf`); a float's
            // text never needs the array quoting, so strings are safe here.
            Some(Bson::Double(_)) => {
                let v: Vec<Option<String>> = items
                    .iter()
                    .map(|x| x.as_f64().map(secantus_pgplan::geo::float8_text))
                    .collect();
                enc.encode_field(&v)
            }
            Some(Bson::Boolean(_)) => {
                let v: Vec<Option<bool>> = items.iter().map(|x| x.as_bool()).collect();
                enc.encode_field(&v)
            }
            // Text, decimals and an EMPTY array all go as strings; an empty
            // one has no element to read a type from and renders as `{}`
            // whatever the column type is.
            _ => {
                let v: Vec<Option<String>> = items
                    .iter()
                    .map(|x| match x {
                        Bson::Null => None,
                        Bson::String(t) => Some(t.clone()),
                        other => Some(render_array_element_text(other)),
                    })
                    .collect();
                enc.encode_field(&v)
            }
        },
        // A `bytea` renders as its `\x…` hex text in the text format.
        Some(Bson::Binary(b)) => {
            enc.encode_field(&Some(secantus_pgplan::bytea::render_hex(&b.bytes).as_str()))
        }
        _ => enc.encode_field(&None::<i32>),
    }
}

/// The wire type an aggregate's result carries.
///
/// Probed against PostgreSQL 14: `count(*)` and `count(col)` are int8 (oid 20),
/// `sum(int4)` is **int8**, not int4, and `min`/`max` return the INPUT type.
fn aggregate_wire_type(item: &AggItem) -> Type {
    match item.func {
        AggFunc::CountStar | AggFunc::Count => Type::INT8,
        AggFunc::Sum => match item.source_type.as_deref() {
            Some("numeric" | "decimal") => Type::NUMERIC,
            _ => Type::INT8,
        },
        AggFunc::Min | AggFunc::Max => item
            .source_type
            .as_deref()
            .map(wire_type)
            .unwrap_or(Type::VARCHAR),
        AggFunc::ArrayAgg => item
            .source_type
            .as_deref()
            .map(|t| wire_type(&format!("{t}[]")))
            .unwrap_or(Type::TEXT_ARRAY),
    }
}

/// One aggregate over one group.
///
/// PostgreSQL's NULL rules, probed on 14: `count(*)` counts ROWS; every other
/// aggregate SKIPS NULLs; and over an empty input `count` is 0 while `sum`,
/// `min` and `max` are **NULL, not zero**.
fn compute_aggregate(item: &AggItem, rows: &[Document]) -> Bson {
    let Some(field) = item.field.as_deref() else {
        // count(*)
        return Bson::Int64(rows.len() as i64);
    };
    let values: Vec<&Bson> = rows
        .iter()
        .filter_map(|d| match d.get(field) {
            None | Some(Bson::Null) => None,
            Some(v) => Some(v),
        })
        .collect();

    match item.func {
        AggFunc::CountStar => Bson::Int64(rows.len() as i64),
        AggFunc::Count => Bson::Int64(values.len() as i64),
        // Group order, NULLs INCLUDED -- a LEFT-JOIN miss surfaces as `[None]`
        // rather than `[]`, which is what psycopg's EnumInfo distinguishes a
        // non-enum by.
        AggFunc::ArrayAgg => Bson::Array(
            rows.iter()
                .map(|d| d.get(field).cloned().unwrap_or(Bson::Null))
                .collect(),
        ),
        AggFunc::Sum => {
            if values.is_empty() {
                return Bson::Null;
            }
            // Integer inputs sum as int8; a numeric anywhere sums EXACTLY as
            // a numeric (PostgreSQL's `sum(numeric)` keeps the widest input
            // scale); a float anywhere makes it a double.
            if values.iter().any(|v| secantus_pgplan::is_numeric(v))
                && !values.iter().any(|v| matches!(v, Bson::Double(_)))
            {
                let texts: Vec<String> = values
                    .iter()
                    .filter_map(|v| secantus_pgplan::numeric::numeric_operand_text(v))
                    .collect();
                if texts.len() == values.len() {
                    if let Some(total) = secantus_pgplan::numeric::sum_numeric_texts(
                        texts.iter().map(String::as_str),
                    ) {
                        return total;
                    }
                }
            }
            if values
                .iter()
                .all(|v| matches!(v, Bson::Int32(_) | Bson::Int64(_)))
            {
                let total: i64 = values
                    .iter()
                    .map(|v| match v {
                        Bson::Int32(x) => i64::from(*x),
                        Bson::Int64(x) => *x,
                        _ => 0,
                    })
                    .sum();
                Bson::Int64(total)
            } else {
                let total: f64 = values
                    .iter()
                    .map(|v| match v {
                        Bson::Int32(x) => f64::from(*x),
                        Bson::Int64(x) => *x as f64,
                        Bson::Double(x) => *x,
                        _ => 0.0,
                    })
                    .sum();
                Bson::Double(total)
            }
        }
        AggFunc::Min | AggFunc::Max => {
            let mut best: Option<&Bson> = None;
            for v in values {
                best = Some(match best {
                    None => v,
                    Some(cur) => {
                        let cmp = compare_values(v, cur);
                        let take = if item.func == AggFunc::Min {
                            cmp == Ordering::Less
                        } else {
                            cmp == Ordering::Greater
                        };
                        if take {
                            v
                        } else {
                            cur
                        }
                    }
                });
            }
            best.cloned().unwrap_or(Bson::Null)
        }
    }
}

/// Sort decoded rows in PostgreSQL's order.
///
/// Deliberately NOT pushed into the storage layer's sort: MongoDB orders null
/// LOW, while PostgreSQL puts NULLs LAST on ASC and FIRST on DESC (probed 14).
/// Pushing an ASC sort down would silently reorder every nullable column.
fn sort_rows(docs: &mut [Document], order: &[OrderKey]) {
    docs.sort_by(|a, b| {
        for key in order {
            let (l, r) = (a.get(&key.field), b.get(&key.field));
            let l_null = matches!(l, None | Some(Bson::Null));
            let r_null = matches!(r, None | Some(Bson::Null));
            let ord = match (l_null, r_null) {
                (true, true) => Ordering::Equal,
                (true, false) => match key.nulls {
                    Nulls::First => Ordering::Less,
                    Nulls::Last => Ordering::Greater,
                },
                (false, true) => match key.nulls {
                    Nulls::First => Ordering::Greater,
                    Nulls::Last => Ordering::Less,
                },
                (false, false) => {
                    let cmp = compare_values(l.unwrap(), r.unwrap());
                    if key.ascending {
                        cmp
                    } else {
                        cmp.reverse()
                    }
                }
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
}

/// Compare two non-null stored values the way PostgreSQL compares the SQL types
/// this slice supports.
///
/// This is the planner's `compare_constants`, which already knows every
/// stored shape -- ints, doubles, Decimal128, the wide-numeric document,
/// strings, bytea, arrays. The local float-only version it replaced treated
/// a `numeric` (Decimal128) as "unsupported" and returned `Equal`, so
/// `ORDER BY` on a numeric column was a no-op.
fn compare_values(a: &Bson, b: &Bson) -> Ordering {
    // A pair no SQL column can hold is kept stable rather than arbitrary.
    secantus_pgplan::compare_values(a, b).unwrap_or(Ordering::Equal)
}

/// A parsed statement: the SQL text plus whatever parameter types the client
/// declared in `Parse`.
///
/// The text is kept rather than a plan, because the extended protocol binds
/// values AFTER parsing and the plan depends on them (`WHERE n = $1` lowers to
/// a different filter for a NULL than for a 5). Re-planning per Bind keeps one
/// set of NULL rules instead of two.
#[derive(Debug, Clone)]
pub struct ParsedStatement {
    pub sql: String,
    pub declared_types: Vec<Type>,
}

pub struct SqlParser;

#[async_trait]
impl QueryParser for SqlParser {
    type Statement = ParsedStatement;

    async fn parse_sql<C>(
        &self,
        _c: &C,
        sql: &str,
        types: &[Option<Type>],
    ) -> PgWireResult<Self::Statement>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        Ok(ParsedStatement {
            sql: sql.to_string(),
            // 0.40 reports an unspecified parameter as `None` rather than a
            // zero oid; both mean "the server decides", so flatten them.
            declared_types: types.iter().flatten().cloned().collect(),
        })
    }

    // These two exist so `ExtendedQueryHandler` can AUTO-implement Describe.
    // We override `do_describe_statement` / `do_describe_portal` explicitly --
    // resolving a result schema needs the catalog, which the parser has no
    // access to -- so the auto path is never taken and these stay empty.
    fn get_parameter_types(&self, stmt: &Self::Statement) -> PgWireResult<Vec<Type>> {
        Ok(stmt.declared_types.clone())
    }

    fn get_result_schema(
        &self,
        _stmt: &Self::Statement,
        _column_format: Option<&Format>,
    ) -> PgWireResult<Vec<FieldInfo>> {
        Ok(Vec::new())
    }
}

/// A PostgreSQL binary `numeric` as its exact decimal text.
///
/// Wire shape: `ndigits`, `weight`, `sign`, `dscale`, then `ndigits` base-10000
/// groups. The value is `sum(digits[i] * 10000^(weight - i))`, and `dscale` is
/// how many digits after the point to SHOW — which is part of the value here,
/// since `1.50` and `1.5` are different numerics.
///
/// Rendered back to text rather than computed into a float: a numeric carries
/// more digits than an f64 can hold, and the text path already knows how to
/// turn this into an exact value.
fn binary_numeric_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 8 {
        return None;
    }
    let be16 = |i: usize| i16::from_be_bytes([bytes[i], bytes[i + 1]]);
    let ndigits = be16(0);
    let weight = i32::from(be16(2));
    let sign = u16::from_be_bytes([bytes[4], bytes[5]]);
    let dscale = usize::from(u16::from_be_bytes([bytes[6], bytes[7]]));
    // 0xC000 is NaN; the infinities (0xD000 / 0xF000) arrived with PG 14.
    match sign {
        0xC000 => return Some("NaN".to_string()),
        0xD000 => return Some("Infinity".to_string()),
        0xF000 => return Some("-Infinity".to_string()),
        _ => {}
    }
    if ndigits < 0 || bytes.len() < 8 + (ndigits as usize) * 2 {
        return None;
    }
    let digits: Vec<i16> = (0..ndigits as usize).map(|i| be16(8 + i * 2)).collect();

    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    if weight < 0 {
        out.push('0');
    } else {
        for i in 0..=weight as usize {
            let d = digits.get(i).copied().unwrap_or(0);
            if i == 0 {
                out.push_str(&d.to_string());
            } else {
                out.push_str(&format!("{d:04}"));
            }
        }
    }
    if dscale > 0 {
        out.push('.');
        let mut frac = String::new();
        let mut group = 1i32;
        while frac.len() < dscale {
            let idx = weight + group;
            let d = if idx >= 0 {
                digits.get(idx as usize).copied().unwrap_or(0)
            } else {
                0
            };
            frac.push_str(&format!("{d:04}"));
            group += 1;
        }
        frac.truncate(dscale);
        out.push_str(&frac);
    }
    Some(out)
}

/// One element of a PostgreSQL binary array, plus the reader position.
///
/// Wire shape: `ndim`, `has_null`, `element oid`, then per dimension a length
/// and a lower bound, then each element as a 4-byte length (-1 for NULL)
/// followed by that many bytes in the ELEMENT's binary format.
fn binary_array(
    bytes: &[u8],
    tz: &secantus_pgplan::TimeZoneSetting,
    cenc: ClientEncoding,
) -> PgWireResult<Bson> {
    if bytes.len() < 12 {
        return Err(unsupported_binary_oid(None));
    }
    let be32 = |i: usize| i32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let ndim = be32(0);
    let elem_oid = be32(8) as u32;
    if ndim == 0 {
        return Ok(Bson::Array(Vec::new()));
    }
    let ndim = usize::try_from(ndim).map_err(|_| unsupported_binary_oid(None))?;
    if bytes.len() < 12 + 8 * ndim {
        return Err(unsupported_binary_oid(None));
    }
    // Each dimension's length; the lower bound beside it is dropped (the
    // parsed value carries none, as with the text form).
    let dims: Vec<usize> = (0..ndim)
        .map(|d| usize::try_from(be32(12 + 8 * d).max(0)).unwrap_or(0))
        .collect();
    let count: usize = dims.iter().product();
    let mut pos = 12 + 8 * ndim;
    let mut flat = Vec::with_capacity(count);
    let ty = Type::from_oid(elem_oid);
    for _ in 0..count {
        if pos + 4 > bytes.len() {
            return Err(unsupported_binary_oid(None));
        }
        let len = be32(pos);
        pos += 4;
        if len < 0 {
            flat.push(Bson::Null);
            continue;
        }
        let end = pos + len as usize;
        if end > bytes.len() {
            return Err(unsupported_binary_oid(None));
        }
        let elem = Bytes::copy_from_slice(&bytes[pos..end]);
        flat.push(decode_parameter(Some(&elem), ty.as_ref(), true, tz, cenc)?);
        pos = end;
    }
    // Reshape the row-major leaves into nested arrays, innermost last.
    let mut level = flat;
    for &d in dims.iter().skip(1).rev() {
        if d == 0 {
            return Ok(Bson::Array(Vec::new()));
        }
        level = level
            .chunks(d)
            .map(|chunk| Bson::Array(chunk.to_vec()))
            .collect();
    }
    Ok(Bson::Array(level))
}

fn unsupported_binary_oid(oid: Option<u32>) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        "0A000".into(),
        format!("binary parameters of type oid {oid:?} are not supported yet"),
    )))
}

/// A pre-serialized field carrying BOTH wire forms of a value `postgres_types`
/// has no `ToSql` for -- a MULTIDIMENSIONAL array. A binary-encodable array
/// column reaches `encode_binary` in EITHER format (psycopg reads arrays as
/// text by default, binary on request), so both are built up front and
/// `encode_field` picks the one the field was described in.
#[derive(Debug)]
struct RawField {
    binary: Vec<u8>,
    text: String,
}

impl ToSql for RawField {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        out.put_slice(&self.binary);
        Ok(IsNull::No)
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

impl ToSqlText for RawField {
    fn to_sql_text(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
        _options: &FormatOptions,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        // Write the array text (`{{1,2},{3,4}}`) verbatim -- `str`'s own
        // ToSqlText re-quotes it against the array type and corrupts it.
        out.put_slice(self.text.as_bytes());
        Ok(IsNull::No)
    }
}

/// Raw bytes of ONE scalar element in `elem`'s binary wire format (no length
/// prefix). Mirrors the single-value arms of `encode_binary`; returns `None`
/// for an element type whose binary layout this server does not emit.
fn element_binary(v: &Bson, elem: &Type) -> Option<Vec<u8>> {
    // A COMPOSITE or anonymous RECORD element: its own binary record format.
    // (Checked before the oid match because a user composite's oid is not one
    // of the built-in codes below.)
    if *elem == Type::RECORD || matches!(elem.kind(), postgres_types::Kind::Composite(_)) {
        let fields = secantus_pgplan::record_field_values(v)?;
        let field_types: Vec<Type> = match elem.kind() {
            postgres_types::Kind::Composite(fs) => fs.iter().map(|f| f.type_().clone()).collect(),
            // A `ROW(...)` record carries its fields' STATIC types (a bare
            // literal is `unknown`, 705); one built any other way is typed
            // from its values.
            _ => match secantus_pgplan::record_field_types(v) {
                Some(names) => names
                    .iter()
                    .map(|n| match n.as_str() {
                        "unknown" => Type::UNKNOWN,
                        other => wire_type(other),
                    })
                    .collect(),
                None => fields.iter().map(record_field_type).collect(),
            },
        };
        return record_binary(fields, &field_types);
    }
    // A user ENUM element: its label's bytes, as the scalar arm sends them.
    if matches!(elem.kind(), postgres_types::Kind::Enum(_)) {
        return match v {
            Bson::String(x) => Some(x.clone().into_bytes()),
            _ => None,
        };
    }
    // A range or multirange, builtin (by oid) or user-defined (by the name the
    // registry minted its oid from): stored as canonical text, encoded from it.
    let range_name = secantus_pgplan::range::range_oid_name(elem.oid())
        .map(str::to_string)
        .or_else(|| secantus_pgplan::range::multirange_oid_name(elem.oid()).map(str::to_string))
        .or_else(|| {
            matches!(
                elem.kind(),
                postgres_types::Kind::Range(_) | postgres_types::Kind::Multirange(_)
            )
            .then(|| secantus_pgplan::user_type_name(i64::from(elem.oid())))
            .flatten()
        });
    if let Some(name) = range_name {
        let Bson::String(text) = v else {
            return None;
        };
        return if secantus_pgplan::range::is_multirange_type(&name) {
            multirange_binary(text, &name)
        } else {
            range_binary(text, &name)
        };
    }
    let int = |v: &Bson| -> Option<i64> {
        match v {
            Bson::Int32(x) => Some(i64::from(*x)),
            Bson::Int64(x) => Some(*x),
            Bson::Double(x) if x.fract() == 0.0 => Some(*x as i64),
            _ => None,
        }
    };
    let float = |v: &Bson| -> Option<f64> {
        match v {
            Bson::Int32(x) => Some(f64::from(*x)),
            Bson::Int64(x) => Some(*x as f64),
            Bson::Double(x) => Some(*x),
            v if secantus_pgplan::is_numeric(v) => {
                secantus_pgplan::numeric::numeric_text_to_f64(&secantus_pgplan::numeric_text(v)?)
            }
            _ => None,
        }
    };
    match elem.oid() {
        16 => match v {
            Bson::Boolean(b) => Some(vec![u8::from(*b)]),
            _ => None,
        },
        21 => Some(i16::try_from(int(v)?).ok()?.to_be_bytes().to_vec()),
        23 => Some(i32::try_from(int(v)?).ok()?.to_be_bytes().to_vec()),
        20 => Some(int(v)?.to_be_bytes().to_vec()),
        26 => Some(u32::try_from(int(v)?).ok()?.to_be_bytes().to_vec()),
        2950 => match v {
            Bson::String(t) => secantus_pgplan::uuid_to_wire(t),
            _ => None,
        },
        869 | 650 => match v {
            Bson::String(t) => secantus_pgplan::net::to_wire(t, elem.oid() == 650),
            _ => None,
        },
        700 => Some((float(v)? as f32).to_be_bytes().to_vec()),
        701 => Some(float(v)?.to_be_bytes().to_vec()),
        1700 => {
            let text = match v {
                v if secantus_pgplan::is_numeric(v) => secantus_pgplan::numeric_text(v)?,
                Bson::Int32(x) => x.to_string(),
                Bson::Int64(x) => x.to_string(),
                Bson::Double(x) => x.to_string(),
                _ => return None,
            };
            numeric_binary(&text)
        }
        // text family: the value's UTF-8, verbatim. Oid 705 (`unknown`) is the
        // type of an untyped string literal inside a record; its binary form is
        // the raw bytes, exactly like text.
        25 | 1043 | 1042 | 19 | 18 | 705 => match v {
            Bson::String(x) => Some(x.clone().into_bytes()),
            _ => None,
        },
        // bytea: the raw bytes verbatim.
        17 => match v {
            Bson::Binary(b) => Some(b.bytes.clone()),
            _ => None,
        },
        // json / jsonb: see `json_binary`.
        114 | 3802 => match v {
            Bson::String(x) => Some(json_binary(x, elem.oid() == 3802)),
            _ => None,
        },
        // date: i32 days since 2000-01-01 (`date_send`).
        1082 => match v {
            Bson::String(t) => Some(secantus_pgplan::date_to_pg_days(t)?.to_be_bytes().to_vec()),
            _ => None,
        },
        // time: i64 microseconds since midnight (`time_send`).
        1083 => match v {
            Bson::String(t) => Some(
                secantus_pgplan::time_to_pg_micros(t)?
                    .to_be_bytes()
                    .to_vec(),
            ),
            _ => None,
        },
        // timetz: i64 microseconds since midnight, then the zone as i32
        // seconds WEST of UTC (`timetz_send`; `+05:30` is sent as -19800).
        1266 => match v {
            Bson::String(t) => {
                let (micros, west) = secantus_pgplan::timetz_to_pg_wire(t)?;
                let mut out = micros.to_be_bytes().to_vec();
                out.extend_from_slice(&west.to_be_bytes());
                Some(out)
            }
            _ => None,
        },
        // timestamp / timestamptz: i64 microseconds since 2000-01-01 UTC.
        1114 | 1184 => Some(timestamp_pg_micros(v)?.to_be_bytes().to_vec()),
        // interval: i64 microseconds, i32 days, i32 months (`interval_send`).
        1186 => {
            let iv = secantus_pgplan::Interval::from_bson(v)?;
            let mut out = iv.micros.to_be_bytes().to_vec();
            out.extend_from_slice(&iv.days.to_be_bytes());
            out.extend_from_slice(&iv.months.to_be_bytes());
            Some(out)
        }
        _ => None,
    }
}

/// The binary wire form of a json (`text` verbatim) or jsonb (a `1` version
/// byte, then the text) value -- PostgreSQL 16's `json_send` / `jsonb_send`.
fn json_binary(text: &str, jsonb: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 1);
    if jsonb {
        out.push(1);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

/// The PostgreSQL binary record wire format: an int32 field count, then per
/// field an int32 oid, an int32 length (`-1` for NULL), and the field's binary
/// bytes. `None` if any field's type has no binary encoder.
fn record_binary(fields: &[Bson], field_types: &[Type]) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&(i32::try_from(fields.len()).ok()?).to_be_bytes());
    for (fv, fty) in fields.iter().zip(field_types) {
        out.extend_from_slice(&(fty.oid() as i32).to_be_bytes());
        match fv {
            Bson::Null => out.extend_from_slice(&(-1i32).to_be_bytes()),
            other => {
                let bytes = element_binary(other, fty)?;
                out.extend_from_slice(&(i32::try_from(bytes.len()).ok()?).to_be_bytes());
                out.extend_from_slice(&bytes);
            }
        }
    }
    Some(out)
}

/// The oid an anonymous-record field carries in the binary format, inferred
/// from its stored value: an untyped string literal is `unknown` (705), which
/// is what PostgreSQL reports for a bare `'x'` inside `ROW(...)`. (A composite
/// column's field oids come from its declared types instead -- see
/// `composite_type` -- so this is only ever consulted for oid-2249 records.)
fn record_field_type(v: &Bson) -> Type {
    match v {
        Bson::Boolean(_) => Type::BOOL,
        Bson::Int32(_) => Type::INT4,
        Bson::Int64(_) => Type::INT8,
        Bson::Double(_) => Type::FLOAT8,
        Bson::Decimal128(_) => Type::NUMERIC,
        v if secantus_pgplan::is_wide_numeric(v) => Type::NUMERIC,
        Bson::Binary(_) => Type::BYTEA,
        // `unknown` (705): an untyped string literal. psycopg decodes it to
        // bytes, which is what its own record-binary tests expect.
        _ => Type::UNKNOWN,
    }
}

/// Build the PostgreSQL binary wire form of a (possibly multidimensional)
/// array: `ndims`, `hasnull`, element oid, then per-dimension `[length][lbound]`,
/// then every leaf in ROW-MAJOR order as `[len][bytes]` (`len = -1` for NULL).
/// `None` if a leaf's element type has no binary encoder, or the nesting is
/// ragged (mongod's stored values are rectangular, so this is a guard).
fn array_binary(items: &[Bson], elem: &Type) -> Option<Vec<u8>> {
    // Dimension sizes: walk the first-element chain down to the leaves.
    // An empty array has NO dimensions (`array_send` writes ndim 0 and no
    // dimension pair), not one dimension of length zero.
    let mut dims: Vec<usize> = Vec::new();
    let mut level: &[Bson] = items;
    while !level.is_empty() {
        dims.push(level.len());
        match level.first() {
            Some(Bson::Array(inner)) => level = inner,
            _ => break,
        }
    }
    let ndims = dims.len();
    let mut flat: Vec<Option<Vec<u8>>> = Vec::new();
    fn walk(
        items: &[Bson],
        depth: usize,
        ndims: usize,
        elem: &Type,
        flat: &mut Vec<Option<Vec<u8>>>,
    ) -> Option<()> {
        for it in items {
            if depth + 1 < ndims {
                match it {
                    Bson::Array(inner) => walk(inner, depth + 1, ndims, elem, flat)?,
                    _ => return None,
                }
            } else {
                match it {
                    Bson::Null => flat.push(None),
                    other => flat.push(Some(element_binary(other, elem)?)),
                }
            }
        }
        Some(())
    }
    walk(items, 0, ndims, elem, &mut flat)?;

    let hasnull = flat.iter().any(Option::is_none);
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&(ndims as i32).to_be_bytes());
    out.extend_from_slice(&i32::from(hasnull).to_be_bytes());
    out.extend_from_slice(&(elem.oid() as i32).to_be_bytes());
    for d in &dims {
        out.extend_from_slice(&(*d as i32).to_be_bytes());
        out.extend_from_slice(&1i32.to_be_bytes()); // lower bound is 1
    }
    for leaf in &flat {
        match leaf {
            None => out.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(bytes) => {
                out.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                out.extend_from_slice(bytes);
            }
        }
    }
    Some(out)
}

/// The element type behind an array oid, for the oids this server knows.
///
/// Used by BOTH parameter formats so an array parameter decodes to an array
/// either way.
fn element_of_array_oid(oid: u32) -> Option<&'static str> {
    Some(match oid {
        1000 => "bool",
        1005 => "int2",
        1007 => "int4",
        1009 => "text",
        1015 => "varchar",
        1016 => "int8",
        1021 => "float4",
        1022 => "float8",
        1231 => "numeric",
        1115 => "timestamp",
        1182 => "date",
        1183 => "time",
        1185 => "timestamptz",
        1270 => "timetz",
        1187 => "interval",
        199 => "json",
        3807 => "jsonb",
        1014 => "bpchar",
        1003 => "name",
        3905 => "int4range",
        3927 => "int8range",
        3907 => "numrange",
        3913 => "daterange",
        3909 => "tsrange",
        3911 => "tstzrange",
        6150 => "int4multirange",
        6151 => "nummultirange",
        6152 => "tsmultirange",
        6153 => "tstzmultirange",
        6155 => "datemultirange",
        6157 => "int8multirange",
        1001 => "bytea",
        1041 => "inet",
        651 => "cidr",
        1034 => "aclitem",
        1020 => "box",
        2951 => "uuid",
        // `oid[]` sent as text (`{1,2}`, psycopg's `[Oid(1), Oid(2)]`) used to
        // stay the literal string, and a binary result then refused it as
        // `cannot send this value as a binary _oid`.
        1028 => "oid",
        _ => return None,
    })
}

/// Decode a binary multirange: a 4-byte count, then each member range
/// length-prefixed in the range's own binary form.
fn binary_multirange(
    bytes: &[u8],
    type_name: &str,
    member: &str,
    tz: &secantus_pgplan::TimeZoneSetting,
    cenc: ClientEncoding,
    oid: u32,
) -> PgWireResult<Bson> {
    if bytes.len() < 4 {
        return Err(unsupported_binary_oid(Some(oid)));
    }
    let count = i32::from_be_bytes(bytes[..4].try_into().expect("checked"));
    let mut pos = 4usize;
    let mut members = Vec::new();
    for _ in 0..count.max(0) {
        if pos + 4 > bytes.len() {
            return Err(unsupported_binary_oid(Some(oid)));
        }
        let len = i32::from_be_bytes(bytes[pos..pos + 4].try_into().expect("checked"));
        pos += 4;
        let n = usize::try_from(len.max(0)).unwrap_or(0);
        if pos + n > bytes.len() {
            return Err(unsupported_binary_oid(Some(oid)));
        }
        let text = binary_range(&bytes[pos..pos + n], member, tz, cenc)?;
        pos += n;
        members.push(secantus_pgplan::value_text(&text));
    }
    let literal = format!("{{{}}}", members.join(","));
    secantus_pgplan::cast_text_to(&literal, type_name, tz).map_err(|e| PgHandler::err(&e))
}

/// Decode a binary range: a flags byte, then each present bound as a 4-byte
/// length followed by that many bytes in the element type's binary format.
///
/// The flag bits are PostgreSQL's own (`rangetypes.h`): 0x01 empty, 0x02
/// lower inclusive, 0x04 upper inclusive, 0x08 lower infinite, 0x10 upper
/// infinite. A bound is present exactly when its infinite bit is clear.
fn binary_range(
    bytes: &[u8],
    type_name: &str,
    tz: &secantus_pgplan::TimeZoneSetting,
    cenc: ClientEncoding,
) -> PgWireResult<Bson> {
    const EMPTY: u8 = 0x01;
    const LB_INF: u8 = 0x08;
    const UB_INF: u8 = 0x10;
    const LB_INC: u8 = 0x02;
    const UB_INC: u8 = 0x04;
    let Some((&flags, mut rest)) = bytes.split_first() else {
        return Err(unsupported_binary_oid(None));
    };
    if flags & EMPTY != 0 {
        return Ok(Bson::String("empty".to_string()));
    }
    let element_oid = secantus_pgplan::range::range_element_oid(type_name);
    let take_bound = |rest: &mut &[u8]| -> PgWireResult<Option<String>> {
        if rest.len() < 4 {
            return Err(unsupported_binary_oid(None));
        }
        let len = i32::from_be_bytes(rest[..4].try_into().expect("checked"));
        *rest = &rest[4..];
        if len < 0 {
            return Ok(None);
        }
        let n = len as usize;
        if rest.len() < n {
            return Err(unsupported_binary_oid(None));
        }
        let raw = Bytes::copy_from_slice(&rest[..n]);
        *rest = &rest[n..];
        let ty = Type::from_oid(element_oid);
        // A builtin range's element is numeric / date / timestamp, where the
        // client encoding is irrelevant; a custom one over `text` carries its
        // bound bytes in the client encoding like any other text parameter.
        let value = decode_parameter(Some(&raw), ty.as_ref(), true, tz, cenc)?;
        Ok(Some(secantus_pgplan::value_text(&value)))
    };
    let lower = if flags & LB_INF == 0 {
        take_bound(&mut rest)?
    } else {
        None
    };
    let upper = if flags & UB_INF == 0 {
        take_bound(&mut rest)?
    } else {
        None
    };
    // Through the range renderer, which quotes a bound that needs it: a
    // text-subtype bound holding a comma or a quote is not a literal until
    // it is quoted.
    let literal = secantus_pgplan::range::render(&secantus_pgplan::range::Range {
        empty: false,
        lower,
        upper,
        lower_inc: flags & LB_INC != 0,
        upper_inc: flags & UB_INC != 0,
    });
    secantus_pgplan::cast_text_to(&literal, type_name, tz).map_err(|e| PgHandler::err(&e))
}

/// A one-column table definition standing in for a generated series, so the
/// row encoder and the describe path can treat it like any other source.
fn series_table_def(series: &secantus_pgplan::Series) -> TableDef {
    TableDef::new(
        "generate_series",
        vec![secantus_pgcatalog::Column::new(
            &series.column,
            "int4",
            false,
        )],
    )
}

/// Parse COPY text or CSV input into rows of optional fields, where `None` is
/// that format's NULL.
///
/// The two formats disagree about exactly one thing that matters here, and it
/// is the same thing they disagree about on output: TEXT spells NULL `\N` and
/// an empty field is an empty string, while CSV spells NULL as an unquoted
/// empty field and an empty string as `""`. A parser that treated an empty CSV
/// field as an empty string would silently turn every NULL into one.
fn copy_parse_text(text: &str, format: secantus_pgplan::CopyFormat) -> Vec<Vec<Option<String>>> {
    use secantus_pgplan::CopyFormat;
    if format == CopyFormat::Csv {
        return copy_parse_csv(text);
    }
    let mut rows = Vec::new();
    for line in text.split('\n') {
        // A trailing newline leaves an empty final line, and `\.` is the
        // end-of-data marker from the historical protocol.
        if line.is_empty() || line == "\\." {
            continue;
        }
        rows.push(
            line.split('\t')
                .map(|f| {
                    if f == "\\N" {
                        None
                    } else {
                        Some(unescape_copy_text(f))
                    }
                })
                .collect(),
        );
    }
    rows
}

/// Undo COPY's text-format escaping for one field.
///
/// PostgreSQL's text COPY recognises `\b \f \n \r \t \v \\`, octal (`\ooo`, up
/// to three digits) and hex (`\xHH`, up to two digits) byte escapes, and treats
/// `\<anything else>` as that character. Handling only `\t \n \r \\` silently
/// corrupted `\b`/`\f`/`\v` (a backspace read back as a literal `b`) and,
/// because a `\<letter>` fell through to "push the letter", dropped the
/// backslash from an escaped `\\` sequence that had already been halved. Working
/// over bytes keeps octal/hex faithful and lets a raw multi-byte UTF-8 value
/// pass through untouched.
fn unescape_copy_text(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&c) = bytes.get(i) else {
            out.push(b'\\');
            break;
        };
        match c {
            b'b' => {
                out.push(0x08);
                i += 1;
            }
            b'f' => {
                out.push(0x0c);
                i += 1;
            }
            b'n' => {
                out.push(b'\n');
                i += 1;
            }
            b'r' => {
                out.push(b'\r');
                i += 1;
            }
            b't' => {
                out.push(b'\t');
                i += 1;
            }
            b'v' => {
                out.push(0x0b);
                i += 1;
            }
            b'0'..=b'7' => {
                let mut val: u32 = 0;
                let mut n = 0;
                while n < 3 && i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                    val = val * 8 + u32::from(bytes[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push((val & 0xff) as u8);
            }
            b'x' if bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit) => {
                i += 1; // consume the `x`
                let mut val: u32 = 0;
                let mut n = 0;
                while n < 2 && i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    val = val * 16 + char::from(bytes[i]).to_digit(16).expect("hex");
                    i += 1;
                    n += 1;
                }
                out.push((val & 0xff) as u8);
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// CSV, where a newline inside quotes is DATA rather than a row separator --
/// so the rows cannot be found by splitting on newlines first.
fn copy_parse_csv(text: &str) -> Vec<Vec<Option<String>>> {
    let mut rows = Vec::new();
    let mut row: Vec<Option<String>> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                // A doubled quote is one literal quote.
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                was_quoted = true;
            }
            ',' => {
                row.push(csv_field(&field, was_quoted));
                field.clear();
                was_quoted = false;
            }
            '\n' => {
                row.push(csv_field(&field, was_quoted));
                field.clear();
                was_quoted = false;
                rows.push(std::mem::take(&mut row));
            }
            '\r' => {}
            _ => field.push(c),
        }
    }
    if !field.is_empty() || was_quoted || !row.is_empty() {
        row.push(csv_field(&field, was_quoted));
        rows.push(row);
    }
    rows
}

/// An UNQUOTED empty CSV field is NULL; a quoted one is the empty string.
fn csv_field(text: &str, was_quoted: bool) -> Option<String> {
    if text.is_empty() && !was_quoted {
        None
    } else {
        Some(text.to_string())
    }
}

/// One COPY row in PostgreSQL's text or CSV format.
///
/// The formats differ in more than the delimiter, and the differences are
/// exactly where NULL lives:
///
/// * TEXT writes `\N` for NULL and escapes newline, tab, carriage return and
///   backslash. An empty string is an empty field.
/// * CSV writes NULL as an EMPTY unquoted field, and therefore has to quote the
///   empty STRING as `""` to keep the two apart. A value is also quoted when it
///   contains the delimiter, a quote or a newline, and an embedded quote is
///   doubled.
///
/// All of it measured against PostgreSQL 14.
fn copy_text_row(
    row: &[Option<Bson>],
    format: secantus_pgplan::CopyFormat,
    schema: &[FieldInfo],
) -> bytes::Bytes {
    use secantus_pgplan::CopyFormat;
    let csv = format == CopyFormat::Csv;
    let mut out = String::new();
    for (i, value) in row.iter().enumerate() {
        if i > 0 {
            out.push(if csv { ',' } else { '\t' });
        }
        let oid = schema.get(i).map(|f| f.datatype().oid()).unwrap_or(0);
        let text = match value {
            None | Some(Bson::Null) => {
                if !csv {
                    out.push_str("\\N");
                }
                // CSV's null is the empty field, so there is nothing to write.
                continue;
            }
            // An inet / cidr field is `inet_out` / `cidr_out`, as in a SELECT:
            // inet drops a full-host mask, cidr keeps it.
            Some(Bson::String(v)) if matches!(oid, 869 | 650) => {
                secantus_pgplan::net::text_out(v, oid == 650)
            }
            Some(Bson::String(v)) => v.clone(),
            Some(Bson::Array(items)) if matches!(oid, 1041 | 651) => {
                let rendered: Vec<Bson> = items
                    .iter()
                    .map(|x| match x {
                        Bson::String(t) => {
                            Bson::String(secantus_pgplan::net::text_out(t, oid == 651))
                        }
                        other => other.clone(),
                    })
                    .collect();
                secantus_pgplan::value_text(&Bson::Array(rendered))
            }
            Some(other) => secantus_pgplan::value_text(other),
        };
        if csv {
            let needs_quote =
                text.is_empty() || text.chars().any(|c| matches!(c, ',' | '"' | '\n' | '\r'));
            if needs_quote {
                out.push('"');
                for c in text.chars() {
                    if c == '"' {
                        out.push('"');
                    }
                    out.push(c);
                }
                out.push('"');
            } else {
                out.push_str(&text);
            }
        } else {
            for c in text.chars() {
                match c {
                    '\u{08}' => out.push_str("\\b"),
                    '\u{0c}' => out.push_str("\\f"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{0b}' => out.push_str("\\v"),
                    '\\' => out.push_str("\\\\"),
                    _ => out.push(c),
                }
            }
        }
    }
    out.push('\n');
    bytes::Bytes::from(out.into_bytes())
}

/// Decode one bound parameter into the value the planner will substitute.
///
/// `None` is SQL NULL. A client may declare a parameter's type as oid 0
/// ("unspecified") and leave the server to infer it, which is why the text path
/// falls back to sniffing the literal rather than assuming `text`.
fn decode_parameter(
    raw: Option<&Bytes>,
    ty: Option<&Type>,
    binary: bool,
    tz: &secantus_pgplan::TimeZoneSetting,
    cenc: ClientEncoding,
) -> PgWireResult<Bson> {
    let Some(bytes) = raw else {
        return Ok(Bson::Null);
    };
    if binary {
        // Binary format is width- and type-exact, so an unknown type here is a
        // genuine "cannot decode" rather than something to guess at.
        return match ty.map(|t| t.oid()) {
            Some(23) if bytes.len() == 4 => Ok(Bson::Int32(i32::from_be_bytes(
                bytes[..4].try_into().expect("checked"),
            ))),
            Some(20) if bytes.len() == 8 => Ok(Bson::Int64(i64::from_be_bytes(
                bytes[..8].try_into().expect("checked"),
            ))),
            Some(21) if bytes.len() == 2 => Ok(Bson::Int32(i32::from(i16::from_be_bytes(
                bytes[..2].try_into().expect("checked"),
            )))),
            Some(701) if bytes.len() == 8 => Ok(Bson::Double(f64::from_be_bytes(
                bytes[..8].try_into().expect("checked"),
            ))),
            Some(700) if bytes.len() == 4 => Ok(Bson::Double(f64::from(f32::from_be_bytes(
                bytes[..4].try_into().expect("checked"),
            )))),
            Some(16) if bytes.len() == 1 => Ok(Bson::Boolean(bytes[0] != 0)),
            // An oid is a 4-byte UNSIGNED integer; through i64 so the value
            // survives the top bit.
            Some(26) if bytes.len() == 4 => Ok(Bson::Int64(i64::from(u32::from_be_bytes(
                bytes[..4].try_into().expect("checked"),
            )))),
            Some(25) | Some(1043) | Some(19) | Some(1042) => {
                // A text-family value's binary wire form is its bytes in the
                // client encoding (same bytes as text format), so decode it
                // back to the internal UTF-8 form. A NUL byte cannot be in a
                // text value: PostgreSQL 16 rejects it as `22021` (a text
                // FORMAT parameter is C-string terminated at the NUL instead
                // -- that path never sees the byte).
                if bytes.contains(&0) {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".to_owned(),
                        "22021".to_owned(), // character_not_in_repertoire
                        "invalid byte sequence for encoding \"UTF8\": 0x00".to_owned(),
                    ))));
                }
                Ok(Bson::String(encoding::decode(cenc, bytes)))
            }
            // `bytea` is raw bytes on the wire -- stored verbatim as Binary.
            Some(17) => Ok(secantus_pgplan::bytea::to_binary(bytes.to_vec())),
            // `uuid` is 16 raw bytes on the wire -> canonical lowercase text
            // (the same value the text path stores).
            Some(2950) => secantus_pgplan::uuid_from_wire(bytes)
                .map(Bson::String)
                .ok_or_else(|| {
                    PgHandler::err(&secantus_pgplan::Error::InvalidText(
                        "invalid binary uuid value".into(),
                    ))
                }),
            // inet / cidr: PostgreSQL's [family,bits,is_cidr,nb,addr] layout,
            // decoded back to the canonical addr/masklen text the store holds.
            Some(869) | Some(650) => secantus_pgplan::net::from_wire(bytes)
                .map(Bson::String)
                .ok_or_else(|| {
                    PgHandler::err(&secantus_pgplan::Error::InvalidText(
                        "invalid binary inet/cidr value".into(),
                    ))
                }),
            // These decode to their CANONICAL TEXT so a binary parameter takes
            // exactly the same path through the planner as a text one -- the
            // text path already turns each of these into the right value, and
            // duplicating that here is how the two formats drift apart.
            Some(1700) => match binary_numeric_text(bytes) {
                Some(t) => secantus_pgplan::parse_numeric(&t).map_err(|e| PgHandler::err(&e)),
                None => Err(unsupported_binary_oid(Some(1700))),
            },
            Some(1082) if bytes.len() == 4 => {
                Ok(Bson::String(secantus_pgplan::render_date_from_pg_days(
                    i32::from_be_bytes(bytes[..4].try_into().expect("checked")),
                )))
            }
            Some(1083) if bytes.len() == 8 => {
                Ok(Bson::String(secantus_pgplan::render_time_from_micros(
                    i64::from_be_bytes(bytes[..8].try_into().expect("checked")),
                )))
            }
            // An instant on the wire -- i64 microseconds since 2000-01-01 UTC.
            // Store it as the same INSTANT carrier a `::timestamptz` literal
            // produces (a BSON date, or a sub-millisecond composite), NOT as
            // session-rendered text: text dropped the zone offset the moment
            // anything re-coerced it as a bare timestamp, so a binary parameter
            // compared unequal to the literal it was meant to equal.
            Some(1184) if bytes.len() == 8 => {
                let pg_micros = i64::from_be_bytes(bytes[..8].try_into().expect("checked"));
                // PostgreSQL encodes the timestamptz infinities as the extreme
                // i64 values; keep them as text, the form the loader expects
                // (the finite-instant carrier cannot hold them).
                if pg_micros == i64::MAX {
                    return Ok(Bson::String("infinity".to_string()));
                }
                if pg_micros == i64::MIN {
                    return Ok(Bson::String("-infinity".to_string()));
                }
                match pg_micros.checked_add(946_684_800 * 1_000_000) {
                    Some(micros) => Ok(secantus_pgplan::timestamptz_value_from_micros(micros)),
                    // A value so far out it overflows the epoch shift is beyond
                    // the finite range this server plans; render it in the
                    // session zone as a last resort rather than panic.
                    None => Ok(Bson::String(secantus_pgplan::render_timestamptz(
                        pg_micros, tz,
                    ))),
                }
            }
            // `timetz` is 8 bytes of microseconds since midnight plus a 4-byte
            // offset in SECONDS WEST of UTC -- the opposite sign to the one the
            // text form prints, which is why this negates.
            Some(1266) if bytes.len() == 12 => {
                let us = i64::from_be_bytes(bytes[..8].try_into().expect("checked"));
                let west = i32::from_be_bytes(bytes[8..12].try_into().expect("checked"));
                Ok(Bson::String(secantus_pgplan::render_timetz(us, -west)))
            }
            // `interval`: 8 bytes of microseconds, then days, then months --
            // three parts on the wire for the same reason they are three parts
            // in the value, since neither converts without a calendar.
            Some(1186) if bytes.len() == 16 => {
                let micros = i64::from_be_bytes(bytes[..8].try_into().expect("checked"));
                let days = i32::from_be_bytes(bytes[8..12].try_into().expect("checked"));
                let months = i32::from_be_bytes(bytes[12..16].try_into().expect("checked"));
                Ok(secantus_pgplan::Interval {
                    months,
                    days,
                    micros,
                }
                .to_bson())
            }
            // `json` is text on the wire, in the client encoding. `jsonb` is
            // the same text behind a one-byte format version, which is 1 and
            // has been since the type shipped -- an unknown version means the
            // client is speaking something this server has never seen, so it
            // refuses rather than guessing at the payload. Both then take the
            // cast a text-format parameter takes, so the value is validated
            // and (for jsonb) normalised the same way whichever format it
            // arrived in: a binary `Jsonb("\u00e0")` used to keep psycopg's
            // ASCII escape where the text one was stored as the character.
            // A range's binary form is a flags byte and then each present
            // bound as a length-prefixed value in the ELEMENT's binary format.
            // Decoding to canonical text keeps it on the same path a literal
            // takes, as with every other type here.
            // A multirange is a count of ranges, then each one length-prefixed
            // in the RANGE's own binary form -- so it reuses the range decoder
            // rather than repeating the flags-and-bounds layout.
            Some(oid) if secantus_pgplan::range::multirange_oid_name(oid).is_some() => {
                let type_name = secantus_pgplan::range::multirange_oid_name(oid).expect("checked");
                let member = secantus_pgplan::range::multirange_member(type_name)
                    .expect("a multirange has a member type");
                binary_multirange(bytes, type_name, &member, tz, cenc, oid)
            }
            Some(oid) if secantus_pgplan::range::range_oid_name(oid).is_some() => {
                let type_name = secantus_pgplan::range::range_oid_name(oid).expect("checked");
                binary_range(bytes, type_name, tz, cenc)
            }
            // A CUSTOM range or multirange: the same layouts, with the type
            // named by the registry the oid was minted from.
            Some(oid)
                if ty.is_some_and(|t| {
                    matches!(
                        t.kind(),
                        postgres_types::Kind::Range(_) | postgres_types::Kind::Multirange(_)
                    )
                }) =>
            {
                let type_name = secantus_pgplan::user_type_name(i64::from(oid))
                    .ok_or_else(|| unsupported_binary_oid(Some(oid)))?;
                match secantus_pgplan::range::multirange_member(&type_name) {
                    Some(member) => binary_multirange(bytes, &type_name, &member, tz, cenc, oid),
                    None => binary_range(bytes, &type_name, tz, cenc),
                }
            }
            Some(114) => secantus_pgplan::cast_text_to(&encoding::decode(cenc, bytes), "json", tz)
                .map_err(|e| PgHandler::err(&e)),
            Some(3802) => match bytes.split_first() {
                Some((1, rest)) => {
                    secantus_pgplan::cast_text_to(&encoding::decode(cenc, rest), "jsonb", tz)
                        .map_err(|e| PgHandler::err(&e))
                }
                _ => Err(unsupported_binary_oid(Some(3802))),
            },
            Some(1114) if bytes.len() == 8 => Ok(Bson::String(
                secantus_pgplan::render_timestamp_from_pg_micros(i64::from_be_bytes(
                    bytes[..8].try_into().expect("checked"),
                )),
            )),
            // Every array oid this server knows, decoded through the element's
            // own binary decoder rather than a per-type array reader.
            Some(oid) if element_of_array_oid(oid).is_some() => binary_array(bytes, tz, cenc),
            // A user ENUM ARRAY (the declared type names it): the same array
            // layout, whose element oid `Type::from_oid` does not know -- so
            // each element takes the user-type arm below and decodes to its
            // label. Membership is checked by the caller against the enum.
            Some(_)
                if ty.is_some_and(|t| {
                    matches!(t.kind(), postgres_types::Kind::Array(inner)
                        if matches!(inner.kind(), postgres_types::Kind::Enum(_)))
                }) =>
            {
                binary_array(bytes, tz, cenc)
            }
            // An oid this server has no decoder for is a USER type -- the
            // known builtins all matched above. A user ENUM's binary format is
            // its label's bytes in the client encoding, so it decodes to the
            // label. Under a single-byte encoding (LATIN1 / LATIN9) every byte
            // is valid; under UTF8 / passthrough invalid bytes are still a
            // refusal. (pgwire resolves the Parse message's oids through
            // `Type::from_oid`, so an enum's raw oid arrives here as `None` --
            // both cases take this arm.)
            other => {
                if cenc.transcodes() {
                    Ok(Bson::String(encoding::decode(cenc, bytes)))
                } else {
                    match std::str::from_utf8(bytes) {
                        Ok(text) => Ok(Bson::String(text.to_string())),
                        Err(_) => Err(unsupported_binary_oid(other)),
                    }
                }
            }
        };
    }

    let text: std::borrow::Cow<'_, str> = std::borrow::Cow::Owned(encoding::decode(cenc, bytes));
    match ty.map(|t| t.oid()) {
        Some(23) | Some(21) => text
            .parse::<i32>()
            .map(Bson::Int32)
            .map_err(|_| invalid_text(&text, "integer")),
        Some(20) => text
            .parse::<i64>()
            .map(Bson::Int64)
            .map_err(|_| invalid_text(&text, "bigint")),
        Some(26) => text
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|v| (0..(1i64 << 32)).contains(v))
            .map(Bson::Int64)
            .ok_or_else(|| invalid_text(&text, "oid")),
        Some(700) | Some(701) => text
            .parse::<f64>()
            .map(Bson::Double)
            .map_err(|_| invalid_text(&text, "double precision")),
        // A `numeric` parameter is EXACT, and was being parsed as an f64 --
        // so a client binding Decimal("0.1") got a float, and one binding
        // `1.50` lost the scale that makes it a different value from `1.5`.
        Some(1700) => secantus_pgplan::parse_numeric(&text).map_err(|e| PgHandler::err(&e)),
        Some(16) => Ok(Bson::Boolean(matches!(
            text.as_ref(),
            "t" | "true" | "TRUE" | "1" | "y" | "yes" | "on"
        ))),
        Some(25) | Some(1043) | Some(19) | Some(1042) => Ok(Bson::String(text.into_owned())),
        Some(17) => secantus_pgplan::bytea::parse_text(&text)
            .map(secantus_pgplan::bytea::to_binary)
            .map_err(|e| PgHandler::err(&e)),
        // A uuid parameter is stored in its canonical hyphenated form, so a
        // client sending the 32-digit spelling (psycopg's text dumper) reads
        // back what PostgreSQL would echo: `8-4-4-4-12`, lowercase.
        Some(2950) => {
            secantus_pgplan::cast_text_to(&text, "uuid", tz).map_err(|e| PgHandler::err(&e))
        }
        Some(869) => secantus_pgplan::net::normalize_inet(&text)
            .map(Bson::String)
            .map_err(|e| PgHandler::err(&e)),
        Some(650) => secantus_pgplan::net::normalize_cidr(&text)
            .map(Bson::String)
            .map_err(|e| PgHandler::err(&e)),
        Some(1033) => {
            secantus_pgplan::cast_text_to(&text, "aclitem", tz).map_err(|e| PgHandler::err(&e))
        }
        Some(603) => {
            secantus_pgplan::cast_text_to(&text, "box", tz).map_err(|e| PgHandler::err(&e))
        }
        // The TYPED text forms. These reach the same value the BINARY path
        // produces for the same oid, which is the whole point: a parameter's
        // meaning cannot depend on the format a client happened to send it in.
        //
        // Left out, they fell through to `sniff_text` and became plain strings,
        // so `array[...] = %s` compared an array to a string and reported
        // "cannot compare" -- 98 failures whose message pointed at comparison
        // when the cause was here, one layer earlier.
        Some(oid @ (1082 | 1083 | 1114 | 1184 | 1266 | 1186 | 114 | 3802)) => {
            let target = match oid {
                1082 => "date",
                1083 => "time",
                1114 => "timestamp",
                1184 => "timestamptz",
                1266 => "timetz",
                114 => "json",
                3802 => "jsonb",
                _ => "interval",
            };
            secantus_pgplan::cast_text_to(&text, target, tz).map_err(|e| PgHandler::err(&e))
        }
        Some(oid) if element_of_array_oid(oid).is_some() => {
            let element = element_of_array_oid(oid).expect("checked");
            secantus_pgplan::cast_text_to(&text, &format!("{element}[]"), tz)
                .map_err(|e| PgHandler::err(&e))
        }
        // A user ENUM ARRAY in text: an array of labels (the caller checks
        // membership), parsed as a text array.
        Some(_)
            if ty.is_some_and(|t| {
                matches!(t.kind(), postgres_types::Kind::Array(inner)
                    if matches!(inner.kind(), postgres_types::Kind::Enum(_)))
            }) =>
        {
            secantus_pgplan::cast_text_to(&text, "text[]", tz).map_err(|e| PgHandler::err(&e))
        }
        // A range or multirange sent as TEXT. Without these it fell through to
        // `sniff_text` and stayed the literal the client wrote, UNCANONICALISED
        // -- so `int4range(10, 20, '[]') = %s` was FALSE against the same range
        // bound as a parameter, because the left side had been rewritten to
        // `[10,21)` and the right side had not. The binary path already went
        // through the cast; this is the same value by the other route.
        Some(oid)
            if secantus_pgplan::range::range_oid_name(oid).is_some()
                || secantus_pgplan::range::multirange_oid_name(oid).is_some() =>
        {
            let target = secantus_pgplan::range::range_oid_name(oid)
                .or_else(|| secantus_pgplan::range::multirange_oid_name(oid))
                .expect("checked");
            secantus_pgplan::cast_text_to(&text, target, tz).map_err(|e| PgHandler::err(&e))
        }
        // oid 0 = the client left the type to us. PostgreSQL infers from
        // context; sniffing the literal covers the shapes this server plans.
        _ => Ok(sniff_text(&text)),
    }
}

fn invalid_text(text: &str, want: &str) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        "22P02".into(), // invalid_text_representation
        format!("invalid input syntax for type {want}: \"{text}\""),
    )))
}

fn sniff_text(text: &str) -> Bson {
    // Only treat it as a number when the number ROUND-TRIPS to the same text.
    // `01` parses as 1, but it is not how anyone writes 1 -- and a client that
    // sent `01` for an unspecified parameter may be sending JSON, where `01`
    // is invalid and must stay invalid. Sniffing must not make a value more
    // acceptable than the client wrote it.
    let round_trips = |rendered: String| rendered == text;
    if let Ok(i) = text.parse::<i32>() {
        if round_trips(i.to_string()) {
            return Bson::Int32(i);
        }
    }
    if let Ok(i) = text.parse::<i64>() {
        if round_trips(i.to_string()) {
            return Bson::Int64(i);
        }
    }
    if let Ok(f) = text.parse::<f64>() {
        if round_trips(f.to_string()) {
            return Bson::Double(f);
        }
    }
    Bson::String(text.to_string())
}

impl PgHandler {
    /// Every bound parameter of a portal, decoded in order.
    ///
    /// `param_types` is the planner name for each parameter -- the client's
    /// declaration, or the type INFERRED from the statement when the client
    /// gave none (`infer_param_types`). A parameter the client left untyped
    /// but the statement compares against a range is decoded AS that range:
    /// psycopg's bare `Range(empty=True)` arrives with oid 0 and, in binary,
    /// as the single flag byte `\x01`, which read as text is a control
    /// character and not a range at all. Only the inferred name can route it
    /// to the range decoder.
    /// Give an UNTYPED parameter sent in BINARY format the type the catalog
    /// implies for it (the column an `insert ... values ($1)` puts it in).
    ///
    /// A text parameter can stay untyped until the planner casts it, but a
    /// binary one cannot: the bytes only mean something under a type.
    /// psycopg sends an empty `Multirange([])` untyped (oid 0 -- no element to
    /// name the type from) and in binary as four zero bytes, and read as text
    /// those were `malformed multirange literal: "\0\0\0\0"`. PostgreSQL
    /// resolves the parameter's type from the target column at Parse, so the
    /// same statement inserts an empty multirange there. Text-format slots
    /// are left alone so their path is unchanged.
    fn type_untyped_binary_params(
        &self,
        portal: &Portal<ParsedStatement>,
        param_types: &mut [Option<String>],
    ) {
        let binary_at = |i: usize| match &portal.parameter_format {
            Format::UnifiedText => false,
            Format::UnifiedBinary => true,
            Format::Individual(codes) => codes.get(i).copied().unwrap_or(0) == 1,
        };
        let untyped_binary: Vec<usize> = (0..portal.parameters.len())
            .filter(|&i| {
                binary_at(i)
                    && portal
                        .statement
                        .parameter_types
                        .get(i)
                        .is_none_or(Option::is_none)
                    && portal
                        .statement
                        .parameter_oids
                        .get(i)
                        .is_none_or(|o| *o == 0)
                    && param_types.get(i).is_none_or(Option::is_none)
            })
            .collect();
        if untyped_binary.is_empty() {
            return;
        }
        let column_type = |table: &str, column: secantus_pgplan::ColumnRef<'_>| {
            self.lookup(table).and_then(|def| {
                let col = match column {
                    secantus_pgplan::ColumnRef::Name(name) => def.column(name),
                    secantus_pgplan::ColumnRef::Position(pos) => def.columns.get(pos),
                };
                col.map(|c| c.pg_type.clone())
            })
        };
        let inferred = secantus_pgplan::catalog_param_types_opt(
            &portal.statement.statement.sql,
            param_types,
            &column_type,
        );
        for i in untyped_binary {
            if let (Some(slot), Some(Some(name))) = (param_types.get_mut(i), inferred.get(i)) {
                *slot = Some(name.clone());
            }
        }
    }

    fn portal_params<S>(
        &self,
        portal: &Portal<S>,
        param_types: &[Option<String>],
    ) -> PgWireResult<Vec<Bson>>
    where
        S: Clone + Send + Sync,
    {
        // Sized by the plan's list, which covers every `$n` in the SQL even
        // when the client's oid list is shorter (or empty).
        let n = portal
            .statement
            .parameter_types
            .len()
            .max(param_types.len());
        let declared: Vec<Option<Type>> = (0..n)
            .map(|i| {
                // A name the plan resolved for the slot: a builtin, or a USER
                // type (a custom range's oid maps to `None` in the statement's
                // types, and its binary form is a range, not text).
                portal
                    .statement
                    .parameter_types
                    .get(i)
                    .cloned()
                    .flatten()
                    .or_else(|| {
                        let name = param_types.get(i).and_then(|n| n.as_deref())?;
                        Some(self.user_wire_type(name).unwrap_or_else(|| wire_type(name)))
                    })
            })
            .collect();
        let oids = &portal.statement.parameter_oids;
        let tz = self.session_timezone();
        let cenc = self.client_encoding();
        portal
            .parameters
            .iter()
            .enumerate()
            .map(|(i, raw)| {
                let binary = match &portal.parameter_format {
                    Format::UnifiedText => false,
                    Format::UnifiedBinary => true,
                    Format::Individual(codes) => codes.get(i).copied().unwrap_or(0) == 1,
                };
                // 0.40 maps an unspecified OR non-builtin oid to `None`. When
                // the raw oid (preserved by the patched `parameter_oids`) names
                // a user COMPOSITE, decode the value into the record BSON a
                // `::comp` literal produces -- the ordinary decoder would sniff
                // the `(a,b)` text into a plain string, so field access and
                // `= row(..)` on the parameter both broke. The planner can
                // also NAME the slot a composite (`row(..)::comp = $1` infers
                // it from the compared operand), in which case `declared` is
                // the composite `Type` itself and its oid takes the same door.
                let declared_ty = declared.get(i).and_then(|t| t.as_ref());
                let composite_oid = match declared_ty {
                    None => oids.get(i).copied().filter(|o| *o != 0),
                    Some(t) if matches!(t.kind(), postgres_types::Kind::Composite(_)) => {
                        Some(t.oid())
                    }
                    Some(_) => None,
                };
                if let Some(oid) = composite_oid {
                    if let Some(bson) =
                        self.decode_composite_param(oid, raw.as_ref(), binary, &tz)?
                    {
                        return Ok(bson);
                    }
                }
                // A raw oid naming a user ENUM or enum ARRAY that the planner
                // did not type itself: resolve it to the wire type so the
                // decoder reads an enum array's `{a,b}` text as an ARRAY of
                // labels rather than sniffing it into one string.
                let enum_ty = match declared_ty {
                    None => oids
                        .get(i)
                        .copied()
                        .filter(|o| *o != 0)
                        .and_then(|o| self.user_wire_type_for_oid(o))
                        .filter(|t| {
                            matches!(t.kind(), postgres_types::Kind::Enum(_))
                                || matches!(t.kind(), postgres_types::Kind::Array(inner)
                                    if matches!(inner.kind(), postgres_types::Kind::Enum(_)))
                        }),
                    Some(_) => None,
                };
                let declared_ty = enum_ty.as_ref().or(declared_ty);
                let value = decode_parameter(raw.as_ref(), declared_ty, binary, &tz, cenc)?;
                self.check_enum_param(i, oids.get(i).copied(), declared_ty, &value)?;
                Ok(value)
            })
            .collect()
    }

    /// Reject a parameter typed as a user ENUM (or an enum ARRAY) whose value
    /// is not one of its labels -- PostgreSQL validates the label at Bind, so
    /// `select $1::text` with an enum-typed `$1` holding a non-label is a
    /// `22P02` before the query runs. Measured on 16.15: the context line is
    /// `unnamed portal parameter $1 = '...'` (the value elided, as
    /// `log_parameter_max_length_on_error` defaults to 0).
    fn check_enum_param(
        &self,
        index: usize,
        raw_oid: Option<u32>,
        declared: Option<&Type>,
        value: &Bson,
    ) -> PgWireResult<()> {
        let enums = self.enums()?;
        let by_oid = |oid: i64| enums.iter().find(|(_, o, _)| *o == oid);
        let found = match declared.map(|t| t.kind()) {
            Some(postgres_types::Kind::Enum(_)) => {
                declared.and_then(|t| by_oid(i64::from(t.oid())))
            }
            Some(postgres_types::Kind::Array(inner))
                if matches!(inner.kind(), postgres_types::Kind::Enum(_)) =>
            {
                by_oid(i64::from(inner.oid()))
            }
            _ => raw_oid.filter(|o| *o != 0).and_then(|o| {
                let o = i64::from(o);
                by_oid(o).or_else(|| by_oid(o - Self::USER_TYPE_ARRAY_OID_OFFSET))
            }),
        };
        let Some((name, _, labels)) = found else {
            return Ok(());
        };
        fn first_bad<'a>(v: &'a Bson, labels: &[String]) -> Option<&'a str> {
            match v {
                Bson::String(s) if !labels.contains(s) => Some(s),
                Bson::Array(items) => items.iter().find_map(|x| first_bad(x, labels)),
                _ => None,
            }
        }
        match first_bad(value, labels) {
            None => Ok(()),
            Some(label) => {
                let mut info = ErrorInfo::new(
                    "ERROR".to_owned(),
                    "22P02".to_owned(),
                    format!("invalid input value for enum {name}: \"{label}\""),
                );
                info.where_context =
                    Some(format!("unnamed portal parameter ${} = '...'", index + 1));
                Err(PgWireError::UserError(Box::new(info)))
            }
        }
    }

    /// The output columns a statement would produce, without running it, or
    /// `None` for a statement that produces no result set at all.
    ///
    /// The two are different wire answers: `select` (no columns) is a
    /// `RowDescription` of zero fields, and psycopg's `stream()` iterates its
    /// one empty row; `insert` is `NoData`. Answering `NoData` for both made
    /// `cur.stream("select")` yield nothing.
    ///
    /// Planned against NULL placeholders: `Describe` arrives before `Bind`, so
    /// no values exist yet, and the result SHAPE does not depend on them.
    fn describe_fields(
        &self,
        sql: &str,
        n_params: usize,
        param_types: &[Option<String>],
    ) -> PgWireResult<Option<Vec<FieldInfo>>> {
        let params = vec![Bson::Null; n_params];
        // Describe resolves table names too, and against the same uncommitted
        // catalog, through `self.lookup`. The DECLARED parameter types
        // come with it: a describe that did not know them answered `42P18` for
        // `pg_typeof($1)` before the execute that does know them ever ran, and
        // the client sees the describe's error.
        let tz = self.session_timezone();
        self.install_user_types();
        let stmt = secantus_pgplan::plan_with_session_types(
            sql,
            &|n| self.lookup(n),
            &params,
            param_types,
            &tz,
        )
        .map_err(|e| Self::err(&e))
        // psycopg learns a statement's columns with a `Describe` sent straight
        // after `Parse`, BEFORE any `Bind`/`Execute`. When the statement is
        // bad (`meh`), that Describe is where planning fails -- and inside a
        // transaction PostgreSQL treats the failure like any other, poisoning
        // the block so the next statement gets `25P02`. Without noting it here
        // the failure went unrecorded and the aborted block kept accepting
        // commands.
        .inspect_err(|_| self.note_failure())?;
        Ok(Some(match stmt {
            // A FETCH describes the CURSOR's columns. Without this arm a
            // prepared FETCH described zero of them, and psycopg prepares any
            // statement it runs six times -- so a cursor read in a loop worked
            // five times and then sent rows the client had no description for.
            //
            // The cursor's schema is stored in TEXT (built at DECLARE), but a
            // BINARY fetch requests binary result columns on its `Bind`, which
            // ran before this Describe -- so the RowDescription must report the
            // binary format the rows will actually arrive in, or the client
            // records the wrong column format (`PGresult.fformat`).
            Statement::Fetch { name, .. } => {
                let want_binary = self
                    .binary_results
                    .load(std::sync::atomic::Ordering::Relaxed);
                let cursors = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
                match cursors.get(&name) {
                    Some(cursor) if want_binary && cursor.typed_rows.is_some() => cursor
                        .schema
                        .iter()
                        .map(|f| rebind_field_format(f, true))
                        .collect::<Vec<_>>(),
                    Some(cursor) => cursor.schema.as_ref().clone(),
                    None => Vec::new(),
                }
            }
            // A generated source describes its one column -- an int4 unless a
            // `generate_series(...)::type` cast retyped it, in which case the
            // described type is the cast's, matching the executor's schema.
            Statement::Select(sel) if sel.series.is_some() => sel
                .columns
                .iter()
                .enumerate()
                .map(|(i, (out, _))| {
                    let ty = match sel.casts.get(i).and_then(|c| c.as_ref()) {
                        Some(expr) => wire_type(secantus_pgplan::column_expr_type(expr)),
                        None => wire_type("int4"),
                    };
                    self.field(out.clone(), ty)
                })
                .collect::<Vec<_>>(),
            Statement::Select(sel) => {
                let def = match &sel.join {
                    Some(join) => secantus_pgplan::join_output_def(join, &|n| self.lookup(n))
                        .map_err(|e| Self::err(&e))?,
                    None => self
                        .lookup(&sel.table)
                        .ok_or_else(|| Self::err(&PlanError::UndefinedTable(sel.table.clone())))?,
                };
                self.row_schema(&def, &sel.columns, &sel.casts)
            }
            // `INSERT ... RETURNING` describes the RETURNING list over the
            // table, exactly as a SELECT of the same list would.
            Statement::Insert(ins) if ins.returning.is_some() => {
                let def = self
                    .lookup(&ins.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(ins.table.clone())))?;
                let returning = ins.returning.as_ref().expect("checked");
                self.row_schema(&def, &returning.columns, &returning.casts)
            }
            // An aggregate over a generated source has no table to look up, and
            // no GROUP BY columns -- only the aggregates themselves.
            Statement::Aggregate(agg) if agg.series.is_some() => agg
                .select
                .iter()
                .map(|(name, col)| {
                    let ty = match col {
                        OutputCol::Agg(i) => aggregate_wire_type(&agg.items[*i]),
                        OutputCol::Group(_) => Type::INT4,
                    };
                    self.field(name.clone(), ty)
                })
                .collect(),
            Statement::Aggregate(agg) => agg
                .select
                .iter()
                .map(|(name, col)| {
                    let ty = match col {
                        OutputCol::Group(i) => {
                            let t = &agg.group_by[*i].pg_type;
                            self.user_wire_type(t).unwrap_or_else(|| wire_type(t))
                        }
                        OutputCol::Agg(i) => aggregate_wire_type(&agg.items[*i]),
                    };
                    self.field(name.clone(), ty)
                })
                .collect(),
            Statement::Show(name) => vec![self.field(canonical_setting(&name), Type::TEXT)],
            Statement::SelectConstant(sc) => sc
                .columns
                .iter()
                .map(|(name, _, ty, typmod)| {
                    let wire = self.user_wire_type(ty).unwrap_or_else(|| wire_type(ty));
                    self.field_mod(name.clone(), wire, *typmod)
                })
                .collect(),
            Statement::ValuesConstant(vc) => vc
                .names
                .iter()
                .zip(&vc.types)
                .map(|(name, ty)| {
                    let wire = self.user_wire_type(ty).unwrap_or_else(|| wire_type(ty));
                    self.field(name.clone(), wire)
                })
                .collect(),
            // CREATE / INSERT / UPDATE / DELETE return no rows.
            _ => return Ok(None),
        }))
    }
}

#[async_trait]
impl ExtendedQueryHandler for PgHandler {
    type Statement = ParsedStatement;
    type QueryParser = SqlParser;

    fn query_parser(&self) -> Arc<Self::QueryParser> {
        Arc::new(SqlParser)
    }

    /// Same as the simple-query hook: a `Parse` message's SQL is in the
    /// client encoding.
    fn decode_query_text<C>(&self, _c: &C, parse: &Parse) -> PgWireResult<String>
    where
        C: ClientInfo,
    {
        Ok(encoding::decode(self.client_encoding(), &parse.query_raw))
    }

    /// The default `Parse` handling, plus the `pg_prepared_statements` row a
    /// NAMED statement gets. The unnamed statement is not listed -- PostgreSQL
    /// does not list it either.
    async fn on_parse<C>(&self, client: &mut C, message: Parse) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let parser = <Self as ExtendedQueryHandler>::query_parser(self);
        let mut message = message;
        let decoded = <Self as ExtendedQueryHandler>::decode_query_text(self, client, &message)?;
        // Read under the session's string syntax at Parse time, when
        // PostgreSQL's scanner reads it; the scanner's warnings
        // (`escape_string_warning`) go out with the Parse they belong to, and
        // before its error when the text is unterminated.
        let rewritten = self.apply_string_syntax(&decoded);
        self.flush_notices(client).await?;
        message.query = rewritten?;
        let stmt = StoredStatement::parse(client, &message, parser).await?;
        // The message's own name, not `stmt.id`: the wire store files the
        // unnamed statement under its `DEFAULT_NAME` placeholder.
        if message.name.as_deref().is_some_and(|n| !n.is_empty()) {
            let record = self.prepared_record(&stmt);
            let mut prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
            // Re-preparing a name replaces the earlier statement of that name
            // (PostgreSQL refuses it with 42P05; the wire store here replaces,
            // and the catalog follows the store).
            prepared.retain(|r| r.name != record.name);
            prepared.push(record);
        }
        pgwire::api::store::PortalStore::put_statement(client.portal_store(), Arc::new(stmt));
        client
            .send(PgWireBackendMessage::ParseComplete(
                pgwire::messages::extendedquery::ParseComplete::new(),
            ))
            .await?;
        Ok(())
    }

    /// The default `Close`, plus dropping a closed STATEMENT's
    /// `pg_prepared_statements` row -- libpq 17+ closes a prepared statement
    /// with this message (`PQclosePrepared`) rather than `DEALLOCATE`.
    async fn on_close<C>(
        &self,
        client: &mut C,
        message: pgwire::messages::extendedquery::Close,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if message.target_type == pgwire::messages::extendedquery::TARGET_TYPE_BYTE_STATEMENT {
            if let Some(name) = message.name.as_deref().filter(|n| !n.is_empty()) {
                self.prepared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|r| r.name != name);
            }
        }
        let name = message.name.as_deref().unwrap_or("");
        match message.target_type {
            pgwire::messages::extendedquery::TARGET_TYPE_BYTE_STATEMENT => {
                pgwire::api::store::PortalStore::rm_statement(client.portal_store(), name);
            }
            pgwire::messages::extendedquery::TARGET_TYPE_BYTE_PORTAL => {
                pgwire::api::store::PortalStore::rm_portal(client.portal_store(), name);
                // A DECLAREd cursor IS a portal of that name on PostgreSQL, so
                // a wire `Close` of it closes the cursor (libpq 17's
                // `PQclosePortal`); the next Describe of it is `34000`.
                if !name.is_empty() {
                    self.cursors
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(name);
                }
            }
            _ => {}
        }
        client
            .send(PgWireBackendMessage::CloseComplete(
                pgwire::messages::extendedquery::CloseComplete::new(),
            ))
            .await?;
        Ok(())
    }

    async fn do_describe_statement<C>(
        &self,
        _c: &mut C,
        target: &StoredStatement<Self::Statement>,
    ) -> PgWireResult<DescribeStatementResponse>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let declared = &target.parameter_types;
        let param_types = self.param_type_names(target);
        let fields =
            self.describe_fields(&target.statement.sql, param_types.len(), &param_types)?;
        // A parameter with no mapped builtin type reports its user-type oid
        // when the raw Parse oid named one (a composite / enum); otherwise
        // the type the STATEMENT gives it -- `$1::int4` describes as int4 and
        // `$1 = 'a'` as text on PostgreSQL 16 -- and `unknown` only when
        // nothing in the statement types it, which is what PostgreSQL does
        // when it cannot infer.
        let column_type = |table: &str, column: secantus_pgplan::ColumnRef<'_>| {
            self.lookup(table).and_then(|def| {
                let col = match column {
                    secantus_pgplan::ColumnRef::Name(name) => def.column(name),
                    secantus_pgplan::ColumnRef::Position(pos) => def.columns.get(pos),
                };
                col.map(|c| c.pg_type.clone())
            })
        };
        let inferred = secantus_pgplan::catalog_param_types_opt(
            &target.statement.sql,
            &param_types,
            &column_type,
        );
        let types: Vec<Type> = (0..param_types.len())
            .map(|i| {
                declared.get(i).cloned().flatten().unwrap_or_else(|| {
                    target
                        .parameter_oids
                        .get(i)
                        .copied()
                        .filter(|o| *o != 0)
                        .and_then(|oid| self.user_wire_type_for_oid(oid))
                        .or_else(|| {
                            let name = inferred.get(i)?.as_deref()?;
                            let t = wire_type(name);
                            // `wire_type` answers varchar for a name it does
                            // not know; only a real varchar keeps that.
                            (t != Type::VARCHAR || matches!(name, "varchar" | "character varying"))
                                .then_some(t)
                        })
                        .unwrap_or(Type::UNKNOWN)
                })
            })
            .collect();
        Ok(match fields {
            Some(fields) => DescribeStatementResponse::new(types, fields),
            None => DescribeStatementResponse::no_data_with_parameters(types),
        })
    }

    /// PostgreSQL exposes a DECLAREd cursor as a PORTAL of the same name, and
    /// psycopg describes that portal straight after the DECLARE to learn the
    /// columns -- before it ever sends a `FETCH`. This server's cursors are
    /// its own, not pgwire portals, so the describe found nothing and every
    /// server cursor died on its first row with "portal not found".
    /// `Sync` ends the statement group the `Execute`s since the last one
    /// opened: commit it, or roll it back if any of them failed. A commit
    /// that fails (a deferred constraint) is the `Sync`'s error, and the
    /// `ReadyForQuery` after it says IDLE, as PostgreSQL's does.
    async fn on_sync<C>(&self, client: &mut C, _message: PgSync) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if let Err(e) = self.close_extended_group(false) {
            let info: ErrorInfo = e.into();
            client
                .send(PgWireBackendMessage::ErrorResponse(info.into()))
                .await?;
            client.set_transaction_status(pgwire::messages::response::TransactionStatus::Idle);
        }
        pgwire::api::store::PortalStore::rm_portal(client.portal_store(), DEFAULT_NAME);
        self.flush_notifications(client).await?;
        client
            .send(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                client.transaction_status(),
            )))
            .await?;
        client.flush().await?;
        Ok(())
    }

    async fn on_describe<C>(&self, client: &mut C, message: Describe) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if message.target_type == TARGET_TYPE_BYTE_PORTAL {
            let name = message.name.as_deref().unwrap_or(DEFAULT_NAME);
            // A real portal of that name wins: a cursor only answers for a
            // name the wire layer does not already know.
            if pgwire::api::store::PortalStore::get_portal(client.portal_store(), name).is_none() {
                let fields = self
                    .cursors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(name)
                    .map(|c| c.schema.as_ref().clone());
                if let Some(fields) = fields {
                    let response = DescribePortalResponse::new(fields);
                    return send_describe_response(client, &response).await;
                }
            }
        }
        self._on_describe(client, message).await
    }

    async fn do_describe_portal<C>(
        &self,
        _c: &mut C,
        target: &Portal<Self::Statement>,
    ) -> PgWireResult<DescribePortalResponse>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        self.note_result_format(&target.result_column_format);
        let param_types = self.param_type_names(target.statement.as_ref());
        let fields = self.describe_fields(
            &target.statement.statement.sql,
            param_types.len(),
            &param_types,
        )?;
        Ok(match fields {
            Some(fields) => DescribePortalResponse::new(fields),
            None => pgwire::api::results::DescribeResponse::no_data(),
        })
    }

    async fn do_query<C>(
        &self,
        _c: &mut C,
        portal: &Portal<Self::Statement>,
        max_rows: usize,
    ) -> PgWireResult<Response>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        self.note_result_format(&portal.result_column_format);
        let mut param_types = secantus_pgplan::infer_param_types(
            &portal.statement.statement.sql,
            &self.param_type_names(portal.statement.as_ref()),
        );
        self.type_untyped_binary_params(portal, &mut param_types);
        // Every statement between two `Sync`s runs in ONE transaction that
        // the `Sync` commits -- or rolls back, if any of them failed. A
        // pipelining client counts on the rollback: after an error, nothing
        // before it in the pipeline may have landed either.
        self.open_extended_group()?;
        let params = self
            .portal_params(portal, &param_types)
            .inspect_err(|_| self.note_failure())?;
        let result = self
            .run_typed(
                &portal.statement.statement.sql,
                &params,
                &param_types,
                max_rows,
            )
            .await
            .inspect_err(|_| self.note_failure());
        // Notices go out before the result -- or the error -- they preceded.
        self.flush_notices(_c).await?;
        self.settle_failed_commit(_c);
        let mut responses = result?;
        // Report any GUC change (TimeZone, DateStyle, ...) the statement made.
        self.report_pending_params(_c).await?;
        // One portal is one statement, so exactly one response.
        Ok(responses.remove(0))
    }
}

/// Parse one COPY text field into the value its column stores.
///
/// `\N` is NULL -- distinct from the empty string, which is a real empty text
/// value. That distinction is the whole reason COPY has an escape at all.
fn copy_field(
    raw: &str,
    pg_type: &str,
    wire: &Type,
    tz: &secantus_pgplan::TimeZoneSetting,
) -> PgWireResult<Bson> {
    if raw == "\\N" {
        return Ok(Bson::Null);
    }
    // An ARRAY, and every other type whose text form a bound parameter is
    // parsed from (bytea, uuid, inet / cidr, box, the datetime family, json /
    // jsonb): the SAME decoder a text-format Bind parameter goes through, so
    // a COPY'd `{a,b}` is stored as the array it is and a COPY'd date in its
    // canonical text. Stored as one raw string, a `text[]` column's `{ab}`
    // went back out as `"{ab}"` -- an array holding the literal -- which
    // psycopg reads as "malformed array: hit the end of the buffer".
    if matches!(wire.kind(), postgres_types::Kind::Array(_))
        || matches!(
            wire.oid(),
            17 | 2950 | 869 | 650 | 603 | 1082 | 1083 | 1114 | 1184 | 1266 | 1186 | 114 | 3802
        )
    {
        let bytes = Bytes::from(raw.to_string());
        return decode_parameter(Some(&bytes), Some(wire), false, tz, ClientEncoding::Utf8);
    }
    // A range or multirange is stored in its CANONICAL text, the same as a
    // literal or a bound parameter -- `{empty}` is `{}` and `[1,5]` over int4
    // is `[1,6)`. Stored raw, `{empty}` went back out as `{empty}`, which no
    // PostgreSQL ever prints and psycopg's loader cannot parse.
    if secantus_pgplan::range::is_range_type(pg_type)
        || secantus_pgplan::range::is_multirange_type(pg_type)
    {
        return secantus_pgplan::cast_text_to(raw, pg_type, tz).map_err(|e| PgHandler::err(&e));
    }
    // `raw` has already been backslash-unescaped by `copy_parse_text` /
    // `unescape_copy_text`. Unescaping again halved a literal `\\` a second time
    // and dropped the backslash from `\<letter>` values -- so the field is used
    // as-is here, and only interpreted per column type.
    let text = raw.to_string();
    let bad = |want: &str| {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "22P02".into(),
            format!("invalid input syntax for type {want}: \"{text}\""),
        )))
    };
    Ok(match pg_type {
        "int2" | "int4" | "integer" | "int" | "smallint" => {
            Bson::Int32(text.trim().parse().map_err(|_| bad("integer"))?)
        }
        "int8" | "bigint" => Bson::Int64(text.trim().parse().map_err(|_| bad("bigint"))?),
        "float4" | "float8" | "real" => {
            Bson::Double(text.trim().parse().map_err(|_| bad("double precision"))?)
        }
        // A numeric is exact: `9223372036854775807` through an f64 came back
        // as `9.223372036854776E+18`.
        "numeric" | "decimal" => {
            secantus_pgplan::parse_numeric(&text).map_err(|e| PgHandler::err(&e))?
        }
        "bool" | "boolean" => match text.trim() {
            "t" | "true" | "y" | "yes" | "on" | "1" => Bson::Boolean(true),
            "f" | "false" | "n" | "no" | "off" | "0" => Bson::Boolean(false),
            _ => return Err(bad("boolean")),
        },
        _ => Bson::String(text),
    })
}

#[async_trait]
impl CopyHandler for PgHandler {
    async fn on_copy_data<C>(&self, _c: &mut C, data: CopyData) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let mut guard = self.copy_in.lock().unwrap_or_else(|e| e.into_inner());
        // A chunk may split a row anywhere, so buffer and parse only at Done.
        match guard.as_mut() {
            Some(state) => state.buffer.extend_from_slice(&data.data),
            None => {
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "57014".into(),
                    "COPY data arrived with no COPY in progress".into(),
                ))))
            }
        }
        Ok(())
    }

    async fn on_copy_done<C>(&self, client: &mut C, _done: CopyDone) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let state = match self
            .copy_in
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            Some(s) => s,
            None => return Ok(()),
        };

        use secantus_pgplan::CopyFormat;
        // Every format is parsed into the same shape -- rows of optional
        // values, where `None` is that format's NULL -- so the insert below is
        // written once.
        let rows: Vec<Vec<Option<Bson>>> = match state.format {
            CopyFormat::Binary => self.parse_binary_copy(&state.buffer, &state.types)?,
            format => {
                let text = String::from_utf8(state.buffer.clone()).map_err(|_| {
                    PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "22021".into(), // character_not_in_repertoire
                        "COPY data is not valid UTF-8".into(),
                    )))
                })?;
                let parsed = copy_parse_text(&text, format);
                let tz = self.session_timezone();
                let mut out = Vec::with_capacity(parsed.len());
                for raw in parsed {
                    let mut row = Vec::with_capacity(raw.len());
                    for (i, value) in raw.into_iter().enumerate() {
                        row.push(match value {
                            None => None,
                            Some(text) => {
                                let ty = state.types.get(i).map(String::as_str).unwrap_or("text");
                                let wire = self.user_wire_type(ty).unwrap_or_else(|| wire_type(ty));
                                Some(copy_field(&text, ty, &wire, &tz)?)
                            }
                        });
                    }
                    out.push(row);
                }
                out
            }
        };

        let mut parsed_rows = Vec::with_capacity(rows.len());
        for raw in rows {
            if raw.len() != state.fields.len() {
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "22P04".into(), // bad_copy_file_format
                    format!(
                        "extra or missing columns for COPY (expected {}, got {})",
                        state.fields.len(),
                        raw.len()
                    ),
                ))));
            }
            let mut doc = Document::new();
            for (field, value) in state.fields.iter().zip(raw) {
                doc.insert(field.clone(), value.unwrap_or(Bson::Null));
            }
            parsed_rows.push(doc);
        }
        let written = parsed_rows.len();
        if !parsed_rows.is_empty() {
            let def = self
                .lookup(&state.table)
                .ok_or_else(|| Self::err(&PlanError::UndefinedTable(state.table.clone())))?;
            // INSIDE the open transaction, as every other write is. A COPY
            // whose rows were written outside it blocked against the
            // transaction's own locks and hung the connection -- which nobody
            // had seen, because resolving the table failed first whenever a
            // transaction was open.
            //
            // The DEFAULT fill is inside too: a column the COPY's column list
            // leaves out takes its default exactly as an INSERT's omitted
            // column does (`copy copy_in (col2, data)` fills a `serial` col1
            // from its sequence -- measured on 16), and advancing the
            // sequence is a write. Done outside the transaction it conflicted
            // with the transaction's own earlier `nextval` and spun on the
            // write-conflict retry until the client gave up.
            let load = || -> PgWireResult<_> {
                let mut rows = parsed_rows;
                self.apply_serial_defaults(&def, &mut rows)?;
                apply_column_defaults(&def, &mut rows);
                let docs = rows
                    .iter()
                    .map(bson::to_vec)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| Self::storage_err("could not encode a COPY row", e))?;
                self.storage
                    .insert(self.db(), &state.table, docs, true)
                    .map_err(|e| Self::storage_err("could not insert COPY rows", e))
            };
            let mut guard = self.txn.lock().unwrap_or_else(|e| e.into_inner());
            let (_, errors) = match guard.as_mut() {
                Some(handle) => self
                    .storage
                    .with_user_transaction(handle, load)
                    .map_err(|e| Self::storage_err("transaction failed", e))?,
                None => load(),
            }?;
            drop(guard);
            if let Some(first) = errors.first() {
                return Err(Self::write_error(&state.table, &def, first));
            }
        }

        // pgwire sends ReadyForQuery after this, but NOT CommandComplete --
        // without it the client waits for a result that never comes and
        // psycopg fails with "not enough values to unpack".
        client
            .feed(PgWireBackendMessage::CommandComplete(CommandComplete::new(
                format!("COPY {written}"),
            )))
            .await?;
        client.flush().await.map_err(PgWireError::from)?;
        Ok(())
    }

    async fn on_copy_fail<C>(&self, _c: &mut C, fail: CopyFail) -> PgWireError
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        // The client abandoned the COPY: drop everything buffered rather than
        // insert a partial load.
        *self.copy_in.lock().unwrap_or_else(|e| e.into_inner()) = None;
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "57014".into(), // query_canceled
            format!("COPY from stdin failed: {}", fail.message),
        )))
    }
}

pub struct HandlerFactory(pub Arc<PgHandler>);

impl PgWireServerHandlers for HandlerFactory {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.0.clone()
    }
    fn cancel_handler(&self) -> Arc<impl pgwire::api::cancel::CancelHandler> {
        Arc::new(CancelBackend)
    }
    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        self.0.clone()
    }
    fn copy_handler(&self) -> Arc<impl CopyHandler> {
        self.0.clone()
    }
    fn startup_handler(&self) -> Arc<impl pgwire::api::auth::StartupHandler> {
        self.0.clone()
    }
    fn idle_timeout(&self) -> Option<(std::time::Duration, ErrorInfo)> {
        self.0.idle_timeout()
    }

    fn idle_event(&self) -> Option<pgwire::api::IdleEventFuture<'_>> {
        let handler = &self.0;
        Some(Box::pin(async move {
            loop {
                if handler
                    .backend
                    .terminate
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    let info: ErrorInfo = PgHandler::admin_shutdown().into();
                    return pgwire::api::IdleEvent::Fatal(info);
                }
                let messages = handler.drain_notifications();
                if !messages.is_empty() {
                    return pgwire::api::IdleEvent::Send(messages);
                }
                handler.backend.wake.notified().await;
            }
        }))
    }
}

#[cfg(test)]
mod wire_format_tests {
    //! The hand-built binary layouts, pinned to the bytes PostgreSQL 16 sends
    //! (`scratchpad/binpin.py`, 2026-09-09, byte-identical on both servers).
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn range_binary_matches_range_send() {
        // flags 0x02 (lower inclusive), two 8-byte timestamp bounds. The
        // input is the STORED text (a bound with a space is quoted), which
        // is what `range_binary` is handed.
        assert_eq!(
            hex(&range_binary(
                "[\"2000-01-01 00:00:00\",\"2000-01-02 00:00:00\")",
                "tsrange"
            )
            .unwrap()),
            "0200000008000000000000000000000008000000141dd76000"
        );
        // An `infinity` timestamp bound is a PRESENT bound holding the wire
        // sentinel (flags 0x06), not an infinite-bound flag.
        assert_eq!(
            hex(&range_binary("[-infinity,infinity]", "tsrange").unwrap()),
            "06000000088000000000000000000000087fffffffffffffff"
        );
        assert_eq!(hex(&range_binary("empty", "int4range").unwrap()), "01");
        // Lower infinite (0x08) + upper inclusive (0x04) — the stored
        // canonical form of `(,5]` over int8 is `(,6)`, so flags are 0x08.
        assert_eq!(
            hex(&range_binary("(,6)", "int8range").unwrap()),
            "08000000080000000000000006"
        );
    }

    #[test]
    fn multirange_binary_matches_multirange_send() {
        assert_eq!(
            hex(&multirange_binary("{[1,3),[5,7)}", "int4multirange").unwrap()),
            "00000002000000110200000004000000010000000400000003\
             000000110200000004000000050000000400000007"
        );
        assert_eq!(
            hex(&multirange_binary("{}", "int4multirange").unwrap()),
            "00000000"
        );
    }

    #[test]
    fn an_empty_array_has_zero_dimensions() {
        // `array_send` writes ndim 0, no dimension pair: 12 bytes in all.
        assert_eq!(
            hex(&array_binary(&[], &Type::INT4).unwrap()),
            "000000000000000000000017"
        );
        assert_eq!(
            hex(&array_binary(&[], &Type::TEXT).unwrap()),
            "000000000000000000000019"
        );
        // A non-empty one still carries its dimension pair.
        assert_eq!(
            hex(&array_binary(&[Bson::Int32(1), Bson::Int32(2)], &Type::INT4).unwrap()),
            "000000010000000000000017000000020000000100000004000000010000000400000002"
        );
    }

    #[test]
    fn datetime_elements_match_their_send_functions() {
        assert_eq!(
            hex(&element_binary(&Bson::String("12:00:00+05:30".into()), &Type::TIMETZ).unwrap()),
            "0000000a0eebb000ffffb2a8"
        );
        assert_eq!(
            hex(&element_binary(&Bson::String("2000-01-02".into()), &Type::DATE).unwrap()),
            "00000001"
        );
        let one_day_two_hours = secantus_pgplan::Interval {
            months: 0,
            days: 1,
            micros: 7_200_000_000,
        }
        .to_bson();
        assert_eq!(
            hex(&element_binary(&one_day_two_hours, &Type::INTERVAL).unwrap()),
            "00000001ad2748000000000100000000"
        );
        // json[] / jsonb[] elements: the text verbatim, and `\x01` + the text.
        assert_eq!(
            hex(&element_binary(&Bson::String("{\"a\":1}".into()), &Type::JSON).unwrap()),
            "7b2261223a317d"
        );
        assert_eq!(
            hex(&element_binary(&Bson::String("{\"a\": 1}".into()), &Type::JSONB).unwrap()),
            "017b2261223a20317d"
        );
    }
}

#[cfg(test)]
mod idle_timeout_guc_tests {
    //! `SET idle_in_transaction_session_timeout` parsing and `SHOW` rendering,
    //! pinned to PostgreSQL 16 (probed 2026-09-09).
    use super::*;

    #[test]
    fn parses_units_floats_and_whitespace() {
        assert_eq!(parse_ms_guc("250"), Some(250));
        assert_eq!(parse_ms_guc("1min"), Some(60_000));
        assert_eq!(parse_ms_guc("1.5s"), Some(1_500));
        assert_eq!(parse_ms_guc("0.5min"), Some(30_000));
        assert_eq!(parse_ms_guc("1.2345s"), Some(1_234));
        assert_eq!(parse_ms_guc("  7  ms "), Some(7));
        assert_eq!(parse_ms_guc("1e3"), Some(1_000));
        assert_eq!(parse_ms_guc("100us"), Some(0));
        assert_eq!(parse_ms_guc("abc"), None);
        assert_eq!(parse_ms_guc("5 fortnights"), None);
    }

    #[test]
    fn renders_like_show() {
        assert_eq!(render_ms_guc(0), "0");
        assert_eq!(render_ms_guc(250), "250ms");
        assert_eq!(render_ms_guc(1_500), "1500ms");
        assert_eq!(render_ms_guc(2_000), "2s");
        assert_eq!(render_ms_guc(60_000), "1min");
        assert_eq!(render_ms_guc(7_200_000), "2h");
    }

    fn refusal(value: &str) -> String {
        match canonical_ms_guc("idle_in_transaction_session_timeout", value) {
            Err(PgWireError::UserError(info)) => {
                assert_eq!(info.code, "22023");
                info.message.clone()
            }
            other => panic!("expected a 22023 refusal, got {other:?}"),
        }
    }

    #[test]
    fn refuses_what_postgres_refuses() {
        assert_eq!(
            refusal("abc"),
            "invalid value for parameter \"idle_in_transaction_session_timeout\": \"abc\""
        );
        assert_eq!(
            refusal("-1"),
            "-1 ms is outside the valid range for parameter \
             \"idle_in_transaction_session_timeout\" (0 .. 2147483647)"
        );
        assert_eq!(
            refusal("2147483648"),
            "invalid value for parameter \"idle_in_transaction_session_timeout\": \"2147483648\""
        );
        assert_eq!(
            canonical_ms_guc("idle_session_timeout", "60000").unwrap(),
            "1min"
        );
    }
}
