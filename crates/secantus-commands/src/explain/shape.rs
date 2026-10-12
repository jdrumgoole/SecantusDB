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
        update: Option<Document>,
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
    s.projection = inner.get_document("projection").ok().cloned();
    s.skip = int_arg(inner, "skip");
    s.limit = int_arg(inner, "limit");
    if inner.contains_key("min") || inner.contains_key("max") {
        s.min_max = Some(if inner.contains_key("min") { "min" } else { "max" });
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
fn statement<'a>(inner: &'a Document, cmd: &str, field: &str) -> Result<&'a Document, CommandError> {
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
fn check_update(u: &Bson) -> Result<Option<Document>, CommandError> {
    match u {
        Bson::Document(d) => {
            if let Some((code, msg)) = secantus_core::update::update_spec_error(d) {
                return Err(err(code, &crate::util::error_code_name(code), msg));
            }
            Ok(Some(d.clone()))
        }
        _ => Ok(None),
    }
}

pub(crate) fn update(coll: &str, inner: &Document) -> Result<Shape, CommandError> {
    let st = statement(inner, "update", "updates")?;
    let Some(u) = st.get("u") else {
        return Err(missing("update.updates.u"));
    };
    let kind = Kind::Update {
        multi: flag(st, "multi"),
        upsert: flag(st, "upsert"),
        update: check_update(u)?,
        returns: false,
    };
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
        (false, Some(u)) => Kind::Update {
            multi: false,
            upsert: flag(inner, "upsert"),
            update: check_update(u)?,
            returns: true,
        },
    };
    let mut s = Shape::new(coll, kind, inner);
    s.filter = filter;
    s.sort = inner.get_document("sort").ok().cloned();
    s.projection = inner.get_document("fields").ok().cloned();
    Ok(s)
}

/// A `$project` that only includes or only excludes plain fields, in the
/// form mongod prints: booleans, with `_id` stated.
fn simple_projection(spec: &Document) -> Option<Document> {
    let mut include: Option<bool> = None;
    let mut id: Option<bool> = None;
    let mut out = Document::new();
    for (field, value) in spec {
        let on = match value {
            Bson::Boolean(b) => *b,
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => crate::util::truthy(value),
            _ => return None,
        };
        if field.contains('.') || field.starts_with('$') {
            return None;
        }
        if field == "_id" {
            id = Some(on);
            continue;
        }
        if include.is_some_and(|i| i != on) {
            return None;
        }
        include = Some(on);
        out.insert(field.clone(), on);
    }
    let include = include?;
    let id = id.unwrap_or(true);
    if include && id {
        // An implied `_id` leads; a stated one keeps its place at the end.
        let mut with_id = doc! { "_id": true };
        with_id.extend(out);
        return Some(with_id);
    }
    out.insert("_id", id);
    Some(out)
}

/// The leading stages of a pipeline the query planner absorbs, folded into
/// `shape`; the stages it leaves are kept in `shape.rest`.
pub(crate) fn absorb(shape: &mut Shape, pipeline: &[Document]) {
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
            ("$sort", Bson::Document(s)) if !paged && shape.projection.is_none() => {
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
            ("$project", Bson::Document(p)) if i + 1 == pipeline.len() => {
                let Some(simple) = simple_projection(p) else {
                    break;
                };
                shape.projection = Some(simple);
            }
            ("$unset", Bson::String(field)) if i + 1 == pipeline.len() && !field.contains('.') => {
                shape.projection = Some(doc! { field.clone(): false, "_id": true });
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
    shape.optimized_pipeline = shape.rest.is_empty();
    shape.pipeline = true;
}

/// A stage the planner left in the pipeline, as `explain` lists it.
pub(crate) fn listed_stage(stage: &Document, db: &str) -> Document {
    let Some((name, arg)) = stage.iter().next() else {
        return stage.clone();
    };
    match (name.as_str(), arg) {
        ("$unwind", Bson::String(path)) => doc! { "$unwind": { "path": path.clone() } },
        ("$sort", Bson::Document(spec)) => doc! { "$sort": { "sortKey": spec.clone() } },
        ("$out", Bson::String(coll)) => doc! { "$out": { "coll": coll.clone(), "db": db } },
        _ => stage.clone(),
    }
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
        out.push(doc! { "$skip": s.skip });
    }
    if s.limit > 0 {
        out.push(doc! { "$limit": s.limit });
    }
    if let Some(p) = s.projection.as_ref().filter(|x| !x.is_empty()) {
        out.push(doc! { "$project": p.clone() });
    }
    out
}
