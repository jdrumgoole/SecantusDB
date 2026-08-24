//! `$group` (and `$sortByCount`) — Rust port of `secantus.aggregate._stage_group`
//! and its accumulators. The trickiest pipeline stage to reproduce faithfully:
//!
//! * **Group-key bucketing** must match Python's dict semantics, where
//!   `1 == 1.0 == True == Decimal128("1")` collapse into one bucket and
//!   embedded docs / arrays recurse (`_hashable`). We canonicalise each key
//!   value into a hashable `GKey` (numbers + bool normalised through
//!   `numeric::NumVal`, so the cross-type collision is exact) and bucket on
//!   that, preserving first-seen `_id` and insertion order. Key types we can't
//!   canonicalise without a fidelity risk (Decimal128, NaN, Binary/Timestamp/
//!   Regex/Min/MaxKey, exotic) defer the whole stage to Python.
//!
//! * **Accumulators** reproduce mongod's exact numeric semantics: `$sum`/`$avg`
//!   accumulate only numeric operands (int stays int, any float widens; string
//!   / bool / null / missing are ignored, Decimal128 → defer); an all-non-numeric
//!   `$sum` is `0`, `$avg` is null. `$min`/`$max` ignore null / missing and order
//!   every other value by BSON cross-type order (`order::bson_lt`, the relation
//!   `ordering._SortKey` uses). `$addToSet` membership uses Python `==`
//!   (`expressions::py_eq`).
//!
//! Any unported / deferring construct returns `Err(())` and the pure-Python
//! `$group` runs instead.

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::{Bson, Document};

use crate::decimal;
use crate::expressions;
use crate::numeric::{self, as_int_like, int_promoted_to_bson, int_to_bson, is_int64, NumVal};

type R<T> = Result<T, ()>;

fn eval(expr: &Bson, doc: &Document, vars: &Document) -> R<Bson> {
    expressions::evaluate(doc, expr, vars).map_err(|_| ())
}

/// Evaluate an accumulator input, distinguishing a missing field from an explicit
/// null: a top-level absent field path yields `None` (skip), everything else
/// (incl. a present null) yields `Some(value)`. Mirrors
/// `expressions.evaluate_or_missing`; `$push` / `$addToSet` skip missing values as
/// mongod does.
fn eval_or_missing(expr: &Bson, doc: &Document, vars: &Document) -> R<Option<Bson>> {
    if let Bson::String(s) = expr {
        if let Some(path) = s.strip_prefix('$') {
            if !path.starts_with('$') {
                return Ok(crate::paths::get_path(doc, path).cloned());
            }
        }
    }
    eval(expr, doc, vars).map(Some)
}

/// Canonical, hashable group-key — mirrors `_hashable` + Python dict equality.
/// Also used by `$densify` for partition keys (same dict semantics).
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum GKey {
    Null,
    Num(NumVal), // int / int64 / double / bool, normalised (1 == 1.0 == True)
    Str(String),
    Date(i64),
    Oid([u8; 12]),
    Doc(Vec<(String, GKey)>), // sorted by field name
    Arr(Vec<GKey>),
}

/// Canonicalise a key value, or `Err(())` for a type we don't bucket faithfully
/// (Decimal128, NaN, Binary/Timestamp/Regex/Min/MaxKey, exotic).
pub fn gkey(v: &Bson) -> R<GKey> {
    match v {
        Bson::Null => Ok(GKey::Null),
        Bson::Boolean(b) => Ok(GKey::Num(
            numeric::classify(&Bson::Int32(i32::from(*b))).unwrap(),
        )),
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => match numeric::classify(v) {
            Some(NumVal::Nan) => Err(()), // NaN never equals itself in a dict probe -> defer
            Some(n) => Ok(GKey::Num(n)),
            None => Err(()),
        },
        Bson::String(s) => Ok(GKey::Str(s.clone())),
        Bson::DateTime(d) => Ok(GKey::Date(d.timestamp_millis())),
        Bson::ObjectId(o) => Ok(GKey::Oid(o.bytes())),
        Bson::Document(d) => {
            let mut items: Vec<(String, GKey)> = Vec::with_capacity(d.len());
            for (k, val) in d {
                items.push((k.clone(), gkey(val)?));
            }
            // `_hashable` sorts the (key, hashed-value) pairs; keys are unique so
            // sorting by key alone reproduces the order.
            items.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(GKey::Doc(items))
        }
        Bson::Array(a) => {
            let mut items = Vec::with_capacity(a.len());
            for val in a {
                items.push(gkey(val)?);
            }
            Ok(GKey::Arr(items))
        }
        // Decimal128, Binary, Timestamp, Regex, Min/MaxKey, exotic -> Python.
        _ => Err(()),
    }
}

/// Running numeric value preserving Python's int-vs-float distinction. The
/// integral variant also tracks whether any operand was int64, so the result
/// promotes to int64 (MongoDB's numeric widening — `numerics.bson_add`).
// Not `Copy`: the decimal arm owns its coefficient digits.
#[derive(Clone)]
// `pub(crate)` only because it is reachable through the `pub(crate) Acc` the
// `windowfields` module reuses; not part of any real cross-module API.
pub(crate) enum Num {
    Int {
        v: i128,
        wide: bool,
    },
    Float(f64),
    /// Decimal dominates the widening order, so a running total switches here
    /// the moment any decimal value arrives and never switches back.
    Dec(decimal::Dec),
}

impl Num {
    /// Move the running total out, leaving a zero behind. `Num` stopped being
    /// `Copy` when the decimal arm arrived; the accumulator loop still only
    /// needs a move, so this keeps the hot `$sum` path allocation-free.
    fn take(&mut self) -> Num {
        std::mem::replace(self, Num::Int { v: 0, wide: false })
    }

    /// Python `self + v`. `Err(())` if `v` isn't numeric (Python `int + str`
    /// etc. raises -> defer). `pub(crate)` so the expression-form `$sum`/`$avg`
    /// accumulators (`expressions.rs`) reuse the exact width logic for parity.
    pub(crate) fn add(self, v: &Bson) -> R<Num> {
        if matches!(self, Num::Dec(_)) || matches!(v, Bson::Decimal128(_)) {
            return self.add_decimal(v);
        }
        match v {
            Bson::Int32(_) | Bson::Int64(_) | Bson::Boolean(_) => {
                let n = as_int_like(v).unwrap();
                Ok(match self {
                    Num::Int { v: a, wide } => Num::Int {
                        v: a + n,
                        wide: wide || is_int64(v),
                    },
                    Num::Float(f) => Num::Float(f + n as f64),
                    // Unreachable — a decimal running total took the branch
                    // above. Deferring (rather than panicking) keeps a wrong
                    // assumption here a slowdown, not a crash.
                    Num::Dec(_) => return Err(()),
                })
            }
            Bson::Double(d) => Ok(match self {
                Num::Int { v: a, .. } => Num::Float(a as f64 + d),
                Num::Float(f) => Num::Float(f + d),
                Num::Dec(_) => return Err(()), // as above
            }),
            _ => Err(()), // string / array / doc / Decimal128 / null -> TypeError
        }
    }

    /// The decimal-domain arm of [`Num::add`]. Bools stay int-like here exactly
    /// as they are on the integer path, so widening to decimal can't silently
    /// change which values a `$sum` counts.
    fn add_decimal(self, v: &Bson) -> R<Num> {
        let a = match self {
            Num::Dec(d) => d,
            Num::Int { v: a, .. } => decimal::parse(&a.to_string()).ok_or(())?,
            Num::Float(f) => decimal::from_bson_accumulator(&Bson::Double(f)).ok_or(())?,
        };
        let b = match v {
            Bson::Boolean(_) => {
                decimal::from_bson(&Bson::Int32(as_int_like(v).ok_or(())? as i32)).ok_or(())?
            }
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
                decimal::from_bson_accumulator(v).ok_or(())?
            }
            _ => return Err(()), // string / array / doc / null -> TypeError
        };
        Ok(Num::Dec(decimal::add(&a, &b).ok_or(())?))
    }

    pub(crate) fn into_bson(self) -> R<Bson> {
        match self {
            Num::Int { v, wide } => int_promoted_to_bson(v, wide).ok_or(()),
            Num::Float(f) => Ok(Bson::Double(f)),
            Num::Dec(d) => decimal::to_bson(&d).ok_or(()),
        }
    }
}

