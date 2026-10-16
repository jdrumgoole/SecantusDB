//! Wire compression (`OP_COMPRESSED`, 2012) end to end, over real WiredTiger:
//! the official driver with each of mongod's three default compressors, and
//! raw frames for what a driver never sends.
//!
//! The expectations are mongod 8.2.11's, measured 2026-10-15 over a raw
//! socket (see `secantus_wire::compression` and `handshake::hello`).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use bson::{doc, Bson, Document};
use mongodb::sync::Client;
use secantus_mdb::Server;
use secantus_wire::{
    build_op_msg_reply, compress_frame, parse_op_compressed, parse_op_msg, Compressor, Header,
    HEADER_SIZE, OP_COMPRESSED, OP_MSG, OP_MSG_FLAG_EXHAUST_ALLOWED, OP_MSG_FLAG_MORE_TO_COME,
};

const ALL: [Compressor; 3] = [Compressor::Snappy, Compressor::Zlib, Compressor::Zstd];

#[test]
fn the_driver_round_trips_crud_cursors_and_a_large_document_under_each_compressor() {
    let server = Server::start().expect("start");
    for c in ALL {
        let uri = format!(
            "{}&compressors={}&serverSelectionTimeoutMS=10000",
            server.uri(),
            c.name()
        );
        let client = Client::with_uri_str(uri).expect("client");
        let db = client.database(&format!("compress_{}", c.name()));
        let hello = db.run_command(doc! {"hello": 1}).run().unwrap();
        // A hello without `compression` repeats what the handshake settled.
        assert_eq!(
            hello.get_array("compression").unwrap(),
            &vec![Bson::String(c.name().into())]
        );

        let coll = db.collection::<Document>("c");
        coll.insert_many((0..1000).map(|i| doc! {"_id": i, "v": "x".repeat(i as usize % 50)}))
            .run()
            .unwrap();
        let big = "y".repeat(15 * 1024 * 1024);
        coll.insert_one(doc! {"_id": "big", "s": &big})
            .run()
            .unwrap();
        coll.update_many(doc! {"_id": {"$lt": 100}}, doc! {"$set": {"u": true}})
            .run()
            .unwrap();
        coll.delete_many(doc! {"_id": {"$gte": 900, "$lt": 950}})
            .run()
            .unwrap();

        let got = coll
            .find_one(doc! {"_id": "big"})
            .run()
            .unwrap()
            .expect("the large document");
        assert_eq!(got.get_str("s").unwrap(), big);

        let ids: Vec<i32> = coll
            .find(doc! {"_id": {"$type": "int"}})
            .sort(doc! {"_id": 1})
            .batch_size(7)
            .run()
            .unwrap()
            .map(|d| d.unwrap().get_i32("_id").unwrap())
            .collect();
        let want: Vec<i32> = (0..900).chain(950..1000).collect();
        assert_eq!(ids, want, "{c:?}");
        assert_eq!(
            coll.count_documents(doc! {"u": true}).run().unwrap(),
            100,
            "{c:?}"
        );
        db.drop().run().unwrap();
    }
}

/// One raw connection that can send each request compressed or not.
struct Raw {
    sock: TcpStream,
    next_id: i32,
}

/// A reply as it came off the wire.
struct Frame {
    /// The compressor id, when the reply was `OP_COMPRESSED`.
    compressor: Option<u8>,
    request_id: i32,
    response_to: i32,
    flags: u32,
    doc: Document,
}

impl Raw {
    fn connect(server: &Server) -> Raw {
        let sock = TcpStream::connect(server.address()).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Raw { sock, next_id: 0 }
    }

    fn op_msg(&mut self, cmd: &Document, flags: u32) -> Vec<u8> {
        self.next_id += 1;
        let mut body = Vec::new();
        cmd.to_writer(&mut body).unwrap();
        // The reply builder frames a request just as well: responseTo 0.
        build_op_msg_reply(0, self.next_id, &body, flags)
    }

    fn send(&mut self, cmd: &Document, compressor: Option<Compressor>, flags: u32) {
        let frame = self.op_msg(cmd, flags);
        let frame = match compressor {
            Some(c) => compress_frame(&frame, c).unwrap(),
            None => frame,
        };
        self.sock.write_all(&frame).unwrap();
    }

