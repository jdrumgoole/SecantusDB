//! mongod's classic plan cache (8.2.11), per collection.
//!
//! mongod runs its trial of candidate plans (`super::trial`) once for a
//! query shape and keeps the winner, keyed by `super::cache_key`. The first
//! trial writes an INACTIVE entry holding the winner's `works`; the next
//! trial of that shape that takes no more works activates it, one that
//! takes more doubles the bar (`internalQueryCacheWorksGrowthCoefficient`,
//! 2) and keeps it inactive. A query that finds an ACTIVE entry runs its
//! plan without a trial, as long as the plan hands over its first batch or
//! runs out within ten times those works (`internalQueryCacheEvictionRatio`);
//! one that does not sends the entry inactive and the query to a trial whose
//! winner is written whatever happens (`CachedPlanStage::replan`). A branch
//! of a rooted `$or` is planned and cached on its own, and takes an active
//! entry's plan as it is (the subplanner), and its trial writes nothing when
//! the best plans tie or the winner returned nothing.
//!
//! The cache is an LRU of `internalQueryCacheMaxEntriesPerCollection`
//! (5000) entries, the latest used first, which is also the order
//! `$planCacheStats` lists them in. It is cleared when the collection's
//! indexes change -- one built, dropped, hidden or unhidden, or made
//! multikey -- and by `planCacheClear`; a collection dropped and made again
//! starts empty. Measured against mongod 8.2.11 (2026-10-27,
//! `tools/probes/plan_cache.py`).
//!
//! In memory, as mongod keeps it: a restart empties it.

use bson::{doc, Bson, Document};
use std::collections::HashMap;
use std::sync::Mutex;

use super::cache_key::CacheKey;

/// `internalQueryCacheMaxEntriesPerCollection`.
const MAX_ENTRIES: usize = 5000;
/// `internalQueryCacheWorksGrowthCoefficient`.
const GROWTH: f64 = 2.0;

/// One cache entry, with what `$planCacheStats` reports of it.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub key: String,
    pub shape_hash: u32,
    pub key_hash: u32,
    /// The shape `planCacheClear` names (`encodeForPlanCacheCommand`).
    pub command_shape: String,
    pub active: bool,
    pub works: i64,
    /// `worksType: "reads"`: the plan runs in SBE, and `works` counts the
    /// keys and documents its trial read.
    pub reads: bool,
    pub created: bson::DateTime,
    pub created_from: Document,
    /// The cached plan's structure (`plan::signature`).
    pub signature: String,
    /// The cached solution (`plan::solution`), which `explain` marks
    /// `isCached`.
    pub solution: String,
    pub cached_plan: Document,
    pub creation: Vec<Document>,
    pub scores: Vec<f64>,
    pub index_filter_set: bool,
    pub solution_hash: i64,
}

/// One collection's cache, and the collection and indexes it was made for.
struct Collection {
    uuid: Vec<u8>,
    catalog: String,
    /// The latest used first.
    entries: Vec<Entry>,
}

/// Each store's collections, by the store's address, database and name.
type Caches = HashMap<(usize, String, String), Collection>;

static CACHES: Mutex<Option<Caches>> = Mutex::new(None);

/// Which collection a cache belongs to: the store (by address), the
/// namespace, the collection's UUID and its indexes as the planner sees
/// them. A different UUID or catalog empties the cache.
pub(crate) struct Place<'a> {
    pub store: usize,
    pub db: &'a str,
    pub coll: &'a str,
    pub uuid: Vec<u8>,
    pub catalog: String,
}

fn with<T>(place: &Place, f: impl FnOnce(&mut Collection) -> T) -> T {
    let mut guard = CACHES.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    let key = (place.store, place.db.to_string(), place.coll.to_string());
    let cache = map.entry(key).or_insert_with(|| Collection {
        uuid: place.uuid.clone(),
        catalog: place.catalog.clone(),
        entries: Vec::new(),
    });
    if cache.uuid != place.uuid || cache.catalog != place.catalog {
        cache.uuid = place.uuid.clone();
        cache.catalog = place.catalog.clone();
        cache.entries.clear();
    }
    f(cache)
}

