//! A spill file as mongod 8.2's external sorter writes it (RM-325).
//!
//! `SortedFileWriter` (`sorter_template_defs.h`) appends each record's
//! `serializeForSorter` bytes -- the key's, then the value's -- to a buffer,
//! and once the buffer passes `kSortedFileBufferSize` (64 KiB) writes it as
//! one chunk: an `int32` length, then the buffer snappy-compressed (the
//! length negated; [`crate::snappy`] is mongod's compressor) when that saves a tenth, or the buffer as it is. A run's
//! last chunk is written when the run is done. What the file takes on disk
//! -- every chunk's length and bytes -- is the sort's
//! `spilledDataStorageSize`, and the bytes before compression its
//! `spilledBytes`.
//!
//! The records are in mongod's own formats, so the compression sees the
//! bytes mongod's sees:
//!
//! - `Value::serializeForSorter`: the type byte, then the value -- a string
//!   as its length and bytes (no NUL), binary data as its subtype, length
//!   and bytes, a document in the `Document` form below, an array as its
//!   count and values;
//! - `Document::serializeForSorter`: the field count, each field's name (a
//!   C string) and `Value`, then its metadata (one zero byte: none);
//! - `BSONObj::serializeForSorter`: the BSON itself.
//!
//! Each is converted from the BSON this crate's encoder writes
//! ([`crate::bsondoc::write`]) and back, byte for byte, so a string that is
//! not UTF-8 or a field name that had to be escaped comes back as it went.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use bson::{Bson, Document};

/// `kSortedFileBufferSize`.
pub(crate) const CHUNK: usize = 64 << 10;

/// What a failure is about, for the caller to name in its error.
#[derive(Debug)]
pub(crate) struct Error {
    pub what: &'static str,
    pub detail: String,
}

type R<T> = Result<T, Error>;

fn err(what: &'static str, detail: impl std::fmt::Display) -> Error {
    Error {
        what,
        detail: detail.to_string(),
    }
}

fn corrupt(detail: impl std::fmt::Display) -> Error {
    err("read", detail)
}

// ------------------------------------------------------------- formats --

/// The BSON of `{"": v}`: the element after the 4-byte length.
fn element_bson(v: &Bson) -> R<Vec<u8>> {
    let mut d = Document::new();
    d.insert("", v.clone());
    let mut buf = Vec::new();
    crate::bsondoc::write(&d, &mut buf).map_err(|e| err("encode a record for", e))?;
    Ok(buf)
}

/// `Value::serializeForSorter` of `v`.
pub(crate) fn write_value(v: &Bson, out: &mut Vec<u8>) -> R<()> {
    let bson = element_bson(v)?;
    // Length, type, the empty name's NUL, then the value.
    let mut at = Cursor::new(&bson[6..]);
    value_from_bson(bson[4], &mut at, out)
}

/// `Document::serializeForSorter` of `d`.
pub(crate) fn write_document(d: &Document, out: &mut Vec<u8>) -> R<()> {
    let mut bson = Vec::new();
    crate::bsondoc::write(d, &mut bson).map_err(|e| err("encode a record for", e))?;
    let mut at = Cursor::new(&bson);
    document_from_bson(&mut at, out)
}

/// `BSONObj::serializeForSorter` of `d`: its BSON.
pub(crate) fn write_bson(d: &Document, out: &mut Vec<u8>) -> R<()> {
    crate::bsondoc::write(d, out).map_err(|e| err("encode a record for", e))
}

/// `WorkingSetMember::serialize` (`working_set.cpp`, mongod 8.2) of a
/// member an index scan made (`RID_AND_IDX`, no document, no metadata): its
/// state, the count of its index keys (one), the index's key pattern and
/// the key as BSON -- the key's every field name empty --, the index's id
/// in the plan's working set, the snapshot it was read in, then its
/// RecordId's format (`Long`) and value, and a zero byte for no metadata.
pub(crate) fn write_index_key_member(
    key_pattern: &Document,
    index_key: &[Bson],
    index_id: u32,
    snapshot_id: u64,
    rid: i64,
    out: &mut Vec<u8>,
) -> R<()> {
    /// `WorkingSetMember::RID_AND_IDX`.
    const RID_AND_IDX: u8 = 1;
    out.push(RID_AND_IDX);
    out.push(1);
    write_bson(key_pattern, out)?;
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    for v in index_key {
        let bson = element_bson(v)?;
        // The element: past the length, before the document's NUL.
        out.extend_from_slice(&bson[4..bson.len() - 1]);
    }
    out.push(0);
    finish_doc(out, start)?;
    out.extend_from_slice(&index_id.to_le_bytes());
    out.extend_from_slice(&snapshot_id.to_le_bytes());
    // `RecordIdFormat::Long`.
    out.push(0);
    out.extend_from_slice(&rid.to_le_bytes());
    out.push(0);
    Ok(())
}

