//! The text functions `builtins` answers that need no regular expression:
//! the encoding conversions (`convert_to`, `convert`, `length(bytea, name)`)
//! and `parse_ident`.

use bson::Bson;

use crate::{Error, Result};

/// The encodings a conversion here knows: UTF8, and the single-byte ones
/// whose every octet is its own code point.
pub(crate) enum Encoding {
    Utf8,
    Latin1,
    Ascii,
}

pub(crate) fn encoding(name: &str, role: &str) -> Result<(Encoding, String)> {
    let key = name.to_ascii_uppercase().replace(['-', '_', ' '], "");
    match key.as_str() {
        "UTF8" | "UNICODE" => Ok((Encoding::Utf8, "UTF8".into())),
        "LATIN1" | "ISO88591" => Ok((Encoding::Latin1, "LATIN1".into())),
        "SQLASCII" => Ok((Encoding::Ascii, "SQL_ASCII".into())),
        _ => Err(Error::Sqlstate(
            "22023",
            format!("invalid {role}encoding name \"{name}\""),
        )),
    }
}

pub(crate) fn invalid_utf8(bytes: &[u8], at: usize) -> Error {
    // PostgreSQL names the bytes of the sequence the leading byte announces.
    let lead = bytes[at];
    let want = if lead >= 0xF0 {
        4
    } else if lead >= 0xE0 {
        3
    } else if lead >= 0xC0 {
        2
    } else {
        1
    };
    let shown: Vec<String> = bytes[at..(at + want).min(bytes.len())]
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect();
    Error::Sqlstate(
        "22021",
        format!(
            "invalid byte sequence for encoding \"UTF8\": {}",
            shown.join(" ")
        ),
    )
}

pub(crate) fn decode(bytes: &[u8], from: &Encoding) -> Result<String> {
    match from {
        Encoding::Utf8 => match std::str::from_utf8(bytes) {
            Ok(s) if !s.contains('\0') => Ok(s.to_string()),
            Ok(_) => Err(Error::Sqlstate(
                "22021",
                "invalid byte sequence for encoding \"UTF8\": 0x00".into(),
            )),
            Err(e) => Err(invalid_utf8(bytes, e.valid_up_to())),
        },
        Encoding::Latin1 => Ok(bytes.iter().map(|b| *b as char).collect()),
        Encoding::Ascii => Ok(String::from_utf8_lossy(bytes).into_owned()),
    }
}

pub(crate) fn encode(s: &str, to: &Encoding, to_name: &str) -> Result<Vec<u8>> {
    match to {
        Encoding::Utf8 | Encoding::Ascii => Ok(s.as_bytes().to_vec()),
        Encoding::Latin1 => s
            .chars()
            .map(|c| {
                u8::try_from(u32::from(c)).map_err(|_| {
                    let mut buf = [0u8; 4];
                    let shown: Vec<String> = c
                        .encode_utf8(&mut buf)
                        .bytes()
                        .map(|b| format!("0x{b:02x}"))
                        .collect();
                    Error::Sqlstate(
                        "22P05",
                        format!(
                            "character with byte sequence {} in encoding \"UTF8\" has no \
                             equivalent in encoding \"{to_name}\"",
                            shown.join(" ")
                        ),
                    )
                })
            })
            .collect(),
    }
}

fn not_an_identifier(input: &str) -> Error {
    Error::Sqlstate(
        "22023",
        format!("string is not a valid identifier: \"{input}\""),
    )
}

/// `parse_ident`: a qualified name split into its parts, unquoted parts
/// folded to lower case. In strict mode anything after the name is an error.
pub(crate) fn parse_ident(input: &str, strict: bool) -> Result<Bson> {
    let chars: Vec<char> = input.chars().collect();
    let space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
    let start = |c: char| c == '_' || c.is_alphabetic() || !c.is_ascii();
    let cont = |c: char| start(c) || c.is_ascii_digit() || c == '$';
    let mut parts = Vec::new();
    let mut i = 0;
    while i < chars.len() && space(chars[i]) {
        i += 1;
    }
    let mut after_dot = false;
    loop {
        let mut missing = false;
        if chars.get(i) == Some(&'"') {
            i += 1;
            let mut part = String::new();
            loop {
                match chars.get(i) {
                    None => return Err(not_an_identifier(input)),
                    Some('"') if chars.get(i + 1) == Some(&'"') => {
                        part.push('"');
                        i += 2;
                    }
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some(c) => {
                        part.push(*c);
                        i += 1;
                    }
                }
            }
            if part.is_empty() {
                return Err(not_an_identifier(input));
            }
            parts.push(Bson::String(part));
        } else if chars.get(i).is_some_and(|c| start(*c)) {
            let from = i;
            while chars.get(i).is_some_and(|c| cont(*c)) {
                i += 1;
            }
            let part: String = chars[from..i].iter().collect();
            parts.push(Bson::String(
                part.chars().map(crate::scalar::simple_lower).collect(),
            ));
        } else {
            missing = true;
        }
        if missing {
            // Nothing where a name must be: after a dot, or at the start.
            if after_dot || parts.is_empty() || strict {
                return Err(not_an_identifier(input));
            }
            break;
        }
        while i < chars.len() && space(chars[i]) {
            i += 1;
        }
        if chars.get(i) == Some(&'.') {
            after_dot = true;
            i += 1;
            while i < chars.len() && space(chars[i]) {
                i += 1;
            }
        } else if i >= chars.len() {
            break;
        } else {
            if strict {
                return Err(not_an_identifier(input));
            }
            break;
        }
    }
    Ok(Bson::Array(parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::text;

    #[test]
    fn parse_ident_folds_what_is_not_quoted() {
        let parts = |s: &str, strict: bool| match parse_ident(s, strict) {
            Ok(Bson::Array(items)) => items.iter().map(text).collect::<Vec<_>>().join("|"),
            other => format!("{other:?}"),
        };
        assert_eq!(parts("SomeSchema.someTable", true), "someschema|sometable");
        assert_eq!(parts("\"A b\".C", true), "A b|c");
        assert_eq!(parts("\"a\"\"b\"", true), "a\"b");
        assert_eq!(parts("a.b()", false), "a|b");
        assert!(parse_ident("a.b()", true).is_err());
        assert!(parse_ident("a..b", true).is_err());
        assert!(parse_ident("", false).is_err());
        assert!(parse_ident("1a", true).is_err());
    }

    #[test]
    fn a_character_latin1_cannot_hold_is_an_error() {
        let (latin1, name) = encoding("LATIN1", "").unwrap();
        assert_eq!(encode("é", &latin1, &name).unwrap(), vec![0xe9]);
        assert!(encode("€", &latin1, &name).is_err());
        assert!(encoding("nosuch", "destination ").is_err());
        assert!(decode(&[0xff], &Encoding::Utf8).is_err());
    }
}
