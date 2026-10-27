//! `dbStats`, `collStats`, `dataSize` and `listDatabases`: the fields mongod
//! reports, in its order and number types, and the arguments it refuses.
//! Measured on mongod 8.2.11 with `tools/probes/ddl_admin_commands.py`.
//!
//! The byte counts are this server's own: it lays documents out differently
//! on disk, so only their presence and type can be mongod's.

use crate::util::{as_i64, coll_arg, command_error};
use crate::{argtypes, CommandContext, CommandError, HandlerResult};
use bson::{doc, Bson, Document};
use secantus_core::query::bson_type_name;

fn err(code: i32, name: &str, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, name, msg.into())
}

fn unknown_field(doc: &Document, cmd: &str, known: &[&str]) -> Result<(), CommandError> {
    match argtypes::unknown_field(doc, known) {
        Some(field) => Err(err(
            40415,
            "IDLUnknownField",
            format!("BSON field '{cmd}.{field}' is an unknown field."),
        )),
        None => Ok(()),
    }
}

/// A count as mongod sends it in `collStats`: an int32 while it fits.
fn int(n: i64) -> Bson {
    match i32::try_from(n) {
        Ok(small) => Bson::Int32(small),
        Err(_) => Bson::Int64(n),
    }
}

/// The `scale` argument, truncated to a whole number; absent is 1.
fn scale_of(doc: &Document, cmd: &str) -> Result<Option<i64>, CommandError> {
    match doc.get("scale") {
        None | Some(Bson::Null) => Ok(None),
        Some(v @ (Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_))) => {
            Ok(Some(as_i64(v).unwrap_or(0)))
        }
        Some(other) => Err(err(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{cmd}.scale' is the wrong type '{}', expected types \
                 '[decimal, int, long, double]'",
                bson_type_name(other)
            ),
        )),
    }
}

/// What a database holds that `dbStats` and `listDatabases` add up.
struct Totals {
    collections: i64,
    views: i64,
    objects: i64,
    data_size: i64,
    indexes: i64,
    index_size: i64,
}

fn totals(ctx: &CommandContext, db: &str) -> Result<Totals, CommandError> {
    let storage = ctx.storage()?;
    let mut t = Totals {
        collections: 0,
        views: 0,
        objects: 0,
        data_size: 0,
        indexes: 0,
        index_size: 0,
    };
    for c in storage.list_collections(db).map_err(command_error)? {
        if crate::views::is_view(storage, db, &c) {
            t.views += 1;
            continue;
        }
        t.collections += 1;
        // The size storer's numbers, as mongod adds them up (RM-191).
        let kept = storage.collection_sizes(db, &c).map_err(command_error)?;
        t.objects += kept.records;
        t.data_size += kept.data_size;
        let sizes = kept.index_sizes;
        t.indexes += storage
            .list_indexes(db, &c)
            .map_or(sizes.len(), |l| l.len()) as i64;
        t.index_size += sizes.values().filter_map(as_i64).sum::<i64>();
    }
    // The views are documents in `system.views`, a collection with an `_id` index.
    // Their bytes are the definitions' (measured 8.2.11, 2026-10-15: a view
    // over five `$match` stages adds the 238 bytes of its definition).
    if t.views > 0 {
        t.collections += 1;
        t.objects += t.views;
        t.indexes += 1;
        for definition in crate::ts_buckets::view_definitions(storage, db)? {
            if definition.contains_key("viewOn") && !is_timeseries_definition(&definition) {
                t.data_size += crate::bsondoc::to_vec(&definition).map_or(0, |b| b.len() as i64);
            }
        }
    }
    Ok(t)
}

/// Whether a `system.views` definition is a time-series collection's, which
/// `totals` does not count as a view.
fn is_timeseries_definition(definition: &Document) -> bool {
    definition
        .get_array("pipeline")
        .ok()
        .and_then(|p| p.first())
        .and_then(Bson::as_document)
        .is_some_and(|stage| stage.contains_key("$_internalUnpackBucket"))
}

