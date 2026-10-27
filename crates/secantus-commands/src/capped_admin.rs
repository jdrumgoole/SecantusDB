//! `convertToCapped`, `cloneCollectionAsCapped`, `compact` and `reIndex`.
//!
//! The first two copy a collection into a capped one: its options carry
//! over, its documents go in insertion order until the cap evicts the oldest,
//! and only the `_id` index survives. `compact` gives the free space in the
//! files a collection is kept in back to the file system, and `autoCompact`
//! runs that in the background; `reIndex` has nothing to rebuild on this
//! storage and answers as `mongod` does when there was nothing to rebuild.
//! On a replica-set member, which is what this server says it is unless
//! started standalone, `mongod` refuses `reIndex` and wants `force` for
//! `compact`. Every code and message was measured against `mongod` 8.2.11,
//! as a replica-set member and standalone.

use bson::{doc, Bson, Document};

use crate::storage::{AutoCompactReply, CompactReply, CompactTarget};
use crate::util::{as_i64, command_error};
use crate::{CommandContext, CommandError, HandlerResult};

fn invalid_namespace(message: impl Into<String>) -> CommandError {
    CommandError::new(73, "InvalidNamespace", message)
}

/// The collection a command names, where `mongod` reports any other type by
/// its name.
fn named(doc: &Document, command: &str) -> Result<String, CommandError> {
    match doc.get(command) {
        Some(Bson::String(name)) => Ok(name.clone()),
        Some(other) => Err(invalid_namespace(format!(
            "Collection name has invalid type {}",
            secantus_core::query::bson_type_name(other)
        ))),
        None => Err(invalid_namespace(
            "Collection name has invalid type missing",
        )),
    }
}

/// The cap in bytes. Anything that is not a positive number is refused, and
/// before the collection is looked at.
fn cap(doc: &Document) -> Result<i64, CommandError> {
    match doc.get("size").and_then(as_i64) {
        Some(size) if size > 0 && !matches!(doc.get("size"), Some(Bson::Boolean(_))) => Ok(size),
        _ => Err(CommandError::new(
            72,
            "InvalidOptions",
            "Capped collection size must be greater than zero",
        )),
    }
}

/// Whether `db.coll` is a view, or a time-series collection, which `mongod`
/// keeps as one.
fn is_view(storage: &dyn crate::storage::Storage, db: &str, coll: &str) -> bool {
    storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("viewOn") || o.contains_key("timeseries"))
}

fn a_view(db: &str, coll: &str) -> CommandError {
    CommandError::new(
        166,
        "CommandNotSupportedOnView",
        format!("Namespace {db}.{coll} is a view, not a collection"),
    )
}

/// Why `db.coll` cannot be the source of a capped copy.
fn source_problem(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> Result<(), CommandError> {
    if is_view(storage, db, coll) {
        return Err(a_view(db, coll));
    }
    if !storage.collection_exists(db, coll).map_err(command_error)? {
        return Err(CommandError::new(
            26,
            "NamespaceNotFound",
            format!("source collection {db}.{coll} does not exist"),
        ));
    }
    Ok(())
}

/// Copy `db.from` into a new capped collection `db.to`.
fn copy_capped(
    storage: &dyn crate::storage::Storage,
    db: &str,
    from: &str,
    to: &str,
    size: i64,
) -> Result<(), CommandError> {
    let mut options = storage
        .get_collection_options(db, from)
        .map_err(command_error)?;
    options.remove("uuid");
    // `max` is not taken: the copy is capped by size alone.
    options.remove("max");
    options.insert("capped", true);
    options.insert("size", crate::admin::int_bson(size));
    storage
        .create_collection_with_options(db, to, &options)
        .map_err(command_error)?;
    let documents = storage
        .find(db, from, &Document::new(), None, None)
        .map_err(command_error)?;
    if documents.is_empty() {
        return Ok(());
    }
    crate::aggregate::insert_or_fail(storage, db, to, documents)
}

/// `cloneCollectionAsCapped`.
pub fn clone_collection_as_capped(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let must_be_string = |field: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!("'{field}' must be of type String"),
        )
    };
    let Some(Bson::String(from)) = doc.get("cloneCollectionAsCapped") else {
        return Err(must_be_string("cloneCollectionAsCapped"));
    };
    let Some(Bson::String(to)) = doc.get("toCollection") else {
        return Err(must_be_string("toCollection"));
    };
    if from.is_empty() {
        return Err(invalid_namespace("Invalid source collection name: "));
    }
    if to.is_empty() {
        return Err(invalid_namespace("Invalid target collection name: "));
    }
    let size = cap(doc)?;
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    source_problem(storage, &db, from)?;
    if is_view(storage, &db, to) {
        return Err(a_view(&db, to));
    }
    if storage.collection_exists(&db, to).map_err(command_error)? {
        return Err(CommandError::new(
            48,
            "NamespaceExists",
            format!(
                "cloneCollectionAsCapped failed - destination collection {db}.{to} already \
                 exists. source collection: {db}.{from}"
            ),
        ));
    }
    copy_capped(storage, &db, from, to, size)?;
    Ok(doc! { "ok": 1.0 })
}

