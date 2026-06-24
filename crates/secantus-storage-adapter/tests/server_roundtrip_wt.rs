//! End-to-end: drive the Rust server over a real TCP socket backed by a real
//! `WtStorage` (via `StorageAdapter`), speaking the wire protocol by hand (no
//! pymongo needed). Proves the whole path — socket → wire parse → dispatch →
//! storage → reply → socket — works in-process over real WiredTiger. (Moved out
//! of `secantus-server`, which is WiredTiger-free and so could only test this
//! against a hand-rolled in-memory storage double.)

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bson::{doc, Bson, Document};
use secantus_commands::{CursorRegistry, Storage as CmdStorage};
use secantus_server::{bind, ServerConfig};
use secantus_storage::Storage as WtStorage;
use secantus_storage_adapter::StorageAdapter;

// --- real-WT storage behind a temp dir that cleans up on drop ------------

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Removes its temp dir on drop (after the server — and thus the WT
/// connection — has been released).
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A real `WtStorage` (adapted to the command `Storage` trait) over a fresh
/// temp dir. Hold the returned guard for the lifetime of the server.
fn wt_storage() -> (Arc<dyn CmdStorage>, TempDir) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("secantus-srvwt-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wt = Arc::new(WtStorage::open(dir.to_str().unwrap()).unwrap());
    let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(wt));
    (adapter, TempDir(dir))
}

// --- wire helpers (client side) -----------------------------------------

const OP_MSG: i32 = 2013;
const OP_QUERY: i32 = 2004;

fn enc(d: &Document) -> Vec<u8> {
    let mut v = Vec::new();
    d.to_writer(&mut v).unwrap();
    v
}

fn read_exact(stream: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).unwrap();
    buf
}

/// Send an OP_MSG (flags=0, single kind-0 body) and return the reply document.
fn op_msg(stream: &mut TcpStream, request_id: i32, body: &Document) -> Document {
    let body_bytes = enc(body);
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_le_bytes()); // flags
    payload.push(0u8); // kind 0
    payload.extend_from_slice(&body_bytes);
    let msg_len = (16 + payload.len()) as i32;
    let mut frame = Vec::new();
    frame.extend_from_slice(&msg_len.to_le_bytes());
    frame.extend_from_slice(&request_id.to_le_bytes());
    frame.extend_from_slice(&0i32.to_le_bytes()); // responseTo
    frame.extend_from_slice(&OP_MSG.to_le_bytes());
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).unwrap();

    let header = read_exact(stream, 16);
    let total = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    let rest = read_exact(stream, total - 16);
    assert_eq!(&rest[0..4], &[0, 0, 0, 0], "reply flags 0");
    assert_eq!(rest[4], 0, "kind 0 section");
    Document::from_reader(&mut &rest[5..]).unwrap()
}

/// Send a legacy OP_QUERY against `admin.$cmd` and return the OP_REPLY doc.
fn op_query(stream: &mut TcpStream, request_id: i32, query: &Document) -> Document {
    let q = enc(query);
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_le_bytes()); // flags
    payload.extend_from_slice(b"admin.$cmd\x00");
    payload.extend_from_slice(&0i32.to_le_bytes()); // skip
    payload.extend_from_slice(&(-1i32).to_le_bytes()); // return
    payload.extend_from_slice(&q);
    let msg_len = (16 + payload.len()) as i32;
    let mut frame = Vec::new();
    frame.extend_from_slice(&msg_len.to_le_bytes());
    frame.extend_from_slice(&request_id.to_le_bytes());
    frame.extend_from_slice(&0i32.to_le_bytes());
    frame.extend_from_slice(&OP_QUERY.to_le_bytes());
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).unwrap();

    let header = read_exact(stream, 16);
    let total = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    let rest = read_exact(stream, total - 16);
    // OP_REPLY: flags(4) cursorId(8) startingFrom(4) numberReturned(4) docs
    Document::from_reader(&mut &rest[20..]).unwrap()
}

