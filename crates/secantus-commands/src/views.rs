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

/// A hint on a view names an index of the collection beneath it: mongod
/// plans the view's pipeline over that collection and refuses a hint that
/// names none of its indexes (2, measured 8.2.11) rather than ignore it.
pub(crate) fn check_hint(
    storage: &dyn crate::storage::Storage,
    db: &str,
    view: &str,
    hint: Option<&Bson>,
    filter: &bson::Document,
) -> Result<(), CommandError> {
    if hint.is_none() {
        return Ok(());
    }
    // The view's pipeline, then the command's query: what mongod plans.
    let tail = if filter.is_empty() {
        Vec::new()
    } else {
        vec![Bson::Document(bson::doc! { "$match": filter.clone() })]
    };
    let (base, pipeline) = crate::aggregate::resolve_view(storage, db, view, tail)?;
    let q = crate::explain::pipeline_canonical(&base, &pipeline);
    let simple = bson::doc! {"locale": "simple"};
    let query = crate::explain::CanonicalQuery {
        ns: format!("{db}.{base}"),
        filter: &q.filter,
        sort: q.sort.as_ref(),
        projection: q.projection.as_ref(),
        skip: Some(q.skip),
        limit: Some(q.limit),
        collation: Some(&simple),
        batch_size: None,
    };
    crate::util::validate_hint(storage, db, &base, hint, &query)
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
    // `system.views` holds the definitions and is not one of them: a write
    // to it has a refusal of its own, which the handler gives.
    let view = |coll: &str| coll != "system.views" && is_view(storage.as_ref(), db, coll);
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
            // An insert's documents usually arrive as a document sequence,
            // which the server hands over raw instead of merging into `doc`.
            let count = match (&ctx.raw_insert_documents, doc.get_array(statements)) {
                (Some(raw), _) if name == "insert" => raw.len(),
                (_, Ok(inline)) => inline.len(),
                _ => 1,
            }
            .max(1);
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
            if !is_view(storage.as_ref(), &target_db, &target_coll) {
                return None;
            }
            // A view of another collation is refused for that first (167).
            if let Err(e) =
                crate::aggregate::involved_view_collation(doc, ctx, Some(coll), pipeline)
            {
                return Some(e.into_reply());
            }
            Some(executor(not_a_collection(&format!(
                "{target_db}.{target_coll}"
            ))))
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
    // The views in the chain: every entry but the collection it ends on.
    // mongod refuses one longer than its limit (measured 8.2.11, 2026-10-14).
    if chain.len() - 1 > MAX_VIEW_DEPTH {
        return Some(CommandError::new(
            165,
            "ViewDepthLimitExceeded",
            format!("View depth limit exceeded; maximum depth is {MAX_VIEW_DEPTH}"),
        ));
    }
    None
}

/// mongod's limit on a chain of views.
pub(crate) const MAX_VIEW_DEPTH: usize = 20;

/// Every namespace a pipeline reads or writes besides its own, in pipeline
/// order: `$lookup` / `$graphLookup` / `$unionWith` sources (a `$lookup`'s
/// `from` only as a string -- the `{db, coll}` form is refused elsewhere),
/// `$out` / `$merge` targets, and those of every sub-pipeline (`$lookup`,
/// `$unionWith`, `$facet`).
fn involved_namespaces(pipeline: &[Bson], db: &str, out: &mut Vec<(String, String)>) {
    for stage in pipeline.iter().filter_map(Bson::as_document) {
        let Some((name, spec)) = stage.iter().next() else {
            continue;
        };
        match (name.as_str(), spec) {
            ("$lookup", Bson::Document(d)) => {
                if let Ok(from) = d.get_str("from") {
                    out.push((db.to_string(), from.to_string()));
                }
                if let Ok(sub) = d.get_array("pipeline") {
                    involved_namespaces(sub, db, out);
                }
            }
            ("$graphLookup", Bson::Document(d)) => {
                if let Ok(from) = d.get_str("from") {
                    out.push((db.to_string(), from.to_string()));
                }
            }
            ("$unionWith", Bson::String(coll)) => out.push((db.to_string(), coll.clone())),
            ("$unionWith", Bson::Document(d)) => {
                if let Ok(coll) = d.get_str("coll") {
                    out.push((db.to_string(), coll.to_string()));
                }
                if let Ok(sub) = d.get_array("pipeline") {
                    involved_namespaces(sub, db, out);
                }
            }
            ("$facet", Bson::Document(d)) => {
                for sub in d.values() {
                    if let Bson::Array(sub) = sub {
                        involved_namespaces(sub, db, out);
                    }
                }
            }
            ("$out" | "$merge", _) => out.extend(write_target(stage, db)),
            _ => {}
        }
    }
}

/// A view an aggregate reads or writes besides its own namespace must have
/// the collation the aggregate runs under -- the one it names, else its own
/// namespace's default, else none -- or mongod refuses the command before it
/// runs (167, measured 8.2.11 2026-10-16: on an empty or missing collection,
/// under `$facet` and in nested pipelines, and for `$out` / `$merge` into a
/// view). The namespace follows the message with no separator, as mongod
/// prints it.
pub(crate) fn involved_view_collation_problem(
    storage: &dyn crate::storage::Storage,
    db: &str,
    pipeline: &[Bson],
    collation: Option<&secantus_core::collation::Collation>,
) -> Option<CommandError> {
    let mut involved = Vec::new();
    involved_namespaces(pipeline, db, &mut involved);
    for (db, coll) in involved {
        let Ok(options) = storage.get_collection_options(&db, &coll) else {
            continue;
        };
        if !options.contains_key("viewOn") {
            continue;
        }
        let own = options
            .get_document("collation")
            .ok()
            .and_then(secantus_core::collation::parse);
        let same = match (collation, &own) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same(b),
            _ => false,
        };
        if !same {
            return Some(CommandError::new(
                167,
                "OptionNotSupportedOnView",
                format!("Cannot override a view's default collation{db}.{coll}"),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn involved_namespaces_walks_every_sub_pipeline() {
        let pipeline = vec![
            Bson::Document(doc! {"$lookup": {"from": "a", "pipeline": [
                {"$unionWith": {"coll": "b", "pipeline": [{"$graphLookup": {"from": "c"}}]}}
            ]}}),
            Bson::Document(doc! {"$facet": {"x": [{"$unionWith": "d"}], "y": []}}),
            Bson::Document(doc! {"$lookup": {"from": {"db": "db", "coll": "skipped"}}}),
            Bson::Document(doc! {"$merge": {"into": {"db": "other", "coll": "e"}}}),
        ];
        let mut out = Vec::new();
        involved_namespaces(&pipeline, "db", &mut out);
        let names: Vec<String> = out.iter().map(|(d, c)| format!("{d}.{c}")).collect();
        assert_eq!(names, ["db.a", "db.b", "db.c", "db.d", "other.e"]);
    }
}