/// `convertToCapped`: a capped copy, swapped in under the collection's name.
pub fn convert_to_capped(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = named(doc, "convertToCapped")?;
    let db = ctx.db_name.clone();
    if coll.is_empty() {
        return Err(invalid_namespace(format!(
            "Invalid namespace specified '{db}'"
        )));
    }
    let size = cap(doc)?;
    let storage = ctx.storage()?;
    source_problem(storage, &db, &coll)?;
    let tmp = format!(
        "tmp{}.convertToCapped.{coll}",
        &bson::oid::ObjectId::new().to_hex()[18..]
    );
    // mongod profiles the conversion under the collection it copies into,
    // and cannot name one for `system.profile` (measured 8.2.11).
    crate::profiler::note_namespace(format!("{db}.{tmp}"));
    if coll == crate::profiler::PROFILE_COLLECTION {
        return Err(CommandError::new(
            2,
            "BadValue",
            format!("Invalid namespace: {db}.{tmp}"),
        ));
    }
    if let Err(e) = copy_capped(storage, &db, &coll, &tmp, size) {
        storage.drop_collection(&db, &tmp).map_err(command_error)?;
        return Err(e);
    }
    // The collection's open cursors read documents the swap is about to
    // replace.
    if let Ok(cursors) = ctx.cursors() {
        cursors.kill_namespace(
            &format!("{db}.{coll}"),
            &crate::admin::dropped_reason(storage, &db, &coll),
        );
    }
    let (renamed, why) = storage
        .rename_collection(&db, &tmp, &db, &coll, true)
        .map_err(command_error)?;
    if !renamed {
        storage.drop_collection(&db, &tmp).map_err(command_error)?;
        return Err(CommandError::new(
            8,
            "UnknownError",
            format!(
                "convertToCapped could not replace {db}.{coll}: {}",
                why.unwrap_or_default()
            ),
        ));
    }
    Ok(doc! { "ok": 1.0 })
}

/// `compact`: give the collection's free disk space back
/// (`secantus_storage::compact`, which says what is compacted where
/// collections share their tables).
///
/// The options are parsed with the command's other fields
/// (`generic::own_field`): `force` and `dryRun` booleans,
/// `freeSpaceTargetMB` a whole number of at least 1. Then, as mongod 8.2.11
/// does (measured as a replica-set member and standalone): a primary needs
/// `force: true`; the collection must exist (a view is refused by name); a
/// time-series collection is compacted through either of its names, as are
/// the oplog (`local.oplog.rs`, on a replica-set member) and `system.views`
/// (when the database has a view). A second `compact` of the same
/// collection while one runs is refused. The reply is `bytesFreed`, or
/// `estimatedBytesFreed` for a dry run.
pub fn compact(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = named(doc, "compact")?;
    let set = |key: &str| matches!(doc.get(key), Some(Bson::Boolean(true)));
    let dry_run = set("dryRun");
    let free_space_target_mb = match doc.get("freeSpaceTargetMB") {
        None | Some(Bson::Null) => None,
        Some(v) => Some(crate::generic::free_space_target_mb("compact", v)?),
    };
    if ctx.replica_set_name.is_some() && !set("force") {
        return Err(CommandError::new(
            20,
            "IllegalOperation",
            "will not run compact on an active replica set primary as this will slow down other \
             running operations. use force:true to force",
        ));
    }
    let storage = ctx.storage()?;
    let db = ctx.db_name.clone();
    let not_found = || CommandError::new(26, "NamespaceNotFound", "collection does not exist");
    let base = coll.strip_prefix(crate::BUCKETS_PREFIX);
    let target = if db == "local" && coll == "oplog.rs" {
        if ctx.replica_set_name.is_none() {
            return Err(not_found());
        }
        CompactTarget::Oplog
    } else if let Some(base) = base.filter(|b| {
        storage
            .get_collection_options(&db, b)
            .is_ok_and(|o| o.contains_key("timeseries"))
    }) {
        CompactTarget::Collection(&db, base)
    } else if coll == "system.views"
        && !storage
            .collection_exists(&db, &coll)
            .map_err(command_error)?
        && !crate::ts_buckets::view_definitions(storage, &db)?.is_empty()
    {
        CompactTarget::Catalog
    } else if crate::views::is_view(storage, &db, &coll) {
        return Err(CommandError::new(
            166,
            "CommandNotSupportedOnView",
            "can't compact a view",
        ));
    } else if storage
        .collection_exists(&db, &coll)
        .map_err(command_error)?
    {
        CompactTarget::Collection(&db, &coll)
    } else {
        return Err(not_found());
    };
    let freed = match storage.compact(target, dry_run, free_space_target_mb) {
        None => 0,
        Some(Ok(CompactReply::Freed(n))) => n,
        Some(Ok(CompactReply::AlreadyRunning)) => {
            return Err(CommandError::new(
                96,
                "OperationFailed",
                format!("Compaction is already in progress for {db}.{coll}"),
            ))
        }
        Some(Ok(CompactReply::Busy(table))) => {
            return Err(CommandError::new(
                11601,
                "Interrupted",
                format!("Compaction interrupted on {table}"),
            ))
        }
        Some(Err(e)) => return Err(command_error(e)),
    };
    // mongod's `appendNumber`: an int when it fits, a long when not.
    let freed = i32::try_from(freed).map_or(Bson::Int64(freed), Bson::Int32);
    let key = if dry_run {
        "estimatedBytesFreed"
    } else {
        "bytesFreed"
    };
    Ok(doc! { key: freed, "ok": 1.0 })
}

