//! Byte-sortable BSON value encoding — Rust port of `secantus.sortkey`.
//!
//! Produces bytes whose lexicographic order matches MongoDB's BSON cross-type
//! sort order, so index entries can live in WiredTiger's sorted B-tree. This
//! is the first leaf engine ported under the Python -> Rust rewrite
//! (tasks/rust-rewrite-plan.md Phase 1); it is the byte-exact counterpart of
//! the pure-Python `encode_value` and is pinned against it by a parity test
//! (`tests/test_rust_sortkey_parity.py`) and the cargo unit tests below.
//!
//! Collation-normalised string keys and the single-byte-exponent overflow
//! fallback are reproduced; `numericOrdering` index encoding is intentionally
//! out of scope (matches Python — those queries fall back to COLLSCAN).

use bson::{Bson, Document};

use crate::collation::{self, Collation};

// Type ranks — must match secantus.sortkey.
pub const RANK_MINKEY: u8 = 1;
const RANK_NULL: u8 = 2;
const RANK_NUMBER: u8 = 3;
const RANK_STRING: u8 = 4;
pub const RANK_DOCUMENT: u8 = 5;
pub const RANK_ARRAY: u8 = 6;
const RANK_BINDATA: u8 = 7;
const RANK_OBJECTID: u8 = 8;
const RANK_BOOL: u8 = 9;
const RANK_DATE: u8 = 10;
const RANK_TIMESTAMP: u8 = 11;
const RANK_REGEX: u8 = 12;
/// JavaScript is its OWN type to mongod, which sorts it between Regex and
/// MaxKey (probed 8.2.11, 2026-09-01). It used to share `RANK_STRING`, because
/// `bson.Code` subclasses `str` in pymongo and the Python encoder this mirrors
/// caught one with an `isinstance(value, str)` test. Must stay in step with
/// `order::type_rank`: this writes the rank byte persisted index entries are
/// sorted by, that drives the in-memory sort, and moving one alone makes an
/// index change the sort answer.
const RANK_JAVASCRIPT: u8 = 13;
/// JavaScript-with-scope is a type of its own too, after plain JavaScript:
/// mongod sorts every `Code` before every `Code` with a scope, then by the
/// code, then by the scope (probed 8.2.11, 2026-10-21; RM-233). Entry format 5.
const RANK_JAVASCRIPT_SCOPE: u8 = 14;
const RANK_MAXKEY: u8 = 15;

const NUM_NAN: u8 = 0x00;
const NUM_NEG_INF: u8 = 0x20;
/// A finite negative. Number-key format 1 wrote 0x40, so a key of either
/// format says which it is, and no key of one format equals a key of the
/// other: a store is re-keyed in place without a new key ever landing on
/// an old one.
const NUM_NEG: u8 = 0x41;
const NUM_ZERO: u8 = 0x80;
/// A finite positive (format 1: 0xC0; see [`NUM_NEG`]).
const NUM_POS: u8 = 0xC1;
const NUM_POS_INF: u8 = 0xFF;

/// Compound-key separator (also the escape sentinel). Matches
/// `secantus.sortkey.COMPOUND_SEP`.
pub const COMPOUND_SEP: &[u8] = b"\x00\x00";

/// The byte after [`RANK_MINKEY`] that makes `undefined`'s key: the value
/// mongod keys an empty array by, between MinKey and null (entry format 6).
/// A MinKey key is the rank alone, so no MinKey key starts with this one.
pub const UNDEFINED_TAIL: u8 = 0x80;

/// What ends a DESCENDING field's key (entry format 6): the field's
/// ascending key inverted, then these two bytes. Inverting alone does not
/// reverse a prefix (inverted, `"a"` still sorts before `"ab"`); with the
/// terminator no descending key starts another, because an ascending key
/// never holds `00 00` where another value's key could end.
pub const DESC_END: &[u8] = b"\xff\xff";

/// Error type for values the encoder doesn't handle (Python's `encode_value`
/// doesn't handle them either — Symbol/Code/etc. route to its document branch
/// and raise). The Python shim treats this as "fall back to the pure-Python
/// path", so we never silently diverge.
#[derive(Debug)]
pub struct UnsupportedValue(pub String);

impl std::fmt::Display for UnsupportedValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sortkey: unsupported BSON value: {}", self.0)
    }
}

/// 0x00 -> 0x00 0xff, order-preserving so 0x00 0x00 is an unambiguous separator.
fn escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        out.push(b);
        if b == 0 {
            out.push(0xff);
        }
    }
    out
}

/// Decimal as value = sign * (digits) * 10^exp, no leading/trailing zero
/// digits. `None` for zero / non-finite.
struct DecimalParts {
    sign: i32,
    digits: Vec<u8>,
    exp: i64,
}

fn normalize(digits: &mut Vec<u8>, exp: &mut i64) {
    while digits.first() == Some(&0) {
        digits.remove(0);
    }
    while digits.last() == Some(&0) {
        digits.pop();
        *exp += 1;
    }
}

fn parse_decimal_str(s: &str) -> Option<DecimalParts> {
    let s = s.trim();
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let (mantissa, exp_extra) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().ok()?),
        None => (rest, 0),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    let mut digits: Vec<u8> = Vec::new();
    for c in int_part.chars().chain(frac_part.chars()) {
        digits.push(c.to_digit(10)? as u8);
    }
    let mut exp = exp_extra - frac_part.len() as i64;
    normalize(&mut digits, &mut exp);
    if digits.is_empty() {
        return None; // zero
    }
    Some(DecimalParts { sign, digits, exp })
}

fn parts_from_int(mut n: i128) -> Option<DecimalParts> {
    if n == 0 {
        return None;
    }
    let sign = if n < 0 { -1 } else { 1 };
    if n < 0 {
        n = -n;
    }
    let mut digits: Vec<u8> = n.to_string().bytes().map(|b| b - b'0').collect();
    let mut exp: i64 = 0;
    normalize(&mut digits, &mut exp);
    Some(DecimalParts { sign, digits, exp })
}

/// The number-key format this encoder writes. Stores record it (the Rust
/// server's `numberKeys` marker, the Python server's `_NUMBER_KEY_FORMAT`) and
/// rebuild the keys of a store written by another.
///
/// * 1 (no marker) -- a double keyed by its SHORTEST decimal (`format!("{d}")`,
///   Python's `Decimal(repr(d))`), so the double `0.1` and `Decimal128("0.1")`
///   shared a key, and a one-byte decimal exponent, so every number whose
///   exponent fell outside -128..=127 shared ONE key per sign, which sorted
///   above every other positive (below every other negative).
/// * 2 -- a double keyed by its EXACT binary value, and a two-byte exponent,
///   enough for every double and every Decimal128. The sign markers moved
///   (0x40 -> 0x41, 0xC0 -> 0xC1), so a finite number's key names its format.
pub const NUMBER_KEY_FORMAT: i32 = 2;

