//! `explain`: the plan mongod would report for a command, and what running
//! it would examine.
//!
//! The reply is built from mongod's planning RULES, measured on 8.2.11 with
//! `tools/probes/explain_commands.py`: which predicates bound which index
//! and how the bounds print, what the fetch rechecks, when an index serves
//! the sort or covers the projection, and the stage vocabulary of each
//! command. It is a description of the plan mongod runs for the query. This
//! server's own storage layer may read the same documents another way.
//!
//! The choice mongod makes by trial when several plans could serve one
//! query is counted over the collection (`trial.rs`), with its plan
//! ranker's scores and tie-breaking bonuses, and the plans it tried are
//! listed as `rejectedPlans` and `allPlansExecution`. Without the
//! collection to count over, a fixed order of preference decides
//! (`plan.rs`). A read keeps mongod's plan cache (`plan_cache.rs`, keyed by
//! `cache_key.rs`); `explain` neither reads nor writes it, as mongod's does
//! not, but reports the key's hashes and which plan is cached (`isCached`).

mod bounds;
mod cache_key;
mod change_stream;
pub(crate) mod debug_string;
mod deps;
pub(crate) mod distinct;
mod entry_size;
mod exec;
pub(crate) mod geo;
pub(crate) mod hoist;
mod hybrid;
mod json_schema;
mod node;
pub(crate) mod order;
mod parsed;
mod plan;
pub(crate) mod plan_cache;
mod sbe;
mod shape;
pub(crate) mod stream;
mod sub_stages;
pub(crate) mod summary;
mod trial;

use crate::util::{as_i64, command_error, decode_docs};
use crate::{CommandContext, CommandError, HandlerResult, SERVER_VERSION};
use bson::{doc, Bson, Document};
pub(crate) use debug_string::CanonicalQuery;
pub(crate) use node::IndexInfo;
pub(crate) use node::Node;
use node::{num, Stats};
pub(crate) use parsed::parsed_query;
pub(crate) use plan::{
    begin_operation, planned_apart, planning_scope, planning_spent, plans_nothing,
};
use plan::{Hint, Query};
use secantus_core::collation::Collation;
use secantus_core::query::bson_type_name;
use shape::{Kind, Shape};
use std::sync::Arc;

fn err(code: i32, name: &str, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, name, msg.into())
}

#[derive(Clone, Copy, PartialEq)]
enum Verbosity {
    QueryPlanner,
    ExecutionStats,
    AllPlans,
}

const EXPLAIN_FIELDS: &[&str] = &["explain", "verbosity"];

/// The commands mongod knows but will not explain.
fn known_command(name: &str) -> bool {
    crate::lookup(name).is_some()
}

fn verbosity_of(doc: &Document) -> Result<Verbosity, CommandError> {
    match doc.get("verbosity") {
        None | Some(Bson::Null) => Ok(Verbosity::AllPlans),
        Some(Bson::String(s)) => match s.as_str() {
            "queryPlanner" => Ok(Verbosity::QueryPlanner),
            "executionStats" => Ok(Verbosity::ExecutionStats),
            "allPlansExecution" => Ok(Verbosity::AllPlans),
            other => Err(err(
                2,
                "BadValue",
                format!(
                    "Enumeration value '{other}' for field 'explain.verbosity' is not a valid value."
                ),
            )),
        },
        Some(other) => Err(err(
            14,
            "TypeMismatch",
            format!(
                "BSON field 'explain.verbosity' is the wrong type '{}', expected type 'string'",
                bson_type_name(other)
            ),
        )),
    }
}

/// Run the command itself for its argument checks, and throw the result away.
fn validate(
    handler: crate::Handler,
    inner: &Document,
    ctx: &mut CommandContext,
) -> Result<(), CommandError> {
    let reply = order::uncounted(|| handler(inner, ctx))?;
    // A streamed first batch is the dry run's, not the reply's.
    ctx.pending_batch = None;
    if let Ok(cursor) = reply.get_document("cursor") {
        if let Some(id) = cursor.get("id").and_then(as_i64).filter(|id| *id != 0) {
            if let Ok(cursors) = ctx.cursors() {
                cursors.kill(&[id]);
            }
        }
    }
    let ok = reply.get("ok").is_some_and(crate::util::truthy);
    if ok {
        return Ok(());
    }
    Err(err(
        reply.get("code").and_then(as_i64).unwrap_or(1) as i32,
        reply.get_str("codeName").unwrap_or("InternalError"),
        reply.get_str("errmsg").unwrap_or("command failed"),
    ))
}

/// The collection a command names, or mongod's refusal of the name.
fn collection(inner: &Document, name: &str, db: &str) -> Result<String, CommandError> {
    match inner.get(name) {
        Some(Bson::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Bson::String(_)) => Err(err(
            73,
            "InvalidNamespace",
            format!("Invalid namespace specified: {db}"),
        )),
        Some(other) => Err(err(
            73,
            "InvalidNamespace",
            format!("Collection name has invalid type {}", bson_type_name(other)),
        )),
        None => Err(err(
            73,
            "InvalidNamespace",
            "Collection name has invalid type missing",
        )),
    }
}

/// An unknown hint, in the words every command on this server uses for it
/// (mongod prints its parse of the query here; `tasks/backlog-rust-mongodb.md`).
fn bad_hint(hint: &Bson) -> CommandError {
    let rendered = match hint {
        Bson::String(s) => format!("\"{s}\""),
        other => secantus_core::aggregate::render_value_compact(other),
    };
    err(
        2,
        "BadValue",
        format!("hint {rendered} does not correspond to an existing index"),
    )
}

fn same_key(a: &Document, b: &Document) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|((ak, av), (bk, bv))| {
            ak == bk
                && match (as_i64(av), as_i64(bv)) {
                    (Some(x), Some(y)) => x == y,
                    _ => av == bv,
                }
        })
}

/// A hint naming no index, refused in mongod's planner's words for the
/// query `s` makes of `ns`: an aggregate's under its collation (`simple`
/// when it names none), a find's with its paging and projection.
fn hint_refusal(ns: &str, s: &Shape) -> CommandError {
    let simple = doc! {"locale": "simple"};
    let named = s.options.get_document("collation").ok();
    let query = CanonicalQuery {
        ns: ns.to_string(),
        filter: &s.filter,
        sort: s.sort.as_ref(),
        projection: s.projection.as_ref(),
        skip: Some(s.skip),
        limit: Some(s.limit),
        collation: if s.pipeline {
            named.or(Some(&simple))
        } else {
            named
        },
        batch_size: (!s.pipeline)
            .then(|| s.options.get("batchSize").and_then(crate::util::as_i64))
            .flatten(),
    };
    err(
        2,
        "BadValue",
        query.planner_error("hint provided does not correspond to an existing index"),
    )
}

fn resolve_hint(hint: Option<&Bson>, catalog: &[Arc<IndexInfo>]) -> Result<Hint, CommandError> {
    match hint {
        None | Some(Bson::Null) => Ok(Hint::None),
        Some(given @ Bson::String(name)) => catalog
            .iter()
            .find(|ix| &ix.name == name)
            .map(|ix| Hint::Index(ix.name.clone()))
            .ok_or_else(|| bad_hint(given)),
        Some(Bson::Document(d)) if d.is_empty() => Ok(Hint::None),
        Some(given @ Bson::Document(d)) => {
            if let Some(n) = d.get("$natural").and_then(as_i64) {
                return Ok(Hint::Natural(n < 0));
            }
            catalog
                .iter()
                .find(|ix| same_key(&ix.key, d))
                .map(|ix| Hint::Index(ix.name.clone()))
                .ok_or_else(|| bad_hint(given))
        }
        Some(_) => Err(err(
            9,
            "FailedToParse",
            "Hint must be a string or an object",
        )),
    }
}

/// Why `notablescan` refuses a read: the plan mongod would run scans the
/// collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableScan {
    /// No index serves the query.
    Unindexed,
    /// A `{$natural: ±1}` hint forces the scan.
    Natural,
}

/// Whether `plan` reads the collection rather than an index.
fn scans_collection(plan: &Node) -> bool {
    match plan {
        Node::CollScan { .. } => true,
        Node::Fetch { input, .. }
        | Node::Sort { input, .. }
        | Node::Skip(_, input)
        | Node::Limit(_, input)
        | Node::Projection { input, .. }
        | Node::ReturnKey(input)
        | Node::Subplan(input)
        | Node::Count(input)
        | Node::Update(input)
        | Node::Delete { input, .. } => scans_collection(input),
        Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
            inputs.iter().any(scans_collection)
        }
        Node::IxScan(_)
        | Node::Eof(_)
        | Node::Express { .. }
        | Node::IdHack(_)
        | Node::ExpressWrite { .. }
        | Node::CountScan(_)
        | Node::FastCount
        | Node::DistinctScan(..) => false,
    }
}

/// What `notablescan` makes of `plan`, planned with `hint`.
fn table_scan(plan: &Node, hint: &Hint) -> Option<TableScan> {
    if !scans_collection(plan) {
        return None;
    }
    Some(match hint {
        Hint::Natural(_) => TableScan::Natural,
        _ => TableScan::Unindexed,
    })
}

/// Whether the plan mongod runs for a read of `coll` -- `filter`, `sort`
/// (a find's or a findAndModify's; `None` for the rest), `hint` and
/// `collation` -- scans the collection. The read's own checks have already
/// passed; a hint the planner does not know is the read's to refuse, so it
/// is not refused here.
pub(crate) fn plans_table_scan(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    sort: Option<&Document>,
    hint: Option<&Bson>,
    collation: Option<&Collation>,
) -> Result<Option<TableScan>, CommandError> {
    let catalog = catalog(storage, db, coll, None).map_err(command_error)?;
    let (catalog, _) =
        crate::index_filters::apply(storage, db, coll, filter, sort, None, collation, catalog);
    let Ok(hint) = resolve_hint(hint, &catalog) else {
        return Ok(None);
    };
    let canonical = parsed::parsed_query(filter);
    let implies = |partial: &Document| storage.query_implies_partial(filter, partial);
    let q = Query {
        canonical: &canonical,
        sort: sort.filter(|s| order::plain_sort(s)),
        projection: None,
        skip: 0,
        limit: 0,
        hint: hint.clone(),
        collation,
        min_max: false,
        return_key: false,
        show_record_id: false,
        batch_size: false,
        keys_only: false,
        wanted: None,
        sbe: false,
        implies: &implies,
        trial: None,
    };
    Ok(table_scan(&plan::plan(&q, &catalog), &hint))
}

/// The filter and sort of the query an aggregate's leading stages make, as
/// `explain` reports it.
pub(crate) fn pipeline_query(coll: &str, pipeline: &[Bson]) -> (Document, Option<Document>) {
    let q = pipeline_canonical(coll, pipeline);
    (q.filter, q.sort)
}

/// The query an aggregate's leading stages make, as mongod's planner holds
/// it: the filter and sort it took in, the projection it adds for the
/// stages it did not, and the paging.
pub(crate) struct PipelineQuery {
    pub filter: Document,
    pub sort: Option<Document>,
    pub projection: Option<Document>,
    pub skip: i64,
    pub limit: i64,
    /// The stages it did not take, in the order they run.
    pub rest: Vec<Bson>,
    /// The `$project` (or `$unset`) stage it took as the projection, as
    /// written; `None` when the projection is the fields `rest` reads.
    pub projection_stage: Option<Bson>,
}

/// How many leading stages of `hoisted` (a pipeline over `db.coll`, its
/// `$match`es hoisted) mongod runs in its slot-based engine, read under `hint`
/// and `collation` (the command's, else the collection's default): what
/// [`secantus_core::sbe::pushdown`] decides, except that an `_id` point read
/// keeps the whole pipeline classic ([`id_hack_read`]).
pub(crate) fn sbe_pushdown(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    hoisted: &[Bson],
    hint: Option<&Bson>,
    collation: Option<&Collation>,
) -> Option<usize> {
    // A query setting's `queryFramework: "classic"` keeps it all classic.
    if crate::query_settings::framework() == Some(crate::query_settings::Framework::Classic) {
        return None;
    }
    if id_hack_read(
        coll,
        hoisted,
        hint,
        same_collation(storage, db, coll, collation),
    ) {
        return None;
    }
    let is_view = |from: &str| crate::views::is_view(storage, db, from);
    secantus_core::sbe::pushdown(hoisted, &is_view)
}

/// Whether the read uses the collection's own collation.
fn same_collation(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    collation: Option<&Collation>,
) -> bool {
    let default = crate::util::default_collation(storage, db, coll);
    match (collation, default.as_ref()) {
        (None, None) => true,
        (Some(a), Some(b)) => a.same(b),
        _ => false,
    }
}

/// Whether mongod runs a `find` of `db.coll` in its slot-based engine: only
/// under a query setting's `queryFramework: "sbe"` (`trySbeEngine`; the
/// default `trySbeRestricted` runs a plain `find` classic), when SBE
/// compiles its filter and projection, and when it is not an `_id` point
/// read, which mongod's express path runs classic (RM-418).
#[allow(clippy::too_many_arguments)]
pub(crate) fn find_runs_in_sbe(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    filter: &Document,
    projection: Option<&Document>,
    skip: i64,
    hint: Option<&Bson>,
    collation: Option<&Collation>,
) -> bool {
    if crate::query_settings::framework() != Some(crate::query_settings::Framework::Sbe)
        || !secantus_core::sbe::find_compatible(filter, projection)
    {
        return false;
    }
    let mut read = vec![Bson::Document(doc! { "$match": filter.clone() })];
    if skip > 0 {
        read.push(Bson::Document(doc! { "$skip": skip }));
    }
    !id_hack_read(
        coll,
        &read,
        hint,
        same_collation(storage, db, coll, collation),
    )
}

