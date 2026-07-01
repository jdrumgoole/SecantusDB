//! `$setWindowFields` (5.0+): partition + sort + per-row windowed accumulators.
//! A storage-free transform, a bounded port of
//! `aggregate._stage_set_window_fields`.
//!
//! Supported: `partitionBy` (an expression, or absent → one partition), an
//! optional `sortBy`, and an `output` map of `{field: {<accumulator>: arg,
//! window?: {...}}}`. The three rank functions (`$rank` / `$denseRank` /
//! `$documentNumber`) plus every `$group` accumulator the shared `group` module
//! knows (`$sum`/`$avg`/`$min`/`$max`/`$first`/`$last`/`$push`/`$addToSet`/
//! `$count`) evaluate over document-based windows (`documents: [lo, hi]`, with
//! `"unbounded"` / `"current"` / integer bounds; a missing/empty window → the
//! whole partition, matching mongod's default) and value-based windows
//! (`range: [lo, hi]` over a single ascending numeric sortBy — rows whose value
//! is in `[cur+lo, cur+hi]`, with `"unbounded"` / `"current"` / numeric bounds).
//!
//! Defers (`Err(())` → Python) on: range windows with a time `unit` or a
//! non-ascending / multi-field / non-numeric sortBy, time-series operators
//! (`$shift` / `$integral` / `$derivative` / ...), any unsupported accumulator,
//! a non-document/empty `output`, an unsortable `sortBy` value, and a
//! `partitionBy` expression the evaluator can't reproduce. Output preserves the
//! *original* input order (only per-partition ordering drives the windows).

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::{Bson, Document};

use crate::{expressions, group, order, paths};

type R<T> = Result<T, ()>;

const RANK_FUNCS: [&str; 3] = ["$rank", "$denseRank", "$documentNumber"];

fn field_value(doc: &Document, field: &str) -> Bson {
    paths::get_path(doc, field).cloned().unwrap_or(Bson::Null)
}

struct OutField<'a> {
    field: &'a str,
    op: &'a str,
    arg: &'a Bson,
    window: Option<&'a Document>,
}

pub fn set_window_fields_stage(
    spec: &Bson,
    docs: Vec<Document>,
    vars: &Document,
) -> R<Vec<Document>> {
    let spec = spec.as_document().ok_or(())?;
    let partition_by = spec.get("partitionBy");
    let sort_by = match spec.get("sortBy") {
        None => None,
        Some(Bson::Document(d)) => Some(d),
        Some(_) => return Err(()),
    };
    let output = spec
        .get("output")
        .and_then(Bson::as_document)
        .filter(|o| !o.is_empty())
        .ok_or(())?;

    // Compile the output fields: each is `{<$accumulator>: arg, window?: {...}}`.
    let mut compiled: Vec<OutField> = Vec::new();
    for (field, field_spec) in output {
        let fs = field_spec.as_document().ok_or(())?;
        let window = match fs.get("window") {
            None => None,
            Some(Bson::Document(w)) => Some(w),
            Some(_) => return Err(()),
        };
        // The accumulator is the single `$`-prefixed key (the optional `window`
        // key is the only other key mongod allows here).
        let mut acc: Option<(&String, &Bson)> = None;
        for (k, v) in fs {
            if !k.starts_with('$') {
                continue;
            }
            if acc.is_some() {
                return Err(()); // more than one accumulator
            }
            acc = Some((k, v));
        }
        let (op, arg) = acc.ok_or(())?;
        let op = op.as_str();
        if RANK_FUNCS.contains(&op) {
            // Rank functions take no argument and no window; `$rank`/`$denseRank`
            // additionally require a `sortBy`.
            let arg_empty =
                matches!(arg, Bson::Null) || matches!(arg, Bson::Document(d) if d.is_empty());
            if !arg_empty || window.is_some() {
                return Err(());
            }
            if (op == "$rank" || op == "$denseRank") && sort_by.is_none() {
                return Err(());
            }
        } else {
            group::new_acc(op)?; // reject unsupported accumulator (incl. time-series) → defer
        }
        compiled.push(OutField {
            field,
            op,
            arg,
            window,
        });
    }

    let needs_rank = compiled.iter().any(|c| RANK_FUNCS.contains(&c.op));
    let needs_range = compiled
        .iter()
        .any(|c| c.window.is_some_and(|w| w.contains_key("range")));

    // Partition, preserving partition-discovery order; each partition holds the
    // *original* indices of its members (in input order).
    let mut partitions: Vec<Vec<usize>> = Vec::new();
    let mut key_index: HashMap<Vec<u8>, usize> = HashMap::new();
    for (i, doc) in docs.iter().enumerate() {
        let key_val = match partition_by {
            Some(by) => expressions::evaluate(doc, by, vars).map_err(|_| ())?,
            None => Bson::Null,
        };
        let mut wrap = Document::new();
        wrap.insert("k", key_val);
        let key = bson::to_vec(&wrap).map_err(|_| ())?;
        let idx = *key_index.entry(key).or_insert_with(|| {
            partitions.push(Vec::new());
            partitions.len() - 1
        });
        partitions[idx].push(i);
    }

    let mut out_docs: Vec<Document> = docs.clone();

    for part in &partitions {
        let slots = sorted_slots(part, &docs, sort_by)?;
        let n = slots.len();
        let ranks = if needs_rank {
            Some(compute_ranks(&slots, &docs, sort_by))
        } else {
            None
        };
        // Range windows resolve bounds against the single ascending numeric
        // sortBy value; `None` (multi-field / descending / non-numeric) makes a
        // range window defer.
        let range_vals = if needs_range {
            range_values(&slots, &docs, sort_by)
        } else {
            None
        };
        for (pos, &orig_i) in slots.iter().enumerate() {
            for c in &compiled {
                let value = if RANK_FUNCS.contains(&c.op) {
                    let r = ranks.as_ref().unwrap();
                    Bson::Int32(match c.op {
                        "$documentNumber" => r.doc_number[pos],
                        "$rank" => r.rank[pos],
                        _ => r.dense[pos], // "$denseRank"
                    })
                } else {
                    let (low, high) = if c.window.is_some_and(|w| w.contains_key("range")) {
                        range_window_bounds(pos, n, c.window.unwrap(), range_vals.as_deref())?
                    } else {
                        window_bounds(pos, n, c.window)?
                    };
                    let mut acc = group::new_acc(c.op)?;
                    if high >= low {
                        for &s in &slots[low as usize..=high as usize] {
                            group::apply_acc(&mut acc, c.arg, &docs[s], vars)?;
                        }
                    }
                    group::finalize_window_value(acc)?
                };
                paths::set_path(&mut out_docs[orig_i], c.field, value).map_err(|_| ())?;
            }
        }
    }

    Ok(out_docs)
}

