//! A minimal blocking MongoDB client, built straight on `secantus-wire`.
//!
//! This is the load agent's connection. It is deliberately *not* the official
//! driver: for a server benchmark the client should add as close to zero
//! overhead as possible, because client-side cost is the usual reason a load
//! machine saturates before the database does. One `TcpStream`, one reusable
//! read buffer, one `OP_MSG` per operation, no connection pool, no topology
//! monitoring, no retry logic.
//!
//! Retries in particular are deliberately absent: a benchmark that silently
//! retries a failed operation reports throughput for work the server did not
//! successfully do. Errors are counted and surfaced.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use bson::{doc, Document};
use secantus_wire::{parse_op_msg, Header, HEADER_SIZE, OP_MSG};

use crate::BenchResult;

pub struct Conn {
    stream: TcpStream,
    db: String,
    request_id: i32,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
}

impl Conn {
    /// Connect and complete the `hello` handshake.
    pub fn connect(addr: &str, db: &str, timeout: Duration) -> BenchResult<Conn> {
        let sock = addr
            .to_socket_addrs()
            .map_err(|e| format!("cannot resolve {addr}: {e}"))?
            .next()
            .ok_or_else(|| format!("no address for {addr}"))?;
        let stream = TcpStream::connect_timeout(&sock, timeout)
            .map_err(|e| format!("connect to {addr} failed: {e}"))?;
        // Nagle would coalesce small command frames and add milliseconds of
        // latency to exactly the request/response pattern being measured.
        stream
            .set_nodelay(true)
            .map_err(|e| format!("set_nodelay: {e}"))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| format!("set_read_timeout: {e}"))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| format!("set_write_timeout: {e}"))?;
        let mut conn = Conn {
            stream,
            db: db.to_string(),
            request_id: 1,
            read_buf: Vec::with_capacity(64 * 1024),
            write_buf: Vec::with_capacity(64 * 1024),
        };
        conn.handshake()?;
        Ok(conn)
    }

    fn handshake(&mut self) -> BenchResult<()> {
        let hello = doc! {
            "hello": 1,
            "client": {
                "driver": { "name": "secantus-bench", "version": env!("CARGO_PKG_VERSION") },
                "os": { "type": std::env::consts::OS },
            },
            "$db": "admin",
        };
        self.command(hello).map(|_| ())
    }

    /// Send one command and return its reply, failing on `ok: 0`.
    pub fn command(&mut self, cmd: Document) -> BenchResult<Document> {
        let reply = self.raw_command(cmd)?;
        let ok = reply
            .get_f64("ok")
            .ok()
            .or_else(|| reply.get_i32("ok").ok().map(f64::from))
            .or_else(|| reply.get_i64("ok").ok().map(|v| v as f64))
            .unwrap_or(0.0);
        if ok != 1.0 {
            let msg = reply.get_str("errmsg").unwrap_or("(no errmsg)");
            let code = reply.get_i32("code").unwrap_or(0);
            return Err(format!("server error {code}: {msg}"));
        }
        Ok(reply)
    }

    fn raw_command(&mut self, cmd: Document) -> BenchResult<Document> {
        self.write_buf.clear();
        // OP_MSG request: header, u32 flags (0), one kind-0 section.
        self.write_buf.extend_from_slice(&[0u8; HEADER_SIZE]);
        self.write_buf.extend_from_slice(&0u32.to_le_bytes());
        self.write_buf.push(0u8);
        cmd.to_writer(&mut self.write_buf)
            .map_err(|e| format!("encode command: {e}"))?;

        let len = self.write_buf.len() as i32;
        self.request_id = self.request_id.wrapping_add(1).max(1);
        let header = Header {
            message_length: len,
            request_id: self.request_id,
            response_to: 0,
            op_code: OP_MSG,
        };
        self.write_buf[0..HEADER_SIZE].copy_from_slice(&header.pack());
        self.stream
            .write_all(&self.write_buf)
            .map_err(|e| format!("write: {e}"))?;

        let mut head = [0u8; HEADER_SIZE];
        self.stream
            .read_exact(&mut head)
            .map_err(|e| format!("read header: {e}"))?;
        let reply_header = Header::unpack(&head).map_err(|e| format!("bad reply header: {e}"))?;
        if reply_header.op_code != OP_MSG {
            return Err(format!(
                "reply op_code {} is not OP_MSG",
                reply_header.op_code
            ));
        }
        let body_len = reply_header
            .body_len()
            .map_err(|e| format!("bad reply length: {e}"))?;
        self.read_buf.clear();
        self.read_buf.resize(body_len, 0);
        self.stream
            .read_exact(&mut self.read_buf)
            .map_err(|e| format!("read body: {e}"))?;
        let msg = parse_op_msg(&self.read_buf).map_err(|e| format!("parse reply: {e}"))?;
        bson::from_slice::<Document>(msg.body).map_err(|e| format!("decode reply: {e}"))
    }

    // -- the operations the load agent issues ------------------------------

    pub fn ping(&mut self) -> BenchResult<()> {
        self.command(doc! { "ping": 1, "$db": "admin" }).map(|_| ())
    }

    pub fn drop_collection(&mut self, coll: &str) -> BenchResult<()> {
        // A missing collection is not an error here: `setup` drops before it
        // creates, and the first run has nothing to drop.
        match self.raw_command(doc! { "drop": coll, "$db": self.db.clone() }) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn create_index_on_n(&mut self, coll: &str) -> BenchResult<()> {
        self.command(doc! {
            "createIndexes": coll,
            "indexes": [ doc! { "key": doc! { "n": 1 }, "name": "n_1" } ],
            "$db": self.db.clone(),
        })
        .map(|_| ())
    }

    pub fn insert(&mut self, coll: &str, documents: Vec<Document>) -> BenchResult<()> {
        let reply = self.command(doc! {
            "insert": coll,
            "documents": documents,
            "ordered": false,
            "$db": self.db.clone(),
        })?;
        // `ok: 1` with a populated writeErrors array is still a failed write;
        // counting it as a success would inflate throughput.
        if let Ok(errors) = reply.get_array("writeErrors") {
            if !errors.is_empty() {
                return Err(format!(
                    "{} write error(s), first: {:?}",
                    errors.len(),
                    errors[0]
                ));
            }
        }
        Ok(())
    }

    pub fn find_by_n(&mut self, coll: &str, n: i64) -> BenchResult<()> {
        self.command(doc! {
            "find": coll,
            "filter": doc! { "n": n },
            "limit": 1i32,
            "$db": self.db.clone(),
        })
        .map(|_| ())
    }

    pub fn update_by_n(&mut self, coll: &str, n: i64) -> BenchResult<()> {
        let reply = self.command(doc! {
            "update": coll,
            "updates": [ doc! { "q": doc! { "n": n }, "u": doc! { "$inc": doc! { "c": 1 } } } ],
            "$db": self.db.clone(),
        })?;
        if let Ok(errors) = reply.get_array("writeErrors") {
            if !errors.is_empty() {
                return Err(format!(
                    "{} write error(s), first: {:?}",
                    errors.len(),
                    errors[0]
                ));
            }
        }
        Ok(())
    }
}

