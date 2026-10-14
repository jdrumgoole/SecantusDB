//! The text functions `builtins` answers that need no regular expression:
//! the encoding conversions (`convert_to`, `convert`, `length(bytea, name)`)
//! and `parse_ident`.

use bson::Bson;

use crate::{Error, Result};

/// The encodings a conversion here knows: UTF8, and the single-byte ones
/// whose every octet is its own code point. Any other encoding PostgreSQL
/// names is `Other`: its ASCII range is ASCII, and anything above it needs a
/// conversion table this server does not carry, so it is refused rather than
/// read as LATIN1.
pub(crate) enum Encoding {
    Utf8,
    Latin1,
    Ascii,
    Other(&'static str),
}

/// `pg_encname_tbl` (PostgreSQL 15, `src/common/encnames.c`): every name
/// `pg_char_to_encoding` accepts, cleaned, and the encoding it names. `abc`,
/// `tcvn` and `vscii` are all WIN1258.
const ENCODING_ALIASES: &[(&str, &str)] = &[
    ("abc", "WIN1258"),
    ("alt", "WIN866"),
    ("big5", "BIG5"),
    ("euccn", "EUC_CN"),
    ("eucjis2004", "EUC_JIS_2004"),
    ("eucjp", "EUC_JP"),
    ("euckr", "EUC_KR"),
    ("euctw", "EUC_TW"),
    ("gb18030", "GB18030"),
    ("gbk", "GBK"),
    ("iso88591", "LATIN1"),
    ("iso885910", "LATIN6"),
    ("iso885913", "LATIN7"),
    ("iso885914", "LATIN8"),
    ("iso885915", "LATIN9"),
    ("iso885916", "LATIN10"),
    ("iso88592", "LATIN2"),
    ("iso88593", "LATIN3"),
    ("iso88594", "LATIN4"),
    ("iso88595", "ISO_8859_5"),
    ("iso88596", "ISO_8859_6"),
    ("iso88597", "ISO_8859_7"),
    ("iso88598", "ISO_8859_8"),
    ("iso88599", "LATIN5"),
    ("johab", "JOHAB"),
    ("koi8", "KOI8R"),
    ("koi8r", "KOI8R"),
    ("koi8u", "KOI8U"),
    ("latin1", "LATIN1"),
    ("latin10", "LATIN10"),
    ("latin2", "LATIN2"),
    ("latin3", "LATIN3"),
    ("latin4", "LATIN4"),
    ("latin5", "LATIN5"),
    ("latin6", "LATIN6"),
    ("latin7", "LATIN7"),
    ("latin8", "LATIN8"),
    ("latin9", "LATIN9"),
    ("mskanji", "SJIS"),
    ("muleinternal", "MULE_INTERNAL"),
    ("shiftjis", "SJIS"),
    ("shiftjis2004", "SHIFT_JIS_2004"),
    ("sjis", "SJIS"),
    ("sqlascii", "SQL_ASCII"),
    ("tcvn", "WIN1258"),
    ("tcvn5712", "WIN1258"),
    ("uhc", "UHC"),
    ("unicode", "UTF8"),
    ("utf8", "UTF8"),
    ("vscii", "WIN1258"),
    ("win", "WIN1251"),
    ("win1250", "WIN1250"),
    ("win1251", "WIN1251"),
    ("win1252", "WIN1252"),
    ("win1253", "WIN1253"),
    ("win1254", "WIN1254"),
    ("win1255", "WIN1255"),
    ("win1256", "WIN1256"),
    ("win1257", "WIN1257"),
    ("win1258", "WIN1258"),
    ("win866", "WIN866"),
    ("win874", "WIN874"),
    ("win932", "SJIS"),
    ("win936", "GBK"),
    ("win949", "UHC"),
    ("win950", "BIG5"),
    ("windows1250", "WIN1250"),
    ("windows1251", "WIN1251"),
    ("windows1252", "WIN1252"),
    ("windows1253", "WIN1253"),
    ("windows1254", "WIN1254"),
    ("windows1255", "WIN1255"),
    ("windows1256", "WIN1256"),
    ("windows1257", "WIN1257"),
    ("windows1258", "WIN1258"),
    ("windows866", "WIN866"),
    ("windows874", "WIN874"),
    ("windows932", "SJIS"),
    ("windows936", "GBK"),
    ("windows949", "UHC"),
    ("windows950", "BIG5"),
];

/// `pg_char_to_encoding`: the encoding a name stands for, matched after
/// `clean_encoding_name` keeps only its letters and digits, folded to lower
/// case. A name of 64 bytes or more is never in the table.
pub(crate) fn encoding_by_name(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.len() >= 64 {
        return None;
    }
    let key: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    ENCODING_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, enc)| *enc)
}