/// The partition's original indices, reordered by `sortBy` (BSON order, stable,
/// reversed multi-pass so earlier fields win; ties keep input order). No
/// `sortBy` → input order unchanged.
fn sorted_slots(part: &[usize], docs: &[Document], sort_by: Option<&Document>) -> R<Vec<usize>> {
    let mut slots = part.to_vec();
    let Some(sb) = sort_by else {
        return Ok(slots);
    };
    let fields: Vec<(&String, bool)> = sb
        .iter()
        .map(|(f, dir)| (f, matches!(dir, Bson::Int32(-1) | Bson::Int64(-1))))
        .collect();
    for (f, _) in &fields {
        if part
            .iter()
            .any(|&i| !order::is_sortable(&field_value(&docs[i], f)))
        {
            return Err(()); // order::cmp's precondition
        }
    }
    for (field, desc) in fields.iter().rev() {
        slots.sort_by(|&a, &b| {
            let o = order::cmp(&field_value(&docs[a], field), &field_value(&docs[b], field));
            if *desc {
                o.reverse()
            } else {
                o
            }
        });
    }
    Ok(slots)
}

struct Ranks {
    doc_number: Vec<i32>,
    rank: Vec<i32>,
    dense: Vec<i32>,
}

/// Per-slot rank vectors over a sorted partition. `$documentNumber` is the
/// 1-indexed slot; `$rank` restarts at the tie-group's first slot (gaps on
/// ties); `$denseRank` increments once per distinct sort key (no gaps). Ties are
/// consecutive slots whose `sortBy` field values all compare `Equal`.
fn compute_ranks(slots: &[usize], docs: &[Document], sort_by: Option<&Document>) -> Ranks {
    let n = slots.len();
    let mut doc_number = vec![0i32; n];
    let mut rank = vec![0i32; n];
    let mut dense = vec![0i32; n];
    if n == 0 {
        return Ranks {
            doc_number,
            rank,
            dense,
        };
    }
    doc_number[0] = 1;
    rank[0] = 1;
    dense[0] = 1;
    let mut cur_rank = 1i32;
    let mut cur_dense = 1i32;
    for i in 1..n {
        doc_number[i] = i as i32 + 1;
        let tied = match sort_by {
            Some(sb) => sb.keys().all(|f| {
                order::cmp(
                    &field_value(&docs[slots[i]], f),
                    &field_value(&docs[slots[i - 1]], f),
                ) == Ordering::Equal
            }),
            None => false,
        };
        if !tied {
            cur_rank = i as i32 + 1;
            cur_dense += 1;
        }
        rank[i] = cur_rank;
        dense[i] = cur_dense;
    }
    Ranks {
        doc_number,
        rank,
        dense,
    }
}

