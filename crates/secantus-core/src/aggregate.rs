//! Aggregation pipeline — Rust port of `secantus.aggregate.apply_pipeline`
//! (Phase 2). Operates on an in-memory list of documents behind a list-of-docs
//! byte seam, reusing the already-ported leaf engines (`query::matches`,
//! `expressions::evaluate`, the `paths` helpers) so a pure pipeline runs end to
//! end in Rust without re-entering Python per stage / per doc.
//!
//! Graceful whole-pipeline fallback: if any stage isn't ported (the storage-
//! backed `$lookup`/`$graphLookup`/`$geoNear`/`$out`/`$merge`, non-deterministic
//! `$sample`, or date-unit `$densify`) or any inner expression defers, the
//! entire `apply_pipeline` returns `Fallback` and the pure-Python pipeline runs
//! instead. Every pipeline stage that doesn't touch `Storage` is now ported.
//!
//! Handled stages: `$match`, `$limit`, `$skip`, `$count`, `$project`,
//! `$addFields`/`$set`, `$unset`, `$replaceRoot`/`$replaceWith`, `$sort`
//! (cross-type BSON ordering via `order::cmp`, deferring Decimal128 / exotic
//! sort keys), `$unwind`, `$group`/`$sortByCount`/`$bucket` (see `group.rs` —
//! defers on the cross-type key / accumulator cases Python can't reproduce
//! without raising), `$facet` (recursive sub-pipelines), and `$densify` (numeric
//! path; date-unit densify defers — see `densify.rs`).

use bson::{Bson, Document};

use std::cmp::Ordering;

use crate::collation::Collation;
use crate::numeric::{as_int_like, int_to_bson};
use crate::{densify, expressions, group, order, paths, query};

#[derive(Debug)]
pub struct Fallback;

type R<T> = Result<T, Fallback>;

fn evaluate(expr: &Bson, doc: &Document, vars: &Document) -> R<Bson> {
    expressions::evaluate(doc, expr, vars).map_err(|_| Fallback)
}

pub fn apply_pipeline(
    mut docs: Vec<Document>,
    pipeline: &[Bson],
    vars: &Document,
    coll: Option<&Collation>,
) -> R<Vec<Document>> {
    for stage in pipeline {
        let Bson::Document(s) = stage else {
            return Err(Fallback);
        };
        if s.len() != 1 {
            return Err(Fallback); // Python raises; defer so it raises there
        }
        let (name, spec) = s.iter().next().unwrap();
        docs = apply_stage(name, spec, docs, vars, coll)?;
    }
    Ok(docs)
}

fn apply_stage(
    name: &str,
    spec: &Bson,
    mut docs: Vec<Document>,
    vars: &Document,
    coll: Option<&Collation>,
) -> R<Vec<Document>> {
    match name {
        "$match" => {
            let Bson::Document(q) = spec else {
                return Err(Fallback);
            };
            let mut out = Vec::new();
            for d in docs {
                if query::matches(&d, q, vars, coll).map_err(|_| Fallback)? {
                    out.push(d);
                }
            }
            Ok(out)
        }
        "$limit" => {
            let n = positive_int(spec)?;
            docs.truncate(n);
            Ok(docs)
        }
        "$skip" => {
            let n = positive_int(spec)?;
            Ok(if n >= docs.len() {
                Vec::new()
            } else {
                docs.split_off(n)
            })
        }
        "$count" => {
            let Bson::String(field) = spec else {
                return Err(Fallback); // Python raises on non-string
            };
            let mut out = Document::new();
            out.insert(
                field.clone(),
                int_to_bson(docs.len() as i128).ok_or(Fallback)?,
            );
            Ok(vec![out])
        }
        "$project" => map_docs(docs, |d| project_one(d, spec_doc(spec)?, vars)),
        "$addFields" | "$set" => map_docs(docs, |d| add_fields_one(d, spec_doc(spec)?, vars)),
        "$unset" => {
            let paths_list = unset_paths(spec)?;
            map_docs(docs, |d| {
                let mut new = d.clone();
                for p in &paths_list {
                    paths::unset_path(&mut new, p);
                }
                Ok(new)
            })
        }
        "$replaceRoot" => {
            let Bson::Document(s) = spec else {
                return Err(Fallback);
            };
            let Some(new_root) = s.get("newRoot") else {
                return Err(Fallback);
            };
            map_docs(docs, |d| replace_root_one(d, new_root, vars))
        }
        "$replaceWith" => map_docs(docs, |d| replace_root_one(d, spec, vars)),
        "$sort" => sort_stage(docs, spec),
        "$unwind" => unwind_stage(docs, spec),
        "$group" => group::group_stage(spec, &docs, vars).map_err(|_| Fallback),
        "$sortByCount" => group::sort_by_count_stage(spec, &docs, vars).map_err(|_| Fallback),
        "$bucket" => group::bucket_stage(spec, &docs, vars).map_err(|_| Fallback),
        "$facet" => facet_stage(spec, docs, vars, coll),
        "$densify" => densify::densify_stage(spec, &docs).map_err(|_| Fallback),
        // storage-backed ($lookup/$geoNear/$out/$merge) / $sample / … -> Python.
        _ => Err(Fallback),
    }
}

