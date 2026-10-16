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
use super::{bounds, catalog, parsed, plan, resolve_hint, Kind, Query};
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
    let s = shape_of(request)?;
    let storage = ctx.storage().ok()?;
    let db = &ctx.db_name;
    if !storage.collection_exists(db, &s.coll).unwrap_or(false) {
        return Some("EOF".to_string());
    }
    let catalog = catalog(storage, db, &s.coll, None).ok()?;
    let collation = crate::util::effective_collation(storage, db, &s.coll, &s.options).ok()?;
    let hint = resolve_hint(s.hint.as_ref(), &catalog).ok()?;
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
        Kind::Distinct(key) => plan::distinct(&q, &catalog, key),
        _ => return None,
    };
    let mut out = Vec::new();
    leaves(&node.to_doc(None), &mut out);
    (!out.is_empty()).then(|| out.join(", "))
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