/// Is a byte in this encoding always one character? (`pg_encoding_max_length`
/// is 1 for every encoding but these.)
pub(crate) fn single_byte(canonical: &str) -> bool {
    !matches!(
        canonical,
        "UTF8"
            | "EUC_JP"
            | "EUC_CN"
            | "EUC_KR"
            | "EUC_TW"
            | "EUC_JIS_2004"
            | "MULE_INTERNAL"
            | "SJIS"
            | "BIG5"
            | "GBK"
            | "UHC"
            | "GB18030"
            | "JOHAB"
            | "SHIFT_JIS_2004"
    )
}

pub(crate) fn encoding(name: &str, role: &str) -> Result<(Encoding, String)> {
    let canonical = encoding_by_name(name).ok_or_else(|| {
        Error::Sqlstate("22023", format!("invalid {role}encoding name \"{name}\""))
    })?;
    let enc = match canonical {
        "UTF8" => Encoding::Utf8,
        "LATIN1" => Encoding::Latin1,
        "SQL_ASCII" => Encoding::Ascii,
        other => Encoding::Other(other),
    };
    Ok((enc, canonical.to_string()))
}

/// Non-ASCII data in an encoding this server has no table for.
pub(crate) fn no_table(canonical: &str) -> Error {
    Error::Unsupported(format!("converting non-ASCII {canonical} data"))
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
        Encoding::Other(_) if bytes.is_ascii() => Ok(bytes.iter().map(|b| *b as char).collect()),
        Encoding::Other(canonical) => Err(no_table(canonical)),
    }
}

pub(crate) fn encode(s: &str, to: &Encoding, to_name: &str) -> Result<Vec<u8>> {
    match to {
        Encoding::Utf8 | Encoding::Ascii => Ok(s.as_bytes().to_vec()),
        Encoding::Other(_) if s.is_ascii() => Ok(s.as_bytes().to_vec()),
        Encoding::Other(canonical) => Err(no_table(canonical)),
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

/// `to_ascii` (`ascii.c`'s `pg_to_ascii`): each byte of the text read in
/// `canonical`, an ASCII byte kept and the rest mapped through the
/// encoding's table. Only LATIN1, LATIN2, LATIN9 and WIN1250 have one.
pub(crate) fn to_ascii(data: &str, canonical: &str) -> Result<String> {
    let (table, range): (&[u8], u8) = match canonical {
        "LATIN1" => (
            b"  cL Y  \"Ca  -R     'u .,      ?AAAAAAACEEEEIIII NOOOOOxOUUUUYTBaaaaaaaceeeeiiii nooooo/ouuuuyty",
            160,
        ),
        "LATIN2" => (
            b" A L LS \"SSTZ-ZZ a,l'ls ,sstz\"zzRAAAALCCCEEEEIIDDNNOOOOxRUUUUYTBraaaalccceeeeiiddnnoooo/ruuuuyt.",
            160,
        ),
        "LATIN9" => (
            b"  cL YS sCa  -R     Zu .z   EeY?AAAAAAACEEEEIIII NOOOOOxOUUUUYTBaaaaaaaceeeeiiii nooooo/ouuuuyty",
            160,
        ),
        "WIN1250" => (
            b"  ' \"    %S<STZZ `'\"\".--  s>stzz   L A  \"CS  -RZ  ,l'u .,as L\"lzRAAAALCCCEEEEIIDDNNOOOOxRUUUUYTBraaaalccceeeeiiddnnoooo/ruuuuyt ",
            128,
        ),
        other => {
            return Err(Error::FeatureNotSupported(format!(
                "encoding conversion from {other} to ASCII not supported"
            )))
        }
    };
    Ok(data
        .bytes()
        .map(|b| {
            if b < 128 {
                b as char
            } else if b < range {
                ' '
            } else {
                table[usize::from(b - range)] as char
            }
        })
        .collect())
}

/// `like_escape` (`do_like_escape`): a LIKE pattern written with escape
/// character `esc` rewritten to use backslash. An empty escape doubles every
/// backslash; a backslash escape is the pattern as it is; any other escape
/// becomes `\`, and a backslash not right after one is doubled. Over text
/// by character, over bytea by byte.
pub(crate) fn like_escape<T: Copy + PartialEq>(
    pattern: &[T],
    esc: &[T],
    backslash: T,
) -> Result<Vec<T>> {
    let mut out = Vec::with_capacity(pattern.len() * 2);
    match esc {
        [] => {
            for &c in pattern {
                if c == backslash {
                    out.push(backslash);
                }
                out.push(c);
            }
        }
        [e] if *e == backslash => out.extend_from_slice(pattern),
        [e] => {
            let mut after = false;
            for &c in pattern {
                if c == *e && !after {
                    out.push(backslash);
                    after = true;
                } else if c == backslash {
                    out.push(backslash);
                    if !after {
                        out.push(backslash);
                    }
                    after = false;
                } else {
                    out.push(c);
                    after = false;
                }
            }
        }
        _ => return Err(Error::Sqlstate("22025", "invalid escape string".into())),
    }
    Ok(out)
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
