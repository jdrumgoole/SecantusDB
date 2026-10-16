//! `OP_COMPRESSED` (2012): the wire-compression wrapper, with the three
//! compressors mongod enables by default (`snappy`, `zstd`, `zlib`).
//!
//! The frame after the standard header is `originalOpcode: int32`,
//! `uncompressedSize: int32`, `compressorId: uint8`, then the compressed bytes
//! of the original message's body (everything after ITS header).
//!
//! What mongod 8.2.11 does, measured 2026-10-15 over a raw socket:
//!
//! - A request may arrive compressed with any enabled compressor, negotiated
//!   or not, and the reply goes back under the compressor the request used
//!   (every frame of an exhaust stream too, and an `OP_REPLY` to a compressed
//!   `OP_QUERY`). `hello` and the SASL commands are answered compressed like
//!   any other: the "never compress these" list in the compression spec is a
//!   rule for the CLIENT.
//! - Id 0 (`noop`) is not enabled by default and closes the connection, as an
//!   unknown id does.
//! - An `uncompressedSize` below zero, or which with a header would exceed the
//!   48,000,000-byte message limit, closes the connection before anything is
//!   decompressed; so does data that does not decompress, or decompresses to a
//!   size other than the one declared.
//!
//! So every failure here is [`WireError::Protocol`] (fatal), and decompression
//! never allocates more than the declared size, which is bounded first: a
//! client's say-so cannot make the server reserve unbounded memory.

use crate::{rd_i32, Header, WireError, HEADER_SIZE, MAX_MESSAGE_SIZE, OP_COMPRESSED};

/// The compressors this server speaks: mongod's default
/// `net.compression.compressors`, `snappy,zstd,zlib`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compressor {
    Snappy,
    Zlib,
    Zstd,
}

/// Length of the `OP_COMPRESSED` prefix: original opcode, uncompressed size,
/// compressor id.
pub const PREFIX_LEN: usize = 4 + 4 + 1;

impl Compressor {
    /// The wire id. `0` is `noop`, which is not enabled.
    pub fn id(self) -> u8 {
        match self {
            Compressor::Snappy => 1,
            Compressor::Zlib => 2,
            Compressor::Zstd => 3,
        }
    }

    /// The compressor an id names, if it is one this server has enabled.
    pub fn from_id(id: u8) -> Option<Compressor> {
        match id {
            1 => Some(Compressor::Snappy),
            2 => Some(Compressor::Zlib),
            3 => Some(Compressor::Zstd),
            _ => None,
        }
    }

    /// The name `hello`'s `compression` list uses.
    pub fn name(self) -> &'static str {
        match self {
            Compressor::Snappy => "snappy",
            Compressor::Zlib => "zlib",
            Compressor::Zstd => "zstd",
        }
    }

    /// The compressor a `hello` names, matched exactly as mongod matches it
    /// (`"ZLIB"` and `"noop"` name none it will answer).
    pub fn from_name(name: &str) -> Option<Compressor> {
        match name {
            "snappy" => Some(Compressor::Snappy),
            "zlib" => Some(Compressor::Zlib),
            "zstd" => Some(Compressor::Zstd),
            _ => None,
        }
    }

    /// Compress `data`.
    pub fn compress(self, data: &[u8]) -> Result<Vec<u8>, WireError> {
        let fail =
            |e: String| WireError::Protocol(format!("{} compression failed: {e}", self.name()));
        match self {
            Compressor::Snappy => snap::raw::Encoder::new()
                .compress_vec(data)
                .map_err(|e| fail(e.to_string())),
            Compressor::Zlib => {
                use std::io::Write;
                let mut enc = flate2::write::ZlibEncoder::new(
                    Vec::with_capacity(data.len() / 2 + 64),
                    flate2::Compression::default(),
                );
                enc.write_all(data).map_err(|e| fail(e.to_string()))?;
                enc.finish().map_err(|e| fail(e.to_string()))
            }
            Compressor::Zstd => zstd::bulk::compress(data, zstd::DEFAULT_COMPRESSION_LEVEL)
                .map_err(|e| fail(e.to_string())),
        }
    }

    /// Decompress `data`, which must come to exactly `size` bytes. The output
    /// buffer is `size` bytes and never grows: data that would decompress to
    /// more is refused rather than followed.
    pub fn decompress(self, data: &[u8], size: usize) -> Result<Vec<u8>, WireError> {
        let fail =
            |e: String| WireError::Protocol(format!("{} decompression failed: {e}", self.name()));
        let out = match self {
            Compressor::Snappy => {
                // The snappy preamble states the length; check it before
                // allocating, so a lying preamble costs nothing.
                let claimed = snap::raw::decompress_len(data).map_err(|e| fail(e.to_string()))?;
                if claimed != size {
                    return Err(fail(format!(
                        "data decompresses to {claimed} bytes, not the {size} declared"
                    )));
                }
                let mut out = vec![0u8; size];
                let n = snap::raw::Decoder::new()
                    .decompress(data, &mut out)
                    .map_err(|e| fail(e.to_string()))?;
                out.truncate(n);
                out
            }
            Compressor::Zlib => {
                let mut out = Vec::with_capacity(size);
                let mut inflater = flate2::Decompress::new(true);
                let status = inflater
                    .decompress_vec(data, &mut out, flate2::FlushDecompress::Finish)
                    .map_err(|e| fail(e.to_string()))?;
                if status != flate2::Status::StreamEnd {
                    // Either the stream is cut short, or it holds more than
                    // `size` bytes and filled the buffer first.
                    return Err(fail(format!(
                        "stream does not end within the {size} bytes declared"
                    )));
                }
                out
            }
            Compressor::Zstd => {
                zstd::bulk::decompress(data, size).map_err(|e| fail(e.to_string()))?
            }
        };
        if out.len() != size {
            return Err(fail(format!(
                "data decompresses to {} bytes, not the {size} declared",
                out.len()
            )));
        }
        Ok(out)
    }
}

