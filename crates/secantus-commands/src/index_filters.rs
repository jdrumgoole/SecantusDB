//! Index filters: `planCacheSetFilter`, `planCacheListFilters` and
//! `planCacheClearFilters` (measured against mongod 8.2.11, 2026-10-20).
//!
//! A filter names the indexes the planner may consider for one query shape
//! of one collection: the query with its values left out (`{a: 1, b: 2}` and
//! `{b: 0, a: 3}` are one shape, `{a: {$in: [1, 2]}}` another, `{a: 1}` a
//! third), the sort, the projection and the collation. A read of that shape
//! plans over those indexes and the collection scan only, and its `explain`
//! says `indexFilterSet: true`. Filters live in memory, as mongod keeps
//! them: a restart, `planCacheClearFilters` or dropping the collection ends
//! them. Setting one for a shape that has one replaces it.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;

use bson::{doc, Bson, Document};

use crate::explain::IndexInfo;
use crate::{CommandContext, CommandError, HandlerResult};

#[derive(Clone)]
struct Filter {
    shape: Document,
    query: Document,
    sort: Document,
    projection: Document,
    collation: Option<Document>,
    indexes: Vec<Bson>,
}

/// One collection's filters, and the collection they were set on.
struct Set {
    uuid: Vec<u8>,
    filters: Vec<Filter>,
}

static FILTERS: Mutex<Option<HashMap<(String, String), Set>>> = Mutex::new(None);

fn bad(msg: &str) -> CommandError {
    CommandError::new(2, "BadValue", msg)
}

/// The collection's UUID, without minting one for a collection that does
/// not exist.
fn uuid_if_exists(storage: &dyn crate::storage::Storage, db: &str, coll: &str) -> Option<Vec<u8>> {
    if !storage.collection_exists(db, coll).unwrap_or(false) {
        return None;
    }
    storage.collection_uuid(db, coll).ok()
}

/// A match expression with its values left out.
fn abstracted(v: &Bson) -> Bson {
    match v {
        Bson::Document(d) => {
            Bson::Document(d.iter().map(|(k, v)| (k.clone(), abstracted(v))).collect())
        }
        Bson::Array(items) if items.iter().all(|i| matches!(i, Bson::Document(_))) => {
            Bson::Array(items.iter().map(abstracted).collect())
        }
        _ => Bson::Null,
    }
}

/// The shape a filter is keyed by and a read is matched on.
pub(crate) fn shape(
    query: &Document,
    sort: Option<&Document>,
    projection: Option<&Document>,
    collation: Option<&Document>,
) -> Document {
    let parsed = crate::explain::parsed_query(query);
    let mut out = doc! {
        "q": abstracted(&Bson::Document(parsed)),
        "s": sort.cloned().unwrap_or_default(),
        "p": projection.cloned().unwrap_or_default(),
    };
    if let Some(c) = collation {
        out.insert("c", c.clone());
    }
    out
}

/// The indexes a filter allows a read of this shape, if one is set.
pub(crate) fn allowed(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    query: &Document,
    sort: Option<&Document>,
    projection: Option<&Document>,
    collation: Option<&Document>,
) -> Option<Vec<Bson>> {
    let mut guard = FILTERS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.as_mut()?;
    let key = (db.to_string(), coll.to_string());
    let set = map.get(&key)?;
    // A collection dropped (and perhaps made again) since has no filters.
    if uuid_if_exists(storage, db, coll).as_ref() != Some(&set.uuid) {
        map.remove(&key);
        return None;
    }
    let wanted = shape(query, sort, projection, collation);
    set.filters
        .iter()
        .find(|f| f.shape == wanted)
        .map(|f| f.indexes.clone())
}

/// `catalog` cut to the indexes `allowed` names, by name or key pattern.
pub(crate) fn restrict(catalog: Vec<Rc<IndexInfo>>, allowed: &[Bson]) -> Vec<Rc<IndexInfo>> {
    catalog
        .into_iter()
        .filter(|ix| {
            allowed.iter().any(|a| match a {
                Bson::String(name) => *name == ix.name,
                Bson::Document(key) => *key == ix.key,
                _ => false,
            })
        })
        .collect()
}

fn collection_of(doc: &Document, command: &str) -> Result<String, CommandError> {
    match doc.get(command) {
        Some(Bson::String(s)) => Ok(s.clone()),
        Some(other) => Err(CommandError::new(
            73,
            "InvalidNamespace",
            format!(
                "Collection name has invalid type {}",
                secantus_core::query::bson_type_name(other)
            ),
        )),
        None => Err(bad("no collection")),
    }
}

fn optional_object<'a>(doc: &'a Document, key: &str) -> Result<Option<&'a Document>, CommandError> {
    match doc.get(key) {
        None => Ok(None),
        Some(Bson::Document(d)) => Ok(Some(d)),
        Some(_) => Err(bad(&format!("optional field {key} must be an object"))),
    }
}

/// The collation a filter keeps: mongod's full spec of it.
fn collation_spec(doc: &Document) -> Result<Option<Document>, CommandError> {
    optional_object(doc, "collation")?;
    Ok(match crate::util::requested_collation(doc)? {
        crate::util::Requested::Given(c) => Some(c.spec().clone()),
        _ => None,
    })
}

