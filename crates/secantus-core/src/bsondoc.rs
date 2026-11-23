//! Decoding stored and received BSON into a [`Document`], literally.
//!
//! `Document::from_reader` goes through serde, and the `bson` crate's serde
//! visitor reads any sub-document that looks like extended JSON as the value
//! it would stand for there: `{a: {$numberLong: "1"}}` comes back as
//! `{a: Int64(1)}`, and `{$oid: "..."}`, `{$date: ...}`, `{$numberInt: ...}`
//! and the rest likewise. To a database those are documents a client stored.
//! Decoding one that way and writing it back replaced the sub-document with a
//! number, so an update of any OTHER field of such a document changed it
//! (measured against mongod 8.2.11, 2026-10-12, which keeps the sub-document).
//!
//! The raw-document conversion used here maps each element to the `Bson` of
//! its own type and nothing else.
//!
//! # Strings that are not UTF-8 (RM-222)
//!
//! A BSON string is a length-prefixed run of bytes, and mongod treats it as
//! one: it stores, returns, compares, hashes and indexes a string whose bytes
//! are not valid UTF-8 without complaint (measured 8.2.11, 2026-10-27: an
//! insert of `{s: <0xff 0xfe>}` answers `n: 1`, and a find returns the same
//! two bytes). The `bson` crate's `Bson::String` holds a Rust `String`, which
//! cannot hold those bytes, so such a string is decoded into a CARRIER: a
//! one-field document whose only key is a lone NUL, holding the bytes as
//! generic binary data ([`invalid_utf8`], [`as_invalid_utf8`]). No client can
//! send that document -- a BSON key is a NUL-terminated C string, so it can
//! never BE a NUL -- and [`write`] / [`to_vec`] turn it back into the string
//! element it came from. The `bson` crate's own encoder refuses a key with a
//! NUL in it, so a carrier that reaches an encoder other than this one fails
//! that write loudly instead of storing something else.
//!
//! A field NAME that is not UTF-8 (mongod keeps those too) is kept in the
//! key itself, escaped: each byte that is not part of valid UTF-8 becomes a
//! NUL followed by its two lowercase hex digits ([`escape_key`]). A real key
//! never contains a NUL, so the escape cannot collide with one, and a dotted
//! path that names the field reaches it: the path's own bytes escape the same
//! way.
//!
//! Code that reads or compares strings asks [`str_bytes`] for the bytes of
//! either form.

use std::borrow::Cow;
use std::fmt;

use bson::spec::BinarySubtype;
use bson::{Binary, Bson, Document, RawDocument};

/// The key of the one field of an invalid-UTF-8 string's carrier.
pub const INVALID_UTF8_KEY: &str = "\0";

/// A document that could not be decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<bson::raw::Error> for Error {
    fn from(e: bson::raw::Error) -> Self {
        Error(e.to_string())
    }
}

fn malformed(what: impl fmt::Display) -> Error {
    Error(format!("malformed value: {what}"))
}

/// The document at the start of `bytes`. Trailing bytes are ignored, as they
/// are by a reader.
pub fn from_bytes(bytes: &[u8]) -> Result<Document, Error> {
    let declared = bytes
        .get(..4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n <= bytes.len());
    let exact = match declared {
        Some(n) => &bytes[..n],
        None => bytes,
    };
    let raw = RawDocument::from_bytes(exact)?;
    match Document::try_from(raw) {
        Ok(doc) => Ok(doc),
        // A string or a field name that is not UTF-8: decode again, keeping
        // those bytes. The rare case, so the common one stays on the crate's
        // decoder.
        Err(e) if matches!(e.kind, bson::raw::ErrorKind::Utf8EncodingError(_)) => {
            read_doc(raw.as_bytes())
        }
        Err(e) => Err(e.into()),
    }
}

