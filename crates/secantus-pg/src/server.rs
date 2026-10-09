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

use secantus_storage::{wt_config, Storage, StorageOptions};
use tokio::net::TcpListener;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::{DatabaseRegistry, HandlerFactory, PgHandler};

/// Whether a store this server opens syncs the WiredTiger log on every commit.
///
/// PostgreSQL's default (`synchronous_commit = on`) is that an acknowledged
/// COMMIT survives a crash, and so does this server's DURABLE mode -- the
/// shipped default. The precedence is the storage's own close-time `durable`
/// rule: `SECANTUS_FORCE_DURABLE=1` always syncs; otherwise
/// `SECANTUS_TEST_FAST_STORAGE=1` (the test suite's fast mode) does not, and
/// anything else does. Pure over its inputs so it is testable without touching
/// the process environment.
pub fn sync_on_commit(force_durable: bool, fast_storage: bool) -> bool {
    force_durable || !fast_storage
}

/// The commit-sync METHOD, matched to PostgreSQL's default on this platform.
///
/// `wt_config` asks for `method=fsync`, and on macOS WiredTiger's fsync is
/// `fcntl(F_FULLFSYNC)` -- a drive-cache flush costing ~5-8 ms a commit.
/// PostgreSQL's macOS default is `wal_sync_method = open_datasync` (an
/// `O_DSYNC` WAL file, no `F_FULLFSYNC`), so there this server uses
/// WiredTiger's `method=dsync`, which opens the log `O_DSYNC` the same way.
///
/// **The guarantee that buys, on macOS: an acknowledged COMMIT survives a
/// process kill (and an OS crash the kernel survives far enough to flush),
/// but NOT a power loss** -- the drive's volatile cache is not flushed. That
/// is exactly PostgreSQL's default guarantee on macOS. Linux keeps
/// `method=fsync` (fdatasync, which is PostgreSQL's Linux default too).
/// Measured 2026-10-03 on an M-series Mac, durable autocommit UPDATE median:
/// fsync 7.9 ms, dsync 149 us, PG15 82 us; a SIGKILL after 20 acked runs
/// lost nothing. The Rust MongoDB
/// server does not call this and is unchanged.
///
/// Under `method=dsync` concurrent commits share log writes: stock WiredTiger
/// gives every synced commit a log slot and a synchronous write of its own,
/// and `cmake/patch_wt_dsync_group.py` lets commits that arrive during a write
/// join the next one. A commit still returns, and becomes visible, only after
/// its record is written. `method=fsync` runs unpatched code.
pub fn commit_sync_method(config: &str) -> String {
    if cfg!(target_os = "macos") {
        config.replace("method=fsync", "method=dsync")
    } else {
        config.to_string()
    }
}

/// The log file size for a store whose commits sync, where the log is
/// zero-filled ([`zero_filled_log`]).
const ZERO_FILLED_LOG_FILE_MAX: &str = "16MB";

/// Is the log zero-filled on this platform when commits sync? Linux only: it
/// is where it was measured, and macOS syncs a different way
/// ([`commit_sync_method`]).
const ZERO_FILL_SYNCED_LOG: bool = cfg!(target_os = "linux");

/// A log file written with zeros when it is created, for a store whose
/// commits sync, as PostgreSQL does for a WAL segment.
///
/// WiredTiger sizes a new log file with `fallocate`, which reserves the
/// blocks but leaves them UNWRITTEN. The first write into such a block makes
/// the filesystem record that it now holds data, so the `fdatasync` after
/// every commit also commits a filesystem journal transaction. Over blocks
/// that already hold zeros it has only the data to flush. Measured 2026-10-09
/// on a DigitalOcean `c-16` (ext4): the average `fdatasync` went from about
/// 150 to 110 us, and a durable `UPDATE` by primary key from 2.3k to 2.7k
/// statements a second with one client and from 9.0k to 12.1k with eight.
/// Preallocation alone (`prealloc=true`) changed nothing.
///
/// The file is 16 MB, not 128: zero-filling 128 MB added 150 ms to every
/// start, 16 MB adds 25 ms, and the throughput is the same. A transaction
/// larger than one file spans files as before.
///
/// It only applies when commits sync. Without a sync per commit there is no
/// `fdatasync` to make cheaper, and the test suite's fast mode starts
/// thousands of stores.
pub fn zero_filled_log(config: &str, synced: bool) -> String {
    if synced && ZERO_FILL_SYNCED_LOG {
        config.replace("prealloc=false)", "prealloc=false,zero_fill=true)")
    } else {
        config.to_string()
    }
}

