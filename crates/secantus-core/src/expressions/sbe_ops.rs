//! Operators as mongod's slot-based engine answers them (RM-280).
//!
//! SBE compiles an expression into checks of its own, in its own order, with
//! its own error codes, so the same operator can fail with another code, or
//! return a value where the classic engine fails, or the reverse. Each
//! function here follows `gen_expression.cpp` (and the VM builtins it calls)
//! of mongod 8.2.11: the checks are made in SBE's order, with SBE's codes and
//! messages, and where SBE computes the same value as the classic engine the
//! classic operator computes it from the already-evaluated arguments.
//!
//! An SBE function that yields `Nothing` makes the operator's result missing,
//! which this evaluator spells `Bson::Undefined`.
//!
//! `apply` returns `None` for an operator whose SBE answer is the classic
//! one, or which SBE does not compile (a stage holding one is not pushed
//! down, so it never gets here).

use super::{apply_classic, eval, eval_field_value, Ctx, R};
use crate::fallback::Fallback;
use bson::{doc, Bson, Document};
use std::cmp::Ordering;

pub(super) fn apply(op: &str, arg: &Bson, ctx: &Ctx) -> Option<R> {
    Some(match op {
        "$abs" => unary_numeric(op, arg, ctx, 7157700, "$abs only supports numeric types")
            .and_then(|v| match v {
                Some(Bson::Int64(i64::MIN)) => {
                    Err(fail(7157701, "can't take $abs of long long min"))
                }
                Some(v) => classic(op, vec![v], ctx),
                None => Ok(Bson::Null),
            }),
        "$ceil" => unary_math(op, arg, ctx, 7157702, "$ceil only supports numeric types"),
        "$floor" => unary_math(op, arg, ctx, 7157703, "$floor only supports numeric types"),
        "$exp" => unary_math(op, arg, ctx, 7157704, "$exp only supports numeric types"),
        "$sqrt" => sqrt(arg, ctx),
        "$ln" => log(
            op,
            arg,
            ctx,
            (7157705, "$ln only supports numeric types"),
            (7157706, "$ln's argument must be a positive number"),
        ),
        "$log10" => log(
            op,
            arg,
            ctx,
            (7157707, "$log10 only supports numeric types"),
            (7157708, "$log10's argument must be a positive number"),
        ),
        "$mod" => modulo(arg, ctx),
        "$divide" => divide(arg, ctx),
        "$pow" => pow(arg, ctx),
        "$round" | "$trunc" => round_trunc(op, arg, ctx),
        "$sin" | "$cos" | "$tan" => trig_bounded(op, arg, ctx, "(-inf", "inf)", None),
        "$asin" | "$acos" | "$atanh" => trig_bounded(op, arg, ctx, "[-1", "1]", Some((-1.0, 1.0))),
        "$acosh" => trig_bounded(op, arg, ctx, "[1", "inf]", Some((1.0, f64::INFINITY))),
        "$atan" | "$asinh" | "$cosh" | "$sinh" | "$tanh" | "$degreesToRadians"
        | "$radiansToDegrees" => trig(op, arg, ctx),
        "$atan2" => atan2(arg, ctx),
        "$substrBytes" | "$substr" => substr_bytes(arg, ctx),
        "$substrCP" => substr_cp(arg, ctx),
        "$toUpper" | "$toLower" => {
            let name = if op == "$toUpper" {
                "toUpper"
            } else {
                "toLower"
            };
            single(arg, ctx)
                .and_then(|v| case_convert(name, v))
                .map(|s| {
                    Bson::String(if op == "$toUpper" {
                        s.to_ascii_uppercase()
                    } else {
                        s.to_ascii_lowercase()
                    })
                })
        }
        "$strcasecmp" => strcasecmp(arg, ctx),
        "$indexOfBytes" | "$indexOfCP" => index_of(op, arg, ctx),
        "$concat" => concat(arg, ctx),
        "$strLenBytes" | "$strLenCP" => {
            let (code, what) = if op == "$strLenBytes" {
                (5155800, "$strLenBytes")
            } else {
                (5155900, "$strLenCP")
            };
            single(arg, ctx).and_then(|v| match v {
                Bson::String(_) => classic(op, vec![v], ctx),
                _ => Err(fail(code, format!("{what} requires a string argument"))),
            })
        }
        "$regexMatch" | "$regexFind" | "$regexFindAll" => regex(op, arg, ctx),
        "$arrayToObject" => array_to_object(arg, ctx),
        "$objectToArray" => single(arg, ctx).and_then(|v| match v {
            Bson::Document(_) => classic(op, vec![v], ctx),
            v if nullish(&v) => Ok(Bson::Null),
            _ => Err(fail(5153215, "$objectToArray requires an object input")),
        }),
        "$bsonSize" => single(arg, ctx).and_then(|v| match v {
            v if nullish(&v) => Ok(Bson::Null),
            Bson::Document(_) => classic(op, vec![v], ctx),
            _ => Err(fail(7158301, "$bsonSize requires a document input")),
        }),
        "$anyElementTrue" => single(arg, ctx).and_then(|v| match v {
            Bson::Array(a) => Ok(Bson::Boolean(a.iter().any(super::truthy))),
            _ => Err(fail(7158300, "$anyElementTrue's argument must be an array")),
        }),
        "$concatArrays" => concat_arrays(arg, ctx),
        "$sortArray" => sort_array(arg, ctx),
        "$setUnion" | "$setIntersection" | "$setDifference" | "$setEquals" | "$setIsSubset" => {
            set_op(op, arg, ctx)
        }
        "$in" => two(arg, ctx).and_then(|(v, arr)| match arr {
            Bson::Array(_) => classic("$in", vec![v, arr], ctx),
            _ => Err(fail(5153700, "$in requires an array as a second argument")),
        }),
        "$trim" | "$ltrim" | "$rtrim" => trim(op, arg, ctx),
        "$tsSecond" | "$tsIncrement" => single(arg, ctx).and_then(|v| match v {
            v if nullish(&v) => Ok(Bson::Null),
            Bson::Timestamp(_) => classic(op, vec![v], ctx),
            _ => Err(fail(
                if op == "$tsSecond" { 7157900 } else { 7157901 },
                format!("{op} expects argument of type timestamp"),
            )),
        }),
        "$reverseArray" => single(arg, ctx).and_then(|v| match v {
            v if nullish(&v) => Ok(Bson::Null),
            Bson::Array(_) => classic("$reverseArray", vec![v], ctx),
            _ => Err(fail(7158002, "$reverseArray argument must be an array")),
        }),
        "$year" | "$month" | "$dayOfMonth" | "$hour" | "$minute" | "$second" | "$millisecond"
        | "$dayOfWeek" | "$dayOfYear" | "$week" | "$isoWeek" | "$isoWeekYear" | "$isoDayOfWeek" => {
            return date_part(op, arg, ctx)
        }
        "$dateAdd" | "$dateSubtract" => date_add(op, arg, ctx),
        "$dateDiff" => date_diff(arg, ctx),
        "$dateToParts" => date_to_parts(arg, ctx),
        "$dateToString" => date_to_string(arg, ctx),
        "$dateFromString" => date_from_string(arg, ctx),
        "$dateFromParts" => date_from_parts(arg, ctx),
        "$dateTrunc" => date_trunc(arg, ctx),
        "$subtract" => subtract(arg, ctx),
        "$add" => add(arg, ctx),
        "$multiply" => multiply(arg, ctx),
        "$range" => range(arg, ctx),
        "$replaceOne" => replace_one(arg, ctx),
        _ => return None,
    })
}

