//! A time-series collection as `mongod` stores it: bucket documents in
//! `system.buckets.<name>`, each data field a compressed column, and the
//! collection itself a view that unpacks them.
//!
//! The collection's own catalog entry keeps its options (`timeseries`,
//! `expireAfterSeconds`); its measurements are only ever in the buckets.
//! Reads unpack the buckets in their `_id` order (the bucket collection is
//! clustered by `_id`, whose timestamp is the window's start), skipping the
//! buckets the bucket-level filter rules out; writes go through the bucket
//! catalog (`bucket_catalog`).

use bson::spec::BinarySubtype;
use bson::{doc, Bson, Document};
use secantus_core::collation::Collation;

use crate::storage::Storage;
use crate::ts_buckets::{Layout, BUCKETS_PREFIX};
use crate::CommandError;

/// The bucket namespace of time-series collection `coll`.
pub(crate) fn buckets_ns(coll: &str) -> String {
    format!("{BUCKETS_PREFIX}{coll}")
}

/// The layout of `db.coll` when it is a time-series collection.
pub(crate) fn layout_of(storage: &dyn Storage, db: &str, coll: &str) -> Option<Layout> {
    storage
        .get_collection_options(db, coll)
        .ok()
        .as_ref()
        .and_then(Layout::of)
}

fn decode(raw: &[u8]) -> Result<Document, CommandError> {
    secantus_core::bsondoc::from_bytes(raw)
        .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))
}

fn encode(doc: &Document) -> Result<Vec<u8>, CommandError> {
    crate::bsondoc::to_vec(doc).map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))
}

/// The options the bucket collection is created with: clustered by `_id`,
/// as `mongod` creates it. What `listCollections` shows for it is built
/// from the time-series collection's own options (`admin`).
pub(crate) fn bucket_collection_options() -> Document {
    doc! { "clusteredIndex": true }
}

/// Create the bucket collection of time-series collection `db.coll`.
pub(crate) fn create_buckets(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
) -> Result<(), CommandError> {
    storage
        .create_collection_with_options(db, &buckets_ns(coll), &bucket_collection_options())
        .map_err(crate::util::command_error)?;
    Ok(())
}

/// The stored bucket documents of `db.coll`, as bytes, in `_id` order.
pub(crate) fn stored_bucket_bytes(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
) -> Result<Vec<Vec<u8>>, CommandError> {
    let ns = buckets_ns(coll);
    if !storage
        .collection_exists(db, &ns)
        .map_err(crate::util::command_error)?
    {
        return Ok(Vec::new());
    }
    let mut rows = storage
        .find(db, &ns, &Document::new(), None, None)
        .map_err(crate::util::command_error)?;
    // Clustered by `_id`: an ObjectId's bytes sort as it does.
    let id_of = |raw: &[u8]| -> Vec<u8> {
        match decode(raw).ok().and_then(|d| d.get("_id").cloned()) {
            Some(Bson::ObjectId(o)) => o.bytes().to_vec(),
            _ => Vec::new(),
        }
    };
    let mut keyed: Vec<(Vec<u8>, Vec<u8>)> = rows.drain(..).map(|r| (id_of(&r), r)).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(keyed.into_iter().map(|(_, r)| r).collect())
}

/// The stored bucket documents of `db.coll`, in `_id` order.
pub(crate) fn stored_buckets(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
) -> Result<Vec<Document>, CommandError> {
    stored_bucket_bytes(storage, db, coll)?
        .iter()
        .map(|r| decode(r))
        .collect()
}