/// Open the store the PostgreSQL server serves, with a per-commit log sync in
/// durable mode (see [`sync_on_commit`]).
///
/// The Rust MongoDB server shares `secantus-storage` and keeps its own
/// default (`transaction_sync` off, `--sync-on-commit` to opt in); this is the
/// PG server choosing the synced configuration, not a storage-wide change.
/// Measured 2026-10-02: without it a `CREATE TABLE` + `INSERT` acknowledged
/// just before a SIGKILL were gone after restart.
pub fn open_storage(home: &str) -> secantus_storage::Result<Storage> {
    open_storage_with_cache(home, "4G")
}

/// [`open_storage`] with a WiredTiger cache cap other than the daemon's 4G, in
/// WiredTiger's syntax (`"256M"`). The embedded [`crate::PgServer`] uses a
/// smaller one, because a test suite starts many servers.
pub fn open_storage_with_cache(home: &str, cache_size: &str) -> secantus_storage::Result<Storage> {
    let force = std::env::var("SECANTUS_FORCE_DURABLE").as_deref() == Ok("1");
    let fast = std::env::var("SECANTUS_TEST_FAST_STORAGE").as_deref() == Ok("1");
    // The same engine knobs as `Storage::open`'s default config, with only
    // `transaction_sync` chosen here.
    let synced = sync_on_commit(force, fast);
    let log_file_max = if synced && ZERO_FILL_SYNCED_LOG {
        ZERO_FILLED_LOG_FILE_MAX
    } else {
        "128MB"
    };
    let config = zero_filled_log(
        &commit_sync_method(&wt_config(cache_size, 1000, synced, log_file_max)),
        synced,
    );
    Storage::open_with_options(
        home,
        &StorageOptions {
            wt_config: Some(config),
            ..StorageOptions::default()
        },
    )
}

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
    /// Set by `stop` once the store is known to be closed (its last reference
    /// dropped, so the close-checkpoint ran).
    store_closed: bool,
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

    /// Whether [`stop`](Self::stop) has run and closed the store. False before
    /// `stop`, and after a `stop` whose drain gave up with the store still
    /// referenced (reported on stderr). A caller that owns the store's
    /// directory removes it only when this is true: deleting files from under
    /// an open WiredTiger is how a `WT_PANIC` happens.
    pub fn store_closed(&self) -> bool {
        self.store_closed
    }

    /// Stop accepting, drain connections, and close the store (checkpointing
    /// WiredTiger). Idempotent — a second call is a no-op — and called by
    /// `Drop`.
    ///
    /// Safe to call from anywhere, a tokio runtime included. The teardown ends
    /// by shutting down the handle's OWN runtime, which tokio refuses from an
    /// async context ("Cannot drop a runtime in a context where blocking is not
    /// allowed"), so a `#[tokio::test]` that let the handle fall out of scope
    /// used to panic in `Drop`. From inside a runtime the teardown now runs on
    /// a plain thread and this call waits for it, so it still returns only
    /// once the store is closed -- the checkpoint is not left to race the
    /// caller.
    pub fn stop(&mut self) {
        if self.storage.is_none() {
            return;
        }
        self.stop_flag.store(true, Ordering::SeqCst);
        // Tell the accept loop and every live connection to finish. Ignore a
        // send error: it only means every receiver is already gone.
        let _ = self.shutdown.send(true);
        if let Some(handle) = self.accept.take() {
            handle.abort();
        }
        let teardown = Teardown {
            address: self.address,
            active: Arc::clone(&self.active),
            runtime: self.runtime.take(),
            storage: self.storage.take(),
        };
        if tokio::runtime::Handle::try_current().is_err() {
            self.store_closed = teardown.run();
            return;
        }
        // Inside a runtime: hand the blocking part to a thread that is not.
        // Joining blocks this worker until the store is closed, which is what
        // `stop` promises; the server's connections run on its own runtime,
        // so nothing the drain waits for needs the caller's.
        match std::thread::Builder::new()
            .name("secantus-pg-stop".into())
            .spawn(move || teardown.run())
        {
            Ok(thread) => match thread.join() {
                Ok(closed) => self.store_closed = closed,
                Err(panic) => std::panic::resume_unwind(panic),
            },
            // No thread to be had: report it rather than return as if stopped
            // (the teardown was moved into the failed spawn and dropped, which
            // is exactly the runtime drop this path exists to avoid).
            Err(e) => eprintln!(
                "secantus-pg: WARNING: could not start the shutdown thread for {}: {e}; \
                 the store's close-checkpoint may not have run",
                self.address
            ),
        }
    }
}