/// Whether mongod reads the front of `pipeline` as an `_id` point query
/// (`isIdHackEligibleQuery`): its filter, as the planner optimises it
/// (a one-value `$in`, a one-clause `$and` / `$or` and an always-true `$expr`
/// fall away), is exactly an `_id` equality to a value with exact bounds (not
/// null, an array or a regex), with no hint, no `$skip` and the collection's
/// own collation. mongod runs such a query in its classic engine, so nothing
/// after it is pushed down (measured 8.2.11, 2026-11-08: `$mod` by zero in
/// a `$group` behind `{$match: {_id: 1}}` is the classic 16610, RM-330).
fn id_hack_read(coll: &str, pipeline: &[Bson], hint: Option<&Bson>, same_collation: bool) -> bool {
    let hinted = hint.is_some_and(|h| !matches!(h, Bson::Document(d) if d.is_empty()));
    if hinted || !same_collation {
        return false;
    }
    let q = pipeline_canonical(coll, pipeline);
    let canonical = parsed_query(&q.filter);
    q.skip == 0 && matches!(plan::point_lookup(&canonical), Some(("_id", _)))
}

/// Whether mongod's query layer runs all of `pipeline` over `db.coll`, so
/// the aggregate has no cursor stage and its checks are a `find`'s
/// (RM-360): its planner takes every stage, or its slot-based engine takes
/// a pipeline of `$match` and `$group` stages. Other stages SBE takes can
/// leave one of theirs to the pipeline (measured 8.2.11, 2026-11-12: a
/// `$count`'s `$project`, a `$bucket`'s `$sort`, a `$sort` after a
/// `$group`), whose count is not modelled yet.
pub(crate) fn query_layer_takes_whole(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    pipeline: &[Bson],
    hint: Option<&Bson>,
    collation: Option<&Collation>,
) -> bool {
    if pipeline_canonical(coll, pipeline).rest.is_empty() {
        return true;
    }
    let hoisted = hoist::hoist_matches(pipeline);
    let grouped = hoisted.iter().all(|s| {
        s.as_document()
            .and_then(|d| d.keys().next())
            .is_some_and(|k| k == "$match" || k == "$group")
    });
    grouped && sbe_pushdown(storage, db, coll, &hoisted, hint, collation) == Some(hoisted.len())
}

pub(crate) fn pipeline_canonical(coll: &str, pipeline: &[Bson]) -> PipelineQuery {
    let stages: Vec<Document> = pipeline
        .iter()
        .map_while(|s| s.as_document().cloned())
        .collect();
    let Ok(mut s) = shape::find(coll, &doc! { "find": coll }) else {
        return PipelineQuery {
            filter: Document::new(),
            sort: None,
            projection: None,
            skip: 0,
            limit: 0,
            rest: pipeline.to_vec(),
            projection_stage: None,
        };
    };
    shape::absorb(&mut s, &stages);
    PipelineQuery {
        filter: s.filter,
        sort: s.sort,
        projection: s.projection,
        skip: s.skip,
        limit: s.limit,
        rest: s.rest.into_iter().map(Bson::Document).collect(),
        projection_stage: s.projection_stage.map(Bson::Document),
    }
}

/// Everything read from storage to plan one collection.
struct Target {
    exists: bool,
    docs: Vec<Document>,
    catalog: Vec<Arc<IndexInfo>>,
}

fn target(ctx: &CommandContext, coll: &str) -> Result<Target, CommandError> {
    let storage = ctx.storage()?;
    let db = &ctx.db_name;
    let exists = storage.collection_exists(db, coll).unwrap_or(false);
    if !exists {
        return Ok(Target {
            exists,
            docs: vec![],
            catalog: vec![],
        });
    }
    let raw = storage
        .find_collated(
            db,
            coll,
            &Document::new(),
            None,
            None,
            None,
            &Document::new(),
        )
        .map_err(command_error)?;
    let docs = decode_docs(raw)?;
    let catalog = catalog(storage, db, coll, Some(&docs)).map_err(command_error)?;
    Ok(Target {
        exists,
        docs,
        catalog,
    })
}

/// The planner's view of a collection's indexes, read from the store.
pub(crate) fn catalog_of_collection(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> Result<Vec<Arc<IndexInfo>>, crate::storage::StorageError> {
    catalog(storage, db, coll, None)
}

/// What the planner knows about each index of a collection. The multikey
/// paths are read from `docs` when the caller holds the whole collection,
/// and from the store's own record of them when it does not.
fn catalog(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    docs: Option<&[Document]>,
) -> Result<Vec<Arc<IndexInfo>>, crate::storage::StorageError> {
    let mut catalog = Vec::new();
    // Without the documents, the store's one read of specs and paths.
    let listed: Vec<(Document, Option<Vec<Vec<String>>>)> = match docs {
        None => storage.planner_indexes(db, coll)?,
        Some(_) => None,
    }
    .map(|list| list.into_iter().map(|(s, p)| (s, Some(p))).collect())
    .map_or_else(
        || {
            storage
                .list_indexes(db, coll)
                .map(|list| list.into_iter().map(|s| (s, None)).collect())
        },
        Ok,
    )?;
    for (spec, stored_paths) in listed {
        let spec = &spec;
        let Ok(key) = spec.get_document("key") else {
            continue;
        };
        // A hidden index is not there as far as the planner is concerned.
        if spec.get_bool("hidden").unwrap_or(false) {
            continue;
        }
        let name = spec.get_str("name").unwrap_or("").to_string();
        let plain = key.values().all(|v| matches!(as_i64(v), Some(1 | -1)))
            && !key.values().any(|v| matches!(v, Bson::String(_)));
        let multikey_paths = if let Some(mut paths) = stored_paths {
            paths.resize(key.len(), vec![]);
            paths
        } else if !storage.index_is_multikey(db, coll, &name) {
            vec![vec![]; key.len()]
        } else if let Some(docs) = docs {
            // What these documents hold, and what the catalog records: a
            // path outlives the array that made it (RM-055).
            let mut paths = exec::multikey_paths(docs, key);
            let recorded = storage.index_multikey_paths(db, coll, &name)?;
            for (have, more) in paths.iter_mut().zip(recorded) {
                for p in more {
                    if !have.contains(&p) {
                        have.push(p);
                    }
                }
                have.sort();
            }
            paths
        } else {
            let mut paths = storage.index_multikey_paths(db, coll, &name)?;
            paths.resize(key.len(), vec![]);
            paths
        };
        catalog.push(Arc::new(IndexInfo {
            unique: name == "_id_" || spec.get_bool("unique").unwrap_or(false),
            sparse: spec.get_bool("sparse").unwrap_or(false),
            partial: spec.get_document("partialFilterExpression").ok().cloned(),
            multikey_paths,
            version: spec.get("v").and_then(as_i64).unwrap_or(2) as i32,
            // The `_id` index of a collection with a default collation
            // carries it, as `listIndexes` shows (RM-376).
            collation: spec
                .get_document("collation")
                .ok()
                .and_then(secantus_core::collation::parse)
                .or_else(|| {
                    (name == "_id_")
                        .then(|| crate::util::default_collation(storage, db, coll))
                        .flatten()
                }),
            plain,
            key: key.clone(),
            name,
        }));
    }
    Ok(catalog)
}

/// The collection `explain` already read, for the planner to count over.
struct Held<'a> {
    docs: &'a [Document],
    collation: Option<&'a Collation>,
}

impl plan::Whole for Held<'_> {
    fn docs(&self, _plans: &[&Node], _works: i64) -> Option<plan::Docs<'_>> {
        Some(plan::Docs {
            docs: std::borrow::Cow::Borrowed(self.docs),
            known: None,
            collation: self.collation,
            reach: None,
            total: Some(self.docs.len()),
        })
    }
}

/// The plan for one command over one collection; `sbe`: the query is a
/// pipeline's front mongod runs in SBE ([`front_runs_in_sbe`]).
fn plan_for(
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    t: &Target,
    ctx: &CommandContext,
    sbe: bool,
) -> Result<(Node, Vec<plan::Ranked>), CommandError> {
    let ns = format!("{}.{}", ctx.db_name, s.coll);
    if !t.exists {
        let eof = Node::Eof("nonExistentNamespace");
        return Ok((
            match s.kind {
                Kind::Count => Node::Count(Box::new(eof)),
                _ => eof,
            },
            Vec::new(),
        ));
    }
    let storage = ctx.storage()?;
    let implies = |partial: &Document| storage.query_implies_partial(&s.filter, partial);
    let hint = resolve_hint(s.hint.as_ref(), &t.catalog).map_err(|e| {
        if e.code == 2 {
            hint_refusal(&ns, s)
        } else {
            e
        }
    })?;
    if let Some(which) = s.min_max {
        let Hint::Index(name) = &hint else {
            return Err(err(
                291,
                "NoQueryExecutionPlans",
                format!(
                    "error processing query: ns={ns}Tree: $and\nSort: {{}}\nProj: {{}}\n planner \
                     returned error :: caused by :: When using min()/max() a hint of which index \
                     to use must be provided"
                ),
            ));
        };
        let index = t.catalog.iter().find(|ix| &ix.name == name);
        let given = s.options.get_document(which).ok();
        let fits = match (index, given) {
            (Some(ix), Some(g)) => ix.key.keys().eq(g.keys()),
            _ => true,
        };
        if !fits {
            return Err(err(
                51174,
                "Location51174",
                format!(
                    "error processing query: ns={ns}Tree: $and\nSort: {{}}\nProj: {{}}\n planner \
                     returned error :: caused by :: The index chosen is not compatible with min/max"
                ),
            ));
        }
    }
    let whole = Held {
        docs: &t.docs,
        collation,
    };
    let trial = plan::Trial {
        total: Some(t.docs.len()),
        matched: None,
        whole: &whole,
        ranked: Default::default(),
        cache: None,
        cached_ran: Default::default(),
    };
    let q = Query {
        canonical,
        sort: s.sort.as_ref(),
        projection: s.projection.as_ref(),
        skip: s.skip,
        limit: s.limit,
        hint,
        collation,
        min_max: s.min_max.is_some(),
        return_key: s.return_key,
        show_record_id: s.show_record_id,
        batch_size: s.batch_size,
        keys_only: false,
        wanted: None,
        sbe,
        implies: &implies,
        trial: Some(&trial),
    };
    let node = match &s.kind {
        Kind::Find => plan::plan(&q, &t.catalog),
        Kind::Count => plan::count(&q, &t.catalog),
        Kind::Distinct(key) => plan::distinct(&q, &t.catalog, key, !s.filter.is_empty()),
        Kind::Update {
            upsert, returns, ..
        } => plan::write(&q, &t.catalog, false, false, *upsert, *returns),
        Kind::Delete { multi, returns } => {
            plan::write(&q, &t.catalog, true, *multi, false, *returns)
        }
    };
    let ranked = std::mem::take(&mut *trial.ranked.borrow_mut());
    Ok((node, ranked))
}

/// The plan cache as `explain` reports it for the query `s` plans: the
/// key's hashes, and the active entry's plan when there is one. mongod's
/// explain neither runs a cached plan nor writes the cache; it only says
/// which plan is the cached one (`isCached`).
struct CacheView {
    key: cache_key::CacheKey,
    /// The active entry's solution (`plan::solution`).
    active: Option<String>,
}

impl CacheView {
    fn cached(&self, node: &Node) -> bool {
        self.active.as_deref() == Some(plan::solution(node).as_str())
    }
}

fn cache_view(
    storage: &dyn crate::storage::Storage,
    db: &str,
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    pipeline: &[Document],
) -> Option<CacheView> {
    let key = cache_key_of(storage, db, s, canonical, collation, pipeline)?;
    let active = plan_cache::place_of(storage, db, &s.coll)
        .ok()
        .flatten()
        .and_then(|place| plan_cache::get(&place, &key))
        .filter(|e| e.active)
        .map(|e| e.solution);
    Some(CacheView { key, active })
}

/// The plan cache key of the query `s` plans, made from the catalog alone:
/// nothing of the cache is read, and nothing written (a collection's cache
/// is reached through its UUID, which a read must not mint).
fn cache_key_of(
    storage: &dyn crate::storage::Storage,
    db: &str,
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    pipeline: &[Document],
) -> Option<cache_key::CacheKey> {
    let full = catalog(storage, db, &s.coll, None).ok()?;
    let implies = |partial: &Document| storage.query_implies_partial(&s.filter, partial);
    let (projection, distinct) = match &s.kind {
        Kind::Find => (s.projection.as_ref(), None),
        Kind::Distinct(key) => (None, Some(key.as_str())),
        _ => (None, None),
    };
    let sbe = front_runs_in_sbe(storage, db, s, collation, pipeline);
    let parts = cache_key::Parts {
        sort: s.sort.as_ref().filter(|d| !d.is_empty()),
        projection,
        collation,
        distinct,
        sbe,
    };
    let ix = cache_key::Indexability {
        catalog: &full,
        collation,
        implies: &implies,
    };
    cache_key::key_for(canonical, &parts, &ix)
}

