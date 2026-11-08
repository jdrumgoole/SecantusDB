//! What each explainable command asks the planner for.

use crate::util::as_i64;
use crate::CommandError;
use bson::{doc, Bson, Document};
use secantus_core::query::bson_type_name;

/// What the command does with the documents the query finds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    Find,
    Count,
    Distinct(String),
    /// `returns`: findAndModify hands the document back.
    Update {
        multi: bool,
        upsert: bool,
        returns: bool,
    },
    Delete {
        multi: bool,
        returns: bool,
    },
}

/// One command, reduced to the query the planner sees.
#[derive(Clone, Debug)]
pub(crate) struct Shape {
    pub coll: String,
    pub kind: Kind,
    pub filter: Document,
    pub sort: Option<Document>,
    pub projection: Option<Document>,
    pub skip: i64,
    pub limit: i64,
    pub hint: Option<Bson>,
    /// The document that names the collation, for `effective_collation`.
    pub options: Document,
    pub min_max: Option<&'static str>,
    pub return_key: bool,
    pub show_record_id: bool,
    pub batch_size: bool,
    /// An aggregate the planner took whole.
    pub optimized_pipeline: bool,
    /// The aggregate stages the planner did not take.
    pub rest: Vec<Document>,
    /// The `$project` (or `$unset`) stage the planner took as the
    /// projection, as written, rather than the fields the stages it left read.
    pub projection_stage: Option<Document>,
    /// The pipeline came from a view definition or an aggregate command.
    pub pipeline: bool,
}

impl Shape {
    fn new(coll: &str, kind: Kind, inner: &Document) -> Shape {
        let mut options = Document::new();
        if let Some(c) = inner.get("collation") {
            options.insert("collation", c.clone());
        }
        Shape {
            coll: coll.to_string(),
            kind,
            filter: Document::new(),
            sort: None,
            projection: None,
            skip: 0,
            limit: 0,
            hint: inner.get("hint").cloned(),
            options,
            min_max: None,
            return_key: false,
            show_record_id: false,
            batch_size: false,
            optimized_pipeline: false,
            rest: vec![],
            projection_stage: None,
            pipeline: false,
        }
    }
}

fn err(code: i32, name: &str, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, name, msg.into())
}

fn wrong_type(path: &str, v: &Bson, expected: &str) -> CommandError {
    err(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
            bson_type_name(v)
        ),
    )
}

fn missing(path: &str) -> CommandError {
    err(
        40414,
        "IDLFailedToParse",
        format!("BSON field '{path}' is missing but a required field"),
    )
}

/// An optional document field; `null` counts as absent.
fn doc_arg(inner: &Document, field: &str, path: &str) -> Result<Document, CommandError> {
    match inner.get(field) {
        None | Some(Bson::Null) => Ok(Document::new()),
        Some(Bson::Document(d)) => Ok(d.clone()),
        Some(other) => Err(wrong_type(path, other, "object")),
    }
}

fn int_arg(inner: &Document, field: &str) -> i64 {
    inner.get(field).and_then(as_i64).unwrap_or(0)
}

fn flag(inner: &Document, field: &str) -> bool {
    inner.get(field).is_some_and(crate::util::truthy)
}

pub(crate) fn find(coll: &str, inner: &Document) -> Result<Shape, CommandError> {
    let mut s = Shape::new(coll, Kind::Find, inner);
    s.filter = doc_arg(inner, "filter", "FindCommandRequest.filter")?;
    s.sort = inner.get_document("sort").ok().cloned();
    // `sort: {$natural: ±1}` is storage order: a hint, and no sort at all.
    if let Some(natural) = s
        .sort
        .as_ref()
        .filter(|x| x.len() == 1)
        .and_then(|x| x.get("$natural"))
    {
        s.hint = Some(Bson::Document(doc! { "$natural": natural.clone() }));
        s.sort = None;
    }
    s.projection = inner.get_document("projection").ok().cloned();
    s.skip = int_arg(inner, "skip");
    s.limit = int_arg(inner, "limit");
    if inner.contains_key("min") || inner.contains_key("max") {
        s.min_max = Some(if inner.contains_key("min") {
            "min"
        } else {
            "max"
        });
        for field in ["min", "max"] {
            if let Some(v) = inner.get(field) {
                s.options.insert(field, v.clone());
            }
        }
    }
    s.return_key = flag(inner, "returnKey");
    s.show_record_id = flag(inner, "showRecordId");
    s.batch_size = inner.contains_key("batchSize");
    Ok(s)
}

pub(crate) fn count(coll: &str, inner: &Document) -> Result<Shape, CommandError> {
    let mut s = Shape::new(coll, Kind::Count, inner);
    s.filter = doc_arg(inner, "query", "count.query")?;
    s.skip = int_arg(inner, "skip");
    s.limit = int_arg(inner, "limit").abs();
    Ok(s)
}

pub(crate) fn distinct(coll: &str, inner: &Document) -> Result<Shape, CommandError> {
    let key = match inner.get("key") {
        None => return Err(missing("distinctCommandRequest.key")),
        Some(Bson::String(k)) => k.clone(),
        Some(other) => return Err(wrong_type("distinctCommandRequest.key", other, "string")),
    };
    let mut s = Shape::new(coll, Kind::Distinct(key), inner);
    s.filter = doc_arg(inner, "query", "distinctCommandRequest.query")?;
    Ok(s)
}