/// The blocking half of [`RunningPgServer::stop`]: wait for the connections,
/// shut the runtime down, and close the store. Owns everything it touches so it
/// can run on a thread of its own.
struct Teardown {
    address: SocketAddr,
    active: Arc<AtomicUsize>,
    runtime: Option<Runtime>,
    storage: Option<Arc<Storage>>,
}

impl Teardown {
    /// Returns whether the store was closed (false only when a reference
    /// outlived the drain, which is reported on stderr).
    fn run(mut self) -> bool {
        // Wait for the connections, bounded. Each holds an `Arc<Storage>`
        // clone and the checkpoint below cannot run while one is outstanding.
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
                Ok(storage) => {
                    drop(storage);
                    true
                }
                Err(still_shared) => {
                    // Never silent: this is a database, and reaching here means
                    // the acknowledged writes since the last checkpoint are at
                    // risk. Report it rather than returning as if stopped.
                    eprintln!(
                        "secantus-pg: WARNING: the store behind {} is still \
                         referenced after the shutdown drain ({} live \
                         connection(s)); its close-checkpoint has NOT run and \
                         writes since the last checkpoint may be lost",
                        self.address,
                        self.active.load(Ordering::SeqCst),
                    );
                    drop(still_shared);
                    false
                }
            }
        } else {
            true
        }
    }
}

impl Drop for RunningPgServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Each connection thread's (and runtime worker's) stack: see `bind`.
pub const WORKER_STACK_BYTES: usize = 256 << 20;

/// Open a listener on `addr` and start serving the PostgreSQL wire protocol
/// over `storage`.
///
/// Synchronous and runtime-free by contract: an embedder calls this from a
/// plain thread (a Python thread, in the `_secantus_server` case), so the
/// handle brings its own tokio runtime rather than requiring an ambient one.
/// The bound address is resolved before returning, which is what makes
/// `"127.0.0.1:0"` usable.
///
/// It may equally be called from INSIDE a runtime -- a `#[tokio::test]`, the
/// way a Rust caller writes one. It used to `block_on` its own runtime to bind
/// the listener, which tokio refuses from an async context ("Cannot start a
/// runtime from within a runtime"), so it panicked before returning a handle.
/// Nothing here blocks on a runtime now: the socket is bound with std, and
/// registered with the handle's runtime by entering it.
///
/// `storage` is taken by value and owned by the returned handle — see the
/// module docs; that ownership is what makes `stop()` checkpoint.
pub fn bind(
    addr: &str,
    storage: Storage,
    databases: Arc<DatabaseRegistry>,
) -> io::Result<RunningPgServer> {
    // Statements are planned and run on each connection's own thread (see
    // `accept_loop`; the runtime below accepts), and planning recurses once per expression level: tokio's 2 MiB default
    // overflowed on a 24-term `||` chain, which ABORTS the process -- every
    // connection with it. The stack is reserved, not committed, so a large
    // one costs address space only. `planning_depth_guard` refuses what even
    // this cannot hold, as PostgreSQL's 54001 does.
    // Transaction ids start above anything an earlier run handed out.
    crate::xids::install(&storage);
    let runtime = Builder::new_multi_thread()
        .thread_stack_size(WORKER_STACK_BYTES)
        .enable_all()
        .build()?;
    let std_listener = std::net::TcpListener::bind(addr)?;
    std_listener.set_nonblocking(true)?;
    let listener = {
        // `from_std` must run inside a runtime to register the socket with its
        // reactor. Entering one is not blocking on it, so this is safe from
        // an async context too.
        let _entered = runtime.enter();
        TcpListener::from_std(std_listener)?
    };
    let address = listener.local_addr()?;

    let storage = Arc::new(storage);
    // Expression indexes keep a computed field on every row; rows another
    // writer left are brought up to date before any client connects.
    for info in databases.all(&storage).unwrap_or_default() {
        if !info.allow_conn {
            continue;
        }
        let handler = PgHandler::new(storage.clone(), databases.clone());
        if handler.select_database(&info.name).is_ok() {
            if let Err(e) = handler.refresh_all_expression_indexes() {
                eprintln!("secantusd-pg: could not rebuild the expression indexes: {e}");
            }
            if let Err(e) = handler.drop_orphan_temp_functions() {
                eprintln!("secantusd-pg: could not drop orphaned temp functions: {e}");
            }
        }
    }
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
        store_closed: false,
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
        // One OS thread per connection, running the connection on a runtime
        // of its own -- PostgreSQL's backend-per-connection model. A
        // statement then runs synchronously on the thread that owns its
        // client, with no hand-off: under the shared multi-thread runtime
        // every statement paid `block_in_place`, whose blocking-pool mutex
        // was the largest contention at 8 inserting clients (`sample`,
        // batch 71), and a statement that ran on a worker instead held up
        // the other connections queued there. A connection that blocks
        // (a lock wait, `pg_sleep`) now blocks only itself.
        let guard = ConnGuard::new(active);
        let std_sock = match sock.into_std() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("secantusd-pg: could not take over a connection: {e}");
                continue;
            }
        };
        let spawned = std::thread::Builder::new()
            .name("secantus-pg-conn".into())
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || {
                let _guard = guard;
                // A ONE-worker multi-thread runtime, not a current-thread one:
                // a statement that sends a notice while it runs
                // (`live_notices`) hands the worker to another thread with
                // `block_in_place` so the socket's I/O keeps being driven. On
                // a current-thread runtime nothing drove it, and the send
                // waited forever (pgjdbc's
                // `StatementTest.concurrentWarningReadAndClear`, batch 71).
                let runtime = match Builder::new_multi_thread()
                    .worker_threads(1)
                    .thread_name("secantus-pg-conn-worker")
                    .thread_stack_size(WORKER_STACK_BYTES)
                    .on_thread_start(|| crate::mark_connection_thread(true))
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("secantusd-pg: could not start a connection's runtime: {e}");
                        return;
                    }
                };
                let conn = runtime.spawn(async move {
                    let sock = match tokio::net::TcpStream::from_std(std_sock) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("secantusd-pg: could not register a connection: {e}");
                            return;
                        }
                    };
                    tokio::select! {
                        _ = pgwire::tokio::process_socket(
                            sock,
                            None,
                            Arc::new(HandlerFactory(handler)),
                        ) => {}
                        // The server is stopping: drop this connection rather
                        // than wait for a client that may never hang up.
                        _ = conn_shutdown.changed() => {}
                    }
                });
                let _ = runtime.block_on(conn);
            });
        if let Err(e) = spawned {
            // The connection is dropped (its socket closes) and the client
            // sees the refusal; never a silent hang.
            eprintln!("secantusd-pg: could not start a connection thread: {e}");
        }
    }
}

