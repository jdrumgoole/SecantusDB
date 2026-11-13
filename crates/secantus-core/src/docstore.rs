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
//! A sub-document is a storage of its own, which its field's `Value` points
//! at: reading a field inside it (`$e.y`) caches that field in the
//! sub-document's storage, and writing one (`e.z`) gives the field a copy of
//! that storage with the field set (`MutableDocument::clone`), so the
//! sub-document's cache counts in the size of the field that holds it
//! (RM-515). Only the top-level BSON a cache read from leaves a sorter's
//! count: a sub-document's does not. A sub-document's storage is shared, as
//! its `Value`s are: by the copy a stage makes of its document, and by a
//! field set to it (`q: "$e"`), so a later read inside it grows each
//! field that holds it.
//!
//! A document the classic engine builds from nothing (an expression
//! object's, a `$group`'s) has an empty BSON object (5 bytes) under its
//! cache. A `$sort` that `$setWindowFields` runs on attaches each document's
//! sort key as metadata, which the document then carries.

use std::cell::RefCell;
use std::rc::Rc;

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

/// What a cached field's value holds beyond its own `Value`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Held {
    /// A size, for a value whose storage this model does not follow.
    Bytes(usize),
    /// A sub-document's storage, which the field's `Value` points at, and
    /// any other `Value` set from it.
    Document(Rc<RefCell<Stored>>),
}

impl Held {
    fn size(&self) -> usize {
        match self {
            Held::Bytes(n) => *n,
            Held::Document(s) => s.borrow().approximate(),
        }
    }

    /// What a value read from BSON holds: a sub-document its own storage,
    /// as read.
    fn read(v: &Bson) -> Self {
        match v {
            Bson::Document(d) => {
                Held::Document(Rc::new(RefCell::new(Stored::read(memlimit::bson_len(d)))))
            }
            _ => Held::Bytes(held(v)),
        }
    }
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
    fields: Vec<(String, Held)>,
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
    fn append(&mut self, name: &str, held: Held) {
        let new_used = align(self.used + ELEMENT + name.len());
        if new_used > self.cache_end {
            self.alloc(new_used);
        }
        self.used = new_used;
        self.fields.push((name.to_string(), held));
    }