    /// Read one reply; `None` when the server closed the connection.
    fn recv(&mut self) -> Option<Frame> {
        let mut head = [0u8; HEADER_SIZE];
        if self.sock.read_exact(&mut head).is_err() {
            return None;
        }
        let header = Header::unpack(&head).unwrap();
        let mut body = vec![0u8; header.message_length as usize - HEADER_SIZE];
        self.sock.read_exact(&mut body).unwrap();
        let (compressor, op, body) = if header.op_code == OP_COMPRESSED {
            let wrapped = parse_op_compressed(&body).unwrap();
            let (c, inflated) = wrapped.decompress().unwrap();
            (Some(c.id()), wrapped.original_opcode, inflated)
        } else {
            (None, header.op_code, body)
        };
        assert_eq!(op, OP_MSG);
        let msg = parse_op_msg(&body).unwrap();
        Some(Frame {
            compressor,
            request_id: header.request_id,
            response_to: header.response_to,
            flags: msg.flags,
            doc: secantus_commands::bsondoc::from_bytes(msg.body).unwrap(),
        })
    }

    fn command(&mut self, cmd: Document, compressor: Option<Compressor>) -> Frame {
        self.send(&cmd, compressor, 0);
        self.recv().expect("a reply")
    }

    fn hello(&mut self, extra: Document) -> Document {
        let mut cmd = doc! {"hello": 1};
        cmd.extend(extra);
        cmd.insert("$db", "admin");
        self.command(cmd, None).doc
    }
}

fn names(list: &[&str]) -> Bson {
    Bson::Array(list.iter().map(|n| Bson::String(n.to_string())).collect())
}

#[test]
fn hello_settles_compressors_as_mongod_does() {
    let server = Server::start().expect("start");
    let answer = |reply: &Document| reply.get("compression").cloned();

    // The ones it speaks, in the client's order, repeats kept, first key.
    let mut raw = Raw::connect(&server);
    let reply = raw.hello(doc! {"helloOk": true, "compression": ["zstd", "noop", "bogus", "ZLIB", "snappy", "zlib", "zstd"]});
    assert_eq!(
        answer(&reply),
        Some(names(&["zstd", "snappy", "zlib", "zstd"]))
    );
    assert_eq!(reply.keys().next().map(String::as_str), Some("compression"));
    assert_eq!(reply.keys().nth(1).map(String::as_str), Some("helloOk"));

    // Kept by a hello that names none (absent or null), and by one refused.
    assert_eq!(
        answer(&raw.hello(doc! {})),
        Some(names(&["zstd", "snappy", "zlib", "zstd"]))
    );
    assert_eq!(
        answer(&raw.hello(doc! {"compression": Bson::Null})),
        Some(names(&["zstd", "snappy", "zlib", "zstd"]))
    );
    let refused = raw.hello(doc! {"compression": "zlib"});
    assert_eq!(refused.get_i32("code").unwrap(), 14, "{refused:?}");
    let refused = raw.hello(doc! {"compression": ["zlib", 1]});
    assert_eq!(refused.get_i32("code").unwrap(), 14, "{refused:?}");
    let refused = raw.hello(doc! {"compression": ["zlib"], "helloOk": "x"});
    assert_eq!(refused.get_i32("code").unwrap(), 14, "{refused:?}");
    assert_eq!(
        answer(&raw.hello(doc! {})),
        Some(names(&["zstd", "snappy", "zlib", "zstd"]))
    );

    // Replaced by the next list; an error AFTER the negotiation keeps it.
    assert_eq!(
        answer(&raw.hello(doc! {"compression": ["snappy"]})),
        Some(names(&["snappy"]))
    );
    let refused = raw.hello(doc! {"compression": ["zlib"], "saslSupportedMechs": "nodot"});
    assert_eq!(refused.get_i32("code").unwrap(), 2, "{refused:?}");
    assert_eq!(answer(&raw.hello(doc! {})), Some(names(&["zlib"])));
    let refused = raw.hello(doc! {"compression": ["zstd"], "speculativeAuthenticate": {}});
    assert_eq!(refused.get_i32("code").unwrap(), 2, "{refused:?}");
    assert_eq!(answer(&raw.hello(doc! {})), Some(names(&["zstd"])));

    // Cleared by an empty list, or one naming nothing it speaks.
    assert_eq!(answer(&raw.hello(doc! {"compression": []})), None);
    assert_eq!(answer(&raw.hello(doc! {})), None);
    assert_eq!(
        answer(&raw.hello(doc! {"compression": ["zlib"]})),
        Some(names(&["zlib"]))
    );
    assert_eq!(answer(&raw.hello(doc! {"compression": ["noop"]})), None);
    assert_eq!(answer(&raw.hello(doc! {})), None);

    // isMaster settles them too.
    let mut raw = Raw::connect(&server);
    let reply = raw.command(
        doc! {"isMaster": 1, "compression": ["zlib"], "$db": "admin"},
        None,
    );
    assert_eq!(answer(&reply.doc), Some(names(&["zlib"])));
}

