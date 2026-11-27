//! The bucket catalog's memory account: `serverStatus`'s
//! `bucketCatalog.memoryUsage`.
//!
//! `mongod` 8.2.11 reports the bytes its time-series bucket catalog
//! allocates through `tracking::Allocator` (`src/mongo/util/tracking/`): the
//! catalog's stripes, the buckets, their summaries and column builders, the
//! maps that find them, the registry that tracks their state. That count is
//! made of the C++ containers' own allocations, so this module charges the
//! bytes those containers allocate, structure for structure, as the
//! **Linux x86-64 build** (the exemplar: libstdc++, Abseil 20250512.1)
//! lays them out -- on every host, since the exemplar is one build
//! (Joe, 2026-11-25). Nothing here is fitted to a number: each constant is
//! a `sizeof` worked out from the source, and the totals it gives were then
//! compared with the official `mongo:8.2.11` / `8.2.12` amd64 images
//! (`tools/probes/linux_bucket_catalog*.py`, 2026-11-26).
//!
//! What is charged, and by which C++ type:
//!
//! - the catalog: 32 `Stripe`s (`tracking::make_unique`, 256 bytes each:
//!   `std::mutex` 40, three `absl::node_hash_map` 48 each, a `std::list` 40,
//!   an `absl::btree_map` 32) and the vector of their `unique_ptr`s;
//! - an Abseil table (`raw_hash_set`): `ControlOffset` 8, then
//!   `capacity + 1 + 15` control bytes (the SSE2 group is 16 wide), the slots
//!   after them, capacity `1, 3, 7, 15, ...` growing when `growth_left` is
//!   spent; a table whose slot is no wider than 16 bytes keeps one element
//!   inline (`SooEnabled`), so a `node_hash_map` pays its node only, until a
//!   second element;
//! - a `Bucket` (544 bytes), its `openBucketsById` and `bucketStates`
//!   nodes, its place in `openBucketsByKey` (the `BucketKey` node, the
//!   metadata `SharedBuffer`: 16 bytes of header and the `BufBuilder`'s
//!   capacity) and in the stripe's idle list;
//! - the bucket's `fieldNames` / `uncommittedFieldNames` sets (`tracking::string`
//!   40 bytes, heap when over 15 characters), its `MinMax` and `Schema`
//!   (`FlatBSON`: a `std::vector` of entries, 216 and 112 bytes each, the
//!   field names, the min and max value buffers), and its `MeasurementMap`
//!   (a `flat_hash_map` of `BSONColumnBuilder`s, 336 bytes a slot, and what
//!   each builder keeps after `intermediate()`).
//!
//! Two limits, named where they apply: an object or array data field's
//! builder in interleaved *appending* mode is charged from its state, not
//! allocation for allocation, and a bucket reopened from its stored
//! document is charged the state it rebuilds.

use std::collections::{HashMap, HashSet};

use bson::{Bson, Document};
use secantus_core::bsoncolumn::{ColumnShape, EncShape};

use crate::bucket_catalog::value_size;
use crate::ts_buckets::canonical_type;

pub(crate) const STRIPES: usize = 32;

// ---------------------------------------------------------------------------
// Sizes: libstdc++ x86-64, Abseil 20250512.1, mongod 8.2.11.

/// `Stripe`, and the `tracking::unique_ptr<Stripe>` that holds it.
const STRIPE: i64 = 256;
const STRIPE_PTR: i64 = 16;
/// What an empty catalog holds.
const BASE: i64 = STRIPES as i64 * (STRIPE + STRIPE_PTR);
/// `pair<const UUID, shared_ptr<ExecutionStats>>`, and the shared block
/// (`_Sp_counted_ptr_inplace`: 16 + allocator 8 + 38 counters).
const STATS_NODE: i64 = 32;
const STATS_BLOCK: i64 = 328;
/// `sizeof(Bucket)`.
const BUCKET: i64 = 544;
/// `pair<const BucketId, unique_ptr<Bucket>>`.
const BYID_NODE: i64 = 56;
/// `pair<const BucketId, variant<BucketState, DirectWriteCounter>>`.
const STATE_NODE: i64 = 48;
/// `_Rb_tree_node<pair<const Era, uint64_t>>`.
const ERA_NODE: i64 = 48;
/// `pair<const BucketKey, flat_hash_set<Bucket*>>`.
const BYKEY_NODE: i64 = 104;
/// `_List_node<Bucket*>`.
const IDLE_NODE: i64 = 24;
/// `pair<const OperationId, shared_ptr<WriteBatch>>`.
const BATCH_NODE: i64 = 24;
/// `SharedBuffer::kHolderSize`.
const HOLDER: i64 = 16;
/// `FlatBSONStore<MinMaxElement, ..>::Entry` and the `Schema`'s.
const ENTRY_MINMAX: i64 = 216;
const ENTRY_SCHEMA: i64 = 112;
/// `pair<const tracking::string, BSONColumnBuilder>`.
const BUILDER_SLOT: usize = 336;
/// A `tracking::string`, and `pair<const string, uint32_t>`.
const STRING_SLOT: usize = 40;
const SEARCH_SLOT: usize = 48;
/// `Simple8bBuilder::PendingValue` for the 64- and 128-bit encoders.
const PENDING_64: i64 = 24;
const PENDING_128: i64 = 48;
/// `sizeof(BSONColumnBuilder::InternalState::SubObjState)`.
const SUBOBJ_STATE: i64 = 256;
/// `pair<ptrdiff_t, size_t>`.
const CONTROL_BLOCK: i64 = 16;
/// `std::string`'s inline capacity.
const SSO: usize = 15;
/// `kMaxLinearSearchLength` of `flat_bson.cpp`.
const MAX_LINEAR_SEARCH: usize = 12;
/// `kArrayFieldName`.
const ARRAY_NAME: &str = "\0";
/// `BufBuilder`'s `kDefaultInitSizeBytes` and `SharedBuffer`'s header.
const BUF_DEFAULT: usize = 512;

