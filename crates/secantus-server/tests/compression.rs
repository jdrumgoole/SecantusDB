//! Wire compression (`OP_COMPRESSED`, 2012) at the connection loop, over a
//! storage that holds nothing: what mongod 8.2.11 does with a compressed
//! request (measured 2026-10-15), checked without WiredTiger so it runs in
//! the `rust` CI job. The driver-level and cursor tests over real WiredTiger
//! are in `crates/secantusdb/tests/compression.rs`. Also here, on the same
//! raw-socket harness: the compression counters `serverStatus` reports, and
//! what the loop does with an opcode it does not serve.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use bson::{doc, Bson, Document};
use secantus_commands::storage::{RawHint, Storage, StorageError, UpdateOutcome};
use secantus_commands::CursorRegistry;
use secantus_server::{bind, RunningServer, ServerConfig};
use secantus_wire::{
    build_op_msg_reply, compress_frame, parse_op_compressed, Compressor, Header, Op, HEADER_SIZE,
    OP_COMPRESSED, OP_MSG,
};

/// A do-nothing storage — `hello` never touches it, but `bind` needs one.
struct NoStorage;

impl Storage for NoStorage {
    fn insert(
        &self,
        _db: &str,
        _coll: &str,
        _docs: Vec<Vec<u8>>,
        _ordered: bool,
    ) -> Result<(usize, Vec<Document>), StorageError> {
        Ok((0, Vec::new()))
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
        _db: &str,
        _coll: &str,
        _filter: &Document,
        _limit: usize,
    ) -> Result<usize, StorageError> {
        Ok(0)
    }
    fn count_matching(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
    ) -> Result<usize, StorageError> {
        Ok(0)
    }
    fn find(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
        _sort: Option<&Document>,
        _hint: Option<RawHint<'_>>,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        Ok(Vec::new())
    }
}

fn start() -> RunningServer {
    let storage: Arc<dyn Storage> = Arc::new(NoStorage);
    let config = ServerConfig {
        replica_set_name: None,
        ..ServerConfig::default()
    };
    bind(
        "127.0.0.1:0",
        config,
        storage,
        Arc::new(CursorRegistry::new()),
    )
    .unwrap()
}

fn connect(addr: SocketAddr) -> TcpStream {
    let sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    sock
}

fn request(cmd: &Document) -> Vec<u8> {
    let mut body = Vec::new();
    cmd.to_writer(&mut body).unwrap();
    build_op_msg_reply(0, 1, &body, 0)
}

/// Read one reply: the compressor id it came under, and its body document.
/// `None` when the server closed the connection.
fn reply(sock: &mut TcpStream) -> Option<(Option<u8>, Document)> {
    let mut head = [0u8; HEADER_SIZE];
    sock.read_exact(&mut head).ok()?;
    let header = Header::unpack(&head).unwrap();
    let mut body = vec![0u8; header.body_len().unwrap()];
    sock.read_exact(&mut body).unwrap();
    let (compressor, op, body) = if header.op_code == OP_COMPRESSED {
        let wrapped = parse_op_compressed(&body).unwrap();
        let (c, inflated) = wrapped.decompress().unwrap();
        (Some(c.id()), wrapped.original_opcode, inflated)
    } else {
        (None, header.op_code, body)
    };
    match secantus_wire::parse_body(op, &body).unwrap() {
        Op::Msg(msg) => Some((
            compressor,
            secantus_commands::bsondoc::from_bytes(msg.body).unwrap(),
        )),
        _ => panic!("expected OP_MSG"),
    }
}

fn command(sock: &mut TcpStream, cmd: Document, c: Option<Compressor>) -> (Option<u8>, Document) {
    let frame = request(&cmd);
    let frame = match c {
        Some(c) => compress_frame(&frame, c).unwrap(),
        None => frame,
    };
    sock.write_all(&frame).unwrap();
    reply(sock).expect("a reply")
}

