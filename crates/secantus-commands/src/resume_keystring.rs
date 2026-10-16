//! What mongod answers for a resume token this server cannot read (RM-075).
//!
//! This server's tokens are its own encoding, so a token it cannot decode is
//! either garbage or another server's. mongod reads `_data` as a KeyString
//! and reports where that failed, so the error a client sees depends on the
//! bytes. This is a transcription of the part of mongod 8.2's decoder that
//! such a token can reach -- `key_string::toBsonSafe` with all-ascending
//! ordering and no `_typeBits` (`src/mongo/db/storage/key_string/
//! key_string.cpp`) -- followed by `ResumeToken::getData`'s checks of the
//! fields it decoded (`src/mongo/db/pipeline/resume_token.cpp`). Each error
//! was measured against mongod 8.2.11.
//!
//! Two shapes are not reproduced and answer [`Verdict::Unmodelled`]: a tiny
//! decimal (whose exponent adjustment is Decimal128 arithmetic), and a token
//! that decodes cleanly -- mongod goes on to look for it in its oplog. A token
//! carrying `_typeBits` is not read here at all.

use bson::Document;

/// mongod's answer for a token's `_data`.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// mongod fails with this code, code name and message.
    Error(i32, &'static str, String),
    /// mongod decodes the token: `has_uuid` says whether it names a
    /// collection, which a single-collection stream requires (260).
    Decoded { has_uuid: bool },
    /// mongod's answer depends on what is not transcribed here.
    Unmodelled,
}

/// A decoded KeyString element, as far as `getData` looks at it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Elem {
    Int(i32),
    Bool(bool),
    Null,
    Object,
    /// BinData: whether it is a 16-byte subtype-4 UUID.
    BinData {
        uuid: bool,
    },
    Other,
}

