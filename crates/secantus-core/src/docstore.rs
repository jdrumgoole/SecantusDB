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
    /// An array an expression built (`RCVector`): what each element holds,
    /// a document an expression object built its own storage, which an
    /// `$unwind` hands to the field it sets (RM-539).
    Array(Vec<Held>),
}

impl Held {
    fn size(&self) -> usize {
        match self {
            Held::Bytes(n) => *n,
            Held::Document(s) => s.borrow().approximate(),
            Held::Array(items) => {
                memlimit::RC_VECTOR
                    + items
                        .iter()
                        .map(|h| memlimit::VALUE + h.size())
                        .sum::<usize>()
            }
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
    /// Whether a stage wrote to it (`_modified`): a document still as it
    /// was read is "trivially convertible" back to its BSON.
    modified: bool,
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
            modified: false,
        }
    }

    /// A document built field by field (`MutableDocument()`).
    pub fn built() -> Self {
        Stored {
            modified: true,
            ..Stored::read(EMPTY_BSON)
        }
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
        self.modified = true;
        match self.cached(name) {
            Some(h) => *h = held,
            None => self.append(name, held),
        }
    }

    /// `MutableDocument::addField`: a new field, never looked up.
    pub fn add_field(&mut self, name: &str, held: usize) {
        self.modified = true;
        self.append(name, Held::Bytes(held));
    }

    /// What the cached field `name`'s value holds, when it is cached: a
    /// copy of the `Value`, which shares a sub-document's storage.
    fn held_of(&self, name: &str) -> Option<Held> {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, h)| h.clone())
    }

    /// `name` set to `v`, the value `expr` evaluated to
    /// (`MutableDocument::setField` with an expression's result): a document
    /// or an array the expression built keeps its own storage
    /// ([`produced_held`]).
    pub fn set_produced(&mut self, name: &str, expr: &Bson, v: &Bson) {
        self.set_held(name, produced(expr, v));
    }

    /// `name`, a cached array an expression built, set to its element `at`
    /// of `of` (`UnwindProcessor`: `setNestedField` with the element's
    /// `Value`, which shares a document's storage). False when the cache
    /// holds no such array, and nothing is set.
    pub fn unwind_element(&mut self, name: &str, at: usize, of: usize) -> bool {
        let element = match self.cached(name) {
            Some(Held::Array(items)) if items.len() == of => items[at].clone(),
            _ => return false,
        };
        self.set_held(name, element);
        true
    }

    /// The storage of the sub-document at the dotted `path` of `doc`, the
    /// document this storage holds, read into the cache (`$replaceRoot:
    /// {newRoot: "$path"}`: the new root is that sub-document's storage).
    /// `None` when the cache holds no sub-document there.
    pub fn root(&mut self, doc: &Document, path: &str) -> Option<Stored> {
        self.read_path(doc, path);
        self.shared(path).map(|s| s.borrow().clone())
    }

    /// The storage of the document `expr`, an expression object, built as
    /// `v` (`ExpressionObject::evaluate`). `None` for any other expression.
    pub fn produced_root(expr: &Bson, v: &Bson) -> Option<Stored> {
        match produced(expr, v) {
            Held::Document(s) => Some(s.borrow().clone()),
            _ => None,
        }
    }

    /// Whether a stage wrote to the document.
    pub fn modified(&self) -> bool {
        self.modified
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
    produced(expr, v).size()
}

