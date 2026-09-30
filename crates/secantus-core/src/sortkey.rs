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
const RANK_MAXKEY: u8 = 14;

const NUM_NAN: u8 = 0x00;
const NUM_NEG_INF: u8 = 0x20;
const NUM_NEG: u8 = 0x40;
const NUM_ZERO: u8 = 0x80;
const NUM_POS: u8 = 0xC0;
const NUM_POS_INF: u8 = 0xFF;

/// Compound-key separator (also the escape sentinel). Matches
/// `secantus.sortkey.COMPOUND_SEP`.
pub const COMPOUND_SEP: &[u8] = b"\x00\x00";

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

fn encode_number_from_parts(p: &DecimalParts) -> Vec<u8> {
    let sci_exp = p.exp + p.digits.len() as i64 - 1;
    let mut bias_e = 128 + sci_exp;
    if !(0..=255).contains(&bias_e) {
        // Out of single-byte exponent range — sort on the correct side of
        // zero, magnitudes within the rank collapse (matches Python).
        return if p.sign > 0 {
            vec![NUM_POS, 0xFF, 0xFF]
        } else {
            vec![NUM_NEG, 0x00, 0x00]
        };
    }
    if p.sign < 0 {
        bias_e = 0xFF - bias_e;
    }
    let mut digits = p.digits.clone();
    if digits.len() % 2 == 1 {
        digits.push(0);
    }
    // `digits` was padded to an even length just above, so the `as_chunks`
    // remainder (`.1`) is always empty and the pairing is exhaustive — same
    // semantics as the `chunks_exact(2)` this replaces.
    let mut pairs: Vec<u8> = digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| c[0] * 10 + c[1] + 1)
        .collect();
    if p.sign < 0 {
        for b in pairs.iter_mut() {
            *b = 0x64 - *b;
        }
    }
    let prefix = if p.sign > 0 { NUM_POS } else { NUM_NEG };
    let terminator = if p.sign > 0 { 0x00 } else { 0xff };
    let mut out = vec![prefix, bias_e as u8];
    out.append(&mut pairs);
    out.push(terminator);
    out
}

fn encode_number(b: &Bson) -> Vec<u8> {
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
                // Python: Decimal(repr(value)); Rust's shortest Display is the
                // equivalent round-tripping decimal for a finite f64.
                parse_decimal_str(&format!("{d}"))
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
    doc.to_writer(&mut buf)
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

/// The `_id` key: [`encode_value`] with formats 1-3's document / array encoding,
/// frozen.
///
/// An `_id` key is not an index entry. It is stored in every document row and
/// keys the `_id` index, so changing it would strand every stored document whose
/// `_id` is a document -- its lookups would compute a key no row carries,
/// silently. Entry format 4 changed how documents and arrays ORDER in secondary
/// indexes; an `_id` key only needs to be the same bytes for the same value,
/// which the old encoding already is. Mirrors `sortkey.encode_id_key`.
pub fn encode_id_key(v: &Bson) -> Result<Vec<u8>, UnsupportedValue> {
    match v {
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
        other => encode_value(other, None),
    }
}

/// Byte-sortable encoding of a single BSON value. Byte-exact counterpart of
/// `secantus.sortkey.encode_value`. `coll` is the index's collation (or `None`);
/// it applies to every string, nested ones included (entry format 4).
pub fn encode_value(v: &Bson, coll: Option<&Collation>) -> Result<Vec<u8>, UnsupportedValue> {
    let mut out = Vec::new();
    match v {
        Bson::MinKey => out.push(RANK_MINKEY),
        Bson::Null => out.push(RANK_NULL),
        Bson::MaxKey => out.push(RANK_MAXKEY),
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
        // Code orders by its code text; mongod ignores the scope for ordering.
        Bson::JavaScriptCode(s) => {
            out.push(RANK_JAVASCRIPT);
            out.extend(escape(s.as_bytes()));
        }
        Bson::JavaScriptCodeWithScope(c) => {
            out.push(RANK_JAVASCRIPT);
            out.extend(escape(c.code.as_bytes()));
        }
        Bson::Document(d) => {
            out.push(RANK_DOCUMENT);
            out.extend(encode_doc(d, coll)?);
        }
        Bson::Array(a) => {
            out.push(RANK_ARRAY);
            out.extend(encode_array(a, coll)?);
        }
        Bson::Binary(b) => {
            out.push(RANK_BINDATA);
            out.extend_from_slice(&(b.bytes.len() as u32).to_be_bytes());
            out.extend(escape(&b.bytes));
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

pub fn encode_value_directed(
    v: &Bson,
    direction: i32,
    coll: Option<&Collation>,
) -> Result<Vec<u8>, UnsupportedValue> {
    let e = encode_value(v, coll)?;
    Ok(if direction == -1 { invert_bytes(&e) } else { e })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::Bson;

    fn ev(v: Bson) -> Vec<u8> {
        encode_value(&v, None).unwrap()
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

    /// An `_id` key keeps formats 1-3's document encoding: it is stored in
    /// every document row.
    #[test]
    fn the_id_key_keeps_the_old_document_encoding() {
        use bson::doc;
        let d = doc! {"b": 2, "a": [1, "x"]};
        let mut raw = Vec::new();
        d.to_writer(&mut raw).unwrap();
        let mut want = vec![RANK_DOCUMENT];
        want.extend(escape(&raw));
        assert_eq!(encode_id_key(&Bson::Document(d)).unwrap(), want);
        assert_eq!(encode_id_key(&Bson::Int32(5)).unwrap(), ev(Bson::Int32(5)));
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
        assert_eq!(desc, invert_bytes(&asc));
    }
}
