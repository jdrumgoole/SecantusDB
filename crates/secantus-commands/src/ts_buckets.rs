//! What a time-series collection's buckets are made of, and how a filter
//! and an index key read on them.
//!
//! `mongod` stores a time-series collection as bucket documents in
//! `system.buckets.<name>` and presents the collection as a view that unpacks
//! them; so does this server (`ts_store`, `bucket_catalog`). This module
//! holds the pieces both sides of that use: the collection's layout (its
//! time and meta fields, its bucket span and rounding), the schema a bucket
//! keeps (`Schema`), the running minimum and maximum (`fold`), the
//! bucket-level filter's comparisons (`bucket_matches`), the streaming
//! bucket scan (`BucketScan`), `system.views`' definitions, the bucket
//! validator, and the translation of index keys between the collection and
//! its buckets. Shapes were read from `mongod` 8.2.11.

use std::cmp::Ordering;

use bson::spec::BinarySubtype;
use bson::{Binary, Bson, Document};

use crate::storage::{IndexEntry, IndexFrom, RecordId};
use crate::CommandError;

/// The storage namespace of a time-series collection's buckets.
pub const BUCKETS_PREFIX: &str = "system.buckets.";

/// The stage a read of `system.buckets.<name>` runs: it takes the collection's
/// name and yields its bucket documents. Not a stage a client can name.
pub const BUCKETS_STAGE: &str = "$_secantusBuckets";

/// How a collection's measurements fall into buckets.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Layout {
    pub time: String,
    pub meta: Option<String>,
    pub span_ms: i64,
    pub rounding_ms: i64,
}

impl Layout {
    /// From a collection's stored options; `None` for any other collection.
    pub(crate) fn of(options: &Document) -> Option<Layout> {
        let ts = options.get_document("timeseries").ok()?;
        let whole = |field: &str| ts.get(field).and_then(crate::util::as_i64);
        let granularity = ts.get_str("granularity").unwrap_or("seconds");
        let (span, rounding) = match granularity {
            "minutes" => (86400, 3600),
            "hours" => (2592000, 86400),
            _ => (3600, 60),
        };
        let span = whole("bucketMaxSpanSeconds").unwrap_or(span).max(1);
        // A custom span is stored with its rounding; a granularity implies one.
        let rounding = whole("bucketRoundingSeconds")
            .unwrap_or(if ts.contains_key("granularity") {
                rounding
            } else {
                span
            })
            .max(1);
        Some(Layout {
            time: ts.get_str("timeField").ok()?.to_string(),
            meta: ts.get_str("metaField").ok().map(str::to_string),
            span_ms: span.saturating_mul(1000),
            rounding_ms: rounding.saturating_mul(1000),
        })
    }

    pub(crate) fn time_of(&self, doc: &Document) -> i64 {
        match doc.get(&self.time) {
            Some(Bson::DateTime(t)) => t.timestamp_millis(),
            _ => 0,
        }
    }

    pub(crate) fn meta_of(&self, doc: &Document) -> Option<Bson> {
        let field = self.meta.as_ref()?;
        doc.get(field).map(sorted_keys)
    }
}

/// `v` with the keys of every document in it sorted, as `mongod` keeps a
/// meta value: `{b: 1, a: 2}` and `{a: 2, b: 1}` are one series.
pub(crate) fn sorted_keys(v: &Bson) -> Bson {
    match v {
        Bson::Document(d) => {
            let mut keys: Vec<&String> = d.keys().collect();
            keys.sort();
            let mut out = Document::new();
            for k in keys {
                out.insert(k.clone(), sorted_keys(&d[k]));
            }
            Bson::Document(out)
        }
        Bson::Array(a) => Bson::Array(a.iter().map(sorted_keys).collect()),
        other => other.clone(),
    }
}

/// What a bucket's measurements have held at each path, as `mongod`'s
/// bucket catalog tracks it (`timeseries::bucket_catalog::Schema`, 8.2): a
/// scalar by its canonical type (every number is one type, so is every
/// string), a document by its fields, an array by position. A measurement
/// that holds another kind at a path the bucket has seen -- an int where a
/// string was, a scalar where a document was -- closes the bucket for good
/// (`RolloverReason::kSchemaChange`, a hard close). The meta field is left
/// out; a path a measurement leaves out changes nothing.

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) enum Schema {
    #[default]
    Unset,
    Value(i32),
    Object(Vec<(String, Schema)>),
    Array(Vec<Schema>),
}

