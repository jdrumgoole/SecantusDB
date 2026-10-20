//! `$score`, `$rankFusion` and `$scoreFusion` in an `aggregate`: parsed and
//! refused with mongod 8.2.11's codes and words, then desugared as mongod
//! desugars them (its `explain` shows the stages, measured 2026-10-20) into
//! stages this server runs, with the score in hidden fields
//! (`secantus_core::hybrid`).
//!
//! Each fusion input pipeline runs over the collection: the first one in
//! place, the rest as `$unionWith`s, each ending on `$_secantusFusePrep`;
//! `$_secantusFuse` then makes one document per `_id`, sorted by score
//! descending and `_id` ascending. `{$meta: "score"}` and `{$meta:
//! "scoreDetails"}` become reads of the hidden fields from the stage that
//! sets them to the stage where the metadata ends.

use bson::{doc, Bson, Document};
use secantus_core::hybrid::{expression_text, SCORE, SCORE_DETAILS};

use crate::CommandError;

const NOT_FIRST: &str =
    "$rankFusion/$scoreFusion can only be the first stage of an aggregation pipeline.";
const NO_PIPELINES: &str = "A hybrid scoring stage should be run with at least one pipeline.";

fn err(code: i32, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, crate::util::error_code_name(code), msg)
}

fn stage_name(stage: &Bson) -> &str {
    stage
        .as_document()
        .and_then(|d| d.keys().next())
        .map_or("", String::as_str)
}

