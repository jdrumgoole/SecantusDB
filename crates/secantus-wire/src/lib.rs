//! `secantus-wire` — the MongoDB wire-protocol framing layer of the Rust server.
//!
//! A faithful, **pure-Rust, PyO3-free** port of `src/secantus/wire.py` (R1 in
//! `tasks/rust-server-plan.md`): the 16-byte little-endian message header,
//! `OP_MSG` (2013) kind-0 body + kind-1 document-sequence parsing, legacy
//! `OP_QUERY` (2004) parsing for the pymongo handshake, and the `OP_MSG` /
//! `OP_REPLY` builders.
//!
//! ## The byte seam
//!
//! Parsing is **framing only**: an [`OpMsg`] / [`OpQuery`] borrows byte slices
//! out of the caller's body buffer (the kind-0 body, each kind-1 document) rather
//! than decoding to owned `bson::Document`s. That keeps the wire layer zero-copy
//! and lets the dispatch layer (R2) decode into whatever representation it wants,
//! once — mirroring the "documents are opaque BSON bytes end-to-end" design.
//! Builders take **already-encoded** BSON bytes for the same reason.
//!
//! ## Error classification (mirrors `wire.py`)
//!
//! `wire.py`'s `read_message` makes a load-bearing distinction the connection
//! loop relies on:
//!
//! * Framing/BSON faults *after* the header is known — a bad inner length, a
//!   doc that fails BSON validation — are **recoverable**: real `mongod` replies
//!   `{ok:0, code:2 BadValue}` and keeps the socket. These are Python's
//!   `_BodyBoundsError` / `bson.InvalidBSON` → `MalformedBodyError`.
//! * Structural faults that make the frame unusable — body too short for the
//!   flags word, an unknown section kind, no kind-0 body — are **fatal**
//!   (Python's bare `WireProtocolError`); the connection loop drops the socket.
//!
//! [`WireError`] carries that split via [`WireError::is_recoverable`]. The header
//! is *not* embedded in the error: the connection loop (R4) reads the header
//! first and pairs it with a recoverable error itself, so this crate never needs
//! to own it.

/// `OP_REPLY` — legacy reply opcode, used only for the `OP_QUERY` handshake.
pub const OP_REPLY: i32 = 1;
/// `OP_UPDATE` — removed legacy write opcode; mongod drops the connection.
pub const OP_UPDATE: i32 = 2001;
/// `OP_INSERT` — removed legacy write opcode; mongod drops the connection.
pub const OP_INSERT: i32 = 2002;
/// `OP_QUERY` — legacy query opcode; pymongo emits exactly one (the handshake).
pub const OP_QUERY: i32 = 2004;
/// `OP_GET_MORE` — removed legacy getMore opcode. mongod refuses it with an
/// `OP_REPLY` and keeps the connection (see [`Op::LegacyGetMore`]).
pub const OP_GET_MORE: i32 = 2005;
/// `OP_DELETE` — removed legacy write opcode; mongod drops the connection.
pub const OP_DELETE: i32 = 2006;
/// `OP_KILL_CURSORS` — removed legacy opcode; mongod drops the connection.
pub const OP_KILL_CURSORS: i32 = 2007;
/// `OP_COMPRESSED` — the compressed-message wrapper (see [`compression`]).
pub const OP_COMPRESSED: i32 = 2012;
/// `OP_MSG` — the modern opcode; everything after the handshake.
pub const OP_MSG: i32 = 2013;

/// `OP_MSG` flag bit: a 4-byte CRC32C checksum trails the sections.
pub const OP_MSG_FLAG_CHECKSUM_PRESENT: u32 = 1 << 0;
/// `OP_MSG` flag bit: more messages follow (no reply expected for this one).
pub const OP_MSG_FLAG_MORE_TO_COME: u32 = 1 << 1;
/// `OP_MSG` flag bit: the client permits exhaust-cursor replies.
pub const OP_MSG_FLAG_EXHAUST_ALLOWED: u32 = 1 << 16;

/// Size of the fixed message header (4 × int32).
pub const HEADER_SIZE: usize = 16;
/// Largest message we accept, matching `mongod`'s `maxMessageSizeBytes`.
pub const MAX_MESSAGE_SIZE: i32 = 48_000_000;
/// Largest single BSON object, matching `mongod`'s `maxBsonObjectSize`.
pub const MAX_BSON_OBJECT_SIZE: i32 = 16 * 1024 * 1024;

/// Minimum BSON document length: a 4-byte length prefix + a 1-byte terminator.
const MIN_BSON_DOC_LEN: i32 = 5;

pub mod compression;
pub use compression::{
    compress_frame, parse_op_compressed, Compressor, OpCompressed,
    PREFIX_LEN as OP_COMPRESSED_PREFIX_LEN,
};

/// A wire-protocol parse failure, split into recoverable and fatal (see the
/// module docs). `Display` text is internal; the connection loop shapes the
/// client-facing `errmsg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Fatal — the frame is unusable; drop the connection (Python's
    /// `WireProtocolError`).
    Protocol(String),
    /// Recoverable — reply `BadValue` and keep the connection (Python's
    /// `MalformedBodyError`, raised from `_BodyBoundsError` / `bson.InvalidBSON`).
    MalformedBody(String),
}

impl WireError {
    /// `true` for a malformed-body fault the server should answer with a
    /// `BadValue` reply (keeping the socket); `false` for a fatal protocol fault.
    pub fn is_recoverable(&self) -> bool {
        matches!(self, WireError::MalformedBody(_))
    }
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Protocol(m) => write!(f, "wire protocol error: {m}"),
            WireError::MalformedBody(m) => write!(f, "malformed message body: {m}"),
        }
    }
}

impl std::error::Error for WireError {}

/// The 16-byte message header: `<message_length, request_id, response_to,
/// op_code>`, all little-endian int32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub message_length: i32,
    pub request_id: i32,
    pub response_to: i32,
    pub op_code: i32,
}

