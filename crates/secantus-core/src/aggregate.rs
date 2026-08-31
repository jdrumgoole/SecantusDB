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
use crate::{densify, expressions, fill, group, order, paths, query, windowfields};

#[derive(Debug)]
pub struct Fallback;

type R<T> = Result<T, Fallback>;

fn evaluate(expr: &Bson, doc: &Document, vars: &Document) -> R<Bson> {
    expressions::evaluate(doc, expr, vars).map_err(|_| Fallback)
}

/// [`evaluate`] in *field-value* position: an absent field path yields the
/// missing marker so `$project` / `$addFields` omit the key instead of writing
/// null.
fn evaluate_or_missing(expr: &Bson, doc: &Document, vars: &Document) -> R<Bson> {
    expressions::evaluate_or_missing(doc, expr, vars).map_err(|_| Fallback)
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
            let n = stage_nonneg_int(spec)?;
            if n == 0 {
                return Err(Fallback); // Python raises 15958 "the limit must be positive"
            }
            docs.truncate(n);
            Ok(docs)
        }
        "$skip" => {
            let n = stage_nonneg_int(spec)?;
            Ok(if n >= docs.len() {
                Vec::new()
            } else {
                docs.split_off(n)
            })
        }
        "$count" => {
            let Bson::String(field) = spec else {
                return Err(Fallback); // Python raises on non-string (40156)
            };
            // empty (40157) / $-prefixed (40158) / dotted (40160) / "_id" (15948).
            if field.is_empty() || field.starts_with('$') || field.contains('.') || field == "_id" {
                return Err(Fallback);
            }
            let mut out = Document::new();
            out.insert(
                field.clone(),
                int_to_bson(docs.len() as i128).ok_or(Fallback)?,
            );
            Ok(vec![out])
        }
        "$project" => {
            let sd = spec_doc(spec)?;
            if sd.is_empty() {
                return Err(Fallback); // Python raises 51272 (needs >= 1 field)
            }
            map_docs(docs, |d| project_one(&d, sd, vars))
        }
        "$addFields" | "$set" => map_docs(docs, |d| add_fields_one(d, spec_doc(spec)?, vars)),
        "$unset" => {
            let paths_list = unset_paths(spec)?;
            map_docs(docs, |mut d| {
                for p in &paths_list {
                    paths::unset_path(&mut d, p);
                }
                Ok(d)
            })
        }
        "$replaceRoot" => {
            let Bson::Document(s) = spec else {
                return Err(Fallback);
            };
            let Some(new_root) = s.get("newRoot") else {
                return Err(Fallback);
            };
            map_docs(docs, |d| replace_root_one(&d, new_root, vars))
        }
        "$replaceWith" => map_docs(docs, |d| replace_root_one(&d, spec, vars)),
        "$sort" => sort_stage(docs, spec),
        "$unwind" => unwind_stage(docs, spec),
        "$group" => group::group_stage(spec, &docs, vars).map_err(|_| Fallback),
        "$sortByCount" => group::sort_by_count_stage(spec, &docs, vars).map_err(|_| Fallback),
        "$bucket" => group::bucket_stage(spec, &docs, vars).map_err(|_| Fallback),
        "$bucketAuto" => group::bucket_auto_stage(spec, &docs, vars).map_err(|_| Fallback),
        "$redact" => redact_stage(spec, docs, vars),
        "$facet" => facet_stage(spec, docs, vars, coll),
        "$densify" => densify::densify_stage(spec, &docs).map_err(|_| Fallback),
        "$fill" => fill::fill_stage(spec, docs, vars).map_err(|_| Fallback),
        "$setWindowFields" => {
            windowfields::set_window_fields_stage(spec, docs, vars).map_err(|_| Fallback)
        }
        // storage-backed ($lookup/$geoNear/$out/$merge) / $sample / … -> Python.
        _ => Err(Fallback),
    }
}

/// `$facet` — run each named sub-pipeline over a copy of the same input docs and
/// collect the results into one output doc. Defers if any sub-pipeline defers.
/// `$redact` — content-based, recursive document / sub-document pruning. The
/// expression is evaluated against each (sub-)document and must return one of the
/// sentinels `$$KEEP` (include as-is, no recursion), `$$PRUNE` (drop it), or
/// `$$DESCEND` (recurse into nested docs / arrays-of-docs). Mirrors
/// `aggregate._stage_redact`; a missing/empty expression or a non-sentinel result
/// defers (Python raises).
fn redact_stage(spec: &Bson, docs: Vec<Document>, vars: &Document) -> R<Vec<Document>> {
    // No empty-spec special case: mongod evaluates `{}` (and `null`) like any
    // other expression and reports the RESULT as a non-sentinel, not the spec
    // as missing. Probed on 8.2.11.
    let vars = redact_vars(vars);
    let mut out = Vec::with_capacity(docs.len());
    for doc in docs {
        if let Some(r) = redact_subdoc(&doc, spec, &vars)? {
            out.push(r);
        }
    }
    Ok(out)
}

