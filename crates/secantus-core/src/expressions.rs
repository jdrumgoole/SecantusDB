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
//! `$millisecond`/`$dayOfWeek`/`$dayOfYear`/`$week`/`$isoWeek`/`$isoDayOfWeek`/
//! `$isoWeekYear`) + `$dateToParts` (with `iso8601`); date arithmetic
//! (`$dateAdd`/`$dateSubtract`/
//! `$dateDiff`/`$dateTrunc` — UTC, dependency-free calendar math); a safe subset
//! of conversions (`$toInt`/`$toDouble`/`$toBool`/`$toString` for numbers/bools/
//! strings); exactly-deterministic math (`$abs`/`$floor`/`$ceil`/`$sqrt`); and
//! `$range`/`$strLenBytes`/`$arrayToObject`.
//!
//! The remaining operators are *principled* defers — they can't be reproduced
//! without a fidelity risk: regex (`$regexMatch`/…) needs Python's `re`;
//! `$dateToString`/`$dateFromString` handle a numeric-directive `strftime`/
//! `strptime` subset + fixed-offset timezones (`$dateToString` also resolves
//! *named* IANA zones via `chrono-tz` — the unambiguous instant→wall-clock
//! direction; `$dateFromString`'s named-zone form still defers, being
//! DST-ambiguous local→instant);
//! `$convert`/`$toDecimal` + float-`str()` / string-parse / Decimal128
//! conversions; `$round`/`$pow`/`$trunc` (rounding mode); `$sortArray`
//! depends on Python's `sorted()` ordering/stability; and non-ASCII case /
//! default-whitespace trim. All defer to the authoritative pure-Python
//! evaluator. `$rand` is non-deterministic, so it's evaluated here directly (a
//! fresh double in [0, 1)) rather than deferred — the two engines agree on the
//! value's shape, not its bits.

use std::cmp::Ordering;

use bson::{Bson, Document};

use crate::numeric::{self, as_float_like, as_int_like, int_to_bson};
use crate::paths;
use crate::regexutil;

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
    } else if matches!(base, "KEEP" | "PRUNE" | "DESCEND") {
        // $redact sentinels: the evaluator returns the `"$$NAME"` string so the
        // `$redact` stage can dispatch on equality (mirrors `expressions`).
        Bson::String(format!("$${base}"))
    } else {
        // $$REMOVE (tied to unported $setField/$project-remove) and undefined
        // vars (Python raises) -> Python.
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
        "$firstN" => op_first_last_n(arg, ctx, true),
        "$lastN" => op_first_last_n(arg, ctx, false),
        "$maxN" => op_max_min_n(arg, ctx, true),
        "$minN" => op_max_min_n(arg, ctx, false),
        "$concatArrays" => op_concat_arrays(arg, ctx),
        "$reverseArray" => op_reverse_array(arg, ctx),
        "$sortArray" => op_sort_array(arg, ctx),
        "$in" => op_in(arg, ctx),
        "$slice" => op_slice(arg, ctx),
        "$indexOfArray" => op_index_of_array(arg, ctx),
        // $sum/$avg/$max/$min as expression operators (MongoDB 5.0+)
        "$sum" => op_expr_sum(arg, ctx),
        "$avg" => op_expr_avg(arg, ctx),
        "$max" => op_expr_max(arg, ctx),
        "$min" => op_expr_min(arg, ctx),
        // strings
        "$concat" => op_concat(arg, ctx),
        "$toLower" => op_to_case(arg, ctx, false),
        "$toUpper" => op_to_case(arg, ctx, true),
        "$strLenCP" => op_str_len_cp(arg, ctx),
        "$split" => op_split(arg, ctx),
        "$substrCP" => op_substr_cp(arg, ctx),
        // mongod: $substr is a deprecated alias of $substrBytes (byte-based).
        "$substrBytes" | "$substr" => op_substr_bytes(arg, ctx),
        "$indexOfCP" => op_index_of(arg, ctx, false),
        "$indexOfBytes" => op_index_of(arg, ctx, true),
        "$trim" => op_trim(arg, ctx, TrimSide::Both),
        "$ltrim" => op_trim(arg, ctx, TrimSide::Left),
        "$rtrim" => op_trim(arg, ctx, TrimSide::Right),
        // objects
        "$mergeObjects" => op_merge_objects(arg, ctx),
        "$median" => op_percentile_expr(arg, ctx, true),
        "$percentile" => op_percentile_expr(arg, ctx, false),
        "$objectToArray" => op_object_to_array(arg, ctx),
        "$getField" => op_get_field(arg, ctx),
        "$setField" => op_set_field(arg, ctx),
        "$zip" => op_zip(arg, ctx),
        // scope-introducing
        "$let" => op_let(arg, ctx),
        "$map" => op_map(arg, ctx),
        "$filter" => op_filter(arg, ctx),
        "$reduce" => op_reduce(arg, ctx),
        // set operators (BSON-order sort/equality; unsortable elements defer)
        "$setUnion" => op_set_union(arg, ctx),
        "$setIntersection" => op_set_intersection(arg, ctx),
        "$setDifference" => op_set_difference(arg, ctx),
        "$setEquals" => op_set_equals(arg, ctx),
        "$setIsSubset" => op_set_is_subset(arg, ctx),
        "$allElementsTrue" => op_elements_true(arg, ctx, true),
        "$anyElementTrue" => op_elements_true(arg, ctx, false),
        "$cmp" => op_cmp(arg, ctx),
        "$binarySize" => op_binary_size(arg, ctx),
        "$bsonSize" => op_bson_size(arg, ctx),
        "$degreesToRadians" => op_deg_rad(arg, ctx, true),
        "$radiansToDegrees" => op_deg_rad(arg, ctx, false),
        // date component extractors — bare date expr or `{date, timezone}` object
        // form (fixed-offset + named IANA zones via chrono-tz; instant→local)
        "$year" => date_part(arg, ctx, DatePart::Year),
        "$month" => date_part(arg, ctx, DatePart::Month),
        "$dayOfMonth" => date_part(arg, ctx, DatePart::Day),
        "$hour" => date_part(arg, ctx, DatePart::Hour),
        "$minute" => date_part(arg, ctx, DatePart::Minute),
        "$second" => date_part(arg, ctx, DatePart::Second),
        "$millisecond" => date_part(arg, ctx, DatePart::Millisecond),
        "$dayOfWeek" => date_part(arg, ctx, DatePart::DayOfWeek),
        "$dayOfYear" => date_part(arg, ctx, DatePart::DayOfYear),
        "$week" => date_part(arg, ctx, DatePart::Week),
        "$isoWeek" => date_part(arg, ctx, DatePart::IsoWeek),
        "$isoDayOfWeek" => date_part(arg, ctx, DatePart::IsoDayOfWeek),
        "$isoWeekYear" => date_part(arg, ctx, DatePart::IsoWeekYear),
        // date arithmetic (UTC; no timezone arg form)
        "$dateAdd" => op_date_add(arg, ctx, 1),
        "$dateSubtract" => op_date_add(arg, ctx, -1),
        "$dateDiff" => op_date_diff(arg, ctx),
        "$dateTrunc" => op_date_trunc(arg, ctx),
        // type conversions (safe subset; Decimal128 / string-parse / float
        // str() defer to Python)
        "$toInt" => op_to_int(arg, ctx),
        "$toLong" => op_to_long(arg, ctx),
        "$toDouble" => op_to_double(arg, ctx),
        "$toDecimal" => op_to_decimal(arg, ctx),
        "$toDate" => op_to_date(arg, ctx),
        "$convert" => op_convert(arg, ctx),
        "$toBool" => op_to_bool(arg, ctx),
        "$toString" => op_to_string(arg, ctx),
        "$regexMatch" => op_regex_match(arg, ctx),
        "$regexFind" => op_regex_find(arg, ctx),
        "$regexFindAll" => op_regex_find_all(arg, ctx),
        // math: the transcendentals $exp/$ln/$log/$log10 are computed natively
        // (Rust f64 and CPython share the platform libm, so the anticipated
        // last-ULP divergence doesn't materialise); $sqrt is IEEE exactly-rounded.
        "$abs" => op_abs(arg, ctx),
        "$floor" => op_floor_ceil(arg, ctx, false),
        "$ceil" => op_floor_ceil(arg, ctx, true),
        "$sqrt" => op_sqrt(arg, ctx),
        "$exp" => op_exp(arg, ctx),
        "$ln" => op_ln(arg, ctx),
        "$log" => op_log(arg, ctx),
        "$log10" => op_log10(arg, ctx),
        "$pow" => op_pow(arg, ctx),
        "$round" => op_round(arg, ctx),
        "$trunc" => op_trunc(arg, ctx),
        // trig (int/long/double -> Double via libm, matching Python `math`
        // bit-for-bit on-platform like $exp/$ln; bool / Decimal128 / domain
        // violations defer to the Python oracle)
        "$sin" => op_trig(arg, ctx, Trig::Sin),
        "$cos" => op_trig(arg, ctx, Trig::Cos),
        "$tan" => op_trig(arg, ctx, Trig::Tan),
        "$asin" => op_trig(arg, ctx, Trig::Asin),
        "$acos" => op_trig(arg, ctx, Trig::Acos),
        "$atan" => op_trig(arg, ctx, Trig::Atan),
        "$atan2" => op_atan2(arg, ctx),
        "$sinh" => op_trig(arg, ctx, Trig::Sinh),
        "$cosh" => op_trig(arg, ctx, Trig::Cosh),
        "$tanh" => op_trig(arg, ctx, Trig::Tanh),
        "$asinh" => op_trig(arg, ctx, Trig::Asinh),
        "$acosh" => op_trig(arg, ctx, Trig::Acosh),
        "$atanh" => op_trig(arg, ctx, Trig::Atanh),
        // bitwise (int / long operands; a bool / double / other defers to Python,
        // whose exact type-error message is the oracle)
        "$bitAnd" => op_bit_fold(arg, ctx, BitOp::And),
        "$bitOr" => op_bit_fold(arg, ctx, BitOp::Or),
        "$bitXor" => op_bit_fold(arg, ctx, BitOp::Xor),
        "$bitNot" => op_bit_not(arg, ctx),
        "$dateFromString" => op_date_from_string(arg, ctx),
        "$dateToString" => op_date_to_string(arg, ctx),
        // misc structural / deterministic
        "$dateToParts" => op_date_to_parts(arg, ctx),
        "$dateFromParts" => op_date_from_parts(arg, ctx),
        "$tsSecond" => op_ts_field(arg, ctx, true),
        "$tsIncrement" => op_ts_field(arg, ctx, false),
        "$type" => op_type(arg, ctx),
        "$isNumber" => op_is_number(arg, ctx),
        "$isArray" => op_is_array(arg, ctx),
        "$strcasecmp" => op_strcasecmp(arg, ctx),
        "$replaceOne" => op_replace(arg, ctx, false),
        "$replaceAll" => op_replace(arg, ctx, true),
        "$range" => op_range(arg, ctx),
        "$strLenBytes" => op_str_len_bytes(arg, ctx),
        "$arrayToObject" => op_array_to_object(arg, ctx),
        // non-deterministic: a fresh uniform double in [0, 1) (mirrors
        // `expressions._op_rand` / `random.random()`; not byte-pinned to it).
        "$rand" => op_rand(arg),
        _ => Err(Fallback),
    }
}

/// Aggregation-expression operators this engine recognises — the `$`-prefixed
/// keys `apply_op` dispatches on, plus the special-cased `$literal`. A recognised
/// operator that can't be reproduced exactly returns `Fallback` (deferred), which
/// is *not* an "unknown operator"; only a `$`-key absent from this set is. Used
/// only by [`first_unknown_expr_operator`] to shape mongod's context-specific
/// unknown-expression error, so it must stay in step with `apply_op`'s arms
/// (guarded by a test).
pub const KNOWN_EXPR_OPS: &[&str] = &[
    "$literal",
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$and",
    "$or",
    "$not",
    "$cond",
    "$ifNull",
    "$switch",
    "$add",
    "$multiply",
    "$subtract",
    "$divide",
    "$mod",
    "$size",
    "$arrayElemAt",
    "$first",
    "$last",
    "$firstN",
    "$lastN",
    "$maxN",
    "$minN",
    "$concatArrays",
    "$reverseArray",
    "$sortArray",
    "$in",
    "$slice",
    "$indexOfArray",
    "$sum",
    "$avg",
    "$max",
    "$min",
    "$concat",
    "$toLower",
    "$toUpper",
    "$strLenCP",
    "$split",
    "$substrCP",
    "$substr",
    "$substrBytes",
    "$indexOfCP",
    "$indexOfBytes",
    "$trim",
    "$ltrim",
    "$rtrim",
    "$median",
    "$mergeObjects",
    "$percentile",
    "$objectToArray",
    "$getField",
    "$setField",
    "$zip",
    "$let",
    "$map",
    "$filter",
    "$reduce",
    "$setUnion",
    "$setIntersection",
    "$setDifference",
    "$setEquals",
    "$setIsSubset",
    "$allElementsTrue",
    "$anyElementTrue",
    "$cmp",
    "$binarySize",
    "$bsonSize",
    "$degreesToRadians",
    "$radiansToDegrees",
    "$year",
    "$month",
    "$dayOfMonth",
    "$hour",
    "$minute",
    "$second",
    "$millisecond",
    "$dayOfWeek",
    "$dayOfYear",
    "$week",
    "$isoWeek",
    "$isoDayOfWeek",
    "$isoWeekYear",
    "$dateAdd",
    "$dateSubtract",
    "$dateDiff",
    "$dateTrunc",
    "$toInt",
    "$toLong",
    "$toDouble",
    "$toDecimal",
    "$toDate",
    "$convert",
    "$toBool",
    "$toString",
    "$regexMatch",
    "$regexFind",
    "$regexFindAll",
    "$abs",
    "$floor",
    "$ceil",
    "$sqrt",
    "$exp",
    "$ln",
    "$log",
    "$log10",
    "$pow",
    "$round",
    "$trunc",
    "$sin",
    "$cos",
    "$tan",
    "$asin",
    "$acos",
    "$atan",
    "$atan2",
    "$sinh",
    "$cosh",
    "$tanh",
    "$asinh",
    "$acosh",
    "$atanh",
    "$bitAnd",
    "$bitOr",
    "$bitXor",
    "$bitNot",
    "$dateFromString",
    "$dateToString",
    "$dateToParts",
    "$dateFromParts",
    "$tsSecond",
    "$tsIncrement",
    "$type",
    "$isNumber",
    "$isArray",
    "$strcasecmp",
    "$replaceOne",
    "$replaceAll",
    "$range",
    "$strLenBytes",
    "$arrayToObject",
    "$rand",
];

/// The first `$`-prefixed expression operator in `expr` (recursing through
/// arrays and nested single-key operator documents) that this engine does not
/// recognise, e.g. `$notreal` for `{$notreal: [1, 2]}`. mongod rejects an
/// unrecognised expression operator with a context-specific "unknown
/// expression" error (`168 InvalidPipelineOperator` in a query `$expr`;
/// `Location31325` inside `$project`), so the command layer uses this to shape
/// that error rather than the generic `Fallback` → `BadValue`. A recognised
/// operator that merely defers to Python is *not* reported here. `None` when
/// every operator in the tree is recognised.
pub fn first_unknown_expr_operator(expr: &Bson) -> Option<String> {
    match expr {
        Bson::Array(a) => {
            for e in a {
                if let Some(op) = first_unknown_expr_operator(e) {
                    return Some(op);
                }
            }
            None
        }
        Bson::Document(d) => {
            if d.len() == 1 {
                let (key, val) = d.iter().next().unwrap();
                if key.starts_with('$') {
                    if !KNOWN_EXPR_OPS.contains(&key.as_str()) {
                        return Some(key.clone());
                    }
                    return first_unknown_expr_operator(val);
                }
            }
            for v in d.values() {
                if let Some(op) = first_unknown_expr_operator(v) {
                    return Some(op);
                }
            }
            None
        }
        _ => None,
    }
}

