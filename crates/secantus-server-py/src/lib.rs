//! `_secantus_server` — R6: the thin embedded Python lifecycle handle over the
//! Rust server.
//!
//! This is the *only* Python-facing surface of the Rust server: `start` (the
//! constructor) / `stop` / `address` / `uri` + the context-manager protocol —
//! lifecycle, **not** operators. The accept loop runs on a GIL-released Rust
//! thread inside the Python process (spawned by `secantus_server::bind`), and a
//! `pymongo` client connects over real TCP. Python is the launcher; it is never
//! in the request path (cf. `tasks/rust-server-plan.md` §2).
//!
//! The constructor opens a WiredTiger-backed `secantus_storage::Storage`, wraps
//! it in the `StorageAdapter` (R4b) to satisfy the command `Storage` trait, and
//! binds the server. Because it links WiredTiger, this crate builds only where WT
//! is available (the wheel's CMake / local maturin), never the WT-less `rust` CI.
//!
//! `PgServer` is the same handle for the Rust **PostgreSQL**-wire server
//! (`secantus_pg::bind`) -- same contract, same shape: Python starts and
//! stops it, psycopg talks to it over real TCP, and no statement ever enters
//! Python. It is behind the default-on `pgserver` cargo feature so a build that
//! does not want the libpg_query / pgwire tree can drop it.

// Fast global allocator for the extension. BSON materialization drives heavy
// alloc/free churn (Finding 1); mimalloc cuts that overhead across all paths.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use secantus_commands::{CursorRegistry, Storage as CmdStorage};
use secantus_server::{bind, RunningServer, ServerConfig};
use secantus_storage::{wt_config, Storage, StorageOptions};
use secantus_storage_adapter::StorageAdapter;

#[cfg(feature = "pgserver")]
use secantus_pg::{
    bind_with as pg_bind_with, DatabaseRegistry, RunningPgServer, ServerOptions, TlsConfig,
};

