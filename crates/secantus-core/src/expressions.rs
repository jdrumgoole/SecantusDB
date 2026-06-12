//! Aggregation expression evaluator — Rust port of `secantus.expressions`,
//! the fourth (and largest) leaf engine. Same graceful-fallback design: a
//! coherent high-value core of operators is reproduced byte/value-for-byte, and
//! anything else makes the whole evaluation return `Fallback` so the shim runs
//! pure Python. Because expressions are recursive, an unsupported operator
//! anywhere in the tree bubbles up and defers the entire `evaluate` call.
//!
//! Also used by the query matcher for `$expr` (Rust->Rust, no re-encode).
//!
//! Handled: field paths, `$$var`/`$$ROOT`/`$$CURRENT`, `$literal`; comparison
//! (`$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`); logic (`$and`/`$or`/`$not`); control
//! flow (`$cond`/`$ifNull`/`$switch`); arithmetic (`$add`/`$subtract`/
//! `$multiply`/`$divide`/`$mod`); array ops (`$size`/`$arrayElemAt`/`$first`/
//! `$last`/`$concatArrays`/`$reverseArray`/`$in`/`$slice`/`$indexOfArray`);
//! string ops (`$concat`/`$toLower`/`$toUpper`/`$strLenCP`/`$strLenBytes`/
//! `$split`/`$substrCP`/`$substrBytes`/`$indexOfCP`/`$indexOfBytes`/`$trim`/
//! `$ltrim`/`$rtrim` — ASCII case only, explicit-chars trim only); object ops
//! (`$mergeObjects`/`$objectToArray`/`$getField`/`$setField`); `$zip`; the
//! scope-introducing `$let`/`$map`/`$filter`/`$reduce`; UTC date component
//! extractors (`$year`/`$month`/`$dayOfMonth`/`$hour`/`$minute`/`$second`/
//! `$dayOfWeek`) + `$dateToParts`; date arithmetic (`$dateAdd`/`$dateSubtract`/
//! `$dateDiff`/`$dateTrunc` — UTC, dependency-free calendar math); a safe subset
//! of conversions (`$toInt`/`$toDouble`/`$toBool`/`$toString` for numbers/bools/
//! strings); exactly-deterministic math (`$abs`/`$floor`/`$ceil`/`$sqrt`); and
//! `$range`/`$strLenBytes`/`$arrayToObject`.
//!
//! The remaining operators are *principled* defers — they can't be reproduced
//! without a fidelity risk: regex (`$regexMatch`/…) needs Python's `re`;
//! `$dateToString`/`$dateFromString` need `strftime`/`strptime` + timezones;
//! `$convert`/`$toDecimal` + float-`str()` / string-parse / Decimal128
//! conversions; `$round`/`$pow`/`$trunc` (rounding mode) and transcendentals
//! (`$exp`/`$ln`/`$log`/`$log10` — last-ULP) risk float divergence; `$sortArray`
//! depends on Python's `sorted()` ordering/stability; `$rand` is
//! non-deterministic; and non-ASCII case / default-whitespace trim. All defer
//! to the authoritative pure-Python evaluator.

use std::cmp::Ordering;

use bson::{Bson, Document};

use crate::numeric::{self, as_float_like, as_int_like, int_to_bson};
use crate::paths;

#[derive(Debug)]
pub struct Fallback;

type R = Result<Bson, Fallback>;

struct Ctx<'a> {
    doc: &'a Document,
    vars: &'a Document,
}

/// Evaluate an aggregation expression against `doc` with the given user vars.
pub fn evaluate(doc: &Document, expr: &Bson, vars: &Document) -> R {
    eval(expr, &Ctx { doc, vars })
}

/// MongoDB truthiness (`secantus.expressions._bool` / `query._truthy`): null is
/// false, numbers are nonzero, everything else (incl. strings/arrays/docs/
/// Decimal128) is true.
pub fn truthy(v: &Bson) -> bool {
    match v {
        Bson::Null => false,
        Bson::Boolean(b) => *b,
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Double(d) => *d != 0.0, // NaN -> true (Python bool(nan))
        _ => true,
    }
}

fn eval(expr: &Bson, ctx: &Ctx) -> R {
    match expr {
        Bson::String(s) => {
            if let Some(var) = s.strip_prefix("$$") {
                resolve_var(var, ctx)
            } else if let Some(path) = s.strip_prefix('$') {
                Ok(paths::get_path(ctx.doc, path)
                    .cloned()
                    .unwrap_or(Bson::Null))
            } else {
                Ok(Bson::String(s.clone()))
            }
        }
        Bson::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for e in a {
                out.push(eval(e, ctx)?);
            }
            Ok(Bson::Array(out))
        }
        Bson::Document(d) => {
            if d.len() == 1 {
                let (key, val) = d.iter().next().unwrap();
                if key.starts_with('$') {
                    return apply_op(key, val, ctx);
                }
            }
            let mut out = Document::new();
            for (k, v) in d {
                out.insert(k.clone(), eval(v, ctx)?);
            }
            Ok(Bson::Document(out))
        }
        other => Ok(other.clone()),
    }
}

fn resolve_var(name: &str, ctx: &Ctx) -> R {
    let (base, rest) = match name.split_once('.') {
        Some((b, r)) => (b, Some(r)),
        None => (name, None),
    };
    let value = if let Some(v) = ctx.vars.get(base) {
        v.clone()
    } else if base == "ROOT" || base == "CURRENT" {
        Bson::Document(ctx.doc.clone())
    } else {
        // $$REMOVE / $$KEEP / $$PRUNE / $$DESCEND sentinels (tied to unported
        // $setField/$redact), and undefined vars (Python raises) -> Python.
        return Err(Fallback);
    };
    match rest {
        None => Ok(value),
        Some(path) => match value {
            Bson::Document(d) => Ok(paths::get_path(&d, path).cloned().unwrap_or(Bson::Null)),
            _ => Ok(Bson::Null),
        },
    }
}

/// `_eval_args`: a list evaluates each element; a scalar becomes a 1-element list.
fn eval_args(arg: &Bson, ctx: &Ctx) -> Result<Vec<Bson>, Fallback> {
    match arg {
        Bson::Array(a) => a.iter().map(|e| eval(e, ctx)).collect(),
        other => Ok(vec![eval(other, ctx)?]),
    }
}

fn is_null(b: &Bson) -> bool {
    matches!(b, Bson::Null)
}

