//! The encoder: `key_string::Builder`.

use super::dec::{self, Big, Mode, Prec, D};
use super::typebits::{TypeBits, K_NEGATIVE_DOUBLE_ZERO};
use super::{
    ctype, Discriminator, Error, Ordering, Result, Version, DCM_EQUAL_TO_DOUBLE,
    DCM_EQUAL_TO_DOUBLE_15, DCM_LARGER_THAN_15, DCM_LESS_THAN_15, K_END, K_GREATER, K_LESS,
    RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES,
};
use bson::{Bson, Decimal128, Document};

/// A collation's sort-key function: turns a string into the bytes KeyString
/// stores for it (`StringTransformFn`). mongod applies it to every string value
/// in a key, nested ones included.
pub type StringTransform<'a> = &'a dyn Fn(&str) -> Vec<u8>;

/// Added to the exponent of doubles tinier than the above, to avoid subnormals.
const TINY_DOUBLE_EXPONENT_SHIFT: i32 = 256;
/// First double that isn't an int64.
const MIN_LARGE_DOUBLE: f64 = 9_223_372_036_854_775_808.0;
/// Integers larger than this may not be representable as doubles.
const MAX_INT_FOR_DOUBLE: f64 = 9_007_199_254_740_992.0;

fn tiny_upshift_f64() -> f64 {
    f64::from_bits(((TINY_DOUBLE_EXPONENT_SHIFT + 1023) as u64) << 52)
}

/// `kPow256[i]`: 256**i.
fn pow256(i: usize) -> f64 {
    (1u64 << (8 * i)) as f64
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Empty,
    AppendingElements,
    EndAdded,
    AppendedRecordId,
}

/// Builds one KeyString: values (each in the direction its position in the
/// [`Ordering`] gives), then the end byte or a discriminator, then optionally a
/// `RecordId`; and the [`TypeBits`] that go with it.
///
/// The three outputs mongod's index code uses are all one call away:
/// `finish()` returns the key bytes (with the RecordId when one was appended)
/// and the TypeBits whose `serialize()` is the index entry's value (or, for a
/// unique index and for `_id_`, follows the RecordId in it).
#[derive(Clone, Debug)]
pub struct Builder {
    version: Version,
    ordering: Ordering,
    discriminator: Discriminator,
    buf: Vec<u8>,
    type_bits: TypeBits,
    elem_count: usize,
    state: State,
    rid_size: usize,
}

impl Builder {
    /// A builder for a stored key (`Discriminator::Inclusive`).
    pub fn new(version: Version, ordering: Ordering) -> Builder {
        Builder::with_discriminator(version, ordering, Discriminator::Inclusive)
    }

    pub fn with_discriminator(
        version: Version,
        ordering: Ordering,
        discriminator: Discriminator,
    ) -> Builder {
        Builder {
            version,
            ordering,
            discriminator,
            buf: Vec::new(),
            type_bits: TypeBits::new(version),
            elem_count: 0,
            state: State::Empty,
            rid_size: 0,
        }
    }