/// Whether the query `s` plans is the front of a pipeline whose `$group` or
/// `$lookup` mongod pushes into its query: it runs in SBE, planned by the
/// classic planner and cached as such (`s`).
fn front_runs_in_sbe(
    storage: &dyn crate::storage::Storage,
    db: &str,
    s: &Shape,
    collation: Option<&Collation>,
    pipeline: &[Document],
) -> bool {
    if pipeline.is_empty() {
        return plain_find(s)
            && find_runs_in_sbe(
                storage,
                db,
                &s.coll,
                &s.filter,
                s.projection.as_ref(),
                s.skip,
                s.hint.as_ref(),
                collation,
            );
    }
    let stages: Vec<Bson> = pipeline.iter().cloned().map(Bson::Document).collect();
    let hoisted = hoist::hoist_matches(&stages);
    sbe_pushdown(storage, db, &s.coll, &hoisted, s.hint.as_ref(), collation).is_some()
}

/// A `find` command of a collection, not a view's pipeline.
fn plain_find(s: &Shape) -> bool {
    matches!(s.kind, Kind::Find) && !s.pipeline
}

/// The plan cache key mongod builds for the query `s` plans, as a `Slow
/// query` line prints it: `(planCacheShapeHash, planCacheKey)` in hex.
pub(crate) fn cache_hashes(
    storage: &dyn crate::storage::Storage,
    db: &str,
    s: &Shape,
    collation: Option<&Collation>,
    pipeline: &[Document],
) -> Option<(String, String)> {
    let canonical = parsed::parsed_query(&s.filter);
    let key = cache_key_of(storage, db, s, &canonical, collation, pipeline)?;
    Some((cache_key::hex(key.shape_hash), cache_key::hex(key.key_hash)))
}

fn query_planner(
    ns: &str,
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    plan: &Node,
    cache: Option<&CacheView>,
) -> Document {
    let mut out = doc! { "namespace": ns };
    // Plans made without parsing the query say nothing about it.
    let unparsed = |n: &Node| matches!(n, Node::Eof("nonExistentNamespace") | Node::IdHack(_));
    let fast_count = match plan {
        Node::FastCount | Node::ExpressWrite { .. } => true,
        Node::Count(input) => matches!(**input, Node::Eof("nonExistentNamespace")),
        Node::Update(input) => unparsed(input),
        Node::Eof("nonExistentNamespace") => {
            matches!(s.kind, Kind::Update { .. } | Kind::Delete { .. })
        }
        _ => false,
    };
    if !fast_count {
        out.insert("parsedQuery", canonical.clone());
        if let Some(c) = collation {
            out.insert("collation", c.spec().clone());
        }
        if let Some(settings) = crate::query_settings::active() {
            out.insert("querySettings", (*settings).clone());
        }
        out.insert("indexFilterSet", false);
        // The plan cache key, wherever mongod built one: not for a
        // collection that does not exist.
        let missing = |n: &Node| matches!(n, Node::Eof("nonExistentNamespace"));
        let nothing = match plan {
            Node::Count(input) | Node::Update(input) => missing(input),
            Node::Delete { input, .. } => missing(input),
            other => missing(other),
        };
        if let (false, Some(c)) = (nothing, cache) {
            let shape = cache_key::hex(c.key.shape_hash);
            out.insert("queryHash", shape.clone());
            out.insert("planCacheShapeHash", shape);
            out.insert("planCacheKey", cache_key::hex(c.key.key_hash));
        }
    }
    if s.optimized_pipeline {
        out.insert("optimizedPipeline", true);
    }
    out.insert("maxIndexedOrSolutionsReached", false);
    out.insert("maxIndexedAndSolutionsReached", false);
    out.insert("maxScansToExplodeReached", false);
    out.insert("prunedSimilarIndexes", false);
    let mut winning = doc! { "isCached": cache.is_some_and(|c| c.cached(plan)) };
    winning.extend(plan.to_doc(None));
    out.insert("winningPlan", winning);
    out.insert("rejectedPlans", Bson::Array(vec![]));
    out
}

/// A `$group` mongod answers from a DISTINCT_SCAN (`$groupByDistinctScan`),
/// planned over the documents `t` holds.
fn distinct_group(
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    t: &Target,
    ctx: &CommandContext,
) -> Result<Option<(Shape, Node, Vec<plan::Ranked>)>, CommandError> {
    let whole = Held {
        docs: &t.docs,
        collation,
    };
    let trial = plan::Trial {
        total: Some(t.docs.len()),
        matched: None,
        whole: &whole,
        ranked: Default::default(),
        cache: None,
        cached_ran: Default::default(),
    };
    distinct_group_with(s, collation, &t.catalog, Some(&trial), |asked| {
        plan_for(asked, canonical, collation, t, ctx, false)
    })
}

/// The read of a pipeline that opens with a `$group` mongod answers from a
/// DISTINCT_SCAN.
pub(crate) struct DistinctGroupRead {
    /// The scan.
    pub plan: Node,
    /// The `$groupByDistinctScan` stage's `newRoot`.
    pub root: Document,
    /// The stages after the group.
    pub rest: Vec<Bson>,
    /// The query the scan reads.
    pub filter: Document,
}

/// [`DistinctGroupRead`] of `pipeline`, planned without its documents.
/// `None`: the pipeline opens with no such `$group`.
pub(crate) fn distinct_group_read(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    pipeline: &[Bson],
    collation: Option<&Collation>,
) -> Result<Option<DistinctGroupRead>, crate::storage::StorageError> {
    let stages: Vec<Document> = pipeline
        .iter()
        .map_while(|s| s.as_document().cloned())
        .collect();
    let Ok(mut s) = shape::find(coll, &doc! { "find": coll }) else {
        return Ok(None);
    };
    shape::absorb(&mut s, &stages);
    if !s.rest.first().is_some_and(|st| st.contains_key("$group")) {
        return Ok(None);
    }
    let catalog = catalog(storage, db, coll, None)?;
    let canonical = parsed::parsed_query(&s.filter);
    let implied = |partial: &Document| storage.query_implies_partial(&s.filter, partial);
    let planned = distinct_group_with(&s, collation, &catalog, None, |asked| {
        let Ok(hint) = resolve_hint(asked.hint.as_ref(), &catalog) else {
            return Ok((Node::Eof(""), Vec::new()));
        };
        let q = Query {
            canonical: &canonical,
            sort: None,
            projection: None,
            skip: 0,
            limit: 0,
            hint,
            collation,
            min_max: false,
            return_key: false,
            show_record_id: false,
            batch_size: false,
            keys_only: false,
            wanted: None,
            sbe: false,
            implies: &implied,
            trial: None,
        };
        let Kind::Distinct(key) = &asked.kind else {
            return Ok((Node::Eof(""), Vec::new()));
        };
        Ok((
            plan::distinct(&q, &catalog, key, !asked.filter.is_empty()),
            Vec::new(),
        ))
    });
    let Ok(Some((shape, plan, _))) = planned else {
        return Ok(None);
    };
    let Some(Ok(root)) = shape
        .rest
        .first()
        .map(|st| st.get_document("$groupByDistinctScan"))
    else {
        return Ok(None);
    };
    let Ok(root) = root.get_document("newRoot").cloned() else {
        return Ok(None);
    };
    let rest = shape.rest[1..]
        .iter()
        .cloned()
        .map(Bson::Document)
        .collect();
    Ok(Some(DistinctGroupRead {
        plan,
        root,
        rest,
        filter: s.filter,
    }))
}

/// A `$group` mongod answers from a DISTINCT_SCAN (`$groupByDistinctScan`,
/// measured 8.2.11, 2026-10-25): its `_id` one field path (or a document of
/// one), its accumulators all `$first` or all `$last` of field paths, after
/// a query its planner reads from an index led by that field (or by fields
/// the query names before it) and a sort an index led by it gives. Of the indexes that serve, mongod takes the one
/// with the fewest fields that holds every field the `$group` reads, else
/// the one with the fewest fields. The scan goes the way the sort reads the
/// index (forward, unsorted), and the other way for `$last`; it fetches the
/// documents when its index does not hold a field the `$group` reads.
/// Sorted or filtered, mongod instead tries the indexes that serve against
/// each other, which `trial` (the collection, when it is to hand) runs.
/// `plan`: the plan of a shape. `None`: not such a `$group`, or no index
/// serves it.
fn distinct_group_with(
    s: &Shape,
    collation: Option<&Collation>,
    catalog: &[Arc<IndexInfo>],
    trial: Option<&plan::Trial>,
    plan: impl Fn(&Shape) -> Result<(Node, Vec<plan::Ranked>), CommandError>,
) -> Result<Option<(Shape, Node, Vec<plan::Ranked>)>, CommandError> {
    let path = |v: &Bson| match v {
        Bson::String(p) if p.starts_with('$') && !p.starts_with("$$") => Some(p[1..].to_string()),
        _ => None,
    };
    if s.kind != Kind::Find || !s.pipeline || s.skip != 0 || s.limit != 0 {
        return Ok(None);
    }
    let Some(Ok(spec)) = s.rest.first().map(|st| st.get_document("$group")) else {
        return Ok(None);
    };
    let Some(id) = spec.get("_id") else {
        return Ok(None);
    };
    let field = match id {
        Bson::Document(d) if d.len() == 1 => d.values().next().and_then(path),
        other => path(other),
    };
    let Some(field) = field else {
        return Ok(None);
    };
    let mut root = doc! { "_id": id.clone() };
    let mut reads = vec![field.clone()];
    let mut last: Option<bool> = None;
    for (name, acc) in spec.iter().filter(|(k, _)| *k != "_id") {
        let Some((op, arg)) = acc.as_document().and_then(|d| d.iter().next()) else {
            return Ok(None);
        };
        let is_last = match op.as_str() {
            "$first" => false,
            "$last" => true,
            _ => return Ok(None),
        };
        // `$first` beside `$last` is a `$group` of its own.
        if last.is_some_and(|l| l != is_last) {
            return Ok(None);
        }
        last = Some(is_last);
        let Some(read) = path(arg) else {
            return Ok(None);
        };
        if !reads.contains(&read) {
            reads.push(read);
        }
        root.insert(name.clone(), arg.clone());
    }
    let last = last.unwrap_or(false);
    let mut asked = s.clone();
    asked.kind = Kind::Distinct(field.clone());
    asked.sort = None;
    asked.projection = None;
    let planned = plan(&asked)?;
    let scan_of = |node: Node| match node {
        Node::Projection { input, .. } => *input,
        other => other,
    };
    // The query's planner must read it from an index led by the field.
    let Node::DistinctScan(first, _, _) = scan_of(planned.0.clone()) else {
        return Ok(None);
    };
    // Whether `ix` gives the sort: `Some(true)` read backward.
    let sorted_by = |ix: &IndexInfo| -> Option<bool> {
        let Some(sort) = &s.sort else {
            return Some(false);
        };
        let fields = ix.fields();
        let mut flipped = None;
        for (i, (k, v)) in sort.iter().enumerate() {
            let (name, dir) = fields.get(i)?;
            let asked_dir = as_i64(v)?;
            if name != k {
                return None;
            }
            let flip = asked_dir.signum() != dir.signum();
            if *flipped.get_or_insert(flip) != flip {
                return None;
            }
        }
        flipped
    };
    let filter_fields = query_fields(&s.filter);
    // The field leads the index, or follows fields the query names (an
    // equality on `a` and `a_1_b_1` for `b`).
    let serves = |ix: &&Arc<IndexInfo>| {
        let Some(at) = ix.position(&field) else {
            return false;
        };
        let fields = ix.fields();
        ix.plain
            && ix.partial.is_none()
            && fields[..at]
                .iter()
                .all(|(f, _)| filter_fields.iter().any(|q| q == f))
            && ix.collation.is_some() == collation.is_some()
            && ix.multikey_paths.get(at).is_none_or(Vec::is_empty)
            && filter_fields.iter().all(|f| ix.position(f).is_some())
            && (at == 0 || s.sort.is_none())
            && sorted_by(ix).is_some()
    };
    let holds = |ix: &IndexInfo| reads.iter().all(|f| ix.position(f).is_some());
    let candidates: Vec<&Arc<IndexInfo>> = catalog.iter().filter(serves).collect();
    fn fewest(list: Vec<&Arc<IndexInfo>>) -> Option<&Arc<IndexInfo>> {
        list.into_iter().fold(None, |best, ix| match best {
            Some(b) if b.key.len() <= ix.key.len() => Some(b),
            _ => Some(ix),
        })
    }
    let serving: Vec<String> = candidates.iter().map(|ix| ix.name.clone()).collect();
    let holding: Vec<&Arc<IndexInfo>> = candidates.iter().copied().filter(|ix| holds(ix)).collect();
    let Some(chosen) = fewest(holding).or_else(|| fewest(candidates)).cloned() else {
        return Ok(None);
    };
    // A scan as the `$group` reads it: through the projection of the
    // fields it needs when its index holds them all, else fetching.
    // `None`: its keys are an array's elements, where `$group` wants the
    // arrays (measured 8.2.11, 2026-10-16: a multikey field is grouped
    // without one).
    let grouped = |node: Node| -> Option<(Node, bool)> {
        let Node::DistinctScan(mut ix, width, scan_fetches) = scan_of(node) else {
            return None;
        };
        if ix
            .index
            .multikey_paths
            .get(width - 1)
            .is_some_and(|p| !p.is_empty())
        {
            return None;
        }
        let fetching = scan_fetches || !holds(&ix.index);
        ix.backward = sorted_by(&ix.index).unwrap_or(false) != last;
        let scan = Node::DistinctScan(ix.clone(), width, fetching);
        if fetching {
            return Some((scan, true));
        }
        let mut spec = Document::new();
        for (k, _) in ix.index.fields() {
            if reads.iter().any(|r| r == k) {
                spec.insert(k, 1_i32);
            }
        }
        if !reads.iter().any(|r| r == "_id") {
            spec.insert("_id", 0_i32);
        }
        Some((
            Node::Projection {
                name: if reads.iter().any(|r| r.contains('.')) {
                    "PROJECTION_DEFAULT"
                } else {
                    "PROJECTION_COVERED"
                },
                spec,
                input: Box::new(scan),
            },
            false,
        ))
    };
    // A plan that is no DISTINCT_SCAN reads through the projection of the
    // fields the `$group` needs.
    let projected = |plan: Node| match (&plan, &s.projection) {
        (Node::Projection { .. }, _) | (_, None) => plan,
        (_, Some(p)) => Node::Projection {
            name: "PROJECTION_SIMPLE",
            spec: p.clone(),
            input: Box::new(plan),
        },
    };
    // Sorted or filtered, mongod plans the read as any query and tries each
    // plan against the others (measured 8.2.11, 2026-11-08): every index
    // that serves the `$group` as a DISTINCT_SCAN in the form the `$group`
    // reads it, beside the plans the query's predicate gives that are none,
    // in the order its planner enumerates them; unsorted and unfiltered,
    // it takes the index above without a trial.
    if let Some(trial) = trial.filter(|_| s.sort.is_some() || !s.filter.is_empty()) {
        let mut plans: Option<Vec<Node>> = Some(Vec::new());
        if planned.1.len() > 1 {
            for r in &planned.1 {
                let tried = match scan_of(r.plan.clone()) {
                    Node::DistinctScan(ix, ..) if serving.contains(&ix.index.name) => {
                        grouped(r.plan.clone()).map(|(n, _)| n)
                    }
                    // A DISTINCT_SCAN that does not give the sort, or a plan
                    // that would have to sort: not modelled.
                    Node::DistinctScan(..) => None,
                    _ if s.sort.is_some() => None,
                    _ => Some(projected(r.plan.clone())),
                };
                plans = plans.zip(tried).map(|(mut v, n)| {
                    v.push(n);
                    v
                });
            }
        } else {
            for name in &serving {
                let mut hinted = asked.clone();
                hinted.hint = Some(Bson::String(name.clone()));
                let tried = grouped(plan(&hinted)?.0).map(|(n, _)| n);
                plans = plans.zip(tried).map(|(mut v, n)| {
                    v.push(n);
                    v
                });
            }
        }
        // In the catalog's order, not the query's (measured 8.2.11,
        // 2026-11-08: over `x_1_z_1`, `x_1`, `x_1_w_1`, `x_1_y_1`, `find`
        // tries `x_1` first and the `$group` `x_1_z_1`).
        if let Some(mut plans) = plans.filter(|p| p.len() > 1) {
            plans.sort_by_key(|p| {
                index_read(p)
                    .and_then(|name| catalog.iter().position(|ix| ix.name == name))
                    .unwrap_or(usize::MAX)
            });
            if let Some(at) = plan::pick_for_group(plans.clone(), trial) {
                let mut ranked = std::mem::take(&mut *trial.ranked.borrow_mut());
                // The winner is saved once as the trial ends, as the plans
                // it beat are (measured 8.2.11: `saveState` 1 on each of its
                // stages, where a query's winner has 0).
                if let Some(winner) = ranked.first_mut() {
                    set_aside(&mut winner.stats);
                }
                let mut shape = s.clone();
                shape.sort = None;
                shape.projection = None;
                shape.rest[0] = doc! { "$groupByDistinctScan": { "newRoot": root } };
                return Ok(Some((shape, plans[at].clone(), ranked)));
            }
        }
    }
    let (node, ranked) = if chosen.name == first.index.name {
        planned
    } else {
        let mut hinted = asked.clone();
        hinted.hint = Some(Bson::String(chosen.name.clone()));
        plan(&hinted)?
    };
    let Some((plan, fetching)) = grouped(node) else {
        return Ok(None);
    };
    let mut ranked: Vec<plan::Ranked> = ranked
        .into_iter()
        .map(|mut r| {
            if !matches!(r.plan, Node::Projection { .. }) && s.projection.is_some() {
                r.plan = projected(r.plan);
                r.stats = Stats {
                    n_returned: r.stats.n_returned,
                    children: vec![r.stats],
                    ..Stats::default()
                };
            }
            r
        })
        .collect();
    if let Some(winner) = ranked.first_mut() {
        winner.plan = plan.clone();
        if fetching {
            if let Some(scan) = winner.stats.children.first().cloned() {
                winner.stats = scan;
            }
        }
    }
    let mut shape = s.clone();
    shape.sort = None;
    shape.projection = None;
    shape.rest[0] = doc! { "$groupByDistinctScan": { "newRoot": root } };
    Ok(Some((shape, plan, ranked)))
}