/// The single statement of an explained write batch.
fn statement<'a>(
    inner: &'a Document,
    cmd: &str,
    field: &str,
) -> Result<&'a Document, CommandError> {
    let list = match inner.get(field) {
        None => return Err(missing(&format!("{cmd}.{field}"))),
        Some(Bson::Array(a)) => a,
        Some(other) => return Err(wrong_type(&format!("{cmd}.{field}"), other, "array")),
    };
    match list.as_slice() {
        [] => Err(err(
            16,
            "InvalidLength",
            "Write batch sizes must be between 1 and 100000. Got 0 operations.",
        )),
        [Bson::Document(one)] => Ok(one),
        [other] => Err(wrong_type(&format!("{cmd}.{field}"), other, "object")),
        _ => Err(err(
            16,
            "InvalidLength",
            "explained write batches must be of size 1",
        )),
    }
}

/// An update document the server would refuse, as mongod words it.
fn check_update(u: &Bson) -> Result<(), CommandError> {
    if let Bson::Document(d) = u {
        if let Some((code, msg)) = secantus_core::update::update_spec_error(d) {
            return Err(err(code, &crate::util::error_code_name(code), msg));
        }
    }
    Ok(())
}

pub(crate) fn update(coll: &str, inner: &Document) -> Result<Shape, CommandError> {
    let st = statement(inner, "update", "updates")?;
    let Some(u) = st.get("u") else {
        return Err(missing("update.updates.u"));
    };
    let kind = Kind::Update {
        multi: flag(st, "multi"),
        upsert: flag(st, "upsert"),
        returns: false,
    };
    check_update(u)?;
    let mut s = Shape::new(coll, kind, st);
    s.filter = doc_arg(st, "q", "update.updates.q")?;
    Ok(s)
}

pub(crate) fn delete(coll: &str, inner: &Document) -> Result<Shape, CommandError> {
    let st = statement(inner, "delete", "deletes")?;
    let limit = match st.get("limit").map(as_i64) {
        None => return Err(missing("delete.deletes.limit")),
        Some(Some(n @ (0 | 1))) => n,
        Some(other) => {
            return Err(err(
                9,
                "FailedToParse",
                format!(
                    "The limit field in delete objects must be 0 or 1. Got {}",
                    other.map_or_else(|| "?".to_string(), |n| n.to_string())
                ),
            ))
        }
    };
    let kind = Kind::Delete {
        multi: limit == 0,
        returns: false,
    };
    let mut s = Shape::new(coll, kind, st);
    s.filter = doc_arg(st, "q", "delete.deletes.q")?;
    Ok(s)
}

pub(crate) fn find_and_modify(coll: &str, inner: &Document) -> Result<Shape, CommandError> {
    let filter = doc_arg(inner, "query", "findAndModify.query")?;
    let remove = flag(inner, "remove");
    let update = inner.get("update").filter(|u| !matches!(u, Bson::Null));
    let kind = match (remove, update) {
        (false, None) => {
            return Err(err(
                9,
                "FailedToParse",
                "Either an update or remove=true must be specified",
            ))
        }
        (true, Some(_)) => {
            return Err(err(
                9,
                "FailedToParse",
                "Cannot specify both an update and remove=true",
            ))
        }
        (true, None) => Kind::Delete {
            multi: false,
            returns: true,
        },
        (false, Some(u)) => {
            check_update(u)?;
            Kind::Update {
                multi: false,
                upsert: flag(inner, "upsert"),
                returns: true,
            }
        }
    };
    let mut s = Shape::new(coll, kind, inner);
    s.filter = filter;
    s.sort = inner.get_document("sort").ok().cloned();
    s.projection = inner.get_document("fields").ok().cloned();
    Ok(s)
}

fn is_paging(stage: &Document) -> bool {
    stage.contains_key("$skip") || stage.contains_key("$limit")
}

/// A stage that hands on one document for each it reads: a `$skip` or
/// `$limit` behind it may go ahead of it (`$score` too, RM-200).
fn keeps_count(stage: &Document) -> bool {
    [
        "$project",
        "$unset",
        "$addFields",
        "$set",
        "$_secantusScore",
    ]
    .iter()
    .any(|k| stage.contains_key(*k))
}

/// Whether the query can take every stage in `ahead`, so a `$skip` or
/// `$limit` pulled up behind them reaches it: its own paging, the
/// `$match` / `$sort` it takes before any paging, and the stages that keep
/// the count. A `$match` after a `$limit`, or an `$unwind`, ends it.
fn query_ahead(ahead: &[Bson]) -> bool {
    let mut paged = false;
    ahead.iter().all(|s| {
        let Some(d) = s.as_document() else {
            return false;
        };
        if is_paging(d) {
            paged = true;
            true
        } else if d.contains_key("$match") || d.contains_key("$sort") || d.contains_key(WINDOW_SORT)
        {
            !paged
        } else {
            keeps_count(d)
        }
    })
}

/// Moves each `$skip` / `$limit` ahead of the count-keeping stages before
/// it while the query can take what is ahead of it; whether any moved.
fn pull_paging(stages: &mut [Bson]) -> bool {
    let doc = |s: &Bson| s.as_document().cloned().unwrap_or_default();
    let mut moved = false;
    let mut i = 1;
    while i < stages.len() {
        if is_paging(&doc(&stages[i]))
            && keeps_count(&doc(&stages[i - 1]))
            && query_ahead(&stages[..i])
        {
            stages.swap(i - 1, i);
            moved = true;
            i = (i - 1).max(1);
        } else {
            i += 1;
        }
    }
    moved
}