/// `$facet` — run each named sub-pipeline over a copy of the same input docs and
/// collect the results into one output doc. Defers if any sub-pipeline defers.
fn facet_stage(
    spec: &Bson,
    docs: Vec<Document>,
    vars: &Document,
    coll: Option<&Collation>,
) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(Fallback);
    };
    let mut out = Document::new();
    for (name, sub) in s {
        let Bson::Array(sub_pipeline) = sub else {
            return Err(Fallback); // Python raises (entry must be a pipeline array)
        };
        let result = apply_pipeline(docs.clone(), sub_pipeline, vars, coll)?;
        out.insert(
            name.clone(),
            Bson::Array(result.into_iter().map(Bson::Document).collect()),
        );
    }
    Ok(vec![out])
}

// --- $sort (mirrors storage.sort_docs / _SortKey) -----------------------

/// `(path, reverse)` pairs from a sort spec. Each direction must be `1`/`-1`
/// (ints or bools coerced as Python's `int(d)`); anything else (textScore meta,
/// floats, ...) defers.
fn sort_fields(spec: &Bson) -> R<Vec<(String, bool)>> {
    let Bson::Document(d) = spec else {
        return Err(Fallback);
    };
    if d.is_empty() {
        return Err(Fallback); // empty spec is a no-op in Python; let it handle it
    }
    let mut out = Vec::with_capacity(d.len());
    for (field, dir) in d {
        let n = as_int_like(dir).ok_or(Fallback)?;
        match n {
            1 => out.push((field.clone(), false)),
            -1 => out.push((field.clone(), true)),
            _ => return Err(Fallback),
        }
    }
    Ok(out)
}

fn sort_stage(docs: Vec<Document>, spec: &Bson) -> R<Vec<Document>> {
    let fields = sort_fields(spec)?;
    // Materialise each doc's sort-key values once (get_path per field), and
    // bail to Python if any key holds a value `order` won't faithfully order
    // (Decimal128 / exotic types) — keeps the comparator total.
    let mut keyed: Vec<(Vec<Bson>, Document)> = Vec::with_capacity(docs.len());
    for d in docs {
        let mut keys = Vec::with_capacity(fields.len());
        for (path, _) in &fields {
            let v = paths::get_path(&d, path).cloned().unwrap_or(Bson::Null);
            if !order::is_sortable(&v) {
                return Err(Fallback);
            }
            keys.push(v);
        }
        keyed.push((keys, d));
    }
    // Stable sort matching Python's Timsort over the tuple of directed keys.
    keyed.sort_by(|(ka, _), (kb, _)| {
        for (i, (_, rev)) in fields.iter().enumerate() {
            let c = order::cmp(&ka[i], &kb[i]);
            let c = if *rev { c.reverse() } else { c };
            if c != Ordering::Equal {
                return c;
            }
        }
        Ordering::Equal
    });
    Ok(keyed.into_iter().map(|(_, d)| d).collect())
}