/// The bytes a [`write_index_key_member`] wrote, read as
/// `WorkingSetMember::deserialize` reads them.
pub(crate) fn read_index_key_member(at: &mut Cursor) -> R<Vec<u8>> {
    let start = at.pos;
    if at.byte()? != 1 {
        return Err(corrupt("a working-set member not of index keys"));
    }
    for _ in 0..at.byte()? {
        for _ in 0..2 {
            let len = usize::try_from(at.peek_i32()?)
                .ok()
                .filter(|n| *n >= 5)
                .ok_or_else(|| corrupt("a key with a bad length"))?;
            at.take(len)?;
        }
        at.take(4 + 8)?;
    }
    match at.byte()? {
        0 => {
            at.take(8)?;
        }
        1 => {
            let n = at.len()?;
            at.take(n)?;
        }
        _ => return Err(corrupt("a RecordId of no format")),
    }
    if at.byte()? != 0 {
        return Err(corrupt("a working-set member with metadata"));
    }
    Ok(at.bytes[start..at.pos].to_vec())
}

/// The value a [`write_value`] wrote.
pub(crate) fn read_value(at: &mut Cursor) -> R<Bson> {
    let mut bson = vec![0, 0, 0, 0];
    let t = at.byte()?;
    bson.push(t);
    bson.push(0);
    value_to_bson(t, at, &mut bson)?;
    bson.push(0);
    finish_doc(&mut bson, 0)?;
    let mut d = crate::bsondoc::from_bytes(&bson).map_err(corrupt)?;
    d.remove("")
        .ok_or_else(|| corrupt("a value that is not one"))
}

/// The document a [`write_document`] wrote.
pub(crate) fn read_document(at: &mut Cursor) -> R<Document> {
    let mut bson = Vec::new();
    document_to_bson(at, &mut bson)?;
    crate::bsondoc::from_bytes(&bson).map_err(corrupt)
}

/// The document a [`write_bson`] wrote.
pub(crate) fn read_bson(at: &mut Cursor) -> R<Document> {
    let len = at.peek_i32()?;
    let len = usize::try_from(len)
        .ok()
        .filter(|n| *n >= 5)
        .ok_or_else(|| corrupt("a document with a bad length"))?;
    crate::bsondoc::from_bytes(at.take(len)?).map_err(corrupt)
}

/// `Value::serializeForSorter` of `v`, where `None` is the missing value:
/// its type byte (EOO) alone.
pub(crate) fn write_value_or_missing(v: Option<&Bson>, out: &mut Vec<u8>) -> R<()> {
    match v {
        Some(v) => write_value(v, out),
        None => {
            out.push(0);
            Ok(())
        }
    }
}

/// The value a [`write_value_or_missing`] wrote.
pub(crate) fn read_value_or_missing(at: &mut Cursor) -> R<Option<Bson>> {
    if at.peek_byte()? == 0 {
        at.byte()?;
        return Ok(None);
    }
    read_value(at).map(Some)
}

/// `Value::serializeForSorter` of an array whose elements may be missing
/// (a `$group`'s compound key, or its accumulators' states): the type byte,
/// the count, then each element.
pub(crate) fn write_parts(parts: &[Option<Bson>], out: &mut Vec<u8>) -> R<()> {
    out.push(0x04);
    let count =
        i32::try_from(parts.len()).map_err(|_| err("encode a record for", "an array too long"))?;
    out.extend_from_slice(&count.to_le_bytes());
    for p in parts {
        write_value_or_missing(p.as_ref(), out)?;
    }
    Ok(())
}

