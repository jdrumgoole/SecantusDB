//! mongod's KeyString V1 encoding of index keys (`key_string.cpp`,
//! `BuilderBase::_appendBsonValue`), without the type bits: the bytes
//! `dbCheck`'s `extraIndexKeysCheck` hashes (RM-416). The decoder for the
//! same format is `resume_keystring.rs`.
//!
//! Each value is a canonical-type byte and a body; a descending field's
//! bytes are inverted; the key ends with the `kEnd` discriminator (4).
//! Numbers of every type share one encoding (the type bits, not kept here,
//! say which a number was), so `1`, `NumberLong(1)` and `1.0` give the same
//! bytes. Measured against mongod 8.2.11, 2026-11-14.

use bson::{Bson, Document};
use secantus_core::collation::Collation;

const MIN_KEY: u8 = 10;
const UNDEFINED: u8 = 15;
const NULLISH: u8 = 20;
const NUMERIC: u8 = 30;
const NUMERIC_NAN: u8 = 30;
const NEGATIVE_LARGE: u8 = 31;
const NEGATIVE_SMALL: u8 = 40;
const ZERO: u8 = 41;
const POSITIVE_SMALL: u8 = 42;
const POSITIVE_LARGE: u8 = 51;
const STRING_LIKE: u8 = 60;
const OBJECT: u8 = 70;
const ARRAY: u8 = 80;
const BIN_DATA: u8 = 90;
const OID: u8 = 100;
const BOOL_FALSE: u8 = 110;
const BOOL_TRUE: u8 = 111;
const DATE: u8 = 120;
const TIMESTAMP: u8 = 130;
const REGEX: u8 = 140;
const DB_REF: u8 = 150;
const CODE: u8 = 160;
const CODE_WITH_SCOPE: u8 = 170;
const MAX_KEY: u8 = 240;
/// The `kEnd` discriminator closing a key.
const END: u8 = 4;

/// `kMinLargeDouble`, 2**63.
const MIN_LARGE_DOUBLE: f64 = 9_223_372_036_854_775_808.0;
/// `kTinyDoubleExponentDownshiftFactor`, 2**-255.
const TINY_DOWNSHIFT_LIMIT: f64 = f64::from_bits((1023 - 255) << 52);
/// `kTinyDoubleExponentUpshiftFactor`, 2**256.
const TINY_UPSHIFT: f64 = f64::from_bits((1023 + 256) << 52);

/// A value this encoder does not write yet.
#[derive(Debug, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);

struct Out<'a> {
    buf: &'a mut Vec<u8>,
    invert: bool,
    collation: Option<&'a Collation>,
}

impl Out<'_> {
    fn byte(&mut self, b: u8) {
        self.buf.push(if self.invert { !b } else { b });
    }

    fn bytes(&mut self, bs: &[u8]) {
        for b in bs {
            self.byte(*b);
        }
    }

    /// A string with each NUL written `00 FF`, then a NUL.
    fn string_with_nuls(&mut self, s: &[u8]) {
        for b in s {
            self.byte(*b);
            if *b == 0 {
                self.byte(0xff);
            }
        }
        self.byte(0);
    }
}

/// `bsonTypeToGenericKeyStringType`: the type byte ahead of a field name.
fn generic_type(v: &Bson) -> u8 {
    match v {
        Bson::MinKey => MIN_KEY,
        Bson::Undefined => UNDEFINED,
        Bson::Null => NULLISH,
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => NUMERIC,
        Bson::String(_) | Bson::Symbol(_) => STRING_LIKE,
        Bson::Document(_) => OBJECT,
        Bson::Array(_) => ARRAY,
        Bson::Binary(_) => BIN_DATA,
        Bson::ObjectId(_) => OID,
        Bson::Boolean(_) => BOOL_FALSE,
        Bson::DateTime(_) => DATE,
        Bson::Timestamp(_) => TIMESTAMP,
        Bson::RegularExpression(_) => REGEX,
        Bson::DbPointer(_) => DB_REF,
        Bson::JavaScriptCode(_) => CODE,
        Bson::JavaScriptCodeWithScope(_) => CODE_WITH_SCOPE,
        Bson::MaxKey => MAX_KEY,
    }
}

/// The KeyString of an index key: each value with its direction
/// (`true` descending), then `kEnd`.
pub fn encode_key(
    values: &[(&Bson, bool)],
    collation: Option<&Collation>,
) -> Result<Vec<u8>, Unsupported> {
    let mut buf = Vec::new();
    for (v, descending) in values {
        let mut out = Out {
            buf: &mut buf,
            invert: *descending,
            collation,
        };
        value(&mut out, v)?;
    }
    buf.push(END);
    Ok(buf)
}

fn object(out: &mut Out<'_>, d: &Document) -> Result<(), Unsupported> {
    for (name, v) in d {
        out.byte(generic_type(v));
        out.string_with_nuls(name.as_bytes());
        value(out, v)?;
    }
    out.byte(0);
    Ok(())
}