/// An in-process handle to a running Rust SecantusDB server. Constructing it
/// binds a socket and starts the accept loop; `stop()` (or `__exit__` / drop)
/// shuts it down.
#[pyclass(name = "RustServer")]
struct RustServer {
    running: Option<RunningServer>,
    host: String,
    port: u16,
    /// The TTL monitor's ticker: its stop flag and thread. It holds the
    /// store open, so it is stopped with the server and on drop.
    ttl_monitor: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

impl RustServer {
    fn stop_ttl_monitor(&mut self) {
        if let Some((stop, ticker)) = self.ttl_monitor.take() {
            stop.store(true, Ordering::Relaxed);
            if ticker.join().is_err() {
                eprintln!("_secantus_server: the TTL monitor thread panicked");
            }
        }
    }
}

impl Drop for RustServer {
    fn drop(&mut self) {
        self.stop_ttl_monitor();
    }
}

#[pymethods]
impl RustServer {
    /// Open the database at `storage_path` and start the server.
    ///
    /// * `port` — `0` (default) lets the OS assign an ephemeral port; read it
    ///   back from `address` / `uri`.
    /// * `replica_set_name` — `Some` advertises the single-node `secantus`
    ///   replica set in `hello` (so change streams are accepted); `None` is a
    ///   plain standalone.
    /// * `require_auth` — when `True`, access control is on: non-handshake
    ///   commands require an authenticated principal (provision users with
    ///   `createUser` over an initially-open admin connection, or pre-seed the
    ///   store) and are checked against the principal's RBAC role grants.
    /// * `tls_cert_file` / `tls_key_file` — enable server-side TLS (both or
    ///   neither). `tls_ca_file` (+ `tls_require_client_cert`) layers on mTLS
    ///   client-certificate verification.
    /// * `cache_size` / `session_max` / `sync_on_commit` — the WiredTiger
    ///   knobs the daemons expose (`--cache-size` / `--session-max` /
    ///   `--sync-on-commit`), threaded into `wt_config`. Defaults match
    ///   `python -m secantus` and the standalone `secantusd-rs` binary
    ///   (4G cache cap — WiredTiger fills it lazily, so idle test servers stay small — 1000 sessions, no per-commit fsync).
    /// * `oplog_async` / `oplog_nonlogged` / `data_nonlogged` /
    ///   `checkpoint_seconds` — the storage write-path modes, per store:
    ///   background oplog drainer, non-logged oplog tables, and the
    ///   log-only-the-oplog data mode with its stable-checkpoint cadence.
    ///   `None` (the default) defers to the matching `SECANTUS_*` env var, so
    ///   env-driven workflows are unchanged; an explicit value wins over the
    ///   environment for THIS server only. An existing store's recorded
    ///   data-logging mode always wins over `data_nonlogged` (the table
    ///   config is create-time-sticky).
    /// * `ttl_sweep_seconds` — how often the TTL monitor deletes expired
    ///   documents. `None` (the default) starts it switched off, as mongod's
    ///   is with `ttlMonitorEnabled=false`, so a test's documents stay put
    ///   unless it asks: `setParameter ttlMonitorEnabled: true` turns it on,
    ///   at mongod's 60-second cadence or `ttlMonitorSleepSecs`.
    /// * `noop_heartbeat_seconds` — how often a noop entry is written to the
    ///   oplog, which keeps a quiet change stream's resume token inside the
    ///   oplog's window and prunes the oplog to its bounds, as the daemon's
    ///   `--noop-heartbeat-seconds` does. `None` (the default) writes none.
    #[new]
    #[pyo3(signature = (
        storage_path,
        port = 0,
        host = "127.0.0.1".to_string(),
        replica_set_name = None,
        enable_oplog = true,
        require_auth = false,
        tls_cert_file = None,
        tls_key_file = None,
        tls_ca_file = None,
        tls_require_client_cert = false,
        cache_size = "4G".to_string(),
        session_max = 1000,
        sync_on_commit = false,
        oplog_async = None,
        oplog_nonlogged = None,
        data_nonlogged = None,
        checkpoint_seconds = None,
        ttl_sweep_seconds = None,
        noop_heartbeat_seconds = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        storage_path: &str,
        port: u16,
        host: String,
        replica_set_name: Option<String>,
        enable_oplog: bool,
        require_auth: bool,
        tls_cert_file: Option<String>,
        tls_key_file: Option<String>,
        tls_ca_file: Option<String>,
        tls_require_client_cert: bool,
        cache_size: String,
        session_max: u32,
        sync_on_commit: bool,
        oplog_async: Option<bool>,
        oplog_nonlogged: Option<bool>,
        data_nonlogged: Option<bool>,
        checkpoint_seconds: Option<u64>,
        ttl_sweep_seconds: Option<f64>,
        noop_heartbeat_seconds: Option<f64>,
    ) -> PyResult<Self> {
        let heartbeat = match noop_heartbeat_seconds {
            Some(secs) if !(secs.is_finite() && secs > 0.0) => {
                return Err(PyValueError::new_err(
                    "noop_heartbeat_seconds must be positive; use None to write none",
                ))
            }
            other => other.map(Duration::from_secs_f64),
        };
        let ttl_period = match ttl_sweep_seconds {
            Some(secs) if !(secs.is_finite() && secs > 0.0) => {
                return Err(PyValueError::new_err(
                    "ttl_sweep_seconds must be positive; use None to start the monitor off",
                ))
            }
            Some(secs) => Duration::from_secs_f64(secs),
            None => Duration::from_secs(60),
        };
        // WiredTiger requires the home directory to exist; create it so any
        // path "just works" (matching the one-or-two-line ergonomic).
        std::fs::create_dir_all(storage_path).map_err(|e| {
            PyRuntimeError::new_err(format!("failed to create storage dir {storage_path}: {e}"))
        })?;
        // Defaults match `python -m secantus`, the Python `SecantusDBServer`,
        // the standalone `secantusdb` binary, AND the engine's own
        // `Storage::open` (all 4G cache cap — WiredTiger fills lazily). Each
        // knob is overridable per handle so tests can exercise non-default
        // WiredTiger configs.
        let mut storage = Storage::open_with_options(
            storage_path,
            &StorageOptions {
                // 128MB, the standalone daemon's default too since 2026-10-13: a
                // log file is allocated in full, and a test suite opens many stores.
                wt_config: Some(wt_config(&cache_size, session_max, sync_on_commit, "128MB")),
                oplog_async,
                oplog_nonlogged,
                data_nonlogged,
                checkpoint_seconds,
                ..StorageOptions::default()
            },
        )
        .map_err(|e| PyRuntimeError::new_err(format!("failed to open storage: {e:?}")))?;
        storage.set_enable_oplog(enable_oplog);
        storage.set_ttl_started_period(ttl_period);
        if ttl_sweep_seconds.is_none() {
            storage.set_ttl_monitor(None, Some(false));
        }
        let storage = Arc::new(storage);

        // TLS: cert + key both required to enable it (matching server.py).
        let tls = match (tls_cert_file, tls_key_file) {
            (Some(cert_file), Some(key_file)) => Some(secantus_server::TlsOptions {
                cert_file,
                key_file,
                ca_file: tls_ca_file,
                require_client_cert: tls_require_client_cert,
            }),
            (None, None) => None,
            _ => {
                return Err(PyRuntimeError::new_err(
                    "tls_cert_file and tls_key_file must both be set or both be None",
                ))
            }
        };

        let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(storage.clone()));
        let cursors = Arc::new(CursorRegistry::new());
        let config = ServerConfig {
            replica_set_name,
            require_auth,
            tls,
            // ON for the EMBEDDED handle, mirroring `SecantusDBServer`'s own
            // default: constructing a server in-process is a test doing it, and
            // every driver failpoint suite needs `configureFailPoint`. The
            // standalone `secantusd-rs` daemon keeps mongod's default of OFF —
            // that is the one an operator exposes on a port.
            enable_test_commands: true,
            ..ServerConfig::default()
        };
        let addr = format!("{host}:{port}");
        let running = bind(&addr, config, adapter, cursors)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to bind {addr}: {e}")))?;
        let bound = running.address();
        // Ticked often; the monitor decides whether a pass is due (and does
        // nothing while switched off), so `setParameter` applies at once.
        let stop_ttl = Arc::new(AtomicBool::new(false));
        // The same thread writes the noop heartbeat when one is due.
        let ticker = {
            let stop = stop_ttl.clone();
            let tick = ttl_period
                .min(heartbeat.unwrap_or(Duration::MAX))
                .min(Duration::from_millis(500));
            let mut beat = std::time::Instant::now();
            std::thread::spawn(move || loop {
                std::thread::sleep(tick);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = storage.ttl_monitor_tick(ttl_period) {
                    eprintln!("_secantus_server: TTL sweep failed: {e:?}");
                }
                if heartbeat.is_some_and(|every| beat.elapsed() >= every) {
                    beat = std::time::Instant::now();
                    if let Err(e) = storage.emit_noop_heartbeat() {
                        eprintln!("_secantus_server: noop heartbeat failed: {e:?}");
                    }
                    if let Err(e) = storage.prune_oplog(None) {
                        eprintln!("_secantus_server: oplog prune failed: {e:?}");
                    }
                }
            })
        };
        Ok(RustServer {
            running: Some(running),
            host: bound.ip().to_string(),
            port: bound.port(),
            ttl_monitor: Some((stop_ttl, ticker)),
        })
    }

    /// The bound `(host, port)`.
    #[getter]
    fn address(&self) -> (String, u16) {
        (self.host.clone(), self.port)
    }

    /// A `mongodb://host:port` connection URI.
    #[getter]
    fn uri(&self) -> String {
        format!("mongodb://{}:{}", self.host, self.port)
    }

    /// The Rust server's embedded version (the `secantus-server` crate version,
    /// bumped in lockstep across the Rust crates). This is the Rust server's own
    /// version line — independent of the `secantus` PyPI package's `0.5.2bN`.
    #[getter]
    fn version(&self) -> &'static str {
        secantus_server::VERSION
    }

    /// Stop the server (idempotent). The GIL is released while the accept loop
    /// is joined.
    fn stop(&mut self, py: Python<'_>) {
        if let Some(mut running) = self.running.take() {
            py.detach(|| running.stop());
        }
        py.detach(|| self.stop_ttl_monitor());
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __exit__(
        &mut self,
        py: Python<'_>,
        _exc_type: &Bound<'_, PyAny>,
        _exc_value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) -> bool {
        self.stop(py);
        false // don't suppress exceptions
    }
}