fn fail(code: i32, message: impl Into<String>) -> Fallback {
    Fallback::mongo(code, message)
}

/// Null, missing or BSON undefined: `generateNullMissingOrUndefined`.
fn nullish(v: &Bson) -> bool {
    matches!(v, Bson::Null | Bson::Undefined)
}

fn is_number(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

fn is_nan(v: &Bson) -> bool {
    match v {
        Bson::Double(f) => f.is_nan(),
        Bson::Decimal128(_) => {
            matches!(crate::decimal::from_bson(v), Some(crate::decimal::Dec::Nan))
        }
        _ => false,
    }
}

/// How a number compares with `bound`, as SBE compares values: NaN below
/// every other number.
fn cmp_num(v: &Bson, bound: f64) -> Ordering {
    crate::order::cmp(v, &Bson::Double(bound))
}

/// `generateNegativeCheck`: below zero, and not NaN.
fn negative(v: &Bson) -> bool {
    !is_nan(v) && cmp_num(v, 0.0) == Ordering::Less
}

/// SBE's `convert` to an integer type: the same number, exactly, or nothing.
fn exact_int(v: &Bson, bits32: bool) -> Option<i64> {
    let n = match v {
        Bson::Int32(n) => i64::from(*n),
        Bson::Int64(n) => *n,
        Bson::Double(f) => {
            if !f.is_finite()
                || f.trunc() != *f
                || *f < -9.223_372_036_854_776e18
                || *f >= 9.223_372_036_854_776e18
            {
                return None;
            }
            *f as i64
        }
        Bson::Decimal128(_) => crate::decimal::decimal_to_i64_exact(v)?,
        _ => return None,
    };
    if bits32 && i32::try_from(n).is_err() {
        return None;
    }
    Some(n)
}

/// The operands of an operator that takes a list (or one bare operand).
///
/// A missing operand stays missing (`Bson::Undefined`): SBE tells it from
/// null, and a check that does not test for either first answers nothing for
/// it.
fn operands(arg: &Bson, ctx: &Ctx) -> Result<Vec<Bson>, Fallback> {
    match arg {
        Bson::Array(items) => items.iter().map(|e| eval_field_value(e, ctx)).collect(),
        other => Ok(vec![eval_field_value(other, ctx)?]),
    }
}

fn single(arg: &Bson, ctx: &Ctx) -> R {
    eval_field_value(arg, ctx)
}

/// An error of the classic operator over already-evaluated arguments: the
/// arguments were literals there, so whether the expression was folded is
/// left for the caller's own expression to decide.
fn unfolded(f: Fallback) -> Fallback {
    match f {
        Fallback::Mongo {
            code,
            message,
            exec,
            bare,
            ..
        } => Fallback::Mongo {
            code,
            message,
            folded: None,
            exec,
            bare,
        },
        other => other,
    }
}

/// The classic operator over arguments already evaluated.
fn classic(op: &str, args: Vec<Bson>, ctx: &Ctx) -> R {
    let lit = |v: Bson| {
        let v = if matches!(v, Bson::Undefined) {
            Bson::Null
        } else {
            v
        };
        Bson::Document(doc! { "$literal": v })
    };
    let arg = if args.len() == 1 && super::fixed_arity(op) == Some(1) {
        lit(args.into_iter().next().unwrap())
    } else {
        Bson::Array(args.into_iter().map(lit).collect())
    };
    apply_classic(op, &arg, ctx).map_err(unfolded)
}

fn classic_named(op: &str, args: Document, ctx: &Ctx) -> R {
    let lit: Document = args
        .into_iter()
        .map(|(k, v)| (k, Bson::Document(doc! { "$literal": v })))
        .collect();
    apply_classic(op, &Bson::Document(lit), ctx).map_err(unfolded)
}

/// The null, then numeric, checks of a one-operand numeric operator; the
/// operand when it is a number, `None` when it is null.
fn unary_numeric(
    _op: &str,
    arg: &Bson,
    ctx: &Ctx,
    code: i32,
    msg: &str,
) -> Result<Option<Bson>, Fallback> {
    let v = single(arg, ctx)?;
    if nullish(&v) {
        return Ok(None);
    }
    if !is_number(&v) {
        return Err(fail(code, msg));
    }
    Ok(Some(v))
}

fn unary_math(op: &str, arg: &Bson, ctx: &Ctx, code: i32, msg: &str) -> R {
    match unary_numeric(op, arg, ctx, code, msg)? {
        Some(v) => classic(op, vec![v], ctx),
        None => Ok(Bson::Null),
    }
}

fn sqrt(arg: &Bson, ctx: &Ctx) -> R {
    let Some(v) = unary_numeric(
        "$sqrt",
        arg,
        ctx,
        7157709,
        "$sqrt only supports numeric types",
    )?
    else {
        return Ok(Bson::Null);
    };
    if negative(&v) {
        return Err(fail(
            7157710,
            "$sqrt's argument must be greater than or equal to 0",
        ));
    }
    classic("$sqrt", vec![v], ctx)
}

fn log(op: &str, arg: &Bson, ctx: &Ctx, type_err: (i32, &str), domain: (i32, &str)) -> R {
    let Some(v) = unary_numeric(op, arg, ctx, type_err.0, type_err.1)? else {
        return Ok(Bson::Null);
    };
    // A NaN of either type is a double NaN.
    if is_nan(&v) {
        return Ok(Bson::Double(f64::NAN));
    }
    if cmp_num(&v, 0.0) != Ordering::Greater {
        return Err(fail(domain.0, domain.1));
    }
    classic(op, vec![v], ctx)
}

fn two(arg: &Bson, ctx: &Ctx) -> Result<(Bson, Bson), Fallback> {
    let mut v = operands(arg, ctx)?.into_iter();
    Ok((
        v.next().unwrap_or(Bson::Undefined),
        v.next().unwrap_or(Bson::Undefined),
    ))
}

fn is_zero(v: &Bson) -> bool {
    match v {
        Bson::Int32(n) => *n == 0,
        Bson::Int64(n) => *n == 0,
        Bson::Double(f) => *f == 0.0,
        Bson::Decimal128(_) => crate::decimal::from_bson(v).is_some_and(|d| d.is_zero()),
        _ => false,
    }
}

fn modulo(arg: &Bson, ctx: &Ctx) -> R {
    let (a, b) = two(arg, ctx)?;
    if nullish(&a) || nullish(&b) {
        return Ok(Bson::Null);
    }
    if !is_number(&a) || !is_number(&b) {
        return Err(fail(7157718, "$mod only supports numeric types"));
    }
    // The VM's `mod` refuses a zero divisor of an integer type; a double or
    // decimal zero divides to NaN.
    if is_zero(&b) {
        return Err(fail(4848403, "can't $mod by zero"));
    }
    classic("$mod", vec![a, b], ctx)
}

fn divide(arg: &Bson, ctx: &Ctx) -> R {
    let (a, b) = two(arg, ctx)?;
    if nullish(&a) || nullish(&b) {
        return Ok(Bson::Null);
    }
    if !is_number(&a) || !is_number(&b) {
        return Err(fail(7157719, "$divide only supports numeric types"));
    }
    if is_zero(&b) {
        return Err(fail(4848401, "can't $divide by zero"));
    }
    classic("$divide", vec![a, b], ctx)
}

fn pow(arg: &Bson, ctx: &Ctx) -> R {
    let (a, b) = two(arg, ctx)?;
    if nullish(&a) || nullish(&b) {
        return Ok(Bson::Null);
    }
    if !is_number(&a) || !is_number(&b) {
        return Err(fail(5154200, "$pow only supports numeric types"));
    }
    if is_zero(&a) && negative(&b) {
        return Err(fail(5154201, "$pow cannot raise 0 to a negative exponent"));
    }
    classic("$pow", vec![a, b], ctx)
}

fn round_trunc(op: &str, arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    let num = args.first().cloned().unwrap_or(Bson::Undefined);
    if nullish(&num) {
        return Ok(Bson::Null);
    }
    if !is_number(&num) {
        return Err(fail(5155300, format!("{op} only supports numeric types")));
    }
    let place = match args.get(1) {
        None => 0,
        Some(p) if nullish(p) => return Ok(Bson::Null),
        Some(p) => match exact_int(p, false) {
            Some(n) if (-20..=100).contains(&n) => n,
            _ => {
                return Err(fail(
                    5155301,
                    format!(
                        "{op} requires \"place\" argument to be an integer between -20 and 100"
                    ),
                ))
            }
        },
    };
    // An integer rounded to a negative place goes through a decimal and back;
    // one that no longer fits a long is SBE's own error.
    if place < 0 {
        if let Some(n) = match num {
            Bson::Int32(n) => Some(i128::from(n)),
            Bson::Int64(n) => Some(i128::from(n)),
            _ => None,
        } {
            let unit = 10i128.pow(place.unsigned_abs() as u32);
            let rounded = if op == "$trunc" {
                n / unit * unit
            } else {
                let q = n / unit;
                let r = n % unit;
                let half = unit / 2;
                let q = if r.abs() > half || (r.abs() == half && q % 2 != 0) {
                    q + r.signum()
                } else {
                    q
                };
                q * unit
            };
            if i64::try_from(rounded).is_err() {
                return Err(fail(
                    5155302,
                    format!("Invalid conversion to long during {op}."),
                ));
            }
        }
    }
    classic(op, vec![num, Bson::Int64(place)], ctx)
}

fn trig(op: &str, arg: &Bson, ctx: &Ctx) -> R {
    let v = single(arg, ctx)?;
    if nullish(&v) {
        return Ok(Bson::Null);
    }
    if !is_number(&v) {
        return Err(fail(7157800, format!("{op} supports only numeric types")));
    }
    classic(op, vec![v], ctx)
}

fn atan2(arg: &Bson, ctx: &Ctx) -> R {
    let (a, b) = two(arg, ctx)?;
    if nullish(&a) || nullish(&b) {
        return Ok(Bson::Null);
    }
    if !is_number(&a) || !is_number(&b) {
        return Err(fail(7157801, "$atan2 supports only numeric types"));
    }
    classic("$atan2", vec![a, b], ctx)
}

fn trig_bounded(
    op: &str,
    arg: &Bson,
    ctx: &Ctx,
    low: &str,
    high: &str,
    bounds: Option<(f64, f64)>,
) -> R {
    let v = single(arg, ctx)?;
    if nullish(&v) {
        return Ok(Bson::Null);
    }
    if !is_number(&v) {
        return Err(fail(7157802, format!("{op} supports only numeric types")));
    }
    if is_nan(&v) {
        return Ok(v);
    }
    let inside = match bounds {
        // (-inf, inf): every finite number.
        None => {
            cmp_num(&v, f64::NEG_INFINITY) == Ordering::Greater
                && cmp_num(&v, f64::INFINITY) == Ordering::Less
        }
        Some((lo, hi)) => cmp_num(&v, lo) != Ordering::Less && cmp_num(&v, hi) != Ordering::Greater,
    };
    if !inside {
        return Err(fail(
            7157803,
            format!("Cannot apply {op}, value must be in {low}, {high}"),
        ));
    }
    classic(op, vec![v], ctx)
}

/// The VM's `coerceToString`: `None` for a type it does not convert.
fn coerce_to_string(v: &Bson) -> Option<String> {
    Some(match v {
        Bson::String(s) | Bson::Symbol(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(f) => super::format_double_g(*f),
        Bson::Decimal128(d) => d.to_string(),
        Bson::DateTime(d) => super::coerce_to_string(&Bson::DateTime(*d)).ok()?,
        Bson::Timestamp(t) => format!("Timestamp({}, {})", t.time, t.increment),
        Bson::Null => String::new(),
        _ => return None,
    })
}

fn substr_bytes(arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    let get = |i: usize| args.get(i).cloned().unwrap_or(Bson::Undefined);
    let (s, start, len) = (get(0), get(1), get(2));
    let s = if nullish(&s) {
        String::new()
    } else {
        coerce_to_string(&s).ok_or_else(|| {
            fail(
                5155608,
                "$substrBytes: string expression could not be resolved to a string",
            )
        })?
    };
    if nullish(&start) || !is_number(&start) || negative(&start) {
        return Err(fail(
            5155603,
            "Starting index must be non-negative numeric type",
        ));
    }
    if nullish(&len) || !is_number(&len) {
        return Err(fail(5155602, "Length must be a numeric type"));
    }
    // Either bound not exactly a long, or a negative start: nothing.
    let (Some(start), Some(len)) = (exact_int(&start, false), exact_int(&len, false)) else {
        return Ok(Bson::Undefined);
    };
    if start < 0 {
        return Ok(Bson::Undefined);
    }
    let bytes = s.as_bytes();
    let length = if len < 0 { bytes.len() as i64 } else { len };
    let cont = |i: i64| (bytes[i as usize] & 0xC0) == 0x80;
    if start < bytes.len() as i64 && cont(start) {
        return Err(fail(
            5155604,
            "Invalid range: starting index is a UTF-8 continuation byte",
        ));
    }
    if start.saturating_add(length) < bytes.len() as i64 && cont(start + length) {
        return Err(fail(
            5155605,
            "Invalid range: ending index is a UTF-8 continuation character",
        ));
    }
    if start >= bytes.len() as i64 {
        return Ok(Bson::String(String::new()));
    }
    let end = if len < 0 {
        bytes.len()
    } else {
        (start as usize)
            .saturating_add(len as usize)
            .min(bytes.len())
    };
    Ok(Bson::String(
        String::from_utf8_lossy(&bytes[start as usize..end]).into_owned(),
    ))
}

fn substr_cp(arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    let get = |i: usize| args.get(i).cloned().unwrap_or(Bson::Undefined);
    let (s, start, len) = (get(0), get(1), get(2));
    let s = if nullish(&s) {
        String::new()
    } else {
        coerce_to_string(&s).ok_or_else(|| {
            fail(
                5155708,
                "$substrCP: string expression could not be resolved to a string",
            )
        })?
    };
    let start = match exact_int(&start, true) {
        Some(n) if !nullish(&start) => n,
        _ => {
            return Err(fail(
                5155700,
                "$substrCP: starting index must be numeric type representable as a 32-bit integral value",
            ))
        }
    };
    if start < 0 {
        return Err(fail(
            5155701,
            "$substrCP: starting index must be a non-negative integer",
        ));
    }
    let len =
        match exact_int(&len, true) {
            Some(n) => n,
            None => return Err(fail(
                5155702,
                "$substrCP: length must be numeric type representable as a 32-bit integral value",
            )),
        };
    if len < 0 {
        return Err(fail(
            5155703,
            "$substrCP: length must be a non-negative integer",
        ));
    }
    Ok(Bson::String(
        s.chars().skip(start as usize).take(len as usize).collect(),
    ))
}

/// `$toUpper` / `$toLower` (and each side of `$strcasecmp`): the string the
/// case conversion applies to.
fn case_convert(name: &str, v: Bson) -> Result<String, Fallback> {
    if nullish(&v) {
        return Ok(String::new());
    }
    let convertible = matches!(
        v,
        Bson::String(_)
            | Bson::Symbol(_)
            | Bson::Int32(_)
            | Bson::Int64(_)
            | Bson::Double(_)
            | Bson::Decimal128(_)
            | Bson::DateTime(_)
            | Bson::Timestamp(_)
    );
    match convertible.then(|| coerce_to_string(&v)).flatten() {
        Some(s) => Ok(s),
        None => Err(fail(
            7158200,
            format!("${name} input type is not supported"),
        )),
    }
}

fn strcasecmp(arg: &Bson, ctx: &Ctx) -> R {
    let (a, b) = two(arg, ctx)?;
    // The right operand is lowered first, so its failure comes first.
    let b = case_convert("toUpper", b)?.to_ascii_uppercase();
    let a = case_convert("toUpper", a)?.to_ascii_uppercase();
    Ok(Bson::Int32(match a.cmp(&b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

fn index_of(op: &str, arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    let get = |i: usize| args.get(i).cloned().unwrap_or(Bson::Undefined);
    let (s, sub) = (get(0), get(1));
    let name = &op[1..];
    if nullish(&s) {
        return Ok(Bson::Null);
    }
    if !matches!(s, Bson::String(_)) {
        return Err(fail(
            7158007,
            format!("${name} string must resolve to a string or null"),
        ));
    }
    if nullish(&sub) {
        return Err(fail(
            7158008,
            format!("${name} substring must resolve to a string"),
        ));
    }
    if !matches!(sub, Bson::String(_)) {
        return Err(fail(
            7158009,
            format!("${name} substring must resolve to a string"),
        ));
    }
    let mut bounds = Vec::new();
    for (i, which) in [(2usize, "start"), (3, "end")] {
        if i == 3 && args.len() < 4 {
            break;
        }
        let v = if i == 2 && args.len() < 3 {
            Bson::Int64(0)
        } else {
            get(i)
        };
        let (nan_code, neg_code) = if which == "start" {
            (7158003, 7158004)
        } else {
            (7158005, 7158006)
        };
        if nullish(&v) || exact_int(&v, true).is_none() {
            return Err(fail(
                nan_code,
                format!("${name} {which} index must resolve to a number"),
            ));
        }
        if negative(&v) {
            return Err(fail(
                neg_code,
                format!("${name} {which} index must be positive"),
            ));
        }
        bounds.push(v);
    }
    let mut all = vec![s, sub];
    all.extend(bounds);
    classic(op, all, ctx)
}

fn concat(arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    if args.iter().any(nullish) {
        return Ok(Bson::Null);
    }
    if args.iter().all(|a| matches!(a, Bson::String(_))) {
        return classic("$concat", args, ctx);
    }
    Err(fail(7158201, "$concat supports only strings"))
}

fn regex(op: &str, arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(spec) = arg else {
        return apply_classic(op, arg, ctx);
    };
    let name = &op[1..];
    let error = |code: i32, msg: &str| fail(code, format!("${name}: {msg}"));
    let null_answer = || match op {
        "$regexMatch" => Bson::Boolean(false),
        "$regexFindAll" => Bson::Array(vec![]),
        _ => Bson::Null,
    };
    let input = eval(spec.get("input").unwrap_or(&Bson::Null), ctx)?;
    let regex_expr = spec.get("regex").cloned().unwrap_or(Bson::Null);
    // A constant pattern is compiled when the stage is built.
    let constant = !matches!(&regex_expr, Bson::String(s) if s.starts_with('$'))
        && !matches!(&regex_expr, Bson::Document(_) | Bson::Array(_));
    let pattern = eval_field_value(&regex_expr, ctx)?;
    if nullish(&input) {
        return Ok(null_answer());
    }
    if !matches!(input, Bson::String(_)) {
        return Err(error(5073401, "input must be of type string"));
    }
    let options = match spec.get("options") {
        None => None,
        Some(o) => Some(eval_field_value(o, ctx)?),
    };
    let bound: Vec<String> = ctx.vars.keys().cloned().collect();
    let options_constant = spec
        .get("options")
        .is_none_or(|o| crate::aggregate::is_constant_expression(o, &bound));
    if constant && options_constant {
        // A constant pattern and options are compiled when the pipeline is
        // optimized, by the classic code.
        return apply_classic(op, arg, ctx);
    }
    // Options given but missing are nothing: the compile fails, unless the
    // pattern is null (the null answer) or missing (nothing).
    if matches!(options, Some(Bson::Undefined)) {
        return match &pattern {
            Bson::Undefined => Ok(Bson::Undefined),
            Bson::Null => Ok(null_answer()),
            _ => Err(error(
                5073403,
                "error occurred while executing the regular expression",
            )),
        };
    }
    let string_options = match &options {
        None => None,
        Some(Bson::String(o)) if o.contains('\0') => {
            return Err(error(
                5126604,
                "regex flags must not have embedded null bytes",
            ))
        }
        Some(Bson::String(o)) => Some(o.clone()),
        Some(Bson::Null) => Some(String::new()),
        Some(_) => {
            return Err(error(
                5126603,
                "regex flags must have either string or null type",
            ))
        }
    };
    let options = match (&string_options, &pattern) {
        (Some(s), Bson::RegularExpression(re)) => {
            let flags = re.options.clone();
            if s.is_empty() {
                Some(flags)
            } else if flags.is_empty() {
                Some(s.clone())
            } else {
                return Err(error(
                    5126605,
                    "regex options cannot be specified in both BSON RegEx and 'options' field",
                ));
            }
        }
        (s, _) => s.clone(),
    };
    // A missing pattern is nothing; a null one, the null answer.
    match &pattern {
        Bson::Undefined => return Ok(Bson::Undefined),
        Bson::Null => return Ok(null_answer()),
        _ => {}
    }
    let (pattern_text, flags) = match (&pattern, options) {
        (Bson::String(p), o) if string_options.is_none() => {
            if p.contains('\0') {
                return Err(error(
                    5126602,
                    "regex pattern must not have embedded null bytes",
                ));
            }
            (p.clone(), o.unwrap_or_default())
        }
        (Bson::RegularExpression(re), None) => (re.pattern.clone(), re.options.clone()),
        (_, None) => {
            return Err(error(
                5126601,
                "regex pattern must have either string or BSON RegEx type",
            ))
        }
        // With options the pattern goes to the compiler as it is: anything but
        // a string compiles to nothing, and the call fails.
        (Bson::String(p), Some(o)) if !p.contains('\0') => (p.clone(), o),
        (Bson::RegularExpression(re), Some(o)) if string_options.is_none() => {
            (re.pattern.clone(), o)
        }
        _ => {
            return Err(error(
                5073403,
                "error occurred while executing the regular expression",
            ))
        }
    };
    // Compiled as the document is read: SBE's own codes for a pattern or
    // flag the compiler refuses.
    classic_named(
        op,
        doc! { "input": input, "regex": pattern_text, "options": flags },
        ctx,
    )
    .map_err(|f| match f.as_mongo() {
        Some((51111, m)) => match m.split_once(": ") {
            Some((_, why)) => fail(5073402, format!("Invalid Regex: {why}")),
            None => f,
        },
        Some((51108, m)) => match m.split_once(" invalid flag") {
            Some((_, rest)) => fail(51108, format!(" invalid flag{rest}")),
            None => f,
        },
        _ => f,
    })
}

fn array_to_object(arg: &Bson, ctx: &Ctx) -> R {
    let v = single(arg, ctx)?;
    let items = match v {
        Bson::Array(items) => items,
        v if nullish(&v) => return Ok(Bson::Null),
        _ => return Err(fail(5153200, "$arrayToObject requires an array input")),
    };
    let mut out = Document::new();
    let Some(first) = items.first() else {
        return Ok(Bson::Document(out));
    };
    let pairs = match first {
        Bson::Array(_) => true,
        Bson::Document(_) => false,
        _ => {
            return Err(fail(
                5153201,
                "Input to $arrayToObject should be either an array or object",
            ))
        }
    };
    for item in &items {
        if pairs {
            let Bson::Array(pair) = item else {
                return Err(fail(
                    5153202,
                    "$arrayToObject requires a consistent input format. Expected an array",
                ));
            };
            let Some(key) = pair.first() else {
                return Err(fail(
                    5153203,
                    "$arrayToObject requires an array of size 2 arrays",
                ));
            };
            let Bson::String(key) = key else {
                return Err(fail(
                    5153204,
                    "$arrayToObject requires an array of key-value pairs, where the key must be of type string",
                ));
            };
            let Some(value) = pair.get(1) else {
                return Err(fail(
                    5153205,
                    "$arrayToObject requires an array of size 2 arrays",
                ));
            };
            if pair.len() > 2 {
                return Err(fail(
                    5153206,
                    "$arrayToObject requires an array of size 2 arrays",
                ));
            }
            if key.contains('\0') {
                return Err(fail(
                    5153207,
                    "Key field cannot contain an embedded null byte",
                ));
            }
            out.insert(key.clone(), value.clone());
        } else {
            let Bson::Document(kv) = item else {
                return Err(fail(
                    5153208,
                    "$arrayToObject requires a consistent input format. Expected an object",
                ));
            };
            let wrong_count = "$arrayToObject requires an object keys of 'k' and 'v'. Found incorrect number of keys";
            let mut keys = kv.iter();
            let Some((first_name, first_value)) = keys.next() else {
                return Err(fail(5153209, wrong_count));
            };
            let Some((second_name, second_value)) = keys.next() else {
                return Err(fail(5153210, wrong_count));
            };
            if keys.next().is_some() {
                return Err(fail(5153211, wrong_count));
            }
            // `k` first, then `v`: the other order is refused.
            if first_name != "k" || second_name != "v" {
                return Err(fail(
                    5153212,
                    "$arrayToObject requires an object with keys 'k' and 'v'.",
                ));
            }
            let Bson::String(key) = first_value else {
                return Err(fail(
                    5153213,
                    "$arrayToObject requires an object with keys 'k' and 'v', where the value of 'k' must be of type string",
                ));
            };
            if key.contains('\0') {
                return Err(fail(
                    5153214,
                    "Key field cannot contain an embedded null byte",
                ));
            }
            out.insert(key.clone(), second_value.clone());
        }
    }
    Ok(Bson::Document(out))
}

fn concat_arrays(arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    if args.iter().all(|a| matches!(a, Bson::Array(_))) {
        return classic("$concatArrays", args, ctx);
    }
    if args.iter().any(nullish) {
        return Ok(Bson::Null);
    }
    Err(fail(7158000, "$concatArrays only supports arrays"))
}

fn is_date(v: &Bson) -> bool {
    matches!(v, Bson::DateTime(_))
}

fn subtract(arg: &Bson, ctx: &Ctx) -> R {
    let (a, b) = two(arg, ctx)?;
    if nullish(&a) || nullish(&b) {
        return Ok(Bson::Null);
    }
    let ok = if is_number(&a) {
        is_number(&b)
    } else {
        is_date(&a) && (is_number(&b) || is_date(&b))
    };
    if !ok {
        return Err(fail(
            7157720,
            "Only numbers and dates are allowed in an $subtract expression. To subtract a number \
             from a date, the date must be the first argument.",
        ));
    }
    classic("$subtract", vec![a, b], ctx)
}

fn add(arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    if args.is_empty() {
        return Ok(Bson::Int32(0));
    }
    if args.iter().any(nullish) {
        return Ok(Bson::Null);
    }
    let mut dates = 0;
    for a in &args {
        if is_date(a) {
            dates += 1;
        } else if !is_number(a) {
            return Err(fail(
                7157723,
                "only numbers and dates are allowed in an $add expression",
            ));
        }
    }
    if dates > 1 {
        return Err(fail(7157722, "only one date allowed in an $add expression"));
    }
    classic("$add", args, ctx)
}

fn multiply(arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    if args.is_empty() {
        return Ok(Bson::Int32(1));
    }
    if args.iter().any(nullish) {
        return Ok(Bson::Null);
    }
    if !args.iter().all(is_number) {
        return Err(fail(
            7157721,
            "only numbers are allowed in an $multiply expression",
        ));
    }
    classic("$multiply", args, ctx)
}

fn range(arg: &Bson, ctx: &Ctx) -> R {
    let args = operands(arg, ctx)?;
    let get = |i: usize| args.get(i).cloned().unwrap_or(Bson::Undefined);
    let step = if args.len() >= 3 {
        get(2)
    } else {
        Bson::Int32(1)
    };
    let (start, end) = (get(0), get(1));
    for (v, which, code) in [
        (&start, "start", 7157711),
        (&end, "end", 7157712),
        (&step, "step", 7157713),
    ] {
        // Not a number is checked without a null test first: a missing
        // operand makes the check, and so the result, nothing.
        if matches!(v, Bson::Undefined) {
            return Ok(Bson::Undefined);
        }
        if !is_number(v) {
            return Err(fail(
                code,
                format!("$range only supports numeric types for {which}"),
            ));
        }
    }
    let mut ints = Vec::new();
    for (v, which, code) in [
        (&start, "start", 7157714),
        (&end, "end", 7157715),
        (&step, "step", 7157716),
    ] {
        match exact_int(v, true) {
            Some(n) => ints.push(n),
            None => {
                let what = if which == "end" {
                    "end argument cannot be represented as a 32-bit integer"
                } else if which == "start" {
                    "start argument cannot be represented as a 32-bit integer"
                } else {
                    "step argument cannot be represented as a 32-bit integer"
                };
                return Err(fail(code, format!("$range {what}")));
            }
        }
    }
    if ints[2] == 0 {
        return Err(fail(7157717, "$range requires a non-zero step value"));
    }
    classic("$range", ints.into_iter().map(Bson::Int64).collect(), ctx)
}

fn replace_one(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(spec) = arg else {
        return apply_classic("$replaceOne", arg, ctx);
    };
    let field = |k: &str| -> R { eval(spec.get(k).unwrap_or(&Bson::Null), ctx) };
    // Evaluated replacement first, then find, then input.
    let replacement = field("replacement")?;
    let find = field("find")?;
    let input = field("input")?;
    for (v, name) in [
        (&input, "input"),
        (&find, "find"),
        (&replacement, "replacement"),
    ] {
        if !nullish(v) && !matches!(v, Bson::String(_)) {
            return Err(fail(
                7158302,
                format!("$replaceOne requires that '{name}' be a string"),
            ));
        }
    }
    if nullish(&input) || nullish(&find) || nullish(&replacement) {
        return Ok(Bson::Null);
    }
    classic_named(
        "$replaceOne",
        doc! { "input": input, "find": find, "replacement": replacement },
        ctx,
    )
}

fn sort_array(arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(spec) = arg else {
        return apply_classic("$sortArray", arg, ctx);
    };
    let input = eval(spec.get("input").unwrap_or(&Bson::Null), ctx)?;
    match input {
        v if nullish(&v) => Ok(Bson::Null),
        Bson::Array(_) => {
            let mut lit = spec.clone();
            lit.insert("input", doc! { "$literal": input });
            apply_classic("$sortArray", &Bson::Document(lit), ctx)
        }
        _ => Err(fail(7158001, "$sortArray input argument must be an array")),
    }
}

fn date_like(v: &Bson) -> bool {
    matches!(
        v,
        Bson::DateTime(_) | Bson::Timestamp(_) | Bson::ObjectId(_)
    )
}

fn tz_valid(v: &Bson) -> bool {
    matches!(v, Bson::String(_)) && super::mongo_tz(Some(v)).is_ok()
}

fn unit_valid(v: &Bson) -> bool {
    super::date_unit_arg("", v).is_ok()
}

fn day_of_week_valid(v: &Bson) -> bool {
    matches!(v, Bson::String(_)) && super::parse_start_of_week("", v).is_ok()
}

/// The fields of a named-argument operator, each evaluated (missing stays
/// missing), and the operator's argument rebuilt from those values for the
/// classic computation.
struct Named {
    values: Document,
    spec: Document,
}

impl Named {
    fn new(arg: &Bson, ctx: &Ctx) -> Result<Option<Self>, Fallback> {
        let Bson::Document(spec) = arg else {
            return Ok(None);
        };
        let mut values = Document::new();
        for (k, v) in spec {
            values.insert(k.clone(), eval_field_value(v, ctx)?);
        }
        Ok(Some(Named {
            values,
            spec: spec.clone(),
        }))
    }

    /// The field's value: missing when absent or missing.
    fn get(&self, k: &str) -> Bson {
        self.values.get(k).cloned().unwrap_or(Bson::Undefined)
    }

    fn has(&self, k: &str) -> bool {
        self.spec.contains_key(k)
    }

    fn constant(&self, k: &str, ctx: &Ctx) -> bool {
        self.spec.get(k).is_none_or(|e| {
            crate::aggregate::is_constant_expression(
                e,
                &ctx.vars.keys().cloned().collect::<Vec<_>>(),
            )
        })
    }

    fn classic(&self, op: &str, ctx: &Ctx) -> R {
        let lit: Document = self
            .values
            .iter()
            .filter(|(_, v)| !matches!(v, Bson::Undefined))
            .map(|(k, v)| (k.clone(), Bson::Document(doc! { "$literal": v.clone() })))
            .collect();
        apply_classic(op, &Bson::Document(lit), ctx).map_err(unfolded)
    }
}

/// `$year`, `$hour`, ...: the builtin's answer when it has one; otherwise the
/// checks in SBE's order, and nothing when they all pass.
fn date_part(op: &str, arg: &Bson, ctx: &Ctx) -> Option<R> {
    let named =
        matches!(arg, Bson::Document(d) if d.keys().next().is_some_and(|k| !k.starts_with('$')));
    let name = &op[1..];
    Some((|| {
        let (date, tz, tz_constant) = if named {
            if let Bson::Document(d) = arg {
                super::precheck_constant_timezone(d, ctx, None)?;
            }
            let n = Named::new(arg, ctx)?.expect("a document");
            let tz = if n.has("timezone") {
                n.get("timezone")
            } else {
                Bson::String("UTC".into())
            };
            (n.get("date"), tz, n.constant("timezone", ctx))
        } else {
            (eval(arg, ctx)?, Bson::String("UTC".into()), true)
        };
        // A constant timezone is checked when the stage is built, before any
        // document: its error has no executor wrapper.
        if tz_constant && !matches!(tz, Bson::String(_)) {
            return Err(fail(
                5157900,
                format!("${name} parameter 'timezone' must be a string"),
            )
            .bare());
        }
        if tz_constant && !tz_valid(&tz) {
            return Err(fail(
                5157901,
                format!("${name} parameter 'timezone' must be a valid timezone"),
            )
            .bare());
        }
        // The builtin runs first; an unknown zone fails inside it as the
        // classic engine fails.
        if date_like(&date) && matches!(tz, Bson::String(_)) {
            let lit =
                Bson::Document(doc! { "date": {"$literal": date}, "timezone": {"$literal": tz} });
            return apply_classic(op, &lit, ctx).map_err(unfolded);
        }
        if nullish(&date) {
            return Ok(Bson::Null);
        }
        if nullish(&tz) {
            return Ok(Bson::Null);
        }
        if !matches!(tz, Bson::String(_)) {
            return Err(fail(
                5157902,
                format!("${name} parameter 'timezone' must be a string"),
            ));
        }
        if !date_like(&date) {
            return Err(fail(
                5157904,
                format!("{name} parameter 'date' must be coercible to date"),
            ));
        }
        Ok(Bson::Undefined)
    })())
}

fn date_add(op: &str, arg: &Bson, ctx: &Ctx) -> R {
    // The classic optimizer's checks of constant arguments come first.
    if let Bson::Document(d) = arg {
        if super::optimize_date_args(op, arg, d, ctx)? {
            return Ok(Bson::Null);
        }
    }
    let Some(n) = Named::new(arg, ctx)? else {
        return apply_classic(op, arg, ctx);
    };
    let name = &op[1..];
    let start = n.get("startDate");
    let unit = n.get("unit");
    let amount = n.get("amount");
    let tz = if n.has("timezone") {
        n.get("timezone")
    } else {
        Bson::String("UTC".into())
    };
    if [&start, &unit, &amount, &tz].iter().any(|v| nullish(v)) {
        return Ok(Bson::Null);
    }
    if !matches!(tz, Bson::String(_)) {
        return Err(fail(
            7157902,
            format!("${name} expects timezone argument of type string"),
        ));
    }
    if !tz_valid(&tz) {
        return Err(fail(7157903, format!("${name} expects a valid timezone")));
    }
    if !date_like(&start) {
        return Err(fail(
            7157904,
            format!("${name} must have startDate argument convertable to date"),
        ));
    }
    if !matches!(unit, Bson::String(_)) {
        return Err(fail(
            7157905,
            format!("${name} expects unit argument of type string"),
        ));
    }
    if !unit_valid(&unit) {
        return Err(fail(7157906, format!("${name} expects a valid time unit")));
    }
    // `$dateSubtract` converts the negated amount, so the long minimum is no
    // amount there.
    let converted = exact_int(&amount, false).filter(|a| op == "$dateAdd" || *a != i64::MIN);
    if converted.is_none() {
        return Err(fail(
            7157907,
            format!("invalid ${name} 'amount' argument value"),
        ));
    }
    n.classic(op, ctx)
}

fn date_diff(arg: &Bson, ctx: &Ctx) -> R {
    // The classic optimizer's checks of constant arguments come first.
    if let Bson::Document(d) = arg {
        if super::optimize_date_args("$dateDiff", arg, d, ctx)? {
            return Ok(Bson::Null);
        }
    }
    let Some(n) = Named::new(arg, ctx)? else {
        return apply_classic("$dateDiff", arg, ctx);
    };
    let start = n.get("startDate");
    let end = n.get("endDate");
    let unit = n.get("unit");
    let tz = if n.has("timezone") {
        n.get("timezone")
    } else {
        Bson::String("UTC".into())
    };
    let week = matches!(&unit, Bson::String(u) if u == "week");
    let sow = n.has("startOfWeek").then(|| n.get("startOfWeek"));
    if [&start, &end, &unit, &tz].iter().any(|v| nullish(v)) {
        return Ok(Bson::Null);
    }
    if week && sow.as_ref().is_some_and(nullish) {
        return Ok(Bson::Null);
    }
    // A constant null `startOfWeek` is folded to a null result.
    if sow.as_ref().is_some_and(nullish) && n.constant("startOfWeek", ctx) {
        return Ok(Bson::Null);
    }
    if !matches!(tz, Bson::String(_)) {
        return Err(fail(
            7157919,
            "$dateDiff parameter 'timezone' must be a string",
        ));
    }
    if !tz_valid(&tz) {
        return Err(fail(
            7157920,
            "$dateDiff parameter 'timezone' must be a valid timezone",
        ));
    }
    if !date_like(&start) {
        return Err(fail(
            7157921,
            "$dateDiff parameter 'startDate' must be coercible to date",
        ));
    }
    if !date_like(&end) {
        return Err(fail(
            7157922,
            "$dateDiff parameter 'endDate' must be coercible to date",
        ));
    }
    if !matches!(unit, Bson::String(_)) {
        return Err(fail(7157923, "$dateDiff parameter 'unit' must be a string"));
    }
    if !unit_valid(&unit) {
        return Err(fail(
            7157924,
            "$dateDiff parameter 'unit' must be a valid time unit",
        ));
    }
    if let (true, Some(sow)) = (week, &sow) {
        if !matches!(sow, Bson::String(_)) {
            return Err(fail(
                7157925,
                "$dateDiff parameter 'startOfWeek' must be a string",
            ));
        }
        if !day_of_week_valid(sow) {
            return Err(fail(
                7157926,
                "$dateDiff parameter 'startOfWeek' must be a valid day of the week",
            ));
        }
    }
    n.classic("$dateDiff", ctx)
}

fn date_to_parts(arg: &Bson, ctx: &Ctx) -> R {
    let Some(n) = Named::new(arg, ctx)? else {
        return apply_classic("$dateToParts", arg, ctx);
    };
    let date = n.get("date");
    let tz = if n.has("timezone") {
        n.get("timezone")
    } else {
        Bson::String("UTC".into())
    };
    let iso = if n.has("iso8601") {
        n.get("iso8601")
    } else {
        Bson::Boolean(false)
    };
    if nullish(&tz) {
        return Ok(Bson::Null);
    }
    if !matches!(tz, Bson::String(_)) {
        return Err(fail(7157912, "$dateToParts timezone must be a string"));
    }
    if !tz_valid(&tz) {
        return Err(fail(
            7157913,
            "$dateToParts timezone must be a valid timezone",
        ));
    }
    if nullish(&iso) {
        return Ok(Bson::Null);
    }
    if !matches!(iso, Bson::Boolean(_)) {
        return Err(fail(7157914, "$dateToParts iso8601 must be a boolean"));
    }
    if nullish(&date) {
        return Ok(Bson::Null);
    }
    if !date_like(&date) {
        return Err(fail(
            7157915,
            "$dateToParts date must have the format of a date",
        ));
    }
    n.classic("$dateToParts", ctx)
}

fn date_to_string(arg: &Bson, ctx: &Ctx) -> R {
    if let Bson::Document(d) = arg {
        super::precheck_constant_timezone(d, ctx, None)?;
    }
    let Some(n) = Named::new(arg, ctx)? else {
        return apply_classic("$dateToString", arg, ctx);
    };
    // A constant format or timezone is checked when the stage is built,
    // before any document is read: its error has no executor wrapper.
    if n.has("format") && n.constant("format", ctx) {
        if let Bson::String(_) = n.get("format") {
            if let Err(f) = n.classic("$dateToString", ctx) {
                if matches!(f.as_mongo(), Some((18534..=18536, _))) {
                    return Err(f.bare());
                }
            }
        }
    }
    let date = n.get("date");
    let format = if n.has("format") {
        n.get("format")
    } else {
        Bson::String(String::new())
    };
    let tz = if n.has("timezone") {
        n.get("timezone")
    } else {
        Bson::String("UTC".into())
    };
    if date_like(&date) && matches!(format, Bson::String(_)) && tz_valid(&tz) {
        if let Ok(v) = n.classic("$dateToString", ctx) {
            return Ok(v);
        }
    }
    if nullish(&date) {
        return match n.spec.get("onNull") {
            Some(_) => Ok(match n.get("onNull") {
                Bson::Undefined => Bson::Undefined,
                v => v,
            }),
            None => Ok(Bson::Null),
        };
    }
    if nullish(&format) || nullish(&tz) {
        return Ok(Bson::Null);
    }
    if !date_like(&date) {
        return Err(fail(
            4997901,
            "$dateToString parameter 'date' must be coercible to date",
        ));
    }
    if !n.constant("timezone", ctx) {
        if !matches!(tz, Bson::String(_)) {
            return Err(fail(
                4997907,
                "$dateToString parameter 'timezone' must be a string",
            ));
        }
        if !tz_valid(&tz) {
            return Err(fail(
                4997908,
                "$dateToString parameter 'timezone' must be a valid timezone",
            ));
        }
    }
    if n.has("format") && !n.constant("format", ctx) {
        if !matches!(format, Bson::String(_)) {
            return Err(fail(
                4997903,
                "$dateToString parameter 'format' must be a string",
            ));
        }
        return Err(fail(
            4997904,
            "$dateToString parameter 'format' must be a valid format",
        ));
    }
    n.classic("$dateToString", ctx)
}

fn date_from_string(arg: &Bson, ctx: &Ctx) -> R {
    if let Bson::Document(d) = arg {
        super::precheck_constant_timezone(d, ctx, None)?;
    }
    let Some(n) = Named::new(arg, ctx)? else {
        return apply_classic("$dateFromString", arg, ctx);
    };
    let on_error = n.has("onError");
    let ds = n.get("dateString");
    if nullish(&ds) {
        return Ok(if n.has("onNull") {
            n.get("onNull")
        } else {
            Bson::Null
        });
    }
    if !matches!(ds, Bson::String(_)) {
        if on_error {
            return Ok(n.get("onError"));
        }
        return Err(fail(
            241,
            "$dateFromString requires that 'dateString' be a string",
        ));
    }
    if n.has("timezone") && nullish(&n.get("timezone")) {
        return Ok(Bson::Null);
    }
    if n.has("format") {
        let format = n.get("format");
        if nullish(&format) {
            return Ok(Bson::Null);
        }
        if !n.constant("format", ctx) && !matches!(format, Bson::String(_)) {
            return Err(fail(
                4997803,
                "$dateFromString requires that 'format' be a string",
            ));
        }
    }
    if n.has("timezone") && !n.constant("timezone", ctx) {
        let tz = n.get("timezone");
        if !matches!(tz, Bson::String(_)) {
            return Err(fail(
                4997807,
                "$dateFromString parameter 'timezone' must be a string",
            ));
        }
        if !tz_valid(&tz) {
            return Err(fail(
                4997808,
                "$dateFromString parameter 'timezone' must be a valid timezone",
            ));
        }
    }
    n.classic("$dateFromString", ctx)
}

fn date_from_parts(arg: &Bson, ctx: &Ctx) -> R {
    let Some(n) = Named::new(arg, ctx)? else {
        return apply_classic("$dateFromParts", arg, ctx);
    };
    let iso = n.has("isoWeekYear");
    let fields: &[&str] = if iso {
        &[
            "isoWeekYear",
            "isoWeek",
            "isoDayOfWeek",
            "hour",
            "minute",
            "second",
            "millisecond",
        ]
    } else {
        &[
            "year",
            "month",
            "day",
            "hour",
            "minute",
            "second",
            "millisecond",
        ]
    };
    // Each part, in order: null when missing or null, else exactly a long.
    let mut parts = Vec::new();
    for f in fields {
        if !n.has(f) {
            parts.push(None);
            continue;
        }
        let v = n.get(f);
        if matches!(v, Bson::Null | Bson::Undefined) {
            parts.push(Some(None));
            continue;
        }
        match exact_int(&v, false) {
            Some(i) => parts.push(Some(Some(i))),
            None => return Err(fail(7157917, format!("'{f}' must evaluate to an integer"))),
        }
    }
    // A missing timezone is nothing, and so null; any other non-string fails.
    if n.has("timezone") && !matches!(n.get("timezone"), Bson::String(_) | Bson::Undefined) {
        return Err(fail(7157918, "'timezone' must evaluate to a string"));
    }
    if n.has("timezone") && matches!(n.get("timezone"), Bson::Undefined) {
        return Ok(Bson::Null);
    }
    if parts.iter().any(|p| matches!(p, Some(None))) {
        return Ok(Bson::Null);
    }
    for (i, f) in fields.iter().enumerate().take(5) {
        let Some(Some(v)) = parts[i] else { continue };
        let (lo, hi) = if i == 0 { (1, 9999) } else { (-32768, 32767) };
        if v < lo || v > hi {
            let msg = if i == 0 {
                format!("'{f}' must evaluate to an integer in the range {lo} to {hi}")
            } else {
                format!("'{f}' must evaluate to a value in the range [{lo}, {hi}]")
            };
            return Err(fail(7157916, msg));
        }
    }
    // An unknown zone fails in the builtin, as the classic engine fails.
    n.classic("$dateFromParts", ctx)
}

fn date_trunc(arg: &Bson, ctx: &Ctx) -> R {
    // The classic optimizer's checks of constant arguments come first.
    if let Bson::Document(d) = arg {
        if super::optimize_date_args("$dateTrunc", arg, d, ctx)? {
            return Ok(Bson::Null);
        }
    }
    let Some(n) = Named::new(arg, ctx)? else {
        return apply_classic("$dateTrunc", arg, ctx);
    };
    let date = n.get("date");
    let unit = n.get("unit");
    let bin = if n.has("binSize") {
        n.get("binSize")
    } else {
        Bson::Int64(1)
    };
    let tz = if n.has("timezone") {
        n.get("timezone")
    } else {
        Bson::String("UTC".into())
    };
    let sow = if n.has("startOfWeek") {
        n.get("startOfWeek")
    } else {
        Bson::String("sun".into())
    };
    let week = matches!(&unit, Bson::String(u) if u == "week");
    let positive_long = |v: &Bson| is_number(v) && exact_int(v, false).is_some_and(|b| b > 0);
    // A constant null `startOfWeek` is folded to a null result.
    if n.has("startOfWeek") && nullish(&sow) && n.constant("startOfWeek", ctx) {
        return Ok(Bson::Null);
    }
    if date_like(&date)
        && unit_valid(&unit)
        && positive_long(&bin)
        && tz_valid(&tz)
        && (!week || day_of_week_valid(&sow))
    {
        return n.classic("$dateTrunc", ctx);
    }
    if [&date, &unit, &bin, &tz].iter().any(|v| nullish(v)) || (week && nullish(&sow)) {
        return Ok(Bson::Null);
    }
    if !n.constant("timezone", ctx) {
        if !matches!(tz, Bson::String(_)) {
            return Err(fail(
                7157930,
                "$dateTrunc parameter 'timezone' must be a string",
            ));
        }
        if !tz_valid(&tz) {
            return Err(fail(
                7157931,
                "$dateTrunc parameter 'timezone' must be a valid timezone",
            ));
        }
    }
    if !date_like(&date) {
        return Err(fail(
            7157932,
            "$dateTrunc parameter 'date' must be coercible to date",
        ));
    }
    if !n.constant("unit", ctx) {
        if !matches!(unit, Bson::String(_)) {
            return Err(fail(
                7157935,
                "$dateTrunc parameter 'unit' must be a string",
            ));
        }
        if !unit_valid(&unit) {
            return Err(fail(
                7157936,
                "$dateTrunc parameter 'unit' must be a valid time unit",
            ));
        }
    }
    if n.has("binSize") && !n.constant("binSize", ctx) && !positive_long(&bin) {
        return Err(fail(
            7157940,
            "$dateTrunc parameter 'binSize' must be coercible to a positive 64-bit integer",
        ));
    }
    if n.has("startOfWeek") && !n.constant("startOfWeek", ctx) && week {
        if !matches!(sow, Bson::String(_)) {
            return Err(fail(
                7157943,
                "$dateTrunc parameter 'startOfWeek' must be a string",
            ));
        }
        if !day_of_week_valid(&sow) {
            return Err(fail(
                7157944,
                "$dateTrunc parameter 'startOfWeek' must be a valid day of the week",
            ));
        }
    }
    // Constant arguments were checked by the classic parser.
    n.classic("$dateTrunc", ctx)
}

fn set_op(op: &str, arg: &Bson, ctx: &Ctx) -> R {
    // The optimizer's checks come before SBE's.
    super::set_optimize_check(op, arg, ctx)?;
    let args = operands(arg, ctx)?;
    let all_arrays = args.iter().all(|a| matches!(a, Bson::Array(_)));
    if matches!(op, "$setEquals" | "$setIsSubset") {
        if !all_arrays {
            return Err(fail(
                7158100,
                format!("All operands of {op} must be arrays."),
            ));
        }
        return classic(op, args, ctx);
    }
    if args.iter().any(nullish) {
        return Ok(Bson::Null);
    }
    if !all_arrays {
        return Err(fail(
            7158101,
            format!("All operands of {op} must be arrays."),
        ));
    }
    classic(op, args, ctx)
}

fn trim(op: &str, arg: &Bson, ctx: &Ctx) -> R {
    let Bson::Document(spec) = arg else {
        return apply_classic(op, arg, ctx);
    };
    let input = eval(spec.get("input").unwrap_or(&Bson::Null), ctx)?;
    let chars = match spec.get("chars") {
        Some(c) => Some(eval(c, ctx)?),
        None => None,
    };
    if nullish(&input) {
        return Ok(Bson::Null);
    }
    if !matches!(input, Bson::String(_)) {
        return Err(fail(
            5156302,
            format!("{op} input expression must be a string"),
        ));
    }
    match &chars {
        Some(c) if nullish(c) => return Ok(Bson::Null),
        Some(Bson::String(_)) | None => {}
        Some(_) => {
            return Err(fail(
                5156303,
                format!("{op} chars expression must be a string if provided"),
            ))
        }
    }
    let mut lit = doc! { "input": input };
    if let Some(c) = chars {
        lit.insert("chars", c);
    }
    classic_named(op, lit, ctx)
}
