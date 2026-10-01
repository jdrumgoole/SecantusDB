//! A surrogate MongoDB server you can start from a test.
//!
//! SecantusDB speaks the real MongoDB wire protocol on a real TCP socket, over
//! the same WiredTiger storage engine MongoDB ships, scoped to a single node.
//! [`Server`] runs one in-process: an application's tests connect to it with
//! the official `mongodb` driver instead of standing up a `mongod`.
//!
//! ```
//! let server = secantus_mdb::Server::start()?;
//! // Connect any driver to `server.uri()`, e.g.
//! // mongodb::sync::Client::with_uri_str(server.uri())
//! assert!(server.uri().starts_with("mongodb://127.0.0.1:"));
//! # Ok::<(), secantus_mdb::Error>(())
//! ```
//!
//! [`Server::start`] gives each server its own temporary store, removed when
//! the server is dropped, and an OS-assigned port, so any number can run in
//! parallel. [`Server::builder`] sets a persistent store, a port, auth, TLS and
//! the WiredTiger cache.
//!
//! Like the embedded Python handle, `start()` advertises a single-node replica
//! set named `secantus` (so drivers accept change streams and transactions)
//! and enables test commands (`configureFailPoint`), because a server started
//! in-process is a test's server. The `secantusd-rs` binary keeps `mongod`'s
//! defaults instead.
//!
//! Building this crate compiles WiredTiger from bundled source, which needs
//! CMake and a C compiler.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use secantus_commands::{CursorRegistry, Storage as CmdStorage};
pub use secantus_server::TlsOptions;
use secantus_server::{bind, RunningServer, ServerConfig};
use secantus_storage::{wt_config, Storage, StorageOptions};
use secantus_storage_adapter::StorageAdapter;

/// The replica-set name [`Server::start`] advertises, as the Python server does.
pub const DEFAULT_REPLICA_SET: &str = "secantus";

/// The WiredTiger cache cap for an embedded server. Smaller than the daemon's
/// 4G because a test suite starts many of these; WiredTiger fills it lazily
/// either way.
pub const DEFAULT_CACHE_SIZE: &str = "256M";

/// The crate version, which is the server's version (`buildInfo` reports it).
pub const VERSION: &str = secantus_server::VERSION;

/// Why a server could not start.
#[derive(Debug)]
pub enum Error {
    /// Creating the storage directory, or binding the socket, failed.
    Io(std::io::Error),
    /// WiredTiger refused to open the store.
    Storage(String),
    /// The builder was given an inconsistent configuration.
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Storage(e) => write!(f, "failed to open storage: {e}"),
            Error::Config(e) => write!(f, "invalid server configuration: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// A running SecantusDB MongoDB server. Dropping it stops the server and, for a
/// temporary store, removes the store.
pub struct Server {
    running: Option<RunningServer>,
    address: SocketAddr,
    storage_path: PathBuf,
    temporary: bool,
    /// Observes the store without keeping it open: after the server stops, a
    /// live reference means a connection thread outlived the drain, and the
    /// directory must not be removed under an open WiredTiger.
    storage: Weak<Storage>,
}

impl Server {
    /// Start a server on `127.0.0.1` with an OS-assigned port and a temporary
    /// store that is removed when the server is dropped.
    pub fn start() -> Result<Server, Error> {
        Server::builder().start()
    }

    /// A builder for a server with a persistent store, a fixed port, auth, TLS
    /// or a different cache size.
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// The bound address.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// A connection string for this server. It carries `directConnection=true`,
    /// so a driver talks to this node and never waits to discover the
    /// replica-set members it advertises.
    pub fn uri(&self) -> String {
        format!("mongodb://{}/?directConnection=true", self.address)
    }

    /// The store's directory.
    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }

    /// Stop the server: close the listener, drain the connections, and close
    /// the store (WiredTiger's final checkpoint). Called by `Drop`; calling it
    /// first makes the moment explicit. Idempotent.
    pub fn stop(&mut self) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        running.stop();
        // Dropping the server drops its reference to the store; the store
        // closes when the last reference goes.
        drop(running);
        if !self.temporary {
            return;
        }
        if self.storage.strong_count() > 0 {
            // A connection thread outlived stop()'s bounded drain and still has
            // the store open. Removing files from under an open WiredTiger is
            // how a WT_PANIC happens, so leave the directory and say so.
            eprintln!(
                "secantus-mdb: not removing {} -- the store is still open",
                self.storage_path.display()
            );
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(&self.storage_path) {
            eprintln!(
                "secantus-mdb: could not remove {}: {e}",
                self.storage_path.display()
            );
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("address", &self.address)
            .field("storage_path", &self.storage_path)
            .field("temporary", &self.temporary)
            .field("running", &self.running.is_some())
            .finish()
    }
}

/// Configures a [`Server`]. Every setting has the default [`Server::start`]
/// uses.
#[derive(Debug, Clone)]
pub struct Builder {
    storage_path: Option<PathBuf>,
    host: String,
    port: u16,
    replica_set: Option<String>,
    auth: bool,
    tls: Option<TlsOptions>,
    cache_size: String,
    test_commands: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Builder {
            storage_path: None,
            host: "127.0.0.1".to_string(),
            port: 0,
            replica_set: Some(DEFAULT_REPLICA_SET.to_string()),
            auth: false,
            tls: None,
            cache_size: DEFAULT_CACHE_SIZE.to_string(),
            test_commands: true,
        }
    }
}

impl Builder {
    /// Keep the store at `path` (created if missing) and leave it there when
    /// the server is dropped. Without this the store is temporary.
    pub fn storage_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.storage_path = Some(path.into());
        self
    }

