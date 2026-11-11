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
use bson::{doc, Binary, Bson, Document};

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
fn canonical_type(v: &Bson) -> i32 {
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
}

/// What one refill of a [`BucketScan`] buffered.
pub(crate) struct Buffered {
    /// The buffered buckets' measurements, in scan order.
    pub docs: Vec<Vec<u8>>,
    /// The scan reached the last bucket.
    pub exhausted: bool,
    /// Bucket documents examined.
    pub examined: i64,
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
        }))
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

    /// The next `want` buckets the filter passes, stopping early once they
    /// come to more than `max_bytes`, unpacked.
    pub(crate) fn next_buffer(
        &mut self,
        storage: &dyn crate::storage::Storage,
        want: usize,
        max_bytes: usize,
    ) -> Result<Buffered, CommandError> {
        let rows = crate::ts_store::stored_bucket_bytes(storage, &self.db, &self.coll)?;
        let mut out = Buffered {
            docs: Vec::new(),
            exhausted: true,
            examined: 0,
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
        Ok(out)
    }
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
                ops.iter()
                    .all(|(op, arg)| internal_expr(bucket, path, op, arg))
            }
            _ => {
                let mut one = Document::new();
                one.insert(key.clone(), value.clone());
                secantus_core::query::matches(bucket, &one, &Document::new(), None).map_err(
                    |f| match f {
                        secantus_core::fallback::Fallback::Mongo { code, message, .. } => {
                            CommandError::new(code, crate::util::error_code_name(code), message)
                        }
                        _ => CommandError::new(2, "BadValue", "unsupported bucket filter"),
                    },
                )?
            }
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `{path: {op: arg}}` for one `$_internalExpr*` comparison over `doc`.
fn internal_expr(doc: &Document, path: &str, op: &str, arg: &Bson) -> bool {
    let mut at: Option<&Bson> = None;
    let mut current = doc;
    for (n, part) in path.split('.').enumerate() {
        match current.get(part) {
            None => {
                at = None;
                break;
            }
            Some(Bson::Array(_)) => return true,
            Some(Bson::Document(d)) if n + 1 < path.split('.').count() => current = d,
            Some(v) if n + 1 == path.split('.').count() => at = Some(v),
            Some(_) => {
                at = None;
                break;
            }
        }
    }
    let ordering = match at {
        Some(v) => secantus_core::order::cmp(v, arg),
        None => Ordering::Less,
    };
    match op {
        "$_internalExprEq" => ordering == Ordering::Equal,
        "$_internalExprGt" => ordering == Ordering::Greater,
        "$_internalExprGte" => ordering != Ordering::Less,
        "$_internalExprLt" => ordering == Ordering::Less,
        "$_internalExprLte" => ordering != Ordering::Greater,
        _ => true,
    }
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
        let mut entry = doc! { "_id": format!("{db}.{name}") };
        if let Ok(view_on) = options.get_str("viewOn") {
            entry.insert("viewOn", view_on);
            entry.insert(
                "pipeline",
                options
                    .get_array("viewPipeline")
                    .cloned()
                    .unwrap_or_default(),
            );
            if let Ok(collation) = options.get_document("collation") {
                entry.insert("collation", collation.clone());
            }
        } else if let Ok(ts) = options.get_document("timeseries") {
            entry.insert("viewOn", format!("{BUCKETS_PREFIX}{name}"));
            entry.insert("pipeline", vec![unpack_stage(ts)]);
        } else {
            continue;
        }
        out.push(entry);
    }
    Ok(out)
}

/// The stage `mongod`'s view of a time-series collection with the options
/// `ts` unpacks its buckets with.
pub(crate) fn unpack_stage(ts: &Document) -> Document {
    let mut unpack = Document::new();
    for field in ["timeField", "metaField", "bucketMaxSpanSeconds"] {
        if let Some(v) = ts.get(field) {
            unpack.insert(field, v.clone());
        }
    }
    doc! { "$_internalUnpackBucket": unpack }
}

/// The validator `mongod` gives `system.buckets.<name>`.
pub(crate) fn validator(time: &str) -> Document {
    let bound = doc! {
        "bsonType": "object",
        "required": [time],
        "properties": { time: { "bsonType": "date" } },
    };
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "control", "data"],
        "properties": {
            "_id": { "bsonType": "objectId" },
            "control": {
                "bsonType": "object",
                "required": ["version", "min", "max"],
                "properties": {
                    "version": { "bsonType": "number" },
                    "min": bound.clone(),
                    "max": bound,
                    "closed": { "bsonType": "bool" },
                    "count": { "bsonType": "number", "minimum": 1 },
                },
                "additionalProperties": false,
            },
            "data": { "bsonType": "object" },
            "meta": {},
        },
        "additionalProperties": false,
    } }
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

    fn layout() -> Layout {
        Layout::of(&doc! {"timeseries": {"timeField": "t", "metaField": "m",
        "granularity": "seconds", "bucketMaxSpanSeconds": 3600}})
        .unwrap()
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