/// The index a plan of one index scan reads.
fn index_read(node: &Node) -> Option<&str> {
    match node {
        Node::IxScan(ix) | Node::DistinctScan(ix, ..) => Some(&ix.index.name),
        Node::Fetch { input, .. } | Node::Projection { input, .. } => index_read(input),
        _ => None,
    }
}

/// The fields a query's clauses name, under `$and` / `$or` / `$nor` too.
fn query_fields(filter: &Document) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, v) in filter {
        match (k.as_str(), v) {
            ("$and" | "$or" | "$nor", Bson::Array(items)) => {
                for f in items
                    .iter()
                    .filter_map(Bson::as_document)
                    .flat_map(query_fields)
                {
                    if !out.contains(&f) {
                        out.push(f);
                    }
                }
            }
            (k, _) if !k.starts_with('$') && !out.iter().any(|f| f == k) => out.push(k.to_string()),
            _ => {}
        }
    }
    out
}

/// A rejected plan's stages, saved once more as the trial ends.
fn set_aside(st: &mut Stats) {
    if let Some(w) = st.work.as_mut() {
        w.saved += 1;
    }
    for c in st.children.iter_mut() {
        set_aside(c);
    }
}

/// Raise each counter of `run` to what `trial` counted in the same stage.
fn at_least(run: &mut Stats, trial: &Stats) {
    run.n_returned = run.n_returned.max(trial.n_returned);
    run.keys = run.keys.max(trial.keys);
    run.docs = run.docs.max(trial.docs);
    if run.children.len() == trial.children.len() {
        for (r, t) in run.children.iter_mut().zip(&trial.children) {
            at_least(r, t);
        }
    }
}

/// The plans mongod tried, as `explain` lists them (the winner first): each
/// under the stage the command puts on top, with its score and what its
/// stages counted during the trial.
fn tried_plans(s: &Shape, ranked: &[plan::Ranked]) -> Vec<(Node, f64, Stats)> {
    ranked
        .iter()
        .map(|r| {
            let on_top = |node: Node, extra: Document| {
                let stats = Stats {
                    extra,
                    children: vec![r.stats.clone()],
                    ..Stats::default()
                };
                (node, r.score, stats)
            };
            match &s.kind {
                Kind::Update { .. } => on_top(
                    Node::Update(Box::new(r.plan.clone())),
                    doc! { "nMatched": 0_i32, "nWouldModify": 0_i32, "nWouldUpsert": 0_i32 },
                ),
                Kind::Delete { multi, .. } => on_top(
                    Node::Delete {
                        batched: *multi,
                        input: Box::new(r.plan.clone()),
                    },
                    doc! { "nWouldDelete": 0_i32 },
                ),
                _ => (r.plan.clone(), r.score, r.stats.clone()),
            }
        })
        .collect()
}

fn execution_stats(
    plan: &Node,
    stats: &Stats,
    verbosity: Verbosity,
    tried: &[(Node, f64, Stats)],
    cache: Option<&CacheView>,
) -> Document {
    let cached = |n: &Node| cache.is_some_and(|c| c.cached(n));
    let (keys, docs) = stats.totals();
    let mut stages = doc! { "isCached": cached(plan) };
    stages.extend(plan.to_doc(Some(stats)));
    let mut out = doc! {
        "executionSuccess": true,
        "nReturned": num(stats.n_returned),
        "executionTimeMillis": 0_i32,
        "totalKeysExamined": num(keys),
        "totalDocsExamined": num(docs),
        "executionStages": stages,
    };
    if verbosity == Verbosity::AllPlans {
        let entries: Vec<Bson> = tried
            .iter()
            .enumerate()
            .map(|(at, (node, score, st))| {
                // mongod's write stage is one object, shared by every plan:
                // the plans it rejected show the counts of the run that
                // followed, the winner the counts it had during the trial.
                let mut st = st.clone();
                if at > 0 && matches!(node, Node::Update(_) | Node::Delete { .. }) {
                    st.extra = stats.extra.clone();
                    st.n_returned = stats.n_returned;
                }
                // A plan the trial rejected is saved once more when it is
                // set aside (measured 8.2.11: `saveState` 1 on each of its
                // stages, the winner's 0).
                if at > 0 {
                    set_aside(&mut st);
                }
                let (keys, docs) = st.totals();
                let mut stages = doc! { "isCached": cached(node) };
                stages.extend(node.to_doc(Some(&st)));
                Bson::Document(doc! {
                    "nReturned": num(st.n_returned),
                    "executionTimeMillisEstimate": 0_i32,
                    "totalKeysExamined": num(keys),
                    "totalDocsExamined": num(docs),
                    "score": *score,
                    "executionStages": stages,
                })
            })
            .collect();
        out.insert("allPlansExecution", entries);
    }
    out
}

fn server_info(ctx: &CommandContext) -> Document {
    let (host, port) = ctx
        .server_address
        .clone()
        .unwrap_or_else(|| ("secantus".to_string(), 0));
    doc! {
        "host": host,
        "port": i32::from(port),
        "version": SERVER_VERSION,
        "gitVersion": "0".repeat(40),
    }
}

/// `serverParameters`. The framework control is the one a query setting
/// chose (RM-418) where the reply is the query's own explain; one with
/// classic `stages` after its `$cursor` reports the server's
/// (`trySbeRestricted`) whatever the setting (`own`: false; measured
/// 8.2.11, 2026-11-10).
pub(crate) fn server_parameters(own: bool) -> Document {
    let control = crate::query_settings::framework().filter(|_| own).map_or(
        "trySbeRestricted",
        crate::query_settings::Framework::control,
    );
    // The memory budgets as `setParameter` left them (RM-211, RM-315).
    let b = secantus_core::memlimit::budgets();
    let int = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    doc! {
        "internalQueryFacetBufferSizeBytes": 104_857_600_i32,
        "internalQueryFacetMaxOutputDocSizeBytes": int(b.facet),
        "internalLookupStageIntermediateDocumentMaxSizeBytes": int(b.lookup),
        "internalDocumentSourceGroupMaxMemoryBytes": int(b.group),
        "internalQueryMaxBlockingSortMemoryUsageBytes": int(b.sort),
        "internalQueryProhibitBlockingMergeOnMongoS": 0_i32,
        "internalQueryMaxAddToSetBytes": int(b.add_to_set),
        "internalDocumentSourceSetWindowFieldsMaxMemoryBytes": int(b.window_fields),
        "internalQueryFrameworkControl": control,
        "internalQueryPlannerIgnoreIndexWithCollationForRegex": 1_i32,
    }
}

/// A blocking stage's memory and spill counters in an explain, as mongod
/// 8.2.11 lists them: a `$sort`'s, a `$group`'s, and a
/// `$_internalSetWindowFields`'s (RM-328).
fn spill_fields(stage: &mut Document, spilled: &secantus_core::memlimit::SpillReport) {
    match stage.keys().next().map(String::as_str) {
        Some("$sort") => {
            stage.insert(
                "totalDataSizeSortedBytesEstimate",
                spilled.sort_total_data_size,
            );
            stage.insert("usedDisk", spilled.sort_spills > 0);
            stage.insert("spills", spilled.sort_spills);
            stage.insert(
                "spilledDataStorageSize",
                spilled.sort_spilled_data_storage_size,
            );
        }
        Some("$group") => {
            if let Some(group) = spilled.groups.last() {
                let per: Document = group
                    .max_accumulator_memory
                    .iter()
                    .map(|(k, v)| (k.clone(), Bson::Int64(*v)))
                    .collect();
                stage.insert("maxAccumulatorMemoryUsageBytes", per);
                stage.insert("totalOutputDataSizeBytes", group.total_output_data_size);
            }
            stage.insert("usedDisk", spilled.group_spills > 0);
            stage.insert("spills", spilled.group_spills);
            stage.insert(
                "spilledDataStorageSize",
                spilled.group_spilled_data_storage_size,
            );
            stage.insert("spilledBytes", spilled.group_spilled_bytes);
            stage.insert("spilledRecords", spilled.group_spilled_records);
        }
        Some("$_internalSetWindowFields") => {
            let window = spilled.windows.last().cloned().unwrap_or_default();
            // In the order of the functions' names (mongod's map).
            let mut functions = window.max_function_memory.clone();
            functions.sort();
            let per: Document = functions
                .into_iter()
                .map(|(k, v)| (k, Bson::Int64(v)))
                .collect();
            stage.insert("maxFunctionMemoryUsageBytes", per);
            stage.insert("maxTotalMemoryUsageBytes", window.max_total_memory);
            stage.insert("usedDisk", window.spills > 0);
            stage.insert("spills", window.spills);
            stage.insert("spilledDataStorageSize", window.spilled_data_storage_size);
            stage.insert("spilledBytes", window.spilled_bytes);
            stage.insert("spilledRecords", window.spilled_records);
        }
        _ => {}
    }
}

