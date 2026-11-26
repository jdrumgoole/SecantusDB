//! What mongod 8.2.11's `executionStats` report for the stages a stage runs
//! inside it (RM-539): each stage of a `$facet`'s pipelines its own
//! `nReturned` and memory figures, a `$unionWith` over `$documents` the
//! stages it runs, with theirs, and a `$lookup` what its pipeline examined.
//!
//! Each count is that of a run of the pipeline as far as the stage, as for
//! the stages of the pipeline itself ([`super::count_and_spills`]).

use bson::{doc, Bson, Document};

use crate::CommandContext;

use super::shape;

/// The stage `stage`, listed as `listed`, with what the stages inside it
/// report: `input` is the pipeline that hands it its documents, run over
/// `target` with the command's `options`.
pub(super) fn fill(
    ctx: &mut CommandContext,
    target: &Bson,
    input: &[Document],
    options: &Document,
    stage: &Document,
    listed: &mut Document,
) {
    let db = ctx.db_name.clone();
    if let Ok(facets) = stage.get_document("$facet") {
        facet(ctx, target, input, options, facets, listed, &db);
    } else if let Ok(spec) = stage.get_document("$unionWith") {
        union_with(ctx, options, spec, listed, &db);
    } else if let Ok(spec) = stage.get_document("$lookup") {
        lookup(ctx, target, input, options, spec, listed);
    }
}

fn counted(stage: &mut Document, n: i64) {
    stage.insert("nReturned", Bson::Int64(n));
    stage.insert("executionTimeMillisEstimate", Bson::Int64(0));
}

/// The stages of `stages` listed, each with the count and memory of a run
/// of `head` and the stages as far as it.
fn listed_runs(
    ctx: &mut CommandContext,
    target: &Bson,
    head: &[Document],
    stages: &[Document],
    options: &Document,
    db: &str,
) -> Vec<Bson> {
    let full = [head, stages].concat();
    let mut before = if head.is_empty() {
        secantus_core::memlimit::SpillReport::default()
    } else {
        super::count_and_spills(ctx, target, head, options).1
    };
    let mut out = Vec::new();
    for (i, stage) in stages.iter().enumerate() {
        let through = super::run_through(&full, head.len() + i);
        let (n, report) = super::count_and_spills(ctx, target, &through, options);
        let spilled = report.less(&before);
        before = report;
        for mut listed in shape::listed_stage(stage, db) {
            super::spill_fields(&mut listed, &spilled);
            counted(&mut listed, n);
            out.push(Bson::Document(listed));
        }
    }
    out
}

/// mongod's tee hands each facet what the stage took; each stage after it
/// counts what it returned.
fn facet(
    ctx: &mut CommandContext,
    target: &Bson,
    input: &[Document],
    options: &Document,
    facets: &Document,
    listed: &mut Document,
    db: &str,
) {
    let Ok(out) = listed.get_document_mut("$facet") else {
        return;
    };
    let mut filled = Document::new();
    let taken = super::count_through(ctx, target, input, options);
    // A facet's pipelines run in the classic engine, over what the stages
    // in front handed the `$facet`: nothing of theirs is pushed into the
    // query (a `$count` over a collection would be SBE's `$group`).
    let mut head = input.to_vec();
    head.push(doc! { "$_internalInhibitOptimization": {} });
    for (field, sub) in facets {
        let Bson::Array(stages) = sub else {
            return;
        };
        // As mongod's optimizer leaves the pipeline (RM-538).
        let stages: Vec<Document> = super::hoist::optimized_sub_pipeline(stages)
            .into_iter()
            .filter_map(|s| match s {
                Bson::Document(d) => Some(d),
                _ => None,
            })
            .collect();
        let mut tee = doc! { "$internalFacetTeeConsumer": {} };
        counted(&mut tee, taken);
        let mut pipeline = vec![Bson::Document(tee)];
        pipeline.extend(listed_runs(ctx, target, &head, &stages, options, db));
        shape::fold_sort_limits(&mut pipeline);
        filled.insert(field.clone(), pipeline);
    }
    *out = filled;
}

