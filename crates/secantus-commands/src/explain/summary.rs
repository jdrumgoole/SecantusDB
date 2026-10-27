//! The `planSummary` of a `Slow query` log line (RM-023).
//!
//! mongod's line names the leaf stages of the plan it ran, an index scan
//! with its key pattern (`Explain::getPlanSummary`): `COLLSCAN`,
//! `IXSCAN { x: 1 }`, `IXSCAN { x: 1 }, IXSCAN { y: 1 }` for an `$or`,
//! `EXPRESS_IXSCAN { _id: 1 }`, `COUNT_SCAN { y: 1 }`, `DISTINCT_SCAN { y: 1 }`
//! and `EOF` (measured 8.2.11, 2026-10-16). The plan is the one `explain`
//! derives, but made from the index catalog alone: no document is read, so
//! logging a slow query never reads the collection again. Where several
//! indexes could serve the query, the planner's fixed preference stands in
//! for mongod's trial of them.

use bson::{doc, Bson, Document};

use super::shape::{self, Shape};
use super::{bounds, catalog, parsed, plan, resolve_hint, Kind, Node, Query};
use crate::CommandContext;

/// The shape of a `find`, `count`, `distinct` or `aggregate`; `None` for
/// any other command.
fn shape_of(request: &Document) -> Option<Shape> {
    let name = request.keys().next()?.as_str();
    let coll = request.get_str(name).ok()?;
    match name {
        "find" => shape::find(coll, request).ok(),
        "count" => shape::count(coll, request).ok(),
        "distinct" => shape::distinct(coll, request).ok(),
        "aggregate" => {
            let mut s = shape::find(coll, &doc! { "find": coll }).ok()?;
            let stages: Vec<Document> = request
                .get_array("pipeline")
                .ok()?
                .iter()
                .map_while(|s| s.as_document().cloned())
                .collect();
            // A pipeline that starts with its own source reads no collection.
            if stages.first().is_some_and(shape::own_source) {
                return None;
            }
            shape::absorb(&mut s, &stages);
            Some(s)
        }
        _ => None,
    }
}

/// Whether `request` is an `aggregate` whose pipeline is its own source
/// (`$documents`, ...): it reads no collection, and examines nothing.
pub(crate) fn reads_no_collection(request: &Document) -> bool {
    request.keys().next().map(String::as_str) == Some("aggregate")
        && request
            .get_array("pipeline")
            .ok()
            .and_then(|p| p.first())
            .and_then(Bson::as_document)
            .is_some_and(shape::own_source)
}

/// The leaf stages of `plan`, depth first, as a `planSummary` names them.
pub(crate) fn leaves_of(plan: &Node, out: &mut Vec<String>) {
    leaves(&plan.to_doc(None), out);
}

/// The leaf stages of an explain plan document, depth first.
fn leaves(stage: &Document, out: &mut Vec<String>) {
    let mut inner = false;
    if let Ok(input) = stage.get_document("inputStage") {
        leaves(input, out);
        inner = true;
    }
    if let Ok(inputs) = stage.get_array("inputStages") {
        for input in inputs.iter().filter_map(Bson::as_document) {
            leaves(input, out);
        }
        inner = true;
    }
    if inner {
        return;
    }
    let Ok(name) = stage.get_str("stage") else {
        return;
    };
    // An express stage gives its key pattern already printed.
    out.push(match stage.get("keyPattern") {
        Some(Bson::Document(key)) => format!("{name} {}", bounds::render_key_pattern(key)),
        Some(Bson::String(key)) => format!("{name} {key}"),
        _ => name.to_string(),
    });
}

/// The `planSummary` for `request`, or `None` for a command with no plan.
pub(crate) fn plan_summary(request: &Document, ctx: &CommandContext) -> Option<String> {
    plan_info(request, ctx).summary
}

/// What a `Slow query` line and profile entry say of a command's plan
/// (RM-023): its `planSummary`; the plan cache key's hashes
/// (`planCacheShapeHash` / `queryHash`, `planCacheKey`), where mongod keys
/// the query; and the engine that runs it (`queryFramework`).
#[derive(Default)]
pub(crate) struct PlanInfo {
    pub summary: Option<String>,
    pub hashes: Option<(String, String)>,
    pub framework: Option<&'static str>,
}

/// [`PlanInfo`] for `request`: a `find`, `count`, `distinct` or `aggregate`
/// over a collection. Nothing for any other command.
///
/// mongod 8.2.11 prints the cache key's hashes for every such read it
/// plans through its plan cache, a `count` it answers from the collection's
/// record count included, and not for an `_id` point read (`EXPRESS`), a
/// `distinct` from a DISTINCT_SCAN, or a collection that does not exist
/// (measured 2026-10-27). `queryFramework` is `sbe` for an `aggregate`
/// whose `$group` or `$lookup` its engine takes, `classic` for the other
/// `find`s, `distinct`s and `aggregate`s; a `count` prints none.
pub(crate) fn plan_info(request: &Document, ctx: &CommandContext) -> PlanInfo {
    match ctx.storage().ok() {
        Some(storage) => plan_info_in(storage, &ctx.db_name, request),
        None => PlanInfo::default(),
    }
}