/// Whether `key`, an encoded value, is a finite number keyed by a number-key
/// format before 2 (its marker is the old one). NaN, the infinities and
/// zero are keyed alike by both formats.
pub fn is_format1_number_key(key: &[u8]) -> bool {
    matches!(key, [RANK_NUMBER, 0x40 | 0xC0, ..])
}

/// `[sign][exponent, 2 bytes][digit pairs][terminator]`. The exponent is the
/// scientific one (`d.ddd * 10^e`) biased by 0x8000, big-endian; the digits
/// are packed two to a byte as `pair + 1` (1..=100), so a shorter mantissa
/// sorts first and the terminator 0x00 sits below every pair. A negative
/// number inverts the exponent and maps each pair to `0x64 - (pair + 1)`,
/// with terminator 0xff, so a larger magnitude sorts first.
fn encode_number_from_parts(p: &DecimalParts) -> Vec<u8> {
    let sci_exp = p.exp + p.digits.len() as i64 - 1;
    // Decimal128 reaches about +-6176, a double +-324: well inside i16.
    let biased = (0x8000 + sci_exp).clamp(0, 0xFFFF) as u16;
    let e = if p.sign < 0 { 0xFFFF - biased } else { biased };
    let mut digits = p.digits.clone();
    if digits.len() % 2 == 1 {
        digits.push(0);
    }
    let mut out = Vec::with_capacity(4 + digits.len() / 2);
    out.push(if p.sign > 0 { NUM_POS } else { NUM_NEG });
    out.extend_from_slice(&e.to_be_bytes());
    for pair in digits.as_chunks::<2>().0 {
        let b = pair[0] * 10 + pair[1] + 1;
        out.push(if p.sign < 0 { 0x64 - b } else { b });
    }
    out.push(if p.sign > 0 { 0x00 } else { 0xff });
    out
}

thread_local! {
    static FORMAT_1: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with this thread's number keys written in number-key format 1, as
/// a build before 2026-10-15 wrote them. For tests that build a store as an
/// earlier version left it, to check it is brought up to date; nothing else
/// may call it.
#[doc(hidden)]
pub fn with_number_key_format_1<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORMAT_1.with(|c| c.set(self.0));
        }
    }
    let _r = Restore(FORMAT_1.with(|c| c.replace(true)));
    f()
}

/// A finite nonzero number's key in number-key format 1: the shortest
/// decimal of a double, a one-byte exponent, one key per sign beyond it.
fn encode_number_format_1(b: &Bson) -> Option<Vec<u8>> {
    let text = match b {
        Bson::Double(d) if d.is_finite() && *d != 0.0 => format!("{d}"),
        Bson::Int32(n) if *n != 0 => n.to_string(),
        Bson::Int64(n) if *n != 0 => n.to_string(),
        Bson::Decimal128(d) => {
            let t = d.to_string();
            let low = t.to_lowercase();
            if low.contains("nan") || low.contains("inf") {
                return None;
            }
            t
        }
        _ => return None,
    };
    let p = parse_decimal_str(&text)?;
    let sci_exp = p.exp + p.digits.len() as i64 - 1;
    let mut bias_e = 128 + sci_exp;
    if !(0..=255).contains(&bias_e) {
        return Some(if p.sign > 0 {
            vec![0xC0, 0xFF, 0xFF]
        } else {
            vec![0x40, 0x00, 0x00]
        });
    }
    if p.sign < 0 {
        bias_e = 0xFF - bias_e;
    }
    let mut digits = p.digits.clone();
    if digits.len() % 2 == 1 {
        digits.push(0);
    }
    let mut out = vec![if p.sign > 0 { 0xC0 } else { 0x40 }, bias_e as u8];
    for pair in digits.as_chunks::<2>().0 {
        let b = pair[0] * 10 + pair[1] + 1;
        out.push(if p.sign < 0 { 0x64 - b } else { b });
    }
    out.push(if p.sign > 0 { 0x00 } else { 0xff });
    Some(out)
}

fn encode_number(b: &Bson) -> Vec<u8> {
    if FORMAT_1.with(|c| c.get()) {
        if let Some(key) = encode_number_format_1(b) {
            return key;
        }
    }
    let parts = match b {
        Bson::Int32(n) => parts_from_int(*n as i128),
        Bson::Int64(n) => parts_from_int(*n as i128),
        Bson::Double(d) => {
            if d.is_nan() {
                return vec![NUM_NAN];
            }
            if d.is_infinite() {
                return vec![if *d > 0.0 { NUM_POS_INF } else { NUM_NEG_INF }];
            }
            if *d == 0.0 {
                None
            } else {
                // The double's EXACT value, as mongod compares it: `0.1` is
                // 0.1000000000000000055511151231257827021181583404541015625.
                let (sign, digits, exp) = crate::numeric::exact_f64_parts(*d);
                Some(DecimalParts {
                    sign: sign as i32,
                    digits,
                    exp,
                })
            }
        }
        Bson::Decimal128(d) => {
            let s = d.to_string();
            let low = s.to_lowercase();
            if low.contains("nan") {
                return vec![NUM_NAN];
            }
            if low.contains("inf") {
                return vec![if s.starts_with('-') {
                    NUM_NEG_INF
                } else {
                    NUM_POS_INF
                }];
            }
            parse_decimal_str(&s)
        }
        _ => unreachable!("encode_number on non-number"),
    };
    match parts {
        None => vec![NUM_ZERO],
        Some(p) => encode_number_from_parts(&p),
    }
}

fn signed_int64_sortable(n: i64) -> [u8; 8] {
    ((n as u64) ^ 0x8000_0000_0000_0000).to_be_bytes()
}

/// Formats 1-3's array encoding (a BSON document with positional keys) -- kept
/// only for [`encode_id_key`].
fn legacy_array_bytes(arr: &[Bson]) -> Result<Vec<u8>, UnsupportedValue> {
    let mut doc = Document::new();
    for (i, v) in arr.iter().enumerate() {
        doc.insert(i.to_string(), v.clone());
    }
    legacy_doc_bytes(&doc)
}

