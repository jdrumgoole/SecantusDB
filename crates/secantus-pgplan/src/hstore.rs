//! The `hstore` extension type: PostgreSQL's key/value map.
//!
//! A value is carried as its CANONICAL text -- what `hstore_out` prints --
//! so a stored value is already the text the wire sends and the text
//! `COPY ... TO STDOUT` writes. The canonical form (measured against
//! PostgreSQL 16 / hstore 1.8):
//!
//! * every key and value is double-quoted, a NULL value is the bare word
//!   `NULL`, pairs are joined by `, `;
//! * `"` and `\` inside a key or value are backslash-escaped;
//! * pairs are sorted by key LENGTH then by key bytes -- the order the
//!   extension keeps them in, not lexical order -- and of duplicate keys the
//!   FIRST wins.
//!
//! The parser is `hstore_io.c`'s state machine (`parse_hstore` / `get_val`)
//! transcribed: unquoted tokens, whitespace around `=>` and `,`, a trailing
//! comma, backslash escaping any next character, and an UNQUOTED
//! case-insensitive `null` meaning NULL. The two error messages are
//! PostgreSQL's, position included -- a BYTE offset, as `ptr - begin` is.

use crate::{Error, Result};

/// The pairs of an hstore in canonical order (length, then bytes), first
/// duplicate kept. `None` is a NULL value.
pub type Pairs = Vec<(String, Option<String>)>;

/// Parse hstore text into canonical pairs.
pub fn parse(text: &str) -> Result<Pairs> {
    let b = text.as_bytes();
    let mut pairs: Vec<(String, Option<String>)> = Vec::new();
    let mut ptr = 0usize;
    let mut key = String::new();
    // parse_hstore's WKEY / WEQ / WGT / WVAL / WDEL.
    let mut st = 0u8;
    loop {
        match st {
            0 => {
                let Some((word, _escaped)) = get_val(b, &mut ptr, false)? else {
                    break;
                };
                key = word;
                st = 1;
            }
            1 => match at(b, ptr) {
                Some(b'=') => st = 2,
                None => return Err(unexpected_end()),
                Some(c) if !c.is_ascii_whitespace() => return Err(syntax_near(b, ptr)),
                _ => {}
            },
            2 => match at(b, ptr) {
                Some(b'>') => st = 3,
                None => return Err(unexpected_end()),
                _ => return Err(syntax_near(b, ptr)),
            },
            3 => {
                let Some((word, escaped)) = get_val(b, &mut ptr, true)? else {
                    return Err(unexpected_end());
                };
                let value = if !escaped && word.eq_ignore_ascii_case("null") {
                    None
                } else {
                    Some(word)
                };
                pairs.push((std::mem::take(&mut key), value));
                st = 4;
            }
            _ => match at(b, ptr) {
                Some(b',') => st = 0,
                None => break,
                Some(c) if !c.is_ascii_whitespace() => return Err(syntax_near(b, ptr)),
                _ => {}
            },
        }
        ptr += 1;
    }
    Ok(unique(pairs))
}

fn at(b: &[u8], i: usize) -> Option<u8> {
    b.get(i).copied()
}

fn unexpected_end() -> Error {
    Error::Parse("syntax error in hstore: unexpected end of string".into())
}

fn syntax_near(b: &[u8], pos: usize) -> Error {
    // `%.*s` with pg_mblen: the whole character at that byte.
    let rest = &b[pos..];
    let ch = std::str::from_utf8(rest)
        .ok()
        .or_else(|| {
            std::str::from_utf8(&rest[..rest.len().min(4)])
                .ok()
                .or_else(|| {
                    (1..=rest.len().min(4)).find_map(|n| std::str::from_utf8(&rest[..n]).ok())
                })
        })
        .and_then(|s| s.chars().next())
        .map(|c| c.to_string())
        .unwrap_or_default();
    Error::Parse(format!(
        "syntax error in hstore, near \"{ch}\" at position {pos}"
    ))
}