fn align_up(x: usize, a: usize) -> usize {
    x.div_ceil(a) * a
}

/// What a `std::string` copy (`make_string`) allocates: its length + 1 once
/// past the inline capacity.
fn string_copy(len: usize) -> i64 {
    if len > SSO {
        len as i64 + 1
    } else {
        0
    }
}

/// A `std::string`'s capacity after an assignment of `len` characters
/// (`_M_create`: at least double the old capacity once it must grow).
fn string_assign(cap: usize, len: usize) -> usize {
    if len <= cap {
        cap
    } else if len < 2 * cap {
        2 * cap
    } else {
        len
    }
}

fn string_alloc(cap: usize) -> i64 {
    if cap > SSO {
        cap as i64 + 1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Abseil's `raw_hash_set`.

/// One Swiss table's allocation: its capacity only changes by growing, so
/// the capacity that holds `n` elements, remembered, is the table's.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Table {
    slot: usize,
    align: usize,
    soo: bool,
    cap: usize,
}

impl Table {
    /// A table whose slots are `slot` bytes, aligned to `align`.
    pub(crate) fn new(slot: usize, align: usize) -> Table {
        // `SooEnabled`: `sizeof(slot_type) <= sizeof(HeapOrSoo)` (16).
        let soo = slot <= 16 && align <= 8;
        Table {
            slot,
            align,
            soo,
            cap: usize::from(soo),
        }
    }

    /// `CapacityToGrowth`.
    fn growth(cap: usize) -> usize {
        cap - cap / 8
    }

    /// The table has held `n` elements: grow it as inserts would have.
    pub(crate) fn fit(&mut self, n: usize) {
        while n > Self::growth(self.cap) {
            self.cap = self.cap * 2 + 1;
        }
    }

    /// `RawHashSetLayout::alloc_size`, nothing while the table is empty of
    /// capacity or keeps its one element inline.
    pub(crate) fn bytes(&self) -> i64 {
        if self.cap == 0 || (self.soo && self.cap == 1) {
            return 0;
        }
        (align_up(8 + self.cap + 1 + 15, self.align) + self.cap * self.slot) as i64
    }
}

// ---------------------------------------------------------------------------
// `BufBuilder`, for the metadata buffer.

/// `BasicBufBuilder`'s bookkeeping: capacity, length and reserved bytes.
struct BufSim {
    cap: usize,
    len: usize,
    reserved: usize,
}

impl BufSim {
    fn new(cap: usize) -> BufSim {
        BufSim {
            cap,
            len: 0,
            reserved: 0,
        }
    }

    /// `_growOutOfLineSlowPath`.
    fn grow_out_of_line(&mut self, by: usize) {
        let min_size = self.len + by + self.reserved + HOLDER as usize;
        let mut realloc = min_size.next_power_of_two();
        // `BSONObjMaxUserSize` .. `kOpMsgReplyBSONBufferMaxSize`.
        const MAX_USER: usize = 16 * 1024 * 1024;
        const OP_MSG_REPLY: usize = MAX_USER * 2 + 64 * 1024;
        if (MAX_USER..=OP_MSG_REPLY).contains(&min_size) || realloc == MAX_USER {
            realloc = OP_MSG_REPLY;
        } else if realloc < 64 {
            realloc = 64;
        }
        self.cap = realloc - HOLDER as usize;
    }

    /// `grow(by)`: claim `by` bytes at the end.
    fn grow(&mut self, by: usize) {
        if by + self.reserved + self.len > self.cap {
            self.grow_out_of_line(by);
        }
        self.len += by;
    }

    /// `reserveBytes(n)`.
    fn reserve(&mut self, n: usize) {
        if n + self.len + self.reserved <= self.cap {
            self.reserved += n;
            return;
        }
        self.grow_out_of_line(n);
        self.reserved += n;
    }

    fn claim(&mut self, n: usize) {
        self.reserved -= n;
    }
}

/// An element appended under `name`: `appendAs` writes its type byte, name
/// and value one after the other; `append(elem)` copies it whole.
fn buf_append_value(b: &mut BufSim, name: &str, v: &Bson, whole: bool) {
    if whole {
        b.grow(2 + name.len() + value_size(v) as usize);
    } else {
        b.grow(1);
        b.grow(name.len() + 1);
        b.grow(value_size(v) as usize);
    }
}

fn buf_sub_obj(b: &mut BufSim, d: &Document) {
    b.grow(4);
    b.reserve(1);
    let mut fields: Vec<(&String, &Bson)> = d.iter().collect();
    fields.sort_by(|x, y| x.0.as_bytes().cmp(y.0.as_bytes()));
    for (k, v) in fields {
        buf_normalized(b, k, v, true);
    }
    b.claim(1);
    b.grow(1);
}

fn buf_sub_array(b: &mut BufSim, a: &[Bson]) {
    b.grow(4);
    b.reserve(1);
    for (i, v) in a.iter().enumerate() {
        buf_normalized(b, &i.to_string(), v, false);
    }
    b.claim(1);
    b.grow(1);
}

/// `metadata::normalize`: an element appended to the builder.
fn buf_normalized(b: &mut BufSim, name: &str, v: &Bson, whole: bool) {
    match v {
        Bson::Document(d) => {
            b.grow(1);
            b.grow(name.len() + 1);
            buf_sub_obj(b, d);
        }
        Bson::Array(a) => {
            b.grow(1);
            b.grow(name.len() + 1);
            buf_sub_array(b, a);
        }
        _ => buf_append_value(b, name, v, whole),
    }
}

/// The bytes `BucketMetadata` allocates for a meta value: the
/// `BSONObjBuilder`'s buffer (`SharedBuffer`: header + capacity), grown as
/// `reserveBytes(elem.size())` and the appends require. Nothing without a
/// meta value.
pub(crate) fn metadata_bytes(name: &str, meta: Option<&Bson>) -> i64 {
    let Some(v) = meta else {
        return 0;
    };
    let mut b = BufSim::new(BUF_DEFAULT);
    // The object's length, and the byte `doneFast` writes last.
    b.grow(4);
    b.reserve(1);
    let elem_size = 2 + name.len() + value_size(v) as usize;
    b.reserve(elem_size);
    buf_normalized(&mut b, name, v, false);
    HOLDER + b.cap as i64
}

// ---------------------------------------------------------------------------
// `FlatBSON`: the `MinMax` and the `Schema` of a bucket.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Unset,
    Value,
    Object,
    Array,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
enum Status {
    Updated,
    Failed,
    NoChange,
}

/// `FlatBSONStore::Data`: what a min or max (or the schema) holds at an
/// entry.
#[derive(Debug, Clone)]
struct Data {
    kind: Kind,
    held: Option<Bson>,
    /// `BSONElementValueBuffer`'s vector: its size and capacity.
    buf_size: usize,
    buf_cap: usize,
    /// The schema keeps the canonical type of its value.
    canon: i32,
}

impl Data {
    fn new() -> Data {
        Data {
            kind: Kind::Unset,
            held: None,
            buf_size: 0,
            buf_cap: 0,
            canon: 0,
        }
    }

    fn set_object(&mut self) {
        self.kind = Kind::Object;
    }

    fn set_array(&mut self) {
        self.kind = Kind::Array;
    }

    fn set_unset(&mut self) {
        self.kind = Kind::Unset;
    }

    /// `setValue`: the value buffer grows (`vector::resize`) when the
    /// value needs more than the largest it has held.
    fn set_value(&mut self, v: &Bson, buffered: bool) {
        self.kind = Kind::Value;
        self.canon = canonical_type(v);
        if buffered {
            // `elem.size() - elem.fieldNameSize() + 1`.
            let size = value_size(v) as usize + 2;
            if self.buf_size < size {
                let add = size - self.buf_size;
                if self.buf_cap - self.buf_size < add {
                    self.buf_cap = self.buf_size + self.buf_size.max(add);
                }
                self.buf_size = size;
            }
            self.held = Some(v.clone());
        }
    }
}

/// `StringMap<uint32_t>`, a `FlatBSONStore` entry's fast lookup.
#[derive(Debug, Clone)]
struct SearchMap {
    idx: HashMap<String, usize>,
    keys: i64,
    table: Table,
}

impl SearchMap {
    fn new() -> SearchMap {
        SearchMap {
            idx: HashMap::new(),
            keys: 0,
            table: Table::new(SEARCH_SLOT, 8),
        }
    }

    fn insert(&mut self, name: &str, at: usize) {
        self.idx.insert(name.to_string(), at);
        self.keys += string_copy(name.len());
        self.table.fit(self.idx.len());
    }

    fn bytes(&self) -> i64 {
        self.keys + self.table.bytes()
    }
}

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    name_cap: usize,
    /// `_offsetEnd`: the entry and its descendants.
    end: usize,
    /// `_offsetParent`.
    parent: usize,
    map: Option<SearchMap>,
    min: Data,
    max: Data,
}