fn is_number(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

fn as_f64(v: &Bson) -> f64 {
    match v {
        Bson::Int32(i) => f64::from(*i),
        Bson::Int64(i) => *i as f64,
        Bson::Double(d) => *d,
        Bson::Decimal128(d) => d.to_string().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// A number as mongod echoes it in these messages: `2`, `-0.5`.
fn number_text(v: &Bson) -> String {
    match v {
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => {
            if d.fract() == 0.0 && d.is_finite() {
                format!("{}", *d as i64)
            } else {
                d.to_string()
            }
        }
        other => other.to_string(),
    }
}

fn unknown_field(path: &str, key: &str) -> CommandError {
    err(
        40415,
        format!("BSON field '{path}.{key}' is an unknown field."),
    )
}

fn missing_field(path: &str) -> CommandError {
    err(
        40414,
        format!("BSON field '{path}' is missing but a required field"),
    )
}

fn enumeration(value: &Bson, path: &str) -> CommandError {
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string);
    err(
        2,
        format!("Enumeration value '{text}' for field '{path}' is not a valid value."),
    )
}

const NORMALIZATIONS: [&str; 3] = ["none", "sigmoid", "minMaxScaler"];

fn normalization(
    v: Option<&Bson>,
    path: &str,
    default: Option<&str>,
) -> Result<String, CommandError> {
    match v {
        None => default
            .map(str::to_string)
            .ok_or_else(|| missing_field(path)),
        Some(Bson::String(s)) if NORMALIZATIONS.contains(&s.as_str()) => Ok(s.clone()),
        Some(other) => Err(enumeration(other, path)),
    }
}

/// `$score` as the internal stage that runs it.
fn score_stage(spec: &Bson) -> Result<Bson, CommandError> {
    let Bson::Document(spec) = spec else {
        return Err(err(
            9,
            format!(
                "The $score stage specification must be an object, found {}",
                secantus_core::query::bson_type_name(spec)
            ),
        ));
    };
    for key in spec.keys() {
        if !matches!(
            key.as_str(),
            "score" | "normalization" | "weight" | "scoreDetails"
        ) {
            return Err(unknown_field("$score", key));
        }
    }
    let score = spec
        .get("score")
        .ok_or_else(|| missing_field("$score.score"))?;
    let normalization = normalization(
        spec.get("normalization"),
        "$score.normalization",
        Some("none"),
    )?;
    let weight = match spec.get("weight") {
        None => 1.0,
        Some(w) if is_number(w) => {
            let v = as_f64(w);
            if v > 1.0 {
                return Err(err(
                    2,
                    format!(
                        "BSON field 'weight' value must be <= 1, actual value '{}'",
                        number_text(w)
                    ),
                ));
            }
            if v < 0.0 {
                return Err(err(
                    2,
                    format!(
                        "BSON field 'weight' value must be >= 0, actual value '{}'",
                        number_text(w)
                    ),
                ));
            }
            v
        }
        Some(w) => {
            return Err(err(
                14,
                format!(
                    "BSON field '$score.weight' is the wrong type '{}', expected types '[double, \
                     long, int, decimal]'",
                    secantus_core::query::bson_type_name(w)
                ),
            ))
        }
    };
    let details = matches!(spec.get("scoreDetails"), Some(Bson::Boolean(true)));
    Ok(Bson::Document(doc! {
        "$_secantusScore": {
            "score": score.clone(),
            "normalization": normalization,
            "weight": weight,
            "details": details,
            "text": expression_text(score),
        }
    }))
}

/// A fusion stage's input pipelines, checked: in the order written.
fn input_pipelines(
    stage: &str,
    pipelines: &Bson,
) -> Result<Vec<(String, Vec<Bson>)>, CommandError> {
    let Bson::Document(pipelines) = pipelines else {
        return Err(err(
            14,
            format!(
                "BSON field '{stage}.input.pipelines' is the wrong type '{}', expected type \
                 'object'",
                secantus_core::query::bson_type_name(pipelines)
            ),
        ));
    };
    let mut out = Vec::new();
    for (name, p) in pipelines {
        if name.starts_with('$') {
            return Err(err(
                16410,
                format!(
                    "{stage} pipeline names must follow the naming rules of field path \
                     expressions. :: caused by :: FieldPath field names may not start with '$', \
                     given '{name}'."
                ),
            ));
        }
        let Bson::Array(stages) = p else {
            return Err(err(
                14,
                format!(
                    "Error parsing {stage}.input.pipelines :: caused by :: A pipeline must be an \
                     array of objects"
                ),
            ));
        };
        if stages.iter().any(|s| !matches!(s, Bson::Document(_))) {
            return Err(err(
                14,
                format!(
                    "Error parsing {stage}.input.pipelines :: caused by :: A pipeline must be an \
                     array of objects"
                ),
            ));
        }
        out.push((name.clone(), stages.clone()));
    }
    if out.is_empty() {
        return Err(err(2, NO_PIPELINES));
    }
    Ok(out)
}

/// The stages a fusion input pipeline may hold: the ones that "retrieve,
/// limit, or order documents".
const SELECTION_STAGES: [&str; 8] = [
    "$match",
    "$sort",
    "$limit",
    "$skip",
    "$geoNear",
    "$sample",
    "$search",
    "$vectorSearch",
];

fn check_input(stage: &str, stages: &[Bson]) -> Result<(), CommandError> {
    let rank = stage == "$rankFusion";
    for s in stages {
        let name = stage_name(s);
        if matches!(name, "$rankFusion" | "$scoreFusion") {
            return Err(err(10170100, NOT_FIRST));
        }
        if name == "$score" {
            if rank {
                return Err(err(
                    10614800,
                    "$rankFusion input pipelines must not contain a $score stage.",
                ));
            }
            continue;
        }
        if !SELECTION_STAGES.contains(&name) {
            return Err(err(
                if rank { 9191103 } else { 9402502 },
                format!(
                    "{name} is not a selection stage because it modifies or transforms the input \
                     documents. Only stages that retrieve, limit, or order documents are allowed."
                ),
            ));
        }
    }
    let first = stages.first().map(stage_name).unwrap_or("");
    if rank && first != "$geoNear" && !stages.iter().any(|s| stage_name(s) == "$sort") {
        return Err(err(
            9191100,
            "Pipeline did not begin with a ranked stage and did not contain an explicit $sort \
             stage. All subpipelines to the $rankFusion stage must begin with one of $search, \
             $vectorSearch, $geoNear, or have a custom $sort in the pipeline.",
        ));
    }
    if !rank && !stages.iter().any(|s| stage_name(s) == "$score") {
        return Err(err(
            9402500,
            "All subpipelines to the $scoreFusion stage must begin with one of $search, \
             $vectorSearch, or have a custom $score in the pipeline. Pipeline did not begin with \
             a scored stage and did not contain an explicit $score stage.",
        ));
    }
    Ok(())
}

/// The per-pipeline weights, 1 where none is given.
fn weights(
    stage: &str,
    names: &[String],
    given: Option<&Bson>,
) -> Result<(Vec<f64>, bool), CommandError> {
    let mut out = vec![1.0; names.len()];
    let Some(given) = given else {
        return Ok((out, false));
    };
    let Bson::Document(given) = given else {
        return Err(err(
            14,
            format!(
                "BSON field '{stage}.combination.weights' is the wrong type '{}', expected type \
                 'object'",
                secantus_core::query::bson_type_name(given)
            ),
        ));
    };
    let unknown: Vec<&String> = given.keys().filter(|k| !names.contains(k)).collect();
    if given.len() > names.len() {
        let listed: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
        return Err(err(
            9460301,
            format!(
                "{stage} input has more weights ({}) than pipelines ({}). If \
                 'combination.weights' is specified, there must be a less or equal number of \
                 weights as pipelines, each of which is unique and existing. Possible extraneous \
                 specified weights = [{}]",
                given.len(),
                names.len(),
                listed.join(", ")
            ),
        ));
    }
    for (name, w) in given {
        if !is_number(w) {
            return Err(err(
                13118,
                format!(
                    "expected {name} to have a numeric type, but it is a {}",
                    secantus_core::query::bson_type_name(w)
                ),
            ));
        }
    }
    if !unknown.is_empty() {
        let suggestions: Vec<String> = unknown
            .iter()
            .map(|u| format!("(provided: '{u}' -> suggested: '{}')", closest(u, names)))
            .collect();
        return Err(err(
            9967500,
            format!(
                "{stage} stage contained ({}) weight(s) in 'combination.weights' that did not \
                 reference valid pipeline names. Suggestions for valid pipeline names: {}",
                unknown.len(),
                suggestions.join(", ")
            ),
        ));
    }
    let short = stage.trim_start_matches('$');
    for (name, w) in given {
        let v = as_f64(w);
        if v < 0.0 {
            return Err(err(
                9460300,
                format!(
                    "{short}'s pipeline weight must be non-negative, but given {} for pipeline \
                     '{name}'.",
                    number_text(w)
                ),
            ));
        }
        if let Some(at) = names.iter().position(|n| n == name) {
            out[at] = v;
        }
    }
    Ok((out, true))
}

/// The pipeline name nearest `to` by edit distance, the first among equals.
fn closest<'a>(to: &str, names: &'a [String]) -> &'a str {
    fn distance(a: &str, b: &str) -> usize {
        let b: Vec<char> = b.chars().collect();
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut prev = row[0];
            row[0] = i + 1;
            for (j, cb) in b.iter().enumerate() {
                let cur = row[j + 1];
                row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
                prev = cur;
            }
        }
        row[b.len()]
    }
    names
        .iter()
        .min_by_key(|n| distance(to, n))
        .map_or("", String::as_str)
}