impl Header {
    /// Parse a header from the first [`HEADER_SIZE`] bytes of `buf`.
    pub fn unpack(buf: &[u8]) -> Result<Header, WireError> {
        if buf.len() < HEADER_SIZE {
            return Err(WireError::Protocol(format!(
                "header needs {HEADER_SIZE} bytes, got {}",
                buf.len()
            )));
        }
        Ok(Header {
            message_length: rd_i32(buf, 0),
            request_id: rd_i32(buf, 4),
            response_to: rd_i32(buf, 8),
            op_code: rd_i32(buf, 12),
        })
    }

    /// Serialise to the 16 header bytes.
    pub fn pack(&self) -> [u8; HEADER_SIZE] {
        let mut out = [0u8; HEADER_SIZE];
        out[0..4].copy_from_slice(&self.message_length.to_le_bytes());
        out[4..8].copy_from_slice(&self.request_id.to_le_bytes());
        out[8..12].copy_from_slice(&self.response_to.to_le_bytes());
        out[12..16].copy_from_slice(&self.op_code.to_le_bytes());
        out
    }

    /// Validate `message_length` against the header floor and [`MAX_MESSAGE_SIZE`]
    /// — the checks `wire.py::read_message` runs before reading the body. Returns
    /// the body length (`message_length - HEADER_SIZE`) on success. Both bounds
    /// are fatal (Python raises a bare `WireProtocolError`).
    pub fn body_len(&self) -> Result<usize, WireError> {
        if (self.message_length as usize) < HEADER_SIZE {
            return Err(WireError::Protocol(format!(
                "message length {} < header size {HEADER_SIZE}",
                self.message_length
            )));
        }
        if self.message_length > MAX_MESSAGE_SIZE {
            return Err(WireError::Protocol(format!(
                "message length {} exceeds max {MAX_MESSAGE_SIZE}",
                self.message_length
            )));
        }
        Ok(self.message_length as usize - HEADER_SIZE)
    }
}

/// One `OP_MSG` kind-1 document sequence: an identifier (e.g. `"documents"`,
/// `"updates"`) and the BSON documents under it, each a borrowed slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSequence<'a> {
    pub identifier: &'a str,
    pub documents: Vec<&'a [u8]>,
}

/// A parsed `OP_MSG`: the flags, the single kind-0 body (borrowed BSON bytes),
/// and any kind-1 document sequences. The server merges the sequences into the
/// body before dispatch, exactly as `OP_MSG` kind-1 sections are defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpMsg<'a> {
    pub flags: u32,
    pub body: &'a [u8],
    pub document_sequences: Vec<DocumentSequence<'a>>,
}

/// A parsed legacy `OP_QUERY` (the pymongo handshake): the collection name and
/// the query/selector documents as borrowed BSON bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpQuery<'a> {
    pub flags: u32,
    pub full_collection_name: &'a str,
    pub number_to_skip: i32,
    pub number_to_return: i32,
    pub query: &'a [u8],
    pub return_fields_selector: Option<&'a [u8]>,
}

/// The supported request shapes, mirroring `wire.py::Message.op`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op<'a> {
    Msg(OpMsg<'a>),
    Query(OpQuery<'a>),
    /// A legacy `OP_GET_MORE` with a readable namespace. mongod 8.2.11 answers
    /// it with an `OP_REPLY` refusing the opcode (code 5739101) and keeps the
    /// connection; nothing after the namespace is read.
    LegacyGetMore,
    /// A frame with an opcode the server does not serve (an unknown number,
    /// `OP_REPLY` sent as a request, the removed `OP_COMMAND`). mongod 8.2.11
    /// sends nothing back and keeps the connection.
    Ignored(i32),
}

/// Parse a message body by `op_code` (the caller has already read the header
/// and the body bytes). What each opcode gets was measured against mongod
/// 8.2.11 on a raw socket:
///
/// * `OP_MSG` / `OP_QUERY` are parsed.
/// * `OP_GET_MORE` is [`Op::LegacyGetMore`] when it has its flags word and a
///   NUL-terminated namespace; without them the frame is unusable (fatal).
/// * `OP_INSERT` / `OP_UPDATE` / `OP_DELETE` / `OP_KILL_CURSORS` are fatal:
///   mongod drops the connection. So is an `OP_COMPRESSED` inside another.
/// * Every other opcode is [`Op::Ignored`] -- unless the body is shorter than
///   4 bytes, which mongod treats as an unusable frame (fatal).
pub fn parse_body(op_code: i32, body: &[u8]) -> Result<Op<'_>, WireError> {
    match op_code {
        OP_MSG => Ok(Op::Msg(parse_op_msg(body)?)),
        OP_QUERY => Ok(Op::Query(parse_op_query(body)?)),
        OP_GET_MORE => {
            if body.len() < 4 || memchr0(body, 4, body.len()).is_none() {
                return Err(WireError::Protocol(
                    "OP_GET_MORE has no NUL-terminated namespace".into(),
                ));
            }
            Ok(Op::LegacyGetMore)
        }
        OP_UPDATE | OP_INSERT | OP_DELETE | OP_KILL_CURSORS | OP_COMPRESSED => Err(
            WireError::Protocol(format!("unsupported op_code {op_code}")),
        ),
        other if body.len() < 4 => Err(WireError::Protocol(format!(
            "op_code {other} with a {}-byte body",
            body.len()
        ))),
        other => Ok(Op::Ignored(other)),
    }
}

/// The `$err` text mongod 8.2.11 answers a legacy `OP_GET_MORE` with.
pub const OP_GET_MORE_REMOVED_ERRMSG: &str = "OP_GET_MORE is no longer supported. The client \
driver may require an upgrade. For more details see \
https://dochub.mongodb.org/core/legacy-opcode-removal";
/// The error code that accompanies [`OP_GET_MORE_REMOVED_ERRMSG`].
pub const OP_GET_MORE_REMOVED_CODE: i32 = 5739101;
/// `OP_REPLY` response flag: the query failed (`QueryFailure`).
pub const OP_REPLY_FLAG_QUERY_FAILURE: u32 = 1 << 1;

/// The `OP_MSG` flag bits a receiver must understand: the low 16. mongod
/// refuses a message with any it does not know, where the high 16 are
/// optional and ignored.
const OP_MSG_REQUIRED_FLAG_BITS: u32 = 0xFFFF;