/// `dbStats`.
pub fn db_stats(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = doc.keys().next().map_or("dbStats", String::as_str);
    unknown_field(doc, "dbStats", &[name, "scale", "freeStorage"])?;
    let scale = scale_of(doc, "dbStats")?.unwrap_or(1);
    if !(1..=(1_i64 << 50)).contains(&scale) {
        return Err(err(
            2,
            "BadValue",
            "Scale factor must be greater than zero and less than or equal to 2^50",
        ));
    }
    let free = match doc.get("freeStorage") {
        None | Some(Bson::Null) => false,
        Some(v @ (Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))) => {
            crate::util::truthy(v)
        }
        Some(v @ Bson::Decimal128(_)) => as_i64(v) != Some(0),
        Some(other) => {
            return Err(err(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'dbStats.freeStorage' is the wrong type '{}', expected types \
                     '[decimal, double, int, bool, long]'",
                    bson_type_name(other)
                ),
            ))
        }
    };
    let t = totals(ctx, &ctx.db_name)?;
    let scaled = |n: i64| n as f64 / scale as f64;
    let mut out = doc! {
        "db": ctx.db_name.clone(),
        "collections": Bson::Int64(t.collections),
        "views": Bson::Int64(t.views),
        "objects": Bson::Int64(t.objects),
        "avgObjSize": if t.objects > 0 { t.data_size as f64 / t.objects as f64 } else { 0.0 },
        "dataSize": scaled(t.data_size),
        "storageSize": scaled(t.data_size),
    };
    if free {
        out.insert("freeStorageSize", 0.0);
    }
    out.insert("indexes", Bson::Int64(t.indexes));
    out.insert("indexSize", scaled(t.index_size));
    if free {
        out.insert("indexFreeStorageSize", 0.0);
    }
    out.insert("totalSize", scaled(t.data_size + t.index_size));
    if free {
        out.insert("totalFreeStorageSize", 0.0);
    }
    out.insert("scaleFactor", Bson::Int64(scale));
    // The filesystem holding the store, scaled like the rest. mongod reports
    // it only for a database that exists, and zeros for one that does not
    // (measured 8.2.11, 2026-10-16).
    let exists = t.collections + t.views > 0;
    let (fs_used, fs_total) = ctx
        .storage()?
        .data_path()
        .filter(|_| exists)
        .and_then(|path| crate::procstats::fs_space(&path))
        .unwrap_or((0, 0));
    out.insert("fsUsedSize", fs_used as f64 / scale as f64);
    out.insert("fsTotalSize", fs_total as f64 / scale as f64);
    out.insert("ok", 1.0);
    Ok(out)
}

/// `collStats`.
pub fn coll_stats(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let name = doc.keys().next().map_or("collStats", String::as_str);
    unknown_field(doc, "collStats", &[name, "scale", "verbose"])?;
    let coll = coll_arg(doc, name)?;
    let scale = match scale_of(doc, "collStats")? {
        Some(n) if n < 1 => {
            return Err(err(
                2,
                "BadValue",
                format!("BSON field 'scale' value must be >= 1, actual value '{n}'"),
            ))
        }
        Some(n) => n,
        None => 1,
    };
    let storage = ctx.storage()?;
    let db = ctx.db_name.clone();
    let ns = format!("{db}.{coll}");
    if crate::views::is_view(storage, &db, &coll) {
        return Err(err(
            166,
            "CommandNotSupportedOnView",
            format!("Namespace {ns} is a view, not a collection"),
        ));
    }
    let options = storage
        .get_collection_options(&db, &coll)
        .unwrap_or_default();
    if options.contains_key("timeseries") {
        return crate::admin::time_series_stats(&coll, ctx);
    }
    let mut out = doc! { "ns": ns };
    out.extend(storage_stats(storage, &db, &coll, scale)?);
    out.insert("ok", 1.0);
    Ok(out)
}

