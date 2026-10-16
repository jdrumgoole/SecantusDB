//! mongod's hashed-index key: `BSONElementHasher::hash64` with seed 0, the
//! value `$toHashedIndexKey` returns.
//!
//! Transcribed from `src/mongo/db/hasher.cpp` (r8.2.1) and checked against
//! mongod 8.2.11 on 140 values of every BSON type a driver emits (2026-10-16):
//!
//! - an MD5 over: the seed as a little-endian `int32`, then the element;
//! - an element is its CANONICAL type (`canonicalizeBSONType`) as a
//!   little-endian `int32`, then its field name with its NUL (not for the
//!   top-level element, whose name is empty and skipped), then:
//!   - a number (int / long / double / decimal) squashed to an `int64` by
//!     `safeNumberLongForHash`, so `1`, `1.0`, `Int64(1)`, `1.5` and
//!     `Decimal128("1")` all hash alike;
//!   - an object or array: each member as an element WITH its name, then the
//!     EOO element (canonical type 0, no name, no value);
//!   - code with scope: the code string with its NUL, then the scope's members
//!     as for an object;
//!   - anything else: the raw BSON value bytes;
//! - the result is the digest's first eight bytes as a little-endian `int64`.

use bson::{Bson, Document};

use crate::decimal::{self, Dec, RoundMode};

/// `$toHashedIndexKey` of an already-evaluated value. `missing` is mongod's
/// missing value, which it hashes as null.
pub fn hash64(v: &Bson, missing: bool) -> i64 {
    let mut m = Md5::new();
    m.update(&0i32.to_le_bytes());
    if missing {
        hash_element(&mut m, None, &Bson::Null);
    } else {
        hash_element(&mut m, None, v);
    }
    let d = m.finish();
    i64::from_le_bytes(d[..8].try_into().unwrap())
}

/// `canonicalizeBSONType`.
fn canonical_type(v: &Bson) -> i32 {
    match v {
        Bson::MinKey => -1,
        Bson::MaxKey => 127,
        Bson::Undefined => 0,
        Bson::Null => 5,
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => 10,
        Bson::String(_) | Bson::Symbol(_) => 15,
        Bson::Document(_) => 20,
        Bson::Array(_) => 25,
        Bson::Binary(_) => 30,
        Bson::ObjectId(_) => 35,
        Bson::Boolean(_) => 40,
        Bson::DateTime(_) => 45,
        Bson::Timestamp(_) => 47,
        Bson::RegularExpression(_) => 50,
        Bson::DbPointer(_) => 55,
        Bson::JavaScriptCode(_) => 60,
        Bson::JavaScriptCodeWithScope(_) => 65,
    }
}

fn hash_element(m: &mut Md5, name: Option<&str>, v: &Bson) {
    m.update(&canonical_type(v).to_le_bytes());
    if let Some(n) = name {
        m.update(n.as_bytes());
        m.update(&[0]);
    }
    match v {
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => {
            m.update(&safe_number_long_for_hash(v).to_le_bytes());
        }
        Bson::Document(d) => hash_members(m, d.iter().map(|(k, v)| (k.as_str(), v))),
        Bson::Array(a) => {
            let names: Vec<String> = (0..a.len()).map(|i| i.to_string()).collect();
            hash_members(m, names.iter().map(String::as_str).zip(a.iter()));
        }
        Bson::JavaScriptCodeWithScope(c) => {
            m.update(c.code.as_bytes());
            m.update(&[0]);
            hash_members(m, c.scope.iter().map(|(k, v)| (k.as_str(), v)));
        }
        other => m.update(&raw_value_bytes(other)),
    }
}

/// Each member with its name, then the EOO element (canonical type 0; an EOO
/// has no name and no value).
fn hash_members<'a>(m: &mut Md5, members: impl Iterator<Item = (&'a str, &'a Bson)>) {
    for (k, v) in members {
        hash_element(m, Some(k), v);
    }
    m.update(&0i32.to_le_bytes());
}

/// The value bytes of a leaf element as BSON writes them: the element encoded
/// under an empty name, less the document header, type byte, name terminator
/// and document terminator.
fn raw_value_bytes(v: &Bson) -> Vec<u8> {
    let mut d = Document::new();
    d.insert("", v.clone());
    let mut buf = Vec::new();
    d.to_writer(&mut buf)
        .expect("a leaf BSON value always encodes");
    // [len:4][type:1][name NUL:1] value [doc NUL:1]
    buf[6..buf.len() - 1].to_vec()
}

/// `BSONElement::safeNumberLongForHash`: `safeNumberLong`, except that a
/// double of exactly 2^63 is LLONG_MIN (the value an x86 cast gave before the
/// conversion was made safe, kept so existing hashed indexes stay valid).
fn safe_number_long_for_hash(v: &Bson) -> i64 {
    const TWO_63: f64 = 9_223_372_036_854_775_808.0;
    match v {
        Bson::Int32(i) => *i as i64,
        Bson::Int64(i) => *i,
        Bson::Double(d) => {
            if *d == TWO_63 {
                i64::MIN
            } else {
                // `as` saturates and sends NaN to 0: exactly `safeNumberLong`.
                *d as i64
            }
        }
        Bson::Decimal128(_) => decimal_to_long(v),
        _ => 0,
    }
}

