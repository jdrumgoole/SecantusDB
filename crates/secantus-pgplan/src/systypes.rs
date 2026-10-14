//! `macaddr`, `macaddr8`, `pg_lsn`, `txid_snapshot` and `pg_snapshot`:
//! input and output transcribed from PostgreSQL 15's `mac.c`, `mac8.c`,
//! `pg_lsn.c` and `xid8funcs.c`.
//!
//! `macaddr` / `macaddr8` are stored as their canonical text
//! (`08:00:2b:01:02:03`), whose byte order IS the type's order (fixed width,
//! lowercase hex). A `pg_lsn` is stored as its 64-bit position (a numeric),
//! so it sorts and subtracts as PostgreSQL's does; it prints as `%X/%X`
//! wherever its type is known. The snapshot types have no ordering and are
//! stored as their canonical text.

use crate::{numeric, Error, Result};
use bson::Bson;

fn bad(ty: &str, input: &str) -> Error {
    Error::InvalidText(format!("invalid input syntax for type {ty}: \"{input}\""))
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// `macaddr_in`: the separated forms `xx:xx:xx:xx:xx:xx` /
/// `xx-xx-...` (one or two digits per byte), and the grouped forms
/// `xxxxxx:xxxxxx`, `xxxxxx-xxxxxx`, `xxxx.xxxx.xxxx`, `xxxx-xxxx-xxxx`,
/// `xxxxxxxxxxxx` (exactly two digits per byte). Leading whitespace and
/// trailing whitespace are allowed, as `sscanf` allows them.
pub fn parse_macaddr(input: &str) -> Result<String> {
    let s = input.trim_matches(|c: char| c.is_ascii_whitespace());
    let bytes = separated(s, ':', 6)
        .or_else(|| separated(s, '-', 6))
        .or_else(|| grouped(s, &[3, 3], ':'))
        .or_else(|| grouped(s, &[3, 3], '-'))
        .or_else(|| grouped(s, &[2, 2, 2], '.'))
        .or_else(|| grouped(s, &[2, 2, 2], '-'))
        .or_else(|| grouped(s, &[6], ' '))
        .ok_or_else(|| bad("macaddr", input))?;
    Ok(render(&bytes))
}

/// `n` bytes of one or two hex digits each, separated by `sep`.
fn separated(s: &str, sep: char, n: usize) -> Option<Vec<u8>> {
    let parts: Vec<&str> = s.split(sep).collect();
    if parts.len() != n {
        return None;
    }
    parts
        .iter()
        .map(|p| {
            let b = p.as_bytes();
            match b.len() {
                1 => hex(b[0]),
                2 => Some(hex(b[0])? * 16 + hex(b[1])?),
                _ => None,
            }
        })
        .collect()
}

/// Groups of `sizes[i]` two-digit bytes, separated by `sep`.
fn grouped(s: &str, sizes: &[usize], sep: char) -> Option<Vec<u8>> {
    let parts: Vec<&str> = if sizes.len() == 1 {
        vec![s]
    } else {
        s.split(sep).collect()
    };
    if parts.len() != sizes.len() {
        return None;
    }
    let mut out = Vec::new();
    for (p, n) in parts.iter().zip(sizes) {
        let b = p.as_bytes();
        if b.len() != n * 2 {
            return None;
        }
        for pair in b.chunks(2) {
            out.push(hex(pair[0])? * 16 + hex(pair[1])?);
        }
    }
    Some(out)
}

fn render(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// `macaddr8_in`: six or eight bytes of two hex digits each, optionally
/// separated by one of `:` `-` `.` (the same one throughout), with
/// surrounding whitespace. A six-byte address becomes the EUI-64 form, with
/// `ff:fe` inserted after the third byte.
pub fn parse_macaddr8(input: &str) -> Result<String> {
    let fail = || bad("macaddr8", input);
    let b = input.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut out: Vec<u8> = Vec::new();
    let mut spacer: Option<u8> = None;
    while i < b.len() && !b[i].is_ascii_whitespace() {
        let hi = hex(b[i]).ok_or_else(fail)?;
        let lo = b.get(i + 1).and_then(|c| hex(*c)).ok_or_else(fail)?;
        out.push(hi * 16 + lo);
        i += 2;
        if out.len() > 8 {
            return Err(fail());
        }
        if let Some(&c) = b.get(i) {
            if matches!(c, b':' | b'-' | b'.') {
                match spacer {
                    None => spacer = Some(c),
                    Some(s) if s != c => return Err(fail()),
                    _ => {}
                }
                i += 1;
                // A separator must be followed by another byte.
                if b.get(i).is_none_or(|c| c.is_ascii_whitespace()) {
                    return Err(fail());
                }
            }
        }
    }
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i != b.len() {
        return Err(fail());
    }
    match out.len() {
        8 => Ok(render(&out)),
        6 => Ok(render(&[
            out[0], out[1], out[2], 0xff, 0xfe, out[3], out[4], out[5],
        ])),
        _ => Err(fail()),
    }
}

/// `macaddr8tomacaddr`: only an address with `ff:fe` in its middle has a
/// six-byte form.
pub fn macaddr8_to_macaddr(stored: &str) -> Result<String> {
    let bytes: Vec<u8> = stored
        .split(':')
        .filter_map(|p| u8::from_str_radix(p, 16).ok())
        .collect();
    if bytes.len() != 8 || bytes[3] != 0xff || bytes[4] != 0xfe {
        return Err(Error::NumericOutOfRange(
            "macaddr8 data out of range to convert to macaddr".into(),
        ));
    }
    Ok(render(&[
        bytes[0], bytes[1], bytes[2], bytes[5], bytes[6], bytes[7],
    ]))
}

/// The raw bytes of a stored `macaddr` / `macaddr8` (its binary form).
pub fn mac_to_wire(stored: &str) -> Option<Vec<u8>> {
    stored
        .split(':')
        .map(|p| u8::from_str_radix(p, 16).ok())
        .collect()
}

/// A `macaddr` / `macaddr8` from its binary form.
pub fn mac_from_wire(bytes: &[u8]) -> String {
    render(bytes)
}

/// `pg_lsn_in`: `%X/%X`, each half one to eight hex digits, nothing else.
pub fn parse_lsn(input: &str) -> Result<u64> {
    let fail = || bad("pg_lsn", input);
    let (hi, lo) = input.split_once('/').ok_or_else(fail)?;
    let half = |s: &str| -> Result<u64> {
        if s.is_empty() || s.len() > 8 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(fail());
        }
        u64::from_str_radix(s, 16).map_err(|_| fail())
    };
    Ok((half(hi)? << 32) | half(lo)?)
}

/// `pg_lsn_out`: `%X/%X`.
pub fn render_lsn(lsn: u64) -> String {
    format!("{:X}/{:X}", lsn >> 32, lsn & 0xffff_ffff)
}

/// A `pg_lsn` as it is stored: its position as a numeric.
pub fn lsn_bson(lsn: u64) -> Bson {
    numeric::numeric_bson(&lsn.to_string())
}

/// The position a stored `pg_lsn` holds.
pub fn lsn_of(v: &Bson) -> Option<u64> {
    match v {
        Bson::Int32(i) => u64::try_from(*i).ok(),
        Bson::Int64(i) => u64::try_from(*i).ok(),
        Bson::String(s) => parse_lsn(s).ok(),
        other => crate::value_text(other).parse::<u64>().ok(),
    }
}

/// A stored `pg_lsn` as `pg_lsn_out` prints it (a value that is not one,
/// as its own text).
pub fn render_lsn_value(v: &Bson) -> String {
    lsn_of(v).map_or_else(|| crate::value_text(v), render_lsn)
}

/// Whether `t` is an operand `pg_lsn_pli` / `pg_lsn_mii` take: a numeric,
/// an integer (implicitly cast to one), or an untyped literal.
fn lsn_offset_type(t: &str) -> bool {
    matches!(
        t,
        "numeric" | "int2" | "int4" | "int8" | "smallint" | "integer" | "bigint" | "unknown"
    )
}

/// The type `pg_lsn op x` yields (`pg_lsn.c`): `pg_lsn + numeric`,
/// `numeric + pg_lsn` and `pg_lsn - numeric` are a `pg_lsn`, and
/// `pg_lsn - pg_lsn` is a `numeric`. `None` for any other pairing.
pub fn lsn_arith_type(op: &str, lt: &str, rt: &str) -> Option<&'static str> {
    match (op, lt, rt) {
        // An untyped operand of `-` is resolved as a second pg_lsn.
        ("-", "pg_lsn", "pg_lsn" | "unknown") => Some("numeric"),
        ("+", "pg_lsn", t) | ("+", t, "pg_lsn") | ("-", "pg_lsn", t) if lsn_offset_type(t) => {
            Some("pg_lsn")
        }
        _ => None,
    }
}