/// Only the top-level fields of the document at the start of `bytes` whose
/// names `keep` accepts, decoded as [`from_bytes`] decodes them; the bytes of
/// every other field are skipped.
pub fn fields(bytes: &[u8], keep: impl Fn(&str) -> bool) -> Result<Document, Error> {
    let fast = || -> Result<Document, bson::raw::Error> {
        let mut out = Document::new();
        for elem in RawDocument::from_bytes(bytes)? {
            let (key, val) = elem?;
            if keep(key) {
                out.insert(key.to_string(), Bson::try_from(val)?);
            }
        }
        Ok(out)
    };
    match fast() {
        Ok(d) => Ok(d),
        // A string or name that is not UTF-8 somewhere: decode it all.
        Err(_) => {
            let mut out = Document::new();
            for (k, v) in from_bytes(bytes)? {
                if keep(&k) {
                    out.insert(k, v);
                }
            }
            Ok(out)
        }
    }
}

/// The carrier of a value of one of the other types that carry text --
/// regex (0x0B), DBPointer (0x0C), JavaScript (0x0D), symbol (0x0E),
/// JavaScript with a scope (0x0F) -- whose text is not UTF-8: its type and
/// its value's raw bytes, under a key no client can send (a NUL and the type
/// in hex, which is never the escape of a name: an escaped byte is above
/// 0x7f). mongod stores and returns such a value (measured 8.2.11,
/// 2026-10-27); [`write`] writes it back as it came.
pub fn raw_text(t: u8, value: Vec<u8>) -> Bson {
    let mut d = Document::new();
    d.insert(
        format!("\0{t:02x}"),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: value,
        }),
    );
    Bson::Document(d)
}

/// The type and raw value bytes of a [`raw_text`] carrier.
pub fn as_raw_text(v: &Bson) -> Option<(u8, &[u8])> {
    let Bson::Document(d) = v else {
        return None;
    };
    if d.len() != 1 {
        return None;
    }
    let (k, x) = d.iter().next()?;
    let t = match k.as_bytes() {
        [0, h @ ..] if h.len() == 2 => u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?,
        _ => return None,
    };
    if !(0x0B..=0x0F).contains(&t) || k != &format!("\0{t:02x}") {
        return None;
    }
    match x {
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes,
        }) => Some((t, bytes)),
        _ => None,
    }
}

/// The text a [`raw_text`] value compares by: a JavaScript or symbol's
/// string bytes, the whole raw value otherwise.
pub fn raw_text_bytes(t: u8, value: &[u8]) -> &[u8] {
    match t {
        0x0D | 0x0E if value.len() >= 5 => &value[4..value.len() - 1],
        _ => value,
    }
}

/// mongod's `$type` name of a [`raw_text`] value's type.
pub fn raw_text_type_name(t: u8) -> &'static str {
    match t {
        0x0B => "regex",
        0x0C => "dbPointer",
        0x0D => "javascript",
        0x0E => "symbol",
        _ => "javascriptWithScope",
    }
}

/// The carrier of a string whose bytes are not UTF-8. Use
/// [`string_from_bytes`] when the bytes may be valid.
pub fn invalid_utf8(bytes: Vec<u8>) -> Bson {
    let mut d = Document::new();
    d.insert(
        INVALID_UTF8_KEY,
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes,
        }),
    );
    Bson::Document(d)
}

/// The string of these bytes: a `Bson::String` when they are UTF-8, its
/// carrier when they are not.
pub fn string_from_bytes(bytes: Vec<u8>) -> Bson {
    match String::from_utf8(bytes) {
        Ok(s) => Bson::String(s),
        Err(e) => invalid_utf8(e.into_bytes()),
    }
}

/// The bytes `v` carries when it is an invalid-UTF-8 string's carrier.
pub fn as_invalid_utf8(v: &Bson) -> Option<&[u8]> {
    let Bson::Document(d) = v else {
        return None;
    };
    carrier_bytes(d)
}

/// The bytes of a carrier document (see [`as_invalid_utf8`]).
pub fn carrier_bytes(d: &Document) -> Option<&[u8]> {
    if d.len() != 1 {
        return None;
    }
    match d.get(INVALID_UTF8_KEY) {
        Some(Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes,
        })) => Some(bytes),
        _ => None,
    }
}

/// The bytes of a string of either form; `None` for any other value.
pub fn str_bytes(v: &Bson) -> Option<&[u8]> {
    match v {
        Bson::String(s) => Some(s.as_bytes()),
        _ => as_invalid_utf8(v),
    }
}