impl Entry {
    fn new() -> Entry {
        Entry {
            name: String::new(),
            name_cap: SSO,
            end: 1,
            parent: 0,
            map: None,
            min: Data::new(),
            max: Data::new(),
        }
    }

    fn set_name(&mut self, name: &str) {
        self.name_cap = string_assign(self.name_cap, name.len());
        self.name = name.to_string();
    }
}

/// `UpdateContext`: whether the min and the max are still being updated.
#[derive(Debug, Clone, Copy)]
struct Ctx {
    min: bool,
    max: bool,
}

/// A `MinMax` or a `Schema`.
#[derive(Debug, Clone)]
pub(crate) struct FlatStore {
    minmax: bool,
    entries: Vec<Entry>,
    /// The entries vector's capacity.
    cap: usize,
}

impl FlatStore {
    pub(crate) fn new(minmax: bool) -> FlatStore {
        let mut root = Entry::new();
        root.min.set_object();
        root.max.set_object();
        FlatStore {
            minmax,
            entries: vec![root],
            cap: 1,
        }
    }

    pub(crate) fn bytes(&self) -> i64 {
        let entry = if self.minmax {
            ENTRY_MINMAX
        } else {
            ENTRY_SCHEMA
        };
        let mut n = self.cap as i64 * entry;
        for e in &self.entries {
            n += string_alloc(e.name_cap);
            n += e.min.buf_cap as i64 + e.max.buf_cap as i64;
            if let Some(m) = &e.map {
                n += m.bytes();
            }
        }
        n
    }