/// The leading stages of a pipeline the query planner absorbs, folded into
/// `shape`; the stages it leaves are kept in `shape.rest`.
pub(crate) fn absorb(shape: &mut Shape, pipeline: &[Document]) {
    // mongod optimises the pipeline to a fixed point (8.2.11): a `$match`
    // goes ahead of the stages that leave the fields it reads, and a `$skip`
    // or `$limit` ahead of the stages that keep the number of documents, as
    // far as the query (`pull_paging`). Each move can open the other:
    // `[$project, $limit, $match]` reads as `[$limit, $match, $project]`,
    // so the query takes the `$limit` and the plan projects what the two
    // stages left read (RM-308); `[$addFields, $limit, $match]` as
    // `[$limit, $match, $addFields]` (RM-371).
    let mut stages: Vec<Bson> = pipeline.iter().cloned().map(Bson::Document).collect();
    loop {
        stages = super::hoist::hoist_matches(&stages);
        if !pull_paging(&mut stages) {
            break;
        }
    }
    let hoisted: Vec<Document> = stages
        .into_iter()
        .filter_map(|s| match s {
            Bson::Document(d) => Some(d),
            _ => None,
        })
        .collect();
    let pipeline = hoisted.as_slice();
    let mut filters: Vec<Document> = Vec::new();
    if !shape.filter.is_empty() {
        filters.push(shape.filter.clone());
    }
    let mut limit: Option<i64> = (shape.limit > 0).then_some(shape.limit);
    let mut taken = 0;
    for (i, stage) in pipeline.iter().enumerate() {
        let Some((name, arg)) = stage.iter().next() else {
            break;
        };
        let paged = shape.skip > 0 || limit.is_some();
        match (name.as_str(), arg) {
            ("$match", Bson::Document(m)) if !paged && shape.projection.is_none() => {
                if !m.is_empty() {
                    filters.push(m.clone());
                }
            }
            ("$sort" | WINDOW_SORT, Bson::Document(s)) if !paged && shape.projection.is_none() => {
                shape.sort = Some(s.clone());
            }
            ("$skip", n) if shape.projection.is_none() => {
                let Some(n) = as_i64(n) else { break };
                shape.skip += n;
                limit = limit.map(|l| (l - n).max(0));
            }
            ("$limit", n) if shape.projection.is_none() => {
                let Some(n) = as_i64(n) else { break };
                limit = Some(limit.map_or(n, |l| l.min(n)));
            }
            // A projection, as mongod prints it, ends what the query takes:
            // the stages after it stay in the pipeline. One that computes or
            // reaches into a document is left to the pipeline behind a sort
            // (measured 8.2.11).
            ("$project" | "$unset", _) => {
                let listed = listed_stage(stage, "");
                let Some(p) = listed
                    .first()
                    .filter(|_| listed.len() == 1)
                    .and_then(|d| d.get_document("$project").ok())
                else {
                    break;
                };
                let general = p
                    .iter()
                    .any(|(k, v)| k.contains('.') || !matches!(v, Bson::Boolean(_)));
                if general && shape.sort.is_some() {
                    break;
                }
                shape.projection = Some(p.clone());
                shape.projection_stage = Some(stage.clone());
                taken = i + 1;
                break;
            }
            _ => break,
        }
        taken = i + 1;
    }
    shape.limit = limit.unwrap_or(0);
    shape.filter = match filters.len() {
        0 => Document::new(),
        1 => filters.remove(0),
        _ => doc! { "$and": filters.into_iter().map(Bson::Document).collect::<Vec<_>>() },
    };
    shape.rest = pipeline[taken..].to_vec();
    // The fields the stages left over read, projected in the query.
    if shape.projection.is_none() && !shape.rest.is_empty() {
        shape.projection = super::deps::projection(&shape.rest);
    }
    shape.optimized_pipeline = shape.rest.is_empty();
    shape.pipeline = true;
}

/// An expression as mongod prints it back: every literal wrapped in `$const`.
pub(super) fn constified(v: &Bson) -> Bson {
    match v {
        // A metadata read prints as written.
        Bson::Document(d) if d.len() == 1 && matches!(d.get("$meta"), Some(Bson::String(_))) => {
            v.clone()
        }
        Bson::Document(d) if d.len() == 1 && d.contains_key("$literal") => {
            Bson::Document(doc! { "$const": d.get("$literal").cloned().unwrap_or(Bson::Null) })
        }
        Bson::String(s) if s.starts_with('$') => v.clone(),
        // mongod rewrites `$sigmoid` when it parses it, and prints the
        // rewrite (8.2.11).
        Bson::Document(d) if d.len() == 1 && d.contains_key("$sigmoid") => constified(
            &secantus_core::expressions::sigmoid_form(d.get("$sigmoid").unwrap_or(&Bson::Null)),
        ),
        Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => Bson::Document(
            d.iter()
                .map(|(op, arg)| {
                    let arg = match arg {
                        Bson::Array(items) => Bson::Array(items.iter().map(constified).collect()),
                        // `$cond`'s object form prints as its argument list.
                        Bson::Document(form) if op == "$cond" => Bson::Array(
                            ["if", "then", "else"]
                                .iter()
                                .map(|k| constified(form.get(*k).unwrap_or(&Bson::Null)))
                                .collect(),
                        ),
                        // A lone argument prints as a list of one.
                        Bson::Document(_) => constified(arg),
                        other => Bson::Array(vec![constified(other)]),
                    };
                    (op.clone(), arg)
                })
                .collect(),
        ),
        Bson::Document(d) => {
            Bson::Document(d.iter().map(|(k, x)| (k.clone(), constified(x))).collect())
        }
        other => Bson::Document(doc! { "$const": other.clone() }),
    }
}