    /// `resetToKey(obj, ord, recordId)` for a stored key: every value of `obj`
    /// in order (field names are not part of a KeyString), then the RecordId.
    pub fn key_for_document(
        version: Version,
        ordering: Ordering,
        obj: &Document,
        record_id: Option<i64>,
    ) -> Result<(Vec<u8>, TypeBits)> {
        let mut b = Builder::new(version, ordering);
        for (_, v) in obj {
            b.append_bson_element(v)?;
        }
        if let Some(rid) = record_id {
            b.append_record_id_long(rid)?;
        }
        b.finish()
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// The key bytes so far.
    pub fn buffer(&self) -> &[u8] {
        &self.buf
    }

    /// The TypeBits so far.
    pub fn type_bits(&self) -> &TypeBits {
        &self.type_bits
    }

    /// The size of the appended RecordId, if one was.
    pub fn record_id_size(&self) -> usize {
        self.rid_size
    }

    fn verify_appending_state(&mut self) -> Result<()> {
        match self.state {
            State::Empty => {
                self.state = State::AppendingElements;
                Ok(())
            }
            State::AppendingElements => Ok(()),
            _ => Err(Error::BadState(
                "a value can only be appended before the end byte".into(),
            )),
        }
    }

    fn should_invert(&self) -> bool {
        self.ordering.inverted(self.elem_count)
    }

    /// `appendBSONElement`.
    pub fn append_bson_element(&mut self, elem: &Bson) -> Result<()> {
        self.append_bson_element_with(elem, None)
    }

    /// `appendBSONElement(elem, f)`: strings pass through the collation's
    /// sort-key function first.
    pub fn append_bson_element_with(
        &mut self,
        elem: &Bson,
        f: Option<StringTransform<'_>>,
    ) -> Result<()> {
        self.verify_appending_state()?;
        let invert = self.should_invert();
        self.append_bson_value(elem, invert, None, f)?;
        self.elem_count += 1;
        Ok(())
    }

    /// `appendDiscriminator`.
    pub fn append_discriminator(&mut self, discriminator: Discriminator) -> Result<()> {
        match self.state {
            State::Empty | State::AppendingElements => {}
            _ => return Err(Error::BadState("the end byte is already appended".into())),
        }
        match discriminator {
            Discriminator::ExclusiveBefore => self.buf.push(K_LESS),
            Discriminator::ExclusiveAfter => self.buf.push(K_GREATER),
            Discriminator::Inclusive => self.buf.push(K_END),
        }
        self.state = State::EndAdded;
        Ok(())
    }

    fn done_appending(&mut self) -> Result<()> {
        if self.state == State::AppendingElements {
            let d = self.discriminator;
            self.append_discriminator(d)?;
        }
        Ok(())
    }

    /// `appendRecordId` for a long RecordId (a normal collection).
    pub fn append_record_id_long(&mut self, rid: i64) -> Result<()> {
        self.done_appending()?;
        if self.state == State::AppendedRecordId {
            return Err(Error::BadState("a RecordId is already appended".into()));
        }
        self.state = State::AppendedRecordId;
        let before = self.buf.len();
        self.append_record_id_long_bytes(rid)?;
        self.rid_size = self.buf.len() - before;
        Ok(())
    }

    /// `appendRecordId` for a binary-string RecordId (a clustered collection).
    pub fn append_record_id_bytes(&mut self, rid: &[u8]) -> Result<()> {
        self.done_appending()?;
        if self.state == State::AppendedRecordId {
            return Err(Error::BadState("a RecordId is already appended".into()));
        }
        self.state = State::AppendedRecordId;
        let before = self.buf.len();
        self.append_record_id_str(rid)?;
        self.rid_size = self.buf.len() - before;
        Ok(())
    }

    /// The finished key (end byte or discriminator and RecordId included) and
    /// its TypeBits.
    pub fn finish(mut self) -> Result<(Vec<u8>, TypeBits)> {
        self.done_appending()?;
        Ok((self.buf, self.type_bits))
    }

    /// `appendTypeBits` + the whole buffer: key bytes followed by the stored
    /// form of the TypeBits, as `Value::getViewWithTypeBits` lays them out.
    /// Also returns the length of the key part.
    pub fn finish_with_type_bits(self) -> Result<(Vec<u8>, usize)> {
        let (mut key, tb) = self.finish()?;
        let n = key.len();
        key.extend_from_slice(&tb.serialize());
        Ok((key, n))
    }

    // ---------------------------------------------------------------------
    // RecordIds
    // ---------------------------------------------------------------------

    fn append_record_id_long_bytes(&mut self, val: i64) -> Result<()> {
        // The encoding can find its own length from the last byte: a count N
        // of the bytes between the first and last (0..=7) sits in the high 3
        // bits of the first byte and the low 3 bits of the last, and the rest
        // is the 64-bit id, big-endian. Negative ids are not encoded.
        let mut raw = val;
        if raw < 0 {
            // `RecordId::minLong()` and the null RecordId encode as 0.
            if raw != i64::MIN {
                return Err(Error::invariant("a negative RecordId in an index key"));
            }
            raw = 0;
        }
        let value = raw as u64;
        let bits_needed = 64 - value.leading_zeros() as usize;
        let extra = if bits_needed <= 10 {
            0
        } else {
            (bits_needed - 10).div_ceil(8)
        };
        if extra >= 8 {
            return Err(Error::invariant("RecordId too large"));
        }
        let first = ((extra as u64) << 5 | (value >> (5 + extra * 8))) as u8;
        let last = ((value << 3) as u8) | extra as u8;
        self.buf.push(first);
        if extra > 0 {
            let be = (value >> 5).to_be_bytes();
            self.buf.extend_from_slice(&be[8 - extra..]);
        }
        self.buf.push(last);
        Ok(())
    }

    fn append_record_id_str(&mut self, s: &[u8]) -> Result<()> {
        // The string as-is, then its size in 7-bit groups, most significant
        // group last-but-continuation: every size byte but the leftmost (the
        // last one) has its 8th bit set, so the size decodes right to left.
        let size = s.len();
        if size == 0 {
            return Err(Error::invariant("an empty string RecordId"));
        }
        if size >= 1 << (7 * RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES) {
            return Err(Error::invariant("a string RecordId is too long"));
        }
        let mut encoded = [0u8; RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES];
        let mut highest = 0usize;
        let mut set = false;
        for n in (0..RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES).rev() {
            encoded[n] = ((size >> (n * 7)) & 0x7f) as u8;
            if encoded[n] != 0 && !set {
                set = true;
                highest = n;
            }
        }
        for e in encoded.iter_mut().take(highest + 1).skip(1) {
            *e |= 0x80;
        }
        self.buf.extend_from_slice(s);
        self.buf.extend_from_slice(&encoded[..=highest]);
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Low-level appends
    // ---------------------------------------------------------------------

    fn put(&mut self, b: u8, invert: bool) {
        self.buf.push(if invert { !b } else { b });
    }

    fn put_bytes(&mut self, bytes: &[u8], invert: bool) {
        if invert {
            self.buf.extend(bytes.iter().map(|b| !b));
        } else {
            self.buf.extend_from_slice(bytes);
        }
    }

    fn put_u64(&mut self, v: u64, invert: bool) {
        self.put_bytes(&v.to_be_bytes(), invert);
    }

    // ---------------------------------------------------------------------
    // Values
    // ---------------------------------------------------------------------

    fn append_bson_value(
        &mut self,
        elem: &Bson,
        invert: bool,
        name: Option<&str>,
        f: Option<StringTransform<'_>>,
    ) -> Result<()> {
        if let Some(n) = name {
            self.put_bytes(n.as_bytes(), invert);
            self.put(0, invert);
        }
        match elem {
            Bson::MinKey => self.put(ctype::MIN_KEY, invert),
            Bson::MaxKey => self.put(ctype::MAX_KEY, invert),
            Bson::Null => self.put(ctype::NULLISH, invert),
            Bson::Undefined => self.put(ctype::UNDEFINED, invert),
            Bson::Double(d) => self.append_number_double(*d, invert),
            Bson::String(s) => self.append_string(s, invert, f),
            Bson::Document(d) => {
                self.put(ctype::OBJECT, invert);
                self.append_bson(d, invert, f)?;
            }
            Bson::Array(a) => {
                self.put(ctype::ARRAY, invert);
                for e in a {
                    // No generic ctype byte: no name is encoded.
                    self.append_bson_value(e, invert, None, f)?;
                }
                self.put(0, invert);
            }
            Bson::Binary(b) => {
                self.put(ctype::BIN_DATA, invert);
                let len = b.bytes.len();
                if len < 0xff {
                    self.put(len as u8, invert);
                } else {
                    self.put(0xff, invert);
                    self.put_bytes(&(len as i32).to_be_bytes(), invert);
                }
                self.put(u8::from(b.subtype), invert);
                self.put_bytes(&b.bytes, invert);
            }
            Bson::ObjectId(o) => {
                self.put(ctype::OID, invert);
                self.put_bytes(&o.bytes(), invert);
            }
            Bson::Boolean(v) => self.put(
                if *v {
                    ctype::BOOL_TRUE
                } else {
                    ctype::BOOL_FALSE
                },
                invert,
            ),
            Bson::DateTime(d) => {
                self.put(ctype::DATE, invert);
                // Offset binary: flip the sign bit.
                let enc = (d.timestamp_millis() as u64) ^ (1u64 << 63);
                self.put_u64(enc, invert);
            }
            Bson::RegularExpression(r) => {
                self.put(ctype::REGEX, invert);
                self.put_bytes(r.pattern.as_bytes(), invert);
                self.put(0, invert);
                self.put_bytes(r.options.as_bytes(), invert);
                self.put(0, invert);
            }
            Bson::DbPointer(_) => {
                let (ns, oid) = db_pointer_parts(elem)
                    .ok_or_else(|| Error::invariant("unreadable DBPointer"))?;
                self.put(ctype::DB_REF, invert);
                self.put_bytes(&(ns.len() as i32).to_be_bytes(), invert);
                self.put_bytes(ns.as_bytes(), invert);
                self.put_bytes(&oid, invert);
            }
            Bson::Symbol(s) => {
                if f.is_some() {
                    return Err(Error::CannotBuildIndexKeys(format!(
                        "KeyString format error: Cannot index type Symbol with a collation. \
                         Failed to index element: {elem}."
                    )));
                }
                self.type_bits.append_symbol();
                self.put(ctype::STRING_LIKE, invert); // Symbols and Strings compare equally
                self.append_string_like(s.as_bytes(), invert);
            }
            Bson::JavaScriptCode(s) => {
                self.put(ctype::CODE, invert);
                self.append_string_like(s.as_bytes(), invert);
            }
            Bson::JavaScriptCodeWithScope(c) => {
                self.put(ctype::CODE_WITH_SCOPE, invert);
                self.append_string_like(c.code.as_bytes(), invert);
                self.append_bson(&c.scope, invert, None)?;
            }
            Bson::Int32(v) => {
                self.type_bits.append_number_int();
                self.append_integer(i64::from(*v), invert);
            }
            Bson::Int64(v) => {
                self.type_bits.append_number_long();
                self.append_integer(*v, invert);
            }
            Bson::Timestamp(t) => {
                self.put(ctype::TIMESTAMP, invert);
                let v = (u64::from(t.time) << 32) | u64::from(t.increment);
                self.put_u64(v, invert);
            }
            Bson::Decimal128(d) => self.append_number_decimal(d, invert)?,
        }
        Ok(())
    }

    fn append_string(&mut self, s: &str, invert: bool, f: Option<StringTransform<'_>>) {
        self.type_bits.append_string();
        self.put(ctype::STRING_LIKE, invert);
        match f {
            Some(t) => {
                let b = t(s);
                self.append_string_like(&b, invert);
            }
            None => self.append_string_like(s.as_bytes(), invert),
        }
    }

    /// `_appendStringLike`: each NUL becomes `00 FF`, then a NUL ends it.
    fn append_string_like(&mut self, s: &[u8], invert: bool) {
        for &b in s {
            if b == 0 {
                self.put_bytes(&[0x00, 0xff], invert);
            } else {
                self.put(b, invert);
            }
        }
        self.put(0, invert);
    }

    /// `_appendBson`: each element as (generic ctype, name, value).
    fn append_bson(
        &mut self,
        obj: &Document,
        invert: bool,
        f: Option<StringTransform<'_>>,
    ) -> Result<()> {
        for (name, v) in obj {
            // Force the order to be based on (ctype, name, value).
            self.put(generic_ctype(v), invert);
            self.append_bson_value(v, invert, Some(name), f)?;
        }
        self.put(0, invert);
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Numbers
    // ---------------------------------------------------------------------

    fn append_number_double(&mut self, num: f64, invert: bool) {
        if num == 0.0 && num.is_sign_negative() {
            self.type_bits.append_zero(K_NEGATIVE_DOUBLE_ZERO);
        } else {
            self.type_bits.append_number_double();
        }
        self.append_double_without_type_bits(num, DCM_EQUAL_TO_DOUBLE, invert);
    }

    fn append_double_without_type_bits(&mut self, num: f64, dcm: u64, invert: bool) {
        let is_negative = num < 0.0;
        let magnitude = if is_negative { -num } else { num };

        // Tests are structured such that NaNs and infinities fall through.
        if magnitude.is_nan() || magnitude < 1.0 {
            if magnitude > 0.0 {
                // This includes subnormal numbers.
                self.append_small_double(num, dcm, invert);
            } else if num == 0.0 {
                // -0.0 and 0.0 collapse here; the type bits tell them apart.
                self.put(ctype::NUMERIC_ZERO, invert);
            } else {
                debug_assert!(num.is_nan());
                self.put(ctype::NUMERIC_NAN, invert);
            }
            return;
        }

        if magnitude < MIN_LARGE_DOUBLE {
            let integer_part = magnitude as u64;
            if integer_part as f64 == magnitude && dcm == DCM_EQUAL_TO_DOUBLE {
                // No fractional part.
                self.append_preshifted_integer_portion(integer_part << 1, is_negative, invert);
                return;
            }

            let fractional_bytes = ((integer_part << 1).leading_zeros() / 8) as usize;
            let ct = if is_negative {
                ctype::NUMERIC_NEGATIVE_8_BYTE_INT + fractional_bytes as u8
            } else {
                ctype::NUMERIC_POSITIVE_8_BYTE_INT - fractional_bytes as u8
            };
            self.put(ct, invert);

            // Multiplying by 256**X is a logical shift left by X bytes.
            let mut encoding = (magnitude * pow256(fractional_bytes)) as u64;
            // Merge in the bit saying there is a fractional part by doubling the
            // integer part and adding 1; the low bytes are the fractional bytes
            // and the two low bits are left free for the DCM.
            encoding = encoding.wrapping_add((integer_part + 1) << (fractional_bytes * 8));
            debug_assert_eq!(encoding & 0x3, 0);
            encoding |= dcm;
            self.put_u64(encoding, if is_negative { !invert } else { invert });
        } else {
            self.append_large_double(num, dcm, invert);
        }
    }

    fn append_small_double(&mut self, value: f64, dcm: u64, invert: bool) {
        let is_negative = value < 0.0;
        let mut magnitude = if is_negative { -value } else { value };
        debug_assert!(!value.is_nan() && value != 0.0 && magnitude < 1.0);

        self.put(
            if is_negative {
                ctype::NUMERIC_NEGATIVE_SMALL_MAGNITUDE
            } else {
                ctype::NUMERIC_POSITIVE_SMALL_MAGNITUDE
            },
            invert,
        );

        let mut encoded: u64;
        if magnitude >= TINIEST_DOUBLE_WITH_2_BIT_DCM_VALUE {
            // Values in [2**-255, 1) get the prefix 0b11.
            encoded = magnitude.to_bits();
            debug_assert_eq!(encoded & (0x3u64 << 62), 0);
            encoded <<= 2;
            encoded |= dcm;
            debug_assert_eq!(encoded >> 62, 0x3);
        } else {
            debug_assert_ne!(dcm, DCM_EQUAL_TO_DOUBLE_15);
            // [denorm_min, 2**-255) get the prefixes 0b01 or 0b10; 0b00 belongs
            // to decimals smaller than that.
            magnitude *= tiny_upshift_f64();
            encoded = magnitude.to_bits();
            encoded <<= 1;
            encoded |= u64::from(dcm != DCM_EQUAL_TO_DOUBLE);
            // Change the two top bits from 0b00 or 0b01 to 0b01 or 0b10.
            encoded = encoded.wrapping_add(1u64 << 62);
            debug_assert!(encoded >> 62 == 0x1 || encoded >> 62 == 0x2);
        }

        self.put_u64(encoded, if is_negative { !invert } else { invert });
    }

    fn append_large_double(&mut self, value: f64, dcm: u64, invert: bool) {
        debug_assert!(!value.is_nan() && value != 0.0);
        debug_assert_ne!(dcm, DCM_EQUAL_TO_DOUBLE_15);

        self.put(
            if value > 0.0 {
                ctype::NUMERIC_POSITIVE_LARGE_MAGNITUDE
            } else {
                ctype::NUMERIC_NEGATIVE_LARGE_MAGNITUDE
            },
            invert,
        );

        let mut encoded = value.to_bits();
        if value.is_finite() {
            encoded <<= 1;
            encoded &= !(1u64 << 63);
            encoded |= u64::from(dcm != DCM_EQUAL_TO_DOUBLE);
        } else {
            encoded = !0u64; // infinity
        }
        self.put_u64(encoded, if value > 0.0 { invert } else { !invert });
    }

    /// Handles NumberLong and NumberInt, which encode alike.
    fn append_integer(&mut self, num: i64, invert: bool) {
        if num == i64::MIN {
            // -2**63 is exactly a double and not a positive int64.
            self.append_large_double(num as f64, DCM_EQUAL_TO_DOUBLE, invert);
            return;
        }
        if num == 0 {
            self.put(ctype::NUMERIC_ZERO, invert);
            return;
        }
        let is_negative = num < 0;
        let magnitude = num.unsigned_abs();
        self.append_preshifted_integer_portion(magnitude << 1, is_negative, invert);
    }

    fn append_preshifted_integer_portion(&mut self, value: u64, is_negative: bool, invert: bool) {
        debug_assert!(value != 0 && value != 1);
        let bytes_needed = (64 - value.leading_zeros() as usize).div_ceil(8);
        let be = value.to_be_bytes();
        let used = &be[8 - bytes_needed..];
        if is_negative {
            self.put(
                ctype::NUMERIC_NEGATIVE_1_BYTE_INT - (bytes_needed as u8 - 1),
                invert,
            );
            self.put_bytes(used, !invert);
        } else {
            self.put(
                ctype::NUMERIC_POSITIVE_1_BYTE_INT + (bytes_needed as u8 - 1),
                invert,
            );
            self.put_bytes(used, invert);
        }
    }

    fn append_number_decimal(&mut self, d128: &Decimal128, invert: bool) -> Result<()> {
        let dec = D::from_bson(d128);
        let (neg, coef, bexp) = match dec {
            D::Fin { neg, coef, bexp } => (neg, coef, bexp),
            D::Nan => {
                self.put(ctype::NUMERIC_NAN, invert);
                self.type_bits.append_number_decimal();
                return Ok(());
            }
            D::Inf { neg } => {
                self.put(
                    if neg {
                        ctype::NUMERIC_NEGATIVE_LARGE_MAGNITUDE
                    } else {
                        ctype::NUMERIC_POSITIVE_LARGE_MAGNITUDE
                    },
                    invert,
                );
                self.put_u64(!0u64, if neg { !invert } else { invert });
                self.type_bits.append_number_decimal();
                return Ok(());
            }
        };
        if coef == 0 {
            let mut zero_exp = bexp;
            if neg {
                zero_exp += dec::MAX_BEXP + 1;
            }
            self.type_bits.append_decimal_zero(zero_exp);
            self.put(ctype::NUMERIC_ZERO, invert);
            return Ok(());
        }

        self.type_bits.append_number_decimal();
        self.type_bits.append_decimal_exponent((bexp & 0x3f) as u8);

        let big = Big::from_u128(neg, coef, bexp as i32 - dec::BIAS);
        let (bin, inexact, overflow) = dec::to_double_toward_zero(&big);

        // Easy case: the decimal actually is a double (many integers, 1.5...).
        if !inexact && !overflow {
            self.append_double_without_type_bits(bin, DCM_EQUAL_TO_DOUBLE, invert);
            return Ok(());
        }

        // Values below the double normal range need special handling: a double
        // would not give 15 digits, if any at all.
        let abs_bin = bin.abs();
        if abs_bin < f64::MIN_POSITIVE {
            return self.append_tiny_decimal(&big, bin, invert);
        }

        // Huge finite values go in directly: not exact, and truncating to the
        // maximum double, so the decimal is outside the range of finite doubles.
        if abs_bin >= f64::MAX {
            return self.append_huge_decimal(&big, invert);
        }

        let round_away = if neg {
            Mode::TowardNegative
        } else {
            Mode::TowardPositive
        };
        const K_1E15: u128 = 1_000_000_000_000_000;

        // If these fall through, a decimal continuation represents the
        // difference between the stored value and the decimal; every path that
        // falls through sets `stored_value`.
        let stored_value: Big = if abs_bin >= MIN_LARGE_DOUBLE {
            // Large decimals not exactly a double need a continuation even with
            // only 15 digits: there is a single DCM bit.
            self.append_large_double(bin, DCM_LARGER_THAN_15, invert);
            from_double_big(bin, Prec::Digits34, round_away)?
        } else if abs_bin < TINIEST_DOUBLE_WITH_2_BIT_DCM_VALUE {
            self.append_small_double(bin, DCM_LARGER_THAN_15, invert);
            from_double_big(bin, Prec::Digits34, round_away)?
        } else if abs_bin >= MAX_INT_FOR_DOUBLE {
            // 'bin' may have lost precision in the integer part, which would
            // miscompare with integers; handle the integer part explicitly.
            let (truncated, has_fraction) = big.truncate_to_integer();
            let integer_part = truncated
                .magnitude_u128()
                .filter(|v| *v < 1u128 << 63)
                .ok_or_else(|| Error::invariant("decimal integer part out of range"))?
                as i64;
            let is_neg = truncated.neg;
            let has_8_bytes = integer_part >= (1i64 << 55);
            let preshifted = (integer_part as u64) << 1;
            self.append_preshifted_integer_portion(
                preshifted | u64::from(has_fraction),
                is_neg,
                invert,
            );
            if !has_fraction {
                return Ok(());
            }
            if !has_8_bytes {
                // A fraction byte follows, but the leading 7 bytes already hold
                // 53 bits of the coefficient: just store the DCM.
                self.put(
                    DCM_LARGER_THAN_15 as u8,
                    if is_neg { !invert } else { invert },
                );
            }
            Big::from_u128(is_neg, integer_part as u128, 0)
        } else if coef < K_1E15 {
            // Common case: at most 15 digits, within the double range where two
            // spare bits hold the DCM, so 'bin' carries 15 digits of precision.
            self.append_double_without_type_bits(bin, DCM_EQUAL_TO_DOUBLE_15, invert);
            return Ok(());
        } else {
            // More digits, but maybe 15 after removing trailing zeros.
            let dec_from_bin = from_double_big(bin, Prec::Digits15, round_away)?;
            if dec_from_bin.cmp_val(&big) == std::cmp::Ordering::Equal {
                self.append_double_without_type_bits(bin, DCM_EQUAL_TO_DOUBLE_15, invert);
                return Ok(());
            }
            // Harder: a decimal continuation. First the double and the kind.
            let dcm = if (big.cmp_val(&dec_from_bin) == std::cmp::Ordering::Less) != neg {
                DCM_LESS_THAN_15
            } else {
                DCM_LARGER_THAN_15
            };
            self.append_double_without_type_bits(bin, dcm, invert);
            from_double_big(bin, Prec::Digits34, round_away)?
        };

        let stored_value = stored_value.normalized34();
        let dec_diff = big.sub(&stored_value);
        if !(dec_diff.neg == neg || dec_diff.is_zero()) {
            return Err(Error::invariant("decimal continuation has the wrong sign"));
        }
        if !dec_diff.is_zero() && dec_diff.exp != stored_value.exp {
            return Err(Error::invariant(
                "decimal continuation exponent differs from the stored value's",
            ));
        }
        let continuation = dec_diff
            .coef_u128()
            .filter(|c| *c <= u128::from(u64::MAX))
            .ok_or_else(|| Error::invariant("decimal continuation exceeds 64 bits"))?
            as u64;
        // The type bits give the precision; the double plus this continuation
        // recovers the value.
        self.put_u64(continuation, if neg { !invert } else { invert });
        Ok(())
    }

    fn append_tiny_decimal(&mut self, dec: &Big, bin: f64, invert: bool) -> Result<()> {
        let is_negative = dec.neg;
        let magnitude = Big {
            neg: false,
            digits: dec.digits.clone(),
            exp: dec.exp,
        };
        self.put(
            if is_negative {
                ctype::NUMERIC_NEGATIVE_SMALL_MAGNITUDE
            } else {
                ctype::NUMERIC_POSITIVE_SMALL_MAGNITUDE
            },
            invert,
        );
        let flip = if is_negative { !invert } else { invert };

        // Smaller than the smallest subnormal double: store the decimal itself.
        if bin == 0.0 {
            let normalized = magnitude.normalized34();
            let (hi, lo) = normalized
                .to_d()
                .ok_or_else(|| Error::invariant("tiny decimal out of range"))?
                .words();
            if hi & (0x3u64 << 62) != 0 {
                return Err(Error::invariant("tiny decimal high bits"));
            }
            self.put_u64(hi, flip);
            self.put_u64(lo, flip);
            return Ok(());
        }

        // A decimal in the subnormal double range is scaled in the decimal
        // domain, rounding down at each step but never below the subnormal.
        let scaled_dec = magnitude
            .mul(&dec::tiny_upshift())
            .round(34, Mode::TowardZero)
            .0;
        let scaled_bin = dec::to_double_toward_zero(&scaled_dec).0;

        // 'scaled_bin' holds the first 15 digits of the scaled decimal and sorts
        // correctly with the scaled double.
        let scaled_bin = scaled_bin.max(bin.abs() * tiny_upshift_f64());
        let mut encoded = scaled_bin.to_bits();
        encoded <<= 1;
        encoded |= 1; // even a zero difference is not exactly equal
        encoded = encoded.wrapping_add(1u64 << 62);
        debug_assert_eq!(encoded >> 62, 0x1);
        self.put_u64(encoded, flip);

        // Round down so a non-negative continuation always exists.
        let stored = from_double_big(scaled_bin, Prec::Digits34, Mode::TowardZero)?;
        let stored = stored
            .mul(&dec::tiny_downshift())
            .round(34, Mode::TowardZero)
            .0
            .normalized34();
        if stored.cmp_val(&magnitude) != std::cmp::Ordering::Less {
            return Err(Error::invariant("tiny decimal stored value is not smaller"));
        }
        let dec_diff = magnitude.sub(&stored);
        if dec_diff.neg {
            return Err(Error::invariant("tiny decimal continuation is negative"));
        }
        let continuation = dec_diff
            .coef_u128()
            .filter(|c| *c <= u128::from(u64::MAX))
            .ok_or_else(|| Error::invariant("tiny decimal continuation exceeds 64 bits"))?
            as u64;
        self.put_u64(continuation, flip);
        Ok(())
    }

    fn append_huge_decimal(&mut self, dec: &Big, invert: bool) -> Result<()> {
        // To use kNumeric{Negative,Positive}LargeMagnitude we must fit between
        // the highest finite double and +/-Inf: force the high bit to 1 (large
        // doubles have it 0) and never encode ~0 here.
        let is_negative = dec.neg;
        let magnitude = Big {
            neg: false,
            digits: dec.digits.clone(),
            exp: dec.exp,
        };
        let (mut hi, lo) = magnitude
            .normalized34()
            .to_d()
            .ok_or_else(|| Error::invariant("huge decimal out of range"))?
            .words();
        if hi >= 1u64 << 63 {
            return Err(Error::invariant("huge decimal high bit"));
        }
        hi |= 1u64 << 63;
        self.put(
            if is_negative {
                ctype::NUMERIC_NEGATIVE_LARGE_MAGNITUDE
            } else {
                ctype::NUMERIC_POSITIVE_LARGE_MAGNITUDE
            },
            invert,
        );
        let flip = if is_negative { !invert } else { invert };
        self.put_u64(hi, flip);
        self.put_u64(lo, flip);
        Ok(())
    }
}

/// `kTiniestDoubleWith2BitDCM` = `2**-255`: doubles smaller than this store only
/// a single bit saying a decimal continuation follows.
const TINIEST_DOUBLE_WITH_2_BIT_DCM_VALUE: f64 = f64::from_bits(((-255i64 + 1023) as u64) << 52);

fn from_double_big(x: f64, prec: Prec, mode: Mode) -> Result<Big> {
    dec::from_double(x, prec, mode)
        .and_then(|d| d.to_big())
        .ok_or_else(|| Error::invariant("double to decimal conversion"))
}

/// `bsonTypeToGenericKeyStringType`.
pub(crate) fn generic_ctype(v: &Bson) -> u8 {
    match v {
        Bson::MinKey => ctype::MIN_KEY,
        Bson::Null => ctype::NULLISH,
        Bson::Undefined => ctype::UNDEFINED,
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => ctype::NUMERIC,
        Bson::String(_) | Bson::Symbol(_) => ctype::STRING_LIKE,
        Bson::Document(_) => ctype::OBJECT,
        Bson::Array(_) => ctype::ARRAY,
        Bson::Binary(_) => ctype::BIN_DATA,
        Bson::ObjectId(_) => ctype::OID,
        Bson::Boolean(_) => ctype::BOOL,
        Bson::DateTime(_) => ctype::DATE,
        Bson::Timestamp(_) => ctype::TIMESTAMP,
        Bson::RegularExpression(_) => ctype::REGEX,
        Bson::DbPointer(_) => ctype::DB_REF,
        Bson::JavaScriptCode(_) => ctype::CODE,
        Bson::JavaScriptCodeWithScope(_) => ctype::CODE_WITH_SCOPE,
        Bson::MaxKey => ctype::MAX_KEY,
    }
}

/// A DBPointer's namespace and ObjectId bytes. `bson` keeps both private, so
/// they are read back from the value's own BSON encoding.
fn db_pointer_parts(v: &Bson) -> Option<(String, [u8; 12])> {
    let mut d = Document::new();
    d.insert("x", v.clone());
    let raw = bson::to_vec(&d).ok()?;
    // doc: i32 len, type byte 0x0c, "x\0", i32 strlen, string + NUL, 12 oid bytes.
    if raw.get(4) != Some(&0x0c) {
        return None;
    }
    let at = 4 + 1 + 2;
    let slen = i32::from_le_bytes(raw.get(at..at + 4)?.try_into().ok()?) as usize;
    let s = raw.get(at + 4..at + 4 + slen.checked_sub(1)?)?;
    let oid_at = at + 4 + slen;
    let oid: [u8; 12] = raw.get(oid_at..oid_at + 12)?.try_into().ok()?;
    Some((String::from_utf8(s.to_vec()).ok()?, oid))
}

/// A DBPointer from its namespace and ObjectId bytes (via its BSON encoding;
/// the type has no public constructor).
pub(crate) fn make_db_pointer(ns: &str, oid: [u8; 12]) -> Option<Bson> {
    let mut body = Vec::new();
    body.push(0x0c);
    body.extend_from_slice(b"x\0");
    body.extend_from_slice(&((ns.len() + 1) as i32).to_le_bytes());
    body.extend_from_slice(ns.as_bytes());
    body.push(0);
    body.extend_from_slice(&oid);
    body.push(0);
    let mut doc = Vec::new();
    doc.extend_from_slice(&((body.len() + 4) as i32).to_le_bytes());
    doc.extend_from_slice(&body);
    let d = Document::from_reader(&mut doc.as_slice()).ok()?;
    d.get("x").cloned()
}