/// The entry for `key`, moved to the front (`LRUKeyValue::get`).
pub(crate) fn get(place: &Place, key: &CacheKey) -> Option<Entry> {
    with(place, |c| {
        let at = c.entries.iter().position(|e| e.key == key.key)?;
        let entry = c.entries.remove(at);
        c.entries.insert(0, entry.clone());
        Some(entry)
    })
}

/// `PlanCache::deactivate`.
pub(crate) fn deactivate(place: &Place, key: &CacheKey) {
    with(place, |c| {
        if let Some(at) = c.entries.iter().position(|e| e.key == key.key) {
            let mut entry = c.entries.remove(at);
            entry.active = false;
            c.entries.insert(0, entry);
        }
    });
}

/// `PlanCache::set`: what a trial's winner does to the entry for its shape
/// (`getNewEntryState`). `new.works` is the trial's.
pub(crate) fn set(place: &Place, mut new: Entry) {
    with(place, |c| {
        let old = c
            .entries
            .iter()
            .position(|e| e.key == new.key)
            .map(|at| c.entries.remove(at));
        match old {
            None => new.active = false,
            Some(old) if old.active && new.works <= old.works => new.active = true,
            Some(old) if old.active => {
                // An active entry with fewer works stays as it is.
                c.entries.insert(0, old);
                return;
            }
            Some(old) if new.works > old.works => {
                // Worse than the inactive entry expected: the bar rises.
                let grown = ((old.works as f64) * GROWTH) as i64;
                new.works = grown.max(old.works + 1);
                new.active = false;
            }
            Some(_) => new.active = true,
        }
        c.entries.insert(0, new);
        c.entries.truncate(MAX_ENTRIES);
    });
}

/// `planCacheClear`: every entry, or those of one command shape.
pub(crate) fn clear(place: &Place, command_shape: Option<&str>) {
    with(place, |c| match command_shape {
        None => c.entries.clear(),
        Some(shape) => c.entries.retain(|e| e.command_shape != shape),
    });
}

/// The entries, the latest used first.
pub(crate) fn entries(place: &Place) -> Vec<Entry> {
    with(place, |c| c.entries.clone())
}

/// The plan's `solutionHash`. mongod's is `absl::Hash` of the solution,
/// seeded per process: two equal plans hash alike within one run, and
/// differently from one run to the next. So is this.
pub(crate) fn solution_hash(solution: &str) -> i64 {
    use std::hash::{BuildHasher, Hasher};
    static SEED: std::sync::OnceLock<std::collections::hash_map::RandomState> =
        std::sync::OnceLock::new();
    let mut h = SEED
        .get_or_init(std::collections::hash_map::RandomState::new)
        .build_hasher();
    h.write(solution.as_bytes());
    h.finish() as i64
}

/// The entry as `$planCacheStats` lists it (`Explain::planCacheEntryToBSON`
/// for the classic cache), before the stage adds `host`.
pub(crate) fn to_doc(e: &Entry) -> Document {
    let shape = super::cache_key::hex(e.shape_hash);
    doc! {
        "version": "1",
        "queryHash": shape.clone(),
        "planCacheShapeHash": shape,
        "planCacheKey": super::cache_key::hex(e.key_hash),
        "isActive": e.active,
        "works": e.works,
        "worksType": if e.reads { "reads" } else { "works" },
        "timeOfCreation": e.created,
        "createdFromQuery": e.created_from.clone(),
        "cachedPlan": e.cached_plan.clone(),
        "creationExecStats": e.creation.iter().cloned().map(Bson::Document).collect::<Vec<_>>(),
        "candidatePlanScores": e.scores.clone(),
        "indexFilterSet": e.index_filter_set,
        "solutionHash": e.solution_hash,
    }
}

