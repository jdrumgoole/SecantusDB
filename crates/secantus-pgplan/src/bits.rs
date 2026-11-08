//! The `bit(n)` and `bit varying(n)` types: a string of `0` / `1` digits,
//! tagged so it is distinct from text (`'101'::text::int` is 101 where
//! `b'101'::int` is 5) and so a fixed-width value is told from a varying one
//! (`pg_typeof` answers `bit` for one and `bit varying` for the other).
//!
//! Every rule here is measured on PostgreSQL 16: a `bit(n)` cast pads with
//! zeros on the RIGHT and truncates on the right (`'1'::bit(3)` is `100`,
//! `b'101'::bit(2)` is `10`); a `varbit(n)` cast truncates and never pads;
//! an integer casts to its two's-complement bits, low bits kept when the
//! target is narrower and zero-extended when wider (`5::bit(4)` is `0101`,
//! `(-1)::bit(4)` is `1111`, `256::bit(8)` is `00000000`); a bit string casts
//! to an integer as an unsigned value of its width, which is why
//! `x'FFFFFFFF'::int` is -1 and `x'FFFFFFFF'::int8` is 4294967295.

use bson::{Bson, Document};

use crate::{Error, Result};

/// The tag key of a fixed-width `bit(n)` value.
pub const BIT_KEY: &str = "__bit";
/// The tag key of a `bit varying` value.
pub const VARBIT_KEY: &str = "__varbit";

/// A bit-string value from its digits.
pub fn bit_value(digits: String, varying: bool) -> Bson {
    let mut d = Document::new();
    d.insert(
        if varying { VARBIT_KEY } else { BIT_KEY },
        Bson::String(digits),
    );
    Bson::Document(d)
}

/// The digits behind a bit-string value and whether it is varying, or `None`
/// for any other value.
pub fn bit_digits(v: &Bson) -> Option<(&str, bool)> {
    match v {
        Bson::Document(d) if d.len() == 1 => d
            .get_str(BIT_KEY)
            .ok()
            .map(|s| (s, false))
            .or_else(|| d.get_str(VARBIT_KEY).ok().map(|s| (s, true))),
        _ => None,
    }
}

/// `bit_in` / `varbit_in`: the digits of a `0`/`1` string, or PostgreSQL's
/// 22P02 naming the first offending character.
pub fn parse_digits(text: &str) -> Result<String> {
    for ch in text.chars() {
        if ch != '0' && ch != '1' {
            return Err(Error::InvalidText(format!(
                "\"{ch}\" is not a valid binary digit"
            )));
        }
    }
    Ok(text.to_string())
}

/// A `b'...'` / `x'...'` literal, as the parser hands it over (the prefix
/// letter followed by the digits): hex digits expand to four bits each.
pub fn literal_digits(literal: &str) -> Result<String> {
    let mut chars = literal.chars();
    let kind = chars.next().map(|c| c.to_ascii_lowercase());
    let body: &str = &literal[1..];
    match kind {
        Some('b') => parse_digits(body),
        Some('x') => {
            let mut out = String::with_capacity(body.len() * 4);
            for ch in body.chars() {
                let n = ch.to_digit(16).ok_or_else(|| {
                    Error::InvalidText(format!("\"{ch}\" is not a valid hexadecimal digit"))
                })?;
                out.push_str(&format!("{n:04b}"));
            }
            Ok(out)
        }
        _ => Err(Error::Unsupported("this bit-string literal".into())),
    }
}

/// Resize a digit string to a declared width: `bit(n)` pads with zeros on
/// the right and truncates; `varbit(n)` only truncates.
pub fn resize(digits: &str, length: Option<usize>, varying: bool) -> String {
    match length {
        None => digits.to_string(),
        Some(n) => {
            let mut out: String = digits.chars().take(n).collect();
            if !varying {
                while out.len() < n {
                    out.push('0');
                }
            }
            out
        }
    }
}

/// An integer's two's-complement bits at the source width (32 for `int4`,
/// 64 for `int8`), the low `n` kept when the target is narrower and zero
/// bits added on the left when it is wider.
pub fn from_integer(value: i64, source_width: usize, length: usize) -> String {
    let bits: String = if source_width == 32 {
        format!("{:032b}", value as i32 as u32)
    } else {
        format!("{:064b}", value as u64)
    };
    if length <= source_width {
        bits[source_width - length..].to_string()
    } else {
        format!("{}{bits}", "0".repeat(length - source_width))
    }
}

/// A bit string's integer value at the target width: `int4` reads up to 32
/// bits as a signed word, `int8` up to 64; more digits than the width holds
/// is `22003`.
pub fn to_integer(digits: &str, width: usize) -> Result<i64> {
    if digits.len() > width {
        return Err(Error::NumericOutOfRange(if width == 32 {
            "integer out of range".into()
        } else {
            "bigint out of range".into()
        }));
    }
    let raw = digits
        .chars()
        .fold(0u64, |acc, c| (acc << 1) | u64::from(c == '1'));
    Ok(if width == 32 {
        i64::from(raw as u32 as i32)
    } else {
        raw as i64
    })
}

/// The bitwise operators. `&`, `|` and `#` need equal widths (22026);
/// `<<` / `>>` shift within the width, filling with zeros; `||` appends and
/// answers a varying string whenever either side is one.
pub fn binary(op: &str, lhs: &Bson, rhs: &Bson) -> Option<Result<Bson>> {
    let (a, a_var) = bit_digits(lhs)?;
    match op {
        "<<" | ">>" => {
            let n = match rhs {
                Bson::Int32(n) => i64::from(*n),
                Bson::Int64(n) => *n,
                _ => return None,
            };
            let width = a.len();
            let shifted = if n.unsigned_abs() as usize >= width {
                "0".repeat(width)
            } else if (op == "<<") == (n >= 0) {
                let k = n.unsigned_abs() as usize;
                format!("{}{}", &a[k..], "0".repeat(k))
            } else {
                let k = n.unsigned_abs() as usize;
                format!("{}{}", "0".repeat(k), &a[..width - k])
            };
            Some(Ok(bit_value(shifted, a_var)))
        }
        "&" | "|" | "#" | "||" => {
            let (b, b_var) = bit_digits(rhs)?;
            if op == "||" {
                return Some(Ok(bit_value(format!("{a}{b}"), a_var || b_var)));
            }
            if a.len() != b.len() {
                let verb = match op {
                    "&" => "AND",
                    "|" => "OR",
                    _ => "XOR",
                };
                return Some(Err(Error::StringDataLengthMismatch(format!(
                    "cannot {verb} bit strings of different sizes"
                ))));
            }
            let out: String = a
                .chars()
                .zip(b.chars())
                .map(|(x, y)| {
                    let (x, y) = (x == '1', y == '1');
                    let r = match op {
                        "&" => x && y,
                        "|" => x || y,
                        _ => x ^ y,
                    };
                    if r {
                        '1'
                    } else {
                        '0'
                    }
                })
                .collect();
            Some(Ok(bit_value(out, a_var || b_var)))
        }
        _ => None,
    }
}

/// `~b'101'` is `010`.
pub fn not(v: &Bson) -> Option<Bson> {
    let (a, var) = bit_digits(v)?;
    let out: String = a
        .chars()
        .map(|c| if c == '1' { '0' } else { '1' })
        .collect();
    Some(bit_value(out, var))
}
