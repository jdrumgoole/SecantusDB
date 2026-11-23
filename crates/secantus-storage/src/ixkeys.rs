//! A secondary index's entries as mongod 8.2.11 writes them: one WiredTiger
//! table per index (`index-<UUID>`, `key_format=u,value_format=u`), each
//! entry keyed by the KeyString of the index key's values with the RecordId
//! appended (`Builder::append_record_id_long`), and holding the TypeBits as
//! its value (empty when there are none).
//!
//! Which values a document contributes (multikey expansion, sparse, partial
//! filters) is decided elsewhere (`index_key_values_c`); this module only
//! encodes them: the directions of the key pattern become the KeyString
//! [`Ordering`], and a string passes through the index's collation sort key,
//! the transform `StringTransformFn` is in mongod.

use bson::{Bson, Document};
use secantus_core::collation::{self, Collation};
use secantus_core::keystring::{self, Builder, Discriminator, Ordering, TypeBits, Version};
use secantus_core::order::BinDataOrder;
use secantus_core::recordid::RecordId;

use crate::{Result, StorageError};

/// An index entry, ready for its table: the key (KeyString and RecordId) and
/// the value (TypeBits).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EntryKey {
    pub key: Vec<u8>,
    pub type_bits: Vec<u8>,
}

/// An `_id_` entry as mongod 8.2.11 writes it: the KeyString of `_id` alone
/// as the key, and as the value the RecordId (`append_record_id_long`'s
/// bytes) followed by the TypeBits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IdEntry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// How an index's values become KeyString bytes.
#[derive(Clone)]
pub(crate) struct KeyShape {
    dirs: Vec<i32>,
    ordering: Ordering,
    collation: Option<Collation>,
    /// PostgreSQL's order for `bytea` (the bytes, a shorter one first, then
    /// the subtype) where mongod orders BinData by length first: a
    /// BinData value is keyed as the text that orders so.
    bytes_order: bool,
    /// The collection is clustered: an entry ends in the RecordId as the
    /// bytes of the KeyString of `_id` (`appendRecordId` of a string), not
    /// a long.
    clustered: bool,
    /// Key slots whose strings are never put through the collation (a
    /// wildcard index's `$_path`).
    uncollated: Vec<usize>,
}

fn unsupported(_: keystring::Error) -> StorageError {
    StorageError::UnsupportedValue
}

impl KeyShape {
    pub fn new(key_spec: &Document, collation: Option<Collation>, order: BinDataOrder) -> Self {
        let dirs: Vec<i32> = key_spec
            .values()
            .map(|d| match d {
                Bson::Int32(n) => *n,
                Bson::Int64(n) => *n as i32,
                Bson::Double(f) => *f as i32,
                _ => 1,
            })
            .collect();
        KeyShape {
            ordering: Ordering::from_directions(&dirs),
            dirs,
            collation,
            bytes_order: order == BinDataOrder::Bytes,
            clustered: false,
            uncollated: Vec::new(),
        }
    }

    /// This shape with the strings of key slot `at` kept out of the
    /// collation too.
    pub fn uncollated_at(mut self, at: usize) -> Self {
        self.uncollated.push(at);
        self
    }

    /// This shape for a wildcard index, whose `$**` field at `at` is two
    /// key slots: `$_path`, always ascending and never collated, then the
    /// value under it in the field's direction.
    pub fn wildcard_at(mut self, at: usize) -> Self {
        self.dirs.insert(at, 1);
        self.ordering = Ordering::from_directions(&self.dirs);
        self.uncollated.push(at);
        self
    }

    /// This shape for the entries of a clustered collection's index.
    pub fn clustered(mut self, clustered: bool) -> Self {
        self.clustered = clustered;
        self
    }

    /// Whether BinData values are keyed as `bytea`-ordered text (see
    /// `bytes_order`): such an index holds them under the string type byte.
    pub fn bytes_order(&self) -> bool {
        self.bytes_order
    }

    /// Each key field's direction, `1` or `-1`.
    pub fn dirs(&self) -> &[i32] {
        &self.dirs
    }

    fn builder(&self, discriminator: Discriminator) -> Builder {
        Builder::with_discriminator(Version::V1, self.ordering, discriminator)
    }

    fn append(&self, b: &mut Builder, v: &Bson) -> Result<()> {
        self.append_at(b, usize::MAX, v)
    }

