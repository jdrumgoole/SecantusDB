//! What mongod refuses to do to a view.
//!
//! A view is stored as a collection whose options carry `viewOn` and
//! `viewPipeline`; `find`, `count`, `distinct` and `aggregate` read through it.
//! Everything that would write to it, index it or measure it as a collection
//! is refused -- and until 2026-10-09 was not: an `insert` into a view was
//! acknowledged and stored rows under the view's own name. Every reply here is
//! what mongod 8.2.11 answered for the same command.

use crate::{CommandContext, CommandError};
use bson::{doc, Bson, Document};

/// Whether `db.coll` is a view.
pub(crate) fn is_view(storage: &dyn crate::storage::Storage, db: &str, coll: &str) -> bool {
    storage
        .get_collection_options(db, coll)
        .map(|o| o.contains_key("viewOn"))
        .unwrap_or(false)
}

fn not_a_collection(ns: &str) -> String {
    format!("Namespace {ns} is a view, not a collection")
}

fn unsupported(msg: String) -> Document {
    CommandError::new(166, "CommandNotSupportedOnView", msg).into_reply()
}

/// `db.coll` split at the first dot.
fn split_ns(ns: &str) -> Option<(&str, &str)> {
    ns.split_once('.')
}

/// The namespace an `$out` / `$merge` stage writes to, when it names one in
/// a form this reads (`"coll"`, `{db, coll}`, and `$merge`'s `into` of either).
fn write_target(stage: &Document, db: &str) -> Option<(String, String)> {
    let (name, spec) = stage.iter().next()?;
    let spec = match (name.as_str(), spec) {
        ("$out", spec) => spec,
        ("$merge", Bson::Document(d)) => d.get("into")?,
        ("$merge", spec) => spec,
        _ => return None,
    };
    match spec {
        Bson::String(coll) => Some((db.to_string(), coll.clone())),
        Bson::Document(d) => Some((
            d.get_str("db").unwrap_or(db).to_string(),
            d.get_str("coll").ok()?.to_string(),
        )),
        _ => None,
    }
}

/// The reply to a command mongod refuses on a view, or `None` to run it.
pub(crate) fn refusal(name: &str, doc: &Document, ctx: &CommandContext) -> Option<Document> {
    let storage = ctx.storage.as_ref()?;
    let db = ctx.db_name.as_str();
    let view = |coll: &str| is_view(storage.as_ref(), db, coll);
    match name {
        // A write reports one error per statement, or only the first when the
        // batch is ordered (the default), and writes nothing.
        "insert" | "update" | "delete" => {
            let coll = doc.get_str(name).ok()?;
            if !view(coll) {
                return None;
            }
            let statements = match name {
                "insert" => "documents",
                "update" => "updates",
                _ => "deletes",
            };
            let count = doc.get_array(statements).map(Vec::len).unwrap_or(1).max(1);
            let ordered = !matches!(doc.get("ordered"), Some(Bson::Boolean(false)));
            let reported = if ordered { 1 } else { count };
            let msg = not_a_collection(&format!("{db}.{coll}"));
            let errors: Vec<Document> = (0..reported)
                .map(|i| doc! { "index": i as i32, "code": 166_i32, "errmsg": msg.clone() })
                .collect();
            let mut reply = doc! { "n": 0_i32 };
            if name == "update" {
                reply.insert("nModified", 0_i32);
            }
            reply.insert("writeErrors", errors);
            reply.insert("ok", 1.0);
            Some(reply)
        }
        "findAndModify" | "findandmodify" | "createIndexes" | "listIndexes" | "dropIndexes"
        | "deleteIndexes" | "collStats" | "collstats" => {
            let coll = doc.get_str(name).ok()?;
            view(coll).then(|| unsupported(not_a_collection(&format!("{db}.{coll}"))))
        }
        "validate" => {
            let coll = doc.get_str(name).ok()?;
            view(coll).then(|| unsupported("Cannot validate a view".to_string()))
        }
        "renameCollection" => {
            let source = doc.get_str(name).ok()?;
            let target = doc.get_str("to").ok()?;
            let (source_db, source_coll) = split_ns(source)?;
            if is_view(storage.as_ref(), source_db, source_coll) {
                return Some(unsupported(format!("cannot rename view: {source}")));
            }
            let (target_db, target_coll) = split_ns(target)?;
            is_view(storage.as_ref(), target_db, target_coll).then(|| {
                CommandError::new(
                    48,
                    "NamespaceExists",
                    format!("a view already exists with that name: {target}"),
                )
                .into_reply()
            })
        }
        "aggregate" => {
            let coll = doc.get_str(name).ok()?;
            let pipeline = doc.get_array("pipeline").ok()?;
            let ns = format!("{db}.{coll}");
            let executor = |inner: String| {
                unsupported(format!(
                    "Executor error during aggregate command on namespace: {ns} :: caused by :: \
                     {inner}"
                ))
            };
            if view(coll) {
                let first = pipeline
                    .first()
                    .and_then(Bson::as_document)
                    .and_then(|s| s.keys().next());
                match first.map(String::as_str) {
                    // A standalone answers that change streams need a replica
                    // set before it looks at the namespace.
                    Some("$changeStream") if ctx.replica_set_name.is_some() => {
                        return Some(unsupported(not_a_collection(&ns)));
                    }
                    Some("$collStats") => return Some(executor(not_a_collection(&ns))),
                    _ => {}
                }
            }
            let (target_db, target_coll) =
                write_target(pipeline.last().and_then(Bson::as_document)?, db)?;
            is_view(storage.as_ref(), &target_db, &target_coll)
                .then(|| executor(not_a_collection(&format!("{target_db}.{target_coll}"))))
        }
        _ => None,
    }
}

