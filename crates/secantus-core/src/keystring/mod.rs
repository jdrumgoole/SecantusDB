//! MongoDB's KeyString (`mongo/db/storage/key_string`, version V1), ported.
//!
//! A KeyString is a BSON-ordered, memcmp-comparable encoding of an index key:
//! the bytes of two keys compare as the BSON values compare. This is the
//! encoding mongod 8.2.11 writes into its index tables, so this module is
//! byte-for-byte that algorithm: the same type bytes, the same numeric
//! encodings (int, long, double with its small / large / fractional cases,
//! Decimal128 with tiny / huge values and decimals equal to a double), the same
//! string and object escaping, the same descending inversion, the same
//! discriminator / end bytes, the same `RecordId` suffixes and the same
//! `TypeBits`.
//!
//! The reference is mongo r8.2.11 `key_string.cpp`; the ground truth is the
//! store a real mongod wrote (`tests/fixtures/storage_model/mongod-8.2.11-layout.json`,
//! checked by `tests/keystring_mongod_fixture.rs`).
//!
//! Only version V1 is implemented: it is the only version mongod 8.x creates
//! indexes with (V0 belongs to index versions that predate Decimal128).
//!
//! ```
//! use bson::Bson;
//! use secantus_core::keystring::{Builder, Ordering, Version};
//!
//! let mut b = Builder::new(Version::V1, Ordering::all_ascending());
//! b.append_bson_element(&Bson::Int32(1)).unwrap();
//! b.append_record_id_long(7).unwrap();
//! let (key, type_bits) = b.finish().unwrap();
//! assert_eq!(key, [0x2b, 0x02, 0x04, 0x00, 0x38]);
//! assert!(type_bits.serialize().is_empty()); // an int is the all-zero TypeBits
//! ```

mod dec;
mod decode;
mod encode;
mod typebits;

#[cfg(test)]
mod tests;

pub use decode::{
    decode_record_id_long, decode_record_id_str_at_end, get_key_size, to_bson,
    without_discriminator_at_end, without_record_id_long_at_end, without_record_id_str_at_end,
};
pub use encode::{Builder, StringTransform};
pub use typebits::TypeBits;

use std::fmt;

/// `key_string::Version`. Only V1 exists here (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Version {
    V1,
}

/// `Ordering`: bit `i` set means key field `i` sorts descending, and its bytes
/// are inverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Ordering {
    bits: u32,
}

impl Ordering {
    /// `Ordering::allAscending()`.
    pub const fn all_ascending() -> Ordering {
        Ordering { bits: 0 }
    }

    /// `Ordering::make(keyPattern)`: one direction per key field, negative is
    /// descending. At most 32 fields count, as in mongod.
    pub fn from_directions(directions: &[i32]) -> Ordering {
        let mut bits = 0u32;
        for (i, d) in directions.iter().enumerate().take(32) {
            if *d < 0 {
                bits |= 1 << i;
            }
        }
        Ordering { bits }
    }

    /// The raw bitset (`Ordering::getBits`).
    pub const fn from_bits(bits: u32) -> Ordering {
        Ordering { bits }
    }

    pub const fn bits(&self) -> u32 {
        self.bits
    }

    /// `Ordering::get(i)`: `-1` for descending, `1` otherwise. Fields past the
    /// 32nd are ascending.
    pub const fn get(&self, i: usize) -> i32 {
        if i < 32 && (self.bits >> i) & 1 == 1 {
            -1
        } else {
            1
        }
    }

    pub(crate) const fn inverted(&self, i: usize) -> bool {
        self.get(i) == -1
    }
}

/// `key_string::Discriminator`: what ends a KeyString built for a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Discriminator {
    /// The end byte `0x04`: the form stored in an index.
    Inclusive,
    /// `0x01`: sorts before every key with the same prefix.
    ExclusiveBefore,
    /// `0xfe`: sorts after every key with the same prefix.
    ExclusiveAfter,
}

/// A KeyString encode or decode failure. `Format` is a malformed buffer (the
/// `KeyString format error:` asserts); the rest are encode-side refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A malformed KeyString or TypeBits buffer.
    Format(String),
    /// A value mongod's KeyString cannot encode (a Symbol under a collation).
    CannotBuildIndexKeys(String),
    /// A broken KeyString invariant (mongod would `invariant()` here).
    Invariant(String),
    /// A builder method called out of order.
    BadState(String),
}