#[test]
fn a_compressed_request_is_answered_under_its_own_compressor() {
    let server = Server::start().expect("start");
    for c in ALL {
        // No negotiation needed: mongod decompresses under any compressor it
        // has enabled, and answers under the one the request used.
        let mut raw = Raw::connect(&server);
        raw.hello(doc! {"compression": ["zlib"]});
        let ping = raw.command(doc! {"ping": 1, "$db": "admin"}, Some(c));
        assert_eq!(ping.compressor, Some(c.id()));
        assert_eq!(ping.doc.get_f64("ok").unwrap(), 1.0);
        // hello itself is answered compressed when it came compressed.
        let hello = raw.command(doc! {"hello": 1, "$db": "admin"}, Some(c));
        assert_eq!(hello.compressor, Some(c.id()));
        assert_eq!(
            hello.doc.get_array("compression").unwrap(),
            &vec![Bson::from("zlib")]
        );
        // An uncompressed request is answered uncompressed.
        let plain = raw.command(doc! {"ping": 1, "$db": "admin"}, None);
        assert_eq!(plain.compressor, None);

        // A cursor, and every frame of an exhaust stream.
        let db = format!("compress_raw_{}", c.name());
        let docs: Vec<Document> = (0..10).map(|i| doc! {"_id": i}).collect();
        let ins = raw.command(doc! {"insert": "c", "documents": docs, "$db": &db}, Some(c));
        assert_eq!(ins.doc.get_i32("n").unwrap(), 10, "{:?}", ins.doc);
        let find = raw.command(doc! {"find": "c", "batchSize": 2, "$db": &db}, Some(c));
        assert_eq!(find.compressor, Some(c.id()));
        let cursor = find.doc.get_document("cursor").unwrap();
        let id = cursor.get_i64("id").unwrap();
        assert_ne!(id, 0);
        raw.send(
            &doc! {"getMore": id, "collection": "c", "batchSize": 3, "$db": &db},
            Some(c),
            OP_MSG_FLAG_EXHAUST_ALLOWED,
        );
        let mut seen = 2;
        loop {
            let frame = raw.recv().expect("an exhaust frame");
            assert_eq!(frame.compressor, Some(c.id()));
            let batch = frame
                .doc
                .get_document("cursor")
                .unwrap()
                .get_array("nextBatch")
                .unwrap();
            seen += batch.len();
            if frame.flags & OP_MSG_FLAG_MORE_TO_COME == 0 {
                break;
            }
        }
        assert_eq!(seen, 10);
        let drop = raw.command(doc! {"dropDatabase": 1, "$db": &db}, Some(c));
        assert_eq!(drop.doc.get_f64("ok").unwrap(), 1.0);
    }
}

/// Send `body` (an `OP_MSG` body) wrapped with the given prefix, and report
/// whether the server answered rather than closing the connection.
fn answers_wrapped(server: &Server, id: u8, declared: i32, data: &[u8]) -> bool {
    let mut raw = Raw::connect(server);
    let mut payload = Vec::new();
    payload.extend_from_slice(&OP_MSG.to_le_bytes());
    payload.extend_from_slice(&declared.to_le_bytes());
    payload.push(id);
    payload.extend_from_slice(data);
    let header = Header {
        message_length: (HEADER_SIZE + payload.len()) as i32,
        request_id: 1,
        response_to: 0,
        op_code: OP_COMPRESSED,
    };
    raw.sock.write_all(&header.pack()).unwrap();
    raw.sock.write_all(&payload).unwrap();
    raw.recv().is_some()
}