enum Stop {
    Error(i32, &'static str, String),
    Unmodelled,
}

fn overflow(want: usize, remaining: usize) -> Stop {
    Stop::Error(
        15,
        "Overflow",
        format!("Invalid advance ({want}) past end of buffer[{remaining}] at offset: 0"),
    )
}

fn format_error(code: i32, msg: impl std::fmt::Display) -> Stop {
    Stop::Error(code, "Location", format!("KeyString format error: {msg}"))
}

const MAX_DEPTH: u32 = 200;

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn skip(&mut self, n: usize) -> Result<&[u8], Stop> {
        if self.remaining() < n {
            return Err(overflow(n, self.remaining()));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, Stop> {
        Ok(self.skip(1)?[0])
    }

    fn be(&mut self, n: usize, inverted: bool) -> Result<u64, Stop> {
        let mut v = 0u64;
        for b in self.skip(n)? {
            v = (v << 8) | u64::from(if inverted { !*b } else { *b });
        }
        Ok(v)
    }

    /// `readCString`: up to and past the next NUL.
    fn cstring(&mut self) -> Result<(), Stop> {
        match self.buf[self.pos..].iter().position(|b| *b == 0) {
            Some(n) => {
                self.pos += n + 1;
                Ok(())
            }
            None => Err(format_error(
                50816,
                "Failed to find null terminator in string.",
            )),
        }
    }

    /// `readCStringWithNuls`: a NUL inside the string is `00 FF`.
    fn cstring_with_nuls(&mut self) -> Result<(), Stop> {
        self.cstring()?;
        while self.remaining() > 0 && self.buf[self.pos] == 0xFF {
            self.pos += 1;
            self.cstring()?;
        }
        Ok(())
    }
}

/// `toBson`: an object's fields until a zero type byte.
fn object(r: &mut Reader<'_>, depth: u32) -> Result<(), Stop> {
    while r.u8()? != 0 {
        r.cstring()?;
        let ctype = r.u8()?;
        value(ctype, r, depth)?;
    }
    Ok(())
}

/// `toBsonValue` with no type bits: every numeric reads as an int.
fn value(ctype: u8, r: &mut Reader<'_>, depth: u32) -> Result<Elem, Stop> {
    if depth > MAX_DEPTH {
        return Err(Stop::Error(
            15,
            "Overflow",
            "KeyString format error: KeyString encoding exceeded maximum allowable BSON \
             nesting depth"
                .to_string(),
        ));
    }
    Ok(match ctype {
        10 | 240 | 15 => Elem::Other,
        20 => Elem::Null,
        111 => Elem::Bool(true),
        110 => Elem::Bool(false),
        120 | 130 => {
            r.skip(8)?;
            Elem::Other
        }
        100 => {
            r.skip(12)?;
            Elem::Other
        }
        60 | 160 => {
            r.cstring_with_nuls()?;
            Elem::Other
        }
        170 => {
            r.cstring_with_nuls()?;
            object(r, depth + 2)?;
            Elem::Other
        }
        90 => {
            let mut size = r.u8()? as usize;
            if size == 0xff {
                size = r.be(4, false)? as usize;
            }
            let subtype = r.u8()?;
            r.skip(size)?;
            // A BSONColumn (subtype 7) is validated; no column a client
            // would put in a resume token passes, and an empty one fails.
            if subtype == 7 {
                return Err(format_error(50833, "Expected valid BSONColumn data"));
            }
            Elem::BinData {
                uuid: subtype == 4 && size == 16,
            }
        }
        140 => {
            r.cstring()?;
            r.cstring()?;
            Elem::Other
        }
        150 => {
            let size = r.be(4, false)? as usize;
            r.skip(size)?;
            r.skip(12)?;
            Elem::Other
        }
        70 => {
            object(r, depth + 1)?;
            Elem::Object
        }
        80 => {
            loop {
                let t = r.u8()?;
                if t == 0 {
                    break;
                }
                value(t, r, depth + 1)?;
            }
            Elem::Other
        }
        // kNumericNaN: an int is not a NaN.
        30 => return Err(format_error(50819, "Invalid type bits for numeric NaN")),
        // kNumericZero.
        41 => Elem::Int(0),
        // Large magnitudes: only a double or a decimal can be one.
        31 | 51 => {
            let encoded = r.be(8, ctype == 31)?;
            if encoded & (1 << 63) != 0 && encoded != u64::MAX {
                return Err(format_error(50818, "Invalid type bits for decimal number."));
            }
            return Err(format_error(50826, "Unexpected type of large number."));
        }
        // Small magnitudes: a fraction, which an int is not.
        40 | 42 => {
            let encoded = r.be(8, ctype == 40)?;
            match encoded >> 62 {
                0 => {
                    let low = r.be(8, ctype == 40)?;
                    let coeff = (u128::from(encoded & ((1u64 << 49) - 1)) << 64) | u128::from(low);
                    if coeff == 0 || coeff >= 10u128.pow(34) {
                        return Err(format_error(50829, "Expected non-zero number for decimal."));
                    }
                    return Err(Stop::Unmodelled);
                }
                1 | 2 => return Err(format_error(50823, "Expected decimal continuation.")),
                _ => {
                    return Err(format_error(
                        50825,
                        "Unexpected type for small number, expected decimal.",
                    ))
                }
            }
        }
        32..=39 | 43..=50 => {
            let negative = ctype <= 39;
            let n = if negative {
                usize::from(39 - ctype + 1)
            } else {
                usize::from(ctype - 43 + 1)
            };
            let encoded = r.be(n, negative)?;
            if encoded & 1 != 0 {
                // A fractional part, read to a full eight bytes first.
                r.be(8 - n, negative)?;
                return Err(format_error(50810, "Expected type Decimal."));
            }
            let mut v = (encoded >> 1) as i64;
            if negative {
                v = -v;
            }
            Elem::Int(v as i32)
        }
        other => return Err(format_error(50811, format!("Unknown type: {other}"))),
    })
}

/// `toBsonSafe`: the top-level elements, until the end marker.
fn decode(bytes: &[u8]) -> Result<Vec<Elem>, Stop> {
    let mut r = Reader { buf: bytes, pos: 0 };
    let mut out = Vec::new();
    while r.remaining() > 0 {
        let mut ctype = r.u8()?;
        if ctype == 1 || ctype == 254 {
            ctype = r.u8()?;
        }
        if ctype == 4 {
            break;
        }
        out.push(value(ctype, &mut r, 1)?);
    }
    Ok(out)
}

fn fail(code: i32, msg: &str) -> Verdict {
    Verdict::Error(code, "Location", msg.to_string())
}

/// `ResumeToken::getData`'s checks of the decoded elements.
fn check_fields(elems: &[Elem]) -> Verdict {
    let mut i = elems.iter();
    if i.next().is_none() {
        return fail(40649, "invalid empty resume token");
    }
    let Some(version) = i.next() else {
        return fail(50796, "Resume Token does not contain version");
    };
    let Elem::Int(version) = *version else {
        return fail(50854, "Invalid resume token: wrong type for version");
    };
    if !(0..=2).contains(&version) {
        return fail(
            50795,
            "Invalid Resume Token: only supports version 0, 1 and 2",
        );
    }
    let mut event_token = true;
    if version >= 1 {
        let Some(t) = i.next() else {
            return fail(51055, "Resume Token does not contain tokenType");
        };
        let Elem::Int(t) = *t else {
            return fail(51056, "Resume Token tokenType is not an int.");
        };
        if t != 0 && t != 128 {
            return Verdict::Error(51057, "Location", format!("Token type {t} not recognized"));
        }
        event_token = t == 128;
    }
    let Some(op_index) = i.next() else {
        return fail(50793, "Resume Token does not contain txnOpIndex");
    };
    let Elem::Int(op_index) = *op_index else {
        return fail(50855, "Resume Token txnOpIndex is not an integer");
    };
    if op_index < 0 {
        return fail(
            50794,
            "Invalid Resume Token: txnOpIndex should be non-negative",
        );
    }
    if version >= 1 {
        let Some(from_invalidate) = i.next() else {
            return fail(50872, "Resume Token does not contain fromInvalidate");
        };
        if !matches!(from_invalidate, Elem::Bool(_)) {
            return fail(50870, "Resume Token fromInvalidate is not a boolean.");
        }
    }
    let Some(uuid) = i.next() else {
        if version > 1 {
            return fail(6189500, "Expected UUID or null");
        }
        return Verdict::Decoded { has_uuid: false };
    };
    let has_uuid = match uuid {
        Elem::Null => false,
        Elem::BinData { uuid: true } => true,
        _ => {
            return Verdict::Error(
                207,
                "InvalidUUID",
                "uuid must be a 16-byte binary field with UUID (4) subtype".to_string(),
            )
        }
    };
    let rest: Vec<&Elem> = i.collect();
    if !event_token && !rest.is_empty() {
        return fail(6189504, "Invalid high water mark token");
    }
    let Some(event_id) = rest.first() else {
        if event_token && version >= 2 {
            return fail(
                6189501,
                "Expected an eventIdentifier for an event resume token",
            );
        }
        return Verdict::Decoded { has_uuid };
    };
    if **event_id != Elem::Object {
        return fail(6189503, "Resume Token eventIdentifier is not an object");
    }
    let mut more = rest[1..].iter();
    if version >= 2 {
        if let Some(fragment) = more.next() {
            if !matches!(fragment, Elem::Int(n) if *n >= 0) {
                return fail(
                    7182501,
                    "Resume token 'fragmentNum' must be a non-negative integer.",
                );
            }
        }
    }
    if more.next().is_some() {
        return fail(40646, "invalid oversized resume token");
    }
    Verdict::Decoded { has_uuid }
}

/// mongod's answer for the resume token `tok`, which this server could not
/// read. `tok` has a `_data` string of hex digits (checked before).
pub fn mongod_verdict(tok: &Document) -> Verdict {
    if tok.contains_key("_typeBits") {
        return Verdict::Unmodelled;
    }
    let Ok(hex) = tok.get_str("_data") else {
        return Verdict::Unmodelled;
    };
    let Some(bytes) = (0..hex.len())
        .step_by(2)
        .map(|i| {
            hex.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        })
        .collect::<Option<Vec<u8>>>()
    else {
        return Verdict::Unmodelled;
    };
    match decode(&bytes) {
        Ok(elems) => check_fields(&elems),
        Err(Stop::Error(code, name, msg)) => Verdict::Error(code, name, msg),
        Err(Stop::Unmodelled) => Verdict::Unmodelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn verdict(hex: &str) -> (i32, String) {
        match mongod_verdict(&doc! { "_data": hex }) {
            Verdict::Error(code, _, msg) => (code, msg),
            other => panic!("{hex}: {other:?}"),
        }
    }

    /// Each answer measured against mongod 8.2.11.
    #[test]
    fn errors_match_mongod() {
        let cases: &[(&str, i32, &str)] = &[
            ("", 40649, "invalid empty resume token"),
            (
                "82",
                15,
                "Invalid advance (8) past end of buffer[0] at offset: 0",
            ),
            ("00", 50811, "KeyString format error: Unknown type: 0"),
            (
                "01",
                15,
                "Invalid advance (1) past end of buffer[0] at offset: 0",
            ),
            ("04", 40649, "invalid empty resume token"),
            ("0A", 50796, "Resume Token does not contain version"),
            (
                "1E",
                50819,
                "KeyString format error: Invalid type bits for numeric NaN",
            ),
            (
                "3C",
                50816,
                "KeyString format error: Failed to find null terminator in string.",
            ),
            (
                "64",
                15,
                "Invalid advance (12) past end of buffer[0] at offset: 0",
            ),
            (
                "96",
                15,
                "Invalid advance (4) past end of buffer[0] at offset: 0",
            ),
            ("FF", 50811, "KeyString format error: Unknown type: 255"),
            (
                "8200000000",
                15,
                "Invalid advance (8) past end of buffer[4] at offset: 0",
            ),
            (
                "0A0A0A0A82AABB",
                15,
                "Invalid advance (8) past end of buffer[2] at offset: 0",
            ),
            (
                "82000000000000000129",
                50793,
                "Resume Token does not contain txnOpIndex",
            ),
            (
                "8200000000000000002B020A",
                51056,
                "Resume Token tokenType is not an int.",
            ),
            (
                "820000000000000001041A",
                50796,
                "Resume Token does not contain version",
            ),
            (
                "8200000000000000012B02",
                51055,
                "Resume Token does not contain tokenType",
            ),
            (
                "8200000000000000012B062C0100296E",
                50795,
                "Invalid Resume Token: only supports version 0, 1 and 2",
            ),
            (
                "8200000000000000012B042B0A296E",
                51057,
                "Token type 5 not recognized",
            ),
            (
                "8200000000000000012B042C010027FD",
                50794,
                "Invalid Resume Token: txnOpIndex should be non-negative",
            ),
            (
                "8200000000000000012B022C010029",
                50872,
                "Resume Token does not contain fromInvalidate",
            ),
            (
                "8200000000000000012B022C0100292B02",
                50870,
                "Resume Token fromInvalidate is not a boolean.",
            ),
            (
                "8200000000000000012B042C0100296E29",
                207,
                "uuid must be a 16-byte binary field with UUID (4) subtype",
            ),
            (
                "8200000000000000012B042C0100296E5A1004000102030405060708090A0B0C0D0E0F",
                6189501,
                "Expected an eventIdentifier for an event resume token",
            ),
            (
                "8200000000000000012B042C0100296E5A1004000102030405060708090A0B0C0D0E0F29",
                6189503,
                "Resume Token eventIdentifier is not an object",
            ),
            (
                "8200000000000000012B042C0100296E5A1004000102030405060708090A0B0C0D0E0F46002B0229",
                40646,
                "invalid oversized resume token",
            ),
            (
                "8200000000000000012B042C0100296E5A1004000102030405060708090A0B0C0D0E0F4600270D",
                7182501,
                "Resume token 'fragmentNum' must be a non-negative integer.",
            ),
            (
                "8200000000000000012B0429296E5A1004000102030405060708090A0B0C0D0E0F4600",
                6189504,
                "Invalid high water mark token",
            ),
            (
                "82000000000000000120000000000000000000",
                50810,
                "KeyString format error: Expected type Decimal.",
            ),
            (
                "0A5A0007",
                50833,
                "KeyString format error: Expected valid BSONColumn data",
            ),
            (
                "0A960000000041",
                15,
                "Invalid advance (12) past end of buffer[1] at offset: 0",
            ),
            (
                "0A8C6100620000",
                50811,
                "KeyString format error: Unknown type: 0",
            ),
            (
                "0AAA6100046E",
                50816,
                "KeyString format error: Failed to find null terminator in string.",
            ),
            (
                "0A502900",
                50854,
                "Invalid resume token: wrong type for version",
            ),
            (
                "0A462900",
                15,
                "Invalid advance (1) past end of buffer[0] at offset: 0",
            ),
            (
                "0A46610029000004",
                50811,
                "KeyString format error: Unknown type: 0",
            ),
            (
                "4A000000",
                50811,
                "KeyString format error: Unknown type: 74",
            ),
        ];
        for (hex, code, msg) in cases {
            assert_eq!(verdict(hex), (*code, msg.to_string()), "{hex}");
        }
    }

    #[test]
    fn a_token_that_decodes_says_whether_it_names_a_collection() {
        assert_eq!(
            mongod_verdict(&doc! { "_data": "8200000000000000012B022C0100296E" }),
            Verdict::Decoded { has_uuid: false }
        );
        assert_eq!(
            mongod_verdict(
                &doc! { "_data": "8200000000000000012B042C0100296E5A1004000102030405060708090A0B0C0D0E0F4600" }
            ),
            Verdict::Decoded { has_uuid: true }
        );
    }
}
