//! A pipeline document's storage as mongod 8.2's classic engine keeps it
//! (`DocumentStorage`, `document_internal.h` / `document.cpp`, 8.2.1), for
//! the sizes its memory accounting reads off it (RM-408).
//!
//! A document read from the collection is its BSON and nothing else. A
//! stage that reads a field of it (`getField`) copies that field into a
//! cache beside the BSON (`constructInCache`), and a stage that changes it
//! (`MutableDocument`) appends the fields it writes to that cache. The cache
//! is one buffer: each field a `ValueElement` (a 16-byte `Value`, two 4-byte
//! words and a kind byte, packed, then its name and a NUL, to 8 bytes) and a
//! hash table of 4-byte buckets at its end; it is allocated in powers of two
//! from 128 bytes, the table doubling from 8 buckets while more than half
//! full. A field's `Value` points at what it holds -- a long string, an
//! array's vector, a sub-document's storage -- and the size counts that as
//! well. So a document's size is:
//!
//! | reader | counts |
//! | --- | --- |
//! | `getApproximateSize` (a window's cache, a `$group`'s output) | `sizeof(Document)` + the storage below |
//! | `currentApproximateSize` | `sizeof(DocumentStorage)` + the buffer + the BSON + what the cached values hold + metadata |
//! | `memUsageForSorter` (a pipeline `$sort`) | the storage, less the BSON bytes of the fields read into the cache |
//!
//! A document the classic engine builds from nothing (an expression
//! object's, a `$group`'s) has an empty BSON object (5 bytes) under its
//! cache. A `$sort` that `$setWindowFields` runs on attaches each document's
//! sort key as metadata, which the document then carries.

use bson::{Bson, Document};

use crate::memlimit;

/// `sizeof(Document)`: the pointer to its storage.
const DOCUMENT: usize = 8;
/// `sizeof(DocumentStorage)` with no metadata (what a read document counts
/// beyond its BSON in a sort: 96, measured 8.2.11).
const STORAGE: usize = memlimit::DOCUMENT_OVERHEAD - DOCUMENT;
/// `sizeof(ValueElement)`, packed: `Value`, `Position`, `nameLen`, `kind`
/// and the first byte of the name (its NUL when the name is empty).
const ELEMENT: usize = 16 + 4 + 4 + 1 + 1;
/// `sizeof(Position)`, one hash bucket.
const BUCKET: usize = 4;
/// `HASH_TAB_INIT_SIZE`.
const INIT_BUCKETS: usize = 8;
/// What an empty BSON object takes.
const EMPTY_BSON: usize = 5;

fn align(n: usize) -> usize {
    n.div_ceil(8) * 8
}

/// One document's storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    /// The backing BSON's size (`_bson.objsize()`).
    bson: usize,
    /// The bytes of that BSON whose fields were read into the cache
    /// (`_numBytesFromBSONInCache`).
    from_bson: usize,
    /// The buffer's whole size (`allocatedBytes()`), 0 before it exists.
    allocated: usize,
    /// Where the fields must end (`_cacheEnd - _cache`).
    cache_end: usize,
    /// What the fields take (`_usedBytes`).
    used: usize,
    /// `hashTabBuckets()`.
    buckets: usize,
    /// The cached fields in buffer order, each with what its value holds
    /// beyond its own `Value`.
    fields: Vec<(String, usize)>,
    /// The metadata's size, when it carries any.
    metadata: usize,
}

impl Stored {
    /// A document read as `bson` bytes of BSON.
    pub fn read(bson: usize) -> Self {
        Stored {
            bson,
            from_bson: 0,
            allocated: 0,
            cache_end: 0,
            used: 0,
            buckets: 1,
            fields: Vec::new(),
            metadata: 0,
        }
    }

    /// A document built field by field (`MutableDocument()`).
    pub fn built() -> Self {
        Stored::read(EMPTY_BSON)
    }

    /// A document built with room for `fields` fields first
    /// (`MutableDocument(n)`, `reserveFields`).
    pub fn reserved(fields: usize) -> Self {
        let mut s = Stored::built();
        let mut buckets = INIT_BUCKETS;
        while buckets < fields {
            buckets *= 2;
        }
        s.buckets = buckets;
        s.cache_end = (fields + 1) * align(ELEMENT);
        s.allocated = s.cache_end + BUCKET * s.buckets;
        s
    }

