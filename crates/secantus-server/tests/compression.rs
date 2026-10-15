//! Wire compression (`OP_COMPRESSED`, 2012) at the connection loop, over a
//! storage that holds nothing: what mongod 8.2.11 does with a compressed
//! request (measured 2026-10-15), checked without WiredTiger so it runs in
//! the `rust` CI job. The driver-level and cursor tests over real WiredTiger
//! are in `crates/secantusdb/tests/compression.rs`.

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
        Op::Query(_) => panic!("expected OP_MSG"),
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
