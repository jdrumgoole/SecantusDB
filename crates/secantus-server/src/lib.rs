//! `secantus-server` — the standalone SecantusDB Rust server (R4).
//!
//! A port of `src/secantus/server.py`'s accept loop + connection handling: a TCP
//! accept loop on a background thread, one thread per connection, each reading
//! wire frames via [`secantus_wire`], dispatching through
//! [`secantus_commands`], and writing replies. A request flows
//! `socket bytes → wire parse → merge kind-1 sequences → dispatch → BSON reply →
//! socket bytes`, never entering Python.
//!
//! The connection loop is **generic over the command [`Storage`] trait**, so
//! this crate is WiredTiger-free and runs over real TCP with any backend (the
//! WT adapter lands separately, R4b). [`RunningServer`] is also the core the
//! embedded Python lifecycle handle (R6) wraps: `bind` → an address pymongo
//! connects to, `stop` → clean shutdown.
//!
//! TLS / mTLS (rustls), auth state + `peer_cert_dn` threading (MONGODB-X509,
//! mongod-form RFC 4514 DNs), metrics / sessions / failpoints, and the
//! connection registry all ship here; see `tasks/rust-server-plan.md` for the
//! phase history.

pub mod args;
pub mod config;

pub use config::{ConfigOverrides, ResolvedConfig};

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bson::{doc, Bson, Document};
use secantus_commands::logbuf::LogBuffer;
use secantus_commands::{
    dispatch, CommandContext, ConnectionAuth, ConnectionKiller, CursorRegistry, PendingBatch,
    Storage,
};
use secantus_wire::{
    build_op_msg_reply, build_op_reply, Header, Op, OpMsg, WireError, HEADER_SIZE,
    OP_MSG_FLAG_EXHAUST_ALLOWED, OP_MSG_FLAG_MORE_TO_COME,
};

/// How long a connection read blocks before re-checking the shutdown flag, so
/// idle connection threads are reaped promptly on `stop`.
const READ_POLL: Duration = Duration::from_millis(250);
/// Accept-loop poll interval (the listener is non-blocking) — same purpose.
const ACCEPT_POLL: Duration = Duration::from_millis(5);
/// Default for [`ServerConfig::message_read_timeout`]: an in-progress message
/// must fully arrive within 10 minutes of its first byte. Generous enough that
/// no legitimate message ever hits it (a real message's bytes are already
/// buffered by the kernel and delivered in milliseconds), tight enough to reap
/// a slow-loris connection that dribbles a partial frame forever.
const DEFAULT_MESSAGE_READ_TIMEOUT: Duration = Duration::from_secs(600);

/// Hard ceiling on the total bytes held across all connections' in-flight
/// message-body buffers. [`MAX_MESSAGE_SIZE`] bounds a *single* message (48 MB),
/// but without a global cap many concurrent large messages could exhaust the
/// heap. A connection reserves `body_len` here before allocating its body
/// buffer and releases the reservation once the message is dispatched and
/// answered. The cap is comfortably larger than `MAX_MESSAGE_SIZE`, so any one
/// message always eventually fits and a waiting connection never wedges.
const MAX_INFLIGHT_BYTES: usize = 512 * 1024 * 1024;

/// A global byte budget for concurrent message-body allocations. `acquire`
/// blocks while granting `n` more would exceed the cap, unless nothing is
/// outstanding (so a lone oversized request proceeds rather than deadlock). The
/// returned [`AllocReservation`] releases the bytes on drop and wakes a waiter.
struct AllocBudget {
    used: Mutex<usize>,
    available: Condvar,
    cap: usize,
}

impl AllocBudget {
    fn new(cap: usize) -> Self {
        AllocBudget {
            used: Mutex::new(0),
            available: Condvar::new(),
            cap,
        }
    }

    /// Reserve `n` bytes, blocking until they fit under the cap. Poison-tolerant
    /// (`into_inner`): a panicked peer must not wedge the whole server.
    fn acquire(&self, n: usize) -> AllocReservation<'_> {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        while *used != 0 && *used + n > self.cap {
            used = self.available.wait(used).unwrap_or_else(|e| e.into_inner());
        }
        *used += n;
        AllocReservation { budget: self, n }
    }
}

/// RAII reservation against an [`AllocBudget`]; releases on drop.
struct AllocReservation<'a> {
    budget: &'a AllocBudget,
    n: usize,
}

impl Drop for AllocReservation<'_> {
    fn drop(&mut self) {
        let mut used = self.budget.used.lock().unwrap_or_else(|e| e.into_inner());
        *used = used.saturating_sub(self.n);
        self.budget.available.notify_all();
    }
}

/// TLS / mTLS options (R5c). Mirrors `server.py`'s `tls_*` constructor knobs:
/// `cert_file` + `key_file` enable server-side TLS; adding `ca_file` (and
/// optionally `require_client_cert`) layers on mTLS client-certificate
/// verification.
#[derive(Clone)]
pub struct TlsOptions {
    /// PEM server certificate chain.
    pub cert_file: String,
    /// PEM private key for `cert_file`.
    pub key_file: String,
    /// PEM CA bundle to verify client certs against (enables mTLS).
    pub ca_file: Option<String>,
    /// Require a verified client cert (mandatory mTLS) vs. accept-if-present.
    pub require_client_cert: bool,
}

/// Server configuration that shapes the handshake and (later) auth.
#[derive(Clone)]
pub struct ServerConfig {
    /// Advertised replica-set name (`Some` ⇒ the single-node `secantus`
    /// replica-set `hello`; `None` ⇒ a plain standalone reply).
    pub replica_set_name: Option<String>,
    /// Whether access control is on (drives `accessControlEnabled`).
    pub require_auth: bool,
    /// TLS / mTLS options. `None` ⇒ plaintext connections.
    pub tls: Option<TlsOptions>,
    /// Once a wire message *starts* arriving, how long the whole message
    /// (header + body) may take to fully arrive before the connection is
    /// dropped. `None` disables the bound. This is a slow-loris defense: a
    /// client that dribbles a single message a byte at a time would otherwise
    /// pin a connection thread indefinitely. It does **not** bound how long a
    /// connection may sit idle *between* messages — an idle pooled connection
    /// (which mongod never closes on its own) is untouched, preserving the
    /// conformance contract. Defaults to 10 minutes via [`Default`].
    pub message_read_timeout: Option<Duration>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            replica_set_name: None,
            require_auth: false,
            tls: None,
            message_read_timeout: Some(DEFAULT_MESSAGE_READ_TIMEOUT),
        }
    }
}

