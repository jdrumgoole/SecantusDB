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
//! (`secantus_pgserver::bind`) -- same contract, same shape: Python starts and
//! stops it, psycopg talks to it over real TCP, and no statement ever enters
//! Python. It is behind the default-on `pgserver` cargo feature so a build that
//! does not want the libpg_query / pgwire tree can drop it.

// Fast global allocator for the extension. BSON materialization drives heavy
// alloc/free churn (Finding 1); mimalloc cuts that overhead across all paths.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use secantus_commands::{CursorRegistry, Storage as CmdStorage};
use secantus_server::{bind, RunningServer, ServerConfig};
use secantus_storage::{wt_config, Storage, StorageOptions};
use secantus_storage_adapter::StorageAdapter;

#[cfg(feature = "pgserver")]
use secantus_pgserver::{bind as pg_bind, DatabaseRegistry, RunningPgServer};

/// An in-process handle to a running Rust SecantusDB server. Constructing it
/// binds a socket and starts the accept loop; `stop()` (or `__exit__` / drop)
/// shuts it down.
#[pyclass(name = "RustServer")]
struct RustServer {
    running: Option<RunningServer>,
    host: String,
    port: u16,
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
    ) -> PyResult<Self> {
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
                // Embedded handle keeps the 128MB log default (many ephemeral in-process
                // instances in a test suite must not each carry a big sparse log); the
                // standalone daemon opts into 2GB for write throughput.
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

        let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(Arc::new(storage)));
        let cursors = Arc::new(CursorRegistry::new());
        let config = ServerConfig {
            replica_set_name,
            require_auth,
            tls,
            ..ServerConfig::default()
        };
        let addr = format!("{host}:{port}");
        let running = bind(&addr, config, adapter, cursors)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to bind {addr}: {e}")))?;
        let bound = running.address();
        Ok(RustServer {
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
/// The handle OWNS the `Storage` it opens -- `secantus_pgserver::bind` takes it
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
    #[new]
    #[pyo3(signature = (
        storage_path,
        port = 0,
        host = "127.0.0.1".to_string(),
        databases = None,
    ))]
    fn new(
        storage_path: &str,
        port: u16,
        host: String,
        databases: Option<Vec<String>>,
    ) -> PyResult<Self> {
        // WiredTiger requires the home directory to exist; create it so any
        // path "just works" (matching the one-or-two-line ergonomic).
        std::fs::create_dir_all(storage_path).map_err(|e| {
            PyRuntimeError::new_err(format!("failed to create storage dir {storage_path}: {e}"))
        })?;
        let storage = Storage::open(storage_path)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to open storage: {e:?}")))?;
        let registry = Arc::new(DatabaseRegistry::new(
            "postgres",
            databases.unwrap_or_default(),
        ));
        let addr = format!("{host}:{port}");
        let running = pg_bind(&addr, storage, registry)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to bind {addr}: {e}")))?;
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

#[cfg(feature = "pgserver")]
impl Drop for PgServer {
    fn drop(&mut self) {
        // Not `stop(py)`: `Drop` has no GIL token, and `RunningPgServer::drop`
        // would run the same shutdown anyway. Taking it here keeps the
        // "stop exactly once" invariant explicit.
        if let Some(mut running) = self.running.take() {
            running.stop();
        }
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
    m.add_class::<RustServer>()?;
    #[cfg(feature = "pgserver")]
    m.add_class::<PgServer>()?;
    // Whether this build carries the PostgreSQL-wire server, so a test can say
    // "skipped because the extension was built without it" rather than
    // "PgServer is missing and I don't know why".
    m.add("HAS_PGSERVER", cfg!(feature = "pgserver"))?;
    Ok(())
}