fn fcv_refusal(name: &str) -> Option<CommandError> {
    if !secantus_core::expressions::fcv_before_82() {
        return None;
    }
    let msg = match name {
        "$rankFusion" => "'featureFlagRankFusionBasic' must be enabled to use rankFusion".to_string(),
        "$scoreFusion" => "$scoreFusion is not allowed in the current feature compatibility \
                           version. See \
                           https://docs.mongodb.com/master/release-notes/8.0-compatibility/#feature-compatibility \
                           for more information."
            .to_string(),
        "$score" => "$score is not allowed in the current configuration. You may need to enable \
                     the corresponding feature flag"
            .to_string(),
        _ => return None,
    };
    Some(err(224, msg))
}

/// A fusion stage as the stages that run it, its input pipelines reading
/// `coll`.
fn fusion(name: &str, spec: &Bson, coll: &str) -> Result<Vec<Bson>, CommandError> {
    let Bson::Document(spec) = spec else {
        return Err(err(
            9,
            format!("{name} must take a nested object but found: {name}: {spec}"),
        ));
    };
    let rank = name == "$rankFusion";
    for key in spec.keys() {
        if !matches!(key.as_str(), "input" | "combination" | "scoreDetails") {
            return Err(unknown_field(name, key));
        }
    }
    let input = match spec.get("input") {
        None => return Err(missing_field(&format!("{name}.input"))),
        Some(Bson::Document(d)) => d,
        Some(other) => {
            return Err(err(
                14,
                format!(
                    "BSON field '{name}.input' is the wrong type '{}', expected type 'object'",
                    secantus_core::query::bson_type_name(other)
                ),
            ))
        }
    };
    for key in input.keys() {
        let known = key == "pipelines" || (!rank && key == "normalization");
        if !known {
            return Err(unknown_field(&format!("{name}.input"), key));
        }
    }
    let pipelines = input
        .get("pipelines")
        .ok_or_else(|| missing_field(&format!("{name}.input.pipelines")))?;
    let pipelines = input_pipelines(name, pipelines)?;
    let norm = if rank {
        None
    } else {
        Some(normalization(
            input.get("normalization"),
            &format!("{name}.input.normalization"),
            None,
        )?)
    };
    let combination = match spec.get("combination") {
        None => Document::new(),
        Some(Bson::Document(d)) => d.clone(),
        Some(other) => {
            return Err(err(
                14,
                format!(
                    "BSON field '{name}.combination' is the wrong type '{}', expected type \
                     'object'",
                    secantus_core::query::bson_type_name(other)
                ),
            ))
        }
    };
    for key in combination.keys() {
        let known = key == "weights" || (!rank && matches!(key.as_str(), "method" | "expression"));
        if !known {
            return Err(unknown_field(&format!("{name}.combination"), key));
        }
    }
    let method = match combination.get("method") {
        None => "average".to_string(),
        Some(Bson::String(m)) if m == "average" || m == "expression" => m.clone(),
        Some(other) => return Err(enumeration(other, &format!("{name}.combination.method"))),
    };
    let expression = combination.get("expression");
    if expression.is_some() != (method == "expression") {
        return Err(err(
            10017300,
            "combination.expression should only be specified when combination.method has the \
             value \"expression\"",
        ));
    }
    if expression.is_some() && combination.contains_key("weights") {
        return Err(err(
            10017301,
            "both combination.expression and combination.weights cannot be specified",
        ));
    }
    for (_, stages) in &pipelines {
        check_input(name, stages)?;
    }
    let names: Vec<String> = pipelines.iter().map(|(n, _)| n.clone()).collect();
    let (weights, _) = weights(name, &names, combination.get("weights"))?;
    let details = matches!(spec.get("scoreDetails"), Some(Bson::Boolean(true)));

    let mut out: Vec<Bson> = Vec::new();
    for (i, (_, stages)) in pipelines.iter().enumerate() {
        let mut p: Vec<Bson> = Vec::with_capacity(stages.len() + 1);
        for s in stages {
            if stage_name(s) == "$score" {
                let spec = s.as_document().and_then(|d| d.get("$score")).cloned();
                p.push(score_stage(&spec.unwrap_or(Bson::Null))?);
            } else {
                p.push(s.clone());
            }
        }
        let mut prep = doc! { "pipe": i as i32 };
        // The rank is the input's `$rank` over its sort key.
        if let Some(by) = stages
            .iter()
            .rev()
            .find_map(|s| s.as_document().and_then(|d| d.get("$sort")))
        {
            prep.insert("by", by.clone());
        }
        if let Some(n) = &norm {
            prep.insert("normalization", n.clone());
        }
        p.push(Bson::Document(doc! { "$_secantusFusePrep": prep }));
        if i == 0 {
            out.extend(p);
        } else {
            out.push(Bson::Document(
                doc! { "$unionWith": { "coll": coll, "pipeline": p } },
            ));
        }
    }
    let mut fuse = doc! {
        "kind": if rank { "rank" } else { "score" },
        "names": names,
        "weights": weights,
        "details": details,
    };
    if let Some(n) = norm {
        fuse.insert("normalization", n);
    }
    if let Some(e) = expression {
        fuse.insert("expression", e.clone());
        fuse.insert("text", expression_text(e));
    }
    out.push(Bson::Document(doc! { "$_secantusFuse": fuse }));
    Ok(out)
}

