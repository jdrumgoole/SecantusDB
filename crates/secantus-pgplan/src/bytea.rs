//! PostgreSQL `bytea`: literal parsing, text rendering, and the byte-level
//! accessor / codec functions.
//!
//! A `bytea` is a `Bson::Binary` (subtype Generic) once stored — the same
//! representation the Python server writes, since the two share one store.
//! Two input text forms are accepted, matching PostgreSQL: **hex** (`\x` then
//! hex digit pairs, whitespace between pairs ignored) and **escape**
//! (printable bytes verbatim, `\\` for one backslash byte, `\ooo` octal).
//! Default OUTPUT is always the `\x` hex form modern PostgreSQL emits.
//!
//! Error surface measured against PostgreSQL 14:
//! - a bad hex digit / odd hex length is `22023`,
//! - a bad escape is `22P02`,
//! - a `get_byte` / `set_byte` index out of range is `2202E`,
//! - an unrecognized `encode` / `decode` format is `22023`.

use crate::{Error, Result};
use bson::{spec::BinarySubtype, Binary, Bson};

/// A byte string this server carries as `bytea`. Recovers raw bytes from a
/// stored `Bson::Binary` or parses either text form.
pub fn parse(value: &Bson) -> Result<Vec<u8>> {
    match value {
        Bson::Binary(b) => Ok(b.bytes.clone()),
        Bson::String(s) => parse_text(s),
        Bson::Null => Ok(Vec::new()),
        other => Err(Error::InvalidText(format!(
            "invalid input syntax for type bytea: cannot use {other:?}"
        ))),
    }
}

/// Parse a `bytea` literal (hex `\x…` or escape form) into raw bytes.
pub fn parse_text(s: &str) -> Result<Vec<u8>> {
    if let Some(rest) = s.strip_prefix("\\x").or_else(|| s.strip_prefix("\\X")) {
        return parse_hex(rest);
    }
    parse_escape(s)
}

/// Hex pairs with any interspersed whitespace ignored.
fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for ch in s.chars() {
        if ch.is_whitespace() {
            continue;
        }
        let nib = ch.to_digit(16).ok_or_else(|| {
            Error::InvalidParameter(format!("invalid hexadecimal digit: \"{ch}\""))
        })? as u8;
        match hi.take() {
            None => hi = Some(nib),
            Some(h) => out.push((h << 4) | nib),
        }
    }
    if hi.is_some() {
        return Err(Error::InvalidParameter(
            "invalid hexadecimal data: odd number of digits".into(),
        ));
    }
    Ok(out)
}

/// Escape form: printable bytes verbatim, `\\` → one 0x5C, `\ooo` octal.
fn parse_escape(s: &str) -> Result<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                out.push(0x5C);
                i += 2;
                continue;
            }
            if i + 3 < bytes.len()
                && bytes[i + 1..i + 4]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                let oct =
                    (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
                out.push(oct);
                i += 4;
                continue;
            }
            return Err(Error::InvalidText(
                "invalid input syntax for type bytea".into(),
            ));
        }
        out.push(bytes[i]);
        i += 1;
    }
    Ok(out)
}

/// The `\x…` hex text PostgreSQL emits for a `bytea` under `bytea_output = hex`.
pub fn render_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("\\x");
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Wrap raw bytes as the stored `bytea` value.
pub fn to_binary(bytes: Vec<u8>) -> Bson {
    Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes,
    })
}

/// `encode(bytea, fmt)` — bytes to text in `hex` / `base64` / `escape`.
pub fn encode(bytes: &[u8], fmt: &str) -> Result<String> {
    match fmt.to_ascii_lowercase().as_str() {
        "hex" => Ok(bytes.iter().map(|b| format!("{b:02x}")).collect()),
        "base64" => Ok(base64_encode(bytes)),
        "escape" => Ok(render_escape(bytes)),
        other => Err(Error::InvalidParameter(format!(
            "unrecognized encoding: \"{other}\""
        ))),
    }
}

/// `decode(text, fmt)` — text to bytes in `hex` / `base64` / `escape`.
pub fn decode(text: &str, fmt: &str) -> Result<Vec<u8>> {
    match fmt.to_ascii_lowercase().as_str() {
        "hex" => parse_hex(text),
        "base64" => base64_decode(text),
        "escape" => parse_escape(text),
        other => Err(Error::InvalidParameter(format!(
            "unrecognized encoding: \"{other}\""
        ))),
    }
}

/// `get_byte(bytea, n)` — the 0-based n-th byte.
pub fn get_byte(bytes: &[u8], n: i64) -> Result<i32> {
    if n < 0 || n as usize >= bytes.len() {
        return Err(index_err(n, bytes.len()));
    }
    Ok(i32::from(bytes[n as usize]))
}

/// `set_byte(bytea, n, v)` — a copy with the n-th byte replaced.
pub fn set_byte(bytes: &[u8], n: i64, v: i64) -> Result<Vec<u8>> {
    if n < 0 || n as usize >= bytes.len() {
        return Err(index_err(n, bytes.len()));
    }
    let mut out = bytes.to_vec();
    out[n as usize] = (v & 0xFF) as u8;
    Ok(out)
}

fn index_err(n: i64, len: usize) -> Error {
    // PostgreSQL: `index N out of valid range, 0..M` where M = len-1 (2202E).
    Error::ArraySubscript(format!(
        "index {n} out of valid range, 0..{}",
        len.saturating_sub(1)
    ))
}

/// PostgreSQL's `encode(...,'escape')`: octal-escape only NUL and high-bit
/// bytes (and `\`), leaving every other byte — control bytes included — raw.
/// This is NOT the bytea text-output escaping; measured against PG 14.
fn render_escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b == 0x5C {
            out.push_str("\\\\");
        } else if b == 0x00 || b >= 0x80 {
            out.push_str(&format!("\\{b:03o}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a' + 26)),
            b'0'..=b'9' => Some(u32::from(c - b'0' + 52)),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut acc = 0u32;
    let mut bits = 0;
    let mut out = Vec::new();
    for c in s.bytes() {
        if c.is_ascii_whitespace() || c == b'=' {
            continue;
        }
        let v = val(c).ok_or_else(|| Error::InvalidParameter("invalid base64".into()))?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}