    fn append_at(&self, b: &mut Builder, slot: usize, v: &Bson) -> Result<()> {
        let mapped;
        let v = if self.bytes_order && has_binary(v) {
            mapped = bytes_ordered(v);
            &mapped
        } else {
            v
        };
        match self
            .collation
            .as_ref()
            .filter(|_| !self.uncollated.contains(&slot))
        {
            Some(c) => {
                let f = |s: &str| collation::sort_key(s, c);
                b.append_bson_element_with(v, Some(&f)).map_err(unsupported)
            }
            None => b.append_bson_element(v).map_err(unsupported),
        }
    }

    /// The entry for `values` held by `rid`.
    pub fn entry(&self, values: &[Bson], rid: &RecordId) -> Result<EntryKey> {
        let mut b = self.builder(Discriminator::Inclusive);
        for (slot, v) in values.iter().enumerate() {
            self.append_at(&mut b, slot, v)?;
        }
        match rid {
            RecordId::Long(n) => b.append_record_id_long(*n),
            RecordId::Bytes(k) => b.append_record_id_bytes(k),
        }
        .map_err(unsupported)?;
        let (key, tb) = b.finish().map_err(unsupported)?;
        Ok(EntryKey {
            key,
            type_bits: tb.serialize(),
        })
    }

    /// The `_id_` entry of document `id` held by `rid` (see [`IdEntry`]).
    pub fn id_entry(&self, id: &Bson, rid: i64) -> Result<IdEntry> {
        let e = self.entry(std::slice::from_ref(id), &RecordId::Long(rid))?;
        let (key, _) = keystring::without_record_id_long_at_end(&e.key).map_err(unsupported)?;
        let mut value = e.key[key.len()..].to_vec();
        value.extend_from_slice(&e.type_bits);
        Ok(IdEntry {
            key: key.to_vec(),
            value,
        })
    }

    /// The `_id_` key of document `id`: what a lookup of it seeks.
    pub fn id_key(&self, id: &Bson) -> Result<Vec<u8>> {
        self.exact(std::slice::from_ref(id))
    }

    /// An `_id_` entry's `_id` and RecordId.
    pub fn decode_id(&self, key: &[u8], value: &[u8]) -> Result<(Bson, i64)> {
        let bad = || StorageError::Internal("an _id index entry is not a KeyString".into());
        let (rid, used) = keystring::decode_record_id_long(value).map_err(|_| bad())?;
        let tb = TypeBits::from_buffer(Version::V1, &value[used..]).map_err(|_| bad())?;
        let mut values = keystring::to_bson(key, self.ordering, &tb).map_err(|_| bad())?;
        if values.len() != 1 {
            return Err(bad());
        }
        Ok((values.remove(0), rid))
    }

    /// The bytes every entry of `values` starts with, end byte included and
    /// no RecordId: a seek for the first of them, and the prefix the rest
    /// share.
    pub fn exact(&self, values: &[Bson]) -> Result<Vec<u8>> {
        let mut b = self.builder(Discriminator::Inclusive);
        for v in values {
            self.append(&mut b, v)?;
        }
        let (key, _) = b.finish().map_err(unsupported)?;
        Ok(key)
    }

    /// The values alone, no end byte: what every entry of those values, and
    /// of any longer key that starts with them, starts with.
    pub fn prefix(&self, values: &[Bson]) -> Result<Vec<u8>> {
        let mut b = self.builder(Discriminator::Inclusive);
        for v in values {
            self.append(&mut b, v)?;
        }
        Ok(b.buffer().to_vec())
    }

    /// A seek key for `values`: before every entry that starts with them
    /// (`makeKeyStringFromBSONKeyForSeek` with `kExclusiveBefore`), or after
    /// every one (`kExclusiveAfter`).
    pub fn seek(&self, values: &[Bson], after: bool) -> Result<Vec<u8>> {
        let mut b = self.builder(if after {
            Discriminator::ExclusiveAfter
        } else {
            Discriminator::ExclusiveBefore
        });
        for v in values {
            self.append(&mut b, v)?;
        }
        let (key, _) = b.finish().map_err(unsupported)?;
        Ok(key)
    }

