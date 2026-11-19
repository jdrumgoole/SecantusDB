//! `key_string::TypeBits`: the bits that restore the original BSON types of a
//! KeyString (1 and 1.0 and NumberLong(1) share key bytes), stored as the value
//! of an index entry.

use super::{Error, Result, Version};

const MAX_BYTES_FOR_SHORT_ENCODING: usize = 127;
const STORED_DECIMAL_EXPONENT_BITS: u32 = 6;

pub(super) const K_STRING: u8 = 0x0;
pub(super) const K_SYMBOL: u8 = 0x1;
pub(super) const K_INT: u8 = 0x0;
pub(super) const K_LONG: u8 = 0x1;
pub(super) const K_DOUBLE: u8 = 0x2;
pub(super) const K_DECIMAL: u8 = 0x3;
pub(super) const K_SPECIAL_ZERO_PREFIX: u8 = 0x3;
pub(super) const K_NEGATIVE_DOUBLE_ZERO: u8 = 0x3;
const K_V1_NEGATIVE_DOUBLE_ZERO: u8 = 0x18;
pub(super) const K_DECIMAL_ZERO_0XXX: u8 = 0x1a;
const K_DECIMAL_ZERO_5XXX: u8 = 0x1f;

/// What a KeyString's TypeBits hold while it is being built, and what an index
/// entry stores once it is done. Bits are packed low to high.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeBits {
    version: Version,
    data: Vec<u8>,
    cur_bit: u32,
    all_zeros: bool,
}

impl TypeBits {
    pub fn new(version: Version) -> TypeBits {
        TypeBits {
            version,
            data: Vec::new(),
            cur_bit: 0,
            all_zeros: true,
        }
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// True when no bit was set to one (also when none was set at all).
    pub fn is_all_zeros(&self) -> bool {
        self.all_zeros
    }

    /// `getBuffer()` / `getSize()`: the size-prefixed form. An all-zero value
    /// is the single byte `0x00`.
    pub fn to_buffer(&self) -> Vec<u8> {
        if self.all_zeros {
            return vec![0];
        }
        let n = self.data.len();
        if n == 1 && self.data[0] & 0x80 == 0 {
            return vec![self.data[0]];
        }
        let mut out = Vec::with_capacity(n + 5);
        if n > MAX_BYTES_FOR_SHORT_ENCODING {
            out.push(0x80);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        } else {
            out.push(0x80 | n as u8);
        }
        out.extend_from_slice(&self.data);
        out
    }

    /// The bytes an index entry stores: what
    /// `KeyString::Value::getTypeBitsView()` holds. `Builder::appendTypeBits`
    /// writes AllZeros as an empty buffer, and that is what mongod stores.
    pub fn serialize(&self) -> Vec<u8> {
        if self.all_zeros {
            Vec::new()
        } else {
            self.to_buffer()
        }
    }

    /// `TypeBits::fromBuffer`: an empty buffer is AllZeros. Reads the whole
    /// of `buf` as one TypeBits and returns it with the bytes it consumed.
    pub fn from_buffer(version: Version, buf: &[u8]) -> Result<TypeBits> {
        Self::from_buffer_prefix(version, buf).map(|(t, _)| t)
    }

    /// Like [`from_buffer`](Self::from_buffer), also returning how many bytes
    /// of `buf` the TypeBits occupied.
    pub fn from_buffer_prefix(version: Version, buf: &[u8]) -> Result<(TypeBits, usize)> {
        let mut tb = TypeBits::new(version);
        if buf.is_empty() {
            return Ok((tb, 0));
        }
        let first = buf[0];
        let (size, skip) = if first > 0 && first < 0x80 {
            (1usize, 0usize)
        } else if first > 0x80 {
            ((first & 0x7f) as usize, 1)
        } else if first == 0x80 {
            if buf.len() < 5 {
                return Err(Error::format("Invalid size byte(s)."));
            }
            let s = u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize;
            if s <= MAX_BYTES_FOR_SHORT_ENCODING {
                return Err(Error::format("Invalid overlong encoding."));
            }
            (s, 5)
        } else {
            (0, 1)
        };
        if buf.len() < skip + size {
            return Err(Error::format("Invalid size byte(s)."));
        }
        if size > 0 {
            tb.all_zeros = false;
        }
        tb.data = buf[skip..skip + size].to_vec();
        tb.cur_bit = (size * 8) as u32;
        Ok((tb, skip + size))
    }

    fn append_bit(&mut self, one_or_zero: u8) {
        debug_assert!(one_or_zero <= 1);
        if one_or_zero == 1 {
            self.all_zeros = false;
        }
        let byte = (self.cur_bit / 8) as usize;
        let off = self.cur_bit % 8;
        if off == 0 {
            self.data.push(one_or_zero);
        } else {
            self.data[byte] |= one_or_zero << off;
        }
        self.cur_bit += 1;
    }

    pub(super) fn append_string(&mut self) {
        self.append_bit(K_STRING);
    }

    pub(super) fn append_symbol(&mut self) {
        self.append_bit(K_SYMBOL);
    }

    fn append_two(&mut self, v: u8) {
        self.append_bit(v >> 1);
        self.append_bit(v & 1);
    }

    pub(super) fn append_number_double(&mut self) {
        self.append_two(K_DOUBLE);
    }

    pub(super) fn append_number_int(&mut self) {
        self.append_two(K_INT);
    }

    pub(super) fn append_number_long(&mut self) {
        self.append_two(K_LONG);
    }

    pub(super) fn append_number_decimal(&mut self) {
        self.append_two(K_DECIMAL);
    }

    /// `appendZero`: the 2-bit zero types, `kNegativeDoubleZero` (5 bits in V1)
    /// and the six 5-bit decimal zero prefixes.
    pub(super) fn append_zero(&mut self, zero_type: u8) {
        let mut z = zero_type;
        match z {
            K_INT | K_DOUBLE | K_LONG => {
                self.append_two(z);
                return;
            }
            K_NEGATIVE_DOUBLE_ZERO => z = K_V1_NEGATIVE_DOUBLE_ZERO,
            K_DECIMAL_ZERO_0XXX..=K_DECIMAL_ZERO_5XXX => {}
            _ => unreachable!("appendZero({zero_type})"),
        }
        debug_assert_eq!(z >> 3, 3);
        for bit_pos in (0..=4).rev() {
            self.append_bit((z >> bit_pos) & 1);
        }
    }

    pub(super) fn append_decimal_zero(&mut self, which_zero: u32) {
        debug_assert!((which_zero >> 12) <= (K_DECIMAL_ZERO_5XXX - K_DECIMAL_ZERO_0XXX) as u32);
        self.append_zero(((which_zero >> 12) as u8) + K_DECIMAL_ZERO_0XXX);
        for bit_pos in (0..=11).rev() {
            self.append_bit(((which_zero >> bit_pos) & 1) as u8);
        }
    }

    pub(super) fn append_decimal_exponent(&mut self, stored_exponent_bits: u8) {
        debug_assert!(u32::from(stored_exponent_bits) < (1 << STORED_DECIMAL_EXPONENT_BITS));
        for bit_pos in (0..STORED_DECIMAL_EXPONENT_BITS).rev() {
            self.append_bit((stored_exponent_bits >> bit_pos) & 1);
        }
    }

    pub(super) fn reader(&self) -> TypeBitsReader<'_> {
        TypeBitsReader {
            data: &self.data,
            cur_bit: 0,
            all_zeros: self.all_zeros,
        }
    }
}