/// `canonicalizeBSONType`: the types `mongod` orders as one.
pub(crate) fn canonical_type(v: &Bson) -> i32 {
    match v {
        Bson::MinKey => -1,
        Bson::Undefined => 0,
        Bson::Null => 5,
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => 10,
        Bson::String(_) | Bson::Symbol(_) => 15,
        Bson::Document(_) => 20,
        Bson::Array(_) => 25,
        Bson::Binary(_) => 30,
        Bson::ObjectId(_) => 35,
        Bson::Boolean(_) => 40,
        Bson::DateTime(_) => 45,
        Bson::Timestamp(_) => 47,
        Bson::RegularExpression(_) => 50,
        Bson::DbPointer(_) => 55,
        Bson::JavaScriptCode(_) => 60,
        Bson::JavaScriptCodeWithScope(_) => 65,
        Bson::MaxKey => 127,
    }
}

impl Schema {
    /// Take `v` in; `false` when its kind conflicts with what is held (the
    /// schema is then part-updated: the caller restores it).
    fn update(&mut self, v: &Bson) -> bool {
        match v {
            Bson::Document(d) => {
                if matches!(self, Schema::Unset) {
                    *self = Schema::Object(Vec::new());
                }
                let Schema::Object(fields) = self else {
                    return false;
                };
                d.iter().all(|(k, v)| Self::field(fields, k).update(v))
            }
            Bson::Array(a) => {
                if matches!(self, Schema::Unset) {
                    *self = Schema::Array(Vec::new());
                }
                let Schema::Array(items) = self else {
                    return false;
                };
                a.iter().enumerate().all(|(i, v)| {
                    if items.len() <= i {
                        items.push(Schema::Unset);
                    }
                    items[i].update(v)
                })
            }
            scalar => {
                let t = canonical_type(scalar);
                match self {
                    Schema::Unset => {
                        *self = Schema::Value(t);
                        true
                    }
                    Schema::Value(held) => *held == t,
                    _ => false,
                }
            }
        }
    }

    fn field<'a>(fields: &'a mut Vec<(String, Schema)>, name: &str) -> &'a mut Schema {
        let at = match fields.iter().position(|(k, _)| k == name) {
            Some(at) => at,
            None => {
                fields.push((name.to_string(), Schema::Unset));
                fields.len() - 1
            }
        };
        &mut fields[at].1
    }

    /// Take a document in whole: `Schema::parseFromBSON` takes a bucket's
    /// `control.min` and `control.max` this way.
    pub(crate) fn take_all(&mut self, doc: &Document) -> bool {
        if matches!(self, Schema::Unset) {
            *self = Schema::Object(Vec::new());
        }
        let Schema::Object(fields) = self else {
            return false;
        };
        doc.iter().all(|(k, v)| Self::field(fields, k).update(v))
    }

    /// Take a measurement in, its meta field left out.
    pub(crate) fn take(&mut self, doc: &Document, layout: &Layout) -> bool {
        if matches!(self, Schema::Unset) {
            *self = Schema::Object(Vec::new());
        }
        let Schema::Object(fields) = self else {
            return false;
        };
        doc.iter()
            .filter(|(k, _)| layout.meta.as_deref() != Some(k.as_str()))
            .all(|(k, v)| Self::field(fields, k).update(v))
    }
}

/// Fold `v` into a bucket's running minimum (or maximum) for one field.
/// Documents fold field by field and arrays element by element, as `mongod`
/// computes `control.min` / `control.max`; anything else is compared whole.
pub(crate) fn fold(acc: Option<Bson>, v: &Bson, want_max: bool) -> Bson {
    let Some(acc) = acc else {
        return v.clone();
    };
    match (acc, v) {
        (Bson::Document(mut a), Bson::Document(b)) => {
            for (k, bv) in b {
                let folded = fold(a.remove(k), bv, want_max);
                a.insert(k.clone(), folded);
            }
            Bson::Document(a)
        }
        (Bson::Array(a), Bson::Array(b)) => {
            let mut a = a.into_iter();
            let mut out = Vec::new();
            for bv in b {
                out.push(fold(a.next(), bv, want_max));
            }
            out.extend(a);
            Bson::Array(out)
        }
        (acc, v) => {
            let wanted = if want_max {
                Ordering::Greater
            } else {
                Ordering::Less
            };
            if secantus_core::order::cmp(v, &acc) == wanted {
                v.clone()
            } else {
                acc
            }
        }
    }
}