/// Inclusive `(low, high)` document-window indices into the sorted partition for
/// the row at `slot`. Missing/empty `window` or one without `documents` → the
/// whole partition. Bounds: `"unbounded"` → partition edge, `"current"` →
/// `slot`, integer `b` → `slot + b`; anything else (incl. bool) → defer.
/// `high < low` signals an empty window (caller uses the empty value). Range
/// windows are routed to [`range_window_bounds`] before this is reached.
fn window_bounds(slot: usize, n: usize, window: Option<&Document>) -> R<(i64, i64)> {
    let last = n as i64 - 1;
    let bounds = match window.and_then(|w| w.get("documents")) {
        None => return Ok((0, last)),
        Some(Bson::Array(b)) if b.len() == 2 => b,
        Some(_) => return Err(()),
    };
    let resolve = |b: &Bson, is_lower: bool| -> R<i64> {
        match b {
            Bson::String(s) if s == "unbounded" => Ok(if is_lower { 0 } else { last }),
            Bson::String(s) if s == "current" => Ok(slot as i64),
            Bson::Int32(i) => Ok(slot as i64 + *i as i64),
            Bson::Int64(i) => Ok(slot as i64 + *i),
            _ => Err(()), // bool / non-int / other string → Python raises
        }
    };
    let low = resolve(&bounds[0], true)?.max(0);
    let high = resolve(&bounds[1], false)?.min(last);
    Ok((low, high))
}

/// A numeric BSON value as `f64` (int / long / double, never bool / Decimal128 —
/// matching the pure `_is_number`). `None` otherwise.
fn as_number(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(d) => Some(*d),
        _ => None,
    }
}

/// The per-slot sortBy values for a range window: `Some` only for a single
/// **ascending** sort field whose values are all numeric (the shape range
/// windows support); anything else → `None`, which makes a range window defer.
fn range_values(
    slots: &[usize],
    docs: &[Document],
    sort_by: Option<&Document>,
) -> Option<Vec<f64>> {
    let sb = sort_by?;
    if sb.len() != 1 {
        return None;
    }
    let (field, dir) = sb.iter().next().unwrap();
    if !matches!(dir, Bson::Int32(1) | Bson::Int64(1)) {
        return None; // ascending only
    }
    slots
        .iter()
        .map(|&i| as_number(&field_value(&docs[i], field)))
        .collect()
}

/// Inclusive `(low, high)` value-window indices: rows whose (ascending) sortBy
/// value falls in `[cur + lo, cur + hi]`. Bounds: `"unbounded"` → open,
/// `"current"` → `cur`, numeric offset → `cur + b`. A time `unit`, a missing
/// single-ascending-numeric sort (`range_vals` is `None`), or a non-numeric
/// bound → defer.
fn range_window_bounds(
    slot: usize,
    n: usize,
    window: &Document,
    range_vals: Option<&[f64]>,
) -> R<(i64, i64)> {
    if window.contains_key("unit") {
        return Err(()); // time-unit ranges defer to Python
    }
    let vals = range_vals.ok_or(())?;
    let bounds = match window.get("range") {
        Some(Bson::Array(b)) if b.len() == 2 => b,
        _ => return Err(()),
    };
    let cur = vals[slot];
    let edge = |b: &Bson| -> R<Option<f64>> {
        match b {
            Bson::String(s) if s == "unbounded" => Ok(None),
            Bson::String(s) if s == "current" => Ok(Some(cur)),
            _ => as_number(b).map(|x| Some(cur + x)).ok_or(()),
        }
    };
    let lo = edge(&bounds[0])?;
    let hi = edge(&bounds[1])?;
    let mut low = 0i64;
    while (low as usize) < n && lo.is_some_and(|l| vals[low as usize] < l) {
        low += 1;
    }
    let mut high = n as i64 - 1;
    while high >= 0 && hi.is_some_and(|h| vals[high as usize] > h) {
        high -= 1;
    }
    Ok((low, high))
}