/// Formats 1-3's document encoding (the escaped raw BSON) -- kept only for
/// [`encode_id_key`].
fn legacy_doc_bytes(doc: &Document) -> Result<Vec<u8>, UnsupportedValue> {
    let mut buf = Vec::new();
    crate::bsondoc::write(doc, &mut buf)
        .map_err(|e| UnsupportedValue(format!("doc encode failed: {e}")))?;
    Ok(escape(&buf))
}

/// The end of a document's or array's elements. Every element starts with its
/// value's type rank, and every rank is >= 1, so a value that is a strict
/// prefix of another -- fewer elements, the rest equal -- sorts first.
const ELEMENTS_END: u8 = 0x00;

/// One element's value inside a document or array: its type rank, then the rest
/// of its key escaped and terminated, so the bytes after it cannot change the
/// comparison (an escaped value never contains `00 00`). Byte-exact counterpart
/// of Python's `sortkey._element_value`.
fn element_value(v: &Bson, coll: Option<&Collation>) -> Result<(u8, Vec<u8>), UnsupportedValue> {
    let key = encode_value(v, coll)?;
    let mut rest = escape(&key[1..]);
    rest.extend_from_slice(COMPOUND_SEP);
    Ok((key[0], rest))
}

/// A document, byte-ordered the way mongod compares documents: element by
/// element, each by the value's canonical TYPE, then the field NAME, then the
/// value; the document that runs out first is the smaller. Entry format 4.
/// Formats 1-3 used the raw BSON, whose leading length made byte order SIZE
/// order -- `{a: 2, b: [3]}` sorted above `{a: 5}`, and an index range bounded
/// by a document scanned the wrong stretch. A nested string takes the index's
/// collation, as mongod's comparison does. Mirrors `sortkey._encode_doc`.
fn encode_doc(d: &Document, coll: Option<&Collation>) -> Result<Vec<u8>, UnsupportedValue> {
    let mut out = Vec::new();
    for (name, v) in d {
        let (rank, rest) = element_value(v, coll)?;
        out.push(rank);
        out.extend(escape(name.as_bytes()));
        out.extend_from_slice(COMPOUND_SEP);
        out.extend(rest);
    }
    out.push(ELEMENTS_END);
    Ok(out)
}

/// An array: its elements in order, as [`encode_doc`] without the names -- two
/// arrays' names are the same positions, so they never decide.
fn encode_array(a: &[Bson], coll: Option<&Collation>) -> Result<Vec<u8>, UnsupportedValue> {
    let mut out = Vec::new();
    for v in a {
        let (rank, rest) = element_value(v, coll)?;
        out.push(rank);
        out.extend(rest);
    }
    out.push(ELEMENTS_END);
    Ok(out)
}

/// The format of the `_id` keys a store holds (its oplog-meta row `idKeys`;
/// none recorded is 1). An `_id` key is stored in every document row and keys
/// the `_id` index, so a store is re-keyed when it opens with an older one.
///
/// * 1 -- [`encode_value`] with formats 1-3's document / array encoding (the
///   raw BSON, whose leading length made byte order SIZE order) and entry
///   format 4's BinData (no subtype), JavaScript-with-scope (JavaScript's
///   rank, code only) and MaxKey (rank 14) keys. The `_id` index was not in
///   mongod's order for those types.
/// * 2 -- [`encode_value`] in mongod's order for every type (RM-080,
///   2026-10-27): a cursor walking the `_id` index returns what mongod's
///   walk returns.
pub const ID_KEY_FORMAT: i32 = 2;

thread_local! {
    static ID_FORMAT_1: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with this thread's `_id` keys written in `_id` key format 1, as a
/// build before 2026-10-27 wrote them. For tests that build a store as an
/// earlier version left it, to check it is brought up to date; nothing else
/// may call it.
#[doc(hidden)]
pub fn with_id_key_format_1<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            ID_FORMAT_1.with(|c| c.set(self.0));
        }
    }
    let _r = Restore(ID_FORMAT_1.with(|c| c.replace(true)));
    f()
}

/// Whether `key`, a stored `_id` key, may be one `_id` key format 1 wrote
/// differently from format 2: a document, BinData, JavaScript (with a scope,
/// format 1's) or MaxKey (format 1's rank 14).
pub fn may_be_format1_id_key(key: &[u8]) -> bool {
    matches!(
        key.first(),
        Some(&RANK_DOCUMENT | &RANK_ARRAY | &RANK_BINDATA | &RANK_JAVASCRIPT | &RANK_JAVASCRIPT_SCOPE)
    )
}

/// The `_id` key (`_id` key format 2): [`encode_value`] in mongod's order,
/// BinData by length, subtype and bytes whichever server writes. `undefined`
/// is refused, as it always was.
pub fn encode_id_key(v: &Bson) -> Result<Vec<u8>, UnsupportedValue> {
    if ID_FORMAT_1.with(|c| c.get()) {
        return encode_id_key_format_1(v);
    }
    if matches!(v, Bson::Undefined) {
        return Err(UnsupportedValue("undefined".into()));
    }
    crate::order::with_bindata_order(crate::order::BinDataOrder::Mongod, || encode_value(v, None))
}

/// The `_id` key `_id` key format 1 wrote: [`encode_value`] with formats
/// 1-3's document / array encoding and entry format 4's BinData,
/// JavaScript-with-scope and MaxKey keys (`order::BinDataOrder::Format4`).
pub fn encode_id_key_format_1(v: &Bson) -> Result<Vec<u8>, UnsupportedValue> {
    crate::order::with_bindata_order(crate::order::BinDataOrder::Format4, || {
        encode_id_key_inner(v)
    })
}

fn encode_id_key_inner(v: &Bson) -> Result<Vec<u8>, UnsupportedValue> {
    match v {
        // A string that is not UTF-8 keys as the string it is (RM-222).
        Bson::Document(_) if crate::bsondoc::as_invalid_utf8(v).is_some() => encode_value(v, None),
        Bson::Document(d) => {
            let mut out = vec![RANK_DOCUMENT];
            out.extend(legacy_doc_bytes(d)?);
            Ok(out)
        }
        Bson::Array(a) => {
            let mut out = vec![RANK_ARRAY];
            out.extend(legacy_array_bytes(a)?);
            Ok(out)
        }
        // `undefined` has an index key since entry format 6; an `_id` key
        // keeps refusing it, as it did before.
        Bson::Undefined => Err(UnsupportedValue("undefined".into())),
        other => encode_value(other, None),
    }
}