/// Why mongod would drop the connection over this `OP_MSG` frame before
/// reading it: a required flag bit it does not know, or a CRC-32C checksum
/// that does not match the message. `None` for a sound frame. Measured
/// 8.2.11, 2026-10-20: both closed the connection, where this server
/// answered as though the frame were sound (the checksum was only trimmed).
pub fn op_msg_frame_problem(header: &Header, body: &[u8]) -> Option<&'static str> {
    if body.len() < 4 {
        return None; // the parser's own refusal
    }
    let flags = rd_u32(body, 0);
    let known = OP_MSG_FLAG_CHECKSUM_PRESENT | OP_MSG_FLAG_MORE_TO_COME;
    if flags & OP_MSG_REQUIRED_FLAG_BITS & !known != 0 {
        return Some("OP_MSG has an unknown required flag bit");
    }
    if flags & OP_MSG_FLAG_CHECKSUM_PRESENT != 0 {
        if body.len() < 8 {
            return Some("OP_MSG too short for its checksum");
        }
        let (covered, trailer) = body.split_at(body.len() - 4);
        // The checksum covers the whole message, header included.
        let mut head = Vec::with_capacity(HEADER_SIZE);
        head.extend_from_slice(&((HEADER_SIZE + body.len()) as i32).to_le_bytes());
        head.extend_from_slice(&header.request_id.to_le_bytes());
        head.extend_from_slice(&header.response_to.to_le_bytes());
        head.extend_from_slice(&header.op_code.to_le_bytes());
        let sum = crc32c(crc32c_update(!0, &head), covered);
        if sum != rd_u32(trailer, 0) {
            return Some("OP_MSG checksum does not match");
        }
    }
    None
}

/// The same `OP_MSG` frame with the checksum flag set and its CRC-32C
/// appended: mongod's reply to a request that carried a checksum carries
/// one too (measured 8.2.11, 2026-10-20). A frame that is not an `OP_MSG`
/// comes back unchanged.
pub fn with_op_msg_checksum(frame: &[u8]) -> Vec<u8> {
    if frame.len() < HEADER_SIZE + 4 || rd_i32(frame, 12) != OP_MSG {
        return frame.to_vec();
    }
    let mut out = Vec::with_capacity(frame.len() + 4);
    out.extend_from_slice(frame);
    out[..4].copy_from_slice(&((frame.len() + 4) as i32).to_le_bytes());
    let flags = rd_u32(&out, HEADER_SIZE) | OP_MSG_FLAG_CHECKSUM_PRESENT;
    out[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&flags.to_le_bytes());
    let sum = crc32c(!0, &out);
    out.extend_from_slice(&sum.to_le_bytes());
    out
}

/// CRC-32C (Castagnoli), the checksum `OP_MSG` carries.
fn crc32c(state: u32, data: &[u8]) -> u32 {
    !crc32c_update(state, data)
}

fn crc32c_update(mut state: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0x82F6_3B78 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    for b in data {
        state = table[((state ^ u32::from(*b)) & 0xFF) as usize] ^ (state >> 8);
    }
    state
}

/// Parse an `OP_MSG` body (everything after the header): a u32 flags word
/// followed by kind-0 / kind-1 sections, optionally trailed by a CRC32C when
/// [`OP_MSG_FLAG_CHECKSUM_PRESENT`] is set. Port of `wire.py::_parse_op_msg`.
pub fn parse_op_msg(buf: &[u8]) -> Result<OpMsg<'_>, WireError> {
    if buf.len() < 4 {
        return Err(WireError::Protocol(
            "OP_MSG body too short for flags".into(),
        ));
    }
    let flags = rd_u32(buf, 0);
    let has_checksum = flags & OP_MSG_FLAG_CHECKSUM_PRESENT != 0;
    // The trailing 4-byte checksum, if present, is not part of the sections.
    let end = if has_checksum {
        buf.len() - 4
    } else {
        buf.len()
    };

    let mut offset = 4usize;
    let mut body: Option<&[u8]> = None;
    let mut sequences: Vec<DocumentSequence<'_>> = Vec::new();

    while offset < end {
        let kind = buf[offset];
        offset += 1;
        match kind {
            0 => {
                if offset + 4 > end {
                    return Err(WireError::MalformedBody(
                        "OP_MSG kind-0 truncated before length".into(),
                    ));
                }
                let doc_len = rd_i32(buf, offset);
                check_doc_len(doc_len, offset, end, "OP_MSG kind-0")?;
                if body.is_some() {
                    return Err(WireError::MalformedBody(
                        "OP_MSG has more than one kind-0 section".into(),
                    ));
                }
                let doc = &buf[offset..offset + doc_len as usize];
                validate_bson(doc, "OP_MSG kind-0")?;
                body = Some(doc);
                offset += doc_len as usize;
            }
            1 => {
                if offset + 4 > end {
                    return Err(WireError::MalformedBody(
                        "OP_MSG kind-1 truncated before length".into(),
                    ));
                }
                // section_len counts the 4 length bytes themselves.
                let section_len = rd_i32(buf, offset);
                if section_len < 4 || offset + section_len as usize > end {
                    return Err(WireError::MalformedBody(format!(
                        "OP_MSG kind-1: declared section length {section_len} invalid"
                    )));
                }
                let section_end = offset + section_len as usize;
                offset += 4;
                let ident_end = memchr0(buf, offset, section_end).ok_or_else(|| {
                    WireError::MalformedBody(
                        "OP_MSG kind-1: identifier missing null terminator".into(),
                    )
                })?;
                let identifier = std::str::from_utf8(&buf[offset..ident_end]).map_err(|_| {
                    WireError::MalformedBody("OP_MSG kind-1: identifier is not valid UTF-8".into())
                })?;
                offset = ident_end + 1;
                let mut docs: Vec<&[u8]> = Vec::new();
                while offset < section_end {
                    if offset + 4 > section_end {
                        return Err(WireError::MalformedBody(
                            "OP_MSG kind-1: truncated inner doc length".into(),
                        ));
                    }
                    let doc_len = rd_i32(buf, offset);
                    check_doc_len(doc_len, offset, section_end, "OP_MSG kind-1 inner")?;
                    let doc = &buf[offset..offset + doc_len as usize];
                    validate_bson(doc, "OP_MSG kind-1 inner")?;
                    docs.push(doc);
                    offset += doc_len as usize;
                }
                sequences.push(DocumentSequence {
                    identifier,
                    documents: docs,
                });
            }
            other => {
                return Err(WireError::Protocol(format!(
                    "unknown OP_MSG section kind {other}"
                )));
            }
        }
    }

    match body {
        Some(body) => Ok(OpMsg {
            flags,
            body,
            document_sequences: sequences,
        }),
        None => Err(WireError::Protocol(
            "OP_MSG has no kind-0 body section".into(),
        )),
    }
}