/// A namespace that does not exist: a command is run against it for its
/// argument checks, so nothing in the real collection can fail the run.
const DRY: &str = "explain.dry.run";

/// How many documents come out of `pipeline` over `target` (the
/// `aggregate` argument: a collection's name, or `1`).
fn count_through(
    ctx: &mut CommandContext,
    target: &Bson,
    pipeline: &[Document],
    options: &Document,
) -> i64 {
    count_and_spills(ctx, target, pipeline, options).0
}

/// [`count_through`], and what the run spilled (RM-315).
fn count_and_spills(
    ctx: &mut CommandContext,
    target: &Bson,
    pipeline: &[Document],
    options: &Document,
) -> (i64, secantus_core::memlimit::SpillReport) {
    let (n, report) =
        secantus_core::memlimit::captured(|| count_inline(ctx, target, pipeline, options));
    (n, report)
}

fn count_inline(
    ctx: &mut CommandContext,
    target: &Bson,
    pipeline: &[Document],
    options: &Document,
) -> i64 {
    // The window's `$sort` runs as the `$sort` it is, a `$match` moved
    // ahead of a `$replaceRoot` as the `$match` it is, and a `$count` as
    // the `$group` and `$project` mongod runs, whose `$group` reports its
    // memory (RM-539).
    let stages: Vec<Bson> = pipeline
        .iter()
        .flat_map(|stage| {
            match (
                stage.get(shape::WINDOW_SORT),
                stage.get(hoist::PUSHED_MATCH),
                stage.get_str("$count"),
            ) {
                (Some(sort), _, _) => vec![Bson::Document(doc! { "$sort": sort.clone() })],
                (_, Some(filter), _) => vec![Bson::Document(doc! { "$match": filter.clone() })],
                // A name `$count` refuses runs as written, for its error.
                (_, _, Ok(field))
                    if stage.len() == 1
                        && !field.is_empty()
                        && !field.starts_with('$')
                        && !field.contains('.')
                        && field != "_id" =>
                {
                    vec![
                        Bson::Document(
                            doc! { "$group": { "_id": Bson::Null, field: { "$sum": 1 } } },
                        ),
                        Bson::Document(doc! { "$project": { field: true, "_id": false } }),
                    ]
                }
                _ => vec![Bson::Document(stage.clone())],
            }
        })
        .collect();
    let mut cmd = doc! {
        "aggregate": target.clone(),
        "pipeline": stages,
        "cursor": { "batchSize": i32::MAX },
    };
    if let Some(c) = options.get("collation") {
        cmd.insert("collation", c.clone());
    }
    // The window's `$sort` attaches each document's sort key (RM-408).
    let _window_sorts = secantus_core::aggregate::window_sorts(
        pipeline
            .iter()
            .filter_map(|stage| stage.get_document(shape::WINDOW_SORT).ok().cloned())
            .collect(),
    );
    // A prefix of a pipeline `hybrid::prepare` made runs as it did.
    let mut run = || order::uncounted(|| crate::aggregate::aggregate_inline(&cmd, ctx));
    let reply = if crate::hybrid::has_prepared_stage(pipeline) {
        crate::hybrid::as_prepared(run)
    } else {
        run()
    };
    let Ok(reply) = reply else {
        return 0;
    };
    let Ok(cursor) = reply.get_document("cursor") else {
        return 0;
    };
    if let Some(id) = cursor.get("id").and_then(as_i64).filter(|id| *id != 0) {
        if let Ok(cursors) = ctx.cursors() {
            cursors.kill(&[id]);
        }
    }
    cursor.get_array("firstBatch").map_or(0, |b| b.len() as i64)
}

/// Whether a stage hands each document on as it reads it.
fn streams(stage: &Document) -> bool {
    matches!(
        stage.keys().next().map(String::as_str),
        Some(
            "$match"
                | hoist::PUSHED_MATCH
                | "$unwind"
                | "$project"
                | "$addFields"
                | "$set"
                | "$unset"
                | "$replaceRoot"
                | "$replaceWith"
                | "$skip"
                | secantus_core::aggregate::INHIBIT_OPTIMIZATION
        )
    )
}

/// mongod's per-stage counts in front of a `$limit` (measured 8.2.11): a
/// stage that streams hands over only what the stage after it pulled, so
/// `[$match, $unwind: "$t", $limit: 3]` returns 2 documents from the
/// cursor and 3 from the `$unwind`; a `$skip` of n reads n more than it
/// hands over (RM-414). Rewrites `counts` (the stages after the cursor) and
/// returns what the cursor handed over, when it streams into the limit.
fn pulled_counts(
    ctx: &mut CommandContext,
    target: &Bson,
    pipeline: &[Document],
    taken: usize,
    options: &Document,
    counts: &mut [i64],
) -> Option<i64> {
    let rest = &pipeline[taken..];
    let at = rest.iter().position(|st| st.contains_key("$limit"))?;
    let limit = rest[at].get("$limit").and_then(as_i64)?;
    // The run of streaming stages that feeds the limit, after the last that
    // does not stream (or after the cursor).
    let from = rest[..at]
        .iter()
        .rposition(|st| !streams(st))
        .map_or(0, |b| b + 1);
    let full_before = |ctx: &mut CommandContext, j: usize, counts: &[i64]| -> i64 {
        if j == 0 {
            count_through(ctx, target, &pipeline[..taken], options)
        } else {
            counts[j - 1]
        }
    };
    let mut demand = limit;
    for j in (from..at).rev() {
        let before = full_before(ctx, j, counts);
        // Asked for no fewer than it has: it reads everything, to the end.
        if demand >= counts[j] {
            demand = before;
            continue;
        }
        counts[j] = demand;
        if let Some(n) = rest[j].get("$skip").and_then(as_i64) {
            demand = (demand + n.max(0)).min(before);
            continue;
        }
        let wanted = demand;
        // The fewest documents stage `j` reads to hand over `wanted`.
        let (mut lo, mut hi) = (0_i64, before);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let mut probe: Vec<Document> = pipeline[..taken + j].to_vec();
            probe.push(doc! { "$limit": Bson::Int64(mid.max(1)) });
            probe.push(rest[j].clone());
            let got = if mid == 0 {
                0
            } else {
                count_through(ctx, target, &probe, options)
            };
            if got >= wanted {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        demand = lo;
    }
    if from == 0 {
        Some(demand)
    } else {
        counts[from - 1] = counts[from - 1].min(demand);
        None
    }
}

/// One explained command, ready to plan.
struct Request {
    shape: Shape,
    echo: Document,
    /// The whole pipeline when the command is one, for per-stage counts.
    pipeline: Vec<Document>,
}

/// Plan and, above `queryPlanner`, run one request; assemble the reply.
fn reply_for(r: &Request, verbosity: Verbosity, ctx: &mut CommandContext) -> HandlerResult {
    let s = &r.shape;
    let db = ctx.db_name.clone();
    let ns = format!("{db}.{}", s.coll);
    let wanted = verbosity != Verbosity::QueryPlanner;
    let source = Bson::String(s.coll.clone());
    // What each stage after the cursor returns: the pipeline run that far.
    let taken = r.pipeline.len() - s.rest.len();
    // What each stage after the cursor spilled: the run through it less the
    // run through the stage before (RM-211, RM-315).
    let mut stage_spills: Vec<secantus_core::memlimit::SpillReport> = Vec::new();
    let mut stage_counts: Vec<i64> = if wanted {
        let mut before = secantus_core::memlimit::SpillReport::default();
        (0..s.rest.len())
            .map(|i| {
                // The stages after it decide what its documents keep, as
                // they do in the whole run: a `$project` of what they read.
                let mut prefix = r.pipeline[..taken + i + 1].to_vec();
                let after: Vec<Bson> = r.pipeline[taken + i + 1..]
                    .iter()
                    .cloned()
                    .map(Bson::Document)
                    .collect();
                // A `$sort` takes the `$limit` behind it in the whole run
                // (`extractLimitForPushdown`) and keeps only that many: so
                // does its run here, or its counters are those of a sort of
                // everything (measured 8.2.11, RM-516: 131400 bytes for the
                // top 200 of 300 documents, not 197100).
                if r.pipeline[taken + i].contains_key("$sort") {
                    if let Some(k) = secantus_core::aggregate::limit_for_pushdown(&after) {
                        prefix.push(doc! { "$limit": i64::try_from(k).unwrap_or(i64::MAX) });
                    }
                }
                if let (false, Some(fields)) =
                    (after.is_empty(), secantus_core::downstream_fields(&after))
                {
                    let mut keep: Document =
                        fields.iter().map(|f| (f.clone(), Bson::Int32(1))).collect();
                    if !fields.contains("_id") {
                        keep.insert("_id", 0);
                    }
                    if !keep.is_empty() {
                        prefix.push(doc! { "$project": keep });
                    }
                }
                let (n, report) = count_and_spills(ctx, &source, &prefix, &s.options);
                stage_spills.push(report.less(&before));
                before = report;
                n
            })
            .collect()
    } else {
        vec![]
    };
    // Under a `$limit`, the stages that stream into it hand over only what
    // it pulls; `None`: the cursor's own count stands.
    let cursor_pulled = if wanted {
        pulled_counts(
            ctx,
            &source,
            &r.pipeline,
            taken,
            &s.options,
            &mut stage_counts,
        )
    } else {
        None
    };
    let storage = ctx.storage()?;
    let collation = crate::util::effective_collation(storage, &db, &s.coll, &s.options)?;
    let canonical = parsed::parsed_query(&s.filter);
    let mut t = target(ctx, &s.coll)?;
    // An index filter set for this shape leaves the planner its indexes.
    let (catalog, filtered) = crate::index_filters::apply(
        storage,
        &db,
        &s.coll,
        &s.filter,
        s.sort.as_ref(),
        s.projection.as_ref(),
        collation.as_ref(),
        std::mem::take(&mut t.catalog),
    );
    t.catalog = catalog;
    let grouped = distinct_group(s, &canonical, collation.as_ref(), &t, ctx)?;
    let pushed_down = match grouped {
        Some(_) => None,
        None => sbe_front(storage, &db, s, collation.as_ref(), &r.pipeline, t.exists),
    };
    let regrouped;
    let (s, (plan, ranked)) = match grouped {
        Some((shape, plan, ranked)) => {
            regrouped = shape;
            (&regrouped, (plan, ranked))
        }
        None => {
            let sbe = front_runs_in_sbe(storage, &db, s, collation.as_ref(), &r.pipeline);
            (
                s,
                plan_for(s, &canonical, collation.as_ref(), &t, ctx, sbe)?,
            )
        }
    };
    if crate::notablescan::enabled(ctx) {
        let why = resolve_hint(s.hint.as_ref(), &t.catalog)
            .ok()
            .and_then(|hint| table_scan(&plan, &hint));
        let read = crate::notablescan::Read {
            coll: &s.coll,
            filter: &s.filter,
            sort: s.sort.as_ref(),
            hint: s.hint.as_ref(),
            collation: collation.as_ref(),
            shown: Default::default(),
        };
        crate::notablescan::refuse_planned(ctx.storage()?, &db, &read, why)?;
    }
    let tried = tried_plans(s, &ranked);
    let cache = cache_view(storage, &db, s, &canonical, collation.as_ref(), &r.pipeline);
    let mut planner = query_planner(
        &ns,
        s,
        &canonical,
        collation.as_ref(),
        &plan,
        cache.as_ref(),
    );
    if filtered && planner.contains_key("indexFilterSet") {
        planner.insert("indexFilterSet", true);
    }
    if tried.len() > 1 {
        let rejected: Vec<Bson> = tried[1..]
            .iter()
            .map(|(node, _, _)| {
                let mut d = doc! { "isCached": cache.as_ref().is_some_and(|c| c.cached(node)) };
                d.extend(node.to_doc(None));
                Bson::Document(d)
            })
            .collect();
        planner.insert("rejectedPlans", rejected);
    }
    if let Some(front) = pushed_down {
        let built = if t.exists {
            // The plan the trial chose is a forward collection scan: the
            // query in front of the group is that scan's filter and paging.
            let forward_scan = matches!(
                plan.without_paging(),
                Node::CollScan {
                    backward: false,
                    ..
                }
            );
            let parsed = parsed::parsed_query(&s.filter);
            let filter = sbe::Filter::parse(&parsed);
            let on = sbe::Front {
                parsed,
                filter,
                skip: s.skip,
                limit: s.limit,
            };
            let filtered = s.filter.is_empty() || on.filter.is_some();
            let uuid = crate::admin::collection_uuid_text(storage, &db, &s.coll);
            let last = front.tail.is_empty() && s.rest.len() == front.pushed;
            front
                .group
                .as_ref()
                .filter(|_| forward_scan && filtered)
                .and_then(|g| sbe::group_plan(g, &uuid, last, &on))
        } else {
            Some(sbe::eof_plan())
        };
        if let Some(built) = built {
            let is_cached = cache.as_ref().is_some_and(|c| c.cached(&plan));
            let sbe = Pushed {
                plan: &built,
                pushed: front.pushed,
                tail: &front.tail,
                is_cached,
                docs: &t.docs,
                stage_counts: &stage_counts,
                stage_spills: &stage_spills,
            };
            return sbe_reply(r, planner, &sbe, verbosity, ctx);
        }
    }
    // A `find` mongod runs in SBE (RM-418): over a forward collection scan
    // with no sort, projection, hint or collation, its scan, filter, skip
    // and limit; any other is still explained in the classic shape.
    if r.pipeline.is_empty() && front_runs_in_sbe(storage, &db, s, collation.as_ref(), &[]) {
        let built = if !t.exists {
            Some(sbe::eof_plan())
        } else {
            let scan = matches!(
                plan.without_paging(),
                Node::CollScan {
                    backward: false,
                    ..
                }
            );
            let simple = s.sort.as_ref().is_none_or(Document::is_empty)
                && s.projection.as_ref().is_none_or(Document::is_empty)
                && s.hint.is_none()
                && collation.is_none()
                && s.min_max.is_none()
                && !s.return_key
                && !s.show_record_id;
            let parsed = parsed::parsed_query(&s.filter);
            let filter = sbe::Filter::parse(&parsed);
            let on = sbe::Front {
                parsed,
                filter,
                skip: s.skip,
                limit: s.limit,
            };
            let filtered = s.filter.is_empty() || on.filter.is_some();
            let uuid = crate::admin::collection_uuid_text(storage, &db, &s.coll);
            (scan && simple && filtered)
                .then(|| sbe::find_plan(&uuid, &on))
                .flatten()
        };
        if let Some(built) = built {
            let is_cached = cache.as_ref().is_some_and(|c| c.cached(&plan));
            let sbe = Pushed {
                plan: &built,
                pushed: 0,
                tail: &[],
                is_cached,
                docs: &t.docs,
                stage_counts: &[],
                stage_spills: &[],
            };
            return sbe_reply(r, planner, &sbe, verbosity, ctx);
        }
    }
    let stats = wanted.then(|| {
        let (multi, upsert, returns) = match &s.kind {
            Kind::Update {
                multi,
                upsert,
                returns,
            } => (*multi, *upsert, *returns),
            Kind::Delete { multi, returns } => (*multi, false, *returns),
            _ => (true, false, false),
        };
        let env = exec::Env {
            docs: &t.docs,
            collation: collation.as_ref(),
            skip: s.skip,
            limit: s.limit,
            multi,
            upsert,
            returns,
            known: None,
            query: Some(&s.filter),
        };
        let mut run = exec::run(&plan, &env);
        // The winner's stages are the ones the trial worked: what they
        // counted then stands where the run read less (a delete of one
        // document after a trial that read them all).
        if let Some((_, _, trial)) = tried.first() {
            at_least(&mut run, trial);
        }
        run
    });
    let mut reply = doc! { "explainVersion": "1" };
    if s.rest.is_empty() {
        reply.insert("queryPlanner", planner);
        if let Some(st) = &stats {
            reply.insert(
                "executionStats",
                execution_stats(&plan, st, verbosity, &tried, cache.as_ref()),
            );
        }
        reply.insert("command", r.echo.clone());
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters(true));
    } else {
        let mut cursor = doc! { "queryPlanner": planner };
        if let Some(st) = &stats {
            cursor.insert(
                "executionStats",
                execution_stats(&plan, st, verbosity, &tried, cache.as_ref()),
            );
        }
        let counted = |stage: &mut Document, n: i64| {
            stage.insert("nReturned", Bson::Int64(n));
            stage.insert("executionTimeMillisEstimate", Bson::Int64(0));
        };
        let mut first = doc! { "$cursor": cursor };
        if let Some(st) = &stats {
            counted(&mut first, cursor_pulled.unwrap_or(st.n_returned));
        }
        let mut stages = vec![Bson::Document(first)];
        for (i, stage) in s.rest.iter().enumerate() {
            for mut listed in shape::listed_stage(stage, &db) {
                if let Some(spilled) = stage_spills.get(i) {
                    spill_fields(&mut listed, spilled);
                }
                if let Some(n) = stage_counts.get(i) {
                    let input = &r.pipeline[..taken + i];
                    sub_stages::fill(ctx, &source, input, &s.options, stage, &mut listed);
                    counted(&mut listed, *n);
                }
                stages.push(Bson::Document(listed));
            }
        }
        reply.insert("stages", stages);
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters(false));
        reply.insert("command", r.echo.clone());
    }
    reply.insert("ok", 1.0);
    Ok(reply)
}

