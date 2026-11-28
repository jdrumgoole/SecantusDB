//! `$fill` (5.3+): fill missing / null fields by value, `locf`, or `linear`
//! interpolation — per partition, optionally sorted. A storage-free transform
//! (it only touches the input docs), a bounded port of `aggregate._stage_fill`.
//!
//! Defers (`Err(Fallback::Defer)` → Python) on any shape Python rejects (bad output/method,
//! both partition forms, `sortBy` non-doc, `method` without `sortBy`), on a
//! non-numeric/non-date value under `linear`, and on a `partitionBy` expression
//! the evaluator can't reproduce.

use crate::vars::Vars;
use std::collections::HashMap;

use bson::{Bson, Document};

use crate::fallback::Fallback;
use crate::{expressions, order, paths};

/// Carries `Fallback` rather than `()` so an error the expression engine NAMED
/// survives to the client instead of being flattened into the generic
/// "not supported by the Rust server". `Fallback::Defer` is the same
/// "cannot reproduce this" signal the unit `()` used to be.
type R<T> = Result<T, Fallback>;

enum Filler {
    Value(String, Bson),
    Locf(String),
    Linear(String),
}

/// A field counts as "filled" when it's present and not null (mirrors
/// `_field_is_filled`); only unfilled fields get a value / are interpolated.
fn is_filled(doc: &Document, field: &str) -> bool {
    matches!(paths::get_path(doc, field), Some(v) if !matches!(v, Bson::Null))
}

/// A numeric / date scalar as `(f64, is_date)` — dates as epoch millis. `None`
/// for anything else (a `linear` field/sort value that isn't interpolatable).
fn as_interp(v: &Bson) -> Option<(f64, bool)> {
    match v {
        Bson::Int32(n) => Some((*n as f64, false)),
        Bson::Int64(n) => Some((*n as f64, false)),
        Bson::Double(d) if !d.is_nan() => Some((*d, false)),
        Bson::DateTime(dt) => Some((dt.timestamp_millis() as f64, true)),
        _ => None,
    }
}

fn field_value(doc: &Document, field: &str) -> Bson {
    paths::get_path(doc, field).cloned().unwrap_or(Bson::Null)
}

fn fill_values(d: &mut Document, fillers: &[Filler], vars: &Vars) -> R<()> {
    for filler in fillers {
        if let Filler::Value(field, expr) = filler {
            if !is_filled(d, field) {
                let v = expressions::evaluate(d, expr, vars)?;
                paths::set_path(d, field, v).map_err(|_| Fallback::Defer)?;
            }
        }
    }
    Ok(())
}

/// `docs` in the order of the `$sort` mongod runs a `$fill` with a method
/// behind: its partition, then `sortBy`, through the blocking sorter (its
/// budget, and its order among ties).
fn window_order(
    docs: Vec<Document>,
    fields: Option<&[String]>,
    by: Option<&Bson>,
    sort_by: Option<&Document>,
    vars: &Vars,
) -> R<Vec<Document>> {
    let partitioned = fields.is_some() || by.is_some();
    let sort: Vec<(String, bool)> = match sort_by {
        Some(s) => crate::aggregate::sort_fields(&Bson::Document(s.clone()))?,
        None => Vec::new(),
    };
    if !partitioned && sort.is_empty() {
        return Ok(docs);
    }
    let mut items: Vec<(Vec<Bson>, Document)> = Vec::with_capacity(docs.len());
    for d in &docs {
        let mut keys = Vec::with_capacity(sort.len() + 1);
        if partitioned {
            keys.push(partition_value(d, fields, by, vars)?);
        }
        keys.extend(crate::aggregate::sort_key_values(d, &sort)?);
        if !keys.iter().all(order::is_sortable) {
            return Err(Fallback::Defer); // order::cmp's precondition
        }
        items.push((keys, d.clone()));
    }
    let offset = usize::from(partitioned);
    let cmp = |ka: &[Bson], kb: &[Bson]| {
        if partitioned {
            let c = order::cmp(&ka[0], &kb[0]);
            if c != std::cmp::Ordering::Equal {
                return c;
            }
        }
        for (i, (_, rev)) in sort.iter().enumerate() {
            let c = order::cmp(&ka[i + offset], &kb[i + offset]);
            let c = if *rev { c.reverse() } else { c };
            if c != std::cmp::Ordering::Equal {
                return c;
            }
        }
        std::cmp::Ordering::Equal
    };
    let sizing = crate::sorter::Sizing {
        beside: None,
        sizes: None,
        source: crate::sorter::SortSource::Stored,
        fields: None,
        budget: crate::memlimit::budgets().sort,
        added: None,
    };
    let order = crate::sorter::blocking_sort_order(items, &cmp, &sizing)?;
    let mut taken: Vec<Option<Document>> = docs.into_iter().map(Some).collect();
    Ok(order
        .iter()
        .map(|&i| taken[i].take().expect("each place once"))
        .collect())
}