fn connect(addr: std::net::SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}

// --- the tests -----------------------------------------------------------

#[test]
fn full_wire_roundtrip() {
    let (storage, _tmp) = wt_storage();
    let cursors = Arc::new(CursorRegistry::new());
    let server = bind(
        "127.0.0.1:0",
        ServerConfig {
            replica_set_name: Some("secantus".into()),
            require_auth: false,
            tls: None,
            ..ServerConfig::default()
        },
        storage,
        cursors,
    )
    .unwrap();
    let addr = server.address();

    let mut stream = connect(addr);

    // 1. OP_MSG hello handshake.
    let reply = op_msg(&mut stream, 1, &doc! {"hello": 1, "$db": "admin"});
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    assert!(reply.get_bool("isWritablePrimary").unwrap());
    assert_eq!(reply.get_str("setName").unwrap(), "secantus");
    assert!(matches!(reply.get("connectionId"), Some(Bson::Int64(_))));

    // 2. ping.
    let reply = op_msg(&mut stream, 2, &doc! {"ping": 1, "$db": "admin"});
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0);

    // 3. insert two docs (inline kind-0 documents array).
    let reply = op_msg(
        &mut stream,
        3,
        &doc! {"insert": "c", "documents": [{"_id": 1, "x": 1}, {"_id": 2, "x": 2}], "$db": "t"},
    );
    assert_eq!(reply.get_i32("n").unwrap(), 2);
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0);

    // 4. count.
    let reply = op_msg(&mut stream, 4, &doc! {"count": "c", "$db": "t"});
    assert_eq!(reply.get_i32("n").unwrap(), 2);

    // 5. find with a filter ⇒ cursor with firstBatch.
    let reply = op_msg(
        &mut stream,
        5,
        &doc! {"find": "c", "filter": {"x": 1}, "$db": "t"},
    );
    let cursor = reply.get_document("cursor").unwrap();
    assert_eq!(cursor.get_str("ns").unwrap(), "t.c");
    let first = cursor.get_array("firstBatch").unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].as_document().unwrap().get_i32("_id").unwrap(), 1);
    assert_eq!(
        cursor.get_i64("id").unwrap(),
        0,
        "small result ⇒ no live cursor"
    );

    // 6. delete.
    let reply = op_msg(
        &mut stream,
        6,
        &doc! {"delete": "c", "deletes": [{"q": {"x": 1}, "limit": 0}], "$db": "t"},
    );
    assert_eq!(reply.get_i32("n").unwrap(), 1);

    // 7. unknown command survives the connection (CommandNotFound, not a drop).
    let reply = op_msg(&mut stream, 7, &doc! {"bogusCmd": 1, "$db": "admin"});
    assert_eq!(reply.get_i32("code").unwrap(), 59);

    // 8. legacy OP_QUERY isMaster handshake still works on the same path.
    let reply = op_query(&mut stream, 8, &doc! {"isMaster": 1});
    assert!(reply.get_bool("ismaster").unwrap());
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
}