/// `pg_lsn_pli` / `pg_lsn_mii`: the position plus or minus a numeric, as a
/// `pg_lsn`. `None` unless [`lsn_arith_type`] says the result is one.
/// The sum is rounded half away from zero and THEN range-checked, as
/// `numeric_pg_lsn` does, so `'0/16' - 22.4` is `0/0` and `- 22.5` is out
/// of range.
pub fn lsn_arith(op: &str, l: &Bson, lt: &str, r: &Bson, rt: &str) -> Option<Result<Bson>> {
    match lsn_arith_type(op, lt, rt) {
        Some("pg_lsn") => {}
        // `pg_lsn - pg_lsn`: the numeric difference the caller computes.
        Some(_) => return None,
        // Any other arithmetic with a pg_lsn has no operator in PostgreSQL;
        // computed on the stored position it answered a number.
        None if (lt == "pg_lsn" || rt == "pg_lsn") && matches!(op, "+" | "-" | "*" | "/" | "%") => {
            return Some(Err(Error::UndefinedFunction(format!(
                "operator does not exist: {} {op} {}",
                crate::display_type(lt),
                crate::display_type(rt)
            ))));
        }
        None => return None,
    }
    let (lsn, offset) = if lt == "pg_lsn" { (l, r) } else { (r, l) };
    if *lsn == Bson::Null || *offset == Bson::Null {
        return Some(Ok(Bson::Null));
    }
    Some((|| {
        let pos = lsn_of(lsn).ok_or_else(|| {
            Error::DataException(format!("not a pg_lsn: {}", crate::value_text(lsn)))
        })?;
        let text = match offset {
            Bson::String(s) => numeric::canonical_numeric_text(s)?,
            other => numeric::numeric_operand_text(other)
                .ok_or_else(|| Error::DataException("not a numeric".into()))?,
        };
        if text == "NaN" {
            return Err(Error::FeatureNotSupported(
                if op == "+" {
                    "cannot add NaN to pg_lsn"
                } else {
                    "cannot subtract NaN from pg_lsn"
                }
                .into(),
            ));
        }
        let sum = numeric::decimal_arith(op, &pos.to_string(), &text)
            .ok_or_else(|| Error::DataException("numeric arithmetic failed".into()))??;
        let sum = numeric::numeric_text(&sum).unwrap_or_else(|| crate::value_text(&sum));
        if sum.ends_with("Infinity") {
            return Err(Error::FeatureNotSupported(
                "cannot convert infinity to pg_lsn".into(),
            ));
        }
        numeric::numeric_text_to_integer(&sum)
            .and_then(|n| u64::try_from(n).ok())
            .map(lsn_bson)
            .ok_or_else(|| Error::InvalidParameter("pg_lsn out of range".into()))
    })())
}