/// MaxKey's rank before entry format 5 gave JavaScript-with-scope its own;
/// `_id` keys and claims keep it (`order::BinDataOrder::Format4`).
const FORMAT_4_RANK_MAXKEY: u8 = 14;

/// Byte-sortable encoding of a single BSON value. Byte-exact counterpart of
/// `secantus.sortkey.encode_value`. `coll` is the index's collation (or `None`);
/// it applies to every string, nested ones included (entry format 4).
pub fn encode_value(v: &Bson, coll: Option<&Collation>) -> Result<Vec<u8>, UnsupportedValue> {
    let mut out = Vec::new();
    // A string whose bytes are not UTF-8 (RM-222) is a string: its bytes,
    // escaped, so it orders among the other strings byte by byte, as mongod
    // orders them. Under a collation it folds as the text with each invalid
    // sequence replaced, which may equal another string's key; the exact
    // match pass rechecks every index candidate.
    // A text-carrying value whose text is not UTF-8: its type's rank, then
    // its text (RM-222). A DBPointer has no key, as any other has none.
    if let Some((t, raw)) = crate::bsondoc::as_raw_text(v) {
        out.push(match t {
            0x0B => RANK_REGEX,
            0x0D => RANK_JAVASCRIPT,
            0x0E => RANK_STRING,
            0x0F => RANK_JAVASCRIPT_SCOPE,
            _ => return Err(UnsupportedValue("a DBPointer".into())),
        });
        out.extend(escape(crate::bsondoc::raw_text_bytes(t, raw)));
        return Ok(out);
    }
    if let Some(bytes) = crate::bsondoc::as_invalid_utf8(v) {
        out.push(RANK_STRING);
        match coll {
            Some(c) => out.extend(escape(
                &collation::normalize_index_bytes(&String::from_utf8_lossy(bytes), c)
                    .ok_or(UnsupportedValue("collation cannot be reproduced".into()))?,
            )),
            None => out.extend(escape(bytes)),
        }
        return Ok(out);
    }
    match v {
        Bson::MinKey => out.push(RANK_MINKEY),
        // mongod's key for an empty array (and for `undefined` itself), which
        // it orders after MinKey and before null (entry format 6).
        Bson::Undefined => out.extend_from_slice(&[RANK_MINKEY, UNDEFINED_TAIL]),
        Bson::Null => out.push(RANK_NULL),
        Bson::MaxKey => out.push(
            if crate::order::bindata_order() == crate::order::BinDataOrder::Format4 {
                FORMAT_4_RANK_MAXKEY
            } else {
                RANK_MAXKEY
            },
        ),
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
            out.push(RANK_NUMBER);
            out.extend(encode_number(v));
        }
        Bson::String(s) => {
            out.push(RANK_STRING);
            // The single-level INDEX fold, byte-identical to Python's
            // `_encode_string` -> `normalize_for_index_bytes`. Ordering under a
            // collation is a DIFFERENT key -- see `encode_sort_value` below --
            // because the fold answers "equal under this collation?" and cannot
            // order two strings differing only in an accent. Keeping both roles
            // in this one function is what broke `test_collation_encoding_parity`:
            // `encode_value(collation=)` means index bytes in Python and had
            // come to mean the sort key here.
            match coll {
                Some(c) => out
                    .extend(escape(&collation::normalize_index_bytes(s, c).ok_or(
                        UnsupportedValue("collation cannot be reproduced".into()),
                    )?)),
                None => out.extend(escape(s.as_bytes())),
            }
        }
        // A collation has nothing to say about JavaScript, so it is deliberately
        // not applied here (mongod compares code text directly). A with-scope
        // Code orders by its code text, then by its scope as a document.
        Bson::JavaScriptCode(s) => {
            out.push(RANK_JAVASCRIPT);
            out.extend(escape(s.as_bytes()));
        }
        Bson::JavaScriptCodeWithScope(c)
            if crate::order::bindata_order() == crate::order::BinDataOrder::Format4 =>
        {
            out.push(RANK_JAVASCRIPT);
            out.extend(escape(c.code.as_bytes()));
        }
        Bson::JavaScriptCodeWithScope(c) => {
            out.push(RANK_JAVASCRIPT_SCOPE);
            out.extend(escape(c.code.as_bytes()));
            out.extend_from_slice(COMPOUND_SEP);
            out.extend(encode_doc(&c.scope, None)?);
        }
        Bson::Document(d) => {
            out.push(RANK_DOCUMENT);
            out.extend(encode_doc(d, coll)?);
        }
        Bson::Array(a) => {
            out.push(RANK_ARRAY);
            out.extend(encode_array(a, coll)?);
        }
        // In this thread's BinData order (`order::BinDataOrder`): mongod's
        // length, then subtype, then bytes (the subtype byte is new in entry
        // format 5, RM-232), or PostgreSQL's bytes, a shorter prefix first --
        // an escaped string's own order. Storage encodes an index's keys under
        // the order the index records.
        Bson::Binary(b) => {
            out.push(RANK_BINDATA);
            match crate::order::bindata_order() {
                crate::order::BinDataOrder::Mongod => {
                    out.extend_from_slice(&(b.bytes.len() as u32).to_be_bytes());
                    out.push(u8::from(b.subtype));
                    out.extend(escape(&b.bytes));
                }
                crate::order::BinDataOrder::Format4 => {
                    out.extend_from_slice(&(b.bytes.len() as u32).to_be_bytes());
                    out.extend(escape(&b.bytes));
                }
                crate::order::BinDataOrder::Bytes => {
                    out.extend(escape(&b.bytes));
                    out.extend_from_slice(COMPOUND_SEP);
                    out.push(u8::from(b.subtype));
                }
            }
        }
        Bson::ObjectId(oid) => {
            out.push(RANK_OBJECTID);
            out.extend_from_slice(&oid.bytes());
        }
        Bson::Boolean(b) => {
            out.push(RANK_BOOL);
            out.push(if *b { 1 } else { 0 });
        }
        Bson::DateTime(dt) => {
            out.push(RANK_DATE);
            out.extend_from_slice(&signed_int64_sortable(dt.timestamp_millis()));
        }
        Bson::Timestamp(ts) => {
            out.push(RANK_TIMESTAMP);
            out.extend_from_slice(&ts.time.to_be_bytes());
            out.extend_from_slice(&ts.increment.to_be_bytes());
        }
        Bson::RegularExpression(r) => {
            out.push(RANK_REGEX);
            out.extend(escape(r.pattern.as_bytes()));
            out.extend_from_slice(COMPOUND_SEP);
            // Normalised, so these index bytes order regexes exactly as
            // `order::cmp` does -- two functions disagreeing about a sort key is
            // how an index comes to change the sort answer. No stored bytes
            // move: a driver's BSON encoder already emits options
            // alphabetically (pymongo renders `/a/mi` as `im` on the wire), so
            // this only corrects a hand-built value no encoder produces, and
            // needs no `entryFormat` bump.
            let (_, options) = crate::regexutil::regex_sort_key(r);
            out.extend(escape(options.as_bytes()));
        }
        other => return Err(UnsupportedValue(format!("{other:?}"))),
    }
    Ok(out)
}

