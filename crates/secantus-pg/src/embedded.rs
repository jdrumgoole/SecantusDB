//! [`PgServer`]: the one-line embedding API, the PostgreSQL counterpart of
//! `secantus_mdb::Server`.
//!
//! It is the assembly a caller of [`bind`] would otherwise write by hand --
//! create the store's directory, [`open_storage_with_cache`], a
//! [`DatabaseRegistry`], [`bind`] -- plus a temporary store that is removed
//! once the server has stopped and the store is closed.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::server::{bind_tls, open_storage_with_cache, RunningPgServer};
use crate::{DatabaseRegistry, TlsConfig};

/// The WiredTiger cache cap for an embedded server. Smaller than the daemon's
/// 4G because a test suite starts many of these; WiredTiger fills it lazily
/// either way.
pub const DEFAULT_CACHE_SIZE: &str = "256M";

/// The database and user a client connects as by default, as on a fresh
/// PostgreSQL cluster.
pub const DEFAULT_DATABASE: &str = "postgres";

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

/// A running SecantusDB PostgreSQL server. Dropping it stops the server,
/// closes the store (WiredTiger's close-checkpoint) and, for a temporary
/// store, removes the store.
///
/// Safe to start and drop inside a `#[tokio::test]`, either flavour: the
/// server runs on a runtime of its own.
pub struct PgServer {
    running: Option<RunningPgServer>,
    address: SocketAddr,
    storage_path: PathBuf,
    temporary: bool,
}

impl PgServer {
    /// Start a server on `127.0.0.1` with an OS-assigned port and a temporary
    /// store that is removed when the server is dropped.
    pub fn start() -> Result<PgServer, Error> {
        PgServer::builder().start()
    }

    /// A builder for a server with a persistent store, a fixed port, extra
    /// databases or a different cache size.
    pub fn builder() -> PgBuilder {
        PgBuilder::default()
    }

    /// The bound address.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// A libpq key/value connection string (`host=... port=... dbname=postgres
    /// user=postgres`), the form `tokio_postgres::connect` and psycopg take.
    pub fn dsn(&self) -> String {
        format!(
            "host={} port={} dbname={DEFAULT_DATABASE} user={DEFAULT_DATABASE}",
            self.address.ip(),
            self.address.port()
        )
    }

    /// The same connection as a URL (`postgresql://postgres@host:port/postgres`),
    /// for clients that take only the URL form (sqlx, JDBC-style tooling).
    pub fn url(&self) -> String {
        format!(
            "postgresql://{DEFAULT_DATABASE}@{}/{DEFAULT_DATABASE}",
            self.address
        )
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
        let closed = running.store_closed();
        drop(running);
        if !self.temporary {
            return;
        }
        if !closed {
            // The drain gave up with the store still open (already reported
            // by `stop`). Removing files from under an open WiredTiger is how
            // a WT_PANIC happens, so leave the directory and say so.
            eprintln!(
                "secantus-pg: not removing {} -- the store is still open",
                self.storage_path.display()
            );
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(&self.storage_path) {
            eprintln!(
                "secantus-pg: could not remove {}: {e}",
                self.storage_path.display()
            );
        }
    }
}

impl Drop for PgServer {
    fn drop(&mut self) {
        self.stop();
    }
}

impl fmt::Debug for PgServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgServer")
            .field("address", &self.address)
            .field("storage_path", &self.storage_path)
            .field("temporary", &self.temporary)
            .field("running", &self.running.is_some())
            .finish()
    }
}

/// Configures a [`PgServer`]. Every setting has the default
/// [`PgServer::start`] uses.
#[derive(Debug, Clone)]
pub struct PgBuilder {
    storage_path: Option<PathBuf>,
    host: String,
    port: u16,
    databases: Vec<String>,
    cache_size: String,
    tls: Option<TlsConfig>,
}

impl Default for PgBuilder {
    fn default() -> Self {
        PgBuilder {
            storage_path: None,
            host: "127.0.0.1".to_string(),
            port: 0,
            databases: Vec::new(),
            cache_size: DEFAULT_CACHE_SIZE.to_string(),
            tls: None,
        }
    }
}

impl PgBuilder {
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

    /// Extra databases a client may connect to without a `CREATE DATABASE`
    /// first. `postgres` and `template1` always exist.
    pub fn databases<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.databases = names.into_iter().map(Into::into).collect();
        self
    }

    /// The WiredTiger cache cap, in WiredTiger's syntax (`"256M"`, `"1G"`).
    pub fn cache_size(mut self, size: impl Into<String>) -> Self {
        self.cache_size = size.into();
        self
    }

    /// Offer TLS with this PEM certificate chain and private key. A client
    /// that asks for TLS gets it; one that does not is still served in the
    /// clear. Without this the server answers a TLS request with "not
    /// supported", as PostgreSQL does with `ssl = off`.
    pub fn tls(mut self, cert_file: impl Into<PathBuf>, key_file: impl Into<PathBuf>) -> Self {
        self.tls = Some(TlsConfig::new(cert_file, key_file));
        self
    }

    /// Open the store and start serving.
    pub fn start(self) -> Result<PgServer, Error> {
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
            // Nothing holds the store open on the error path: a failed bind
            // dropped the storage, which closed it.
            let _ = std::fs::remove_dir_all(&storage_path);
        }
        started
    }

    fn start_in(&self, storage_path: &Path, temporary: bool) -> Result<PgServer, Error> {
        let home = storage_path
            .to_str()
            .ok_or_else(|| Error::Config(format!("{} is not UTF-8", storage_path.display())))?;
        let storage = open_storage_with_cache(home, &self.cache_size)
            .map_err(|e| Error::Storage(format!("{e:?}")))?;
        let databases = Arc::new(DatabaseRegistry::new(
            DEFAULT_DATABASE,
            self.databases.clone(),
        ));
        let addr = format!("{}:{}", self.host, self.port);
        let running =
            bind_tls(&addr, storage, databases, self.tls.as_ref()).map_err(|e| match e.kind() {
                // A certificate or key that could not be used, not a socket.
                std::io::ErrorKind::InvalidData => Error::Config(e.to_string()),
                _ => Error::Io(e),
            })?;
        Ok(PgServer {
            address: running.address(),
            running: Some(running),
            storage_path: storage_path.to_path_buf(),
            temporary,
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
            std::env::temp_dir().join(format!("secantus-pg-{}-{nanos}-{n}", std::process::id()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}