/// Shared state every connection thread reads.
struct Shared {
    config: ServerConfig,
    storage: Arc<dyn Storage>,
    cursors: Arc<CursorRegistry>,
    transactions: Arc<secantus_commands::transactions::TransactionRegistry>,
    /// Server-wide `configureFailPoint` registry, shared across connections.
    failpoints: Arc<secantus_commands::failpoints::FailPointRegistry>,
    address: SocketAddr,
    next_conn_id: AtomicI64,
    next_reply_id: AtomicI64,
    stop: AtomicBool,
    /// Live per-connection threads. `stop` waits for this to reach zero before
    /// returning, so every connection thread has released its `Arc<Storage>` and
    /// the WiredTiger connection can close cleanly (its final checkpoint must not
    /// race a data-dir removal / reopen — see `ConnGuard`). Held behind its own
    /// `Arc` (not via `Shared`) so a connection thread can drop its `Shared`
    /// ref — and thus the storage ref — *before* it decrements the count.
    active: Arc<AtomicUsize>,
    /// The built rustls server config when TLS is on (shared across connections).
    tls: Option<Arc<rustls::ServerConfig>>,
    /// Global cap on concurrent in-flight message-body allocations.
    alloc_budget: AllocBudget,
    /// Live-connection registry: `conn_id → a clone of the connection's socket`,
    /// so `killOp` can close a connection by its id. Each connection registers
    /// on accept and unregisters on exit (`ConnEntryGuard`). Poison-tolerant.
    conns: Arc<Mutex<HashMap<i64, TcpStream>>>,
    /// The `ConnectionKiller` handed to each request's `CommandContext` so the
    /// `killOp` handler can reach `conns` without depending on server internals.
    conn_killer: Arc<dyn ConnectionKiller>,
    /// In-memory log ring buffer surfaced by `getLog`; the server appends
    /// connection-lifecycle lines and hands it to each request's context.
    logs: Arc<LogBuffer>,
}

/// Closes a connection by `conn_id` on behalf of `killOp`, by shutting down the
/// registered socket clone. Backs [`ConnectionKiller`].
struct ConnRegistry {
    conns: Arc<Mutex<HashMap<i64, TcpStream>>>,
}

impl ConnectionKiller for ConnRegistry {
    fn kill(&self, conn_id: i64) -> bool {
        let conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        match conns.get(&conn_id) {
            Some(sock) => {
                // Best-effort: `shutdown` makes the peer's read return 0 and the
                // connection thread exit (which unregisters the entry). An
                // already-closed socket erroring here is harmless.
                let _ = sock.shutdown(Shutdown::Both);
                true
            }
            None => false,
        }
    }
}

/// RAII: removes a connection's entry from the registry when its thread exits.
struct ConnEntryGuard {
    conns: Arc<Mutex<HashMap<i64, TcpStream>>>,
    conn_id: i64,
}

impl Drop for ConnEntryGuard {
    fn drop(&mut self) {
        self.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.conn_id);
    }
}

/// The Rust server's own version, embedded at compile time from the crate
/// version (`crates/*/Cargo.toml`, bumped in lockstep). This is the canonical
/// "Rust server version" — distinct from the Python server's `0.5.2bN` PyPI
/// version (the two diverged at `0.5.2`; see the project `CLAUDE.md`). The
/// `secantusd-rs` binary's `--version`, the embedded Python handle's `.version`,
/// and `buildInfo.secantusVersion` all surface this value.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A running server: a bound address + a handle to stop the accept loop. Drop
/// (or [`RunningServer::stop`]) shuts it down.
pub struct RunningServer {
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl RunningServer {
    /// The bound local address (use `port=0` to get an OS-assigned port).
    pub fn address(&self) -> SocketAddr {
        self.shared.address
    }

    /// The `mongodb://` URI a driver can connect with.
    pub fn uri(&self) -> String {
        format!("mongodb://{}", self.shared.address)
    }

    /// Signal shutdown, join the accept loop, and **wait for the connection
    /// threads to drain** before returning. Connection threads are detached, but
    /// each holds an `Arc<Storage>`; if `stop` returned while one was still alive,
    /// the WiredTiger connection wouldn't close until that thread later exited —
    /// and its final close-checkpoint would then race the caller removing or
    /// reopening the data dir (observed as `WT_PANIC: ... the system must
    /// restart` when `WiredTigerHS.wt` vanished mid-checkpoint). Draining makes
    /// teardown synchronous and the data dir quiescent on return. Mirrors the
    /// Python server's "drain active connections before storage.close()".
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
        // Connection threads notice the flag within READ_POLL (plain reads) or
        // wake from a tailable getMore's ~1s oplog wait, then drop their storage
        // ref. Poll until none remain; bounded so a wedged thread can't hang stop.
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.shared.active.load(Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(READ_POLL);
        }
    }
}

