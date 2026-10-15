//! The multi-byte encodings (EUC_JP, SJIS, BIG5, GB18030, ...) as
//! PostgreSQL 15 verifies and converts them.
//!
//! Two halves, kept apart as PostgreSQL keeps them:
//!
//! * the VERIFIER (`wchar.c`'s `pg_*_verifychar`, transcribed and checked
//!   against PostgreSQL) decides which bytes are a character at all; what
//!   it refuses is 22021 `invalid byte sequence`;
//! * the CONVERSION (`LocalToUtf` / `UtfToLocal` over the maps in
//!   `conversion_procs`) maps a valid character, and one with no mapping
//!   is 22P05 `has no equivalent`.
//!
//! The maps are measured, not written by hand: `tools/pg_mbcs_tables.py`
//! asks PostgreSQL 15.19 to convert every byte sequence each verifier
//! accepts and every code point, and writes `mbcs_data/` (format described
//! there), which `mbcs_maps` reads. The two directions are not inverses --
//! JOHAB's verifier refuses most of the bytes its own encoder writes -- so
//! both are kept. GB18030's four-byte ranges are `utf8_and_gb18030.c`'s
//! rule (`mbcs_maps`).

use std::sync::OnceLock;

use crate::builtins_text::{decode, encode, Encoding};
use crate::Error;

use crate::mbcs_maps::{self as maps, code_of, gb18030_decode, gb18030_encode, push_code, Table};

/// A multi-byte encoding, by PostgreSQL's canonical name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Mb(usize);

const NAMES: [&str; 12] = [
    "EUC_JP",
    "EUC_CN",
    "EUC_KR",
    "EUC_TW",
    "SJIS",
    "BIG5",
    "GBK",
    "UHC",
    "GB18030",
    "JOHAB",
    "SHIFT_JIS_2004",
    "EUC_JIS_2004",
];

