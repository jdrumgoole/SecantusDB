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
//! **Deferred (tracked in `tasks/rust-server-plan.md`):** TLS / mTLS (R4 tail),
//! auth state + `peer_cert_dn` threading (R5), metrics / sessions / failpoints /
//! connection registry (their command slices), and sourcing `cluster_time` from
//! storage (the command `Storage` trait doesn't expose `current_cluster_time`
//! yet — `hello`'s replica-set `lastWrite` uses a zero timestamp until then).

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bson::{doc, Bson, Document};
use secantus_commands::{dispatch, CommandContext, ConnectionAuth, CursorRegistry, Storage};
use secantus_wire::{
    build_op_msg_reply, build_op_reply, Header, Op, OpMsg, WireError, HEADER_SIZE,
    OP_MSG_FLAG_MORE_TO_COME,
};

/// How long a connection read blocks before re-checking the shutdown flag, so
/// idle connection threads are reaped promptly on `stop`.
const READ_POLL: Duration = Duration::from_millis(250);
/// Accept-loop poll interval (the listener is non-blocking) — same purpose.
const ACCEPT_POLL: Duration = Duration::from_millis(5);

/// Server configuration that shapes the handshake and (later) auth.
#[derive(Clone, Default)]
pub struct ServerConfig {
    /// Advertised replica-set name (`Some` ⇒ the single-node `secantus`
    /// replica-set `hello`; `None` ⇒ a plain standalone reply).
    pub replica_set_name: Option<String>,
    /// Whether access control is on (drives `accessControlEnabled`).
    pub require_auth: bool,
}