#[test]
fn a_compressed_frame_that_does_not_add_up_closes_the_connection() {
    let server = Server::start().expect("start");
    let mut raw = Raw::connect(&server);
    let frame = raw.op_msg(&doc! {"ping": 1, "$db": "admin"}, 0);
    let body = &frame[HEADER_SIZE..];
    let size = body.len() as i32;
    for c in ALL {
        let data = c.compress(body).unwrap();
        assert!(answers_wrapped(&server, c.id(), size, &data), "{c:?}");
        for declared in [size + 1, size - 1, 0, -1, 48_000_000, i32::MAX] {
            assert!(
                !answers_wrapped(&server, c.id(), declared, &data),
                "{c:?} declared {declared}"
            );
        }
        assert!(
            !answers_wrapped(&server, c.id(), size, b"\x01\x02garbage"),
            "{c:?}"
        );
    }
    // noop is not enabled, and nothing else is a compressor.
    for id in [0u8, 4, 255] {
        assert!(!answers_wrapped(&server, id, size, body), "id {id}");
    }
    // The server is unhurt.
    let mut raw = Raw::connect(&server);
    assert_eq!(
        raw.command(doc! {"ping": 1, "$db": "admin"}, Some(Compressor::Zstd))
            .doc
            .get_f64("ok")
            .unwrap(),
        1.0
    );
}

/// An exhaust getMore streams each getMore reply as it is, and the reply that
/// closes the cursor ends the stream (RM-146). Measured on mongod 8.2.11 with
/// `find` batchSize 3: over 10 documents the frames are [3,4,5] and [6,7,8]
/// with `moreToCome` and the live id, then [9] without it and id 0; over 9
/// the last frame is an empty batch with id 0. Each frame after the first
/// answers the frame before it: its `responseTo` is that frame's requestID.
#[test]
fn an_exhaust_stream_ends_on_the_reply_that_closes_the_cursor() {
    let server = Server::start().expect("start");
    for compressor in [None, Some(Compressor::Zlib)] {
        for (n, last) in [(10, vec![9]), (9, vec![])] {
            let mut raw = Raw::connect(&server);
            let db = format!("exhaust_end_{n}");
            let docs: Vec<Document> = (0..n).map(|i| doc! {"_id": i}).collect();
            raw.command(doc! {"insert": "c", "documents": docs, "$db": &db}, None);
            let find = raw.command(
                doc! {"find": "c", "batchSize": 3, "sort": {"_id": 1}, "$db": &db},
                None,
            );
            let id = find
                .doc
                .get_document("cursor")
                .unwrap()
                .get_i64("id")
                .unwrap();
            assert_ne!(id, 0);
            raw.send(
                &doc! {"getMore": id, "collection": "c", "batchSize": 3, "$db": &db},
                compressor,
                OP_MSG_FLAG_EXHAUST_ALLOWED,
            );
            let mut answers = raw.next_id;
            let mut frames = Vec::new();
            while frames.len() < 6 {
                let frame = raw.recv().expect("an exhaust frame");
                assert_eq!(frame.compressor, compressor.map(Compressor::id));
                assert_eq!(
                    frame.response_to,
                    answers,
                    "{n} docs, frame {}",
                    frames.len()
                );
                answers = frame.request_id;
                let cursor = frame.doc.get_document("cursor").unwrap();
                let ids: Vec<i32> = cursor
                    .get_array("nextBatch")
                    .unwrap()
                    .iter()
                    .map(|d| d.as_document().unwrap().get_i32("_id").unwrap())
                    .collect();
                let more = frame.flags & OP_MSG_FLAG_MORE_TO_COME != 0;
                frames.push((more, cursor.get_i64("id").unwrap() != 0, ids));
                if !more {
                    break;
                }
            }
            assert_eq!(
                frames,
                vec![
                    (true, true, vec![3, 4, 5]),
                    (true, true, vec![6, 7, 8]),
                    (false, false, last),
                ],
                "{n} docs, {compressor:?}"
            );
            // The connection is still in step: the next reply is the ping's.
            let ping = raw.command(doc! {"ping": 1, "$db": "admin"}, None);
            assert_eq!(ping.response_to, raw.next_id);
            raw.command(doc! {"dropDatabase": 1, "$db": &db}, None);
        }
    }
}