/// Bitwise-NOT every byte — order-reversing, for descending index entries.
pub fn invert_bytes(b: &[u8]) -> Vec<u8> {
    b.iter().map(|x| x ^ 0xFF).collect()
}

/// `encode_value`, bytes inverted when `direction == -1`.
/// **Only for physical B-tree placement — never for comparison.**
///
/// Inverting the bytes gives a descending column the right ORDER inside the
/// index, where the storage engine sorts by raw bytes and there is nowhere to
/// put a direction. It is not a general descending comparator, because
/// **inversion does not reverse a PREFIX relationship**: `""` encodes to a
/// strict prefix of `"a"`'s key, and a shorter byte string sorts first both
/// before and after inversion. Sorting documents by these keys therefore put
/// every prefix chain in ASCENDING order inside a descending result (measured
/// against mongod 8.2.11, 2026-09-06 — `["", "a", "ab", "abc", "b"]` sorted
/// descending came back `["", "b", "a", "ab", "abc"]`).
///
/// To ORDER values, compare `encode_value` outputs and negate for a descending
/// column — prefix-shorter-first is exactly right ascending, and its reverse is
/// exactly right descending. `storage::sort_key` / `compare_sort_keys` do that.
/// Sort-ordering key for a value under `coll` — the three-level collation key
/// for strings (primary base letters / secondary accent marks / tertiary case),
/// and `encode_value` for everything else.
///
/// Separate from `encode_value` on purpose. `encode_value` is the INDEX
/// encoder: its bytes go in the entries table on disk and must stay
/// byte-identical to Python's. This one is only ever compared in memory within
/// a single sort, so it is free to carry the extra levels that mongod orders by
/// and that the single-level fold provably cannot express.
pub fn encode_sort_value(v: &Bson, coll: Option<&Collation>) -> Result<Vec<u8>, UnsupportedValue> {
    if let (Some(bytes), Some(c)) = (crate::bsondoc::as_invalid_utf8(v), coll) {
        let mut out = vec![RANK_STRING];
        out.extend(escape(&collation::sort_level_bytes(
            &String::from_utf8_lossy(bytes),
            c,
        )));
        return Ok(out);
    }
    match (v, coll) {
        (Bson::String(s), Some(c)) => {
            let mut out = Vec::new();
            out.push(RANK_STRING);
            out.extend(escape(&collation::sort_level_bytes(s, c)));
            Ok(out)
        }
        _ => encode_value(v, coll),
    }
}

/// The key an index field of `direction` holds for `v`: [`encode_value`]
/// for an ascending field; for a descending one, those bytes inverted and
/// then [`DESC_END`] (entry format 6), so that the stored order of a
/// descending field is exactly the reverse of the ascending order, prefixes
/// included -- mongod's order, which a cursor walking the index returns.
pub fn encode_value_directed(
    v: &Bson,
    direction: i32,
    coll: Option<&Collation>,
) -> Result<Vec<u8>, UnsupportedValue> {
    let e = encode_value(v, coll)?;
    Ok(if direction == -1 {
        let mut out = invert_bytes(&e);
        out.extend_from_slice(DESC_END);
        out
    } else {
        e
    })
}

/// The ascending key behind one field's stored key `part` (a slice
/// [`split_directed`] gave): the bytes themselves for an ascending field,
/// the inversion without [`DESC_END`] for a descending one.
pub fn ascending_part(part: &[u8], direction: i32) -> Vec<u8> {
    if direction == -1 {
        invert_bytes(part.strip_suffix(DESC_END).unwrap_or(part))
    } else {
        part.to_vec()
    }
}

/// The length of the one stored key of a field of `direction` at the start
/// of `key`, its terminator included for a descending field.
fn directed_len(key: &[u8], direction: i32) -> Option<usize> {
    if direction != -1 {
        return value_len(key);
    }
    // The inversion of the ascending key is self-delimiting the same way;
    // read it through an inverted copy of the rest.
    let ascending = invert_bytes(key);
    let n = value_len(&ascending)?;
    (key.get(n..n + 2)? == DESC_END).then_some(n + 2)
}

/// A compound key's stored per-field keys, one per entry of `directions`,
/// as [`encode_value_directed`] wrote them and joined by [`COMPOUND_SEP`].
/// `None` when `key` is not that.
pub fn split_directed<'a>(key: &'a [u8], directions: &[i32]) -> Option<Vec<&'a [u8]>> {
    let mut out = Vec::with_capacity(directions.len());
    let mut rest = key;
    for (i, &dir) in directions.iter().enumerate() {
        let n = directed_len(rest, dir)?;
        out.push(&rest[..n]);
        rest = &rest[n..];
        if i + 1 < directions.len() {
            rest = rest.strip_prefix(COMPOUND_SEP)?;
        }
    }
    rest.is_empty().then_some(out)
}