/// The bytes of `a` and `b` when both are strings and at least one is not
/// UTF-8 -- the pairs a comparison that matches `Bson::String` on both sides
/// would miss.
pub fn carrier_pair<'a>(a: &'a Bson, b: &'a Bson) -> Option<(&'a [u8], &'a [u8])> {
    if matches!((a, b), (Bson::String(_), Bson::String(_))) {
        return None;
    }
    Some((str_bytes(a)?, str_bytes(b)?))
}

/// The text of a string's bytes, each sequence that is not UTF-8 replaced
/// by U+FFFD, for code that can only read text (a collation, an error
/// message).
pub fn lossy(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

/// Whether `v` is a string (BSON type 2), of either form.
pub fn is_string(v: &Bson) -> bool {
    matches!(v, Bson::String(_)) || as_invalid_utf8(v).is_some()
}

/// A string's bytes as an error MESSAGE quotes them: itself when UTF-8,
/// else each byte outside valid UTF-8 as two NULs and its two lowercase hex
/// digits. The reply writes those bytes back out (`names::message_bytes`),
/// as mongod quotes them (RM-222). Two NULs, not one, so that a client's own
/// NUL in a quoted string -- which the escape of a NAME (one NUL) would
/// collide with -- is left as it is.
pub fn message_escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 8);
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            out.push_str("\0\0");
            out.push_str(&format!("{b:02x}"));
        }
    }
    out
}

/// A field name of these bytes: itself when they are UTF-8, escaped when
/// not (each byte outside valid UTF-8 becomes NUL + two lowercase hex digits).
pub fn escape_key(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 8);
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            out.push('\0');
            out.push_str(&format!("{b:02x}"));
        }
    }
    out
}

/// The bytes a field name stands for: the name itself, or the bytes of an
/// escaped one. `None` for a name holding a NUL that is not that escape,
/// which no BSON key can encode.
pub fn key_bytes(key: &str) -> Option<Cow<'_, [u8]>> {
    if !key.contains('\0') {
        return Some(Cow::Borrowed(key.as_bytes()));
    }
    let b = key.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0 {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    // Only the canonical escape of bytes that are NOT UTF-8 stands for them.
    (escape_key(&out) == key).then_some(Cow::Owned(out))
}

/// A field name as a string VALUE (`$objectToArray`'s `k`): the name, or,
/// for an escaped one, the string of the bytes it stands for.
pub fn key_string(key: String) -> Bson {
    match key_bytes(&key) {
        Some(Cow::Owned(b)) => invalid_utf8(b),
        _ => Bson::String(key),
    }
}

fn rd_len(b: &[u8], at: usize, min: i32) -> Result<usize, Error> {
    let n = b
        .get(at..at + 4)
        .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| malformed("length runs past the end of the document"))?;
    if n < min {
        return Err(malformed(format!("length {n} is too small")));
    }
    Ok(n as usize)
}

/// One whole document, `b` exactly its bytes.
fn read_doc(b: &[u8]) -> Result<Document, Error> {
    let declared = rd_len(b, 0, 5)?;
    if declared != b.len() || b[b.len() - 1] != 0 {
        return Err(malformed("document length or terminator"));
    }
    let end = b.len() - 1;
    let mut out = Document::new();
    let mut i = 4;
    while i < end {
        let t = b[i];
        i += 1;
        let k_end = i + b[i..end]
            .iter()
            .position(|c| *c == 0)
            .ok_or_else(|| malformed("unterminated key"))?;
        let key = match std::str::from_utf8(&b[i..k_end]) {
            Ok(k) => k.to_string(),
            Err(_) => escape_key(&b[i..k_end]),
        };
        i = k_end + 1;
        let (value, used) = read_value(t, &b[i..end])?;
        i += used;
        out.insert(key, value);
    }
    Ok(out)
}