impl Error {
    pub(crate) fn format(msg: impl Into<String>) -> Error {
        Error::Format(msg.into())
    }
    pub(crate) fn invariant(msg: impl Into<String>) -> Error {
        Error::Invariant(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Format(m) => write!(f, "KeyString format error: {m}"),
            Error::CannotBuildIndexKeys(m) => write!(f, "{m}"),
            Error::Invariant(m) => write!(f, "KeyString invariant failed: {m}"),
            Error::BadState(m) => write!(f, "KeyString builder state error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The canonical type bytes (`CType`). 0-9 and 246-255 are reserved.
pub mod ctype {
    pub const MIN_KEY: u8 = 10;
    pub const UNDEFINED: u8 = 15;
    pub const NULLISH: u8 = 20;
    pub const NUMERIC: u8 = 30;
    pub const STRING_LIKE: u8 = 60;
    pub const OBJECT: u8 = 70;
    pub const ARRAY: u8 = 80;
    pub const BIN_DATA: u8 = 90;
    pub const OID: u8 = 100;
    pub const BOOL: u8 = 110;
    pub const DATE: u8 = 120;
    pub const TIMESTAMP: u8 = 130;
    pub const REGEX: u8 = 140;
    pub const DB_REF: u8 = 150;
    pub const CODE: u8 = 160;
    pub const CODE_WITH_SCOPE: u8 = 170;
    pub const MAX_KEY: u8 = 240;

    pub const NUMERIC_NAN: u8 = NUMERIC;
    pub const NUMERIC_NEGATIVE_LARGE_MAGNITUDE: u8 = NUMERIC + 1;
    pub const NUMERIC_NEGATIVE_8_BYTE_INT: u8 = NUMERIC + 2;
    pub const NUMERIC_NEGATIVE_1_BYTE_INT: u8 = NUMERIC + 9;
    pub const NUMERIC_NEGATIVE_SMALL_MAGNITUDE: u8 = NUMERIC + 10;
    pub const NUMERIC_ZERO: u8 = NUMERIC + 11;
    pub const NUMERIC_POSITIVE_SMALL_MAGNITUDE: u8 = NUMERIC + 12;
    pub const NUMERIC_POSITIVE_1_BYTE_INT: u8 = NUMERIC + 13;
    pub const NUMERIC_POSITIVE_8_BYTE_INT: u8 = NUMERIC + 20;
    pub const NUMERIC_POSITIVE_LARGE_MAGNITUDE: u8 = NUMERIC + 21;

    pub const BOOL_FALSE: u8 = BOOL;
    pub const BOOL_TRUE: u8 = BOOL + 1;

    /// Bytes of an integer encoding for the int ctypes.
    pub fn num_bytes_for_int(c: u8) -> usize {
        if c >= NUMERIC_POSITIVE_1_BYTE_INT {
            (c - NUMERIC_POSITIVE_1_BYTE_INT) as usize + 1
        } else {
            (NUMERIC_NEGATIVE_1_BYTE_INT - c) as usize + 1
        }
    }
}

/// The end byte.
pub(crate) const K_END: u8 = 0x4;
/// Discriminators: they overlay CType / end bytes, so they sort outside them,
/// and they are never 0 or 255 (the string NUL escape).
pub(crate) const K_LESS: u8 = 1;
pub(crate) const K_GREATER: u8 = 254;

/// The `DecimalContinuationMarker`.
pub(crate) const DCM_EQUAL_TO_DOUBLE: u64 = 0x0;
pub(crate) const DCM_LESS_THAN_15: u64 = 0x1;
pub(crate) const DCM_EQUAL_TO_DOUBLE_15: u64 = 0x2;
pub(crate) const DCM_LARGER_THAN_15: u64 = 0x3;

/// A string RecordId's size is encoded in 7-bit groups over at most four
/// bytes (sizes below 2**28).
pub(crate) const RECORD_ID_STR_ENCODED_SIZE_MAX_BYTES: usize = 4;