/// `cast(x AS pg_lsn)`: from its text.
pub fn cast_lsn(value: &Bson) -> Result<Bson> {
    match value {
        Bson::Null => Ok(Bson::Null),
        Bson::String(s) => parse_lsn(s).map(lsn_bson),
        other => Err(Error::CannotCoerce(format!(
            "cannot cast type {} to pg_lsn",
            crate::inferred_type(other)
        ))),
    }
}

/// `pg_snapshot_in` / `txid_snapshot_in`: `xmin:xmax:xip,...`, where
/// `0 < xmin <= xmax` and every xip lies in `[xmin, xmax)` in ascending
/// order; a repeated xip is dropped.
pub fn parse_snapshot(input: &str, _ty: &str) -> Result<String> {
    // `txid_snapshot_in` is `pg_snapshot_in`, error message and all.
    let fail = || bad("pg_snapshot", input);
    let mut parts = input.splitn(3, ':');
    let num = |s: Option<&str>| -> Result<u64> {
        let s = s.ok_or_else(fail)?;
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            return Err(fail());
        }
        s.parse::<u64>().map_err(|_| fail())
    };
    let xmin = num(parts.next())?;
    let xmax = num(parts.next())?;
    let rest = parts.next().ok_or_else(fail)?;
    if xmin == 0 || xmax == 0 || xmin > xmax {
        return Err(fail());
    }
    let mut xips: Vec<u64> = Vec::new();
    if !rest.is_empty() {
        for x in rest.split(',') {
            let v = num(Some(x))?;
            if v < xmin || v >= xmax || xips.last().is_some_and(|l| v < *l) {
                return Err(fail());
            }
            if xips.last() != Some(&v) {
                xips.push(v);
            }
        }
    }
    let list: Vec<String> = xips.iter().map(u64::to_string).collect();
    Ok(format!("{xmin}:{xmax}:{}", list.join(",")))
}