/// What mongod runs in SBE after the query a pipeline's leading stages
/// make.
struct SbeFront {
    /// How many of the stages after the query it runs.
    pushed: usize,
    /// The `$group` among them that is built here (RM-071), the one a
    /// `$count` is included (RM-072).
    group: Option<Document>,
    /// The classic stages a pushed stage leaves after the plan: a
    /// `$count`'s `$project`.
    tail: Vec<Document>,
}

/// How many of the stages after the query mongod runs in SBE with it, and
/// the `$group` among them that is built here (RM-071): only where the
/// `$group`, or a `$count`, is the one stage pushed, and the query has no
/// sort, projection, hint or collation (its filter and paging are checked
/// against the plan, [`sbe::group_plan`]). Over a collection that does not
/// exist any pushed pipeline is the same EOF plan. `$lookup`, a sorted
/// front and a collation are still explained in the classic shape.
fn sbe_front(
    storage: &dyn crate::storage::Storage,
    db: &str,
    s: &Shape,
    collation: Option<&Collation>,
    pipeline: &[Document],
    exists: bool,
) -> Option<SbeFront> {
    let stages: Vec<Bson> = pipeline.iter().cloned().map(Bson::Document).collect();
    let hoisted = hoist::hoist_matches(&stages);
    let n = sbe_pushdown(storage, db, &s.coll, &hoisted, s.hint.as_ref(), collation)?;
    let taken = pipeline.len().checked_sub(s.rest.len())?;
    let pushed = n.checked_sub(taken)?;
    if pushed == 0 {
        return None;
    }
    let first = s.rest.first()?;
    // `$count: name` is `$group: {_id: null, name: {$sum: 1}}` and then
    // `$project: {name: true, _id: false}`, which stays classic.
    let (group, tail) = match (first.get_document("$group"), first.get_str("$count")) {
        (Ok(g), _) => (Some(g.clone()), vec![]),
        (_, Ok(name)) => (
            Some(doc! { "_id": Bson::Null, name: { "$sum": 1 } }),
            shape::listed_stage(first, db).into_iter().skip(1).collect(),
        ),
        _ => (None, vec![]),
    };
    let plain = group.is_some()
        && pushed == 1
        && s.sort.is_none()
        && s.projection_stage.is_none()
        && s.hint.is_none()
        && collation.is_none();
    (plain || !exists).then_some(SbeFront {
        pushed,
        group,
        tail,
    })
}

/// A slot-based plan ready to report, with the per-stage counts of the
/// classic stages after it.
struct Pushed<'a> {
    plan: &'a sbe::Plan,
    /// How many of the stages after the query the plan runs.
    pushed: usize,
    /// The classic stages the last of them leaves after the plan.
    tail: &'a [Document],
    is_cached: bool,
    docs: &'a [Document],
    stage_counts: &'a [i64],
    stage_spills: &'a [secantus_core::memlimit::SpillReport],
}

/// The reply for a pipeline mongod runs in SBE: `explainVersion` "2", the
/// slot-based `winningPlan`, and the plan's own `executionStats`; under a
/// `$cursor` when classic stages follow it.
fn sbe_reply(
    r: &Request,
    planner: Document,
    p: &Pushed<'_>,
    verbosity: Verbosity,
    ctx: &mut CommandContext,
) -> HandlerResult {
    let s = &r.shape;
    let db = ctx.db_name.clone();
    let whole = s.rest.len() == p.pushed && p.tail.is_empty();
    let mut qp = Document::new();
    for (k, v) in planner {
        if k == "maxIndexedOrSolutionsReached" && whole && !r.pipeline.is_empty() {
            qp.insert("optimizedPipeline", true);
        }
        if k == "winningPlan" {
            qp.insert(k, p.plan.winning_plan(p.is_cached));
        } else {
            qp.insert(k, v);
        }
    }
    let wanted = verbosity != Verbosity::QueryPlanner;
    let all_plans = verbosity == Verbosity::AllPlans;
    let mut reply = doc! { "explainVersion": "2" };
    if whole {
        reply.insert("queryPlanner", qp);
        if wanted {
            let pull = if r.pipeline.is_empty() {
                sbe::Pull::Find
            } else {
                sbe::Pull::Whole
            };
            let (stats, _) = p.plan.execution_stats(p.docs, pull, all_plans);
            reply.insert("executionStats", stats);
        }
        reply.insert("command", r.echo.clone());
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters(true));
        reply.insert("ok", 1.0);
        return Ok(reply);
    }
    let mut cursor = doc! { "queryPlanner": qp };
    let mut pulled = None;
    if wanted {
        // What the `$cursor` handed over: the count of the last stage it
        // runs, as the stages after it pulled it.
        let handed = p.stage_counts.get(p.pushed - 1).copied().unwrap_or(0);
        let limit = s
            .rest
            .get(p.pushed)
            .filter(|_| p.tail.is_empty())
            .and_then(|st| st.get("$limit"))
            .and_then(as_i64);
        let (stats, produced) =
            p.plan
                .execution_stats(p.docs, sbe::Pull::Batches { limit, handed }, all_plans);
        // A plan that ran to the end handed over everything it produced.
        pulled = Some(handed.min(produced));
        cursor.insert("executionStats", stats);
    }
    let counted = |stage: &mut Document, n: i64| {
        stage.insert("nReturned", Bson::Int64(n));
        stage.insert("executionTimeMillisEstimate", Bson::Int64(0));
    };
    let mut first = doc! { "$cursor": cursor };
    if let Some(n) = pulled {
        counted(&mut first, n);
    }
    let mut stages = vec![Bson::Document(first)];
    for listed in p.tail {
        let mut listed = listed.clone();
        if let Some(n) = p.stage_counts.get(p.pushed - 1) {
            counted(&mut listed, *n);
        }
        stages.push(Bson::Document(listed));
    }
    let taken = r.pipeline.len() - s.rest.len();
    let source = Bson::String(s.coll.clone());
    for (i, stage) in s.rest.iter().enumerate().skip(p.pushed) {
        for mut listed in shape::listed_stage(stage, &db) {
            if let Some(spilled) = p.stage_spills.get(i) {
                spill_fields(&mut listed, spilled);
            }
            if let Some(n) = p.stage_counts.get(i) {
                let input = &r.pipeline[..taken + i];
                sub_stages::fill(ctx, &source, input, &s.options, stage, &mut listed);
                counted(&mut listed, *n);
            }
            stages.push(Bson::Document(listed));
        }
    }
    reply.insert("stages", stages);
    reply.insert("serverInfo", server_info(ctx));
    reply.insert("serverParameters", server_parameters(false));
    reply.insert("command", r.echo.clone());
    reply.insert("ok", 1.0);
    Ok(reply)
}

/// The stages a `count` or `distinct` on a view adds after its `$match`.
fn view_tail(kind: &Kind) -> Vec<Document> {
    match kind {
        Kind::Count => vec![doc! { "$count": "count" }],
        Kind::Distinct(key) => vec![
            doc! { "$replaceRoot": { "newRoot": {
                "_internalUnwoundArray": { "$_internalFindAllValuesAtPath": key.clone() }
            } } },
            doc! { "$unwind": {
                "path": "$_internalUnwoundArray",
                "preserveNullAndEmptyArrays": true,
            } },
            doc! { "$group": {
                "_id": Bson::Null,
                "distinct": { "$addToSet": "$_internalUnwoundArray" },
            } },
        ],
        _ => vec![],
    }
}

