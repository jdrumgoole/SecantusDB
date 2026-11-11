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

/// `path` with the meta field's name as the bucket keeps it (`meta`), when
/// it is in the meta field.
fn meta_path(path: &str, meta: &str) -> Option<String> {
    let rest = path.strip_prefix(meta)?;
    (rest.is_empty() || rest.starts_with('.')).then(|| format!("meta{rest}"))
}

/// A meta-only filter as a filter on the buckets.
fn bucket_meta_filter(filter: &Document, meta: &str) -> Document {
    let mut out = Document::new();
    for (k, v) in filter {
        match (k.as_str(), v) {
            ("$and" | "$or" | "$nor", Bson::Array(parts)) => {
                let parts: Vec<Bson> = parts
                    .iter()
                    .map(|p| match p {
                        Bson::Document(d) => Bson::Document(bucket_meta_filter(d, meta)),
                        other => other.clone(),
                    })
                    .collect();
                out.insert(k.clone(), parts);
            }
            _ => match meta_path(k, meta) {
                Some(path) => {
                    out.insert(path, v.clone());
                }
                None => {
                    out.insert(k.clone(), v.clone());
                }
            },
        }
    }
    out
}

/// `update`'s one statement on a time-series collection, which mongod
/// 8.2.11 takes only as a multi update of the meta field
/// (`timeseries::update_problem` has refused the rest): an update of the
/// buckets' `meta`. Its `n` and `nModified` count measurements, not
/// buckets.
pub(crate) fn update_meta(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    statement: &Document,
    let_vars: &Document,
    collation: Option<&Collation>,
) -> Result<(i32, i32), CommandError> {
    let Some(layout) = layout_of(storage, db, coll) else {
        return Ok((0, 0));
    };
    let Some(meta) = layout.meta.as_deref() else {
        return Ok((0, 0));
    };
    let ns = buckets_ns(coll);
    let q = statement.get_document("q").cloned().unwrap_or_default();
    let filter = bucket_meta_filter(&q, meta);
    let mut update = Document::new();
    for (op, targets) in statement.get_document("u").cloned().unwrap_or_default() {
        let Bson::Document(targets) = targets else {
            update.insert(op, targets);
            continue;
        };
        let mut moved = Document::new();
        for (path, value) in targets {
            let path = meta_path(&path, meta).unwrap_or(path);
            let value = match (op.as_str(), value) {
                ("$rename", Bson::String(to)) => Bson::String(meta_path(&to, meta).unwrap_or(to)),
                (_, v) => v,
            };
            moved.insert(path, value);
        }
        update.insert(op, moved);
    }
    let array_filters: Vec<Document> = statement
        .get_array("arrayFilters")
        .map(|a| a.iter().filter_map(Bson::as_document).cloned().collect())
        .unwrap_or_default();
    let rows = storage
        .find_collated(db, &ns, &filter, None, None, collation, let_vars)
        .map_err(crate::util::command_error)?;
    let uuid = storage.collection_uuid(db, &ns).ok();
    let (mut n, mut modified) = (0i32, 0i32);
    for raw in rows {
        let bucket = decode(&raw)?;
        let Some(Bson::ObjectId(id)) = bucket.get("_id") else {
            continue;
        };
        let count = bucket
            .get_document("control")
            .ok()
            .and_then(|c| c.get("count"))
            .and_then(crate::util::as_i64)
            .unwrap_or(0) as i32;
        if let (Some(catalog), Some(uuid)) = (storage.bucket_catalog(), &uuid) {
            catalog.clear_bucket(uuid, id);
        }
        let outcome = storage
            .update_matching_array_filters(
                db,
                &ns,
                &doc! { "_id": *id },
                &update,
                false,
                false,
                &array_filters,
                let_vars,
                collation,
                None,
                false,
                false,
            )
            .map_err(crate::util::command_error)?;
        n += count * outcome.matched as i32;
        modified += count * outcome.modified as i32;
    }
    Ok((n, modified))
}