    fn end_of(&self, pos: usize) -> usize {
        pos + self.entries[pos].end
    }

    /// `Obj::insert`: a new entry at `at` in the object at `obj`; its
    /// position.
    fn insert(&mut self, obj: usize, at: usize, name: &str) -> usize {
        if self.entries.len() == self.cap {
            self.cap *= 2;
        }
        let mut e = Entry::new();
        e.set_name(name);
        e.parent = at - obj;
        self.entries.insert(at, e);
        if self.entries[obj].map.is_some() {
            let off = at - obj;
            if let Some(m) = &mut self.entries[obj].map {
                m.insert(name, off);
            }
        }
        let mut it = at;
        let mut parent = obj;
        while it != parent {
            self.entries[parent].end += 1;
            let end = parent + self.entries[parent].end;
            let mut next = it + self.entries[it].end;
            while next != end {
                self.entries[next].parent += 1;
                if self.entries[parent].map.is_some() {
                    let n = self.entries[next].name.clone();
                    if let Some(m) = &mut self.entries[parent].map {
                        if let Some(i) = m.idx.get_mut(&n) {
                            *i += 1;
                        }
                    }
                }
                next += self.entries[next].end;
            }
            it = parent;
            parent -= self.entries[parent].parent;
        }
        at
    }

    /// `Obj::search(first, last, name)`.
    fn search(&mut self, obj: usize, first: usize, last: usize, name: &str) -> usize {
        if let Some(m) = &self.entries[obj].map {
            return match m.idx.get(name) {
                Some(i) => obj + i,
                None => last,
            };
        }
        let mut first = first;
        let mut remaining = MAX_LINEAR_SEARCH;
        while first != last && remaining != 0 {
            if self.entries[first].name == name {
                return first;
            }
            if self.entries[first].name == ARRAY_NAME {
                self.entries[first].set_name(name);
                return first;
            }
            first += self.entries[first].end;
            remaining -= 1;
        }
        if first == last {
            return last;
        }
        let mut map = SearchMap::new();
        let mut it = obj + 1;
        let end = self.end_of(obj);
        while it != end {
            map.insert(&self.entries[it].name.clone(), self.entries[it].parent);
            it += self.entries[it].end;
        }
        self.entries[obj].map = Some(map);
        self.search(obj, first, last, name)
    }

    fn data(&mut self, pos: usize, max: bool) -> &mut Data {
        if max {
            &mut self.entries[pos].max
        } else {
            &mut self.entries[pos].min
        }
    }

    /// `_setTypeObject` / `_setTypeArray`.
    fn set_type(&mut self, obj: usize, max: bool, array: bool) {
        let prev = self.data(obj, max).kind;
        let want = if array { Kind::Array } else { Kind::Object };
        if prev != want {
            if array {
                self.data(obj, max).set_array();
            } else {
                self.data(obj, max).set_object();
            }
            let end = self.end_of(obj);
            let mut it = obj + 1;
            while it != end {
                self.data(it, max).set_unset();
                it += self.entries[it].end;
            }
        }
    }

    /// `update(doc, omitField)`.
    pub(crate) fn update(&mut self, doc: &Document, omit: Option<&str>) -> bool {
        let ctx = Ctx {
            min: true,
            max: true,
        };
        self.update_obj(0, doc, ctx, omit) != Status::Failed
    }

    fn update_obj(&mut self, obj: usize, doc: &Document, ctx: Ctx, omit: Option<&str>) -> Status {
        let mut it = obj + 1;
        let mut end = self.end_of(obj);
        let mut all_handled = 0usize;
        let mut skipped = false;
        let mut status = Status::NoChange;
        for (name, v) in doc {
            if omit == Some(name.as_str()) {
                continue;
            }
            if it == end && skipped {
                let begin = self.nth_child(obj, all_handled);
                it = self.search(obj, begin, end, name);
            }
            if it == end {
                it = self.insert(obj, it, name);
            } else if self.entries[it].name == ARRAY_NAME {
                self.entries[it].set_name(name);
            } else if self.entries[it].name != *name {
                let next = it + self.entries[it].end;
                let found = self.search(obj, next, end, name);
                if found == end {
                    if skipped {
                        let begin = self.nth_child(obj, all_handled);
                        let found2 = self.search(obj, begin, it, name);
                        if found2 == it {
                            it = self.insert(obj, it, name);
                        } else {
                            it = found2;
                        }
                    } else {
                        it = self.insert(obj, it, name);
                    }
                } else {
                    it = found;
                    skipped = true;
                }
            }
            let sub = self.update_entry(it, v, ctx);
            if sub != Status::NoChange {
                status = sub;
            }
            end = self.end_of(obj);
            it += self.entries[it].end;
            if !skipped {
                all_handled += 1;
            }
            if status == Status::Failed {
                break;
            }
        }
        status
    }