/// `$rand`: a uniform random double in [0, 1). The argument must be an empty
/// document (anything else is a parse error in mongod). Mirrors
/// `expressions._op_rand` — non-deterministic, so the two engines agree on the
/// *shape* (a double in range), not the exact value.
fn op_rand(arg: &Bson) -> R {
    match arg {
        Bson::Document(d) if d.is_empty() => Ok(Bson::Double(rand::random::<f64>())),
        _ => Err(Fallback), // non-empty / wrong-typed arg -> Python raises
    }
}

#[derive(Clone, Copy)]
enum BitOp {
    And,
    Or,
    Xor,
}

/// A `$bit*` operand as `(value, is_long)` — int (32) or long (64) only. Bool /
/// double / anything else returns `None` so the caller defers to Python (whose
/// exact "only supports int and long operands" error is the oracle). Values are
/// held in `i64`; an `Int32` sign-extends, and the caller narrows back with
/// `as i32` when no long operand was seen.
fn bit_operand(v: &Bson) -> Option<(i64, bool)> {
    match v {
        Bson::Int32(n) => Some((*n as i64, false)),
        Bson::Int64(n) => Some((*n, true)),
        _ => None, // bool / double / string / ... -> Python raises
    }
}

/// Wrap a bitwise result: `Int64` when any operand was long, else `Int32`
/// (the low 32 bits — correct two's-complement for int operands, matching the
/// pure `_bit_result`).
fn bit_result(value: i64, is_long: bool) -> Bson {
    if is_long {
        Bson::Int64(value)
    } else {
        Bson::Int32(value as i32)
    }
}

/// `$bitAnd` / `$bitOr` / `$bitXor`: fold int/long operands. A null operand makes
/// the result null; the result is long iff any operand was long; an empty list is
/// the operator's identity (all-ones for and, 0 for or/xor). Mirrors the pure
/// `_op_bit_fold`.
fn op_bit_fold(arg: &Bson, ctx: &Ctx, op: BitOp) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.iter().any(is_null) {
        return Ok(Bson::Null);
    }
    let mut acc: i64 = match op {
        BitOp::And => -1,
        BitOp::Or | BitOp::Xor => 0,
    };
    let mut is_long = false;
    for v in &vals {
        let (n, lng) = bit_operand(v).ok_or(Fallback)?;
        is_long |= lng;
        acc = match op {
            BitOp::And => acc & n,
            BitOp::Or => acc | n,
            BitOp::Xor => acc ^ n,
        };
    }
    Ok(bit_result(acc, is_long))
}

/// `$bitNot`: bitwise complement of a single int/long operand (null → null).
fn op_bit_not(arg: &Bson, ctx: &Ctx) -> R {
    let v = eval(arg, ctx)?;
    if is_null(&v) {
        return Ok(Bson::Null);
    }
    let (n, is_long) = bit_operand(&v).ok_or(Fallback)?;
    Ok(bit_result(!n, is_long))
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
    if let Some(r) = numeric::fast_cmp_numberish(a, b) {
        return Ok(r);
    }
    if numeric::is_numberish(a) != numeric::is_numberish(b) {
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
    if matches!(idx, Bson::Boolean(_)) {
        return Err(Fallback); // Python raises 28690
    }
    let i = match coerce_index(&idx) {
        IdxCoerce::Int(i) => i as i128,
        IdxCoerce::Fractional => return Err(Fallback), // Python raises 28691
        IdxCoerce::NotNumber => return Ok(Bson::Null),
    };
    let a = match &arr {
        Bson::Array(a) => a,
        Bson::Null => return Ok(Bson::Null),
        _ => return Err(Fallback), // non-array first arg -> Python raises 28689
    };
    let len = a.len() as i128;
    let resolved = if i < 0 { i + len } else { i };
    if (0..len).contains(&resolved) {
        Ok(a[resolved as usize].clone())
    } else {
        // Out of range evaluates to MISSING, not null, so `$project` omits the
        // field. `Bson::Undefined` is this engine's missing marker and the
        // project stage already skips it. Probed against mongod 6.0.16 on
        // `[1, 2]`: index 9 and index -9 both give `{_id: 1}` with no field,
        // while a missing/null input array really is null. Mirrors
        // `expressions.py::_array_elem_at`.
        Ok(Bson::Undefined)
    }
}

fn op_first_last(arg: &Bson, ctx: &Ctx, first: bool) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::Array(a) => Ok(if a.is_empty() {
            Bson::Null
        } else if first {
            a[0].clone()
        } else {
            a[a.len() - 1].clone()
        }),
        _ => Err(Fallback), // non-array -> Python raises 28689
    }
}

/// Shared `{n, input}` validation for `$firstN` / `$lastN` / `$maxN` / `$minN`.
/// Returns `(n, array)` for a valid spec, else `Fallback` so Python raises the
/// exact mongod error (5788200 / 5787902-8, verified against mongod 6.0). Accepts
/// an integral double `n` (mongod does). A null / missing / non-array `input` is
/// an **error** (defer), not null. Mirrors the pure `_nelem_n_and_input`.
fn nelem_n_and_input(arg: &Bson, ctx: &Ctx) -> Result<(usize, Vec<Bson>), Fallback> {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let n = match eval(d.get("n").ok_or(Fallback)?, ctx)? {
        Bson::Int32(x) => x as i64,
        Bson::Int64(x) => x,
        Bson::Double(x) if x.is_finite() && x.fract() == 0.0 => x as i64,
        _ => return Err(Fallback), // non-integral / non-numeric -> Python raises
    };
    if n <= 0 {
        return Err(Fallback); // Python raises 5787908
    }
    let arr = match eval(d.get("input").ok_or(Fallback)?, ctx)? {
        Bson::Array(a) => a,
        _ => return Err(Fallback), // null / missing / non-array -> Python raises 5788200
    };
    Ok((n as usize, arr))
}

/// `$firstN` / `$lastN` (expression form): the first / last `n` elements of an
/// array (whole array when it has fewer than `n`). See `nelem_n_and_input` for the
/// mongod-faithful `{n, input}` validation. Mirrors the pure `_first_last_n`.
fn op_first_last_n(arg: &Bson, ctx: &Ctx, first: bool) -> R {
    let (n, arr) = nelem_n_and_input(arg, ctx)?;
    let n = n.min(arr.len());
    let out: Vec<Bson> = if first {
        arr[..n].to_vec()
    } else {
        arr[arr.len() - n..].to_vec()
    };
    Ok(Bson::Array(out))
}

/// `$maxN` / `$minN` (expression form): the `n` largest / smallest elements of an
/// array by BSON order — null *elements* ignored, `$maxN` descending, `$minN`
/// ascending. `{n, input}` validation matches mongod (`nelem_n_and_input` — a null
/// / non-array input raises, unlike the elements). A non-null element outside the
/// sortable subset (bool / Decimal128 / NaN / …) defers to Python, whose `_SortKey`
/// handles the wider set — the same `order::cmp` / `is_sortable` contract
/// `$sortArray` relies on. Mirrors the pure `_max_min_n`.
fn op_max_min_n(arg: &Bson, ctx: &Ctx, largest: bool) -> R {
    let (n, arr) = nelem_n_and_input(arg, ctx)?;
    // mongod ignores null elements; the rest must be in the sortable subset so
    // `order::cmp` reproduces Python's `_SortKey` order (else defer).
    let mut vals: Vec<Bson> = arr.into_iter().filter(|x| !is_null(x)).collect();
    if !vals.iter().all(crate::order::is_sortable) {
        return Err(Fallback);
    }
    // Descending via `cmp(b, a)` keeps equal elements in original order (stable),
    // matching Python's `sorted(reverse=True)`.
    vals.sort_by(|a, b| {
        if largest {
            crate::order::cmp(b, a)
        } else {
            crate::order::cmp(a, b)
        }
    });
    let n = n.min(vals.len());
    Ok(Bson::Array(vals[..n].to_vec()))
}

fn op_concat_arrays(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(parts) = arg else {
        return Err(Fallback);
    };
    let mut out: Vec<Bson> = Vec::new();
    for p in parts {
        match eval(p, ctx)? {
            Bson::Array(a) => out.extend(a),
            Bson::Null => return Ok(Bson::Null), // null operand -> null result
            _ => return Err(Fallback),           // non-array -> Python raises 28664
        }
    }
    Ok(Bson::Array(out))
}

