//! Scalar built-in functions.
//!
//! Types matter as much as values here. PostgreSQL's `length` answers `int4`,
//! its `exp` answers `float8`, and `abs` gives back whatever it was handed —
//! and the two rounding families disagree, exactly as they do in casts:
//! `round` on a `numeric` goes half AWAY FROM ZERO while on a `float8` it goes
//! half TO EVEN. Every case here was measured against PostgreSQL 14 rather
//! than assumed.

use crate::{compare_constants, decimal_arith, parse_numeric, Error, Result};
use bson::Bson;

/// Is this a scalar built-in this server implements?
pub fn is_scalar(name: &str) -> bool {
    SCALAR_NAMES.contains(&name)
}

const SCALAR_NAMES: &[&str] = &[
    "upper",
    "lower",
    "initcap",
    "length",
    "char_length",
    "character_length",
    "octet_length",
    "bit_length",
    "btrim",
    "trim",
    "ltrim",
    "rtrim",
    "substr",
    "substring",
    "replace",
    "repeat",
    "reverse",
    "left",
    "right",
    "strpos",
    "position",
    "concat",
    "concat_ws",
    "md5",
    "chr",
    "ascii",
    "split_part",
    "starts_with",
    "abs",
    "ceil",
    "ceiling",
    "floor",
    "round",
    "trunc",
    "sqrt",
    "exp",
    "ln",
    "log",
    "log10",
    "power",
    "pow",
    "mod",
    "sign",
    "div",
    "greatest",
    "least",
];

fn text(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => d.to_string(),
        Bson::Decimal128(d) => crate::plain_numeric_text(&d.to_string()),
        Bson::Boolean(b) => (if *b { "true" } else { "false" }).to_string(),
        other => format!("{other:?}"),
    }
}

fn as_f64(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Double(d) => Some(*d),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        _ => None,
    }
}

fn as_i64(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(*d as i64),
        Bson::Decimal128(d) => d.to_string().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

fn wrong_args(name: &str) -> Error {
    Error::Parse(format!(
        "function {name} does not exist with that argument list"
    ))
}

/// A one-based, clamped slice of a string by CHARACTERS, which is how
/// PostgreSQL's `substring` counts: a start before 1 does not shift the text,
/// it consumes part of the requested length.
fn substring(s: &str, start: i64, len: Option<i64>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let end = match len {
        Some(l) => start.saturating_add(l),
        None => i64::MAX,
    };
    chars
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let pos = *i as i64 + 1;
            pos >= start && (len.is_none() || pos < end)
        })
        .map(|(_, c)| *c)
        .collect()
}

/// Evaluate a scalar built-in. `None` means "not one of ours".
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !is_scalar(name) {
        return None;
    }
    Some(eval(name, args))
}