    /// `std::advance(obj.begin(), n)`: `n` siblings on.
    fn nth_child(&self, obj: usize, n: usize) -> usize {
        let mut it = obj + 1;
        for _ in 0..n {
            it += self.entries[it].end;
        }
        it
    }

    /// `_update`.
    fn update_entry(&mut self, pos: usize, elem: &Bson, ctx: Ctx) -> Status {
        match elem {
            Bson::Document(d) => {
                let (mut status, ctx) = self.should_update(pos, elem, ctx, false);
                if status == Status::Updated {
                    status = self.update_obj(pos, d, ctx, None);
                }
                status
            }
            Bson::Array(a) => {
                let (mut status, ctx) = self.should_update(pos, elem, ctx, true);
                if status == Status::Updated {
                    let mut it = pos + 1;
                    let mut end = self.end_of(pos);
                    for sub in a {
                        if status == Status::Failed {
                            break;
                        }
                        if it == end {
                            it = self.insert(pos, it, ARRAY_NAME);
                        }
                        let s = self.update_entry(it, sub, ctx);
                        if s != Status::NoChange {
                            status = s;
                        }
                        end = self.end_of(pos);
                        it += self.entries[it].end;
                    }
                    let _ = end;
                }
                status
            }
            scalar => self.maybe_update_value(pos, scalar, ctx),
        }
    }

    /// `_shouldUpdateObj` / `_shouldUpdateArr`.
    fn should_update(&mut self, pos: usize, elem: &Bson, ctx: Ctx, array: bool) -> (Status, Ctx) {
        if !self.minmax {
            let data = &self.entries[pos].min;
            let want = if array { Kind::Array } else { Kind::Object };
            return if data.kind == Kind::Unset {
                self.set_type(pos, false, array);
                (Status::Updated, ctx)
            } else if data.kind != want {
                (Status::Failed, ctx)
            } else {
                (Status::Updated, ctx)
            };
        }
        let canon = canonical_type(elem);
        let own = if array { Kind::Array } else { Kind::Object };
        let other_canon = if array { 20 } else { 25 };
        let should = |data: &Data, less: bool| {
            data.kind == own
                || data.kind == Kind::Unset
                || (data.kind == Kind::Object && array && cmp_ok(canon - other_canon, less))
                || (data.kind == Kind::Array && !array && cmp_ok(canon - other_canon, less))
                || (data.kind == Kind::Value && cmp_ok(canon - data.canon, less))
        };
        let update_min = ctx.min && should(&self.entries[pos].min, true);
        if update_min {
            self.set_type(pos, false, array);
        }
        let update_max = ctx.max && should(&self.entries[pos].max, false);
        if update_max {
            self.set_type(pos, true, array);
        }
        (
            if update_min || update_max {
                Status::Updated
            } else {
                Status::NoChange
            },
            Ctx {
                min: update_min,
                max: update_max,
            },
        )
    }

    /// `_maybeUpdateValue`.
    fn maybe_update_value(&mut self, pos: usize, elem: &Bson, ctx: Ctx) -> Status {
        if !self.minmax {
            let canon = canonical_type(elem);
            let data = &mut self.entries[pos].min;
            return if data.kind == Kind::Unset {
                data.set_value(elem, false);
                Status::Updated
            } else if data.kind != Kind::Value || canon != data.canon {
                Status::Failed
            } else {
                Status::Updated
            };
        }
        let canon = canonical_type(elem);
        for (flag, max) in [(ctx.min, false), (ctx.max, true)] {
            if !flag {
                continue;
            }
            let data = self.data(pos, max);
            let less = !max;
            let take = match data.kind {
                Kind::Unset => true,
                Kind::Object => cmp_ok(canon - 20, less),
                Kind::Array => cmp_ok(canon - 25, less),
                Kind::Value => {
                    let held = data.held.clone().unwrap_or(Bson::Null);
                    let ord = secantus_core::order::cmp(elem, &held);
                    if less {
                        ord == std::cmp::Ordering::Less
                    } else {
                        ord == std::cmp::Ordering::Greater
                    }
                }
            };
            if take {
                data.set_value(elem, true);
            }
        }
        if ctx.min || ctx.max {
            Status::Updated
        } else {
            Status::NoChange
        }
    }
}

/// `comp(typeComp, 0)` with `std::less` (min) or `std::greater` (max).
fn cmp_ok(diff: i32, less: bool) -> bool {
    if less {
        diff < 0
    } else {
        diff > 0
    }
}

// ---------------------------------------------------------------------------
// One bucket's account.

/// What a bucket's structures hold that is not derived from the bucket's
/// own fields: the summaries (their entries are history) and the tables
/// whose capacity only grows.
#[derive(Debug, Clone)]
pub(crate) struct BucketMem {
    pub(crate) stripe: usize,
    /// The series, for the catalog's per-key structures.
    pub(crate) series: Vec<u8>,
    /// The metadata buffer: charged once per series.
    pub(crate) meta_bytes: i64,
    pub(crate) minmax: FlatStore,
    pub(crate) schema: FlatStore,
    field_names: Table,
    uncommitted: Table,
    builders: Table,
    /// The bucket's charge when last counted.
    pub(crate) counted: i64,
}