/// The value of type `t` at the start of `rest`, and how many bytes it took.
fn read_value(t: u8, rest: &[u8]) -> Result<(Bson, usize), Error> {
    let size = match t {
        0x01 | 0x09 | 0x11 | 0x12 => 8,
        0x02 | 0x0D | 0x0E => 4 + rd_len(rest, 0, 1)?,
        0x03 | 0x04 | 0x0F => rd_len(rest, 0, 5)?,
        0x05 => 5 + rd_len(rest, 0, 0)?,
        0x06 | 0x0A | 0x7F | 0xFF => 0,
        0x07 => 12,
        0x08 => 1,
        0x0B => {
            let p = rest
                .iter()
                .position(|c| *c == 0)
                .ok_or_else(|| malformed("unterminated regex"))?;
            let o = rest[p + 1..]
                .iter()
                .position(|c| *c == 0)
                .ok_or_else(|| malformed("unterminated regex options"))?;
            p + 1 + o + 1
        }
        0x0C => 4 + rd_len(rest, 0, 1)? + 12,
        0x10 => 4,
        0x13 => 16,
        other => return Err(malformed(format!("unknown element type {other:#04x}"))),
    };
    let v = rest
        .get(..size)
        .ok_or_else(|| malformed("value runs past the end of the document"))?;
    let value = match t {
        0x02 => {
            if v[size - 1] != 0 {
                return Err(malformed("string is not NUL-terminated"));
            }
            string_from_bytes(v[4..size - 1].to_vec())
        }
        0x03 => Bson::Document(read_doc(v)?),
        0x04 => Bson::Array(read_doc(v)?.into_iter().map(|(_, x)| x).collect()),
        // JavaScript, a symbol, a regex, a DBPointer or JavaScript with a
        // scope whose text is not UTF-8: kept as its raw value (RM-222).
        0x0B..=0x0F => match single(t, v) {
            Ok(value) => value,
            Err(_) => raw_text(t, v.to_vec()),
        },
        _ => single(t, v)?,
    };
    Ok((value, size))
}

/// Any other value, through the crate's own decoder.
fn single(t: u8, v: &[u8]) -> Result<Bson, Error> {
    let mut buf = Vec::with_capacity(v.len() + 7);
    buf.extend_from_slice(&((v.len() + 7) as i32).to_le_bytes());
    buf.push(t);
    buf.push(0);
    buf.extend_from_slice(v);
    buf.push(0);
    let mut d = Document::try_from(RawDocument::from_bytes(&buf)?)?;
    d.remove("")
        .ok_or_else(|| malformed("value did not decode"))
}

/// Encode `doc` onto `out`, writing an invalid-UTF-8 string's carrier and an
/// escaped field name as the bytes they stand for.
pub fn write(doc: &Document, out: &mut Vec<u8>) -> Result<(), bson::ser::Error> {
    let start = out.len();
    match doc.to_writer(&mut *out) {
        Ok(()) => Ok(()),
        // The crate refuses a key holding a NUL, which is what a carrier and
        // an escaped name are: write those here.
        Err(bson::ser::Error::InvalidCString(_)) => {
            out.truncate(start);
            write_doc(doc, out)
        }
        Err(e) => Err(e),
    }
}

/// [`write`] into a new buffer.
pub fn to_vec(doc: &Document) -> Result<Vec<u8>, bson::ser::Error> {
    let mut out = Vec::new();
    write(doc, &mut out)?;
    Ok(out)
}

/// [`write`] as a [`bson::RawDocumentBuf`].
pub fn to_raw(doc: &Document) -> Result<bson::RawDocumentBuf, bson::ser::Error> {
    let bytes = to_vec(doc)?;
    bson::RawDocumentBuf::from_bytes(bytes).map_err(|e| {
        bson::ser::Error::Io(std::sync::Arc::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        )))
    })
}

fn patch_len(out: &mut [u8], start: usize) {
    let len = (out.len() - start) as i32;
    out[start..start + 4].copy_from_slice(&len.to_le_bytes());
}

fn write_doc(doc: &Document, out: &mut Vec<u8>) -> Result<(), bson::ser::Error> {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    for (k, v) in doc {
        let key = key_bytes(k).ok_or_else(|| bson::ser::Error::InvalidCString(k.clone()))?;
        write_element(&key, v, out)?;
    }
    out.push(0);
    patch_len(out, start);
    Ok(())
}