#[test]
fn a_compressed_request_is_answered_under_the_same_compressor() {
    let mut server = start();
    let mut sock = connect(server.address());
    let (_, hello) = command(
        &mut sock,
        doc! {"hello": 1, "compression": ["zstd", "noop", "zlib", "snappy"], "$db": "admin"},
        None,
    );
    assert_eq!(hello.keys().next().map(String::as_str), Some("compression"));
    let want: Vec<Bson> = ["zstd", "zlib", "snappy"].map(Bson::from).to_vec();
    assert_eq!(hello.get_array("compression").unwrap(), &want);
    for c in [Compressor::Snappy, Compressor::Zlib, Compressor::Zstd] {
        let (used, ping) = command(&mut sock, doc! {"ping": 1, "$db": "admin"}, Some(c));
        assert_eq!(used, Some(c.id()));
        assert_eq!(ping.get_f64("ok").unwrap(), 1.0);
        let (used, hello) = command(&mut sock, doc! {"hello": 1, "$db": "admin"}, Some(c));
        assert_eq!(used, Some(c.id()));
        assert_eq!(hello.get_array("compression").unwrap(), &want);
    }
    let (used, _) = command(&mut sock, doc! {"ping": 1, "$db": "admin"}, None);
    assert_eq!(used, None);
    server.stop();
}

#[test]
fn a_frame_whose_declared_size_is_wrong_closes_the_connection() {
    let mut server = start();
    let frame = request(&doc! {"ping": 1, "$db": "admin"});
    let body = &frame[HEADER_SIZE..];
    for (id, declared, data) in [
        (
            2u8,
            body.len() as i32 + 1,
            Compressor::Zlib.compress(body).unwrap(),
        ),
        (
            1,
            body.len() as i32 - 1,
            Compressor::Snappy.compress(body).unwrap(),
        ),
        (3, -1, Compressor::Zstd.compress(body).unwrap()),
        (3, 48_000_000, Compressor::Zstd.compress(body).unwrap()),
        (0, body.len() as i32, body.to_vec()),
    ] {
        let mut sock = connect(server.address());
        let mut payload = OP_MSG.to_le_bytes().to_vec();
        payload.extend_from_slice(&declared.to_le_bytes());
        payload.push(id);
        payload.extend_from_slice(&data);
        let header = Header {
            message_length: (HEADER_SIZE + payload.len()) as i32,
            request_id: 1,
            response_to: 0,
            op_code: OP_COMPRESSED,
        };
        sock.write_all(&header.pack()).unwrap();
        sock.write_all(&payload).unwrap();
        assert!(reply(&mut sock).is_none(), "id {id}, declared {declared}");
    }
    server.stop();
}

/// `serverStatus.network.compression` counts, per compressor, the message
/// bodies it compressed and decompressed: header and `OP_COMPRESSED` prefix
/// excluded, as mongod 8.2.11 counts them (RM-147).
#[test]
fn server_status_counts_what_each_compressor_handled() {
    let mut server = start();
    let mut sock = connect(server.address());
    let counts = |sock: &mut TcpStream| {
        let (_, status) = command(sock, doc! {"serverStatus": 1, "$db": "admin"}, None);
        status
            .get_document("network")
            .unwrap()
            .get_document("compression")
            .unwrap()
            .clone()
    };
    let before = counts(&mut sock);
    let zero = doc! {
        "compressor": {"bytesIn": 0_i64, "bytesOut": 0_i64},
        "decompressor": {"bytesIn": 0_i64, "bytesOut": 0_i64},
    };
    assert_eq!(
        before,
        doc! {"snappy": zero.clone(), "zstd": zero.clone(), "zlib": zero.clone()}
    );

    // One zlib ping, its sizes worked out the way mongod counts them.
    let frame = request(&doc! {"ping": 1, "$db": "admin"});
    let body_len = (frame.len() - HEADER_SIZE) as i64;
    let wrapped = compress_frame(&frame, Compressor::Zlib).unwrap();
    let compressed_len = (wrapped.len() - HEADER_SIZE - 9) as i64;
    sock.write_all(&wrapped).unwrap();
    let mut head = [0u8; HEADER_SIZE];
    sock.read_exact(&mut head).unwrap();
    let header = Header::unpack(&head).unwrap();
    assert_eq!(header.op_code, OP_COMPRESSED);
    let mut reply_body = vec![0u8; header.body_len().unwrap()];
    sock.read_exact(&mut reply_body).unwrap();
    let reply = parse_op_compressed(&reply_body).unwrap();

    let after = counts(&mut sock);
    assert_eq!(
        after.get_document("zlib").unwrap(),
        &doc! {
            "compressor": {
                "bytesIn": i64::from(reply.uncompressed_size),
                "bytesOut": reply.data.len() as i64,
            },
            "decompressor": {"bytesIn": compressed_len, "bytesOut": body_len},
        }
    );
    assert_eq!(after.get_document("snappy").unwrap(), &zero);
    assert_eq!(after.get_document("zstd").unwrap(), &zero);
    server.stop();
}

