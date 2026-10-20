//! The hybrid-search stages mongod 8.2 added: `$score`, `$rankFusion` and
//! `$scoreFusion` (measured 8.2.11, 2026-10-20).
//!
//! mongod parses each into stages it already has (its `explain` shows the
//! desugaring), keeping the score in the document's metadata. This engine
//! keeps no metadata, so the score rides in two hidden fields, [`SCORE`] and
//! [`SCORE_DETAILS`], which the command layer reads `{$meta: "score"}` and
//! `{$meta: "scoreDetails"}` from and takes off again. The command layer also
//! does the desugaring that needs the collection (each fusion input pipeline
//! becomes a `$unionWith`); what is left are the three stages here:
//!
//! * `$_secantusScore` -- `$score`: evaluate, normalise, weight;
//! * `$_secantusFusePrep` -- the end of one fusion input pipeline: each
//!   document's rank there (1-based), or its score normalised over that
//!   pipeline's documents;
//! * `$_secantusFuse` -- one document per `_id`, its fused score, sorted by
//!   score descending and `_id` ascending, as mongod's desugaring sorts.

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::{doc, Bson, Document};

use crate::fallback::Fallback;

type R<T> = Result<T, Fallback>;

/// The hidden field a document's score is kept in.
pub const SCORE: &str = "__secantus_meta_score";
/// The hidden field a document's score details are kept in.
pub const SCORE_DETAILS: &str = "__secantus_meta_scoreDetails";
/// The field a fusion input pipeline wraps each document in.
const FUSE: &str = "__secantus_fuse";

const SCORE_DESCRIPTION: &str = "the score calculated from multiplying a weight in the range \
                                 [0,1] with either a normalized or nonnormalized value:";
const RANK_DESCRIPTION: &str = "value output by reciprocal rank fusion algorithm, computed as \
                                sum of (weight * (1 / (60 + rank))) across input pipelines from \
                                which this document is output, from:";
const FUSION_DESCRIPTION: &str = "the value calculated by combining the scores (either \
                                  normalized or raw) across input pipelines from which this \
                                  document is output from:";

/// An expression as mongod prints it in score details: the argument wrapped
/// in `{ string: ... }` and rendered as its `BSONObj::toString` does,
/// strings in single quotes (measured: `"$x"` is `{ string: '$x' }`).
pub fn expression_text(expr: &Bson) -> String {
    format!("{{ string: {} }}", render(expr))
}