fn eval(name: &str, args: &[Bson]) -> Result<Bson> {
    // Most scalar built-ins are NULL-propagating: given a NULL argument the
    // answer is NULL, not an error. Four are not, and all four IGNORE a NULL
    // argument instead: `concat` and `concat_ws` skip them, and `greatest` /
    // `least` pick the extreme of what remains, so `greatest(1, NULL)` is 1.
    if !matches!(name, "concat" | "concat_ws" | "greatest" | "least")
        && args.iter().any(|a| a == &Bson::Null)
    {
        return Ok(Bson::Null);
    }
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Bson::Null);
    let s = |i: usize| text(&arg(i));
    let need = |n: usize| -> Result<()> {
        if args.len() == n {
            Ok(())
        } else {
            Err(wrong_args(name))
        }
    };

    match name {
        "upper" => {
            need(1)?;
            Ok(Bson::String(s(0).to_uppercase()))
        }
        "lower" => {
            need(1)?;
            Ok(Bson::String(s(0).to_lowercase()))
        }
        "initcap" => {
            need(1)?;
            let mut out = String::new();
            let mut fresh = true;
            for c in s(0).chars() {
                if c.is_alphanumeric() {
                    if fresh {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    fresh = false;
                } else {
                    out.push(c);
                    fresh = true;
                }
            }
            Ok(Bson::String(out))
        }
        // `length` counts CHARACTERS; `octet_length` counts bytes. They differ
        // the moment the text is not ASCII.
        "length" | "char_length" | "character_length" => {
            need(1)?;
            Ok(Bson::Int32(s(0).chars().count() as i32))
        }
        "octet_length" => {
            need(1)?;
            Ok(Bson::Int32(s(0).len() as i32))
        }
        "bit_length" => {
            need(1)?;
            Ok(Bson::Int32((s(0).len() * 8) as i32))
        }
        "btrim" | "trim" | "ltrim" | "rtrim" => {
            if args.is_empty() || args.len() > 2 {
                return Err(wrong_args(name));
            }
            let subject = s(0);
            let set: Vec<char> = if args.len() == 2 {
                s(1).chars().collect()
            } else {
                vec![' ']
            };
            let trimmed = match name {
                "ltrim" => subject.trim_start_matches(|c| set.contains(&c)).to_string(),
                "rtrim" => subject.trim_end_matches(|c| set.contains(&c)).to_string(),
                _ => subject.trim_matches(|c| set.contains(&c)).to_string(),
            };
            Ok(Bson::String(trimmed))
        }
        "substr" | "substring" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(wrong_args(name));
            }
            let start = as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?;
            let len = if args.len() == 3 {
                let l = as_i64(&arg(2)).ok_or_else(|| wrong_args(name))?;
                if l < 0 {
                    return Err(Error::InvalidText(
                        "negative substring length not allowed".into(),
                    ));
                }
                Some(l)
            } else {
                None
            };
            Ok(Bson::String(substring(&s(0), start, len)))
        }
        "replace" => {
            need(3)?;
            Ok(Bson::String(s(0).replace(&s(1), &s(2))))
        }
        "repeat" => {
            need(2)?;
            let n = as_i64(&arg(1)).unwrap_or(0).max(0) as usize;
            Ok(Bson::String(s(0).repeat(n)))
        }
        "reverse" => {
            need(1)?;
            Ok(Bson::String(s(0).chars().rev().collect()))
        }
        "left" | "right" => {
            need(2)?;
            let chars: Vec<char> = s(0).chars().collect();
            let n = as_i64(&arg(1)).unwrap_or(0);
            // A negative count means "all but this many from the other end".
            let take = if n >= 0 {
                (n as usize).min(chars.len())
            } else {
                chars.len().saturating_sub(n.unsigned_abs() as usize)
            };
            let out: String = if name == "left" {
                chars.iter().take(take).collect()
            } else {
                chars.iter().skip(chars.len() - take).collect()
            };
            Ok(Bson::String(out))
        }
        // 1-based, and 0 when absent.
        "strpos" | "position" => {
            need(2)?;
            let (haystack, needle) = (s(0), s(1));
            Ok(Bson::Int32(match haystack.find(&needle) {
                Some(byte_idx) => haystack[..byte_idx].chars().count() as i32 + 1,
                None => 0,
            }))
        }
        "concat" => Ok(Bson::String(
            args.iter()
                .filter(|a| *a != &Bson::Null)
                .map(text)
                .collect::<Vec<_>>()
                .join(""),
        )),
        "concat_ws" => {
            if args.is_empty() {
                return Err(wrong_args(name));
            }
            let sep = s(0);
            Ok(Bson::String(
                args[1..]
                    .iter()
                    .filter(|a| *a != &Bson::Null)
                    .map(text)
                    .collect::<Vec<_>>()
                    .join(&sep),
            ))
        }
        "md5" => {
            need(1)?;
            Ok(Bson::String(md5_hex(s(0).as_bytes())))
        }
        "chr" => {
            need(1)?;
            let n = as_i64(&arg(0)).ok_or_else(|| wrong_args(name))?;
            let c = u32::try_from(n)
                .ok()
                .filter(|n| *n != 0)
                .and_then(char::from_u32)
                .ok_or_else(|| {
                    Error::InvalidText(format!("requested character too large for encoding: {n}"))
                })?;
            Ok(Bson::String(c.to_string()))
        }
        "ascii" => {
            need(1)?;
            Ok(Bson::Int32(s(0).chars().next().map_or(0, |c| c as i32)))
        }
        "split_part" => {
            need(3)?;
            let n = as_i64(&arg(2)).unwrap_or(0);
            let subject = s(0);
            let sep = s(1);
            let parts: Vec<&str> = subject.split(&sep as &str).collect();
            let idx = if n > 0 {
                (n - 1) as usize
            } else {
                return Err(Error::InvalidText(
                    "field position must be greater than zero".into(),
                ));
            };
            Ok(Bson::String(
                parts.get(idx).copied().unwrap_or("").to_string(),
            ))
        }
        "starts_with" => {
            need(2)?;
            Ok(Bson::Boolean(s(0).starts_with(&s(1))))
        }
        // --- numeric -------------------------------------------------------
        // `abs` gives back the type it was handed, so an exact numeric stays
        // exact rather than becoming a float.
        "abs" => {
            need(1)?;
            Ok(match arg(0) {
                Bson::Int32(i) => Bson::Int32(i.abs()),
                Bson::Int64(i) => Bson::Int64(i.abs()),
                Bson::Double(d) => Bson::Double(d.abs()),
                Bson::Decimal128(d) => {
                    let t = d.to_string();
                    Bson::Decimal128(parse_numeric(t.strip_prefix('-').unwrap_or(&t))?)
                }
                other => return Err(Error::Unsupported(format!("abs of {other:?}"))),
            })
        }
        "sign" => {
            need(1)?;
            let f = as_f64(&arg(0)).ok_or_else(|| wrong_args(name))?;
            let out = if f > 0.0 {
                1
            } else if f < 0.0 {
                -1
            } else {
                0
            };
            // `sign` answers `float8` for a float or an integer, and `numeric`
            // only when handed one -- so `sign(-3)` is `-1.0`, not `-1`.
            Ok(match arg(0) {
                Bson::Decimal128(_) => {
                    Bson::Decimal128(parse_numeric(&out.to_string()).expect("a one-digit decimal"))
                }
                _ => Bson::Double(f64::from(out)),
            })
        }
        "ceil" | "ceiling" | "floor" | "trunc" | "round" => numeric_rounding(name, args),
        "sqrt" | "exp" | "ln" | "log" | "log10" | "power" | "pow" => float_math(name, args),
        "mod" => {
            need(2)?;
            match (arg(0), arg(1)) {
                (_, b) if as_f64(&b) == Some(0.0) => Err(Error::DivisionByZero),
                (Bson::Int32(a), b) => Ok(Bson::Int32(
                    a % i32::try_from(as_i64(&b).unwrap_or(1)).unwrap_or(1),
                )),
                (a, b) => {
                    let (x, y) = (
                        as_i64(&a).ok_or_else(|| wrong_args(name))?,
                        as_i64(&b).ok_or_else(|| wrong_args(name))?,
                    );
                    Ok(Bson::Int64(x % y))
                }
            }
        }
        // `div` is defined on `numeric`, so integer arguments are coerced and
        // the answer is a `numeric` -- not the `int8` the arithmetic suggests.
        "div" => {
            need(2)?;
            let (a, b) = (
                as_i64(&arg(0)).ok_or_else(|| wrong_args(name))?,
                as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?,
            );
            if b == 0 {
                return Err(Error::DivisionByZero);
            }
            Ok(Bson::Decimal128(parse_numeric(&(a / b).to_string())?))
        }
        "greatest" | "least" => {
            if args.is_empty() {
                return Err(wrong_args(name));
            }
            // NULLs are IGNORED here, so an all-NULL call answers NULL and a
            // mixed one answers the extreme of the non-NULLs.
            let mut best: Option<Bson> = None;
            for a in args.iter().filter(|a| *a != &Bson::Null) {
                best = Some(match best {
                    None => a.clone(),
                    Some(cur) => {
                        let take = match compare_constants(a, &cur) {
                            Some(o) => {
                                (name == "greatest") == (o == std::cmp::Ordering::Greater)
                                    && o != std::cmp::Ordering::Equal
                            }
                            None => false,
                        };
                        if take {
                            a.clone()
                        } else {
                            cur
                        }
                    }
                });
            }
            Ok(best.unwrap_or(Bson::Null))
        }
        _ => Err(Error::Unsupported(format!("function {name}()"))),
    }
}