/// A collection's sizes and counts, divided by `scale`: what `collStats`
/// answers after `ns`, and the `storageStats` of a `$collStats` stage. One
/// function, so the two cannot give different numbers for one collection
/// (the stage once answered a size of 0 for every collection).
pub(crate) fn storage_stats(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    scale: i64,
) -> Result<Document, CommandError> {
    let options = storage.get_collection_options(db, coll).unwrap_or_default();
    let exists = storage.collection_exists(db, coll).unwrap_or(false);
    // The size storer's numbers, read without reading a document, as
    // mongod's are (RM-191).
    let kept = storage.collection_sizes(db, coll).map_err(command_error)?;
    let count = kept.records;
    let size = kept.data_size;
    let measured = kept.index_sizes;
    // Every index is listed, one that holds no entry yet included.
    let mut sizes = Document::new();
    for index in storage.list_indexes(db, coll).unwrap_or_default() {
        if let Ok(name) = index.get_str("name") {
            let size = measured.get(name).and_then(as_i64).unwrap_or(0);
            sizes.insert(name, Bson::Int64(size));
        }
    }
    let index_total: i64 = sizes.values().filter_map(as_i64).sum();
    let scaled: Document = sizes
        .iter()
        .map(|(k, v)| (k.clone(), int(as_i64(v).unwrap_or(0) / scale)))
        .collect();
    let mut out = doc! { "size": int(size / scale), "count": int(count) };
    if count > 0 {
        out.insert("avgObjSize", int(size / count));
    }
    out.insert("numOrphanDocs", 0_i32);
    out.insert("storageSize", int(size / scale));
    if !exists {
        out.insert("totalSize", 0_i32);
        out.insert("nindexes", 0_i32);
        out.insert("totalIndexSize", 0_i32);
        out.insert("indexSizes", Document::new());
        out.insert("scaleFactor", int(scale));
        return Ok(out);
    }
    out.insert("freeStorageSize", 0_i32);
    let capped = storage
        .collection_is_capped(db, coll)
        .map_err(command_error)?;
    out.insert("capped", capped);
    if capped {
        out.insert("max", int(options.get("max").and_then(as_i64).unwrap_or(0)));
        if let Some(max_size) = options.get("size").and_then(as_i64) {
            out.insert("maxSize", int(max_size / scale));
        }
    }
    out.insert("nindexes", sizes.len() as i32);
    out.insert("indexBuilds", Bson::Array(vec![]));
    out.insert("totalIndexSize", int(index_total / scale));
    out.insert("indexSizes", scaled);
    out.insert("totalSize", int((size + index_total) / scale));
    out.insert("scaleFactor", int(scale));
    Ok(out)
}

/// `dataSize`: the bytes and documents of one namespace.
pub fn data_size(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let ns = match doc.values().next() {
        Some(Bson::String(ns)) => ns.clone(),
        None | Some(Bson::Null) => {
            return Err(err(
                40414,
                "IDLFailedToParse",
                "BSON field 'dataSize.dataSize' is missing but a required field".to_string(),
            ))
        }
        Some(other) => {
            return Err(err(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'dataSize.dataSize' is the wrong type '{}', expected type \
                     'string'",
                    bson_type_name(other)
                ),
            ))
        }
    };
    let object = |field: &str| -> Result<Option<&Document>, CommandError> {
        match doc.get(field) {
            None | Some(Bson::Null) => Ok(None),
            Some(Bson::Document(d)) => Ok(Some(d)),
            Some(other) => Err(err(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'dataSize.{field}' is the wrong type '{}', expected type \
                     'object'",
                    bson_type_name(other)
                ),
            )),
        }
    };
    let pattern = object("keyPattern")?;
    let min = object("min")?;
    let max = object("max")?;
    let estimate = crate::serverinfo::flag(doc, "dataSize", "estimate")?;
    let Some((db, coll)) = ns
        .split_once('.')
        .filter(|(d, c)| !d.is_empty() && !c.is_empty())
    else {
        return Err(err(
            73,
            "InvalidNamespace",
            format!("Namespace {ns}is not a valid collection name"),
        ));
    };
    let bounds = match (min, max) {
        (Some(min), Some(max)) => Some((min, max)),
        (None, None) => None,
        (Some(_), None) => {
            return Err(err(
                2,
                "BadValue",
                "Max must be set if min is set.".to_string(),
            ))
        }
        (None, Some(_)) => {
            return Err(err(
                2,
                "BadValue",
                "Max must not be set if min is not set.".to_string(),
            ))
        }
    };
    let storage = ctx.storage()?;
    // A time-series collection is a view over its buckets to mongod, and
    // `dataSize` refuses a view (measured 8.2.11).
    if storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        return Err(err(
            166,
            "CommandNotSupportedOnView",
            format!("Namespace {db}.{coll} is a view, not a collection"),
        ));
    }
    // The collection's record count and size as its size storer keeps them
    // (RM-191). mongod reads them first: a collection they say is empty --
    // or one that does not exist -- answers zeros with no `estimate`, before
    // any index is looked for (measured 8.2.11).
    let kept = storage.collection_sizes(db, coll).map_err(command_error)?;
    if kept.records == 0 {
        return Ok(doc! {
            "size": Bson::Int64(0),
            "numObjects": Bson::Int64(0),
            "millis": Bson::Int64(0),
            "ok": 1.0,
        });
    }
    let (size, count) = match bounds {
        Some((min, max)) => {
            let (size, count) = bounded_size(storage, db, coll, pattern, min, max)?;
            // An estimate is the average document's size, from the kept
            // numbers, for each one counted.
            if estimate {
                ((kept.data_size / kept.records) * count, count)
            } else {
                (size, count)
            }
        }
        // An estimate is the kept numbers themselves; otherwise mongod reads
        // the collection, so what it answers is what has committed.
        None if estimate => (kept.data_size, kept.records),
        None => (
            storage
                .collection_data_size(db, coll)
                .map_err(command_error)?,
            storage
                .count_matching(db, coll, &Document::new())
                .map_err(command_error)? as i64,
        ),
    };
    Ok(doc! {
        "size": Bson::Int64(size),
        "numObjects": Bson::Int64(count),
        "millis": Bson::Int64(0),
        "estimate": estimate,
        "ok": 1.0,
    })
}