fn apply_op(op: &str, arg: &Bson, ctx: &Ctx) -> R {
    match op {
        "$literal" => Ok(arg.clone()),
        // $eq/$ne use Python `==` (total: null==null is true, different types
        // are unequal); the ordering ops use Python `<`/`>` (incomparable —
        // incl. null-vs-null — raises TypeError, caught as false).
        "$eq" => eq_op(arg, ctx, false),
        "$ne" => eq_op(arg, ctx, true),
        "$gt" => ord_op(arg, ctx, |o| o == Ordering::Greater),
        "$gte" => ord_op(arg, ctx, |o| o != Ordering::Less),
        "$lt" => ord_op(arg, ctx, |o| o == Ordering::Less),
        "$lte" => ord_op(arg, ctx, |o| o != Ordering::Greater),
        "$and" => logic(arg, ctx, true),
        "$or" => logic(arg, ctx, false),
        "$not" => {
            let inner = match arg {
                Bson::Array(a) if !a.is_empty() => &a[0],
                other => other,
            };
            Ok(Bson::Boolean(!truthy(&eval(inner, ctx)?)))
        }
        "$cond" => op_cond(arg, ctx),
        "$ifNull" => op_if_null(arg, ctx),
        "$switch" => op_switch(arg, ctx),
        "$add" => arith_nary(arg, ctx, false),
        "$multiply" => arith_nary(arg, ctx, true),
        "$subtract" => op_subtract(arg, ctx),
        "$divide" => op_divide(arg, ctx),
        "$mod" => op_mod(arg, ctx),
        "$size" => op_size(arg, ctx),
        "$arrayElemAt" => op_array_elem_at(arg, ctx),
        "$first" => op_first_last(arg, ctx, true),
        "$last" => op_first_last(arg, ctx, false),
        "$concatArrays" => op_concat_arrays(arg, ctx),
        "$reverseArray" => op_reverse_array(arg, ctx),
        "$in" => op_in(arg, ctx),
        "$slice" => op_slice(arg, ctx),
        "$indexOfArray" => op_index_of_array(arg, ctx),
        // strings
        "$concat" => op_concat(arg, ctx),
        "$toLower" => op_to_case(arg, ctx, false),
        "$toUpper" => op_to_case(arg, ctx, true),
        "$strLenCP" => op_str_len_cp(arg, ctx),
        "$split" => op_split(arg, ctx),
        "$substrCP" | "$substr" => op_substr_cp(arg, ctx),
        "$substrBytes" => op_substr_bytes(arg, ctx),
        "$indexOfCP" => op_index_of(arg, ctx, false),
        "$indexOfBytes" => op_index_of(arg, ctx, true),
        "$trim" => op_trim(arg, ctx, TrimSide::Both),
        "$ltrim" => op_trim(arg, ctx, TrimSide::Left),
        "$rtrim" => op_trim(arg, ctx, TrimSide::Right),
        // objects
        "$mergeObjects" => op_merge_objects(arg, ctx),
        "$objectToArray" => op_object_to_array(arg, ctx),
        "$getField" => op_get_field(arg, ctx),
        "$setField" => op_set_field(arg, ctx),
        "$zip" => op_zip(arg, ctx),
        // scope-introducing
        "$let" => op_let(arg, ctx),
        "$map" => op_map(arg, ctx),
        "$filter" => op_filter(arg, ctx),
        "$reduce" => op_reduce(arg, ctx),
        // date component extractors (UTC; no timezone arg form)
        "$year" => date_part(arg, ctx, DatePart::Year),
        "$month" => date_part(arg, ctx, DatePart::Month),
        "$dayOfMonth" => date_part(arg, ctx, DatePart::Day),
        "$hour" => date_part(arg, ctx, DatePart::Hour),
        "$minute" => date_part(arg, ctx, DatePart::Minute),
        "$second" => date_part(arg, ctx, DatePart::Second),
        "$dayOfWeek" => date_part(arg, ctx, DatePart::DayOfWeek),
        // date arithmetic (UTC; no timezone arg form)
        "$dateAdd" => op_date_add(arg, ctx, 1),
        "$dateSubtract" => op_date_add(arg, ctx, -1),
        "$dateDiff" => op_date_diff(arg, ctx),
        "$dateTrunc" => op_date_trunc(arg, ctx),
        // type conversions (safe subset; Decimal128 / string-parse / float
        // str() defer to Python)
        "$toInt" => op_to_int(arg, ctx),
        "$toDouble" => op_to_double(arg, ctx),
        "$toBool" => op_to_bool(arg, ctx),
        "$toString" => op_to_string(arg, ctx),
        // math (exactly-deterministic only; $round/$pow/$trunc and the
        // transcendentals — exp/ln/log/log10 — are deferred for rounding / ULP
        // fidelity, $sqrt is IEEE exactly-rounded so it's safe)
        "$abs" => op_abs(arg, ctx),
        "$floor" => op_floor_ceil(arg, ctx, false),
        "$ceil" => op_floor_ceil(arg, ctx, true),
        "$sqrt" => op_sqrt(arg, ctx),
        // misc structural / deterministic
        "$dateToParts" => op_date_to_parts(arg, ctx),
        "$range" => op_range(arg, ctx),
        "$strLenBytes" => op_str_len_bytes(arg, ctx),
        "$arrayToObject" => op_array_to_object(arg, ctx),
        _ => Err(Fallback),
    }
}

// --- comparison ---------------------------------------------------------

fn eq_op(arg: &Bson, ctx: &Ctx, negate: bool) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback); // Python unpacks exactly 2 -> ValueError otherwise
    }
    let e = py_eq(&vals[0], &vals[1])?;
    Ok(Bson::Boolean(if negate { !e } else { e }))
}

fn ord_op(arg: &Bson, ctx: &Ctx, pred: fn(Ordering) -> bool) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback);
    }
    Ok(Bson::Boolean(match py_order(&vals[0], &vals[1])? {
        Some(o) => pred(o),
        None => false, // Python `<`/`>` on incomparable operands raises -> False
    }))
}

/// Python ordering (`<`/`>`): `None` when the operands aren't orderable
/// (different types, null, regex — Python raises `TypeError`, caught as false).
/// `Err(Fallback)` for Decimal128 / arrays / docs / exotic (deferred).
pub fn py_order(a: &Bson, b: &Bson) -> Result<Option<Ordering>, Fallback> {
    if matches!(a, Bson::Decimal128(_) | Bson::Array(_) | Bson::Document(_))
        || matches!(b, Bson::Decimal128(_) | Bson::Array(_) | Bson::Document(_))
        || is_exotic(a)
        || is_exotic(b)
    {
        return Err(Fallback);
    }
    let (numa, numb) = (as_num(a), as_num(b));
    if let (Some(na), Some(nb)) = (&numa, &numb) {
        return Ok(numeric::cmp(na, nb));
    }
    if numa.is_some() != numb.is_some() {
        return Ok(None); // numberish vs non-numberish -> TypeError -> False
    }
    Ok(match (a, b) {
        (Bson::String(x), Bson::String(y)) => Some(x.cmp(y)),
        (Bson::DateTime(x), Bson::DateTime(y)) => {
            Some(x.timestamp_millis().cmp(&y.timestamp_millis()))
        }
        (Bson::Timestamp(x), Bson::Timestamp(y)) => {
            Some((x.time, x.increment).cmp(&(y.time, y.increment)))
        }
        (Bson::ObjectId(x), Bson::ObjectId(y)) => Some(x.bytes().cmp(&y.bytes())),
        // Python `Binary < Binary` compares bytes content (subtype ignored).
        (Bson::Binary(x), Bson::Binary(y)) => Some(x.bytes.cmp(&y.bytes)),
        // null-vs-null, regex, and different types are not orderable in Python.
        _ => None,
    })
}

/// numberish view (bool as 0/1) for comparison; `None` if not int/int64/
/// double/bool.
fn as_num(b: &Bson) -> Option<numeric::NumVal> {
    match b {
        Bson::Boolean(v) => Some(numeric::classify(&Bson::Int32(i32::from(*v))).unwrap()),
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => numeric::classify(b),
        _ => None,
    }
}

fn is_exotic(b: &Bson) -> bool {
    matches!(
        b,
        Bson::JavaScriptCode(_)
            | Bson::JavaScriptCodeWithScope(_)
            | Bson::Symbol(_)
            | Bson::DbPointer(_)
            | Bson::Undefined
    )
}

// --- logic / control flow ----------------------------------------------

fn logic(arg: &Bson, ctx: &Ctx, all: bool) -> R {
    let Bson::Array(items) = arg else {
        return Err(Fallback);
    };
    for item in items {
        let t = truthy(&eval(item, ctx)?);
        if all && !t {
            return Ok(Bson::Boolean(false));
        }
        if !all && t {
            return Ok(Bson::Boolean(true));
        }
    }
    Ok(Bson::Boolean(all))
}

fn op_cond(arg: &Bson, ctx: &Ctx) -> R {
    match arg {
        Bson::Document(d) => {
            let (cond, then, els) = (d.get("if"), d.get("then"), d.get("else"));
            let (Some(cond), Some(then), Some(els)) = (cond, then, els) else {
                return Err(Fallback);
            };
            if truthy(&eval(cond, ctx)?) {
                eval(then, ctx)
            } else {
                eval(els, ctx)
            }
        }
        Bson::Array(a) if a.len() == 3 => {
            if truthy(&eval(&a[0], ctx)?) {
                eval(&a[1], ctx)
            } else {
                eval(&a[2], ctx)
            }
        }
        _ => Err(Fallback),
    }
}

fn op_if_null(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(items) = arg else {
        return Err(Fallback);
    };
    if items.len() < 2 {
        return Err(Fallback);
    }
    let (fallback, checks) = items.split_last().unwrap();
    for check in checks {
        let v = eval(check, ctx)?;
        if !is_null(&v) {
            return Ok(v);
        }
    }
    eval(fallback, ctx)
}

fn op_switch(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let Some(Bson::Array(branches)) = d.get("branches") else {
        return Err(Fallback);
    };
    for branch in branches {
        let Bson::Document(b) = branch else {
            return Err(Fallback);
        };
        let (Some(case), Some(then)) = (b.get("case"), b.get("then")) else {
            return Err(Fallback);
        };
        if truthy(&eval(case, ctx)?) {
            return eval(then, ctx);
        }
    }
    match d.get("default") {
        Some(def) => eval(def, ctx),
        None => Err(Fallback), // Python raises when no branch matches and no default
    }
}

// --- arithmetic ---------------------------------------------------------

