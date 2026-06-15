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

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use secantus_commands::{CursorRegistry, Storage as CmdStorage};
use secantus_server::{bind, RunningServer, ServerConfig};
use secantus_storage::Storage;
use secantus_storage_adapter::StorageAdapter;

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
    ) -> PyResult<Self> {
        // WiredTiger requires the home directory to exist; create it so any
        // path "just works" (matching the one-or-two-line ergonomic).
        std::fs::create_dir_all(storage_path).map_err(|e| {
            PyRuntimeError::new_err(format!("failed to create storage dir {storage_path}: {e}"))
        })?;
        let mut storage = Storage::open(storage_path)
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
            py.allow_threads(|| running.stop());
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
    Ok(())
}