/// The elements a [`write_parts`] wrote.
pub(crate) fn read_parts(at: &mut Cursor) -> R<Vec<Option<Bson>>> {
    if at.byte()? != 0x04 {
        return Err(corrupt("a record whose parts are not an array"));
    }
    let count = at.len()?;
    let mut parts = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        parts.push(read_value_or_missing(at)?);
    }
    Ok(parts)
}

/// Bytes being read, with every read bounds-checked: a short or damaged
/// spill fails the query, never panics.
pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    #[cfg(test)]
    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.bytes.len())
            .ok_or_else(|| corrupt("a record that runs past its chunk"))?;
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn byte(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }

    fn peek_byte(&self) -> R<u8> {
        self.bytes
            .get(self.pos)
            .copied()
            .ok_or_else(|| corrupt("a record that runs past its chunk"))
    }

    fn i32(&mut self) -> R<i32> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn peek_i32(&self) -> R<i32> {
        let b = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| corrupt("a record that runs past its chunk"))?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn len(&mut self) -> R<usize> {
        usize::try_from(self.i32()?).map_err(|_| corrupt("a negative length"))
    }

    /// A C string, its NUL included.
    fn cstr(&mut self) -> R<&'a [u8]> {
        let rest = &self.bytes[self.pos.min(self.bytes.len())..];
        let n = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| corrupt("a name without its NUL"))?;
        self.take(n + 1)
    }
}

/// The fixed width of a value of BSON type `t`, for the types whose value
/// is the same bytes in both forms.
fn fixed(t: u8) -> Option<usize> {
    match t {
        0x0A | 0x06 | 0xFF | 0x7F => Some(0),
        0x08 => Some(1),
        0x10 => Some(4),
        0x01 | 0x09 | 0x11 | 0x12 => Some(8),
        0x07 => Some(12),
        0x13 => Some(16),
        _ => None,
    }
}

/// One BSON value of type `t` at `at`, written in `Value` form (its type
/// byte first).
fn value_from_bson(t: u8, at: &mut Cursor, out: &mut Vec<u8>) -> R<()> {
    out.push(t);
    if let Some(n) = fixed(t) {
        out.extend_from_slice(at.take(n)?);
        return Ok(());
    }
    match t {
        // String, JavaScript, symbol: the length without its NUL.
        0x02 | 0x0D | 0x0E => {
            let n = at.len()?;
            let s = at.take(n)?;
            let text = s.get(..n.saturating_sub(1)).unwrap_or_default();
            out.extend_from_slice(&(text.len() as i32).to_le_bytes());
            out.extend_from_slice(text);
        }
        0x03 => document_from_bson(at, out)?,
        0x04 => {
            let start = at.pos;
            let total = at.len()?;
            let mut items = Vec::new();
            let mut count = 0i32;
            loop {
                let et = at.byte()?;
                if et == 0 {
                    break;
                }
                at.cstr()?;
                value_from_bson(et, at, &mut items)?;
                count += 1;
            }
            if at.pos - start != total {
                return Err(corrupt("an array with a bad length"));
            }
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&items);
        }
        // Binary data: the subtype, then the length.
        0x05 => {
            let n = at.len()?;
            let subtype = at.byte()?;
            out.push(subtype);
            out.extend_from_slice(&(n as i32).to_le_bytes());
            out.extend_from_slice(at.take(n)?);
        }
        // Regular expression: the pattern and the flags, as they are.
        0x0B => {
            out.extend_from_slice(at.cstr()?);
            out.extend_from_slice(at.cstr()?);
        }
        // DBPointer: the ObjectId, then the namespace as a C string.
        0x0C => {
            let n = at.len()?;
            let ns = at.take(n)?;
            out.extend_from_slice(at.take(12)?);
            out.extend_from_slice(ns);
        }
        // JavaScript with a scope: the code's length and bytes, then the
        // scope's BSON.
        0x0F => {
            let _total = at.len()?;
            let n = at.len()?;
            let code = at.take(n)?;
            let text = code.get(..n.saturating_sub(1)).unwrap_or_default();
            out.extend_from_slice(&(text.len() as i32).to_le_bytes());
            out.extend_from_slice(text);
            let scope = at.peek_i32()?;
            let scope = usize::try_from(scope).map_err(|_| corrupt("a scope with a bad length"))?;
            out.extend_from_slice(at.take(scope)?);
        }
        _ => {
            return Err(err(
                "encode a record for",
                format!("a value of type {t:#04x}"),
            ))
        }
    }
    Ok(())
}