/// The length of the one encoded value at the start of `key`, an
/// [`encode_value`] output (ascending, in number-key format 2), or `None` if
/// `key` does not start with one. Every encoding is self-delimiting: a rank
/// byte, then a body of fixed width, or escaped (so it holds no `00 00`) up
/// to the next separator or the end, or with its own terminator. So a
/// compound key -- values joined by [`COMPOUND_SEP`] -- can be split, which
/// splitting at the separator cannot do: a negative number's digit pair 99
/// encodes as `0x00`, and a positive number ends in one.
pub fn value_len(key: &[u8]) -> Option<usize> {
    let rank = *key.first()?;
    let body = &key[1..];
    let len = match rank {
        // Index entries are entry format 5, where 14 is JavaScript with a
        // scope (format 4's MaxKey, kept only by `_id` keys).
        RANK_MINKEY if body.first() == Some(&UNDEFINED_TAIL) => 1,
        RANK_MINKEY | RANK_NULL | RANK_MAXKEY => 0,
        RANK_NUMBER => number_len(body)?,
        RANK_STRING | RANK_JAVASCRIPT => escaped_len(body),
        RANK_JAVASCRIPT_SCOPE => {
            let code = escaped_len(body);
            let rest = body.get(code..)?.strip_prefix(COMPOUND_SEP)?;
            code + 2 + elements_len(rest, true)?
        }
        RANK_DOCUMENT => elements_len(body, true)?,
        RANK_ARRAY => elements_len(body, false)?,
        RANK_BINDATA => {
            let n = u32::from_be_bytes(body.get(..4)?.try_into().ok()?) as usize;
            // Length, subtype, then `n` bytes escaped.
            let mut at = 5;
            for _ in 0..n {
                let b = *body.get(at)?;
                at += if b == 0 { 2 } else { 1 };
            }
            if at > body.len() {
                return None;
            }
            at
        }
        RANK_OBJECTID => 12,
        RANK_BOOL => 1,
        RANK_DATE | RANK_TIMESTAMP => 8,
        RANK_REGEX => {
            let pattern = escaped_len(body);
            let rest = body.get(pattern..)?.strip_prefix(COMPOUND_SEP)?;
            pattern + 2 + escaped_len(rest)
        }
        _ => return None,
    };
    (len <= body.len()).then_some(1 + len)
}

/// Escaped bytes run to the next `00 00` or the end: an escaped `0x00` is
/// always followed by `0xff`.
fn escaped_len(body: &[u8]) -> usize {
    body.windows(2)
        .position(|w| w == COMPOUND_SEP)
        .unwrap_or(body.len())
}

/// A number's body: one byte for NaN, the infinities and zero; otherwise
/// sign, two exponent bytes, digit pairs and a terminator that no pair
/// takes (`0x00` after a positive's pairs, `0xff` after a negative's).
fn number_len(body: &[u8]) -> Option<usize> {
    match *body.first()? {
        NUM_NAN | NUM_NEG_INF | NUM_ZERO | NUM_POS_INF => Some(1),
        NUM_POS => Some(3 + body.get(3..)?.iter().position(|&b| b == 0x00)? + 1),
        NUM_NEG => Some(3 + body.get(3..)?.iter().position(|&b| b == 0xff)? + 1),
        _ => None,
    }
}

/// A document's or array's elements, to and with [`ELEMENTS_END`]. Each
/// element is its rank byte, (a document's) escaped name and a separator,
/// then its value's escaped body and a separator.
fn elements_len(body: &[u8], named: bool) -> Option<usize> {
    let mut at = 0;
    loop {
        let rank = *body.get(at)?;
        at += 1;
        if rank == ELEMENTS_END {
            return Some(at);
        }
        if named {
            at += escaped_len(body.get(at..)?);
            at += 2;
        }
        at += escaped_len(body.get(at..)?);
        at += 2;
        if at > body.len() {
            return None;
        }
    }
}