    fn cached(&mut self, name: &str) -> Option<&mut Held> {
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
            self.append(name, Held::read(v));
        }
    }

    /// A read of the dotted `path` of `doc`, the document this storage
    /// holds (`getNestedField`, `ExpressionFieldPath::evaluatePath`): each
    /// field on it from the BSON into the cache of the document that holds
    /// it, the first time. A read stops at a value that is not a document
    /// read from BSON (an array's elements are not followed).
    pub fn read_path(&mut self, doc: &Document, path: &str) {
        let (top, rest) = match path.split_once('.') {
            Some((top, rest)) => (top, Some(rest)),
            None => (path, None),
        };
        self.read_field(doc, top);
        if let Some(rest) = rest {
            self.read_nested(doc, top, rest);
        }
    }

    /// The part `rest` of a path below the cached field `top` of `doc`, read
    /// into that field's storage.
    pub fn read_nested(&mut self, doc: &Document, top: &str, rest: &str) {
        if let (Some(Held::Document(sub)), Some(Bson::Document(d))) =
            (self.cached(top), doc.get(top))
        {
            sub.borrow_mut().read_path(d, rest);
        }
    }

    /// The storage of the sub-document at the dotted `path`, when the cache
    /// holds one there: what an expression reading that path returns.
    fn shared(&self, path: &str) -> Option<Rc<RefCell<Stored>>> {
        let (top, rest) = match path.split_once('.') {
            Some((top, rest)) => (top, Some(rest)),
            None => (path, None),
        };
        let Held::Document(sub) = self.fields.iter().find(|(n, _)| n == top).map(|(_, h)| h)?
        else {
            return None;
        };
        match rest {
            None => Some(Rc::clone(sub)),
            Some(rest) => sub.borrow().shared(rest),
        }
    }

    /// `name` set to the value at the dotted `path` of this document, read
    /// into its cache (`{name: "$path"}`): a sub-document's storage is then
    /// shared by the two fields. False when the cache holds no sub-document
    /// at `path`, and nothing is set.
    pub fn set_shared(&mut self, name: &str, path: &str) -> bool {
        match self.shared(path) {
            Some(sub) => {
                self.set_held(name, Held::Document(sub));
                true
            }
            None => false,
        }
    }

    /// Whether the field `name` is in the cache.
    pub fn has_cached(&self, name: &str) -> bool {
        self.fields.iter().any(|(n, _)| n == name)
    }

    /// A write of the dotted `path` (`ProjectionNode::applyExpressions` for
    /// an `$addFields` / `$set` path into a sub-document): the field above
    /// it is read from `input` (the document this storage holds, `None` for
    /// one built here) through the output's `peek`, then set to a copy of
    /// its sub-document with the path below written -- or, where it held no
    /// document, to one built with it. `out`: the document the stage
    /// returned at this level; `leaf`: what the value written holds.
    pub fn set_path(&mut self, input: Option<&Document>, out: &Document, path: &str, leaf: usize) {
        let Some((top, rest)) = path.split_once('.') else {
            self.set_field(path, leaf);
            return;
        };
        if let Some(input) = input {
            self.read_field(input, top);
        }
        let Some(Bson::Document(out_sub)) = out.get(top) else {
            // An array below (each element written), or nothing at all: sized
            // as the value the stage returned.
            self.set_field(top, out.get(top).map_or(0, held));
            return;
        };
        let in_sub = input.and_then(|d| d.get(top));
        let sub = match (self.cached(top), in_sub) {
            (Some(Held::Document(sub)), in_sub) => {
                let mut sub = sub.borrow().clone();
                let in_sub = match in_sub {
                    Some(Bson::Document(d)) => Some(d),
                    _ => None,
                };
                sub.set_path(in_sub, out_sub, rest, leaf);
                Some(sub)
            }
            // A sub-document this model has a size for, not a storage.
            (Some(Held::Bytes(_)), Some(Bson::Document(_))) => None,
            // No document was there: one is built with the path in it.
            _ => {
                let mut sub = Stored::built();
                sub.set_path(None, out_sub, rest, leaf);
                Some(sub)
            }
        };
        match sub {
            Some(sub) => self.set_held(top, Held::Document(Rc::new(RefCell::new(sub)))),
            None => self.set_field(top, held(&Bson::Document(out_sub.clone()))),
        }
    }

    /// A write of the top-level field `name` (`MutableDocument::setField`,
    /// `addField`, `remove`): the cached field when there is one, a new one
    /// in the cache otherwise. `held`: what its new value holds beyond its
    /// `Value` (0 for a removed field).
    pub fn set_field(&mut self, name: &str, held: usize) {
        self.set_held(name, Held::Bytes(held));
    }

    fn set_held(&mut self, name: &str, held: Held) {
        match self.cached(name) {
            Some(h) => *h = held,
            None => self.append(name, held),
        }
    }

    /// `MutableDocument::addField`: a new field, never looked up.
    pub fn add_field(&mut self, name: &str, held: usize) {
        self.append(name, Held::Bytes(held));
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
            + self.fields.iter().map(|(_, h)| h.size()).sum::<usize>()
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

/// The field paths `expr` reads, in the order it reads them: each `$path`
/// whole (variables, `$$...`, read none).
pub fn paths_read(expr: &Bson, out: &mut Vec<String>) {
    match expr {
        Bson::String(s) if s.starts_with('$') && !s.starts_with("$$") => {
            let path = &s[1..];
            if !path.is_empty() && !out.iter().any(|f| f == path) {
                out.push(path.to_string());
            }
        }
        Bson::Document(d) => {
            if d.contains_key("$literal") {
                return;
            }
            for v in d.values() {
                paths_read(v, out);
            }
        }
        Bson::Array(a) => {
            for v in a {
                paths_read(v, out);
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

    // RM-515's documents: `e` is `{x: 1, y: <50 bytes>}`, 70 bytes of BSON.
    fn nested_doc() -> Document {
        doc! { "_id": 1, "e": { "x": 1, "y": "w".repeat(50) }, "n": 1 }
    }

    #[test]
    fn a_read_inside_a_sub_document_caches_it_there() {
        // mongod 8.2.11: `$addFields: {q: "$e.y"}` counts 202 bytes more a
        // document than a read of `e` alone -- the sub-document's 128-byte
        // cache and the 74 its string holds -- in a sort and a window alike.
        let d = nested_doc();
        let mut top = Stored::read(memlimit::bson_len(&d));
        top.read_field(&d, "e");
        let mut nested = Stored::read(memlimit::bson_len(&d));
        nested.read_path(&d, "e.y");
        assert_eq!(nested.for_sorter() - top.for_sorter(), 202);
        assert_eq!(nested.approximate() - top.approximate(), 202);
    }

    #[test]
    fn a_write_inside_a_sub_document_sets_it_on_a_copy() {
        // `$set: {"e.z": 1}`: `e` is read, then holds a copy of its storage
        // with `z` cached (128 bytes) over the BSON it was read from.
        let d = nested_doc();
        let mut out = d.clone();
        out.get_document_mut("e").unwrap().insert("z", 1);
        let mut s = Stored::read(memlimit::bson_len(&d));
        s.set_path(Some(&d), &out, "e.z", 0);
        let e = memlimit::element_len("e", d.get("e").unwrap());
        let sub = DOCUMENT + STORAGE + 70 + 128;
        assert_eq!(
            s.approximate(),
            DOCUMENT + STORAGE + 128 + memlimit::bson_len(&d) + sub
        );
        assert_eq!(s.for_sorter(), s.current() - e);
    }

    #[test]
    fn a_field_set_from_a_sub_document_shares_its_storage() {
        // `{q: "$e"}`, then a read of `e.y`: both fields hold the one
        // storage, and each counts what the read cached in it.
        let d = nested_doc();
        let mut s = Stored::read(memlimit::bson_len(&d));
        s.read_path(&d, "e");
        assert!(s.set_shared("q", "e"));
        let before = s.approximate();
        s.read_nested(&d, "e", "y");
        assert_eq!(s.approximate() - before, 2 * 202);
    }

    #[test]
    fn reserved_fields_match_a_group_document() {
        let mut s = Stored::reserved(2);
        s.add_field("_id", 0);
        s.add_field("n", 0);
        assert_eq!(s.approximate(), 237);
    }
}
