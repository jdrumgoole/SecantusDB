//! The input functions of the types a stored rule's constants hold, to the
//! datum bytes `outDatum` prints: PostgreSQL's in-memory representation on
//! a little-endian, 8-byte-Datum machine.

use super::catalog::*;
use super::nodes::{Datum, Oid};
use super::parse::Analyzer;
use super::{unsupported, AnalysisError, R};

/// A varlena with a 4-byte header.
fn varlena(data: &[u8]) -> Datum {
    let size = (data.len() + 4) as u32;
    let mut out = (size << 2).to_le_bytes().to_vec();
    out.extend_from_slice(data);
    Datum::ByRef(out)
}

fn byval_i64(v: i64, len: u32) -> Datum {
    Datum::from_i64(v, len)
}

/// `stringTypeDatum`: `type`'s input function on `s`.
pub(crate) fn type_input(a: &Analyzer<'_>, t: Oid, s: &str, typmod: i32) -> R<Datum> {
    let cat = a.cat;
    match t {
        BOOLOID => Ok(byval_i64(i64::from(bool_in(s)?), 1)),
        CHAROID => {
            let b = s.as_bytes();
            let c: u8 = if b.len() == 4
                && b[0] == b'\\'
                && b[1..].iter().all(|x| (b'0'..=b'7').contains(x))
            {
                ((b[1] - b'0') << 6) | ((b[2] - b'0') << 3) | (b[3] - b'0')
            } else {
                b.first().copied().unwrap_or(0)
            };
            Ok(byval_i64(i64::from(c as i8), 1))
        }
        NAMEOID => {
            let mut out = vec![0u8; 64];
            let mut n = s.len().min(63);
            while !s.is_char_boundary(n) {
                n -= 1;
            }
            out[..n].copy_from_slice(&s.as_bytes()[..n]);
            Ok(Datum::ByRef(out))
        }
        TEXTOID | VARCHAROID | BPCHAROID => Ok(varlena(s.as_bytes())),
        BYTEAOID => {
            let b = crate::bytea::parse_text(s).map_err(|_| bad(s, "bytea"))?;
            Ok(varlena(&b))
        }
        DATEOID => date_in(s),
        TIMEOID => {
            let text = crate::parse_time(s).map_err(|_| bad(s, "time without time zone"))?;
            let micros = time_text_micros(&text).ok_or_else(|| bad(s, "time without time zone"))?;
            Ok(byval_i64(adjust_time_for_typmod(micros, typmod), 8))
        }
        TIMESTAMPOID => {
            let unix =
                crate::parse_timestamp(s).map_err(|_| bad(s, "timestamp without time zone"))?;
            Ok(byval_i64(
                adjust_timestamp_for_typmod(unix - PG_EPOCH_MICROS, typmod),
                8,
            ))
        }
        TIMESTAMPTZOID => {
            let unix = crate::parse_timestamptz(s, &crate::session_timezone())
                .map_err(|_| bad(s, "timestamp with time zone"))?;
            Ok(byval_i64(
                adjust_timestamp_for_typmod(unix - PG_EPOCH_MICROS, typmod),
                8,
            ))
        }
        UUIDOID => {
            let canon = crate::parse_uuid(s.trim()).ok_or_else(|| bad(s, "uuid"))?;
            let hex: String = canon.chars().filter(|c| *c != '-').collect();
            let bytes = (0..16)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
                .map_err(|_| bad(s, "uuid"))?;
            Ok(Datum::ByRef(bytes))
        }
        INT2OID => {
            let v: i16 = s.trim().parse().map_err(|_| bad(s, "smallint"))?;
            Ok(byval_i64(v.into(), 2))
        }
        INT4OID => {
            let v: i32 = s.trim().parse().map_err(|_| bad(s, "integer"))?;
            Ok(byval_i64(v.into(), 4))
        }
        INT8OID => {
            let v: i64 = s.trim().parse().map_err(|_| bad(s, "bigint"))?;
            Ok(byval_i64(v, 8))
        }
        OIDOID => {
            let v: u32 = s.trim().parse().map_err(|_| bad(s, "oid"))?;
            Ok(byval_i64(i64::from(v), 4))
        }
        FLOAT4OID => {
            let v: f32 = parse_float(s)?.0 as f32;
            Ok(byval_i64(i64::from(v.to_bits() as i32), 4))
        }
        FLOAT8OID => {
            let v = parse_float(s)?.0;
            Ok(byval_i64(v.to_bits() as i64, 8))
        }
        NUMERICOID => numeric_in(s, typmod),
        // interval_in: `Interval {TimeOffset time; int32 day; int32 month}`,
        // 16 bytes passed by reference. A typmod narrows the value
        // (AdjustIntervalForTypmod), which is not transcribed.
        INTERVALOID => {
            if typmod != -1 {
                return unsupported("an interval literal with a typmod in a stored rule");
            }
            interval_in(s)
        }
        1560 | 1562 => bit_in(s, typmod),
        REGCLASSOID => {
            let oid = if let Ok(n) = s.trim().parse::<u32>() {
                n
            } else {
                let names = parse_qualified_identifier(s)?;
                let rv = pg_query::protobuf::RangeVar {
                    schemaname: if names.len() > 1 {
                        names[names.len() - 2].clone()
                    } else {
                        String::new()
                    },
                    relname: names.last().cloned().unwrap_or_default(),
                    inh: true,
                    ..Default::default()
                };
                a.range_var_get_relid(&rv)?
            };
            Ok(byval_i64(i64::from(oid), 4))
        }
        REGTYPEOID => {
            let oid = if let Ok(n) = s.trim().parse::<u32>() {
                n
            } else {
                let names = parse_qualified_identifier(s)?;
                a.lookup_type_name_oid(&names, false)?
            };
            Ok(byval_i64(i64::from(oid), 4))
        }
        REGNAMESPACEOID => {
            let oid = if let Ok(n) = s.trim().parse::<u32>() {
                n
            } else {
                let names = parse_qualified_identifier(s)?;
                cat.namespace_oid(names.last().map_or("", String::as_str))
                    .ok_or_else(|| AnalysisError(format!("schema \"{s}\" does not exist")))?
            };
            Ok(byval_i64(i64::from(oid), 4))
        }
        REGPROCOID => {
            let oid = if let Ok(n) = s.trim().parse::<u32>() {
                n
            } else {
                let names = parse_qualified_identifier(s)?;
                let (schema, name) = super::func::deconstruct_qualified_name(&names)?;
                let mut found = cat.procs_named(name).filter(|p| match schema {
                    Some(sc) => cat.namespace_oid(sc) == Some(p.nsp),
                    None => a.path.contains(&p.nsp),
                });
                let first = found
                    .next()
                    .ok_or_else(|| AnalysisError(format!("function \"{s}\" does not exist")))?;
                if found.next().is_some() {
                    return unsupported(format!("more than one function named \"{s}\""));
                }
                first.oid
            };
            Ok(byval_i64(i64::from(oid), 4))
        }
        _ => {
            if let Some(row) = cat.type_row(t) {
                if row.len == -1 && row.subscript == F_ARRAY_SUBSCRIPT_HANDLER && row.elem != 0 {
                    return array_in(a, s, row.elem);
                }
            }
            unsupported(format!("a literal of type {t} in a stored rule"))
        }
    }
}