/// An in-process handle to a running Rust SecantusDB **PostgreSQL**-wire server
/// (`secantusd-pg`'s engine). Constructing it opens the store and binds a
/// socket; `stop()` (or `__exit__` / drop) shuts it down.
///
/// The handle OWNS the `Storage` it opens -- `secantus_pg::bind` takes it
/// by value for exactly this reason. WiredTiger's close-checkpoint runs when
/// that last reference is dropped, which `stop()` does after draining the
/// connections; a design where the caller could hold a second reference would
/// leave the checkpoint quietly unrun and lose every acknowledged write since
/// the previous one. So there is deliberately no way to reach the store from
/// Python: `stop()` is the only thing that ends its life, and it always
/// checkpoints.
#[cfg(feature = "pgserver")]
#[pyclass(name = "PgServer")]
struct PgServer {
    running: Option<RunningPgServer>,
    host: String,
    port: u16,
}

#[cfg(feature = "pgserver")]
#[pymethods]
impl PgServer {
    /// Open the database at `storage_path` and start the PostgreSQL server.
    ///
    /// * `port` -- `0` (default) lets the OS assign an ephemeral port; read it
    ///   back from `port` / `address` / `dsn`. Binding `0` is also the only
    ///   race-free option under `pytest -n auto`: probing for a free port and
    ///   passing it in leaves a window in which another worker can take it.
    /// * `databases` -- extra database names a client may connect to without a
    ///   `CREATE DATABASE` first. `postgres` / `template1` always exist.
    /// * `tls_cert_file` / `tls_key_file` -- offer TLS with this certificate
    ///   and key (both or neither), as `secantusd-pg --tls-cert-file`.
    /// * `tls_ca_file` -- the CAs a client certificate must chain to, and
    ///   `tls_client_cert` what a client must send: `"verify-ca"`,
    ///   `"verify-full"` or `"cert"`.
    /// * `tls_require` -- refuse a connection that is not TLS.
    /// * `trust_unknown_users` -- let a user there is no role for connect
    ///   without a password, as a role with no password does. By default
    ///   such a user is refused, as PostgreSQL refuses a role that does not
    ///   exist: connect as `postgres`, create the role, or set this.
    /// * `refuse_unknown_users` -- the old opt-in for that default; accepted
    ///   and changes nothing.
    /// * `tls_crl_file` -- PEM revocation lists a client certificate is
    ///   checked against (`ssl_crl_file`).
    /// * `tls_client_name` -- `"CN"` (default) or `"DN"`: what of a client
    ///   certificate names the user (`clientname=`).
    /// * `tls_ident_file` / `tls_user_map` -- a `pg_ident.conf` and the map
    ///   in it that turns the certificate's name into a user (both or
    ///   neither), as `secantusd-pg --tls-ident-file --tls-user-map`.
    /// * `hba_file` / `ident_file` -- a `pg_hba.conf`, whose lines then decide
    ///   every login, and the `pg_ident.conf` its `map=` options name. The
    ///   file takes the place of `tls_require`, `tls_client_cert` and
    ///   `trust_unknown_users`; giving one beside it is an error.
    #[new]
    #[pyo3(signature = (
        storage_path,
        port = 0,
        host = "127.0.0.1".to_string(),
        databases = None,
        tls_cert_file = None,
        tls_key_file = None,
        tls_ca_file = None,
        tls_client_cert = None,
        tls_require = false,
        refuse_unknown_users = false,
        hba_file = None,
        ident_file = None,
        tls_crl_file = None,
        tls_client_name = None,
        tls_ident_file = None,
        tls_user_map = None,
        trust_unknown_users = false,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        storage_path: &str,
        port: u16,
        host: String,
        databases: Option<Vec<String>>,
        tls_cert_file: Option<String>,
        tls_key_file: Option<String>,
        tls_ca_file: Option<String>,
        tls_client_cert: Option<String>,
        tls_require: bool,
        refuse_unknown_users: bool,
        hba_file: Option<String>,
        ident_file: Option<String>,
        tls_crl_file: Option<String>,
        tls_client_name: Option<String>,
        tls_ident_file: Option<String>,
        tls_user_map: Option<String>,
        trust_unknown_users: bool,
    ) -> PyResult<Self> {
        // The options are checked before the store is opened, so a mistake in
        // them costs no WiredTiger open and leaves no directory behind.
        let tls = match (tls_cert_file, tls_key_file) {
            (Some(cert), Some(key)) => {
                let mut tls = TlsConfig::new(cert, key).require_tls(tls_require);
                if let Some(ca) = tls_ca_file {
                    tls = tls.ca_file(ca);
                }
                if let Some(mode) = tls_client_cert {
                    let mode = mode
                        .parse()
                        .map_err(|e| PyValueError::new_err(format!("tls_client_cert: {e}")))?;
                    tls = tls.client_cert(mode);
                }
                if let Some(crl) = tls_crl_file {
                    tls = tls.crl_file(crl);
                }
                if let Some(name) = tls_client_name {
                    let name = name
                        .parse()
                        .map_err(|e| PyValueError::new_err(format!("tls_client_name: {e}")))?;
                    tls = tls.client_name(name);
                }
                match (tls_ident_file, tls_user_map) {
                    (Some(file), Some(map)) => tls = tls.user_map(file, map),
                    (Some(_), None) => {
                        return Err(PyValueError::new_err("tls_ident_file needs tls_user_map"))
                    }
                    (None, Some(_)) => {
                        return Err(PyValueError::new_err("tls_user_map needs tls_ident_file"))
                    }
                    (None, None) => {}
                }
                Some(tls)
            }
            (None, None) => {
                if tls_ca_file.is_some()
                    || tls_client_cert.is_some()
                    || tls_require
                    || tls_crl_file.is_some()
                    || tls_client_name.is_some()
                    || tls_ident_file.is_some()
                    || tls_user_map.is_some()
                {
                    return Err(PyValueError::new_err(
                        "tls_ca_file, tls_client_cert, tls_require, tls_crl_file, \
                         tls_client_name, tls_ident_file and tls_user_map need \
                         tls_cert_file and tls_key_file",
                    ));
                }
                None
            }
            _ => {
                return Err(PyValueError::new_err(
                    "tls_cert_file and tls_key_file go together: give both or neither",
                ))
            }
        };
        let mut options = ServerOptions::default()
            .tls(tls)
            .trust_unknown_users(trust_unknown_users);
        // The default since 0.1.0-beta.17: accepted so a caller that asked
        // for it still starts.
        let _ = refuse_unknown_users;
        if let Some(path) = hba_file {
            options = options.hba_file(path);
        }
        if let Some(path) = ident_file {
            options = options.ident_file(path);
        }
        // WiredTiger requires the home directory to exist; create it so any
        // path "just works" (matching the one-or-two-line ergonomic).
        std::fs::create_dir_all(storage_path).map_err(|e| {
            PyRuntimeError::new_err(format!("failed to create storage dir {storage_path}: {e}"))
        })?;
        // The PG server's own open: a per-commit log sync in durable mode, so
        // an acknowledged COMMIT survives a process kill (as `secantusd-pg`).
        let storage = secantus_pg::open_storage(storage_path)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to open storage: {e:?}")))?;
        let registry = Arc::new(DatabaseRegistry::new(
            "postgres",
            databases.unwrap_or_default(),
        ));
        let addr = format!("{host}:{port}");
        // The login files are read inside `bind_with`, before the socket is
        // bound, so a file that cannot be used is this error and never a
        // server that came up without what it was asked for.
        let running = pg_bind_with(&addr, storage, registry, &options)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to start on {addr}: {e}")))?;
        let bound = running.address();
        Ok(PgServer {
            running: Some(running),
            host: bound.ip().to_string(),
            port: bound.port(),
        })
    }

    /// The bound `(host, port)`.
    #[getter]
    fn address(&self) -> (String, u16) {
        (self.host.clone(), self.port)
    }

    /// The bound port (kernel-assigned when the server was started on `0`).
    #[getter]
    fn port(&self) -> u16 {
        self.port
    }

    /// A libpq connection string psycopg can be handed directly. Matches what
    /// the psycopg gauge and the slice tests build by hand.
    #[getter]
    fn dsn(&self) -> String {
        format!(
            "host={} port={} dbname=postgres user=postgres",
            self.host, self.port
        )
    }

    /// The Rust server's embedded version -- the same line `RustServer.version`
    /// reports, since the crates are bumped in lockstep.
    #[getter]
    fn version(&self) -> &'static str {
        secantus_server::VERSION
    }

    /// Read the certificate, key, CA file and `pg_hba.conf` / `pg_ident.conf`
    /// again, as SIGHUP does for `secantusd-pg`. A connection made afterwards
    /// gets what they hold now. On an error the server keeps what it had.
    fn reload(&self) -> PyResult<()> {
        let running = self
            .running
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("the server is stopped"))?;
        running
            .reloader()
            .reload()
            .map_err(|e| PyRuntimeError::new_err(format!("reload failed: {e}")))
    }

    /// Stop the server and close the store, checkpointing WiredTiger
    /// (idempotent). The GIL is released while connections drain.
    fn stop(&mut self, py: Python<'_>) {
        if let Some(mut running) = self.running.take() {
            py.detach(|| running.stop());
        }
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    // No explicit `__del__` / `Drop`: dropping the handle drops the
    // `RunningPgServer`, whose own `Drop` runs the same shutdown (and the
    // close-checkpoint). Same arrangement as `RustServer`.
    fn __exit__(
        &mut self,
        py: Python<'_>,
        _exc_type: &Bound<'_, PyAny>,
        _exc_value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) -> bool {
        self.stop(py);
        false // don't suppress exceptions
    }
}

#[pymodule]
fn _secantus_server(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add(
        "__doc__",
        "Embedded lifecycle handle for the SecantusDB Rust server: start / stop \
         an in-process Rust server (WiredTiger-backed) that pymongo connects to \
         over TCP. Python is only the launcher, never in the request path.",
    )?;
    // Module-level `__version__` so `_secantus_server.__version__` reports the
    // embedded Rust server version without having to start a server.
    m.add("__version__", secantus_server::VERSION)?;
    // The git tree hash of the crates this extension was built from, so a test
    // run can tell whether the installed `.so` matches the checkout. Empty when
    // built without git history (an sdist, a build container), in which case the
    // collection-time check abstains rather than failing the run.
    m.add("__source_tree__", env!("SECANTUS_SOURCE_TREE"))?;
    m.add_class::<RustServer>()?;
    #[cfg(feature = "pgserver")]
    m.add_class::<PgServer>()?;
    // Whether this build carries the PostgreSQL-wire server, so a test can say
    // "skipped because the extension was built without it" rather than
    // "PgServer is missing and I don't know why".
    m.add("HAS_PGSERVER", cfg!(feature = "pgserver"))?;
    Ok(())
}
