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
//! Not reproduced: the plan cache. The choice mongod makes by trial when
//! several plans could serve one query is counted over the collection
//! (`trial.rs`), and the plans it tried are listed as `rejectedPlans` and
//! `allPlansExecution`; between plans the trial scores alike a fixed order
//! of preference decides (`plan.rs`). A multikey index with two predicates
//! on one field is tried once here, where mongod tries a plan per predicate.

mod bounds;
pub(crate) mod debug_string;
mod deps;
pub(crate) mod distinct;
mod exec;
pub(crate) mod geo;
pub(crate) mod hoist;
mod json_schema;
mod node;
pub(crate) mod order;
mod parsed;
mod plan;
mod shape;
pub(crate) mod summary;
mod trial;

use crate::util::{as_i64, command_error, decode_docs};
use crate::{CommandContext, CommandError, HandlerResult, SERVER_VERSION};
use bson::{doc, Bson, Document};
pub(crate) use debug_string::CanonicalQuery;
pub(crate) use node::IndexInfo;
use node::{num, Node, Stats};
pub(crate) use parsed::parsed_query;
use plan::{Hint, Query};
use secantus_core::collation::Collation;
use secantus_core::query::bson_type_name;
use shape::{Kind, Shape};
use std::rc::Rc;

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
    };
    err(
        2,
        "BadValue",
        query.planner_error("hint provided does not correspond to an existing index"),
    )
}

fn resolve_hint(hint: Option<&Bson>, catalog: &[Rc<IndexInfo>]) -> Result<Hint, CommandError> {
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
        pipeline: false,
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
        };
    };
    shape::absorb(&mut s, &stages);
    PipelineQuery {
        filter: s.filter,
        sort: s.sort,
        projection: s.projection,
        skip: s.skip,
        limit: s.limit,
    }
}

