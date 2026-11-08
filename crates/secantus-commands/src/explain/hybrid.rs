//! `$score`, `$rankFusion` and `$scoreFusion` as `explain` lists them: the
//! stages mongod 8.2.11 desugars them into (measured 2026-11-08, RM-200).
//!
//! This server runs them as the stages `crate::hybrid` makes, which keep the
//! score in hidden fields; each lists here as the stages of mongod's that it
//! stands for, with the score read back as the metadata it is there. The
//! counts of a run stage stand for every stage it lists as, which is where
//! mongod's counts change: at the end of each input pipeline, at the
//! `$unionWith` and at the `$group`.

use bson::{doc, Bson, Document};
use secantus_core::hybrid::{
    FUSION_DESCRIPTION, RANK_DESCRIPTION, SCORE, SCORE_DESCRIPTION, SCORE_DETAILS,
};

use super::shape::{constified, listed_stage, WINDOW_SORT};

/// The field a `$scoreFusion` input pipeline wraps each document in.
const DOCS: &str = "_internal_scoreFusion_docs";
/// The field a `$scoreFusion` keeps each pipeline's scores in.
const FIELDS: &str = "_internal_scoreFusion_internal_fields";
/// The field `$score` keeps the score it computed in.
const RAW: &str = "internal_raw_score";
/// The field `$score`'s `minMaxScaler` scales the score in.
const MIN_MAX: &str = "internal_min_max_scaler_normalization_score";

/// A stage `crate::hybrid` made, as the stages mongod lists for it; `None`
/// for any other stage.
pub(super) fn listed(stage: &Document, db: &str) -> Option<Vec<Document>> {
    let (name, spec) = stage.iter().next()?;
    match (name.as_str(), spec) {
        ("$_secantusScore", Bson::Document(spec)) => Some(score(spec)),
        ("$_secantusFusePrep", Bson::Document(spec)) => Some(match spec.get_str("kind") {
            Ok("rank") => rank_prep(spec),
            _ => score_prep(spec),
        }),
        ("$_secantusFuse", Bson::Document(spec)) => Some(match spec.get_str("kind") {
            Ok("rank") => rank_fuse(spec),
            _ => score_fuse(spec),
        }),
        // Where the hidden fields come off: no stage of mongod's.
        ("$unset", spec) if *spec == hidden() => Some(Vec::new()),
        ("$_secantusCarry", Bson::Document(spec)) => {
            let inner = spec.get_document("stage").ok()?;
            Some(listed_stage(&unhide_stage(inner), db))
        }
        // A `$unionWith` a fusion input pipeline runs in: its stages listed.
        ("$unionWith", Bson::Document(spec))
            if spec
                .get_array("pipeline")
                .is_ok_and(|p| has_hybrid(&Bson::Array(p.clone()))) =>
        {
            let mut out = spec.clone();
            let stages: Vec<Bson> = spec
                .get_array("pipeline")
                .ok()?
                .iter()
                .filter_map(Bson::as_document)
                .flat_map(|s| listed_stage(s, db))
                .map(Bson::Document)
                .collect();
            out.insert("pipeline", stages);
            Some(vec![doc! { "$unionWith": out }])
        }
        ("$facet", Bson::Document(facets)) if has_hybrid(&Bson::Document(facets.clone())) => {
            let mut out = Document::new();
            for (field, sub) in facets {
                let Bson::Array(stages) = sub else {
                    return None;
                };
                let mut listed = vec![Bson::Document(doc! { "$internalFacetTeeConsumer": {} })];
                for s in rank_sorts(stages).iter().filter_map(Bson::as_document) {
                    listed.extend(listed_stage(s, db).into_iter().map(Bson::Document));
                }
                out.insert(field.clone(), listed);
            }
            Some(vec![doc! { "$facet": out }])
        }
        _ => None,
    }
}

/// A stage that reads the hidden score fields, as the metadata reads it was
/// written with; `None` when it reads none.
pub(super) fn unhidden(stage: &Document) -> Option<Document> {
    reads_hidden(&Bson::Document(stage.clone())).then(|| unhide_stage(stage))
}