// --- $unwind (mirrors aggregate._stage_unwind) --------------------------

fn unwind_stage(docs: Vec<Document>, spec: &Bson) -> R<Vec<Document>> {
    let (path, preserve_null, include_index) = match spec {
        Bson::String(s) => (s.trim_start_matches('$').to_string(), false, None),
        Bson::Document(d) => {
            let Some(Bson::String(raw)) = d.get("path") else {
                return Err(Fallback); // non-string path -> Python raises
            };
            let preserve = match d.get("preserveNullAndEmptyArrays") {
                None => false,
                Some(Bson::Boolean(b)) => *b,
                // Python's `bool(...)` of arbitrary values -> defer the odd cases.
                _ => return Err(Fallback),
            };
            let include = match d.get("includeArrayIndex") {
                None | Some(Bson::Null) => None,
                Some(Bson::String(s)) => Some(s.clone()),
                _ => return Err(Fallback),
            };
            (raw.trim_start_matches('$').to_string(), preserve, include)
        }
        _ => return Err(Fallback),
    };

    let mut out: Vec<Document> = Vec::new();
    for doc in &docs {
        match paths::get_path(doc, &path) {
            Some(Bson::Array(arr)) => {
                if arr.is_empty() {
                    if preserve_null {
                        let mut new = doc.clone();
                        paths::unset_path(&mut new, &path);
                        if let Some(idx) = &include_index {
                            new.insert(idx.clone(), Bson::Null);
                        }
                        out.push(new);
                    }
                    continue;
                }
                let arr = arr.clone();
                for (i, elem) in arr.into_iter().enumerate() {
                    let mut new = doc.clone();
                    paths::set_path(&mut new, &path, elem).map_err(|_| Fallback)?;
                    if let Some(idx) = &include_index {
                        new.insert(idx.clone(), int_to_bson(i as i128).ok_or(Fallback)?);
                    }
                    out.push(new);
                }
            }
            None | Some(Bson::Null) => {
                if preserve_null {
                    let mut new = doc.clone();
                    if let Some(idx) = &include_index {
                        new.insert(idx.clone(), Bson::Null);
                    }
                    out.push(new);
                }
            }
            Some(_) => {
                let mut new = doc.clone();
                if let Some(idx) = &include_index {
                    new.insert(idx.clone(), Bson::Null);
                }
                out.push(new);
            }
        }
    }
    Ok(out)
}

fn map_docs(docs: Vec<Document>, mut f: impl FnMut(&Document) -> R<Document>) -> R<Vec<Document>> {
    let mut out = Vec::with_capacity(docs.len());
    for d in &docs {
        out.push(f(d)?);
    }
    Ok(out)
}

fn spec_doc(spec: &Bson) -> R<&Document> {
    match spec {
        Bson::Document(d) => Ok(d),
        _ => Err(Fallback),
    }
}

/// `int(spec)` for $limit/$skip, restricted to non-negative integers (negative
/// values hit Python's slice semantics — deferred; floats also defer).
fn positive_int(spec: &Bson) -> R<usize> {
    match as_int_like(spec) {
        Some(n) if n >= 0 => Ok(n as usize),
        _ => Err(Fallback),
    }
}

fn unset_paths(spec: &Bson) -> R<Vec<String>> {
    match spec {
        Bson::String(s) => Ok(vec![s.clone()]),
        Bson::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for v in a {
                match v {
                    Bson::String(s) => out.push(s.clone()),
                    _ => return Err(Fallback),
                }
            }
            Ok(out)
        }
        _ => Err(Fallback),
    }
}

