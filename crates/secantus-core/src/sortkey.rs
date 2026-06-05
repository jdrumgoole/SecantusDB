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
const RANK_MINKEY: u8 = 1;
const RANK_NULL: u8 = 2;
const RANK_NUMBER: u8 = 3;
const RANK_STRING: u8 = 4;
const RANK_DOCUMENT: u8 = 5;
const RANK_ARRAY: u8 = 6;
const RANK_BINDATA: u8 = 7;
const RANK_OBJECTID: u8 = 8;
const RANK_BOOL: u8 = 9;
const RANK_DATE: u8 = 10;
const RANK_TIMESTAMP: u8 = 11;
const RANK_REGEX: u8 = 12;
const RANK_MAXKEY: u8 = 13;

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
    let mut pairs: Vec<u8> = digits
        .chunks_exact(2)
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

/// Encode an array as a BSON document with positional string keys, matching
/// `secantus.sortkey._encode_array` (so array equality lines up at the index).
fn encode_array_bytes(arr: &[Bson]) -> Result<Vec<u8>, UnsupportedValue> {
    let mut doc = Document::new();
    for (i, v) in arr.iter().enumerate() {
        doc.insert(i.to_string(), v.clone());
    }
    doc_to_escaped_bytes(&doc)
}

fn doc_to_escaped_bytes(doc: &Document) -> Result<Vec<u8>, UnsupportedValue> {
    let mut buf = Vec::new();
    doc.to_writer(&mut buf)
        .map_err(|e| UnsupportedValue(format!("doc encode failed: {e}")))?;
    Ok(escape(&buf))
}

/// Byte-sortable encoding of a single BSON value. Byte-exact counterpart of
/// `secantus.sortkey.encode_value`. `coll` is the index's collation (or `None`);
/// it applies only to top-level string values — strings nested inside documents
/// / arrays are encoded as raw BSON, matching Python's `_encode_doc` /
/// `_encode_array` (which don't thread collation).
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
            let bytes = match coll {
                Some(c) => collation::normalize_index_bytes(s, c)
                    .ok_or_else(|| UnsupportedValue("collation defers to Python".into()))?,
                None => s.as_bytes().to_vec(),
            };
            out.extend(escape(&bytes));
        }
        Bson::Document(d) => {
            out.push(RANK_DOCUMENT);
            out.extend(doc_to_escaped_bytes(d)?);
        }
        Bson::Array(a) => {
            out.push(RANK_ARRAY);
            out.extend(encode_array_bytes(a)?);
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
            out.extend(escape(r.options.as_bytes()));
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