fn raw_frame(op_code: i32, request_id: i32, body: &[u8]) -> Vec<u8> {
    let header = Header {
        message_length: (HEADER_SIZE + body.len()) as i32,
        request_id,
        response_to: 0,
        op_code,
    };
    let mut out = header.pack().to_vec();
    out.extend_from_slice(body);
    out
}

/// mongod 8.2.11 sends nothing back for a frame whose opcode it does not
/// serve and answers the next request on the same connection -- compressed
/// or not (RM-144).
#[test]
fn an_unserved_opcode_is_ignored_and_the_connection_kept() {
    let mut server = start();
    let mut sock = connect(server.address());
    for op in [9999, 1, 2010] {
        sock.write_all(&raw_frame(op, 7, &[0u8; 20])).unwrap();
        let (_, ping) = command(&mut sock, doc! {"ping": 1, "$db": "admin"}, None);
        assert_eq!(ping.get_f64("ok").unwrap(), 1.0, "op {op}");
    }
    let wrapped = compress_frame(&raw_frame(9999, 8, &[0u8; 20]), Compressor::Zlib).unwrap();
    sock.write_all(&wrapped).unwrap();
    let (_, ping) = command(&mut sock, doc! {"ping": 1, "$db": "admin"}, None);
    assert_eq!(ping.get_f64("ok").unwrap(), 1.0);
    // A body too short to hold anything is an unusable frame: closed.
    let mut short = connect(server.address());
    short.write_all(&raw_frame(9999, 9, &[0u8; 3])).unwrap();
    assert!(reply(&mut short).is_none());
    server.stop();
}

/// mongod 8.2.11 refuses a legacy OP_GET_MORE with an OP_REPLY --
/// QueryFailure, cursor 0, one `{$err, code: 5739101, ok: 0}` document --
/// and keeps the connection (RM-145).
#[test]
fn a_legacy_get_more_is_refused_with_an_op_reply() {
    let mut server = start();
    let mut sock = connect(server.address());
    let mut body = 0i32.to_le_bytes().to_vec();
    body.extend_from_slice(b"db.c\x00");
    body.extend_from_slice(&0i32.to_le_bytes());
    body.extend_from_slice(&12345i64.to_le_bytes());
    sock.write_all(&raw_frame(2005, 11, &body)).unwrap();
    let mut head = [0u8; HEADER_SIZE];
    sock.read_exact(&mut head).unwrap();
    let header = Header::unpack(&head).unwrap();
    assert_eq!((header.op_code, header.response_to), (1, 11));
    let mut reply_body = vec![0u8; header.body_len().unwrap()];
    sock.read_exact(&mut reply_body).unwrap();
    let flags = u32::from_le_bytes(reply_body[0..4].try_into().unwrap());
    let cursor_id = i64::from_le_bytes(reply_body[4..12].try_into().unwrap());
    let starting_from = i32::from_le_bytes(reply_body[12..16].try_into().unwrap());
    let returned = i32::from_le_bytes(reply_body[16..20].try_into().unwrap());
    assert_eq!((flags, cursor_id, starting_from, returned), (2, 0, 0, 1));
    let refusal = secantus_commands::bsondoc::from_bytes(&reply_body[20..]).unwrap();
    assert_eq!(
        refusal,
        doc! {
            "$err": "OP_GET_MORE is no longer supported. The client driver may require an \
                     upgrade. For more details see \
                     https://dochub.mongodb.org/core/legacy-opcode-removal",
            "code": 5739101,
            "ok": 0.0,
        }
    );
    let (_, ping) = command(&mut sock, doc! {"ping": 1, "$db": "admin"}, None);
    assert_eq!(ping.get_f64("ok").unwrap(), 1.0);
    server.stop();
}