/// The `$sort` mongod parses a `$setWindowFields` into, as this explain
/// carries it through the planner: a `$sort` to every rule that moves or
/// absorbs one, listed with `outputSortKeyMetadata`, which a `$sort` the
/// pipeline wrote does not show (RM-328). Never sent by a client: the
/// command's own checks refuse the name before explain plans anything.
pub(crate) const WINDOW_SORT: &str = "$_secantusWindowSort";

/// The field a `$setWindowFields` partitioned by a computed key adds.
const PARTITION_KEY: &str = "__internal_setWindowFields_partition_key";

/// Whether `v` reads a variable (`$$NOW`, `$$ROOT`, a `let`).
fn reads_variable(v: &Bson) -> bool {
    match v {
        Bson::String(s) => s.starts_with("$$"),
        Bson::Array(items) => items.iter().any(reads_variable),
        Bson::Document(d) if d.contains_key("$literal") => false,
        Bson::Document(d) => d.values().any(reads_variable),
        _ => false,
    }
}

/// The field path a `partitionBy` names: `$a.b` and `$$ROOT.a.b` / `$$CURRENT.a.b`
/// are `a.b`.
fn partition_path(by: &Bson) -> Option<String> {
    let s = by.as_str()?;
    let path = match s.strip_prefix("$$") {
        Some(var) => {
            let (base, rest) = var.split_once('.')?;
            if base != "ROOT" && base != "CURRENT" {
                return None;
            }
            rest
        }
        None => s.strip_prefix('$')?,
    };
    (!path.is_empty()).then(|| path.to_string())
}