fn op_reverse_array(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::Array(mut a) => {
            a.reverse();
            Ok(Bson::Array(a))
        }
        _ => Err(Fallback), // non-array -> Python raises 34435
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
        // A non-array second argument is mongod Location40081 -> Python raises.
        return Err(Fallback);
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

/// Coerce a numeric arg to i64, truncating a finite double toward zero -- the
/// `$substrBytes` semantics (mongod accepts any double there, unlike `$substrCP`
/// which rejects a fractional one). `None` for a non-finite double / non-number
/// (the caller defers to the Python oracle).
fn trunc_index(b: &Bson) -> Option<i64> {
    match b {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        Bson::Double(d) if d.is_finite() => Some(d.trunc() as i64),
        _ => None,
    }
}

/// Outcome of coercing a numeric aggregation index argument to `i64`, the way
/// mongod does: an int (or a whole-number double) is the index; a double with a
/// fractional part is rejected (Python raises the op's per-arg code, so the Rust
/// core defers); anything else is a non-number the caller handles (null / -1).
/// `bool` is handled by the caller's own guard before this is reached.
enum IdxCoerce {
    Int(i64),
    Fractional,
    NotNumber,
}

fn coerce_index(b: &Bson) -> IdxCoerce {
    match b {
        Bson::Int32(n) => IdxCoerce::Int(*n as i64),
        Bson::Int64(n) => IdxCoerce::Int(*n),
        Bson::Double(d) => {
            if d.is_finite() && d.fract() == 0.0 {
                IdxCoerce::Int(*d as i64)
            } else {
                IdxCoerce::Fractional
            }
        }
        _ => IdxCoerce::NotNumber,
    }
}

fn op_slice(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Array(a) = arg else {
        return Err(Fallback);
    };
    if a.len() != 2 && a.len() != 3 {
        return Err(Fallback);
    }
    let arr = match eval(&a[0], ctx)? {
        Bson::Array(v) => v,
        Bson::Null => return Ok(Bson::Null),
        _ => return Err(Fallback), // non-array input -> Python raises 28724
    };
    let len = arr.len() as i64;
    let (start, stop) = if a.len() == 2 {
        let n_v = eval(&a[1], ctx)?;
        if matches!(n_v, Bson::Boolean(_)) {
            return Err(Fallback); // Python raises 28725
        }
        let n = match coerce_index(&n_v) {
            IdxCoerce::Int(n) => n,
            IdxCoerce::Fractional => return Err(Fallback), // Python raises 28726
            IdxCoerce::NotNumber => return Ok(Bson::Null),
        };
        if n >= 0 {
            (0, n)
        } else {
            (n, len)
        }
    } else {
        let pos_v = eval(&a[1], ctx)?;
        let n_v = eval(&a[2], ctx)?;
        if matches!(pos_v, Bson::Boolean(_)) || matches!(n_v, Bson::Boolean(_)) {
            return Err(Fallback); // Python raises 28725 / 28727
        }
        let pos = match coerce_index(&pos_v) {
            IdxCoerce::Int(p) => p,
            IdxCoerce::Fractional => return Err(Fallback), // Python raises 28726
            IdxCoerce::NotNumber => return Ok(Bson::Null),
        };
        let n = match coerce_index(&n_v) {
            IdxCoerce::Int(n) => n,
            IdxCoerce::Fractional => return Err(Fallback), // Python raises 28728
            IdxCoerce::NotNumber => return Ok(Bson::Null),
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

/// The values an expression-form `$sum`/`$avg`/`$max`/`$min` reduces over: an
/// array argument contributes its elements, a null/absent argument contributes
/// nothing, and any other value is a single element. Mirrors
/// `_expr_acc_values`.
fn expr_acc_values(arg: &Bson, ctx: &Ctx) -> Result<Vec<Bson>, Fallback> {
    match eval(arg, ctx)? {
        Bson::Array(a) => Ok(a),
        Bson::Null => Ok(Vec::new()),
        other => Ok(vec![other]),
    }
}

fn op_expr_sum(arg: &Bson, ctx: &Ctx) -> R {
    // Reuses the group-accumulator `Num` width logic (int32 < int64 < double <
    // decimal); non-numeric elements are ignored.
    let mut running = crate::group::Num::Int { v: 0, wide: false };
    for v in expr_acc_values(arg, ctx)? {
        match v {
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
                running = running.add(&v).map_err(|_| Fallback)?;
            }
            _ => {} // bool / string / null / doc / array -> ignored
        }
    }
    running.into_bson().map_err(|_| Fallback)
}

fn op_expr_avg(arg: &Bson, ctx: &Ctx) -> R {
    let mut total = crate::group::Num::Int { v: 0, wide: false };
    let mut count: i64 = 0;
    for v in expr_acc_values(arg, ctx)? {
        match v {
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
                total = total.add(&v).map_err(|_| Fallback)?;
                count += 1;
            }
            _ => {}
        }
    }
    if count == 0 {
        return Ok(Bson::Null);
    }
    let tf = match total {
        crate::group::Num::Int { v, .. } => {
            if v.unsigned_abs() > (1u128 << 53) {
                return Err(Fallback); // precision: defer to Python int/int divide
            }
            v as f64
        }
        crate::group::Num::Float(f) => f,
        // Stay in the decimal domain — an f64 divide would narrow the type and
        // drop digits.
        crate::group::Num::Dec(d) => {
            return crate::decimal::to_bson(&crate::decimal::div_int(&d, count).ok_or(Fallback)?)
                .ok_or(Fallback);
        }
    };
    Ok(Bson::Double(tf / count as f64))
}

fn op_expr_extreme(arg: &Bson, ctx: &Ctx, want_max: bool) -> R {
    let mut best: Option<Bson> = None;
    for v in expr_acc_values(arg, ctx)? {
        if matches!(v, Bson::Null) {
            continue; // null never updates
        }
        match &best {
            None => best = Some(v),
            Some(cur) => {
                let replace = if want_max {
                    crate::order::bson_lt(cur, &v)
                } else {
                    crate::order::bson_lt(&v, cur)
                };
                match replace {
                    Some(true) => best = Some(v),
                    Some(false) => {}
                    None => return Err(Fallback), // unorderable -> Python
                }
            }
        }
    }
    Ok(best.unwrap_or(Bson::Null))
}

fn op_expr_max(arg: &Bson, ctx: &Ctx) -> R {
    op_expr_extreme(arg, ctx, true)
}

fn op_expr_min(arg: &Bson, ctx: &Ctx) -> R {
    op_expr_extreme(arg, ctx, false)
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
        let sv = eval(&a[2], ctx)?;
        if matches!(sv, Bson::Boolean(_)) {
            return Err(Fallback); // Python raises 40096
        }
        match coerce_index(&sv) {
            IdxCoerce::Int(x) => x,
            IdxCoerce::Fractional => return Err(Fallback), // Python raises 40096
            IdxCoerce::NotNumber => return Ok(Bson::Int32(-1)),
        }
    } else {
        0
    };
    let end = if a.len() >= 4 {
        let ev = eval(&a[3], ctx)?;
        if matches!(ev, Bson::Boolean(_)) {
            return Err(Fallback); // Python raises 40096
        }
        match coerce_index(&ev) {
            IdxCoerce::Int(x) => x,
            IdxCoerce::Fractional => return Err(Fallback), // Python raises 40096
            IdxCoerce::NotNumber => return Ok(Bson::Int32(-1)),
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
            // A null / missing operand short-circuits to a null result (mongod),
            // left-to-right.
            Bson::Null => return Ok(Bson::Null),
            Bson::String(s) => out.push_str(&s),
            // A non-string operand is Location16702 -> defer so Python raises it.
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
    let start_v = eval(&a[1], ctx)?;
    let length_v = eval(&a[2], ctx)?;
    if matches!(start_v, Bson::Boolean(_)) || matches!(length_v, Bson::Boolean(_)) {
        return Err(Fallback); // Python raises 34450 / 34452
    }
    // Whole-number double coerces to int; fractional (34451/34453) and
    // non-numeric both defer to the Python oracle.
    let (IdxCoerce::Int(start), IdxCoerce::Int(length)) =
        (coerce_index(&start_v), coerce_index(&length_v))
    else {
        return Err(Fallback);
    };
    if start < 0 || length < 0 {
        return Err(Fallback); // Python raises 34455 (start) / 34454 (length)
    }
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

/// Expression-form `$median` / `$percentile` over an array input — mongod's
/// discrete percentile (`sorted[max(0, ceil(p*n) - 1)]` as a double), sharing
/// the value filter and rank math with the group accumulators. An invalid
/// spec defers to Python, which raises mongod's exact error.
fn op_percentile_expr(arg: &Bson, ctx: &Ctx, is_median: bool) -> R {
    let Bson::Document(spec) = arg else {
        return Err(Fallback);
    };
    if spec.get_str("method") != Ok("approximate") {
        return Err(Fallback);
    }
    let input = spec.get("input").ok_or(Fallback)?;
    let ps: Option<Vec<f64>> = if is_median {
        None
    } else {
        let Some(Bson::Array(raw)) = spec.get("p") else {
            return Err(Fallback);
        };
        let mut parsed = Vec::with_capacity(raw.len());
        for p in raw {
            let f = match p {
                Bson::Int32(n) => *n as f64,
                Bson::Int64(n) => *n as f64,
                Bson::Double(d) => *d,
                _ => return Err(Fallback),
            };
            if !(0.0..=1.0).contains(&f) {
                return Err(Fallback);
            }
            parsed.push(f);
        }
        Some(parsed)
    };
    let raw = eval(input, ctx)?;
    let items: Vec<&Bson> = match &raw {
        Bson::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut values: Vec<f64> = items
        .into_iter()
        .filter_map(crate::group::percentile_f64)
        .collect();
    values.sort_by(|a, b| a.partial_cmp(b).expect("NaN excluded at collect"));
    Ok(if is_median {
        crate::group::percentile_rank(&values, 0.5)
    } else {
        Bson::Array(
            ps.unwrap_or_default()
                .iter()
                .map(|p| crate::group::percentile_rank(&values, *p))
                .collect(),
        )
    })
}

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
            // Evaluate `input` missing-aware: an input field-path that resolves
            // to a *missing* field makes the whole `$getField` "missing" (mongod
            // 6.0: input missing -> missing, input null -> null). We represent the
            // "missing" value as `Bson::Undefined` — the internal marker a
            // `$project`/`$addFields` computed field omits from the output rather
            // than emitting as null (see `add_fields_one`/`project_one`).
            let input = match d.get("input") {
                Some(Bson::String(s)) if s.starts_with('$') && !s.starts_with("$$") => {
                    match paths::get_path(ctx.doc, &s[1..]) {
                        Some(v) => v.clone(),
                        None => return Ok(Bson::Undefined), // input missing -> missing
                    }
                }
                Some(e) => eval(e, ctx)?,
                None => Bson::Document(ctx.doc.clone()),
            };
            (field, input)
        }
        _ => return Err(Fallback),
    };
    match input {
        // A field absent from the input document resolves to the "missing" value
        // (`Bson::Undefined`) — which a `$project`/`$addFields` computed field
        // omits rather than emitting as null. A field present with an explicit
        // null returns null.
        Bson::Document(doc) => match doc.get(&field) {
            Some(v) => Ok(v.clone()),
            None => Ok(Bson::Undefined),
        },
        _ => Ok(Bson::Null), // null / non-document input -> None
    }
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
    let arr = match eval_opt(d.get("input"), ctx)? {
        Bson::Array(a) => a,
        Bson::Null => return Ok(Bson::Null),
        _ => return Err(Fallback), // non-array input -> Python raises 16883
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
    let arr = match eval_opt(d.get("input"), ctx)? {
        Bson::Array(a) => a,
        Bson::Null => return Ok(Bson::Null),
        _ => return Err(Fallback), // non-array input -> Python raises 28651
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
    let arr = match eval_opt(d.get("input"), ctx)? {
        Bson::Array(a) => a,
        Bson::Null => return Ok(Bson::Null),
        _ => return Err(Fallback), // non-array input -> Python raises 40080
    };
    let mut acc = eval_opt(d.get("initialValue"), ctx)?;
    let null = Bson::Null;
    let in_expr = d.get("in").unwrap_or(&null);
    for elem in arr {
        acc = eval_with_vars(in_expr, ctx, &[("value", acc), ("this", elem)])?;
    }
    Ok(acc)
}

// --- set operators ------------------------------------------------------

/// Evaluate a set operator's array arguments, requiring every one to be an array
/// and every element to be in the sortable subset (else defer — Python's
/// `_SortKey` / `_bson_lt` handles the wider set, matching `$sortArray`/`$maxN`).
fn set_arrays(arg: &Bson, ctx: &Ctx, n: Option<usize>) -> Result<Vec<Vec<Bson>>, Fallback> {
    let vals = eval_args(arg, ctx)?;
    if n.is_some_and(|k| vals.len() != k) {
        return Err(Fallback);
    }
    let mut out = Vec::with_capacity(vals.len());
    for v in vals {
        let Bson::Array(a) = v else {
            return Err(Fallback); // non-array -> Python raises
        };
        if !a.iter().all(crate::order::is_sortable) {
            return Err(Fallback);
        }
        out.push(a);
    }
    Ok(out)
}

fn set_eq(a: &Bson, b: &Bson) -> bool {
    crate::order::cmp(a, b) == Ordering::Equal
}

fn set_dedup_sorted(mut items: Vec<Bson>) -> Bson {
    items.sort_by(crate::order::cmp);
    items.dedup_by(|a, b| crate::order::cmp(a, b) == Ordering::Equal);
    Bson::Array(items)
}

fn op_set_union(arg: &Bson, ctx: &Ctx) -> R {
    let arrays = set_arrays(arg, ctx, None)?;
    Ok(set_dedup_sorted(arrays.into_iter().flatten().collect()))
}

fn op_set_intersection(arg: &Bson, ctx: &Ctx) -> R {
    let arrays = set_arrays(arg, ctx, None)?;
    let Some(first) = arrays.first() else {
        return Ok(Bson::Array(Vec::new()));
    };
    let result: Vec<Bson> = first
        .iter()
        .filter(|x| arrays[1..].iter().all(|o| o.iter().any(|y| set_eq(x, y))))
        .cloned()
        .collect();
    Ok(set_dedup_sorted(result))
}

fn op_set_difference(arg: &Bson, ctx: &Ctx) -> R {
    let arrays = set_arrays(arg, ctx, Some(2))?;
    let (a, b) = (&arrays[0], &arrays[1]);
    let mut out: Vec<Bson> = Vec::new();
    for x in a {
        if !b.iter().any(|y| set_eq(x, y)) && !out.iter().any(|y| set_eq(x, y)) {
            out.push(x.clone());
        }
    }
    Ok(Bson::Array(out))
}

fn op_set_equals(arg: &Bson, ctx: &Ctx) -> R {
    let arrays = set_arrays(arg, ctx, None)?;
    let base = match arrays.first() {
        Some(a) => set_dedup_sorted(a.clone()),
        None => Bson::Array(Vec::new()),
    };
    for other in &arrays[1..] {
        if set_dedup_sorted(other.clone()) != base {
            return Ok(Bson::Boolean(false));
        }
    }
    Ok(Bson::Boolean(true))
}

fn op_set_is_subset(arg: &Bson, ctx: &Ctx) -> R {
    let arrays = set_arrays(arg, ctx, Some(2))?;
    let (a, b) = (&arrays[0], &arrays[1]);
    Ok(Bson::Boolean(
        a.iter().all(|x| b.iter().any(|y| set_eq(x, y))),
    ))
}

fn op_elements_true(arg: &Bson, ctx: &Ctx, all: bool) -> R {
    let arr = eval_args(arg, ctx)?;
    let Some(Bson::Array(a)) = arr.into_iter().next() else {
        return Err(Fallback);
    };
    Ok(Bson::Boolean(if all {
        a.iter().all(truthy)
    } else {
        a.iter().any(truthy)
    }))
}

/// `$cmp`: three-way BSON-order comparison of two values → -1 / 0 / 1. Operands
/// outside the sortable subset defer (Python's `_bson_lt` handles them).
fn op_cmp(arg: &Bson, ctx: &Ctx) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 || !vals.iter().all(crate::order::is_sortable) {
        return Err(Fallback);
    }
    Ok(Bson::Int32(match crate::order::cmp(&vals[0], &vals[1]) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

/// `$binarySize`: byte length of a string (UTF-8) or binary. Null → null.
fn op_binary_size(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::String(s) => Ok(Bson::Int32(s.len() as i32)),
        Bson::Binary(b) => Ok(Bson::Int32(b.bytes.len() as i32)),
        _ => Err(Fallback),
    }
}

/// `$bsonSize`: the BSON-encoded byte size of a document. Null → null.
fn op_bson_size(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::Document(d) => {
            let mut buf = Vec::new();
            d.to_writer(&mut buf).map_err(|_| Fallback)?;
            Ok(Bson::Int32(buf.len() as i32))
        }
        _ => Err(Fallback),
    }
}

/// `$degreesToRadians` (`to_rad`) / `$radiansToDegrees`. Non-numeric / bool
/// defers (Python raises). Decimal128 defers to the pure oracle.
fn op_deg_rad(arg: &Bson, ctx: &Ctx, to_rad: bool) -> R {
    let x = match eval(arg, ctx)? {
        Bson::Null => return Ok(Bson::Null),
        Bson::Int32(n) => n as f64,
        Bson::Int64(n) => n as f64,
        Bson::Double(d) => d,
        _ => return Err(Fallback),
    };
    Ok(Bson::Double(if to_rad {
        x * std::f64::consts::PI / 180.0
    } else {
        x * 180.0 / std::f64::consts::PI
    }))
}

#[derive(Clone, Copy)]
enum Trig {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
}

/// Unary trig. int/long/double -> Double; null -> null; bool / Decimal128 /
/// non-numeric -> Python (which raises `Location28765`). Domain / finiteness
/// violations also defer (Python raises `Location50989`). Mirrors the
/// `_make_trig` factory in `expressions.py`.
fn op_trig(arg: &Bson, ctx: &Ctx, kind: Trig) -> R {
    use Trig::*;
    let x = match eval(arg, ctx)? {
        Bson::Null => return Ok(Bson::Null),
        Bson::Int32(n) => n as f64,
        Bson::Int64(n) => n as f64,
        Bson::Double(d) => d,
        _ => return Err(Fallback), // bool / Decimal128 / non-numeric -> Python
    };
    let bad = match kind {
        Sin | Cos | Tan => !x.is_finite(),
        Asin | Acos | Atanh => !(-1.0..=1.0).contains(&x),
        Acosh => x < 1.0,
        _ => false, // atan / sinh / cosh / tanh / asinh accept every finite + inf
    };
    if bad {
        return Err(Fallback);
    }
    if matches!(kind, Atanh) {
        // atanh(±1) = ±inf (Python special-cases to dodge a math domain error).
        if x.abs() == 1.0 {
            return Ok(Bson::Double(if x > 0.0 {
                f64::INFINITY
            } else {
                f64::NEG_INFINITY
            }));
        }
        // libm (and CPython `math.atanh` / mongod) compute atanh with exact odd
        // symmetry; Rust's `f64::atanh` is off by 1 ULP for some negative inputs,
        // so force the symmetry to keep the three servers bit-for-bit.
        return Ok(Bson::Double(if x < 0.0 {
            -((-x).atanh())
        } else {
            x.atanh()
        }));
    }
    Ok(Bson::Double(match kind {
        Sin => x.sin(),
        Cos => x.cos(),
        Tan => x.tan(),
        Asin => x.asin(),
        Acos => x.acos(),
        Atan => x.atan(),
        Sinh => x.sinh(),
        Cosh => x.cosh(),
        Tanh => x.tanh(),
        Asinh => x.asinh(),
        Acosh => x.acosh(),
        Atanh => unreachable!("handled above"),
    }))
}

/// `$atan2`: [y, x] -> atan2(y, x). Null if either arg is null; bool /
/// Decimal128 / non-numeric defers (Python raises `Location51044`).
fn op_atan2(arg: &Bson, ctx: &Ctx) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback);
    }
    if is_null(&vals[0]) || is_null(&vals[1]) {
        return Ok(Bson::Null);
    }
    let extract = |b: &Bson| match b {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(d) => Some(*d),
        _ => None,
    };
    let (Some(y), Some(x)) = (extract(&vals[0]), extract(&vals[1])) else {
        return Err(Fallback);
    };
    Ok(Bson::Double(y.atan2(x)))
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
    Millisecond,
    DayOfWeek,
    DayOfYear,
    Week,
    IsoWeek,
    IsoDayOfWeek,
    IsoWeekYear,
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

/// Resolve a date-extractor operand (`$year`/`$hour`/…) to epoch millis **already
/// shifted into the requested timezone** (so a component read off it is local
/// wall-clock). `Ok(None)` for a null / missing operand (→ `Bson::Null`); a
/// present non-date operand (a string, a number, …) returns `Err(Fallback)` so the
/// Python oracle raises mongod's `Location16006` ("can't convert … to Date").
///
/// mongod accepts a bare date expression *or* a `{date, timezone}` object; the
/// object form is a document with a `date` key that isn't itself an operator
/// (`{$op: …}`). A `timezone` shifts the wall clock: fixed-offset (`±HH:MM` / `UTC`
/// / `GMT`) is constant, a *named* IANA zone resolves its DST-correct offset at the
/// instant via `chrono-tz` (the unambiguous instant→wall-clock direction, matching
/// `$dateToString` and Python `zoneinfo`). An unknown zone / non-string timezone
/// defers to Python.
fn date_operand_millis(arg: &Bson, ctx: &Ctx) -> Result<Option<i64>, Fallback> {
    if let Bson::Document(d) = arg {
        let is_operator = d.len() == 1 && d.keys().next().is_some_and(|k| k.starts_with('$'));
        if d.contains_key("date") && !is_operator {
            let millis = match eval(d.get("date").unwrap(), ctx)? {
                Bson::DateTime(dt) => dt.timestamp_millis(),
                Bson::Null => return Ok(None), // null / missing -> null
                _ => return Err(Fallback),     // non-date -> Python raises Location16006
            };
            let offset_ms = timezone_offset_ms(d.get("timezone"), millis)?;
            return Ok(Some(millis + offset_ms));
        }
    }
    match eval(arg, ctx)? {
        Bson::DateTime(dt) => Ok(Some(dt.timestamp_millis())),
        Bson::Null => Ok(None), // null / missing -> null
        _ => Err(Fallback),     // non-date -> Python raises Location16006
    }
}

fn date_part(arg: &Bson, ctx: &Ctx, part: DatePart) -> R {
    let Some(millis) = date_operand_millis(arg, ctx)? else {
        return Ok(Bson::Null); // null / missing operand -> null
    };
    let days = millis.div_euclid(86_400_000);
    let ms_of_day = millis.rem_euclid(86_400_000);
    let value = match part {
        DatePart::Hour => ms_of_day / 3_600_000,
        DatePart::Minute => (ms_of_day / 60_000) % 60,
        DatePart::Second => (ms_of_day / 1000) % 60,
        DatePart::Millisecond => ms_of_day % 1000,
        // mongod $dayOfWeek: Sunday=1 .. Saturday=7 (1970-01-01 was Thursday=5).
        DatePart::DayOfWeek => (days + 4).rem_euclid(7) + 1,
        // ISO weekday: Monday=1 .. Sunday=7 (1970-01-01 was Thursday=4).
        DatePart::IsoDayOfWeek => (days + 3).rem_euclid(7) + 1,
        DatePart::IsoWeek | DatePart::IsoWeekYear => {
            let (y, m, d) = civil_from_days(days);
            let (iso_year, iso_week) = iso_year_week(y, m, d).ok_or(Fallback)?;
            match part {
                DatePart::IsoWeek => iso_week,
                DatePart::IsoWeekYear => iso_year,
                _ => unreachable!(),
            }
        }
        DatePart::DayOfYear => {
            let (y, m, d) = civil_from_days(days);
            days_from_civil(y, m, d) - days_from_civil(y, 1, 1) + 1
        }
        // US week (mongod $week): weeks start Sunday, 0-53; week 0 is the days
        // before the year's first Sunday. Mirrors strftime `%U`.
        DatePart::Week => {
            let (y, _m, _d) = civil_from_days(days);
            let jan1 = days_from_civil(y, 1, 1);
            let yday0 = days - jan1; // 0-based day of year
                                     // Weekday of Jan 1 with Sunday=0 .. Saturday=6.
            let jan1_wday_sun0 = (jan1 + 4).rem_euclid(7);
            let days_to_first_sunday = (7 - jan1_wday_sun0).rem_euclid(7);
            if yday0 < days_to_first_sunday {
                0
            } else {
                (yday0 - days_to_first_sunday) / 7 + 1
            }
        }
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

/// ISO-8601 (year, week) for a civil date, via `chrono`. `None` (→ defer to
/// Python) when the date falls outside chrono's `NaiveDate` range.
fn iso_year_week(y: i64, m: i64, d: i64) -> Option<(i64, i64)> {
    use chrono::{Datelike, NaiveDate};
    let date = NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m as u32, d as u32)?;
    let iso = date.iso_week();
    Some((iso.year() as i64, iso.week() as i64))
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

/// `$dateFromString` — bounded port of `expressions._op_date_from_string`.
///
/// Handles a `dateString` in canonical ISO-8601 (no `format`, no separate
/// `timezone` field): `YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS`, optionally with a
/// trailing `Z` (UTC) or a fixed `±HH:MM` offset. All produce a whole-second UTC
/// instant, which equals the bson-normalised form of the pure oracle's (possibly
/// tz-aware) datetime.
///
/// A fixed-offset `timezone` field (`±HHMM` / `±HH:MM` / `UTC` / `GMT`) interprets
/// a *naive* dateString as being in that zone (`utc = wall - offset`); a string
/// that already carries a `Z` / offset ignores it, mirroring the pure oracle.
///
/// A string `format` is parsed by `strptime_millis` (the numeric-directive
/// subset); a null/absent format uses the ISO parser above.
///
/// Defers (`Fallback` → Python) on: a *named* IANA `timezone` (needs a tz
/// database), a `format` directive/shape `strptime_millis` doesn't reproduce,
/// **fractional seconds** (BSON is millisecond-only but `fromisoformat` keeps
/// microseconds), a space separator or other non-canonical/offset shape, an
/// out-of-range/invalid field, or a non-string `dateString`. A null `dateString`
/// returns `onNull` (or null).
fn op_date_from_string(arg: &Bson, ctx: &Ctx) -> R {
    let spec = arg.as_document().ok_or(Fallback)?;
    // A string `format` selects strptime; a null/absent format uses ISO parsing.
    let format = match spec.get("format") {
        None | Some(Bson::Null) => None,
        Some(Bson::String(f)) => Some(f.as_str()),
        Some(_) => return Err(Fallback), // non-string format -> Python
    };
    // A fixed-offset `timezone` field interprets a *naive* dateString as being in
    // that zone (`utc = wall - offset`); a named zone / malformed offset defers.
    let tz_offset = match spec.get("timezone") {
        None => None,
        Some(Bson::String(s)) => Some(resolve_tz_offset(s).ok_or(Fallback)?),
        Some(_) => return Err(Fallback), // Python raises "timezone must be a string"
    };
    let raw = match spec.get("dateString") {
        Some(e) => eval(e, ctx)?,
        None => Bson::Null,
    };
    if matches!(raw, Bson::Null) {
        return match spec.get("onNull") {
            Some(e) => eval(e, ctx),
            None => Ok(Bson::Null),
        };
    }
    let Bson::String(s) = raw else {
        return Err(Fallback); // Python raises "dateString must be a string"
    };
    // strptime always yields a naive instant, so the tz always applies; the ISO
    // path applies it only when the string carries no offset of its own (the pure
    // oracle's `parsed.tzinfo is None` guard).
    let (millis, apply_tz) = match format {
        Some(fmt) => (strptime_millis(&s, fmt).ok_or(Fallback)?, true),
        None => {
            let has_embedded_offset =
                s.ends_with('Z') || (s.len() == 25 && matches!(s.as_bytes()[19], b'+' | b'-'));
            (parse_iso(&s).ok_or(Fallback)?, !has_embedded_offset)
        }
    };
    let millis = match tz_offset {
        Some(off) if apply_tz => millis - off as i128 * 60_000,
        _ => millis,
    };
    bounded_datetime(millis)
}

/// `$dateToString` — bounded port of `expressions._op_date_to_string`. Formats a
/// date (UTC) per a strftime-style `format`, defaulting to
/// `"%Y-%m-%dT%H:%M:%S.%LZ"`. A non-datetime `date` (incl. null) → null.
///
/// A `timezone` shifts the wall clock before rendering (naive input is treated as
/// UTC): a fixed offset (`±HHMM` / `±HH:MM` / `UTC` / `GMT`) is constant, and a
/// *named* IANA zone (`America/New_York`, …) resolves its DST-correct offset at the
/// rendered instant via `chrono-tz`. An unknown zone name defers to Python.
///
/// Renders the unambiguous directives — `%Y` (4-digit year), `%m`/`%d`/`%H`/`%M`/
/// `%S` (2-digit), `%L` (3-digit millis), `%j` (3-digit day-of-year), `%w`
/// (mongod Sunday=1..7), `%u` (ISO Monday=1..7), `%%` — and defers (`Fallback`)
/// on an unknown/malformed `timezone`, any other directive (`%z`/`%Z`/`%G`/`%V`/
/// `%U`/locale names — Python `strftime` handles those), a non-4-digit year (glibc
/// `%Y` padding differs), or a non-string `format`.
fn op_date_to_string(arg: &Bson, ctx: &Ctx) -> R {
    let spec = arg.as_document().ok_or(Fallback)?;
    let millis = match eval(spec.get("date").ok_or(Fallback)?, ctx)? {
        Bson::DateTime(dt) => dt.timestamp_millis(),
        Bson::Null => return Ok(Bson::Null), // null date -> null
        _ => return Err(Fallback),           // non-date -> Python raises Location16006
    };
    // A `timezone` shifts the wall clock before rendering (naive input is UTC,
    // matching BSON Date semantics); see `timezone_offset_ms` for the fixed-offset
    // vs named-zone resolution.
    let tz_offset = timezone_offset_ms(spec.get("timezone"), millis)?;
    let fmt = match spec.get("format") {
        None => "%Y-%m-%dT%H:%M:%S.%LZ",
        Some(Bson::String(s)) => s.as_str(),
        Some(_) => return Err(Fallback), // Python raises "format must be a string"
    };
    Ok(Bson::String(render_date(millis + tz_offset, fmt)?))
}

fn render_date(millis: i64, fmt: &str) -> Result<String, Fallback> {
    let days = millis.div_euclid(86_400_000);
    let ms_of_day = millis.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    if !(1000..=9999).contains(&y) {
        return Err(Fallback); // glibc `%Y` zero-pads only in this range
    }
    let hh = ms_of_day / 3_600_000;
    let mi = (ms_of_day / 60_000) % 60;
    let ss = (ms_of_day / 1000) % 60;
    let frac_ms = ms_of_day % 1000;
    let py_weekday = (days + 3).rem_euclid(7); // 1970-01-01 = Thursday(3); Mon=0..Sun=6
    let day_of_year = days - days_from_civil(y, 1, 1) + 1;
    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{y:04}")),
            Some('m') => out.push_str(&format!("{m:02}")),
            Some('d') => out.push_str(&format!("{d:02}")),
            Some('H') => out.push_str(&format!("{hh:02}")),
            Some('M') => out.push_str(&format!("{mi:02}")),
            Some('S') => out.push_str(&format!("{ss:02}")),
            Some('L') => out.push_str(&format!("{frac_ms:03}")),
            Some('j') => out.push_str(&format!("{day_of_year:03}")),
            Some('w') => out.push_str(&(((py_weekday + 1) % 7) + 1).to_string()),
            Some('u') => out.push_str(&(py_weekday + 1).to_string()),
            Some('%') => out.push('%'),
            _ => return Err(Fallback), // unknown directive -> Python strftime
        }
    }
    Ok(out)
}

/// Parse ISO-8601 into epoch milliseconds (UTC). Accepts the naive canonical
/// forms treated as UTC, plus a full datetime with a trailing `Z` or a fixed
/// `±HH:MM` offset (`utc = wall - offset`). Fractional seconds / other shapes →
/// `None` (defer).
fn parse_iso(s: &str) -> Option<i128> {
    if let Some(base) = s.strip_suffix('Z') {
        // A `Z` designator only follows a full datetime.
        return if base.len() == 19 {
            parse_naive(base)
        } else {
            None
        };
    }
    // A full datetime followed by a `±HH:MM` offset is exactly 25 chars.
    if s.len() == 25 && matches!(s.as_bytes()[19], b'+' | b'-') {
        let (base, tz) = s.split_at(19);
        let off_min = parse_offset(tz)?;
        return parse_naive(base).map(|ms| ms - off_min as i128 * 60_000);
    }
    parse_naive(s)
}

/// `$dateFromString` `format` (strptime) for the bounded numeric-directive subset
/// — epoch millis (UTC / naive), or `None` (defer to the pure oracle) for an
/// unsupported directive, a non-matching input, or an out-of-range field.
///
/// The format is translated into a regex built from CPython `_strptime`'s *exact*
/// per-directive sub-patterns, so field matching is identical by construction;
/// `\A…\z` requires the whole input to be consumed (Python's "unconverted data
/// remains" check). Supported directives: `%Y` `%y` `%m` `%d` `%H` `%M` `%S` `%j`
/// `%%`; whitespace runs match `\s+` and literals match themselves
/// (case-insensitively, like `TimeRE`). Any other directive, `%j` combined with
/// `%m`/`%d`, a second of 60/61 (leap second — `datetime` rejects it), or an
/// invalid day-of-month defers to Python.
fn strptime_millis(data: &str, format: &str) -> Option<i128> {
    let mut pat = String::from(r"(?i)\A");
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let frag = match chars.next()? {
                'Y' => r"(?P<Y>\d\d\d\d)",
                'y' => r"(?P<y>\d\d)",
                'm' => r"(?P<m>1[0-2]|0[1-9]|[1-9])",
                'd' => r"(?P<d>3[0-1]|[1-2]\d|0[1-9]|[1-9]| [1-9])",
                'H' => r"(?P<H>2[0-3]|[0-1]\d|\d)",
                'M' => r"(?P<M>[0-5]\d|\d)",
                'S' => r"(?P<S>6[0-1]|[0-5]\d|\d)",
                'j' => r"(?P<j>36[0-6]|3[0-5]\d|[1-2]\d\d|0[1-9]\d|00[1-9]|[1-9]\d|0[1-9]|[1-9])",
                '%' => {
                    pat.push('%');
                    continue;
                }
                _ => return None, // unsupported directive -> defer
            };
            pat.push_str(frag);
        } else if c.is_whitespace() {
            pat.push_str(r"\s+");
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
        } else {
            pat.push_str(&regex::escape(&c.to_string()));
        }
    }
    pat.push_str(r"\z");
    let re = regex::Regex::new(&pat).ok()?;
    let caps = re.captures(data)?;
    let num = |name: &str| -> Option<i64> { caps.name(name)?.as_str().trim().parse().ok() };
    // Year: %Y (4-digit) or %y (2-digit, Python's 00-68→2000s / 69-99→1900s
    // pivot), else the strptime default of 1900.
    let year = match num("Y") {
        Some(y) => y,
        None => match num("y") {
            Some(y) if y <= 68 => 2000 + y,
            Some(y) => 1900 + y,
            None => 1900,
        },
    };
    let (month, day) = match num("j") {
        Some(j) => {
            if caps.name("m").is_some() || caps.name("d").is_some() {
                return None; // %j combined with %m/%d -> defer
            }
            let (yy, mm, dd) = civil_from_days(days_from_civil(year, 1, 1) + (j - 1));
            if yy != year {
                return None; // day-of-year overflowed the year -> defer
            }
            (mm, dd)
        }
        None => (num("m").unwrap_or(1), num("d").unwrap_or(1)),
    };
    let (hh, mi, ss) = (
        num("H").unwrap_or(0),
        num("M").unwrap_or(0),
        num("S").unwrap_or(0),
    );
    // The regexes already bound most fields; still reject an invalid day-of-month
    // and a 60/61 leap second (which `datetime` raises on) -> defer.
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) || ss > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days as i128 * 86_400_000 + (hh * 3_600_000 + mi * 60_000 + ss * 1000) as i128)
}

