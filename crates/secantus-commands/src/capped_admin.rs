//! `convertToCapped`, `cloneCollectionAsCapped`, `compact` and `reIndex`.
//!
//! The first two copy a collection into a capped one: its options carry
//! over, its documents go in insertion order until the cap evicts the oldest,
//! and only the `_id` index survives. `compact` and `reIndex` have nothing to
//! do on this storage and answer as `mongod` does when there was nothing to
//! free or rebuild; on a replica-set member, which is what this server says
//! it is unless started standalone, `mongod` refuses both. Every code and
//! message was measured against `mongod` 8.2.11, as a replica-set member and
//! standalone.

use bson::{doc, Bson, Document};

use crate::util::{as_i64, bool_field, command_error};
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

/// `compact`. There is never anything to free here.
pub fn compact(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = named(doc, "compact")?;
    if ctx.replica_set_name.is_some() && !bool_field(doc, "force", false) {
        return Err(CommandError::new(
            20,
            "IllegalOperation",
            "will not run compact on an active replica set primary as this will slow down other \
             running operations. use force:true to force",
        ));
    }
    let storage = ctx.storage()?;
    if crate::views::is_view(storage, &ctx.db_name, &coll) {
        return Err(CommandError::new(
            166,
            "CommandNotSupportedOnView",
            "can't compact a view",
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
    Ok(doc! { "bytesFreed": 0_i32, "ok": 1.0 })
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