/// The size and number of the documents whose key is in `[min, max)`, read
/// the way mongod reads them: as a range of an index, so in the index's order
/// of ALL values and not one type's. The bounds name the leading fields of
/// an index (`_id` has one always), which must not be multikey.
fn bounded_size(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    pattern: Option<&Document>,
    min: &Document,
    max: &Document,
) -> Result<(i64, i64), CommandError> {
    let no_index = || {
        err(
            96,
            "OperationFailed",
            "Couldn't find valid index containing key pattern".to_string(),
        )
    };
    let fields: Vec<&str> = match pattern {
        Some(pattern) => pattern.keys().map(String::as_str).collect(),
        None => min.keys().map(String::as_str).collect(),
    };
    let served = fields == ["_id"]
        || storage
            .list_indexes(db, coll)
            .map_err(command_error)?
            .iter()
            .any(|index| {
                let leads = index.get_document("key").is_ok_and(|key| {
                    key.iter()
                        .map(|(name, way)| (name.as_str(), as_i64(way)))
                        .take(fields.len())
                        .eq(fields.iter().map(|name| (*name, Some(1))))
                });
                leads
                    && index
                        .get_str("name")
                        .is_ok_and(|name| !storage.index_is_multikey(db, coll, name))
            });
    if !served {
        return Err(no_index());
    }
    for bound in [min, max] {
        if !bound.keys().map(String::as_str).eq(fields.iter().copied()) {
            return Err(err(
                16634,
                "Location16634",
                format!(
                    "field names of bound {} do not match those of keyPattern {}",
                    secantus_core::query::bson_value_repr(&Bson::Document(bound.clone())),
                    secantus_core::query::bson_value_repr(&Bson::Document(
                        pattern.unwrap_or(min).clone()
                    )),
                ),
            ));
        }
    }
    let key_of = |source: &Document| -> Vec<Vec<u8>> {
        fields
            .iter()
            .map(|name| {
                let value = source.get(*name).cloned().unwrap_or(Bson::Null);
                secantus_core::sortkey::encode_value(&value, None).unwrap_or_default()
            })
            .collect()
    };
    let (low, high) = (key_of(min), key_of(max));
    let mut size = 0_i64;
    let mut count = 0_i64;
    for raw in storage
        .find(db, coll, &Document::new(), None, None)
        .map_err(command_error)?
    {
        let Ok(document) = crate::bsondoc::from_bytes(&raw) else {
            continue;
        };
        let key = key_of(&document);
        if key >= low && key < high {
            size += raw.len() as i64;
            count += 1;
        }
    }
    Ok((size, count))
}