/// `$setWindowFields` as mongod 8.2 parses it
/// (`DocumentSourceInternalSetWindowFields::createFromBson`'s caller, measured
/// 8.2.1, 2026-11-08): a `$sort` on its partition and `sortBy`, then the
/// `$_internalSetWindowFields` that runs the windows. A partition that is a
/// field path is sorted on as it is; a constant one is no partition; any
/// other is computed into a field by an `$addFields` first, and dropped by a
/// `$project` after. With neither a partition nor a `sortBy` there is no
/// `$sort`.
pub(crate) fn desugared_window(spec: &Document) -> Vec<Document> {
    let mut internal = Document::new();
    let mut sort = Document::new();
    let mut before = None;
    let mut after = None;
    match spec.get("partitionBy") {
        None => {}
        Some(by)
            if !reads_variable(by) && secantus_core::aggregate::is_constant_expression(by, &[]) => {
        }
        Some(by) => match partition_path(by) {
            Some(path) => {
                sort.insert(path, 1);
                internal.insert("partitionBy", by.clone());
            }
            None => {
                before = Some(doc! { "$addFields": { PARTITION_KEY: by.clone() } });
                sort.insert(PARTITION_KEY, 1);
                internal.insert("partitionBy", format!("${PARTITION_KEY}"));
                after = Some(doc! { "$project": { PARTITION_KEY: false, "_id": true } });
            }
        },
    }
    if let Some(Bson::Document(by)) = spec.get("sortBy") {
        for (k, v) in by {
            sort.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in spec {
        if k != "partitionBy" {
            internal.insert(k.clone(), v.clone());
        }
    }
    let mut out: Vec<Document> = before.into_iter().collect();
    if !sort.is_empty() {
        out.push(doc! { WINDOW_SORT: sort });
    }
    out.push(doc! { "$_internalSetWindowFields": internal });
    out.extend(after);
    out
}

/// `pipeline` with each `$setWindowFields` as the stages mongod parses it
/// into ([`desugared_window`]).
pub(crate) fn desugared(pipeline: &[Bson]) -> Vec<Bson> {
    pipeline
        .iter()
        .flat_map(|stage| match stage.as_document() {
            Some(d) if d.len() == 1 => match d.get("$setWindowFields") {
                Some(Bson::Document(spec)) => desugared_window(spec)
                    .into_iter()
                    .map(Bson::Document)
                    .collect(),
                _ => vec![stage.clone()],
            },
            _ => vec![stage.clone()],
        })
        .collect()
}

/// The window functions that take no window, which mongod lists without one.
const WINDOWLESS: &[&str] = &[
    "$rank",
    "$denseRank",
    "$documentNumber",
    "$shift",
    "$expMovingAvg",
    "$locf",
    "$linearFill",
];

/// One `$_internalSetWindowFields` output as mongod 8.2.1 prints it back
/// (measured 2026-11-08): the function's argument with its literals as
/// `$const` and its own fields in the order the function serialises them,
/// `$count` as the `$sum` of 1 it is, and the window -- whole partition
/// when none was written -- except on the functions that take none.
fn listed_window_output(spec: &Document) -> Document {
    let window = spec.get("window");
    let Some((op, arg)) = spec.iter().find(|(k, _)| k.starts_with('$')) else {
        return spec.clone();
    };
    let field = |d: &Document, k: &str| d.get(k).map(constified);
    let listed = match (op.as_str(), arg) {
        ("$count", _) => doc! { "$sum": { "$const": 1 } },
        ("$rank" | "$denseRank" | "$documentNumber", _) => doc! { op.clone(): arg.clone() },
        ("$shift", Bson::Document(d)) => {
            let mut out = Document::new();
            if let Some(by) = d.get("by") {
                out.insert("by", by.clone());
            }
            if let Some(o) = field(d, "output") {
                out.insert("output", o);
            }
            out.insert("default", d.get("default").cloned().unwrap_or(Bson::Null));
            doc! { "$shift": out }
        }
        ("$expMovingAvg", Bson::Document(d)) => {
            let mut out = Document::new();
            if let Some(n) = d.get("N") {
                // Held as a `long long`.
                out.insert("N", as_i64(n).map_or_else(|| n.clone(), Bson::Int64));
            }
            if let Some(alpha) = d.get("alpha") {
                // Held as a decimal (`Decimal128(double)`, 15 digits).
                let as_decimal = secantus_core::expressions::evaluate(
                    &Document::new(),
                    &Bson::Document(doc! { "$toDecimal": alpha.clone() }),
                    &Document::new(),
                )
                .unwrap_or_else(|_| alpha.clone());
                out.insert("alpha", as_decimal);
            }
            if let Some(i) = field(d, "input") {
                out.insert("input", i);
            }
            doc! { "$expMovingAvg": out }
        }
        ("$topN" | "$bottomN" | "$top" | "$bottom", Bson::Document(d)) => {
            let mut out = Document::new();
            if let Some(n) = field(d, "n") {
                out.insert("n", n);
            }
            if let Some(o) = field(d, "output") {
                out.insert("output", o);
            }
            if let Some(by) = d.get("sortBy") {
                out.insert("sortBy", by.clone());
            }
            doc! { op.clone(): out }
        }
        ("$firstN" | "$lastN" | "$minN" | "$maxN", Bson::Document(d)) => {
            let mut out = Document::new();
            if let Some(n) = field(d, "n") {
                out.insert("n", n);
            }
            if let Some(i) = field(d, "input") {
                out.insert("input", i);
            }
            doc! { op.clone(): out }
        }
        ("$minMaxScaler", Bson::Document(d)) => {
            let mut out = Document::new();
            if let Some(i) = field(d, "input") {
                out.insert("input", i);
            }
            out.insert("min", d.get("min").cloned().unwrap_or(Bson::Int32(0)));
            out.insert("max", d.get("max").cloned().unwrap_or(Bson::Int32(1)));
            doc! { "$minMaxScaler": out }
        }
        ("$percentile" | "$median" | "$derivative" | "$integral", Bson::Document(d)) => {
            let mut out = d.clone();
            if let Some(i) = field(d, "input") {
                out.insert("input", i);
            }
            doc! { op.clone(): out }
        }
        (_, Bson::Array(items)) => {
            doc! { op.clone(): items.iter().map(constified).collect::<Vec<_>>() }
        }
        _ => doc! { op.clone(): constified(arg) },
    };
    let mut out = listed;
    match window {
        Some(w) => {
            out.insert("window", w.clone());
        }
        None if !WINDOWLESS.contains(&op.as_str()) => {
            out.insert("window", doc! { "documents": ["unbounded", "unbounded"] });
        }
        None => {}
    }
    out
}

/// A `$_internalSetWindowFields` spec as mongod prints it back.
fn listed_internal_window(spec: &Document) -> Document {
    let mut out = Document::new();
    for key in ["partitionBy", "sortBy"] {
        if let Some(v) = spec.get(key) {
            out.insert(key, v.clone());
        }
    }
    let mut output = Document::new();
    if let Ok(fields) = spec.get_document("output") {
        for (k, v) in fields {
            let listed = match v {
                Bson::Document(f) => Bson::Document(listed_window_output(f)),
                other => other.clone(),
            };
            output.insert(k.clone(), listed);
        }
    }
    out.insert("output", output);
    out
}

/// A stage the planner left in the pipeline, as `explain` lists it: the
/// stages mongod parses it into, each printed as mongod prints it back
/// (measured 8.2.11).
pub(crate) fn listed_stage(stage: &Document, db: &str) -> Vec<Document> {
    let Some((name, arg)) = stage.iter().next() else {
        return vec![stage.clone()];
    };
    // The stages `$score`, `$rankFusion` and `$scoreFusion` run as, as mongod
    // lists the stages it desugars them into.
    if let Some(listed) = super::hybrid::listed(stage, db) {
        return listed;
    }
    if let Some(plain) = super::hybrid::unhidden(stage) {
        return listed_stage(&plain, db);
    }
    let wide = |v: &Bson| as_i64(v).map_or_else(|| v.clone(), Bson::Int64);
    let one = |d: Document| vec![d];
    let count_of = |field: &str| {
        doc! { field: { "$sum": { "$const": 1 } } }
    };
    match (name.as_str(), arg) {
        ("$unwind", Bson::String(path)) => one(doc! { "$unwind": { "path": path.clone() } }),
        ("$sort", Bson::Document(spec)) => one(doc! { "$sort": { "sortKey": spec.clone() } }),
        (WINDOW_SORT, Bson::Document(spec)) => one(doc! { "$sort": {
            "sortKey": spec.clone(),
            "outputSortKeyMetadata": true,
        } }),
        ("$setWindowFields", Bson::Document(spec)) => desugared_window(spec)
            .iter()
            .flat_map(|stage| listed_stage(stage, db))
            .collect(),
        ("$_internalSetWindowFields", Bson::Document(spec)) => {
            one(doc! { "$_internalSetWindowFields": listed_internal_window(spec) })
        }
        ("$out", Bson::String(coll)) => one(doc! { "$out": { "coll": coll.clone(), "db": db } }),
        ("$limit" | "$skip", n) => one(doc! { name.clone(): wide(n) }),
        ("$sample", Bson::Document(spec)) => {
            let mut out = spec.clone();
            if let Some(size) = spec.get("size") {
                out.insert("size", wide(size));
            }
            one(doc! { "$sample": out })
        }
        ("$match", Bson::Document(filter)) => {
            one(doc! { "$match": super::parsed::parsed_query(filter) })
        }
        ("$addFields" | "$set", Bson::Document(fields)) => {
            one(doc! { name.clone(): nested_fields(fields, &constified) })
        }
        ("$project", Bson::Document(spec)) => match listed_projection(spec) {
            Some(p) => one(doc! { "$project": p }),
            None => one(stage.clone()),
        },
        ("$unset", spec) => {
            let names: Vec<String> = match spec {
                Bson::String(f) => vec![f.clone()],
                Bson::Array(items) => items
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect(),
                _ => return one(stage.clone()),
            };
            let mut out = Document::new();
            for n in names {
                out.insert(n, false);
            }
            out.insert("_id", true);
            one(doc! { "$project": out })
        }
        // A collection-less source, as the stages mongod turns it into.
        ("$documents", docs) => vec![
            doc! { "$queue": [{}] },
            doc! { "$project": { "_id": true, "_tempDocumentsField": constified(docs) } },
            doc! { "$unwind": { "path": "$_tempDocumentsField" } },
            doc! { "$replaceRoot": { "newRoot": "$_tempDocumentsField" } },
        ],
        ("$replaceWith", root) => one(doc! { "$replaceRoot": { "newRoot": constified(root) } }),
        ("$replaceRoot", Bson::Document(spec)) => match spec.get("newRoot") {
            Some(root) => one(doc! { "$replaceRoot": { "newRoot": constified(root) } }),
            None => one(stage.clone()),
        },
        ("$group", Bson::Document(spec)) => match listed_group(spec) {
            Some(g) => one(doc! { "$group": g }),
            None => one(stage.clone()),
        },
        ("$count", Bson::String(field)) => {
            let mut group = doc! { "_id": { "$const": Bson::Null } };
            group.extend(count_of(field));
            group.insert("$willBeMerged", false);
            vec![
                doc! { "$group": group },
                doc! { "$project": { field.clone(): true, "_id": false } },
            ]
        }
        ("$sortByCount", by) => {
            let mut group = doc! { "_id": constified(by) };
            group.extend(count_of("count"));
            group.insert("$willBeMerged", false);
            vec![
                doc! { "$group": group },
                doc! { "$sort": { "sortKey": { "count": -1 } } },
            ]
        }
        ("$bucket", Bson::Document(spec)) => match listed_bucket(spec) {
            Some(group) => vec![
                doc! { "$group": group },
                doc! { "$sort": { "sortKey": { "_id": 1 } } },
            ],
            None => one(stage.clone()),
        },
        ("$facet", Bson::Document(facets)) => {
            let mut out = Document::new();
            for (field, sub) in facets {
                let Bson::Array(stages) = sub else {
                    return one(stage.clone());
                };
                let mut listed = vec![Bson::Document(doc! { "$internalFacetTeeConsumer": {} })];
                for s in stages.iter().filter_map(Bson::as_document) {
                    listed.extend(listed_stage(s, db).into_iter().map(Bson::Document));
                }
                out.insert(field.clone(), listed);
            }
            one(doc! { "$facet": out })
        }
        ("$lookup", Bson::Document(spec))
            if spec.contains_key("pipeline") && !spec.contains_key("let") =>
        {
            let mut out = Document::new();
            for (k, v) in spec {
                if k == "pipeline" {
                    out.insert("let", Document::new());
                }
                out.insert(k.clone(), v.clone());
            }
            one(doc! { "$lookup": out })
        }
        ("$merge", into) => {
            let spec = match into {
                Bson::Document(d) => d.clone(),
                other => doc! { "into": other.clone() },
            };
            let target = match spec.get("into") {
                Some(Bson::String(coll)) => doc! { "db": db, "coll": coll.clone() },
                Some(Bson::Document(d)) => d.clone(),
                _ => Document::new(),
            };
            let pick = |field: &str, default: Bson| spec.get(field).cloned().unwrap_or(default);
            one(doc! { "$merge": {
                "into": target,
                "on": pick("on", Bson::String("_id".into())),
                "whenMatched": pick("whenMatched", Bson::String("merge".into())),
                "whenNotMatched": pick("whenNotMatched", Bson::String("insert".into())),
                "allowMergeOnNullishValues": true,
            } })
        }
        _ => one(stage.clone()),
    }
}

/// Fields whose dotted names mongod prints as nested documents, each value
/// passed through `value`.
fn nested_fields(fields: &Document, value: &dyn Fn(&Bson) -> Bson) -> Document {
    let mut out = Document::new();
    for (k, v) in fields {
        let mut parts: Vec<&str> = k.split('.').collect();
        let last = parts.pop().unwrap_or_default();
        let mut at = &mut out;
        for p in parts {
            if !matches!(at.get(p), Some(Bson::Document(_))) {
                at.insert(p, Document::new());
            }
            at = match at.get_mut(p) {
                Some(Bson::Document(d)) => d,
                _ => return fields.clone(),
            };
        }
        at.insert(last, value(v));
    }
    out
}

/// A `$project` as mongod prints it back: an inclusion leads with `_id:
/// true` (or ends with `_id: false`), plain fields before nested ones and
/// expressions; an exclusion lists its fields, then `_id: true` (or leads
/// with `_id: false`). `None`: a shape this does not print.
fn listed_projection(spec: &Document) -> Option<Document> {
    let on = |v: &Bson| match v {
        Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => {
            Some(crate::util::truthy(v))
        }
        _ => None,
    };
    let id = spec.get("_id");
    let id_on = id.map(on);
    let rest: Vec<(&String, &Bson)> = spec.iter().filter(|(k, _)| *k != "_id").collect();
    let exclusion = !rest.is_empty() && rest.iter().all(|(_, v)| on(v) == Some(false));
    let nested = |plain: &[(&String, &Bson)], value: &dyn Fn(&Bson) -> Bson| {
        let d: Document = plain
            .iter()
            .map(|(k, v)| ((*k).clone(), (*v).clone()))
            .collect();
        nested_fields(&d, value)
    };
    let mut out = Document::new();
    if exclusion || (rest.is_empty() && id_on == Some(Some(false))) {
        if id_on == Some(Some(false)) {
            out.insert("_id", false);
        }
        out.extend(nested(&rest, &|_| Bson::Boolean(false)));
        if id_on != Some(Some(false)) {
            out.insert("_id", true);
        }
        return Some(out);
    }
    if rest.iter().any(|(_, v)| on(v) == Some(false)) {
        return None;
    }
    let plain: Vec<(&String, &Bson)> = rest
        .iter()
        .filter(|(k, v)| on(v) == Some(true) && !k.contains('.'))
        .copied()
        .collect();
    // A computed `_id` is printed among the other expressions.
    let others: Vec<(&String, &Bson)> = spec
        .iter()
        .filter(|(k, v)| {
            if *k == "_id" {
                on(v).is_none()
            } else {
                !(on(v) == Some(true) && !k.contains('.'))
            }
        })
        .collect();
    let value = |v: &Bson| match on(v) {
        Some(b) => Bson::Boolean(b),
        None => match v {
            Bson::Document(d) if !d.keys().any(|k| k.starts_with('$')) => {
                Bson::Document(listed_projection(d).map_or_else(
                    || d.clone(),
                    |mut p| {
                        p.remove("_id");
                        p
                    },
                ))
            }
            other => constified(other),
        },
    };
    let keep_id = id_on != Some(Some(false));
    let computed_id = id.is_some_and(|v| on(v).is_none());
    if keep_id && !computed_id {
        out.insert("_id", true);
    }
    out.extend(nested(&plain, &value));
    out.extend(nested(&others, &value));
    if !keep_id {
        out.insert("_id", false);
    }
    Some(out)
}

/// A `$group` as mongod prints it back: literals as `$const`, `$count` as a
/// sum of ones, and `$willBeMerged`.
fn listed_group(spec: &Document) -> Option<Document> {
    let mut out = Document::new();
    for (k, v) in spec {
        if k == "_id" {
            out.insert("_id", constified(v));
            continue;
        }
        let acc = v.as_document()?;
        let (op, arg) = acc.iter().next()?;
        let listed = if op == "$count" {
            doc! { "$sum": { "$const": 1 } }
        } else {
            doc! { op.clone(): constified(arg) }
        };
        out.insert(k.clone(), listed);
    }
    out.insert("$willBeMerged", false);
    Some(out)
}

/// A `$bucket` as the `$group` mongod turns it into: a `$switch` over the
/// boundaries.
fn listed_bucket(spec: &Document) -> Option<Document> {
    let by = constified(spec.get("groupBy")?);
    let bounds = spec.get_array("boundaries").ok()?;
    let branches: Vec<Bson> = bounds
        .windows(2)
        .map(|pair| {
            Bson::Document(doc! {
                "case": { "$and": [
                    { "$gte": [by.clone(), { "$const": pair[0].clone() }] },
                    { "$lt": [by.clone(), { "$const": pair[1].clone() }] },
                ] },
                "then": { "$const": pair[0].clone() },
            })
        })
        .collect();
    let mut switch = doc! { "branches": branches };
    if let Some(d) = spec.get("default") {
        switch.insert("default", doc! { "$const": d.clone() });
    }
    let mut group = doc! { "_id": { "$switch": switch } };
    match spec.get_document("output") {
        Ok(output) => {
            for (k, v) in output {
                let acc = v.as_document()?;
                let (op, arg) = acc.iter().next()?;
                group.insert(k.clone(), doc! { op.clone(): constified(arg) });
            }
        }
        Err(_) => {
            group.insert("count", doc! { "$sum": { "$const": 1 } });
        }
    }
    group.insert("$willBeMerged", false);
    Some(group)
}

/// Whether a stage reads something other than the collection's documents,
/// so that no query is planned in front of it.
pub(crate) fn own_source(stage: &Document) -> bool {
    matches!(
        stage.keys().next().map(String::as_str),
        Some("$collStats" | "$indexStats" | "$documents" | "$currentOp" | "$listSessions")
    )
}

/// The pipeline a `find` on a view runs after the view's own.
pub(crate) fn find_as_pipeline(s: &Shape) -> Vec<Document> {
    let mut out = Vec::new();
    if !s.filter.is_empty() {
        out.push(doc! { "$match": s.filter.clone() });
    }
    if let Some(sort) = s.sort.as_ref().filter(|x| !x.is_empty()) {
        out.push(doc! { "$sort": sort.clone() });
    }
    if s.skip > 0 {
        out.push(doc! { "$skip": Bson::Int64(s.skip) });
    }
    if s.limit > 0 {
        out.push(doc! { "$limit": Bson::Int64(s.limit) });
    }
    if let Some(p) = s.projection.as_ref().filter(|x| !x.is_empty()) {
        out.push(doc! { "$project": p.clone() });
    }
    out
}

/// The command as `explain` hands it back. `count` and `findAndModify` come
/// back parsed, in their own field order and without `$db`; the rest come
/// back as sent, with `$db` added.
pub(crate) fn echo(inner: &Document, db: &str) -> Document {
    let name = inner.keys().next().cloned().unwrap_or_default();
    let wide = |v: &Bson| as_i64(v).map_or_else(|| v.clone(), Bson::Int64);
    let late = ["maxTimeMS", "comment"];
    let mut out = Document::new();
    let ordered: &[&str] = match name.to_ascii_lowercase().as_str() {
        "count" => &["query", "limit", "skip", "hint", "collation"],
        "findandmodify" => &[
            "query",
            "fields",
            "sort",
            "hint",
            "collation",
            "arrayFilters",
            "remove",
            "update",
            "upsert",
            "new",
            "bypassDocumentValidation",
            "let",
        ],
        _ => {
            let mut plain = inner.clone();
            plain.insert("$db", db);
            return plain;
        }
    };
    let count = ordered[1] == "limit";
    if let Some(target) = inner.get(&name) {
        out.insert(
            if count { "count" } else { "findAndModify" },
            target.clone(),
        );
    }
    for field in ordered {
        let value = match (inner.get(*field), *field) {
            (None | Some(Bson::Null), "query") => Bson::Document(Document::new()),
            (None | Some(Bson::Null), _) => continue,
            (Some(v), "limit") => Bson::Int64(as_i64(v).unwrap_or(0).abs()),
            (Some(v), "skip") => wide(v),
            (Some(Bson::String(s)), "hint") => Bson::Document(doc! { "$hint": s.clone() }),
            (Some(v), "remove" | "new" | "upsert" | "bypassDocumentValidation") => {
                Bson::Boolean(crate::util::truthy(v))
            }
            (Some(v), _) => v.clone(),
        };
        out.insert(*field, value);
    }
    for (k, v) in inner.iter().skip(1) {
        if !ordered.contains(&k.as_str()) && !late.contains(&k.as_str()) && !k.starts_with('$') {
            out.insert(k.clone(), v.clone());
        }
    }
    for field in late {
        if let Some(v) = inner.get(field) {
            out.insert(
                field,
                if field == "maxTimeMS" {
                    wide(v)
                } else {
                    v.clone()
                },
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{absorb, find, Shape};
    use bson::{doc, Document};

    fn absorbed(pipeline: Vec<Document>) -> Shape {
        let mut shape = find("c", &doc! { "find": "c" }).expect("a find shape");
        absorb(&mut shape, &pipeline);
        shape
    }

    /// mongod 8.2.11 reads `[$project, $limit, $match]` as `[$limit,
    /// $match, $project]`: the query takes the `$limit`, and projects what
    /// the two stages it leaves read (RM-308).
    #[test]
    fn a_match_goes_ahead_of_a_projection_a_limit_moved_past() {
        let s = absorbed(vec![
            doc! { "$project": { "x": 1 } },
            doc! { "$limit": 3 },
            doc! { "$match": { "x": 1 } },
        ]);
        assert_eq!(s.limit, 3);
        assert_eq!(s.projection, Some(doc! { "_id": 1, "x": 1 }));
        assert_eq!(
            s.rest,
            vec![
                doc! { "$match": { "x": 1 } },
                doc! { "$project": { "x": 1 } }
            ]
        );
    }

    /// The same through an `$addFields`, which keeps the number of
    /// documents too (RM-371).
    #[test]
    fn a_limit_goes_ahead_of_a_reshape() {
        let s = absorbed(vec![
            doc! { "$addFields": { "w": 1 } },
            doc! { "$limit": 3 },
            doc! { "$match": { "x": 1 } },
        ]);
        assert_eq!(s.limit, 3);
        assert_eq!(
            s.rest,
            vec![
                doc! { "$match": { "x": 1 } },
                doc! { "$addFields": { "w": 1 } }
            ]
        );
    }

    /// A `$limit` moves ahead only as far as the query: not behind an
    /// `$unwind`, nor behind a `$match` that a `$limit` already keeps out
    /// of the query (8.2.11).
    #[test]
    fn a_limit_stays_where_the_query_cannot_reach_it() {
        let unwound = absorbed(vec![
            doc! { "$unwind": "$t" },
            doc! { "$project": { "x": 1 } },
            doc! { "$limit": 2 },
        ]);
        assert_eq!(unwound.limit, 0);
        assert_eq!(
            unwound.rest[1..],
            [doc! { "$project": { "x": 1 } }, doc! { "$limit": 2 }]
        );
        let paged = absorbed(vec![
            doc! { "$limit": 3 },
            doc! { "$match": { "x": 1 } },
            doc! { "$project": { "x": 1 } },
            doc! { "$limit": 2 },
        ]);
        assert_eq!(paged.limit, 3);
        assert_eq!(
            paged.rest,
            vec![
                doc! { "$match": { "x": 1 } },
                doc! { "$project": { "x": 1 } },
                doc! { "$limit": 2 },
            ]
        );
    }
}
