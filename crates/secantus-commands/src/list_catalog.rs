//! `$listCatalog`: the catalog entry of every collection and view, built
//! from this server's own catalog (decided 2026-10-15).
//!
//! mongod 8.2.11 lists each collection as `{db, name, type: "collection",
//! md: {ns, options: {uuid, ...}, indexes: [{spec, ready, multikey,
//! multikeyPaths}]}, idxIdent, ident, ns}`, then each view (and each
//! time-series collection, as the view mongod keeps over its buckets) as
//! `{db, name, type, _id, viewOn, pipeline, ns}`. Here:
//!
//! - `ident` names the WiredTiger table that really holds the collection's
//!   documents: one of the shared `table:secantus_documents_sh<N>` shards,
//!   picked by a hash of `(db, collection)`. A `system.buckets.<name>` is the
//!   time-series collection's own table, and `system.views` the collection
//!   registry, which is where view definitions are kept. A set member's
//!   `local.oplog.rs` spans the oplog's shard tables, so no one table names
//!   it and it has no `ident`.
//! - `idxIdent` maps each index to the table of its entries: the `_id`
//!   index is the `_id -> RecordId` map, every other index the one shared
//!   entries table.
//! - `multikeyPaths` is left out: the store keeps a multikey flag per index,
//!   not mongod's durable record of which paths held an array. `multikey`
//!   is that flag.
//! - Collections come in this catalog's order (by database, then name), not
//!   mongod's creation order.

use crate::util::command_error;
use crate::CommandError;
use bson::{doc, Bson, Document};
use std::cell::Cell;

pub(crate) const STAGE: &str = "$listCatalog";

thread_local! {
    /// Whether the server running this thread's pipeline is a replica-set
    /// member, which has a `local.oplog.rs`.
    static SET_MEMBER: Cell<bool> = const { Cell::new(false) };
}

/// Say whether the server running this thread's pipeline is a set member.
pub(crate) fn set_member(member: bool) {
    SET_MEMBER.with(|m| m.set(member));
}

/// What mongod refuses first about a `$listCatalog` in `pipeline`
/// (8.2.11), before it looks at where the stage stands: a specification
/// that is not `{}` (6200600).
pub(crate) fn spec_problem(pipeline: &[Bson]) -> Result<(), CommandError> {
    for stage in pipeline {
        let Some(spec) = stage.as_document().and_then(|d| d.get(STAGE)) else {
            continue;
        };
        if !matches!(spec, Bson::Document(d) if d.is_empty()) {
            return Err(CommandError::new(
                6200600,
                "Location6200600",
                "The $listCatalog stage specification must be an empty object",
            ));
        }
    }
    Ok(())
}

/// Where mongod lets a `$listCatalog` stand (8.2.11): first (40602), and
/// collectionless only on `admin` (73).
pub(crate) fn place_problem(
    pipeline: &[Bson],
    db: &str,
    coll: Option<&str>,
) -> Result<(), CommandError> {
    let named = |stage: &Bson| stage.as_document().is_some_and(|d| d.contains_key(STAGE));
    if pipeline.iter().skip(1).any(named) {
        return Err(not_first());
    }
    match pipeline.first().filter(|stage| named(stage)) {
        None => Ok(()),
        Some(_) if coll.is_none() && db != "admin" => Err(CommandError::new(
            73,
            "InvalidNamespace",
            "Collectionless $listCatalog must be run against the 'admin' database with \
             {aggregate: 1}",
        )),
        Some(_) => Ok(()),
    }
}

/// mongod's refusal of a `$listCatalog` that is not the first stage, which
/// is also what reading one through a view gets: the view's own stages go
/// in front of it.
pub(crate) fn not_first() -> CommandError {
    CommandError::new(
        40602,
        "Location40602",
        "$listCatalog is only valid as the first stage in a pipeline",
    )
}

/// The stage's documents: every collection's and view's entry when `coll`
/// is `None` (run on `admin`), else the entry of collection `db.coll`, none
/// when there is no such collection.
pub(crate) fn entries(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: Option<&str>,
) -> Result<Vec<Document>, CommandError> {
    let set_member = SET_MEMBER.with(Cell::get);
    let databases = match coll {
        Some(_) => vec![db.to_string()],
        None => storage.list_databases().map_err(command_error)?,
    };
    let mut collections = Vec::new();
    let mut views = Vec::new();
    for db in &databases {
        let names = crate::admin::listed_collection_names(storage, db, set_member)?;
        let listed = crate::admin::collection_entries(storage, db, &names)?;
        for entry in &listed {
            let name = entry.get_str("name").unwrap_or_default();
            if coll.is_some_and(|c| c != name) {
                continue;
            }
            match entry.get_str("type") {
                Ok("collection") => {
                    collections.push(collection_entry(storage, db, entry, set_member)?)
                }
                _ if coll.is_some() => return Err(not_first()),
                _ => {}
            }
        }
        if coll.is_none() {
            views.extend(view_entries(storage, db, &listed)?);
        }
    }
    collections.extend(views);
    Ok(collections)
}