/// Resolve a MongoDB `timezone` field to a fixed UTC offset in signed minutes,
/// mirroring the offset branch of the pure `_resolve_timezone`. Handles the UTC
/// aliases (`UTC` / `GMT` / `Etc/UTC` / `Etc/GMT` → 0) and a `±HHMM` / `±HH:MM`
/// offset (four digits after the sign, colon optional). Returns `None` for a
/// named IANA zone (needs a tz database) or a malformed offset — the caller
/// defers those to Python, which resolves the zone or raises.
fn resolve_tz_offset(name: &str) -> Option<i64> {
    match name {
        "UTC" | "GMT" | "Etc/UTC" | "Etc/GMT" => Some(0),
        _ if name.starts_with(['+', '-']) => {
            let sign = if name.starts_with('-') { -1 } else { 1 };
            let digits: String = name[1..].chars().filter(|c| *c != ':').collect();
            if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None; // malformed -> defer (Python raises)
            }
            let hh: i64 = digits[..2].parse().ok()?;
            let mm: i64 = digits[2..].parse().ok()?;
            // Python's datetime.timezone rejects a magnitude >= 24h; defer those
            // (and any out-of-range minute) so the oracle decides.
            if hh > 23 || mm > 59 {
                return None;
            }
            Some(sign * (hh * 60 + mm))
        }
        _ => None, // named zone -> defer
    }
}

/// Resolve a *named* IANA zone (`"America/New_York"`, `"Europe/Dublin"`, …) to its
/// signed UTC offset **in milliseconds at the given UTC instant** — DST-correct,
/// since the offset of a zone depends on when you ask. Returns `None` for a name
/// `chrono-tz` doesn't know (the caller defers to Python, which resolves it or
/// raises "unknown timezone").
///
/// This is the *instant → wall-clock* direction, which is total and unambiguous:
/// every UTC instant maps to exactly one local time in a zone (unlike the reverse,
/// where a DST gap/overlap makes a naive local time ambiguous). So `chrono-tz` and
/// Python `zoneinfo` return the identical offset for any instant whose governing
/// rule both tz databases agree on — which is why `$dateToString` can compute
/// natively here while `$dateFromString`'s named-zone form still defers.
fn named_tz_offset_ms(name: &str, utc_millis: i64) -> Option<i64> {
    use chrono::{DateTime, Offset, TimeZone, Utc};
    let tz: chrono_tz::Tz = name.parse().ok()?;
    let secs = utc_millis.div_euclid(1000);
    let nanos = (utc_millis.rem_euclid(1000) * 1_000_000) as u32;
    let instant: DateTime<Utc> = DateTime::from_timestamp(secs, nanos)?;
    let offset_secs = tz
        .offset_from_utc_datetime(&instant.naive_utc())
        .fix()
        .local_minus_utc();
    Some(offset_secs as i64 * 1000)
}