/// A BSON document at `at`, written in `Document` form.
fn document_from_bson(at: &mut Cursor, out: &mut Vec<u8>) -> R<()> {
    let start = at.pos;
    let total = at.len()?;
    let count_at = out.len();
    out.extend_from_slice(&[0; 4]);
    let mut count = 0i32;
    loop {
        let t = at.byte()?;
        if t == 0 {
            break;
        }
        out.extend_from_slice(at.cstr()?);
        value_from_bson(t, at, out)?;
        count += 1;
    }
    if at.pos - start != total {
        return Err(corrupt("a document with a bad length"));
    }
    out[count_at..count_at + 4].copy_from_slice(&count.to_le_bytes());
    // `DocumentMetadataFields::serializeForSorter`: none.
    out.push(0);
    Ok(())
}

/// Write the length of the document that starts at `start` of `bson`.
fn finish_doc(bson: &mut [u8], start: usize) -> R<()> {
    let n = i32::try_from(bson.len() - start).map_err(|_| corrupt("a record too large"))?;
    bson[start..start + 4].copy_from_slice(&n.to_le_bytes());
    Ok(())
}

/// One `Value`'s value (its type `t` already read) at `at`, written as a
/// BSON value.
fn value_to_bson(t: u8, at: &mut Cursor, out: &mut Vec<u8>) -> R<()> {
    if let Some(n) = fixed(t) {
        out.extend_from_slice(at.take(n)?);
        return Ok(());
    }
    match t {
        0x02 | 0x0D | 0x0E => {
            let n = at.len()?;
            out.extend_from_slice(&((n + 1) as i32).to_le_bytes());
            out.extend_from_slice(at.take(n)?);
            out.push(0);
        }
        0x03 => document_to_bson(at, out)?,
        0x04 => {
            let n = at.len()?;
            let start = out.len();
            out.extend_from_slice(&[0; 4]);
            for i in 0..n {
                let et = at.byte()?;
                out.push(et);
                out.extend_from_slice(i.to_string().as_bytes());
                out.push(0);
                value_to_bson(et, at, out)?;
            }
            out.push(0);
            finish_doc(out, start)?;
        }
        0x05 => {
            let subtype = at.byte()?;
            let n = at.len()?;
            out.extend_from_slice(&(n as i32).to_le_bytes());
            out.push(subtype);
            out.extend_from_slice(at.take(n)?);
        }
        0x0B => {
            out.extend_from_slice(at.cstr()?);
            out.extend_from_slice(at.cstr()?);
        }
        0x0C => {
            let oid = at.take(12)?;
            let ns = at.cstr()?;
            out.extend_from_slice(&(ns.len() as i32).to_le_bytes());
            out.extend_from_slice(ns);
            out.extend_from_slice(oid);
        }
        0x0F => {
            let start = out.len();
            out.extend_from_slice(&[0; 4]);
            let n = at.len()?;
            out.extend_from_slice(&((n + 1) as i32).to_le_bytes());
            out.extend_from_slice(at.take(n)?);
            out.push(0);
            let scope = usize::try_from(at.peek_i32()?)
                .map_err(|_| corrupt("a scope with a bad length"))?;
            out.extend_from_slice(at.take(scope)?);
            finish_doc(out, start)?;
        }
        _ => return Err(corrupt(format!("a value of type {t:#04x}"))),
    }
    Ok(())
}

/// One `Document` at `at`, written as BSON.
fn document_to_bson(at: &mut Cursor, out: &mut Vec<u8>) -> R<()> {
    let n = at.len()?;
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    for _ in 0..n {
        let name = at.cstr()?;
        let t = at.byte()?;
        out.push(t);
        out.extend_from_slice(name);
        value_to_bson(t, at, out)?;
    }
    if at.byte()? != 0 {
        return Err(corrupt("a document with metadata"));
    }
    out.push(0);
    finish_doc(out, start)
}

