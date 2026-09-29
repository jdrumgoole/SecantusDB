//! Bit strings: `bit(n)` and `bit varying(n)` (`varbit`).
//!
//! A value is its canonical `'0'`/`'1'` TEXT -- exactly what the Python server
//! stores, so the two servers share the representation (`bitstr.py`). That
//! makes a bit string indistinguishable from text at run time, so every
//! operator here is chosen from the operands' STATIC types, never from the
//! value.
//!
//! Semantics measured against PostgreSQL 14: an explicit cast pads or
//! truncates (`B'1010'::bit(2)` is `10`, `'1010'::bit` is bit(1), so `1`), an
//! assignment refuses a length mismatch (`22026`) or an over-long varbit
//! (`22001`); `&` `|` `#` need equal lengths; `int::bit(n)` keeps the low `n`
//! bits and sign-extends past the integer's width; `bit::int` reads the bits
//! as two's complement and refuses more bits than the integer has.

use bson::Bson;

use crate::{Error, Result};

/// Whether a static type name is a bit-string type.
pub fn is_bit_type(t: &str) -> bool {
    matches!(t, "bit" | "varbit" | "bit varying")
}

fn invalid_binary(c: char) -> Error {
    Error::InvalidText(format!("\"{c}\" is not a valid binary digit"))
}

/// `bit_in`: `B...` / `X...` literal bodies and plain `0`/`1` text.
pub fn parse(text: &str) -> Result<String> {
    let t = text;
    let (hex, body) = match t.chars().next() {
        Some('x' | 'X') => (true, &t[1..]),
        Some('b' | 'B') => (false, &t[1..]),
        _ => (false, t),
    };
    if hex {
        let mut out = String::with_capacity(body.len() * 4);
        for c in body.chars() {
            let v = c.to_digit(16).ok_or_else(|| {
                Error::InvalidText(format!("\"{c}\" is not a valid hexadecimal digit"))
            })?;
            out.push_str(&format!("{v:04b}"));
        }
        return Ok(out);
    }
    if let Some(c) = body.chars().find(|c| !matches!(c, '0' | '1')) {
        return Err(invalid_binary(c));
    }
    Ok(body.to_string())
}

/// An explicit cast to `bit(n)` (pad with zeros or truncate) or `varbit(n)`
/// (truncate). `n` of `None` is `bit(1)` for bit and unbounded for varbit.
pub fn fit_explicit(bits: &str, ty: &str, n: Option<usize>) -> String {
    let varying = ty != "bit";
    let n = match n {
        Some(n) => n,
        None if varying => return bits.to_string(),
        None => 1,
    };
    let mut out: String = bits.chars().take(n).collect();
    if !varying {
        while out.len() < n {
            out.push('0');
        }
    }
    out
}

/// An ASSIGNMENT to a `bit(n)` / `varbit(n)` column: no padding, no
/// truncation -- a mismatch is an error.
pub fn fit_assignment(bits: &str, ty: &str, n: Option<usize>) -> Result<String> {
    match (ty, n) {
        ("bit", Some(n)) if bits.len() != n => Err(Error::Sqlstate(
            "22026",
            format!(
                "bit string length {} does not match type bit({n})",
                bits.len()
            ),
        )),
        ("bit", None) if bits.len() != 1 => Err(Error::Sqlstate(
            "22026",
            format!(
                "bit string length {} does not match type bit(1)",
                bits.len()
            ),
        )),
        (_, Some(n)) if ty != "bit" && bits.len() > n => Err(Error::Sqlstate(
            "22001",
            format!("bit string too long for type bit varying({n})"),
        )),
        _ => Ok(bits.to_string()),
    }
}