#[test]
fn find_getmore_killcursors_over_the_wire() {
    let (storage, _tmp) = wt_storage();
    let cursors = Arc::new(CursorRegistry::new());
    let server = bind("127.0.0.1:0", ServerConfig::default(), storage, cursors).unwrap();
    let mut stream = connect(server.address());

    op_msg(
        &mut stream,
        1,
        &doc! {"insert": "c", "documents": [
            {"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 4}, {"_id": 5}
        ], "$db": "t"},
    );

    let reply = op_msg(
        &mut stream,
        2,
        &doc! {"find": "c", "batchSize": 2, "$db": "t"},
    );
    let cursor = reply.get_document("cursor").unwrap();
    assert_eq!(cursor.get_array("firstBatch").unwrap().len(), 2);
    let cid = cursor.get_i64("id").unwrap();
    assert_ne!(cid, 0);

    let reply = op_msg(
        &mut stream,
        3,
        &doc! {"getMore": cid, "collection": "c", "batchSize": 2, "$db": "t"},
    );
    let cursor = reply.get_document("cursor").unwrap();
    assert_eq!(cursor.get_array("nextBatch").unwrap().len(), 2);
    assert_eq!(cursor.get_i64("id").unwrap(), cid);

    let reply = op_msg(
        &mut stream,
        4,
        &doc! {"killCursors": "c", "cursors": [cid], "$db": "t"},
    );
    assert_eq!(
        reply.get_array("cursorsKilled").unwrap(),
        &vec![Bson::Int64(cid)]
    );
}

/// Send an OP_MSG with the `exhaustAllowed` flag (1<<16), then read replies
/// until one arrives without the `moreToCome` bit (1<<1).
fn op_msg_exhaust(stream: &mut TcpStream, request_id: i32, body: &Document) -> Vec<Document> {
    let body_bytes = enc(body);
    let mut payload = Vec::new();
    payload.extend_from_slice(&(1u32 << 16).to_le_bytes()); // exhaustAllowed
    payload.push(0u8); // kind 0
    payload.extend_from_slice(&body_bytes);
    let msg_len = (16 + payload.len()) as i32;
    let mut frame = Vec::new();
    frame.extend_from_slice(&msg_len.to_le_bytes());
    frame.extend_from_slice(&request_id.to_le_bytes());
    frame.extend_from_slice(&0i32.to_le_bytes()); // responseTo
    frame.extend_from_slice(&OP_MSG.to_le_bytes());
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).unwrap();

    let mut replies = Vec::new();
    loop {
        let header = read_exact(stream, 16);
        let total = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let rest = read_exact(stream, total - 16);
        let flags = u32::from_le_bytes(rest[0..4].try_into().unwrap());
        assert_eq!(rest[4], 0, "kind 0 section");
        replies.push(Document::from_reader(&mut &rest[5..]).unwrap());
        if flags & (1 << 1) == 0 {
            break; // moreToCome clear ⇒ last reply
        }
    }
    replies
}