/// A bucket's measurements, as `mongod`'s `BucketUnpacker` gives them: the
/// time field, the meta value, then the other data fields in the order the
/// bucket holds them; a skip in a column leaves the field out.
pub(crate) fn unpack(bucket: &Document, layout: &Layout) -> Result<Vec<Document>, CommandError> {
    let corrupt = |what: &str| {
        CommandError::new(
            8,
            "UnknownError",
            format!("time-series bucket {:?}: {what}", bucket.get("_id")),
        )
    };
    let data = bucket
        .get_document("data")
        .map_err(|_| corrupt("no data"))?;
    let meta = layout.meta.as_ref().and_then(|_| bucket.get("meta"));
    let mut columns: Vec<(&str, Vec<Option<Bson>>)> = Vec::new();
    for (name, v) in data {
        let values = match v {
            Bson::Binary(b) if b.subtype == BinarySubtype::Column => {
                secantus_core::bsoncolumn::decode(&b.bytes)
                    .map_err(|e| corrupt(e.message))?
                    .into_iter()
                    .map(|v| v.map(|v| value_of(&v)).transpose())
                    .collect::<Result<Vec<_>, _>>()?
            }
            // An uncompressed column (a bucket written by hand): an object
            // keyed by position.
            Bson::Document(d) => {
                let mut out: Vec<Option<Bson>> = Vec::new();
                for (k, v) in d {
                    let Ok(i) = k.parse::<usize>() else { continue };
                    if out.len() <= i {
                        out.resize(i + 1, None);
                    }
                    out[i] = Some(v.clone());
                }
                out
            }
            _ => return Err(corrupt("a data field is not a column")),
        };
        columns.push((name.as_str(), values));
    }
    let count = columns
        .iter()
        .find(|(n, _)| *n == layout.time)
        .map_or(0, |(_, v)| v.len());
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut m = Document::new();
        if let Some((_, values)) = columns.iter().find(|(n, _)| *n == layout.time) {
            if let Some(Some(t)) = values.get(i) {
                m.insert(layout.time.clone(), t.clone());
            }
        }
        if let (Some(field), Some(meta)) = (&layout.meta, meta) {
            m.insert(field.clone(), meta.clone());
        }
        for (name, values) in &columns {
            if *name == layout.time {
                continue;
            }
            if let Some(Some(v)) = values.get(i) {
                m.insert((*name).to_string(), v.clone());
            }
        }
        out.push(m);
    }
    Ok(out)
}

/// A decoded column value as BSON.
fn value_of(v: &secantus_core::bsoncolumn::Value) -> Result<Bson, CommandError> {
    let mut raw = Vec::with_capacity(v.value.len() + 7);
    raw.extend_from_slice(&((v.value.len() + 7) as i32).to_le_bytes());
    raw.push(v.ty);
    raw.push(0);
    raw.extend_from_slice(&v.value);
    raw.push(0);
    let doc = decode(&raw)?;
    doc.get("")
        .cloned()
        .ok_or_else(|| CommandError::new(8, "UnknownError", "an empty column value"))
}

/// The measurements of `db.coll` that `filter` matches, in `mongod`'s scan
/// order (bucket by bucket, each in the order it holds them).
pub(crate) fn measurements(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    vars: &Document,
    collation: Option<&Collation>,
) -> Result<Vec<Document>, CommandError> {
    let Some(layout) = layout_of(storage, db, coll) else {
        return Ok(Vec::new());
    };
    let bucket_filter = crate::timeseries::bucket_filter(
        filter,
        &layout.time,
        layout.meta.as_deref(),
        layout.span_ms / 1000,
    );
    let mut out = Vec::new();
    for bucket in stored_buckets(storage, db, coll)? {
        if !bucket_filter.is_empty() && !crate::ts_buckets::bucket_matches(&bucket, &bucket_filter)?
        {
            continue;
        }
        for m in unpack(&bucket, &layout)? {
            if filter.is_empty()
                || secantus_core::query::matches(&m, filter, vars, collation)
                    .map_err(match_error)?
            {
                out.push(m);
            }
        }
    }
    Ok(out)
}

/// A matcher failure as the command error `mongod` answers.
pub(crate) fn match_error(f: secantus_core::fallback::Fallback) -> CommandError {
    match f {
        secantus_core::fallback::Fallback::Mongo { code, message, .. } => {
            CommandError::new(code, crate::util::error_code_name(code), message)
        }
        _ => CommandError::new(
            2,
            "BadValue",
            "query uses a construct the Rust server does not support",
        ),
    }
}

/// Drop the bucket collection of `db.coll`, when it has one, and forget its
/// buckets.
pub(crate) fn drop_buckets(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
) -> Result<(), CommandError> {
    let ns = buckets_ns(coll);
    if !storage
        .collection_exists(db, &ns)
        .map_err(crate::util::command_error)?
    {
        return Ok(());
    }
    if let (Some(catalog), Ok(uuid)) = (storage.bucket_catalog(), storage.collection_uuid(db, &ns))
    {
        catalog.clear(&uuid);
    }
    storage
        .drop_collection(db, &ns)
        .map_err(crate::util::command_error)?;
    Ok(())
}