/// `_appendBsonValue`.
fn value(out: &mut Out<'_>, v: &Bson) -> Result<(), Unsupported> {
    if let Some(raw) = secantus_core::bsondoc::as_invalid_utf8(v) {
        // A string that is not UTF-8 is written as its bytes.
        out.byte(STRING_LIKE);
        out.string_with_nuls(raw);
        return Ok(());
    }
    if secantus_core::bsondoc::as_raw_text(v).is_some() {
        return Err(Unsupported(
            "a regex, DBPointer or JavaScript value that is not UTF-8",
        ));
    }
    match v {
        Bson::MinKey => out.byte(MIN_KEY),
        Bson::MaxKey => out.byte(MAX_KEY),
        Bson::Undefined => out.byte(UNDEFINED),
        Bson::Null => out.byte(NULLISH),
        Bson::Boolean(b) => out.byte(if *b { BOOL_TRUE } else { BOOL_FALSE }),
        Bson::DateTime(d) => {
            out.byte(DATE);
            out.bytes(&((d.timestamp_millis() as u64) ^ (1 << 63)).to_be_bytes());
        }
        Bson::Timestamp(t) => {
            out.byte(TIMESTAMP);
            out.bytes(&((u64::from(t.time) << 32) | u64::from(t.increment)).to_be_bytes());
        }
        Bson::ObjectId(o) => {
            out.byte(OID);
            out.bytes(&o.bytes());
        }
        Bson::String(s) | Bson::Symbol(s) => {
            out.byte(STRING_LIKE);
            match out.collation {
                // The collator's comparison key stands for the string.
                Some(c) => {
                    let key = secantus_core::collation::sort_key(s, c);
                    let key = key.strip_suffix(&[0]).unwrap_or(&key).to_vec();
                    out.string_with_nuls(&key);
                }
                None => out.string_with_nuls(s.as_bytes()),
            }
        }
        Bson::JavaScriptCode(s) => {
            out.byte(CODE);
            out.string_with_nuls(s.as_bytes());
        }
        Bson::JavaScriptCodeWithScope(c) => {
            out.byte(CODE_WITH_SCOPE);
            out.string_with_nuls(c.code.as_bytes());
            object(out, &c.scope)?;
        }
        Bson::Binary(b) => {
            out.byte(BIN_DATA);
            let len = b.bytes.len();
            if len < 0xff {
                out.byte(len as u8);
            } else {
                out.byte(0xff);
                out.bytes(&(len as u32).to_be_bytes());
            }
            out.byte(u8::from(b.subtype));
            out.bytes(&b.bytes);
        }
        Bson::RegularExpression(r) => {
            out.byte(REGEX);
            out.bytes(r.pattern.as_bytes());
            out.byte(0);
            out.bytes(r.options.as_bytes());
            out.byte(0);
        }
        Bson::DbPointer(p) => {
            // The driver keeps a DBPointer's parts private: read them back
            // from its BSON.
            let raw = crate::bsondoc::to_vec(&bson::doc! {"": p.clone()})
                .map_err(|_| Unsupported("DBPointer"))?;
            // doc: len(4) type(1) name "\0" strlen(4) ns\0 oid(12) \0
            let body = &raw[4 + 1 + 1..];
            let n = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
            let ns = &body[4..4 + n - 1];
            let oid = &body[4 + n..4 + n + 12];
            out.byte(DB_REF);
            out.bytes(&((ns.len()) as u32).to_be_bytes());
            out.bytes(ns);
            out.bytes(oid);
        }
        Bson::Document(d) => {
            out.byte(OBJECT);
            object(out, d)?;
        }
        Bson::Array(items) => {
            out.byte(ARRAY);
            for item in items {
                value(out, item)?;
            }
            out.byte(0);
        }
        Bson::Int32(n) => integer(out, i64::from(*n)),
        Bson::Int64(n) => integer(out, *n),
        Bson::Double(d) => double(out, *d),
        Bson::Decimal128(_) => return Err(Unsupported("Decimal128")),
    }
    Ok(())
}

/// `_appendInteger`.
fn integer(out: &mut Out<'_>, n: i64) {
    if n == 0 {
        out.byte(ZERO);
    } else if n == i64::MIN {
        large_double(out, -MIN_LARGE_DOUBLE);
    } else {
        preshifted_integer(out, n.unsigned_abs() << 1, n < 0);
    }
}

/// `_appendPreshiftedIntegerPortion`: the fewest big-endian bytes, their
/// count in the type byte.
fn preshifted_integer(out: &mut Out<'_>, encoded: u64, negative: bool) -> usize {
    let n = 8 - (encoded.leading_zeros() / 8) as usize;
    out.byte(if negative {
        NEGATIVE_SMALL - n as u8
    } else {
        POSITIVE_SMALL + n as u8
    });
    let be = encoded.to_be_bytes();
    for b in &be[8 - n..] {
        out.byte(if negative { !*b } else { *b });
    }
    n
}