/// The bucket document mongod writes for `measurements` (in time order)
/// when it rewrites a bucket (`makeNewDocumentForWrite`, then
/// `compressBucket`): the summaries and columns of what is left, the
/// window's start kept as `control.min.<time>`.
fn rewritten_bucket(
    id: bson::oid::ObjectId,
    meta: Option<&Bson>,
    measurements: &[Document],
    layout: &Layout,
    min_time: &Bson,
) -> Result<Document, CommandError> {
    let mut min = Document::new();
    let mut max = Document::new();
    let mut fields: Vec<String> = Vec::new();
    for m in measurements {
        for (name, v) in m {
            if layout.meta.as_deref() == Some(name.as_str()) {
                continue;
            }
            if !fields.contains(name) {
                fields.push(name.clone());
            }
            for (acc, want_max) in [(&mut min, false), (&mut max, true)] {
                match acc.get_mut(name) {
                    Some(slot) => {
                        let held = std::mem::replace(slot, Bson::Null);
                        *slot = crate::ts_buckets::fold(Some(held), v, want_max);
                    }
                    None => {
                        acc.insert(name.clone(), crate::ts_buckets::fold(None, v, want_max));
                    }
                }
            }
        }
    }
    if let Some(slot) = min.get_mut(&layout.time) {
        *slot = min_time.clone();
    }
    let mut data = Document::new();
    for field in &fields {
        let mut builder = secantus_core::bsoncolumn::ColumnBuilder::new();
        for m in measurements {
            match m.get(field) {
                Some(v) => {
                    let bytes = encode(&doc! {"": v.clone()})?;
                    builder.append(Some(secantus_core::bsoncolumn::Element {
                        ty: bytes[4],
                        value: &bytes[6..bytes.len() - 1],
                    }));
                }
                None => builder.skip(),
            }
        }
        data.insert(
            field.clone(),
            Bson::Binary(bson::Binary {
                subtype: BinarySubtype::Column,
                bytes: builder.finalize(),
            }),
        );
    }
    let mut doc = doc! {
        "_id": id,
        "control": {
            "version": crate::bucket_catalog::SORTED_VERSION,
            "min": min,
            "max": max,
            "count": measurements.len() as i32,
        },
    };
    if let Some(meta) = meta {
        doc.insert("meta", meta.clone());
    }
    doc.insert("data", data);
    Ok(doc)
}

/// Delete the measurements of `db.coll` that `filter` matches, at most one
/// when `just_one`: each bucket that holds one is rewritten with what is
/// left, or deleted when nothing is (mongod 8.2.11's time-series delete).
/// The measurements deleted, in scan order.
pub(crate) fn delete(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    just_one: bool,
    vars: &Document,
    collation: Option<&Collation>,
) -> Result<Vec<Document>, CommandError> {
    let Some(layout) = layout_of(storage, db, coll) else {
        return Ok(Vec::new());
    };
    let ns = buckets_ns(coll);
    let bucket_filter = crate::timeseries::bucket_filter(
        filter,
        &layout.time,
        layout.meta.as_deref(),
        layout.span_ms / 1000,
    );
    let uuid = storage.collection_uuid(db, &ns).ok();
    let mut deleted = Vec::new();
    for bucket in stored_buckets(storage, db, coll)? {
        if just_one && !deleted.is_empty() {
            break;
        }
        if !bucket_filter.is_empty() && !crate::ts_buckets::bucket_matches(&bucket, &bucket_filter)?
        {
            continue;
        }
        let Some(Bson::ObjectId(id)) = bucket.get("_id").cloned() else {
            continue;
        };
        let mut kept = Vec::new();
        let mut gone = Vec::new();
        for m in unpack(&bucket, &layout)? {
            let hit = (!just_one || gone.is_empty())
                && (filter.is_empty()
                    || secantus_core::query::matches(&m, filter, vars, collation)
                        .map_err(match_error)?);
            if hit {
                gone.push(m);
            } else {
                kept.push(m);
            }
        }
        if gone.is_empty() {
            continue;
        }
        if let (Some(catalog), Some(uuid)) = (storage.bucket_catalog(), &uuid) {
            catalog.clear_bucket(uuid, &id);
        }
        if kept.is_empty() {
            storage
                .delete_matching(db, &ns, &doc! { "_id": id }, 1)
                .map_err(crate::util::command_error)?;
        } else {
            kept.sort_by_key(|m| layout.time_of(m));
            let min_time = bucket
                .get_document("control")
                .ok()
                .and_then(|c| c.get_document("min").ok())
                .and_then(|m| m.get(&layout.time))
                .cloned()
                .unwrap_or(Bson::Null);
            let doc = rewritten_bucket(id, bucket.get("meta"), &kept, &layout, &min_time)?;
            storage
                .update_matching(db, &ns, &doc! { "_id": id }, &doc, false, false)
                .map_err(crate::util::command_error)?;
        }
        deleted.extend(gone);
    }
    Ok(deleted)
}

