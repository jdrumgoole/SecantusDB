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
    inner: &Document,
    verbosity: Verbosity,
    ctx: &mut CommandContext,
) -> Document {
    let mut fusions = fusion_inputs(inner).into_iter();
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
        // The `$unionWith` a `$rankFusion` / `$scoreFusion` input pipeline
        // runs in (RM-696): mongod explains the input pipeline the user
        // wrote under its `$cursor`, then the stages it desugars the
        // fusion into, listed (RM-200), which are not a pipeline the
        // parser takes.
        let fusion = fusions.next().flatten();
        let sub = match fusion {
            Some((prefix, listed_len)) => {
                let Some(mut sub) = explained(&coll, &prefix, verbosity, ctx) else {
                    continue;
                };
                sub.extend(pipeline.iter().skip(listed_len).cloned());
                sub
            }
            None => {
                if super::hybrid::has_hybrid(&Bson::Array(pipeline.clone())) {
                    continue;
                }
                match explained(&coll, &pipeline, verbosity, ctx) {
                    Some(sub) => sub,
                    None => continue,
                }
            }
        };
        let mut out = doc! { "coll": coll };
        out.insert("pipeline", sub);
        listed.insert("$unionWith", out);
    }
    reply
}

/// For each top-level `$unionWith` over a collection, in order: when it is
/// one a fusion input pipeline runs in, the stages of that pipeline the
/// user wrote (those before the first the fusion added) and how many
/// stages they list as.
fn fusion_inputs(inner: &Document) -> Vec<Option<(Vec<Bson>, usize)>> {
    let (Ok(coll), Ok(pipeline)) = (inner.get_str("aggregate"), inner.get_array("pipeline")) else {
        return Vec::new();
    };
    if !crate::hybrid::mentions_hybrid(pipeline) {
        return Vec::new();
    }
    let Ok(prepared) = crate::hybrid::prepare_for_explain(pipeline.clone(), Some(coll)) else {
        return Vec::new();
    };
    prepared
        .iter()
        .filter_map(Bson::as_document)
        .filter_map(|d| d.get_document("$unionWith").ok())
        .filter(|spec| spec.contains_key("coll"))
        .map(|spec| {
            let sub = spec.get_array("pipeline").ok()?;
            if !super::hybrid::has_hybrid(&Bson::Array(sub.clone())) {
                return None;
            }
            let prefix: Vec<Bson> = sub
                .iter()
                .take_while(|s| s.as_document().is_some_and(super::hybrid::is_user_stage))
                .cloned()
                .collect();
            let listed = prefix
                .iter()
                .filter_map(Bson::as_document)
                .map(|d| super::shape::listed_stage(d, "").len())
                .sum();
            Some((prefix, listed))
        })
        .collect()
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
