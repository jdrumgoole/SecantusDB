//! A record's identity in its collection, as mongod's `RecordId` is: a
//! 64-bit counter for an ordinary collection, or the bytes of the KeyString
//! of the document's `_id` for a clustered one (a time-series collection's
//! buckets, `config.system.preimages`, a collection made with
//! `clusteredIndex`). A table's rows are in RecordId order either way.

use std::fmt;

/// See the module comment. `Long` orders before `Bytes`; a collection holds
/// only one kind, so the order between kinds is never read.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecordId {
    /// An ordinary collection's RecordId, minted by a per-collection counter.
    Long(i64),
    /// A clustered collection's RecordId: the KeyString of `_id`, with no
    /// TypeBits and no end byte.
    Bytes(Vec<u8>),
}

impl RecordId {
    /// The counter value, for a RecordId of an ordinary collection.
    pub fn as_long(&self) -> Option<i64> {
        match self {
            RecordId::Long(n) => Some(*n),
            RecordId::Bytes(_) => None,
        }
    }

    /// The key bytes, for a RecordId of a clustered collection.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            RecordId::Long(_) => None,
            RecordId::Bytes(b) => Some(b),
        }
    }

    /// How a `$recordId` / `showRecordId` shows it: a NumberLong, or the
    /// key bytes as BinData (measured, mongod 8.2.11).
    pub fn to_bson(&self) -> bson::Bson {
        match self {
            RecordId::Long(n) => bson::Bson::Int64(*n),
            RecordId::Bytes(b) => bson::Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::Generic,
                bytes: b.clone(),
            }),
        }
    }
}

impl From<i64> for RecordId {
    fn from(n: i64) -> Self {
        RecordId::Long(n)
    }
}

impl PartialEq<i64> for RecordId {
    fn eq(&self, other: &i64) -> bool {
        matches!(self, RecordId::Long(n) if n == other)
    }
}

impl fmt::Debug for RecordId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordId::Long(n) => write!(f, "RecordId({n})"),
            RecordId::Bytes(b) => {
                write!(f, "RecordId(")?;
                for x in b {
                    write!(f, "{x:02x}")?;
                }
                write!(f, ")")
            }
        }
    }
}