/// The cache of collection `db.coll` of `storage`, as its reads key it.
/// `None` when there is no such collection.
pub(crate) fn place_of<'a>(
    storage: &dyn crate::storage::Storage,
    db: &'a str,
    coll: &'a str,
) -> Result<Option<Place<'a>>, crate::storage::StorageError> {
    if !storage.collection_exists(db, coll)? {
        return Ok(None);
    }
    let Ok(uuid) = storage.collection_uuid(db, coll) else {
        return Ok(None);
    };
    let catalog = super::catalog(storage, db, coll, None)?;
    Ok(Some(Place {
        store: super::order::store_address(storage),
        db,
        coll,
        uuid,
        catalog: fingerprint(storage, db, coll, &catalog)?,
    }))
}

/// The collection's indexes as the cache is cleared by: every index,
/// hidden ones too, and which of their paths hold arrays. mongod clears the
/// cache when an index is built, dropped, hidden or unhidden, and when one
/// becomes multikey.
pub(crate) fn fingerprint(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    catalog: &[std::rc::Rc<super::IndexInfo>],
) -> Result<String, crate::storage::StorageError> {
    let specs = storage.list_indexes(db, coll)?;
    Ok(format!("{specs:?}{catalog:?}"))
}

/// `planCacheClear` (8.2.11, `plan_cache_clear_command.cpp`): every entry
/// of the collection, or with `query` (and `sort`, `projection`,
/// `collation`) those of that shape. A collection that does not exist has
/// nothing to clear.
pub(crate) fn clear_command(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    cmd: &Document,
) -> Result<(), crate::CommandError> {
    let bad = |m: &str| crate::CommandError::new(2, "BadValue", m);
    let shape = if let Some(query) = cmd.get("query") {
        let Bson::Document(query) = query else {
            return Err(bad("required field query must be an object"));
        };
        let object = |field: &str| -> Result<Option<&Document>, crate::CommandError> {
            match cmd.get(field) {
                None => Ok(None),
                Some(Bson::Document(d)) => Ok(Some(d)),
                Some(_) => Err(bad(&format!("optional field {field} must be an object"))),
            }
        };
        let sort = object("sort")?;
        let projection = object("projection")?;
        let collation = match cmd.get("collation") {
            None => None,
            Some(Bson::Document(d)) if d.is_empty() => {
                return Err(bad("optional field collation cannot be an empty object"))
            }
            Some(Bson::Document(d)) => secantus_core::collation::parse(d),
            Some(_) => return Err(bad("optional field collation must be an object")),
        };
        let parsed = super::parsed_query(query);
        let Some(e) = super::cache_key::parse(&parsed) else {
            return Ok(());
        };
        Some(super::cache_key::command_shape(
            &e,
            sort.filter(|s| !s.is_empty()),
            projection,
            collation.as_ref(),
        ))
    } else {
        if ["sort", "projection", "collation"]
            .iter()
            .any(|f| cmd.contains_key(*f))
        {
            return Err(bad("sort, projection, or collation provided without query"));
        }
        None
    };
    let place = place_of(storage, db, coll).map_err(crate::util::command_error)?;
    if let Some(place) = place {
        clear(&place, shape.as_deref());
    }
    Ok(())
}

/// Index filters change what a shape is planned over: setting or clearing
/// one removes that shape's entries (`removePlanCacheEntriesByPlanCache
/// CommandKeys`, keyed by query, sort, projection and the collation given).
pub(crate) fn clear_filtered_shape(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    query: &Document,
    sort: Option<&Document>,
    projection: Option<&Document>,
    collation: Option<&Document>,
) -> Result<(), crate::CommandError> {
    let parsed = super::parsed_query(query);
    let Some(e) = super::cache_key::parse(&parsed) else {
        return Ok(());
    };
    let collation = collation.and_then(secantus_core::collation::parse);
    let shape = super::cache_key::command_shape(
        &e,
        sort.filter(|s| !s.is_empty()),
        projection,
        collation.as_ref(),
    );
    if let Some(place) = place_of(storage, db, coll).map_err(crate::util::command_error)? {
        clear(&place, Some(&shape));
    }
    Ok(())
}

fn bad_value(msg: String) -> crate::CommandError {
    crate::CommandError::new(9, "FailedToParse", msg)
}

