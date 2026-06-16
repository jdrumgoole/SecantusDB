//! End-to-end: drive the Rust server over a real TCP socket with an in-memory
//! `Storage`, speaking the wire protocol by hand (no pymongo needed). Proves the
//! whole path — socket → wire parse → dispatch → reply → socket — works
//! in-process, which is exactly what the embedded Python handle (R6) will wrap.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bson::{doc, Bson, Document};
use secantus_commands::storage::{RawHint, Storage, StorageError, UpdateOutcome};
use secantus_commands::CursorRegistry;
use secantus_server::{bind, ServerConfig};

// --- a minimal in-memory Storage ----------------------------------------

#[derive(Default)]
struct MemStorage {
    cols: Mutex<HashMap<(String, String), Vec<Document>>>,
}

fn matches(d: &Document, filter: &Document) -> bool {
    filter.iter().all(|(k, v)| d.get(k) == Some(v))
}

fn enc(d: &Document) -> Vec<u8> {
    let mut v = Vec::new();
    d.to_writer(&mut v).unwrap();
    v
}

impl Storage for MemStorage {
    fn insert(
        &self,
        db: &str,
        coll: &str,
        docs: Vec<Vec<u8>>,
        _ordered: bool,
    ) -> Result<(usize, Vec<Document>), StorageError> {
        let mut cols = self.cols.lock().unwrap();
        let bucket = cols.entry((db.into(), coll.into())).or_default();
        for b in &docs {
            bucket.push(Document::from_reader(&mut b.as_slice()).unwrap());
        }
        Ok((docs.len(), vec![]))
    }
    fn update_matching(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
        _update: &Document,
        _multi: bool,
        _upsert: bool,
    ) -> Result<UpdateOutcome, StorageError> {
        Ok(UpdateOutcome::default())
    }
    fn delete_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        _limit: usize,
    ) -> Result<usize, StorageError> {
        let mut cols = self.cols.lock().unwrap();
        let bucket = cols.entry((db.into(), coll.into())).or_default();
        let before = bucket.len();
        bucket.retain(|d| !matches(d, filter));
        Ok(before - bucket.len())
    }
    fn count_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<usize, StorageError> {
        let cols = self.cols.lock().unwrap();
        Ok(cols
            .get(&(db.into(), coll.into()))
            .map(|b| b.iter().filter(|d| matches(d, filter)).count())
            .unwrap_or(0))
    }
    fn find(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        _sort: Option<&Document>,
        _hint: Option<RawHint<'_>>,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let cols = self.cols.lock().unwrap();
        Ok(cols
            .get(&(db.into(), coll.into()))
            .map(|b| b.iter().filter(|d| matches(d, filter)).map(enc).collect())
            .unwrap_or_default())
    }
}

// --- wire helpers (client side) -----------------------------------------

const OP_MSG: i32 = 2013;
const OP_QUERY: i32 = 2004;

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

    // Read reply: header then body.
    let header = read_exact(stream, 16);
    let total = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    let rest = read_exact(stream, total - 16);
    // OP_MSG reply: flags(4) + kind(1) + bson
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

// --- the test ------------------------------------------------------------

#[test]
fn full_wire_roundtrip() {
    let storage: Arc<dyn Storage> = Arc::new(MemStorage::default());
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
    // connectionId must be int64.
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
    let storage: Arc<dyn Storage> = Arc::new(MemStorage::default());
    let cursors = Arc::new(CursorRegistry::new());
    let server = bind("127.0.0.1:0", ServerConfig::default(), storage, cursors).unwrap();
    let mut stream = connect(server.address());

    // seed 5 docs
    op_msg(
        &mut stream,
        1,
        &doc! {"insert": "c", "documents": [
            {"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 4}, {"_id": 5}
        ], "$db": "t"},
    );

    // find batchSize 2 ⇒ live cursor
    let reply = op_msg(
        &mut stream,
        2,
        &doc! {"find": "c", "batchSize": 2, "$db": "t"},
    );
    let cursor = reply.get_document("cursor").unwrap();
    assert_eq!(cursor.get_array("firstBatch").unwrap().len(), 2);
    let cid = cursor.get_i64("id").unwrap();
    assert_ne!(cid, 0);

    // getMore drains the next batch
    let reply = op_msg(
        &mut stream,
        3,
        &doc! {"getMore": cid, "collection": "c", "batchSize": 2, "$db": "t"},
    );
    let cursor = reply.get_document("cursor").unwrap();
    assert_eq!(cursor.get_array("nextBatch").unwrap().len(), 2);
    assert_eq!(cursor.get_i64("id").unwrap(), cid);

    // killCursors closes it
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

/// Slow-loris defense (`ServerConfig::message_read_timeout`): a client that
/// starts a wire message and then dribbles/stalls without completing it is
/// dropped once the timeout elapses from the first byte — instead of pinning a
/// connection thread forever. An idle connection that has sent *nothing* is
/// untouched (only an in-progress message is bounded), preserving the
/// mongod-conformant "never close an idle pooled connection" behaviour.
#[test]
fn slow_loris_partial_message_is_dropped_but_idle_is_not() {
    use std::time::Instant;

    let storage: Arc<dyn Storage> = Arc::new(MemStorage::default());
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

    // 1. An idle connection that sends no bytes at all sits past 2× the timeout
    //    and is still fully usable (the timeout does not bound idle waits).
    let mut idle = connect(addr);
    std::thread::sleep(Duration::from_millis(900));
    let reply = op_msg(&mut idle, 1, &doc! {"hello": 1, "$db": "admin"});
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0);

    // 2. A started-but-never-finished message is reaped. Send 4 bytes of a
    //    16-byte header (the message has "started") then stall.
    let mut stream = connect(addr);
    stream.write_all(&[16, 0, 0, 0]).unwrap();
    stream.flush().unwrap();

    // The server closes the socket shortly after the timeout ⇒ the client read
    // returns EOF (0 bytes). The client's own 5s read timeout would otherwise
    // fire at ~5s, so a bounded elapsed proves the *server* closed it.
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