/// The entry of one collection, from its `listCollections` entry.
fn collection_entry(
    storage: &dyn crate::storage::Storage,
    db: &str,
    listed: &Document,
    set_member: bool,
) -> Result<Document, CommandError> {
    let name = listed.get_str("name").unwrap_or_default();
    let ns = format!("{db}.{name}");
    let mut options = Document::new();
    if let Some(uuid) = listed.get_document("info").ok().and_then(|i| i.get("uuid")) {
        options.insert("uuid", uuid.clone());
    }
    if let Ok(given) = listed.get_document("options") {
        options.extend(given.clone());
    }
    let oplog = db == "local" && name == "oplog.rs" && set_member;
    // A derived namespace's indexes are those of what it is derived from:
    // the time-series collection's for its buckets, and for `system.views`
    // the `_id` index its `listCollections` entry names.
    let (indexes, stored) = match crate::ts_buckets::bucketed(storage, db, name) {
        Some((base, options)) => {
            let indexes = match crate::ts_buckets::Layout::of(&options) {
                Some(layout) => crate::ts_bucket_ns::bucket_indexes(storage, db, &base, &layout)?,
                None => Vec::new(),
            };
            (indexes, base)
        }
        None => {
            let indexes = match crate::admin::listed_indexes(storage, db, name, oplog)? {
                Some(indexes) => indexes,
                None => listed
                    .get_document("idIndex")
                    .map(|ix| vec![ix.clone()])
                    .unwrap_or_default(),
            };
            (indexes, name.to_string())
        }
    };
    let mut md_indexes = Vec::new();
    let mut idx_ident = Document::new();
    for spec in indexes {
        // A clustered collection's key is the collection itself, not an
        // index: mongod lists none.
        if spec.get_bool("clustered") == Ok(true) {
            continue;
        }
        let index = spec.get_str("name").unwrap_or_default().to_string();
        let multikey = match spec.get_document("key") {
            Ok(key) if key.values().any(|v| matches!(v, Bson::String(_))) => {
                geo_multikey(storage, db, &stored, key)?
            }
            _ => storage.index_is_multikey(db, &stored, &index),
        };
        if let Some(table) = storage.index_ident(db, name, &index) {
            idx_ident.insert(index.clone(), table);
        }
        md_indexes.push(Bson::Document(doc! {
            "spec": spec,
            "ready": true,
            "multikey": multikey,
        }));
    }
    let mut md = doc! {
        "ns": &ns,
        "options": options,
        "indexes": md_indexes,
    };
    if name.starts_with(crate::ts_buckets::BUCKETS_PREFIX) {
        // Measurements are bucketed by type as mongod 8.2 does, so no
        // bucket holds two types for one field.
        md.insert("timeseriesBucketsMayHaveMixedSchemaData", false);
    }
    let mut entry = doc! {
        "db": db,
        "name": name,
        "type": "collection",
    };
    if db == "config" {
        entry.insert("configDebugDump", false);
    }
    entry.insert("md", md);
    entry.insert("idxIdent", idx_ident);
    if let Some(table) = storage.collection_ident(db, name) {
        entry.insert("ident", table);
    }
    entry.insert("ns", ns);
    Ok(entry)
}

/// The entries of `db`'s views and time-series collections, from the
/// definitions mongod keeps in `system.views`.
fn view_entries(
    storage: &dyn crate::storage::Storage,
    db: &str,
    listed: &[Document],
) -> Result<Vec<Document>, CommandError> {
    let mut out = Vec::new();
    for definition in crate::ts_buckets::view_definitions(storage, db)? {
        let id = definition.get_str("_id").unwrap_or_default().to_string();
        let name = id
            .strip_prefix(&format!("{db}."))
            .unwrap_or(&id)
            .to_string();
        let kind = listed
            .iter()
            .find(|e| e.get_str("name") == Ok(name.as_str()))
            .and_then(|e| e.get_str("type").ok())
            .unwrap_or("view")
            .to_string();
        let mut entry = doc! { "db": db, "name": &name, "type": kind };
        entry.extend(definition);
        entry.insert("ns", id);
        out.push(entry);
    }
    Ok(out)
}

/// Whether a geo index is multikey as mongod counts it, from the documents:
/// the store flags every geo index multikey for its own planner, where
/// mongod does so only for a document with several locations in a geo
/// field (`[[1, 2], [3, 4]]`, not the one pair `[1, 2]`) or an array on the
/// way to any key field. Unlike mongod's flag, this does not stay set once
/// those documents are gone.
fn geo_multikey(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    key: &Document,
) -> Result<bool, CommandError> {
    for blob in storage
        .find(db, coll, &Document::new(), None, None)
        .map_err(command_error)?
    {
        let Ok(doc) = secantus_core::bsondoc::from_bytes(&blob) else {
            continue;
        };
        for (field, kind) in key {
            let parts: Vec<&str> = field.split('.').collect();
            if multikey_at(
                &Bson::Document(doc.clone()),
                &parts,
                matches!(kind, Bson::String(_)),
            ) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Whether `value` holds an array that makes an index on `path` multikey.
/// At a geo field's end, an array is one location unless its elements are
/// themselves locations.
fn multikey_at(value: &Bson, path: &[&str], geo: bool) -> bool {
    match (value, path.split_first()) {
        (Bson::Array(items), None) if geo => items
            .iter()
            .any(|item| matches!(item, Bson::Array(_) | Bson::Document(_))),
        (Bson::Array(_), _) => true,
        (Bson::Document(d), Some((first, rest))) => {
            d.get(*first).is_some_and(|v| multikey_at(v, rest, geo))
        }
        _ => false,
    }
}