/// C's `strtoul(s, NULL, 0)`, which `xidin` / `cidin` / `xid8in` read
/// their text with: leading whitespace, an optional sign, a `0x` (hex) or
/// `0` (octal) prefix, then as many digits as there are -- the rest is
/// ignored, and no digits at all is 0. A negative value wraps; an overflow
/// saturates.
fn strtoul(s: &str) -> u64 {
    let s = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (neg, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (radix, digits) = if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (16, r)
    } else if s.starts_with('0') {
        (8, s)
    } else {
        (10, s)
    };
    let mut v: u64 = 0;
    for c in digits.chars() {
        let Some(d) = c.to_digit(radix) else {
            break;
        };
        v = match v
            .checked_mul(u64::from(radix))
            .and_then(|x| x.checked_add(u64::from(d)))
        {
            Some(x) => x,
            None => return u64::MAX,
        };
    }
    if neg {
        v.wrapping_neg()
    } else {
        v
    }
}

/// `xidin` / `cidin`: a 32-bit counter.
pub fn cast_xid(value: &Bson) -> Bson {
    match value {
        Bson::Null => Bson::Null,
        other => Bson::Int64(i64::from(strtoul(&crate::value_text(other)) as u32)),
    }
}

/// `xid8in`: a 64-bit counter.
pub fn cast_xid8(value: &Bson) -> Bson {
    match value {
        Bson::Null => Bson::Null,
        other => numeric::numeric_bson(&strtoul(&crate::value_text(other)).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macaddr_forms() {
        for s in [
            "08:00:2b:01:02:03",
            "08-00-2b-01-02-03",
            "08002b:010203",
            "08002b-010203",
            "0800.2b01.0203",
            "0800-2b01-0203",
            "08002b010203",
            "8:0:2b:1:2:3",
        ] {
            assert_eq!(parse_macaddr(s).unwrap(), "08:00:2b:01:02:03", "{s}");
        }
        assert!(parse_macaddr("08:00:2b:01:02").is_err());
        assert!(parse_macaddr("0800.2b01.020").is_err());
    }

    #[test]
    fn macaddr8_forms() {
        assert_eq!(
            parse_macaddr8("08:00:2b:01:02:03").unwrap(),
            "08:00:2b:ff:fe:01:02:03"
        );
        assert_eq!(
            parse_macaddr8("0800.2b01.0203.0405").unwrap(),
            "08:00:2b:01:02:03:04:05"
        );
        assert_eq!(
            parse_macaddr8("08002b0102030405").unwrap(),
            "08:00:2b:01:02:03:04:05"
        );
        assert!(parse_macaddr8("08:00-2b:01:02:03").is_err());
        assert_eq!(
            macaddr8_to_macaddr("08:00:2b:ff:fe:01:02:03").unwrap(),
            "08:00:2b:01:02:03"
        );
    }

    #[test]
    fn lsn_and_snapshot() {
        assert_eq!(render_lsn(parse_lsn("16/B374D848").unwrap()), "16/B374D848");
        assert_eq!(render_lsn(parse_lsn("00000016/0000000A").unwrap()), "16/A");
        assert!(parse_lsn("16/").is_err() && parse_lsn(" 1/1").is_err());
        assert_eq!(
            parse_snapshot("10:20:10,14,14,15", "txid_snapshot").unwrap(),
            "10:20:10,14,15"
        );
        assert!(parse_snapshot("0:20:", "pg_snapshot").is_err());
        assert!(parse_snapshot("10:20:21", "pg_snapshot").is_err());
        assert_eq!(strtoul("-1") as u32, u32::MAX);
        assert_eq!(strtoul(" 0x10"), 16);
        assert_eq!(strtoul("abc"), 0);
    }
}