/// [`plan_info`] for `request` on `db` of `storage`.
pub(crate) fn plan_info_in(
    storage: &dyn crate::storage::Storage,
    db: &str,
    request: &Document,
) -> PlanInfo {
    let name = request.keys().next().map(String::as_str).unwrap_or("");
    let mut out = PlanInfo::default();
    // A read of a view is the view's pipeline over its collection, with the
    // read's query after it: planned as that `aggregate`.
    if let Some(Bson::String(coll)) = request.get(name) {
        if matches!(name, "find" | "count" | "distinct") && crate::views::is_view(storage, db, coll) {
            let mut tail: Vec<Bson> = Vec::new();
            let filter = match name {
                "find" => request.get_document("filter").ok(),
                _ => request.get_document("query").ok(),
            };
            if let Some(f) = filter.filter(|f| !f.is_empty()) {
                tail.push(Bson::Document(doc! { "$match": f.clone() }));
            }
            if let Ok(sort) = request.get_document("sort") {
                tail.push(Bson::Document(doc! { "$sort": sort.clone() }));
            }
            let Ok((base, pipeline)) = crate::aggregate::resolve_view(storage, db, coll, tail)
            else {
                return out;
            };
            let resolved = doc! { "aggregate": base, "pipeline": pipeline, "cursor": {} };
            let mut info = plan_info_in(storage, db, &resolved);
            if name == "count" {
                info.framework = None;
            }
            return info;
        }
    }
    let Some(s) = shape_of(request) else {
        // A pipeline with a source of its own still runs in an engine.
        if name == "aggregate" {
            out.framework = Some("classic");
        }
        return out;
    };
    let stages: Vec<Document> = match name {
        "aggregate" => request
            .get_array("pipeline")
            .map(|p| p.iter().filter_map(|s| s.as_document().cloned()).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    out.framework = match name {
        "find" | "distinct" => Some("classic"),
        "aggregate" => {
            let pipeline: Vec<Bson> = stages.iter().cloned().map(Bson::Document).collect();
            let hoisted = super::hoist::hoist_matches(&pipeline);
            let is_view = |from: &str| crate::views::is_view(storage, db, from);
            Some(
                if secantus_core::sbe::pushdown(&hoisted, &is_view).is_some() {
                    "sbe"
                } else {
                    "classic"
                },
            )
        }
        _ => None,
    };
    if !storage.collection_exists(db, &s.coll).unwrap_or(false) {
        out.summary = Some("EOF".to_string());
        return out;
    }
    let Ok(collation) = crate::util::effective_collation(storage, db, &s.coll, &s.options) else {
        return out;
    };
    let Ok(full) = catalog(storage, db, &s.coll, None) else {
        return out;
    };
    let (catalog, _) = crate::index_filters::apply(
        storage,
        db,
        &s.coll,
        &s.filter,
        s.sort.as_ref(),
        s.projection.as_ref(),
        collation.as_ref(),
        full,
    );
    let Ok(hint) = resolve_hint(s.hint.as_ref(), &catalog) else {
        return out;
    };
    let canonical = parsed::parsed_query(&s.filter);
    let implies = |partial: &Document| storage.query_implies_partial(&s.filter, partial);
    let q = Query {
        canonical: &canonical,
        sort: s.sort.as_ref(),
        projection: s.projection.as_ref(),
        skip: s.skip,
        limit: s.limit,
        hint,
        collation: collation.as_ref(),
        min_max: s.min_max.is_some(),
        return_key: s.return_key,
        show_record_id: s.show_record_id,
        batch_size: s.batch_size,
        keys_only: false,
        wanted: None,
        pipeline: s.pipeline,
        implies: &implies,
        trial: None,
    };
    let node = match &s.kind {
        Kind::Find => plan::plan(&q, &catalog),
        Kind::Count => plan::count(&q, &catalog),
        Kind::Distinct(key) => plan::distinct(&q, &catalog, key, !s.filter.is_empty()),
        _ => return out,
    };
    let mut leaves_of = Vec::new();
    leaves(&node.to_doc(None), &mut leaves_of);
    out.summary = (!leaves_of.is_empty()).then(|| leaves_of.join(", "));
    let scan = match &node {
        Node::Projection { input, .. } => input.as_ref(),
        other => other,
    };
    let distinct_scan = matches!(scan, Node::DistinctScan(..)) && matches!(s.kind, Kind::Distinct(_));
    if distinct_scan {
        out.framework = None;
    }
    let unkeyed = distinct_scan || matches!(node, Node::Express { .. } | Node::IdHack(_) | Node::Eof(_));
    if !unkeyed {
        out.hashes = super::cache_hashes(storage, db, &s, collation.as_ref(), &stages);
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    fn summary(plan: Document) -> String {
        let mut out = Vec::new();
        leaves(&plan, &mut out);
        out.join(", ")
    }

    /// The shapes mongod 8.2.11 printed for the same plans.
    #[test]
    fn leaves_are_named_with_their_key_patterns() {
        assert_eq!(summary(doc! {"stage": "COLLSCAN"}), "COLLSCAN");
        assert_eq!(
            summary(
                doc! {"stage": "FETCH", "inputStage": {"stage": "IXSCAN", "keyPattern": {"x": 1}}}
            ),
            "IXSCAN { x: 1 }"
        );
        assert_eq!(
            summary(
                doc! {"stage": "SUBPLAN", "inputStage": {"stage": "FETCH", "inputStage": {
                "stage": "OR", "inputStages": [
                    {"stage": "IXSCAN", "keyPattern": {"x": 1}},
                    {"stage": "IXSCAN", "keyPattern": {"y": 1}},
                ]}}}
            ),
            "IXSCAN { x: 1 }, IXSCAN { y: 1 }"
        );
        assert_eq!(
            summary(doc! {"stage": "EXPRESS_IXSCAN", "keyPattern": "{ _id: 1 }"}),
            "EXPRESS_IXSCAN { _id: 1 }"
        );
        assert_eq!(summary(doc! {"stage": "EOF"}), "EOF");
    }
}
