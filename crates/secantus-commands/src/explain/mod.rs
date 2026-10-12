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
//! Not reproduced: the rejected plans, the plan cache, and the choice
//! mongod makes by trial when several indexes could serve one query (a
//! fixed order of preference stands in for it, see `plan.rs`).

mod bounds;
mod exec;
mod node;
mod parsed;
mod plan;
mod shape;

use crate::util::{as_i64, command_error, decode_docs};
use crate::{CommandContext, CommandError, HandlerResult, SERVER_VERSION};
use bson::{doc, Bson, Document};
use node::{num, IndexInfo, Node, Stats};
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
    let reply = handler(inner, ctx)?;
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
        None => Err(err(73, "InvalidNamespace", "Collection name has invalid type missing")),
    }
}

/// An unknown hint, in the words every command on this server uses for it
/// (mongod prints its parse of the query here; `tasks/backlog.md`).
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
        Some(_) => Err(err(9, "FailedToParse", "Hint must be a string or an object")),
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
        .find_collated(db, coll, &Document::new(), None, None, None, &Document::new())
        .map_err(command_error)?;
    let docs = decode_docs(raw)?;
    let mut catalog = Vec::new();
    for spec in storage.list_indexes(db, coll).map_err(command_error)? {
        let Ok(key) = spec.get_document("key") else {
            continue;
        };
        let name = spec.get_str("name").unwrap_or("").to_string();
        let plain = key.values().all(|v| matches!(as_i64(v), Some(1 | -1)))
            && !key.values().any(|v| matches!(v, Bson::String(_)));
        let multikey = storage.index_is_multikey(db, coll, &name);
        catalog.push(Rc::new(IndexInfo {
            unique: name == "_id_" || spec.get_bool("unique").unwrap_or(false),
            sparse: spec.get_bool("sparse").unwrap_or(false),
            partial: spec.get_document("partialFilterExpression").ok().cloned(),
            multikey_paths: if multikey {
                exec::multikey_paths(&docs, key)
            } else {
                vec![vec![]; key.len()]
            },
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
    Ok(Target {
        exists,
        docs,
        catalog,
    })
}

/// The plan for one command over one collection.
fn plan_for(
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    t: &Target,
    ctx: &CommandContext,
) -> Result<Node, CommandError> {
    let ns = format!("{}.{}", ctx.db_name, s.coll);
    if !t.exists {
        let eof = Node::Eof("nonExistentNamespace");
        return Ok(match s.kind {
            Kind::Count => Node::Count(Box::new(eof)),
            _ => eof,
        });
    }
    let storage = ctx.storage()?;
    let implies = |partial: &Document| storage.query_implies_partial(&s.filter, partial);
    let hint = resolve_hint(s.hint.as_ref(), &t.catalog)?;
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
        implies: &implies,
    };
    Ok(match &s.kind {
        Kind::Find => plan::plan(&q, &t.catalog),
        Kind::Count => plan::count(&q, &t.catalog),
        Kind::Distinct(key) => plan::distinct(&q, &t.catalog, key),
        Kind::Update {
            upsert, returns, ..
        } => plan::write(&q, &t.catalog, false, false, *upsert, *returns),
        Kind::Delete { multi, returns } => {
            plan::write(&q, &t.catalog, true, *multi, false, *returns)
        }
    })
}

fn query_planner(
    ns: &str,
    s: &Shape,
    canonical: &Document,
    collation: Option<&Collation>,
    plan: &Node,
) -> Document {
    let mut out = doc! { "namespace": ns };
    let fast_count = match plan {
        Node::FastCount => true,
        Node::Count(input) => matches!(**input, Node::Eof("nonExistentNamespace")),
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

fn execution_stats(plan: &Node, stats: &Stats, verbosity: Verbosity) -> Document {
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
        out.insert("allPlansExecution", Bson::Array(vec![]));
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

/// Plan and, above `queryPlanner`, run one shape; assemble the reply.
fn reply_for(
    s: &Shape,
    echo: Document,
    verbosity: Verbosity,
    ctx: &CommandContext,
) -> HandlerResult {
    let storage = ctx.storage()?;
    let db = ctx.db_name.clone();
    let ns = format!("{db}.{}", s.coll);
    let collation = crate::util::effective_collation(storage, &db, &s.coll, &s.options)?;
    let canonical = parsed::parsed_query(&s.filter);
    let t = target(ctx, &s.coll)?;
    let plan = plan_for(s, &canonical, collation.as_ref(), &t, ctx)?;
    let planner = query_planner(&ns, s, &canonical, collation.as_ref(), &plan);
    let stats = (verbosity != Verbosity::QueryPlanner).then(|| {
        let (multi, upsert, update, returns) = match &s.kind {
            Kind::Update {
                multi,
                upsert,
                update,
                returns,
            } => (*multi, *upsert, update.as_ref(), *returns),
            Kind::Delete { multi, returns } => (*multi, false, None, *returns),
            _ => (true, false, None, false),
        };
        let env = exec::Env {
            docs: &t.docs,
            collation: collation.as_ref(),
            skip: s.skip,
            limit: s.limit,
            multi,
            upsert,
            update,
            returns,
        };
        exec::run(&plan, &env)
    });
    let mut reply = doc! { "explainVersion": "1" };
    if s.rest.is_empty() {
        reply.insert("queryPlanner", planner);
        if let Some(st) = &stats {
            reply.insert("executionStats", execution_stats(&plan, st, verbosity));
        }
        reply.insert("command", echo);
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters());
    } else {
        let mut cursor = doc! { "queryPlanner": planner };
        if let Some(st) = &stats {
            cursor.insert("executionStats", execution_stats(&plan, st, verbosity));
        }
        let mut stages = vec![Bson::Document(doc! { "$cursor": cursor })];
        stages.extend(
            s.rest
                .iter()
                .map(|st| Bson::Document(shape::listed_stage(st, &db))),
        );
        reply.insert("stages", stages);
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters());
        reply.insert("command", echo);
    }
    reply.insert("ok", 1.0);
    Ok(reply)
}

/// A read on a view is the view's pipeline with the read's own stages after.
fn through_view(mut s: Shape, extra: Vec<Document>, ctx: &CommandContext) -> Result<Shape, CommandError> {
    let storage = ctx.storage()?;
    let mut tail = extra;
    for _ in 0..20 {
        let options = storage
            .get_collection_options(&ctx.db_name, &s.coll)
            .unwrap_or_default();
        let Ok(on) = options.get_str("viewOn") else {
            break;
        };
        let mut pipeline: Vec<Document> = options
            .get_array("pipeline")
            .map(|p| p.iter().filter_map(Bson::as_document).cloned().collect())
            .unwrap_or_default();
        pipeline.extend(tail);
        tail = pipeline;
        s.coll = on.to_string();
    }
    s.filter = Document::new();
    s.sort = None;
    s.projection = None;
    s.skip = 0;
    s.limit = 0;
    shape::absorb(&mut s, &tail);
    Ok(s)
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
        return Err(err(72, "InvalidOptions", "Command does not support writeConcern"));
    }
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
    let echo = shape::echo(&inner, &db);

    let lowered = name.to_ascii_lowercase();
    let shape = match name.as_str() {
        "find" => {
            let coll = collection(&inner, "find", &db)?;
            validate(crate::find::find, &inner, ctx)?;
            shape::find(&coll, &inner)?
        }
        "count" => {
            let coll = collection(&inner, "count", &db)?;
            validate(crate::crud::count, &inner, ctx)?;
            shape::count(&coll, &inner)?
        }
        "distinct" => {
            let coll = collection(&inner, "distinct", &db)?;
            let s = shape::distinct(&coll, &inner)?;
            validate(crate::distinct::distinct, &inner, ctx)?;
            s
        }
        "aggregate" => return aggregate(&inner, echo, verbosity, ctx),
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
        "explain" => return Err(err(20, "IllegalOperation", "Explain cannot explain itself.")),
        _ if known_command(&name) => {
            return Err(err(20, "IllegalOperation", format!("Cannot explain cmd: {name}")))
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
    let shape = if crate::views::is_view(storage, &db, &shape.coll) {
        let extra = shape::find_as_pipeline(&shape);
        through_view(shape, extra, ctx)?
    } else {
        shape
    };
    reply_for(&shape, echo, verbosity, ctx)
}

fn refuse_view(coll: &str, ctx: &CommandContext) -> Result<(), CommandError> {
    let storage = ctx.storage()?;
    if crate::views::is_view(storage, &ctx.db_name, coll) {
        return Err(err(
            166,
            "CommandNotSupportedOnView",
            format!("Namespace {}.{coll} is a view, not a collection", ctx.db_name),
        ));
    }
    Ok(())
}

fn aggregate(
    inner: &Document,
    echo: Document,
    verbosity: Verbosity,
    ctx: &mut CommandContext,
) -> HandlerResult {
    if inner.contains_key("explain") {
        return Err(err(
            9,
            "FailedToParse",
            "The 'explain' option is illegal when a explain verbosity is also provided",
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
            return Err(err(14, "TypeMismatch", "A pipeline must be an array of objects"))
        }
    };
    // The command's own checks, without the stage that would write.
    let mut dry = inner.clone();
    let reads: Vec<Bson> = pipeline
        .iter()
        .filter(|s| !s.contains_key("$out") && !s.contains_key("$merge"))
        .cloned()
        .map(Bson::Document)
        .collect();
    dry.insert("pipeline", reads);
    if !dry.contains_key("cursor") {
        dry.insert("cursor", Document::new());
    }
    validate(crate::aggregate::aggregate, &dry, ctx)?;
    let Some(Bson::String(coll)) = inner.get("aggregate") else {
        // A collectionless aggregate: the stages, as given.
        let stages: Vec<Bson> = pipeline.into_iter().map(Bson::Document).collect();
        let mut reply = doc! { "explainVersion": "1", "stages": stages };
        reply.insert("serverInfo", server_info(ctx));
        reply.insert("serverParameters", server_parameters());
        reply.insert("command", echo);
        reply.insert("ok", 1.0);
        return Ok(reply);
    };
    let s = shape::find(coll, &doc! { "find": coll.clone() })?;
    let mut s = Shape {
        hint: inner.get("hint").cloned(),
        options: match inner.get("collation") {
            Some(c) => doc! { "collation": c.clone() },
            None => Document::new(),
        },
        ..s
    };
    let storage = ctx.storage()?;
    let s = if crate::views::is_view(storage, &ctx.db_name, coll) {
        through_view(s, pipeline, ctx)?
    } else {
        shape::absorb(&mut s, &pipeline);
        s
    };
    reply_for(&s, echo, verbosity, ctx)
}