/// An error as PostgreSQL reports it: SQLSTATE and message.
pub(crate) type Failure = (&'static str, String);

pub(crate) fn by_name(canonical: &str) -> Option<Mb> {
    NAMES.iter().position(|n| *n == canonical).map(Mb)
}

impl Mb {
    pub(crate) fn name(self) -> &'static str {
        NAMES[self.0]
    }

    /// `pg_encoding_mblen`: how long the character a lead byte starts is,
    /// whether or not the rest is there.
    fn mblen(self, s: &[u8]) -> usize {
        let c = s[0];
        if c < 0x80 {
            return 1;
        }
        match self.name() {
            "EUC_JP" | "EUC_JIS_2004" | "EUC_KR" | "JOHAB" => match c {
                0x8E => 2,
                0x8F => 3,
                _ => 2,
            },
            "EUC_TW" => match c {
                0x8E => 4,
                0x8F => 3,
                _ => 2,
            },
            // Measured: an EUC_CN error shows three bytes from 0x8e as from
            // 0x8f, though its verifier refuses both as a lead.
            "EUC_CN" if c == 0x8E || c == 0x8F => 3,
            "SJIS" | "SHIFT_JIS_2004" if (0xA1..=0xDF).contains(&c) => 1,
            // `pg_gb18030_mblen` reads the second byte to tell two bytes
            // from four.
            "GB18030" if s.get(1).is_some_and(|b| (0x30..=0x39).contains(b)) => 4,
            _ => 2,
        }
    }

    /// `pg_*_verifychar`: the length of the valid character at the start of
    /// `s` (its first byte at 0x80 or above), or `None`.
    fn verify_char(self, s: &[u8]) -> Option<usize> {
        let euc = |b: u8| (0xA1..=0xFE).contains(&b);
        let need = |n: usize| (s.len() >= n).then_some(n);
        let c = s[0];
        match self.name() {
            "EUC_JP" | "EUC_JIS_2004" => match c {
                0x8E => need(2).filter(|_| (0xA1..=0xDF).contains(&s[1])),
                0x8F => need(3).filter(|_| euc(s[1]) && euc(s[2])),
                _ => need(2).filter(|_| euc(c) && euc(s[1])),
            },
            "EUC_CN" | "EUC_KR" => need(2).filter(|_| euc(c) && euc(s[1])),
            "EUC_TW" => match c {
                0x8E => need(4).filter(|_| (0xA1..=0xA7).contains(&s[1]) && euc(s[2]) && euc(s[3])),
                // SS3 is unused in EUC_TW.
                0x8F => None,
                // Only the second byte is range-checked (measured: 0xff 0xe7
                // is a character with no mapping, not an invalid one).
                _ => need(2).filter(|_| euc(s[1])),
            },
            "SJIS" | "SHIFT_JIS_2004" => {
                if self.mblen(s) == 1 {
                    return Some(1);
                }
                let head = (0x81..=0x9F).contains(&c) || (0xE0..=0xFC).contains(&c);
                need(2).filter(|_| {
                    let t = s[1];
                    head && ((0x40..=0x7E).contains(&t) || (0x80..=0xFC).contains(&t))
                })
            }
            // Any second byte but NUL -- and never 0x8d 0x20, the sequence
            // PostgreSQL 15.11 made invalid in every encoding so that a
            // quoting routine has one to emit.
            "BIG5" | "GBK" | "UHC" => need(2).filter(|_| s[1] != 0 && s[..2] != [0x8D, 0x20]),
            "GB18030" => {
                let digit = |b: u8| (0x30..=0x39).contains(&b);
                let byte = |b: u8| (0x81..=0xFE).contains(&b);
                if s.len() >= 4 && digit(s[1]) {
                    (byte(c) && byte(s[2]) && digit(s[3])).then_some(4)
                } else if s.len() >= 2 && byte(c) {
                    let t = s[1];
                    ((0x40..=0x7E).contains(&t) || (0x80..=0xFE).contains(&t)).then_some(2)
                } else {
                    None
                }
            }
            // JOHAB's verifier is the EUC one: its trailing bytes must be in
            // 0xA1..=0xFE, which refuses most of what its encoder writes.
            "JOHAB" => {
                let n = self.mblen(s);
                need(n).filter(|_| s[1..n].iter().all(|b| euc(*b)))
            }
            _ => None,
        }
    }

    /// PostgreSQL's 22021 for the character at `at`: as many of its bytes
    /// as its lead byte announces and the input still has.
    fn invalid(self, bytes: &[u8], at: usize) -> Failure {
        (
            "22021",
            format!(
                "invalid byte sequence for encoding \"{}\": {}",
                self.name(),
                self.shown(bytes, at)
            ),
        )
    }

    fn untranslatable(self, bytes: &[u8], at: usize, to: &str) -> Failure {
        (
            "22P05",
            format!(
                "character with byte sequence {} in encoding \"{}\" has no equivalent in \
                 encoding \"{to}\"",
                self.shown(bytes, at),
                self.name()
            ),
        )
    }

    fn shown(self, bytes: &[u8], at: usize) -> String {
        let n = self.mblen(&bytes[at..]).min(bytes.len() - at).min(8);
        bytes[at..at + n]
            .iter()
            .map(|b| format!("0x{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `pg_verify_mbstr_len`: the characters of `bytes`, each as its start
    /// and length, or the 22021 for the first that is not one.
    pub(crate) fn verify(self, bytes: &[u8]) -> Result<Vec<(usize, usize)>, Failure> {
        let mut chars = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            let l = match bytes[at] {
                0 => return Err(self.invalid(bytes, at)),
                b if b < 0x80 => 1,
                _ => self
                    .verify_char(&bytes[at..])
                    .ok_or_else(|| self.invalid(bytes, at))?,
            };
            chars.push((at, l));
            at += l;
        }
        Ok(chars)
    }

    fn table(self) -> &'static Table {
        static TABLES: [OnceLock<Table>; 12] = [const { OnceLock::new() }; 12];
        TABLES[self.0].get_or_init(|| Table::load(maps::DATA[self.0]))
    }

    /// `convert_from`: the bytes verified whole (`pg_convert` checks its
    /// source first), then converted.
    pub(crate) fn decode(self, bytes: &[u8]) -> Result<String, Failure> {
        self.verify(bytes)?;
        self.decode_stream(bytes)
    }

    /// A client's bytes to text (`LocalToUtf`, which `pg_any_to_server`
    /// runs with no check first): character by character, so the first bad
    /// one is the error, whether it is not a character (22021) or has no
    /// mapping (22P05). Measured: SJIS `0xf0 0xa0 0x80 0x80` is the 22P05
    /// for `0xf0 0xa0`, though `0x80 0x80` is not SJIS at all.
    pub(crate) fn decode_stream(self, bytes: &[u8]) -> Result<String, Failure> {
        let table = self.table();
        let mut out = String::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            let l = match bytes[at] {
                0 => return Err(self.invalid(bytes, at)),
                b if b < 0x80 => {
                    out.push(char::from(b));
                    at += 1;
                    continue;
                }
                _ => self
                    .verify_char(&bytes[at..])
                    .ok_or_else(|| self.invalid(bytes, at))?,
            };
            let code = code_of(&bytes[at..at + l]);
            if let Some(&(a, b)) = table.pairs.get(&code) {
                out.extend([a, b].into_iter().filter_map(char::from_u32));
            } else {
                let cp = table.decode.get(&code).copied().or_else(|| {
                    (self.name() == "GB18030" && l == 4)
                        .then(|| gb18030_decode(code))
                        .flatten()
                });
                match cp.and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => return Err(self.untranslatable(bytes, at, "UTF8")),
                }
            }
            at += l;
        }
        Ok(out)
    }

    /// The code of one character, or `None` (`UtfToLocal`'s map lookup).
    fn encode_char(self, c: char) -> Option<u32> {
        let cp = u32::from(c);
        let table = self.table();
        let code = match table.encode.get(&cp) {
            Some(&code) => code,
            None => table.default_encode.get(&cp).copied().unwrap_or(0),
        };
        // GB18030's map first, then its rule (`conv_utf8_to_18030`).
        match code {
            0 if self.name() == "GB18030" => gb18030_encode(cp),
            0 => None,
            code => Some(code),
        }
    }

    /// `convert_to` / text to a client: each character's bytes, a pair of
    /// characters the encoding has one code for taken together (EUC_JIS_2004
    /// and SHIFT_JIS_2004's combined characters), or the first character
    /// with none.
    pub(crate) fn encode(self, s: &str) -> Result<Vec<u8>, char> {
        let table = self.table();
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::with_capacity(s.len());
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            if c.is_ascii() {
                out.push(c as u8);
                continue;
            }
            if let Some(&next) = chars
                .get(i)
                .filter(|_| table.pair_first.contains(&u32::from(c)))
            {
                if let Some(&code) = table.pair_encode.get(&(u32::from(c), u32::from(next))) {
                    push_code(&mut out, code);
                    i += 1;
                    continue;
                }
            }
            push_code(&mut out, self.encode_char(c).ok_or(c)?);
        }
        Ok(out)
    }
}