/// `listDatabases`.
pub fn list_databases(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    unknown_field(
        doc,
        "listDatabases",
        &["listDatabases", "nameOnly", "filter", "authorizedDatabases"],
    )?;
    let name_only = match doc.get("nameOnly") {
        None | Some(Bson::Null) => false,
        Some(v @ (Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))) => {
            crate::util::truthy(v)
        }
        Some(v @ Bson::Decimal128(_)) => as_i64(v) != Some(0),
        Some(other) => {
            return Err(err(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'listDatabases.nameOnly' is the wrong type '{}', expected \
                     types '[int, double, bool, long, decimal]'",
                    bson_type_name(other)
                ),
            ))
        }
    };
    argtypes::require_object(doc, "filter", "listDatabases.filter")?;
    match doc.get("authorizedDatabases") {
        None | Some(Bson::Null | Bson::Boolean(_)) => {}
        Some(other) => {
            return Err(err(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'listDatabases.authorizedDatabases' is the wrong type '{}', \
                     expected type 'bool'",
                    bson_type_name(other)
                ),
            ))
        }
    }
    if ctx.db_name != "admin" {
        return Err(err(
            13,
            "Unauthorized",
            "listDatabases may only be run against the admin database.",
        ));
    }
    let filter = doc
        .get("filter")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());
    if let Some((code, msg)) = filter.and_then(argtypes::unknown_operator_in_filter) {
        return Err(err(code, &crate::util::error_code_name(code), msg));
    }
    let storage = ctx.storage()?;
    // A user without `listDatabases` on the cluster, or one asking for
    // `authorizedDatabases`, sees the databases it has any action in
    // (measured 8.2.11: `read` on `ae` lists `ae` alone, `readAnyDatabase`
    // every database but not through its cluster privilege alone).
    let authorized_only = crate::util::bool_field(doc, "authorizedDatabases", false);
    let visible = match crate::rbac::enforced(ctx)? {
        Some(az)
            if authorized_only
                || !az.is_authorized(&crate::privileges::Resource::Cluster, &["listDatabases"]) =>
        {
            Some(az)
        }
        _ => None,
    };
    let mut listed: Vec<Document> = Vec::new();
    let mut total: i64 = 0;
    // mongod applies a filter on `name` alone before it reads any size, and
    // takes the global lock in IS for each size it reads (measured 8.2.11,
    // 2026-10-24: one more per database listed, all of them for a filter on
    // anything else, none with `nameOnly`).
    let name_leaf = filter.filter(|f| f.len() == 1 && f.contains_key("name"));
    let mut sized = 0;
    for name in storage.list_databases().map_err(command_error)? {
        if visible
            .as_ref()
            .is_some_and(|az| !az.any_action_in_db(&name))
        {
            continue;
        }
        let entry = if name_only {
            doc! { "name": name }
        } else {
            if name_leaf.is_some_and(|f| {
                !secantus_core::query::matches(&doc! { "name": &name }, f, &Document::new(), None)
                    .unwrap_or(false)
            }) {
                continue;
            }
            sized += 1;
            let t = totals(ctx, &name)?;
            doc! {
                "name": name,
                "sizeOnDisk": Bson::Int64(t.data_size + t.index_size),
                // A database is empty when it holds no collection, not no bytes.
                "empty": t.collections == 0,
            }
        };
        let keep = filter.is_none_or(|f| {
            secantus_core::query::matches(&entry, f, &Document::new(), None).unwrap_or(false)
        });
        if keep {
            total += entry.get("sizeOnDisk").and_then(as_i64).unwrap_or(0);
            listed.push(entry);
        }
    }
    crate::lock_manager::global_reads(sized);
    let mut out = doc! { "databases": listed.into_iter().map(Bson::Document).collect::<Vec<_>>() };
    if !name_only {
        out.insert("totalSize", Bson::Int64(total));
        out.insert("totalSizeMb", Bson::Int64(total / (1024 * 1024)));
    }
    out.insert("ok", 1.0);
    Ok(out)
}

/// What `dropDatabase` refuses before it drops anything. Every one of these
/// used to drop the database.
pub(crate) fn drop_database_problem(doc: &Document) -> Result<(), CommandError> {
    match doc.get("dropDatabase") {
        None | Some(Bson::Null) => {
            return Err(err(
                40414,
                "IDLFailedToParse",
                "BSON field 'dropDatabase.dropDatabase' is missing but a required field",
            ))
        }
        Some(v @ (Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_))) => {
            let one = match v {
                Bson::Double(d) => *d == 1.0,
                other => as_i64(other) == Some(1),
            };
            if !one {
                return Err(err(
                    5_255_100,
                    "Location5255100",
                    "Have to pass 1 as 'drop' parameter",
                ));
            }
        }
        Some(other) => {
            return Err(err(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'dropDatabase.dropDatabase' is the wrong type '{}', expected \
                     types '[decimal, double, int, long]'",
                    bson_type_name(other)
                ),
            ))
        }
    }
    unknown_field(doc, "dropDatabase", &["dropDatabase"])
}
