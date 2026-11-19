//! The decoder: `key_string::toBson` and the RecordId / discriminator readers.

use super::dec::{self, Big, Mode, Prec, D};
use super::encode::make_db_pointer;
use super::typebits::{
    TypeBits, TypeBitsReader, K_DECIMAL, K_DECIMAL_ZERO_0XXX, K_DOUBLE, K_INT, K_LONG,
    K_NEGATIVE_DOUBLE_ZERO, K_SYMBOL,
};
use super::{
    ctype, Error, Ordering, Result, DCM_EQUAL_TO_DOUBLE, DCM_EQUAL_TO_DOUBLE_15,
    DCM_LARGER_THAN_15, DCM_LESS_THAN_15, K_END, K_GREATER, K_LESS,
    RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES,
};
use bson::{
    spec::BinarySubtype, Binary, Bson, DateTime, Document, JavaScriptCodeWithScope, Regex,
    Timestamp,
};

/// `BSONDepth::getMaxAllowableDepth()`.
const MAX_DEPTH: u32 = 200;

struct Rd<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn new(buf: &'a [u8]) -> Rd<'a> {
        Rd { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn skip(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Error::format("buffer too short"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn peek(&self) -> Result<u8> {
        self.buf
            .get(self.pos)
            .copied()
            .ok_or_else(|| Error::format("buffer too short"))
    }

    fn u8(&mut self, inverted: bool) -> Result<u8> {
        let b = self.skip(1)?[0];
        Ok(if inverted { !b } else { b })
    }

    fn u32_be(&mut self, inverted: bool) -> Result<u32> {
        let mut b: [u8; 4] = self.skip(4)?.try_into().unwrap();
        if inverted {
            b.iter_mut().for_each(|x| *x = !*x);
        }
        Ok(u32::from_be_bytes(b))
    }

    fn u64_be(&mut self, inverted: bool) -> Result<u64> {
        let mut b: [u8; 8] = self.skip(8)?.try_into().unwrap();
        if inverted {
            b.iter_mut().for_each(|x| *x = !*x);
        }
        Ok(u64::from_be_bytes(b))
    }

    fn bytes(&mut self, n: usize, inverted: bool) -> Result<Vec<u8>> {
        let s = self.skip(n)?;
        Ok(if inverted {
            s.iter().map(|b| !b).collect()
        } else {
            s.to_vec()
        })
    }

    /// `readCString`.
    fn cstring(&mut self) -> Result<&'a [u8]> {
        let rest = &self.buf[self.pos..];
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| Error::format("Failed to find null terminator in string."))?;
        self.pos += end + 1;
        Ok(&rest[..end])
    }

    /// `readCStringWithNuls`: `00 FF` stands for an embedded NUL.
    fn cstring_with_nuls(&mut self) -> Result<Vec<u8>> {
        let mut out = self.cstring()?.to_vec();
        while self.remaining() > 0 && self.peek()? == 0xff {
            out.push(0);
            self.skip(1)?;
            out.extend_from_slice(self.cstring()?);
        }
        Ok(out)
    }

    /// `readInvertedCString`.
    fn inverted_cstring(&mut self) -> Result<Vec<u8>> {
        let rest = &self.buf[self.pos..];
        let end = rest
            .iter()
            .position(|b| *b == 0xff)
            .ok_or_else(|| Error::format("Failed to find '0xFF' in inverted string."))?;
        self.pos += end + 1;
        Ok(rest[..end].iter().map(|b| !b).collect())
    }

    /// `readInvertedCStringWithNuls`: `FF 00` stands for an embedded NUL.
    fn inverted_cstring_with_nuls(&mut self) -> Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        let mut first = true;
        loop {
            if !first {
                self.skip(1)?;
                out.push(0xff); // flipped to NUL with the rest
            }
            first = false;
            let rest = &self.buf[self.pos..];
            let end = rest
                .iter()
                .position(|b| *b == 0xff)
                .ok_or_else(|| Error::format("Failed to find '0xFF' in inverted string."))?;
            out.extend_from_slice(&rest[..end]);
            self.pos += end + 1;
            if !(self.remaining() > 0 && self.peek()? == 0x00) {
                break;
            }
        }
        Ok(out.iter().map(|b| !b).collect())
    }

    fn string_like(&mut self, inverted: bool) -> Result<String> {
        let raw = if inverted {
            self.inverted_cstring_with_nuls()?
        } else {
            self.cstring_with_nuls()?
        };
        utf8(raw)
    }
}

fn utf8(b: Vec<u8>) -> Result<String> {
    String::from_utf8(b).map_err(|_| Error::format("string is not valid UTF-8"))
}

/// Decode a KeyString (as built for `ordering`) back to its values. Field
/// names are not stored, so the result is one `Bson` per key field. Anything
/// after the end byte (a RecordId) is ignored.
pub fn to_bson(data: &[u8], ordering: Ordering, type_bits: &TypeBits) -> Result<Vec<Bson>> {
    let mut tb = type_bits.reader();
    let mut reader = Rd::new(data);
    let mut out = Vec::new();
    let mut i = 0usize;
    while reader.remaining() > 0 {
        let invert = ordering.inverted(i);
        let mut ct = reader.u8(invert)?;
        if ct == K_LESS || ct == K_GREATER {
            // A discriminator is logically part of the previous field; it only
            // appears in query keys, never in a stored one.
            ct = reader.u8(invert)?;
        }
        if ct == K_END {
            break;
        }
        out.push(to_bson_value(ct, &mut reader, &mut tb, invert, 1)?);
        i += 1;
    }
    Ok(out)
}

fn to_bson_object(
    reader: &mut Rd<'_>,
    tb: &mut TypeBitsReader<'_>,
    inverted: bool,
    depth: u32,
) -> Result<Document> {
    let mut doc = Document::new();
    while reader.u8(inverted)? != 0 {
        let name = if inverted {
            utf8(reader.inverted_cstring()?)?
        } else {
            utf8(reader.cstring()?.to_vec())?
        };
        let ct = reader.u8(inverted)?;
        let v = to_bson_value(ct, reader, tb, inverted, depth)?;
        doc.insert(name, v);
    }
    Ok(doc)
}

/// `readDecimalContinuation`.
fn read_decimal_continuation(reader: &mut Rd<'_>, inverted: bool, num: D) -> Result<D> {
    let continuation = reader.u64_be(inverted)?;
    let big = num
        .to_big()
        .ok_or_else(|| Error::format("Invalid decimal continuation."))?
        .normalized34();
    let neg = big.neg;
    let exp = big.exp;
    let sum = big.add(&Big::from_u128(neg, u128::from(continuation), exp));
    // The sum is exact, but may need a 35th digit that is a zero (a carry out
    // of 999...9): decimal128 drops it and raises the exponent, which is not
    // an inexact result.
    let (sum, inexact) = sum.round(dec::COEF_DIGITS, Mode::NearestEven);
    if inexact {
        return Err(Error::format(
            "Unexpected inexact flag set after Decimal addition.",
        ));
    }
    sum.to_d()
        .ok_or_else(|| Error::format("Unexpected inexact flag set after Decimal addition."))
}

/// `adjustDecimalExponent`: the low six bits of the exponent are in the type
/// bits; rescale `num` (exactly) to the exponent they name.
fn adjust_decimal_exponent(tb: &mut TypeBitsReader<'_>, num: D) -> Result<D> {
    const MAX_EXP_ADJUST: u32 = 33;
    const MAX_EXP_INCREMENT_FOR_ZERO_HIGH_COEFFICIENT: u32 = 19;
    let D::Fin {
        neg,
        coef,
        bexp: orig,
    } = num
    else {
        return Err(Error::format("Expected non-zero number for decimal."));
    };
    if coef == 0 {
        return Err(Error::format("Expected non-zero number for decimal."));
    }
    let stored = u32::from(tb.read_decimal_exponent()?);
    let mut high = (orig & !0x3f) | stored;
    // An exponent not less than num's that matches the stored bits.
    if high < orig {
        high += 64;
    }
    if high == orig {
        return Ok(num);
    }
    let (new_coef, new_exp);
    if high <= orig + MAX_EXP_ADJUST
        && ((coef >> 64) != 0 || high <= orig + MAX_EXP_INCREMENT_FOR_ZERO_HIGH_COEFFICIENT)
    {
        // Increase the exponent, right-shifting the coefficient: exact.
        if high > dec::MAX_BEXP {
            return Err(Error::format(
                "Unexpected exponent values after adjusting Decimal.",
            ));
        }
        let p = 10u128.pow(high - orig);
        if coef % p != 0 {
            return Err(Error::format(
                "Unexpected signaling flag for Decimal quantization.",
            ));
        }
        new_coef = coef / p;
        new_exp = high;
    } else {
        // Decrease the exponent, left-shifting the coefficient.
        let low = high
            .checked_sub(64)
            .ok_or_else(|| Error::format("Unexpected exponent values after adjusting Decimal."))?;
        if i64::from(low) < i64::from(orig) - i64::from(MAX_EXP_ADJUST) {
            return Err(Error::format(
                "Unexpected exponent values after adjusting Decimal.",
            ));
        }
        let p = 10u128.pow(orig - low);
        new_coef = coef
            .checked_mul(p)
            .filter(|c| *c < 10u128.pow(34))
            .ok_or_else(|| Error::format("Unexpected exponent values after adjusting Decimal."))?;
        new_exp = low;
    }
    if (new_exp & 0x3f) != (high & 0x3f) {
        return Err(Error::format("Unexpected biased exponent in decimal."));
    }
    Ok(D::Fin {
        neg,
        coef: new_coef,
        bexp: new_exp,
    })
}

fn dec_bson(d: D) -> Bson {
    Bson::Decimal128(d.to_bson())
}

fn from_double(x: f64, prec: Prec, mode: Mode) -> Result<D> {
    dec::from_double(x, prec, mode)
        .ok_or_else(|| Error::format("double to decimal conversion out of range"))
}

fn negate(d: D) -> D {
    match d {
        D::Fin { neg, coef, bexp } => D::Fin {
            neg: !neg,
            coef,
            bexp,
        },
        D::Inf { neg } => D::Inf { neg: !neg },
        D::Nan => D::Nan,
    }
}

fn to_bson_value(
    ct: u8,
    reader: &mut Rd<'_>,
    tb: &mut TypeBitsReader<'_>,
    inverted: bool,
    depth: u32,
) -> Result<Bson> {
    if depth > MAX_DEPTH {
        return Err(Error::format(
            "KeyString encoding exceeded maximum allowable BSON nesting depth",
        ));
    }
    let mut is_negative = false;
    let mut inverted = inverted;
    match ct {
        ctype::MIN_KEY => Ok(Bson::MinKey),
        ctype::MAX_KEY => Ok(Bson::MaxKey),
        ctype::NULLISH => Ok(Bson::Null),
        ctype::UNDEFINED => Ok(Bson::Undefined),
        ctype::BOOL_TRUE => Ok(Bson::Boolean(true)),
        ctype::BOOL_FALSE => Ok(Bson::Boolean(false)),
        ctype::DATE => {
            let v = reader.u64_be(inverted)? ^ (1u64 << 63);
            Ok(Bson::DateTime(DateTime::from_millis(v as i64)))
        }
        ctype::TIMESTAMP => {
            let v = reader.u64_be(inverted)?;
            Ok(Bson::Timestamp(Timestamp {
                time: (v >> 32) as u32,
                increment: v as u32,
            }))
        }
        ctype::OID => {
            let b = reader.bytes(12, inverted)?;
            Ok(Bson::ObjectId(bson::oid::ObjectId::from_bytes(
                b.try_into().unwrap(),
            )))
        }
        ctype::STRING_LIKE => {
            let original = tb.read_string_like()?;
            let raw = if inverted {
                reader.inverted_cstring_with_nuls()?
            } else {
                reader.cstring_with_nuls()?
            };
            if original == K_SYMBOL {
                Ok(Bson::Symbol(utf8(raw)?))
            } else {
                // Bytes that are not UTF-8 (a collation's sort key, say)
                // come back as the carrier of such a string.
                Ok(crate::bsondoc::string_from_bytes(raw))
            }
        }
        ctype::CODE => Ok(Bson::JavaScriptCode(reader.string_like(inverted)?)),
        ctype::CODE_WITH_SCOPE => {
            let code = reader.string_like(inverted)?;
            // BSON validation counts a CodeWithScope as two nesting levels.
            let scope = to_bson_object(reader, tb, inverted, depth + 2)?;
            Ok(Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                code,
                scope,
            }))
        }
        ctype::BIN_DATA => {
            let mut size = reader.u8(inverted)? as usize;
            if size == 0xff {
                size = reader.u32_be(inverted)? as usize;
            }
            let subtype = reader.u8(inverted)?;
            let bytes = reader.bytes(size, inverted)?;
            Ok(Bson::Binary(Binary {
                subtype: BinarySubtype::from(subtype),
                bytes,
            }))
        }
        ctype::REGEX => {
            let (pattern, options) = if inverted {
                (reader.inverted_cstring()?, reader.inverted_cstring()?)
            } else {
                (reader.cstring()?.to_vec(), reader.cstring()?.to_vec())
            };
            Ok(Bson::RegularExpression(Regex {
                pattern: utf8(pattern)?,
                options: utf8(options)?,
            }))
        }
        ctype::DB_REF => {
            let size = reader.u32_be(inverted)? as usize;
            let ns = utf8(reader.bytes(size, inverted)?)?;
            let oid: [u8; 12] = reader.bytes(12, inverted)?.try_into().unwrap();
            make_db_pointer(&ns, oid).ok_or_else(|| Error::format("invalid DBRef"))
        }
        ctype::OBJECT => Ok(Bson::Document(to_bson_object(
            reader,
            tb,
            inverted,
            depth + 1,
        )?)),
        ctype::ARRAY => {
            let mut arr = Vec::new();
            loop {
                let et = reader.u8(inverted)?;
                if et == 0 {
                    break;
                }
                arr.push(to_bson_value(et, reader, tb, inverted, depth + 1)?);
            }
            Ok(Bson::Array(arr))
        }

        // Numerics.
        ctype::NUMERIC_NAN => {
            let t = tb.read_numeric()?;
            if t == K_DOUBLE {
                Ok(Bson::Double(f64::NAN))
            } else if t == K_DECIMAL {
                Ok(dec_bson(D::Nan))
            } else {
                Err(Error::format("Invalid type bits for numeric NaN"))
            }
        }
        ctype::NUMERIC_ZERO => {
            let zero_type = tb.read_zero()?;
            match zero_type {
                K_DOUBLE => Ok(Bson::Double(0.0)),
                K_INT => Ok(Bson::Int32(0)),
                K_LONG => Ok(Bson::Int64(0)),
                K_NEGATIVE_DOUBLE_ZERO => Ok(Bson::Double(-0.0)),
                z if (K_DECIMAL_ZERO_0XXX..=K_DECIMAL_ZERO_0XXX + 5).contains(&z) => {
                    let which = tb.read_decimal_zero(z)?;
                    let neg = which > dec::MAX_BEXP;
                    let bexp = if neg {
                        which - (dec::MAX_BEXP + 1)
                    } else {
                        which
                    };
                    if bexp > dec::MAX_BEXP {
                        return Err(Error::format("Invalid numeric zero decimal."));
                    }
                    Ok(dec_bson(D::Fin { neg, coef: 0, bexp }))
                }
                _ => Err(Error::format("Invalid numeric zero decimal.")),
            }
        }

        ctype::NUMERIC_NEGATIVE_LARGE_MAGNITUDE | ctype::NUMERIC_POSITIVE_LARGE_MAGNITUDE => {
            if ct == ctype::NUMERIC_NEGATIVE_LARGE_MAGNITUDE {
                inverted = !inverted;
                is_negative = true;
            }
            let original = tb.read_numeric()?;
            let mut encoded = reader.u64_be(inverted)?;
            let mut has_continuation = false;
            let mut bin: f64;
            if encoded & (1u64 << 63) == 0 {
                // In the range of finite doubles.
                has_continuation = encoded & 1 == 1;
                encoded >>= 1;
                encoded |= 1u64 << 62; // implied leading exponent bit
                bin = f64::from_bits(encoded);
                if is_negative {
                    bin = -bin;
                }
            } else if encoded == !0u64 {
                bin = if is_negative {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                };
            } else {
                // A huge decimal, stored directly.
                if original != K_DECIMAL {
                    return Err(Error::format("Invalid type bits for decimal number."));
                }
                let hi = encoded & !(1u64 << 63);
                let lo = reader.u64_be(inverted)?;
                let mut d = D::from_words(hi, lo);
                if is_negative {
                    d = negate(d);
                }
                if matches!(d, D::Fin { .. }) {
                    d = adjust_decimal_exponent(tb, d)?;
                }
                return Ok(dec_bson(d));
            }

            // 'bin' holds the input, rounded toward zero for a decimal.
            if original == K_DOUBLE {
                Ok(Bson::Double(bin))
            } else if original == K_LONG {
                if bin != i64::MIN as f64 {
                    return Err(Error::format("Unexpected value for large number."));
                }
                Ok(Bson::Int64(i64::MIN))
            } else if original == K_DECIMAL {
                let round_away = if is_negative {
                    Mode::TowardNegative
                } else {
                    Mode::TowardPositive
                };
                let mut d = from_double(bin, Prec::Digits34, round_away)?;
                if has_continuation {
                    d = read_decimal_continuation(reader, inverted, d)?;
                }
                if matches!(d, D::Fin { .. }) {
                    d = adjust_decimal_exponent(tb, d)?;
                }
                Ok(dec_bson(d))
            } else {
                Err(Error::format("Unexpected type of large number."))
            }
        }

        ctype::NUMERIC_NEGATIVE_SMALL_MAGNITUDE | ctype::NUMERIC_POSITIVE_SMALL_MAGNITUDE => {
            if ct == ctype::NUMERIC_NEGATIVE_SMALL_MAGNITUDE {
                inverted = !inverted;
                is_negative = true;
            }
            let original = tb.read_numeric()?;
            let mut encoded = reader.u64_be(inverted)?;
            match encoded >> 62 {
                0x0 => {
                    // A decimal smaller in magnitude than 2**-1074.
                    let lo = reader.u64_be(inverted)?;
                    let d = adjust_decimal_exponent(tb, D::from_words(encoded, lo))?;
                    Ok(dec_bson(if is_negative { negate(d) } else { d }))
                }
                0x1 | 0x2 => {
                    // A tiny double or decimal, 2**-1074 to 2**-255 exclusive,
                    // its exponent shifted by 256 to avoid subnormals.
                    let has_continuation = encoded & 1 == 1;
                    encoded -= 1u64 << 62;
                    encoded >>= 1;
                    let scaled_bin = f64::from_bits(encoded);
                    if original == K_DOUBLE {
                        if has_continuation {
                            return Err(Error::format("Unexpected decimal continuation."));
                        }
                        let bin = scaled_bin * f64::from_bits(((-256 + 1023) as u64) << 52);
                        return Ok(Bson::Double(if is_negative { -bin } else { bin }));
                    }
                    if !(original == K_DECIMAL && has_continuation) {
                        return Err(Error::format("Expected decimal continuation."));
                    }
                    let d = if scaled_bin
                        < f64::MIN_POSITIVE * f64::from_bits(((256 + 1023) as u64) << 52)
                    {
                        // The real double would be subnormal: scale in the
                        // decimal domain, rounding down as the encoder did.
                        let scaled = from_double(scaled_bin, Prec::Digits34, Mode::TowardZero)?
                            .to_big()
                            .ok_or_else(|| Error::format("Invalid decimal."))?;
                        let prod = scaled
                            .mul(&dec::tiny_downshift())
                            .round(34, Mode::TowardZero)
                            .0;
                        prod.to_d()
                            .ok_or_else(|| Error::format("decimal out of range"))?
                    } else {
                        let bin = scaled_bin * f64::from_bits(((-256 + 1023) as u64) << 52);
                        from_double(bin, Prec::Digits34, Mode::TowardPositive)?
                    };
                    let d = read_decimal_continuation(reader, inverted, d)?;
                    let d = adjust_decimal_exponent(tb, if is_negative { negate(d) } else { d })?;
                    Ok(dec_bson(d))
                }
                0x3 => {
                    // A small double, 2**-255 or more in magnitude.
                    let dcm = encoded & 3;
                    encoded >>= 2;
                    let bin = f64::from_bits(encoded);
                    if original == K_DOUBLE {
                        if dcm != DCM_EQUAL_TO_DOUBLE {
                            return Err(Error::format("Decimal contuation mismatch."));
                        }
                        return Ok(Bson::Double(if is_negative { -bin } else { bin }));
                    }
                    if original != K_DECIMAL {
                        return Err(Error::format(
                            "Unexpected type for small number, expected decimal.",
                        ));
                    }
                    let d = match dcm {
                        DCM_EQUAL_TO_DOUBLE_15 => {
                            from_double(bin, Prec::Digits15, Mode::TowardPositive)?
                        }
                        DCM_EQUAL_TO_DOUBLE => {
                            from_double(bin, Prec::Digits34, Mode::TowardPositive)?
                        }
                        _ => {
                            let d = from_double(bin, Prec::Digits34, Mode::TowardPositive)?;
                            read_decimal_continuation(reader, inverted, d)?
                        }
                    };
                    let d = adjust_decimal_exponent(tb, if is_negative { negate(d) } else { d })?;
                    Ok(dec_bson(d))
                }
                _ => unreachable!(),
            }
        }

        c if (ctype::NUMERIC_NEGATIVE_8_BYTE_INT..=ctype::NUMERIC_NEGATIVE_1_BYTE_INT)
            .contains(&c)
            || (ctype::NUMERIC_POSITIVE_1_BYTE_INT..=ctype::NUMERIC_POSITIVE_8_BYTE_INT)
                .contains(&c) =>
        {
            if c <= ctype::NUMERIC_NEGATIVE_1_BYTE_INT {
                inverted = !inverted;
                is_negative = true;
            }
            let original = tb.read_numeric()?;
            let mut encoded_integer_part = 0u64;
            for _ in 0..ctype::num_bytes_for_int(c) {
                encoded_integer_part =
                    (encoded_integer_part << 8) | u64::from(reader.u8(inverted)?);
            }
            let have_fractional_part = encoded_integer_part & 1 == 1;
            let integer_part = (encoded_integer_part >> 1) as i64;

            if !have_fractional_part {
                let ip = if is_negative {
                    -integer_part
                } else {
                    integer_part
                };
                return match original {
                    K_DOUBLE => Ok(Bson::Double(ip as f64)),
                    K_INT => Ok(Bson::Int32(ip as i32)),
                    K_LONG => Ok(Bson::Int64(ip)),
                    K_DECIMAL => {
                        let d = adjust_decimal_exponent(tb, D::from_i64(ip))?;
                        Ok(dec_bson(d))
                    }
                    _ => Err(Error::format("Unexpected type for positive int.")),
                };
            }

            // V1: every numeric value with a fraction has at least 8 bytes.
            let frac_bytes = 8 - ctype::num_bytes_for_int(c);
            let mut encoded_fraction = integer_part as u64;
            for _ in 0..frac_bytes {
                encoded_fraction = (encoded_fraction << 8) | u64::from(reader.u8(inverted)?);
            }
            // Zero out the DCM and convert the whole binary fraction.
            let inv_pow256 = 1.0 / (1u64 << (8 * frac_bytes)) as f64;
            let bin = ((encoded_fraction & !3u64) as f64) * inv_pow256;
            if original == K_DOUBLE {
                return Ok(Bson::Double(if is_negative { -bin } else { bin }));
            }
            // The two low bits are the DCM, except for the 8-byte case where
            // it is already known.
            let dcm = if frac_bytes > 0 {
                encoded_fraction & 3
            } else {
                DCM_LARGER_THAN_15
            };
            if original != K_DECIMAL {
                return Err(Error::format("Expected type Decimal."));
            }
            let d = match dcm {
                DCM_EQUAL_TO_DOUBLE_15 => from_double(bin, Prec::Digits15, Mode::TowardPositive)?,
                DCM_EQUAL_TO_DOUBLE => from_double(bin, Prec::Digits34, Mode::TowardPositive)?,
                _ => {
                    let d = if (integer_part as f64) > (1u64 << 53) as f64 {
                        D::from_i64(integer_part)
                    } else {
                        from_double(bin, Prec::Digits34, Mode::TowardPositive)?
                    };
                    read_decimal_continuation(reader, inverted, d)?
                }
            };
            let d = adjust_decimal_exponent(tb, if is_negative { negate(d) } else { d })?;
            Ok(dec_bson(d))
        }
        other => Err(Error::format(format!("Unknown type: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// Skipping values (`filterKeyFromKeyString`) and the key size
// ---------------------------------------------------------------------------

fn skip_bson(reader: &mut Rd<'_>, inverted: bool) -> Result<()> {
    while reader.u8(inverted)? != 0 {
        if inverted {
            reader.inverted_cstring()?;
        } else {
            reader.cstring()?;
        }
        let ct = reader.u8(inverted)?;
        skip_value(ct, reader, inverted)?;
    }
    Ok(())
}

fn skip_value(ct: u8, reader: &mut Rd<'_>, inverted: bool) -> Result<()> {
    let mut inverted = inverted;
    match ct {
        ctype::MIN_KEY
        | ctype::MAX_KEY
        | ctype::NULLISH
        | ctype::UNDEFINED
        | ctype::BOOL_TRUE
        | ctype::BOOL_FALSE
        | ctype::NUMERIC_NAN
        | ctype::NUMERIC_ZERO => {}
        ctype::DATE | ctype::TIMESTAMP => {
            reader.skip(8)?;
        }
        ctype::OID => {
            reader.skip(12)?;
        }
        ctype::STRING_LIKE | ctype::CODE => {
            reader.string_like(inverted).map(|_| ()).or_else(|e| {
                // Skipping must not depend on UTF-8 validity.
                if matches!(e, Error::Format(ref m) if m.contains("UTF-8")) {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
        }
        ctype::CODE_WITH_SCOPE => {
            if inverted {
                reader.inverted_cstring_with_nuls()?;
            } else {
                reader.cstring_with_nuls()?;
            }
            skip_bson(reader, inverted)?;
        }
        ctype::BIN_DATA => {
            let mut size = reader.u8(inverted)? as usize;
            if size == 0xff {
                size = reader.u32_be(inverted)? as usize;
            }
            reader.u8(inverted)?;
            reader.skip(size)?;
        }
        ctype::REGEX => {
            if inverted {
                reader.inverted_cstring()?;
                reader.inverted_cstring()?;
            } else {
                reader.cstring()?;
                reader.cstring()?;
            }
        }
        ctype::DB_REF => {
            let size = reader.u32_be(inverted)? as usize;
            reader.skip(size)?;
            reader.skip(12)?;
        }
        ctype::OBJECT => skip_bson(reader, inverted)?,
        ctype::ARRAY => loop {
            let et = reader.u8(inverted)?;
            if et == 0 {
                break;
            }
            skip_value(et, reader, inverted)?;
        },
        ctype::NUMERIC_NEGATIVE_LARGE_MAGNITUDE | ctype::NUMERIC_POSITIVE_LARGE_MAGNITUDE => {
            if ct == ctype::NUMERIC_NEGATIVE_LARGE_MAGNITUDE {
                inverted = !inverted;
            }
            let encoded = reader.u64_be(inverted)?;
            if encoded == !0u64 {
                return Ok(());
            }
            if encoded & (1u64 << 63) == 0 {
                if encoded & 1 == 1 {
                    reader.skip(8)?;
                }
            } else {
                reader.skip(8)?;
            }
        }
        ctype::NUMERIC_NEGATIVE_SMALL_MAGNITUDE | ctype::NUMERIC_POSITIVE_SMALL_MAGNITUDE => {
            if ct == ctype::NUMERIC_NEGATIVE_SMALL_MAGNITUDE {
                inverted = !inverted;
            }
            let encoded = reader.u64_be(inverted)?;
            match encoded >> 62 {
                0x0 => {
                    reader.skip(8)?;
                }
                0x1 | 0x2 => {
                    if encoded & 1 == 1 {
                        reader.skip(8)?;
                    }
                }
                _ => {
                    let dcm = encoded & 3;
                    if dcm == DCM_LESS_THAN_15 || dcm == DCM_LARGER_THAN_15 {
                        reader.skip(8)?;
                    }
                }
            }
        }
        c if (ctype::NUMERIC_NEGATIVE_8_BYTE_INT..=ctype::NUMERIC_NEGATIVE_1_BYTE_INT)
            .contains(&c)
            || (ctype::NUMERIC_POSITIVE_1_BYTE_INT..=ctype::NUMERIC_POSITIVE_8_BYTE_INT)
                .contains(&c) =>
        {
            if c <= ctype::NUMERIC_NEGATIVE_1_BYTE_INT {
                inverted = !inverted;
            }
            let mut encoded_integer_part = 0u64;
            for _ in 0..ctype::num_bytes_for_int(c) {
                encoded_integer_part =
                    (encoded_integer_part << 8) | u64::from(reader.u8(inverted)?);
            }
            if encoded_integer_part & 1 == 0 {
                return Ok(());
            }
            let integer_part = encoded_integer_part >> 1;
            let frac_bytes = 8 - ctype::num_bytes_for_int(c);
            let mut encoded_fraction = integer_part;
            for _ in 0..frac_bytes {
                encoded_fraction = (encoded_fraction << 8) | u64::from(reader.u8(inverted)?);
            }
            let dcm = if frac_bytes > 0 {
                encoded_fraction & 3
            } else {
                DCM_LARGER_THAN_15
            };
            if dcm != DCM_EQUAL_TO_DOUBLE_15 && dcm != DCM_EQUAL_TO_DOUBLE {
                reader.skip(8)?;
            }
        }
        other => return Err(Error::format(format!("Unknown type: {other}"))),
    }
    Ok(())
}

/// `getKeySize`: the number of bytes the key's values and end byte occupy, or
/// 0 when `data` ends before a complete key.
pub fn get_key_size(data: &[u8], ordering: Ordering) -> Result<usize> {
    if data.is_empty() {
        return Err(Error::invariant("empty KeyString"));
    }
    let mut reader = Rd::new(data);
    let mut i = 0;
    loop {
        if reader.remaining() == 0 {
            return Ok(0);
        }
        let invert = ordering.inverted(i);
        let ct = reader.u8(invert)?;
        if ct == K_END {
            return Ok(data.len() - reader.remaining());
        }
        skip_value(ct, &mut reader, invert)?;
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// RecordIds and discriminators
// ---------------------------------------------------------------------------

/// `decodeRecordIdLong(buffer)`: the RecordId at the START of `buf` and the
/// bytes it occupies.
pub fn decode_record_id_long(buf: &[u8]) -> Result<(i64, usize)> {
    if buf.len() < 2 {
        return Err(Error::format("Input too short to decode RecordId"));
    }
    let first = buf[0];
    let num_extra = (first >> 5) as usize;
    let mut repr = u64::from(first & 0x1f);
    let rid_size = num_extra + 2;
    if buf.len() < rid_size {
        return Err(Error::format("Encoded RecordId size is too big"));
    }
    let mut offset = 1;
    while offset < num_extra + 1 {
        repr = (repr << 8) | u64::from(buf[offset]);
        offset += 1;
    }
    let last = buf[offset];
    if (last & 0x7) as usize != num_extra {
        return Err(Error::format(
            "Number of extra bytes for RecordId is not encoded correctly",
        ));
    }
    repr = (repr << 5) | u64::from(last >> 3);
    Ok((repr as i64, rid_size))
}

/// `withoutRecordIdLongAtEnd`: the buffer without its trailing long RecordId,
/// and that RecordId.
pub fn without_record_id_long_at_end(buf: &[u8]) -> Result<(&[u8], i64)> {
    if buf.len() < 2 {
        return Err(Error::format("Input too short to encode RecordId"));
    }
    let last = *buf.last().unwrap();
    let rid_size = 2 + (last & 0x7) as usize;
    if buf.len() < rid_size {
        return Err(Error::format("Encoded RecordId size is too big"));
    }
    let (rid, _) = decode_record_id_long(&buf[buf.len() - rid_size..])?;
    Ok((&buf[..buf.len() - rid_size], rid))
}

/// `withoutRecordIdStrAtEnd`: the buffer without its trailing binary-string
/// RecordId, and that RecordId's bytes.
pub fn without_record_id_str_at_end(buf: &[u8]) -> Result<(&[u8], &[u8])> {
    if buf.is_empty() {
        return Err(Error::format("Input too short to decode RecordId"));
    }
    // The size is decoded right to left, up to the size byte without the
    // continuation bit.
    let mut sizes = [0u8; RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES];
    let mut size_byte_id = 0usize;
    while buf[buf.len() - 1 - size_byte_id] & 0x80 != 0 {
        if buf.len() <= size_byte_id + 1 {
            return Err(Error::format("size bytes too long"));
        }
        if size_byte_id >= RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES {
            return Err(Error::format(
                "size bytes longer than maximum allowed bytes",
            ));
        }
        sizes[size_byte_id] = buf[buf.len() - 1 - size_byte_id] & 0x7f;
        size_byte_id += 1;
    }
    if size_byte_id >= RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES {
        return Err(Error::format(
            "size bytes longer than maximum allowed bytes",
        ));
    }
    sizes[size_byte_id] = buf[buf.len() - 1 - size_byte_id];
    let num_segments = size_byte_id + 1;
    let mut rid_size = 0usize;
    for id in (0..=size_byte_id).rev() {
        rid_size += (sizes[id] as usize) << ((num_segments - id - 1) * 7);
    }
    if buf.len() < rid_size + num_segments {
        return Err(Error::format("RecordId too long"));
    }
    let offset = buf.len() - rid_size - num_segments;
    Ok((&buf[..offset], &buf[offset..offset + rid_size]))
}

/// `decodeRecordIdStrAtEnd`.
pub fn decode_record_id_str_at_end(buf: &[u8]) -> Result<&[u8]> {
    without_record_id_str_at_end(buf).map(|(_, rid)| rid)
}

/// `withoutDiscriminatorAtEnd`: drop the trailing end / discriminator byte.
pub fn without_discriminator_at_end(buf: &[u8]) -> Result<&[u8]> {
    if buf.len() < 2 {
        return Err(Error::format("Input too short to decode RecordId"));
    }
    let c = *buf.last().unwrap();
    if c != K_END && c != K_LESS && c != K_GREATER {
        return Err(Error::format(format!(
            "Expect keystring to have a kEnd, kLess, or kGreater byte at the end. Found: {c}"
        )));
    }
    Ok(&buf[..buf.len() - 1])
}