/// OP_MSG exhaust: a getMore with `exhaustAllowed` streams every remaining batch
/// back over the same socket with `moreToCome`, ending with an empty `id: 0`.
#[test]
fn exhaust_getmore_streams_remaining_batches() {
    let (storage, _tmp) = wt_storage();
    let cursors = Arc::new(CursorRegistry::new());
    let server = bind("127.0.0.1:0", ServerConfig::default(), storage, cursors).unwrap();
    let mut stream = connect(server.address());

    op_msg(
        &mut stream,
        1,
        &doc! {"insert": "c", "documents": [
            {"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 4}, {"_id": 5}
        ], "$db": "t"},
    );

    let reply = op_msg(
        &mut stream,
        2,
        &doc! {"find": "c", "batchSize": 2, "$db": "t"},
    );
    let cid = reply.get_document("cursor").unwrap().get_i64("id").unwrap();
    assert_ne!(cid, 0);

    let replies = op_msg_exhaust(
        &mut stream,
        3,
        &doc! {"getMore": cid, "collection": "c", "batchSize": 2, "$db": "t"},
    );
    assert!(
        replies.len() >= 2,
        "exhaust should produce multiple replies"
    );
    let last = replies.last().unwrap();
    assert_eq!(
        last.get_document("cursor").unwrap().get_i64("id").unwrap(),
        0,
        "stream terminates with cursor id 0"
    );
    let streamed: usize = replies
        .iter()
        .map(|r| {
            r.get_document("cursor")
                .ok()
                .and_then(|c| c.get_array("nextBatch").ok())
                .map(|b| b.len())
                .unwrap_or(0)
        })
        .sum();
    assert_eq!(
        streamed, 3,
        "remaining 3 docs streamed (2 were in firstBatch)"
    );
}

/// Streaming-SDAM monitor: an awaitable `hello` sent with `exhaustAllowed` must
/// get a continuous stream of `moreToCome` hello replies, and a client close
/// must reap the monitor thread (stop() must not hang). Guards the "Server ended
/// moreToCome unexpectedly" mongosh flake.
#[test]
fn awaitable_exhaust_hello_streams_more_to_come() {
    let (storage, _tmp) = wt_storage();
    let cursors = Arc::new(CursorRegistry::new());
    let server = bind("127.0.0.1:0", ServerConfig::default(), storage, cursors).unwrap();
    let mut stream = connect(server.address());

    let hello = doc! {
        "hello": 1,
        "maxAwaitTimeMS": 100i32,
        "topologyVersion": {"counter": 0i64},
        "$db": "admin",
    };
    let body = enc(&hello);
    let mut payload = Vec::new();
    payload.extend_from_slice(&(1u32 << 16).to_le_bytes()); // exhaustAllowed
    payload.push(0u8); // kind 0
    payload.extend_from_slice(&body);
    let msg_len = (16 + payload.len()) as i32;
    let mut frame = Vec::new();
    frame.extend_from_slice(&msg_len.to_le_bytes());
    frame.extend_from_slice(&1i32.to_le_bytes());
    frame.extend_from_slice(&0i32.to_le_bytes());
    frame.extend_from_slice(&OP_MSG.to_le_bytes());
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).unwrap();

    for _ in 0..2 {
        let header = read_exact(&mut stream, 16);
        let total = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let rest = read_exact(&mut stream, total - 16);
        let flags = u32::from_le_bytes(rest[0..4].try_into().unwrap());
        assert_ne!(
            flags & (1 << 1),
            0,
            "streaming hello reply must set moreToCome"
        );
        let d = Document::from_reader(&mut &rest[5..]).unwrap();
        assert!(d.get_bool("isWritablePrimary").unwrap());
    }

    // Client closes: the monitor thread must notice (EOF) and exit so stop()
    // drains promptly instead of pinning the connection for maxAwaitTimeMS.
    drop(stream);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut srv = server;
    std::thread::spawn(move || {
        srv.stop();
        // Fully drop the server (releasing its storage ref and closing the WT
        // connection's final checkpoint) *before* signalling done, so the test
        // doesn't remove the temp dir out from under an in-flight close.
        drop(srv);
        let _ = tx.send(());
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(15)).is_ok(),
        "stop() hung — streaming monitor thread was not reaped"
    );
}

/// Slow-loris defense (`ServerConfig::message_read_timeout`): a client that
/// starts a wire message and then stalls is dropped once the timeout elapses;
/// an idle connection that has sent *nothing* is untouched.
#[test]
fn slow_loris_partial_message_is_dropped_but_idle_is_not() {
    use std::time::Instant;

    let (storage, _tmp) = wt_storage();
    let cursors = Arc::new(CursorRegistry::new());
    let server = bind(
        "127.0.0.1:0",
        ServerConfig {
            message_read_timeout: Some(Duration::from_millis(400)),
            ..ServerConfig::default()
        },
        storage,
        cursors,
    )
    .unwrap();
    let addr = server.address();

    // 1. Idle connection (no bytes) sits past 2× the timeout and is still usable.
    let mut idle = connect(addr);
    std::thread::sleep(Duration::from_millis(900));
    let reply = op_msg(&mut idle, 1, &doc! {"hello": 1, "$db": "admin"});
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0);

    // 2. A started-but-never-finished message is reaped (4 of 16 header bytes).
    let mut stream = connect(addr);
    stream.write_all(&[16, 0, 0, 0]).unwrap();
    stream.flush().unwrap();

    let start = Instant::now();
    let mut buf = [0u8; 16];
    let n = stream.read(&mut buf).unwrap_or(0);
    let elapsed = start.elapsed();

    assert_eq!(n, 0, "server should close the stalled connection (EOF)");
    assert!(
        elapsed >= Duration::from_millis(250),
        "should not close before the timeout elapses: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "server (not the client's 5s read timeout) should close it: {elapsed:?}"
    );
}