/// A read on a view is an aggregate on what the view reads: the view's
/// pipeline, then the read's own stages. mongod plans, and echoes, that.
fn through_view(
    s: &Shape,
    own: Vec<Document>,
    settings: bool,
    ctx: &CommandContext,
) -> Result<Request, CommandError> {
    let storage = ctx.storage()?;
    let mut base = s.coll.clone();
    let mut pipeline = own;
    let mut collation = doc! { "locale": "simple" };
    for _ in 0..20 {
        let options = storage
            .get_collection_options(&ctx.db_name, &base)
            .unwrap_or_default();
        let Ok(on) = options.get_str("viewOn") else {
            break;
        };
        if let Ok(c) = options.get_document("collation") {
            collation = c.clone();
        }
        let mut whole: Vec<Document> = options
            .get_array("viewPipeline")
            .map(|p| p.iter().filter_map(Bson::as_document).cloned().collect())
            .unwrap_or_default();
        whole.extend(pipeline);
        pipeline = whole;
        base = on.to_string();
    }
    let mut shape = shape::find(&base, &doc! { "find": base.clone() })?;
    shape.hint = s.hint.clone();
    shape.options = s.options.clone();
    let stages: Vec<Bson> = pipeline.iter().cloned().map(Bson::Document).collect();
    let pipeline: Vec<Document> = hoist::hoist_matches(&shape::desugared(&stages))
        .into_iter()
        .filter_map(|b| match b {
            Bson::Document(d) => Some(d),
            _ => None,
        })
        .collect();
    shape::absorb(&mut shape, &pipeline);
    let mut echo = doc! {
        "aggregate": base,
        "pipeline": stages,
        "cursor": {},
        "collation": collation,
    };
    if settings {
        echo.insert("querySettings", Document::new());
    }
    Ok(Request {
        shape,
        echo,
        pipeline,
    })
}

fn refuse_view(coll: &str, ctx: &CommandContext) -> Result<(), CommandError> {
    let storage = ctx.storage()?;
    if crate::views::is_view(storage, &ctx.db_name, coll) {
        return Err(err(
            166,
            "CommandNotSupportedOnView",
            format!(
                "Namespace {}.{coll} is a view, not a collection",
                ctx.db_name
            ),
        ));
    }
    Ok(())
}

/// The command with its collection swapped for one that does not exist.
/// What depends on the real collection's indexes is checked by the planner.
fn dry(inner: &Document, name: &str) -> Document {
    let mut out = inner.clone();
    out.insert(name, DRY);
    for field in ["hint", "min", "max"] {
        out.remove(field);
    }
    out
}

/// `explain`.
/// `explain`, with the explained query's `queryShapeHash` after its plan
/// (`queryPlanner`, `executionStats` or `stages`), where mongod 8.2.1 puts
/// it (RM-358).
pub fn explain(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let reply = explain_reply(doc, ctx)?;
    let Some(shape) = doc
        .get_document("explain")
        .ok()
        .and_then(|inner| crate::query_shape::hash(&ctx.db_name, inner))
    else {
        return Ok(reply);
    };
    let after = ["executionStats", "queryPlanner", "stages"]
        .into_iter()
        .find(|k| reply.contains_key(*k));
    let mut out = Document::new();
    for (k, v) in reply {
        let here = after == Some(k.as_str());
        out.insert(k, v);
        if here {
            out.insert("queryShapeHash", shape.clone());
        }
    }
    Ok(out)
}

/// An `explain` request, parsed.
pub(crate) struct Parsed {
    inner: Document,
    verbosity: Verbosity,
    inline: bool,
    name: String,
}

/// Parse an `explain` as mongod's parser does before the command runs --
/// and so before it takes a lock (RM-185): its own fields, then the
/// explained command's name and namespace. Every error here is one mongod
/// answers having taken no lock.
pub(crate) fn parse(doc: &Document, db: &str) -> Result<Parsed, CommandError> {
    let inner = match doc.get("explain") {
        None | Some(Bson::Null) => {
            return Err(err(
                40414,
                "IDLFailedToParse",
                "BSON field 'explain.explain' is missing but a required field",
            ))
        }
        Some(Bson::Document(d)) => d.clone(),
        Some(other) => {
            return Err(err(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'explain.explain' is the wrong type '{}', expected type 'object'",
                    bson_type_name(other)
                ),
            ))
        }
    };
    if let Some(field) = crate::argtypes::unknown_field(doc, EXPLAIN_FIELDS) {
        return Err(err(
            40415,
            "IDLUnknownField",
            format!("BSON field 'explain.{field}' is an unknown field."),
        ));
    }
    let verbosity = verbosity_of(doc)?;
    crate::argtypes::require_number(doc, "maxTimeMS", "explain.maxTimeMS")?;
    if doc.contains_key("writeConcern") {
        return Err(err(
            72,
            "InvalidOptions",
            "Command does not support writeConcern",
        ));
    }
    // An aggregate's own `explain: true` arrives here marked.
    let inline = doc.contains_key("$inline");
    let name = inner.keys().next().cloned().unwrap_or_default();
    if let Ok(other) = inner.get_str("$db") {
        if other != db {
            return Err(err(
                73,
                "InvalidNamespace",
                format!("Mismatched $db in explain command. Expected {db} but got {other}"),
            ));
        }
    }
    if name == "find" {
        if inner.contains_key("$db") {
            return Err(err(
                40413,
                "IDLDuplicateField",
                "BSON field 'FindCommandRequest.$db' is a duplicate field",
            ));
        }
        if let Some(id) = inner.get_document("lsid").ok().and_then(|l| l.get("id")) {
            if !matches!(id, Bson::Binary(_)) {
                return Err(err(
                    14,
                    "TypeMismatch",
                    format!(
                        "BSON field 'FindCommandRequest.lsid.id' is the wrong type '{}', \
                         expected type 'binData'",
                        bson_type_name(id)
                    ),
                ));
            }
        }
    }
    let lowered = name.to_ascii_lowercase();
    match name.as_str() {
        "find" | "count" | "distinct" | "update" | "delete" => {
            collection(&inner, &name, db)?;
        }
        "aggregate" => {}
        _ if lowered == "findandmodify" || lowered == "mapreduce" => {
            collection(&inner, &name, db)?;
        }
        other => return Err(unexplainable(other)),
    }
    Ok(Parsed {
        inner,
        verbosity,
        inline,
        name,
    })
}

/// mongod's refusal of an explained command it does not explain.
/// [`parse`], as the dispatcher runs it ahead of the command: the explained
/// command's name and body, which the dispatcher parses next, or `None` for
/// a command mongod knows and cannot explain, which it refuses as the
/// `explain` runs (`{explain: {insert: ...}}` is logged and counts as a
/// connection's first command, measured 8.2.11, RM-386). An `explain` of an
/// `explain` is refused as it parses, taking no lock (RM-185).
pub(crate) fn parse_ahead(
    doc: &Document,
    db: &str,
) -> Result<Option<(String, Document)>, CommandError> {
    match parse(doc, db) {
        Ok(parsed) => Ok(Some((parsed.name, parsed.inner))),
        Err(e) if e.code == 20 && !explains_an_explain(doc) => Ok(None),
        Err(e) => Err(e),
    }
}

fn explains_an_explain(doc: &Document) -> bool {
    doc.get_document("explain")
        .ok()
        .and_then(|inner| inner.keys().next())
        .is_some_and(|name| name == "explain")
}

fn unexplainable(name: &str) -> CommandError {
    if name == "explain" {
        err(20, "IllegalOperation", "Explain cannot explain itself.")
    } else if known_command(name) {
        err(
            20,
            "IllegalOperation",
            format!("Cannot explain cmd: {name}"),
        )
    } else {
        err(
            59,
            "CommandNotFound",
            format!("Explain failed due to unknown command: {name}"),
        )
    }
}

fn explain_reply(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let _budgets = secantus_core::memlimit::with_budgets(crate::aggregate::budgets_of(ctx));
    let _spill =
        secantus_core::spill_table::with_tables(ctx.storage().ok().and_then(|s| s.spill_tables()));
    let Parsed {
        inner,
        verbosity,
        inline,
        name,
    } = parse(doc, &ctx.db_name)?;
    let db = ctx.db_name.clone();
    let echo = shape::echo(&inner, &db);
    // The views the explained read resolves are on the explain's line
    // (RM-362, measured 8.2.1). mongod resolves them for a `count` or an
    // `aggregate` before it parses the command, and for a `find` or a
    // `distinct` after: one of those that does not parse is refused naming
    // the view, with no `resolvedViews` (measured 8.2.11, RM-483).
    let note_views = |ctx: &CommandContext| {
        if let (Ok(storage), Some(Bson::String(coll))) = (ctx.storage(), inner.get(&name)) {
            let pipeline = inner
                .get_array("pipeline")
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            crate::opdebug::note_resolved_views(storage, &db, Some(coll), pipeline);
        }
    };
    if matches!(name.as_str(), "count" | "aggregate") {
        note_views(ctx);
    }

    let lowered = name.to_ascii_lowercase();
    let shape = match name.as_str() {
        "find" => {
            let coll = collection(&inner, "find", &db)?;
            validate(crate::find::find, &dry(&inner, "find"), ctx)?;
            let s = shape::find(&coll, &inner)?;
            note_views(ctx);
            s
        }
        "count" => {
            let coll = collection(&inner, "count", &db)?;
            validate(crate::crud::count, &dry(&inner, "count"), ctx)?;
            shape::count(&coll, &inner)?
        }
        "distinct" => {
            let coll = collection(&inner, "distinct", &db)?;
            let s = shape::distinct(&coll, &inner)?;
            validate(crate::distinct::distinct, &dry(&inner, "distinct"), ctx)?;
            note_views(ctx);
            s
        }
        "aggregate" => return aggregate(&inner, echo, inline, verbosity, ctx),
        "update" | "delete" => {
            let coll = collection(&inner, &name, &db)?;
            let s = if name == "update" {
                shape::update(&coll, &inner)?
            } else {
                shape::delete(&coll, &inner)?
            };
            refuse_view(&coll, ctx)?;
            s
        }
        _ if lowered == "findandmodify" => {
            let coll = collection(&inner, &name, &db)?;
            let s = shape::find_and_modify(&coll, &inner)?;
            refuse_view(&coll, ctx)?;
            s
        }
        // mongod explains a map/reduce by running its JavaScript; with no
        // engine it refuses as it refuses the command (`--noscripting`
        // 8.2.11, 2026-10-20).
        _ if lowered == "mapreduce" => {
            let coll = collection(&inner, &name, &db)?;
            return Err(err(
                31264,
                "Location31264",
                format!(
                    "Executor error during MapReduce command on namespace: {db}.{coll} :: \
                     caused by :: Cannot run server-side javascript without the javascript \
                     engine enabled"
                ),
            ));
        }
        other => return Err(unexplainable(other)),
    };
    let storage = ctx.storage()?;
    let request = if crate::views::is_view(storage, &db, &shape.coll) {
        let mut own = shape::find_as_pipeline(&shape);
        own.extend(view_tail(&shape.kind));
        let settings = !matches!(shape.kind, Kind::Count);
        through_view(&shape, own, settings, ctx)?
    } else {
        Request {
            shape,
            echo,
            pipeline: vec![],
        }
    };
    reply_for(&request, verbosity, ctx)
}

fn aggregate(
    inner: &Document,
    mut echo: Document,
    inline: bool,
    verbosity: Verbosity,
    ctx: &mut CommandContext,
) -> HandlerResult {
    if inner.contains_key("explain") && !inline {
        return Err(err(
            9,
            "FailedToParse",
            "The 'explain' option is illegal when a explain verbosity is also provided",
        ));
    }
    // An explained aggregate takes no write concern, whatever it says and
    // however the explain was asked for (mongod's wording, space and all).
    if inner.contains_key("writeConcern") {
        return Err(err(
            9,
            "FailedToParse",
            "Aggregation explain does not support the'writeConcern' option",
        ));
    }
    let pipeline: Vec<Document> = match inner.get("pipeline") {
        None => {
            return Err(err(
                40414,
                "IDLFailedToParse",
                "BSON field 'aggregate.pipeline' is missing but a required field",
            ))
        }
        Some(Bson::Array(a)) if a.iter().all(|s| matches!(s, Bson::Document(_))) => {
            a.iter().filter_map(Bson::as_document).cloned().collect()
        }
        Some(_) => {
            return Err(err(
                14,
                "TypeMismatch",
                "A pipeline must be an array of objects",
            ))
        }
    };
    // The command's own checks: over no documents, and without the stage
    // that would write.
    let mut checked = inner.clone();
    checked.remove("explain");
    let reads: Vec<Bson> = pipeline
        .iter()
        .filter(|s| !s.contains_key("$out") && !s.contains_key("$merge"))
        .cloned()
        .map(Bson::Document)
        .collect();
    checked.insert("pipeline", reads);
    if !checked.contains_key("cursor") {
        checked.insert("cursor", Document::new());
    }
    let target_coll = match inner.get("aggregate") {
        Some(Bson::String(coll)) => Some(coll.clone()),
        _ => None,
    };
    if target_coll.is_some() {
        checked.insert("aggregate", DRY);
    }
    let plan_cache_stats = pipeline
        .first()
        .is_some_and(|s| s.contains_key("$planCacheStats"));
    match validate(crate::aggregate::aggregate, &checked, ctx) {
        // The dry run's namespace is not there, which is all a `$collStats`
        // stage is saying when it cannot read its count or its sizes.
        Err(e) if e.code == 26 && e.errmsg.contains("in $collStats stage") => {}
        // Nor a plan cache for `$planCacheStats` to read, which `explain`
        // lists without reading it (RM-466).
        Err(e) if e.code == 50933 && plan_cache_stats => {}
        other => other?,
    }
    let db = ctx.db_name.clone();
    if inline {
        echo = inner.clone();
        echo.insert("$db", db.clone());
    }
    // A change stream reads the oplog, through the stages it expands into.
    if pipeline
        .first()
        .is_some_and(|s| s.contains_key("$changeStream"))
    {
        return change_stream::reply(inner, &pipeline, echo, verbosity, ctx);
    }
    // `$planCacheStats` reads the collection's plan cache, not its
    // documents, through no view (measured 8.2.11).
    let own_source = plan_cache_stats || pipeline.first().is_some_and(shape::own_source);
    let Some(coll) = target_coll.filter(|_| !own_source) else {
        let target = inner.get("aggregate").cloned().unwrap_or(Bson::Int32(1));
        return own_source_reply(&target, &pipeline, echo, verbosity, ctx);
    };
    let mut s = shape::find(&coll, &doc! { "find": coll.clone() })?;
    s.hint = inner.get("hint").cloned();
    if let Some(c) = inner.get("collation") {
        s.options.insert("collation", c.clone());
    }
    let storage = ctx.storage()?;
    let request = if crate::views::is_view(storage, &db, &coll) {
        through_view(&s, pipeline, false, ctx)?
    } else {
        // The pipeline as mongod reorders it, which the per-stage counts
        // run a prefix of: `$score`, `$rankFusion` and `$scoreFusion` as the
        // stages that run them, which `explain` lists as the stages mongod
        // desugars them into (RM-200).
        let mut staged: Vec<Bson> = pipeline.into_iter().map(Bson::Document).collect();
        if crate::hybrid::mentions_hybrid(&staged) {
            staged = crate::hybrid::prepare_for_explain(staged, Some(&coll))?;
        }
        let pipeline: Vec<Document> = hoist::hoist_matches(&shape::desugared(&staged))
            .into_iter()
            .filter_map(|b| match b {
                Bson::Document(d) => Some(d),
                _ => None,
            })
            .collect();
        shape::absorb(&mut s, &pipeline);
        Request {
            shape: s,
            echo,
            pipeline,
        }
    };
    reply_for(&request, verbosity, ctx)
}

