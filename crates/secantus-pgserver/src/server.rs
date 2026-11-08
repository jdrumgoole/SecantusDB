//! The serve loop, as a library: [`bind`] → [`RunningPgServer`] → `stop`.
//!
//! There is exactly ONE accept path in this crate and this is it. The
//! `secantusd-pg` binary is a CLI wrapper around [`bind`], and the embedded
//! Python handle (`_secantus_server`'s `PgServer`) wraps the same call — so a
//! durability or shutdown fix lands in both by construction, which is the whole
//! reason the loop moved out of `main.rs`.
//!
//! # Storage ownership is the durability contract
//!
//! `Storage` checkpoints WiredTiger in its `Drop`. Without that checkpoint every
//! acknowledged write since the last one is lost — measured 2026-08-31: a
//! SIGTERM after `CREATE TABLE` + `INSERT` left the catalog document and the
//! rows both gone, while the client had been told the writes succeeded.
//!
//! So [`bind`] takes `Storage` **by value** and [`RunningPgServer`] owns it for
//! the rest of its life. It is deliberately NOT an `Arc<Storage>` parameter: a
//! caller holding a second `Arc` would keep the store alive past `stop()`, the
//! checkpoint would not run, and nothing would say so. Handing ownership over
//! makes "stopping the server checkpoints the store" a property of the type
//! rather than a rule someone has to remember.
//!
//! [`RunningPgServer::stop`] therefore: tells the accept loop and every live
//! connection to finish, drains them (each holds an `Arc<Storage>` clone), shuts
//! the accept runtime down, and only then drops the last `Arc` — which is where
//! the checkpoint happens. If a wedged connection still holds a reference when
//! the bounded drain gives up, the checkpoint cannot run, and `stop` says so
//! loudly on stderr rather than returning as though the data were safe.
//!
//! The connections have to be TOLD, not just waited for. A pooled PostgreSQL
//! connection sits idle in `process_socket` indefinitely -- a real backend never
//! hangs up on one -- so a `stop()` that only waited would block for the whole
//! drain timeout every time a caller left a connection open, which in an
//! embedded test is most of the time (measured: 10.8s). Each connection
//! therefore selects its socket against a shutdown watch, and drops the
//! connection when it fires. Cancelling there is safe: storage calls are
//! synchronous, so a connection is never cancelled part-way through a write,
//! and an open transaction rolls back with its handle.
//!
//! # One thread per connection
//!
//! The accept loop is a task on a small tokio runtime; each accepted socket is
//! handed to its own OS thread with its own single-threaded runtime
//! ([`serve_connection`]). A statement is a blocking WiredTiger call, and on
//! its own thread it may simply block -- no `block_in_place`, no worker
//! hand-off, no other connection affected. PostgreSQL is a process per
//! connection; this is the same shape one level down.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use secantus_storage::Storage;
use tokio::net::TcpListener;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::{DatabaseRegistry, HandlerFactory, PgHandler};

/// How long `stop` waits for live connection tasks to finish before giving up
/// on a clean drain and tearing the runtime down anyway. A backstop, not the
/// expected path: the shutdown watch ends idle connections immediately.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval for that drain.
const DRAIN_POLL: Duration = Duration::from_millis(10);
/// How long the accept runtime's shutdown waits for its tasks. Connections are
/// not on it (each has its own thread), so this is only the accept loop.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Increments the live-connection count for a connection task's lifetime and
/// decrements on drop — so a cancelled or panicking task still releases its
/// slot. The count reaching zero is what tells `stop` that no task holds an
/// `Arc<Storage>` any more.
struct ConnGuard(Arc<AtomicUsize>);

impl ConnGuard {
    fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        ConnGuard(active)
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A running PostgreSQL-wire server: a bound address, the runtime its accept
/// loop lives on (connections run on their own threads), and the `Storage` it
/// owns.
///
/// Dropping it (or calling [`stop`](Self::stop)) shuts the server down and
/// closes the store, checkpointing WiredTiger. See the module docs for why the
/// handle owns the storage.
pub struct RunningPgServer {
    address: SocketAddr,
    /// `None` once stopped. Only the accept loop lives here.
    runtime: Option<Runtime>,
    accept: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    /// Broadcast to the accept loop and every live connection. Sending
    /// `true` is what makes an idle pooled connection let go promptly.
    shutdown: watch::Sender<bool>,
    active: Arc<AtomicUsize>,
    /// The last `Arc` to the store. `stop` drops it — that is the checkpoint.
    storage: Option<Arc<Storage>>,
}

impl RunningPgServer {
    /// The address the listener actually BOUND. With `127.0.0.1:0` this is how
    /// the caller learns the kernel-assigned port; it is resolved inside
    /// [`bind`], before the handle exists, so there is never a window in which
    /// the port is chosen but not yet listened on.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// A libpq connection string a client can use (`dbname=postgres
    /// user=postgres`). Matches what the psycopg gauge and the slice tests
    /// build by hand.
    pub fn dsn(&self) -> String {
        format!(
            "host={} port={} dbname=postgres user=postgres",
            self.address.ip(),
            self.address.port()
        )
    }

