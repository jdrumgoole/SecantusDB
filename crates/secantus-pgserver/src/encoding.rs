//! `client_encoding`: name validation / canonicalisation and byte transcoding
//! between the server's internal UTF-8 and the client's declared encoding.
//!
//! The server stores and evaluates everything in UTF-8. `client_encoding`
//! governs the bytes on the wire in BOTH directions: an outgoing text value is
//! transcoded from UTF-8 to the client encoding, and an incoming text parameter
//! is decoded from the client encoding to UTF-8.
//!
//! Only two encodings need real byte-for-byte transcoding: `LATIN1` and
//! `LATIN9`. `UTF8` is the internal form itself, and `SQL_ASCII` passes bytes
//! through unchanged -- both leave the internal UTF-8 bytes exactly as they are,
//! which is what the server already emitted before this module existed. Every
//! other real PostgreSQL encoding name (`EUC_TW`, `WIN1252`, ...) is accepted as
//! a session setting and reported back verbatim, but its output is passed
//! through as UTF-8: a client that lacks the Python codec (`EUC_TW`) raises
//! before it ever decodes a value, exactly as it does against real PostgreSQL,
//! and no measured client round-trips non-ASCII through an unconverted encoding.
//!
//! Keeping the transcoding gated on `LATIN1` / `LATIN9` is deliberate: the
//! default `UTF8` path is byte-identical to the pre-existing code, so declaring
//! and reporting `client_encoding` cannot regress a UTF-8 client.

/// The client encoding as it bears on transcoding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClientEncoding {
    /// The internal form; output and input bytes are UTF-8 verbatim.
    Utf8,
    /// ISO-8859-1: Unicode code points U+0000..=U+00FF map 1:1 to bytes.
    Latin1,
    /// ISO-8859-15: LATIN1 with eight positions reassigned (the euro sign and
    /// seven others).
    Latin9,
    /// Bytes pass through unchanged in both directions, exactly as UTF8 does
    /// (the server's internal form is UTF-8). Covers `SQL_ASCII` and every
    /// accepted-but-not-transcoded encoding.
    Passthrough,
}

impl ClientEncoding {
    /// Whether outgoing / incoming text bytes must be transcoded. Only LATIN1
    /// and LATIN9 differ from the internal UTF-8 form.
    pub fn transcodes(self) -> bool {
        matches!(self, ClientEncoding::Latin1 | ClientEncoding::Latin9)
    }
}

