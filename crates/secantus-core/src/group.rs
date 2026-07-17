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
//! * **Accumulators** reproduce Python's exact numeric and raise-on-mixed-type
//!   semantics: `$sum`/`$avg` accumulate with Python `+` (int stays int, any
//!   float widens — non-numeric operands `TypeError` → defer); `$min`/`$max`
//!   use native `<`/`>` (via `expressions::py_order`, which raises/`None`s on
//!   the cross-type cases) and so defer rather than guess; `$addToSet`
//!   membership uses Python `==` (`expressions::py_eq`).
//!
//! Any unported / deferring construct returns `Err(())` and the pure-Python
//! `$group` runs instead.

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::{Bson, Document};

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
#[derive(Clone, Copy)]
// `pub(crate)` only because it is reachable through the `pub(crate) Acc` the
// `windowfields` module reuses; not part of any real cross-module API.
pub(crate) enum Num {
    Int { v: i128, wide: bool },
    Float(f64),
}

impl Num {
    /// Python `self + v`. `Err(())` if `v` isn't numeric (Python `int + str`
    /// etc. raises -> defer).
    fn add(self, v: &Bson) -> R<Num> {
        match v {
            Bson::Int32(_) | Bson::Int64(_) | Bson::Boolean(_) => {
                let n = as_int_like(v).unwrap();
                Ok(match self {
                    Num::Int { v: a, wide } => Num::Int {
                        v: a + n,
                        wide: wide || is_int64(v),
                    },
                    Num::Float(f) => Num::Float(f + n as f64),
                })
            }
            Bson::Double(d) => Ok(match self {
                Num::Int { v: a, .. } => Num::Float(a as f64 + d),
                Num::Float(f) => Num::Float(f + d),
            }),
            _ => Err(()), // string / array / doc / Decimal128 / null -> TypeError
        }
    }

    fn to_bson(self) -> R<Bson> {
        match self {
            Num::Int { v, wide } => int_promoted_to_bson(v, wide).ok_or(()),
            Num::Float(f) => Ok(Bson::Double(f)),
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
    Avg(Option<(Num, i64)>), // None until the first non-null value (field stays absent)
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

/// `arg == 1` in Python — true for int 1, float 1.0, and bool True.
fn arg_is_one(arg: &Bson) -> bool {
    matches!(arg, Bson::Int32(1) | Bson::Int64(1) | Bson::Boolean(true))
        || matches!(arg, Bson::Double(d) if *d == 1.0)
}

pub(crate) fn apply_acc(acc: &mut Acc, arg: &Bson, doc: &Document, vars: &Document) -> R<()> {
    match acc {
        Acc::Sum(running) => {
            // `1 if arg == 1 else evaluate(arg)`, then None -> 0.
            if arg_is_one(arg) {
                *running = running.add(&Bson::Int32(1))?;
            } else {
                let v = eval(arg, doc, vars)?;
                if !is_null(&v) {
                    *running = running.add(&v)?;
                }
            }
        }
        Acc::Count(n) => *n += 1,
        Acc::Avg(state) => {
            let v = eval(arg, doc, vars)?;
            if !is_null(&v) {
                let (total, count) = match state.take() {
                    Some(s) => s,
                    None => (Num::Int { v: 0, wide: false }, 0),
                };
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

/// `$min` / `$max`: `cur is None or (v is not None and v <cmp> cur)`. Uses
/// Python's native `<`/`>` (via `py_order`), which raises on cross-type — so an
/// unorderable pair defers the whole stage rather than guessing.
fn update_extreme(cur: &mut Option<Bson>, v: Bson, want: Ordering) -> R<()> {
    if is_null(&v) {
        return Ok(()); // null never updates and never "unsets"
    }
    match cur {
        None => *cur = Some(v),
        Some(existing) => match expressions::py_order(&v, existing).map_err(|_| ())? {
            Some(ord) if ord == want => *cur = Some(v),
            Some(_) => {}
            None => return Err(()), // Python's `v > cur` would TypeError -> defer
        },
    }
    Ok(())
}

fn finalize(id: Bson, accs: Vec<(&str, Acc)>) -> R<Document> {
    let mut out = Document::new();
    out.insert("_id".to_string(), id);
    for (field, acc) in accs {
        match acc {
            Acc::Sum(n) => {
                out.insert(field.to_string(), n.to_bson()?);
            }
            Acc::Count(n) => {
                out.insert(field.to_string(), int_to_bson(n as i128).ok_or(())?);
            }
            Acc::Avg(state) => {
                // Field absent when no non-null value was seen (matches Python,
                // where the bucket key is never created).
                if let Some((total, count)) = state {
                    let tf = match total {
                        Num::Int { v: a, .. } => {
                            if a.unsigned_abs() > (1u128 << 53) {
                                return Err(()); // precision: defer to Python int/int divide
                            }
                            a as f64
                        }
                        Num::Float(f) => f,
                    };
                    out.insert(field.to_string(), Bson::Double(tf / count as f64));
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
        Acc::Sum(n) => n.to_bson()?,
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
pub fn bucket_stage(spec: &Bson, docs: &[Document], vars: &Document) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(());
    };
    let group_by = s.get("groupBy").cloned().unwrap_or(Bson::Null);
    let Some(Bson::Array(boundaries)) = s.get("boundaries") else {
        return Err(()); // missing / non-array boundaries -> Python raises
    };
    if boundaries.len() < 2 {
        return Err(());
    }
    // `default is not None` — an explicit null default counts as absent.
    let default = match s.get("default") {
        None | Some(Bson::Null) => None,
        Some(v) => Some(v.clone()),
    };
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
            if let Some(di) = default_idx {
                placed[di].push(d);
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
pub fn bucket_auto_stage(spec: &Bson, docs: &[Document], vars: &Document) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(());
    };
    let Some(group_by) = s.get("groupBy") else {
        return Err(());
    };
    let n_buckets = match s.get("buckets") {
        Some(Bson::Int32(n)) if *n >= 1 => *n as usize,
        Some(Bson::Int64(n)) if *n >= 1 => *n as usize,
        _ => return Err(()),
    };
    let default_output = bson::doc! {"count": {"$sum": 1i32}};
    let output_spec: &Document = match s.get("output") {
        Some(Bson::Document(d)) if !d.is_empty() => d,
        None | Some(Bson::Document(_)) => &default_output,
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
    fn avg_is_double_and_absent_when_all_null() {
        let out = g(
            bson::bson!({"_id": Bson::Null, "a": {"$avg": "$v"}}),
            vec![doc! {"v": 2i32}, doc! {"v": 4i32}],
        );
        assert_eq!(out[0].get("a"), Some(&Bson::Double(3.0)));
        let out2 = g(
            bson::bson!({"_id": Bson::Null, "a": {"$avg": "$missing"}}),
            vec![doc! {"v": 2i32}],
        );
        assert!(out2[0].get("a").is_none()); // field absent
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
    fn min_cross_type_defers() {
        assert!(group_stage(
            &bson::bson!({"_id": Bson::Null, "m": {"$min": "$v"}}),
            &[doc! {"v": 1i32}, doc! {"v": "x"}],
            &Document::new()
        )
        .is_err());
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