/// `get_val`: read one key or value. `Ok(None)` is end-of-string before any
/// token (only legal for a key). The flag says the token was quoted.
fn get_val(b: &[u8], ptr: &mut usize, ignoreeq: bool) -> Result<Option<(String, bool)>> {
    // GV_WAITVAL / GV_INVAL / GV_INESCVAL / GV_WAITESCIN / GV_WAITESCESCIN
    let mut st = 0u8;
    let mut word: Vec<u8> = Vec::new();
    let mut escaped = false;
    loop {
        let c = at(b, *ptr);
        match st {
            0 => match c {
                Some(b'"') => {
                    escaped = true;
                    st = 2;
                }
                None => return Ok(None),
                Some(b'=') if !ignoreeq => return Err(syntax_near(b, *ptr)),
                Some(b'\\') => st = 3,
                Some(ch) if !ch.is_ascii_whitespace() => {
                    word.push(ch);
                    st = 1;
                }
                _ => {}
            },
            1 => match c {
                Some(b'\\') => st = 3,
                Some(b'=') if !ignoreeq => {
                    *ptr -= 1;
                    return Ok(Some((finish(word), escaped)));
                }
                Some(b',') if ignoreeq => {
                    *ptr -= 1;
                    return Ok(Some((finish(word), escaped)));
                }
                Some(ch) if ch.is_ascii_whitespace() => {
                    return Ok(Some((finish(word), escaped)));
                }
                None => {
                    *ptr -= 1;
                    return Ok(Some((finish(word), escaped)));
                }
                Some(ch) => word.push(ch),
            },
            2 => match c {
                Some(b'\\') => st = 4,
                Some(b'"') => return Ok(Some((finish(word), escaped))),
                None => return Err(unexpected_end()),
                Some(ch) => word.push(ch),
            },
            3 => match c {
                None => return Err(unexpected_end()),
                Some(ch) => {
                    word.push(ch);
                    st = 1;
                }
            },
            _ => match c {
                None => return Err(unexpected_end()),
                Some(ch) => {
                    word.push(ch);
                    st = 2;
                }
            },
        }
        *ptr += 1;
    }
}

fn finish(word: Vec<u8>) -> String {
    // The input was a &str, and escapes copy whole bytes, so every token is
    // a byte-slice of valid UTF-8 (a backslash before a multibyte lead byte
    // copies the lead and the continuation bytes follow through INVAL).
    String::from_utf8(word).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// `hstoreUniquePairs`: order by key length then bytes, keep the first of
/// each key.
pub fn unique(pairs: Pairs) -> Pairs {
    let mut out: Pairs = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        if out.iter().any(|(ok, _)| *ok == k) {
            continue;
        }
        out.push((k, v));
    }
    out.sort_by(|(a, _), (b, _)| {
        a.len()
            .cmp(&b.len())
            .then_with(|| a.as_bytes().cmp(b.as_bytes()))
    });
    out
}

fn push_quoted(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

/// `hstore_out`.
pub fn render(pairs: &Pairs) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        push_quoted(&mut out, k);
        out.push_str("=>");
        match v {
            Some(v) => push_quoted(&mut out, v),
            None => out.push_str("NULL"),
        }
    }
    out
}

/// Parse and re-render: the canonical text of any hstore input.
pub fn canonical(text: &str) -> Result<String> {
    Ok(render(&parse(text)?))
}

/// `hstore_send`: `int32 count`, then per pair `int32 keylen, key, int32
/// vallen (-1 for NULL), value`.
pub fn to_binary(pairs: &Pairs) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(pairs.len() as i32).to_be_bytes());
    for (k, v) in pairs {
        out.extend_from_slice(&(k.len() as i32).to_be_bytes());
        out.extend_from_slice(k.as_bytes());
        match v {
            Some(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                out.extend_from_slice(v.as_bytes());
            }
            None => out.extend_from_slice(&(-1i32).to_be_bytes()),
        }
    }
    out
}