/// `$redact`'s decision markers, bound only for the duration of its own
/// evaluation — which is what mongod does. Outside `$redact` these names are
/// undefined variables (17276), so they cannot leak into user output, and the
/// stage dispatches on the marker rather than on the string `"$$KEEP"`, so a
/// stored string can no longer impersonate a decision.
///
/// Comparison is by VALUE here (vars live in a BSON document), where the Python
/// port compares by identity. The payloads are deliberately distinctive, so a
/// stored Binary is the only collision — the same assumption mongod makes about
/// its internal constants. Keep the payloads identical across the two ports.
fn redact_marker(name: &str) -> Bson {
    Bson::Binary(bson::Binary {
        subtype: bson::spec::BinarySubtype::UserDefined(0x80),
        bytes: format!("$redact.{name}").into_bytes(),
    })
}

fn redact_vars(vars: &Document) -> Document {
    let mut v = vars.clone();
    for name in ["KEEP", "PRUNE", "DESCEND"] {
        v.insert(name, redact_marker(name));
    }
    v
}

/// The decision a `$redact` expression produced, or the offending value.
enum Decision {
    Keep,
    Prune,
    Descend,
    Other(Bson),
}

fn redact_decide(doc: &Document, spec: &Bson, vars: &Document) -> R<Decision> {
    let v = evaluate(spec, doc, vars)?;
    Ok(if v == redact_marker("KEEP") {
        Decision::Keep
    } else if v == redact_marker("PRUNE") {
        Decision::Prune
    } else if v == redact_marker("DESCEND") {
        Decision::Descend
    } else {
        Decision::Other(v)
    })
}

fn redact_subdoc(doc: &Document, spec: &Bson, vars: &Document) -> R<Option<Document>> {
    match redact_decide(doc, spec, vars)? {
        Decision::Keep => Ok(Some(doc.clone())),
        Decision::Prune => Ok(None),
        Decision::Descend => Ok(Some(redact_descend(doc, spec, vars)?)),
        // Nameable at the command layer as mongod's 17053 — see
        // [`redact_runtime_error`], which recovers the offending value.
        Decision::Other(_) => Err(Fallback),
    }
}

fn redact_descend(doc: &Document, spec: &Bson, vars: &Document) -> R<Document> {
    let mut out = Document::new();
    for (k, v) in doc {
        match v {
            Bson::Document(sub) => {
                if let Some(r) = redact_subdoc(sub, spec, vars)? {
                    out.insert(k, r);
                }
            }
            Bson::Array(arr) => {
                out.insert(k, Bson::Array(redact_list(arr, spec, vars)?));
            }
            other => {
                out.insert(k, other.clone());
            }
        }
    }
    Ok(out)
}

/// Redact one array, recursing into NESTED arrays as mongod does.
///
/// This walked only the top level, so a sub-document one array deeper —
/// `[[{lvl: 9}]]` — was passed through untouched and returned to the caller.
/// mongod prunes it and leaves the inner array in place (`[[]]`).
fn redact_list(values: &[Bson], spec: &Bson, vars: &Document) -> R<Vec<Bson>> {
    let mut out = Vec::with_capacity(values.len());
    for elem in values {
        match elem {
            Bson::Document(sub) => {
                if let Some(r) = redact_subdoc(sub, spec, vars)? {
                    out.push(Bson::Document(r));
                }
            }
            Bson::Array(inner) => out.push(Bson::Array(redact_list(inner, spec, vars)?)),
            other => out.push(other.clone()),
        }
    }
    Ok(out)
}