/// Resolve a `timezone` spec field to a signed wall-clock offset in **milliseconds
/// at the given UTC instant**, for the instant→wall-clock date operators
/// (`$dateToString`, the `{date, timezone}` extractors, `$dateToParts`). Absent /
/// `null` timezone → 0 (UTC), matching Python's `_resolve_timezone(None)`; a
/// fixed-offset (`±HH:MM` / `UTC` / `GMT`) is constant; a named IANA zone resolves
/// its DST-correct offset at the instant via `chrono-tz`. An unknown zone name or a
/// non-string timezone → `Fallback` (defer to the Python oracle, which resolves it
/// or raises). This is *not* used by `$dateFromString`, whose named-zone
/// (local→instant) direction is DST-ambiguous and deliberately defers.
fn timezone_offset_ms(tz: Option<&Bson>, utc_millis: i64) -> Result<i64, Fallback> {
    match tz {
        None | Some(Bson::Null) => Ok(0),
        Some(Bson::String(s)) => match resolve_tz_offset(s) {
            Some(off_min) => Ok(off_min * 60_000),
            None => named_tz_offset_ms(s, utc_millis).ok_or(Fallback),
        },
        Some(_) => Err(Fallback), // Python raises "timezone must be a string"
    }
}

/// A fixed `±HH:MM` UTC offset in signed minutes, or `None` if malformed.
fn parse_offset(tz: &str) -> Option<i64> {
    let b = tz.as_bytes();
    if b.len() != 6 || b[3] != b':' {
        return None;
    }
    let sign = match b[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hh: i64 = tz.get(1..3)?.parse().ok()?;
    let mm: i64 = tz.get(4..6)?.parse().ok()?;
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(sign * (hh * 60 + mm))
}

/// Parse strict canonical naive ISO-8601 (`YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS`)
/// into epoch milliseconds (treating the wall clock as UTC), or `None` for a
/// non-fixed-width shape or out-of-range field. Fixed-width so it can't drift
/// from `fromisoformat`.
fn parse_naive(s: &str) -> Option<i128> {
    let b = s.as_bytes();
    // Only date-only or whole-second forms (fractional seconds defer — see above).
    if b.len() != 10 && b.len() != 19 {
        return None;
    }
    let digits = |lo: usize, hi: usize| -> Option<i64> {
        let part = s.get(lo..hi)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y = digits(0, 4)?;
    let m = digits(5, 7)?;
    let d = digits(8, 10)?;
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let (mut hh, mut mi, mut ss) = (0i64, 0i64, 0i64);
    if b.len() == 19 {
        if b[10] != b'T' || b[13] != b':' || b[16] != b':' {
            return None;
        }
        hh = digits(11, 13)?;
        mi = digits(14, 16)?;
        ss = digits(17, 19)?;
        if hh > 23 || mi > 59 || ss > 59 {
            return None;
        }
    }
    let day_ms = days_from_civil(y, m, d) as i128 * 86_400_000;
    let time_ms = ((hh * 3600 + mi * 60 + ss) * 1000) as i128;
    Some(day_ms + time_ms)
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
/// An integer date argument (amount / binSize): an int or a whole double; a
/// fractional double / bool / non-numeric -> None (defer so Python raises the
/// mongod error rather than coercing a bool or rejecting a valid whole double).
fn date_int(v: &Bson) -> Option<i128> {
    match v {
        Bson::Int32(n) => Some(*n as i128),
        Bson::Int64(n) => Some(*n as i128),
        Bson::Double(d) if d.fract() == 0.0 => Some(*d as i128),
        _ => None,
    }
}

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
    let Some(amount) = date_int(&amount_v) else {
        return Err(Fallback); // fractional / bool / non-numeric -> Python raises 5166405
    };
    shift_date(start_dt.timestamp_millis(), &unit, sign * amount)
}

fn op_date_diff(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    // A missing required *parameter* (key absent) is a mongod parse error
    // (Location5166303/4/5) -> Python raises; a present-but-null one yields null.
    for param in ["startDate", "endDate", "unit"] {
        if !d.contains_key(param) {
            return Err(Fallback);
        }
    }
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
        Some(e) => match date_int(&eval(e, ctx)?) {
            Some(n) if n >= 1 => n as i64,
            // fractional / bool / non-numeric (5439017) or < 1 (5439018) -> Python.
            _ => return Err(Fallback),
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
    // $toInt targets int32: an int64 or a double outside [i32::MIN, i32::MAX]
    // overflows (Python raises 241). Non-finite / Decimal128 / string -> Python.
    let n: i64 = match eval(arg, ctx)? {
        Bson::Null => return Ok(Bson::Null),
        Bson::Int32(v) => v as i64,
        Bson::Int64(v) => v,
        Bson::Boolean(b) => i64::from(b),
        Bson::Double(d) => {
            if !d.is_finite() {
                return Err(Fallback);
            }
            let t = d.trunc();
            if t < i32::MIN as f64 || t > i32::MAX as f64 {
                return Err(Fallback);
            }
            t as i64
        }
        _ => return Err(Fallback),
    };
    if !(i32::MIN as i64..=i32::MAX as i64).contains(&n) {
        return Err(Fallback); // overflow int32 -> Python raises 241
    }
    Ok(Bson::Int32(n as i32))
}

fn op_to_long(arg: &Bson, ctx: &Ctx) -> R {
    // $toLong targets int64: a double is truncated toward zero (out of i64 range
    // -> Python raises 241). Non-finite / Decimal128 / string -> Python.
    let n: i64 = match eval(arg, ctx)? {
        Bson::Null => return Ok(Bson::Null),
        Bson::Int32(v) => v as i64,
        Bson::Int64(v) => v,
        Bson::Boolean(b) => i64::from(b),
        Bson::Double(d) => {
            if !d.is_finite() {
                return Err(Fallback);
            }
            let t = d.trunc();
            // [-2^63, 2^63): a double at or beyond 2^63 can't be an i64.
            if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&t) {
                return Err(Fallback); // overflow int64 -> Python raises 241
            }
            t as i64
        }
        _ => return Err(Fallback),
    };
    Ok(Bson::Int64(n))
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

fn op_to_decimal(arg: &Bson, ctx: &Ctx) -> R {
    let v = eval(arg, ctx)?;
    match v {
        Bson::Null => Ok(Bson::Null),
        Bson::Decimal128(_) => Ok(v),
        Bson::Int32(n) => decimal_from_str(&n.to_string()),
        Bson::Int64(n) => decimal_from_str(&n.to_string()),
        Bson::Boolean(b) => decimal_from_str(if b { "1" } else { "0" }),
        // mongod converts a double at a fixed 15 significant digits, so
        // `$toDecimal: 4.125` is `4.12500000000000` — the shortest round-trip
        // text would answer `4.125`, a different quantum. (The `$sum`/`$avg`
        // accumulators use a *different* rule; see `crate::decimal`.)
        Bson::Double(d) if d.is_finite() => crate::decimal::from_bson(&Bson::Double(d))
            .and_then(|v| crate::decimal::to_bson(&v))
            .ok_or(Fallback),
        Bson::String(ref s) => decimal_from_str(s),
        _ => Err(Fallback),
    }
}

fn decimal_from_str(s: &str) -> R {
    s.parse::<bson::Decimal128>()
        .map(Bson::Decimal128)
        .map_err(|_| Fallback)
}

/// `$toDate: <expr>` — shorthand for `$convert: {input: <expr>, to: "date"}`.
/// Delegates to the exact `convert_value(.., 9)` date path so the two stay
/// identical. `null` -> null; a `DateTime` is returned unchanged; every other
/// source type (int/long/double millis, ISO string, ObjectId) defers to Python,
/// mirroring the `$convert` date path (which defers those to keep the tz-aware /
/// naive datetime parity intact).
fn op_to_date(arg: &Bson, ctx: &Ctx) -> R {
    let v = eval(arg, ctx)?;
    if matches!(v, Bson::Null) {
        return Ok(Bson::Null);
    }
    match convert_value(&v, 9) {
        Conv::Ok(out) => Ok(out),
        Conv::Failed | Conv::Unsupported => Err(Fallback),
    }
}

/// `$convert` outcome for one (value, target): a successful conversion, a
/// supported conversion that *failed* (Python would raise → `onError` applies),
/// or a target/source combination this bounded port doesn't implement (defer the
/// whole `$convert` to Python).
enum Conv {
    Ok(Bson),
    Failed,
    Unsupported,
}

/// `$convert` (`{input, to, onNull?, onError?}`) — bounded port of
/// `expressions._op_convert`. Numeric / bool / decimal conversions are handled
/// here; string / objectId targets and string/Decimal128 numeric sources defer
/// to Python. `null` input → `onNull` (or null); a failed *supported* conversion
/// → `onError` (or defer so Python raises the same error).
fn op_convert(arg: &Bson, ctx: &Ctx) -> R {
    let spec = arg.as_document().ok_or(Fallback)?;
    let (Some(input), Some(to)) = (spec.get("input"), spec.get("to")) else {
        return Err(Fallback); // Python raises: requires {input, to}
    };
    let value = eval(input, ctx)?;
    let target = eval(to, ctx)?;
    if matches!(value, Bson::Null) {
        return match spec.get("onNull") {
            Some(on) => eval(on, ctx),
            None => Ok(Bson::Null),
        };
    }
    let code = convert_target_code(&target).ok_or(Fallback)?;
    match convert_value(&value, code) {
        Conv::Ok(v) => Ok(v),
        Conv::Failed => match spec.get("onError") {
            Some(on) => eval(on, ctx),
            None => Err(Fallback), // Python raises "$convert failed"
        },
        Conv::Unsupported => Err(Fallback),
    }
}

/// mongod's `$convert` target codes (`expressions._CONVERT_TARGETS`): the string
/// alias or the numeric code. A double / unknown target -> `None` (Python raises).
fn convert_target_code(target: &Bson) -> Option<i32> {
    let code = match target {
        Bson::String(s) => match s.as_str() {
            "double" => 1,
            "string" => 2,
            "objectId" => 7,
            "bool" => 8,
            "date" => 9,
            "int" => 16,
            "long" => 18,
            "decimal" => 19,
            _ => return None,
        },
        Bson::Int32(n) => *n,
        Bson::Int64(n) => *n as i32,
        _ => return None,
    };
    matches!(code, 1 | 2 | 7 | 8 | 9 | 16 | 18 | 19).then_some(code)
}

fn convert_value(value: &Bson, code: i32) -> Conv {
    match code {
        // double
        1 => match value {
            Bson::Boolean(b) => Conv::Ok(Bson::Double(if *b { 1.0 } else { 0.0 })),
            Bson::Int32(n) => Conv::Ok(Bson::Double(*n as f64)),
            Bson::Int64(n) => Conv::Ok(Bson::Double(*n as f64)),
            Bson::Double(_) => Conv::Ok(value.clone()),
            _ => Conv::Unsupported, // Decimal128 / string / date -> Python
        },
        // bool — non-(bool/int/double/string/Decimal128) is truthy (Python's else).
        8 => match value {
            Bson::Boolean(_) => Conv::Ok(value.clone()),
            Bson::Int32(n) => Conv::Ok(Bson::Boolean(*n != 0)),
            Bson::Int64(n) => Conv::Ok(Bson::Boolean(*n != 0)),
            Bson::Double(d) => Conv::Ok(Bson::Boolean(*d != 0.0)),
            Bson::String(s) => Conv::Ok(Bson::Boolean(!s.is_empty())),
            Bson::Decimal128(_) => Conv::Unsupported, // decimal compare -> Python
            _ => Conv::Ok(Bson::Boolean(true)),
        },
        // int (16) / long (18)
        16 | 18 => match value {
            Bson::Boolean(b) => wrap_int(i128::from(*b), code),
            Bson::Int32(n) => wrap_int(*n as i128, code),
            Bson::Int64(n) => wrap_int(*n as i128, code),
            Bson::Double(d) if d.is_finite() && *d >= i64::MIN as f64 && *d <= i64::MAX as f64 => {
                wrap_int(d.trunc() as i128, code)
            }
            Bson::Double(_) => Conv::Failed, // int(inf/overflow) raises -> onError
            _ => Conv::Unsupported,          // Decimal128 / string -> Python
        },
        // decimal (the full $toDecimal set, incl. parseable strings)
        19 => match value {
            Bson::Decimal128(_) => Conv::Ok(value.clone()),
            Bson::Boolean(b) => decimal_conv(if *b { "1" } else { "0" }),
            Bson::Int32(n) => decimal_conv(&n.to_string()),
            Bson::Int64(n) => decimal_conv(&n.to_string()),
            // 15 significant digits, as `$toDecimal` — mongod-probed 6.0.16.
            Bson::Double(d) if d.is_finite() => {
                match crate::decimal::from_bson(&Bson::Double(*d))
                    .and_then(|v| crate::decimal::to_bson(&v))
                {
                    Some(b) => Conv::Ok(b),
                    None => Conv::Unsupported,
                }
            }
            Bson::String(s) => decimal_conv(s),
            _ => Conv::Unsupported,
        },
        // date — passthrough only. int/float/string -> Python: the oracle
        // builds a *tz-aware* datetime for the numeric path, which wouldn't
        // compare equal to a bson-decoded naive datetime, so we defer it.
        9 => match value {
            // bool -> date is a *supported-but-failed* conversion (mongod 241), so
            // `$convert`'s onError applies; without onError, Python raises 241.
            Bson::Boolean(_) => Conv::Failed,
            Bson::DateTime(_) => Conv::Ok(value.clone()),
            // int / long / double: milliseconds since the Unix epoch -> date.
            Bson::Int32(n) => Conv::Ok(Bson::DateTime(bson::DateTime::from_millis(*n as i64))),
            Bson::Int64(n) => Conv::Ok(Bson::DateTime(bson::DateTime::from_millis(*n))),
            Bson::Double(d) if d.is_finite() => {
                Conv::Ok(Bson::DateTime(bson::DateTime::from_millis(*d as i64)))
            }
            Bson::Double(_) => Conv::Failed, // inf/NaN -> onError / raise
            // string (ISO parse) / objectId (embedded timestamp) -> Python oracle.
            _ => Conv::Unsupported,
        },
        _ => Conv::Unsupported,
    }
}

fn wrap_int(n: i128, code: i32) -> Conv {
    // int (16) targets int32, long (18) targets int64; out of range overflows
    // (Python raises 241 -> onError, else "$convert failed").
    let (lo, hi) = if code == 18 {
        (i64::MIN as i128, i64::MAX as i128)
    } else {
        (i32::MIN as i128, i32::MAX as i128)
    };
    if !(lo..=hi).contains(&n) {
        return Conv::Failed;
    }
    if code == 18 {
        Conv::Ok(Bson::Int64(n as i64))
    } else {
        Conv::Ok(Bson::Int32(n as i32))
    }
}

fn decimal_conv(s: &str) -> Conv {
    match s.parse::<bson::Decimal128>() {
        Ok(d) => Conv::Ok(Bson::Decimal128(d)),
        Err(_) => Conv::Failed,
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

// --- regex expression operators -----------------------------------------
// $regexMatch / $regexFind / $regexFindAll, mirroring the pure-Python ops. Each
// evaluates `input` first and, if it isn't a string, returns the empty result
// (false / null / []) *before* touching the regex — matching Python's
// short-circuit. `regex` + optional `options` are compiled via the shared
// `regexutil` (linear engine, else backtracking). $regexMatch uses `is_match`
// and the positional finds use `captures` — both served by whichever engine
// compiled the pattern (the backtracking `fancy-regex` is Python-`re`-compatible,
// so lookaround / backreference captures parity-match too); only a pattern
// neither engine compiles, or a backtrack-limit error, defers.

/// Compile `{regex, options?}` from an already-validated (input-is-string) spec.
fn compile_regex(spec: &Document, ctx: &Ctx) -> Result<regexutil::CompiledRegex, Fallback> {
    let pattern = match spec.get("regex") {
        Some(e) => eval(e, ctx)?,
        None => return Err(Fallback), // Python `_resolve_regex` raises
    };
    let options = match spec.get("options") {
        Some(e) => Some(eval(e, ctx)?),
        None => None,
    };
    regexutil::compile(&pattern, options.as_ref()).map_err(|_| Fallback)
}

/// The evaluated `input`, or `None` when it isn't a string (caller returns the
/// operator's empty value without compiling the regex — Python's short-circuit).
fn regex_input(spec: &Document, ctx: &Ctx) -> Result<Option<String>, Fallback> {
    let input = match spec.get("input") {
        Some(e) => eval(e, ctx)?,
        None => Bson::Null,
    };
    Ok(match input {
        Bson::String(s) => Some(s),
        Bson::Null => None,
        // A non-string, non-null input is mongod Location51104 -> Python raises.
        _ => return Err(Fallback),
    })
}

/// `{match, idx, captures}` for one hit — `idx` is a code-point index, captures
/// are the matched substring or `null` (Python `m.groups()`).
fn regex_match_doc(m: regexutil::RegexMatch) -> Bson {
    let captures: Vec<Bson> = m
        .captures
        .into_iter()
        .map(|c| c.map(Bson::String).unwrap_or(Bson::Null))
        .collect();
    let mut d = Document::new();
    d.insert("match", Bson::String(m.text));
    d.insert("idx", Bson::Int32(m.codepoint_idx as i32));
    d.insert("captures", Bson::Array(captures));
    Bson::Document(d)
}

fn op_regex_match(arg: &Bson, ctx: &Ctx) -> R {
    let spec = arg.as_document().ok_or(Fallback)?;
    let Some(s) = regex_input(spec, ctx)? else {
        return Ok(Bson::Boolean(false));
    };
    let re = compile_regex(spec, ctx)?;
    Ok(Bson::Boolean(re.is_match(&s)))
}

fn op_regex_find(arg: &Bson, ctx: &Ctx) -> R {
    let spec = arg.as_document().ok_or(Fallback)?;
    let Some(s) = regex_input(spec, ctx)? else {
        return Ok(Bson::Null);
    };
    let re = compile_regex(spec, ctx)?;
    match re.find_first(&s).map_err(|_| Fallback)? {
        None => Ok(Bson::Null),
        Some(m) => Ok(regex_match_doc(m)),
    }
}

fn op_regex_find_all(arg: &Bson, ctx: &Ctx) -> R {
    let spec = arg.as_document().ok_or(Fallback)?;
    let Some(s) = regex_input(spec, ctx)? else {
        return Ok(Bson::Array(Vec::new()));
    };
    let re = compile_regex(spec, ctx)?;
    let matches = re.find_all(&s).map_err(|_| Fallback)?;
    Ok(Bson::Array(
        matches.into_iter().map(regex_match_doc).collect(),
    ))
}

// --- math ---------------------------------------------------------------

/// The f64 for a unary math operator, deferring bool *and* non-numeric to Python
/// (which raises Location28765 / 51081 rather than coercing a bool or computing
/// on a string). Real numbers only; null is handled by the caller.
fn math_float(v: &Bson) -> Result<f64, Fallback> {
    if matches!(v, Bson::Boolean(_)) {
        return Err(Fallback);
    }
    as_float_like(v).ok_or(Fallback)
}

fn op_abs(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::Int32(n) => int_to_bson((n as i128).abs()).ok_or(Fallback),
        Bson::Int64(n) => int_to_bson((n as i128).abs()).ok_or(Fallback),
        Bson::Double(d) => Ok(Bson::Double(d.abs())),
        // bool / Decimal128 / non-numeric: Python raises 28765 -> defer.
        _ => Err(Fallback),
    }
}

fn op_floor_ceil(arg: &Bson, ctx: &Ctx, ceil: bool) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        // math.floor/ceil of an int returns it unchanged.
        v @ (Bson::Int32(_) | Bson::Int64(_)) => Ok(v),
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
            let f = math_float(&v)?; // bool / Decimal128 / non-numeric -> Python
                                     // A negative argument is mongod's Location28714 — defer so
                                     // Python raises it (NaN passes through as sqrt(nan) = nan).
            if f < 0.0 {
                Err(Fallback)
            } else {
                Ok(Bson::Double(f.sqrt()))
            }
        }
    }
}

// `$exp`: e**x. Numeric -> Double; null -> null; bool / Decimal128 / non-numeric
// -> Python (which raises 28765). Mirrors `expressions._op_exp`.
fn op_exp(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        v => Ok(Bson::Double(math_float(&v)?.exp())),
    }
}