/// Increments the live-connection count for a connection thread's lifetime and
/// decrements on drop — so an early `?` return or a panic still releases the
/// slot. Holds only the counter `Arc` (never `Shared`), so it must be created
/// *after* the thread already owns its `Shared`/storage ref and dropped *after*
/// that ref is released: the decrement then signals "this thread holds no
/// storage", which is exactly the invariant `stop`'s drain relies on.
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

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Bind `addr` (e.g. `"127.0.0.1:0"`), start the accept loop on a background
/// thread, and return the running server. The accept loop spawns one thread per
/// connection.
/// Monotonic-ish wall-clock seconds for the transaction lifetime reaper
/// (injected as the registry's clock). Drift is irrelevant — only elapsed time
/// since a transaction's last use matters.
fn now_secs_f64() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn bind(
    addr: &str,
    config: ServerConfig,
    storage: Arc<dyn Storage>,
    cursors: Arc<CursorRegistry>,
) -> io::Result<RunningServer> {
    let listener = TcpListener::bind(addr)?;
    let address = listener.local_addr()?;
    listener.set_nonblocking(true)?;

    // Build the rustls config up front so a bad cert / key fails `bind`, not the
    // first connection.
    let tls = match &config.tls {
        Some(opts) => Some(build_tls_config(opts)?),
        None => None,
    };

    // The multi-document-transaction registry: commit/rollback close over the
    // storage so the registry can drive the WT transaction for each handle.
    let transactions = {
        use secantus_commands::transactions::{Transaction, TransactionRegistry};
        let s_commit = storage.clone();
        let s_rollback = storage.clone();
        Arc::new(TransactionRegistry::new(
            Box::new(move |txn: &mut Transaction| {
                if let Some(h) = txn.handle.as_mut() {
                    let _ = s_commit.commit_user_transaction(h.as_mut());
                }
            }),
            Box::new(move |txn: &mut Transaction| {
                if let Some(h) = txn.handle.as_mut() {
                    let _ = s_rollback.rollback_user_transaction(h.as_mut());
                }
            }),
            secantus_commands::transactions::DEFAULT_LIFETIME_SECONDS,
            Box::new(now_secs_f64),
        ))
    };

    let conns: Arc<Mutex<HashMap<i64, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
    let conn_killer: Arc<dyn ConnectionKiller> = Arc::new(ConnRegistry {
        conns: conns.clone(),
    });
    let logs = Arc::new(LogBuffer::new());
    logs.append(
        "I",
        "CONTROL",
        format!("SecantusDB (rust) {VERSION} started"),
    );
    let shared = Arc::new(Shared {
        config,
        storage,
        cursors,
        transactions,
        failpoints: Arc::new(secantus_commands::failpoints::FailPointRegistry::new()),
        address,
        next_conn_id: AtomicI64::new(1),
        next_reply_id: AtomicI64::new(1),
        stop: AtomicBool::new(false),
        active: Arc::new(AtomicUsize::new(0)),
        tls,
        alloc_budget: AllocBudget::new(MAX_INFLIGHT_BYTES),
        conns,
        conn_killer,
        logs,
    });

    let accept_shared = shared.clone();
    let accept = thread::spawn(move || accept_loop(listener, accept_shared));

    Ok(RunningServer {
        shared,
        accept: Some(accept),
    })
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                // Disable Nagle: reply paths write small frames back-to-back
                // and with Nagle on the second write waits for the peer's
                // delayed ACK — ~40ms per round trip on Linux CI, invisible
                // on macOS loopback. The Python servers' identical fix took
                // pgjdbc's generated-keys batch tests from 41.5s to 0.2s
                // each; mongod and PostgreSQL both set TCP_NODELAY
                // unconditionally. Best-effort: a failed setsockopt on an
                // already-dying socket must not kill the accept loop.
                let _ = stream.set_nodelay(true);
                let conn_shared = shared.clone();
                // Counter Arc is independent of `Shared`, so the guard can outlive
                // `conn_shared`'s drop (which releases this thread's storage ref).
                let active = shared.active.clone();
                // Detached: the thread exits on EOF or when the stop flag is set
                // (its blocking reads use READ_POLL timeouts). `stop` waits on the
                // count, not a JoinHandle.
                thread::spawn(move || {
                    let guard = ConnGuard::new(active);
                    let _ = handle_connection(stream, conn_shared);
                    // `conn_shared` (and every per-request storage clone) is now
                    // dropped; only then decrement, so a zero count guarantees no
                    // connection thread still references storage.
                    drop(guard);
                });
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break,
        }
    }
}