fn hidden() -> Bson {
    Bson::Array(vec![Bson::from(SCORE), Bson::from(SCORE_DETAILS)])
}

fn has_hybrid(v: &Bson) -> bool {
    match v {
        Bson::Document(d) => d
            .iter()
            .any(|(k, v)| k.starts_with("$_secantus") || has_hybrid(v)),
        Bson::Array(items) => items.iter().any(has_hybrid),
        _ => false,
    }
}

fn reads_hidden(v: &Bson) -> bool {
    match v {
        Bson::String(s) => s.contains(SCORE),
        Bson::Document(d) => d
            .iter()
            .any(|(k, v)| k.starts_with(SCORE) || reads_hidden(v)),
        Bson::Array(items) => items.iter().any(reads_hidden),
        _ => false,
    }
}

fn unhide_stage(stage: &Document) -> Document {
    let Some((name, spec)) = stage.iter().next() else {
        return stage.clone();
    };
    let spec = match (name.as_str(), spec) {
        // `{f: {$meta: "score"}}` was rewritten to the hidden field,
        // descending; mongod names the computed key.
        ("$sort", Bson::Document(keys)) => {
            let mut computed = 0;
            let mut out = Document::new();
            for (k, v) in keys {
                if k == SCORE {
                    out.insert(format!("$computed{computed}"), doc! { "$meta": "score" });
                    computed += 1;
                } else {
                    out.insert(k.clone(), v.clone());
                }
            }
            Bson::Document(out)
        }
        // The hidden fields an inclusion keeps.
        ("$project", Bson::Document(fields)) => Bson::Document(
            fields
                .iter()
                .filter(|(k, _)| *k != SCORE && *k != SCORE_DETAILS)
                .map(|(k, v)| (k.clone(), unhide(v)))
                .collect(),
        ),
        _ => unhide(spec),
    };
    doc! { name.clone(): spec }
}

fn unhide(v: &Bson) -> Bson {
    match v {
        Bson::String(s) if *s == format!("${SCORE}") => Bson::Document(doc! { "$meta": "score" }),
        Bson::String(s) if *s == format!("${SCORE_DETAILS}") => {
            Bson::Document(doc! { "$meta": "scoreDetails" })
        }
        Bson::Document(d) => {
            Bson::Document(d.iter().map(|(k, v)| (k.clone(), unhide(v))).collect())
        }
        Bson::Array(items) => Bson::Array(items.iter().map(unhide).collect()),
        other => other.clone(),
    }
}

/// A facet sub-pipeline with the `$sort` a `$rankFusion` input ranks by as
/// the `$sort` that keeps its sort key, which mongod lists with
/// `outputSortKeyMetadata` (at the top level the cursor absorbs it).
fn rank_sorts(stages: &[Bson]) -> Vec<Bson> {
    let mut out: Vec<Bson> = stages.to_vec();
    for (k, stage) in stages.iter().enumerate() {
        let rank_prep = stage
            .as_document()
            .and_then(|d| d.get_document("$_secantusFusePrep").ok())
            .is_some_and(|p| p.get_str("kind") == Ok("rank"));
        if !rank_prep {
            continue;
        }
        let sort = out[..k]
            .iter()
            .rposition(|s| s.as_document().is_some_and(|d| d.contains_key("$sort")));
        if let Some(j) = sort {
            let by = out[j]
                .as_document()
                .and_then(|d| d.get("$sort"))
                .cloned()
                .unwrap_or(Bson::Null);
            out[j] = Bson::Document(doc! { WINDOW_SORT: by });
        }
    }
    out
}

fn meta(kind: &str) -> Bson {
    Bson::Document(doc! { "$meta": kind })
}

fn constant(v: impl Into<Bson>) -> Bson {
    Bson::Document(doc! { "$const": v.into() })
}

fn sigmoid(x: Bson) -> Bson {
    Bson::Document(doc! { "$divide": [
        { "$const": 1 },
        { "$add": [{ "$const": 1 }, { "$exp": [{ "$multiply": [{ "$const": -1 }, x] }] }] },
    ] })
}