/// What the bucket's own fields say.
pub(crate) struct BucketView<'a> {
    pub field_names: &'a HashSet<String>,
    pub uncommitted: &'a HashSet<String>,
    pub columns: Vec<(&'a str, ColumnShape)>,
    /// The bucket sits in the stripe's idle list.
    pub idle: bool,
    /// A batch is open on the bucket (`bucket.batches` holds an entry).
    pub batch_open: bool,
}

impl BucketMem {
    pub(crate) fn new(stripe: usize, series: Vec<u8>, meta_bytes: i64) -> BucketMem {
        BucketMem {
            stripe,
            series,
            meta_bytes,
            minmax: FlatStore::new(true),
            schema: FlatStore::new(false),
            field_names: Table::new(STRING_SLOT, 8),
            uncommitted: Table::new(STRING_SLOT, 8),
            builders: Table::new(BUILDER_SLOT, 16),
            counted: 0,
        }
    }

    /// What the bucket's structures allocate now (the metadata buffer and
    /// the catalog's per-series and per-stripe structures are the
    /// catalog's).
    pub(crate) fn bytes(&mut self, v: &BucketView<'_>) -> i64 {
        let mut n = BUCKET + BYID_NODE + STATE_NODE;
        if v.idle {
            n += IDLE_NODE;
        }
        if v.batch_open {
            n += BATCH_NODE;
        }
        n += self.minmax.bytes() + self.schema.bytes();
        self.field_names.fit(v.field_names.len());
        n += self.field_names.bytes();
        n += v
            .field_names
            .iter()
            .map(|s| string_copy(s.len()))
            .sum::<i64>();
        self.uncommitted.fit(v.uncommitted.len());
        n += self.uncommitted.bytes();
        n += v
            .uncommitted
            .iter()
            .map(|s| string_copy(s.len()))
            .sum::<i64>();
        self.builders.fit(v.columns.len());
        n += self.builders.bytes();
        for (name, shape) in &v.columns {
            n += string_copy(name.len());
            n += column_bytes(shape);
        }
        n
    }
}

/// What a column builder keeps after `intermediate()`.
fn column_bytes(shape: &ColumnShape) -> i64 {
    fn enc(e: &EncShape) -> i64 {
        string_copy(e.prev_len) + e.pending as i64 * if e.wide { PENDING_128 } else { PENDING_64 }
    }
    match shape {
        ColumnShape::Regular { state, retained } => {
            enc(state) + retained.map_or(0, |n| HOLDER + n as i64)
        }
        ColumnShape::Interleaved {
            determining,
            reference_cap,
            reference_shared,
            buffered,
            states,
            ..
        } => {
            if *determining {
                // `referenceSubObj`, the buffered objects (the first shares
                // the reference's buffer until a merge replaces it, a skip
                // holds none) and the vector of them, as many as there are.
                let mut n = HOLDER + *reference_cap as i64 + 16 * buffered.len() as i64;
                for (i, len) in buffered.iter().enumerate() {
                    if i == 0 && *reference_shared {
                        continue;
                    }
                    if *len != 5 {
                        n += HOLDER + *len as i64;
                    }
                }
                n
            } else {
                // The reference, then one stream per scalar path.
                let mut n = HOLDER + *reference_cap as i64 + SUBOBJ_STATE * states.len() as i64;
                for s in states {
                    n += enc(&s.state);
                    n += HOLDER + sub_buffer_cap(s.buffer_len) as i64;
                    n += CONTROL_BLOCK * s.control_blocks as i64;
                }
                n
            }
        }
    }
}

/// A sub-stream's `BufBuilder` capacity for `len` bytes written: 32 to
/// start, growing to the next power of two when it must.
fn sub_buffer_cap(len: usize) -> usize {
    if len <= 32 {
        32
    } else {
        (len + HOLDER as usize).next_power_of_two().max(64) - HOLDER as usize
    }
}

// ---------------------------------------------------------------------------
// The catalog's account.

#[derive(Debug, Clone)]
struct KeyMem {
    open: usize,
    set: Table,
    meta_bytes: i64,
}

#[derive(Debug, Clone)]
struct StripeMem {
    by_id: Table,
    by_key: Table,
    open: usize,
    keys: HashMap<Vec<u8>, KeyMem>,
    /// Archived buckets, and the root leaf's slots (`absl::btree_map`).
    archived: usize,
    archived_slots: usize,
}

impl StripeMem {
    fn new() -> StripeMem {
        StripeMem {
            by_id: Table::new(8, 8),
            by_key: Table::new(8, 8),
            open: 0,
            keys: HashMap::new(),
            archived: 0,
            archived_slots: 0,
        }
    }

    /// The archived buckets' btree: a root leaf growing 1, 2, 4, 5 slots
    /// (48 bytes each after a 16 byte header), then nodes of five.
    fn archived_bytes(&self) -> i64 {
        const SLOT: usize = 48;
        const LEAF: usize = 16;
        if self.archived == 0 {
            return 0;
        }
        if self.archived <= 5 {
            return (LEAF + self.archived_slots.max(1) * SLOT) as i64;
        }
        // An internal node and leaves about three-quarters full.
        let leaves = (self.archived * 4).div_ceil(15);
        (leaves * (LEAF + 5 * SLOT) + LEAF + 5 * SLOT + 6 * 8) as i64
    }
}