/// Parse a legacy `OP_QUERY` body. Port of `wire.py::_parse_op_query`, hardened
/// with the same bounds discipline as the `OP_MSG` path (the Python version
/// predates the bounds checks; out-of-range lengths here surface as recoverable
/// `MalformedBody` rather than crashing the connection thread).
pub fn parse_op_query(buf: &[u8]) -> Result<OpQuery<'_>, WireError> {
    if buf.len() < 4 {
        return Err(WireError::Protocol("OP_QUERY body too short".into()));
    }
    let flags = rd_u32(buf, 0);
    let mut offset = 4usize;

    let name_end = memchr0(buf, offset, buf.len()).ok_or_else(|| {
        WireError::MalformedBody("OP_QUERY: collection name missing null terminator".into())
    })?;
    let full_collection_name = std::str::from_utf8(&buf[offset..name_end]).map_err(|_| {
        WireError::MalformedBody("OP_QUERY: collection name is not valid UTF-8".into())
    })?;
    offset = name_end + 1;

    if offset + 8 > buf.len() {
        return Err(WireError::MalformedBody(
            "OP_QUERY: truncated before skip/return".into(),
        ));
    }
    let number_to_skip = rd_i32(buf, offset);
    offset += 4;
    let number_to_return = rd_i32(buf, offset);
    offset += 4;

    if offset + 4 > buf.len() {
        return Err(WireError::MalformedBody(
            "OP_QUERY: truncated before query document".into(),
        ));
    }
    let query_len = rd_i32(buf, offset);
    check_doc_len(query_len, offset, buf.len(), "OP_QUERY query")?;
    let query = &buf[offset..offset + query_len as usize];
    validate_bson(query, "OP_QUERY query")?;
    offset += query_len as usize;

    let return_fields_selector = if offset < buf.len() {
        if offset + 4 > buf.len() {
            return Err(WireError::MalformedBody(
                "OP_QUERY: truncated before field selector".into(),
            ));
        }
        let sel_len = rd_i32(buf, offset);
        check_doc_len(sel_len, offset, buf.len(), "OP_QUERY selector")?;
        let sel = &buf[offset..offset + sel_len as usize];
        validate_bson(sel, "OP_QUERY selector")?;
        Some(sel)
    } else {
        None
    };

    Ok(OpQuery {
        flags,
        full_collection_name,
        number_to_skip,
        number_to_return,
        query,
        return_fields_selector,
    })
}

/// Build an `OP_MSG` reply frame: header + u32 flags + a single kind-0 section
/// wrapping `body_bytes` (already BSON-encoded). Port of
/// `wire.py::build_op_msg_reply`.
pub fn build_op_msg_reply(
    response_to: i32,
    request_id: i32,
    body_bytes: &[u8],
    flags: u32,
) -> Vec<u8> {
    // payload = flags(4) + kind(1) + body
    let payload_len = 4 + 1 + body_bytes.len();
    let message_length = (HEADER_SIZE + payload_len) as i32;
    let header = Header {
        message_length,
        request_id,
        response_to,
        op_code: OP_MSG,
    };
    let mut out = Vec::with_capacity(HEADER_SIZE + payload_len);
    out.extend_from_slice(&header.pack());
    out.extend_from_slice(&flags.to_le_bytes());
    out.push(0u8); // kind-0 section marker
    out.extend_from_slice(body_bytes);
    out
}

/// Serialize a cursor-command reply *body* (the kind-0 BSON document that
/// [`build_op_msg_reply`] then frames), splicing the pre-encoded document blobs
/// into `cursor.<batch_field>` as a BSON array **without decoding them**.
///
/// `envelope` is the reply document the handler produced *minus* the batch —
/// `{ cursor: { id, ns }, ok: 1.0, <gossip…> }`. Each element of `batch` is an
/// already-encoded BSON document (a stored blob or a cursor-registry entry).
/// The output is byte-identical to building
/// `{ cursor: { <batch_field>: [<docs>], id, ns }, ok, … }` as an owned
/// `Document` and serializing it — but it skips the decode→`IndexMap`→re-encode
/// round-trip that `docs_to_bson` + `Document::to_writer` otherwise pay for
/// every served document (the reply-path hot spot in
/// `tasks/rust-perf-findings.md`). The blobs are memcpy'd in as array elements.
///
/// The batch field is emitted first inside `cursor` so the layout matches the
/// old `doc!{ "cursor": { "firstBatch"/"nextBatch": …, "id": …, "ns": … } }`
/// ordering exactly (asserted byte-for-byte by the unit test).
///
/// `envelope` is that document already encoded. Every element is copied
/// through byte for byte, never decoded, so a string the envelope holds whose
/// bytes are not UTF-8 (a namespace mongod accepted) reaches the client as it
/// is (RM-222).
pub fn encode_cursor_reply(
    envelope: &[u8],
    batch_field: &str,
    batch: &[Vec<u8>],
) -> Result<Vec<u8>, WireError> {
    let internal = |m: &str| WireError::Protocol(format!("encode_cursor_reply: {m}"));
    let elements = raw_elements(envelope).ok_or_else(|| internal("malformed envelope"))?;
    let mut out = vec![0u8; 4];
    let mut have_cursor = false;
    for (t, key, value) in elements {
        out.push(t);
        out.extend_from_slice(key);
        out.push(0);
        if key != b"cursor" {
            out.extend_from_slice(value);
            continue;
        }
        if t != 0x03 {
            return Err(internal("reply `cursor` field is not a document"));
        }
        have_cursor = true;
        let cursor_start = out.len();
        out.extend_from_slice(&[0; 4]);
        // Batch first, then the handler's `id` / `ns` in their order.
        out.push(0x04);
        out.extend_from_slice(batch_field.as_bytes());
        out.push(0);
        let array_start = out.len();
        out.extend_from_slice(&[0; 4]);
        for (i, blob) in batch.iter().enumerate() {
            let len = blob
                .get(..4)
                .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n >= 5 && *n <= blob.len())
                .ok_or_else(|| internal("malformed batch document"))?;
            out.push(0x03);
            out.extend_from_slice(i.to_string().as_bytes());
            out.push(0);
            out.extend_from_slice(&blob[..len]);
        }
        out.push(0);
        patch_len(&mut out, array_start);
        // The cursor document's own elements, without its length and NUL.
        out.extend_from_slice(&value[4..value.len() - 1]);
        out.push(0);
        patch_len(&mut out, cursor_start);
    }
    if !have_cursor {
        return Err(internal("reply has no `cursor` field"));
    }
    out.push(0);
    patch_len(&mut out, 0);
    Ok(out)
}