fn write_element(key: &[u8], v: &Bson, out: &mut Vec<u8>) -> Result<(), bson::ser::Error> {
    let head = |t: u8, out: &mut Vec<u8>| {
        out.push(t);
        out.extend_from_slice(key);
        out.push(0);
    };
    if let Some((t, raw)) = as_raw_text(v) {
        head(t, out);
        out.extend_from_slice(raw);
        return Ok(());
    }
    if let Some(bytes) = as_invalid_utf8(v) {
        head(0x02, out);
        out.extend_from_slice(&((bytes.len() + 1) as i32).to_le_bytes());
        out.extend_from_slice(bytes);
        out.push(0);
        return Ok(());
    }
    match v {
        Bson::Document(d) => {
            head(0x03, out);
            write_doc(d, out)
        }
        Bson::Array(a) => {
            head(0x04, out);
            let start = out.len();
            out.extend_from_slice(&[0; 4]);
            for (i, x) in a.iter().enumerate() {
                write_element(i.to_string().as_bytes(), x, out)?;
            }
            out.push(0);
            patch_len(out, start);
            Ok(())
        }
        Bson::JavaScriptCodeWithScope(c) => {
            head(0x0F, out);
            let start = out.len();
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&((c.code.len() + 1) as i32).to_le_bytes());
            out.extend_from_slice(c.code.as_bytes());
            out.push(0);
            write_doc(&c.scope, out)?;
            patch_len(out, start);
            Ok(())
        }
        other => {
            // Every other value through the crate's encoder, under an empty
            // key: `[len][type]["" NUL][value][NUL]`.
            let mut one = Document::new();
            one.insert("", other.clone());
            let bytes = bson::to_vec(&one)?;
            head(bytes[4], out);
            out.extend_from_slice(&bytes[6..bytes.len() - 1]);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::from_bytes;
    use bson::{doc, Bson, Document};

    fn bytes(d: &Document) -> Vec<u8> {
        let mut out = Vec::new();
        d.to_writer(&mut out).unwrap();
        out
    }

    /// Every extended-JSON wrapper, as a sub-document and inside an array.
    #[test]
    fn a_sub_document_that_looks_like_extended_json_stays_a_document() {
        let wrappers = [
            doc! { "$numberLong": "1" },
            doc! { "$numberInt": "1" },
            doc! { "$numberDouble": "1.5" },
            doc! { "$numberDecimal": "1.5" },
            doc! { "$oid": "0123456789abcdef01234567" },
            doc! { "$date": 5_i64 },
            doc! { "$date": { "$numberLong": "5" } },
            doc! { "$symbol": "s" },
            doc! { "$code": "f()" },
            doc! { "$timestamp": { "t": 1, "i": 2 } },
            doc! { "$regularExpression": { "pattern": "a", "options": "i" } },
            doc! { "$binary": { "base64": "AA==", "subType": "00" } },
            doc! { "$uuid": "00000000-0000-0000-0000-000000000000" },
            doc! { "$minKey": 1 },
            doc! { "$maxKey": 1 },
            doc! { "$undefined": true },
            doc! { "$dbPointer": { "$ref": "c", "$id": { "$oid": "0123456789abcdef01234567" } } },
        ];
        for wrapper in wrappers {
            let stored = doc! { "_id": 1, "a": wrapper.clone(), "l": [wrapper.clone()] };
            let back = from_bytes(&bytes(&stored)).unwrap();
            assert_eq!(
                back.get("a"),
                Some(&Bson::Document(wrapper.clone())),
                "{wrapper}"
            );
            assert_eq!(bytes(&back), bytes(&stored), "{wrapper}");
        }
    }

    #[test]
    fn typed_values_round_trip() {
        let stored = doc! {
            "_id": bson::oid::ObjectId::new(),
            "i": 1_i32,
            "l": 1_i64,
            "d": 1.5,
            "s": "x",
            "n": Bson::Null,
            "t": bson::Timestamp { time: 1, increment: 2 },
            "when": bson::DateTime::from_millis(5),
            "o": { "x": [1, { "y": true }] },
        };
        assert_eq!(from_bytes(&bytes(&stored)).unwrap(), stored);
    }

    #[test]
    fn trailing_bytes_are_ignored_and_a_short_buffer_is_refused() {
        let mut b = bytes(&doc! { "a": 1 });
        b.extend_from_slice(b"junk");
        assert_eq!(from_bytes(&b).unwrap(), doc! { "a": 1 });
        assert!(from_bytes(&b[..5]).is_err());
        assert!(from_bytes(&[]).is_err());
    }

    /// A document holding `elements` (each `[type][key NUL][value]`).
    fn raw_doc(elements: &[u8]) -> Vec<u8> {
        let mut out = ((elements.len() + 5) as i32).to_le_bytes().to_vec();
        out.extend_from_slice(elements);
        out.push(0);
        out
    }

    fn raw_string(key: &[u8], value: &[u8]) -> Vec<u8> {
        let mut out = vec![0x02];
        out.extend_from_slice(key);
        out.push(0);
        out.extend_from_slice(&((value.len() + 1) as i32).to_le_bytes());
        out.extend_from_slice(value);
        out.push(0);
        out
    }

    /// RM-222: a string whose bytes are not UTF-8 decodes to its carrier and
    /// encodes back to the same bytes, at the top, nested and in an array.
    #[test]
    fn a_string_that_is_not_utf8_round_trips_byte_for_byte() {
        let mut elements = vec![0x10, b'_', b'i', b'd', 0, 1, 0, 0, 0];
        elements.extend(raw_string(b"s", b"\xff\xfe"));
        elements.extend(raw_string(b"ok", "é".as_bytes()));
        let inner = raw_doc(&raw_string(b"t", b"a\xc3"));
        elements.push(0x03);
        elements.extend_from_slice(b"d\0");
        elements.extend(&inner);
        let arr = raw_doc(&raw_string(b"0", b"\xed\xa0\x80"));
        elements.push(0x04);
        elements.extend_from_slice(b"l\0");
        elements.extend(&arr);
        let wire = raw_doc(&elements);

        let doc = from_bytes(&wire).unwrap();
        assert_eq!(
            super::str_bytes(doc.get("s").unwrap()),
            Some(&b"\xff\xfe"[..])
        );
        assert_eq!(doc.get_str("ok").unwrap(), "é");
        assert!(super::is_string(
            doc.get_document("d").unwrap().get("t").unwrap()
        ));
        assert_eq!(super::to_vec(&doc).unwrap(), wire);
        // The crate's own encoder refuses the carrier rather than writing
        // something else.
        assert!(doc.to_writer(Vec::new()).is_err());
    }

    /// A field name that is not UTF-8 is kept escaped, and written back as
    /// its own bytes.
    #[test]
    fn a_field_name_that_is_not_utf8_round_trips() {
        let mut elements = vec![0x10, b'k', 0xff, 0, 1, 0, 0, 0];
        elements.extend(raw_string(b"a\xc3.b", b"x"));
        let wire = raw_doc(&elements);
        let doc = from_bytes(&wire).unwrap();
        assert_eq!(doc.get_i32("k\u{0}ff").unwrap(), 1);
        assert_eq!(doc.get_str("a\u{0}c3.b").unwrap(), "x");
        assert_eq!(super::to_vec(&doc).unwrap(), wire);
    }

    #[test]
    fn only_the_canonical_escape_of_invalid_bytes_is_a_name() {
        assert_eq!(super::key_bytes("k\u{0}ff").as_deref(), Some(&b"k\xff"[..]));
        // Escaping bytes that ARE UTF-8, an upper-case digit, a short escape
        // and a bare NUL stand for nothing.
        for bad in ["\u{0}61", "k\u{0}FF", "k\u{0}f", "a\u{0}"] {
            assert_eq!(super::key_bytes(bad), None, "{bad:?}");
            let d = doc! { bad: 1 };
            assert!(super::to_vec(&d).is_err(), "{bad:?}");
        }
    }

    /// RM-222: such a string is a string to the comparator, the index key
    /// encoder, the matcher and `$type` -- ordered by its bytes among the
    /// other strings, as mongod 8.2.11 orders them.
    #[test]
    fn a_string_that_is_not_utf8_is_a_string_everywhere() {
        use crate::{order, sortkey};
        let ff = super::invalid_utf8(b"\xff\xfe".to_vec());
        let mix = super::invalid_utf8(b"a\xffb".to_vec());
        let (a, z, e) = (Bson::from("a"), Bson::from("z"), Bson::from("é"));
        // mongod 8.2.11 sorts a < a\xffb < z < é < \xff\xfe, before objects.
        let mut values = vec![ff.clone(), e.clone(), z.clone(), mix.clone(), a.clone()];
        values.sort_by(order::cmp);
        assert_eq!(
            values,
            vec![a.clone(), mix.clone(), z.clone(), e.clone(), ff.clone()]
        );
        assert_eq!(
            order::cmp(&ff, &Bson::Document(doc! {})),
            std::cmp::Ordering::Less
        );
        let key = |v: &Bson| sortkey::encode_value(v, None).unwrap();
        assert!(key(&a) < key(&mix) && key(&mix) < key(&z) && key(&e) < key(&ff));
        assert_eq!(key(&ff)[0], key(&a)[0], "the string rank");
        assert_eq!(sortkey::encode_id_key(&ff).unwrap(), key(&ff));
        let doc = doc! { "s": mix.clone() };
        let no_vars = Document::new();
        for (filter, hit) in [
            (doc! { "s": mix.clone() }, true),
            (doc! { "s": ff.clone() }, false),
            (doc! { "s": { "$gt": "a", "$lt": "z" } }, true),
            (doc! { "s": { "$type": "string" } }, true),
            (doc! { "s": { "$type": "object" } }, false),
            (doc! { "s": { "$regex": "a" } }, false),
        ] {
            assert_eq!(
                crate::query::matches(&doc, &filter, &no_vars, None).unwrap(),
                hit,
                "{filter}"
            );
        }
    }

    /// RM-222: JavaScript, a symbol, a regex, a DBPointer and JavaScript
    /// with a scope whose text is not UTF-8 keep their raw value, and report
    /// their own type.
    #[test]
    fn the_other_text_types_keep_their_bytes() {
        let values: [(u8, &[u8]); 5] = [
            (0x0D, b"\x02\x00\x00\x00\xff\x00"),
            (0x0E, b"\x02\x00\x00\x00\xff\x00"),
            (0x0B, b"\xff\x00i\x00"),
            (
                0x0C,
                b"\x04\x00\x00\x00d.\xff\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            ),
            (
                0x0F,
                b"\x0f\x00\x00\x00\x02\x00\x00\x00\xff\x00\x05\x00\x00\x00\x00",
            ),
        ];
        for (t, value) in values {
            let mut element = vec![t, b'v', 0];
            element.extend_from_slice(value);
            let wire = raw_doc(&element);
            let doc = from_bytes(&wire).unwrap();
            assert_eq!(
                super::as_raw_text(doc.get("v").unwrap()),
                Some((t, value)),
                "{t:#x}"
            );
            assert_eq!(super::to_vec(&doc).unwrap(), wire, "{t:#x}");
            assert_eq!(
                crate::query::bson_type_name(doc.get("v").unwrap()),
                super::raw_text_type_name(t)
            );
        }
    }

    #[test]
    fn valid_documents_encode_as_the_crate_does() {
        let d = doc! { "a": 1, "b": { "c": [1, "x", { "d": Bson::Null }] }, "e": "é" };
        assert_eq!(super::to_vec(&d).unwrap(), bytes(&d));
        // A carrier beside every other type takes the slow path and still
        // writes those types as the crate does.
        let mut with = d.clone();
        with.insert("z", super::invalid_utf8(vec![0x80]));
        let enc = super::to_vec(&with).unwrap();
        let back = from_bytes(&enc).unwrap();
        assert_eq!(back, with);
        assert_eq!(
            &enc[4..enc.len() - 1][..bytes(&d).len() - 5],
            &bytes(&d)[4..bytes(&d).len() - 1]
        );
    }
}