    /// `DocumentStorage::alloc`.
    fn alloc(&mut self, new_size: usize) {
        while self.fields.len() * 2 > self.buckets || self.buckets < INIT_BUCKETS {
            self.buckets *= 2;
        }
        let hash = BUCKET * self.buckets;
        let mut capacity = 128;
        while capacity < new_size + hash {
            capacity *= 2;
        }
        self.allocated = capacity;
        self.cache_end = capacity - hash;
    }

    /// `DocumentStorage::appendField`.
    fn append(&mut self, name: &str, held: usize) {
        let new_used = align(self.used + ELEMENT + name.len());
        if new_used > self.cache_end {
            self.alloc(new_used);
        }
        self.used = new_used;
        self.fields.push((name.to_string(), held));
    }

    fn cached(&mut self, name: &str) -> Option<&mut usize> {
        self.fields
            .iter_mut()
            .find(|(n, _)| n == name)
            .map(|(_, held)| held)
    }

    /// A read of the top-level field `name` of `doc`, the document this
    /// storage holds (`findField`): from the BSON into the cache the first
    /// time. A field the document does not have is not cached.
    pub fn read_field(&mut self, doc: &Document, name: &str) {
        if self.cached(name).is_some() {
            return;
        }
        if let Some(v) = doc.get(name) {
            self.from_bson += memlimit::element_len(name, v);
            self.append(name, held(v));
        }
    }

    /// A write of the top-level field `name` (`MutableDocument::setField`,
    /// `addField`, `remove`): the cached field when there is one, a new one
    /// in the cache otherwise. `held`: what its new value holds beyond its
    /// `Value` (0 for a removed field).
    pub fn set_field(&mut self, name: &str, held: usize) {
        match self.cached(name) {
            Some(h) => *h = held,
            None => self.append(name, held),
        }
    }

    /// `MutableDocument::addField`: a new field, never looked up.
    pub fn add_field(&mut self, name: &str, held: usize) {
        self.append(name, held);
    }

    /// The document carries `bytes` of metadata from now on.
    pub fn set_metadata(&mut self, bytes: usize) {
        self.metadata = bytes;
    }

    /// `DocumentStorage::currentApproximateSize`.
    pub fn current(&self) -> usize {
        STORAGE
            + self.allocated
            + self.metadata
            + self.bson
            + self.fields.iter().map(|(_, h)| h).sum::<usize>()
    }

    /// `Document::getApproximateSize`.
    pub fn approximate(&self) -> usize {
        DOCUMENT + self.current()
    }

    /// `Document::memUsageForSorter`.
    pub fn for_sorter(&self) -> usize {
        self.current() - self.from_bson
    }
}

/// What a value read from BSON holds beyond its own `Value`
/// (`Value::getApproximateSize` less `sizeof(Value)`).
pub fn held(v: &Bson) -> usize {
    memlimit::classic_size(v) - memlimit::VALUE
}

/// Whether `expr` is an expression object (`{a: <expr>, ...}`) rather than
/// an operator.
fn is_object_expression(expr: &Document) -> bool {
    expr.keys().next().is_some_and(|k| !k.starts_with('$'))
}

/// What `v`, the value `expr` evaluated to, holds beyond its own `Value`.
/// An expression object builds its document field by field
/// (`evaluate(ExpressionObject)`: `MutableDocument` with no reserve, every
/// field added, a missing value too); an array literal builds a vector of
/// its elements. Anything else is sized as a value read from BSON.
pub fn produced_held(expr: &Bson, v: &Bson) -> usize {
    match (expr, v) {
        (Bson::Document(e), Bson::Document(d)) if is_object_expression(e) => {
            let mut s = Stored::built();
            for (k, sub) in e {
                let h = d.get(k).map_or(0, |val| produced_held(sub, val));
                s.add_field(k, h);
            }
            s.approximate()
        }
        (Bson::Array(es), Bson::Array(vs)) if es.len() == vs.len() => {
            memlimit::RC_VECTOR
                + es.iter()
                    .zip(vs)
                    .map(|(e, v)| memlimit::VALUE + produced_held(e, v))
                    .sum::<usize>()
        }
        _ => held(v),
    }
}