/// `safeNumberLong` of a Decimal128: NaN is 0, out of range clamps, anything
/// else rounds half to even (`Decimal128::toLong`'s default mode).
fn decimal_to_long(v: &Bson) -> i64 {
    match decimal::from_bson(v) {
        None | Some(Dec::Nan) => 0,
        Some(Dec::Inf(s)) => {
            if s > 0 {
                i64::MAX
            } else {
                i64::MIN
            }
        }
        Some(d @ Dec::Fin { .. }) => {
            if d.is_zero() {
                return 0;
            }
            let negative = matches!(d, Dec::Fin { sign, .. } if sign < 0);
            let clamp = if negative { i64::MIN } else { i64::MAX };
            decimal::round_to_exp(&d, 0, RoundMode::HalfEven)
                .and_then(|r| decimal::trunc_to_i64(&r))
                .unwrap_or(clamp)
        }
    }
}

/// MD5 (RFC 1321). Small enough to carry here rather than add a dependency to
/// this crate and every lockfile that pulls it in.
struct Md5 {
    state: [u32; 4],
    buf: Vec<u8>,
    len: u64,
}

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

impl Md5 {
    fn new() -> Self {
        Md5 {
            state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476],
            buf: Vec::with_capacity(64),
            len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        self.buf.extend_from_slice(data);
        let full = self.buf.len() / 64 * 64;
        if full > 0 {
            let blocks: Vec<u8> = self.buf.drain(..full).collect();
            for block in blocks.as_chunks::<64>().0 {
                self.compress(block);
            }
        }
    }

    fn finish(mut self) -> [u8; 16] {
        let bits = self.len.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        while (self.buf.len() + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_le_bytes());
        let len = self.len;
        self.update(&pad);
        self.len = len;
        let mut out = [0u8; 16];
        for (i, w) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut x = [0u32; 16];
        for (i, w) in x.iter_mut().enumerate() {
            *w = u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let t = a
                .wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(x[g])
                .rotate_left(S[i]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(t);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{doc, oid::ObjectId, Decimal128, JavaScriptCodeWithScope, Timestamp};
    use std::str::FromStr;

    fn md5_hex(s: &[u8]) -> String {
        let mut m = Md5::new();
        m.update(s);
        m.finish().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn md5_rfc1321_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
        let mut m = Md5::new();
        for chunk in b"message digest".chunks(3) {
            m.update(chunk);
        }
        let hex: String = m.finish().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "f96b697d7cb7938d525a2f31aaf161d0");
    }

    fn dec(s: &str) -> Bson {
        Bson::Decimal128(Decimal128::from_str(s).unwrap())
    }

    /// Answers of mongod 8.2.11 (2026-10-16).
    #[test]
    fn matches_mongod_8_2_11() {
        let cases: Vec<(Bson, i64)> = vec![
            (Bson::Int32(1), 5902408780260971510),
            (Bson::Double(1.5), 5902408780260971510),
            (Bson::Int64(1), 5902408780260971510),
            (dec("1"), 5902408780260971510),
            (Bson::String("abc".into()), 8478485326885698097),
            (Bson::Null, 2338878944348059895),
            (Bson::Boolean(true), 6405873908747105701),
            (Bson::Boolean(false), 6289544573401934092),
            (Bson::Array(vec![]), 8849948234993459283),
            (
                Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)]),
                8312708174902235769,
            ),
            (
                Bson::Array(vec![Bson::Null, Bson::Null]),
                -4476635310016884380,
            ),
            (Bson::Document(doc! {}), 7980500913326740417),
            (Bson::Document(doc! {"a": 1}), -8705610352600103477),
            (
                Bson::ObjectId(ObjectId::parse_str("000000000000000000000000").unwrap()),
                -4293118519463489418,
            ),
            (
                Bson::Timestamp(Timestamp {
                    time: 1,
                    increment: 2,
                }),
                6130793394493442031,
            ),
            (
                Bson::Timestamp(Timestamp {
                    time: 0,
                    increment: 0,
                }),
                -7867208682377458672,
            ),
            (Bson::MinKey, 7961148599568647290),
            (Bson::MaxKey, 5504842513779440750),
            (Bson::Double(-0.0), 4854801880128277513),
            (Bson::Double(f64::NAN), 4854801880128277513),
            (
                Bson::Double(9_223_372_036_854_775_808.0),
                4532067210535695462,
            ),
            (Bson::Double(1e19), 921523596458303250),
            (Bson::Double(f64::NEG_INFINITY), 4532067210535695462),
            (
                Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                    code: "x".into(),
                    scope: doc! {"a": 1},
                }),
                1114765668211402900,
            ),
            (
                Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                    code: "x".into(),
                    scope: doc! {},
                }),
                -7891586450251910328,
            ),
        ];
        for (v, want) in cases {
            assert_eq!(hash64(&v, false), want, "{v:?}");
        }
        assert_eq!(hash64(&Bson::Undefined, true), 2338878944348059895);
    }

    #[test]
    fn decimal_rounds_half_to_even_and_clamps() {
        assert_eq!(decimal_to_long(&dec("2.5")), 2);
        assert_eq!(decimal_to_long(&dec("3.5")), 4);
        assert_eq!(decimal_to_long(&dec("-2.5")), -2);
        assert_eq!(decimal_to_long(&dec("NaN")), 0);
        assert_eq!(decimal_to_long(&dec("1E+30")), i64::MAX);
        assert_eq!(decimal_to_long(&dec("-1E+30")), i64::MIN);
        assert_eq!(decimal_to_long(&dec("0E+100")), 0);
        assert_eq!(decimal_to_long(&dec("9223372036854775808")), i64::MAX);
    }
}
