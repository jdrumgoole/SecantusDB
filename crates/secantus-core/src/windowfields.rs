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
//! Defers (`Err(Fallback::Defer)` → Python) on: range windows or `$derivative` / `$integral`
//! with a variable-length `unit` (`month`/`quarter`/`year`), a non-ascending /
//! multi-field / non-numeric sortBy, a `unit` without a date sortBy, any
//! unsupported accumulator, a non-document/empty `output`, an unsortable
//! `sortBy` value, and a `partitionBy` expression the evaluator can't reproduce.
//! Output preserves the *original* input order (only per-partition ordering
//! drives the windows).

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::{Bson, Document};

use crate::fallback::Fallback;
use crate::summation::DoubleDoubleSum;
use crate::{decimal, expressions, group, order, paths};

/// Carries `Fallback` rather than `()` so an error the expression engine NAMED
/// survives to the client instead of being flattened into the generic
/// "not supported by the Rust server". `Fallback::Defer` is the same
/// "cannot reproduce this" signal the unit `()` used to be.
type R<T> = Result<T, Fallback>;

const RANK_FUNCS: [&str; 3] = ["$rank", "$denseRank", "$documentNumber"];
/// Ops whose output is a per-slot vector precomputed once over the sorted
/// partition (prefix accumulation / gap-fill), rather than a window accumulator.
const PREFIX_OPS: [&str; 3] = ["$expMovingAvg", "$locf", "$linearFill"];