    /// The bytes of `v` as the key field at `pos` holds it (inverted when
    /// that field is descending), with no end byte.
    pub fn field_bytes(&self, pos: usize, v: &Bson) -> Result<Vec<u8>> {
        let mut b = self.builder(Discriminator::Inclusive);
        for _ in 0..pos {
            b.append_bson_element(&Bson::Null).map_err(unsupported)?;
        }
        let from = b.buffer().len();
        self.append(&mut b, v)?;
        Ok(b.buffer()[from..].to_vec())
    }

    /// An entry's values and RecordId.
    pub fn decode(&self, key: &[u8], type_bits: &[u8]) -> Result<(Vec<Bson>, RecordId)> {
        let bad = || StorageError::Internal("an index entry is not a KeyString".into());
        let tb = TypeBits::from_buffer(Version::V1, type_bits).map_err(|_| bad())?;
        let (without_rid, rid) = if self.clustered {
            let (p, r) = keystring::without_record_id_str_at_end(key).map_err(|_| bad())?;
            (p, RecordId::Bytes(r.to_vec()))
        } else {
            let (p, r) = keystring::without_record_id_long_at_end(key).map_err(|_| bad())?;
            (p, RecordId::Long(r))
        };
        let values = keystring::to_bson(without_rid, self.ordering, &tb).map_err(|_| bad())?;
        Ok((values, rid))
    }
}

/// The RecordId an `_id_` entry's value starts with.
pub(crate) fn id_entry_rid(value: &[u8]) -> Option<i64> {
    keystring::decode_record_id_long(value)
        .ok()
        .map(|(rid, _)| rid)
}

/// The RecordId at the end of an entry's key.
pub(crate) fn entry_rid(key: &[u8], clustered: bool) -> Option<RecordId> {
    if clustered {
        keystring::without_record_id_str_at_end(key)
            .ok()
            .map(|(_, rid)| RecordId::Bytes(rid.to_vec()))
    } else {
        keystring::without_record_id_long_at_end(key)
            .ok()
            .map(|(_, rid)| RecordId::Long(rid))
    }
}

/// An entry's key without its RecordId: the KeyString of its values and the
/// end byte.
pub(crate) fn entry_prefix(key: &[u8], clustered: bool) -> Option<&[u8]> {
    if clustered {
        keystring::without_record_id_str_at_end(key)
            .ok()
            .map(|(p, _)| p)
    } else {
        keystring::without_record_id_long_at_end(key)
            .ok()
            .map(|(p, _)| p)
    }
}

fn has_binary(v: &Bson) -> bool {
    match v {
        Bson::Binary(_) => true,
        Bson::Array(a) => a.iter().any(has_binary),
        Bson::Document(d) => d.values().any(has_binary),
        _ => false,
    }
}