/// mongod's `Value::toString` — the rendering `$redact`'s 17053 uses.
///
/// NOT the shell form `argtypes::render_stage_value` produces: no inner spaces
/// in containers (`{k: 1}` / `[1, "a"]`), an ObjectId bare rather than
/// `ObjectId('…')`, and a date as ISO-8601 rather than `new Date(<ms>)`. Two
/// renderers, both mongod's, used by different messages — probed on 8.2.11.
pub fn render_value_compact(v: &Bson) -> String {
    match v {
        Bson::Null => "null".to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::String(s) => format!("\"{s}\""),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(d) => {
            if d.fract() == 0.0 && d.is_finite() {
                format!("{}", *d as i64)
            } else {
                d.to_string()
            }
        }
        Bson::Decimal128(d) => d.to_string(),
        Bson::ObjectId(oid) => oid.to_hex(),
        // mongod always renders exactly three fractional digits and a `Z`.
        // RFC-3339 formatting omits the fraction when it is zero, which showed
        // up as `2026-01-02T03:04:05Z` against mongod's `...05.000Z`.
        Bson::DateTime(d) => {
            let ms = d.timestamp_millis();
            let secs = ms.div_euclid(1000);
            let frac = ms.rem_euclid(1000);
            match bson::DateTime::from_millis(secs * 1000).try_to_rfc3339_string() {
                Ok(base) => {
                    let stem = base
                        .split_once('.')
                        .map(|(head, _)| head.to_string())
                        .unwrap_or_else(|| base.trim_end_matches('Z').replace("+00:00", ""));
                    format!("{stem}.{frac:03}Z")
                }
                Err(_) => format!("{d:?}"),
            }
        }
        Bson::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(render_value_compact)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Bson::Document(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, v)| format!("{k}: {}", render_value_compact(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => format!("{other:?}"),
    }
}

/// Name a `$redact` failure that the engine could only signal as `Fallback`.
///
/// The `update::arith_type_error` template: a standalone validator that names
/// the errors it *can* name, leaving `Fallback` for genuinely unimplemented
/// constructs. It exists because `Fallback` is a unit struct carrying no code —
/// widening it would touch 34 construction sites in this file plus the PyO3
/// boundary, and on the Python server `Fallback` legitimately means "the pure
/// engine runs instead". On a server with no Python it became a generic
/// `BadValue`, which is what `$redact`'s 17053 surfaced as.
///
/// Re-runs the stages preceding each `$redact` so the decision is evaluated
/// against the documents that stage would actually have seen, then returns
/// mongod's `(code, errmsg)` for the first non-sentinel result.
pub fn redact_runtime_error(
    docs: &[Document],
    stages: &[Bson],
    vars: &Document,
    collation: Option<&Collation>,
) -> Option<(i32, String)> {
    for (i, stage) in stages.iter().enumerate() {
        let Bson::Document(d) = stage else { continue };
        let Some(spec) = d.get("$redact") else {
            continue;
        };
        let input = apply_pipeline(docs.to_vec(), &stages[..i], vars, collation).ok()?;
        let rvars = redact_vars(vars);
        for doc in &input {
            if let Ok(Decision::Other(v)) = redact_decide(doc, spec, &rvars) {
                return Some((
                    17053,
                    format!(
                        "$redact's expression should not return anything aside from the \
                         variables $$KEEP, $$DESCEND, and $$PRUNE, but returned {}",
                        render_value_compact(&v)
                    ),
                ));
            }
        }
    }
    None
}

fn facet_stage(
    spec: &Bson,
    mut docs: Vec<Document>,
    vars: &Document,
    coll: Option<&Collation>,
) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(Fallback);
    };
    if s.is_empty() {
        return Err(Fallback); // Python raises 40169 (must be a non-empty object)
    }
    let n = s.len();
    let mut out = Document::new();
    for (i, (name, sub)) in s.iter().enumerate() {
        let Bson::Array(sub_pipeline) = sub else {
            return Err(Fallback); // Python raises 40170 (entry must be an array)
        };
        // Each stage must be a non-empty object and not a nested $facet — else
        // defer so Python raises 40171 / 40600.
        for stage in sub_pipeline {
            match stage {
                Bson::Document(d) if !d.is_empty() && !d.contains_key("$facet") => {}
                _ => return Err(Fallback),
            }
        }
        // The last sub-pipeline can consume the input docs directly; earlier
        // ones each need their own copy.
        let input = if i + 1 == n {
            std::mem::take(&mut docs)
        } else {
            docs.clone()
        };
        let result = apply_pipeline(input, sub_pipeline, vars, coll)?;
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
        return Err(Fallback); // Python raises 15976 (must have at least one key)
    }
    let mut out = Vec::with_capacity(d.len());
    for (field, dir) in d {
        // mongod accepts int/long ±1 or a whole double ±1.0. A bool is "Illegal
        // key" (15974), not 1/-1 (so don't let as_int_like coerce true -> 1); a
        // numeric non-±1 defers so Python raises 15975; non-numeric -> 15974.
        let n = match dir {
            Bson::Boolean(_) => None,
            Bson::Int32(_) | Bson::Int64(_) => as_int_like(dir),
            Bson::Double(f) if f.fract() == 0.0 => Some(*f as i128),
            _ => None,
        };
        match n {
            Some(1) => out.push((field.clone(), false)),
            Some(-1) => out.push((field.clone(), true)),
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
        for (path, rev) in &fields {
            let v = paths::get_path(&d, path).cloned().unwrap_or(Bson::Null);
            if !order::is_sortable(&v) {
                return Err(Fallback);
            }
            // mongod sorts an array-valued field by one representative element:
            // its minimum ascending, its maximum descending.
            keys.push(order::array_sort_value(v, *rev).ok_or(Fallback)?);
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
        Bson::String(s) => {
            if !s.starts_with('$') {
                return Err(Fallback); // bare path -> Python raises 28818
            }
            (s.trim_start_matches('$').to_string(), false, None)
        }
        Bson::Document(d) => {
            let Some(Bson::String(raw)) = d.get("path") else {
                return Err(Fallback); // non-string path -> Python raises 28808
            };
            if !raw.starts_with('$') {
                return Err(Fallback); // bare path -> Python raises 28818
            }
            let preserve = match d.get("preserveNullAndEmptyArrays") {
                None => false,
                Some(Bson::Boolean(b)) => *b,
                _ => return Err(Fallback), // non-bool -> Python raises 28809
            };
            let include = match d.get("includeArrayIndex") {
                None | Some(Bson::Null) => None,
                // A non-empty, non-`$`-prefixed string; else defer (28810 / 28822).
                Some(Bson::String(s)) if !s.is_empty() && !s.starts_with('$') => Some(s.clone()),
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
                // Iterate the array in place and clone elements one at a time —
                // cloning the whole array up front (then discarding it as each
                // doc's copy overwrites the field) doubled the per-element work.
                for (i, elem) in arr.iter().enumerate() {
                    let mut new = doc.clone();
                    paths::set_path(&mut new, &path, elem.clone()).map_err(|_| Fallback)?;
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

/// Map each input doc through `f`, passing it **by value** so stages that only
/// add/remove fields can mutate the doc in place instead of cloning it.
fn map_docs(docs: Vec<Document>, mut f: impl FnMut(Document) -> R<Document>) -> R<Vec<Document>> {
    let mut out = Vec::with_capacity(docs.len());
    for d in docs {
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
/// A `$limit` / `$skip` argument, mongod-style: an int or a whole-number double
/// is the count; a bool / non-number, a fractional double, or a negative value
/// defers to the Python oracle (which raises mongod's 5107201 / 5107200). `$limit`
/// additionally rejects zero at the call site (15958).
fn stage_nonneg_int(spec: &Bson) -> R<usize> {
    let n = match spec {
        Bson::Int32(n) => *n as i64,
        Bson::Int64(n) => *n,
        Bson::Double(d) if d.is_finite() && d.fract() == 0.0 => *d as i64,
        // A decimal IS a number here: mongod runs `$skip: Decimal128("2")`
        // (probed 6.0.16), and the pure engine accepts it too, so deferring
        // would answer a generic BadValue on the Rust server where mongod
        // answers rows.
        Bson::Decimal128(d) => match d.to_string().parse::<f64>() {
            Ok(v) if v.is_finite() && v.fract() == 0.0 => v as i64,
            _ => return Err(Fallback),
        },
        _ => return Err(Fallback), // bool / fractional / non-number
    };
    if n < 0 {
        return Err(Fallback); // Python raises "Expected a non-negative number"
    }
    Ok(n as usize)
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

fn add_fields_one(mut doc: Document, spec: &Document, vars: &Document) -> R<Document> {
    // Every field is evaluated against the ORIGINAL doc (matching Python — a
    // computed field doesn't see another set in the same stage), so evaluate all
    // expressions first, then mutate `doc` in place rather than cloning it.
    let mut computed = Vec::with_capacity(spec.len());
    for (path, expr) in spec {
        // `evaluate_or_missing`: a direct field path that doesn't exist is
        // MISSING, not null, and mongod omits the key rather than adding it.
        computed.push((path, evaluate_or_missing(expr, &doc, vars)?));
    }
    for (path, v) in computed {
        // A computed value of `Bson::Undefined` is the "missing" marker (e.g. a
        // `$getField` on an absent field): mongod omits the field. Unset any
        // existing value at the path rather than writing the marker.
        if matches!(v, Bson::Undefined) {
            paths::unset_path(&mut doc, path);
            continue;
        }
        paths::set_path(&mut doc, path, v).map_err(|_| Fallback)?;
    }
    Ok(doc)
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
            let v = evaluate_or_missing(expr, doc, vars)?;
            // `Bson::Undefined` is the "missing" marker (e.g. `$getField` on an
            // absent field): mongod omits the field rather than emitting null.
            if matches!(v, Bson::Undefined) {
                continue;
            }
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

#[cfg(test)]
mod redact_tests {
    use super::*;
    use bson::doc;

    fn run(spec: Bson, doc_in: Document) -> R<Vec<Document>> {
        redact_stage(&spec, vec![doc_in], &Document::new())
    }

    /// The bug this whole slice exists for: a STORED string must not be
    /// mistaken for the `$$KEEP` decision.
    #[test]
    fn a_stored_string_cannot_impersonate_a_decision() {
        let d = doc! {"_id": 1, "tag": "$$KEEP", "secret": "s"};
        assert!(run(Bson::String("$tag".into()), d).is_err());
        // The real variable still works.
        let d = doc! {"_id": 1};
        assert_eq!(
            run(Bson::String("$$KEEP".into()), d.clone()).unwrap(),
            vec![d]
        );
    }

    #[test]
    fn descend_recurses_into_nested_arrays() {
        let spec: Bson = bson::from_document(doc! {
            "$cond": [{"$lte": [{"$ifNull": ["$lvl", 0]}, 3]}, "$$DESCEND", "$$PRUNE"]
        })
        .unwrap();
        let d = doc! {"_id": 1, "lvl": 1, "n": [[{"lvl": 9, "x": 1}], {"lvl": 1, "y": 2}]};
        // The level-9 sub-doc is pruned; its (now empty) inner array remains.
        assert_eq!(
            run(spec, d).unwrap(),
            vec![doc! {"_id": 1, "lvl": 1, "n": [[], {"lvl": 1, "y": 2}]}]
        );
    }

    #[test]
    fn the_decision_names_are_undefined_outside_redact() {
        // Not bound globally: evaluating them without the redact bindings defers.
        let r = expressions::evaluate(&doc! {}, &Bson::String("$$KEEP".into()), &Document::new());
        assert!(r.is_err());
    }

    /// mongod's compact `Value::toString`, which is NOT the shell rendering.
    #[test]
    fn the_17053_value_rendering_is_mongods_compact_form() {
        assert_eq!(render_value_compact(&Bson::Int32(5)), "5");
        assert_eq!(render_value_compact(&Bson::String("x".into())), "\"x\"");
        assert_eq!(render_value_compact(&Bson::Boolean(true)), "true");
        assert_eq!(render_value_compact(&Bson::Null), "null");
        // No inner spaces, unlike `argtypes::render_stage_value`.
        assert_eq!(
            render_value_compact(&Bson::Document(doc! {"k": 1, "j": "s"})),
            "{k: 1, j: \"s\"}"
        );
        assert_eq!(
            render_value_compact(&Bson::Array(vec![Bson::Int32(1), Bson::String("a".into())])),
            "[1, \"a\"]"
        );
        // A bare ObjectId, and a date with exactly three fractional digits.
        let oid = bson::oid::ObjectId::parse_str("64b7f9a2c1d2e3f4a5b6c7d8").unwrap();
        assert_eq!(
            render_value_compact(&Bson::ObjectId(oid)),
            "64b7f9a2c1d2e3f4a5b6c7d8"
        );
        let dt = bson::DateTime::from_millis(1_767_323_045_000);
        assert_eq!(
            render_value_compact(&Bson::DateTime(dt)),
            "2026-01-02T03:04:05.000Z"
        );
    }

    #[test]
    fn the_runtime_error_names_mongods_code_and_value() {
        let stages = vec![Bson::Document(doc! {"$redact": 5})];
        let (code, msg) = redact_runtime_error(&[doc! {"_id": 1}], &stages, &Document::new(), None)
            .expect("expected a named error");
        assert_eq!(code, 17053);
        assert!(msg.ends_with("but returned 5"), "{msg}");
        // A well-formed $redact names nothing.
        let ok = vec![Bson::Document(doc! {"$redact": "$$KEEP"})];
        assert!(redact_runtime_error(&[doc! {"_id": 1}], &ok, &Document::new(), None).is_none());
    }
}