pub fn fill_stage(spec: &Bson, docs: Vec<Document>, vars: &Vars) -> R<Vec<Document>> {
    let spec = spec.as_document().ok_or(Fallback::Defer)?;
    let output = spec
        .get("output")
        .and_then(Bson::as_document)
        .filter(|o| !o.is_empty())
        .ok_or(Fallback::Defer)?;
    let mut fillers: Vec<Filler> = Vec::new();
    for (field, action) in output {
        let action = action.as_document().ok_or(Fallback::Defer)?;
        if let Some(v) = action.get("value") {
            fillers.push(Filler::Value(field.clone(), v.clone()));
        } else {
            match action.get("method").and_then(Bson::as_str) {
                Some("locf") => fillers.push(Filler::Locf(field.clone())),
                Some("linear") => fillers.push(Filler::Linear(field.clone())),
                _ => return Err(Fallback::Defer),
            }
        }
    }
    let has_method = fillers.iter().any(|f| !matches!(f, Filler::Value(..)));
    // `sortBy` must be a document if present, and is required under any `method`.
    let sort_by = match spec.get("sortBy") {
        None => None,
        Some(Bson::Document(d)) => Some(d),
        Some(_) => return Err(Fallback::Defer),
    };
    if has_method && sort_by.is_none() {
        return Err(Fallback::Defer);
    }
    // `partitionBy` and `partitionByFields` are mutually exclusive.
    let part_by = spec.get("partitionBy");
    let part_fields: Option<Vec<String>> = match spec.get("partitionByFields") {
        None => None,
        Some(_) if part_by.is_some() => return Err(Fallback::Defer),
        Some(Bson::Array(a)) => Some(
            a.iter()
                .map(|f| f.as_str().map(str::to_string).ok_or(Fallback::Defer))
                .collect::<R<Vec<_>>>()?,
        ),
        Some(_) => return Err(Fallback::Defer),
    };

    // A `$fill` with values only is mongod's `$addFields`: no partition and
    // no sort, so every document keeps its place (measured 8.2.11, RM-767).
    if !has_method {
        let mut docs = docs;
        for d in docs.iter_mut() {
            fill_values(d, &fillers, vars)?;
        }
        return Ok(docs);
    }

    // Otherwise mongod runs it as a `$setWindowFields` behind ONE `$sort` on
    // the partition, then `sortBy`: documents whose keys tie come as that
    // sort leaves them, which is not the order they came in (RM-767).
    let docs = window_order(docs, part_fields.as_deref(), part_by, sort_by, vars)?;

    // Partition, preserving partition-discovery order (the sort's).
    let mut groups: Vec<Vec<Document>> = Vec::new();
    let mut key_index: HashMap<Vec<u8>, usize> = HashMap::new();
    for doc in docs {
        let key = partition_key(&doc, part_fields.as_deref(), part_by, vars)?;
        let idx = *key_index.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[idx].push(doc);
    }

    let sort_field: Option<String> = sort_by.and_then(|s| s.keys().next().cloned());

    for group in groups.iter_mut() {
        for filler in &fillers {
            match filler {
                Filler::Value(field, expr) => {
                    for d in group.iter_mut() {
                        if !is_filled(d, field) {
                            let v = expressions::evaluate(d, expr, vars)?;
                            paths::set_path(d, field, v).map_err(|_| Fallback::Defer)?;
                        }
                    }
                }
                Filler::Locf(field) => apply_locf(group, field),
                Filler::Linear(field) => {
                    apply_linear(group, field, sort_field.as_deref().ok_or(Fallback::Defer)?)?
                }
            }
        }
    }

    Ok(groups.into_iter().flatten().collect())
}