    /// The interface to bind. Default `127.0.0.1`.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// The port to bind. Default `0`, an OS-assigned port, which is the only
    /// race-free choice when tests run in parallel.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// The replica-set name to advertise, or `None` for a plain standalone.
    /// Change streams and transactions need a replica set.
    pub fn replica_set(mut self, name: Option<&str>) -> Self {
        self.replica_set = name.map(str::to_string);
        self
    }

    /// Require authentication (SCRAM-SHA-256).
    pub fn auth(mut self, on: bool) -> Self {
        self.auth = on;
        self
    }

    /// Serve TLS.
    pub fn tls(mut self, tls: TlsOptions) -> Self {
        self.tls = Some(tls);
        self
    }

    /// The WiredTiger cache cap, in WiredTiger's syntax (`"256M"`, `"1G"`).
    pub fn cache_size(mut self, size: impl Into<String>) -> Self {
        self.cache_size = size.into();
        self
    }

    /// Accept test-only commands such as `configureFailPoint`. Default on.
    pub fn test_commands(mut self, on: bool) -> Self {
        self.test_commands = on;
        self
    }

    /// Open the store and start serving.
    pub fn start(self) -> Result<Server, Error> {
        if self.cache_size.trim().is_empty() || self.cache_size.contains([',', '(', ')', '=']) {
            return Err(Error::Config(format!(
                "cache_size {:?} is not a WiredTiger size",
                self.cache_size
            )));
        }
        let (storage_path, temporary) = match self.storage_path.clone() {
            Some(p) => {
                std::fs::create_dir_all(&p)?;
                (p, false)
            }
            None => (temporary_dir()?, true),
        };
        let started = self.start_in(&storage_path, temporary);
        if started.is_err() && temporary {
            // Nothing holds the store open on the error path.
            let _ = std::fs::remove_dir_all(&storage_path);
        }
        started
    }

    fn start_in(&self, storage_path: &Path, temporary: bool) -> Result<Server, Error> {
        let home = storage_path
            .to_str()
            .ok_or_else(|| Error::Config(format!("{} is not UTF-8", storage_path.display())))?;
        let storage = Storage::open_with_options(
            home,
            &StorageOptions {
                // The embedded handle's 128MB log: many short-lived stores must
                // not each carry a large sparse log file.
                wt_config: Some(wt_config(&self.cache_size, 1000, false, "128MB")),
                ..StorageOptions::default()
            },
        )
        .map_err(|e| Error::Storage(e.to_string()))?;
        let storage = Arc::new(storage);
        let weak = Arc::downgrade(&storage);
        let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(storage));
        let config = ServerConfig {
            replica_set_name: self.replica_set.clone(),
            require_auth: self.auth,
            tls: self.tls.clone(),
            enable_test_commands: self.test_commands,
            ..ServerConfig::default()
        };
        let addr = format!("{}:{}", self.host, self.port);
        let running = bind(&addr, config, adapter, Arc::new(CursorRegistry::new()))?;
        Ok(Server {
            address: running.address(),
            running: Some(running),
            storage_path: storage_path.to_path_buf(),
            temporary,
            storage: weak,
        })
    }
}

/// A new, empty directory under the system temp dir, unique to this process
/// and call.
fn temporary_dir() -> std::io::Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("secantus-mdb-{}-{nanos}-{n}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}