/// The catalog's memory account: every structure the stripes, the registry
/// and the statistics keep, apart from what a bucket owns.
#[derive(Debug, Clone)]
pub(crate) struct MemState {
    stripes: Vec<StripeMem>,
    states: Table,
    states_n: usize,
    open_total: usize,
    stats: Table,
    stats_n: usize,
    buckets: i64,
    /// Where the stripes' idle lists' order comes from: each bucket marked
    /// idle takes the next number (`push_front`), the oldest is the lowest.
    idle_clock: u64,
}

impl Default for MemState {
    fn default() -> Self {
        MemState::new()
    }
}

/// `BucketKey::hash`: a hash of the collection and the series. `mongod`'s is
/// `absl::Hash`, whose seed is drawn at random for each process, so which
/// stripe a series lands in differs from run to run and spreads evenly over
/// the 32 stripes. This is SipHash seeded at random for each process, which
/// does the same. (It was FNV-1a: its low bits are a bijection of the
/// input's low bits, so the stripes of series that differ in one byte
/// collided half as often as `mongod`'s, and an idle bucket expired in too
/// few runs: RM-711, CI run 46029656831.)
pub(crate) fn key_hash(uuid: &[u8], series: &[u8]) -> u64 {
    use std::hash::{BuildHasher, Hash, Hasher};
    static SEED: std::sync::OnceLock<std::hash::RandomState> = std::sync::OnceLock::new();
    let mut h = SEED.get_or_init(std::hash::RandomState::new).build_hasher();
    uuid.hash(&mut h);
    series.hash(&mut h);
    h.finish()
}

/// The stripe a series falls in (`key.hash % stripes.size()`).
pub(crate) fn stripe_of(uuid: &[u8], series: &[u8]) -> usize {
    (key_hash(uuid, series) % STRIPES as u64) as usize
}

impl MemState {
    pub(crate) fn new() -> MemState {
        MemState {
            stripes: (0..STRIPES).map(|_| StripeMem::new()).collect(),
            states: Table::new(8, 8),
            states_n: 0,
            open_total: 0,
            stats: Table::new(8, 8),
            stats_n: 0,
            buckets: 0,
            idle_clock: 0,
        }
    }

    /// The place of a bucket just marked idle in its stripe's idle list.
    pub(crate) fn next_idle(&mut self) -> u64 {
        self.idle_clock += 1;
        self.idle_clock
    }

    /// `memoryUsage`.
    pub(crate) fn total(&self) -> i64 {
        let mut n = BASE + self.buckets;
        n += self.stats_n as i64 * (STATS_NODE + STATS_BLOCK) + self.stats.bytes();
        n += self.states.bytes();
        if self.open_total > 0 {
            n += ERA_NODE;
        }
        for s in &self.stripes {
            n += s.by_id.bytes() + s.by_key.bytes() + s.archived_bytes();
            for k in s.keys.values() {
                n += BYKEY_NODE + k.set.bytes() + k.meta_bytes;
            }
        }
        n
    }

    /// A collection's execution statistics were created.
    pub(crate) fn stats_added(&mut self) {
        self.stats_n += 1;
        self.stats.fit(self.stats_n);
    }

    /// A collection's statistics were released (the table keeps its
    /// capacity).
    pub(crate) fn stats_removed(&mut self) {
        self.stats_n = self.stats_n.saturating_sub(1);
    }

    /// A bucket joined the catalog (`allocateBucket`, `loadBucketIntoCatalog`).
    pub(crate) fn bucket_opened(&mut self, m: &BucketMem) {
        let s = &mut self.stripes[m.stripe];
        s.open += 1;
        s.by_id.fit(s.open);
        let fresh = !s.keys.contains_key(&m.series);
        let k = s.keys.entry(m.series.clone()).or_insert_with(|| KeyMem {
            open: 0,
            set: Table::new(8, 8),
            meta_bytes: m.meta_bytes,
        });
        k.open += 1;
        k.set.fit(k.open);
        if fresh {
            let n = s.keys.len();
            s.by_key.fit(n);
        }
        self.states_n += 1;
        self.states.fit(self.states_n);
        self.open_total += 1;
    }

    /// A bucket left the catalog.
    pub(crate) fn bucket_closed(&mut self, m: &BucketMem) {
        let s = &mut self.stripes[m.stripe];
        s.open = s.open.saturating_sub(1);
        if let Some(k) = s.keys.get_mut(&m.series) {
            k.open = k.open.saturating_sub(1);
            if k.open == 0 {
                s.keys.remove(&m.series);
            }
        }
        self.states_n = self.states_n.saturating_sub(1);
        self.open_total = self.open_total.saturating_sub(1);
        self.buckets -= m.counted;
    }

    /// A bucket's own charge changed.
    pub(crate) fn bucket_resized(&mut self, m: &mut BucketMem, now: i64) {
        self.buckets += now - m.counted;
        m.counted = now;
    }

    /// A bucket joined the stripe's archived buckets.
    pub(crate) fn archived_added(&mut self, stripe: usize) {
        let s = &mut self.stripes[stripe];
        s.archived += 1;
        s.archived_slots = match s.archived {
            1 => 1,
            2 => 2,
            3 | 4 => 4,
            _ => 5,
        }
        .max(s.archived_slots);
    }