/// `bitfromint4` / `bitfromint8`: the low `n` bits, sign-extended past the
/// integer's `width`.
pub fn from_int(v: i64, width: usize, n: usize) -> String {
    (0..n)
        .map(|i| {
            // Bit i from the left of an n-bit string is bit (n-1-i) of v.
            let pos = n - 1 - i;
            let bit = if pos >= width {
                v < 0
            } else {
                (v >> pos) & 1 == 1
            };
            if bit {
                '1'
            } else {
                '0'
            }
        })
        .collect()
}

/// `bittoint4` / `bittoint8`: two's complement over `width` bits.
pub fn to_int(bits: &str, width: usize) -> Result<i64> {
    if bits.len() > width {
        return Err(Error::NumericOutOfRange(
            (if width == 32 {
                "integer out of range"
            } else {
                "bigint out of range"
            })
            .into(),
        ));
    }
    let mut u: u64 = 0;
    for c in bits.chars() {
        u = (u << 1) | u64::from(c == '1');
    }
    Ok(if width == 32 {
        i64::from(u as u32 as i32)
    } else {
        u as i64
    })
}

fn same_size(a: &str, b: &str, verb: &str) -> Result<()> {
    if a.len() != b.len() {
        return Err(Error::Sqlstate(
            "22026",
            format!("cannot {verb} bit strings of different sizes"),
        ));
    }
    Ok(())
}

fn shift(a: &str, n: i64, left: bool) -> String {
    let len = a.len() as i64;
    let (n, left) = if n < 0 { (-n, !left) } else { (n, left) };
    let n = n.min(len) as usize;
    let zeros = "0".repeat(n);
    if left {
        format!("{}{zeros}", &a[n..])
    } else {
        format!("{zeros}{}", &a[..a.len() - n])
    }
}

/// A binary operator over bit strings, or `None` when `op` is not one.
pub fn binary(op: &str, a: &Bson, b: &Bson) -> Option<Result<Bson>> {
    if *a == Bson::Null || *b == Bson::Null {
        return matches!(op, "&" | "|" | "#" | "<<" | ">>" | "||").then_some(Ok(Bson::Null));
    }
    let text = |v: &Bson| crate::value_text(v);
    let zip = |a: &str, b: &str, f: fn(bool, bool) -> bool| -> String {
        a.chars()
            .zip(b.chars())
            .map(|(x, y)| if f(x == '1', y == '1') { '1' } else { '0' })
            .collect()
    };
    let (x, y) = (text(a), text(b));
    let out = match op {
        "&" => same_size(&x, &y, "AND").map(|_| zip(&x, &y, |p, q| p && q)),
        "|" => same_size(&x, &y, "OR").map(|_| zip(&x, &y, |p, q| p || q)),
        "#" => same_size(&x, &y, "XOR").map(|_| zip(&x, &y, |p, q| p != q)),
        "||" => Ok(format!("{x}{y}")),
        "<<" | ">>" => {
            let n = match b {
                Bson::Int32(i) => i64::from(*i),
                Bson::Int64(i) => *i,
                _ => return None,
            };
            Ok(shift(&x, n, op == "<<"))
        }
        _ => return None,
    };
    Some(out.map(Bson::String))
}

/// `~b`.
pub fn not(a: &Bson) -> Bson {
    match a {
        Bson::Null => Bson::Null,
        other => Bson::String(
            crate::value_text(other)
                .chars()
                .map(|c| if c == '1' { '0' } else { '1' })
                .collect(),
        ),
    }
}