/// What `v`, the value `expr` evaluated to, holds, as [`produced_held`]
/// counts it: with the storage of each document an expression object built.
fn produced(expr: &Bson, v: &Bson) -> Held {
    match (expr, v) {
        (Bson::Document(e), Bson::Document(d)) if is_object_expression(e) => {
            let mut s = Stored::built();
            for (k, sub) in e {
                let h = d.get(k).map_or(Held::Bytes(0), |val| produced(sub, val));
                s.modified = true;
                s.append(k, h);
            }
            Held::Document(Rc::new(RefCell::new(s)))
        }
        (Bson::Array(es), Bson::Array(vs)) if es.len() == vs.len() => {
            Held::Array(es.iter().zip(vs).map(|(e, v)| produced(e, v)).collect())
        }
        _ => Held::Bytes(held(v)),
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

/// One level of an inclusion `$project` (`InclusionNode`,
/// `projection_node.cpp`, 8.2.1): the fields it copies, the sub-projections
/// under it and the fields it computes, the last two in the spec's order
/// (`_orderToProcessAdditionsAndChildren`).
#[derive(Debug, Default)]
pub struct Inclusion {
    projected: Vec<String>,
    children: Vec<(String, Inclusion)>,
    expressions: Vec<(String, Bson)>,
    order: Vec<String>,
}

/// What a projection makes of a value, kept as far as its size needs.
enum Built {
    /// No value (`Value()`), which a document still gives a field to.
    Missing,
    /// A value copied or computed, and what it holds beyond its `Value`.
    Leaf(Held),
    /// A document the projection built, and its fields.
    Doc(Stored, Vec<(String, Built)>),
    /// An array it rebuilt element by element.
    Array(Vec<Built>),
}

impl Built {
    /// What the value holds beyond its `Value`. An array's missing
    /// elements count nothing (measured 8.2.11: `{"r.x": 1}` over
    /// `r: [3]` holds 40, as over `r: []`).
    fn held(&self) -> usize {
        match self {
            Built::Missing => 0,
            Built::Leaf(h) => h.size(),
            Built::Doc(s, _) => s.approximate(),
            Built::Array(vs) => {
                memlimit::RC_VECTOR
                    + vs.iter()
                        .filter(|v| !matches!(v, Built::Missing))
                        .map(|v| memlimit::VALUE + v.held())
                        .sum::<usize>()
            }
        }
    }

    /// The `Value` a document's field holds: a copied one shares its
    /// sub-document's storage.
    fn as_held(&self) -> Held {
        match self {
            Built::Leaf(h) => h.clone(),
            _ => Held::Bytes(self.held()),
        }
    }
}

/// Set `name` to `v` in a document being built (`MutableDocument::setField`).
fn set_built(s: &mut Stored, fields: &mut Vec<(String, Built)>, name: &str, v: Built) {
    s.set_held(name, v.as_held());
    match fields.iter_mut().find(|(n, _)| n == name) {
        Some((_, old)) => *old = v,
        None => fields.push((name.to_string(), v)),
    }
}

impl Inclusion {
    /// The tree of an inclusion `$project` spec; `None` for any other
    /// projection (an exclusion, or one with `$slice`, `$elemMatch` or
    /// `$meta`), whose storage is not modelled.
    pub fn parse(spec: &Document) -> Option<Inclusion> {
        use crate::projection::{flag_truthy, is_computed_spec, is_flag_value};
        let flat = crate::projection::flatten_projection_spec(spec).ok()?;
        let mut root = Inclusion::default();
        let mut id = None;
        let mut included = false;
        for (key, value) in &flat {
            if key == "_id" && is_flag_value(value) {
                id = Some(flag_truthy(value));
                continue;
            }
            if key == "_id" || key.starts_with("_id.") {
                id = Some(false);
            }
            let expr = if is_flag_value(value) {
                if !flag_truthy(value) {
                    return None;
                }
                None
            } else if is_computed_spec(value) {
                Some(value.clone())
            } else {
                return None;
            };
            root.insert(key, expr);
            included = true;
        }
        if !included && id != Some(true) {
            return None;
        }
        // `_id` is copied unless the spec excludes or computes it.
        if id != Some(false) {
            root.projected.insert(0, "_id".to_string());
        }
        Some(root)
    }

    fn insert(&mut self, path: &str, expr: Option<Bson>) {
        match path.split_once('.') {
            Some((head, rest)) => {
                if !self.children.iter().any(|(n, _)| n == head) {
                    self.order.push(head.to_string());
                    self.children.push((head.to_string(), Inclusion::default()));
                }
                let (_, child) = self
                    .children
                    .iter_mut()
                    .find(|(n, _)| n == head)
                    .expect("just added");
                child.insert(rest, expr);
            }
            None => match expr {
                Some(e) => {
                    self.order.push(path.to_string());
                    self.expressions.push((path.to_string(), e));
                }
                None => self.projected.push(path.to_string()),
            },
        }
    }

    /// `subtreeContainsComputedFields`.
    fn computed(&self) -> bool {
        !self.expressions.is_empty() || self.children.iter().any(|(_, c)| c.computed())
    }

    fn child(&self, name: &str) -> Option<&Inclusion> {
        self.children
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| c)
    }

    /// The storage of what the projection makes of `input`, whose storage
    /// is `s` (`ProjectionNode::applyToDocument`), `out` being that
    /// result. A projection with nothing to compute takes a document no
    /// stage wrote to straight from its BSON into new BSON
    /// (`FastPathEligibleInclusionNode`); otherwise it builds the document
    /// with room for every field it names (`MutableDocument(n)`), copying
    /// each field it keeps in the input's order and then setting each
    /// computed one (measured 8.2.11).
    pub fn apply(&self, input: &Document, s: &Stored, out: &Document, vars: &Document) -> Stored {
        let mut built = if !self.computed() && !s.modified() {
            Stored::read(memlimit::bson_len(out))
        } else {
            // Copying a field caches it in the input (the field iterator's
            // `constructInCache`), and what the computed fields read is
            // cached there too: inside a sub-document, in the storage a
            // copied field shares (RM-515).
            let mut s_in = s.clone();
            let (mut st, mut fields) = self.project_doc(input, Some(&mut s_in));
            self.read_paths(input, &mut s_in);
            if self.computed() {
                self.apply_expressions(&mut st, &mut fields, input, Some(&s_in), vars);
            }
            st
        };
        built.set_metadata(s.metadata);
        built
    }

    /// The reads each computed field's expression makes of `root`, whose
    /// storage is `s`.
    fn read_paths(&self, root: &Document, s: &mut Stored) {
        for (_, expr) in &self.expressions {
            let mut read = Vec::new();
            paths_read(expr, &mut read);
            for path in &read {
                s.read_path(root, path);
            }
        }
        for (_, child) in &self.children {
            child.read_paths(root, s);
        }
    }

    /// `initializeOutputDocument` and `applyProjections` over `doc`.
    /// `cached`: the document's storage, whose cache holds the values a
    /// stage set, and takes each field copied from its BSON.
    fn project_doc(
        &self,
        doc: &Document,
        mut cached: Option<&mut Stored>,
    ) -> (Stored, Vec<(String, Built)>) {
        let mut st =
            Stored::reserved(self.projected.len() + self.children.len() + self.expressions.len());
        let mut fields = Vec::new();
        for (k, v) in doc {
            let built = if self.projected.iter().any(|p| p == k) {
                let copied = cached.as_deref_mut().and_then(|s| {
                    s.read_field(doc, k);
                    s.held_of(k)
                });
                Built::Leaf(copied.unwrap_or_else(|| Held::read(v)))
            } else if let Some(child) = self.child(k) {
                child.project_value(v)
            } else {
                continue;
            };
            st.set_held(k, built.as_held());
            fields.push((k.clone(), built));
        }
        (st, fields)
    }

    /// `applyProjectionsToValue`: a document projected, an array element by
    /// element (nested arrays too, as `$project` recurses), anything else
    /// nothing.
    fn project_value(&self, v: &Bson) -> Built {
        match v {
            Bson::Document(d) => {
                let (st, fields) = self.project_doc(d, None);
                Built::Doc(st, fields)
            }
            Bson::Array(a) => Built::Array(a.iter().map(|e| self.project_value(e)).collect()),
            _ => Built::Missing,
        }
    }

    /// `applyExpressions`: each sub-projection and computed field, in the
    /// spec's order, set on the document being built.
    fn apply_expressions(
        &self,
        st: &mut Stored,
        fields: &mut Vec<(String, Built)>,
        root: &Document,
        input: Option<&Stored>,
        vars: &Document,
    ) {
        for name in &self.order {
            let v = match self.child(name) {
                Some(child) => {
                    let current = fields
                        .iter_mut()
                        .find(|(n, _)| n == name)
                        .map(|(_, v)| std::mem::replace(v, Built::Missing))
                        .unwrap_or(Built::Missing);
                    child.expressions_to_value(current, root, vars)
                }
                None => {
                    let expr = self
                        .expressions
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, e)| e)
                        .expect("an expression or a child");
                    computed_value(expr, root, input, vars)
                }
            };
            set_built(st, fields, name, v);
        }
    }

    /// `applyExpressionsToValue`.
    fn expressions_to_value(&self, v: Built, root: &Document, vars: &Document) -> Built {
        match v {
            Built::Doc(mut st, mut fields) => {
                self.apply_expressions(&mut st, &mut fields, root, None, vars);
                Built::Doc(st, fields)
            }
            Built::Array(vs) => Built::Array(
                vs.into_iter()
                    .map(|e| self.expressions_to_value(e, root, vars))
                    .collect(),
            ),
            _ if self.computed() => {
                // A value with no fields to set is replaced by a document of
                // the computed ones (`{"a.b": {$literal: 1}}` over `{a: 1}`).
                let mut st = Stored::built();
                let mut fields = Vec::new();
                self.apply_expressions(&mut st, &mut fields, root, None, vars);
                Built::Doc(st, fields)
            }
            _ => Built::Missing,
        }
    }
}