    /// An archived bucket left the stripe.
    pub(crate) fn archived_removed(&mut self, stripe: usize) {
        let s = &mut self.stripes[stripe];
        s.archived = s.archived.saturating_sub(1);
        if s.archived == 0 {
            s.archived_slots = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{doc, oid::ObjectId};

    #[test]
    fn stripes_spread_as_evenly_as_a_random_hash_does() {
        // 21 series that differ in one byte, as the RM-711 probe opens them,
        // in many collections: once the sixth is open, each new series lands
        // in a stripe already holding one with probability 1 - (31/32)^k,
        // about 4.98 times in the fifteen that follow. FNV-1a gave 2.68.
        let series: Vec<Vec<u8>> = (-1..20)
            .map(|m| crate::bsondoc::to_vec(&doc! {"m": m}).unwrap())
            .collect();
        let runs = 3000u32;
        let mut total = 0u32;
        for run in 0..runs {
            let uuid = [run.to_be_bytes(), run.to_le_bytes(), [7; 4], [9; 4]].concat();
            let mut seen = std::collections::HashSet::new();
            for (k, s) in series.iter().enumerate() {
                if !seen.insert(stripe_of(&uuid, s)) && k >= 6 {
                    total += 1;
                }
            }
        }
        let mean = f64::from(total) / f64::from(runs);
        assert!(
            (4.7..5.3).contains(&mean),
            "stripe collisions per run {mean}, a uniform hash gives about 5"
        );
    }

    #[test]
    fn table_capacities_and_bytes() {
        let mut t = Table::new(8, 8);
        assert_eq!(t.bytes(), 0);
        t.fit(1);
        assert_eq!(t.bytes(), 0, "one element stays inline");
        t.fit(2);
        assert_eq!(t.bytes(), 56);
        t.fit(4);
        assert_eq!(t.bytes(), 88);
        let mut s = Table::new(STRING_SLOT, 8);
        s.fit(1);
        assert_eq!(s.bytes(), 72);
        s.fit(2);
        assert_eq!(s.bytes(), 152);
        let mut b = Table::new(BUILDER_SLOT, 16);
        b.fit(1);
        assert_eq!(b.bytes(), 368);
        b.fit(3);
        assert_eq!(b.bytes(), 32 + 3 * 336);
    }

    #[test]
    fn metadata_buffer_sizes_follow_the_builder() {
        // Measured on Linux mongod 8.2.11 (`meta str N`).
        let small = Bson::String("a".into());
        assert_eq!(metadata_bytes("m", Some(&small)), 528);
        assert_eq!(metadata_bytes("m", Some(&Bson::Int32(7))), 528);
        assert_eq!(
            metadata_bytes("m", Some(&Bson::String("s".repeat(400)))),
            1024
        );
        assert_eq!(
            metadata_bytes("m", Some(&Bson::String("s".repeat(480)))),
            1024
        );
        assert_eq!(
            metadata_bytes("m", Some(&Bson::String("s".repeat(497)))),
            2048
        );
        assert_eq!(
            metadata_bytes("m", Some(&Bson::String("s".repeat(1000)))),
            2048
        );
        assert_eq!(
            metadata_bytes("m", Some(&Bson::String("s".repeat(2000)))),
            4096
        );
        assert_eq!(metadata_bytes("m", None), 0);
        let obj = Bson::Document(doc! {"a": 1, "b": "x".repeat(600)});
        assert_eq!(metadata_bytes("m", Some(&obj)), 2048);
    }

    /// `{t, _id}` into a bucket whose control document set `t`: 3 entries
    /// in each summary.
    #[test]
    fn summaries_of_one_measurement() {
        let mut mm = FlatStore::new(true);
        let mut sc = FlatStore::new(false);
        let when = Bson::DateTime(bson::DateTime::from_millis(0));
        assert!(mm.update(&doc! {"t": when.clone()}, None));
        assert!(mm.update(
            &doc! {"_id": ObjectId::new(), "t": when.clone(), "m": "a"},
            Some("m")
        ));
        assert!(sc.update(
            &doc! {"_id": ObjectId::new(), "t": when, "m": "a"},
            Some("m")
        ));
        // 4 entries of 216 and of 112, `t`'s buffers 10 + 10, `_id`'s 14 + 14.
        assert_eq!(mm.bytes(), 4 * 216 + 48);
        assert_eq!(sc.bytes(), 4 * 112);
    }

    /// An empty catalog holds its 32 stripes and their pointers; a
    /// collection's statistics and its second collection's table slot are
    /// charged as measured on Linux mongod (`two collections`: +56).
    #[test]
    fn the_catalog_charges_stripes_statistics_and_tables() {
        let mut m = MemState::new();
        assert_eq!(m.total(), 32 * (256 + 16));
        m.stats_added();
        assert_eq!(m.total(), 8704 + 32 + 328);
        m.stats_added();
        assert_eq!(m.total(), 8704 + 2 * (32 + 328) + 56);
    }

    #[test]
    fn long_field_names_are_strings_on_the_heap() {
        let mut mm = FlatStore::new(true);
        let name = "f".repeat(20);
        let mut d = Document::new();
        d.insert(name, 1);
        assert!(mm.update(&d, None));
        // Two entries (capacity 2): the name assigned to a string (31), the
        // value's min and max buffers (6 + 6).
        assert_eq!(mm.bytes(), 2 * 216 + 31 + 12);
    }
}