    /// Stop accepting, drain connections, and close the store (checkpointing
    /// WiredTiger). Idempotent — a second call is a no-op — and called by
    /// `Drop`.
    ///
    /// Must not be called from inside a tokio runtime: it drops one.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        // Tell the accept loop and every live connection to finish. Ignore a
        // send error: it only means every receiver is already gone.
        let _ = self.shutdown.send(true);
        if let Some(handle) = self.accept.take() {
            handle.abort();
        }
        // Then wait for them, bounded. Each holds an `Arc<Storage>` clone and
        // the checkpoint below cannot run while one is outstanding.
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while self.active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(DRAIN_POLL);
        }
        // A connection the drain did not catch is a thread inside a synchronous
        // storage call; it is not cancelled mid-write -- the shutdown watch is
        // only observed at an await point -- so it finishes the statement, sees
        // the watch, and lets go of the store then. The accept runtime has no
        // connections on it; this only ends the accept loop.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        }
        if let Some(storage) = self.storage.take() {
            match Arc::try_unwrap(storage) {
                // The close-checkpoint runs here, in `Storage::drop`.
                Ok(storage) => drop(storage),
                Err(still_shared) => {
                    // Never silent: this is a database, and reaching here means
                    // the acknowledged writes since the last checkpoint are at
                    // risk. Report it rather than returning as if stopped.
                    eprintln!(
                        "secantus-pgserver: WARNING: the store behind {} is still \
                         referenced after the shutdown drain ({} live \
                         connection(s)); its close-checkpoint has NOT run and \
                         writes since the last checkpoint may be lost",
                        self.address,
                        self.active.load(Ordering::SeqCst),
                    );
                    drop(still_shared);
                }
            }
        }
    }
}

impl Drop for RunningPgServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Open a listener on `addr` and start serving the PostgreSQL wire protocol
/// over `storage`.
///
/// Synchronous and runtime-free by contract: an embedder calls this from a
/// plain thread (a Python thread, in the `_secantus_server` case), so the
/// handle brings its own tokio runtime rather than requiring an ambient one.
/// The bound address is resolved before returning, which is what makes
/// `"127.0.0.1:0"` usable.
///
/// `storage` is taken by value and owned by the returned handle — see the
/// module docs; that ownership is what makes `stop()` checkpoint.
pub fn bind(
    addr: &str,
    storage: Storage,
    databases: Arc<DatabaseRegistry>,
) -> io::Result<RunningPgServer> {
    // One worker is plenty: the only task here is the accept loop. Connections
    // get their own threads (`serve_connection`).
    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("secantus-pg-accept")
        .enable_all()
        .build()?;
    let listener = runtime.block_on(async { TcpListener::bind(addr).await })?;
    let address = listener.local_addr()?;

    let storage = Arc::new(storage);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    let (shutdown, shutdown_rx) = watch::channel(false);

    let accept = {
        let storage = storage.clone();
        let stop_flag = stop_flag.clone();
        let active = active.clone();
        runtime.spawn(accept_loop(
            listener,
            storage,
            databases,
            stop_flag,
            active,
            shutdown_rx,
        ))
    };

    Ok(RunningPgServer {
        address,
        runtime: Some(runtime),
        accept: Some(accept),
        stop_flag,
        shutdown,
        active,
        storage: Some(storage),
    })
}

async fn accept_loop(
    listener: TcpListener,
    storage: Arc<Storage>,
    databases: Arc<DatabaseRegistry>,
    stop_flag: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    mut shutdown: watch::Receiver<bool>,
) {
    while !stop_flag.load(Ordering::SeqCst) {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = shutdown.changed() => return,
        };
        let (sock, _) = match accepted {
            Ok(v) => v,
            Err(_) => continue,
        };
        let handler = Arc::new(PgHandler::new(storage.clone(), databases.clone()));
        let active = active.clone();
        let conn_shutdown = shutdown.clone();
        // The guard is taken HERE, not on the connection thread, so the count
        // covers the window between accept and the thread starting: `stop`
        // must not conclude that no connection holds the store while one is
        // still being handed off.
        let guard = ConnGuard::new(active);
        // Off the accept runtime's I/O driver and onto the connection's own
        // thread (see `serve_connection`). `into_std` deregisters the socket
        // from this runtime; the connection re-registers it with its own.
        let Ok(sock) = sock.into_std() else { continue };
        let spawned = std::thread::Builder::new()
            .name("secantus-pg-conn".into())
            .spawn(move || serve_connection(sock, handler, conn_shutdown, guard));
        if let Err(e) = spawned {
            // The guard went into the closure and never ran; the socket is
            // dropped with it, which the client sees as a hangup.
            eprintln!("secantus-pgserver: could not start a connection thread: {e}");
        }
    }
}

/// One connection, one OS thread, one single-threaded tokio runtime.
///
/// A statement runs SYNCHRONOUSLY inside the handler -- storage calls are
/// blocking WiredTiger calls -- so whatever runs it must be allowed to block.
/// On a shared multi-thread runtime that meant `block_in_place` around every
/// statement, which hands the worker's core to another thread and takes it
/// back afterwards: measured at ~4us of a 40us `select 1` round trip
/// (2026-09-27), and a cross-thread migration on every statement besides.
/// PostgreSQL is a process per connection; a thread per connection is the
/// same shape, and it makes blocking free: a long `pg_sleep` or scan holds up
/// its own connection and nothing else, and the accept loop -- which is what a
/// `CancelRequest` needs to reach -- stays on its own runtime.
fn serve_connection(
    sock: std::net::TcpStream,
    handler: Arc<PgHandler>,
    mut shutdown: watch::Receiver<bool>,
    guard: ConnGuard,
) {
    let _guard = guard;
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("secantus-pgserver: could not build a connection runtime: {e}");
            return;
        }
    };
    runtime.block_on(async move {
        let Ok(sock) = tokio::net::TcpStream::from_std(sock) else {
            return;
        };
        tokio::select! {
            _ = pgwire::tokio::process_socket(
                sock,
                None,
                Arc::new(HandlerFactory(handler)),
            ) => {}
            // The server is stopping: drop this connection rather than wait
            // for a client that may never hang up.
            _ = shutdown.changed() => {}
        }
    });
}