fn bad(s: &str, t: &str) -> AnalysisError {
    AnalysisError(format!("invalid input syntax for type {t}: \"{s}\""))
}

fn bool_in(s: &str) -> R<bool> {
    let l = s.trim().to_ascii_lowercase();
    match l.as_str() {
        "t" | "tr" | "tru" | "true" | "y" | "ye" | "yes" | "on" | "1" => Ok(true),
        "f" | "fa" | "fal" | "fals" | "false" | "n" | "no" | "of" | "off" | "0" => Ok(false),
        _ => Err(bad(s, "boolean")),
    }
}

fn parse_float(s: &str) -> R<(f64,)> {
    let t = s.trim().to_ascii_lowercase();
    let v = match t.as_str() {
        "nan" => f64::NAN,
        "infinity" | "inf" | "+infinity" | "+inf" => f64::INFINITY,
        "-infinity" | "-inf" => f64::NEG_INFINITY,
        _ => t.parse().map_err(|_| bad(s, "double precision"))?,
    };
    Ok((v,))
}

/// `stringToQualifiedNameList`.
fn parse_qualified_identifier(s: &str) -> R<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = s.trim().chars().peekable();
    loop {
        let mut part = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    Some('"') => {
                        if chars.peek() == Some(&'"') {
                            chars.next();
                            part.push('"');
                        } else {
                            break;
                        }
                    }
                    Some(c) => part.push(c),
                    None => return unsupported("an unterminated quoted identifier"),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == '.' {
                    break;
                }
                part.push(c.to_ascii_lowercase());
                chars.next();
            }
            part = part.trim().to_string();
        }
        out.push(part);
        match chars.next() {
            Some('.') => continue,
            None => break,
            Some(_) => return unsupported("an improper qualified name"),
        }
    }
    Ok(out)
}