/// `Value::getApproximateSize` of `v`, the value `expr` evaluated to.
pub fn produced_size(expr: &Bson, v: &Bson) -> usize {
    memlimit::VALUE + produced_held(expr, v)
}

/// The top-level fields `expr` reads, in the order it reads them: the
/// first part of each `$path` (variables, `$$...`, read none).
pub fn fields_read(expr: &Bson, out: &mut Vec<String>) {
    match expr {
        Bson::String(s) if s.starts_with('$') && !s.starts_with("$$") => {
            let top = s[1..].split('.').next().unwrap_or_default();
            if !top.is_empty() && !out.iter().any(|f| f == top) {
                out.push(top.to_string());
            }
        }
        Bson::Document(d) => {
            if d.contains_key("$literal") {
                return;
            }
            for v in d.values() {
                fields_read(v, out);
            }
        }
        Bson::Array(a) => {
            for v in a {
                fields_read(v, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    // The documents the 8.2.11 probes read (2026-11-10): `{_id, k, s, n}`
    // with a 1000-byte `s`, 1036 bytes of BSON.
    fn probe_doc() -> Document {
        doc! { "_id": 1, "k": 1, "s": "x".repeat(1000), "n": 1 }
    }

    #[test]
    fn a_read_document_is_its_bson_and_storage() {
        let d = probe_doc();
        let s = Stored::read(memlimit::bson_len(&d));
        assert_eq!(s.approximate(), memlimit::document_size(&d));
        assert_eq!(s.for_sorter(), 1036 + 96);
    }

    #[test]
    fn an_added_field_takes_a_buffer_and_a_read_one_is_not_counted_twice() {
        // `$addFields` then a `$sort` on mongod 8.2.11: what a document
        // counts beyond its BSON and storage, by the name added and the
        // field its expression reads.
        let d = probe_doc();
        let sorted = |name: &str, read: Option<&str>| {
            let mut s = Stored::read(memlimit::bson_len(&d));
            if let Some(f) = read {
                s.read_field(&d, f);
            }
            let h = read.map_or(0, |f| held(d.get(f).unwrap()));
            s.set_field(name, h);
            s.for_sorter() - 1036 - 96
        };
        for (len, constant, from_n, from_s) in [
            (1, 128, 121, 1168),
            (34, 128, 121, 1168),
            (40, 128, 249, 1296),
            (60, 128, 249, 1296),
            (80, 256, 249, 1296),
        ] {
            let name = "q".repeat(len);
            assert_eq!(sorted(&name, None), constant, "{len}");
            assert_eq!(sorted(&name, Some("n")), from_n, "{len}");
            assert_eq!(sorted(&name, Some("s")), from_s, "{len}");
        }
    }

    #[test]
    fn an_unwound_document_holds_its_cached_field() {
        // A window's cache after `$unwind: "$s"` (a string: the document
        // passes through with `s` read into its cache): 2292 a document on
        // mongod 8.2.1.
        let d = probe_doc();
        let mut s = Stored::read(memlimit::bson_len(&d));
        s.read_field(&d, "s");
        assert_eq!(s.approximate(), 2292);
    }

    #[test]
    fn an_expression_object_builds_its_document() {
        // `{x: "$n"}` pushed: 253 an element on mongod 8.2.1; a compound
        // `_id` `{a: "$k", b: "$k"}` 114 more than the same document read.
        let one = produced_size(
            &Bson::Document(doc! {"x": "$n"}),
            &Bson::Document(doc! {"x": 1}),
        );
        assert_eq!(one, 253);
        let id = doc! {"a": 1, "b": 1};
        let two = produced_size(
            &Bson::Document(doc! {"a": "$k", "b": "$k"}),
            &Bson::Document(id.clone()),
        );
        assert_eq!(two - memlimit::classic_size(&Bson::Document(id)), 114);
    }

    #[test]
    fn reserved_fields_match_a_group_document() {
        let mut s = Stored::reserved(2);
        s.add_field("_id", 0);
        s.add_field("n", 0);
        assert_eq!(s.approximate(), 237);
    }
}