/// `ceil` / `floor` / `trunc` / `round`, which keep an exact input exact.
///
/// `round(numeric)` goes half AWAY FROM ZERO and `round(float8)` goes half TO
/// EVEN — the same split the integer casts have, and the same trap.
fn numeric_rounding(name: &str, args: &[Bson]) -> Result<Bson> {
    let subject = args.first().cloned().unwrap_or(Bson::Null);
    // `round(x, n)` is numeric-only in PostgreSQL and keeps n decimal places.
    if args.len() == 2 {
        if name != "round" && name != "trunc" {
            return Err(wrong_args(name));
        }
        let places = as_i64(&args[1]).unwrap_or(0);
        let text = text(&subject);
        return round_decimal_text(&text, places, name == "round")
            .map(Bson::Decimal128)
            .ok_or_else(|| wrong_args(name));
    }
    if args.len() != 1 {
        return Err(wrong_args(name));
    }
    Ok(match subject {
        Bson::Int32(_) | Bson::Int64(_) => subject,
        Bson::Double(d) => Bson::Double(match name {
            "ceil" | "ceiling" => d.ceil(),
            "floor" => d.floor(),
            "trunc" => d.trunc(),
            _ => d.round_ties_even(),
        }),
        Bson::Decimal128(_) => {
            let t = text(&subject);
            let places = 0;
            let out = match name {
                "ceil" | "ceiling" => decimal_ceil_floor(&t, true),
                "floor" => decimal_ceil_floor(&t, false),
                "trunc" => round_decimal_text(&t, places, false),
                _ => round_decimal_text(&t, places, true),
            };
            Bson::Decimal128(out.ok_or_else(|| wrong_args(name))?)
        }
        other => return Err(Error::Unsupported(format!("{name} of {other:?}"))),
    })
}