// `$ln`: natural log. A non-positive argument is mongod's Location28766 —
// defer so Python raises it (NaN passes through as ln(nan) = nan). Mirrors
// `expressions._op_ln`.
fn op_ln(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        v => {
            let f = math_float(&v)?;
            if f <= 0.0 {
                Err(Fallback)
            } else {
                Ok(Bson::Double(f.ln()))
            }
        }
    }
}

fn op_log10(arg: &Bson, ctx: &Ctx) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        v => {
            let f = math_float(&v)?;
            // A non-positive argument is mongod's Location28761 — defer so
            // Python raises it (NaN passes through as log10(nan) = nan).
            if f <= 0.0 {
                Err(Fallback)
            } else {
                Ok(Bson::Double(f.log10()))
            }
        }
    }
}

// `$log`: [number, base] -> log_base(number). Null if any arg is null; an
// out-of-domain arg (n <= 0, base <= 0, base == 1) defers so Python raises
// mongod's Location28758/28759. Mirrors `expressions._op_log`.
fn op_log(arg: &Bson, ctx: &Ctx) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback); // Python raises on a non-2 arg
    }
    if is_null(&vals[0]) || is_null(&vals[1]) {
        return Ok(Bson::Null);
    }
    // bool / non-numeric argument or base is mongod's Location28756 / 28757 —
    // defer so Python raises them (math_float rejects bool, unlike as_float_like).
    let (Ok(n), Ok(base)) = (math_float(&vals[0]), math_float(&vals[1])) else {
        return Err(Fallback);
    };
    // Out-of-domain args are mongod's Location28758 (argument) / 28759
    // (base) — defer so Python raises them (NaN passes through as nan).
    if n <= 0.0 || base <= 0.0 || base == 1.0 {
        return Err(Fallback);
    }
    // CPython's math.log(n, base) is log(n)/log(base); same operations -> same
    // result under the shared platform libm.
    Ok(Bson::Double(n.ln() / base.ln()))
}

// `$pow`: [base, exp]. Both integer types with a non-negative exponent give an
// exact integer (Python `int**int`); anything else (a float operand, a negative
// exponent, or integer overflow past int64) is a Double / defers. Mirrors
// `expressions._op_pow`.
fn op_pow(arg: &Bson, ctx: &Ctx) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback);
    }
    if is_null(&vals[0]) || is_null(&vals[1]) {
        return Ok(Bson::Null);
    }
    // A bool operand defers (Python raises 28762 base / 28763 exponent);
    // as_float_like would otherwise coerce it to 1.0.
    if matches!(vals[0], Bson::Boolean(_)) || matches!(vals[1], Bson::Boolean(_)) {
        return Err(Fallback);
    }
    if let (b @ (Bson::Int32(_) | Bson::Int64(_)), e @ (Bson::Int32(_) | Bson::Int64(_))) =
        (&vals[0], &vals[1])
    {
        let base = as_int_like(b).unwrap();
        let exp = as_int_like(e).unwrap();
        if exp >= 0 {
            // checked_pow on i128, then narrow; overflow / exp > u32 -> Python
            // (a bignum pymongo couldn't encode anyway).
            return u32::try_from(exp)
                .ok()
                .and_then(|e| base.checked_pow(e))
                .and_then(int_to_bson)
                .ok_or(Fallback);
        }
        // negative exponent -> float, handled below.
    }
    let (Some(a), Some(b)) = (as_float_like(&vals[0]), as_float_like(&vals[1])) else {
        return Err(Fallback);
    };
    if a == 0.0 && b < 0.0 {
        return Err(Fallback); // Python raises 28764 (base 0, negative exponent)
    }
    // f64::powf already yields NaN for a negative base with a fractional
    // exponent (matching mongod / the Python complex->NaN guard).
    Ok(Bson::Double(a.powf(b)))
}

// Shared arg parse for `$round` / `$trunc`: `[n, place?]` or a bare `n`. A
// non-integer `place` becomes 0 (mirrors the Python impls). An empty list defers
// (Python raises / index-errors).
fn round_trunc_args(arg: &Bson, ctx: &Ctx) -> Result<(Bson, i32), Fallback> {
    match arg {
        Bson::Array(a) if !a.is_empty() => {
            let n = eval(&a[0], ctx)?;
            let place = if a.len() > 1 {
                match eval(&a[1], ctx)? {
                    Bson::Boolean(_) => return Err(Fallback), // Python raises 16004
                    Bson::Int32(i) => i,
                    Bson::Int64(i) => i as i32,
                    Bson::Double(d) if d.is_finite() && d.fract() == 0.0 => d as i32,
                    Bson::Double(_) => return Err(Fallback), // fractional -> Python raises 51082
                    _ => 0,
                }
            } else {
                0
            };
            Ok((n, place))
        }
        Bson::Array(_) => Err(Fallback),
        other => Ok((eval(other, ctx)?, 0)),
    }
}

// `$round`: round-half-to-even (Python `round`). An int stays an int (unchanged
// for place >= 0; rounded to the 10^|place| place for place < 0); a double rounds
// to `place` decimals as a double. Mirrors `expressions._op_round`.
fn op_round(arg: &Bson, ctx: &Ctx) -> R {
    let (n, place) = round_trunc_args(arg, ctx)?;
    match n {
        Bson::Null => Ok(Bson::Null),
        Bson::Int32(_) | Bson::Int64(_) => {
            if place >= 0 {
                return Ok(n);
            }
            let iv = as_int_like(&n).unwrap() as f64;
            let scale = 10f64.powi(-place);
            int_to_bson(((iv / scale).round_ties_even() * scale) as i128).ok_or(Fallback)
        }
        Bson::Double(d) => {
            let factor = 10f64.powi(place);
            Ok(Bson::Double((d * factor).round_ties_even() / factor))
        }
        _ => Err(Fallback), // Decimal128 / non-numeric -> Python
    }
}

// `$trunc`: truncate toward zero to `place` decimals. Python computes
// `math.trunc(n * 10**place) / 10**place`, and `/` is float division, so the
// result is always a double (even for an int input). Mirrors `_op_trunc`.
fn op_trunc(arg: &Bson, ctx: &Ctx) -> R {
    let (n, place) = round_trunc_args(arg, ctx)?;
    match n {
        Bson::Null => Ok(Bson::Null),
        _ => {
            let nf = math_float(&n)?; // bool / non-numeric -> Python (51081)
            let factor = 10f64.powi(place);
            Ok(Bson::Double((nf * factor).trunc() / factor))
        }
    }
}

// The sort key for one `$sortArray` element under a `sortBy` field: the field
// value for a document element (missing -> null), else the whole element. Mirrors
// `expressions._make_sort_key`.
fn sort_array_field_value(elem: &Bson, field: &str) -> Bson {
    match elem {
        Bson::Document(d) => paths::get_path(d, field).cloned().unwrap_or(Bson::Null),
        other => other.clone(),
    }
}

// `$sortArray`: {input, sortBy}. `sortBy` is `1`/`-1` (sort whole elements) or a
// `{field: dir, …}` document (sort by fields). Sorting uses BSON sort order
// (`order::cmp`, the same order as `$sort` / Python's `_SortKey`) and is stable.
// Defers when any sort value isn't totally orderable (bool / NaN / Decimal128 /
// …) — `order::cmp`'s precondition — matching the "defer to Python" engine
// contract. Mirrors `expressions._op_sort_array`.
fn op_sort_array(arg: &Bson, ctx: &Ctx) -> R {
    let spec = arg.as_document().ok_or(Fallback)?;
    let (Some(input), Some(sort_by)) = (spec.get("input"), spec.get("sortBy")) else {
        return Err(Fallback); // Python raises: requires {input, sortBy}
    };
    let mut out = match eval(input, ctx)? {
        Bson::Null => return Ok(Bson::Null),
        Bson::Array(a) => a,
        _ => return Err(Fallback), // Python raises: input must be an array
    };
    match sort_by {
        Bson::Int32(_) | Bson::Int64(_) => {
            if !out.iter().all(crate::order::is_sortable) {
                return Err(Fallback);
            }
            let desc = as_int_like(sort_by) == Some(-1);
            // `cmp(b, a)` for descending keeps equal elements in original order
            // (stable), matching Python's `sorted(reverse=True)`.
            out.sort_by(|a, b| {
                if desc {
                    crate::order::cmp(b, a)
                } else {
                    crate::order::cmp(a, b)
                }
            });
        }
        Bson::Document(fields) => {
            // bson's Document iterator isn't double-ended, so collect to reverse.
            let field_list: Vec<(&String, &Bson)> = fields.iter().collect();
            for (field, _) in &field_list {
                if !out
                    .iter()
                    .all(|e| crate::order::is_sortable(&sort_array_field_value(e, field)))
                {
                    return Err(Fallback);
                }
            }
            // Reversed multi-pass stable sort (sort by the last field first), so
            // earlier fields take precedence — mirrors the Python impl.
            for (field, direction) in field_list.iter().rev() {
                let desc = as_int_like(direction) == Some(-1);
                out.sort_by(|a, b| {
                    let o = crate::order::cmp(
                        &sort_array_field_value(a, field),
                        &sort_array_field_value(b, field),
                    );
                    if desc {
                        o.reverse()
                    } else {
                        o
                    }
                });
            }
        }
        _ => return Err(Fallback), // Python raises: sortBy must be int or document
    }
    Ok(Bson::Array(out))
}

// --- $dateToParts (timezone-aware; `iso8601: true` switches to ISO parts) ---