fn patch_len(out: &mut [u8], start: usize) {
    let len = (out.len() - start) as i32;
    out[start..start + 4].copy_from_slice(&len.to_le_bytes());
}

/// One element of an encoded document: its type, key and value bytes.
type RawElement<'a> = (u8, &'a [u8], &'a [u8]);

/// The top-level elements of one encoded document, or `None` when it is
/// malformed.
fn raw_elements(doc: &[u8]) -> Option<Vec<RawElement<'_>>> {
    let len = usize::try_from(rd_i32_checked(doc, 0)?).ok()?;
    if len < 5 || len > doc.len() || doc[len - 1] != 0 {
        return None;
    }
    let end = len - 1;
    let mut out = Vec::new();
    let mut i = 4;
    while i < end {
        let t = doc[i];
        i += 1;
        let k_end = i + doc[i..end].iter().position(|c| *c == 0)?;
        let key = &doc[i..k_end];
        i = k_end + 1;
        let size = value_size(t, &doc[i..end])?;
        out.push((t, key, doc.get(i..i + size)?));
        i += size;
    }
    (i == end).then_some(out)
}

fn rd_i32_checked(b: &[u8], at: usize) -> Option<i32> {
    b.get(at..at + 4)
        .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// The size of a BSON value of type `t` at the start of `rest`.
fn value_size(t: u8, rest: &[u8]) -> Option<usize> {
    let len_at = |at: usize, min: i32| -> Option<usize> {
        let n = rd_i32_checked(rest, at)?;
        (n >= min).then_some(n as usize)
    };
    let size = match t {
        0x01 | 0x09 | 0x11 | 0x12 => 8,
        0x02 | 0x0D | 0x0E => 4 + len_at(0, 1)?,
        0x03 | 0x04 | 0x0F => len_at(0, 5)?,
        0x05 => 5 + len_at(0, 0)?,
        0x06 | 0x0A | 0x7F | 0xFF => 0,
        0x07 => 12,
        0x08 => 1,
        0x0B => {
            let p = rest.iter().position(|c| *c == 0)?;
            p + 1 + rest[p + 1..].iter().position(|c| *c == 0)? + 1
        }
        0x0C => 4 + len_at(0, 1)? + 12,
        0x10 => 4,
        0x13 => 16,
        _ => return None,
    };
    (size <= rest.len()).then_some(size)
}

/// Build a legacy `OP_REPLY` frame for the `OP_QUERY` handshake. Port of
/// `wire.py::build_op_reply`; `documents` are already BSON-encoded.
pub fn build_op_reply(
    response_to: i32,
    request_id: i32,
    documents: &[&[u8]],
    cursor_id: i64,
    starting_from: i32,
    response_flags: u32,
) -> Vec<u8> {
    let docs_len: usize = documents.iter().map(|d| d.len()).sum();
    // payload = response_flags(4) + cursor_id(8) + starting_from(4) + count(4) + docs
    let payload_len = 4 + 8 + 4 + 4 + docs_len;
    let message_length = (HEADER_SIZE + payload_len) as i32;
    let header = Header {
        message_length,
        request_id,
        response_to,
        op_code: OP_REPLY,
    };
    let mut out = Vec::with_capacity(HEADER_SIZE + payload_len);
    out.extend_from_slice(&header.pack());
    out.extend_from_slice(&response_flags.to_le_bytes());
    out.extend_from_slice(&cursor_id.to_le_bytes());
    out.extend_from_slice(&starting_from.to_le_bytes());
    out.extend_from_slice(&(documents.len() as i32).to_le_bytes());
    for doc in documents {
        out.extend_from_slice(doc);
    }
    out
}

/// Validate a declared BSON length field before slicing, mirroring
/// `wire.py::_check_doc_len`. `at` is the position of the length field itself; a
/// negative length trips the floor check (it is `< MIN_BSON_DOC_LEN`), so the
/// later `as usize` cast is always on a non-negative value.
fn check_doc_len(doc_len: i32, at: usize, end: usize, where_: &str) -> Result<(), WireError> {
    if doc_len < MIN_BSON_DOC_LEN {
        return Err(WireError::MalformedBody(format!(
            "{where_}: declared BSON length {doc_len} below minimum {MIN_BSON_DOC_LEN}"
        )));
    }
    if at + doc_len as usize > end {
        return Err(WireError::MalformedBody(format!(
            "{where_}: declared BSON length {doc_len} would read past end (at={at}, end={end})"
        )));
    }
    Ok(())
}

/// Validate a BSON document's declared length against its slice — a quick
/// structural sanity check without a full decode. The caller (`merge_op_msg_body`
/// in the server) performs the authoritative `Document::from_reader` decode that
/// catches deeper BSON encoding faults; doing a full decode here too was a
/// redundant double-parse (the earlier `validate_bson` decoded the doc and
/// discarded the result). Keeping the length check at the wire layer means
/// obviously-truncated docs are caught early (recoverable `BadValue`).
fn validate_bson(slice: &[u8], where_: &str) -> Result<(), WireError> {
    if slice.len() < 5 {
        return Err(WireError::MalformedBody(format!(
            "{where_}: BSON too short ({} bytes)",
            slice.len()
        )));
    }
    let declared = i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]);
    if declared as usize != slice.len() {
        return Err(WireError::MalformedBody(format!(
            "{where_}: BSON declared length {declared} != slice length {}",
            slice.len()
        )));
    }
    Ok(())
}