/// Whether `pipeline` has a stage this module desugars, or reads the score
/// metadata they set.
pub(crate) fn mentions_hybrid(pipeline: &[Bson]) -> bool {
    pipeline.iter().any(|s| {
        matches!(stage_name(s), "$score" | "$rankFusion" | "$scoreFusion")
            || mentions(s, "score")
            || mentions(s, "scoreDetails")
    })
}

/// Parse and desugar the hybrid stages of `pipeline`, an `aggregate` on
/// `coll`, and rewrite the score metadata reads.
pub(crate) fn prepare(pipeline: Vec<Bson>, coll: Option<&str>) -> Result<Vec<Bson>, CommandError> {
    let mut out: Vec<Bson> = Vec::with_capacity(pipeline.len());
    for (i, stage) in pipeline.into_iter().enumerate() {
        let name = stage_name(&stage).to_string();
        let spec = stage.as_document().and_then(|d| d.get(&name)).cloned();
        match name.as_str() {
            "$rankFusion" | "$scoreFusion" => {
                if let Some(e) = fcv_refusal(&name) {
                    return Err(e);
                }
                if i != 0 {
                    return Err(err(10170100, NOT_FIRST));
                }
                let coll = coll.ok_or_else(|| err(10170100, NOT_FIRST))?;
                out.extend(fusion(&name, &spec.unwrap_or(Bson::Null), coll)?);
            }
            "$score" => {
                if let Some(e) = fcv_refusal(&name) {
                    return Err(e);
                }
                out.push(score_stage(&spec.unwrap_or(Bson::Null))?);
            }
            _ => out.push(stage),
        }
    }
    rewrite_metadata(&mut out)?;
    Ok(out)
}