/// Why an encoding name was refused, mirroring PostgreSQL's two distinct
/// answers.
#[derive(Debug)]
pub enum EncodingError {
    /// The name is not a PostgreSQL encoding at all -> `22023`.
    Invalid,
    /// A real encoding, but not usable as a client encoding (`MULE_INTERNAL`)
    /// -> `0A000`.
    Unconvertible(&'static str),
}

/// PostgreSQL's canonical encoding names (the output of `pg_encoding_to_char`),
/// probed against PostgreSQL 16. `MULE_INTERNAL` is present so it can be
/// recognised and rejected with the right (`0A000`) error rather than treated
/// as an unknown name.
const CANONICAL: &[&str] = &[
    "SQL_ASCII",
    "EUC_JP",
    "EUC_CN",
    "EUC_KR",
    "EUC_TW",
    "EUC_JIS_2004",
    "UTF8",
    "MULE_INTERNAL",
    "LATIN1",
    "LATIN2",
    "LATIN3",
    "LATIN4",
    "LATIN5",
    "LATIN6",
    "LATIN7",
    "LATIN8",
    "LATIN9",
    "LATIN10",
    "WIN1256",
    "WIN1258",
    "WIN866",
    "WIN874",
    "KOI8R",
    "WIN1251",
    "WIN1252",
    "ISO_8859_5",
    "ISO_8859_6",
    "ISO_8859_7",
    "ISO_8859_8",
    "WIN1250",
    "WIN1253",
    "WIN1254",
    "WIN1255",
    "WIN1257",
    "KOI8U",
    "SJIS",
    "BIG5",
    "GBK",
    "UHC",
    "GB18030",
    "JOHAB",
    "SHIFT_JIS_2004",
];

/// Explicit aliases PostgreSQL accepts that do not reduce to a canonical name
/// by [`clean`] alone: `UNICODE` for UTF8 and the `ISO-8859-N` spellings of the
/// LATIN encodings. The `ISO_8859_5..8` names are canonical in their own right
/// and are handled by [`CANONICAL`]; only the LATIN-numbered ones need mapping.
const ALIASES: &[(&str, &str)] = &[
    ("UNICODE", "UTF8"),
    ("ISO88591", "LATIN1"),
    ("ISO88592", "LATIN2"),
    ("ISO88593", "LATIN3"),
    ("ISO88594", "LATIN4"),
    ("ISO88599", "LATIN5"),
    ("ISO885910", "LATIN6"),
    ("ISO885913", "LATIN7"),
    ("ISO885914", "LATIN8"),
    ("ISO885915", "LATIN9"),
    ("ISO885916", "LATIN10"),
    ("SQLASCII", "SQL_ASCII"),
];

/// PostgreSQL's `clean_encoding_name`: drop every non-alphanumeric character
/// and uppercase the rest, so `iso-8859-15`, `ISO_8859_15` and `iso885915` all
/// collapse to the same key.
fn clean(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

/// Resolve a user-supplied encoding name to its canonical PostgreSQL spelling,
/// or say why it was refused.
pub fn canonical_name(name: &str) -> Result<&'static str, EncodingError> {
    let key = clean(name);
    if key == "MULEINTERNAL" {
        return Err(EncodingError::Unconvertible("MULE_INTERNAL"));
    }
    for c in CANONICAL {
        if c == &"MULE_INTERNAL" {
            continue;
        }
        if clean(c) == key {
            return Ok(c);
        }
    }
    for (alias, canonical) in ALIASES {
        if *alias == key {
            return Ok(canonical);
        }
    }
    Err(EncodingError::Invalid)
}

/// The transcoding behaviour for a canonical encoding name.
pub fn client_encoding(canonical: &str) -> ClientEncoding {
    match canonical {
        "UTF8" => ClientEncoding::Utf8,
        "LATIN1" => ClientEncoding::Latin1,
        "LATIN9" => ClientEncoding::Latin9,
        _ => ClientEncoding::Passthrough,
    }
}

/// The eight LATIN9 positions that differ from LATIN1: `(byte, code point)`.
const LATIN9_DIFFS: &[(u8, char)] = &[
    (0xA4, '\u{20AC}'), // €
    (0xA6, '\u{0160}'), // Š
    (0xA8, '\u{0161}'), // š
    (0xB4, '\u{017D}'), // Ž
    (0xB8, '\u{017E}'), // ž
    (0xBC, '\u{0152}'), // Œ
    (0xBD, '\u{0153}'), // œ
    (0xBE, '\u{0178}'), // Ÿ
];

/// Decode wire bytes in the client encoding into an internal UTF-8 string.
///
/// LATIN1 and LATIN9 are single-byte encodings in which every byte is valid, so
/// this cannot fail for them; every other encoding takes the UTF-8 path (the
/// internal form), where invalid bytes are the caller's concern.
pub fn decode(enc: ClientEncoding, bytes: &[u8]) -> String {
    match enc {
        ClientEncoding::Latin1 => bytes.iter().map(|&b| b as char).collect(),
        ClientEncoding::Latin9 => bytes
            .iter()
            .map(|&b| {
                LATIN9_DIFFS
                    .iter()
                    .find(|(byte, _)| *byte == b)
                    .map(|(_, ch)| *ch)
                    .unwrap_or(b as char)
            })
            .collect(),
        ClientEncoding::Utf8 | ClientEncoding::Passthrough => {
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

/// Transcode an internal UTF-8 byte slice to the client encoding.
///
/// Returns `Err(ch)` with the first code point that has no representation in the
/// target encoding, which the caller turns into PostgreSQL's `22P05`
/// untranslatable-character error. Only ever called for LATIN1 / LATIN9; the
/// other encodings emit their input unchanged and never reach here.
pub fn encode(enc: ClientEncoding, utf8: &[u8]) -> Result<Vec<u8>, char> {
    let text = String::from_utf8_lossy(utf8);
    match enc {
        ClientEncoding::Latin1 => text
            .chars()
            .map(|c| u8::try_from(c as u32).map_err(|_| c))
            .collect(),
        ClientEncoding::Latin9 => text
            .chars()
            .map(|c| {
                if let Some((byte, _)) = LATIN9_DIFFS.iter().find(|(_, ch)| *ch == c) {
                    return Ok(*byte);
                }
                // A code point that LATIN9 reassigned away from its LATIN1 byte
                // cannot be represented by that byte any more.
                if LATIN9_DIFFS
                    .iter()
                    .any(|(byte, _)| *byte as u32 == c as u32)
                {
                    return Err(c);
                }
                u8::try_from(c as u32).map_err(|_| c)
            })
            .collect(),
        ClientEncoding::Utf8 | ClientEncoding::Passthrough => Ok(utf8.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalises_common_spellings() {
        assert_eq!(canonical_name("utf8").unwrap(), "UTF8");
        assert_eq!(canonical_name("utf-8").unwrap(), "UTF8");
        assert_eq!(canonical_name("utf_8").unwrap(), "UTF8");
        assert_eq!(canonical_name("unicode").unwrap(), "UTF8");
        assert_eq!(canonical_name("latin1").unwrap(), "LATIN1");
        assert_eq!(canonical_name("latin9").unwrap(), "LATIN9");
        assert_eq!(canonical_name("iso8859-15").unwrap(), "LATIN9");
        assert_eq!(canonical_name("iso-8859-15").unwrap(), "LATIN9");
        assert_eq!(canonical_name("eucjp").unwrap(), "EUC_JP");
        assert_eq!(canonical_name("euc-jp").unwrap(), "EUC_JP");
        assert_eq!(canonical_name("sql_ascii").unwrap(), "SQL_ASCII");
        assert_eq!(canonical_name("EUC_TW").unwrap(), "EUC_TW");
        assert_eq!(canonical_name("iso_8859_5").unwrap(), "ISO_8859_5");
    }

    #[test]
    fn rejects_invalid_and_unconvertible() {
        assert!(matches!(canonical_name("wat"), Err(EncodingError::Invalid)));
        assert!(matches!(
            canonical_name("mule_internal"),
            Err(EncodingError::Unconvertible("MULE_INTERNAL"))
        ));
    }

    #[test]
    fn latin9_euro_round_trips() {
        let enc = client_encoding("LATIN9");
        assert_eq!(enc, ClientEncoding::Latin9);
        let bytes = encode(enc, "€".as_bytes()).unwrap();
        assert_eq!(bytes, vec![0xA4]);
        assert_eq!(decode(enc, &bytes), "€");
    }

    #[test]
    fn latin1_cannot_hold_the_euro() {
        let enc = client_encoding("LATIN1");
        assert_eq!(encode(enc, "€".as_bytes()), Err('€'));
        // The LATIN1 byte for the currency sign decodes back to it, and the
        // euro is simply not representable.
        assert_eq!(encode(enc, "abc".as_bytes()).unwrap(), b"abc".to_vec());
    }

    #[test]
    fn latin9_reassigned_bytes() {
        let enc = client_encoding("LATIN9");
        // 0xA4 is the euro in LATIN9, so the LATIN1 currency sign U+00A4 no
        // longer has a byte there.
        assert_eq!(encode(enc, "\u{00A4}".as_bytes()), Err('\u{00A4}'));
        assert_eq!(decode(enc, &[0xA4]), "€");
        assert_eq!(decode(enc, &[0xA6]), "Š");
    }

    #[test]
    fn utf8_and_passthrough_are_identity() {
        for enc in [ClientEncoding::Utf8, ClientEncoding::Passthrough] {
            assert_eq!(encode(enc, "€".as_bytes()).unwrap(), "€".as_bytes());
            assert_eq!(decode(enc, "€".as_bytes()), "€");
            assert!(!enc.transcodes());
        }
    }
}
