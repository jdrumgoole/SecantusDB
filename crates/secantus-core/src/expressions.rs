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
//! `$multiply`/`$divide`/`$mod`); and common array ops (`$size`/`$arrayElemAt`/
//! `$first`/`$last`/`$concatArrays`/`$reverseArray`/`$in`). Everything else
//! (strings, dates, regex, conversions, `$map`/`$filter`/`$reduce`/`$let`,
//! object ops) -> Python.

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
fn py_order(a: &Bson, b: &Bson) -> Result<Option<Ordering>, Fallback> {
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
    if !mul && vals.len() == 1 {
        // Python $add returns the single value unchanged (preserving bool/string
        // identity); only multi-arg goes through numeric folding.
        return Ok(vals[0].clone());
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
    // Decimal128 division has type-specific semantics -> defer.
    let (Some(a), Some(b)) = (as_float_like(&vals[0]), as_float_like(&vals[1])) else {
        return Err(Fallback);
    };
    if b == 0.0 {
        return Ok(Bson::Null); // Python: b == 0 -> None
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
    // Only integer mod with a positive divisor is reproduced cheaply; Python's
    // float mod and divisor-signed semantics are deferred.
    if let (Some(a), Some(b)) = (as_int_like(&vals[0]), as_int_like(&vals[1])) {
        if b == 0 {
            return Ok(Bson::Null); // Python: b == 0 -> None
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
        assert!(evaluate(&doc! {}, &bson::bson!({"$toUpper": "x"}), &Document::new()).is_err());
        assert!(evaluate(
            &doc! {},
            &bson::bson!({"$add": ["a", "b"]}),
            &Document::new()
        )
        .is_err());
    }
}
