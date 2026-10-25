//! The database's encoding, as a session in it works in (RP-064): the
//! text a byte function (`octet_length`, `::bytea`, `md5`, `chr`, `ascii`)
//! reads is the text's bytes in the server encoding, which a database
//! created with `ENCODING 'LATIN1'` keeps in place of UTF-8. Text is held as
//! Unicode here whatever the encoding; its server bytes are made when a
//! function asks for them.

use std::borrow::Cow;
use std::cell::RefCell;

thread_local! {
    static SERVER_ENCODING: RefCell<String> = RefCell::new("UTF8".to_string());
}

/// The eight LATIN9 positions that differ from LATIN1: `(byte, code point)`.
const LATIN9_DIFFS: &[(u8, char)] = &[
    (0xA4, '\u{20AC}'),
    (0xA6, '\u{0160}'),
    (0xA8, '\u{0161}'),
    (0xB4, '\u{017D}'),
    (0xB8, '\u{017E}'),
    (0xBC, '\u{0152}'),
    (0xBD, '\u{0153}'),
    (0xBE, '\u{0178}'),
];

/// Install the session's server encoding (its canonical name).
pub fn set_server_encoding(name: &str) {
    SERVER_ENCODING.with(|e| {
        let mut e = e.borrow_mut();
        if *e != name {
            *e = name.to_string();
        }
    });
}

/// The session's server encoding.
pub fn server_encoding() -> String {
    SERVER_ENCODING.with(|e| e.borrow().clone())
}

fn is_utf8(name: &str) -> bool {
    matches!(name, "UTF8" | "SQL_ASCII")
}

/// Is the server encoding one with characters of more than one byte
/// (`pg_encoding_max_length > 1`)?
pub fn multi_byte() -> bool {
    SERVER_ENCODING.with(|e| {
        let name = e.borrow();
        name.as_str() == "UTF8" || crate::multi_byte_encoding(&name)
    })
}

/// `s` as the server encoding's bytes. A character it has no byte for
/// cannot be here: the client's text was refused on its way in (22P05).
pub fn bytes(s: &str) -> Cow<'_, [u8]> {
    SERVER_ENCODING.with(|e| {
        let name = e.borrow();
        if is_utf8(&name) || s.is_ascii() {
            return Cow::Borrowed(s.as_bytes());
        }
        Cow::Owned(encode(&name, s).unwrap_or_else(|_| s.as_bytes().to_vec()))
    })
}

/// `s` in encoding `name`, or the first character it has no bytes for.
pub fn encode(name: &str, s: &str) -> Result<Vec<u8>, char> {
    if is_utf8(name) {
        return Ok(s.as_bytes().to_vec());
    }
    if name == "LATIN1" {
        return s
            .chars()
            .map(|c| u8::try_from(c as u32).map_err(|_| c))
            .collect();
    }
    if name == "LATIN9" {
        return s
            .chars()
            .map(|c| {
                if let Some((b, _)) = LATIN9_DIFFS.iter().find(|(_, ch)| *ch == c) {
                    return Ok(*b);
                }
                if LATIN9_DIFFS.iter().any(|(b, _)| u32::from(*b) == c as u32) {
                    return Err(c);
                }
                u8::try_from(c as u32).map_err(|_| c)
            })
            .collect();
    }
    if let Some(table) = crate::single_byte_table(name) {
        return s
            .chars()
            .map(|c| {
                if c.is_ascii() {
                    return Ok(c as u8);
                }
                table
                    .iter()
                    .position(|&u| u != 0 && u32::from(u) == c as u32)
                    .and_then(|i| u8::try_from(i + 0x80).ok())
                    .ok_or(c)
            })
            .collect();
    }
    if crate::multi_byte_encoding(name) {
        return crate::encode_multi_byte(name, s);
    }
    Ok(s.as_bytes().to_vec())
}

/// The character byte `b` is in a single-byte server encoding, as `chr`
/// makes it.
pub fn char_of_byte(b: u8) -> Option<char> {
    if b.is_ascii() {
        return Some(char::from(b));
    }
    SERVER_ENCODING.with(|e| {
        let name = e.borrow();
        match name.as_str() {
            "LATIN1" => Some(char::from(b)),
            "LATIN9" => Some(
                LATIN9_DIFFS
                    .iter()
                    .find(|(x, _)| *x == b)
                    .map_or(char::from(b), |(_, c)| *c),
            ),
            other => crate::single_byte_table(other)
                .and_then(|t| t.get(usize::from(b - 0x80)).copied())
                .filter(|u| *u != 0)
                .and_then(|u| char::from_u32(u32::from(u))),
        }
    })
}