fn arith_nary(arg: &Bson, ctx: &Ctx, mul: bool) -> R {
    let vals = eval_args(arg, ctx)?;
    if !mul && vals.is_empty() {
        return Err(Fallback); // Python $add of [] indexes values[0] -> IndexError
    }
    if vals.iter().any(is_null) {
        return Ok(Bson::Null);
    }
    // BSON arithmetic rejects bool (mongod: "$multiply only supports
    // numeric types, not bool") — Python raises, so defer instead of
    // folding bools as 0/1 like as_int_like would.
    if vals.iter().any(|v| matches!(v, Bson::Boolean(_))) {
        return Err(Fallback);
    }
    if !mul && vals.len() == 1 {
        // Python returns a single NUMERIC value unchanged; any other
        // single-arg type now raises there ($add type-checks even one
        // operand) -> defer.
        if as_float_like(&vals[0]).is_some() {
            return Ok(vals[0].clone());
        }
        return Err(Fallback);
    }
    fold_arith(&vals, mul)
}

fn fold_arith(vals: &[Bson], mul: bool) -> R {
    // All operands must be numberish (int/int64/double/bool); else defer (Python
    // would do string/list concat or raise on Decimal128/date).
    if vals.iter().all(|v| as_int_like(v).is_some()) {
        let mut acc: i128 = if mul { 1 } else { 0 };
        for v in vals {
            let n = as_int_like(v).unwrap();
            acc = if mul {
                acc.checked_mul(n).ok_or(Fallback)?
            } else {
                acc.checked_add(n).ok_or(Fallback)?
            };
        }
        return int_to_bson(acc).ok_or(Fallback);
    }
    if vals.iter().all(|v| as_float_like(v).is_some()) {
        let mut acc: f64 = if mul { 1.0 } else { 0.0 };
        for v in vals {
            let f = as_float_like(v).unwrap();
            acc = if mul { acc * f } else { acc + f };
        }
        return Ok(Bson::Double(acc));
    }
    Err(Fallback)
}

fn op_subtract(arg: &Bson, ctx: &Ctx) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback);
    }
    if is_null(&vals[0]) || is_null(&vals[1]) {
        return Ok(Bson::Null);
    }
    // bool is not BSON-numeric (Python raises) -> defer.
    if matches!(vals[0], Bson::Boolean(_)) || matches!(vals[1], Bson::Boolean(_)) {
        return Err(Fallback);
    }
    if let (Some(a), Some(b)) = (as_int_like(&vals[0]), as_int_like(&vals[1])) {
        return int_to_bson(a.checked_sub(b).ok_or(Fallback)?).ok_or(Fallback);
    }
    if let (Some(a), Some(b)) = (as_float_like(&vals[0]), as_float_like(&vals[1])) {
        return Ok(Bson::Double(a - b));
    }
    Err(Fallback) // datetime/Decimal128 subtraction -> Python
}

fn op_divide(arg: &Bson, ctx: &Ctx) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback);
    }
    if is_null(&vals[0]) || is_null(&vals[1]) {
        return Ok(Bson::Null);
    }
    // bool is not BSON-numeric (Python raises) -> defer.
    if matches!(vals[0], Bson::Boolean(_)) || matches!(vals[1], Bson::Boolean(_)) {
        return Err(Fallback);
    }
    // Decimal128 division has type-specific semantics -> defer.
    let (Some(a), Some(b)) = (as_float_like(&vals[0]), as_float_like(&vals[1])) else {
        return Err(Fallback);
    };
    if b == 0.0 {
        return Err(Fallback); // Python raises "can't $divide by zero" (code 2)
    }
    Ok(Bson::Double(a / b)) // Python `/` is always float division
}

fn op_mod(arg: &Bson, ctx: &Ctx) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback);
    }
    if is_null(&vals[0]) || is_null(&vals[1]) {
        return Ok(Bson::Null);
    }
    // bool is not BSON-numeric (Python raises) -> defer.
    if matches!(vals[0], Bson::Boolean(_)) || matches!(vals[1], Bson::Boolean(_)) {
        return Err(Fallback);
    }
    // Only integer mod with a positive divisor is reproduced cheaply; Python's
    // float mod and divisor-signed semantics are deferred.
    if let (Some(a), Some(b)) = (as_int_like(&vals[0]), as_int_like(&vals[1])) {
        if b == 0 {
            return Err(Fallback); // Python raises "can't $mod by zero" (16610)
        }
        if b > 0 {
            return int_to_bson(a.rem_euclid(b)).ok_or(Fallback);
        }
    }
    Err(Fallback)
}

// --- array ops ----------------------------------------------------------

fn op_size(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Array(a) => Ok(Bson::Int32(a.len() as i32)),
        _ => Err(Fallback), // Python raises on non-array
    }
}

fn op_array_elem_at(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(pair) = arg else {
        return Err(Fallback);
    };
    if pair.len() != 2 {
        return Err(Fallback);
    }
    let arr = eval(&pair[0], ctx)?;
    let idx = eval(&pair[1], ctx)?;
    let (Bson::Array(a), Some(i)) = (&arr, as_int_like(&idx)) else {
        return Ok(Bson::Null);
    };
    let len = a.len() as i128;
    let resolved = if i < 0 { i + len } else { i };
    if (0..len).contains(&resolved) {
        Ok(a[resolved as usize].clone())
    } else {
        Ok(Bson::Null)
    }
}

fn op_first_last(arg: &Bson, ctx: &Ctx, first: bool) -> R {
    match eval(arg, ctx)? {
        Bson::Array(a) if !a.is_empty() => Ok(if first {
            a[0].clone()
        } else {
            a[a.len() - 1].clone()
        }),
        _ => Ok(Bson::Null),
    }
}

fn op_concat_arrays(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(parts) = arg else {
        return Err(Fallback);
    };
    let mut out: Vec<Bson> = Vec::new();
    for p in parts {
        match eval(p, ctx)? {
            Bson::Array(a) => out.extend(a),
            _ => return Ok(Bson::Null), // any non-array part -> null
        }
    }
    Ok(Bson::Array(out))
}

fn op_reverse_array(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Array(mut a) => {
            a.reverse();
            Ok(Bson::Array(a))
        }
        _ => Ok(Bson::Null),
    }
}

fn op_in(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(pair) = arg else {
        return Err(Fallback);
    };
    if pair.len() != 2 {
        return Err(Fallback);
    }
    let needle = eval(&pair[0], ctx)?;
    let Bson::Array(hay) = eval(&pair[1], ctx)? else {
        return Ok(Bson::Boolean(false));
    };
    for elem in &hay {
        if py_eq(&needle, elem)? {
            return Ok(Bson::Boolean(true));
        }
    }
    Ok(Bson::Boolean(false))
}

// --- more array ops -----------------------------------------------------

/// Python slice index normalisation: negatives count from the end, clamped to
/// `[0, len]`.
fn norm_index(i: i64, len: i64) -> i64 {
    if i < 0 {
        (len + i).max(0)
    } else {
        i.min(len)
    }
}

fn slice_int(b: &Bson) -> Option<i64> {
    as_int_like(b).map(|x| x as i64)
}

fn op_slice(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(a) = arg else {
        return Err(Fallback);
    };
    if a.len() != 2 && a.len() != 3 {
        return Err(Fallback);
    }
    let Bson::Array(arr) = eval(&a[0], ctx)? else {
        return Ok(Bson::Null); // non-array input -> null
    };
    let len = arr.len() as i64;
    let (start, stop) = if a.len() == 2 {
        let Some(n) = slice_int(&eval(&a[1], ctx)?) else {
            return Ok(Bson::Null);
        };
        if n >= 0 {
            (0, n)
        } else {
            (n, len)
        }
    } else {
        let (Some(pos), Some(n)) = (slice_int(&eval(&a[1], ctx)?), slice_int(&eval(&a[2], ctx)?))
        else {
            return Ok(Bson::Null);
        };
        (pos, pos.saturating_add(n))
    };
    let (s, e) = (norm_index(start, len), norm_index(stop, len));
    Ok(Bson::Array(if s >= e {
        Vec::new()
    } else {
        arr[s as usize..e as usize].to_vec()
    }))
}