/// `numeric_in` (typmod -1).
/// Microseconds between the Unix epoch and PostgreSQL's (2000-01-01).
const PG_EPOCH_MICROS: i64 = 946_684_800_000_000;

/// `date_in`: days since 2000-01-01, `DATEVAL_NOBEGIN` / `DATEVAL_NOEND`
/// for the infinities.
fn date_in(s: &str) -> R<Datum> {
    let text = crate::parse_date(s).map_err(|_| bad(s, "date"))?;
    let days: i32 = match text.as_str() {
        "infinity" => i32::MAX,
        "-infinity" => i32::MIN,
        t => {
            let (body, bc) = match t.strip_suffix(" BC") {
                Some(b) => (b, true),
                None => (t, false),
            };
            let mut parts = body.splitn(3, '-');
            let mut next = || parts.next().and_then(|p| p.parse::<i64>().ok());
            let (y, m, d) = match (next(), next(), next()) {
                (Some(y), Some(m), Some(d)) => (y, m, d),
                _ => return Err(bad(s, "date")),
            };
            let y = if bc { 1 - y } else { y };
            let jd = date2j(y, m, d) - date2j(2000, 1, 1);
            i32::try_from(jd).map_err(|_| bad(s, "date"))?
        }
    };
    Ok(byval_i64(i64::from(days), 4))
}

/// `date2j`: the Julian day number of a proleptic Gregorian date.
fn date2j(mut y: i64, mut m: i64, d: i64) -> i64 {
    if m > 2 {
        m += 1;
        y += 4800;
    } else {
        m += 13;
        y += 4799;
    }
    let century = y / 100;
    let mut julian = y * 365 - 32167;
    julian += y / 4 - century + century / 4;
    julian += 7834 * m / 256 + d;
    julian
}

/// `HH:MM:SS[.ffffff]` as microseconds since midnight.
fn time_text_micros(t: &str) -> Option<i64> {
    let mut it = t.splitn(3, ':');
    let h: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let sec = it.next().unwrap_or("0");
    let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
    let s: i64 = whole.parse().ok()?;
    let mut us = 0i64;
    if !frac.is_empty() {
        let digits: String = frac.chars().chain(std::iter::repeat('0')).take(6).collect();
        us = digits.parse().ok()?;
    }
    Some(((h * 60 + m) * 60 + s) * 1_000_000 + us)
}

/// `AdjustTimestampForTypmod`: round to the declared fractional digits.
fn adjust_timestamp_for_typmod(t: i64, typmod: i32) -> i64 {
    if !(0..=5).contains(&typmod) || t == i64::MAX || t == i64::MIN {
        return t;
    }
    let scale = 10i64.pow(6 - typmod as u32);
    let offset = scale / 2;
    if t >= 0 {
        ((t + offset) / scale) * scale
    } else {
        -((((-t) + offset) / scale) * scale)
    }
}

/// `AdjustTimeForTypmod`.
fn adjust_time_for_typmod(t: i64, typmod: i32) -> i64 {
    if !(0..=5).contains(&typmod) {
        return t;
    }
    let scale = 10i64.pow(6 - typmod as u32);
    ((t + scale / 2) / scale) * scale
}