/// The values of a compound key, `fields` of them joined by
/// [`COMPOUND_SEP`], each an ascending [`encode_value`] output. `None` when
/// `key` is not that.
pub fn split_compound(key: &[u8], fields: usize) -> Option<Vec<&[u8]>> {
    let mut out = Vec::with_capacity(fields);
    let mut rest = key;
    for i in 0..fields {
        let n = value_len(rest)?;
        out.push(&rest[..n]);
        rest = &rest[n..];
        if i + 1 < fields {
            rest = rest.strip_prefix(COMPOUND_SEP)?;
        }
    }
    rest.is_empty().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::Bson;

    /// Every kind of value, alone and joined into compound keys, splits back
    /// into exactly its parts: the separator alone cannot split them.
    #[test]
    fn compound_keys_split_into_their_values() {
        use bson::spec::BinarySubtype;
        use bson::{doc, Binary, JavaScriptCodeWithScope, Regex, Timestamp};
        let values = vec![
            Bson::MinKey,
            Bson::Null,
            Bson::MaxKey,
            Bson::Int32(0),
            Bson::Int32(1),
            Bson::Int32(-1),
            Bson::Int64(-9999),
            Bson::Int64(-99_9999),
            Bson::Int64(1_000_001),
            Bson::Double(-0.0099),
            Bson::Double(f64::NAN),
            Bson::Double(f64::INFINITY),
            Bson::Double(f64::NEG_INFINITY),
            Bson::Double(123.456),
            Bson::String(String::new()),
            Bson::String("a\0b\0\0".into()),
            Bson::String("plain".into()),
            Bson::JavaScriptCode("x\0".into()),
            Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                code: "f\0".into(),
                scope: doc! {"a": -99, "b": ""},
            }),
            Bson::Document(doc! {}),
            Bson::Document(doc! {"a": -99, "b\0": {"c": [1, "x\0", null]}, "": ""}),
            Bson::Array(vec![]),
            Bson::Array(vec![
                Bson::Int32(-99),
                Bson::Array(vec![]),
                Bson::String("\0".into()),
            ]),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![0, 0, 0, 1, 0],
            }),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes: vec![0; 16],
            }),
            Bson::ObjectId(bson::oid::ObjectId::from_bytes([0; 12])),
            Bson::Boolean(false),
            Bson::Boolean(true),
            Bson::DateTime(bson::DateTime::from_millis(0)),
            Bson::DateTime(bson::DateTime::from_millis(-1)),
            Bson::Timestamp(Timestamp {
                time: 0,
                increment: 0,
            }),
            Bson::RegularExpression(Regex {
                pattern: "a\0".into(),
                options: "i".into(),
            }),
            Bson::RegularExpression(Regex {
                pattern: String::new(),
                options: String::new(),
            }),
        ];
        let keys: Vec<Vec<u8>> = values
            .iter()
            .map(|v| encode_value(v, None).unwrap())
            .collect();
        for k in &keys {
            assert_eq!(value_len(k), Some(k.len()), "{k:?}");
        }
        // Every ordered pair and triple.
        for a in &keys {
            for b in &keys {
                let mut joined = a.clone();
                joined.extend_from_slice(COMPOUND_SEP);
                joined.extend_from_slice(b);
                assert_eq!(
                    split_compound(&joined, 2),
                    Some(vec![&a[..], &b[..]]),
                    "{a:?} {b:?}"
                );
                for c in keys.iter().step_by(3) {
                    let mut three = joined.clone();
                    three.extend_from_slice(COMPOUND_SEP);
                    three.extend_from_slice(c);
                    assert_eq!(
                        split_compound(&three, 3),
                        Some(vec![&a[..], &b[..], &c[..]]),
                        "{a:?} {b:?} {c:?}"
                    );
                }
            }
        }
        // A random sweep of numbers, the encoding most likely to hold zeros.
        let mut seed: u64 = 0x5eed;
        for _ in 0..20_000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let n = (seed >> 1) as i64 % 10_000_000_000 - 5_000_000_000;
            let d = f64::from_bits(seed);
            for v in [
                Bson::Int64(n),
                Bson::Double(if d.is_finite() { d } else { 0.5 }),
            ] {
                let k = encode_value(&v, None).unwrap();
                let mut joined = k.clone();
                joined.extend_from_slice(COMPOUND_SEP);
                joined.extend_from_slice(&k);
                assert_eq!(
                    split_compound(&joined, 2),
                    Some(vec![&k[..], &k[..]]),
                    "{v:?}"
                );
            }
        }
        assert_eq!(split_compound(&keys[3], 2), None);
    }

    fn ev(v: Bson) -> Vec<u8> {
        encode_value(&v, None).unwrap()
    }

    /// Entry format 5 (RM-232 / RM-233): BinData by length, subtype, bytes;
    /// JavaScript before JavaScript-with-scope, which orders by code then
    /// scope -- mongod 8.2.11's order. Each key must also agree with
    /// `order::cmp`, or an index changes the sort answer.
    #[test]
    fn bindata_and_javascript_keys_follow_mongod() {
        use bson::spec::BinarySubtype;
        use bson::{doc, Binary, JavaScriptCodeWithScope};
        let bin = |b: &[u8], s: BinarySubtype| {
            Bson::Binary(Binary {
                subtype: s,
                bytes: b.to_vec(),
            })
        };
        let cws = |c: &str, s: Document| {
            Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                code: c.into(),
                scope: s,
            })
        };
        let ascending = [
            bin(b"\x02", BinarySubtype::Generic),
            bin(b"\x01", BinarySubtype::UserDefined(0x80)),
            bin(b"\x01\x01", BinarySubtype::Generic),
            Bson::JavaScriptCode("c".into()),
            cws("a", doc! {"x": 1}),
            cws("a", doc! {"x": 2}),
            cws("b", doc! {}),
            Bson::MaxKey,
        ];
        for w in ascending.windows(2) {
            assert!(
                ev(w[0].clone()) < ev(w[1].clone()),
                "{:?} !< {:?}",
                w[0],
                w[1]
            );
            assert_eq!(crate::order::cmp(&w[0], &w[1]), std::cmp::Ordering::Less);
        }
    }

    /// Number-key format 2: every number keyed by its exact value, in order,
    /// whatever its exponent (mongod 8.2.11's order, measured 2026-10-15).
    #[test]
    fn numbers_are_keyed_by_exact_value_at_any_exponent() {
        let dec = |s: &str| Bson::Decimal128(s.parse().unwrap());
        let ascending = [
            Bson::Double(f64::NAN),
            Bson::Double(f64::NEG_INFINITY),
            dec("-1E+6144"),
            Bson::Double(-3e250),
            Bson::Double(-1.0),
            Bson::Double(-1e-300),
            dec("-1E-6176"),
            Bson::Int32(0),
            dec("1E-6176"),
            Bson::Double(5e-324),
            Bson::Double(1e-300),
            dec("0.1"),
            dec("0.1000000000000000055511151231257827"),
            Bson::Double(0.1),
            Bson::Int32(5),
            Bson::Double(2f64.powi(62)),
            Bson::Int64(4611686018427387950),
            Bson::Double(3e250),
            dec("1E+400"),
            dec("1E+6144"),
            Bson::Double(f64::INFINITY),
        ];
        for pair in ascending.windows(2) {
            assert!(
                ev(pair[0].clone()) < ev(pair[1].clone()),
                "{:?} should key below {:?}",
                pair[0],
                pair[1]
            );
        }
        // Equal values, equal keys.
        assert_eq!(ev(Bson::Int64(1 << 62)), ev(Bson::Double(2f64.powi(62))));
        assert_eq!(ev(Bson::Int32(5)), ev(dec("5.000")));
        assert_eq!(ev(Bson::Double(0.5)), ev(dec("0.5")));
    }

    /// Entry format 4: documents and arrays keyed by VALUE. Each pair is
    /// `a < b` by mongod 8.2.11's `$cmp` (measured 2026-09-30); the same pairs
    /// are in Python's `test_sortkey.py`.
    #[test]
    fn documents_and_arrays_are_keyed_by_value() {
        use bson::{bson, doc};
        let pairs: Vec<(Bson, Bson)> = vec![
            // a longer document is not a larger one: raw BSON said it was
            (bson!({"a": 2, "b": [3]}), bson!({"a": 5})),
            (bson!({"a": 1}), bson!({"a": 1, "b": 0})),
            (Bson::Document(doc! {}), bson!({"a": Bson::MinKey})),
            // the element's TYPE decides before its field NAME
            (bson!({"b": 1}), bson!({"a": "x"})),
            (bson!({"a": 1}), bson!({"b": 1})),
            (bson!({"a": 1}), bson!({"a": 2})),
            (bson!([9]), bson!([10])),
            (bson!([1, 2, 3]), bson!([9])),
            (bson!([1, 2]), bson!([1, 2, 0])),
            (bson!([]), bson!([Bson::MinKey])),
            (bson!({"a": {"b": 1}}), bson!({"a": {"b": 1, "c": 0}})),
            (bson!({"a": [1, {"x": 2}]}), bson!({"a": [1, {"x": 3}]})),
        ];
        for (a, b) in pairs {
            assert!(ev(a.clone()) < ev(b.clone()), "{a} should sort below {b}");
        }
        // mongod compares {a: 1} and {a: 1.0} equal; raw BSON did not.
        assert_eq!(ev(bson!({"a": 1})), ev(bson!({"a": 1.0})));
    }

    /// `_id` key format 1 kept formats 1-3's document encoding; format 2
    /// keys every `_id` in mongod's order, BinData with its subtype whichever
    /// server's thread writes it.
    #[test]
    fn id_keys_follow_mongods_order() {
        use bson::doc;
        let d = doc! {"b": 2, "a": [1, "x"]};
        let mut raw = Vec::new();
        d.to_writer(&mut raw).unwrap();
        let mut old = vec![RANK_DOCUMENT];
        old.extend(escape(&raw));
        assert_eq!(encode_id_key_format_1(&Bson::Document(d.clone())).unwrap(), old);
        assert_eq!(with_id_key_format_1(|| encode_id_key(&Bson::Document(d.clone()))).unwrap(), old);
        assert_eq!(encode_id_key(&Bson::Document(d.clone())).unwrap(), ev(Bson::Document(d)));
        assert_eq!(encode_id_key(&Bson::Int32(5)).unwrap(), ev(Bson::Int32(5)));
        // A longer document is not a larger one.
        let small = encode_id_key(&bson::bson!({"a": 2, "b": [3]})).unwrap();
        let large = encode_id_key(&bson::bson!({"a": 5})).unwrap();
        assert!(small < large);
        let bin = Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Uuid,
            bytes: vec![1; 16],
        });
        let mongod = encode_id_key(&bin).unwrap();
        let pg = crate::order::with_bindata_order(crate::order::BinDataOrder::Bytes, || {
            encode_id_key(&bin).unwrap()
        });
        assert_eq!(mongod, pg);
        assert!(may_be_format1_id_key(&encode_id_key_format_1(&bin).unwrap()));
        assert!(!may_be_format1_id_key(&ev(Bson::Int32(5))));
    }

    #[test]
    fn ranks_order_across_types() {
        // null < number < string < bool < date (sample of the rank ladder).
        assert!(ev(Bson::Null) < ev(Bson::Int32(0)));
        assert!(ev(Bson::Int32(0)) < ev(Bson::String("".into())));
        assert!(ev(Bson::String("z".into())) < ev(Bson::Boolean(false)));
    }

    #[test]
    fn cross_type_numeric_collision() {
        // The headline property: equal numeric value -> identical key bytes,
        // regardless of int32 / int64 / double / decimal128 representation.
        let i = ev(Bson::Int32(3));
        let l = ev(Bson::Int64(3));
        let d = ev(Bson::Double(3.0));
        let dec = ev(Bson::Decimal128("3".parse().unwrap()));
        assert_eq!(i, l);
        assert_eq!(i, d);
        assert_eq!(i, dec);
    }

    #[test]
    fn numbers_sort_correctly() {
        let mut keys = [
            ev(Bson::Double(-2.5)),
            ev(Bson::Int32(-1)),
            ev(Bson::Int32(0)),
            ev(Bson::Double(1.5)),
            ev(Bson::Int32(1000)),
        ];
        let sorted = keys.clone();
        keys.sort();
        assert_eq!(
            keys, sorted,
            "encoded numbers must already be in value order"
        );
    }

    #[test]
    fn directed_inverts_for_descending() {
        let asc = ev(Bson::Int32(5));
        let desc = encode_value_directed(&Bson::Int32(5), -1, None).unwrap();
        let mut want = invert_bytes(&asc);
        want.extend_from_slice(DESC_END);
        assert_eq!(desc, want);
        assert_eq!(ascending_part(&desc, -1), asc);
    }

    /// Entry format 6: a descending field's keys are in exactly the reverse
    /// of the ascending order, a string before its own prefixes (mongod
    /// 8.2.11 returns `["c", "ba", "b", "abc", "ab", "a", ""]` walking
    /// `{s: -1}`), alone and as a compound key's leading field.
    #[test]
    fn descending_keys_reverse_prefixes() {
        let values: Vec<Bson> = vec![
            Bson::MinKey,
            Bson::Undefined,
            Bson::Null,
            Bson::Int32(-99),
            Bson::Int32(0),
            Bson::Double(1.5),
            Bson::String(String::new()),
            Bson::String("\0".into()),
            Bson::String("\0\0".into()),
            Bson::String("a".into()),
            Bson::String("a\0".into()),
            Bson::String("ab".into()),
            Bson::String("abc".into()),
            Bson::String("b".into()),
            Bson::JavaScriptCode("f".into()),
            Bson::JavaScriptCode("fg".into()),
            Bson::MaxKey,
        ];
        for w in values.windows(2) {
            assert!(ev(w[0].clone()) < ev(w[1].clone()), "{:?} {:?}", w[0], w[1]);
            let d = |v: &Bson| encode_value_directed(v, -1, None).unwrap();
            assert!(d(&w[0]) > d(&w[1]), "descending {:?} {:?}", w[0], w[1]);
            // Followed by another field, as in a compound index.
            let mut a = d(&w[0]);
            a.extend_from_slice(COMPOUND_SEP);
            a.extend(ev(Bson::Int32(0)));
            let mut b = d(&w[1]);
            b.extend_from_slice(COMPOUND_SEP);
            b.extend(ev(Bson::MaxKey));
            assert!(a > b, "compound {:?} {:?}", w[0], w[1]);
        }
    }

    /// A compound key of mixed directions splits back into its stored
    /// per-field keys, and each into the ascending key it came from.
    #[test]
    fn directed_keys_split_into_their_values() {
        use bson::doc;
        let values = vec![
            Bson::MinKey,
            Bson::Undefined,
            Bson::Null,
            Bson::Int64(-99_9999),
            Bson::Double(0.5),
            Bson::String("a\0b".into()),
            Bson::String(String::new()),
            Bson::Document(doc! {"a": [1, "x\0"]}),
            Bson::Array(vec![]),
            Bson::Boolean(true),
            Bson::MaxKey,
        ];
        for a in &values {
            for b in &values {
                for dirs in [[1, -1], [-1, 1], [-1, -1], [1, 1]] {
                    let pa = encode_value_directed(a, dirs[0], None).unwrap();
                    let pb = encode_value_directed(b, dirs[1], None).unwrap();
                    let mut key = pa.clone();
                    key.extend_from_slice(COMPOUND_SEP);
                    key.extend_from_slice(&pb);
                    let parts = split_directed(&key, &dirs).expect("splits");
                    assert_eq!(parts, vec![&pa[..], &pb[..]], "{a:?} {b:?} {dirs:?}");
                    assert_eq!(ascending_part(parts[0], dirs[0]), ev(a.clone()));
                    assert_eq!(ascending_part(parts[1], dirs[1]), ev(b.clone()));
                }
            }
        }
    }

    /// mongod keys an empty array by `undefined`, after MinKey and before
    /// null; the key is not a MinKey key's extension of anything else.
    #[test]
    fn undefined_keys_between_minkey_and_null() {
        let u = ev(Bson::Undefined);
        assert!(ev(Bson::MinKey) < u);
        assert!(u < ev(Bson::Null));
        assert_eq!(value_len(&u), Some(2));
        assert!(encode_id_key(&Bson::Undefined).is_err());
    }
}