/// The collection `coll` names when it is the bucket namespace of a
/// time-series collection: `(name, options)`.
pub(crate) fn bucketed(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> Option<(String, Document)> {
    let name = coll.strip_prefix(BUCKETS_PREFIX)?;
    let options = storage.get_collection_options(db, name).ok()?;
    options
        .contains_key("timeseries")
        .then(|| (name.to_string(), options))
}

/// The bucket documents of the time-series collection `db.coll`, as they
/// are stored, in `_id` order.
pub(crate) fn of_collection(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> Result<Vec<Document>, CommandError> {
    crate::ts_store::stored_buckets(storage, db, coll)
}

/// What a read of `db.coll` returns: `found` as read. A time-series
/// collection's reads (`ts_store::find`) already give its measurements in
/// `mongod`'s scan order, each in `mongod`'s field order.
pub(crate) fn as_read(
    _storage: &dyn crate::storage::Storage,
    _db: &str,
    _coll: &str,
    found: Vec<Vec<u8>>,
    _sorted: bool,
) -> Result<Vec<Vec<u8>>, CommandError> {
    Ok(found)
}

/// A read of a time-series collection bucket by bucket, as `mongod`'s
/// cursor stage reads `system.buckets.<name>` under `$_internalUnpackBucket`
/// (measured 8.2.11, 2026-11-09): it buffers bucket documents, those the
/// bucket-level filter passes, and the stage after it unpacks each into its
/// measurements as the cursor reaches it. A refill reads the buckets as
/// they are then and goes on after the last bucket buffered, in `_id`
/// order, which is the order `system.buckets` holds them in.
pub(crate) struct BucketScan {
    db: String,
    coll: String,
    layout: Layout,
    /// The bucket-level filter (`timeseries::bucket_filter`).
    filter: Document,
    /// The `_id` of the last bucket buffered.
    after: Option<[u8; 12]>,
    /// The index scans over the buckets the scan reads in the order of: one
    /// for an IXSCAN, the children of an `OR` in turn for a union (RM-701).
    indexes: Vec<IndexPosition>,
    /// The scan [`Self::indexes`] is reading.
    at: usize,
    /// The buckets it has met (a multikey index holds a bucket more than
    /// once, and an `OR` returns a bucket once from its children).
    seen: std::collections::HashSet<RecordId>,
    /// The buckets a trial between several plans handed over before the
    /// first read: the cursor stage's first buffer holds them all (RM-810).
    trial: usize,
    /// The buckets passing the filter that earlier buffers took.
    taken: usize,
}

/// The buckets the next buffer takes when `taken` are read and a trial left
/// `trial` buffered ([`BucketScan::refill_size`]).
fn refill(taken: usize, trial: usize) -> usize {
    let (mut reach, mut step) = (32, 32);
    while reach <= taken {
        step *= 2;
        reach += step;
    }
    let want = reach - taken;
    if taken == 0 {
        want.max(trial)
    } else {
        want
    }
}

/// One index scan of a [`BucketScan`], and the last entry it reached.
struct IndexPosition {
    name: String,
    last: Option<(Vec<u8>, RecordId)>,
    /// The scan's bounds: per key field, the intervals an entry's value
    /// lies in; none for a scan of the whole index.
    bounds: Option<Vec<Vec<EncodedInterval>>>,
    /// What the scan's stage rechecks on each bucket it fetches.
    filters: Vec<Document>,
}

/// An interval of one key field, as the index's sort-key encodings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EncodedInterval {
    pub lo: Vec<u8>,
    pub lo_incl: bool,
    pub hi: Vec<u8>,
    pub hi_incl: bool,
}

/// One child of an `OR` over indexes of the buckets (mongod's plan for an
/// `$or` whose every branch an index answers): the index, its bounds, and
/// what is rechecked on a bucket it returns.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UnionScan {
    pub index: String,
    pub bounds: Vec<Vec<EncodedInterval>>,
    pub filters: Vec<Document>,
}