/// The stages that end the score metadata: they make documents of their
/// own (measured 8.2.11: `$lookup`, `$unwind`, `$replaceRoot` and the
/// projections keep it; `$group` reads it and drops it).
const ENDS: &[&str] = &[
    "$group",
    "$bucket",
    "$bucketAuto",
    "$sortByCount",
    "$count",
    "$facet",
    "$_secantusFusePrep",
];

/// The stages that make one new document from each, which keep the
/// metadata only when run through `$_secantusCarry`.
const REPLACES: &[&str] = &["$replaceRoot", "$replaceWith"];

fn mentions(v: &Bson, kind: &str) -> bool {
    match v {
        Bson::Document(d) => {
            if d.len() == 1 {
                if let Some(Bson::String(k)) = d.get("$meta") {
                    return k == kind;
                }
                if d.contains_key("$literal") || d.contains_key("$const") {
                    return false;
                }
            }
            d.values().any(|x| mentions(x, kind))
        }
        Bson::Array(items) => items.iter().any(|x| mentions(x, kind)),
        _ => false,
    }
}

fn unavailable(kind: &str) -> CommandError {
    err(
        40218,
        format!("query requires {kind} metadata, but it is not available"),
    )
}

/// `{$meta: "score"}` / `"scoreDetails"` into reads of the hidden fields,
/// from the stage that sets them to the stage the metadata ends at, where
/// the fields come off again. A read with no score in reach is mongod's
/// parse error, 40218.
fn rewrite_metadata(stages: &mut Vec<Bson>) -> Result<(), CommandError> {
    let any_set = stages
        .iter()
        .any(|s| matches!(stage_name(s), "$_secantusScore" | "$_secantusFuse"));
    if !any_set {
        for s in stages.iter() {
            if mentions(s, "score") {
                return Err(unavailable("score"));
            }
            if mentions(s, "scoreDetails") {
                return Err(unavailable("scoreDetails"));
            }
        }
        return Ok(());
    }
    let mut out: Vec<Bson> = Vec::with_capacity(stages.len() + 2);
    let (mut live, mut details_live) = (false, false);
    let hidden = || Bson::Array(vec![Bson::from(SCORE), Bson::from(SCORE_DETAILS)]);
    for mut stage in std::mem::take(stages) {
        let name = stage_name(&stage).to_string();
        let ends = ENDS.contains(&name.as_str());
        if mentions(&stage, "score") && !live {
            return Err(unavailable("score"));
        }
        if mentions(&stage, "scoreDetails") && !details_live {
            return Err(unavailable("scoreDetails"));
        }
        if live {
            crate::agg_meta::replace(&mut stage, "score", SCORE);
            crate::agg_meta::replace(&mut stage, "scoreDetails", SCORE_DETAILS);
        }
        // `$sort: {f: {$meta: "score"}}` -- now `{f: "$<SCORE>"}` -- sorts by
        // the score, highest first.
        if name == "$sort" {
            if let Some(Bson::Document(spec)) =
                stage.as_document_mut().and_then(|d| d.get_mut("$sort"))
            {
                let read = Bson::String(format!("${SCORE}"));
                let rewritten: Document = spec
                    .iter()
                    .map(|(k, v)| {
                        if *v == read {
                            (SCORE.to_string(), Bson::Int32(-1))
                        } else {
                            (k.clone(), v.clone())
                        }
                    })
                    .collect();
                *spec = rewritten;
            }
        }
        if live && ends {
            // `$group` reads the metadata and drops it with the documents.
            live = false;
            details_live = false;
        } else if live && REPLACES.contains(&name.as_str()) {
            stage = Bson::Document(doc! { "$_secantusCarry": { "stage": stage } });
        } else if live && name == "$project" {
            if let Some(Bson::Document(spec)) =
                stage.as_document_mut().and_then(|d| d.get_mut("$project"))
            {
                let inclusion = spec.iter().any(|(k, v)| {
                    k != "_id"
                        && !matches!(v, Bson::Int32(0) | Bson::Int64(0) | Bson::Boolean(false))
                        && !matches!(v, Bson::Double(d) if *d == 0.0)
                });
                if inclusion {
                    spec.insert(SCORE, 1);
                    spec.insert(SCORE_DETAILS, 1);
                }
            }
        }
        let sets = |key: &str| {
            stage
                .as_document()
                .and_then(|d| d.get_document(key).ok())
                .map(|s| s.get_bool("details").unwrap_or(false))
        };
        if let Some(details) = sets("$_secantusScore") {
            live = true;
            details_live |= details;
        } else if let Some(details) = sets("$_secantusFuse") {
            live = true;
            details_live = details;
        }
        out.push(stage);
    }
    if live {
        // Off before a closing `$out` / `$merge`, which would store them.
        let at = match out.last().map(stage_name) {
            Some("$out" | "$merge") => out.len() - 1,
            _ => out.len(),
        };
        out.insert(at, Bson::Document(doc! { "$unset": hidden() }));
    }
    *stages = out;
    Ok(())
}