/// A `$unionWith` whose pipeline makes its own documents runs it as the
/// stages `$documents` is (`$queue`, `$project`, `$unwind`,
/// `$replaceRoot`), which it lists with the documents' expression as parsed,
/// not folded into one constant (measured 8.2.11). One over a collection is
/// explained as an aggregate of its own (`super::union`, RM-426).
fn union_with(
    ctx: &mut CommandContext,
    options: &Document,
    spec: &Document,
    listed: &mut Document,
    db: &str,
) {
    if spec.contains_key("coll") {
        return;
    }
    let Ok(pipeline) = spec.get_array("pipeline") else {
        return;
    };
    let stages: Vec<Document> = pipeline
        .iter()
        .filter_map(Bson::as_document)
        .cloned()
        .collect();
    let Some(docs) = stages.first().and_then(|s| s.get("$documents")).cloned() else {
        return;
    };
    let Ok(out) = listed.get_document_mut("$unionWith") else {
        return;
    };
    let target = Bson::Int32(1);
    let field = super::DOCUMENTS_FIELD;
    let path = format!("${field}");
    let unwound = vec![
        doc! { "$documents": [{}] },
        doc! { "$addFields": { field: docs.clone() } },
        doc! { "$unwind": { "path": path.clone() } },
    ];
    let mut head = unwound.clone();
    head.push(doc! { "$replaceRoot": { "newRoot": path.clone() } });
    let each = super::count_through(ctx, &target, &unwound, options);
    let mut queue = doc! { "$queue": [] };
    counted(&mut queue, 1);
    let mut project = doc! { "$project": { "_id": true, field: parsed(&docs) } };
    counted(&mut project, 1);
    let mut unwind = doc! { "$unwind": { "path": path.clone() } };
    counted(&mut unwind, each);
    let mut replace = doc! { "$replaceRoot": { "newRoot": path } };
    counted(&mut replace, each);
    let mut listed_pipeline: Vec<Bson> = [queue, project, unwind, replace]
        .into_iter()
        .map(Bson::Document)
        .collect();
    listed_pipeline.extend(listed_runs(ctx, &target, &head, &stages[1..], options, db));
    out.insert("pipeline", listed_pipeline);
}

/// An expression as mongod's parser leaves it, before the optimizer folds
/// it: an array and an expression object element by element, a field path
/// as written, any other value a `$const`. An operator is listed as
/// [`shape::constified`] lists it.
fn parsed(v: &Bson) -> Bson {
    match v {
        Bson::Array(items) => Bson::Array(items.iter().map(parsed).collect()),
        Bson::Document(d) if d.keys().next().is_none_or(|k| !k.starts_with('$')) => {
            Bson::Document(d.iter().map(|(k, x)| (k.clone(), parsed(x))).collect())
        }
        Bson::String(s) if s.starts_with('$') => v.clone(),
        Bson::Document(_) => shape::constified(v),
        other => Bson::Document(doc! { "$const": other.clone() }),
    }
}