/// Per-connection setup: read-timeout (for shutdown polling), then either the
/// TLS handshake (extracting the client cert DN) or plaintext, before driving
/// the request loop in [`serve`].
fn handle_connection(tcp: TcpStream, shared: Arc<Shared>) -> io::Result<()> {
    // The listener is non-blocking and (on macOS/BSD) the accepted socket
    // inherits O_NONBLOCK. Put it back to blocking so a large `write_all` (a
    // reply bigger than the kernel send buffer, ~1 MB) blocks until drained
    // instead of failing with WouldBlock and dropping the connection. Reads
    // still get a timeout via `set_read_timeout` (blocking-with-timeout).
    tcp.set_nonblocking(false)?;
    tcp.set_read_timeout(Some(READ_POLL))?;
    let conn_id = shared.next_conn_id.fetch_add(1, Ordering::SeqCst);
    // Register a killable handle (a clone of this socket) under `conn_id` so a
    // `killOp` from another connection can `shutdown` it; the guard unregisters
    // when this thread exits. Cloning failure is non-fatal — the connection just
    // isn't killable (better than refusing it). The clone shares the OS socket,
    // so a `shutdown` on it tears down this connection (TLS included, since the
    // underlying TCP is what closes).
    if let Ok(kill_handle) = tcp.try_clone() {
        shared
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(conn_id, kill_handle);
    }
    let _conn_entry = ConnEntryGuard {
        conns: shared.conns.clone(),
        conn_id,
    };
    // Record the accept in the getLog ring buffer (mirrors the Python server's
    // "connection accepted" NETWORK line).
    let peer = tcp
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    shared.logs.append(
        "I",
        "NETWORK",
        format!("connection accepted from {peer} #{conn_id}"),
    );
    // Per-connection auth state, shared across every request on this socket so a
    // SCRAM conversation (saslStart → saslContinue) and the authenticated
    // principals persist for the connection's lifetime.
    let conn_auth = Arc::new(Mutex::new(ConnectionAuth::new()));

    match shared.tls.clone() {
        Some(tls_config) => {
            let mut tcp = tcp;
            let mut conn = match rustls::ServerConnection::new(tls_config) {
                Ok(c) => c,
                Err(_) => return Ok(()),
            };
            // Drive the handshake, tolerating the shutdown-poll read timeout.
            while conn.is_handshaking() {
                if shared.stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                match conn.complete_io(&mut tcp) {
                    Ok(_) => {}
                    Err(ref e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::TimedOut =>
                    {
                        continue;
                    }
                    // Handshake failure / peer hung up — drop the connection.
                    Err(_) => return Ok(()),
                }
            }
            let peer_cert_dn = conn
                .peer_certificates()
                .and_then(|certs| certs.first())
                .and_then(|cert| cert_subject_dn(cert.as_ref()));
            let mut stream = rustls::StreamOwned::new(conn, tcp);
            serve(&mut stream, peer_cert_dn, conn_id, &conn_auth, &shared)
        }
        None => {
            let mut stream = tcp;
            serve(&mut stream, None, conn_id, &conn_auth, &shared)
        }
    }
}

/// The request loop, generic over the transport (`TcpStream` or a rustls
/// `StreamOwned`). Reads framed wire messages, dispatches, writes replies.
fn serve<S: Read + Write>(
    stream: &mut S,
    peer_cert_dn: Option<String>,
    conn_id: i64,
    conn_auth: &Arc<Mutex<ConnectionAuth>>,
    shared: &Arc<Shared>,
) -> io::Result<()> {
    loop {
        // A fresh per-message deadline: `None` until the first byte of this
        // message arrives, so an idle connection waiting between requests is
        // never bounded — only an in-progress message is. It's threaded through
        // both the header and body reads (the body inherits the armed deadline)
        // so the *whole* message must complete within the timeout of its first
        // byte, then reset on the next loop iteration.
        let mut deadline: Option<Instant> = None;

        // Header.
        let mut header_buf = [0u8; HEADER_SIZE];
        match read_full(stream, &mut header_buf, shared, &mut deadline)? {
            ReadOutcome::Filled => {}
            ReadOutcome::Closed => return Ok(()),
        }
        let header = match Header::unpack(&header_buf) {
            Ok(h) => h,
            // A malformed header is unframeable — drop the connection.
            Err(_) => return Ok(()),
        };
        let body_len = match header.body_len() {
            Ok(n) => n,
            Err(_) => return Ok(()),
        };

        // Body. Reserve against the global allocation budget first so a flood of
        // concurrent large messages can't exhaust the heap; the reservation is
        // released at the end of this loop iteration when `_body_budget` drops.
        let _body_budget = shared.alloc_budget.acquire(body_len);
        let mut body = vec![0u8; body_len];
        match read_full(stream, &mut body, shared, &mut deadline)? {
            ReadOutcome::Filled => {}
            ReadOutcome::Closed => return Ok(()),
        }

        match secantus_wire::parse_body(header.op_code, &body) {
            Ok(Op::Msg(msg)) => {
                let more_to_come = msg.flags & OP_MSG_FLAG_MORE_TO_COME != 0;
                let exhaust_allowed = msg.flags & OP_MSG_FLAG_EXHAUST_ALLOWED != 0;
                let (request, raw_insert_docs) = match merge_op_msg_body(&msg) {
                    Ok(d) => d,
                    Err(e) => {
                        // A kind-0/kind-1 doc failed BSON validation — recoverable.
                        send_bad_value(stream, &header, shared, &e)?;
                        continue;
                    }
                };
                let (reply, close_conn, pending) = run_dispatch(
                    &request,
                    raw_insert_docs,
                    conn_id,
                    shared,
                    conn_auth,
                    &peer_cert_dn,
                );
                // A `closeConnection` failpoint drops the socket without replying,
                // so the driver observes a network error.
                if close_conn {
                    return Ok(());
                }
                if more_to_come {
                    // Fire-and-forget request (`w: 0` write): spec forbids a reply.
                    continue;
                }
                // OP_MSG exhaust: a getMore with `exhaustAllowed` set streams every
                // remaining batch back over the same socket with `moreToCome`,
                // instead of one reply per getMore. mongod only streams on getMore
                // (the find/aggregate reply that opens the cursor is sent normally).
                if exhaust_allowed
                    && request.contains_key("getMore")
                    && matches!(reply.get("cursor"), Some(Bson::Document(_)))
                {
                    // The streamer splices pending blobs straight onto the
                    // wire per frame — no decode→re-encode of the batch.
                    if !stream_exhaust_getmore(
                        stream,
                        &header,
                        shared,
                        &request,
                        conn_id,
                        conn_auth,
                        &peer_cert_dn,
                        reply,
                        pending,
                    )? {
                        return Ok(());
                    }
                } else if exhaust_allowed
                    && is_hello_command(&request)
                    && request.contains_key("maxAwaitTimeMS")
                    && reply_ok(&reply)
                {
                    // Streaming-SDAM monitor: an awaitable `hello`/`isMaster`
                    // (carries `maxAwaitTimeMS`) sent with `exhaustAllowed` wants
                    // a continuous `moreToCome` hello stream. Honour it so the
                    // driver's monitor doesn't raise "Server ended moreToCome
                    // unexpectedly" on teardown.
                    if !stream_awaitable_hello(
                        stream,
                        &header,
                        shared,
                        &request,
                        conn_id,
                        conn_auth,
                        &peer_cert_dn,
                        reply,
                    )? {
                        return Ok(());
                    }
                } else {
                    write_op_msg(stream, &header, shared, &reply, pending.as_ref())?;
                }
            }
            Ok(Op::Query(query)) => {
                // Legacy OP_QUERY — the pymongo handshake. Decode the query doc,
                // dispatch, reply with OP_REPLY.
                let request = match Document::from_reader(&mut &query.query[..]) {
                    Ok(d) => d,
                    Err(e) => {
                        send_bad_value(
                            stream,
                            &header,
                            shared,
                            &WireError::MalformedBody(e.to_string()),
                        )?;
                        continue;
                    }
                };
                let mut ctx = make_context(conn_id, shared, conn_auth, &peer_cert_dn);
                ctx.db_name = db_from_namespace(query.full_collection_name);
                let reply = dispatch(&request, &mut ctx);
                write_op_reply(stream, &header, shared, &reply)?;
            }
            Err(e) if e.is_recoverable() => {
                send_bad_value(stream, &header, shared, &e)?;
            }
            // Fatal protocol fault (unsupported op_code, etc.) — drop.
            Err(_) => return Ok(()),
        }
    }
}

/// Build the shared rustls server config from [`TlsOptions`]: the server cert
/// chain and key, plus (when `ca_file` is set) a client-cert verifier —
/// mandatory under `require_client_cert`, accept-if-present otherwise.
fn build_tls_config(opts: &TlsOptions) -> io::Result<Arc<rustls::ServerConfig>> {
    use rustls::server::WebPkiClientVerifier;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let certs = load_certs(&opts.cert_file)?;
    let key = load_key(&opts.key_file)?;

    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

    let config = if let Some(ca) = &opts.ca_file {
        let mut roots = rustls::RootCertStore::empty();
        for cert in load_certs(ca)? {
            roots
                .add(cert)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        }
        let roots = Arc::new(roots);
        let verifier_builder = WebPkiClientVerifier::builder_with_provider(roots, provider);
        let verifier = if opts.require_client_cert {
            verifier_builder.build()
        } else {
            verifier_builder.allow_unauthenticated().build()
        }
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };

    let config = config
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    Ok(Arc::new(config))
}

/// Load a PEM certificate chain.
fn load_certs(path: &str) -> io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()
}