/// `hstore_recv`: the layout above back to canonical pairs.
pub fn from_binary(bytes: &[u8]) -> Result<Pairs> {
    let bad = || Error::InvalidText("insufficient data left in message".into());
    let mut pos = 0usize;
    let read_i32 = |pos: &mut usize| -> Result<i32> {
        let s = bytes.get(*pos..*pos + 4).ok_or_else(bad)?;
        *pos += 4;
        Ok(i32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    let count = read_i32(&mut pos)?;
    if count < 0 {
        return Err(Error::InvalidText(format!(
            "invalid number of pairs {count}"
        )));
    }
    let mut pairs = Vec::with_capacity(count as usize);
    let read_str = |pos: &mut usize, len: i32| -> Result<String> {
        let len = usize::try_from(len).map_err(|_| bad())?;
        let s = bytes.get(*pos..*pos + len).ok_or_else(bad)?;
        *pos += len;
        String::from_utf8(s.to_vec())
            .map_err(|_| Error::InvalidText("invalid byte sequence for encoding \"UTF8\"".into()))
    };
    for _ in 0..count {
        let klen = read_i32(&mut pos)?;
        if klen < 0 {
            return Err(Error::InvalidText(
                "null value not allowed for hstore key".into(),
            ));
        }
        let key = read_str(&mut pos, klen)?;
        let vlen = read_i32(&mut pos)?;
        let value = if vlen < 0 {
            None
        } else {
            Some(read_str(&mut pos, vlen)?)
        };
        pairs.push((key, value));
    }
    Ok(unique(pairs))
}

/// `hstore -> text`: the value at a key, NULL when absent or NULL.
pub fn get(pairs: &Pairs, key: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.clone())
}

/// `hstore ? text`.
pub fn has_key(pairs: &Pairs, key: &str) -> bool {
    pairs.iter().any(|(k, _)| k == key)
}

/// `hstore || hstore`: the RIGHT side's value wins for a shared key.
pub fn concat(left: &Pairs, right: &Pairs) -> Pairs {
    let mut merged: Pairs = right.clone();
    merged.extend(left.iter().cloned());
    unique(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> String {
        canonical(s).unwrap()
    }

    fn err(s: &str) -> String {
        canonical(s).unwrap_err().to_string()
    }

    #[test]
    fn canonical_forms_measured_on_16() {
        assert_eq!(c(""), "");
        assert_eq!(c("   "), "");
        assert_eq!(c("a=>1"), "\"a\"=>\"1\"");
        assert_eq!(c("a => b"), "\"a\"=>\"b\"");
        assert_eq!(c("a=>1,"), "\"a\"=>\"1\"");
        assert_eq!(c("a=>null"), "\"a\"=>NULL");
        assert_eq!(c("a=>NULL"), "\"a\"=>NULL");
        assert_eq!(c("a=>\"null\""), "\"a\"=>\"null\"");
        assert_eq!(c("a=>b=c"), "\"a\"=>\"b=c\"");
        assert_eq!(
            c("b=>2, a=>1, aa=>3"),
            "\"a\"=>\"1\", \"b\"=>\"2\", \"aa\"=>\"3\""
        );
        assert_eq!(c("a=>1, a=>2"), "\"a\"=>\"1\"");
        assert_eq!(c(r#""a\\"=>"1""#), r#""a\\"=>"1""#);
        assert_eq!(c(r#""a\""=>"1""#), r#""a\""=>"1""#);
        assert_eq!(
            c("\"a\"=>\"'\", \"'\"=>\"2\""),
            "\"'\"=>\"2\", \"a\"=>\"'\""
        );
    }

    #[test]
    fn errors_measured_on_16() {
        assert_eq!(err("a"), "syntax error in hstore: unexpected end of string");
        assert_eq!(
            err("\"a\""),
            "syntax error in hstore: unexpected end of string"
        );
        assert_eq!(
            err("a=>"),
            "syntax error in hstore: unexpected end of string"
        );
        assert_eq!(
            err("\"a=>\"1\""),
            "syntax error in hstore, near \"1\" at position 5"
        );
        assert_eq!(
            err("a==>b"),
            "syntax error in hstore, near \"=\" at position 2"
        );
        assert_eq!(
            err("a=>1 b=>2"),
            "syntax error in hstore, near \"b\" at position 5"
        );
    }

    #[test]
    fn binary_round_trip() {
        let pairs = parse("a=>1, b=>NULL").unwrap();
        let bytes = to_binary(&pairs);
        assert_eq!(
            bytes,
            b"\x00\x00\x00\x02\x00\x00\x00\x01a\x00\x00\x00\x011\x00\x00\x00\x01b\xff\xff\xff\xff"
        );
        assert_eq!(from_binary(&bytes).unwrap(), pairs);
    }
}