/// `v` with each BinData value replaced by text that orders as PostgreSQL
/// orders `bytea`: the bytes in hex, then a separator that sorts before
/// every hex digit (so a shorter value comes first), then the subtype.
fn bytes_ordered(v: &Bson) -> Bson {
    match v {
        Bson::Binary(b) => {
            let mut s = String::with_capacity(b.bytes.len() * 2 + 4);
            for byte in &b.bytes {
                s.push_str(&format!("{byte:02x}"));
            }
            s.push('\u{1}');
            s.push_str(&format!("{:02x}", u8::from(b.subtype)));
            Bson::String(s)
        }
        Bson::Array(a) => Bson::Array(a.iter().map(bytes_ordered).collect()),
        Bson::Document(d) => Bson::Document(
            d.iter()
                .map(|(k, x)| (k.clone(), bytes_ordered(x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn an_entry_is_the_keystring_then_the_record_id() {
        let shape = KeyShape::new(&doc! {"a": 1, "b": -1}, None, BinDataOrder::Mongod);
        let e = shape
            .entry(
                &[Bson::Int32(1), Bson::String("x".into())],
                &RecordId::Long(7),
            )
            .unwrap();
        let mut b = Builder::new(Version::V1, Ordering::from_directions(&[1, -1]));
        b.append_bson_element(&Bson::Int32(1)).unwrap();
        b.append_bson_element(&Bson::String("x".into())).unwrap();
        b.append_record_id_long(7).unwrap();
        let (key, tb) = b.finish().unwrap();
        assert_eq!(e.key, key);
        assert_eq!(e.type_bits, tb.serialize());
        assert_eq!(entry_rid(&e.key, false), Some(RecordId::Long(7)));
        let (values, rid) = shape.decode(&e.key, &e.type_bits).unwrap();
        assert_eq!(values, vec![Bson::Int32(1), Bson::String("x".into())]);
        assert_eq!(rid, RecordId::Long(7));
    }

    #[test]
    fn an_id_entry_holds_the_record_id_in_its_value() {
        let shape = KeyShape::new(&doc! {"_id": 1}, None, BinDataOrder::Mongod);
        let id = Bson::Double(0.5);
        let e = shape.id_entry(&id, 7).unwrap();
        assert_eq!(e.key, shape.id_key(&id).unwrap());
        assert_eq!(id_entry_rid(&e.value), Some(7));
        let (back, rid) = shape.decode_id(&e.key, &e.value).unwrap();
        assert_eq!((back, rid), (id, 7));
    }

    #[test]
    fn exact_is_the_prefix_of_every_entry_of_those_values() {
        let shape = KeyShape::new(&doc! {"a": 1}, None, BinDataOrder::Mongod);
        let exact = shape.exact(&[Bson::Int32(5)]).unwrap();
        for rid in [1, 300, i64::MAX] {
            let e = shape
                .entry(&[Bson::Int32(5)], &RecordId::Long(rid))
                .unwrap();
            assert!(e.key.starts_with(&exact));
        }
        let other = shape.entry(&[Bson::Int32(6)], &RecordId::Long(1)).unwrap();
        assert!(!other.key.starts_with(&exact));
        assert!(shape.seek(&[Bson::Int32(5)], false).unwrap() < exact);
        assert!(shape.seek(&[Bson::Int32(5)], true).unwrap() > exact);
    }

    #[test]
    fn a_postgresql_index_orders_binary_values_as_bytea() {
        use bson::spec::BinarySubtype::Generic;
        let bin = |b: &[u8]| {
            Bson::Binary(bson::Binary {
                subtype: Generic,
                bytes: b.to_vec(),
            })
        };
        let key = |shape: &KeyShape, v: &Bson| {
            shape
                .entry(std::slice::from_ref(v), &RecordId::Long(1))
                .unwrap()
                .key
        };
        let pg = KeyShape::new(&doc! {"b": 1}, None, BinDataOrder::Bytes);
        let mongod = KeyShape::new(&doc! {"b": 1}, None, BinDataOrder::Mongod);
        // PostgreSQL: the bytes, a shorter value first when it is a prefix.
        assert!(key(&pg, &bin(&[1])) < key(&pg, &bin(&[1, 0])));
        assert!(key(&pg, &bin(&[1, 0xff])) < key(&pg, &bin(&[2])));
        // mongod: the length first.
        assert!(key(&mongod, &bin(&[2])) < key(&mongod, &bin(&[1, 0])));
    }

    #[test]
    fn a_collated_string_is_keyed_by_its_sort_key() {
        let c = collation::parse(&doc! {"locale": "en", "strength": 2}).unwrap();
        let shape = KeyShape::new(&doc! {"s": 1}, Some(c), BinDataOrder::Mongod);
        let a = shape
            .entry(&[Bson::String("Abc".into())], &RecordId::Long(1))
            .unwrap();
        let b = shape
            .entry(&[Bson::String("aBC".into())], &RecordId::Long(1))
            .unwrap();
        assert_eq!(a.key, b.key);
    }

    #[test]
    fn a_clustered_entry_ends_in_the_key_bytes_of_its_record_id() {
        // The RecordId of a clustered collection is the KeyString of `_id`;
        // an entry appends it as `appendRecordId` does a string RecordId:
        // the bytes, then their length (measured on mongod 8.2.11: `x: 1`
        // of `_id: 1` is `2b02 04 2b02 02`).
        let shape = KeyShape::new(&doc! {"x": 1}, None, BinDataOrder::Mongod).clustered(true);
        let rid = RecordId::Bytes(vec![0x2b, 0x02]);
        let e = shape.entry(&[Bson::Int32(1)], &rid).unwrap();
        assert_eq!(e.key, vec![0x2b, 0x02, 0x04, 0x2b, 0x02, 0x02]);
        assert_eq!(entry_rid(&e.key, true), Some(rid.clone()));
        assert_eq!(entry_prefix(&e.key, true), Some(&[0x2b, 0x02, 0x04][..]));
        let (values, back) = shape.decode(&e.key, &e.type_bits).unwrap();
        assert_eq!(values, vec![Bson::Int32(1)]);
        assert_eq!(back, rid);
    }
}