fn op_date_to_parts(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    match eval_opt(d.get("date"), ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::DateTime(dt) => {
            // A `timezone` shifts the instant into that zone before the parts are
            // read (instant→wall-clock, unambiguous); absent/UTC leaves it in UTC.
            let base = dt.timestamp_millis();
            let millis = base + timezone_offset_ms(d.get("timezone"), base)?;
            let days = millis.div_euclid(86_400_000);
            let ms = millis.rem_euclid(86_400_000);
            let (y, m, dy) = civil_from_days(days);
            let iso8601 = match d.get("iso8601") {
                None => false,
                Some(e) => match eval(e, ctx)? {
                    Bson::Boolean(b) => b,
                    _ => return Err(Fallback),
                },
            };
            let mut out = Document::new();
            if iso8601 {
                let (iso_year, iso_week) = iso_year_week(y, m, dy).ok_or(Fallback)?;
                let iso_dow = (days + 3).rem_euclid(7) + 1;
                out.insert("isoWeekYear".to_string(), Bson::Int32(iso_year as i32));
                out.insert("isoWeek".to_string(), Bson::Int32(iso_week as i32));
                out.insert("isoDayOfWeek".to_string(), Bson::Int32(iso_dow as i32));
            } else {
                out.insert("year".to_string(), Bson::Int32(y as i32));
                out.insert("month".to_string(), Bson::Int32(m as i32));
                out.insert("day".to_string(), Bson::Int32(dy as i32));
            }
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

/// A `$dateFromParts` component as an `i64`, or `None` (defer — Python raises
/// `Location40515`) for a non-integral / non-numeric value. Integral doubles are
/// accepted; bool is not.
fn dfp_int(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(x) => Some(*x as i64),
        Bson::Int64(x) => Some(*x),
        Bson::Double(x) if x.is_finite() && x.fract() == 0.0 => Some(*x as i64),
        _ => None,
    }
}

/// `$dateFromParts`: build a date from calendar components with mongod's rollover
/// (month 13 → next January, day 0 → last of previous month, …). Components default
/// to month/day = 1 and time = 0; any null component → null. `year` is required and
/// in 1-9999. A fixed-offset `timezone` interprets the components as local time
/// (local→instant, `utc = local - offset`). Everything Python raises on — a missing
/// / out-of-range `year`, a non-integral component, the ISO-week form — and a
/// *named* `timezone` (DST-ambiguous local→instant) defers to Python. Mirrors the
/// pure `_op_date_from_parts`.
/// Read a `$dateFromParts` component: absent → `default`; null → `Ok(None)` (the
/// caller returns null); non-integral → `Fallback` (Python raises 40515). Integral
/// doubles accepted.
fn dfp_comp(d: &Document, name: &str, default: i64, ctx: &Ctx) -> Result<Option<i64>, Fallback> {
    match d.get(name) {
        None => Ok(Some(default)),
        Some(e) => match eval(e, ctx)? {
            Bson::Null => Ok(None),
            v => Ok(Some(dfp_int(&v).ok_or(Fallback)?)),
        },
    }
}

/// Days since 1970-01-01 of the Monday of ISO week 1 of `iso_year`, via `chrono`.
fn iso_week1_monday_days(iso_year: i64) -> Option<i64> {
    use chrono::{Datelike, NaiveDate, Weekday};
    let d = NaiveDate::from_isoywd_opt(i32::try_from(iso_year).ok()?, 1, Weekday::Mon)?;
    Some(days_from_civil(
        d.year() as i64,
        d.month() as i64,
        d.day() as i64,
    ))
}

fn op_date_from_parts(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    // Shared time components (any null -> null).
    macro_rules! comp {
        ($name:literal, $default:literal) => {
            match dfp_comp(d, $name, $default, ctx)? {
                Some(v) => v,
                None => return Ok(Bson::Null),
            }
        };
    }
    let hour = comp!("hour", 0);
    let minute = comp!("minute", 0);
    let second = comp!("second", 0);
    let ms = comp!("millisecond", 0);
    let is_iso = d.contains_key("isoWeekYear")
        || d.contains_key("isoWeek")
        || d.contains_key("isoDayOfWeek");
    let total_days = if is_iso {
        if !d.contains_key("isoWeekYear") {
            return Err(Fallback); // Python raises 40516
        }
        let iso_year = comp!("isoWeekYear", 0);
        let iso_week = comp!("isoWeek", 1);
        let iso_dow = comp!("isoDayOfWeek", 1);
        if !(1..=9999).contains(&iso_year) {
            return Err(Fallback);
        }
        iso_week1_monday_days(iso_year).ok_or(Fallback)? + (iso_week - 1) * 7 + (iso_dow - 1)
    } else {
        if !d.contains_key("year") {
            return Err(Fallback); // Python raises 40516
        }
        let year = comp!("year", 0);
        let month = comp!("month", 1);
        let day = comp!("day", 1);
        if !(1..=9999).contains(&year) {
            return Err(Fallback); // Python raises 40523
        }
        let total_months = year * 12 + (month - 1);
        let base_year = total_months.div_euclid(12);
        let base_month = total_months.rem_euclid(12) + 1;
        if !(1..=9999).contains(&base_year) {
            return Err(Fallback); // rollover pushed the year out of range -> Python
        }
        days_from_civil(base_year, base_month, 1) + (day - 1)
    };
    let mut millis =
        total_days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1_000 + ms;
    match d.get("timezone") {
        None | Some(Bson::Null) => {}
        Some(Bson::String(s)) => match resolve_tz_offset(s) {
            Some(off_min) => millis -= off_min * 60_000, // local -> utc
            None => return Err(Fallback),                // named zone -> Python
        },
        Some(_) => return Err(Fallback),
    }
    Ok(Bson::DateTime(bson::DateTime::from_millis(millis)))
}

/// `$tsSecond` (seconds) / `$tsIncrement` (increment) of a BSON Timestamp, as a
/// long. Null / missing → null; a non-timestamp defers (Python raises 5687301/2).
fn op_ts_field(arg: &Bson, ctx: &Ctx, seconds: bool) -> R {
    match eval(arg, ctx)? {
        Bson::Null => Ok(Bson::Null),
        Bson::Timestamp(ts) => Ok(Bson::Int64(if seconds {
            ts.time as i64
        } else {
            ts.increment as i64
        })),
        _ => Err(Fallback),
    }
}

/// The BSON type string mongod's `$type` reports for a value.
fn type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Null => "null",
        Bson::Boolean(_) => "bool",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal",
        Bson::String(_) => "string",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Timestamp(_) => "timestamp",
        Bson::RegularExpression(_) => "regex",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Undefined => "undefined",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::DbPointer(_) => "dbPointer",
    }
}

/// `$type`: the BSON type string. A field path that doesn't resolve yields
/// `"missing"` (mongod distinguishes an absent field from an explicit null).
fn op_type(arg: &Bson, ctx: &Ctx) -> R {
    if let Bson::String(s) = arg {
        if let Some(path) = s.strip_prefix('$') {
            if !path.starts_with('$') && crate::paths::get_path(ctx.doc, path).is_none() {
                return Ok(Bson::String("missing".into()));
            }
        }
    }
    Ok(Bson::String(type_name(&eval(arg, ctx)?).into()))
}

/// `$isNumber`: true for int / long / double / decimal (not bool).
fn op_is_number(arg: &Bson, ctx: &Ctx) -> R {
    Ok(Bson::Boolean(matches!(
        eval(arg, ctx)?,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )))
}

/// `$isArray`: true iff the argument is an array.
fn op_is_array(arg: &Bson, ctx: &Ctx) -> R {
    Ok(Bson::Boolean(matches!(eval(arg, ctx)?, Bson::Array(_))))
}