#[cfg(test)]
mod sync_tests {
    use super::{
        commit_sync_method, sync_on_commit, wt_config, zero_filled_log, ZERO_FILL_SYNCED_LOG,
    };

    /// Durable (the shipped default) syncs per commit; the test suite's fast
    /// mode does not; `SECANTUS_FORCE_DURABLE=1` wins over fast mode.
    #[test]
    fn sync_on_commit_follows_the_durable_precedence() {
        assert!(sync_on_commit(false, false));
        assert!(!sync_on_commit(false, true));
        assert!(sync_on_commit(true, true));
        assert!(sync_on_commit(true, false));
    }

    /// macOS commits with `O_DSYNC` (PostgreSQL's `open_datasync`), not
    /// WiredTiger's `F_FULLFSYNC`; elsewhere the config is untouched.
    #[test]
    fn commit_sync_method_matches_postgres_default() {
        let cfg = wt_config("4G", 1000, true, "128MB");
        let got = commit_sync_method(&cfg);
        if cfg!(target_os = "macos") {
            assert!(got.contains("transaction_sync=(enabled=true,method=dsync)"));
        } else {
            assert_eq!(got, cfg);
        }
    }

    /// The log is zero-filled only where commits sync, and only on Linux.
    #[test]
    fn the_log_is_zero_filled_only_when_commits_sync() {
        let synced = wt_config("4G", 1000, true, "16MB");
        let got = zero_filled_log(&synced, true);
        if ZERO_FILL_SYNCED_LOG {
            assert!(got.contains("log=(enabled=true,file_max=16MB,prealloc=false,zero_fill=true)"));
        } else {
            assert_eq!(got, synced);
        }
        let unsynced = wt_config("4G", 1000, false, "128MB");
        assert_eq!(zero_filled_log(&unsynced, false), unsynced);
    }
}