/// What a computed field's expression gives over `root`: a field path to
/// a value the input caches is that value, sharing a sub-document's
/// storage.
fn computed_value(expr: &Bson, root: &Document, input: Option<&Stored>, vars: &Document) -> Built {
    if let (Bson::String(p), Some(s)) = (expr, input) {
        if let Some(path) = p.strip_prefix('$').filter(|f| !f.starts_with('$')) {
            if let Some(sub) = s.shared(path) {
                return Built::Leaf(Held::Document(sub));
            }
            if let Some(h) = s.held_of(path) {
                return Built::Leaf(h);
            }
        }
    }
    match crate::expressions::evaluate_or_missing(root, expr, vars) {
        Ok(Bson::Undefined) | Err(_) => Built::Missing,
        Ok(v) => Built::Leaf(Held::Bytes(produced_held(expr, &v))),
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

    /// What an inclusion `$project` by `spec` builds of `input`, behind a
    /// stage that wrote `t` (an `$unwind`), for a `$sort`.
    fn projected(spec: Document, input: &Document) -> usize {
        let mut s = Stored::read(memlimit::bson_len(input));
        s.read_field(input, "t");
        s.set_field("t", field_held(input, "t"));
        let out = Document::new(); // unused: the input was written to
        Inclusion::parse(&spec)
            .expect("an inclusion")
            .apply(input, &s, &out, &Document::new())
            .for_sorter()
    }

    fn field_held(d: &Document, name: &str) -> usize {
        d.get(name).map_or(0, held)
    }

    #[test]
    fn an_inclusion_project_reserves_a_field_for_each_it_names() {
        // mongod 8.2.11, `[{$unwind: "$t"}, {$project: ...}, {$sort}]`: each
        // document less its 16-byte sort key.
        let d = doc! { "_id": 1, "b": "y".repeat(100), "t": 1, "n": 1 };
        assert_eq!(projected(doc! {"t": 1, "n": 1}, &d), 261);
        assert_eq!(projected(doc! {"t": 1, "n": 1, "_id": 0}, &d), 229);
        // A field the document lacks is still reserved for.
        assert_eq!(projected(doc! {"t": 1, "n": 1, "zz": 1}, &d), 293);
        // A computed field too, and what its value holds.
        let concat = doc! {"t": 1, "n": 1, "x": {"$concat": ["$b", "!"]}};
        assert_eq!(projected(concat, &d), 418);
    }

    #[test]
    fn a_sub_projection_builds_documents_and_arrays_of_its_own() {
        let d = doc! {
            "_id": 1,
            "e": {"x": 1, "y": "w".repeat(50)},
            "r": [{"x": 1, "y": 2}, 3, {"y": 1}],
            "t": 1,
            "n": 1,
        };
        // `e`: a document with room for one field (205).
        assert_eq!(projected(doc! {"t": 1, "e.x": 1}, &d), 261 + 205);
        // `r`: 40 for the vector, 221 for each document, nothing for `3`.
        assert_eq!(projected(doc! {"r.x": 1, "n": 1}, &d), 261 + 482);
        // A computed field under `r`: the scalar becomes `{q: 2}` (237).
        let q = doc! {"r.q": {"$add": [1, 1]}, "n": 1};
        assert_eq!(projected(q, &d), 261 + 735);
    }

    #[test]
    fn an_unwritten_document_is_projected_into_new_bson() {
        let d = doc! { "_id": 1, "b": "y".repeat(100), "t": 5, "n": 1 };
        let s = Stored::read(memlimit::bson_len(&d));
        let out = doc! { "_id": 1, "t": 5, "n": 1 };
        let inc = Inclusion::parse(&doc! {"t": 1, "n": 1}).expect("an inclusion");
        assert_eq!(
            inc.apply(&d, &s, &out, &Document::new()).for_sorter(),
            memlimit::bson_len(&out) + STORAGE
        );
        // With something to compute it builds the document all the same.
        let inc = Inclusion::parse(&doc! {"t": 1, "x": {"$add": [1, 1]}}).expect("an inclusion");
        assert_eq!(inc.apply(&d, &s, &out, &Document::new()).for_sorter(), 261);
    }

    #[test]
    fn only_an_inclusion_is_modelled() {
        assert!(Inclusion::parse(&doc! {"b": 0}).is_none());
        assert!(Inclusion::parse(&doc! {"_id": 0}).is_none());
        assert!(Inclusion::parse(&doc! {"t": {"$slice": 1}}).is_none());
        assert!(Inclusion::parse(&doc! {"_id": 1}).is_some());
        assert!(Inclusion::parse(&doc! {"_id": 0, "t": 1}).is_some());
    }

    #[test]
    fn reserved_fields_match_a_group_document() {
        let mut s = Stored::reserved(2);
        s.add_field("_id", 0);
        s.add_field("n", 0);
        assert_eq!(s.approximate(), 237);
    }
}