fn op_index_of_array(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(a) = arg else {
        return Err(Fallback);
    };
    if !(2..=4).contains(&a.len()) {
        return Err(Fallback);
    }
    let arr_v = eval(&a[0], ctx)?;
    if is_null(&arr_v) {
        return Ok(Bson::Null);
    }
    let Bson::Array(arr) = arr_v else {
        return Err(Fallback); // non-array (non-null) -> Python raises
    };
    let needle = eval(&a[1], ctx)?;
    let len = arr.len() as i64;
    let start = if a.len() >= 3 {
        match slice_int(&eval(&a[2], ctx)?) {
            Some(x) => x,
            None => return Ok(Bson::Int32(-1)),
        }
    } else {
        0
    };
    let end = if a.len() >= 4 {
        match slice_int(&eval(&a[3], ctx)?) {
            Some(x) => x,
            None => return Ok(Bson::Int32(-1)),
        }
    } else {
        len
    };
    let mut i = start.max(0);
    let hi = end.min(len);
    while i < hi {
        if py_eq(&arr[i as usize], &needle)? {
            return Ok(Bson::Int32(i as i32));
        }
        i += 1;
    }
    Ok(Bson::Int32(-1))
}

// --- strings ------------------------------------------------------------

fn op_concat(arg: &Bson, ctx: &Ctx) -> R {
    let mut out = String::new();
    for p in eval_args(arg, ctx)? {
        match p {
            Bson::Null => {}
            Bson::String(s) => out.push_str(&s),
            // Python str()-coerces non-strings; the formatting (esp. floats)
            // is risky to reproduce, so defer.
            _ => return Err(Fallback),
        }
    }
    Ok(Bson::String(out))
}

fn op_to_case(arg: &Bson, ctx: &Ctx, upper: bool) -> R {
    match eval(arg, ctx)? {
        Bson::String(s) => {
            if s.is_ascii() {
                Ok(Bson::String(if upper {
                    s.to_ascii_uppercase()
                } else {
                    s.to_ascii_lowercase()
                }))
            } else {
                Err(Fallback) // Unicode case mapping may differ from Python -> defer
            }
        }
        other => Ok(other), // non-string passes through unchanged (matches Python)
    }
}

fn op_str_len_cp(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::String(s) => Ok(Bson::Int32(s.chars().count() as i32)),
        _ => Err(Fallback), // Python raises on non-string
    }
}

fn op_split(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(a) = arg else {
        return Err(Fallback);
    };
    if a.len() != 2 {
        return Err(Fallback);
    }
    let s = eval(&a[0], ctx)?;
    let sep = eval(&a[1], ctx)?;
    if is_null(&s) || is_null(&sep) {
        return Ok(Bson::Null);
    }
    let (Bson::String(s), Bson::String(sep)) = (s, sep) else {
        return Err(Fallback);
    };
    if sep.is_empty() {
        return Err(Fallback); // Python "".split with empty sep raises
    }
    Ok(Bson::Array(
        s.split(sep.as_str())
            .map(|p| Bson::String(p.to_string()))
            .collect(),
    ))
}

fn op_substr_cp(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(a) = arg else {
        return Err(Fallback);
    };
    if a.len() != 3 {
        return Err(Fallback);
    }
    let s = eval(&a[0], ctx)?;
    if is_null(&s) {
        return Ok(Bson::String(String::new())); // Python returns "" for null input
    }
    let Bson::String(s) = s else {
        return Err(Fallback);
    };
    let (Some(start), Some(length)) =
        (slice_int(&eval(&a[1], ctx)?), slice_int(&eval(&a[2], ctx)?))
    else {
        return Err(Fallback);
    };
    let chars: Vec<char> = s.chars().collect();
    let clen = chars.len() as i64;
    let stop = if length < 0 {
        clen
    } else {
        start.saturating_add(length)
    };
    let (s_i, e_i) = (norm_index(start, clen), norm_index(stop, clen));
    let out: String = if s_i >= e_i {
        String::new()
    } else {
        chars[s_i as usize..e_i as usize].iter().collect()
    };
    Ok(Bson::String(out))
}

// --- objects ------------------------------------------------------------

fn op_merge_objects(arg: &Bson, ctx: &Ctx) -> R {
    let items: Vec<&Bson> = match arg {
        Bson::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut result = Document::new();
    for item in items {
        match eval(item, ctx)? {
            Bson::Null => {}
            Bson::Document(d) => {
                for (k, v) in d {
                    result.insert(k, v);
                }
            }
            _ => return Err(Fallback), // Python raises on non-document arg
        }
    }
    Ok(Bson::Document(result))
}

/// Python `bool()` of a *literal* spec value (not evaluated): empty
/// collections / strings, zero, false, null, and a missing key are all falsy.
fn py_bool_literal(v: Option<&Bson>) -> bool {
    match v {
        None | Some(Bson::Null) => false,
        Some(Bson::Boolean(b)) => *b,
        Some(Bson::Int32(n)) => *n != 0,
        Some(Bson::Int64(n)) => *n != 0,
        Some(Bson::Double(d)) => *d != 0.0,
        Some(Bson::String(s)) => !s.is_empty(),
        Some(Bson::Array(a)) => !a.is_empty(),
        Some(Bson::Document(d)) => !d.is_empty(),
        Some(_) => true,
    }
}

fn op_get_field(arg: &Bson, ctx: &Ctx) -> R {
    // `field` is taken literally (the whole point of $getField vs `$path` is
    // dotted/dollared field names); `input` defaults to $$CURRENT (the doc).
    let (field, input) = match arg {
        Bson::String(s) => (s.clone(), Bson::Document(ctx.doc.clone())),
        Bson::Document(d) => {
            let Some(fe) = d.get("field") else {
                return Err(Fallback); // Python raises when field is absent
            };
            let Bson::String(field) = eval(fe, ctx)? else {
                return Err(Fallback); // field must evaluate to a string
            };
            let input = match d.get("input") {
                Some(e) => eval(e, ctx)?,
                None => Bson::Document(ctx.doc.clone()),
            };
            (field, input)
        }
        _ => return Err(Fallback),
    };
    Ok(match input {
        Bson::Document(doc) => doc.get(&field).cloned().unwrap_or(Bson::Null),
        _ => Bson::Null, // null / non-document input -> None
    })
}

fn op_set_field(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let (Some(fe), Some(ie), Some(ve)) = (d.get("field"), d.get("input"), d.get("value")) else {
        return Err(Fallback); // field/input/value all required
    };
    let Bson::String(field) = eval(fe, ctx)? else {
        return Err(Fallback);
    };
    let input = eval(ie, ctx)?;
    if is_null(&input) {
        return Ok(Bson::Null);
    }
    let Bson::Document(mut doc) = input else {
        return Err(Fallback); // non-document input -> Python raises
    };
    // value $$REMOVE makes eval defer (resolve_var returns Fallback), so the
    // field-drop case is handled by the pure-Python path.
    let value = eval(ve, ctx)?;
    doc.insert(field, value);
    Ok(Bson::Document(doc))
}

fn op_zip(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let Some(inputs_expr) = d.get("inputs") else {
        return Err(Fallback);
    };
    let inputs_val = eval(inputs_expr, ctx)?;
    if is_null(&inputs_val) {
        return Ok(Bson::Null);
    }
    let Bson::Array(inputs) = inputs_val else {
        return Err(Fallback);
    };
    let mut arrs: Vec<&Vec<Bson>> = Vec::with_capacity(inputs.len());
    for a in &inputs {
        match a {
            Bson::Array(v) => arrs.push(v),
            _ => return Err(Fallback), // inputs must be an array of arrays
        }
    }
    let n_inputs = arrs.len();
    // useLongestLength / defaults are read as *literals* (Python doesn't eval).
    let use_longest = py_bool_literal(d.get("useLongestLength"));
    let defaults: Vec<Bson> = match d.get("defaults") {
        Some(dv) if py_bool_literal(Some(dv)) => match dv {
            Bson::Array(dl) => dl.clone(),
            _ => return Err(Fallback), // truthy non-list -> Python raises
        },
        _ => vec![Bson::Null; n_inputs],
    };
    let mut out: Vec<Bson> = Vec::new();
    if use_longest {
        let n = arrs.iter().map(|a| a.len()).max().unwrap_or(0);
        for i in 0..n {
            let row: Vec<Bson> = arrs
                .iter()
                .enumerate()
                .map(|(j, a)| {
                    a.get(i)
                        .cloned()
                        .unwrap_or_else(|| defaults.get(j).cloned().unwrap_or(Bson::Null))
                })
                .collect();
            out.push(Bson::Array(row));
        }
    } else {
        let n = arrs.iter().map(|a| a.len()).min().unwrap_or(0);
        for i in 0..n {
            out.push(Bson::Array(arrs.iter().map(|a| a[i].clone()).collect()));
        }
    }
    Ok(Bson::Array(out))
}

fn op_object_to_array(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::Document(d) => {
            let arr = d
                .into_iter()
                .map(|(k, v)| {
                    let mut e = Document::new();
                    e.insert("k".to_string(), Bson::String(k));
                    e.insert("v".to_string(), v);
                    Bson::Document(e)
                })
                .collect();
            Ok(Bson::Array(arr))
        }
        _ => Err(Fallback), // Python raises on non-document
    }
}