/// `TypeBits::Reader`.
pub(super) struct TypeBitsReader<'a> {
    data: &'a [u8],
    cur_bit: u32,
    all_zeros: bool,
}

impl TypeBitsReader<'_> {
    fn read_bit(&mut self) -> Result<u8> {
        if self.all_zeros {
            return Ok(0);
        }
        let byte = (self.cur_bit / 8) as usize;
        let off = self.cur_bit % 8;
        self.cur_bit += 1;
        let b = self
            .data
            .get(byte)
            .ok_or_else(|| Error::format("Invalid size byte(s)."))?;
        Ok(u8::from(b & (1 << off) != 0))
    }

    pub(super) fn read_string_like(&mut self) -> Result<u8> {
        self.read_bit()
    }

    pub(super) fn read_numeric(&mut self) -> Result<u8> {
        let high = self.read_bit()?;
        Ok((high << 1) | self.read_bit()?)
    }

    pub(super) fn read_zero(&mut self) -> Result<u8> {
        let mut res = self.read_numeric()?;
        if res == K_SPECIAL_ZERO_PREFIX {
            for _ in 0..3 {
                res = (res << 1) | self.read_bit()?;
            }
        }
        if res == K_V1_NEGATIVE_DOUBLE_ZERO {
            res = K_NEGATIVE_DOUBLE_ZERO;
        }
        Ok(res)
    }

    pub(super) fn read_decimal_zero(&mut self, zero_type: u8) -> Result<u32> {
        let mut which = u32::from(zero_type - K_DECIMAL_ZERO_0XXX);
        for _ in 0..12 {
            which = (which << 1) | u32::from(self.read_bit()?);
        }
        Ok(which)
    }

    pub(super) fn read_decimal_exponent(&mut self) -> Result<u8> {
        let mut bits = 0u8;
        for _ in 0..STORED_DECIMAL_EXPONENT_BITS {
            bits = (bits << 1) | self.read_bit()?;
        }
        Ok(bits)
    }
}