/// Load the first PEM private key (PKCS#8 / PKCS#1 / SEC1).
fn load_key(path: &str) -> io::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no private key in key file"))
}

/// Extract a peer certificate's subject DN as an RFC 4514 string
/// (most-specific-first, short OID names), for `MONGODB-X509`.
/// The verified client cert's subject as a mongod-style RFC 4514 DN string:
/// most-specific-first (CN before OU before O …), comma-separated with NO
/// space, short OID names, and the RFC 4514 value escaping — byte-for-byte
/// the string `secantus.auth.subject_dn_from_peercert` produces on the
/// Python server, so a user record provisioned against either server
/// authenticates on the other. (x509-parser's own `Display` is
/// least-specific-first with `", "` separators — the raw certificate order —
/// which never matches the stored DN username.)
fn cert_subject_dn(der: &[u8]) -> Option<String> {
    use x509_parser::prelude::FromDer;
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(der).ok()?;
    let mut rdn_strs: Vec<String> = Vec::new();
    // Iterate in reverse so the resulting string is most-specific-first.
    let rdns: Vec<_> = cert.subject().iter().collect();
    for rdn in rdns.into_iter().rev() {
        let mut attrs: Vec<String> = Vec::new();
        for attr in rdn.iter() {
            let oid = attr.attr_type().to_id_string();
            let short = dn_short_name(&oid);
            let value = attr.as_str().map(str::to_string).unwrap_or_else(|_| {
                // Non-UTF8 / unusual encodings: fall back to lossy bytes,
                // mirroring Python's str() of whatever ssl decoded.
                String::from_utf8_lossy(attr.attr_value().data).into_owned()
            });
            attrs.push(format!("{short}={}", escape_dn_value(&value)));
        }
        rdn_strs.push(attrs.join("+"));
    }
    if rdn_strs.is_empty() {
        return None;
    }
    Some(rdn_strs.join(","))
}

/// RFC 4514 short names for the common subject attribute OIDs — the same set
/// `secantus.auth._DN_SHORT_NAMES` maps (an unknown OID keeps its dotted id).
fn dn_short_name(oid: &str) -> &str {
    match oid {
        "2.5.4.3" => "CN",
        "2.5.4.10" => "O",
        "2.5.4.11" => "OU",
        "2.5.4.7" => "L",
        "2.5.4.8" => "ST",
        "2.5.4.6" => "C",
        "2.5.4.9" => "STREET",
        "0.9.2342.19200300.100.1.25" => "DC",
        "0.9.2342.19200300.100.1.1" => "UID",
        "2.5.4.5" => "serialNumber",
        "1.2.840.113549.1.9.1" => "emailAddress",
        other => other,
    }
}

/// RFC 4514 value escaping, mirroring `secantus.auth._escape_dn_value`.
fn escape_dn_value(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    for (i, ch) in chars.iter().enumerate() {
        let special = matches!(ch, ',' | '+' | '"' | '\\' | '<' | '>' | ';' | '=' | '#')
            || (i == 0 && *ch == ' ')
            || (i == chars.len() - 1 && *ch == ' ');
        if special {
            out.push('\\');
        }
        out.push(*ch);
    }
    out
}

/// Build the request document: the kind-0 body with each kind-1 document
/// sequence merged in under its identifier (`server.py::_merge_op_msg_body`).
/// Returns the merged command document and, for the raw-BSON write path, the
/// `insert` documents kept as un-decoded byte slices. For an `insert` whose
/// `documents` arrived as a kind-1 sequence (the common driver shape) that
/// sequence is **not** decoded/merged into the body — the raw slices are returned
/// separately so the handler can store the client's bytes verbatim, skipping the
/// merge-decode → re-encode → storage-decode round-trip. Every other command (and
/// an insert whose docs are inline in the kind-0 body) merges as before with a
/// `None` raw side-channel.
/// The raw (un-decoded) BSON byte slices of an `insert`'s kind-1 `documents`
/// sequence — the raw-BSON write side-channel.
type RawInsertDocs = Vec<Vec<u8>>;

fn merge_op_msg_body(msg: &OpMsg) -> Result<(Document, Option<RawInsertDocs>), WireError> {
    let mut body = Document::from_reader(&mut &msg.body[..])
        .map_err(|e| WireError::MalformedBody(e.to_string()))?;
    let is_insert = body.keys().next().map(String::as_str) == Some("insert");
    let mut raw_insert: Option<Vec<Vec<u8>>> = None;
    for seq in &msg.document_sequences {
        if is_insert && seq.identifier == "documents" {
            // Divert un-decoded: the insert handler stores these bytes directly.
            raw_insert = Some(seq.documents.iter().map(|d| d.to_vec()).collect());
            continue;
        }
        let mut docs = Vec::with_capacity(seq.documents.len());
        for d in &seq.documents {
            let parsed = Document::from_reader(&mut &d[..])
                .map_err(|e| WireError::MalformedBody(e.to_string()))?;
            docs.push(Bson::Document(parsed));
        }
        body.insert(seq.identifier.to_string(), docs);
    }
    Ok((body, raw_insert))
}