/// `planCacheSetFilter`.
pub fn set_filter(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = collection_of(doc, "planCacheSetFilter")?;
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    if crate::views::is_view(storage, &db, &coll) {
        return Err(CommandError::new(
            166,
            "CommandNotSupportedOnView",
            format!("Namespace {db}.{coll} is a view, not a collection"),
        ));
    }
    let query = match doc.get("query") {
        Some(Bson::Document(q)) => q.clone(),
        Some(_) => return Err(bad("required field query must be an object")),
        None => return Err(bad("required field query missing")),
    };
    let sort = optional_object(doc, "sort")?.cloned().unwrap_or_default();
    let projection = optional_object(doc, "projection")?
        .cloned()
        .unwrap_or_default();
    let collation = collation_spec(doc)?;
    let indexes = match doc.get("indexes") {
        None => return Err(bad("required field indexes missing")),
        Some(Bson::Array(a)) if a.is_empty() => {
            return Err(bad(
                "required field indexes must contain at least one index",
            ))
        }
        Some(Bson::Array(a)) => {
            if a.iter()
                .any(|i| !matches!(i, Bson::Document(_) | Bson::String(_)))
            {
                return Err(bad("each item in indexes must be an object or string"));
            }
            a.clone()
        }
        Some(_) => return Err(bad("required field indexes must be an array")),
    };
    if !storage.collection_exists(&db, &coll).unwrap_or(false) {
        return Err(bad("no such collection"));
    }
    let uuid = storage
        .collection_uuid(&db, &coll)
        .map_err(|_| bad("no such collection"))?;
    let filter = Filter {
        shape: shape(&query, Some(&sort), Some(&projection), collation.as_ref()),
        query,
        sort,
        projection,
        collation,
        indexes,
    };
    let mut guard = FILTERS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    let set = map.entry((db, coll)).or_insert_with(|| Set {
        uuid: uuid.clone(),
        filters: Vec::new(),
    });
    if set.uuid != uuid {
        set.uuid = uuid;
        set.filters.clear();
    }
    set.filters.retain(|f| f.shape != filter.shape);
    set.filters.push(filter);
    Ok(doc! {"ok": 1.0})
}

/// The filters set on `db.coll` now (none for a collection dropped since).
fn current(storage: &dyn crate::storage::Storage, db: &str, coll: &str) -> Vec<Filter> {
    let mut guard = FILTERS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = guard.as_mut() else {
        return Vec::new();
    };
    let key = (db.to_string(), coll.to_string());
    let Some(set) = map.get(&key) else {
        return Vec::new();
    };
    if uuid_if_exists(storage, db, coll).as_ref() != Some(&set.uuid) {
        map.remove(&key);
        return Vec::new();
    }
    set.filters.clone()
}

/// `planCacheListFilters`.
pub fn list_filters(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = collection_of(doc, "planCacheListFilters")?;
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    if crate::views::is_view(storage, &db, &coll) {
        return Err(CommandError::new(
            166,
            "CommandNotSupportedOnView",
            format!("Namespace {db}.{coll} is a view, not a collection"),
        ));
    }
    let listed: Vec<Bson> = current(storage, &db, &coll)
        .into_iter()
        .map(|f| {
            let mut d = doc! { "query": f.query, "sort": f.sort, "projection": f.projection };
            if let Some(c) = f.collation {
                d.insert("collation", c);
            }
            d.insert("indexes", f.indexes);
            Bson::Document(d)
        })
        .collect();
    Ok(doc! {"filters": listed, "ok": 1.0})
}

/// `planCacheClearFilters`: the filter of one shape, or every filter of the
/// collection when no `query` is given.
pub fn clear_filters(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = collection_of(doc, "planCacheClearFilters")?;
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    if crate::views::is_view(storage, &db, &coll) {
        return Err(CommandError::new(
            166,
            "CommandNotSupportedOnView",
            format!("Namespace {db}.{coll} is a view, not a collection"),
        ));
    }
    let query = match doc.get("query") {
        None => None,
        Some(Bson::Document(q)) => Some(q.clone()),
        Some(_) => return Err(bad("optional field query must be an object")),
    };
    let sort = optional_object(doc, "sort")?.cloned();
    let projection = optional_object(doc, "projection")?.cloned();
    let collation = collation_spec(doc)?;
    if query.is_none() && (sort.is_some() || projection.is_some() || collation.is_some()) {
        return Err(bad("sort, projection, or collation provided without query"));
    }
    current(storage, &db, &coll);
    let mut guard = FILTERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(set) = guard
        .as_mut()
        .and_then(|m| m.get_mut(&(db.clone(), coll.clone())))
    {
        match query {
            None => set.filters.clear(),
            Some(q) => {
                let wanted = shape(&q, sort.as_ref(), projection.as_ref(), collation.as_ref());
                set.filters.retain(|f| f.shape != wanted);
            }
        }
    }
    Ok(doc! {"ok": 1.0})
}

/// `catalog` as a read of this shape may plan over it: cut to the filter's
/// indexes when one is set, with whether one was.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    query: &Document,
    sort: Option<&Document>,
    projection: Option<&Document>,
    collation: Option<&secantus_core::collation::Collation>,
    catalog: Vec<Rc<IndexInfo>>,
) -> (Vec<Rc<IndexInfo>>, bool) {
    let spec = collation.map(|c| c.spec().clone());
    match allowed(storage, db, coll, query, sort, projection, spec.as_ref()) {
        Some(indexes) => (restrict(catalog, &indexes), true),
        None => (catalog, false),
    }
}