/// The window functions mongod runs as removable state over a window that
/// drops documents.
const REMOVABLE_OPS: [&str; 4] = ["$sum", "$avg", "$stdDevPop", "$stdDevSamp"];

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
    let spec = spec.as_document().ok_or(Fallback::Defer)?;
    let partition_by = spec.get("partitionBy");
    let sort_by = match spec.get("sortBy") {
        None => None,
        Some(Bson::Document(d)) => Some(d),
        Some(_) => return Err(Fallback::Defer),
    };
    let output = spec
        .get("output")
        .and_then(Bson::as_document)
        .filter(|o| !o.is_empty())
        .ok_or(Fallback::Defer)?;

    // Compile the output fields: each is `{<$accumulator>: arg, window?: {...}}`.
    let mut compiled: Vec<OutField> = Vec::new();
    for (field, field_spec) in output {
        let fs = field_spec.as_document().ok_or(Fallback::Defer)?;
        let window = match fs.get("window") {
            None => None,
            Some(Bson::Document(w)) => Some(w),
            Some(_) => return Err(Fallback::Defer),
        };
        // The accumulator is the single `$`-prefixed key (the optional `window`
        // key is the only other key mongod allows here).
        let mut acc: Option<(&String, &Bson)> = None;
        for (k, v) in fs {
            if !k.starts_with('$') {
                continue;
            }
            if acc.is_some() {
                return Err(Fallback::Defer); // more than one accumulator
            }
            acc = Some((k, v));
        }
        let (op, arg) = acc.ok_or(Fallback::Defer)?;
        let op = op.as_str();
        if RANK_FUNCS.contains(&op) {
            // Rank functions take no argument and no window; `$rank`/`$denseRank`
            // additionally require a `sortBy`.
            let arg_empty =
                matches!(arg, Bson::Null) || matches!(arg, Bson::Document(d) if d.is_empty());
            if !arg_empty || window.is_some() {
                return Err(Fallback::Defer);
            }
            if (op == "$rank" || op == "$denseRank") && sort_by.is_none() {
                return Err(Fallback::Defer);
            }
        } else if op == "$shift" {
            // Position-based (like the rank funcs): requires a sortBy, no window,
            // and an `{output, by: <int>, default?}` spec.
            let spec = arg.as_document().ok_or(Fallback::Defer)?;
            if window.is_some() || sort_by.is_none() || !spec.contains_key("output") {
                return Err(Fallback::Defer);
            }
            if !matches!(spec.get("by"), Some(Bson::Int32(_)) | Some(Bson::Int64(_))) {
                return Err(Fallback::Defer);
            }
        } else if op == "$expMovingAvg" {
            // Prefix accumulation: requires a sortBy, no window, `{input, N|alpha}`.
            let spec = arg.as_document().ok_or(Fallback::Defer)?;
            if window.is_some() || sort_by.is_none() || !spec.contains_key("input") {
                return Err(Fallback::Defer);
            }
            ema_alpha(spec)?; // validates exactly-one-of N/alpha and their ranges
        } else if op == "$locf" || op == "$linearFill" {
            // Gap-fill over the sorted partition; `arg` is the input expression.
            // No window; requires a sortBy ($linearFill also a numeric x-axis,
            // checked when the vector is built).
            if window.is_some() || sort_by.is_none() {
                return Err(Fallback::Defer);
            }
        } else if op == "$derivative" || op == "$integral" {
            // Window operators over the sortBy value (x) and input (y). Requires a
            // sortBy; a time `unit` scales the x-axis against a date sortBy
            // (validated in `ts_window_value` where the partition values are known).
            let spec = arg.as_document().ok_or(Fallback::Defer)?;
            if sort_by.is_none() || !spec.contains_key("input") {
                return Err(Fallback::Defer);
            }
        } else if op == "$mergeObjects" {
            // $mergeObjects is a $group-only accumulator; mongod rejects it as a
            // window function (FailedToParse) — defer so Python raises the code.
            return Err(Fallback::Defer);
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

    // Partition; each partition holds the *original* indices of its members
    // (in input order), and the key it was found under.
    let mut partitions: Vec<Vec<usize>> = Vec::new();
    let mut keys: Vec<Bson> = Vec::new();
    let mut key_index: HashMap<Vec<u8>, usize> = HashMap::new();
    for (i, doc) in docs.iter().enumerate() {
        let key_val = match partition_by {
            Some(by) => expressions::evaluate(doc, by, vars)?,
            None => Bson::Null,
        };
        if matches!(key_val, Bson::Array(_)) {
            return Err(Fallback::mongo(
                14,
                "An expression used to partition cannot evaluate to value of type array",
            )
            .exec());
        }
        let mut wrap = Document::new();
        wrap.insert("k", crate::collation::fold_active(key_val.clone()));
        let key = bson::to_vec(&wrap).map_err(|_| Fallback::Defer)?;
        let idx = *key_index.entry(key).or_insert_with(|| {
            partitions.push(Vec::new());
            keys.push(key_val);
            partitions.len() - 1
        });
        partitions[idx].push(i);
    }
    // mongod sorts by the partition key first (`$setWindowFields` is a
    // `$sort` on the key and `sortBy`), so the partitions come out in key
    // order -- under the collation, null and missing as one -- and stay in
    // the order they were found when keys compare equal. Measured 8.2.11,
    // 2026-10-16, on five specs with and without a collation. (An earlier
    // note here recorded first-seen order; it was not re-measured then.)
    if keys.iter().all(order::is_comparable) {
        let mut by_key: Vec<usize> = (0..partitions.len()).collect();
        by_key.sort_by(|&a, &b| order::cmp(&keys[a], &keys[b]));
        let mut taken: Vec<Option<Vec<usize>>> = partitions.into_iter().map(Some).collect();
        partitions = by_key
            .iter()
            .map(|&i| taken[i].take().unwrap_or_default())
            .collect();
    }

    let mut out_docs: Vec<Document> = docs.clone();
    // mongod emits PARTITION BY PARTITION, in partition-key order (above),
    // and within each partition in `sortBy` order -- not the input order.
    // Wrong order is wrong RESULTS as
    // soon as a `$limit` follows, so this is not cosmetic. `sorted_slots`
    // already yields exactly that sequence, so recording it costs nothing.
    // Mirrors `aggregate._stage_set_window_fields`.
    let mut emit_order: Vec<usize> = Vec::with_capacity(docs.len());

    for part in &partitions {
        let slots = sorted_slots(part, &docs, sort_by)?;
        emit_order.extend(slots.iter().copied());
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
        // `$sum` / `$avg` / `$stdDevPop` / `$stdDevSamp` over a window that
        // drops documents run mongod's removable state along the partition; a
        // whole-partition or `unbounded`-lower window takes the accumulator
        // path below, as on mongod.
        let mut std_dev_vecs: Vec<Option<Vec<Bson>>> = Vec::with_capacity(compiled.len());
        for c in &compiled {
            std_dev_vecs.push(
                if REMOVABLE_OPS.contains(&c.op) && window_removes(c.window) {
                    Some(removable_window(
                        c,
                        &slots,
                        &docs,
                        range_vals.as_deref(),
                        range_dates.as_deref(),
                        vars,
                    )?)
                } else {
                    None
                },
            );
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
                } else if let Some(v) = &std_dev_vecs[ci] {
                    v[pos].clone()
                } else if c.op == "$shift" {
                    shift_value(
                        c.arg.as_document().ok_or(Fallback::Defer)?,
                        pos,
                        &slots,
                        &docs,
                        vars,
                    )?
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
                        c.arg.as_document().ok_or(Fallback::Defer)?,
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
                paths::set_path(&mut out_docs[orig_i], c.field, value)
                    .map_err(|_| Fallback::Defer)?;
            }
        }
    }

    Ok(emit_order
        .into_iter()
        .map(|i| out_docs[i].clone())
        .collect())
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
            return Err(Fallback::Defer); // order::cmp's precondition
        }
    }
    for (field, desc) in fields.iter().rev() {
        slots.sort_by(|&a, &b| {
            // An array sorts by its smallest element (largest, descending).
            let o = order::cmp(
                &order::sort_repr(field_value(&docs[a], field), *desc),
                &order::sort_repr(field_value(&docs[b], field), *desc),
            );
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
            Some(sb) => sb.iter().all(|(f, dir)| {
                let desc = matches!(dir, Bson::Int32(-1) | Bson::Int64(-1));
                order::cmp(
                    &order::sort_repr(field_value(&docs[slots[i]], f), desc),
                    &order::sort_repr(field_value(&docs[slots[i - 1]], f), desc),
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
/// Whether mongod runs this window through a removable executor: a
/// `documents` or `range` window whose lower bound is not `unbounded`.
fn window_removes(window: Option<&Document>) -> bool {
    let Some(w) = window else {
        return false;
    };
    match w.get("documents").or_else(|| w.get("range")) {
        Some(Bson::Array(b)) if b.len() == 2 => {
            !matches!(&b[0], Bson::String(s) if s == "unbounded")
        }
        _ => false,
    }
}

/// A removable window function per position, moving one state along the
/// partition as mongod's removable executors do
/// (`window_function_exec_removable_{document,range}.cpp`, 8.2): the window
/// is an index interval that never moves back; the documents past its old
/// upper end are added, in order, then as many values as its lower end
/// moved are dropped from the front of the queue. A window that skips ahead
/// adds the documents it skipped and drops them again, as mongod does, and
/// an empty window drops everything.
fn removable_window(
    c: &OutField,
    slots: &[usize],
    docs: &[Document],
    range_vals: Option<&[f64]>,
    range_dates: Option<&[f64]>,
    vars: &Document,
) -> R<Vec<Bson>> {
    let n = slots.len();
    let mut values: Vec<Bson> = Vec::with_capacity(n);
    for &s in slots {
        values.push(expressions::evaluate(&docs[s], c.arg, vars)?);
    }
    let mut state = match c.op {
        "$sum" => Removable::Sum(RemovableSum::new()),
        "$avg" => Removable::Avg(RemovableSum::new(), 0),
        _ => Removable::StdDev(RemovableStdDev::new(c.op == "$stdDevSamp")),
    };
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut last: Option<(i64, i64)> = None;
    let mut out = Vec::with_capacity(n);
    for pos in 0..n {
        let (lo, hi) = if c.window.is_some_and(|w| w.contains_key("range")) {
            range_window_bounds(pos, n, c.window.unwrap(), range_vals, range_dates)?
        } else {
            window_bounds(pos, n, c.window)?
        };
        let current = (hi >= lo).then_some((lo, hi));
        let (add, remove) = match (last, current) {
            (None, None) => (None, 0),
            (None, Some(w)) => (Some(w), 0),
            (Some((ol, ou)), None) => (None, ou - ol + 1),
            (Some((ol, ou)), Some((l, u))) => ((ou < u).then_some((ou + 1, u)), (l - ol).max(0)),
        };
        if let Some((a, b)) = add {
            for i in a..=b {
                state.update(&values[i as usize], 1)?;
                queue.push_back(i as usize);
            }
        }
        for _ in 0..remove {
            if let Some(i) = queue.pop_front() {
                state.update(&values[i], -1)?;
            }
        }
        last = current;
        out.push(state.value()?);
    }
    Ok(out)
}

enum Removable {
    StdDev(RemovableStdDev),
    Sum(RemovableSum),
    /// `WindowFunctionAvg`: the sum and a count of the numeric values.
    Avg(RemovableSum, i64),
}

impl Removable {
    fn update(&mut self, v: &Bson, q: i64) -> R<()> {
        match self {
            Removable::StdDev(s) => s.update(v, q),
            Removable::Sum(s) => s.update(v, q),
            Removable::Avg(s, count) => {
                if is_numeric(v) {
                    s.update(v, q)?;
                    *count += q;
                }
                Ok(())
            }
        }
    }

    fn value(&mut self) -> R<Bson> {
        match self {
            Removable::StdDev(s) => Ok(s.value()),
            Removable::Sum(s) => s.value(),
            Removable::Avg(s, count) => {
                if *count == 0 {
                    return Ok(Bson::Null);
                }
                let n = *count;
                Ok(match s.value()? {
                    Bson::Int32(i) => Bson::Double(f64::from(i) / n as f64),
                    Bson::Int64(i) => Bson::Double(i as f64 / n as f64),
                    Bson::Double(d) if d.is_finite() => Bson::Double(d / n as f64),
                    Bson::Decimal128(d) => {
                        let x = decimal::from_bson(&Bson::Decimal128(d)).ok_or(Fallback::Defer)?;
                        if matches!(x, decimal::Dec::Fin { .. }) {
                            decimal::to_bson(&decimal::div_int(&x, n).ok_or(Fallback::Defer)?)
                                .ok_or(Fallback::Defer)?
                        } else {
                            Bson::Decimal128(d)
                        }
                    }
                    other => other,
                })
            }
        }
    }
}

fn is_numeric(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

/// mongod's `RemovableSum` (`window_function_sum.{h,cpp}`, 8.2): an
/// `AccumulatorSum` that a removed value is added to negated, with NaN and
/// the infinities counted apart, and the answer narrowed back to the types
/// still in the window (the sum's own type only ever widens).
struct RemovableSum {
    sum: group::Num,
    pos_inf: i64,
    neg_inf: i64,
    nan: i64,
    doubles: i64,
    decimals: i64,
}

impl RemovableSum {
    fn new() -> Self {
        RemovableSum {
            sum: group::Num::zero(),
            pos_inf: 0,
            neg_inf: 0,
            nan: 0,
            doubles: 0,
            decimals: 0,
        }
    }

    fn process(&mut self, v: &Bson) -> R<()> {
        self.sum = std::mem::replace(&mut self.sum, group::Num::zero()).add(v)?;
        Ok(())
    }

    fn update(&mut self, v: &Bson, q: i64) -> R<()> {
        match v {
            Bson::Int32(i) => {
                if *i == i32::MIN && q == -1 {
                    self.process(&Bson::Int32(i32::MAX))?;
                    self.process(&Bson::Int32(1))
                } else {
                    self.process(&Bson::Int32(i * q as i32))
                }
            }
            Bson::Int64(i) => {
                if *i == i64::MIN && q == -1 {
                    self.process(&Bson::Int64(i64::MAX))?;
                    self.process(&Bson::Int32(1))
                } else {
                    self.process(&Bson::Int64(i * q))
                }
            }
            Bson::Double(d) => {
                self.doubles += q;
                if d.is_nan() {
                    self.nan += q;
                } else if *d == f64::INFINITY {
                    self.pos_inf += q;
                } else if *d == f64::NEG_INFINITY {
                    self.neg_inf += q;
                } else {
                    self.process(&Bson::Double(d * q as f64))?;
                }
                Ok(())
            }
            Bson::Decimal128(_) => {
                self.decimals += q;
                match decimal::from_bson(v).ok_or(Fallback::Defer)? {
                    decimal::Dec::Nan => self.nan += q,
                    decimal::Dec::Inf(sign) if sign > 0 => self.pos_inf += q,
                    decimal::Dec::Inf(_) => self.neg_inf += q,
                    d => {
                        let d = if q == -1 { decimal::neg(&d) } else { d };
                        self.process(&decimal::to_bson(&d).ok_or(Fallback::Defer)?)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn value(&self) -> R<Bson> {
        let special = |dec: &str, dbl: f64| -> R<Bson> {
            Ok(if self.decimals > 0 {
                decimal::to_bson(&decimal::parse(dec).ok_or(Fallback::Defer)?)
                    .ok_or(Fallback::Defer)?
            } else {
                Bson::Double(dbl)
            })
        };
        if self.nan > 0 || (self.pos_inf > 0 && self.neg_inf > 0) {
            return special("NaN", f64::NAN);
        }
        if self.pos_inf > 0 {
            return special("Infinity", f64::INFINITY);
        }
        if self.neg_inf > 0 {
            return special("-Infinity", f64::NEG_INFINITY);
        }
        let int_or_long = |v: i64| i32::try_from(v).map_or(Bson::Int64(v), Bson::Int32);
        Ok(match self.sum.clone().into_bson()? {
            Bson::Decimal128(d) if self.decimals == 0 => {
                let x = decimal::from_bson(&Bson::Decimal128(d)).ok_or(Fallback::Defer)?;
                let as_double = || -> R<Bson> {
                    Ok(Bson::Double(
                        d.to_string().parse::<f64>().map_err(|_| Fallback::Defer)?,
                    ))
                };
                if self.doubles > 0 {
                    as_double()?
                } else if let Some(v) = integral_i64(&x) {
                    int_or_long(v)
                } else {
                    as_double()?
                }
            }
            Bson::Double(d) if self.doubles == 0 && d >= i64::MIN as f64 && d < i64::MAX as f64 => {
                int_or_long(d.round() as i64)
            }
            Bson::Int64(v) => int_or_long(v),
            other => other,
        })
    }
}

/// `Decimal128::toLong` without a signalling flag: an integral decimal in
/// `i64`'s range, `i64::MIN` included.
fn integral_i64(d: &decimal::Dec) -> Option<i64> {
    let decimal::Dec::Fin { sign, coeff, exp } = d else {
        return None;
    };
    if !decimal::is_integral(d) {
        return None;
    }
    let mut acc: i128 = 0;
    let digits = coeff.len() as i64 + i64::from(*exp);
    if digits > 20 {
        return coeff.iter().all(|&c| c == 0).then_some(0);
    }
    for (i, &c) in coeff.iter().enumerate() {
        // Digits a negative exponent drops are zeros (the value is integral).
        if (i as i64) < coeff.len() as i64 + i64::from((*exp).min(0)) {
            acc = acc * 10 + i128::from(c);
        }
    }
    for _ in 0..(*exp).max(0) {
        acc *= 10;
    }
    i64::try_from(acc * i128::from(*sign)).ok()
}

/// mongod's `WindowFunctionStdDev` (`window_function_stddev.h`, 8.2): a
/// running sum and `m2`, both compensated sums, updated by Welford's step
/// in the count-scaled form. On arm64 mongod's `count * x - sum` is a fused
/// multiply-add, and so is this one.
struct RemovableStdDev {
    sum: group::Num,
    m2: DoubleDoubleSum,
    count: i64,
    nonfinite: i64,
    samp: bool,
}

impl RemovableStdDev {
    fn new(samp: bool) -> Self {
        RemovableStdDev {
            sum: group::Num::zero(),
            m2: DoubleDoubleSum::default(),
            count: 0,
            nonfinite: 0,
            samp,
        }
    }

    fn update(&mut self, v: &Bson, q: i64) -> R<()> {
        let x = match v {
            Bson::Int32(i) => f64::from(*i),
            Bson::Int64(i) => *i as f64,
            Bson::Double(d) => *d,
            // `coerceToDouble`: the nearest double.
            Bson::Decimal128(d) => d.to_string().parse::<f64>().map_err(|_| Fallback::Defer)?,
            _ => return Ok(()),
        };
        if !x.is_finite() {
            self.nonfinite += q;
            return Ok(());
        }
        if self.count == 0 {
            self.count = 1;
            self.sum = std::mem::replace(&mut self.sum, group::Num::zero()).add(v)?;
            return Ok(());
        }
        if self.count + q == 0 {
            // mongod's `reset()`, which also forgets the non-finite values
            // still in the window: their later removals take the count
            // below zero, and the window answers a number again.
            self.sum = group::Num::zero();
            self.m2 = DoubleDoubleSum::default();
            self.count = 0;
            self.nonfinite = 0;
            return Ok(());
        }
        let sum = match self.sum.clone().into_bson()? {
            Bson::Int32(i) => f64::from(i),
            Bson::Int64(i) => i as f64,
            Bson::Double(d) => d,
            Bson::Decimal128(d) => d.to_string().parse::<f64>().map_err(|_| Fallback::Defer)?,
            _ => return Err(Fallback::Defer),
        };
        let delta = (self.count as f64).mul_add(x, -sum);
        self.count += q;
        let qf = q as f64;
        self.sum =
            std::mem::replace(&mut self.sum, group::Num::zero()).add(&Bson::Double(x * qf))?;
        self.m2
            .add_double(delta * delta * qf / (self.count * (self.count - q)) as f64);
        Ok(())
    }

    fn value(&mut self) -> Bson {
        // A window holding a NaN or an infinity has no deviation.
        if self.nonfinite > 0 {
            return Bson::Null;
        }
        let adjusted = if self.samp {
            self.count - 1
        } else {
            self.count
        };
        if adjusted <= 0 {
            return Bson::Null;
        }
        let m2 = self.m2.get_double();
        if m2 < 0.0 || (!self.samp && self.count == 1) {
            // mongod resets a negative `m2` (rounding) and answers int 0.
            self.m2 = DoubleDoubleSum::default();
            return Bson::Int32(0);
        }
        Bson::Double((m2 / adjusted as f64).sqrt())
    }
}

fn window_bounds(slot: usize, n: usize, window: Option<&Document>) -> R<(i64, i64)> {
    let last = n as i64 - 1;
    let bounds = match window.and_then(|w| w.get("documents")) {
        None => return Ok((0, last)),
        Some(Bson::Array(b)) if b.len() == 2 => b,
        Some(_) => return Err(Fallback::Defer),
    };
    let resolve = |b: &Bson, is_lower: bool| -> R<i64> {
        match b {
            Bson::String(s) if s == "unbounded" => Ok(if is_lower { 0 } else { last }),
            Bson::String(s) if s == "current" => Ok(slot as i64),
            Bson::Int32(i) => Ok(slot as i64 + *i as i64),
            Bson::Int64(i) => Ok(slot as i64 + *i),
            _ => Err(Fallback::Defer), // bool / non-int / other string → Python raises
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
        _ => return Err(Fallback::Defer),
    };
    let output = spec.get("output").ok_or(Fallback::Defer)?;
    let idx = pos as i64 + by;
    if idx >= 0 && (idx as usize) < slots.len() {
        expressions::evaluate(&docs[slots[idx as usize]], output, vars)
    } else if let Some(default) = spec.get("default") {
        expressions::evaluate(&docs[slots[pos]], default, vars)
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
            let unit_ms = window_unit_ms(u).ok_or(Fallback::Defer)? as f64;
            let dates = range_dates.ok_or(Fallback::Defer)?;
            scaled = dates.iter().map(|ms| ms / unit_ms).collect();
            &scaled
        }
        Some(_) => return Err(Fallback::Defer), // non-string unit → defer
        None => range_vals.ok_or(Fallback::Defer)?,
    };
    let input = spec.get("input").ok_or(Fallback::Defer)?;
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for s in low..=high {
        let s = s as usize;
        let y = as_number(&expressions::evaluate(&docs[slots[s]], input, vars)?)
            .ok_or(Fallback::Defer)?;
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
/// given `alpha` in (0, 1). Exactly one must be present, else `Err(Fallback::Defer)` (defer).
fn ema_alpha(spec: &Document) -> R<f64> {
    let has_n = spec.contains_key("N");
    let has_alpha = spec.contains_key("alpha");
    if has_n == has_alpha {
        return Err(Fallback::Defer);
    }
    if has_n {
        let n = match spec.get("N") {
            Some(Bson::Int32(n)) if *n >= 1 => *n as f64,
            Some(Bson::Int64(n)) if *n >= 1 => *n as f64,
            _ => return Err(Fallback::Defer),
        };
        Ok(2.0 / (n + 1.0))
    } else {
        match spec.get("alpha") {
            Some(Bson::Double(a)) if *a > 0.0 && *a < 1.0 => Ok(*a),
            _ => Err(Fallback::Defer), // int alpha can't be in (0,1); non-number defers
        }
    }
}

/// Per-slot exponential moving average over the sorted partition, mirroring
/// `_compute_ema`: `ema[0] = input[0]`; `ema[i] = input[i]*alpha +
/// ema[i-1]*(1-alpha)`. Same IEEE-double ops as the oracle → bit-for-bit match.
fn ema_values(slots: &[usize], docs: &[Document], spec: &Document, vars: &Document) -> R<Vec<f64>> {
    let alpha = ema_alpha(spec)?;
    let input = spec.get("input").ok_or(Fallback::Defer)?;
    let mut out = Vec::with_capacity(slots.len());
    let mut prev: Option<f64> = None;
    for &i in slots {
        let v = expressions::evaluate(&docs[i], input, vars)?;
        let x = as_number(&v).ok_or(Fallback::Defer)?; // non-numeric input -> defer
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
        "$expMovingAvg" => {
            Ok(
                ema_values(slots, docs, arg.as_document().ok_or(Fallback::Defer)?, vars)?
                    .into_iter()
                    .map(Bson::Double)
                    .collect(),
            )
        }
        "$locf" => locf_values(slots, docs, arg, vars),
        "$linearFill" => linear_fill_values(slots, docs, arg, range_vals, vars),
        _ => Err(Fallback::Defer),
    }
}

/// `$locf` — last observation of `expr` carried forward over the sorted
/// partition; leading nulls (before the first non-null) stay null.
fn locf_values(slots: &[usize], docs: &[Document], expr: &Bson, vars: &Document) -> R<Vec<Bson>> {
    let mut out = Vec::with_capacity(slots.len());
    let mut last: Option<Bson> = None;
    for &i in slots {
        let v = expressions::evaluate(&docs[i], expr, vars)?;
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
    let xs = range_vals.ok_or(Fallback::Defer)?;
    let vals: Vec<Bson> = slots
        .iter()
        .map(|&i| expressions::evaluate(&docs[i], expr, vars))
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
        let y0 = as_number(&vals[a]).ok_or(Fallback::Defer)?;
        let y1 = as_number(&vals[b]).ok_or(Fallback::Defer)?;
        if x1 == x0 {
            return Err(Fallback::Defer);
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
            (
                window_unit_ms(u).ok_or(Fallback::Defer)? as f64,
                range_dates.ok_or(Fallback::Defer)?,
            )
        }
        Some(_) => return Err(Fallback::Defer), // non-string unit → defer
        None => {
            // A date sortBy requires a `unit`.
            if range_dates.is_some() {
                return Err(Fallback::Defer);
            }
            (1.0, range_vals.ok_or(Fallback::Defer)?)
        }
    };
    let bounds = match window.get("range") {
        Some(Bson::Array(b)) if b.len() == 2 => b,
        _ => return Err(Fallback::Defer),
    };
    let cur = vals[slot];
    let edge = |b: &Bson| -> R<Option<f64>> {
        match b {
            Bson::String(s) if s == "unbounded" => Ok(None),
            Bson::String(s) if s == "current" => Ok(Some(cur)),
            _ => as_number(b)
                .map(|x| Some(cur + x * unit_ms))
                .ok_or(Fallback::Defer),
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
