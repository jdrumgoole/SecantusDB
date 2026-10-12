//! The buckets of a time-series collection.
//!
//! `mongod` stores a time-series collection as bucket documents in
//! `system.buckets.<name>` and presents the collection as a view that unpacks
//! them. This server stores the measurements as ordinary documents, in the
//! format the Python server shares, and DERIVES the buckets when something
//! asks for them: a read of `system.buckets.<name>`, the order a scan of the
//! collection returns its measurements in, the counts `collStats` and
//! `validate` report.
//!
//! The derivation replays the measurements in insertion order through
//! `mongod`'s placement rule: a measurement joins a bucket of its meta value
//! whose time window holds it and which has room, and otherwise opens one
//! whose window starts at its time rounded down. What it cannot know is how
//! the measurements were batched, and `mongod` places a batch as a whole (it
//! sorts the batch first), so a batch that runs backwards in time can land in
//! fewer buckets there than here. Shapes were read from `mongod` 8.2.11.

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};

use crate::CommandError;

/// The storage namespace of a time-series collection's buckets.
pub const BUCKETS_PREFIX: &str = "system.buckets.";

/// The stage a read of `system.buckets.<name>` runs: it takes the collection's
/// name and yields its bucket documents. Not a stage a client can name.
pub const BUCKETS_STAGE: &str = "$_secantusBuckets";

/// The most measurements `mongod` puts in one bucket.
const MAX_COUNT: usize = 1000;

/// How a collection's measurements fall into buckets.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Layout {
    pub time: String,
    pub meta: Option<String>,
    span_ms: i64,
    rounding_ms: i64,
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

    fn time_of(&self, doc: &Document) -> i64 {
        match doc.get(&self.time) {
            Some(Bson::DateTime(t)) => t.timestamp_millis(),
            _ => 0,
        }
    }

    fn meta_of(&self, doc: &Document) -> Option<Bson> {
        let field = self.meta.as_ref()?;
        doc.get(field).map(sorted_keys)
    }
}

/// `v` with the keys of every document in it sorted, as `mongod` keeps a
/// meta value: `{b: 1, a: 2}` and `{a: 2, b: 1}` are one series.
fn sorted_keys(v: &Bson) -> Bson {
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

/// One derived bucket: where its window starts, its meta value, and the
/// measurements in it (indices into the input, in time order).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bucket {
    start_ms: i64,
    meta: Option<Bson>,
    pub members: Vec<usize>,
}

/// Place `docs` (the collection's measurements, in insertion order) into
/// buckets, returned in the order `system.buckets` holds them: by the start
/// of the window, then by which was opened first.
pub(crate) fn place(docs: &[Document], layout: &Layout) -> Vec<Bucket> {
    let mut buckets: Vec<Bucket> = Vec::new();
    // Per meta value, the buckets opened for it, oldest first.
    let mut by_meta: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
    for (i, doc) in docs.iter().enumerate() {
        let t = layout.time_of(doc);
        let meta = layout.meta_of(doc);
        let key = match &meta {
            Some(m) => bson::to_vec(&doc! {"m": m.clone()}).unwrap_or_default(),
            None => Vec::new(),
        };
        let opened = by_meta.entry(key).or_default();
        // The newest bucket that can take it: `mongod` writes to the open
        // bucket, and reopens an older one for a measurement that fits it.
        let home = opened.iter().rev().copied().find(|&b| {
            let bucket = &buckets[b];
            t >= bucket.start_ms
                && t - bucket.start_ms < layout.span_ms
                && bucket.members.len() < MAX_COUNT
        });
        match home {
            Some(b) => buckets[b].members.push(i),
            None => {
                opened.push(buckets.len());
                buckets.push(Bucket {
                    start_ms: t.div_euclid(layout.rounding_ms) * layout.rounding_ms,
                    meta,
                    members: vec![i],
                });
            }
        }
    }
    for bucket in &mut buckets {
        bucket.members.sort_by_key(|&i| layout.time_of(&docs[i]));
    }
    // A stable sort: buckets with one start keep the order they opened in.
    buckets.sort_by_key(|b| b.start_ms.div_euclid(1000));
    buckets
}