/// What one refill of a [`BucketScan`] buffered.
pub(crate) struct Buffered {
    /// The buffered buckets' measurements, in scan order.
    pub docs: Vec<Vec<u8>>,
    /// The scan reached the last bucket.
    pub exhausted: bool,
    /// Bucket documents examined.
    pub examined: i64,
    /// The buckets that passed the filter and were unpacked.
    pub buckets: usize,
}

impl BucketScan {
    /// A scan of `db.coll`, when it is a time-series collection; `filter`
    /// is the measurement-level query.
    pub(crate) fn of(
        storage: &dyn crate::storage::Storage,
        db: &str,
        coll: &str,
        _ns: &str,
        filter: &Document,
    ) -> Result<Option<BucketScan>, CommandError> {
        let Some(layout) = storage
            .get_collection_options(db, coll)
            .ok()
            .as_ref()
            .and_then(Layout::of)
        else {
            return Ok(None);
        };
        let filter = crate::timeseries::bucket_filter(
            filter,
            &layout.time,
            layout.meta.as_deref(),
            layout.span_ms / 1000,
        );
        Ok(Some(BucketScan {
            db: db.to_string(),
            coll: coll.to_string(),
            layout,
            filter,
            after: None,
            indexes: Vec::new(),
            at: 0,
            seen: Default::default(),
            trial: 0,
            taken: 0,
        }))
    }

    /// This scan read in the order of index `name` over the buckets, as
    /// mongod's IXSCAN of it under `$_internalUnpackBucket`; `None` when
    /// the buckets hold no such index or the store cannot stream it.
    pub(crate) fn through_index(
        mut self,
        storage: &dyn crate::storage::Storage,
        name: &str,
    ) -> Result<Option<BucketScan>, crate::storage::StorageError> {
        let ns = crate::ts_store::buckets_ns(&self.coll);
        if !storage
            .list_indexes(&self.db, &ns)?
            .iter()
            .any(|ix| ix.get_str("name") == Ok(name))
        {
            return Ok(None);
        }
        if storage
            .index_entries_from(&self.db, &ns, name, IndexFrom::Edge, None, 0, false)?
            .is_none()
        {
            return Ok(None);
        }
        self.indexes = vec![IndexPosition {
            name: name.to_string(),
            last: None,
            bounds: None,
            filters: Vec::new(),
        }];
        Ok(Some(self))
    }

    /// This scan read as mongod's `OR` of the index scans `children`, one
    /// after the other, each bucket once (RM-701); `None` when the buckets
    /// hold no such index or the store cannot stream it.
    pub(crate) fn through_union(
        mut self,
        storage: &dyn crate::storage::Storage,
        children: &[UnionScan],
    ) -> Result<Option<BucketScan>, crate::storage::StorageError> {
        let ns = crate::ts_store::buckets_ns(&self.coll);
        let listed = storage.list_indexes(&self.db, &ns)?;
        let mut positions = Vec::with_capacity(children.len());
        for child in children {
            if !listed
                .iter()
                .any(|ix| ix.get_str("name") == Ok(child.index.as_str()))
                || storage
                    .index_entries_from(
                        &self.db,
                        &ns,
                        &child.index,
                        IndexFrom::Edge,
                        None,
                        0,
                        false,
                    )?
                    .is_none()
            {
                return Ok(None);
            }
            positions.push(IndexPosition {
                name: child.index.clone(),
                last: None,
                bounds: Some(child.bounds.clone()),
                filters: child.filters.clone(),
            });
        }
        self.indexes = positions;
        Ok(Some(self))
    }

    /// The bucket-level filter the scan applies.
    pub(crate) fn bucket_filter(&self) -> &Document {
        &self.filter
    }

    /// The bucket-level filter names the meta field, which an index over
    /// the buckets can answer.
    pub(crate) fn filters_meta(&self) -> bool {
        fn names_meta(d: &Document) -> bool {
            d.iter().any(|(k, v)| {
                k == "meta"
                    || k.starts_with("meta.")
                    || match v {
                        Bson::Array(items) => {
                            items.iter().filter_map(Bson::as_document).any(names_meta)
                        }
                        _ => false,
                    }
            })
        }
        names_meta(&self.filter)
    }