/// A parsed `OP_COMPRESSED` body: the opcode and size of the message it
/// wraps, the compressor id as sent, and the compressed bytes (borrowed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpCompressed<'a> {
    pub original_opcode: i32,
    pub uncompressed_size: i32,
    pub compressor_id: u8,
    pub data: &'a [u8],
}

/// Parse an `OP_COMPRESSED` body (everything after the header).
pub fn parse_op_compressed(body: &[u8]) -> Result<OpCompressed<'_>, WireError> {
    if body.len() < PREFIX_LEN {
        return Err(WireError::Protocol(format!(
            "OP_COMPRESSED body of {} bytes is shorter than its {PREFIX_LEN}-byte prefix",
            body.len()
        )));
    }
    Ok(OpCompressed {
        original_opcode: rd_i32(body, 0),
        uncompressed_size: rd_i32(body, 4),
        compressor_id: body[8],
        data: &body[PREFIX_LEN..],
    })
}

impl OpCompressed<'_> {
    /// Check the compressor and the declared size, then decompress. Returns
    /// the compressor the request used (the reply goes back under it) and the
    /// original message's body.
    pub fn decompress(&self) -> Result<(Compressor, Vec<u8>), WireError> {
        let compressor = Compressor::from_id(self.compressor_id).ok_or_else(|| {
            WireError::Protocol(format!(
                "compressor id {} is not one this server has enabled",
                self.compressor_id
            ))
        })?;
        if self.uncompressed_size < 0 {
            return Err(WireError::Protocol(format!(
                "decompressed message would be negative in size ({})",
                self.uncompressed_size
            )));
        }
        let size = self.uncompressed_size as usize;
        if size + HEADER_SIZE > MAX_MESSAGE_SIZE as usize {
            return Err(WireError::Protocol(format!(
                "decompressed message of {} bytes would exceed the {MAX_MESSAGE_SIZE}-byte maximum",
                size + HEADER_SIZE
            )));
        }
        let body = compressor.decompress(self.data, size)?;
        Ok((compressor, body))
    }

    /// The header of the message this one wraps: the original opcode, and the
    /// length the uncompressed message has.
    pub fn original_header(&self, outer: &Header) -> Header {
        Header {
            message_length: self.uncompressed_size.saturating_add(HEADER_SIZE as i32),
            request_id: outer.request_id,
            response_to: outer.response_to,
            op_code: self.original_opcode,
        }
    }
}