/// One accumulator's running state.
// Shared with `windowfields` (`$setWindowFields`), which reuses the same
// per-op accumulator state / step / finalize logic over sliding document
// windows rather than whole groups — hence `pub(crate)`.
pub(crate) enum Acc {
    Sum(Num),
    Count(i64),
    Avg(Option<(Num, i64)>), // None until the first numeric value (finalises to null)
    Min(Option<Bson>),       // None == "unset or null-equivalent"
    Max(Option<Bson>),
    First(Option<Bson>), // None == not yet seen any doc
    Last(Option<Bson>),
    Push(Vec<Bson>),
    AddToSet(Vec<Bson>),
    // `$mergeObjects`: merge each per-doc operand document into the accumulator
    // (later keys override earlier — `dict.update` semantics). A null/missing
    // operand is skipped; a non-null, non-document operand defers to Python
    // (which raises Location 40400). An all-missing group still yields `{}`.
    MergeObjects(Document),
    // `$stdDevPop` / `$stdDevSamp`: collect the numeric values, compute at
    // finalize. `pop` selects population (÷n, 0 for a single value) vs sample
    // (÷n-1, null for <2 values). Field stays absent when no numeric value seen.
    StdDev {
        values: Vec<f64>,
        pop: bool,
    },
    // `$firstN` / `$lastN` / `$maxN` / `$minN` accumulators: collect the per-doc
    // `input` value; result computed at finalize. `n` is parsed from the spec on
    // the first `apply` (constant across the group). `$firstN`/`$lastN` keep null
    // values; `$maxN`/`$minN` drop them (mongod-faithful, three-way verified).
    NElem {
        kind: NElemKind,
        n: Option<usize>,
        vals: Vec<Bson>,
    },
    // `$median` / `$percentile`: collect numeric values (int / long / double /
    // Decimal128 as f64; bool and NaN excluded — mongod-probed 7.0.12), then
    // compute mongod's discrete percentile (`sorted[max(0, ceil(p*n) - 1)]`,
    // returned as a double; null / per-p nulls when no value was seen) at
    // finalize. `ps` parses from the spec on the first apply (None = $median).
    // An invalid spec defers to Python, which raises mongod's exact error.
    Percentile {
        is_median: bool,
        ps: Option<Vec<f64>>,
        values: Vec<f64>,
    },
    // `$top` / `$bottom` / `$topN` / `$bottomN`: collect `(sortBy-values, output)`
    // per doc; at finalize stable-sort by the `sortBy` directions and take the
    // top/bottom output(s). `n` and the sort directions are parsed on first apply.
    TopN {
        kind: TopNKind,
        n: Option<usize>,
        dirs: Vec<bool>, // true == descending
        items: Vec<(Vec<Bson>, Bson)>,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum NElemKind {
    First,
    Last,
    Max,
    Min,
}

#[derive(Clone, Copy)]
pub(crate) enum TopNKind {
    Top,
    Bottom,
    TopN,
    BottomN,
}

struct Compiled<'a> {
    field: &'a str,
    op: &'a str,
    arg: &'a Bson,
}

pub(crate) fn new_acc(op: &str) -> R<Acc> {
    Ok(match op {
        "$sum" => Acc::Sum(Num::Int { v: 0, wide: false }),
        "$count" => Acc::Count(0),
        "$avg" => Acc::Avg(None),
        "$min" => Acc::Min(None),
        "$max" => Acc::Max(None),
        "$first" => Acc::First(None),
        "$last" => Acc::Last(None),
        "$push" => Acc::Push(Vec::new()),
        "$addToSet" => Acc::AddToSet(Vec::new()),
        "$mergeObjects" => Acc::MergeObjects(Document::new()),
        "$stdDevPop" => Acc::StdDev {
            values: Vec::new(),
            pop: true,
        },
        "$stdDevSamp" => Acc::StdDev {
            values: Vec::new(),
            pop: false,
        },
        "$firstN" => Acc::NElem {
            kind: NElemKind::First,
            n: None,
            vals: Vec::new(),
        },
        "$lastN" => Acc::NElem {
            kind: NElemKind::Last,
            n: None,
            vals: Vec::new(),
        },
        "$maxN" => Acc::NElem {
            kind: NElemKind::Max,
            n: None,
            vals: Vec::new(),
        },
        "$minN" => Acc::NElem {
            kind: NElemKind::Min,
            n: None,
            vals: Vec::new(),
        },
        "$top" => Acc::TopN {
            kind: TopNKind::Top,
            n: None,
            dirs: Vec::new(),
            items: Vec::new(),
        },
        "$bottom" => Acc::TopN {
            kind: TopNKind::Bottom,
            n: None,
            dirs: Vec::new(),
            items: Vec::new(),
        },
        "$topN" => Acc::TopN {
            kind: TopNKind::TopN,
            n: None,
            dirs: Vec::new(),
            items: Vec::new(),
        },
        "$bottomN" => Acc::TopN {
            kind: TopNKind::BottomN,
            n: None,
            dirs: Vec::new(),
            items: Vec::new(),
        },
        "$median" => Acc::Percentile {
            is_median: true,
            ps: None,
            values: Vec::new(),
        },
        "$percentile" => Acc::Percentile {
            is_median: false,
            ps: None,
            values: Vec::new(),
        },
        _ => return Err(()), // unsupported accumulator -> Python (raises or handles)
    })
}

fn is_null(b: &Bson) -> bool {
    matches!(b, Bson::Null)
}

/// The f64 a value contributes to `$median` / `$percentile`, or None to skip
/// it. mongod-probed: int / long / double / Decimal128 count (as doubles),
/// bool and NaN are excluded, everything else is skipped. The Decimal128 →
/// f64 path is `str::parse` on the decimal string — the same correctly-rounded
/// conversion as Python's `float(Decimal(...))`.
pub(crate) fn percentile_f64(v: &Bson) -> Option<f64> {
    let f = match v {
        Bson::Int32(n) => *n as f64,
        Bson::Int64(n) => *n as f64,
        Bson::Double(d) => *d,
        Bson::Decimal128(d) => d.to_string().parse::<f64>().ok()?,
        _ => return None,
    };
    (!f.is_nan()).then_some(f)
}

/// mongod's discrete percentile over sorted values:
/// `sorted[max(0, ceil(p*n) - 1)]` as a double; null when no value was seen.
pub(crate) fn percentile_rank(sorted_values: &[f64], p: f64) -> Bson {
    if sorted_values.is_empty() {
        return Bson::Null;
    }
    let idx = ((p * sorted_values.len() as f64).ceil() as i64 - 1).max(0) as usize;
    Bson::Double(sorted_values[idx.min(sorted_values.len() - 1)])
}

/// Shared `$median` / `$percentile` finalize: sort (no NaN was collected) and
/// rank; a percentile with no parsed `ps` (impossible for a non-empty group)
/// yields an empty array.
fn percentile_finalize(is_median: bool, ps: Option<Vec<f64>>, mut values: Vec<f64>) -> Bson {
    values.sort_by(|a, b| a.partial_cmp(b).expect("NaN excluded at collect"));
    if is_median {
        percentile_rank(&values, 0.5)
    } else {
        Bson::Array(
            ps.unwrap_or_default()
                .iter()
                .map(|p| percentile_rank(&values, *p))
                .collect(),
        )
    }
}

/// `arg == 1 and not isinstance(arg, bool)` in Python — the literal-count
/// fast path. A bool operand is *not* one here (mongod ignores it in `$sum`).
fn arg_is_one(arg: &Bson) -> bool {
    matches!(arg, Bson::Int32(1) | Bson::Int64(1)) || matches!(arg, Bson::Double(d) if *d == 1.0)
}

pub(crate) fn apply_acc(acc: &mut Acc, arg: &Bson, doc: &Document, vars: &Document) -> R<()> {
    match acc {
        Acc::Sum(running) => {
            // mongod sums only numeric operands (int / long / double / decimal);
            // string / bool / null / missing / array / doc are ignored.
            if arg_is_one(arg) {
                *running = running.take().add(&Bson::Int32(1))?;
            } else {
                if let v @ (Bson::Int32(_)
                | Bson::Int64(_)
                | Bson::Double(_)
                | Bson::Decimal128(_)) = eval(arg, doc, vars)?
                {
                    *running = running.take().add(&v)?;
                }
            }
        }
        Acc::Count(n) => *n += 1,
        Acc::Avg(state) => {
            // Averages only numeric values; an all-non-numeric group finalises
            // to null.
            if let v @ (Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)) =
                eval(arg, doc, vars)?
            {
                let (total, count) = state.take().unwrap_or((Num::Int { v: 0, wide: false }, 0));
                *state = Some((total.add(&v)?, count + 1));
            }
        }
        Acc::Min(cur) => update_extreme(cur, eval(arg, doc, vars)?, Ordering::Less)?,
        Acc::Max(cur) => update_extreme(cur, eval(arg, doc, vars)?, Ordering::Greater)?,
        Acc::First(slot) => {
            if slot.is_none() {
                // `if field not in bucket` — set once (even to null). Wrap so a
                // null first value is distinguishable from "unset".
                *slot = Some(eval(arg, doc, vars)?);
            }
        }
        Acc::Last(slot) => *slot = Some(eval(arg, doc, vars)?),
        Acc::Push(list) => {
            if let Some(v) = eval_or_missing(arg, doc, vars)? {
                list.push(v);
            }
        }
        Acc::AddToSet(list) => {
            let Some(v) = eval_or_missing(arg, doc, vars)? else {
                return Ok(());
            };
            let mut present = false;
            for existing in list.iter() {
                if expressions::py_eq(&v, existing).map_err(|_| ())? {
                    present = true;
                    break;
                }
            }
            if !present {
                list.push(v);
            }
        }
        Acc::MergeObjects(merged) => {
            // Null/missing operand -> skip. Non-null, non-document operand ->
            // defer to Python (which raises Location 40400). Document -> merge
            // with later-key-wins semantics.
            if let Some(v) = eval_or_missing(arg, doc, vars)? {
                match v {
                    Bson::Null => {}
                    Bson::Document(d) => {
                        for (k, val) in d {
                            merged.insert(k, val);
                        }
                    }
                    _ => return Err(()),
                }
            }
        }
        Acc::StdDev { values, .. } => {
            // Python appends every non-null value; a non-numeric would then blow
            // up `sum(values)` at finalize, so we defer such a group to Python
            // (which raises). `null` is skipped. Bool counts as 0/1 (Python sums
            // bools as ints) — matches the pure evaluator.
            let v = eval(arg, doc, vars)?;
            if !is_null(&v) {
                values.push(numeric_f64(&v).ok_or(())?);
            }
        }
        Acc::Percentile {
            is_median,
            ps,
            values,
        } => {
            let Bson::Document(spec) = arg else {
                return Err(()); // Python raises 7429703 / 40414
            };
            if spec.get_str("method") != Ok("approximate") {
                return Err(()); // missing (40414) or non-approximate (BadValue)
            }
            let input = spec.get("input").ok_or(())?;
            if !*is_median && ps.is_none() {
                let Some(Bson::Array(raw)) = spec.get("p") else {
                    return Err(()); // missing (40414) or non-array (7750301)
                };
                let mut parsed = Vec::with_capacity(raw.len());
                for p in raw {
                    let f = match p {
                        Bson::Int32(n) => *n as f64,
                        Bson::Int64(n) => *n as f64,
                        Bson::Double(d) => *d,
                        _ => return Err(()), // Python raises 7750303
                    };
                    if !(0.0..=1.0).contains(&f) {
                        return Err(());
                    }
                    parsed.push(f);
                }
                *ps = Some(parsed);
            }
            let v = eval(input, doc, vars)?;
            if let Some(f) = percentile_f64(&v) {
                values.push(f);
            }
        }
        Acc::NElem { n, vals, .. } => {
            // `arg` is `{n, input}`. Validate n (positive integral; integral double
            // accepted) and require `input`; anything invalid defers to Python,
            // which raises the exact mongod code. `input` is collected per doc,
            // null included (finalize drops nulls for max/min only).
            let Bson::Document(d) = arg else {
                return Err(());
            };
            let nn = match eval(d.get("n").ok_or(())?, doc, vars)? {
                Bson::Int32(x) => x as i64,
                Bson::Int64(x) => x,
                Bson::Double(x) if x.is_finite() && x.fract() == 0.0 => x as i64,
                _ => return Err(()),
            };
            if nn <= 0 {
                return Err(());
            }
            *n = Some(nn as usize);
            vals.push(eval(d.get("input").ok_or(())?, doc, vars)?);
        }
        Acc::TopN {
            kind,
            n,
            dirs,
            items,
        } => {
            // `arg` is `{n?, sortBy, output}`. Any invalid shape defers to Python,
            // which raises the exact mongod code (5788002-5, 10065, 5787908).
            let Bson::Document(d) = arg else {
                return Err(());
            };
            let has_n = matches!(kind, TopNKind::TopN | TopNKind::BottomN);
            if has_n != d.contains_key("n") {
                return Err(()); // topN/bottomN need n; top/bottom reject it
            }
            let Some(Bson::Document(sortby)) = d.get("sortBy") else {
                return Err(()); // missing / non-object sortBy
            };
            if !d.contains_key("output") {
                return Err(());
            }
            let nn = if has_n {
                match eval(d.get("n").unwrap(), doc, vars)? {
                    Bson::Int32(x) => x as i64,
                    Bson::Int64(x) => x,
                    Bson::Double(x) if x.is_finite() && x.fract() == 0.0 => x as i64,
                    _ => return Err(()),
                }
            } else {
                1
            };
            if nn <= 0 {
                return Err(());
            }
            *n = Some(nn as usize);
            if dirs.is_empty() {
                *dirs = sortby
                    .values()
                    .map(|v| as_int_like(v) == Some(-1))
                    .collect();
            }
            let sort_vals: Vec<Bson> = sortby
                .keys()
                .map(|f| {
                    crate::paths::get_path(doc, f)
                        .cloned()
                        .unwrap_or(Bson::Null)
                })
                .collect();
            let output = eval(d.get("output").unwrap(), doc, vars)?;
            items.push((sort_vals, output));
        }
    }
    Ok(())
}