fn add_fields_one(doc: &Document, spec: &Document, vars: &Document) -> R<Document> {
    let mut result = doc.clone();
    for (path, expr) in spec {
        // Each field is evaluated against the ORIGINAL doc (matching Python).
        let v = evaluate(expr, doc, vars)?;
        paths::set_path(&mut result, path, v).map_err(|_| Fallback)?;
    }
    Ok(result)
}

fn replace_root_one(doc: &Document, expr: &Bson, vars: &Document) -> R<Document> {
    match evaluate(expr, doc, vars)? {
        Bson::Document(d) => Ok(d),
        _ => Err(Fallback), // Python raises if newRoot isn't a document
    }
}

// --- $project (mirrors aggregate._project_one) --------------------------

fn is_one(v: &Bson) -> bool {
    matches!(v, Bson::Int32(1) | Bson::Int64(1) | Bson::Boolean(true))
        || matches!(v, Bson::Double(d) if *d == 1.0)
}

fn is_zero(v: &Bson) -> bool {
    matches!(v, Bson::Int32(0) | Bson::Int64(0) | Bson::Boolean(false))
        || matches!(v, Bson::Double(d) if *d == 0.0)
}

/// Mapping-only path presence (matches `_path_present`, which does NOT walk
/// into arrays — unlike `paths::get_path`).
fn path_present(doc: &Document, path: &str) -> bool {
    let mut cur = doc;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        match cur.get(part) {
            Some(Bson::Document(d)) => cur = d,
            Some(_) => return parts.peek().is_none(),
            None => return false,
        }
    }
    true
}