/// Dispatch one request, returning the reply plus whether the connection should
/// be dropped (a `closeConnection` failpoint fired — the reply is discarded).
fn run_dispatch(
    request: &Document,
    raw_insert_docs: Option<RawInsertDocs>,
    conn_id: i64,
    shared: &Arc<Shared>,
    conn_auth: &Arc<Mutex<ConnectionAuth>>,
    peer_cert_dn: &Option<String>,
) -> (Document, bool, Option<PendingBatch>) {
    let mut ctx = make_context(conn_id, shared, conn_auth, peer_cert_dn);
    ctx.raw_insert_documents = raw_insert_docs;
    if let Ok(db) = request.get_str("$db") {
        ctx.db_name = db.to_string();
    }
    // Defense-in-depth: every handler is contracted to return a `CommandError`
    // rather than panic, and the known interior-NUL vector is rejected earlier
    // in `dispatch_inner` (#139). But if any handler ever panics, catch it here
    // and reply with a wire-level `InternalError` instead of letting the panic
    // unwind the connection thread and drop the socket with no reply — matching
    // the Python server's dispatch-level catch-all.
    let reply =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(request, &mut ctx)))
            .unwrap_or_else(|_| {
                let mut d = Document::new();
                d.insert("ok", 0.0_f64);
                d.insert("errmsg", "internal server error");
                d.insert("code", 1_i32);
                d.insert("codeName", "InternalError");
                d
            });
    // `pending_batch` is set by `find` / `getMore` to hand the reply's document
    // batch to the wire as pre-encoded blobs (spliced by `write_op_msg` /
    // `materialize_batch`) instead of an owned `Bson::Array` in the reply.
    (reply, ctx.close_connection, ctx.pending_batch.take())
}

fn make_context(
    conn_id: i64,
    shared: &Arc<Shared>,
    conn_auth: &Arc<Mutex<ConnectionAuth>>,
    peer_cert_dn: &Option<String>,
) -> CommandContext {
    let mut ctx = CommandContext::new(conn_id)
        .with_storage(shared.storage.clone())
        .with_cursors(shared.cursors.clone())
        .with_transactions(shared.transactions.clone())
        .with_failpoints(shared.failpoints.clone())
        .with_conn_auth(conn_auth.clone())
        .with_conn_killer(shared.conn_killer.clone())
        .with_logs(shared.logs.clone())
        // `next_conn_id` starts at 1 and is bumped per accepted connection, so
        // it is the lifetime total plus one; `conns` holds the live sockets.
        .with_conn_stats(secantus_commands::ConnStats {
            current: shared.conns.lock().map(|c| c.len() as i64).unwrap_or(0),
            total_created: shared.next_conn_id.load(Ordering::SeqCst) - 1,
        });
    ctx.server_address = Some((shared.address.ip().to_string(), shared.address.port()));
    ctx.replica_set_name = shared.config.replica_set_name.clone();
    ctx.require_auth = shared.config.require_auth;
    ctx.peer_cert_dn = peer_cert_dn.clone();
    ctx
}

fn db_from_namespace(ns: &str) -> String {
    let db = ns.split('.').next().unwrap_or("");
    if db.is_empty() {
        "admin".to_string()
    } else {
        db.to_string()
    }
}

fn next_reply_id(shared: &Arc<Shared>) -> i32 {
    shared.next_reply_id.fetch_add(1, Ordering::SeqCst) as i32
}

fn write_op_msg<S: Write>(
    stream: &mut S,
    header: &Header,
    shared: &Arc<Shared>,
    reply: &Document,
    batch: Option<&PendingBatch>,
) -> io::Result<()> {
    let body_bytes = cursor_or_plain_body(reply, batch)?;
    let frame = build_op_msg_reply(header.request_id, next_reply_id(shared), &body_bytes, 0);
    stream.write_all(&frame)
}