// --- scope-introducing ops ($let / $map / $filter / $reduce) ------------

/// Evaluate `expr` with extra `(name, value)` variable bindings layered on top
/// of `ctx`'s scope (mirrors `_Ctx.with_var`). The document (so `$$ROOT` and
/// bare `$field` paths) is unchanged — only the var scope grows.
fn eval_with_vars(expr: &Bson, ctx: &Ctx, extra: &[(&str, Bson)]) -> R {
    let mut vars = ctx.vars.clone();
    for (name, value) in extra {
        vars.insert((*name).to_string(), value.clone());
    }
    eval(
        expr,
        &Ctx {
            doc: ctx.doc,
            vars: &vars,
        },
    )
}

/// The `as` variable name for `$map`/`$filter` (default `"this"`); a non-string
/// `as` defers.
fn as_var_name(spec: Option<&Bson>) -> Result<String, Fallback> {
    match spec {
        None => Ok("this".to_string()),
        Some(Bson::String(s)) => Ok(s.clone()),
        Some(_) => Err(Fallback),
    }
}

fn op_let(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let (Some(Bson::Document(bindings)), Some(in_expr)) = (d.get("vars"), d.get("in")) else {
        return Err(Fallback); // Python requires {vars, in} with vars a document
    };
    // Each binding is evaluated against the *original* scope (bindings don't see
    // each other), then all are layered for the `in` expression.
    let mut vars = ctx.vars.clone();
    for (name, vexpr) in bindings {
        let v = eval(vexpr, ctx)?;
        vars.insert(name.clone(), v);
    }
    eval(
        in_expr,
        &Ctx {
            doc: ctx.doc,
            vars: &vars,
        },
    )
}

fn op_map(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let Bson::Array(arr) = eval_opt(d.get("input"), ctx)? else {
        return Ok(Bson::Null); // non-array input -> null
    };
    let var = as_var_name(d.get("as"))?;
    let null = Bson::Null;
    let in_expr = d.get("in").unwrap_or(&null);
    let mut out = Vec::with_capacity(arr.len());
    for elem in arr {
        out.push(eval_with_vars(in_expr, ctx, &[(&var, elem)])?);
    }
    Ok(Bson::Array(out))
}

fn op_filter(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let Bson::Array(arr) = eval_opt(d.get("input"), ctx)? else {
        return Ok(Bson::Null);
    };
    let var = as_var_name(d.get("as"))?;
    let null = Bson::Null;
    let cond = d.get("cond").unwrap_or(&null);
    // `limit` only bounds the output when it evaluates to an integer.
    let limit = match d.get("limit") {
        Some(e) => as_int_like(&eval(e, ctx)?),
        None => None,
    };
    let mut out: Vec<Bson> = Vec::new();
    for elem in arr {
        if truthy(&eval_with_vars(cond, ctx, &[(&var, elem.clone())])?) {
            out.push(elem);
            if let Some(lim) = limit {
                if out.len() as i128 >= lim {
                    break;
                }
            }
        }
    }
    Ok(Bson::Array(out))
}

fn op_reduce(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let Bson::Array(arr) = eval_opt(d.get("input"), ctx)? else {
        return Ok(Bson::Null);
    };
    let mut acc = eval_opt(d.get("initialValue"), ctx)?;
    let null = Bson::Null;
    let in_expr = d.get("in").unwrap_or(&null);
    for elem in arr {
        acc = eval_with_vars(in_expr, ctx, &[("value", acc), ("this", elem)])?;
    }
    Ok(acc)
}

/// Evaluate an optional sub-expression; a missing key behaves like Python's
/// `_eval(None)` (yields null).
fn eval_opt(expr: Option<&Bson>, ctx: &Ctx) -> R {
    match expr {
        Some(e) => eval(e, ctx),
        None => Ok(Bson::Null),
    }
}

// --- date component extractors (UTC) ------------------------------------

#[derive(Clone, Copy)]
enum DatePart {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    DayOfWeek,
}

/// Civil (year, month, day) from a count of days since 1970-01-01 (Howard
/// Hinnant's algorithm), valid for the full BSON date range.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

fn date_part(arg: &Bson, ctx: &Ctx, part: DatePart) -> R {
    let Bson::DateTime(dt) = eval(arg, ctx)? else {
        return Ok(Bson::Null); // Python returns None for non-datetime
    };
    let millis = dt.timestamp_millis();
    let days = millis.div_euclid(86_400_000);
    let ms_of_day = millis.rem_euclid(86_400_000);
    let value = match part {
        DatePart::Hour => ms_of_day / 3_600_000,
        DatePart::Minute => (ms_of_day / 60_000) % 60,
        DatePart::Second => (ms_of_day / 1000) % 60,
        // mongod $dayOfWeek: Sunday=1 .. Saturday=7 (1970-01-01 was Thursday=5).
        DatePart::DayOfWeek => (days + 4).rem_euclid(7) + 1,
        _ => {
            let (y, m, d) = civil_from_days(days);
            match part {
                DatePart::Year => y,
                DatePart::Month => m,
                DatePart::Day => d,
                _ => unreachable!(),
            }
        }
    };
    Ok(Bson::Int32(value as i32))
}

// --- date arithmetic ($dateAdd / $dateSubtract / $dateDiff / $dateTrunc) -

/// Inverse of `civil_from_days` (Howard Hinnant): days since 1970-01-01 for a
/// proleptic-Gregorian (year, month, day).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        _ => 28,
    }
}

// Python `datetime` range in BSON milliseconds: [0001-01-01, 9999-12-31
// 23:59:59.999]. Date arithmetic landing outside defers to Python (which raises
// OverflowError / ValueError).
const DATETIME_MIN_MS: i64 = -62_135_596_800_000;
const DATETIME_MAX_MS: i64 = 253_402_300_799_999;

fn bounded_datetime(millis: i128) -> R {
    if (DATETIME_MIN_MS as i128..=DATETIME_MAX_MS as i128).contains(&millis) {
        Ok(Bson::DateTime(bson::DateTime::from_millis(millis as i64)))
    } else {
        Err(Fallback)
    }
}

fn shift_ms(start: i64, amount: i128, unit_ms: i128) -> R {
    bounded_datetime(start as i128 + amount * unit_ms)
}

fn add_months(start: i64, months: i128) -> R {
    let days = start.div_euclid(86_400_000);
    let ms_of_day = start.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let total = (m - 1) as i128 + months;
    let new_year = y as i128 + total.div_euclid(12);
    let new_month = total.rem_euclid(12) + 1; // [1, 12]
    if !(1..=9999).contains(&new_year) {
        return Err(Fallback); // Python datetime year out of range
    }
    let last = days_in_month(new_year as i64, new_month as i64);
    let new_day = d.min(last);
    let new_days = days_from_civil(new_year as i64, new_month as i64, new_day);
    bounded_datetime(new_days as i128 * 86_400_000 + ms_of_day as i128)
}

/// `_shift_date`: positive `amount` adds; an unsupported unit defers.
fn shift_date(start: i64, unit: &str, amount: i128) -> R {
    match unit {
        "year" => add_months(start, amount * 12),
        "quarter" => add_months(start, amount * 3),
        "month" => add_months(start, amount),
        "week" => shift_ms(start, amount, 604_800_000),
        "day" => shift_ms(start, amount, 86_400_000),
        "hour" => shift_ms(start, amount, 3_600_000),
        "minute" => shift_ms(start, amount, 60_000),
        "second" => shift_ms(start, amount, 1000),
        "millisecond" => shift_ms(start, amount, 1),
        _ => Err(Fallback),
    }
}

/// `$dateAdd` (sign +1) / `$dateSubtract` (sign -1).
fn op_date_add(arg: &Bson, ctx: &Ctx, sign: i128) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let start = eval_opt(d.get("startDate"), ctx)?;
    let amount_v = eval_opt(d.get("amount"), ctx)?;
    if is_null(&start) || is_null(&amount_v) {
        return Ok(Bson::Null);
    }
    let Bson::DateTime(start_dt) = start else {
        return Err(Fallback); // not a datetime -> Python raises
    };
    let Bson::String(unit) = eval_opt(d.get("unit"), ctx)? else {
        return Err(Fallback); // unit must be a string
    };
    let Some(amount) = as_int_like(&amount_v) else {
        return Err(Fallback); // amount must be an integer
    };
    shift_date(start_dt.timestamp_millis(), &unit, sign * amount)
}