/// Whether PostgreSQL converts `from` to `to` directly.
pub(crate) fn direct(from: &str, to: &str) -> bool {
    maps::DIRECT
        .iter()
        .any(|(f, t, _, _)| *f == from && *t == to)
}

/// `convert` between a direct pair: the source verified, then each
/// character through the pair's own table, or through Unicode where that
/// table agrees with it.
pub(crate) fn convert_direct(from: Mb, to: Mb, bytes: &[u8]) -> Result<Vec<u8>, Failure> {
    let (i, diffs) = maps::direct_diffs(from.name(), to.name());
    let fail = |at: usize| match maps::DIRECT[i].2 {
        "22021" => from.invalid(bytes, at),
        _ => from.untranslatable(bytes, at, to.name()),
    };
    let mut out = Vec::with_capacity(bytes.len());
    for (at, l) in from.verify(bytes)? {
        if l == 1 && bytes[at] < 0x80 {
            out.push(bytes[at]);
            continue;
        }
        let code = match diffs.get(&code_of(&bytes[at..at + l])) {
            Some(0) => return Err(fail(at)),
            Some(&code) => code,
            None => {
                let text = from.decode(&bytes[at..at + l]).map_err(|_| fail(at))?;
                let converted = to.encode(&text).map_err(|_| fail(at))?;
                out.extend(converted);
                continue;
            }
        };
        push_code(&mut out, code);
    }
    Ok(out)
}

