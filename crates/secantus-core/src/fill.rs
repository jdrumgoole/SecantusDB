//! `$fill` (5.3+): fill missing / null fields by value, `locf`, or `linear`
//! interpolation — per partition, optionally sorted. A storage-free transform
//! (it only touches the input docs), a bounded port of `aggregate._stage_fill`.
//!
//! Defers (`Err(())` → Python) on any shape Python rejects (bad output/method,
//! both partition forms, `sortBy` non-doc, `method` without `sortBy`), on a
//! non-numeric/non-date value under `linear`, and on a `partitionBy` expression
//! the evaluator can't reproduce.

use std::collections::HashMap;

use bson::{Bson, Document};

use crate::{expressions, order, paths};

type R<T> = Result<T, ()>;

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

pub fn fill_stage(spec: &Bson, docs: Vec<Document>, vars: &Document) -> R<Vec<Document>> {
    let spec = spec.as_document().ok_or(())?;
    let output = spec
        .get("output")
        .and_then(Bson::as_document)
        .filter(|o| !o.is_empty())
        .ok_or(())?;
    let mut fillers: Vec<Filler> = Vec::new();
    for (field, action) in output {
        let action = action.as_document().ok_or(())?;
        if let Some(v) = action.get("value") {
            fillers.push(Filler::Value(field.clone(), v.clone()));
        } else {
            match action.get("method").and_then(Bson::as_str) {
                Some("locf") => fillers.push(Filler::Locf(field.clone())),
                Some("linear") => fillers.push(Filler::Linear(field.clone())),
                _ => return Err(()),
            }
        }
    }
    let has_method = fillers.iter().any(|f| !matches!(f, Filler::Value(..)));
    // `sortBy` must be a document if present, and is required under any `method`.
    let sort_by = match spec.get("sortBy") {
        None => None,
        Some(Bson::Document(d)) => Some(d),
        Some(_) => return Err(()),
    };
    if has_method && sort_by.is_none() {
        return Err(());
    }
    // `partitionBy` and `partitionByFields` are mutually exclusive.
    let part_by = spec.get("partitionBy");
    let part_fields: Option<Vec<String>> = match spec.get("partitionByFields") {
        None => None,
        Some(_) if part_by.is_some() => return Err(()),
        Some(Bson::Array(a)) => Some(
            a.iter()
                .map(|f| f.as_str().map(str::to_string).ok_or(()))
                .collect::<R<Vec<_>>>()?,
        ),
        Some(_) => return Err(()),
    };

    // Partition, preserving partition-discovery order.
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
        if let Some(s) = sort_by {
            sort_partition(group, s)?;
        }
        for filler in &fillers {
            match filler {
                Filler::Value(field, expr) => {
                    for d in group.iter_mut() {
                        if !is_filled(d, field) {
                            let v = expressions::evaluate(d, expr, vars).map_err(|_| ())?;
                            paths::set_path(d, field, v).map_err(|_| ())?;
                        }
                    }
                }
                Filler::Locf(field) => apply_locf(group, field),
                Filler::Linear(field) => {
                    apply_linear(group, field, sort_field.as_deref().ok_or(())?)?
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
    vars: &Document,
) -> R<Vec<u8>> {
    let key_val: Bson = if let Some(fields) = fields {
        let mut d = Document::new();
        for f in fields {
            d.insert(f.clone(), field_value(doc, f));
        }
        Bson::Document(d)
    } else if let Some(by) = by {
        expressions::evaluate(doc, by, vars).map_err(|_| ())?
    } else {
        Bson::Null
    };
    let mut wrap = Document::new();
    wrap.insert("k", key_val);
    bson::to_vec(&wrap).map_err(|_| ())
}

/// Stable multi-field sort by `sortBy` (BSON order, like `$sort` / `_SortKey`):
/// reversed multi-pass so earlier fields take precedence; ties keep input order.
fn sort_partition(part: &mut [Document], sort_by: &Document) -> R<()> {
    let fields: Vec<(&String, bool)> = sort_by
        .iter()
        .map(|(f, dir)| (f, matches!(dir, Bson::Int32(-1) | Bson::Int64(-1))))
        .collect();
    for (f, _) in &fields {
        if part.iter().any(|d| !order::is_sortable(&field_value(d, f))) {
            return Err(()); // order::cmp's precondition
        }
    }
    for (field, desc) in fields.iter().rev() {
        part.sort_by(|a, b| {
            let o = order::cmp(&field_value(a, field), &field_value(b, field));
            if *desc {
                o.reverse()
            } else {
                o
            }
        });
    }
    Ok(())
}

/// Last-observation-carried-forward within a sorted partition; leading nulls
/// (before the first observed value) stay null.
fn apply_locf(part: &mut [Document], field: &str) {
    let mut last: Option<Bson> = None;
    for doc in part.iter_mut() {
        match paths::get_path(doc, field) {
            Some(v) if !matches!(v, Bson::Null) => last = Some(v.clone()),
            _ => {
                if let Some(l) = last.clone() {
                    let _ = paths::set_path(doc, field, l);
                }
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
        let (x0, _) = as_interp(&field_value(&part[lo], sort_field)).ok_or(())?;
        let (x1, _) = as_interp(&field_value(&part[hi], sort_field)).ok_or(())?;
        let (y0, d0) = as_interp(&field_value(&part[lo], field)).ok_or(())?;
        let (y1, d1) = as_interp(&field_value(&part[hi], field)).ok_or(())?;
        if x1 == x0 {
            return Err(()); // coincident sort keys → undefined slope
        }
        let is_date = d0 && d1;
        for d in &mut part[lo + 1..hi] {
            let (x, _) = as_interp(&field_value(d, sort_field)).ok_or(())?;
            let frac = (x - x0) / (x1 - x0);
            let y = y0 + (y1 - y0) * frac;
            let val = if is_date {
                Bson::DateTime(bson::DateTime::from_millis(y.round() as i64))
            } else {
                Bson::Double(y)
            };
            paths::set_path(d, field, val).map_err(|_| ())?;
        }
    }
    Ok(())
}