fn op_date_diff(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let start = eval_opt(d.get("startDate"), ctx)?;
    let end = eval_opt(d.get("endDate"), ctx)?;
    if is_null(&start) || is_null(&end) {
        return Ok(Bson::Null);
    }
    let (Bson::DateTime(s), Bson::DateTime(e)) = (start, end) else {
        return Err(Fallback);
    };
    let Bson::String(unit) = eval_opt(d.get("unit"), ctx)? else {
        return Err(Fallback);
    };
    let (sm, em) = (s.timestamp_millis(), e.timestamp_millis());
    let (sy, smo, sd) = civil_from_days(sm.div_euclid(86_400_000));
    let (ey, emo, ed) = civil_from_days(em.div_euclid(86_400_000));
    let dms = (em - sm) as i128;
    let value: i128 = match unit.as_str() {
        "year" => (ey - sy - i64::from((emo, ed) < (smo, sd))) as i128,
        "quarter" => {
            let (sq, eq) = ((smo - 1) / 3, (emo - 1) / 3);
            ((ey - sy) * 4 + (eq - sq)) as i128
        }
        "month" => ((ey - sy) * 12 + (emo - smo) - i64::from(ed < sd)) as i128,
        // day/week use the integer `timedelta.days` (floor). The sub-day units
        // go through Python's lossy `timedelta.total_seconds()`, which is
        // `total_microseconds / 10**6` — an int/int *correctly-rounded* true
        // division — then `// n` (floor) for hour/minute and `int(...)`
        // (truncate toward zero) for second/ms. We reproduce that float path so
        // the last-digit rounding matches. The `total_us as f64` conversion is
        // exact only while `|total_us| <= 2**53`; beyond that a second rounding
        // could diverge from CPython's single correctly-rounded int/int divide,
        // so we defer extreme dates to Python.
        "day" => dms.div_euclid(86_400_000),
        "week" => dms.div_euclid(86_400_000).div_euclid(7),
        "hour" | "minute" | "second" | "millisecond" => {
            let total_us = dms * 1000;
            if total_us.unsigned_abs() > (1u128 << 53) {
                return Err(Fallback);
            }
            let ts = total_us as f64 / 1_000_000.0;
            match unit.as_str() {
                "hour" => (ts / 3600.0).floor() as i128,
                "minute" => (ts / 60.0).floor() as i128,
                "second" => ts.trunc() as i128,
                _ => (ts * 1000.0).trunc() as i128, // millisecond
            }
        }
        _ => return Err(Fallback),
    };
    int_to_bson(value).ok_or(Fallback)
}

fn op_date_trunc(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let date = eval_opt(d.get("date"), ctx)?;
    if is_null(&date) {
        return Ok(Bson::Null);
    }
    let Bson::DateTime(dt) = date else {
        return Err(Fallback);
    };
    let Bson::String(unit) = eval_opt(d.get("unit"), ctx)? else {
        return Err(Fallback);
    };
    let bin: i64 = match d.get("binSize") {
        Some(e) => match as_int_like(&eval(e, ctx)?) {
            Some(n) if n >= 1 => n as i64,
            _ => return Err(Fallback), // binSize must be a positive integer
        },
        None => 1,
    };
    let millis = dt.timestamp_millis();
    let days = millis.div_euclid(86_400_000);
    let ms_of_day = millis.rem_euclid(86_400_000);
    let (y, m, _d) = civil_from_days(days);
    let result: i128 = match unit.as_str() {
        "year" => {
            let ny = y - (y - 1).rem_euclid(bin);
            days_from_civil(ny, 1, 1) as i128 * 86_400_000
        }
        "quarter" => {
            let qi = (m - 1) / 3;
            let qi = qi - qi.rem_euclid(bin);
            days_from_civil(y, qi * 3 + 1, 1) as i128 * 86_400_000
        }
        "month" => {
            let nm = m - (m - 1).rem_euclid(bin);
            days_from_civil(y, nm, 1) as i128 * 86_400_000
        }
        "week" => {
            let epoch = days_from_civil(1970, 1, 5); // a Monday
            let wd = (days - epoch).div_euclid(7);
            let wd = wd - wd.rem_euclid(bin);
            (epoch as i128 + wd as i128 * 7) * 86_400_000
        }
        "day" => {
            let dd = days - days.rem_euclid(bin);
            dd as i128 * 86_400_000
        }
        // Python truncates the *field* (keeping the higher fields) via
        // date.replace, not the total count since midnight.
        "hour" => {
            let hf = ms_of_day / 3_600_000;
            let nh = hf - hf % bin;
            days as i128 * 86_400_000 + nh as i128 * 3_600_000
        }
        "minute" => {
            let hf = ms_of_day / 3_600_000;
            let mf = (ms_of_day / 60_000) % 60;
            let nm = mf - mf % bin;
            days as i128 * 86_400_000 + hf as i128 * 3_600_000 + nm as i128 * 60_000
        }
        "second" => {
            let hf = ms_of_day / 3_600_000;
            let mf = (ms_of_day / 60_000) % 60;
            let sf = (ms_of_day / 1000) % 60;
            let ns = sf - sf % bin;
            days as i128 * 86_400_000
                + hf as i128 * 3_600_000
                + mf as i128 * 60_000
                + ns as i128 * 1000
        }
        "millisecond" => {
            let sub = ms_of_day % 1000;
            (millis - (sub % bin)) as i128
        }
        _ => return Err(Fallback),
    };
    bounded_datetime(result)
}

// --- type conversions ---------------------------------------------------

fn op_to_int(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        // Python returns an already-int value unchanged (preserving int32 vs
        // int64), rather than re-widening.
        v @ (Bson::Int32(_) | Bson::Int64(_)) => Ok(v),
        Bson::Boolean(b) => Ok(Bson::Int32(i32::from(b))),
        Bson::Double(d) => {
            if !d.is_finite() {
                return Err(Fallback); // Python int(nan/inf) raises
            }
            let t = d.trunc();
            if t < i64::MIN as f64 || t > i64::MAX as f64 {
                return Err(Fallback);
            }
            int_to_bson(t as i128).ok_or(Fallback)
        }
        // Decimal128 / string parsing -> Python (edge-case-prone).
        _ => Err(Fallback),
    }
}

fn op_to_double(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::Boolean(b) => Ok(Bson::Double(if b { 1.0 } else { 0.0 })),
        Bson::Int32(n) => Ok(Bson::Double(n as f64)),
        Bson::Int64(n) => Ok(Bson::Double(n as f64)),
        v @ Bson::Double(_) => Ok(v),
        _ => Err(Fallback), // Decimal128 / string parsing -> Python
    }
}

fn op_to_bool(arg: &Bson, ctx: &Ctx) -> R {
    Ok(match eval(arg, ctx)? {
        Bson::Null => Bson::Null,
        Bson::Boolean(b) => Bson::Boolean(b),
        Bson::Int32(n) => Bson::Boolean(n != 0),
        Bson::Int64(n) => Bson::Boolean(n != 0),
        Bson::Double(d) => Bson::Boolean(d != 0.0), // NaN -> true
        Bson::String(s) => Bson::Boolean(!s.is_empty()),
        Bson::Decimal128(_) => return Err(Fallback),
        // Python: every other type is truthy.
        _ => Bson::Boolean(true),
    })
}

fn op_to_string(arg: &Bson, ctx: &Ctx) -> R {
    Ok(match eval(arg, ctx)? {
        Bson::Null => Bson::Null,
        Bson::Int32(n) => Bson::String(n.to_string()),
        Bson::Int64(n) => Bson::String(n.to_string()),
        // Python str(True) == "True" (capitalised — this impl uses str(), not
        // mongod's lowercase). Reproduce that exactly.
        Bson::Boolean(b) => Bson::String(if b { "True" } else { "False" }.to_string()),
        v @ Bson::String(_) => v,
        // float str() / Decimal128 / datetime isoformat / ObjectId etc. -> Python.
        _ => return Err(Fallback),
    })
}

// --- math ---------------------------------------------------------------

fn op_abs(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::Int32(n) => int_to_bson((n as i128).abs()).ok_or(Fallback),
        Bson::Int64(n) => int_to_bson((n as i128).abs()).ok_or(Fallback),
        Bson::Boolean(b) => Ok(Bson::Int32(i32::from(b))),
        Bson::Double(d) => Ok(Bson::Double(d.abs())),
        _ => Err(Fallback), // Decimal128 / non-numeric: Python abs() raises
    }
}

