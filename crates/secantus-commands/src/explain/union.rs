//! A `$unionWith` over a collection as `explain` lists it (RM-426): mongod
//! 8.2.11 explains the sub-pipeline as it explains an aggregate of its own,
//! so the listing's `pipeline` is a `$cursor` holding the sub-query's
//! `queryPlanner` (and `executionStats` above that verbosity), then the
//! stages the cursor did not absorb.

use std::cell::Cell;

use bson::{doc, Bson, Document};

use crate::CommandContext;

use super::Verbosity;

/// `reply`, an aggregate's explain, with each top-level `$unionWith` over
/// a collection listing its sub-pipeline explained.
pub(super) fn explain_unions(
    mut reply: Document,
    verbosity: Verbosity,
    ctx: &mut CommandContext,
) -> Document {
    let Ok(stages) = reply.get_array_mut("stages") else {
        return reply;
    };
    for stage in stages.iter_mut() {
        let Some(listed) = stage.as_document_mut() else {
            continue;
        };
        let (coll, pipeline) = match listed.get("$unionWith") {
            Some(Bson::String(c)) => (c.clone(), Vec::new()),
            Some(Bson::Document(spec)) => match spec.get_str("coll") {
                Ok(c) => (
                    c.to_owned(),
                    spec.get_array("pipeline").cloned().unwrap_or_default(),
                ),
                Err(_) => continue,
            },
            _ => continue,
        };
        // The hidden stages a `$rankFusion` / `$scoreFusion` input runs in
        // are listed as mongod desugars them (RM-200); they are not a
        // pipeline the parser takes.
        if super::hybrid::has_hybrid(&Bson::Array(pipeline.clone())) {
            continue;
        }
        let Some(sub) = explained(&coll, &pipeline, verbosity, ctx) else {
            continue;
        };
        let mut out = doc! { "coll": coll };
        out.insert("pipeline", sub);
        listed.insert("$unionWith", out);
    }
    reply
}

/// The stages mongod lists for `pipeline` over `coll`: its explain's
/// `stages`, which open with the sub-query's `$cursor`.
fn explained(
    coll: &str,
    pipeline: &[Bson],
    verbosity: Verbosity,
    ctx: &mut CommandContext,
) -> Option<Vec<Bson>> {
    let name = match verbosity {
        Verbosity::QueryPlanner => "queryPlanner",
        Verbosity::ExecutionStats => "executionStats",
        Verbosity::AllPlans => "allPlansExecution",
    };
    let cmd = doc! {
        "explain": { "aggregate": coll, "pipeline": pipeline.to_vec(), "cursor": {} },
        "verbosity": name,
    };
    let reply = {
        let _nested = Nested::enter();
        super::explain_reply(&cmd, ctx).ok()?
    };
    reply.get_array("stages").ok().cloned()
}

thread_local! {
    static NESTED: Cell<usize> = const { Cell::new(0) };
}

/// Whether the explain being built is a `$unionWith`'s sub-pipeline, which
/// mongod always runs under a `$cursor` of its own
/// (`DocumentSourceCursor`): never the whole reply a pipeline the query
/// absorbs is at the top level, and saved and restored as a cursor is.
pub(super) fn nested() -> bool {
    NESTED.with(|n| n.get() > 0)
}

struct Nested;

impl Nested {
    fn enter() -> Self {
        NESTED.with(|n| n.set(n.get() + 1));
        Nested
    }
}

impl Drop for Nested {
    fn drop(&mut self) {
        NESTED.with(|n| n.set(n.get() - 1));
    }
}