/// `interval_in` without a typmod: the 16-byte `Interval` struct.
pub(crate) fn interval_in(s: &str) -> R<Datum> {
    let iv = crate::parse_interval(s).map_err(|_| bad(s, "interval"))?;
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&iv.micros.to_le_bytes());
    out.extend_from_slice(&iv.days.to_le_bytes());
    out.extend_from_slice(&iv.months.to_le_bytes());
    Ok(Datum::ByRef(out))
}

pub(crate) fn numeric_in(s: &str, typmod: i32) -> R<Datum> {
    if typmod >= 0 {
        return unsupported("a numeric literal with a type modifier");
    }
    let t = s.trim();
    let bytes = t.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        neg = bytes[i] == b'-';
        i += 1;
    }
    let lower = t[i..].to_ascii_lowercase();
    if lower == "nan" {
        // NUMERIC_NAN, short form special.
        return Ok(varlena(&0xC000u16.to_le_bytes()));
    }
    let mut decdigits: Vec<u8> = vec![0; 4];
    let mut dweight: i32 = -1;
    let mut dscale: i32 = 0;
    let mut have_dp = false;
    if i < bytes.len() && bytes[i] == b'.' {
        have_dp = true;
        i += 1;
    }
    if i >= bytes.len() || !bytes[i].is_ascii_digit() {
        return Err(bad(s, "numeric"));
    }
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            decdigits.push(c - b'0');
            if !have_dp {
                dweight += 1;
            } else {
                dscale += 1;
            }
            i += 1;
        } else if c == b'.' {
            if have_dp {
                return Err(bad(s, "numeric"));
            }
            have_dp = true;
            i += 1;
        } else {
            break;
        }
    }
    let ddigits = decdigits.len() as i32 - 4;
    decdigits.extend_from_slice(&[0, 0, 0]);
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let exp: i32 = t[i + 1..].parse().map_err(|_| bad(s, "numeric"))?;
        dweight += exp;
        dscale -= exp;
        if dscale < 0 {
            dscale = 0;
        }
    } else if i < bytes.len() {
        return Err(bad(s, "numeric"));
    }
    let weight = if dweight >= 0 {
        (dweight + 1 + 3) / 4 - 1
    } else {
        -((-dweight - 1) / 4 + 1)
    };
    let offset = (weight + 1) * 4 - (dweight + 1);
    let ndigits = (ddigits + offset + 3) / 4;
    let mut digits: Vec<i16> = Vec::new();
    let mut idx = (4 - offset) as usize;
    for _ in 0..ndigits {
        let d = |k: usize| i16::from(*decdigits.get(k).unwrap_or(&0));
        digits.push(((d(idx) * 10 + d(idx + 1)) * 10 + d(idx + 2)) * 10 + d(idx + 3));
        idx += 4;
    }
    Ok(make_numeric(neg, weight, dscale, digits))
}

/// `make_result`.
fn make_numeric(neg: bool, mut weight: i32, dscale: i32, digits: Vec<i16>) -> Datum {
    let mut start = 0;
    let mut n = digits.len();
    while n > 0 && digits[start] == 0 {
        start += 1;
        weight -= 1;
        n -= 1;
    }
    while n > 0 && digits[start + n - 1] == 0 {
        n -= 1;
    }
    let mut neg = neg;
    if n == 0 {
        weight = 0;
        neg = false;
    }
    let mut data = Vec::new();
    if dscale <= 63 && (-64..=63).contains(&weight) {
        let mut h: u16 = 0x8000;
        if neg {
            h |= 0x2000;
        }
        h |= ((dscale as u16) << 7) & 0x1F80;
        if weight < 0 {
            h |= 0x0040;
        }
        h |= (weight as u16) & 0x003F;
        data.extend_from_slice(&h.to_le_bytes());
    } else {
        let sign: u16 = if neg { 0x4000 } else { 0 };
        let h = sign | (dscale as u16 & 0x3FFF);
        data.extend_from_slice(&h.to_le_bytes());
        data.extend_from_slice(&(weight as i16).to_le_bytes());
    }
    for d in &digits[start..start + n] {
        data.extend_from_slice(&d.to_le_bytes());
    }
    varlena(&data)
}