    /// A trial between several plans handed `buckets` over before this scan
    /// read any (RM-810).
    pub(crate) fn buffer_trial(&mut self, buckets: usize) {
        self.trial = buckets;
    }

    /// How many buckets the cursor stage's next buffer takes: each buffer
    /// reaches the next of 32, 96, 224, ... buckets in all (the sizes 32, 64,
    /// 128, ... that doubling gives, measured 8.2.11, 2026-11-28), and the
    /// first holds a trial's whole buffer where that is more (101 buckets,
    /// then the buffer that reaches 224).
    pub(crate) fn refill_size(&self) -> usize {
        refill(self.taken, self.trial)
    }

    /// The next `want` buckets the filter passes, stopping early once they
    /// come to more than `max_bytes`, unpacked.
    pub(crate) fn next_buffer(
        &mut self,
        storage: &dyn crate::storage::Storage,
        want: usize,
        max_bytes: usize,
    ) -> Result<Buffered, CommandError> {
        if !self.indexes.is_empty() {
            return self.next_indexed(storage, want, max_bytes);
        }
        let rows = crate::ts_store::stored_bucket_bytes(storage, &self.db, &self.coll)?;
        let mut out = Buffered {
            docs: Vec::new(),
            exhausted: true,
            examined: 0,
            buckets: 0,
        };
        let (mut count, mut bytes) = (0usize, 0usize);
        for raw in rows {
            let document = secantus_core::bsondoc::from_bytes(&raw[..])
                .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))?;
            let Some(Bson::ObjectId(id)) = document.get("_id") else {
                continue;
            };
            let key = id.bytes();
            if self.after.is_some_and(|after| key <= after) {
                continue;
            }
            if count >= want || (count > 0 && bytes > max_bytes) {
                out.exhausted = false;
                break;
            }
            out.examined += 1;
            self.after = Some(key);
            if !self.filter.is_empty() && !bucket_matches(&document, &self.filter)? {
                continue;
            }
            bytes += raw.len();
            count += 1;
            for m in crate::ts_store::unpack(&document, &self.layout)? {
                out.docs.push(
                    crate::bsondoc::to_vec(&m)
                        .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))?,
                );
            }
        }
        out.buckets = count;
        self.taken += count;
        Ok(out)
    }
}

impl BucketScan {
    /// [`Self::next_buffer`] through the indexes: the entries of each from
    /// the last one reached, in turn, each bucket fetched as it is now.
    fn next_indexed(
        &mut self,
        storage: &dyn crate::storage::Storage,
        want: usize,
        max_bytes: usize,
    ) -> Result<Buffered, CommandError> {
        let ns = crate::ts_store::buckets_ns(&self.coll);
        let err = |e| crate::util::command_error(e);
        let mut out = Buffered {
            docs: Vec::new(),
            exhausted: false,
            examined: 0,
            buckets: 0,
        };
        let (mut count, mut bytes) = (0usize, 0usize);
        const CHUNK: usize = 64;
        loop {
            if count >= want || (count > 0 && bytes > max_bytes) {
                out.buckets = count;
                self.taken += count;
                return Ok(out);
            }
            let Some(pos) = self.indexes.get(self.at) else {
                out.exhausted = true;
                out.buckets = count;
                self.taken += count;
                return Ok(out);
            };
            let (from, after) = match &pos.last {
                Some((k, rid)) => (IndexFrom::Resume(k), Some(rid.clone())),
                None => (IndexFrom::Edge, None),
            };
            let entries = storage
                .index_entries_from(&self.db, &ns, &pos.name, from, after, CHUNK, false)
                .map_err(err)?
                .unwrap_or_default();
            if entries.is_empty() {
                // This scan is done: an `OR` goes on with the next child.
                self.at += 1;
                continue;
            }
            let mut stopped = false;
            for IndexEntry { raw, resume, rid } in entries {
                if count >= want || (count > 0 && bytes > max_bytes) {
                    stopped = true;
                    break;
                }
                let pos = &mut self.indexes[self.at];
                pos.last = Some((resume, rid.clone()));
                // An entry outside the scan's bounds is not read.
                if pos.bounds.as_ref().is_some_and(|b| !within(&raw, b)) {
                    continue;
                }
                // A scan alone returns a multikey index's bucket once; an
                // `OR` returns a bucket from its children once, after the
                // child's own recheck.
                let single = self.indexes.len() == 1;
                if single && !self.seen.insert(rid.clone()) {
                    continue;
                }
                let Some(Some(raw)) = storage
                    .records_by_id(&self.db, &ns, std::slice::from_ref(&rid))
                    .map_err(err)?
                    .and_then(|mut v| v.pop())
                else {
                    continue;
                };
                out.examined += 1;
                let document = secantus_core::bsondoc::from_bytes(&raw[..])
                    .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))?;
                let mut passes = self.filter.is_empty() || bucket_matches(&document, &self.filter)?;
                for f in &self.indexes[self.at].filters {
                    passes = passes && bucket_matches(&document, f)?;
                }
                if !passes || (!single && !self.seen.insert(rid)) {
                    continue;
                }
                bytes += raw.len();
                count += 1;
                for m in crate::ts_store::unpack(&document, &self.layout)? {
                    out.docs.push(
                        crate::bsondoc::to_vec(&m)
                            .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))?,
                    );
                }
            }
            if stopped {
                out.buckets = count;
                self.taken += count;
                return Ok(out);
            }
        }
    }
}