fn op_floor_ceil(arg: &Bson, ctx: &Ctx, ceil: bool) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        // math.floor/ceil of an int returns it unchanged.
        v @ (Bson::Int32(_) | Bson::Int64(_)) => Ok(v),
        Bson::Boolean(b) => Ok(Bson::Int32(i32::from(b))),
        Bson::Double(d) => {
            if !d.is_finite() {
                return Err(Fallback); // math.floor(nan/inf) raises
            }
            let r = if ceil { d.ceil() } else { d.floor() };
            if r < i64::MIN as f64 || r > i64::MAX as f64 {
                return Err(Fallback);
            }
            int_to_bson(r as i128).ok_or(Fallback)
        }
        _ => Err(Fallback),
    }
}

fn op_sqrt(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        v => {
            let Some(f) = as_float_like(&v) else {
                return Err(Fallback); // Decimal128 / non-numeric -> Python
            };
            // Python: math.sqrt(v) if v >= 0 else None. NaN >= 0 is false -> None.
            if f >= 0.0 {
                Ok(Bson::Double(f.sqrt()))
            } else {
                Ok(Bson::Null)
            }
        }
    }
}

// --- $dateToParts (UTC; ignores timezone/iso8601 like the Python impl) ---

fn op_date_to_parts(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    match eval_opt(d.get("date"), ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::DateTime(dt) => {
            let millis = dt.timestamp_millis();
            let days = millis.div_euclid(86_400_000);
            let ms = millis.rem_euclid(86_400_000);
            let (y, m, dy) = civil_from_days(days);
            let mut out = Document::new();
            out.insert("year".to_string(), Bson::Int32(y as i32));
            out.insert("month".to_string(), Bson::Int32(m as i32));
            out.insert("day".to_string(), Bson::Int32(dy as i32));
            out.insert("hour".to_string(), Bson::Int32((ms / 3_600_000) as i32));
            out.insert(
                "minute".to_string(),
                Bson::Int32(((ms / 60_000) % 60) as i32),
            );
            out.insert("second".to_string(), Bson::Int32(((ms / 1000) % 60) as i32));
            out.insert("millisecond".to_string(), Bson::Int32((ms % 1000) as i32));
            Ok(Bson::Document(out))
        }
        _ => Err(Fallback),
    }
}

// --- $range -------------------------------------------------------------

const MAX_RANGE_SIZE: i128 = 100_000;

fn range_int(b: &Bson) -> Result<i64, Fallback> {
    // Python requires int and not bool.
    match b {
        Bson::Int32(n) => Ok(*n as i64),
        Bson::Int64(n) => Ok(*n),
        _ => Err(Fallback),
    }
}

fn op_range(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(a) = arg else {
        return Err(Fallback);
    };
    if !(2..=3).contains(&a.len()) {
        return Err(Fallback);
    }
    let start = range_int(&eval(&a[0], ctx)?)? as i128;
    let end = range_int(&eval(&a[1], ctx)?)? as i128;
    let step = if a.len() == 3 {
        range_int(&eval(&a[2], ctx)?)? as i128
    } else {
        1
    };
    if step == 0 {
        return Err(Fallback);
    }
    let delta = end - start;
    if (delta > 0) == (step > 0) && delta != 0 {
        let size = (delta.abs() + step.abs() - 1) / step.abs();
        if size > MAX_RANGE_SIZE {
            return Err(Fallback); // Python raises past the cap
        }
    }
    let mut out = Vec::new();
    let mut i = start;
    while (step > 0 && i < end) || (step < 0 && i > end) {
        out.push(int_to_bson(i).ok_or(Fallback)?);
        i += step;
    }
    Ok(Bson::Array(out))
}

fn op_str_len_bytes(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::String(s) => Ok(Bson::Int32(s.len() as i32)), // UTF-8 byte length
        _ => Err(Fallback),
    }
}

// --- string index / substr / trim --------------------------------------

fn find_slice<T: PartialEq>(hay: &[T], needle: &[T]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Emulate Python `str.find(sub, start, end)` (or `bytes.find`). CPython's
/// `adjust_indices` clamps `end` to `[0, len]` but only floors `start` at 0 —
/// it is NOT capped at `len` (unlike slice indexing), so `start > end` (incl.
/// `start > len`) yields -1 even for an empty needle. The match must lie within
/// `[start, end)`; the result is an index in the original sequence (or -1).
fn index_of_window<T: PartialEq>(hay: &[T], needle: &[T], start: i64, end: i64) -> i32 {
    let n = hay.len() as i64;
    let e = if end > n {
        n
    } else if end < 0 {
        (end + n).max(0)
    } else {
        end
    };
    let s = if start < 0 { (start + n).max(0) } else { start };
    if s > e {
        return -1; // empty/invalid window -> not found (matches CPython)
    }
    match find_slice(&hay[s as usize..e as usize], needle) {
        Some(r) => (s + r as i64) as i32,
        None => -1,
    }
}

fn op_index_of(arg: &Bson, ctx: &Ctx, bytes: bool) -> R {
    let Bson::Array(a) = arg else {
        return Err(Fallback);
    };
    if !(2..=4).contains(&a.len()) {
        return Err(Fallback);
    }
    let s = eval(&a[0], ctx)?;
    if is_null(&s) {
        return Ok(Bson::Null);
    }
    let (Bson::String(s), Bson::String(needle)) = (s, eval(&a[1], ctx)?) else {
        return Err(Fallback); // non-string operands -> Python raises
    };
    let len = if bytes { s.len() } else { s.chars().count() } as i64;
    // start/end: Python isinstance(int) (incl bool); non-int -> -1.
    let bound = |idx: usize, default: i64, ctx: &Ctx| -> Result<Option<i64>, Fallback> {
        if a.len() <= idx {
            return Ok(Some(default));
        }
        Ok(slice_int(&eval(&a[idx], ctx)?))
    };
    let (Some(start), Some(end)) = (bound(2, 0, ctx)?, bound(3, len, ctx)?) else {
        return Ok(Bson::Int32(-1));
    };
    let idx = if bytes {
        index_of_window(s.as_bytes(), needle.as_bytes(), start, end)
    } else {
        let hay: Vec<char> = s.chars().collect();
        let nd: Vec<char> = needle.chars().collect();
        index_of_window(&hay, &nd, start, end)
    };
    Ok(Bson::Int32(idx))
}

fn op_substr_bytes(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(a) = arg else {
        return Err(Fallback);
    };
    if a.len() != 3 {
        return Err(Fallback);
    }
    let s = eval(&a[0], ctx)?;
    if is_null(&s) {
        return Ok(Bson::String(String::new()));
    }
    let Bson::String(s) = s else {
        return Err(Fallback);
    };
    let (Some(start), Some(length)) =
        (slice_int(&eval(&a[1], ctx)?), slice_int(&eval(&a[2], ctx)?))
    else {
        return Err(Fallback);
    };
    let bytes = s.as_bytes();
    let blen = bytes.len() as i64;
    let stop = if length < 0 {
        blen
    } else {
        start.saturating_add(length)
    };
    let (s_i, e_i) = (norm_index(start, blen), norm_index(stop, blen));
    let slice: &[u8] = if s_i >= e_i {
        &[]
    } else {
        &bytes[s_i as usize..e_i as usize]
    };
    // Python decodes with errors='replace'; we only handle clean UTF-8 and defer
    // on a broken boundary (replacement granularity can differ from Python's).
    match std::str::from_utf8(slice) {
        Ok(st) => Ok(Bson::String(st.to_string())),
        Err(_) => Err(Fallback),
    }
}

#[derive(Clone, Copy)]
enum TrimSide {
    Both,
    Left,
    Right,
}

fn op_trim(arg: &Bson, ctx: &Ctx, side: TrimSide) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let input = eval_opt(d.get("input"), ctx)?;
    if is_null(&input) {
        return Ok(Bson::Null);
    }
    let Bson::String(s) = input else {
        return Err(Fallback); // non-string input -> Python raises
    };
    // Only the explicit `chars`-string form is reproduced; the default
    // whitespace strip (Python `str.strip()`) defers — Python's whitespace set
    // differs from Rust's at the edges (e.g. U+001C..U+001F).
    let chars = match d.get("chars") {
        Some(e) => eval(e, ctx)?,
        None => return Err(Fallback),
    };
    let Bson::String(chars) = chars else {
        return Err(Fallback);
    };
    let pat = |c: char| chars.contains(c);
    let trimmed = match side {
        TrimSide::Both => s.trim_matches(pat),
        TrimSide::Left => s.trim_start_matches(pat),
        TrimSide::Right => s.trim_end_matches(pat),
    };
    Ok(Bson::String(trimmed.to_string()))
}

