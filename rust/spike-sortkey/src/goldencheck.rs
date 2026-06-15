//! Spike 3 — byte-exact port of `secantus.sortkey.encode_value`.
//!
//! Reads the framed golden-vector stream emitted by
//! `spike_sortkey_golden.py` (records `{label, v, k}`), recomputes the sort
//! key for each `v` with the Rust port below, and asserts it equals the
//! Python-produced `k` byte-for-byte. Covers the scalar ranks; documents and
//! arrays route through `bson::encode` + escape and are validated separately
//! by Spike 1's fidelity result.
//!
//! The interesting code is `encode_number`: the "lexical decimal" form that
//! makes int32 / int64 / double / Decimal128 of equal value collide on equal
//! key bytes and sort correctly across the unified numeric type.

use std::io::Read;

use bson::{Binary, Bson, Document, RawDocument};

// Type ranks — must match secantus.sortkey.
const RANK_MINKEY: u8 = 1;
const RANK_NULL: u8 = 2;
const RANK_NUMBER: u8 = 3;
const RANK_STRING: u8 = 4;
const RANK_BINDATA: u8 = 7;
const RANK_OBJECTID: u8 = 8;
const RANK_BOOL: u8 = 9;
const RANK_DATE: u8 = 10;
const RANK_TIMESTAMP: u8 = 11;
const RANK_MAXKEY: u8 = 13;

const NUM_NAN: u8 = 0x00;
const NUM_NEG_INF: u8 = 0x20;
const NUM_NEG: u8 = 0x40;
const NUM_ZERO: u8 = 0x80;
const NUM_POS: u8 = 0xC0;
const NUM_POS_INF: u8 = 0xFF;

fn escape(data: &[u8]) -> Vec<u8> {
    // 0x00 -> 0x00 0xff, order-preserving.
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        out.push(b);
        if b == 0 {
            out.push(0xff);
        }
    }
    out
}

/// Decimal in the form value = sign * (digits as integer) * 10^exp, with no
/// leading or trailing zero digits. `None` for zero / non-finite.
struct DecimalParts {
    sign: i32,
    digits: Vec<u8>, // each 0..=9, most-significant first, no leading/trailing zeros
    exp: i64,
}

fn parse_decimal_str(s: &str) -> Option<DecimalParts> {
    let s = s.trim();
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    if rest.eq_ignore_ascii_case("nan")
        || rest.eq_ignore_ascii_case("inf")
        || rest.eq_ignore_ascii_case("infinity")
    {
        return None;
    }
    // split mantissa / exponent
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
    let s = n.to_string();
    let mut digits: Vec<u8> = s.bytes().map(|b| b - b'0').collect();
    let mut exp: i64 = 0;
    normalize(&mut digits, &mut exp);
    Some(DecimalParts { sign, digits, exp })
}

/// Strip leading zeros (no exp change) and trailing zeros (exp += 1 each),
/// mirroring Python `Decimal.normalize()` on a finite non-zero magnitude.
fn normalize(digits: &mut Vec<u8>, exp: &mut i64) {
    while digits.first() == Some(&0) {
        digits.remove(0);
    }
    while digits.last() == Some(&0) {
        digits.pop();
        *exp += 1;
    }
}

fn encode_number_from_parts(p: &DecimalParts) -> Vec<u8> {
    let sci_exp = p.exp + p.digits.len() as i64 - 1;
    let mut bias_e = 128 + sci_exp;
    if !(0..=255).contains(&bias_e) {
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
                // Python uses Decimal(repr(value)); Rust's shortest Display is
                // the equivalent round-tripping decimal for finite f64.
                parse_decimal_str(&format!("{d}"))
            }
        }
        Bson::Decimal128(d) => {
            let s = d.to_string();
            if s.eq_ignore_ascii_case("nan") || s.to_lowercase().contains("nan") {
                return vec![NUM_NAN];
            }
            if s.to_lowercase().contains("inf") {
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

fn signed_int64_sortable(n: i64) -> Vec<u8> {
    ((n as u64) ^ 0x8000_0000_0000_0000).to_be_bytes().to_vec()
}

fn encode_value(v: &Bson) -> Vec<u8> {
    let rank = rank_of(v);
    let mut out = vec![rank];
    match v {
        Bson::MinKey | Bson::Null | Bson::MaxKey => {}
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
            out.extend(encode_number(v));
        }
        Bson::String(s) => out.extend(escape(s.as_bytes())),
        Bson::Boolean(b) => out.push(if *b { 1 } else { 0 }),
        Bson::ObjectId(oid) => out.extend_from_slice(&oid.bytes()),
        Bson::DateTime(dt) => out.extend(signed_int64_sortable(dt.timestamp_millis())),
        Bson::Timestamp(ts) => {
            out.extend_from_slice(&ts.time.to_be_bytes());
            out.extend_from_slice(&ts.increment.to_be_bytes());
        }
        Bson::Binary(Binary { bytes, .. }) => {
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend(escape(bytes));
        }
        other => panic!("rank/encode mismatch for {other:?}"),
    }
    out
}

fn rank_of(v: &Bson) -> u8 {
    match v {
        Bson::MinKey => RANK_MINKEY,
        Bson::Null => RANK_NULL,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => RANK_NUMBER,
        Bson::String(_) => RANK_STRING,
        Bson::Binary(_) => RANK_BINDATA,
        Bson::ObjectId(_) => RANK_OBJECTID,
        Bson::Boolean(_) => RANK_BOOL,
        Bson::DateTime(_) => RANK_DATE,
        Bson::Timestamp(_) => RANK_TIMESTAMP,
        Bson::MaxKey => RANK_MAXKEY,
        other => panic!("unsupported value in spike: {other:?}"),
    }
}

fn main() {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("read stdin");

    let mut offset = 0usize;
    let mut total = 0usize;
    let mut failures = 0usize;

    while offset < input.len() {
        let declared = i32::from_le_bytes(input[offset..offset + 4].try_into().unwrap()) as usize;
        let raw = RawDocument::from_bytes(&input[offset..offset + declared]).expect("raw doc");
        let doc: Document = raw.try_into().expect("doc");
        offset += declared;
        total += 1;

        let label = doc.get_str("label").unwrap_or("?").to_string();
        let v = doc.get("v").expect("v");
        let expected = match doc.get("k") {
            Some(Bson::Binary(b)) => b.bytes.clone(),
            _ => panic!("record {label}: missing binary k"),
        };
        let got = encode_value(v);
        if got == expected {
            println!("  [ok]   {label}");
        } else {
            failures += 1;
            println!("  [DIFF] {label}");
            println!("      python: {}", hex(&expected));
            println!("      rust  : {}", hex(&got));
        }
    }

    println!(
        "\nRESULT: {}",
        if failures == 0 {
            format!("PASS — {total} sort keys byte-identical to secantus.sortkey")
        } else {
            format!("FAIL — {failures}/{total} diverged")
        }
    );
    if failures > 0 {
        std::process::exit(1);
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