/// Whether an index entry (`raw`: its key fields' ascending sort-key
/// encodings joined by the compound separator) lies within `bounds`, the
/// intervals each key field's value must fall in.
fn within(raw: &[u8], bounds: &[Vec<EncodedInterval>]) -> bool {
    let Some(fields) = secantus_core::sortkey::split_directed(raw, &vec![1; bounds.len()]) else {
        // Not a key this reads: leave the entry to the recheck.
        return true;
    };
    fields.iter().zip(bounds).all(|(value, intervals)| {
        let value: &[u8] = value;
        intervals.iter().any(|i| {
            let above = match value.cmp(&i.lo[..]) {
                Ordering::Greater => true,
                Ordering::Equal => i.lo_incl,
                Ordering::Less => false,
            };
            above
                && match value.cmp(&i.hi[..]) {
                    Ordering::Less => true,
                    Ordering::Equal => i.hi_incl,
                    Ordering::Greater => false,
                }
        })
    })
}

/// Whether bucket document `bucket` passes the bucket-level filter. The
/// `$_internalExpr*` comparisons are `mongod`'s: no type bracketing, a
/// missing field below everything, and an array met on the path always
/// matching (`InternalExprComparisonMatchExpression`); the rest is an
/// ordinary query.
pub(crate) fn bucket_matches(bucket: &Document, filter: &Document) -> Result<bool, CommandError> {
    for (key, value) in filter {
        let ok = match (key.as_str(), value) {
            ("$and", Bson::Array(items)) => {
                let mut all = true;
                for item in items.iter().filter_map(Bson::as_document) {
                    if !bucket_matches(bucket, item)? {
                        all = false;
                        break;
                    }
                }
                all
            }
            ("$or", Bson::Array(items)) => {
                let mut any = false;
                for item in items.iter().filter_map(Bson::as_document) {
                    if bucket_matches(bucket, item)? {
                        any = true;
                        break;
                    }
                }
                any
            }
            (path, Bson::Document(ops))
                if ops.keys().any(|op| op.starts_with("$_internalExpr")) =>
            {
                ops.iter().all(|(op, arg)| {
                    secantus_core::query::internal_expr_matches(bucket, path, op, arg)
                })
            }
            _ => {
                let mut one = Document::new();
                one.insert(key.clone(), value.clone());
                secantus_core::query::matches(bucket, &one, &secantus_core::vars::Vars::new(), None)
                    .map_err(|f| match f {
                        secantus_core::fallback::Fallback::Mongo { code, message, .. } => {
                            CommandError::new(code, crate::util::error_code_name(code), message)
                        }
                        _ => CommandError::new(2, "BadValue", "unsupported bucket filter"),
                    })?
            }
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A stable UUID for a namespace that is derived rather than stored, from
/// the identity of what it is derived from.
pub(crate) fn derived_uuid(seed: &[u8], salt: u8) -> Bson {
    let mut bytes = [0u8; 16];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325 ^ u64::from(salt);
    for (i, slot) in bytes.iter_mut().enumerate() {
        for b in seed {
            hash = (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash = (hash ^ i as u64).wrapping_mul(0x0000_0100_0000_01b3);
        *slot = (hash >> 32) as u8;
    }
    // Version 4, variant 1, as every other collection UUID here reads.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Bson::Binary(Binary {
        subtype: BinarySubtype::Uuid,
        bytes: bytes.to_vec(),
    })
}

/// The stage a read of `system.views` runs: it yields the database's view
/// definitions as `mongod` stores them. Not a stage a client can name.
pub const VIEWS_STAGE: &str = "$_secantusViews";

/// The documents of `<db>.system.views`: one per view, and one per
/// time-series collection, which `mongod` keeps as a view over its buckets.
pub(crate) fn view_definitions(
    storage: &dyn crate::storage::Storage,
    db: &str,
) -> Result<Vec<Document>, CommandError> {
    let mut out = Vec::new();
    for name in storage
        .list_collections(db)
        .map_err(crate::util::command_error)?
    {
        let options = storage
            .get_collection_options(db, &name)
            .map_err(crate::util::command_error)?;
        if let Some(entry) = secantus_core::viewdefs::view_definition(db, &name, &options) {
            out.push(entry);
        }
    }
    Ok(out)
}

/// The stage `mongod`'s view of a time-series collection with the options
/// `ts` unpacks its buckets with.
pub(crate) fn unpack_stage(ts: &Document) -> Document {
    secantus_core::viewdefs::unpack_stage(ts)
}

/// The validator `mongod` gives `system.buckets.<name>`.
pub(crate) fn validator(time: &str) -> Document {
    secantus_core::viewdefs::buckets_validator(time)
}

/// An index key on the collection as `mongod` keeps it on the buckets: the
/// meta field is `meta`, and a measurement field is the pair of bounds it
/// lies between.
pub(crate) fn bucket_key(key: &Document, layout: &Layout) -> Document {
    let mut out = Document::new();
    for (field, direction) in key {
        if let Some(rest) = meta_path(field, layout) {
            out.insert(format!("meta{rest}"), direction.clone());
            continue;
        }
        let descending = crate::util::as_i64(direction).is_some_and(|d| d < 0);
        let (first, second) = if descending {
            ("max", "min")
        } else {
            ("min", "max")
        };
        out.insert(format!("control.{first}.{field}"), direction.clone());
        out.insert(format!("control.{second}.{field}"), direction.clone());
    }
    out
}

/// `field` relative to the meta field (`""` for the field itself, `.a` for
/// `<meta>.a`), when it is in it.
fn meta_path<'a>(field: &'a str, layout: &Layout) -> Option<&'a str> {
    let meta = layout.meta.as_deref()?;
    let rest = field.strip_prefix(meta)?;
    (rest.is_empty() || rest.starts_with('.')).then_some(rest)
}

/// The collection's own key for an index key given on the buckets, or `None`
/// when it is not one `mongod` can present on the collection.
pub(crate) fn measurement_key(key: &Document, layout: &Layout) -> Option<Document> {
    let mut out = Document::new();
    let mut fields = key.iter().peekable();
    while let Some((field, direction)) = fields.next() {
        if let Some(rest) = field.strip_prefix("meta") {
            if rest.is_empty() || rest.starts_with('.') {
                out.insert(
                    format!("{}{rest}", layout.meta.as_deref()?),
                    direction.clone(),
                );
                continue;
            }
        }
        // A bound pair, `control.min.f` then `control.max.f` (or reversed
        // for a descending key), is the one field `f`.
        let name = field
            .strip_prefix("control.min.")
            .or_else(|| field.strip_prefix("control.max."))?;
        let (pair, _) = fields.next()?;
        let pair_name = pair
            .strip_prefix("control.min.")
            .or_else(|| pair.strip_prefix("control.max."))?;
        if pair_name != name || pair == field {
            return None;
        }
        out.insert(name.to_string(), direction.clone());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn layout() -> Layout {
        Layout::of(&doc! {"timeseries": {"timeField": "t", "metaField": "m",
        "granularity": "seconds", "bucketMaxSpanSeconds": 3600}})
        .unwrap()
    }

    /// An index entry is read by a scan when each key field lies in one of
    /// its intervals (RM-701).
    /// The cursor stage's buffers reach 32, 96, 224 buckets; a trial's
    /// results are the first one whole (RM-810).
    #[test]
    fn the_buffers_reach_the_next_of_32_96_224_buckets() {
        assert_eq!(refill(0, 0), 32);
        assert_eq!(refill(32, 0), 64);
        assert_eq!(refill(96, 0), 128);
        assert_eq!(refill(0, 50), 50);
        assert_eq!(refill(0, 20), 32);
        assert_eq!(refill(101, 0), 123);
    }

    #[test]
    fn an_entry_is_within_the_bounds_of_its_scan() {
        let key = |v: i32, w: i32| {
            let mut raw = secantus_core::sortkey::encode_value(&Bson::Int32(v), None).unwrap();
            raw.extend_from_slice(secantus_core::sortkey::COMPOUND_SEP);
            raw.extend(secantus_core::sortkey::encode_value(&Bson::Int32(w), None).unwrap());
            raw
        };
        let enc = |v: Bson| secantus_core::sortkey::encode_value(&v, None).unwrap();
        let bounds = vec![
            vec![EncodedInterval {
                lo: enc(Bson::MinKey),
                lo_incl: true,
                hi: enc(Bson::Int32(100)),
                hi_incl: false,
            }],
            vec![EncodedInterval {
                lo: enc(Bson::MinKey),
                lo_incl: true,
                hi: enc(Bson::MaxKey),
                hi_incl: true,
            }],
        ];
        assert!(within(&key(5, 3000), &bounds));
        assert!(within(&key(99, 0), &bounds));
        assert!(!within(&key(100, 0), &bounds));
        assert!(!within(&key(4000, 1), &bounds));
    }

    /// The bucket-level filter's `$_internalExpr*` comparisons: across
    /// types by BSON order, a missing field below everything, an array on
    /// the path matching whatever is asked (RM-348).
    #[test]
    fn a_bucket_filter_compares_as_mongod_does() {
        let bucket = doc! {
            "control": {"min": {"v": 3, "s": "a", "a": [1, 9]}, "max": {"v": 7, "s": "z"}},
            "meta": 4,
        };
        let passes = |f: Document| bucket_matches(&bucket, &f).unwrap();
        assert!(passes(doc! {"control.max.v": {"$_internalExprGte": 7}}));
        assert!(!passes(doc! {"control.max.v": {"$_internalExprGt": 7}}));
        assert!(passes(doc! {"control.min.v": {"$_internalExprLte": 3}}));
        // A number sorts below a string, whatever its value.
        assert!(passes(doc! {"control.max.v": {"$_internalExprLt": "0"}}));
        assert!(passes(doc! {"control.min.s": {"$_internalExprGt": 1000}}));
        // Missing: below everything.
        assert!(!passes(doc! {"control.max.w": {"$_internalExprGte": 0}}));
        assert!(passes(doc! {"control.min.w": {"$_internalExprLt": 0}}));
        // An array on the path matches.
        assert!(passes(doc! {"control.min.a": {"$_internalExprGt": 100}}));
        assert!(passes(doc! {"$or": [
            {"control.max.v": {"$_internalExprGt": 100}},
            {"meta": 4},
        ]}));
        assert!(!passes(doc! {"$and": [
            {"control.max.v": {"$_internalExprGte": 0}},
            {"meta": 5},
        ]}));
    }

    #[test]
    fn index_keys_translate_both_ways() {
        let l = layout();
        let on_buckets = bucket_key(&doc! {"m": 1, "t": 1}, &l);
        assert_eq!(
            on_buckets,
            doc! {"meta": 1, "control.min.t": 1, "control.max.t": 1}
        );
        assert_eq!(
            measurement_key(&on_buckets, &l),
            Some(doc! {"m": 1, "t": 1})
        );
        assert_eq!(
            bucket_key(&doc! {"m.x": -1, "v": -1}, &l),
            doc! {"meta.x": -1, "control.max.v": -1, "control.min.v": -1}
        );
        assert_eq!(measurement_key(&doc! {"meta": 1}, &l), Some(doc! {"m": 1}));
        assert_eq!(measurement_key(&doc! {"control.min.v": 1}, &l), None);
        assert_eq!(measurement_key(&doc! {"data.v": 1}, &l), None);
    }
}