/// The reply body bytes: when a `PendingBatch` is present the batch blobs are
/// spliced into `cursor.<field>` without decoding (`encode_cursor_reply`);
/// otherwise the reply document is serialized as-is.
fn cursor_or_plain_body(reply: &Document, batch: Option<&PendingBatch>) -> io::Result<Vec<u8>> {
    match batch {
        Some(pending) => {
            secantus_wire::encode_cursor_reply(reply, pending.batch_field, &pending.batch)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        }
        None => {
            let mut body_bytes = Vec::with_capacity(256);
            reply
                .to_writer(&mut body_bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(body_bytes)
        }
    }
}

/// Write one OP_MSG reply with the given flag bits (e.g. `moreToCome` for an
/// exhaust-stream batch).
fn write_op_msg_flags<S: Write>(
    stream: &mut S,
    header: &Header,
    shared: &Arc<Shared>,
    reply: &Document,
    flags: u32,
) -> io::Result<()> {
    let mut body_bytes = Vec::with_capacity(256);
    reply
        .to_writer(&mut body_bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let frame = build_op_msg_reply(header.request_id, next_reply_id(shared), &body_bytes, flags);
    stream.write_all(&frame)
}

/// Stream the rest of an exhaust cursor over one socket. Called when a getMore
/// arrives with the OP_MSG `exhaustAllowed` flag set: `first_reply` (the getMore
/// handler's reply) and every subsequent batch are sent with `moreToCome` set,
/// pulling further batches with synthetic getMores, until the cursor drains —
/// then a final `id: 0` reply with `moreToCome` clear closes the stream. mongod
/// keeps the cursor alive until a getMore returns an empty batch, so a cursor
/// that drains on a non-empty batch still gets a trailing empty reply. Mirrors
/// `server.py::_stream_exhaust_getmore`. `Ok(true)` = stream written (connection
/// survives); `Ok(false)` = a write failed mid-stream (drop the connection).
#[allow(clippy::too_many_arguments)]
fn stream_exhaust_getmore<S: Write>(
    stream: &mut S,
    header: &Header,
    shared: &Arc<Shared>,
    request: &Document,
    conn_id: i64,
    conn_auth: &Arc<Mutex<ConnectionAuth>>,
    peer_cert_dn: &Option<String>,
    first_reply: Document,
    first_pending: Option<PendingBatch>,
) -> io::Result<bool> {
    let target_id = match request.get("getMore") {
        Some(b) => b.clone(),
        None => return Ok(true),
    };
    let coll = request.get_str("collection").unwrap_or("").to_string();
    let db = request.get_str("$db").unwrap_or("admin").to_string();
    let ns = first_reply
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get_str("ns").ok())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{db}.{coll}"));

    // `pending` batches splice onto the wire undecoded (`cursor_or_plain_body`),
    // exactly like the non-exhaust reply path.
    let send = |stream: &mut S,
                doc: &Document,
                pending: Option<&PendingBatch>,
                more: bool|
     -> io::Result<bool> {
        let flags = if more { OP_MSG_FLAG_MORE_TO_COME } else { 0 };
        let body = cursor_or_plain_body(doc, pending)?;
        let frame = build_op_msg_reply(header.request_id, next_reply_id(shared), &body, flags);
        match stream.write_all(&frame) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    };

    let mut doc = first_reply;
    let mut pending = first_pending;
    loop {
        let (drained, batch_empty) = match doc.get("cursor") {
            Some(Bson::Document(c)) => {
                let empty = match &pending {
                    Some(p) => p.batch.is_empty(),
                    // Materialized fallback (a handler that didn't splice).
                    None => c
                        .get_array("nextBatch")
                        .or_else(|_| c.get_array("firstBatch"))
                        .map(|a| a.is_empty())
                        .unwrap_or(true),
                };
                (c.get_i64("id").unwrap_or(0) == 0, empty)
            }
            // An error reply (ok: 0) mid-stream — deliver it without moreToCome.
            _ => return send(stream, &doc, None, false),
        };
        if drained {
            if !batch_empty {
                // Re-advertise the ORIGINAL cursor id with this final batch
                // (the exhaust protocol closes with a separate empty frame).
                let sent = match pending.take() {
                    Some(p) => send(
                        stream,
                        &doc! {"cursor": {"id": target_id.clone(), "ns": &ns}, "ok": 1.0},
                        Some(&p),
                        true,
                    )?,
                    None => {
                        let batch: Vec<Bson> = doc
                            .get_document("cursor")
                            .ok()
                            .and_then(|c| {
                                c.get_array("nextBatch")
                                    .or_else(|_| c.get_array("firstBatch"))
                                    .ok()
                            })
                            .cloned()
                            .unwrap_or_default();
                        send(
                            stream,
                            &doc! {"cursor": {"nextBatch": batch, "id": target_id.clone(), "ns": &ns}, "ok": 1.0},
                            None,
                            true,
                        )?
                    }
                };
                if !sent {
                    return Ok(false);
                }
            }
            let empty: Vec<Bson> = Vec::new();
            return send(
                stream,
                &doc! {"cursor": {"nextBatch": empty, "id": 0i64, "ns": &ns}, "ok": 1.0},
                None,
                false,
            );
        }
        if batch_empty {
            // A live cursor that yielded nothing (tailable/awaitData wait expired):
            // deliver this empty batch without moreToCome and stop streaming so we
            // don't spin. Normal cursors never reach here (empty drains id to 0).
            return send(stream, &doc, pending.as_ref(), false);
        }
        if !send(stream, &doc, pending.as_ref(), true)? {
            return Ok(false);
        }
        let mut getmore = doc! {"getMore": target_id.clone(), "collection": &coll, "$db": &db};
        if let Some(bs) = request.get("batchSize") {
            getmore.insert("batchSize", bs.clone());
        }
        if let Some(mt) = request.get("maxTimeMS") {
            getmore.insert("maxTimeMS", mt.clone());
        }
        let (reply, _close, p) =
            run_dispatch(&getmore, None, conn_id, shared, conn_auth, peer_cert_dn);
        doc = reply;
        pending = p;
    }
}

/// True if `request` is a `hello` / `isMaster` / `ismaster` handshake command.
fn is_hello_command(request: &Document) -> bool {
    matches!(
        request.keys().next().map(String::as_str),
        Some("hello") | Some("isMaster") | Some("ismaster")
    )
}

/// True if `reply.ok` is truthy (1.0 / nonzero int).
fn reply_ok(reply: &Document) -> bool {
    matches!(reply.get("ok"), Some(Bson::Double(d)) if *d != 0.0)
        || matches!(reply.get("ok"), Some(Bson::Int32(n)) if *n != 0)
        || matches!(reply.get("ok"), Some(Bson::Int64(n)) if *n != 0)
}

/// Stream awaitable (streaming-SDAM) `hello` replies over an exhaust monitor
/// connection. Once a driver sees `topologyVersion` it switches its monitor to
/// the streaming protocol: it sends `hello` with `maxAwaitTimeMS` and the
/// `exhaustAllowed` flag and keeps the connection open expecting a *continuous*
/// stream of `moreToCome` replies (mongod holds each until the topology changes
/// or `maxAwaitTimeMS` elapses). If the server answers with a single
/// `moreToCome`-clear reply, the driver's monitor still treats the connection as
/// a live stream and, when the socket later closes, raises "Server ended
/// moreToCome unexpectedly" and clears the pool. Our topology is fixed, so we
/// re-emit the same hello state every `maxAwaitTimeMS` with `moreToCome` set;
/// the wait polls `shared.stop` so shutdown is prompt, and on shutdown we send a
/// final `moreToCome`-clear reply (a clean end the driver accepts silently).
/// Mirrors `server.py::_stream_awaitable_hello`. `Ok(true)` = ended cleanly,
/// `Ok(false)` = a write failed (drop the connection).
#[allow(clippy::too_many_arguments)]
fn stream_awaitable_hello<S: Read + Write>(
    stream: &mut S,
    header: &Header,
    shared: &Arc<Shared>,
    request: &Document,
    conn_id: i64,
    conn_auth: &Arc<Mutex<ConnectionAuth>>,
    peer_cert_dn: &Option<String>,
    first_reply: Document,
) -> io::Result<bool> {
    let max_await_ms = request
        .get_i64("maxAwaitTimeMS")
        .or_else(|_| request.get_i32("maxAwaitTimeMS").map(i64::from))
        .unwrap_or(10_000)
        .max(0) as u64;

    // Establish the stream with the reply the handler already produced.
    if write_op_msg_flags(
        stream,
        header,
        shared,
        &first_reply,
        OP_MSG_FLAG_MORE_TO_COME,
    )
    .is_err()
    {
        return Ok(false);
    }
    loop {
        // Hold up to maxAwaitTimeMS (topology never changes). Each iteration
        // probes the socket (its read timeout is `READ_POLL`): a 0-byte read is
        // EOF — the client closed it or a kill shut it down — and any bytes mid-
        // stream are an unexpected client message; either way we drop the stream.
        // `shared.stop` is checked too so the monitor thread is reaped promptly
        // on shutdown, ending the stream with a clean `moreToCome`-clear reply.
        let deadline = Instant::now() + Duration::from_millis(max_await_ms);
        loop {
            if shared.stop.load(Ordering::SeqCst) {
                let _ = write_op_msg_flags(stream, header, shared, &first_reply, 0);
                return Ok(true);
            }
            if Instant::now() >= deadline {
                break;
            }
            let mut probe = [0u8; 1];
            match stream.read(&mut probe) {
                Ok(0) => return Ok(false), // EOF: client closed / killed
                Ok(_) => return Ok(false), // unexpected data mid-stream: drop
                Err(ref e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut => {}
                Err(_) => return Ok(false), // socket error: drop
            }
        }
        // Streaming `hello` never sets `pending_batch` (no cursor), so ignore it.
        let (reply, _close, _pending) =
            run_dispatch(request, None, conn_id, shared, conn_auth, peer_cert_dn);
        if write_op_msg_flags(stream, header, shared, &reply, OP_MSG_FLAG_MORE_TO_COME).is_err() {
            return Ok(false);
        }
    }
}

fn write_op_reply<S: Write>(
    stream: &mut S,
    header: &Header,
    shared: &Arc<Shared>,
    reply: &Document,
) -> io::Result<()> {
    let mut body_bytes = Vec::with_capacity(256);
    reply
        .to_writer(&mut body_bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let frame = build_op_reply(
        header.request_id,
        next_reply_id(shared),
        &[&body_bytes],
        0,
        0,
        0,
    );
    stream.write_all(&frame)
}

/// Reply to a malformed-body request with `{ok:0, code:2 BadValue}` so the
/// connection survives (`server.py`'s `MalformedBodyError` path). The errmsg
/// carries "invalid BSON" to match what `tests/test_wire_malformed.py` asserts.
fn send_bad_value<S: Write>(
    stream: &mut S,
    header: &Header,
    shared: &Arc<Shared>,
    err: &WireError,
) -> io::Result<()> {
    let reply = doc! {
        "ok": 0.0,
        "errmsg": format!("invalid BSON in body: {err}"),
        "code": 2,
        "codeName": "BadValue",
    };
    write_op_msg(stream, header, shared, &reply, None)
}

enum ReadOutcome {
    Filled,
    Closed,
}

/// Fill `buf` from `stream`, tolerating the read-timeout (re-checking the stop
/// flag between waits) and treating EOF as `Closed`.
///
/// `deadline` arms the message-read timeout (`ServerConfig::message_read_timeout`):
/// it stays `None` while no byte has arrived (an idle wait is never bounded), is
/// set to `now + timeout` on the first byte read, and is honoured on every
/// subsequent poll. Passing the same `deadline` to the header read and then the
/// body read makes the whole message subject to one timeout measured from its
/// first byte. A lapsed deadline returns `Closed` (the connection is dropped),
/// matching how EOF and the stop flag are handled.
fn read_full<S: Read>(
    stream: &mut S,
    buf: &mut [u8],
    shared: &Arc<Shared>,
    deadline: &mut Option<Instant>,
) -> io::Result<ReadOutcome> {
    let timeout = shared.config.message_read_timeout;
    let mut filled = 0;
    while filled < buf.len() {
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(ReadOutcome::Closed);
        }
        // An armed deadline bounds an in-progress message even while the read is
        // blocked with no further progress (a client that sends a partial frame
        // then stalls). Checked before each poll so a stalled body — whose
        // deadline was armed during the header read — is reaped too.
        if let Some(dl) = *deadline {
            if Instant::now() >= dl {
                return Ok(ReadOutcome::Closed);
            }
        }
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Ok(ReadOutcome::Closed), // EOF
            Ok(n) => {
                filled += n;
                // First byte of this message: arm the completion deadline.
                if deadline.is_none() {
                    if let Some(t) = timeout {
                        *deadline = Some(Instant::now() + t);
                    }
                }
            }
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                // Read timeout: loop to re-check the stop flag and deadline. (If
                // we got a partial read before timing out, `filled` is preserved.)
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(ReadOutcome::Filled)
}

#[cfg(test)]
mod alloc_budget_tests {
    use super::AllocBudget;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn reservation_releases_on_drop() {
        let b = AllocBudget::new(100);
        {
            let _r = b.acquire(60);
            assert_eq!(*b.used.lock().unwrap(), 60);
        }
        assert_eq!(*b.used.lock().unwrap(), 0);
    }

    #[test]
    fn over_cap_blocks_until_released() {
        let b = Arc::new(AllocBudget::new(100));
        let first = b.acquire(80); // 80/100 used

        let started = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let b2 = b.clone();
        let started2 = started.clone();
        let done2 = done.clone();
        let h = thread::spawn(move || {
            started2.store(true, Ordering::SeqCst);
            // 80 + 40 > 100 → must block until `first` is released.
            let _r = b2.acquire(40);
            done2.store(true, Ordering::SeqCst);
        });

        // Give the waiter time to reach the blocking wait.
        while !started.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(50));
        assert!(!done.load(Ordering::SeqCst), "should still be blocked");

        drop(first); // frees 80 → the 40 now fits
        h.join().unwrap();
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(*b.used.lock().unwrap(), 0);
    }

    #[test]
    fn lone_oversized_request_proceeds() {
        // Nothing outstanding: a request larger than the cap must not deadlock.
        let b = AllocBudget::new(100);
        let _r = b.acquire(500);
        assert_eq!(*b.used.lock().unwrap(), 500);
    }
}