/// Why mongod would refuse `pipeline` over `view_on` as the definition of
/// view `db.view`, in the order it checks: an empty `viewOn`, a stage a view
/// may not hold, an unknown stage, a cycle. `None` when it is acceptable.
pub(crate) fn definition_problem(
    storage: &dyn crate::storage::Storage,
    db: &str,
    view: &str,
    view_on: &str,
    pipeline: Option<&[Bson]>,
) -> Option<CommandError> {
    let ns = format!("{db}.{view}");
    if view_on.is_empty() {
        return Some(CommandError::new(2, "BadValue", "'viewOn' cannot be empty"));
    }
    let invalid = |why: String| {
        CommandError::new(
            167,
            "OptionNotSupportedOnView",
            format!("Invalid pipeline for view {ns} :: caused by :: {why}"),
        )
    };
    for (i, stage) in pipeline.unwrap_or_default().iter().enumerate() {
        let name = stage
            .as_document()
            .and_then(|s| s.keys().next())
            .map(String::as_str);
        match name {
            Some(stage @ ("$out" | "$merge")) => {
                return Some(invalid(format!(
                    "The aggregation stage {stage} in location {i} of the pipeline cannot be \
                     used in the view definition of {ns} because it writes to disk"
                )))
            }
            Some("$changeStream") => {
                return Some(invalid(
                    "$changeStream cannot be used in a view definition".to_string(),
                ))
            }
            _ => {}
        }
    }
    if let Some(pipeline) = pipeline {
        if let Err(e) = crate::aggregate::validate_stage_names(pipeline) {
            return Some(e);
        }
    }
    // A view defined, directly or through other views, on itself. mongod
    // prints the view and then the path that returns to it.
    let mut chain = vec![view.to_string()];
    let mut current = view_on.to_string();
    while chain.len() <= 64 {
        chain.push(current.clone());
        if current == view {
            let path: Vec<String> = chain.iter().map(|c| format!("{db}.{c}")).collect();
            return Some(CommandError::new(
                5,
                "GraphContainsCycle",
                format!("View cycle detected: {ns} => {}", path.join(" => ")),
            ));
        }
        match storage
            .get_collection_options(db, &current)
            .ok()
            .and_then(|o| o.get_str("viewOn").ok().map(String::from))
        {
            Some(next) => current = next,
            None => break,
        }
    }
    None
}