/// A stable byte identity for a doc's partition: the field tuple, a `partitionBy`
/// expression value, or a single bucket (`null`) when neither is given.
fn partition_key(
    doc: &Document,
    fields: Option<&[String]>,
    by: Option<&Bson>,
    vars: &Vars,
) -> R<Vec<u8>> {
    let key_val = partition_value(doc, fields, by, vars)?;
    let mut wrap = Document::new();
    wrap.insert("k", crate::collation::fold_active(key_val));
    crate::bsondoc::to_vec(&wrap).map_err(|_| Fallback::Defer)
}

/// The value a doc is partitioned by.
fn partition_value(
    doc: &Document,
    fields: Option<&[String]>,
    by: Option<&Bson>,
    vars: &Vars,
) -> R<Bson> {
    Ok(if let Some(fields) = fields {
        let mut d = Document::new();
        for f in fields {
            d.insert(f.clone(), field_value(doc, f));
        }
        Bson::Document(d)
    } else if let Some(by) = by {
        expressions::evaluate(doc, by, vars)?
    } else {
        Bson::Null
    })
}

/// Last-observation-carried-forward within a sorted partition; leading nulls
/// (before the first observed value) stay null.
fn apply_locf(part: &mut [Document], field: &str) {
    let mut last: Option<Bson> = None;
    for doc in part.iter_mut() {
        match paths::get_path(doc, field) {
            Some(v) if !matches!(v, Bson::Null) => last = Some(v.clone()),
            _ => {
                // With nothing to carry forward the field is still
                // written, as null (measured 8.2.11, 2026-10-10).
                let _ = paths::set_path(doc, field, last.clone().unwrap_or(Bson::Null));
            }
        }
    }
}

/// Linear interpolation along `sort_field` between non-null anchors; leading /
/// trailing nulls stay null. A number field yields a double; a date field yields
/// a date (interpolated on epoch millis).
fn apply_linear(part: &mut [Document], field: &str, sort_field: &str) -> R<()> {
    let anchors: Vec<usize> = (0..part.len())
        .filter(|&i| is_filled(&part[i], field))
        .collect();
    for w in anchors.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        let (x0, _) = as_interp(&field_value(&part[lo], sort_field)).ok_or(Fallback::Defer)?;
        let (x1, _) = as_interp(&field_value(&part[hi], sort_field)).ok_or(Fallback::Defer)?;
        let (y0, d0) = as_interp(&field_value(&part[lo], field)).ok_or(Fallback::Defer)?;
        let (y1, d1) = as_interp(&field_value(&part[hi], field)).ok_or(Fallback::Defer)?;
        if x1 == x0 {
            return Err(Fallback::Defer); // coincident sort keys → undefined slope
        }
        let is_date = d0 && d1;
        for d in &mut part[lo + 1..hi] {
            let (x, _) = as_interp(&field_value(d, sort_field)).ok_or(Fallback::Defer)?;
            let frac = (x - x0) / (x1 - x0);
            let y = y0 + (y1 - y0) * frac;
            let val = if is_date {
                Bson::DateTime(bson::DateTime::from_millis(y.round() as i64))
            } else {
                Bson::Double(y)
            };
            paths::set_path(d, field, val).map_err(|_| Fallback::Defer)?;
        }
    }
    Ok(())
}