/// The field `$documents` unwinds its documents from (mongod's
/// `_tempDocumentsField`).
const DOCUMENTS_FIELD: &str = "_tempDocumentsField";

/// The reply for a pipeline that makes its own documents rather than read a
/// collection through a query (`$documents`, `$queryStats`,
/// `$listLocalSessions`, ...): its stages as mongod reorders and lists them,
/// and above `queryPlanner` what each returned (measured 8.2.11, RM-414).
/// `$documents` is the stages mongod runs it as -- a `$queue` of one empty
/// document, which the run has taken by the time it is listed, a
/// `$project` of the documents, an `$unwind` of them and a `$replaceRoot`
/// that a `$match` behind it moves ahead of.
fn own_source_reply(
    target: &Bson,
    pipeline: &[Document],
    echo: Document,
    verbosity: Verbosity,
    ctx: &mut CommandContext,
) -> HandlerResult {
    let db = ctx.db_name.clone();
    let wanted = verbosity != Verbosity::QueryPlanner;
    let plan_cache_stats = pipeline
        .first()
        .is_some_and(|s| s.contains_key("$planCacheStats"));
    // Run, `$planCacheStats` needs its collection; listed, it does not
    // (measured 8.2.11: `executionStats` of a missing one fails, with no
    // executor's prefix on the message).
    if let (true, true, Bson::String(coll)) = (wanted, plan_cache_stats, target) {
        let storage = ctx.storage()?;
        if plan_cache::place_of(storage, &db, coll)
            .map_err(crate::util::command_error)?
            .is_none()
        {
            return Err(err(
                50933,
                "Location50933",
                format!("collection '{db}.{coll}' does not exist"),
            ));
        }
    }
    // The `$match` that `$planCacheStats` absorbed runs as the stage's own.
    let mut absorbed = false;
    let (head, run_head, after) = match pipeline.split_first() {
        None => (Vec::new(), Vec::new(), Vec::new()),
        Some((source, rest)) => {
            let staged: Vec<Bson> = rest.iter().cloned().map(Bson::Document).collect();
            let mut after = shape::desugared(&staged);
            let mut head = shape::listed_stage(source, &db);
            let mut run_head = match source.get("$documents") {
                Some(docs) => {
                    // The `$replaceRoot` is one of the stages after it.
                    head.truncate(3);
                    after.insert(
                        0,
                        Bson::Document(doc! {
                            "$replaceRoot": { "newRoot": format!("${DOCUMENTS_FIELD}") }
                        }),
                    );
                    vec![
                        doc! { "$documents": [{}] },
                        doc! { "$addFields": { DOCUMENTS_FIELD: docs.clone() } },
                        doc! { "$unwind": { "path": format!("${DOCUMENTS_FIELD}") } },
                    ]
                }
                None => vec![source.clone()],
            };
            if plan_cache_stats {
                let taken = plan_cache::absorb_matches(after);
                head = vec![taken.listed];
                if let Some(m) = taken.run {
                    run_head.push(m);
                    absorbed = true;
                }
                after = taken.rest;
            }
            if source.contains_key("$listLocalSessions") {
                let caller = crate::session::authenticated(ctx);
                for stage in &mut head {
                    if let Some(Bson::Document(spec)) = stage.get("$listLocalSessions") {
                        let listed = listed_local_sessions(spec, &caller);
                        stage.insert("$listLocalSessions", listed);
                    }
                }
            }
            let after: Vec<Document> = hoist::hoist_matches(&after)
                .into_iter()
                .filter_map(|b| match b {
                    Bson::Document(d) => Some(d),
                    _ => None,
                })
                .collect();
            (head, run_head, after)
        }
    };
    // What each stage returns: the pipeline run through it, the first under
    // what a `$limit` downstream pulls ([`pulled_counts`]).
    let mut run = run_head.clone();
    run.extend(after.iter().cloned());
    let mut counts: Vec<i64> = Vec::new();
    let mut spills: Vec<secantus_core::memlimit::SpillReport> = Vec::new();
    let mut first = None;
    if wanted && !run.is_empty() {
        let options = Document::new();
        let mut before = secantus_core::memlimit::SpillReport::default();
        for i in 1..run.len() {
            let (n, report) = count_and_spills(ctx, target, &run[..=i], &options);
            spills.push(report.less(&before));
            before = report;
            counts.push(n);
        }
        first = Some(
            match pulled_counts(ctx, target, &run, 1, &options, &mut counts) {
                Some(n) => n,
                None => count_through(ctx, target, &run[..1], &options),
            },
        );
    }
    let counted = |stage: &mut Document, n: i64| {
        stage.insert("nReturned", Bson::Int64(n));
        stage.insert("executionTimeMillisEstimate", Bson::Int64(0));
    };
    let mut stages: Vec<Bson> = Vec::new();
    let documents = run_head.len() == 3;
    for (i, mut listed) in head.into_iter().enumerate() {
        if wanted {
            if listed.contains_key("$queue") {
                // The run has taken the one document it queued.
                listed.insert("$queue", Bson::Array(Vec::new()));
            }
            // `$documents`' `$project` and `$unwind` are runs of their own.
            let n = match (documents, i) {
                (true, 1 | 2) => counts.get(i - 1).copied(),
                _ if absorbed => counts.first().copied(),
                _ => first,
            };
            if let Some(n) = n {
                counted(&mut listed, n);
            }
        }
        stages.push(Bson::Document(listed));
    }
    let offset = run_head.len().saturating_sub(1);
    for (i, stage) in after.iter().enumerate() {
        for mut listed in shape::listed_stage(stage, &db) {
            if let Some(spilled) = spills.get(offset + i) {
                spill_fields(&mut listed, spilled);
            }
            if let Some(n) = counts.get(offset + i) {
                let input = &run[..run_head.len() + i];
                sub_stages::fill(ctx, target, input, &Document::new(), stage, &mut listed);
                counted(&mut listed, *n);
            }
            stages.push(Bson::Document(listed));
        }
    }
    let mut reply = doc! { "explainVersion": "1", "stages": stages };
    reply.insert("serverInfo", server_info(ctx));
    reply.insert("serverParameters", server_parameters(false));
    reply.insert("command", echo);
    reply.insert("ok", 1.0);
    Ok(reply)
}

/// A `$listLocalSessions` spec as mongod lists it back (measured 8.2.11):
/// `allUsers` always, and unless it is true the `users` whose sessions it
/// lists -- the caller's own when none were named (the empty user without
/// authentication).
fn listed_local_sessions(spec: &Document, caller: &[(String, String)]) -> Document {
    let all = spec.get("allUsers").is_some_and(crate::util::truthy);
    if all {
        return doc! { "allUsers": true };
    }
    let named: Vec<Bson> = spec
        .get_array("users")
        .map(|users| {
            users
                .iter()
                .filter_map(Bson::as_document)
                .map(|u| {
                    doc! {
                        "db": u.get("db").cloned().unwrap_or_else(|| "".into()),
                        "user": u.get("user").cloned().unwrap_or_else(|| "".into()),
                    }
                    .into()
                })
                .collect()
        })
        .unwrap_or_default();
    let users = if !named.is_empty() {
        named
    } else if caller.is_empty() {
        vec![doc! { "db": "", "user": "" }.into()]
    } else {
        caller
            .iter()
            .map(|(db, user)| doc! { "db": db.clone(), "user": user.clone() }.into())
            .collect()
    };
    doc! { "allUsers": false, "users": users }
}

#[cfg(test)]
mod tests {
    use super::id_hack_read;
    use bson::{doc, Bson, Document};

    fn reads(stages: Vec<Document>, hint: Option<&Bson>, same_collation: bool) -> bool {
        let pipeline: Vec<Bson> = stages.into_iter().map(Bson::Document).collect();
        let hoisted = super::hoist::hoist_matches(&pipeline);
        id_hack_read("c", &hoisted, hint, same_collation)
    }

    fn group() -> Document {
        doc! { "$group": { "_id": null, "r": { "$first": { "$mod": ["$n", 0] } } } }
    }

    /// The `$match`es mongod 8.2.11 answers with `explainVersion` 1 in front
    /// of a `$group` (2026-11-08, RM-330).
    #[test]
    fn an_id_point_read_is_classic() {
        for m in [
            doc! { "_id": 1 },
            doc! { "_id": { "$eq": 1 } },
            doc! { "_id": { "$in": [1] } },
            doc! { "_id": { "$in": [1, 1.0] } },
            doc! { "_id": { "$in": [{ "a": 1 }, { "a": 1 }] } },
            doc! { "_id": { "a": 1 } },
            doc! { "_id": { "$eq": { "$x": 1 } } },
            doc! { "_id": "x" },
            doc! { "_id": Bson::MinKey },
            doc! { "_id": Bson::MaxKey },
            doc! { "$and": [{ "_id": 1 }] },
            doc! { "$and": [{ "_id": 1 }, { "_id": 1 }] },
            doc! { "$or": [{ "_id": 1 }] },
            doc! { "_id": 1, "$expr": true },
        ] {
            assert!(
                reads(vec![doc! { "$match": m.clone() }, group()], None, true),
                "{m}"
            );
        }
        let m = doc! { "$match": { "_id": 1 } };
        for between in [
            doc! { "$sort": { "i": 1 } },
            doc! { "$project": { "n": 1 } },
            doc! { "$limit": 1 },
            doc! { "$skip": 0 },
            doc! { "$match": { "_id": 1 } },
        ] {
            assert!(
                reads(vec![m.clone(), between.clone(), group()], None, true),
                "{between}"
            );
        }
        assert!(reads(
            vec![doc! { "$sort": { "i": 1 } }, m.clone(), group()],
            None,
            true
        ));
    }

    /// The ones it answers with `explainVersion` 2: not an `_id` point read.
    #[test]
    fn anything_else_is_not() {
        for m in [
            doc! { "_id": { "$in": [1, 2] } },
            doc! { "_id": { "$in": [] } },
            doc! { "_id": { "$gt": 0 } },
            doc! { "_id": 1, "i": 1 },
            doc! { "i": 1 },
            doc! { "_id": [1] },
            doc! { "_id": null },
            doc! { "_id": { "$in": [null] } },
            doc! { "_id": { "$in": [[1]] } },
            doc! { "_id": { "$in": [1, null] } },
            doc! { "_id": { "$ne": 1 } },
            doc! { "_id": { "$regex": "a" } },
            doc! { "_id": { "$eq": Bson::RegularExpression(bson::Regex { pattern: "a".into(), options: String::new() }) } },
            doc! { "_id": { "$eq": 1, "$gt": 0 } },
            doc! { "_id": { "$exists": true } },
        ] {
            assert!(
                !reads(vec![doc! { "$match": m.clone() }, group()], None, true),
                "{m}"
            );
        }
        let m = doc! { "$match": { "_id": 1 } };
        assert!(!reads(
            vec![m.clone(), doc! { "$match": { "i": 1 } }, group()],
            None,
            true
        ));
        assert!(!reads(
            vec![m.clone(), doc! { "$skip": 1 }, group()],
            None,
            true
        ));
        let hint = Bson::Document(doc! { "_id": 1 });
        assert!(!reads(vec![m.clone(), group()], Some(&hint), true));
        assert!(!reads(
            vec![m.clone(), group()],
            Some(&Bson::String("_id_".into())),
            true
        ));
        assert!(!reads(vec![m, group()], None, false));
    }
}