/// `$planCacheStats`' argument, as mongod parses it (8.2.11): an object
/// holding at most `allHosts`, which only a sharded cluster takes.
pub(crate) fn check_spec(spec: &Bson) -> Result<(), crate::CommandError> {
    let Bson::Document(d) = spec else {
        return Err(bad_value(format!(
            "$planCacheStats value must be an object. Found: {}",
            secantus_core::query::bson_type_name(spec)
        )));
    };
    let mut fields = d.iter();
    if let Some((name, value)) = fields.next() {
        if name != "allHosts" {
            return Err(bad_value(format!(
                "$planCacheStats parameters object may contain only 'allHosts' field. Found: {name}"
            )));
        }
        if fields.next().is_some() {
            return Err(bad_value(
                "$planCacheStats parameters object may contain at most one field.".into(),
            ));
        }
        if crate::util::truthy(value) {
            return Err(crate::CommandError::new(
                4503200,
                "Location4503200",
                "$planCacheStats stage supports allHosts parameter only for sharded clusters",
            ));
        }
    }
    Ok(())
}

/// `$planCacheStats`: the collection's cache entries, the latest used first,
/// those `matched` keeps (a `$match` right after the stage is absorbed into
/// it and sees the entry before `host` is added), each with this server's
/// `host`.
pub(crate) fn stats_stage(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    host: &str,
    matched: &dyn Fn(&Document) -> Result<bool, crate::CommandError>,
) -> Result<Vec<Document>, crate::CommandError> {
    let ns = format!("{db}.{coll}");
    let place = place_of(storage, db, coll)
        .map_err(crate::util::command_error)?
        .ok_or_else(|| {
            crate::CommandError::new(
                50933,
                "Location50933",
                format!(
                    "Executor error during aggregate command on namespace: {ns} :: caused by :: \
                     collection '{ns}' does not exist"
                ),
            )
        })?;
    let mut out = Vec::new();
    for e in entries(&place) {
        let mut d = to_doc(&e);
        if !matched(&d)? {
            continue;
        }
        d.insert("host", host);
        out.push(d);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place() -> Place<'static> {
        Place {
            store: 0xdead_beef,
            db: "t",
            coll: "plan_cache_unit",
            uuid: vec![1],
            catalog: "a_1,b_1".into(),
        }
    }

    fn entry(works: i64) -> Entry {
        Entry {
            key: "an[eqa,eqb]||||fc<1><1>".into(),
            shape_hash: 0,
            key_hash: 0,
            command_shape: "an[eqa,eqb]|||".into(),
            active: false,
            works,
            reads: false,
            created: bson::DateTime::now(),
            created_from: Document::new(),
            signature: String::new(),
            solution: String::new(),
            cached_plan: Document::new(),
            creation: vec![],
            scores: vec![],
            index_filter_set: false,
            solution_hash: 0,
        }
    }

    #[test]
    fn inactive_then_active_then_grows() {
        let p = place();
        let key = CacheKey {
            key: "an[eqa,eqb]||||fc<1><1>".into(),
            shape_hash: 0,
            key_hash: 0,
        };
        clear(&p, None);
        // mongod 8.2.11 (2026-10-27): 101 works, inactive; a second trial
        // of 202 works keeps it inactive at 202; one of 101 activates it.
        set(&p, entry(101));
        let e = get(&p, &key).unwrap();
        assert!(!e.active);
        assert_eq!(e.works, 101);
        set(&p, entry(150));
        let e = get(&p, &key).unwrap();
        assert!(!e.active);
        assert_eq!(e.works, 202);
        set(&p, entry(101));
        let e = get(&p, &key).unwrap();
        assert!(e.active);
        assert_eq!(e.works, 101);
        // Deactivated, it takes a trial of fewer works to come back.
        deactivate(&p, &key);
        assert!(!get(&p, &key).unwrap().active);
        set(&p, entry(61));
        let e = get(&p, &key).unwrap();
        assert!(e.active);
        assert_eq!(e.works, 61);
        // A new catalog empties the cache.
        let moved = Place {
            catalog: "a_1,b_1,c_1".into(),
            ..place()
        };
        assert!(get(&moved, &key).is_none());
    }
}