/// Wrap a complete message frame (header and body) in `OP_COMPRESSED`,
/// keeping its request id and `responseTo`.
pub fn compress_frame(frame: &[u8], compressor: Compressor) -> Result<Vec<u8>, WireError> {
    let inner = Header::unpack(frame)?;
    let body = &frame[HEADER_SIZE..];
    let compressed = compressor.compress(body)?;
    let total = HEADER_SIZE + PREFIX_LEN + compressed.len();
    let header = Header {
        message_length: i32::try_from(total)
            .map_err(|_| WireError::Protocol(format!("compressed frame of {total} bytes")))?,
        request_id: inner.request_id,
        response_to: inner.response_to,
        op_code: OP_COMPRESSED,
    };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&header.pack());
    out.extend_from_slice(&inner.op_code.to_le_bytes());
    out.extend_from_slice(&(body.len() as i32).to_le_bytes());
    out.push(compressor.id());
    out.extend_from_slice(&compressed);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_op_msg_reply, OP_MSG};

    const ALL: [Compressor; 3] = [Compressor::Snappy, Compressor::Zlib, Compressor::Zstd];

    fn frame() -> Vec<u8> {
        let mut body = Vec::new();
        bson::doc! {"ok": 1.0, "pad": "x".repeat(4000)}
            .to_writer(&mut body)
            .unwrap();
        build_op_msg_reply(7, 9, &body, 0)
    }

    fn outer_body(frame: &[u8]) -> &[u8] {
        &frame[HEADER_SIZE..]
    }

    #[test]
    fn round_trips_a_frame_under_each_compressor() {
        let plain = frame();
        for c in ALL {
            let wrapped = compress_frame(&plain, c).unwrap();
            let outer = Header::unpack(&wrapped).unwrap();
            assert_eq!(outer.op_code, OP_COMPRESSED);
            assert_eq!((outer.request_id, outer.response_to), (9, 7));
            assert_eq!(outer.message_length as usize, wrapped.len());
            let parsed = parse_op_compressed(outer_body(&wrapped)).unwrap();
            assert_eq!(parsed.original_opcode, OP_MSG);
            assert_eq!(parsed.compressor_id, c.id());
            let (used, body) = parsed.decompress().unwrap();
            assert_eq!(used, c);
            assert_eq!(body, &plain[HEADER_SIZE..]);
            assert_eq!(
                parsed.original_header(&outer),
                Header::unpack(&plain).unwrap()
            );
        }
    }

    #[test]
    fn names_and_ids_are_mongods() {
        for (c, id, name) in [
            (Compressor::Snappy, 1, "snappy"),
            (Compressor::Zlib, 2, "zlib"),
            (Compressor::Zstd, 3, "zstd"),
        ] {
            assert_eq!((c.id(), c.name()), (id, name));
            assert_eq!(Compressor::from_id(id), Some(c));
            assert_eq!(Compressor::from_name(name), Some(c));
        }
        // noop is not enabled by default; names match exactly.
        for id in [0u8, 4, 255] {
            assert_eq!(Compressor::from_id(id), None);
        }
        for name in ["noop", "ZLIB", "disabled", ""] {
            assert_eq!(Compressor::from_name(name), None);
        }
    }

    fn with_size(
        c: Compressor,
        data: &[u8],
        size: i32,
    ) -> Result<(Compressor, Vec<u8>), WireError> {
        OpCompressed {
            original_opcode: OP_MSG,
            uncompressed_size: size,
            compressor_id: c.id(),
            data,
        }
        .decompress()
    }

    #[test]
    fn a_declared_size_other_than_the_real_one_is_fatal() {
        let body = vec![b'a'; 1000];
        for c in ALL {
            let data = c.compress(&body).unwrap();
            assert!(with_size(c, &data, 1000).is_ok(), "{c:?}");
            for size in [999, 1001, 0, 1] {
                let err = with_size(c, &data, size).unwrap_err();
                assert!(!err.is_recoverable(), "{c:?} {size}: {err}");
            }
        }
    }

    #[test]
    fn a_size_past_the_message_limit_is_refused_before_decompressing() {
        for c in ALL {
            for size in [-1, i32::MAX, MAX_MESSAGE_SIZE - HEADER_SIZE as i32 + 1] {
                let err = with_size(c, b"", size).unwrap_err();
                assert!(!err.is_recoverable());
            }
        }
    }

    #[test]
    fn data_that_inflates_past_its_declared_size_stops_at_it() {
        // 40 MB of zeros compresses to a few kilobytes; declaring 1 KB must
        // not decompress the rest.
        let big = vec![0u8; 40_000_000];
        for c in ALL {
            let data = c.compress(&big).unwrap();
            assert!(data.len() < 4_000_000, "{c:?}: {}", data.len());
            assert!(with_size(c, &data, 1024).is_err(), "{c:?}");
        }
    }

    #[test]
    fn garbage_and_unknown_ids_are_fatal() {
        for c in ALL {
            assert!(with_size(c, b"\x01\x02garbage", 22).is_err(), "{c:?}");
        }
        for id in [0u8, 4, 255] {
            let parsed = OpCompressed {
                original_opcode: OP_MSG,
                uncompressed_size: 3,
                compressor_id: id,
                data: b"abc",
            };
            assert!(!parsed.decompress().unwrap_err().is_recoverable());
        }
        assert!(parse_op_compressed(&[0u8; 8]).is_err());
    }
}