/// Everything read from storage to plan one collection.
struct Target {
    exists: bool,
    docs: Vec<Document>,
    catalog: Vec<Rc<IndexInfo>>,
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

/// What the planner knows about each index of a collection. The multikey
/// paths are read from `docs` when the caller holds the whole collection,
/// and from the store's own record of them when it does not.
fn catalog(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    docs: Option<&[Document]>,
) -> Result<Vec<Rc<IndexInfo>>, crate::storage::StorageError> {
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
        catalog.push(Rc::new(IndexInfo {
            unique: name == "_id_" || spec.get_bool("unique").unwrap_or(false),
            sparse: spec.get_bool("sparse").unwrap_or(false),
            partial: spec.get_document("partialFilterExpression").ok().cloned(),
            multikey_paths,
            version: spec.get("v").and_then(as_i64).unwrap_or(2) as i32,
            collation: spec
                .get_document("collation")
                .ok()
                .and_then(secantus_core::collation::parse),
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
    fn env(&self) -> Option<exec::Env<'_>> {
        Some(exec::Env {
            docs: self.docs,
            collation: self.collation,
            skip: 0,
            limit: 0,
            multi: true,
            upsert: false,
            returns: false,
            known: None,
        })
    }
}

/// The plan for one command over one collection.
fn plan_for(
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    t: &Target,
    ctx: &CommandContext,
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
        pipeline: s.pipeline,
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

fn query_planner(
    ns: &str,
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    plan: &Node,
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
        out.insert("indexFilterSet", false);
    }
    if s.optimized_pipeline {
        out.insert("optimizedPipeline", true);
    }
    out.insert("maxIndexedOrSolutionsReached", false);
    out.insert("maxIndexedAndSolutionsReached", false);
    out.insert("maxScansToExplodeReached", false);
    out.insert("prunedSimilarIndexes", false);
    let mut winning = doc! { "isCached": false };
    winning.extend(plan.to_doc(None));
    out.insert("winningPlan", winning);
    out.insert("rejectedPlans", Bson::Array(vec![]));
    out
}

/// A `$group` mongod answers from a DISTINCT_SCAN (`$groupByDistinctScan`,
/// measured 8.2.11): its `_id` one field path (or a document of one), its
/// accumulators `$first` / `$last` of field paths, after no sort or a sort
/// on that field alone, over an index that serves `distinct` on the field.
/// The scan fetches the documents when an accumulator reads another field.
/// `None`: not such a `$group`, or no index serves it.
fn distinct_group(
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    t: &Target,
    ctx: &CommandContext,
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
    let mut fetching = false;
    for (name, acc) in spec.iter().filter(|(k, _)| *k != "_id") {
        let Some((op, arg)) = acc.as_document().and_then(|d| d.iter().next()) else {
            return Ok(None);
        };
        let Some(read) = path(arg).filter(|_| op == "$first" || op == "$last") else {
            return Ok(None);
        };
        fetching |= read != field;
        root.insert(name.clone(), arg.clone());
    }
    let backward = match &s.sort {
        None => false,
        Some(sort) => match (sort.len(), sort.get(&field).and_then(as_i64)) {
            (1, Some(dir)) => dir < 0,
            _ => return Ok(None),
        },
    };
    let mut asked = s.clone();
    asked.kind = Kind::Distinct(field.clone());
    asked.sort = None;
    asked.projection = None;
    let (node, ranked) = plan_for(&asked, canonical, collation, t, ctx)?;
    // A scan through a collated index fetches (`plan::distinct`).
    let scan = match node {
        Node::Projection { input, .. } => *input,
        other => other,
    };
    let Node::DistinctScan(mut ix, width, scan_fetches) = scan else {
        return Ok(None);
    };
    // Its keys are an array's elements, where `$group` wants the arrays
    // (measured 8.2.11, 2026-10-16: a multikey field is grouped without one).
    if ix
        .index
        .multikey_paths
        .get(width - 1)
        .is_some_and(|p| !p.is_empty())
    {
        return Ok(None);
    }
    fetching |= scan_fetches;
    ix.backward = backward;
    let scan = Node::DistinctScan(ix, width, fetching);
    let plan = if fetching {
        scan
    } else {
        Node::Projection {
            name: if field.contains('.') {
                "PROJECTION_DEFAULT"
            } else {
                "PROJECTION_COVERED"
            },
            spec: doc! { field.clone(): 1_i32, "_id": 0_i32 },
            input: Box::new(scan),
        }
    };
    // The other plans tried read through the projection of the fields the
    // `$group` needs.
    let ranked: Vec<plan::Ranked> = ranked
        .into_iter()
        .map(|mut r| {
            if !matches!(r.plan, Node::Projection { .. }) {
                if let Some(p) = &s.projection {
                    r.plan = Node::Projection {
                        name: "PROJECTION_SIMPLE",
                        spec: p.clone(),
                        input: Box::new(r.plan),
                    };
                    r.stats = Stats {
                        n_returned: r.stats.n_returned,
                        children: vec![r.stats],
                        ..Stats::default()
                    };
                }
            }
            r
        })
        .collect();
    let mut ranked: Vec<plan::Ranked> = ranked;
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
) -> Document {
    let (keys, docs) = stats.totals();
    let mut stages = doc! { "isCached": false };
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
                let (keys, docs) = st.totals();
                let mut stages = doc! { "isCached": false };
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

fn server_parameters() -> Document {
    doc! {
        "internalQueryFacetBufferSizeBytes": 104_857_600_i32,
        "internalQueryFacetMaxOutputDocSizeBytes": 104_857_600_i32,
        "internalLookupStageIntermediateDocumentMaxSizeBytes": 104_857_600_i32,
        "internalDocumentSourceGroupMaxMemoryBytes": 104_857_600_i32,
        "internalQueryMaxBlockingSortMemoryUsageBytes": 104_857_600_i32,
        "internalQueryProhibitBlockingMergeOnMongoS": 0_i32,
        "internalQueryMaxAddToSetBytes": 104_857_600_i32,
        "internalDocumentSourceSetWindowFieldsMaxMemoryBytes": 104_857_600_i32,
        "internalQueryFrameworkControl": "trySbeRestricted",
        "internalQueryPlannerIgnoreIndexWithCollationForRegex": 1_i32,
    }
}

/// A namespace that does not exist: a command is run against it for its
/// argument checks, so nothing in the real collection can fail the run.
const DRY: &str = "explain.dry.run";

/// How many documents come out of `pipeline` over `coll`.
fn count_through(
    ctx: &mut CommandContext,
    coll: &str,
    pipeline: &[Document],
    options: &Document,
) -> i64 {
    let stages: Vec<Bson> = pipeline.iter().cloned().map(Bson::Document).collect();
    let mut cmd = doc! {
        "aggregate": coll,
        "pipeline": stages,
        "cursor": { "batchSize": i32::MAX },
    };
    if let Some(c) = options.get("collation") {
        cmd.insert("collation", c.clone());
    }
    let Ok(reply) = order::uncounted(|| crate::aggregate::aggregate(&cmd, ctx)) else {
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
                | "$unwind"
                | "$project"
                | "$addFields"
                | "$set"
                | "$unset"
                | "$replaceRoot"
                | "$replaceWith"
        )
    )
}

/// mongod's per-stage counts in front of a `$limit` (measured 8.2.11): a
/// stage that streams hands over only what the stage after it pulled, so
/// `[$match, $unwind: "$t", $limit: 3]` returns 2 documents from the
/// cursor and 3 from the `$unwind`. Rewrites `counts` (the stages after the
/// cursor) and returns what the cursor handed over, when it streams into
/// the limit.
fn pulled_counts(
    ctx: &mut CommandContext,
    coll: &str,
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
            count_through(ctx, coll, &pipeline[..taken], options)
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
                count_through(ctx, coll, &probe, options)
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
    // What each stage after the cursor returns: the pipeline run that far.
    let taken = r.pipeline.len() - s.rest.len();
    let mut stage_counts: Vec<i64> = if wanted {
        (0..s.rest.len())
            .map(|i| count_through(ctx, &s.coll, &r.pipeline[..taken + i + 1], &s.options))
            .collect()
    } else {
        vec![]
    };
    // Under a `$limit`, the stages that stream into it hand over only what
    // it pulls; `None`: the cursor's own count stands.
    let cursor_pulled = if wanted {
        pulled_counts(
            ctx,
            &s.coll,
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
    let regrouped;
    let (s, (plan, ranked)) = match grouped {
        Some((shape, plan, ranked)) => {
            regrouped = shape;
            (&regrouped, (plan, ranked))
        }
        None => (s, plan_for(s, &canonical, collation.as_ref(), &t, ctx)?),
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
    let mut planner = query_planner(&ns, s, &canonical, collation.as_ref(), &plan);
    if filtered && planner.contains_key("indexFilterSet") {
        planner.insert("indexFilterSet", true);
    }
    if tried.len() > 1 {
        let rejected: Vec<Bson> = tried[1..]
            .iter()
            .map(|(node, _, _)| {
                let mut d = doc! { "isCached": false };
                d.extend(node.to_doc(None));
                Bson::Document(d)
            })
            .collect();
        planner.insert("rejectedPlans", rejected);
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
                execution_stats(&plan, st, verbosity, &tried),
            );
        }
        reply.insert("command", r.echo.clone());
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters());
    } else {
        let mut cursor = doc! { "queryPlanner": planner };
        if let Some(st) = &stats {
            cursor.insert(
                "executionStats",
                execution_stats(&plan, st, verbosity, &tried),
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
                if let Some(n) = stage_counts.get(i) {
                    counted(&mut listed, *n);
                }
                stages.push(Bson::Document(listed));
            }
        }
        reply.insert("stages", stages);
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters());
        reply.insert("command", r.echo.clone());
    }
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
    let pipeline: Vec<Document> = hoist::hoist_matches(&stages)
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
pub fn explain(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
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
    let db = ctx.db_name.clone();
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
    let echo = shape::echo(&inner, &db);

    let lowered = name.to_ascii_lowercase();
    let shape = match name.as_str() {
        "find" => {
            let coll = collection(&inner, "find", &db)?;
            validate(crate::find::find, &dry(&inner, "find"), ctx)?;
            shape::find(&coll, &inner)?
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
        "explain" => {
            return Err(err(
                20,
                "IllegalOperation",
                "Explain cannot explain itself.",
            ))
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
        _ if known_command(&name) => {
            return Err(err(
                20,
                "IllegalOperation",
                format!("Cannot explain cmd: {name}"),
            ))
        }
        _ => {
            return Err(err(
                59,
                "CommandNotFound",
                format!("Explain failed due to unknown command: {name}"),
            ))
        }
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
    match validate(crate::aggregate::aggregate, &checked, ctx) {
        // The dry run's namespace is not there, which is all a `$collStats`
        // stage is saying when it cannot read its count or its sizes.
        Err(e) if e.code == 26 && e.errmsg.contains("in $collStats stage") => {}
        other => other?,
    }
    let db = ctx.db_name.clone();
    if inline {
        echo = inner.clone();
        echo.insert("$db", db.clone());
    }
    let own_source = pipeline.first().is_some_and(shape::own_source);
    let Some(coll) = target_coll.filter(|_| !own_source) else {
        // No collection is read through a query: the stages, as mongod lists them.
        let stages: Vec<Bson> = pipeline
            .iter()
            .flat_map(|s| shape::listed_stage(s, &db))
            .map(Bson::Document)
            .collect();
        let mut reply = doc! { "explainVersion": "1", "stages": stages };
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters());
        reply.insert("command", echo);
        reply.insert("ok", 1.0);
        return Ok(reply);
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
        // run a prefix of.
        let staged: Vec<Bson> = pipeline.into_iter().map(Bson::Document).collect();
        let pipeline: Vec<Document> = hoist::hoist_matches(&staged)
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
