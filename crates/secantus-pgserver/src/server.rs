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
//! the tokio runtime down so any straggler is dropped, and only then drops the
//! last `Arc` — which is where the checkpoint happens. If a wedged task still
//! holds a reference when the bounded drain gives up, the checkpoint cannot
//! run, and `stop` says so loudly on stderr rather than returning as though the
//! data were safe.
//!
//! The connections have to be TOLD, not just waited for. A pooled PostgreSQL
//! connection sits idle in `process_socket` indefinitely -- a real backend never
//! hangs up on one -- so a `stop()` that only waited would block for the whole
//! drain timeout every time a caller left a connection open, which in an
//! embedded test is most of the time (measured: 10.8s). Each connection task
//! therefore selects its socket against a shutdown watch, and drops the
//! connection when it fires. Cancelling there is safe: storage calls are
//! synchronous, so a task is never cancelled part-way through a write, and an
//! open transaction rolls back with its handle.

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
/// How long the runtime shutdown waits for a worker still inside a synchronous
/// storage call. Storage calls are sync, so a task is never cancelled mid-write;
/// this only bounds how long we wait for one to come back.
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
/// loop lives on, and the `Storage` it owns.
///
/// Dropping it (or calling [`stop`](Self::stop)) shuts the server down and
/// closes the store, checkpointing WiredTiger. See the module docs for why the
/// handle owns the storage.
pub struct RunningPgServer {
    address: SocketAddr,
    /// `None` once stopped. Dropping the runtime is what cancels any connection
    /// task the drain did not catch.
    runtime: Option<Runtime>,
    accept: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    /// Broadcast to the accept loop and every live connection task. Sending
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
        // Dropping the runtime cancels whatever the drain did not catch. A task
        // inside a synchronous storage call is not cancelled mid-write —
        // cancellation only happens at an await point — so this cannot tear a
        // write; it only bounds how long we wait for one to return.
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
    let runtime = Builder::new_multi_thread().enable_all().build()?;
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
        let mut conn_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _guard = ConnGuard::new(active);
            tokio::select! {
                _ = pgwire::tokio::process_socket(
                    sock,
                    None,
                    Arc::new(HandlerFactory(handler)),
                ) => {}
                // The server is stopping: drop this connection rather than wait
                // for a client that may never hang up.
                _ = conn_shutdown.changed() => {}
            }
        });
    }
}