// --------------------------------------------------------------- files --

/// What a run wrote: `bytes` before compression (`spilledBytes`), `stored`
/// on disk (`spilledDataStorageSize`), and the `chunks` it was written in
/// (`metrics.query.sort.spillToDisk` counts them, RM-326).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Written {
    pub bytes: i64,
    pub stored: i64,
    pub chunks: i64,
}

/// `SortedFileWriter`: records into chunks, chunks into a file.
pub(crate) struct Writer {
    path: PathBuf,
    out: BufWriter<File>,
    buffer: Vec<u8>,
    written: Written,
}

impl Writer {
    pub(crate) fn create(path: &Path) -> R<Writer> {
        let file = File::create(path).map_err(|e| err("create", e))?;
        Ok(Writer {
            path: path.to_path_buf(),
            out: BufWriter::new(file),
            buffer: Vec::new(),
            written: Written::default(),
        })
    }

    /// The buffer the next record is written into; [`Writer::added`] once
    /// it is.
    pub(crate) fn buffer(&mut self) -> &mut Vec<u8> {
        &mut self.buffer
    }

    /// `addAlreadySorted`: a record was written to the buffer.
    pub(crate) fn added(&mut self) -> R<()> {
        if self.buffer.len() > CHUNK {
            self.write_chunk()?;
        }
        Ok(())
    }

    /// `writeChunk`.
    fn write_chunk(&mut self) -> R<()> {
        let size = self.buffer.len();
        if size == 0 {
            return Ok(());
        }
        self.written.bytes += size as i64;
        let compressed = crate::snappy::compress(&self.buffer);
        let (signed, payload) = if compressed.len() < size / 10 * 9 {
            (-(compressed.len() as i64), &compressed[..])
        } else {
            (size as i64, &self.buffer[..])
        };
        let signed = i32::try_from(signed).map_err(|_| err("write", "a chunk too large"))?;
        self.out
            .write_all(&signed.to_le_bytes())
            .and_then(|()| self.out.write_all(payload))
            .map_err(|e| err("write", e))?;
        self.written.stored += 4 + payload.len() as i64;
        self.written.chunks += 1;
        self.buffer.clear();
        Ok(())
    }

    /// `done`: the last chunk written and the file synced.
    pub(crate) fn done(mut self) -> R<Written> {
        self.write_chunk()?;
        self.out.flush().map_err(|e| err("write", e))?;
        let file = self.out.into_inner().map_err(|e| err("write", e.error()))?;
        file.sync_all().map_err(|e| err("write", e))?;
        let on_disk = file.metadata().map_err(|e| err("measure", e))?.len() as i64;
        if on_disk != self.written.stored {
            return Err(err(
                "write",
                format!(
                    "{} holds {on_disk} bytes, not the {} written",
                    self.path.display(),
                    self.written.stored
                ),
            ));
        }
        Ok(self.written)
    }
}

/// A run being read back, chunk by chunk.
pub(crate) struct Reader {
    input: BufReader<File>,
    chunk: Vec<u8>,
    pos: usize,
}

impl Reader {
    pub(crate) fn open(path: &Path) -> R<Reader> {
        let file = File::open(path).map_err(|e| err("open", e))?;
        Ok(Reader {
            input: BufReader::new(file),
            chunk: Vec::new(),
            pos: 0,
        })
    }

    /// Read the next record with `f`, from the next chunk when this one is
    /// finished. A record never spans two chunks.
    pub(crate) fn record<T>(&mut self, f: impl FnOnce(&mut Cursor) -> R<T>) -> R<T> {
        if self.pos >= self.chunk.len() {
            self.next_chunk()?;
        }
        let mut at = Cursor::new(&self.chunk[self.pos..]);
        let out = f(&mut at)?;
        self.pos += at.pos;
        Ok(out)
    }