/// Stable-sort the `(sort_values, output)` items by the `sortBy` directions and
/// return the top/bottom output(s). `$top`/`$bottom` return a single value;
/// `$topN`/`$bottomN` return an array. A sort value outside the sortable subset
/// defers to Python (its `_SortKey` handles the wider set). Mirrors the pure
/// `_topn_finalize`.
fn topn_result(
    kind: TopNKind,
    n: usize,
    dirs: &[bool],
    mut items: Vec<(Vec<Bson>, Bson)>,
) -> R<Bson> {
    for (sv, _) in &items {
        if !sv.iter().all(crate::order::is_sortable) {
            return Err(());
        }
    }
    items.sort_by(|a, b| {
        for (i, desc) in dirs.iter().enumerate() {
            let (av, bv) = (a.0.get(i), b.0.get(i));
            let ord = match (av, bv) {
                (Some(x), Some(y)) => crate::order::cmp(x, y),
                _ => Ordering::Equal,
            };
            let ord = if *desc { ord.reverse() } else { ord };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    let outputs: Vec<Bson> = items.into_iter().map(|(_, o)| o).collect();
    Ok(match kind {
        TopNKind::Top => outputs.into_iter().next().unwrap_or(Bson::Null),
        TopNKind::Bottom => outputs.into_iter().last().unwrap_or(Bson::Null),
        TopNKind::TopN => {
            let k = n.min(outputs.len());
            Bson::Array(outputs[..k].to_vec())
        }
        TopNKind::BottomN => {
            let k = n.min(outputs.len());
            Bson::Array(outputs[outputs.len() - k..].to_vec())
        }
    })
}

/// Finalize an N-element accumulator to its result list. `$firstN`/`$lastN` keep
/// nulls (first/last `n` in insertion order); `$maxN`/`$minN` drop nulls, then sort
/// via the `order::cmp`/`is_sortable` contract (`$maxN` descending, `$minN`
/// ascending) — an element outside the sortable subset defers the group to Python.
fn nelem_result(kind: NElemKind, n: usize, vals: Vec<Bson>) -> R<Vec<Bson>> {
    match kind {
        NElemKind::First => {
            let k = n.min(vals.len());
            Ok(vals[..k].to_vec())
        }
        NElemKind::Last => {
            let k = n.min(vals.len());
            Ok(vals[vals.len() - k..].to_vec())
        }
        NElemKind::Max | NElemKind::Min => {
            let mut nn: Vec<Bson> = vals.into_iter().filter(|x| !is_null(x)).collect();
            if !nn.iter().all(crate::order::is_sortable) {
                return Err(());
            }
            let largest = matches!(kind, NElemKind::Max);
            nn.sort_by(|a, b| {
                if largest {
                    crate::order::cmp(b, a)
                } else {
                    crate::order::cmp(a, b)
                }
            });
            let k = n.min(nn.len());
            Ok(nn[..k].to_vec())
        }
    }
}

/// A numeric value as `f64` for `$stdDev*`: int / long / double, plus bool as
/// `0.0`/`1.0` (Python folds bools into the numeric sum). Anything else → `None`,
/// so the caller defers the group to Python (whose `sum()` would raise).
fn numeric_f64(b: &Bson) -> Option<f64> {
    match b {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(d) => Some(*d),
        Bson::Boolean(x) => Some(if *x { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Population / sample standard deviation, mirroring the pure `_std_dev`: `None`
/// for an empty set, and additionally for a sample (`pop == false`) with < 2
/// values (population of a single value is `0.0`). Squares with plain
/// multiplication (`d * d`) and roots with `sqrt` — both correctly-rounded IEEE
/// operations that reproduce CPython's `(x - mean) ** 2` / `... ** 0.5` bit-for-bit
/// (a fuzz seed exposed that `f64::powf(2.0)` can round differently from
/// multiplication). Sums in document order to match `sum(...)`.
fn std_dev(values: &[f64], pop: bool) -> Option<f64> {
    let n = values.len();
    if n == 0 || (!pop && n < 2) {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let denom = if pop { n } else { n - 1 } as f64;
    let var = values
        .iter()
        .map(|x| {
            let d = x - mean;
            d * d
        })
        .sum::<f64>()
        / denom;
    Some(var.sqrt())
}

/// `$min` / `$max`: ignore null / missing and order every other value by BSON
/// cross-type order (`order::bson_lt`, the same relation `ordering._SortKey`
/// uses — bool > string > number > …). `None` defers only a genuinely
/// unorderable pair (DBPointer, an unclassifiable Decimal128).
fn update_extreme(cur: &mut Option<Bson>, v: Bson, want: Ordering) -> R<()> {
    if is_null(&v) {
        return Ok(()); // null never updates and never "unsets"
    }
    match cur {
        None => *cur = Some(v),
        Some(existing) => {
            // $max replaces when existing < v; $min when v < existing.
            let replace = match want {
                Ordering::Greater => crate::order::bson_lt(existing, &v),
                _ => crate::order::bson_lt(&v, existing),
            };
            match replace {
                Some(true) => *cur = Some(v),
                Some(false) => {}
                None => return Err(()),
            }
        }
    }
    Ok(())
}

fn finalize(id: Bson, accs: Vec<(&str, Acc)>) -> R<Document> {
    let mut out = Document::new();
    out.insert("_id".to_string(), id);
    for (field, acc) in accs {
        match acc {
            Acc::Sum(n) => {
                out.insert(field.to_string(), n.into_bson()?);
            }
            Acc::Count(n) => {
                out.insert(field.to_string(), int_to_bson(n as i128).ok_or(())?);
            }
            Acc::Avg(state) => {
                // All-non-numeric group -> null (mongod), matching Python's
                // always-created avg state finalising to null.
                match state {
                    Some((total, count)) => {
                        let val = match total {
                            Num::Int { v: a, .. } => {
                                if a.unsigned_abs() > (1u128 << 53) {
                                    return Err(()); // precision: defer to Python int/int divide
                                }
                                Bson::Double(a as f64 / count as f64)
                            }
                            Num::Float(f) => Bson::Double(f / count as f64),
                            // Stay in the decimal domain — an f64 divide would
                            // narrow the type and drop digits.
                            Num::Dec(d) => {
                                decimal::to_bson(&decimal::div_int(&d, count).ok_or(())?)
                                    .ok_or(())?
                            }
                        };
                        out.insert(field.to_string(), val);
                    }
                    None => {
                        out.insert(field.to_string(), Bson::Null);
                    }
                }
            }
            Acc::Min(v) | Acc::Max(v) => {
                out.insert(field.to_string(), v.unwrap_or(Bson::Null));
            }
            Acc::First(v) | Acc::Last(v) => {
                out.insert(field.to_string(), v.unwrap_or(Bson::Null));
            }
            Acc::Push(list) => {
                out.insert(field.to_string(), Bson::Array(list));
            }
            Acc::AddToSet(list) => {
                out.insert(field.to_string(), Bson::Array(list));
            }
            Acc::MergeObjects(merged) => {
                out.insert(field.to_string(), Bson::Document(merged));
            }
            Acc::StdDev { values, pop } => {
                // No numeric value seen -> the pure code never creates the bucket
                // key, so the field is absent. Otherwise `_std_dev` may still be
                // null (sample of a single value), which the pure code writes.
                if !values.is_empty() {
                    let v = std_dev(&values, pop).map_or(Bson::Null, Bson::Double);
                    out.insert(field.to_string(), v);
                }
            }
            Acc::Percentile {
                is_median,
                ps,
                values,
            } => {
                out.insert(
                    field.to_string(),
                    percentile_finalize(is_median, ps, values),
                );
            }
            Acc::NElem { kind, n, vals } => {
                out.insert(
                    field.to_string(),
                    Bson::Array(nelem_result(kind, n.unwrap_or(0), vals)?),
                );
            }
            Acc::TopN {
                kind,
                n,
                dirs,
                items,
            } => {
                out.insert(
                    field.to_string(),
                    topn_result(kind, n.unwrap_or(1), &dirs, items)?,
                );
            }
        }
    }
    Ok(out)
}

/// Finalize a single accumulator to its scalar value, for `$setWindowFields`
/// (one value per output field per row, not a whole group doc). Differs from
/// `finalize` only in that `$avg` over no non-null value yields `Null` rather
/// than an absent field — mongod's `$setWindowFields` writes the window's
/// empty/degenerate value (`_empty_window_value`) into every row. Because a
/// fresh accumulator applied over zero window docs already lands on those
/// defaults (`$sum`/`$count` -> 0, `$push`/`$addToSet` -> [], everything else
/// -> Null), the empty-window case needs no special path.
pub(crate) fn finalize_window_value(acc: Acc) -> R<Bson> {
    Ok(match acc {
        Acc::Sum(n) => n.into_bson()?,
        Acc::Count(n) => int_to_bson(n as i128).ok_or(())?,
        Acc::Avg(state) => match state {
            Some((total, count)) => {
                let tf = match total {
                    Num::Int { v: a, .. } => {
                        if a.unsigned_abs() > (1u128 << 53) {
                            return Err(()); // precision: defer to Python int/int divide
                        }
                        a as f64
                    }
                    Num::Float(f) => f,
                    // A decimal total stays in the decimal domain — dividing
                    // through f64 would both narrow the type and lose digits.
                    Num::Dec(d) => {
                        return decimal::to_bson(&decimal::div_int(&d, count).ok_or(())?).ok_or(());
                    }
                };
                Bson::Double(tf / count as f64)
            }
            None => Bson::Null,
        },
        Acc::Min(v) | Acc::Max(v) => v.unwrap_or(Bson::Null),
        Acc::First(v) | Acc::Last(v) => v.unwrap_or(Bson::Null),
        Acc::Push(list) | Acc::AddToSet(list) => Bson::Array(list),
        // Empty window -> {} (a fresh accumulator over zero docs), matching
        // `_empty_window_value("$mergeObjects")`.
        Acc::MergeObjects(merged) => Bson::Document(merged),
        // A window writes a value into every row: empty / degenerate -> null.
        Acc::StdDev { values, pop } => std_dev(&values, pop).map_or(Bson::Null, Bson::Double),
        Acc::Percentile {
            is_median,
            ps,
            values,
        } => percentile_finalize(is_median, ps, values),
        Acc::NElem { kind, n, vals } => Bson::Array(nelem_result(kind, n.unwrap_or(0), vals)?),
        Acc::TopN {
            kind,
            n,
            dirs,
            items,
        } => topn_result(kind, n.unwrap_or(1), &dirs, items)?,
    })
}

/// Accumulator ops that take a **single expression argument** and reach no
/// document field except through `$path` strings inside that argument. For
/// these, walking the argument with [`collect_fields`] yields a complete set of
/// the top-level fields the accumulator reads. Deliberately excludes
/// `$top`/`$topN`/`$bottom`/`$bottomN` — those carry a `sortBy: {field: 1}`
/// whose field is a bare *key*, not a `$field` string, so a field walk would
/// miss it. Anything not listed defers to a full decode (correct, just no raw
/// speedup); new accumulators default to that safe path.
const SIMPLE_ACCUMULATORS: &[&str] = &[
    "$sum",
    "$avg",
    "$min",
    "$max",
    "$first",
    "$last",
    "$push",
    "$addToSet",
    "$mergeObjects",
    "$stdDevPop",
    "$stdDevSamp",
    "$count",
];

/// The set of top-level document fields a `$group` spec reads — or `None` when
/// an expression references the whole document (`$$ROOT`/`$$CURRENT`), accesses
/// a field by computed/implicit name (`$getField`/`$setField`/`$unsetField`),
/// or uses an accumulator outside [`SIMPLE_ACCUMULATORS`]. When `Some(fields)`,
/// decoding **only** `fields` from each input document and running the ordinary
/// [`group_stage`] on the result is byte-identical to running it on the fully
/// decoded documents, because every field path the evaluator can reach is
/// present with its original value (an absent field decodes to the same
/// "missing" either way). This is the pushdown that lets the command layer skip
/// materializing untouched fields of wide documents ahead of a `$group`.
pub fn referenced_top_level_fields(spec: &Bson) -> Option<std::collections::BTreeSet<String>> {
    let Bson::Document(s) = spec else {
        return None;
    };
    // No `_id` → `group_stage` errors and defers anyway; don't special-case it.
    s.get("_id")?;
    let mut fields = std::collections::BTreeSet::new();
    for (k, v) in s {
        if k == "_id" {
            if !collect_fields(v, &mut fields) {
                return None;
            }
        } else {
            // Accumulator: must be a single-op doc `{$op: arg}`.
            let Bson::Document(acc) = v else {
                return None;
            };
            if acc.len() != 1 {
                return None;
            }
            let (op, arg) = acc.iter().next().unwrap();
            if !SIMPLE_ACCUMULATORS.contains(&op.as_str()) {
                return None;
            }
            if !collect_fields(arg, &mut fields) {
                return None;
            }
        }
    }
    Some(fields)
}

/// Walk an aggregation expression, inserting the top-level component of every
/// `$field.path` string into `out`. Returns `false` to signal the caller must
/// full-decode: a `$$ROOT`/`$$CURRENT`/`$$REMOVE` whole-document reference, or a
/// `$getField`/`$setField`/`$unsetField`/`$function`/`$accumulator` operator
/// that can read a field by a name not expressed as a `$path` string. Local
/// variables (`$$this`, `$$value`, `$let` bindings, `$map`/`$filter` `as`) are
/// ignored: they resolve to array elements or already-evaluated values, never
/// to a top-level document field, and the arrays/values they range over are
/// themselves reached through `$path` strings collected here.
fn collect_fields(expr: &Bson, out: &mut std::collections::BTreeSet<String>) -> bool {
    match expr {
        Bson::String(s) => {
            if let Some(var) = s.strip_prefix("$$") {
                let base = var.split('.').next().unwrap_or(var);
                // Whole-doc / field-removal system vars can't be bounded.
                !matches!(base, "ROOT" | "CURRENT" | "REMOVE")
            } else if let Some(path) = s.strip_prefix('$') {
                if !path.is_empty() {
                    let top = path.split('.').next().unwrap_or(path);
                    out.insert(top.to_string());
                }
                true
            } else {
                true // plain string constant
            }
        }
        Bson::Array(a) => a.iter().all(|e| collect_fields(e, out)),
        Bson::Document(d) => {
            if d.len() == 1 {
                let (k, v) = d.iter().next().unwrap();
                if k == "$literal" {
                    return true; // argument is opaque data, never a field path
                }
                if matches!(
                    k.as_str(),
                    "$getField" | "$setField" | "$unsetField" | "$function" | "$accumulator"
                ) {
                    return false; // computed / implicit-CURRENT field access
                }
                if k.starts_with('$') {
                    return collect_fields(v, out);
                }
            }
            d.values().all(|v| collect_fields(v, out))
        }
        _ => true,
    }
}

/// Compile the accumulator specs (each must be a single-op doc) and run the
/// group. Shared by `$group` and `$sortByCount`.
fn run_group(
    id_expr: &Bson,
    accumulators: &[(String, Bson)],
    docs: &[Document],
    vars: &Document,
) -> R<Vec<Document>> {
    // Validate accumulator shape up front (each is `{op: arg}`).
    let mut compiled: Vec<Compiled> = Vec::with_capacity(accumulators.len());
    for (field, spec) in accumulators {
        let Bson::Document(d) = spec else {
            return Err(()); // Python raises
        };
        if d.len() != 1 {
            return Err(());
        }
        let (op, arg) = d.iter().next().unwrap();
        new_acc(op)?; // reject unsupported ops before doing work
        compiled.push(Compiled { field, op, arg });
    }

    let mut index: HashMap<GKey, usize> = HashMap::new();
    let mut ids: Vec<Bson> = Vec::new();
    let mut states: Vec<Vec<Acc>> = Vec::new();

    for d in docs {
        let key_val = eval(id_expr, d, vars)?;
        let gk = gkey(&key_val)?;
        let idx = match index.get(&gk) {
            Some(i) => *i,
            None => {
                let i = ids.len();
                index.insert(gk, i);
                ids.push(key_val);
                let mut accs = Vec::with_capacity(compiled.len());
                for c in &compiled {
                    accs.push(new_acc(c.op)?);
                }
                states.push(accs);
                i
            }
        };
        for (c, acc) in compiled.iter().zip(states[idx].iter_mut()) {
            apply_acc(acc, c.arg, d, vars)?;
        }
    }

    let mut out = Vec::with_capacity(ids.len());
    for (id, accs) in ids.into_iter().zip(states) {
        let paired: Vec<(&str, Acc)> = compiled.iter().map(|c| c.field).zip(accs).collect();
        out.push(finalize(id, paired)?);
    }
    Ok(out)
}

/// `$group` stage entry point.
pub fn group_stage(spec: &Bson, docs: &[Document], vars: &Document) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(());
    };
    let Some(id_expr) = s.get("_id") else {
        return Err(()); // Python raises "requires an _id expression"
    };
    let accumulators: Vec<(String, Bson)> = s
        .iter()
        .filter(|(k, _)| *k != "_id")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    run_group(id_expr, &accumulators, docs, vars)
}

/// `$sortByCount` — `group({_id: spec, count: {$sum: 1}})` then a stable sort by
/// `count` descending (ties keep group insertion order, matching Python's
/// `list.sort(reverse=True)` stability).
pub fn sort_by_count_stage(spec: &Bson, docs: &[Document], vars: &Document) -> R<Vec<Document>> {
    // mongod: a $-prefixed path string, or a single-`$`-key expression object;
    // anything else (number/bool/array/null -> 40149, bare string -> 40148,
    // non-expression object -> 40147) defers so Python raises the exact code.
    match spec {
        Bson::String(s) if s.starts_with('$') => {}
        Bson::Document(d)
            if d.len() == 1 && d.keys().next().is_some_and(|k| k.starts_with('$')) => {}
        _ => return Err(()),
    }
    let count_acc = bson::doc! {"$sum": 1i32};
    let accumulators = vec![("count".to_string(), Bson::Document(count_acc))];
    let mut grouped = run_group(spec, &accumulators, docs, vars)?;
    grouped.sort_by(|a, b| {
        let ca = a.get("count").and_then(as_int_like).unwrap_or(0);
        let cb = b.get("count").and_then(as_int_like).unwrap_or(0);
        cb.cmp(&ca) // descending; stable -> ties keep order
    });
    Ok(grouped)
}

/// Accumulate `output_spec` over `docs` into a single bucket with a fixed
/// `_id` (shared by `$bucket`). Mirrors the per-bucket `_accumulate` /
/// `_finalize` loop in `_stage_bucket`. Caller guarantees `docs` is non-empty
/// (an empty `$bucket` bucket emits only `{_id}` — the accumulator fields are
/// never created, matching the pure code).
fn accumulate_into(
    id: Bson,
    output_spec: &Document,
    docs: &[&Document],
    vars: &Document,
) -> R<Document> {
    let mut compiled: Vec<(&str, &Bson, Acc)> = Vec::with_capacity(output_spec.len());
    for (field, spec) in output_spec {
        let Bson::Document(d) = spec else {
            return Err(()); // Python raises (accumulator must be a doc)
        };
        if d.len() != 1 {
            return Err(());
        }
        let (op, arg) = d.iter().next().unwrap();
        compiled.push((field.as_str(), arg, new_acc(op)?));
    }
    for d in docs {
        for (_field, arg, acc) in compiled.iter_mut() {
            apply_acc(acc, arg, d, vars)?;
        }
    }
    let paired: Vec<(&str, Acc)> = compiled.into_iter().map(|(f, _a, acc)| (f, acc)).collect();
    finalize(id, paired)
}

/// `$bucket` — place each doc into the half-open boundary range
/// `boundaries[i] <= value < boundaries[i+1]` (Python's native `<=`/`<` via
/// `expressions::py_order`, so cross-type / Decimal128 / array-doc boundaries
/// defer rather than guess), falling to `default` when unplaced, then run the
/// `output` accumulators per bucket.
/// Canonical type name for a `$bucket` boundary — the numeric BSON types collapse
/// to one bracket (mongod requires all boundaries the same type).
fn bucket_ctype(v: &Bson) -> &'static str {
    match v {
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => "number",
        other => crate::query::bson_type_name(other),
    }
}

pub fn bucket_stage(spec: &Bson, docs: &[Document], vars: &Document) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(());
    };
    let group_by = match s.get("groupBy") {
        None | Some(Bson::Null) => return Err(()), // missing groupBy -> Python raises 40198
        Some(v) => v.clone(),
    };
    let Some(Bson::Array(boundaries)) = s.get("boundaries") else {
        return Err(()); // missing / non-array boundaries -> Python raises
    };
    if boundaries.len() < 2 {
        return Err(());
    }
    // Boundaries must all be the same canonical type (40193) and strictly
    // ascending (40194) -- previously unsorted/mixed boundaries were accepted.
    let ct0 = bucket_ctype(&boundaries[0]);
    for w in boundaries.windows(2) {
        if bucket_ctype(&w[1]) != ct0 {
            return Err(());
        }
        if !matches!(
            expressions::py_order(&w[0], &w[1]).map_err(|_| ())?,
            Some(Ordering::Less)
        ) {
            return Err(());
        }
    }
    // `default is not None` — an explicit null default counts as absent.
    let default = match s.get("default") {
        None | Some(Bson::Null) => None,
        Some(v) => Some(v.clone()),
    };
    if let Some(dv) = &default {
        // default must lie outside [first, last) -- below the first boundary or
        // >= the last (mongod 40199).
        let below = matches!(
            expressions::py_order(dv, &boundaries[0]).map_err(|_| ())?,
            Some(Ordering::Less)
        );
        let below_last = matches!(
            expressions::py_order(dv, &boundaries[boundaries.len() - 1]).map_err(|_| ())?,
            Some(Ordering::Less)
        );
        if !below && below_last {
            return Err(());
        }
    }
    let default_output = bson::doc! {"count": {"$sum": 1i32}};
    let output_spec: &Document = match s.get("output") {
        Some(Bson::Document(d)) if !d.is_empty() => d,
        None | Some(Bson::Document(_)) => &default_output, // absent / empty -> default
        Some(_) => return Err(()), // truthy non-doc -> Python `.items()` raises
    };

    // Bucket keys = boundaries[..-1] then `default`. Python keys them in a dict,
    // so equal keys would collapse / reset — defer those pathological collisions.
    let nb = boundaries.len();
    let mut seen: Vec<GKey> = Vec::new();
    let mut keys: Vec<Bson> = Vec::new();
    for b in &boundaries[..nb - 1] {
        let gk = gkey(b)?;
        if seen.contains(&gk) {
            return Err(());
        }
        seen.push(gk);
        keys.push(b.clone());
    }
    let default_idx = if let Some(dv) = &default {
        let gk = gkey(dv)?;
        if seen.contains(&gk) {
            return Err(());
        }
        keys.push(dv.clone());
        Some(keys.len() - 1)
    } else {
        None
    };

    let mut placed: Vec<Vec<&Document>> = vec![Vec::new(); keys.len()];
    for d in docs {
        let v = eval(&group_by, d, vars)?;
        let mut put = false;
        for i in 0..nb - 1 {
            // `boundaries[i] <= v` (skip this bucket on TypeError / NaN-False).
            match expressions::py_order(&boundaries[i], &v).map_err(|_| ())? {
                None => continue,
                Some(Ordering::Greater) => continue, // lo > v
                Some(_) => {}
            }
            // `v < boundaries[i+1]`.
            match expressions::py_order(&v, &boundaries[i + 1]).map_err(|_| ())? {
                Some(Ordering::Less) => {
                    placed[i].push(d);
                    put = true;
                    break;
                }
                _ => continue, // >= hi, or TypeError/NaN -> not this bucket
            }
        }
        if !put {
            match default_idx {
                Some(di) => placed[di].push(d),
                // No matching bucket and no default: mongod errors (Python raises
                // 7158303). Previously the document was silently DROPPED.
                None => return Err(()),
            }
        }
    }

    let mut out = Vec::with_capacity(keys.len());
    for (key, bucket_docs) in keys.into_iter().zip(placed) {
        if bucket_docs.is_empty() {
            // Empty bucket: only `_id` (accumulator fields are never created).
            let mut doc = Document::new();
            doc.insert("_id".to_string(), key);
            out.push(doc);
        } else {
            out.push(accumulate_into(key, output_spec, &bucket_docs, vars)?);
        }
    }
    Ok(out)
}

/// `$bucketAuto` — sort docs by the `groupBy` value (byte-sortable sort key, so
/// cross-type order matches mongod / storage) and split them into at most
/// `buckets` chunks of roughly equal count, then run the `output` accumulators
/// (default `{count: {$sum: 1}}`) per chunk with `_id: {min, max}`. Mirrors
/// `aggregate._stage_bucket_auto`: a pure count-chunking — documents that share a
/// boundary value are *not* coalesced into one bucket (so N equal values still
/// split across buckets), matching the Python server.
// mongod's preferred-number rounding series for $bucketAuto `granularity`,
// stored exactly as mongod stores them (integer-valued doubles) so that
// `series_element * multiplier` reproduces mongod's f64 boundaries bit-for-bit.
// Mirrors `secantus.aggregate._BUCKET_AUTO_SERIES`; verified hex-exact against
// mongod 7.0.12. See `granularity_rounder_preferred_numbers.cpp`.
fn series_for(name: &str) -> Option<&'static [f64]> {
    Some(match name {
        "R5" => &[10.0, 16.0, 25.0, 40.0, 63.0],
        "R10" => &[
            100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0,
        ],
        "R20" => &[
            100.0, 112.0, 125.0, 140.0, 160.0, 180.0, 200.0, 224.0, 250.0, 280.0, 315.0, 355.0,
            400.0, 450.0, 500.0, 560.0, 630.0, 710.0, 800.0, 900.0,
        ],
        "R40" => &[
            100.0, 106.0, 112.0, 118.0, 125.0, 132.0, 140.0, 150.0, 160.0, 170.0, 180.0, 190.0,
            200.0, 212.0, 224.0, 236.0, 250.0, 265.0, 280.0, 300.0, 315.0, 355.0, 375.0, 400.0,
            425.0, 450.0, 475.0, 500.0, 530.0, 560.0, 600.0, 630.0, 670.0, 710.0, 750.0, 800.0,
            850.0, 900.0, 950.0,
        ],
        "R80" => &[
            103.0, 109.0, 115.0, 122.0, 128.0, 136.0, 145.0, 155.0, 165.0, 175.0, 185.0, 195.0,
            206.0, 218.0, 230.0, 243.0, 258.0, 272.0, 290.0, 307.0, 325.0, 345.0, 365.0, 387.0,
            412.0, 437.0, 462.0, 487.0, 515.0, 545.0, 575.0, 615.0, 650.0, 690.0, 730.0, 775.0,
            825.0, 875.0, 925.0, 975.0,
        ],
        "1-2-5" => &[10.0, 20.0, 50.0],
        "E6" => &[10.0, 15.0, 22.0, 33.0, 47.0, 68.0],
        "E12" => &[
            10.0, 12.0, 15.0, 18.0, 22.0, 27.0, 33.0, 39.0, 47.0, 56.0, 68.0, 82.0,
        ],
        "E24" => &[
            10.0, 11.0, 12.0, 13.0, 15.0, 16.0, 18.0, 20.0, 22.0, 24.0, 27.0, 30.0, 33.0, 36.0,
            39.0, 43.0, 47.0, 51.0, 56.0, 62.0, 68.0, 75.0, 82.0, 91.0,
        ],
        "E48" => &[
            100.0, 105.0, 110.0, 115.0, 121.0, 127.0, 133.0, 140.0, 147.0, 154.0, 162.0, 169.0,
            178.0, 187.0, 196.0, 205.0, 215.0, 226.0, 237.0, 249.0, 261.0, 274.0, 287.0, 301.0,
            316.0, 332.0, 348.0, 365.0, 383.0, 402.0, 422.0, 442.0, 464.0, 487.0, 511.0, 536.0,
            562.0, 590.0, 619.0, 649.0, 681.0, 715.0, 750.0, 787.0, 825.0, 866.0, 909.0, 953.0,
        ],
        "E96" => &[
            100.0, 102.0, 105.0, 107.0, 110.0, 113.0, 115.0, 118.0, 121.0, 124.0, 127.0, 130.0,
            133.0, 137.0, 140.0, 143.0, 147.0, 150.0, 154.0, 158.0, 162.0, 165.0, 169.0, 174.0,
            178.0, 182.0, 187.0, 191.0, 196.0, 200.0, 205.0, 210.0, 215.0, 221.0, 226.0, 232.0,
            237.0, 243.0, 249.0, 255.0, 261.0, 267.0, 274.0, 280.0, 287.0, 294.0, 301.0, 309.0,
            316.0, 324.0, 332.0, 340.0, 348.0, 357.0, 365.0, 374.0, 383.0, 392.0, 402.0, 412.0,
            422.0, 432.0, 442.0, 453.0, 464.0, 475.0, 487.0, 499.0, 511.0, 523.0, 536.0, 549.0,
            562.0, 576.0, 590.0, 604.0, 619.0, 634.0, 649.0, 665.0, 681.0, 698.0, 715.0, 732.0,
            750.0, 768.0, 787.0, 806.0, 825.0, 845.0, 866.0, 887.0, 909.0, 931.0, 953.0, 976.0,
        ],
        "E192" => &[
            100.0, 101.0, 102.0, 104.0, 105.0, 106.0, 107.0, 109.0, 110.0, 111.0, 113.0, 114.0,
            115.0, 117.0, 118.0, 120.0, 121.0, 123.0, 124.0, 126.0, 127.0, 129.0, 130.0, 132.0,
            133.0, 135.0, 137.0, 138.0, 140.0, 142.0, 143.0, 145.0, 147.0, 149.0, 150.0, 152.0,
            154.0, 156.0, 158.0, 160.0, 162.0, 164.0, 165.0, 167.0, 169.0, 172.0, 174.0, 176.0,
            178.0, 180.0, 182.0, 184.0, 187.0, 189.0, 191.0, 193.0, 196.0, 198.0, 200.0, 203.0,
            205.0, 208.0, 210.0, 213.0, 215.0, 218.0, 221.0, 223.0, 226.0, 229.0, 232.0, 234.0,
            237.0, 240.0, 243.0, 246.0, 249.0, 252.0, 255.0, 258.0, 261.0, 264.0, 267.0, 271.0,
            274.0, 277.0, 280.0, 284.0, 287.0, 291.0, 294.0, 298.0, 301.0, 305.0, 309.0, 312.0,
            316.0, 320.0, 324.0, 328.0, 332.0, 336.0, 340.0, 344.0, 348.0, 352.0, 357.0, 361.0,
            365.0, 370.0, 374.0, 379.0, 383.0, 388.0, 392.0, 397.0, 402.0, 407.0, 412.0, 417.0,
            422.0, 427.0, 432.0, 437.0, 442.0, 448.0, 453.0, 459.0, 464.0, 470.0, 475.0, 481.0,
            487.0, 493.0, 499.0, 505.0, 511.0, 517.0, 523.0, 530.0, 536.0, 542.0, 549.0, 556.0,
            562.0, 569.0, 576.0, 583.0, 590.0, 597.0, 604.0, 612.0, 619.0, 626.0, 634.0, 642.0,
            649.0, 657.0, 665.0, 673.0, 681.0, 690.0, 698.0, 706.0, 715.0, 723.0, 732.0, 741.0,
            750.0, 759.0, 768.0, 777.0, 787.0, 796.0, 806.0, 816.0, 825.0, 835.0, 845.0, 856.0,
            866.0, 876.0, 887.0, 898.0, 909.0, 920.0, 931.0, 942.0, 953.0, 965.0, 976.0, 988.0,
        ],
        _ => return None,
    })
}

fn is_valid_granularity(name: &str) -> bool {
    name == "POWERSOF2" || series_for(name).is_some()
}

/// mongod `GranularityRounderPreferredNumbers::roundUp` (double path).
fn round_up_series(number: f64, series: &[f64]) -> f64 {
    if number == 0.0 || number == f64::INFINITY {
        return number;
    }
    let mut multiplier = 1.0;
    while number >= series[series.len() - 1] * multiplier {
        multiplier *= 10.0;
    }
    while number < series[0] * multiplier {
        let previous_min = series[0] * multiplier;
        multiplier /= 10.0;
        if number >= series[series.len() - 1] * multiplier {
            return previous_min;
        }
    }
    // smallest series element with number < series*multiplier (strict upper bound)
    for &s in series {
        if number < s * multiplier {
            return s * multiplier;
        }
    }
    series[series.len() - 1] * multiplier
}

/// mongod `GranularityRounderPreferredNumbers::roundDown` (double path).
fn round_down_series(number: f64, series: &[f64]) -> f64 {
    if number == 0.0 || number == f64::INFINITY {
        return number;
    }
    let mut multiplier = 1.0;
    while number <= series[0] * multiplier {
        multiplier /= 10.0;
    }
    if multiplier == 0.0 {
        return 0.0;
    }
    while number > series[series.len() - 1] * multiplier {
        let previous_max = series[series.len() - 1] * multiplier;
        multiplier *= 10.0;
        if number <= series[0] * multiplier {
            return previous_max;
        }
    }
    // largest series element with series*multiplier < number (strict)
    let mut prev = series[0] * multiplier;
    for &s in &series[1..] {
        let scaled = s * multiplier;
        if scaled >= number {
            return prev;
        }
        prev = scaled;
    }
    prev
}

/// mongod `GranularityRounderPowersOfTwo` (double path).
fn round_up_pow2(v: f64) -> f64 {
    if v == 0.0 || v == f64::INFINITY {
        return v;
    }
    2.0_f64.powf(v.log2().floor() + 1.0)
}

fn round_down_pow2(v: f64) -> f64 {
    if v == 0.0 || v == f64::INFINITY {
        return v;
    }
    2.0_f64.powf(v.log2().ceil() - 1.0)
}

/// Coerce a groupBy value to the double mongod's rounder works on, or `Err(())`
/// (defer) for a value mongod would reject (non-numeric / NaN / negative) or a
/// Decimal128 (the standing precision deferral). The Python engine raises the
/// exact 40258 / 40259 / 40260; on the Rust server a defer surfaces as BadValue.
fn granularity_coerce(v: &Bson) -> R<f64> {
    let f = match v {
        Bson::Int32(n) => *n as f64,
        Bson::Int64(n) => *n as f64,
        Bson::Double(d) => *d,
        _ => return Err(()),
    };
    if f.is_nan() || f < 0.0 {
        return Err(());
    }
    Ok(f)
}

/// mongod `DocumentSourceBucketAuto::populateNextBucket` with a granularity
/// rounder. Mirrors `secantus.aggregate._bucket_auto_granular`; hex-exact vs
/// mongod 7.0.12.
fn bucket_auto_granular(
    keyed: &[(Vec<u8>, Bson, &Document)],
    n_buckets: usize,
    granularity: &str,
    output_spec: &Document,
    vars: &Document,
) -> R<Vec<Document>> {
    let n = keyed.len();
    let mut values: Vec<f64> = Vec::with_capacity(n);
    for (_, v, _) in keyed {
        values.push(granularity_coerce(v)?);
    }
    let is_pow2 = granularity == "POWERSOF2";
    let series: &[f64] = if is_pow2 {
        &[]
    } else {
        series_for(granularity).ok_or(())?
    };
    let rup = |x: f64| {
        if is_pow2 {
            round_up_pow2(x)
        } else {
            round_up_series(x, series)
        }
    };
    let rdn = |x: f64| {
        if is_pow2 {
            round_down_pow2(x)
        } else {
            round_down_series(x, series)
        }
    };

    let mut approx = (n as f64 / n_buckets as f64 + 0.5).floor() as i64; // std::round (positive)
    if approx < 1 {
        approx = 1;
    }

    let mut out: Vec<Document> = Vec::new();
    let mut idx = 0usize;
    let mut previous_max: Option<f64> = None;
    let mut carry: Option<usize> = None;
    let mut bucket_num = 0usize;
    loop {
        bucket_num += 1;
        if carry.is_none() && idx >= n {
            break;
        }
        let cur_i = if let Some(c) = carry {
            c
        } else {
            let c = idx;
            idx += 1;
            c
        };
        let cur_min = previous_max.unwrap_or_else(|| rdn(values[cur_i]));
        let mut cur_max = values[cur_i];
        let mut chunk: Vec<usize> = vec![cur_i];
        let is_last = bucket_num == n_buckets;
        let mut i = 1i64;
        while idx < n && (i < approx || is_last) {
            cur_max = values[idx];
            chunk.push(idx);
            idx += 1;
            i += 1;
        }
        let mut next_i: Option<usize> = if idx < n {
            let c = idx;
            idx += 1;
            Some(c)
        } else {
            None
        };
        let boundary = rup(cur_max);
        // Absorb values that now fall below the rounded boundary (boundary fixed).
        while let Some(ni) = next_i {
            if boundary > values[ni] {
                chunk.push(ni);
                next_i = if idx < n {
                    let c = idx;
                    idx += 1;
                    Some(c)
                } else {
                    None
                };
            } else {
                break;
            }
        }
        let bucket_max = match next_i {
            Some(ni) if boundary == 0.0 => rdn(values[ni]),
            _ => boundary,
        };
        let id = Bson::Document(bson::doc! { "min": cur_min, "max": bucket_max });
        let chunk_docs: Vec<&Document> = chunk.iter().map(|&ci| keyed[ci].2).collect();
        out.push(accumulate_into(id, output_spec, &chunk_docs, vars)?);
        previous_max = Some(bucket_max);
        carry = next_i;
        if carry.is_none() && idx >= n {
            break;
        }
    }
    Ok(out)
}

pub fn bucket_auto_stage(spec: &Bson, docs: &[Document], vars: &Document) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(());
    };
    let Some(group_by) = s.get("groupBy") else {
        return Err(());
    };
    // buckets: a positive integer, or a whole double (mongod accepts 2.0). Any
    // other value (bool, fractional double, non-positive, non-number, missing)
    // defers so Python raises the exact code (40241/40242/40243/40246).
    let n_buckets = match s.get("buckets") {
        Some(Bson::Int32(n)) if *n >= 1 => *n as usize,
        Some(Bson::Int64(n)) if *n >= 1 => *n as usize,
        Some(Bson::Double(d)) if d.fract() == 0.0 && *d >= 1.0 => *d as usize,
        _ => return Err(()),
    };
    let default_output = bson::doc! {"count": {"$sum": 1i32}};
    let output_spec: &Document = match s.get("output") {
        Some(Bson::Document(d)) if !d.is_empty() => d,
        None | Some(Bson::Document(_)) => &default_output,
        Some(_) => return Err(()),
    };
    // A non-string / unknown granularity defers so Python raises 40261 / 40257.
    let granularity: Option<&str> = match s.get("granularity") {
        None => None,
        Some(Bson::String(g)) if is_valid_granularity(g) => Some(g.as_str()),
        Some(_) => return Err(()),
    };

    // Evaluate groupBy per doc, then sort by the byte-sortable encoding (the same
    // order Python's `_SortKey` gives). A value the encoder can't represent
    // defers the whole stage to Python.
    let mut pairs: Vec<(Bson, &Document)> = Vec::with_capacity(docs.len());
    for d in docs {
        pairs.push((eval(group_by, d, vars)?, d));
    }
    let mut keyed: Vec<(Vec<u8>, Bson, &Document)> = Vec::with_capacity(pairs.len());
    for (v, d) in pairs {
        let k = crate::sortkey::encode_value(&v, None).map_err(|_| ())?;
        keyed.push((k, v, d));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    if keyed.is_empty() {
        return Ok(Vec::new());
    }

    if let Some(gran) = granularity {
        return bucket_auto_granular(&keyed, n_buckets, gran, output_spec, vars);
    }

    let bucket_size = std::cmp::max(1, keyed.len() / n_buckets);
    let mut out: Vec<Document> = Vec::new();
    let mut i = 0usize;
    while i < keyed.len() && out.len() < n_buckets {
        let is_last = out.len() == n_buckets - 1;
        let end = if is_last {
            keyed.len()
        } else {
            std::cmp::min(i + bucket_size, keyed.len())
        };
        let chunk = &keyed[i..end];
        if chunk.is_empty() {
            break;
        }
        // Upper bound: the next chunk's first value when there is one, else this
        // chunk's last value (mirrors Python's `pairs[i + bucket_size]` lookahead).
        let upper = if !is_last && i + bucket_size < keyed.len() {
            keyed[i + bucket_size].1.clone()
        } else {
            chunk[chunk.len() - 1].1.clone()
        };
        let id = Bson::Document(bson::doc! { "min": chunk[0].1.clone(), "max": upper });
        let chunk_docs: Vec<&Document> = chunk.iter().map(|(_, _, d)| *d).collect();
        out.push(accumulate_into(id, output_spec, &chunk_docs, vars)?);
        i += chunk.len();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn g(spec: bson::Bson, docs: Vec<Document>) -> Vec<Document> {
        group_stage(&spec, &docs, &Document::new()).expect("should not defer")
    }

    #[test]
    fn sum_and_count_collide_numeric_keys() {
        // keys 1 (int) and 1.0 (double) and true bucket together.
        let docs = vec![
            doc! {"k": 1i32, "v": 10i32},
            doc! {"k": 1.0f64, "v": 5i32},
            doc! {"k": true, "v": 2i32},
            doc! {"k": 2i32, "v": 7i32},
        ];
        let out = g(
            bson::bson!({"_id": "$k", "total": {"$sum": "$v"}, "n": {"$sum": 1}}),
            docs,
        );
        // First bucket: _id == 1 (first-seen), total 17, n 3.
        assert_eq!(out[0].get("_id"), Some(&Bson::Int32(1)));
        assert_eq!(out[0].get("total"), Some(&Bson::Int32(17)));
        assert_eq!(out[0].get("n"), Some(&Bson::Int32(3)));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn avg_is_double_and_null_when_no_numeric() {
        let out = g(
            bson::bson!({"_id": Bson::Null, "a": {"$avg": "$v"}}),
            vec![doc! {"v": 2i32}, doc! {"v": 4i32}],
        );
        assert_eq!(out[0].get("a"), Some(&Bson::Double(3.0)));
        let out2 = g(
            bson::bson!({"_id": Bson::Null, "a": {"$avg": "$missing"}}),
            vec![doc! {"v": 2i32}],
        );
        assert_eq!(out2[0].get("a"), Some(&Bson::Null)); // mongod: null, not absent
    }

    #[test]
    fn min_max_first_last_push() {
        let docs = vec![doc! {"v": 3i32}, doc! {"v": 1i32}, doc! {"v": 2i32}];
        let out = g(
            bson::bson!({
                "_id": Bson::Null,
                "mn": {"$min": "$v"}, "mx": {"$max": "$v"},
                "f": {"$first": "$v"}, "l": {"$last": "$v"},
                "p": {"$push": "$v"},
            }),
            docs,
        );
        assert_eq!(out[0].get("mn"), Some(&Bson::Int32(1)));
        assert_eq!(out[0].get("mx"), Some(&Bson::Int32(3)));
        assert_eq!(out[0].get("f"), Some(&Bson::Int32(3)));
        assert_eq!(out[0].get("l"), Some(&Bson::Int32(2)));
        assert_eq!(
            out[0].get("p"),
            Some(&Bson::Array(vec![
                Bson::Int32(3),
                Bson::Int32(1),
                Bson::Int32(2)
            ]))
        );
    }

    #[test]
    fn topn_bottomn_accumulators() {
        // docs: (s, score) — sort by score desc: x2(9), x1(3), x3(1).
        let docs = vec![
            doc! {"s": "x1", "score": 3i32},
            doc! {"s": "x2", "score": 9i32},
            doc! {"s": "x3", "score": 1i32},
        ];
        let out = g(
            bson::bson!({
                "_id": Bson::Null,
                "tn": {"$topN": {"n": 2, "sortBy": {"score": -1}, "output": "$s"}},
                "bn": {"$bottomN": {"n": 2, "sortBy": {"score": 1}, "output": "$s"}},
                "t": {"$top": {"sortBy": {"score": -1}, "output": "$s"}},
                "b": {"$bottom": {"sortBy": {"score": -1}, "output": "$s"}},
            }),
            docs,
        );
        let s = |v: &str| Bson::String(v.into());
        // topN score desc: top 2 = x2, x1.
        assert_eq!(out[0].get("tn"), Some(&Bson::Array(vec![s("x2"), s("x1")])));
        // bottomN score asc [x3,x1,x2] -> last 2 = x1, x2.
        assert_eq!(out[0].get("bn"), Some(&Bson::Array(vec![s("x1"), s("x2")])));
        // $top / $bottom are single values.
        assert_eq!(out[0].get("t"), Some(&s("x2")));
        assert_eq!(out[0].get("b"), Some(&s("x3")));
        // Validation: $top with n, missing sortBy/output, n<=0 -> defer (Python raises).
        let d = vec![doc! {"s": "a", "score": 1i32}];
        for bad in [
            bson::bson!({"$top": {"n": 2, "sortBy": {"score": -1}, "output": "$s"}}),
            bson::bson!({"$topN": {"n": 2, "output": "$s"}}),
            bson::bson!({"$topN": {"n": 2, "sortBy": {"score": -1}}}),
            bson::bson!({"$topN": {"n": 0, "sortBy": {"score": -1}, "output": "$s"}}),
        ] {
            let spec = bson::bson!({"_id": Bson::Null, "r": bad});
            assert!(group_stage(&spec, &d, &Document::new()).is_err());
        }
    }

    #[test]
    fn nelem_accumulators() {
        // Group values in doc order: 3, 1, null, 5, 2.
        let docs = vec![
            doc! {"v": 3i32},
            doc! {"v": 1i32},
            doc! {"v": Bson::Null},
            doc! {"v": 5i32},
            doc! {"v": 2i32},
        ];
        let out = g(
            bson::bson!({
                "_id": Bson::Null,
                "f": {"$firstN": {"n": 2, "input": "$v"}},
                "l": {"$lastN": {"n": 2, "input": "$v"}},
                "mx": {"$maxN": {"n": 2, "input": "$v"}},
                "mn": {"$minN": {"n": 2, "input": "$v"}},
                "f3": {"$firstN": {"n": 3, "input": "$v"}},
                "mx3": {"$maxN": {"n": 3, "input": "$v"}},
            }),
            docs,
        );
        let arr = |xs: &[i32]| Bson::Array(xs.iter().map(|x| Bson::Int32(*x)).collect());
        assert_eq!(out[0].get("f"), Some(&arr(&[3, 1])));
        assert_eq!(out[0].get("l"), Some(&arr(&[5, 2])));
        assert_eq!(out[0].get("mx"), Some(&arr(&[5, 3])));
        assert_eq!(out[0].get("mn"), Some(&arr(&[1, 2])));
        // firstN keeps null; maxN drops it.
        assert_eq!(
            out[0].get("f3"),
            Some(&Bson::Array(vec![
                Bson::Int32(3),
                Bson::Int32(1),
                Bson::Null
            ]))
        );
        assert_eq!(out[0].get("mx3"), Some(&arr(&[5, 3, 2])));
    }

    #[test]
    fn std_dev_pop_and_samp() {
        // Values 2, 4, 6: mean 4, pop var (4+0+4)/3 = 8/3 -> sqrt ~1.63299;
        // sample var 8/2 = 4 -> 2.0.
        let docs = vec![doc! {"v": 2i32}, doc! {"v": 4i32}, doc! {"v": 6i32}];
        let out = g(
            bson::bson!({"_id": Bson::Null, "p": {"$stdDevPop": "$v"}, "s": {"$stdDevSamp": "$v"}}),
            docs,
        );
        assert_eq!(out[0].get("p"), Some(&Bson::Double((8.0f64 / 3.0).sqrt())));
        assert_eq!(out[0].get("s"), Some(&Bson::Double(2.0)));
        // Single value: pop -> 0.0, samp -> null. All-missing -> field absent.
        let out2 = g(
            bson::bson!({
                "_id": Bson::Null,
                "p": {"$stdDevPop": "$v"}, "s": {"$stdDevSamp": "$v"},
                "m": {"$stdDevPop": "$missing"},
            }),
            vec![doc! {"v": 5i32}],
        );
        assert_eq!(out2[0].get("p"), Some(&Bson::Double(0.0)));
        assert_eq!(out2[0].get("s"), Some(&Bson::Null));
        assert!(out2[0].get("m").is_none());
    }

    #[test]
    fn min_max_cross_type_bson_order() {
        // mongod orders every non-null value by BSON cross-type order
        // (number < string < bool), no longer deferring a mixed group.
        let docs = [
            doc! {"v": 10i32},
            doc! {"v": "x"},
            doc! {"v": true},
            doc! {"v": Bson::Null},
        ];
        let out = group_stage(
            &bson::bson!({"_id": Bson::Null, "mn": {"$min": "$v"}, "mx": {"$max": "$v"}}),
            &docs,
            &Document::new(),
        )
        .unwrap();
        assert_eq!(out[0].get("mn"), Some(&Bson::Int32(10))); // smallest = number
        assert_eq!(out[0].get("mx"), Some(&Bson::Boolean(true))); // largest = bool
    }

    #[test]
    fn sum_avg_ignore_non_numeric() {
        let docs = [
            doc! {"v": 10i32},
            doc! {"v": "hi"},
            doc! {"v": true},
            doc! {"v": Bson::Null},
            doc! {"v": 2.5f64},
        ];
        let out = group_stage(
            &bson::bson!({"_id": Bson::Null, "s": {"$sum": "$v"}, "a": {"$avg": "$v"}}),
            &docs,
            &Document::new(),
        )
        .unwrap();
        assert_eq!(out[0].get("s"), Some(&Bson::Double(12.5))); // 10 + 2.5
        assert_eq!(out[0].get("a"), Some(&Bson::Double(6.25))); // (10 + 2.5) / 2
                                                                // An all-non-numeric group: $sum -> 0, $avg -> null.
        let out2 = group_stage(
            &bson::bson!({"_id": Bson::Null, "s": {"$sum": "$v"}, "a": {"$avg": "$v"}}),
            &[doc! {"v": "x"}, doc! {"v": true}],
            &Document::new(),
        )
        .unwrap();
        assert_eq!(out2[0].get("s"), Some(&Bson::Int32(0)));
        assert_eq!(out2[0].get("a"), Some(&Bson::Null));
    }

    #[test]
    fn add_to_set_dedups_by_equality() {
        let out = g(
            bson::bson!({"_id": Bson::Null, "s": {"$addToSet": "$v"}}),
            vec![doc! {"v": 1i32}, doc! {"v": 1.0f64}, doc! {"v": 2i32}],
        );
        // 1 and 1.0 are `==` in Python -> deduped, first wins.
        assert_eq!(
            out[0].get("s"),
            Some(&Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)]))
        );
    }

    #[test]
    fn sort_by_count_desc_stable() {
        let docs = vec![
            doc! {"k": "a"},
            doc! {"k": "b"},
            doc! {"k": "a"},
            doc! {"k": "c"},
            doc! {"k": "b"},
        ];
        let out = sort_by_count_stage(&bson::bson!("$k"), &docs, &Document::new()).unwrap();
        // a:2, b:2, c:1 -> a, b (insertion-order tie), c
        assert_eq!(out[0].get("_id"), Some(&Bson::String("a".into())));
        assert_eq!(out[1].get("_id"), Some(&Bson::String("b".into())));
        assert_eq!(out[2].get("_id"), Some(&Bson::String("c".into())));
    }
}
