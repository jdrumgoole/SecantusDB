//! `$setWindowFields` (5.0+): partition + sort + per-row windowed accumulators.
//! A storage-free transform, a bounded port of
//! `aggregate._stage_set_window_fields`.
//!
//! Supported: `partitionBy` (an expression, or absent → one partition), an
//! optional `sortBy`, and an `output` map of `{field: {<accumulator>: arg,
//! window?: {...}}}`. The three rank functions (`$rank` / `$denseRank` /
//! `$documentNumber`), the position-based `$shift` (`{output, by, default?}`),
//! the prefix-accumulated `$expMovingAvg` (`{input, N|alpha}`), the gap-fill
//! `$locf` / `$linearFill` (`<expr>`), the window `$derivative` / `$integral`
//! (`{input, unit?}` — slope / trapezoidal area over the sortBy x-axis; a
//! fixed-duration `unit` scales the x-axis against a date sortBy so the rate is
//! *per unit*), plus every
//! `$group` accumulator the shared `group` module knows
//! (`$sum`/`$avg`/`$min`/`$max`/`$first`/`$last`/`$push`/`$addToSet`/
//! `$count`) evaluate over document-based windows (`documents: [lo, hi]`, with
//! `"unbounded"` / `"current"` / integer bounds; a missing/empty window → the
//! whole partition, matching mongod's default) and value-based windows
//! (`range: [lo, hi]` over a single ascending numeric sortBy — rows whose value
//! is in `[cur+lo, cur+hi]`, with `"unbounded"` / `"current"` / numeric bounds).
//! A range window with a fixed-duration time `unit` (`week`/`day`/`hour`/
//! `minute`/`second`/`millisecond`) offsets against a date sortBy's epoch millis.
//!
//! Defers (`Err(())` → Python) on: range windows or `$derivative` / `$integral`
//! with a variable-length `unit` (`month`/`quarter`/`year`), a non-ascending /
//! multi-field / non-numeric sortBy, a `unit` without a date sortBy, any
//! unsupported accumulator, a non-document/empty `output`, an unsortable
//! `sortBy` value, and a `partitionBy` expression the evaluator can't reproduce.
//! Output preserves the *original* input order (only per-partition ordering
//! drives the windows).

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::{Bson, Document};

use crate::{expressions, group, order, paths};

type R<T> = Result<T, ()>;