/// A `$lookup` reports what its pipeline examined in the collection it read
/// (`totalDocsExamined`, `totalKeysExamined`, `collectionScans`,
/// `indexesUsed`), summed over the runs of its pipeline (measured 8.2.11):
/// none for a pipeline that makes its own documents. mongod's sub-pipeline
/// cache runs the read of `from` once for every outer document unless the
/// pipeline's first stage is correlated (a `let` variable in it, or the
/// `foreignField` match the join fields put in front), and then at each.
/// The equality form without a pipeline runs in SBE and is RM-683.
fn lookup(
    ctx: &mut CommandContext,
    target: &Bson,
    input: &[Document],
    options: &Document,
    spec: &Document,
    listed: &mut Document,
) {
    let Ok(pipeline) = spec.get_array("pipeline") else {
        return;
    };
    let documents = pipeline
        .first()
        .and_then(Bson::as_document)
        .is_some_and(|s| s.contains_key("$documents"));
    if documents {
        listed.insert("totalDocsExamined", Bson::Int64(0));
        listed.insert("totalKeysExamined", Bson::Int64(0));
        listed.insert("collectionScans", Bson::Int64(0));
        listed.insert("indexesUsed", Bson::Array(Vec::new()));
        return;
    }
    let Ok(from) = spec.get_str("from") else {
        return;
    };
    let let_spec = spec.get_document("let").ok();
    let join = match (spec.get_str("localField"), spec.get_str("foreignField")) {
        (Ok(lf), Ok(ff)) => Some((lf, ff)),
        _ => None,
    };
    let correlated = join.is_some()
        || pipeline
            .first()
            .is_some_and(|first| crate::aggregate::uses_variable(first, let_spec));
    let outer = super::documents_through(ctx, target, input, options);
    let runs: Vec<Option<Document>> = if correlated {
        outer
            .iter()
            .filter_map(Bson::as_document)
            .cloned()
            .map(Some)
            .collect()
    } else if outer.is_empty() {
        Vec::new()
    } else {
        vec![None]
    };
    let mut examined = Examined::default();
    for run in runs {
        let mut stages = pipeline.clone();
        let mut lets = Document::new();
        if let Some(doc) = &run {
            if let Some((lf, ff)) = join {
                let value = crate::aggregate::lookup_local_value(doc, lf).unwrap_or(Bson::Null);
                let test = match value {
                    Bson::Array(items) => doc! { "$in": items },
                    v => doc! { "$eq": v },
                };
                stages.insert(0, Bson::Document(doc! { "$match": { ff: test } }));
            }
            for (name, expr) in let_spec.into_iter().flatten() {
                let bound = match secantus_core::expressions::evaluate_field(
                    doc,
                    expr,
                    &Default::default(),
                ) {
                    Ok(secantus_core::expressions::Field::Value(v)) => {
                        Bson::Document(doc! { "$literal": v })
                    }
                    _ => Bson::String("$$REMOVE".into()),
                };
                lets.insert(name.clone(), bound);
            }
        } else if let Some(ls) = let_spec {
            // An uncorrelated first stage reads none of them; the rest run
            // in the classic engine after the cursor, which is not counted.
            for (name, _) in ls {
                lets.insert(name.clone(), Bson::Null);
            }
        }
        let mut inner = doc! { "aggregate": from, "pipeline": stages, "cursor": {} };
        if !lets.is_empty() {
            inner.insert("let", lets);
        }
        if let Some(c) = options.get("collation") {
            inner.insert("collation", c.clone());
        }
        let cmd = doc! { "explain": inner, "verbosity": "executionStats" };
        let Ok(reply) = super::explain_reply(&cmd, ctx) else {
            return;
        };
        let stats = reply
            .get_array("stages")
            .ok()
            .and_then(|s| s.first())
            .and_then(Bson::as_document)
            .and_then(|s| s.get_document("$cursor").ok())
            .and_then(|c| c.get_document("executionStats").ok())
            .or_else(|| reply.get_document("executionStats").ok());
        let Some(stats) = stats else {
            return;
        };
        examined.add(stats);
    }
    listed.insert("totalDocsExamined", Bson::Int64(examined.docs));
    listed.insert("totalKeysExamined", Bson::Int64(examined.keys));
    listed.insert("collectionScans", Bson::Int64(examined.scans));
    listed.insert(
        "indexesUsed",
        Bson::Array(examined.indexes.into_iter().map(Bson::String).collect()),
    );
}

/// What a `$lookup`'s reads examined, summed.
#[derive(Default)]
struct Examined {
    docs: i64,
    keys: i64,
    scans: i64,
    indexes: Vec<String>,
}

impl Examined {
    fn add(&mut self, stats: &Document) {
        let num = |k: &str| stats.get(k).and_then(crate::util::as_i64).unwrap_or(0);
        self.docs += num("totalDocsExamined");
        self.keys += num("totalKeysExamined");
        if let Ok(stages) = stats.get_document("executionStages") {
            self.walk(stages);
        }
    }

    fn walk(&mut self, stage: &Document) {
        match stage.get_str("stage") {
            Ok("COLLSCAN") => self.scans += 1,
            Ok("IXSCAN" | "COUNT_SCAN" | "DISTINCT_SCAN") => {
                if let Ok(name) = stage.get_str("indexName") {
                    if !self.indexes.iter().any(|i| i == name) {
                        self.indexes.push(name.to_string());
                    }
                }
            }
            _ => {}
        }
        for key in [
            "inputStage",
            "innerStage",
            "outerStage",
            "thenStage",
            "elseStage",
        ] {
            if let Ok(child) = stage.get_document(key) {
                self.walk(child);
            }
        }
        if let Ok(children) = stage.get_array("inputStages") {
            for child in children.iter().filter_map(Bson::as_document) {
                self.walk(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examined_sums_runs_and_names_each_index_once() {
        let mut e = Examined::default();
        let ixscan = doc! {
            "totalDocsExamined": 1_i64, "totalKeysExamined": 1_i64,
            "executionStages": { "stage": "FETCH", "inputStage": { "stage": "IXSCAN", "indexName": "b_1" } },
        };
        e.add(&ixscan);
        e.add(&ixscan);
        e.add(&doc! { "totalDocsExamined": 2_i32, "executionStages": { "stage": "COLLSCAN" } });
        assert_eq!((e.docs, e.keys, e.scans), (4, 2, 1));
        assert_eq!(e.indexes, vec!["b_1".to_string()]);
    }
}