fn render(v: &Bson) -> String {
    match v {
        Bson::Document(d) if d.is_empty() => "{}".to_string(),
        Bson::Document(d) => {
            let fields: Vec<String> = d
                .iter()
                .map(|(k, v)| format!("{k}: {}", render(v)))
                .collect();
            format!("{{ {} }}", fields.join(", "))
        }
        Bson::Array(a) if a.is_empty() => "[]".to_string(),
        Bson::Array(a) => {
            let items: Vec<String> = a.iter().map(render).collect();
            format!("[ {} ]", items.join(", "))
        }
        Bson::String(s) => format!("'{s}'"),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => crate::query::fmt_double_parse(*d),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// A score as a double; `None` for a value that is not a number.
fn as_score(v: &Bson) -> Option<f64> {
    match v {
        Bson::Double(d) => Some(*d),
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Decimal128(d) => d.to_string().parse::<f64>().ok(),
        _ => None,
    }
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// mongod's `$minMaxScaler` over `values` into `[0, 1]`; every value 0
/// when they are all equal (measured).
fn min_max(values: &[f64]) -> Vec<f64> {
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    values
        .iter()
        .map(|v| {
            if max > min {
                (v - min) / (max - min)
            } else {
                0.0
            }
        })
        .collect()
}

fn normalised(raw: &[f64], normalization: &str) -> Vec<f64> {
    match normalization {
        "sigmoid" => raw.iter().map(|v| sigmoid(*v)).collect(),
        "minMaxScaler" => min_max(raw),
        _ => raw.to_vec(),
    }
}

fn str_field<'a>(spec: &'a Document, key: &str) -> R<&'a str> {
    spec.get_str(key).map_err(|_| Fallback::Defer)
}

fn f64_field(spec: &Document, key: &str) -> R<f64> {
    spec.get(key).and_then(as_score).ok_or(Fallback::Defer)
}

/// `$_secantusScore {score, normalization, weight, details, text}`.
pub fn score_stage(docs: Vec<Document>, spec: &Bson, vars: &Document) -> R<Vec<Document>> {
    let spec = spec.as_document().ok_or(Fallback::Defer)?;
    let expr = spec.get("score").ok_or(Fallback::Defer)?;
    let normalization = str_field(spec, "normalization")?;
    let weight = f64_field(spec, "weight")?;
    let details = spec.get_bool("details").unwrap_or(false);
    let text = spec.get_str("text").unwrap_or_default();
    let mut raw = Vec::with_capacity(docs.len());
    for d in &docs {
        let v = crate::expressions::evaluate_or_missing(d, expr, vars)?;
        let Some(score) = as_score(&v) else {
            let found = match v {
                Bson::Undefined => "missing",
                other => crate::query::bson_type_name(&other),
            };
            return Err(Fallback::mongo(
                14,
                format!("Meta field 'score' requires numeric type, found {found}"),
            )
            .with_folded(false));
        };
        raw.push(score);
    }
    let scaled = normalised(&raw, normalization);
    Ok(docs
        .into_iter()
        .zip(raw.iter().zip(scaled))
        .map(|(mut d, (raw, scaled))| {
            let value = scaled * weight;
            d.insert(SCORE, value);
            if details {
                d.insert(
                    SCORE_DETAILS,
                    doc! {
                        "value": value,
                        "description": SCORE_DESCRIPTION,
                        "rawScore": *raw,
                        "normalization": normalization,
                        "weight": weight,
                        "expression": text,
                        "details": [],
                    },
                );
            }
            d
        })
        .collect())
}

/// Each document's score metadata, for [`restore_metadata`].
pub fn take_metadata(docs: &[Document]) -> Vec<(Option<Bson>, Option<Bson>)> {
    docs.iter()
        .map(|d| (d.get(SCORE).cloned(), d.get(SCORE_DETAILS).cloned()))
        .collect()
}

/// Give each of `docs` back the metadata [`take_metadata`] took from the
/// document it was made from.
pub fn restore_metadata(
    mut docs: Vec<Document>,
    kept: Vec<(Option<Bson>, Option<Bson>)>,
) -> R<Vec<Document>> {
    if docs.len() != kept.len() {
        return Err(Fallback::Defer);
    }
    for (d, (score, details)) in docs.iter_mut().zip(kept) {
        if let Some(v) = score {
            d.insert(SCORE, v);
        }
        if let Some(v) = details {
            d.insert(SCORE_DETAILS, v);
        }
    }
    Ok(docs)
}

/// The 1-based rank of each of `docs`, which arrive sorted by `by`:
/// mongod's `$rank`, so documents whose sort keys are equal share a rank
/// and the next one skips (measured: `y` descending over `4, 3, 2, 1, 0, 0`
/// ranks the last two 5 and 5). Without a sort, the position.
fn ranks(docs: &[Document], by: Option<&Bson>) -> R<Vec<i64>> {
    let Some(by) = by else {
        return Ok((1..=docs.len() as i64).collect());
    };
    let fields = crate::aggregate::sort_fields(by)?;
    let mut out = Vec::with_capacity(docs.len());
    let mut prev: Option<Vec<Bson>> = None;
    for (at, d) in docs.iter().enumerate() {
        let key = crate::aggregate::sort_key_values(d, &fields)?;
        let tied = prev.as_ref().is_some_and(|p| {
            p.len() == key.len()
                && p.iter()
                    .zip(&key)
                    .all(|(a, b)| crate::order::cmp(a, b) == Ordering::Equal)
        });
        let rank = if tied { out[at - 1] } else { at as i64 + 1 };
        out.push(rank);
        prev = Some(key);
    }
    Ok(out)
}

/// `$_secantusFusePrep {pipe, by?, normalization?}`: wrap each document of
/// one input pipeline with its pipeline, its rank there ([`ranks`]) and, for
/// `$scoreFusion`, its raw score and the score normalised over this
/// pipeline's documents.
pub fn fuse_prep_stage(docs: Vec<Document>, spec: &Bson) -> R<Vec<Document>> {
    let spec = spec.as_document().ok_or(Fallback::Defer)?;
    let pipe = spec.get_i32("pipe").map_err(|_| Fallback::Defer)?;
    let normalization = spec.get_str("normalization").ok();
    let ranked = ranks(&docs, spec.get("by"))?;
    let raw: Vec<f64> = docs
        .iter()
        .map(|d| d.get(SCORE).and_then(as_score).unwrap_or(0.0))
        .collect();
    let scaled = normalization.map(|n| normalised(&raw, n));
    Ok(docs
        .into_iter()
        .enumerate()
        .map(|(at, mut d)| {
            let sub = d.remove(SCORE_DETAILS);
            d.remove(SCORE);
            let mut wrap = doc! { "pipe": pipe, "rank": ranked[at], "doc": d };
            if let Some(scaled) = &scaled {
                wrap.insert("raw", raw[at]);
                wrap.insert("value", scaled[at]);
            }
            if let Some(sub) = sub {
                wrap.insert("sub", sub);
            }
            doc! { FUSE: wrap }
        })
        .collect())
}

/// What one document has from one input pipeline: its best rank, or its
/// best score.
#[derive(Clone)]
struct Seen {
    rank: i64,
    raw: f64,
    value: f64,
    sub: Option<Bson>,
}

/// A score mongod reduces from `0` with `$max`: a value that is not above
/// the `0` it starts from stays that integer 0 (measured: a document whose
/// score is 0 reports `0`, not `0.0`).
fn reduced(v: f64) -> Bson {
    if v > 0.0 {
        Bson::Double(v)
    } else {
        Bson::Int32(0)
    }
}

/// `$_secantusFuse {kind, names, weights, normalization?, expression?,
/// text?, details}`.
pub fn fuse_stage(docs: Vec<Document>, spec: &Bson, vars: &Document) -> R<Vec<Document>> {
    let spec = spec.as_document().ok_or(Fallback::Defer)?;
    let rank_fusion = str_field(spec, "kind")? == "rank";
    let names: Vec<String> = spec
        .get_array("names")
        .map_err(|_| Fallback::Defer)?
        .iter()
        .filter_map(|n| n.as_str().map(str::to_string))
        .collect();
    let weights: Vec<f64> = spec
        .get_array("weights")
        .map_err(|_| Fallback::Defer)?
        .iter()
        .filter_map(as_score)
        .collect();
    if weights.len() != names.len() {
        return Err(Fallback::Defer);
    }
    let details = spec.get_bool("details").unwrap_or(false);
    let expression = spec.get("expression");

    // One entry per `_id`, in the order each first arrived.
    let mut order: Vec<(Document, Vec<Option<Seen>>)> = Vec::new();
    let mut at: HashMap<Vec<u8>, usize> = HashMap::new();
    for wrapped in docs {
        let Some(Bson::Document(w)) = wrapped.get(FUSE) else {
            return Err(Fallback::Defer);
        };
        let pipe = w.get_i32("pipe").map_err(|_| Fallback::Defer)? as usize;
        let doc = w.get_document("doc").map_err(|_| Fallback::Defer)?;
        let id = doc.get("_id").cloned().unwrap_or(Bson::Null);
        let key = crate::sortkey::encode_value(&id, None).map_err(|_| Fallback::Defer)?;
        let slot = *at.entry(key).or_insert_with(|| {
            order.push((doc.clone(), vec![None; names.len()]));
            order.len() - 1
        });
        let seen = Seen {
            rank: w.get_i64("rank").unwrap_or(0),
            raw: w.get_f64("raw").unwrap_or(0.0),
            value: w.get_f64("value").unwrap_or(0.0),
            sub: w.get("sub").cloned(),
        };
        let entry = &mut order[slot].1[pipe];
        let better = match entry {
            None => true,
            Some(old) if rank_fusion => seen.rank < old.rank,
            Some(old) => seen.value > old.value,
        };
        if better {
            *entry = Some(seen);
        }
    }

    let mut out: Vec<(f64, Document)> = Vec::with_capacity(order.len());
    for (mut doc, seen) in order {
        let (score, detail) = if rank_fusion {
            let mut score = 0.0;
            let mut parts = Vec::new();
            for (i, s) in seen.iter().enumerate() {
                match s {
                    Some(s) => {
                        score += (1.0 / (s.rank as f64 + 60.0)) * weights[i];
                        parts.push(Bson::Document(doc! {
                            "inputPipelineName": &names[i],
                            "rank": s.rank,
                            "weight": weights[i],
                            "details": [],
                        }));
                    }
                    None => parts.push(Bson::Document(doc! {
                        "inputPipelineName": &names[i],
                        "rank": "NA",
                    })),
                }
            }
            let detail = doc! {
                "value": score,
                "description": RANK_DESCRIPTION,
                "details": parts,
            };
            (score, detail)
        } else {
            let values: Vec<f64> = seen
                .iter()
                .enumerate()
                .map(|(i, s)| s.as_ref().map_or(0.0, |s| s.value * weights[i]).max(0.0))
                .collect();
            let score = match expression {
                Some(expr) => {
                    let mut with = vars.clone();
                    for (i, name) in names.iter().enumerate() {
                        with.insert(name.clone(), reduced(values[i]));
                    }
                    let v = crate::expressions::evaluate(&doc, expr, &with)?;
                    as_score(&v).ok_or_else(|| {
                        Fallback::mongo(
                            14,
                            format!(
                                "Meta field 'score' requires numeric type, found {}",
                                crate::query::bson_type_name(&v)
                            ),
                        )
                        .with_folded(false)
                    })?
                }
                None => values.iter().sum::<f64>() / values.len() as f64,
            };
            let mut parts = Vec::new();
            for (i, s) in seen.iter().enumerate() {
                let mut part = doc! { "inputPipelineName": &names[i] };
                match s {
                    Some(s) => {
                        part.insert("inputPipelineRawScore", reduced(s.raw));
                        part.insert("weight", weights[i]);
                        part.insert("value", reduced(values[i]));
                        part.insert("details", s.sub.clone().unwrap_or(Bson::Array(Vec::new())));
                    }
                    None => {
                        part.insert("inputPipelineRawScore", 0);
                        part.insert("weight", weights[i]);
                        part.insert("value", 0);
                    }
                }
                parts.push(Bson::Document(part));
            }
            let combination = match expression {
                Some(_) => doc! {
                    "method": "custom expression",
                    "expression": spec.get_str("text").unwrap_or_default(),
                },
                None => doc! { "method": "average" },
            };
            let detail = doc! {
                "value": score,
                "description": FUSION_DESCRIPTION,
                "normalization": spec.get_str("normalization").unwrap_or("none"),
                "combination": combination,
                "details": parts,
            };
            (score, detail)
        };
        doc.insert(SCORE, score);
        if details {
            doc.insert(SCORE_DETAILS, detail);
        }
        out.push((score, doc));
    }
    // Score descending, then `_id` ascending.
    out.sort_by(|(a, da), (b, db)| {
        b.partial_cmp(a).unwrap_or(Ordering::Equal).then_with(|| {
            crate::order::cmp(
                da.get("_id").unwrap_or(&Bson::Null),
                db.get("_id").unwrap_or(&Bson::Null),
            )
        })
    });
    Ok(out.into_iter().map(|(_, d)| d).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_print_as_mongod_prints_them() {
        assert_eq!(
            expression_text(&Bson::String("$x".into())),
            "{ string: '$x' }"
        );
        assert_eq!(expression_text(&Bson::Int32(5)), "{ string: 5 }");
        assert_eq!(
            expression_text(&bson::bson!({"$multiply": ["$x", {"$literal": 2.5}]})),
            "{ string: { $multiply: [ '$x', { $literal: 2.5 } ] } }"
        );
    }

    #[test]
    fn min_max_of_equal_values_is_zero() {
        assert_eq!(min_max(&[3.0, 3.0]), vec![0.0, 0.0]);
        assert_eq!(min_max(&[1.0, 2.0, 3.0]), vec![0.0, 0.5, 1.0]);
    }
}