    fn next_chunk(&mut self) -> R<()> {
        let mut len = [0u8; 4];
        self.input
            .read_exact(&mut len)
            .map_err(|e| err("read", e))?;
        let signed = i32::from_le_bytes(len);
        let n = signed.unsigned_abs() as usize;
        let mut payload = vec![0u8; n];
        self.input
            .read_exact(&mut payload)
            .map_err(|e| err("read", e))?;
        self.chunk = if signed < 0 {
            snap::raw::Decoder::new()
                .decompress_vec(&payload)
                .map_err(|e| corrupt(format!("a chunk that does not decompress: {e}")))?
        } else {
            payload
        };
        self.pos = 0;
        if self.chunk.is_empty() {
            return Err(corrupt("an empty chunk"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;
    use bson::oid::ObjectId;
    use bson::spec::BinarySubtype;

    fn every_type() -> Document {
        doc! {
            "d": 1.5, "s": "héllo", "o": { "a": 1, "b": [1, "x", { "c": null }] },
            "a": [], "bin": bson::Binary { subtype: BinarySubtype::Uuid, bytes: vec![1; 16] },
            "u": Bson::Undefined, "oid": ObjectId::from_bytes([7; 12]), "t": true,
            "date": bson::DateTime::from_millis(-5), "n": Bson::Null,
            "re": bson::Regex { pattern: "a.b".into(), options: "i".into() },
            "js": Bson::JavaScriptCode("f()".into()),
            "sym": Bson::Symbol("sym".into()),
            "jsw": Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope {
                code: "g()".into(), scope: doc! { "x": 1 } }),
            "i": 7, "ts": bson::Timestamp { time: 3, increment: 4 }, "l": 9i64,
            "dec": "1.10".parse::<bson::Decimal128>().unwrap(),
            "min": Bson::MinKey, "max": Bson::MaxKey,
            "bad": crate::bsondoc::invalid_utf8(vec![0xff, 0xfe]),
        }
    }

    #[test]
    fn values_and_documents_round_trip() {
        let d = every_type();
        let mut buf = Vec::new();
        write_document(&d, &mut buf).unwrap();
        write_value(
            &Bson::Array(vec![Bson::Int32(1), Bson::String("k".into())]),
            &mut buf,
        )
        .unwrap();
        write_bson(&d, &mut buf).unwrap();
        let mut at = Cursor::new(&buf);
        assert_eq!(read_document(&mut at).unwrap(), d);
        assert_eq!(
            read_value(&mut at).unwrap(),
            Bson::Array(vec![Bson::Int32(1), Bson::String("k".into())])
        );
        assert_eq!(read_bson(&mut at).unwrap(), d);
        assert!(at.at_end());
    }

    #[test]
    fn the_forms_are_mongods() {
        // `Value::serializeForSorter` of "ab": type, length, bytes.
        let mut v = Vec::new();
        write_value(&Bson::String("ab".into()), &mut v).unwrap();
        assert_eq!(v, [2, 2, 0, 0, 0, b'a', b'b']);
        // `Document::serializeForSorter` of {a: 1}: count, name, value,
        // no metadata.
        let mut d = Vec::new();
        write_document(&doc! { "a": 1 }, &mut d).unwrap();
        assert_eq!(d, [1, 0, 0, 0, b'a', 0, 0x10, 1, 0, 0, 0, 0]);
        // Binary data: subtype before length.
        let mut b = Vec::new();
        write_value(
            &Bson::Binary(bson::Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![9],
            }),
            &mut b,
        )
        .unwrap();
        assert_eq!(b, [5, 0, 1, 0, 0, 0, 9]);
    }

    #[test]
    fn chunks_compress_when_that_saves_a_tenth() {
        let dir = std::env::temp_dir().join(format!("secantus-spill-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run");
        let mut w = Writer::create(&path).unwrap();
        let d = doc! { "s": "x".repeat(1000) };
        for _ in 0..100 {
            write_bson(&d, w.buffer()).unwrap();
            w.added().unwrap();
        }
        let written = w.done().unwrap();
        assert_eq!(written.bytes, 100 * 1013);
        // Two chunks (the first passes 64 KiB), each far smaller compressed.
        assert!(written.stored < written.bytes / 10, "{written:?}");
        assert_eq!(
            std::fs::metadata(&path).unwrap().len() as i64,
            written.stored
        );
        let mut r = Reader::open(&path).unwrap();
        for _ in 0..100 {
            assert_eq!(r.record(read_bson).unwrap(), d);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