/// The document every insert writes: an `n` sequence counter, a mutable `c`
/// for updates to bump, and a payload that sets the wire size.
pub fn make_document(n: i64, payload: &str) -> Document {
    doc! { "n": n, "c": 0i32, "payload": payload }
}

/// How to fill the payload.
///
/// This is not a cosmetic choice. Both engines compress their tables (zlib
/// here, snappy in mongod), so a payload of one repeated character compresses
/// to almost nothing and any measurement of bytes-on-disk becomes a
/// measurement of the compressor. `Random` is the honest setting whenever
/// storage volume matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    /// A single repeated character — highly compressible.
    Repeat,
    /// Pseudo-random bytes — effectively incompressible.
    Random,
}

impl Payload {
    pub fn parse(name: &str) -> crate::BenchResult<Payload> {
        match name {
            "repeat" => Ok(Payload::Repeat),
            "random" => Ok(Payload::Random),
            other => Err(format!(
                "unknown --payload {other:?} (expected: repeat | random)"
            )),
        }
    }
}

/// Build a payload of `bytes` characters.
///
/// `Random` draws from a printable alphabet so the value stays a BSON string
/// (matching `Repeat`'s shape exactly — only the entropy differs), seeded so a
/// run is reproducible.
///
/// **Vary `seed` per document.** A single random payload reused across every
/// document is incompressible *within* a document and perfectly compressible
/// *across* them, which is not what "random" is for: WiredTiger compresses
/// blocks holding many records, so 20,000 identical 8 KiB payloads collapsed
/// to an 8 MB table and made a storage measurement meaningless.
pub fn make_payload(kind: Payload, bytes: usize, seed: u64) -> String {
    match kind {
        Payload::Repeat => "x".repeat(bytes),
        Payload::Random => {
            const ALPHABET: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            // xorshift64*: no dependency, and good enough that the bytes do not
            // compress. The point is entropy, not cryptography.
            let mut state = seed | 1;
            let mut out = String::with_capacity(bytes);
            for _ in 0..bytes {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                let v = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
                out.push(ALPHABET[(v >> 33) as usize % ALPHABET.len()] as char);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_random_payload_does_not_compress_like_a_repeated_one() {
        // The whole reason the option exists: a repeated character makes any
        // bytes-on-disk measurement a measurement of the compressor.
        let repeated = make_payload(Payload::Repeat, 4096, 1);
        let random = make_payload(Payload::Random, 4096, 1);
        assert_eq!(repeated.len(), 4096);
        assert_eq!(random.len(), 4096);
        assert_eq!(
            repeated
                .chars()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            1
        );
        // A high distinct-character count is a cheap proxy for entropy.
        assert!(
            random
                .chars()
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 50,
            "random payload looks non-random"
        );
    }

    #[test]
    fn a_random_payload_is_reproducible_for_a_given_seed() {
        assert_eq!(
            make_payload(Payload::Random, 64, 7),
            make_payload(Payload::Random, 64, 7)
        );
        assert_ne!(
            make_payload(Payload::Random, 64, 7),
            make_payload(Payload::Random, 64, 8)
        );
    }

    #[test]
    fn payload_kinds_parse_and_reject_nonsense() {
        assert_eq!(Payload::parse("repeat").unwrap(), Payload::Repeat);
        assert_eq!(Payload::parse("random").unwrap(), Payload::Random);
        assert!(Payload::parse("gzip").is_err());
    }

    #[test]
    fn documents_carry_the_payload_and_counter() {
        let d = make_document(42, "xxxx");
        assert_eq!(d.get_i64("n").unwrap(), 42);
        assert_eq!(d.get_i32("c").unwrap(), 0);
        assert_eq!(d.get_str("payload").unwrap(), "xxxx");
    }

    #[test]
    fn document_size_tracks_the_payload() {
        let mut small = Vec::new();
        make_document(1, &"x".repeat(64))
            .to_writer(&mut small)
            .unwrap();
        let mut large = Vec::new();
        make_document(1, &"x".repeat(8192))
            .to_writer(&mut large)
            .unwrap();
        assert!(large.len() - small.len() >= 8000);
    }
}

#[cfg(test)]
mod payload_entropy_tests {
    use super::*;

    #[test]
    fn different_seeds_give_substantially_different_payloads() {
        // The bug this guards: one payload reused for every document is
        // incompressible WITHIN a document and perfectly compressible ACROSS
        // documents, so a storage measurement reads the compressor instead of
        // the engine. Per-document seeds are what make a random dataset random.
        let a = make_payload(Payload::Random, 512, 1);
        let b = make_payload(Payload::Random, 512, 2);
        let differing = a.chars().zip(b.chars()).filter(|(x, y)| x != y).count();
        assert!(differing > 400, "only {differing}/512 characters differ");
    }

    #[test]
    fn a_repeated_payload_is_identical_regardless_of_seed() {
        // `repeat` is the comparability default; its content must not drift.
        assert_eq!(
            make_payload(Payload::Repeat, 64, 1),
            make_payload(Payload::Repeat, 64, 999)
        );
    }
}