/// Bring a store written before time-series buckets were stored up to
/// date: a time-series collection whose measurements are still documents
/// of its own table has them placed into buckets, one at a time in the
/// order they were stored (the order this server placed them in when it
/// derived its buckets), then taken out of the table. Run as the store is
/// opened, before the server answers anyone; nothing it writes is logged.
///
/// The buckets are written before a measurement is removed, and a
/// collection that still holds measurements has its bucket collection
/// rebuilt from them, so a conversion cut short is done again from the
/// start the next time the store opens. The measurements converted.
pub fn upgrade_time_series(storage: &dyn Storage) -> Result<usize, CommandError> {
    let _quiet = storage.unreplicated();
    let mut converted = 0;
    for db in storage
        .list_databases()
        .map_err(crate::util::command_error)?
    {
        for coll in storage
            .list_collections(&db)
            .map_err(crate::util::command_error)?
        {
            if layout_of(storage, &db, &coll).is_none() {
                continue;
            }
            let stored = storage
                .find(&db, &coll, &Document::new(), None, None)
                .map_err(crate::util::command_error)?;
            let ns = buckets_ns(&coll);
            let has_buckets = storage
                .collection_exists(&db, &ns)
                .map_err(crate::util::command_error)?;
            if stored.is_empty() {
                if !has_buckets {
                    create_buckets(storage, &db, &coll)?;
                }
                continue;
            }
            if has_buckets {
                storage
                    .drop_collection(&db, &ns)
                    .map_err(crate::util::command_error)?;
            }
            create_buckets(storage, &db, &coll)?;
            let batches: Vec<Vec<crate::bucket_catalog::Measurement>> = stored
                .iter()
                .enumerate()
                .map(|(index, raw)| {
                    decode(raw).map(|doc| vec![crate::bucket_catalog::Measurement { index, doc }])
                })
                .collect::<Result<_, _>>()?;
            let layout = layout_of(storage, &db, &coll).expect("checked above");
            let target = crate::bucket_catalog::Target {
                storage,
                db: &db,
                coll: &coll,
                layout: &layout,
            };
            // A catalog of its own: the server's starts empty, as it would
            // after a restart.
            let catalog = crate::bucket_catalog::BucketCatalog::new();
            let outcomes = crate::bucket_catalog::insert(&catalog, &target, batches)?;
            if let Some((_, Err(e))) = outcomes.iter().find(|(_, o)| o.is_err()) {
                return Err(CommandError::new(
                    e.code,
                    "TimeseriesUpgradeFailed",
                    format!(
                        "converting {db}.{coll} to stored buckets: {}; its measurements are \
                         kept",
                        e.errmsg
                    ),
                ));
            }
            storage
                .delete_matching(&db, &coll, &Document::new(), 0)
                .map_err(crate::util::command_error)?;
            converted += stored.len();
        }
    }
    Ok(converted)
}
