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
        lookup(spec, listed);
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
    let mut run = head.to_vec();
    let mut before = if head.is_empty() {
        secantus_core::memlimit::SpillReport::default()
    } else {
        super::count_and_spills(ctx, target, head, options).1
    };
    let mut out = Vec::new();
    for stage in stages {
        run.push(stage.clone());
        let (n, report) = super::count_and_spills(ctx, target, &run, options);
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
        let stages: Vec<Document> = stages
            .iter()
            .filter_map(Bson::as_document)
            .cloned()
            .collect();
        let mut tee = doc! { "$internalFacetTeeConsumer": {} };
        counted(&mut tee, taken);
        let mut pipeline = vec![Bson::Document(tee)];
        pipeline.extend(listed_runs(ctx, target, &head, &stages, options, db));
        filled.insert(field.clone(), pipeline);
    }
    *out = filled;
}

/// A `$unionWith` whose pipeline makes its own documents runs it as the
/// stages `$documents` is (`$queue`, `$project`, `$unwind`,
/// `$replaceRoot`), which it lists with the documents' expression as parsed,
/// not folded into one constant (measured 8.2.11). One over a collection is
/// RM-681.
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

/// A `$lookup` reports what its pipeline examined in the collections it read
/// (`totalDocsExamined`, `totalKeysExamined`, `collectionScans`,
/// `indexesUsed`): none for a pipeline that makes its own documents. One
/// that reads a collection is RM-682.
fn lookup(spec: &Document, listed: &mut Document) {
    let documents = spec
        .get_array("pipeline")
        .ok()
        .and_then(|p| p.first())
        .and_then(Bson::as_document)
        .is_some_and(|s| s.contains_key("$documents"));
    if !documents {
        return;
    }
    listed.insert("totalDocsExamined", Bson::Int64(0));
    listed.insert("totalKeysExamined", Bson::Int64(0));
    listed.insert("collectionScans", Bson::Int64(0));
    listed.insert("indexesUsed", Bson::Array(Vec::new()));
}