/// Find the next `\x00` in `buf[start..limit]`, returning its absolute index.
fn memchr0(buf: &[u8], start: usize, limit: usize) -> Option<usize> {
    buf[start..limit]
        .iter()
        .position(|&b| b == 0)
        .map(|i| start + i)
}

#[inline]
fn rd_i32(buf: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

#[inline]
fn rd_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn enc(d: &bson::Document) -> Vec<u8> {
        let mut v = Vec::new();
        d.to_writer(&mut v).unwrap();
        v
    }

    /// The whole point of `encode_cursor_reply`: its spliced output must be
    /// byte-for-byte identical to building the same reply as an owned
    /// `Document` (with the batch decoded into a `Bson::Array`) and serializing
    /// it. If this holds, no driver can tell the fast path from the old one.
    fn assert_splice_matches(envelope: bson::Document, batch_field: &str, docs: &[bson::Document]) {
        let blobs: Vec<Vec<u8>> = docs.iter().map(enc).collect();

        // The old path: decode the blobs into `Bson::Document`s, insert the
        // array into `cursor.<batch_field>` (first, as the handlers did), and
        // serialize the whole owned document.
        let mut owned = envelope.clone();
        let cursor = owned.get_document_mut("cursor").unwrap();
        let arr: Vec<bson::Bson> = docs.iter().cloned().map(bson::Bson::Document).collect();
        // Rebuild `cursor` with the batch field first to match the splice order.
        let mut rebuilt = bson::Document::new();
        rebuilt.insert(batch_field, arr);
        for (k, v) in cursor.iter() {
            rebuilt.insert(k, v.clone());
        }
        *cursor = rebuilt;
        let expected = enc(&owned);

        let got = encode_cursor_reply(&enc(&envelope), batch_field, &blobs).unwrap();
        assert_eq!(
            got, expected,
            "spliced reply body must byte-match the owned reply"
        );
    }

    /// RM-222: a batch document and an envelope string whose bytes are not
    /// UTF-8 are copied through as they are.
    #[test]
    fn encode_cursor_reply_copies_strings_that_are_not_utf8() {
        // {_id: 1, s: <ff fe>}
        let blob: Vec<u8> = [
            &24i32.to_le_bytes()[..],
            &[0x10, b'_', b'i', b'd', 0, 1, 0, 0, 0],
            &[0x02, b's', 0, 3, 0, 0, 0, 0xff, 0xfe, 0],
            &[0],
        ]
        .concat();
        assert_eq!(blob.len(), 24);
        // {cursor: {id: 0L, ns: "d.<ff>"}, ok: 1.0}
        let cursor: Vec<u8> = [
            &[0x12, b'i', b'd', 0][..],
            &0i64.to_le_bytes(),
            &[0x02, b'n', b's', 0, 4, 0, 0, 0, b'd', b'.', 0xff, 0],
        ]
        .concat();
        let mut cursor_doc = ((cursor.len() + 5) as i32).to_le_bytes().to_vec();
        cursor_doc.extend(&cursor);
        cursor_doc.push(0);
        let mut env_elems = vec![0x03];
        env_elems.extend_from_slice(b"cursor\0");
        env_elems.extend(&cursor_doc);
        env_elems.push(0x01);
        env_elems.extend_from_slice(b"ok\0");
        env_elems.extend(1.0f64.to_le_bytes());
        let mut envelope = ((env_elems.len() + 5) as i32).to_le_bytes().to_vec();
        envelope.extend(&env_elems);
        envelope.push(0);

        let got =
            encode_cursor_reply(&envelope, "firstBatch", std::slice::from_ref(&blob)).unwrap();
        let find = |needle: &[u8]| got.windows(needle.len()).any(|w| w == needle);
        assert!(find(&blob), "the batch document is copied whole");
        assert!(find(b"d.\xff\0"), "the namespace keeps its bytes");
        assert_eq!(
            i32::from_le_bytes([got[0], got[1], got[2], got[3]]) as usize,
            got.len()
        );
    }

    #[test]
    fn encode_cursor_reply_byte_matches_owned() {
        // find firstBatch, multiple docs, with gossip fields after `cursor`.
        assert_splice_matches(
            doc! {
                "cursor": { "id": 0i64, "ns": "db.coll" },
                "ok": 1.0,
                "$clusterTime": { "clusterTime": bson::Timestamp { time: 5, increment: 1 } },
                "operationTime": bson::Timestamp { time: 5, increment: 1 },
            },
            "firstBatch",
            &[
                doc! { "_id": 1, "x": "a" },
                doc! { "_id": 2, "x": "b", "nested": { "y": [1, 2, 3] } },
            ],
        );

        // getMore nextBatch with a live cursor id.
        assert_splice_matches(
            doc! { "cursor": { "id": 987654321i64, "ns": "db.coll" }, "ok": 1.0 },
            "nextBatch",
            &[doc! { "_id": 3 }],
        );

        // Empty batch (batchSize:0 / drained cursor) — array must still frame.
        assert_splice_matches(
            doc! { "cursor": { "id": 0i64, "ns": "db.coll" }, "ok": 1.0 },
            "firstBatch",
            &[],
        );
    }

    #[test]
    fn header_roundtrips() {
        let h = Header {
            message_length: 123,
            request_id: 42,
            response_to: 7,
            op_code: OP_MSG,
        };
        let packed = h.pack();
        assert_eq!(packed.len(), HEADER_SIZE);
        assert_eq!(Header::unpack(&packed).unwrap(), h);
    }

    #[test]
    fn header_too_short_is_fatal() {
        let err = Header::unpack(&[0u8; 8]).unwrap_err();
        assert!(!err.is_recoverable());
    }

    #[test]
    fn body_len_bounds() {
        let short = Header {
            message_length: 4,
            request_id: 0,
            response_to: 0,
            op_code: OP_MSG,
        };
        assert!(short.body_len().is_err());
        let huge = Header {
            message_length: MAX_MESSAGE_SIZE + 1,
            request_id: 0,
            response_to: 0,
            op_code: OP_MSG,
        };
        assert!(huge.body_len().is_err());
        let ok = Header {
            message_length: 100,
            request_id: 0,
            response_to: 0,
            op_code: OP_MSG,
        };
        assert_eq!(ok.body_len().unwrap(), 100 - HEADER_SIZE);
    }

    /// Wrap a body document in a flags word + kind-0 section, like the pymongo
    /// command path and `tests/test_wire_malformed.py`.
    fn op_msg_body(body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&0u32.to_le_bytes()); // flags
        v.push(0u8); // kind 0
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn parse_kind0_only() {
        let body = enc(&doc! {"ping": 1, "$db": "admin"});
        let frame = op_msg_body(&body);
        let msg = parse_op_msg(&frame).unwrap();
        assert_eq!(msg.flags, 0);
        assert_eq!(msg.body, body.as_slice());
        assert!(msg.document_sequences.is_empty());
        // The borrowed body re-decodes to the original document.
        assert_eq!(
            bson::Document::try_from(bson::RawDocument::from_bytes(msg.body).unwrap()).unwrap(),
            doc! {"ping": 1, "$db": "admin"}
        );
    }

    #[test]
    fn parse_kind0_plus_kind1_sequence() {
        let body = enc(&doc! {"insert": "c", "$db": "t"});
        let d0 = enc(&doc! {"_id": 1});
        let d1 = enc(&doc! {"_id": 2});

        let mut frame = Vec::new();
        frame.extend_from_slice(&0u32.to_le_bytes()); // flags
        frame.push(0u8); // kind 0
        frame.extend_from_slice(&body);

        // kind-1: section_len + identifier + docs
        let ident = b"documents\x00";
        let section_len = (4 + ident.len() + d0.len() + d1.len()) as i32;
        frame.push(1u8); // kind 1
        frame.extend_from_slice(&section_len.to_le_bytes());
        frame.extend_from_slice(ident);
        frame.extend_from_slice(&d0);
        frame.extend_from_slice(&d1);

        let msg = parse_op_msg(&frame).unwrap();
        assert_eq!(msg.body, body.as_slice());
        assert_eq!(msg.document_sequences.len(), 1);
        let seq = &msg.document_sequences[0];
        assert_eq!(seq.identifier, "documents");
        assert_eq!(seq.documents, vec![d0.as_slice(), d1.as_slice()]);
    }

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        // RFC 3720's check value for "123456789".
        assert_eq!(crc32c(!0, b"123456789"), 0xE306_9283);
    }

    #[test]
    fn op_msg_frame_problem_checks_flags_and_checksum() {
        let mut body = Vec::new();
        body.extend_from_slice(&OP_MSG_FLAG_CHECKSUM_PRESENT.to_le_bytes());
        body.push(0);
        body.extend_from_slice(&enc(&doc! {"ping": 1, "$db": "admin"}));
        let header = Header {
            message_length: (HEADER_SIZE + body.len() + 4) as i32,
            request_id: 7,
            response_to: 0,
            op_code: OP_MSG,
        };
        let mut head = Vec::new();
        head.extend_from_slice(&header.message_length.to_le_bytes());
        head.extend_from_slice(&7i32.to_le_bytes());
        head.extend_from_slice(&0i32.to_le_bytes());
        head.extend_from_slice(&OP_MSG.to_le_bytes());
        let sum = crc32c(crc32c_update(!0, &head), &body);
        let mut good = body.clone();
        good.extend_from_slice(&sum.to_le_bytes());
        assert_eq!(op_msg_frame_problem(&header, &good), None);
        let mut bad = body.clone();
        bad.extend_from_slice(&(sum ^ 1).to_le_bytes());
        assert!(op_msg_frame_problem(&header, &bad).is_some());
        let mut flagged = good.clone();
        flagged[0] |= 1 << 3;
        assert!(op_msg_frame_problem(&header, &flagged).is_some());
        // An optional (high) bit is ignored.
        let mut optional = good;
        optional[3] |= 1 << 5;
        let sum2 = {
            let covered = &optional[..optional.len() - 4];
            crc32c(crc32c_update(!0, &head), covered)
        };
        let n = optional.len();
        optional[n - 4..].copy_from_slice(&sum2.to_le_bytes());
        assert_eq!(op_msg_frame_problem(&header, &optional), None);
    }

    #[test]
    fn checksum_flag_trims_trailing_four_bytes() {
        let body = enc(&doc! {"ping": 1});
        let mut frame = Vec::new();
        frame.extend_from_slice(&OP_MSG_FLAG_CHECKSUM_PRESENT.to_le_bytes());
        frame.push(0u8);
        frame.extend_from_slice(&body);
        frame.extend_from_slice(&0xDEADBEEFu32.to_le_bytes()); // fake CRC
        let msg = parse_op_msg(&frame).unwrap();
        assert_eq!(msg.body, body.as_slice());
    }

    #[test]
    fn duplicate_kind0_is_recoverable() {
        let body = enc(&doc! {"ping": 1});
        let mut frame = op_msg_body(&body);
        frame.push(0u8);
        frame.extend_from_slice(&body);
        let err = parse_op_msg(&frame).unwrap_err();
        assert!(err.is_recoverable());
    }

    #[test]
    fn unknown_section_kind_is_fatal() {
        let body = enc(&doc! {"ping": 1});
        let mut frame = op_msg_body(&body);
        frame.push(2u8); // bogus kind
        let err = parse_op_msg(&frame).unwrap_err();
        assert!(!err.is_recoverable());
    }

    #[test]
    fn no_kind0_body_is_fatal() {
        let frame = 0u32.to_le_bytes().to_vec(); // flags only, no sections
        let err = parse_op_msg(&frame).unwrap_err();
        assert!(!err.is_recoverable());
    }

    #[test]
    fn body_too_short_for_flags_is_fatal() {
        let err = parse_op_msg(&[0u8, 0u8]).unwrap_err();
        assert!(!err.is_recoverable());
    }

    /// The exact malformed body from `tests/test_wire_malformed.py`: a doc that
    /// declares 30 bytes but supplies fewer, tripping the bounds check.
    #[test]
    fn malformed_declared_length_is_recoverable() {
        let mut doc_bytes = Vec::new();
        doc_bytes.extend_from_slice(&30i32.to_le_bytes());
        doc_bytes.extend_from_slice(b"\x05a\x00\x05\x00\x00\x00\x00garbage");
        doc_bytes.push(0u8);
        let frame = op_msg_body(&doc_bytes);
        let err = parse_op_msg(&frame).unwrap_err();
        assert!(err.is_recoverable(), "got {err:?}");
    }

    /// A doc whose declared length fits the buffer but whose contents are not
    /// valid BSON passes the wire layer's structural check (length-only). The
    /// full BSON decode happens in the server's `merge_op_msg_body`, which
    /// surfaces the error as a recoverable `BadValue` reply. The wire layer
    /// intentionally avoids a full decode to eliminate the double-parse overhead.
    #[test]
    fn invalid_bson_content_passes_wire_layer() {
        // length=8 (matches the slice, so framing is fine), but element type code
        // 0x14 is not a valid BSON type. The wire layer accepts it; the server
        // layer's `merge_op_msg_body` will reject it.
        let doc_bytes = [8u8, 0, 0, 0, 0x14, b'x', 0x00, 0x00];
        let frame = op_msg_body(&doc_bytes);
        let msg = parse_op_msg(&frame).unwrap();
        assert_eq!(msg.body, doc_bytes.as_slice());
    }

    #[test]
    fn parse_op_query_handshake() {
        let query = enc(&doc! {"isMaster": 1});
        let mut frame = Vec::new();
        frame.extend_from_slice(&0u32.to_le_bytes()); // flags
        frame.extend_from_slice(b"admin.$cmd\x00"); // full collection name
        frame.extend_from_slice(&0i32.to_le_bytes()); // numberToSkip
        frame.extend_from_slice(&(-1i32).to_le_bytes()); // numberToReturn
        frame.extend_from_slice(&query);
        let q = parse_op_query(&frame).unwrap();
        assert_eq!(q.full_collection_name, "admin.$cmd");
        assert_eq!(q.number_to_return, -1);
        assert_eq!(q.query, query.as_slice());
        assert!(q.return_fields_selector.is_none());
    }

    #[test]
    fn parse_op_query_with_selector() {
        let query = enc(&doc! {"find": "c"});
        let selector = enc(&doc! {"_id": 1});
        let mut frame = Vec::new();
        frame.extend_from_slice(&0u32.to_le_bytes());
        frame.extend_from_slice(b"t.c\x00");
        frame.extend_from_slice(&0i32.to_le_bytes());
        frame.extend_from_slice(&0i32.to_le_bytes());
        frame.extend_from_slice(&query);
        frame.extend_from_slice(&selector);
        let q = parse_op_query(&frame).unwrap();
        assert_eq!(q.return_fields_selector, Some(selector.as_slice()));
    }

    #[test]
    fn parse_body_unsupported_opcode_is_fatal() {
        let err = parse_body(OP_COMPRESSED, &[]).unwrap_err();
        assert!(!err.is_recoverable());
        // The removed legacy write opcodes drop the connection on mongod
        // 8.2.11, whatever the body holds.
        for op in [OP_UPDATE, OP_INSERT, OP_DELETE, OP_KILL_CURSORS] {
            let err = parse_body(op, &[0u8; 24]).unwrap_err();
            assert!(!err.is_recoverable(), "op {op}");
        }
    }

    #[test]
    fn parse_body_unknown_opcode_is_ignored() {
        // mongod 8.2.11 ignores an opcode it does not serve -- OP_REPLY sent
        // as a request and the removed OP_COMMAND (2010) among them...
        for op in [9999, OP_REPLY, 0, -5, 2003, 2010, 2011, i32::MAX] {
            assert_eq!(parse_body(op, &[0u8; 20]), Ok(Op::Ignored(op)), "op {op}");
        }
        assert_eq!(parse_body(9999, &[0u8; 4]), Ok(Op::Ignored(9999)));
        // ...but drops the connection when the body is shorter than 4 bytes.
        for n in 0..4 {
            assert!(parse_body(9999, &vec![0u8; n]).is_err(), "{n} bytes");
        }
    }

    #[test]
    fn parse_body_legacy_get_more_needs_a_namespace() {
        let mut body = 0i32.to_le_bytes().to_vec();
        body.extend_from_slice(b"db.c\x00");
        body.extend_from_slice(&0i32.to_le_bytes());
        body.extend_from_slice(&12345i64.to_le_bytes());
        assert_eq!(parse_body(OP_GET_MORE, &body), Ok(Op::LegacyGetMore));
        // Nothing past the namespace is read (mongod 8.2.11).
        assert_eq!(
            parse_body(OP_GET_MORE, &[0, 0, 0, 0, 0]),
            Ok(Op::LegacyGetMore)
        );
        // No flags word, or no NUL after it: an unusable frame.
        for bad in [&[][..], &[0u8; 4][..], &[0xffu8; 12][..], b"\0\0\0\0db.c"] {
            let err = parse_body(OP_GET_MORE, bad).unwrap_err();
            assert!(!err.is_recoverable());
        }
    }

    #[test]
    fn build_op_msg_reply_roundtrips() {
        let body = enc(&doc! {"ok": 1.0});
        let frame = build_op_msg_reply(99, 100, &body, 0);
        let header = Header::unpack(&frame).unwrap();
        assert_eq!(header.op_code, OP_MSG);
        assert_eq!(header.response_to, 99);
        assert_eq!(header.request_id, 100);
        assert_eq!(header.message_length as usize, frame.len());
        // Reparse the payload after the header.
        let msg = parse_op_msg(&frame[HEADER_SIZE..]).unwrap();
        assert_eq!(msg.body, body.as_slice());
    }

    #[test]
    fn build_op_reply_layout() {
        let d0 = enc(&doc! {"ismaster": true});
        let frame = build_op_reply(5, 6, &[&d0], 0, 0, 0);
        let header = Header::unpack(&frame).unwrap();
        assert_eq!(header.op_code, OP_REPLY);
        assert_eq!(header.message_length as usize, frame.len());
        // documents count is at header(16) + flags(4) + cursor(8) + starting(4).
        let count = rd_i32(&frame, HEADER_SIZE + 4 + 8 + 4);
        assert_eq!(count, 1);
    }
}
