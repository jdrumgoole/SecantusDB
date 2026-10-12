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
        t.objects += storage
            .count_matching(db, &c, &Document::new())
            .map_err(command_error)? as i64;
        t.data_size += storage
            .collection_data_size(db, &c)
            .map_err(command_error)?;
        let sizes = storage.index_sizes(db, &c).map_err(command_error)?;
        t.indexes += storage
            .list_indexes(db, &c)
            .map_or(sizes.len(), |l| l.len()) as i64;
        t.index_size += sizes.values().filter_map(as_i64).sum::<i64>();
    }
    // The views are documents in `system.views`, a collection with an `_id` index.
    if t.views > 0 {
        t.collections += 1;
        t.objects += t.views;
        t.indexes += 1;
    }
    Ok(t)
}

/// `dbStats`.
pub fn db_stats(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    unknown_field(doc, "dbStats", &["dbStats", "scale", "freeStorage"])?;
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
    // The filesystem's own numbers are not this server's to report.
    out.insert("fsUsedSize", 0.0);
    out.insert("fsTotalSize", 0.0);
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
    let exists = storage.collection_exists(&db, &coll).unwrap_or(false);
    let count = storage
        .count_matching(&db, &coll, &Document::new())
        .map_err(command_error)? as i64;
    let size = storage
        .collection_data_size(&db, &coll)
        .map_err(command_error)?;
    let measured = storage.index_sizes(&db, &coll).map_err(command_error)?;
    // Every index is listed, one that holds no entry yet included.
    let mut sizes = Document::new();
    for index in storage.list_indexes(&db, &coll).unwrap_or_default() {
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
    let mut out = doc! { "ns": ns, "size": int(size / scale), "count": int(count) };
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
        out.insert("ok", 1.0);
        return Ok(out);
    }
    out.insert("freeStorageSize", 0_i32);
    let capped = storage
        .collection_is_capped(&db, &coll)
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
    out.insert("ok", 1.0);
    Ok(out)
}

/// `dataSize`: the bytes and documents of one namespace.
pub fn data_size(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let ns = match doc.get("dataSize") {
        Some(Bson::String(ns)) => ns.clone(),
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
        None => String::new(),
    };
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
    let storage = ctx.storage()?;
    let exists = storage.collection_exists(db, coll).unwrap_or(false);
    let count = storage
        .count_matching(db, coll, &Document::new())
        .map_err(command_error)? as i64;
    let size = storage
        .collection_data_size(db, coll)
        .map_err(command_error)?;
    let mut out = doc! {
        "size": Bson::Int64(size),
        "numObjects": Bson::Int64(count),
        "millis": Bson::Int64(0),
    };
    if exists {
        out.insert("estimate", false);
    }
    out.insert("ok", 1.0);
    Ok(out)
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
    let mut listed: Vec<Document> = Vec::new();
    let mut total: i64 = 0;
    for name in storage.list_databases().map_err(command_error)? {
        let entry = if name_only {
            doc! { "name": name }
        } else {
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