const RANK_FUNCS: [&str; 3] = ["$rank", "$denseRank", "$documentNumber"];
/// Ops whose output is a per-slot vector precomputed once over the sorted
/// partition (prefix accumulation / gap-fill), rather than a window accumulator.
const PREFIX_OPS: [&str; 3] = ["$expMovingAvg", "$locf", "$linearFill"];

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
        } else if op == "$shift" {
            // Position-based (like the rank funcs): requires a sortBy, no window,
            // and an `{output, by: <int>, default?}` spec.
            let spec = arg.as_document().ok_or(())?;
            if window.is_some() || sort_by.is_none() || !spec.contains_key("output") {
                return Err(());
            }
            if !matches!(spec.get("by"), Some(Bson::Int32(_)) | Some(Bson::Int64(_))) {
                return Err(());
            }
        } else if op == "$expMovingAvg" {
            // Prefix accumulation: requires a sortBy, no window, `{input, N|alpha}`.
            let spec = arg.as_document().ok_or(())?;
            if window.is_some() || sort_by.is_none() || !spec.contains_key("input") {
                return Err(());
            }
            ema_alpha(spec)?; // validates exactly-one-of N/alpha and their ranges
        } else if op == "$locf" || op == "$linearFill" {
            // Gap-fill over the sorted partition; `arg` is the input expression.
            // No window; requires a sortBy ($linearFill also a numeric x-axis,
            // checked when the vector is built).
            if window.is_some() || sort_by.is_none() {
                return Err(());
            }
        } else if op == "$derivative" || op == "$integral" {
            // Window operators over the sortBy value (x) and input (y). Requires a
            // sortBy; a time `unit` scales the x-axis against a date sortBy
            // (validated in `ts_window_value` where the partition values are known).
            let spec = arg.as_document().ok_or(())?;
            if sort_by.is_none() || !spec.contains_key("input") {
                return Err(());
            }
        } else if op == "$mergeObjects" {
            // $mergeObjects is a $group-only accumulator; mongod rejects it as a
            // window function (FailedToParse) — defer so Python raises the code.
            return Err(());
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
    // Range windows, `$linearFill`, and `$derivative`/`$integral` all need the
    // single-ascending-numeric sortBy values (window bounds / interp / rate x-axis).
    let needs_range = compiled.iter().any(|c| {
        c.window.is_some_and(|w| w.contains_key("range"))
            || matches!(c.op, "$linearFill" | "$derivative" | "$integral")
    });

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
        // Epoch-millis of a date sortBy — the x-axis for a time-`unit` range
        // window (mutually exclusive with the numeric `range_vals`).
        let range_dates = if needs_range {
            range_dates(&slots, &docs, sort_by)
        } else {
            None
        };
        // $expMovingAvg / $locf / $linearFill each produce a per-slot vector —
        // precompute once per partition per output field (parallel to `compiled`).
        let mut prefix_vecs: Vec<Option<Vec<Bson>>> = Vec::with_capacity(compiled.len());
        for c in &compiled {
            prefix_vecs.push(if PREFIX_OPS.contains(&c.op) {
                Some(window_vector(
                    c.op,
                    &slots,
                    &docs,
                    c.arg,
                    range_vals.as_deref(),
                    vars,
                )?)
            } else {
                None
            });
        }
        for (pos, &orig_i) in slots.iter().enumerate() {
            for (ci, c) in compiled.iter().enumerate() {
                let value = if RANK_FUNCS.contains(&c.op) {
                    let r = ranks.as_ref().unwrap();
                    Bson::Int32(match c.op {
                        "$documentNumber" => r.doc_number[pos],
                        "$rank" => r.rank[pos],
                        _ => r.dense[pos], // "$denseRank"
                    })
                } else if PREFIX_OPS.contains(&c.op) {
                    prefix_vecs[ci].as_ref().unwrap()[pos].clone()
                } else if c.op == "$shift" {
                    shift_value(c.arg.as_document().ok_or(())?, pos, &slots, &docs, vars)?
                } else if c.op == "$derivative" || c.op == "$integral" {
                    let (low, high) = if c.window.is_some_and(|w| w.contains_key("range")) {
                        range_window_bounds(
                            pos,
                            n,
                            c.window.unwrap(),
                            range_vals.as_deref(),
                            range_dates.as_deref(),
                        )?
                    } else {
                        window_bounds(pos, n, c.window)?
                    };
                    ts_window_value(
                        c.op,
                        c.arg.as_document().ok_or(())?,
                        low,
                        high,
                        &slots,
                        &docs,
                        range_vals.as_deref(),
                        range_dates.as_deref(),
                        vars,
                    )?
                } else {
                    let (low, high) = if c.window.is_some_and(|w| w.contains_key("range")) {
                        range_window_bounds(
                            pos,
                            n,
                            c.window.unwrap(),
                            range_vals.as_deref(),
                            range_dates.as_deref(),
                        )?
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

/// `$shift` — the `output` expression evaluated on the row `by` positions away in
/// the sorted partition, or `default` (a constant, evaluated on the current row)
/// / `null` when that position is out of the partition. Mirrors `_shift_value`.
fn shift_value(
    spec: &Document,
    pos: usize,
    slots: &[usize],
    docs: &[Document],
    vars: &Document,
) -> R<Bson> {
    let by = match spec.get("by") {
        Some(Bson::Int32(n)) => *n as i64,
        Some(Bson::Int64(n)) => *n,
        _ => return Err(()),
    };
    let output = spec.get("output").ok_or(())?;
    let idx = pos as i64 + by;
    if idx >= 0 && (idx as usize) < slots.len() {
        expressions::evaluate(&docs[slots[idx as usize]], output, vars).map_err(|_| ())
    } else if let Some(default) = spec.get("default") {
        expressions::evaluate(&docs[slots[pos]], default, vars).map_err(|_| ())
    } else {
        Ok(Bson::Null)
    }
}

/// `$derivative` / `$integral` over the window `[low, high]`, using the sortBy
/// value (x) and `input` (y). `$derivative` is the slope between the first and
/// last window points (null if fewer than two, or the x's coincide); `$integral`
/// is the trapezoidal area. Without a `unit` the x-axis is the numeric
/// `range_vals`; with a fixed-duration `unit` it is the date sortBy's epoch
/// millis (`range_dates`) divided by the unit span, so the rate is *per unit*.
/// `unit` requires a date sortBy; a variable-length unit defers. Mirrors
/// `_ts_window_value`.
#[allow(clippy::too_many_arguments)]
fn ts_window_value(
    op: &str,
    spec: &Document,
    low: i64,
    high: i64,
    slots: &[usize],
    docs: &[Document],
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
    vars: &Document,
) -> R<Bson> {
    let scaled: Vec<f64>;
    let xs: &[f64] = match spec.get("unit") {
        Some(Bson::String(u)) => {
            // `unit` requires a date sortBy; scale its millis into the unit.
            let unit_ms = window_unit_ms(u).ok_or(())? as f64;
            let dates = range_dates.ok_or(())?;
            scaled = dates.iter().map(|ms| ms / unit_ms).collect();
            &scaled
        }
        Some(_) => return Err(()), // non-string unit → defer
        None => range_vals.ok_or(())?,
    };
    let input = spec.get("input").ok_or(())?;
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for s in low..=high {
        let s = s as usize;
        let y = as_number(&expressions::evaluate(&docs[slots[s]], input, vars).map_err(|_| ())?)
            .ok_or(())?;
        pts.push((xs[s], y));
    }
    if op == "$derivative" {
        if pts.len() < 2 {
            return Ok(Bson::Null);
        }
        let (x0, y0) = pts[0];
        let (x1, y1) = pts[pts.len() - 1];
        Ok(if x1 == x0 {
            Bson::Null
        } else {
            Bson::Double((y1 - y0) / (x1 - x0))
        })
    } else {
        let mut total = 0.0; // $integral — trapezoidal sum
        for w in pts.windows(2) {
            let ((xa, ya), (xb, yb)) = (w[0], w[1]);
            total += (xb - xa) * (ya + yb) / 2.0;
        }
        Ok(Bson::Double(total))
    }
}

/// `$expMovingAvg` smoothing factor: `2/(N+1)` from a positive-int `N`, or a
/// given `alpha` in (0, 1). Exactly one must be present, else `Err(())` (defer).
fn ema_alpha(spec: &Document) -> R<f64> {
    let has_n = spec.contains_key("N");
    let has_alpha = spec.contains_key("alpha");
    if has_n == has_alpha {
        return Err(());
    }
    if has_n {
        let n = match spec.get("N") {
            Some(Bson::Int32(n)) if *n >= 1 => *n as f64,
            Some(Bson::Int64(n)) if *n >= 1 => *n as f64,
            _ => return Err(()),
        };
        Ok(2.0 / (n + 1.0))
    } else {
        match spec.get("alpha") {
            Some(Bson::Double(a)) if *a > 0.0 && *a < 1.0 => Ok(*a),
            _ => Err(()), // int alpha can't be in (0,1); non-number defers
        }
    }
}

/// Per-slot exponential moving average over the sorted partition, mirroring
/// `_compute_ema`: `ema[0] = input[0]`; `ema[i] = input[i]*alpha +
/// ema[i-1]*(1-alpha)`. Same IEEE-double ops as the oracle → bit-for-bit match.
fn ema_values(slots: &[usize], docs: &[Document], spec: &Document, vars: &Document) -> R<Vec<f64>> {
    let alpha = ema_alpha(spec)?;
    let input = spec.get("input").ok_or(())?;
    let mut out = Vec::with_capacity(slots.len());
    let mut prev: Option<f64> = None;
    for &i in slots {
        let v = expressions::evaluate(&docs[i], input, vars).map_err(|_| ())?;
        let x = as_number(&v).ok_or(())?; // non-numeric input -> defer
        let ema = match prev {
            None => x,
            Some(p) => x * alpha + p * (1.0 - alpha),
        };
        out.push(ema);
        prev = Some(ema);
    }
    Ok(out)
}

/// Per-slot output vector for the prefix/gap-fill window operators, mirroring
/// `_compute_window_vector`. `$expMovingAvg` needs `arg` as a `{input, N|alpha}`
/// doc; `$locf` / `$linearFill` take `arg` as the input expression directly.
fn window_vector(
    op: &str,
    slots: &[usize],
    docs: &[Document],
    arg: &Bson,
    range_vals: Option<&[f64]>,
    vars: &Document,
) -> R<Vec<Bson>> {
    match op {
        "$expMovingAvg" => Ok(ema_values(slots, docs, arg.as_document().ok_or(())?, vars)?
            .into_iter()
            .map(Bson::Double)
            .collect()),
        "$locf" => locf_values(slots, docs, arg, vars),
        "$linearFill" => linear_fill_values(slots, docs, arg, range_vals, vars),
        _ => Err(()),
    }
}

/// `$locf` — last observation of `expr` carried forward over the sorted
/// partition; leading nulls (before the first non-null) stay null.
fn locf_values(slots: &[usize], docs: &[Document], expr: &Bson, vars: &Document) -> R<Vec<Bson>> {
    let mut out = Vec::with_capacity(slots.len());
    let mut last: Option<Bson> = None;
    for &i in slots {
        let v = expressions::evaluate(&docs[i], expr, vars).map_err(|_| ())?;
        if matches!(v, Bson::Null) {
            out.push(last.clone().unwrap_or(Bson::Null));
        } else {
            last = Some(v.clone());
            out.push(v);
        }
    }
    Ok(out)
}

/// `$linearFill` — interpolate `expr`'s nulls between non-null anchors along the
/// (single ascending numeric) sortBy x-axis. Leading / trailing nulls stay null.
/// Same IEEE-double math as `_compute_linear_fill`. Non-numeric sort / anchor,
/// coincident x, or a missing x-axis (`range_vals` is `None`) → defer.
fn linear_fill_values(
    slots: &[usize],
    docs: &[Document],
    expr: &Bson,
    range_vals: Option<&[f64]>,
    vars: &Document,
) -> R<Vec<Bson>> {
    let xs = range_vals.ok_or(())?;
    let vals: Vec<Bson> = slots
        .iter()
        .map(|&i| expressions::evaluate(&docs[i], expr, vars).map_err(|_| ()))
        .collect::<R<Vec<_>>>()?;
    let mut out = vals.clone();
    let anchors: Vec<usize> = vals
        .iter()
        .enumerate()
        .filter(|(_, v)| !matches!(v, Bson::Null))
        .map(|(i, _)| i)
        .collect();
    for w in anchors.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (x0, x1) = (xs[a], xs[b]);
        let y0 = as_number(&vals[a]).ok_or(())?;
        let y1 = as_number(&vals[b]).ok_or(())?;
        if x1 == x0 {
            return Err(());
        }
        for i in (a + 1)..b {
            out[i] = Bson::Double(y0 + (y1 - y0) * ((xs[i] - x0) / (x1 - x0)));
        }
    }
    Ok(out)
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

/// Fixed-duration `unit` → milliseconds, for `range` windows with a time `unit`.
/// Variable-length `month` / `quarter` / `year` (and any unknown unit) → `None`,
/// which makes the range window defer to Python. Mirrors `_WINDOW_UNIT_MS`.
fn window_unit_ms(unit: &str) -> Option<i64> {
    match unit {
        "week" => Some(604_800_000),
        "day" => Some(86_400_000),
        "hour" => Some(3_600_000),
        "minute" => Some(60_000),
        "second" => Some(1_000),
        "millisecond" => Some(1),
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

/// The per-slot epoch-millis of a single **ascending** `DateTime` sortBy field —
/// the x-axis a time-`unit` range window offsets against. `None` unless the sort
/// is a single ascending field whose values are *all* dates. Mirrors the Python
/// `range_date_ms` branch; kept separate from `range_values` (raw dates) so the
/// non-range window ops keep deferring on a non-numeric sortBy.
fn range_dates(slots: &[usize], docs: &[Document], sort_by: Option<&Document>) -> Option<Vec<f64>> {
    let sb = sort_by?;
    if sb.len() != 1 {
        return None;
    }
    let (field, dir) = sb.iter().next().unwrap();
    if !matches!(dir, Bson::Int32(1) | Bson::Int64(1)) {
        return None; // ascending only
    }
    let raw: Vec<Bson> = slots
        .iter()
        .map(|&i| field_value(&docs[i], field))
        .collect();
    if raw.is_empty() || !raw.iter().all(|v| matches!(v, Bson::DateTime(_))) {
        return None;
    }
    raw.iter()
        .map(|v| match v {
            Bson::DateTime(dt) => Some(dt.timestamp_millis() as f64),
            _ => None,
        })
        .collect()
}

/// Inclusive `(low, high)` value-window indices: rows whose (ascending) sortBy
/// value falls in `[cur + lo, cur + hi]`. Bounds: `"unbounded"` → open,
/// `"current"` → `cur`, numeric offset → `cur + b * unit_ms`. A fixed-duration
/// time `unit` scales the offset and pins the x-axis to the date sortBy's epoch
/// millis (`range_dates`). mongod's rule holds both ways: `unit` requires a date
/// sortBy, a date sortBy requires `unit`. A variable-length `unit`
/// (month/quarter/year), a missing single-ascending sort, or a non-numeric bound
/// → defer.
fn range_window_bounds(
    slot: usize,
    n: usize,
    window: &Document,
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
) -> R<(i64, i64)> {
    let (unit_ms, vals): (f64, &[f64]) = match window.get("unit") {
        Some(Bson::String(u)) => {
            // `unit` requires a date sortBy.
            (window_unit_ms(u).ok_or(())? as f64, range_dates.ok_or(())?)
        }
        Some(_) => return Err(()), // non-string unit → defer
        None => {
            // A date sortBy requires a `unit`.
            if range_dates.is_some() {
                return Err(());
            }
            (1.0, range_vals.ok_or(())?)
        }
    };
    let bounds = match window.get("range") {
        Some(Bson::Array(b)) if b.len() == 2 => b,
        _ => return Err(()),
    };
    let cur = vals[slot];
    let edge = |b: &Bson| -> R<Option<f64>> {
        match b {
            Bson::String(s) if s == "unbounded" => Ok(None),
            Bson::String(s) if s == "current" => Ok(Some(cur)),
            _ => as_number(b).map(|x| Some(cur + x * unit_ms)).ok_or(()),
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