fn whole_partition() -> Document {
    doc! { "documents": ["unbounded", "unbounded"] }
}

fn number(spec: &Document, key: &str) -> f64 {
    match spec.get(key) {
        Some(Bson::Double(d)) => *d,
        Some(Bson::Int32(i)) => f64::from(*i),
        Some(Bson::Int64(i)) => *i as f64,
        _ => 1.0,
    }
}

fn flag(spec: &Document, key: &str) -> bool {
    spec.get_bool(key).unwrap_or(false)
}

fn strings(spec: &Document, key: &str) -> Vec<String> {
    spec.get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn numbers(spec: &Document, key: &str) -> Vec<f64> {
    spec.get_array(key)
        .map(|a| {
            a.iter()
                .map(|v| match v {
                    Bson::Double(d) => *d,
                    Bson::Int32(i) => f64::from(*i),
                    Bson::Int64(i) => *i as f64,
                    _ => 1.0,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `$score {score, normalization, weight, scoreDetails}`.
fn score(spec: &Document) -> Vec<Document> {
    let expr = spec.get("score").cloned().unwrap_or(Bson::Null);
    let normalization = spec.get_str("normalization").unwrap_or("none");
    let weight = number(spec, "weight");
    let mut out = vec![
        doc! { "$setMetadata": { "score": constified(&expr) } },
        doc! { "$replaceRoot": { "newRoot": { "docs": "$$ROOT" } } },
        doc! { "$addFields": { RAW: meta("score") } },
    ];
    match normalization {
        "sigmoid" => out.push(doc! { "$setMetadata": { "score": sigmoid(meta("score")) } }),
        "minMaxScaler" => {
            out.push(doc! { "$_internalSetWindowFields": {
                "sortBy": { MIN_MAX: -1 },
                "output": { MIN_MAX: {
                    "$minMaxScaler": { "input": meta("score"), "min": 0, "max": 1 },
                    "window": whole_partition(),
                } },
            } });
            out.push(doc! { "$setMetadata": { "score": format!("${MIN_MAX}") } });
        }
        _ => {}
    }
    if weight != 1.0 {
        out.push(doc! { "$setMetadata": {
            "score": { "$multiply": [meta("score"), constant(weight)] },
        } });
    }
    if flag(spec, "details") {
        out.push(doc! { "$setMetadata": { "scoreDetails": {
            "value": meta("score"),
            "description": constant(SCORE_DESCRIPTION),
            "rawScore": format!("${RAW}"),
            "normalization": constant(normalization),
            "weight": constant(weight),
            "expression": constant(spec.get_str("text").unwrap_or_default()),
            "details": constant(Bson::Array(Vec::new())),
        } } });
    }
    out.push(doc! { "$replaceRoot": { "newRoot": "$docs" } });
    out
}

/// The end of one `$rankFusion` input pipeline: each document's rank there,
/// and its weighted reciprocal.
fn rank_prep(spec: &Document) -> Vec<Document> {
    let name = spec.get_str("name").unwrap_or_default();
    let rank = format!("{name}_rank");
    let mut out = vec![
        doc! { "$replaceRoot": { "newRoot": { "docs": "$$ROOT" } } },
        doc! { "$_internalSetWindowFields": {
            "sortBy": { "order": 1 },
            "output": { rank.clone(): { "$rank": {} } },
        } },
        doc! { "$addFields": { format!("{name}_score"): { "$multiply": [
            { "$divide": [{ "$const": 1 }, { "$add": [format!("${rank}"), { "$const": 60 }] }] },
            constant(number(spec, "weight")),
        ] } } },
    ];
    if flag(spec, "details") {
        out.push(doc! { "$addFields": {
            format!("{name}_scoreDetails"): { "details": constant(Bson::Array(Vec::new())) },
        } });
    }
    out
}

/// The end of one `$scoreFusion` input pipeline: each document wrapped,
/// with its score there normalised and weighted.
fn score_prep(spec: &Document) -> Vec<Document> {
    let name = spec.get_str("name").unwrap_or_default();
    let normalization = spec.get_str("normalization").unwrap_or("none");
    let field = |suffix: &str| format!("{name}_{suffix}");
    let value = match normalization {
        "sigmoid" => sigmoid(meta("score")),
        _ => meta("score"),
    };
    let mut out = vec![
        doc! { "$replaceRoot": { "newRoot": { DOCS: "$$ROOT" } } },
        doc! { "$addFields": { FIELDS: {
            field("score"): { "$multiply": [value, constant(number(spec, "weight"))] },
        } } },
    ];
    if flag(spec, "details") {
        let sub = if flag(spec, "subDetails") {
            meta("scoreDetails")
        } else {
            constant(Bson::Array(Vec::new()))
        };
        out.push(doc! { "$addFields": { FIELDS: { field("rawScore"): meta("score") } } });
        out.push(doc! { "$addFields": { FIELDS: { field("scoreDetails"): { "details": sub } } } });
    }
    if normalization == "minMaxScaler" {
        let path = format!("{FIELDS}.{}", field("score"));
        out.push(doc! { "$_internalSetWindowFields": {
            "sortBy": { path.clone(): -1 },
            "output": { path.clone(): {
                "$minMaxScaler": { "input": format!("${path}"), "min": 0, "max": 1 },
                "window": whole_partition(),
            } },
        } });
    }
    out
}

/// `$rankFusion`'s end: one document per `_id`, its ranks summed.
fn rank_fuse(spec: &Document) -> Vec<Document> {
    let names = strings(spec, "names");
    let weights = numbers(spec, "weights");
    let details = flag(spec, "details");
    let best = |path: String| doc! { "$max": { "$ifNull": [path, { "$const": 0 }] } };
    let mut group = doc! { "_id": "$docs._id", "docs": { "$first": "$docs" } };
    for n in &names {
        group.insert(format!("{n}_score"), best(format!("${n}_score")));
        if details {
            group.insert(format!("{n}_rank"), best(format!("${n}_rank")));
            group.insert(
                format!("{n}_scoreDetails"),
                doc! { "$mergeObjects": format!("${n}_scoreDetails") },
            );
        }
    }
    group.insert("$willBeMerged", false);
    let sum: Vec<Bson> = names
        .iter()
        .map(|n| Bson::from(format!("${n}_score")))
        .collect();
    let mut not_ranked = Document::new();
    for n in &names {
        let rank = format!("${n}_rank");
        not_ranked.insert(
            format!("{n}_rank"),
            doc! { "$cond": [{ "$eq": [rank.clone(), { "$const": 0 }] }, { "$const": "NA" }, rank] },
        );
    }
    let mut out = vec![
        doc! { "$group": group },
        doc! { "$addFields": { "score": { "$add": sum.clone() } } },
        doc! { "$addFields": not_ranked },
    ];
    if details {
        let each: Vec<Bson> = names
            .iter()
            .zip(&weights)
            .map(|(n, w)| {
                let rank = format!("${n}_rank");
                Bson::Document(doc! { "$mergeObjects": [
                    {
                        "inputPipelineName": constant(n.as_str()),
                        "rank": rank.clone(),
                        "weight": { "$cond": [
                            { "$eq": [rank, { "$const": "NA" }] },
                            "$$REMOVE",
                            constant(*w),
                        ] },
                    },
                    format!("${n}_scoreDetails"),
                ] })
            })
            .collect();
        out.push(doc! { "$addFields": { "calculatedScoreDetails": each } });
        out.push(doc! { "$setMetadata": { "scoreDetails": {
            "value": "$score",
            "description": constant(RANK_DESCRIPTION),
            "details": "$calculatedScoreDetails",
        } } });
    } else {
        out.push(doc! { "$setMetadata": { "score": { "$add": sum } } });
    }
    out.push(doc! { "$sort": { "sortKey": { "score": -1, "_id": 1 } } });
    out.push(doc! { "$replaceRoot": { "newRoot": "$docs" } });
    out
}

/// `$scoreFusion`'s end: one document per `_id`, its scores combined.
fn score_fuse(spec: &Document) -> Vec<Document> {
    let names = strings(spec, "names");
    let weights = numbers(spec, "weights");
    let details = flag(spec, "details");
    let normalization = spec.get_str("normalization").unwrap_or("none");
    let inner = |n: &str, suffix: &str| format!("${FIELDS}.{n}_{suffix}");
    let mut pushed = Document::new();
    let mut initial = Document::new();
    let mut reduced = Document::new();
    for n in &names {
        let max = |suffix: &str| {
            let key = format!("{n}_{suffix}");
            doc! { "$max": [format!("$$value.{key}"), format!("$$this.{key}")] }
        };
        pushed.insert(
            format!("{n}_score"),
            doc! { "$ifNull": [inner(n, "score"), { "$const": 0 }] },
        );
        initial.insert(format!("{n}_score"), 0);
        reduced.insert(format!("{n}_score"), max("score"));
        if details {
            pushed.insert(
                format!("{n}_rawScore"),
                doc! { "$ifNull": [inner(n, "rawScore"), { "$const": 0 }] },
            );
            pushed.insert(format!("{n}_scoreDetails"), inner(n, "scoreDetails"));
            initial.insert(format!("{n}_rawScore"), 0);
            initial.insert(format!("{n}_scoreDetails"), Document::new());
            reduced.insert(format!("{n}_rawScore"), max("rawScore"));
            let key = format!("{n}_scoreDetails");
            reduced.insert(
                key.clone(),
                doc! { "$mergeObjects": [format!("$$value.{key}"), format!("$$this.{key}")] },
            );
        }
    }
    let combined = match spec.get("expression") {
        Some(expression) => {
            let vars: Document = names
                .iter()
                .map(|n| (n.clone(), Bson::from(inner(n, "score"))))
                .collect();
            Bson::Document(doc! { "$let": { "vars": vars, "in": constified(expression) } })
        }
        None => Bson::Document(doc! { "$avg": names
        .iter()
        .map(|n| Bson::from(inner(n, "score")))
        .collect::<Vec<_>>() }),
    };
    let mut out = vec![
        doc! { "$group": {
            "_id": format!("${DOCS}._id"),
            DOCS: { "$first": format!("${DOCS}") },
            FIELDS: { "$push": pushed },
            "$willBeMerged": false,
        } },
        doc! { "$project": {
            "_id": true,
            DOCS: true,
            FIELDS: { "$reduce": {
                "input": format!("${FIELDS}"),
                "initialValue": { "$const": initial },
                "in": reduced,
            } },
        } },
        doc! { "$replaceRoot": { "newRoot": { "$mergeObjects": [format!("${DOCS}"), "$$ROOT"] } } },
        doc! { "$project": { DOCS: false, "_id": true } },
        doc! { "$setMetadata": { "score": combined } },
    ];
    if details {
        let each: Vec<Bson> = names
            .iter()
            .zip(&weights)
            .map(|(n, w)| {
                Bson::Document(doc! { "$mergeObjects": [
                    {
                        "inputPipelineName": constant(n.as_str()),
                        "inputPipelineRawScore": inner(n, "rawScore"),
                        "weight": constant(*w),
                        "value": inner(n, "score"),
                    },
                    inner(n, "scoreDetails"),
                ] })
            })
            .collect();
        let combination = match spec.get_str("text") {
            Ok(text) => doc! { "method": "custom expression", "expression": text },
            Err(_) => doc! { "method": "average" },
        };
        out.push(doc! { "$addFields": { FIELDS: { "calculatedScoreDetails": each } } });
        out.push(doc! { "$setMetadata": { "scoreDetails": {
            "value": meta("score"),
            "description": constant(FUSION_DESCRIPTION),
            "normalization": constant(normalization),
            "combination": constant(combination),
            "details": format!("${FIELDS}.calculatedScoreDetails"),
        } } });
    }
    out.push(doc! { "$sort": { "sortKey": { "$computed0": meta("score"), "_id": 1 } } });
    out.push(doc! { "$project": { FIELDS: false, "_id": true } });
    out
}