/// `autoCompact`: WiredTiger's background compaction on or off
/// (`secantus_storage::auto_compact`). The switch and the options are
/// parsed first (`generic`), then it is refused from any database but
/// `admin`; enabling it while it runs with another `freeSpaceTargetMB` or
/// `runOnce` is 23 AlreadyInitialized, as on mongod 8.2.11, a change sent
/// before the server has taken up the last one is 314 ObjectIsBusy, and a
/// store held in memory refuses it as mongod's in-memory engine does.
pub fn auto_compact(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let enable = matches!(doc.get("autoCompact"), Some(Bson::Boolean(true)));
    let run_once = matches!(doc.get("runOnce"), Some(Bson::Boolean(true)));
    let free_space_target_mb = match doc.get("freeSpaceTargetMB") {
        None | Some(Bson::Null) => None,
        Some(v) => Some(crate::generic::free_space_target_mb("autoCompact", v)?),
    };
    if ctx.db_name != "admin" {
        return Err(CommandError::new(
            13,
            "Unauthorized",
            "autoCompact may only be run against the admin database.",
        ));
    }
    let storage = ctx.storage()?;
    match storage.auto_compact(enable, run_once, free_space_target_mb) {
        None | Some(Ok(AutoCompactReply::InMemory)) => Err(CommandError::new(
            20,
            "IllegalOperation",
            "autoCompact() cannot be executed for in-memory configurations",
        )),
        Some(Ok(AutoCompactReply::Done)) => Ok(doc! { "ok": 1.0 }),
        Some(Ok(AutoCompactReply::AlreadyRunning)) => Err(CommandError::new(
            23,
            "AlreadyInitialized",
            "Cannot reconfigure background compaction while it's already running.",
        )),
        // mongod's `wtRCToStatus` text: its prefix, the code, the C
        // library's words for it, and WiredTiger's message.
        Some(Ok(AutoCompactReply::Busy(text))) => Err(CommandError::new(
            314,
            "ObjectIsBusy",
            format!(
                "Failed to configure auto compact 16: {text} - Background compact is busy \
                 processing a previous command"
            ),
        )),
        Some(Err(e)) => Err(command_error(e)),
    }
}

/// `reIndex`. The indexes are always current, so the reply lists them.
pub fn re_index(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = named(doc, "reIndex")?;
    if ctx.replica_set_name.is_some() {
        return Err(CommandError::new(
            20,
            "IllegalOperation",
            format!(
                "reIndex is only allowed on a standalone mongod instance. Cannot reIndex \
                 '{}.{coll}' while replication is active",
                ctx.db_name
            ),
        ));
    }
    let storage = ctx.storage()?;
    if crate::views::is_view(storage, &ctx.db_name, &coll) {
        return Err(CommandError::new(
            166,
            "CommandNotSupportedOnView",
            "can't re-index a view",
        ));
    }
    if !storage
        .collection_exists(&ctx.db_name, &coll)
        .map_err(command_error)?
    {
        return Err(CommandError::new(
            26,
            "NamespaceNotFound",
            "collection does not exist",
        ));
    }
    let indexes: Vec<Bson> = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .into_iter()
        .map(|mut ix| {
            for internal in crate::admin::INDEX_CATALOG_ONLY {
                ix.remove(*internal);
            }
            ix.remove("ns");
            Bson::Document(ix)
        })
        .collect();
    let n = indexes.len() as i32;
    Ok(doc! { "nIndexesWas": n, "nIndexes": n, "indexes": indexes, "ok": 1.0 })
}