/// Shared state every connection thread reads.
struct Shared {
    config: ServerConfig,
    storage: Arc<dyn Storage>,
    cursors: Arc<CursorRegistry>,
    address: SocketAddr,
    next_conn_id: AtomicI64,
    next_reply_id: AtomicI64,
    stop: AtomicBool,
}

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

    /// Signal shutdown and join the accept loop. Connection threads notice the
    /// flag within `READ_POLL` and exit on their own.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
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
pub fn bind(
    addr: &str,
    config: ServerConfig,
    storage: Arc<dyn Storage>,
    cursors: Arc<CursorRegistry>,
) -> io::Result<RunningServer> {
    let listener = TcpListener::bind(addr)?;
    let address = listener.local_addr()?;
    listener.set_nonblocking(true)?;

    let shared = Arc::new(Shared {
        config,
        storage,
        cursors,
        address,
        next_conn_id: AtomicI64::new(1),
        next_reply_id: AtomicI64::new(1),
        stop: AtomicBool::new(false),
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
                let conn_shared = shared.clone();
                // Detached: the thread exits on EOF or when the stop flag is set
                // (its blocking reads use READ_POLL timeouts).
                thread::spawn(move || {
                    let _ = handle_connection(stream, conn_shared);
                });
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(stream: TcpStream, shared: Arc<Shared>) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_POLL))?;
    let mut stream = stream;
    let conn_id = shared.next_conn_id.fetch_add(1, Ordering::SeqCst);
    // Per-connection auth state, shared across every request on this socket so a
    // SCRAM conversation (saslStart → saslContinue) and the authenticated
    // principals persist for the connection's lifetime.
    let conn_auth = Arc::new(Mutex::new(ConnectionAuth::new()));

    loop {
        // Header.
        let mut header_buf = [0u8; HEADER_SIZE];
        match read_full(&mut stream, &mut header_buf, &shared)? {
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

        // Body.
        let mut body = vec![0u8; body_len];
        match read_full(&mut stream, &mut body, &shared)? {
            ReadOutcome::Filled => {}
            ReadOutcome::Closed => return Ok(()),
        }

        match secantus_wire::parse_body(header.op_code, &body) {
            Ok(Op::Msg(msg)) => {
                let more_to_come = msg.flags & OP_MSG_FLAG_MORE_TO_COME != 0;
                let request = match merge_op_msg_body(&msg) {
                    Ok(d) => d,
                    Err(e) => {
                        // A kind-0/kind-1 doc failed BSON validation — recoverable.
                        send_bad_value(&mut stream, &header, &shared, &e)?;
                        continue;
                    }
                };
                let reply = run_dispatch(&request, conn_id, &shared, &conn_auth);
                if !more_to_come {
                    write_op_msg(&mut stream, &header, &shared, &reply)?;
                }
            }
            Ok(Op::Query(query)) => {
                // Legacy OP_QUERY — the pymongo handshake. Decode the query doc,
                // dispatch, reply with OP_REPLY.
                let request = match Document::from_reader(&mut &query.query[..]) {
                    Ok(d) => d,
                    Err(e) => {
                        send_bad_value(
                            &mut stream,
                            &header,
                            &shared,
                            &WireError::MalformedBody(e.to_string()),
                        )?;
                        continue;
                    }
                };
                let mut ctx = make_context(conn_id, &shared, &conn_auth);
                ctx.db_name = db_from_namespace(query.full_collection_name);
                let reply = dispatch(&request, &mut ctx);
                write_op_reply(&mut stream, &header, &shared, &reply)?;
            }
            Err(e) if e.is_recoverable() => {
                send_bad_value(&mut stream, &header, &shared, &e)?;
            }
            // Fatal protocol fault (unsupported op_code, etc.) — drop.
            Err(_) => return Ok(()),
        }
    }
}

/// Build the request document: the kind-0 body with each kind-1 document
/// sequence merged in under its identifier (`server.py::_merge_op_msg_body`).
fn merge_op_msg_body(msg: &OpMsg) -> Result<Document, WireError> {
    let mut body = Document::from_reader(&mut &msg.body[..])
        .map_err(|e| WireError::MalformedBody(e.to_string()))?;
    for seq in &msg.document_sequences {
        let mut docs = Vec::with_capacity(seq.documents.len());
        for d in &seq.documents {
            let parsed = Document::from_reader(&mut &d[..])
                .map_err(|e| WireError::MalformedBody(e.to_string()))?;
            docs.push(Bson::Document(parsed));
        }
        body.insert(seq.identifier.to_string(), docs);
    }
    Ok(body)
}

fn run_dispatch(
    request: &Document,
    conn_id: i64,
    shared: &Arc<Shared>,
    conn_auth: &Arc<Mutex<ConnectionAuth>>,
) -> Document {
    let mut ctx = make_context(conn_id, shared, conn_auth);
    if let Ok(db) = request.get_str("$db") {
        ctx.db_name = db.to_string();
    }
    dispatch(request, &mut ctx)
}

fn make_context(
    conn_id: i64,
    shared: &Arc<Shared>,
    conn_auth: &Arc<Mutex<ConnectionAuth>>,
) -> CommandContext {
    let mut ctx = CommandContext::new(conn_id)
        .with_storage(shared.storage.clone())
        .with_cursors(shared.cursors.clone())
        .with_conn_auth(conn_auth.clone());
    ctx.server_address = Some((shared.address.ip().to_string(), shared.address.port()));
    ctx.replica_set_name = shared.config.replica_set_name.clone();
    ctx.require_auth = shared.config.require_auth;
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

fn write_op_msg(
    stream: &mut TcpStream,
    header: &Header,
    shared: &Arc<Shared>,
    reply: &Document,
) -> io::Result<()> {
    let mut body_bytes = Vec::new();
    reply
        .to_writer(&mut body_bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let frame = build_op_msg_reply(header.request_id, next_reply_id(shared), &body_bytes, 0);
    stream.write_all(&frame)
}

fn write_op_reply(
    stream: &mut TcpStream,
    header: &Header,
    shared: &Arc<Shared>,
    reply: &Document,
) -> io::Result<()> {
    let mut body_bytes = Vec::new();
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
fn send_bad_value(
    stream: &mut TcpStream,
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
    write_op_msg(stream, header, shared, &reply)
}

enum ReadOutcome {
    Filled,
    Closed,
}

/// Fill `buf` from `stream`, tolerating the read-timeout (re-checking the stop
/// flag between waits) and treating EOF as `Closed`.
fn read_full(
    stream: &mut TcpStream,
    buf: &mut [u8],
    shared: &Arc<Shared>,
) -> io::Result<ReadOutcome> {
    let mut filled = 0;
    while filled < buf.len() {
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(ReadOutcome::Closed);
        }
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Ok(ReadOutcome::Closed), // EOF
            Ok(n) => filled += n,
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                // Read timeout: loop to re-check the stop flag. (If we got a
                // partial read before timing out, `filled` is preserved.)
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(ReadOutcome::Filled)
}