/// A measurement as the collection returns it: the time field, the meta
/// field with its keys sorted, then the rest in the order it was written.
pub(crate) fn measurement(doc: &Document, layout: &Layout) -> Document {
    let mut out = Document::new();
    if let Some(t) = doc.get(&layout.time) {
        out.insert(layout.time.clone(), t.clone());
    }
    if let Some(field) = &layout.meta {
        if let Some(m) = doc.get(field) {
            out.insert(field.clone(), sorted_keys(m));
        }
    }
    for (k, v) in doc {
        if k != &layout.time && Some(k) != layout.meta.as_ref() {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Fold `v` into a bucket's running minimum (or maximum) for one field.
/// Documents fold field by field and arrays element by element, as `mongod`
/// computes `control.min` / `control.max`; anything else is compared whole.
fn fold(acc: Option<Bson>, v: &Bson, want_max: bool) -> Bson {
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

/// One simple8b word holding a single "no value here" slot.
const SKIP_WORD: u64 = 0xFFFF_FFFF_FFFF_FFFE;

/// A bucket column (BSON binary subtype 7): one field's values across the
/// bucket's measurements, `None` where a measurement has no such field.
///
/// Every value is written as a literal element and every gap as a skip. That
/// is a valid column, and `mongod` unpacks it to the same measurements, but
/// it is not the bytes `mongod` writes: `mongod` delta-encodes runs.
fn column(values: &[Option<&Bson>]) -> Result<Bson, CommandError> {
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < values.len() {
        match values[i] {
            Some(v) => {
                let element = bson::to_vec(&doc! {"": v.clone()})
                    .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))?;
                // The element alone: without the document's length and its
                // closing zero.
                out.extend_from_slice(&element[4..element.len() - 1]);
                i += 1;
            }
            None => {
                let run = values[i..]
                    .iter()
                    .take(16)
                    .take_while(|v| v.is_none())
                    .count();
                out.push(0x80 | (run as u8 - 1));
                for _ in 0..run {
                    out.extend_from_slice(&SKIP_WORD.to_le_bytes());
                }
                i += run;
            }
        }
    }
    out.push(0);
    Ok(Bson::Binary(Binary {
        subtype: BinarySubtype::Column,
        bytes: out,
    }))
}

/// A bucket's `_id`: its window's start in the timestamp, as `mongod` has it,
/// then bytes that keep buckets opened later in order after it.
fn bucket_id(ns: &str, start_ms: i64, opened: usize) -> bson::oid::ObjectId {
    let mut bytes = [0u8; 12];
    bytes[..4].copy_from_slice(&(start_ms.div_euclid(1000) as u32).to_be_bytes());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in ns.bytes() {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    bytes[4..8].copy_from_slice(&(hash as u32).to_be_bytes());
    bytes[8..].copy_from_slice(&(opened as u32).to_be_bytes());
    bson::oid::ObjectId::from_bytes(bytes)
}

/// The bucket documents of a collection holding `docs` (in insertion order).
pub(crate) fn documents(
    ns: &str,
    docs: &[Document],
    layout: &Layout,
) -> Result<Vec<Document>, CommandError> {
    let mut out = Vec::new();
    for (opened, bucket) in place(docs, layout).into_iter().enumerate() {
        // The data fields, in the order the bucket first met them.
        let mut fields: Vec<&String> = Vec::new();
        for &i in &bucket.members {
            for k in docs[i].keys() {
                if Some(k) != layout.meta.as_ref() && !fields.contains(&k) {
                    fields.push(k);
                }
            }
        }
        let mut min = Document::new();
        let mut max = Document::new();
        let mut data = Document::new();
        for field in fields {
            let values: Vec<Option<&Bson>> =
                bucket.members.iter().map(|&i| docs[i].get(field)).collect();
            for v in values.iter().flatten() {
                let low = fold(min.remove(field), v, false);
                min.insert(field.clone(), low);
                let high = fold(max.remove(field), v, true);
                max.insert(field.clone(), high);
            }
            data.insert(field.clone(), column(&values)?);
        }
        // The minimum time is the start of the window, not the first reading.
        min.insert(
            layout.time.clone(),
            bson::DateTime::from_millis(bucket.start_ms),
        );
        let mut doc = doc! {
            "_id": bucket_id(ns, bucket.start_ms, opened),
            "control": {
                "version": 2,
                "min": min,
                "max": max,
                "count": bucket.members.len() as i32,
            },
        };
        if let Some(meta) = bucket.meta {
            doc.insert("meta", meta);
        }
        doc.insert("data", data);
        out.push(doc);
    }
    Ok(out)
}

/// The order a scan returns measurements in: bucket by bucket, each in time
/// order. Indices into `docs`.
pub(crate) fn scan_order(docs: &[Document], layout: &Layout) -> Vec<usize> {
    place(docs, layout)
        .into_iter()
        .flat_map(|b| b.members)
        .collect()
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

/// Every measurement of `db.coll`, in insertion order.
fn stored(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> Result<Vec<Vec<u8>>, CommandError> {
    storage
        .find(db, coll, &Document::new(), None, None)
        .map_err(crate::util::command_error)
}

fn decoded(raw: &[Vec<u8>]) -> Result<Vec<Document>, CommandError> {
    raw.iter()
        .map(|b| {
            secantus_core::bsondoc::from_bytes(&b[..])
                .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))
        })
        .collect()
}

/// The bucket documents of the time-series collection `db.coll`.
pub(crate) fn of_collection(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> Result<Vec<Document>, CommandError> {
    let options = storage
        .get_collection_options(db, coll)
        .map_err(crate::util::command_error)?;
    let Some(layout) = Layout::of(&options) else {
        return Ok(Vec::new());
    };
    let docs = decoded(&stored(storage, db, coll)?)?;
    documents(&format!("{db}.{coll}"), &docs, &layout)
}

/// What a read of `db.coll` returns when it is a time-series collection:
/// `found` (the matching measurements, in insertion order unless `sorted`)
/// in `mongod`'s scan order and each in `mongod`'s field order. Any other
/// collection's documents pass through untouched.
pub(crate) fn as_read(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    found: Vec<Vec<u8>>,
    sorted: bool,
) -> Result<Vec<Vec<u8>>, CommandError> {
    if found.is_empty() {
        return Ok(found);
    }
    let Some(layout) = storage
        .get_collection_options(db, coll)
        .ok()
        .as_ref()
        .and_then(Layout::of)
    else {
        return Ok(found);
    };
    let shaped = |raw: &Vec<u8>| -> Result<Vec<u8>, CommandError> {
        let doc = secantus_core::bsondoc::from_bytes(&raw[..])
            .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))?;
        bson::to_vec(&measurement(&doc, &layout))
            .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))
    };
    if sorted {
        return found.iter().map(shaped).collect();
    }
    // Where each stored measurement falls in the scan order. Two identical
    // documents are interchangeable, so matching by bytes is enough.
    let all = stored(storage, db, coll)?;
    let order = scan_order(&decoded(&all)?, &layout);
    let mut rank: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (position, &i) in order.iter().enumerate().rev() {
        rank.entry(all[i].as_slice()).or_default().push(position);
    }
    let mut placed: Vec<(usize, &Vec<u8>)> = found
        .iter()
        .map(|raw| {
            let position = rank
                .get_mut(raw.as_slice())
                .and_then(Vec::pop)
                .unwrap_or(usize::MAX);
            (position, raw)
        })
        .collect();
    placed.sort_by_key(|(position, _)| *position);
    placed.into_iter().map(|(_, raw)| shaped(raw)).collect()
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
            let mut unpack = Document::new();
            for field in ["timeField", "metaField", "bucketMaxSpanSeconds"] {
                if let Some(v) = ts.get(field) {
                    unpack.insert(field, v.clone());
                }
            }
            entry.insert("viewOn", format!("{BUCKETS_PREFIX}{name}"));
            entry.insert("pipeline", vec![doc! { "$_internalUnpackBucket": unpack }]);
        } else {
            continue;
        }
        out.push(entry);
    }
    Ok(out)
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

    fn at(minutes: i64) -> Bson {
        // 2026-01-01T00:00:00Z
        Bson::DateTime(bson::DateTime::from_millis(
            1_767_225_600_000 + minutes * 60_000,
        ))
    }

    /// What `mongod` 8.2.11 made of the same six measurements, one at a time.
    #[test]
    fn measurements_fall_into_mongods_buckets() {
        let docs = vec![
            doc! {"_id": 1, "t": at(0), "m": {"b": 1, "a": 2}, "v": 1, "s": "x"},
            doc! {"_id": 2, "t": at(1), "m": "b", "v": 2},
            doc! {"_id": 3, "t": at(2), "m": {"a": 2, "b": 1}, "v": 3.5},
            doc! {"_id": 4, "t": at(3), "v": 4},
            doc! {"_id": 5, "t": at(120), "m": "b", "v": 5},
            doc! {"_id": 6, "t": at(30), "m": "b", "v": 6},
        ];
        let buckets = place(&docs, &layout());
        let members: Vec<Vec<usize>> = buckets.iter().map(|b| b.members.clone()).collect();
        // {a, b} at 00:00, "b" at 00:01 (reopened for 00:30), none at 00:03,
        // "b" again at 02:00.
        assert_eq!(members, vec![vec![0, 2], vec![1, 5], vec![3], vec![4]]);
        assert_eq!(scan_order(&docs, &layout()), vec![0, 2, 1, 5, 3, 4]);
    }

    #[test]
    fn a_bucket_document_has_mongods_shape() {
        let docs = vec![
            doc! {"_id": 1, "t": at(0), "m": {"b": 1, "a": 2}, "v": 1, "s": "x"},
            doc! {"_id": 3, "t": at(2), "m": {"a": 2, "b": 1}, "v": 3.5, "o": {"k": [1, 2]}},
        ];
        let buckets = documents("d.c", &docs, &layout()).unwrap();
        assert_eq!(buckets.len(), 1);
        let b = &buckets[0];
        let keys: Vec<&str> = b.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["_id", "control", "meta", "data"]);
        assert_eq!(b.get_document("meta").unwrap(), &doc! {"a": 2, "b": 1});
        let control = b.get_document("control").unwrap();
        assert_eq!(control.get_i32("count").ok(), Some(2));
        assert_eq!(
            control.get_document("min").unwrap(),
            &doc! {"_id": 1, "t": at(0), "v": 1, "s": "x", "o": {"k": [1, 2]}}
        );
        assert_eq!(
            control.get_document("max").unwrap(),
            &doc! {"_id": 3, "t": at(2), "v": 3.5, "s": "x", "o": {"k": [1, 2]}}
        );
        // `s` is a literal string, then one skipped slot, then the end.
        let Some(Bson::Binary(s)) = b.get_document("data").unwrap().get("s") else {
            panic!("no column");
        };
        assert_eq!(s.subtype, BinarySubtype::Column);
        let mut expected = vec![0x02, 0x00, 0x02, 0x00, 0x00, 0x00, b'x', 0x00, 0x80];
        expected.extend_from_slice(&SKIP_WORD.to_le_bytes());
        expected.push(0);
        assert_eq!(s.bytes, expected);
    }

    #[test]
    fn a_measurement_leads_with_time_and_meta() {
        let doc = doc! {"_id": 1, "v": 2, "m": {"b": 1, "a": {"z": 1, "y": 2}}, "t": at(0)};
        assert_eq!(
            measurement(&doc, &layout()),
            doc! {"t": at(0), "m": {"a": {"y": 2, "z": 1}, "b": 1}, "_id": 1, "v": 2}
        );
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