/// The bit-string functions, given the static type of the first argument.
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if args.contains(&Bson::Null) {
        return Some(Ok(Bson::Null));
    }
    let s = |i: usize| crate::value_text(&args[i]);
    let int = |i: usize| match &args[i] {
        Bson::Int32(v) => Some(i64::from(*v)),
        Bson::Int64(v) => Some(*v),
        _ => None,
    };
    Some(Ok(match (name, args.len()) {
        ("length" | "bit_length", 1) => Bson::Int32(s(0).len() as i32),
        ("octet_length", 1) => Bson::Int32(s(0).len().div_ceil(8) as i32),
        ("bit_count", 1) => Bson::Int64(s(0).chars().filter(|c| *c == '1').count() as i64),
        ("get_bit", 2) => {
            let b = s(0);
            let i = int(1)?;
            if i < 0 || i >= b.len() as i64 {
                return Some(Err(Error::Sqlstate(
                    "2202E",
                    format!(
                        "bit index {i} out of valid range (0..{})",
                        b.len() as i64 - 1
                    ),
                )));
            }
            Bson::Int32(i32::from(b.as_bytes()[i as usize] == b'1'))
        }
        ("set_bit", 3) => {
            let b = s(0);
            let i = int(1)?;
            let v = int(2)?;
            if i < 0 || i >= b.len() as i64 {
                return Some(Err(Error::Sqlstate(
                    "2202E",
                    format!(
                        "bit index {i} out of valid range (0..{})",
                        b.len() as i64 - 1
                    ),
                )));
            }
            if v != 0 && v != 1 {
                return Some(Err(Error::Sqlstate(
                    "22023",
                    "new bit must be 0 or 1".into(),
                )));
            }
            let mut bytes = b.into_bytes();
            bytes[i as usize] = if v == 1 { b'1' } else { b'0' };
            Bson::String(String::from_utf8(bytes).unwrap_or_default())
        }
        ("position" | "strpos", 2) => {
            // `position(sub IN s)` arrives as `position(s, sub)`.
            let (hay, needle) = (s(0), s(1));
            Bson::Int32(if needle.is_empty() {
                1
            } else {
                hay.find(&needle).map_or(0, |p| p as i32 + 1)
            })
        }
        ("substring" | "substr", 2 | 3) => {
            let b = s(0);
            let start = int(1)?;
            let len = if args.len() == 3 {
                int(2)?
            } else {
                i64::MAX / 4
            };
            if len < 0 {
                return Some(Err(Error::Sqlstate(
                    "22011",
                    "negative substring length not allowed".into(),
                )));
            }
            let from = start.max(1);
            let to = start.saturating_add(len).min(b.len() as i64 + 1);
            Bson::String(if to <= from {
                String::new()
            } else {
                b[(from - 1) as usize..(to - 1) as usize].to_string()
            })
        }
        ("overlay", 3 | 4) => {
            let (b, p) = (s(0), s(1));
            let from = int(2)?;
            let len = if args.len() == 4 {
                int(3)?
            } else {
                p.len() as i64
            };
            let from = from.max(1) as usize;
            let head: String = b.chars().take(from - 1).collect();
            let tail: String = b.chars().skip(from - 1 + len.max(0) as usize).collect();
            Bson::String(format!("{head}{p}{tail}"))
        }
        _ => return None,
    }))
}

/// A bit function's result type.
pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "length" | "bit_length" | "octet_length" | "get_bit" | "position" | "strpos" => "int4",
        "bit_count" => "int8",
        "set_bit" | "substring" | "substr" | "overlay" => "varbit",
        _ => return None,
    })
}

/// `varbit_send`: an int32 bit count, then the bits packed MSB-first.
pub fn to_wire(bits: &str) -> Vec<u8> {
    let mut out = (bits.len() as i32).to_be_bytes().to_vec();
    for chunk in bits.as_bytes().chunks(8) {
        let mut byte = 0u8;
        for (i, b) in chunk.iter().enumerate() {
            if *b == b'1' {
                byte |= 0x80 >> i;
            }
        }
        out.push(byte);
    }
    out
}

/// `varbit_recv`: the inverse of [`to_wire`].
pub fn from_wire(bytes: &[u8]) -> Option<String> {
    let n = usize::try_from(i32::from_be_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    let data = bytes.get(4..)?;
    (0..n)
        .map(|i| {
            let byte = *data.get(i / 8)?;
            Some(if byte & (0x80 >> (i % 8)) != 0 {
                '1'
            } else {
                '0'
            })
        })
        .collect()
}