/// `bit_in`.
pub(crate) fn bit_in(s: &str, typmod: i32) -> R<Datum> {
    let (hex, body) = if let Some(r) = s.strip_prefix(['b', 'B']) {
        (false, r)
    } else if let Some(r) = s.strip_prefix(['x', 'X']) {
        (true, r)
    } else {
        (false, s)
    };
    let mut bits = Vec::new();
    for c in body.chars() {
        if hex {
            let v = c.to_digit(16).ok_or_else(|| bad(s, "bit"))?;
            for k in (0..4).rev() {
                bits.push(((v >> k) & 1) as u8);
            }
        } else {
            match c {
                '0' => bits.push(0),
                '1' => bits.push(1),
                _ => return Err(bad(s, "bit")),
            }
        }
    }
    if typmod > 0 && bits.len() as i32 != typmod {
        return unsupported("bit string length does not match type");
    }
    let mut data = (bits.len() as i32).to_le_bytes().to_vec();
    for chunk in bits.chunks(8) {
        let mut b = 0u8;
        for (k, bit) in chunk.iter().enumerate() {
            b |= bit << (7 - k);
        }
        data.push(b);
    }
    Ok(varlena(&data))
}

fn align(n: usize, a: char) -> usize {
    let al = match a {
        'd' => 8,
        'i' => 4,
        's' => 2,
        _ => 1,
    };
    n.div_ceil(al) * al
}

/// `array_in` for a one-dimensional array without NULLs.
fn array_in(a: &Analyzer<'_>, s: &str, elem: Oid) -> R<Datum> {
    let t = s.trim();
    let inner = t
        .strip_prefix('{')
        .and_then(|x| x.strip_suffix('}'))
        .ok_or_else(|| bad(s, "array"))?;
    let mut items: Vec<String> = Vec::new();
    if !inner.trim().is_empty() {
        let mut cur = String::new();
        let mut quoted = false;
        let mut was_quoted = false;
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                '"' => {
                    quoted = !quoted;
                    was_quoted = true;
                }
                ',' if !quoted => {
                    items.push(finish_item(&cur, was_quoted)?);
                    cur.clear();
                    was_quoted = false;
                }
                '{' | '}' if !quoted => return unsupported("a multidimensional array literal"),
                c => cur.push(c),
            }
        }
        items.push(finish_item(&cur, was_quoted)?);
    }
    let row = a
        .cat
        .type_row(elem)
        .ok_or_else(|| AnalysisError("an array element type".into()))?
        .clone();
    let ndim: i32 = if items.is_empty() { 0 } else { 1 };
    let mut data = Vec::new();
    for it in &items {
        let d = type_input(a, elem, it, -1)?;
        let bytes: Vec<u8> = match d {
            Datum::ByVal { bytes, .. } => bytes[..row.len as usize].to_vec(),
            Datum::ByRef(b) => b,
        };
        let pos = align(data.len(), row.align);
        data.resize(pos, 0);
        data.extend_from_slice(&bytes);
    }
    let header = if ndim == 0 { 16 } else { align(16 + 8, 'd') };
    let total = header + data.len();
    let mut out = Vec::new();
    out.extend_from_slice(&((total as u32) << 2).to_le_bytes());
    out.extend_from_slice(&ndim.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&elem.to_le_bytes());
    if ndim == 1 {
        out.extend_from_slice(&(items.len() as i32).to_le_bytes());
        out.extend_from_slice(&1i32.to_le_bytes());
    }
    out.resize(header, 0);
    out.extend_from_slice(&data);
    Ok(Datum::ByRef(out))
}

fn finish_item(s: &str, quoted: bool) -> R<String> {
    let v = if quoted {
        s.to_string()
    } else {
        s.trim().to_string()
    };
    if !quoted && v.eq_ignore_ascii_case("null") {
        return unsupported("a NULL array element in a stored rule");
    }
    Ok(v)
}