/// `_appendDoubleWithoutTypeBits`, with the decimal continuation marker
/// "equal to the double".
fn double(out: &mut Out<'_>, num: f64) {
    if num.is_nan() {
        out.byte(NUMERIC_NAN);
        return;
    }
    if num == 0.0 {
        out.byte(ZERO);
        return;
    }
    let negative = num.is_sign_negative();
    let magnitude = num.abs();
    if magnitude < MIN_LARGE_DOUBLE {
        let integer_part = magnitude as u64;
        if integer_part as f64 == magnitude {
            preshifted_integer(out, integer_part << 1, negative);
            return;
        }
        if integer_part == 0 {
            small_double(out, magnitude, negative);
            return;
        }
        // An integer part and a fraction: the integer's bytes, low bit set,
        // then the fraction filling the rest of eight bytes, its low two
        // bits the continuation marker (0).
        let n = preshifted_integer(out, (integer_part << 1) | 1, negative);
        let frac_bytes = 8 - n;
        let scaled = (magnitude - integer_part as f64) * 256f64.powi(frac_bytes as i32);
        let fraction = scaled as u64;
        let be = fraction.to_be_bytes();
        for b in &be[8 - frac_bytes..] {
            out.byte(if negative { !*b } else { *b });
        }
        return;
    }
    large_double(out, num);
}

/// `_appendSmallDouble`: a magnitude below one.
fn small_double(out: &mut Out<'_>, magnitude: f64, negative: bool) {
    let encoded = if magnitude >= TINY_DOWNSHIFT_LIMIT {
        magnitude.to_bits() << 2
    } else {
        ((magnitude * TINY_UPSHIFT).to_bits() << 1) + (1 << 62)
    };
    out.byte(if negative {
        NEGATIVE_SMALL
    } else {
        POSITIVE_SMALL
    });
    for b in encoded.to_be_bytes() {
        out.byte(if negative { !b } else { b });
    }
}

/// `_appendLargeDouble`: 2**63 and above, infinity included.
fn large_double(out: &mut Out<'_>, num: f64) {
    let negative = num.is_sign_negative();
    let magnitude = num.abs();
    let encoded = if magnitude.is_infinite() {
        u64::MAX
    } else {
        (magnitude.to_bits() & !(1 << 62)) << 1
    };
    out.byte(if negative {
        NEGATIVE_LARGE
    } else {
        POSITIVE_LARGE
    });
    for b in encoded.to_be_bytes() {
        out.byte(if negative { !b } else { b });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;
    use md5::{Digest, Md5};

    fn md5_of(keys: &[Bson]) -> String {
        let mut m = Md5::new();
        for k in keys {
            m.update(encode_key(&[(k, false)], None).unwrap());
        }
        m.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Digests mongod 8.2.11 wrote for one-key batches, 2026-11-14.
    #[test]
    fn matches_mongod_digests() {
        let cases: &[(Bson, &str)] = &[
            (Bson::Int32(1), "b5c54e4624123e81d712a68d99825698"),
            (Bson::Null, "984ea8b06596c0c007d9f86d797bd776"),
            (
                Bson::String("ab".into()),
                "e60c7d0f872e988cc09acd60a33ed2df",
            ),
            (Bson::Double(1.5), "ab1f8fd94c2a8138db664d4c1e43f7ac"),
            (Bson::Double(f64::NAN), "d58e078a948eed7070cb33950025c968"),
            (
                Bson::Double(f64::NEG_INFINITY),
                "0519d64453eedc73d0899c850008108a",
            ),
            (Bson::Int64(i64::MIN), "8df5bb3401f16e920e9d930a49d8ac8a"),
            (Bson::Double(-2.25), "210a2d1b1dfb7e6e7efbf553e0f393d2"),
            (Bson::Double(-0.0), "9f2d45ee695def07af5eb95de82561ce"),
            (Bson::Double(1e-300), "9c8d5ab80fe0276ad6dd5b9d14b7abba"),
            (Bson::Double(0.1), "71a00cf6e0ca37cc90f309313841cda0"),
            (Bson::Double(123456.789), "a5a73b84af4fdcbb8abc0b0eccde4ce8"),
            (
                Bson::Double(2f64.powi(60)),
                "e78a1c83d08db21e2091604a7f8bd63c",
            ),
            (Bson::Double(1e300), "4a4d9050b915e3ebdb0f0541b3d8d25e"),
            (
                Bson::Double(f64::INFINITY),
                "65550c111b004e96d0f3220aa4260045",
            ),
        ];
        for (v, want) in cases {
            assert_eq!(md5_of(std::slice::from_ref(v)), *want, "{v:?}");
        }
    }

    #[test]
    fn compound_and_descending() {
        let k = encode_key(
            &[(&Bson::Int32(1), false), (&Bson::String("a".into()), false)],
            None,
        )
        .unwrap();
        assert_eq!(k, vec![43, 2, 60, b'a', 0, 4]);
        let d = encode_key(&[(&Bson::Int32(1), true)], None).unwrap();
        assert_eq!(d, vec![!43, !2, 4]);
        let o = encode_key(&[(&Bson::Document(doc! {"a": 1}), false)], None).unwrap();
        assert_eq!(o, vec![70, 30, b'a', 0, 43, 2, 0, 4]);
    }
}