fn project_one(doc: &Document, spec: &Document, vars: &Document) -> R<Document> {
    let mut inclusions: Vec<&str> = Vec::new();
    let mut exclusions: Vec<&str> = Vec::new();
    let mut computed: Vec<(&str, &Bson)> = Vec::new();
    let mut id_handling: Option<i32> = None;

    for (key, value) in spec {
        if key == "_id" {
            if is_zero(value) {
                id_handling = Some(0);
            } else if is_one(value) {
                id_handling = Some(1);
            } else {
                computed.push(("_id", value));
                id_handling = Some(1);
            }
            continue;
        }
        if is_one(value) {
            inclusions.push(key);
        } else if is_zero(value) {
            exclusions.push(key);
        } else {
            computed.push((key, value));
        }
    }

    let has_inclusion = !inclusions.is_empty() || !computed.is_empty();
    let has_exclusion = !exclusions.is_empty();
    if has_inclusion && has_exclusion {
        return Err(Fallback); // Python raises (mix of inclusion/exclusion)
    }

    if has_inclusion {
        let mut result = Document::new();
        if id_handling != Some(0) {
            if let Some(id) = doc.get("_id") {
                result.insert("_id".to_string(), id.clone());
            }
        }
        for path in inclusions {
            let gp = paths::get_path(doc, path);
            let present =
                matches!(gp, Some(v) if !matches!(v, Bson::Null)) || path_present(doc, path);
            if present {
                let v = gp.cloned().unwrap_or(Bson::Null);
                paths::set_path(&mut result, path, v).map_err(|_| Fallback)?;
            }
        }
        for (key, expr) in computed {
            let v = evaluate(expr, doc, vars)?;
            paths::set_path(&mut result, key, v).map_err(|_| Fallback)?;
        }
        return Ok(result);
    }

    let mut result = doc.clone();
    for path in exclusions {
        paths::unset_path(&mut result, path);
    }
    if id_handling == Some(0) {
        result.remove("_id");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn run(docs: Vec<Document>, pipeline: Vec<Bson>) -> Vec<Document> {
        apply_pipeline(docs, &pipeline, &Document::new(), None).expect("should not fall back")
    }

    #[test]
    fn match_limit_skip_count() {
        let docs = vec![doc! {"a": 1}, doc! {"a": 2}, doc! {"a": 3}, doc! {"a": 4}];
        assert_eq!(
            run(
                docs.clone(),
                vec![bson::bson!({"$match": {"a": {"$gt": 1}}})]
            ),
            vec![doc! {"a": 2}, doc! {"a": 3}, doc! {"a": 4}]
        );
        assert_eq!(run(docs.clone(), vec![bson::bson!({"$limit": 2})]).len(), 2);
        assert_eq!(
            run(docs.clone(), vec![bson::bson!({"$skip": 3})]),
            vec![doc! {"a": 4}]
        );
        assert_eq!(
            run(docs, vec![bson::bson!({"$count": "n"})]),
            vec![doc! {"n": 4i32}]
        );
    }

    #[test]
    fn project_add_unset() {
        assert_eq!(
            run(
                vec![doc! {"_id": 1, "a": 2, "b": 3}],
                vec![bson::bson!({"$project": {"a": 1}})]
            ),
            vec![doc! {"_id": 1, "a": 2}]
        );
        assert_eq!(
            run(
                vec![doc! {"a": 5}],
                vec![bson::bson!({"$addFields": {"b": {"$add": ["$a", 1]}}})]
            ),
            vec![doc! {"a": 5, "b": 6}]
        );
        assert_eq!(
            run(
                vec![doc! {"a": 1, "b": 2}],
                vec![bson::bson!({"$unset": "b"})]
            ),
            vec![doc! {"a": 1}]
        );
    }

    #[test]
    fn replace_root_and_chain() {
        assert_eq!(
            run(
                vec![doc! {"x": {"y": 1}}],
                vec![bson::bson!({"$replaceRoot": {"newRoot": "$x"}})]
            ),
            vec![doc! {"y": 1}]
        );
        // chained: $match then $project
        let out = run(
            vec![doc! {"_id": 1, "v": 10}, doc! {"_id": 2, "v": 20}],
            vec![
                bson::bson!({"$match": {"v": {"$gte": 15}}}),
                bson::bson!({"$project": {"v": 1, "_id": 0}}),
            ],
        );
        assert_eq!(out, vec![doc! {"v": 20}]);
    }

    #[test]
    fn unported_stage_defers() {
        // $sample is non-deterministic -> always defers to Python.
        assert!(apply_pipeline(
            vec![doc! {"a": 1}],
            &[bson::bson!({"$sample": {"size": 1}})],
            &Document::new(),
            None
        )
        .is_err());
    }

    #[test]
    fn sort_cross_type_and_direction() {
        let docs = vec![doc! {"a": 3}, doc! {"a": 1}, doc! {"a": 2}];
        assert_eq!(
            run(docs.clone(), vec![bson::bson!({"$sort": {"a": 1}})]),
            vec![doc! {"a": 1}, doc! {"a": 2}, doc! {"a": 3}]
        );
        assert_eq!(
            run(docs, vec![bson::bson!({"$sort": {"a": -1}})]),
            vec![doc! {"a": 3}, doc! {"a": 2}, doc! {"a": 1}]
        );
    }

    #[test]
    fn unwind_basic_and_index() {
        assert_eq!(
            run(
                vec![doc! {"_id": 1, "t": [10, 20]}],
                vec![bson::bson!({"$unwind": "$t"})]
            ),
            vec![doc! {"_id": 1, "t": 10}, doc! {"_id": 1, "t": 20}]
        );
        // empty array drops the doc unless preserveNullAndEmptyArrays
        assert_eq!(
            run(
                vec![doc! {"_id": 1, "t": []}],
                vec![bson::bson!({"$unwind": "$t"})]
            ),
            Vec::<Document>::new()
        );
        assert_eq!(
            run(
                vec![doc! {"_id": 1, "t": [9]}],
                vec![bson::bson!({"$unwind": {"path": "$t", "includeArrayIndex": "i"}})]
            ),
            vec![doc! {"_id": 1, "t": 9, "i": 0i32}]
        );
    }
}