/// `$strcasecmp`: case-insensitive compare of two strings → -1 / 0 / 1. A null
/// operand is the empty string; non-ASCII defers (case mapping may differ from
/// Python — same contract as `$toUpper`); a non-string operand defers.
fn op_strcasecmp(arg: &Bson, ctx: &Ctx) -> R {
    let vals = eval_args(arg, ctx)?;
    if vals.len() != 2 {
        return Err(Fallback);
    }
    let to_str = |v: &Bson| -> Result<String, Fallback> {
        match v {
            Bson::Null => Ok(String::new()),
            Bson::String(s) if s.is_ascii() => Ok(s.to_ascii_uppercase()),
            // mongod $toString-coerces an operand; an integer matches Python's
            // `str(int)`. Double / date / Decimal128 / bool defer (double string
            // formatting + bool -> Location16007 are Python's).
            Bson::Int32(n) => Ok(n.to_string()),
            Bson::Int64(n) => Ok(n.to_string()),
            _ => Err(Fallback),
        }
    };
    let (a, b) = (to_str(&vals[0])?, to_str(&vals[1])?);
    Ok(Bson::Int32(match a.cmp(&b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

/// `$replaceOne` / `$replaceAll`: replace occurrence(s) of `find` in `input` with
/// `replacement`. Any null input/find/replacement → null; a non-string one defers
/// (Python raises 51745). Mirrors the pure `_op_replace`.
fn op_replace(arg: &Bson, ctx: &Ctx, all: bool) -> R {
    let Bson::Document(d) = arg else {
        return Err(Fallback);
    };
    let (Some(ie), Some(fe), Some(re)) = (d.get("input"), d.get("find"), d.get("replacement"))
    else {
        return Err(Fallback);
    };
    let (iv, fv, rv) = (eval(ie, ctx)?, eval(fe, ctx)?, eval(re, ctx)?);
    if matches!(iv, Bson::Null) || matches!(fv, Bson::Null) || matches!(rv, Bson::Null) {
        return Ok(Bson::Null);
    }
    let (Bson::String(input), Bson::String(find), Bson::String(rep)) = (iv, fv, rv) else {
        return Err(Fallback); // non-string -> Python raises
    };
    let out = if all {
        input.replace(&find, &rep)
    } else {
        input.replacen(&find, &rep, 1)
    };
    Ok(Bson::String(out))
}

// --- $range -------------------------------------------------------------

const MAX_RANGE_SIZE: i128 = 100_000;

fn range_int(b: &Bson) -> Result<i64, Fallback> {
    // int or whole-number double is the value; bool / fractional double /
    // non-number all defer to the Python oracle (which raises 34443-34448).
    match coerce_index(b) {
        IdxCoerce::Int(i) => Ok(i),
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
    // start/end must be a non-negative int or whole double (mongod). A fractional
    // double / bool / non-numeric (40096) or a negative index (40097) defers so
    // Python raises the exact error.
    let bound = |idx: usize, default: i64, ctx: &Ctx| -> Result<i64, Fallback> {
        if a.len() <= idx {
            return Ok(default);
        }
        let n = match eval(&a[idx], ctx)? {
            Bson::Int32(n) => n as i64,
            Bson::Int64(n) => n,
            Bson::Double(d) if d.fract() == 0.0 => d as i64,
            _ => return Err(Fallback), // fractional / bool / non-numeric -> 40096
        };
        if n < 0 {
            return Err(Fallback); // negative -> 40097
        }
        Ok(n)
    };
    let (start, end) = (bound(2, 0, ctx)?, bound(3, len, ctx)?);
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
    let start_v = eval(&a[1], ctx)?;
    let length_v = eval(&a[2], ctx)?;
    if matches!(start_v, Bson::Boolean(_)) || matches!(length_v, Bson::Boolean(_)) {
        return Err(Fallback); // Python raises 16034 / 16035
    }
    // $substrBytes truncates a double toward zero (not reject-fractional).
    let (Some(start), Some(length)) = (trunc_index(&start_v), trunc_index(&length_v)) else {
        return Err(Fallback);
    };
    let bytes = s.as_bytes();
    let blen = bytes.len() as i64;
    // mongod rejects a negative start (Python raises 50752); a negative length
    // is fine (means "to the end").
    if start < 0 {
        return Err(Fallback);
    }
    // mongod rejects a byte range whose start is a UTF-8 continuation byte, or
    // whose end splits a character -- even for an empty (length 0) range, which
    // the from_utf8 check below would miss. Python raises 28656 / 28657 here;
    // the core defers.
    if start < blen && (bytes[start as usize] & 0xC0) == 0x80 {
        return Err(Fallback); // Python raises 28656
    }
    let end = if length < 0 {
        blen
    } else {
        start.saturating_add(length)
    };
    if (0..blen).contains(&end) && (bytes[end as usize] & 0xC0) == 0x80 {
        return Err(Fallback); // Python raises 28657
    }
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
    if is_null(&chars) {
        return Ok(Bson::Null); // chars: null -> null result (mongod)
    }
    let Bson::String(chars) = chars else {
        return Err(Fallback); // non-string chars -> Python raises 50700
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
    // Symbol / JS-Code (with or without scope) compare by value — used by the
    // oplog update-diff to detect a changed Code/Symbol field, and by `$eq`.
    // Cross-type and other exotic values (DbPointer / Undefined) still defer.
    match (a, b) {
        (Bson::Symbol(x), Bson::Symbol(y)) => return Ok(x == y),
        (Bson::JavaScriptCode(x), Bson::JavaScriptCode(y)) => return Ok(x == y),
        (Bson::JavaScriptCodeWithScope(x), Bson::JavaScriptCodeWithScope(y)) => {
            return Ok(x.code == y.code && x.scope == y.scope)
        }
        _ => {}
    }
    if matches!(a, Bson::Decimal128(_))
        || matches!(b, Bson::Decimal128(_))
        || is_exotic(a)
        || is_exotic(b)
    {
        return Err(Fallback);
    }
    if let Some(r) = numeric::fast_cmp_numberish(a, b) {
        return Ok(r == Some(std::cmp::Ordering::Equal));
    }
    if numeric::is_numberish(a) != numeric::is_numberish(b) {
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
    fn unknown_expr_operator_detected() {
        assert_eq!(
            first_unknown_expr_operator(&bson::bson!({"$notreal": [1, 2]})),
            Some("$notreal".to_string())
        );
        // Nested inside a recognised operator's argument.
        assert_eq!(
            first_unknown_expr_operator(&bson::bson!({"$add": [1, {"$bogus": 2}]})),
            Some("$bogus".to_string())
        );
        // A field-path / literal / recognised operator is not "unknown".
        assert_eq!(
            first_unknown_expr_operator(&Bson::String("$a".into())),
            None
        );
        assert_eq!(
            first_unknown_expr_operator(&bson::bson!({"$add": [1, 2]})),
            None
        );
    }

    #[test]
    fn known_expr_ops_all_route() {
        // Every name in KNOWN_EXPR_OPS must dispatch in `apply_op` — i.e. calling
        // it must NOT hit the `_ => Err(Fallback)` unknown-operator arm. We can't
        // observe the arm directly, so we assert `first_unknown_expr_operator`
        // (which shares the list) agrees the op is recognised, and cross-check
        // that a made-up name is flagged. This guards the list against drift.
        for op in KNOWN_EXPR_OPS {
            let expr = Bson::Document(doc! { *op: Bson::Array(vec![]) });
            assert_eq!(
                first_unknown_expr_operator(&expr),
                None,
                "{op} should be recognised"
            );
        }
        assert_eq!(
            first_unknown_expr_operator(&bson::bson!({"$definitelyNotAnOp": 1})),
            Some("$definitelyNotAnOp".to_string())
        );
    }

    #[test]
    fn rand_returns_double_in_unit_interval() {
        for _ in 0..256 {
            match evaluate(&doc! {}, &bson::bson!({"$rand": {}}), &Document::new()).unwrap() {
                Bson::Double(v) => assert!((0.0..1.0).contains(&v), "out of range: {v}"),
                other => panic!("$rand returned non-double: {other:?}"),
            }
        }
        // Non-empty / wrong-typed argument defers (Python raises a parse error).
        assert!(evaluate(
            &doc! {},
            &bson::bson!({"$rand": {"x": 1}}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(&doc! {}, &bson::bson!({"$rand": 5}), &Document::new()).is_err());
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
    fn new_expression_ops() {
        let ts = doc! {"t": Bson::Timestamp(bson::Timestamp { time: 1700000000, increment: 7 })};
        assert_eq!(
            ev(ts.clone(), bson::bson!({"$tsSecond": "$t"})),
            Bson::Int64(1700000000)
        );
        assert_eq!(
            ev(ts.clone(), bson::bson!({"$tsIncrement": "$t"})),
            Bson::Int64(7)
        );
        assert_eq!(ev(ts, bson::bson!({"$tsSecond": "$x"})), Bson::Null); // missing -> null
                                                                          // $type incl. missing.
        assert_eq!(
            ev(doc! {"a": 5i32}, bson::bson!({"$type": "$a"})),
            Bson::String("int".into())
        );
        assert_eq!(
            ev(doc! {"a": [1]}, bson::bson!({"$type": "$a"})),
            Bson::String("array".into())
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$type": "$zzz"})),
            Bson::String("missing".into())
        );
        // $isNumber / $isArray.
        assert_eq!(
            ev(doc! {"a": 5i32}, bson::bson!({"$isNumber": "$a"})),
            Bson::Boolean(true)
        );
        assert_eq!(
            ev(doc! {"a": true}, bson::bson!({"$isNumber": "$a"})),
            Bson::Boolean(false)
        );
        assert_eq!(
            ev(doc! {"a": [1]}, bson::bson!({"$isArray": "$a"})),
            Bson::Boolean(true)
        );
        // $strcasecmp (null -> "").
        assert_eq!(
            ev(doc! {}, bson::bson!({"$strcasecmp": ["abc", "ABC"]})),
            Bson::Int32(0)
        );
        assert_eq!(
            ev(
                doc! {"n": Bson::Null},
                bson::bson!({"$strcasecmp": ["$n", "a"]})
            ),
            Bson::Int32(-1)
        );
        // $replaceOne / $replaceAll.
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$replaceOne": {"input": "abcabc", "find": "bc", "replacement": "X"}})
            ),
            Bson::String("aXabc".into())
        );
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$replaceAll": {"input": "abcabc", "find": "bc", "replacement": "X"}})
            ),
            Bson::String("aXaX".into())
        );
        // ISO-week $dateFromParts.
        let iso = ev(
            doc! {},
            bson::bson!({"$dateFromParts": {"isoWeekYear": 2023, "isoWeek": 5, "isoDayOfWeek": 3}}),
        );
        assert_eq!(
            iso.as_datetime().unwrap().timestamp_millis(),
            1_675_209_600_000
        ); // 2023-02-01
           // Error paths defer.
        assert!(evaluate(
            &doc! {"a": 5i32},
            &bson::bson!({"$tsSecond": "$a"}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &doc! {},
            &bson::bson!({"$replaceOne": {"input": "a", "find": 5, "replacement": "b"}}),
            &Document::new()
        )
        .is_err());
    }

    #[test]
    fn set_operators() {
        let arr = |xs: &[i32]| Bson::Array(xs.iter().map(|x| Bson::Int32(*x)).collect());
        // setUnion / setIntersection sorted; setDifference first-array order.
        assert_eq!(
            ev(doc! {}, bson::bson!({"$setUnion": [[3, 1, 2], [5, 4]]})),
            arr(&[1, 2, 3, 4, 5])
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$setUnion": [[3, 3, 1], [1, 2]]})),
            arr(&[1, 2, 3])
        );
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$setIntersection": [[3, 1, 2, 5], [2, 5, 1]]})
            ),
            arr(&[1, 2, 5])
        );
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$setDifference": [[5, 3, 1, 2], [3]]})
            ),
            arr(&[5, 1, 2])
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$setEquals": [[1, 2], [2, 1]]})),
            Bson::Boolean(true)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$setIsSubset": [[1, 2], [1, 2, 3]]})),
            Bson::Boolean(true)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$allElementsTrue": [[1, true]]})),
            Bson::Boolean(true)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$allElementsTrue": [[1, 0]]})),
            Bson::Boolean(false)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$anyElementTrue": [[0, false, 1]]})),
            Bson::Boolean(true)
        );
        // $cmp / sizes / degrees.
        assert_eq!(ev(doc! {}, bson::bson!({"$cmp": [1, 2]})), Bson::Int32(-1));
        assert_eq!(ev(doc! {}, bson::bson!({"$cmp": [5, 5]})), Bson::Int32(0));
        assert_eq!(
            ev(doc! {}, bson::bson!({"$binarySize": "héllo"})),
            Bson::Int32(6)
        );
        assert_eq!(
            ev(doc! {"a": 5i32}, bson::bson!({"$bsonSize": "$$ROOT"})),
            Bson::Int32(12)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$degreesToRadians": 180})),
            Bson::Double(std::f64::consts::PI)
        );
        // A non-array set arg / cross-type-sortless element defers.
        assert!(evaluate(
            &doc! {},
            &bson::bson!({"$setUnion": [[1], 5]}),
            &Document::new()
        )
        .is_err());
    }

    #[test]
    fn trig() {
        assert_eq!(ev(doc! {}, bson::bson!({"$sin": 0})), Bson::Double(0.0));
        assert_eq!(ev(doc! {}, bson::bson!({"$cos": 0})), Bson::Double(1.0_f64));
        assert_eq!(
            ev(doc! {}, bson::bson!({"$asin": 1})),
            Bson::Double(std::f64::consts::FRAC_PI_2)
        );
        assert_eq!(
            ev(doc! {}, bson::bson!({"$atan2": [1, 1]})),
            Bson::Double(std::f64::consts::FRAC_PI_4)
        );
        // atanh(±1) -> ±inf (not a domain error).
        assert_eq!(
            ev(doc! {}, bson::bson!({"$atanh": 1})),
            Bson::Double(f64::INFINITY)
        );
        // null -> null.
        assert_eq!(ev(doc! {}, bson::bson!({"$sin": Bson::Null})), Bson::Null);
        // Domain / type violations defer to Python.
        for expr in [
            bson::bson!({"$asin": 5}),            // out of [-1,1]
            bson::bson!({"$acosh": 0.5}),         // < 1
            bson::bson!({"$sin": f64::INFINITY}), // non-finite
            bson::bson!({"$cos": "hi"}),          // non-numeric
            bson::bson!({"$atan2": ["hi", 1]}),   // atan2 non-numeric
        ] {
            assert!(evaluate(&doc! {}, &expr, &Document::new()).is_err());
        }
    }

    #[test]
    fn date_from_parts() {
        let ms = |v: &Bson| v.as_datetime().unwrap().timestamp_millis();
        // 2023-06-15T00:00Z
        let basic = ev(
            doc! {},
            bson::bson!({"$dateFromParts": {"year": 2023, "month": 6, "day": 15}}),
        );
        assert_eq!(ms(&basic), 1_686_787_200_000);
        // month 13 rolls to 2024-01-01.
        let roll = ev(
            doc! {},
            bson::bson!({"$dateFromParts": {"year": 2023, "month": 13, "day": 1}}),
        );
        assert_eq!(ms(&roll), 1_704_067_200_000);
        // day 0 -> last of previous month (2023-05-31); ms 1500 -> +1.5s.
        let d0 = ev(
            doc! {},
            bson::bson!({"$dateFromParts": {"year": 2023, "month": 6, "day": 0}}),
        );
        assert_eq!(ms(&d0), 1_685_491_200_000);
        // integral double accepted; defaults month/day=1.
        let dflt = ev(doc! {}, bson::bson!({"$dateFromParts": {"year": 2023.0}}));
        assert_eq!(ms(&dflt), 1_672_531_200_000); // 2023-01-01
                                                  // fixed-offset timezone: local 12:00 +05:00 -> 07:00 UTC.
        let tz = ev(
            doc! {},
            bson::bson!({"$dateFromParts": {"year": 2023, "month": 6, "day": 15, "hour": 12, "timezone": "+05:00"}}),
        );
        assert_eq!(ms(&tz), 1_686_787_200_000 + 7 * 3_600_000);
        // null component -> null.
        assert_eq!(
            ev(
                doc! {},
                bson::bson!({"$dateFromParts": {"year": Bson::Null}})
            ),
            Bson::Null
        );
        // Non-integral, missing year, out-of-range year, named tz -> defer.
        for bad in [
            bson::bson!({"$dateFromParts": {"year": 2023, "month": 6.5}}),
            bson::bson!({"$dateFromParts": {"month": 6}}),
            bson::bson!({"$dateFromParts": {"year": 10000}}),
            bson::bson!({"$dateFromParts": {"year": 2023, "timezone": "America/New_York"}}),
        ] {
            assert!(evaluate(&doc! {}, &bad, &Document::new()).is_err());
        }
    }

    #[test]
    fn date_to_string_named_timezone() {
        // Named IANA zone: DST-correct, instant→wall-clock (matches Python
        // zoneinfo). 2023-07-15T16:30Z is EDT (-04:00) → 12:30; 2023-01-15T16:30Z
        // is EST (-05:00) → 11:30. Same UTC hour-of-day, one hour apart locally.
        let summer = bson::DateTime::from_millis(1_689_438_600_000);
        let winter = bson::DateTime::from_millis(1_673_800_200_000);
        assert_eq!(
            ev(
                doc! {"d": summer},
                bson::bson!({"$dateToString": {"date": "$d", "format": "%Y-%m-%d %H:%M", "timezone": "America/New_York"}})
            ),
            Bson::String("2023-07-15 12:30".into())
        );
        assert_eq!(
            ev(
                doc! {"d": winter},
                bson::bson!({"$dateToString": {"date": "$d", "format": "%Y-%m-%d %H:%M", "timezone": "America/New_York"}})
            ),
            Bson::String("2023-01-15 11:30".into())
        );
        // Europe/Dublin in summer is IST (+01:00) → 17:30.
        assert_eq!(
            ev(
                doc! {"d": summer},
                bson::bson!({"$dateToString": {"date": "$d", "format": "%H:%M", "timezone": "Europe/Dublin"}})
            ),
            Bson::String("17:30".into())
        );
    }

    #[test]
    fn date_to_parts_timezone() {
        // 2023-01-15T16:30:45Z is EST (-05:00) in New York → local hour 11, still
        // the 15th. Bare (no tz) reads UTC hour 16. Fixed offset shifts too.
        let winter = bson::DateTime::from_millis(1_673_800_245_000);
        let d = doc! {"d": winter};
        let utc = ev(d.clone(), bson::bson!({"$dateToParts": {"date": "$d"}}));
        assert_eq!(utc.as_document().unwrap().get_i32("hour").unwrap(), 16);
        let ny = ev(
            d.clone(),
            bson::bson!({"$dateToParts": {"date": "$d", "timezone": "America/New_York"}}),
        );
        let nyd = ny.as_document().unwrap();
        assert_eq!(nyd.get_i32("hour").unwrap(), 11);
        assert_eq!(nyd.get_i32("day").unwrap(), 15);
        assert_eq!(nyd.get_i32("second").unwrap(), 45);
        let off = ev(
            d,
            bson::bson!({"$dateToParts": {"date": "$d", "timezone": "+05:30"}}),
        );
        assert_eq!(off.as_document().unwrap().get_i32("hour").unwrap(), 22);
    }

    #[test]
    fn date_extractors_timezone_object_form() {
        // 2023-01-15T16:30Z is EST (-05:00) in New York → local hour 11, still the
        // 15th. A bare date reads UTC (hour 16). Fixed-offset and named zones agree.
        let winter = bson::DateTime::from_millis(1_673_800_200_000);
        let d = doc! {"d": winter};
        assert_eq!(ev(d.clone(), bson::bson!({"$hour": "$d"})), Bson::Int32(16));
        assert_eq!(
            ev(
                d.clone(),
                bson::bson!({"$hour": {"date": "$d", "timezone": "America/New_York"}})
            ),
            Bson::Int32(11)
        );
        assert_eq!(
            ev(
                d.clone(),
                bson::bson!({"$dayOfMonth": {"date": "$d", "timezone": "America/New_York"}})
            ),
            Bson::Int32(15)
        );
        assert_eq!(
            ev(
                d.clone(),
                bson::bson!({"$hour": {"date": "$d", "timezone": "-05:00"}})
            ),
            Bson::Int32(11)
        );
        // No timezone in the object form reads UTC.
        assert_eq!(
            ev(d.clone(), bson::bson!({"$hour": {"date": "$d"}})),
            Bson::Int32(16)
        );
        // Summer: New York is EDT (-04:00) → hour 12 from 16:30Z.
        let summer = bson::DateTime::from_millis(1_689_438_600_000);
        assert_eq!(
            ev(
                doc! {"d": summer},
                bson::bson!({"$hour": {"date": "$d", "timezone": "America/New_York"}})
            ),
            Bson::Int32(12)
        );
    }

    #[test]
    fn unsupported_falls_back() {
        // An *unknown* zone name still defers (Python resolves it or raises), as do
        // non-ASCII $toUpper and string $add.
        let d = bson::DateTime::from_millis(1_689_438_600_000);
        assert!(evaluate(
            &doc! {"d": d},
            &bson::bson!({"$dateToString": {"date": "$d", "timezone": "Not/AZone"}}),
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
    fn max_n_min_n() {
        let d = doc! {"a": [3i32, 1i32, 4i32, 1i32, 5i32, 9i32, 2i32, 6i32]};
        assert_eq!(
            ev(d.clone(), bson::bson!({"$maxN": {"n": 3, "input": "$a"}})),
            Bson::Array(vec![Bson::Int32(9), Bson::Int32(6), Bson::Int32(5)])
        );
        assert_eq!(
            ev(d.clone(), bson::bson!({"$minN": {"n": 3, "input": "$a"}})),
            Bson::Array(vec![Bson::Int32(1), Bson::Int32(1), Bson::Int32(2)])
        );
        // Null elements are ignored.
        let dn =
            doc! {"a": [Bson::Int32(3), Bson::Null, Bson::Int32(1), Bson::Null, Bson::Int32(5)]};
        assert_eq!(
            ev(dn.clone(), bson::bson!({"$maxN": {"n": 2, "input": "$a"}})),
            Bson::Array(vec![Bson::Int32(5), Bson::Int32(3)])
        );
        assert_eq!(
            ev(dn, bson::bson!({"$minN": {"n": 2, "input": "$a"}})),
            Bson::Array(vec![Bson::Int32(1), Bson::Int32(3)])
        );
        // null / missing / non-array input, invalid n, bool element -> error (defer).
        assert!(evaluate(
            &d,
            &bson::bson!({"$maxN": {"n": 2, "input": "$missing"}}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &d,
            &bson::bson!({"$maxN": {"n": 2, "input": Bson::Null}}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &d,
            &bson::bson!({"$maxN": {"n": 0, "input": "$a"}}),
            &Document::new()
        )
        .is_err());
        let db = doc! {"a": [Bson::Boolean(true), Bson::Int32(1)]};
        assert!(evaluate(
            &db,
            &bson::bson!({"$maxN": {"n": 1, "input": "$a"}}),
            &Document::new()
        )
        .is_err());
    }

    #[test]
    fn first_n_last_n() {
        let d = doc! {"a": [10i32, 20i32, 30i32, 40i32, 50i32]};
        assert_eq!(
            ev(d.clone(), bson::bson!({"$firstN": {"n": 2, "input": "$a"}})),
            Bson::Array(vec![Bson::Int32(10), Bson::Int32(20)])
        );
        assert_eq!(
            ev(d.clone(), bson::bson!({"$lastN": {"n": 2, "input": "$a"}})),
            Bson::Array(vec![Bson::Int32(40), Bson::Int32(50)])
        );
        // n larger than the array -> whole array.
        assert_eq!(
            ev(
                d.clone(),
                bson::bson!({"$firstN": {"n": 10, "input": "$a"}})
            ),
            Bson::Array(vec![
                Bson::Int32(10),
                Bson::Int32(20),
                Bson::Int32(30),
                Bson::Int32(40),
                Bson::Int32(50)
            ])
        );
        // An integral double n is accepted (mongod does).
        assert_eq!(
            ev(
                d.clone(),
                bson::bson!({"$firstN": {"n": 2.0, "input": "$a"}})
            ),
            Bson::Array(vec![Bson::Int32(10), Bson::Int32(20)])
        );
        // null / missing / non-array input, invalid n -> error (defer; mongod raises).
        assert!(evaluate(
            &d,
            &bson::bson!({"$firstN": {"n": 2, "input": "$missing"}}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &d,
            &bson::bson!({"$firstN": {"n": 2, "input": Bson::Null}}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &d,
            &bson::bson!({"$firstN": {"n": 0, "input": "$a"}}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &d,
            &bson::bson!({"$lastN": {"n": 1.5, "input": "$a"}}),
            &Document::new()
        )
        .is_err());
        assert!(evaluate(
            &d,
            &bson::bson!({"$firstN": {"n": 2, "input": 5}}),
            &Document::new()
        )
        .is_err());
    }

    #[test]
    fn bitwise_ops() {
        let d = doc! {"a": 12i32, "b": 10i32, "big": 65280i64, "neg": -5i32};
        assert_eq!(
            ev(d.clone(), bson::bson!({"$bitAnd": ["$a", "$b"]})),
            Bson::Int32(8)
        );
        assert_eq!(
            ev(d.clone(), bson::bson!({"$bitOr": ["$a", "$b", 1]})),
            Bson::Int32(15)
        );
        assert_eq!(
            ev(d.clone(), bson::bson!({"$bitXor": ["$a", "$b"]})),
            Bson::Int32(6)
        );
        assert_eq!(
            ev(d.clone(), bson::bson!({"$bitNot": "$a"})),
            Bson::Int32(-13)
        );
        assert_eq!(
            ev(d.clone(), bson::bson!({"$bitNot": "$neg"})),
            Bson::Int32(4)
        );
        // Any long operand -> long result.
        assert_eq!(
            ev(d.clone(), bson::bson!({"$bitAnd": ["$big", 255i32]})),
            Bson::Int64(0)
        );
        // Empty list -> identity (all-ones for and, 0 for or/xor); null -> null.
        assert_eq!(ev(d.clone(), bson::bson!({"$bitAnd": []})), Bson::Int32(-1));
        assert_eq!(ev(d.clone(), bson::bson!({"$bitOr": []})), Bson::Int32(0));
        assert_eq!(
            ev(d.clone(), bson::bson!({"$bitAnd": ["$a", "$missing"]})),
            Bson::Null
        );
        // Non-integer operand defers (Python raises the type error).
        assert!(evaluate(&d, &bson::bson!({"$bitAnd": ["$a", 1.5]}), &Document::new()).is_err());
        assert!(evaluate(&d, &bson::bson!({"$bitNot": true}), &Document::new()).is_err());
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