/// Insert `batches` of measurements into time-series collection `db.coll`
/// through the server's bucket catalog (a catalog of its own when the
/// store keeps none). The outcome of each measurement, by its index.
pub(crate) fn insert(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    batches: Vec<Vec<crate::bucket_catalog::Measurement>>,
) -> Result<std::collections::HashMap<usize, crate::bucket_catalog::Outcome>, CommandError> {
    let Some(layout) = layout_of(storage, db, coll) else {
        return Err(CommandError::new(
            26,
            "NamespaceNotFound",
            format!("{db}.{coll} is not a time-series collection"),
        ));
    };
    if !storage
        .collection_exists(db, &buckets_ns(coll))
        .map_err(crate::util::command_error)?
    {
        create_buckets(storage, db, coll)?;
    }
    let target = crate::bucket_catalog::Target {
        storage,
        db,
        coll,
        layout: &layout,
    };
    let own;
    let catalog = match storage.bucket_catalog() {
        Some(c) => c,
        None => {
            own = crate::bucket_catalog::BucketCatalog::new();
            &own
        }
    };
    crate::bucket_catalog::insert(catalog, &target, batches)
}

/// Insert `docs` as one batch, failing on the first measurement that fails
/// (`$out`'s write).
pub(crate) fn insert_or_fail(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    docs: Vec<Document>,
) -> Result<(), CommandError> {
    let batch = docs
        .into_iter()
        .enumerate()
        .map(|(index, doc)| crate::bucket_catalog::Measurement { index, doc })
        .collect();
    let outcomes = insert(storage, db, coll, vec![batch])?;
    let mut failed: Vec<(usize, CommandError)> = outcomes
        .into_iter()
        .filter_map(|(i, o)| o.err().map(|e| (i, e)))
        .collect();
    failed.sort_by_key(|(i, _)| *i);
    match failed.into_iter().next() {
        Some((_, e)) => Err(e),
        None => Ok(()),
    }
}

/// A read of `db.coll` when it is a time-series collection: the matching
/// measurements in scan order, or by `sort` (ties in scan order). `None`
/// for any other collection.
pub(crate) fn find(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    sort: Option<&Document>,
    collation: Option<&Collation>,
    vars: &Document,
) -> Result<Option<Vec<Vec<u8>>>, crate::storage::StorageError> {
    if layout_of(storage, db, coll).is_none() {
        return Ok(None);
    }
    let as_storage = |e: CommandError| crate::storage::StorageError::WriteError {
        code: e.code,
        errmsg: e.errmsg,
        exec: false,
    };
    let mut found = measurements(storage, db, coll, filter, vars, collation).map_err(as_storage)?;
    if let Some(sort) = sort.filter(|s| !s.is_empty()) {
        let _active = secantus_core::collation::activate(collation);
        let fields = secantus_core::aggregate::sort_fields(&Bson::Document(sort.clone()))
            .map_err(match_error)
            .map_err(as_storage)?;
        let mut keyed = Vec::with_capacity(found.len());
        for d in found {
            let key = secantus_core::aggregate::sort_key_values(&d, &fields)
                .map_err(match_error)
                .map_err(as_storage)?;
            keyed.push((key, d));
        }
        keyed.sort_by(|(a, _), (b, _)| {
            for (i, (_, reverse)) in fields.iter().enumerate() {
                let c = secantus_core::order::cmp(&a[i], &b[i]);
                let c = if *reverse { c.reverse() } else { c };
                if c != std::cmp::Ordering::Equal {
                    return c;
                }
            }
            std::cmp::Ordering::Equal
        });
        found = keyed.into_iter().map(|(_, d)| d).collect();
    }
    found
        .iter()
        .map(encode)
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
        .map_err(as_storage)
}

/// The `timeseries` section of `collStats`: the bucket namespace, how many
/// buckets it holds and their average size, then the bucket catalog's
/// execution statistics for the collection (mongod 8.2.11).
pub(crate) fn stats_section(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    buckets: i64,
    size: i64,
) -> Result<Document, CommandError> {
    let int = crate::admin::int_bson;
    let mut out = doc! {
        "bucketsNs": format!("{db}.{}", buckets_ns(coll)),
        "bucketCount": int(buckets),
        "avgBucketSize": int(if buckets > 0 { size / buckets } else { 0 }),
    };
    let stats = match (
        storage.bucket_catalog(),
        storage.collection_uuid(db, &buckets_ns(coll)),
    ) {
        (Some(catalog), Ok(uuid)) => catalog.collection_stats(&uuid),
        _ => crate::bucket_catalog::ExecutionStats::default(),
    };
    stats.append_to(&mut out);
    Ok(out)
}