fn op_array_to_object(arg: &Bson, ctx: &Ctx) -> R {
    let entries = match eval(arg, ctx)? {
        Bson::Null => return Ok(Bson::Null),
        Bson::Array(a) => a,
        _ => return Err(Fallback),
    };
    let mut out = Document::new();
    for e in entries {
        match e {
            Bson::Document(d) => {
                let (Some(Bson::String(k)), Some(v)) = (d.get("k"), d.get("v")) else {
                    return Err(Fallback); // missing k/v or non-string key -> Python
                };
                out.insert(k.clone(), v.clone());
            }
            Bson::Array(pair) if pair.len() == 2 => {
                let Bson::String(k) = &pair[0] else {
                    return Err(Fallback);
                };
                out.insert(k.clone(), pair[1].clone());
            }
            _ => return Err(Fallback),
        }
    }
    Ok(Bson::Document(out))
}

/// Python `==` (used by `$in` membership and `$eq` element semantics, and by
/// the diff engine): numbers bridge with bool-as-int, strings/null/oid/date/etc.
/// by type, arrays/docs structurally. `Err(Fallback)` for Decimal128
/// (uncertain) and exotic types.
pub fn py_eq(a: &Bson, b: &Bson) -> Result<bool, Fallback> {
    if matches!(a, Bson::Decimal128(_))
        || matches!(b, Bson::Decimal128(_))
        || is_exotic(a)
        || is_exotic(b)
    {
        return Err(Fallback);
    }
    if let (Some(na), Some(nb)) = (as_num(a), as_num(b)) {
        return Ok(numeric::eq(&na, &nb));
    }
    if as_num(a).is_some() != as_num(b).is_some() {
        return Ok(false);
    }
    Ok(match (a, b) {
        (Bson::Null, Bson::Null) => true,
        (Bson::String(x), Bson::String(y)) => x == y,
        (Bson::ObjectId(x), Bson::ObjectId(y)) => x == y,
        (Bson::DateTime(x), Bson::DateTime(y)) => x == y,
        (Bson::Timestamp(x), Bson::Timestamp(y)) => x == y,
        (Bson::Binary(x), Bson::Binary(y)) => x.subtype == y.subtype && x.bytes == y.bytes,
        (Bson::RegularExpression(x), Bson::RegularExpression(y)) => {
            x.pattern == y.pattern && x.options == y.options
        }
        (Bson::Array(x), Bson::Array(y)) => {
            if x.len() != y.len() {
                false
            } else {
                for (xe, ye) in x.iter().zip(y) {
                    if !py_eq(xe, ye)? {
                        return Ok(false);
                    }
                }
                true
            }
        }
        (Bson::Document(x), Bson::Document(y)) => {
            if x.len() != y.len() {
                false
            } else {
                for (k, xv) in x {
                    match y.get(k) {
                        Some(yv) if py_eq(xv, yv)? => {}
                        _ => return Ok(false),
                    }
                }
                true
            }
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn ev(doc: Document, expr: Bson) -> Bson {
        evaluate(&doc, &expr, &Document::new()).expect("should not fall back")
    }

    #[test]
    fn paths_and_literals() {
        assert_eq!(ev(doc! {"a": 5}, Bson::String("$a".into())), Bson::Int32(5));
        assert_eq!(
            ev(doc! {}, Bson::String("hi".into())),
            Bson::String("hi".into())
        );
        assert_eq!(ev(doc! {}, Bson::String("$missing".into())), Bson::Null);
    }

    #[test]
    fn comparison_and_logic() {
        assert_eq!(
            ev(doc! {"a": 5, "b": 3}, bson::bson!({"$gt": ["$a", "$b"]})),
            Bson::Boolean(true)
        );
        assert_eq!(
            ev(doc! {"a": 1, "b": 3}, bson::bson!({"$gt": ["$a", "$b"]})),
            Bson::Boolean(false)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$eq": [1, 1.0]})),
            Bson::Boolean(true)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$and": [true, {"$eq": [1, 1]}]})),
            Bson::Boolean(true)
        );
    }

    #[test]
    fn arithmetic() {
        assert_eq!(
            ev(doc! {}, bson::bson!({"$add": [1, 2, 3]})),
            Bson::Int32(6)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$subtract": [10, 3]})),
            Bson::Int32(7)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$multiply": [2, 2.5]})),
            Bson::Double(5.0)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$divide": [9, 2]})),
            Bson::Double(4.5)
        );
        assert_eq!(ev(doc! {}, bson::bson!({"$add": ["$x", 1]})), Bson::Null); // missing -> null
    }

    #[test]
    fn control_flow_and_arrays() {
        assert_eq!(
            ev(
                doc! {"a": 5},
                bson::bson!({"$cond": [{"$gt": ["$a", 0]}, "pos", "neg"]})
            ),
            Bson::String("pos".into())
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$ifNull": ["$x", 7]})),
            Bson::Int32(7)
        );
        assert_eq!(
            ev(
                doc! {"a": [10, 20, 30]},
                bson::bson!({"$arrayElemAt": ["$a", -1]})
            ),
            Bson::Int32(30)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$in": [2, [1, 2, 3]]})),
            Bson::Boolean(true)
        );
    }

    #[test]
    fn unsupported_falls_back() {
        // Still-unported op, non-ASCII $toUpper, and string $add all defer.
        assert!(evaluate(
            &doc! {},
            &bson::bson!({"$dateToString": {"date": "$d"}}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &doc! {},
            &bson::bson!({"$toUpper": "café"}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &doc! {},
            &bson::bson!({"$add": ["a", "b"]}),
            &Document::new()
        )
        .is_err());
    }

    #[test]
    fn string_and_object_ops() {
        assert_eq!(
            ev(doc! {}, bson::bson!({"$concat": ["a", "b", "c"]})),
            Bson::String("abc".into())
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$toUpper": "abc"})),
            Bson::String("ABC".into())
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$strLenCP": "hello"})),
            Bson::Int32(5)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$split": ["a,b,c", ","]})),
            Bson::Array(vec!["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$substrCP": ["hello", 1, 3]})),
            Bson::String("ell".into())
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$slice": [[1, 2, 3, 4], 2]})),
            Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)])
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$indexOfArray": [[1, 2, 3], 2]})),
            Bson::Int32(1)
        );
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$mergeObjects": [{"a": 1}, {"b": 2}]})
            ),
            Bson::Document(doc! {"a": 1, "b": 2})
        );
    }

    #[test]
    fn scope_ops() {
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$map": {"input": [1, 2, 3], "in": {"$add": ["$$this", 10]}}})
            ),
            Bson::Array(vec![Bson::Int32(11), Bson::Int32(12), Bson::Int32(13)])
        );
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$filter": {"input": [1, 2, 3, 4], "as": "n",
                    "cond": {"$gt": ["$$n", 2]}}})
            ),
            Bson::Array(vec![Bson::Int32(3), Bson::Int32(4)])
        );
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$reduce": {"input": [1, 2, 3], "initialValue": 0,
                    "in": {"$add": ["$$value", "$$this"]}}})
            ),
            Bson::Int32(6)
        );
        assert_eq!(
            ev(
                doc! {"x": 5},
                bson::bson!({"$let": {"vars": {"d": {"$add": ["$x", 1]}},
                    "in": {"$multiply": ["$$d", 2]}}})
            ),
            Bson::Int32(12)
        );
    }

    #[test]
    fn date_extractors() {
        // 2026-06-05T12:34:56Z (a Friday)
        let dt = Bson::DateTime(bson::DateTime::from_millis(1_780_662_896_000));
        let d = doc! {"d": dt};
        assert_eq!(
            ev(d.clone(), bson::bson!({"$year": "$d"})),
            Bson::Int32(2026)
        );
        assert_eq!(ev(d.clone(), bson::bson!({"$month": "$d"})), Bson::Int32(6));
        assert_eq!(
            ev(d.clone(), bson::bson!({"$dayOfMonth": "$d"})),
            Bson::Int32(5)
        );
        assert_eq!(ev(d.clone(), bson::bson!({"$hour": "$d"})), Bson::Int32(12));
        assert_eq!(
            ev(d.clone(), bson::bson!({"$minute": "$d"})),
            Bson::Int32(34)
        );
        assert_eq!(
            ev(d.clone(), bson::bson!({"$second": "$d"})),
            Bson::Int32(56)
        );
        assert_eq!(ev(d, bson::bson!({"$dayOfWeek": "$d"})), Bson::Int32(6)); // Friday
    }
}
