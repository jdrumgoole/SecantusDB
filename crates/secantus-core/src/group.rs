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
use crate::numeric::{self, as_int_like, int_to_bson, NumVal};

type R<T> = Result<T, ()>;

fn eval(expr: &Bson, doc: &Document, vars: &Document) -> R<Bson> {
    expressions::evaluate(doc, expr, vars).map_err(|_| ())
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

/// Running numeric value preserving Python's int-vs-float distinction.
#[derive(Clone, Copy)]
enum Num {
    Int(i128),
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
                    Num::Int(a) => Num::Int(a + n),
                    Num::Float(f) => Num::Float(f + n as f64),
                })
            }
            Bson::Double(d) => Ok(match self {
                Num::Int(a) => Num::Float(a as f64 + d),
                Num::Float(f) => Num::Float(f + d),
            }),
            _ => Err(()), // string / array / doc / Decimal128 / null -> TypeError
        }
    }

    fn to_bson(self) -> R<Bson> {
        match self {
            Num::Int(a) => int_to_bson(a).ok_or(()),
            Num::Float(f) => Ok(Bson::Double(f)),
        }
    }
}

/// One accumulator's running state.
enum Acc {
    Sum(Num),
    Count(i64),
    Avg(Option<(Num, i64)>), // None until the first non-null value (field stays absent)
    Min(Option<Bson>),       // None == "unset or null-equivalent"
    Max(Option<Bson>),
    First(Option<Bson>), // None == not yet seen any doc
    Last(Option<Bson>),
    Push(Vec<Bson>),
    AddToSet(Vec<Bson>),
}

struct Compiled<'a> {
    field: &'a str,
    op: &'a str,
    arg: &'a Bson,
}

fn new_acc(op: &str) -> R<Acc> {
    Ok(match op {
        "$sum" => Acc::Sum(Num::Int(0)),
        "$count" => Acc::Count(0),
        "$avg" => Acc::Avg(None),
        "$min" => Acc::Min(None),
        "$max" => Acc::Max(None),
        "$first" => Acc::First(None),
        "$last" => Acc::Last(None),
        "$push" => Acc::Push(Vec::new()),
        "$addToSet" => Acc::AddToSet(Vec::new()),
        _ => return Err(()), // unsupported accumulator -> Python (raises or handles)
    })
}

fn is_null(b: &Bson) -> bool {
    matches!(b, Bson::Null)
}

/// `arg == 1` in Python — true for int 1, float 1.0, and bool True.
fn arg_is_one(arg: &Bson) -> bool {
    matches!(arg, Bson::Int32(1) | Bson::Int64(1) | Bson::Boolean(true))
        || matches!(arg, Bson::Double(d) if *d == 1.0)
}

fn apply_acc(acc: &mut Acc, arg: &Bson, doc: &Document, vars: &Document) -> R<()> {
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
                    None => (Num::Int(0), 0),
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
        Acc::Push(list) => list.push(eval(arg, doc, vars)?),
        Acc::AddToSet(list) => {
            let v = eval(arg, doc, vars)?;
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
    }
    Ok(())
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
                        Num::Int(a) => {
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
        }
    }
    Ok(out)
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