/// Round or truncate a decimal to `places`, on the DIGITS. Rounding is half
/// away from zero, which is what PostgreSQL does for `numeric`.
fn round_decimal_text(text: &str, places: i64, round: bool) -> Option<bson::Decimal128> {
    let (neg, body) = match text.trim().strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, text.trim()),
    };
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    let places = places.max(0) as usize;
    let mut digits: Vec<u8> = format!("{int_part}{frac_part}").into_bytes();
    let frac_len = frac_part.len();
    if places >= frac_len {
        // Nothing to remove; pad so the scale is exactly `places`.
        let mut out = String::new();
        if neg {
            out.push('-');
        }
        out.push_str(int_part);
        if places > 0 {
            out.push('.');
            out.push_str(&format!("{frac_part:0<places$}"));
        }
        return parse_numeric(&out).ok();
    }
    let drop = frac_len - places;
    let keep = digits.len() - drop;
    let round_up = round && digits.get(keep).is_some_and(|d| *d >= b'5');
    digits.truncate(keep);
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let s: String = String::from_utf8(digits).ok()?;
    let split = s.len().saturating_sub(places);
    let (whole, frac) = s.split_at(split);
    let whole = if whole.is_empty() { "0" } else { whole };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(whole);
    if places > 0 {
        out.push('.');
        out.push_str(frac);
    }
    parse_numeric(&out).ok()
}

fn decimal_ceil_floor(text: &str, up: bool) -> Option<bson::Decimal128> {
    let truncated = round_decimal_text(text, 0, false)?;
    let neg = text.trim_start().starts_with('-');
    let has_fraction = text
        .split_once('.')
        .is_some_and(|(_, f)| f.chars().any(|c| c != '0'));
    if !has_fraction {
        return Some(truncated);
    }
    let adjust = if up { !neg } else { neg };
    if !adjust {
        return Some(truncated);
    }
    let one = if neg { "-1" } else { "1" };
    match decimal_arith("+", &truncated.to_string(), one) {
        Some(Ok(Bson::Decimal128(d))) => Some(d),
        _ => None,
    }
}

/// The float-valued family. PostgreSQL answers `float8` for all of these when
/// given a float or an integer, and `numeric` only where the input is one --
/// which for `ln` / `log` / `sqrt` it keeps, so those are left to the float
/// path here and their exactness is not claimed.
fn float_math(name: &str, args: &[Bson]) -> Result<Bson> {
    let f =
        |i: usize| -> Result<f64> { args.get(i).and_then(as_f64).ok_or_else(|| wrong_args(name)) };
    let out = match name {
        "sqrt" => {
            let x = f(0)?;
            if x < 0.0 {
                return Err(Error::InvalidText(
                    "cannot take square root of a negative number".into(),
                ));
            }
            x.sqrt()
        }
        "exp" => f(0)?.exp(),
        "ln" => {
            let x = f(0)?;
            if x <= 0.0 {
                return Err(Error::InvalidText(
                    "cannot take logarithm of a non-positive number".into(),
                ));
            }
            x.ln()
        }
        "log" | "log10" if args.len() == 1 => {
            let x = f(0)?;
            if x <= 0.0 {
                return Err(Error::InvalidText(
                    "cannot take logarithm of a non-positive number".into(),
                ));
            }
            x.log10()
        }
        "log" => f(1)?.log(f(0)?),
        "power" | "pow" => f(0)?.powf(f(1)?),
        _ => return Err(Error::Unsupported(format!("function {name}()"))),
    };
    Ok(Bson::Double(out))
}

/// MD5, for `md5()`. Small enough to carry rather than take a dependency for.
fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}