/// `pg_convert` where either side is a multi-byte encoding: the source
/// verified as its encoding first (22021, NUL included); then the bytes
/// unchanged when there is nothing to convert (one encoding to itself,
/// either side SQL_ASCII, no bytes); then through Unicode when either side
/// is UTF8, or by a direct conversion; any other pair has no conversion
/// function (42883). Measured on PostgreSQL 15.19.
pub(crate) fn convert_multi(
    bytes: &[u8],
    from: &Encoding,
    to: &Encoding,
    to_name: &str,
) -> crate::Result<Vec<u8>> {
    if matches!(from, Encoding::Other(_)) || matches!(to, Encoding::Other(_)) {
        return encode(&decode(bytes, from)?, to, to_name);
    }
    let from_name = name_of(from);
    match from {
        Encoding::Multi(mb) => {
            mb.verify(bytes)
                .map_err(|(state, msg)| Error::Sqlstate(state, msg))?;
        }
        Encoding::Utf8 => {
            decode(bytes, from)?;
        }
        _ => {
            if bytes.contains(&0) {
                return Err(Error::Sqlstate(
                    "22021",
                    format!("invalid byte sequence for encoding \"{from_name}\": 0x00"),
                ));
            }
        }
    }
    // From SQL_ASCII nothing is converted, but the bytes must be valid in
    // the target (`pg_do_encoding_conversion`).
    if let (Encoding::Ascii, Encoding::Multi(mb)) = (from, to) {
        mb.verify(bytes)
            .map_err(|(state, msg)| Error::Sqlstate(state, msg))?;
    }
    if bytes.is_empty()
        || from_name == to_name
        || matches!(from, Encoding::Ascii)
        || matches!(to, Encoding::Ascii)
    {
        return Ok(bytes.to_vec());
    }
    match (from, to) {
        (Encoding::Utf8, _) | (_, Encoding::Utf8) => encode(&decode(bytes, from)?, to, to_name),
        (Encoding::Multi(a), Encoding::Multi(b)) if direct(a.name(), b.name()) => {
            convert_direct(*a, *b, bytes).map_err(|(state, msg)| Error::Sqlstate(state, msg))
        }
        _ => Err(Error::Sqlstate(
            "42883",
            format!(
                "default conversion function for encoding \"{from_name}\" to \"{to_name}\" does \
                 not exist"
            ),
        )),
    }
}

fn name_of(enc: &Encoding) -> &'static str {
    match enc {
        Encoding::Utf8 => "UTF8",
        Encoding::Latin1 => "LATIN1",
        Encoding::Ascii => "SQL_ASCII",
        Encoding::Single(name, _) | Encoding::Other(name) => name,
        Encoding::Multi(mb) => mb.name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mb(name: &str) -> Mb {
        by_name(name).unwrap()
    }

    #[test]
    fn decodes_and_encodes_as_measured() {
        // PostgreSQL 15.19: `convert_from('\xa4a2', 'EUC_JP')` is `あ`,
        // `convert_to('あ', 'SJIS')` is `\x82a0`.
        assert_eq!(mb("EUC_JP").decode(&[0xA4, 0xA2]).unwrap(), "あ");
        assert_eq!(mb("SJIS").encode("aあ").unwrap(), vec![b'a', 0x82, 0xA0]);
        assert_eq!(mb("SJIS").encode("é"), Err('é'));
        assert_eq!(
            mb("GB18030").encode("\u{10000}").unwrap(),
            vec![0x90, 0x30, 0x81, 0x30]
        );
        assert_eq!(
            mb("GB18030").decode(&[0x81, 0x30, 0xD3, 0x30]).unwrap(),
            "\u{452}"
        );
    }

    #[test]
    fn the_verifier_comes_first() {
        let (state, msg) = mb("EUC_JP").decode(&[b'a', 0xA4]).unwrap_err();
        assert_eq!(
            (state, msg.as_str()),
            (
                "22021",
                "invalid byte sequence for encoding \"EUC_JP\": 0xa4"
            )
        );
        let (state, _) = mb("EUC_JP").decode(&[0xA2, 0xAF]).unwrap_err();
        assert_eq!(state, "22P05");
        assert!(mb("BIG5").decode(&[0x8D, 0x20]).is_err());
    }

    #[test]
    fn combined_characters_are_one_code() {
        // SHIFT_JIS_2004 0x82f5 is `か` with a combining semi-voiced mark.
        let s = mb("SHIFT_JIS_2004").decode(&[0x82, 0xF5]).unwrap();
        assert_eq!(s.chars().count(), 2);
        assert_eq!(mb("SHIFT_JIS_2004").encode(&s).unwrap(), vec![0x82, 0xF5]);
    }
}
