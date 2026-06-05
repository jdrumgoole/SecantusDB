//! Query matching — Rust port of `secantus.query.matches` (the field-level and
//! document-level operators), the second leaf engine of the rewrite.
//!
//! Design: faithful where it's cheap to be, **fall back to Python otherwise**.
//! Any construct this module can't reproduce byte-for-byte signals `Fallback`,
//! which the Python shim turns into "run the pure-Python matcher instead". That
//! keeps the port strictly correct: the operators handled here match the Python
//! implementation exactly (pinned by `tests/test_rust_query_parity.py`), and
//! everything else (collation, `$expr`, `$jsonSchema`, geo, any regex, `$all`,
//! structural/compound equality, exotic BSON types) defers to Python.

use std::cmp::Ordering;

use bson::{Bson, Document};

use crate::{expressions, numeric};

/// Signal that the pure-Python matcher must handle this query/value.
#[derive(Debug)]
pub struct Fallback;

type R = Result<bool, Fallback>;

/// Entry point. `Ok(b)` is the match result; `Err(Fallback)` means defer to
/// Python (the query uses something not ported yet). `vars` carries user vars
/// for `$expr` (`$$name` / `$$ROOT`).
pub fn matches(doc: &Document, query: &Document, vars: &Document) -> R {
    for (k, v) in query.iter() {
        if !match_clause(doc, k, v, vars)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn as_doc(b: &Bson) -> Result<&Document, Fallback> {
    match b {
        Bson::Document(d) => Ok(d),
        _ => Err(Fallback),
    }
}

fn match_clause(doc: &Document, key: &str, cond: &Bson, vars: &Document) -> R {
    match key {
        "$and" => {
            let arr = cond.as_array().ok_or(Fallback)?;
            for c in arr {
                if !matches(doc, as_doc(c)?, vars)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "$or" => {
            let arr = cond.as_array().ok_or(Fallback)?;
            for c in arr {
                if matches(doc, as_doc(c)?, vars)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "$nor" => {
            let arr = cond.as_array().ok_or(Fallback)?;
            for c in arr {
                if matches(doc, as_doc(c)?, vars)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "$comment" => Ok(true),
        "$expr" => {
            // _truthy(evaluate(cond, doc, vars)); the evaluator defers (and so
            // do we) for any expression operator it doesn't yet handle.
            let value = expressions::evaluate(doc, cond, vars).map_err(|_| Fallback)?;
            Ok(expressions::truthy(&value))
        }
        // $jsonSchema, $where, $text, ... -> Python.
        _ if key.starts_with('$') => Err(Fallback),
        _ => field_matches(&resolve_path(doc, key), cond),
    }
}

/// Resolve a dotted path into the list of values it reaches, mirroring
/// `secantus.query._resolve_path`: walks into maps and arrays, fans out over
/// array elements for non-index path parts, and yields `None` for MISSING.
fn resolve_path(doc: &Document, path: &str) -> Vec<Option<Bson>> {
    let mut current: Vec<Option<Bson>> = vec![Some(Bson::Document(doc.clone()))];
    for part in path.split('.') {
        let mut nxt: Vec<Option<Bson>> = Vec::new();
        for cur in &current {
            match cur {
                Some(Bson::Document(d)) => nxt.push(d.get(part).cloned()),
                Some(Bson::Array(arr)) => {
                    if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
                        let idx: Result<usize, _> = part.parse();
                        nxt.push(idx.ok().and_then(|i| arr.get(i)).cloned());
                    } else {
                        for elem in arr {
                            if let Bson::Document(ed) = elem {
                                nxt.push(ed.get(part).cloned());
                            }
                        }
                    }
                }
                _ => nxt.push(None),
            }
        }
        current = nxt;
    }
    current
}

fn is_operator_dict(d: &Document) -> bool {
    !d.is_empty() && d.keys().all(|k| k.starts_with('$'))
}

fn field_matches(values: &[Option<Bson>], cond: &Bson) -> R {
    match cond {
        Bson::RegularExpression(_) => Err(Fallback), // regex semantics -> Python `re`
        Bson::Document(d) if is_operator_dict(d) => {
            for (op, arg) in d.iter() {
                if !op_matches(values, op, arg)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => eq_with_array(values, cond),
    }
}

fn op_matches(values: &[Option<Bson>], op: &str, arg: &Bson) -> R {
    match op {
        "$eq" => eq_with_array(values, arg),
        "$ne" => Ok(!eq_with_array(values, arg)?),
        "$gt" => cmp_op(values, arg, |o| o == Ordering::Greater),
        "$gte" => cmp_op(values, arg, |o| o != Ordering::Less),
        "$lt" => cmp_op(values, arg, |o| o == Ordering::Less),
        "$lte" => cmp_op(values, arg, |o| o != Ordering::Greater),
        "$in" => {
            let arr = arg.as_array().ok_or(Fallback)?;
            for cand in arr {
                if eq_with_array(values, cand)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "$nin" => {
            let arr = arg.as_array().ok_or(Fallback)?;
            for cand in arr {
                if eq_with_array(values, cand)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "$exists" => {
            let present = values.iter().any(|v| v.is_some());
            Ok(present == truthy(arg)?)
        }
        "$not" => Ok(!field_matches(values, arg)?),
        "$type" => op_type(values, arg),
        "$size" => op_size(values, arg),
        "$elemMatch" => op_elem_match(values, arg),
        "$mod" => op_mod(values, arg),
        "$bitsAllSet" => op_bits(values, arg, |v, m| v & m == m),
        "$bitsAnySet" => op_bits(values, arg, |v, m| v & m != 0),
        "$bitsAllClear" => op_bits(values, arg, |v, m| v & m == 0),
        "$bitsAnyClear" => op_bits(values, arg, |v, m| v & m != m),
        // $all, $regex/$options, geo, and anything unknown -> Python (Python
        // raises QueryError for genuinely-unknown operators, preserved).
        _ => Err(Fallback),
    }
}

// --- equality -----------------------------------------------------------

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

fn eq_with_array(values: &[Option<Bson>], expected: &Bson) -> R {
    for v in values {
        match v {
            None => {
                if matches!(expected, Bson::Null) {
                    return Ok(true);
                }
            }
            Some(val) => {
                if eq_scalar(val, expected)? {
                    return Ok(true);
                }
                if let Bson::Array(arr) = val {
                    for e in arr {
                        if eq_scalar(e, expected)? {
                            return Ok(true);
                        }
                    }
                }
            }
        }
    }
    Ok(false)
}

fn eq_scalar(v: &Bson, expected: &Bson) -> R {
    // Compound / regex / exotic expected -> structural or special semantics we
    // don't reproduce: defer to Python.
    if matches!(
        expected,
        Bson::RegularExpression(_) | Bson::Document(_) | Bson::Array(_)
    ) || is_exotic(expected)
    {
        return Err(Fallback);
    }
    let v_bool = matches!(v, Bson::Boolean(_));
    let e_bool = matches!(expected, Bson::Boolean(_));
    if v_bool != e_bool {
        return Ok(false);
    }
    if let (Bson::Boolean(a), Bson::Boolean(b)) = (v, expected) {
        return Ok(a == b);
    }
    if let (Some(na), Some(nb)) = (numeric::classify(v), numeric::classify(expected)) {
        return Ok(numeric::eq(&na, &nb));
    }
    Ok(match (v, expected) {
        (Bson::Null, Bson::Null) => true,
        (Bson::String(a), Bson::String(b)) => a == b,
        (Bson::ObjectId(a), Bson::ObjectId(b)) => a == b,
        (Bson::DateTime(a), Bson::DateTime(b)) => a == b,
        (Bson::Timestamp(a), Bson::Timestamp(b)) => a == b,
        (Bson::Binary(a), Bson::Binary(b)) => a.subtype == b.subtype && a.bytes == b.bytes,
        (Bson::MinKey, Bson::MinKey) => true,
        (Bson::MaxKey, Bson::MaxKey) => true,
        _ => false,
    })
}

// --- comparison ---------------------------------------------------------

fn cmp_op(values: &[Option<Bson>], target: &Bson, pred: fn(Ordering) -> bool) -> R {
    for v in values {
        let Some(val) = v else { continue };
        if let Some(o) = compare_values(val, target)? {
            if pred(o) {
                return Ok(true);
            }
        }
        if let Bson::Array(arr) = val {
            for e in arr {
                if let Some(o) = compare_values(e, target)? {
                    if pred(o) {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(false)
}

/// Ordering between two values, or `None` when not comparable (Python's
/// comparison raises `TypeError`, which the matcher treats as no-match).
/// `Err(Fallback)` for cases whose Python semantics we don't reproduce (bool
/// participating as int, structural array/doc ordering, exotic types).
fn compare_values(a: &Bson, b: &Bson) -> Result<Option<Ordering>, Fallback> {
    // Python compares bool as int (bool is an int subclass) for $gt/$lt; rather
    // than reproduce that quirk, defer any bool operand to Python.
    if matches!(a, Bson::Boolean(_)) || matches!(b, Bson::Boolean(_)) {
        return Err(Fallback);
    }
    if let (Some(na), Some(nb)) = (numeric::classify(a), numeric::classify(b)) {
        return Ok(numeric::cmp(&na, &nb));
    }
    if matches!(a, Bson::Array(_) | Bson::Document(_))
        || matches!(b, Bson::Array(_) | Bson::Document(_))
        || is_exotic(a)
        || is_exotic(b)
    {
        return Err(Fallback);
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
        (Bson::Binary(x), Bson::Binary(y)) => Some(x.bytes.cmp(&y.bytes)),
        // Different (non-numeric) types or null-vs-null: not comparable.
        _ => None,
    })
}

// --- $exists / truthiness ----------------------------------------------

fn truthy(arg: &Bson) -> Result<bool, Fallback> {
    Ok(match arg {
        Bson::Boolean(b) => *b,
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Double(d) => *d != 0.0, // NaN -> true (matches Python bool(nan))
        Bson::Null => false,
        Bson::String(s) => !s.is_empty(),
        Bson::Array(a) => !a.is_empty(),
        Bson::Document(d) => !d.is_empty(),
        // Decimal128 truthiness in Python keys on object identity (always
        // true), but $exists: Decimal128(0) is pathological — defer.
        Bson::Decimal128(_) => return Err(Fallback),
        _ => true,
    })
}

// --- $type --------------------------------------------------------------

fn matches_type(v: &Bson, spec: &Bson) -> bool {
    let alias: Option<&str> = match spec {
        Bson::String(s) => Some(s.as_str()),
        _ => None,
    };
    let code: Option<i64> = match spec {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        _ => None,
    };
    let is = |names: &[&str], codes: &[i64], hit: bool| -> bool {
        hit && (alias.map(|a| names.contains(&a)).unwrap_or(false)
            || code.map(|c| codes.contains(&c)).unwrap_or(false))
    };
    is(&["double"], &[1], matches!(v, Bson::Double(_)))
        || is(&["string"], &[2], matches!(v, Bson::String(_)))
        || is(&["object"], &[3], matches!(v, Bson::Document(_)))
        || is(&["array"], &[4], matches!(v, Bson::Array(_)))
        || is(&["binData"], &[5], matches!(v, Bson::Binary(_)))
        || is(&["objectId"], &[7], matches!(v, Bson::ObjectId(_)))
        || is(&["bool"], &[8], matches!(v, Bson::Boolean(_)))
        || is(&["date"], &[9], matches!(v, Bson::DateTime(_)))
        || is(&["null"], &[10], matches!(v, Bson::Null))
        || is(&["regex"], &[11], matches!(v, Bson::RegularExpression(_)))
        || is(&["int"], &[16], matches!(v, Bson::Int32(_)))
        || is(&["long"], &[18], matches!(v, Bson::Int64(_)))
        || is(&["decimal"], &[19], matches!(v, Bson::Decimal128(_)))
        || is(
            &["number"],
            &[],
            matches!(
                v,
                Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
            ),
        )
}

fn op_type(values: &[Option<Bson>], spec: &Bson) -> R {
    // spec is a single alias/code or an array of them. A non-alias/code spec
    // element (e.g. a float code) is pathological -> Python.
    let specs: Vec<&Bson> = match spec {
        Bson::Array(a) => a.iter().collect(),
        single => vec![single],
    };
    for s in &specs {
        if !matches!(s, Bson::String(_) | Bson::Int32(_) | Bson::Int64(_)) {
            return Err(Fallback);
        }
    }
    for v in values {
        let Some(val) = v else { continue };
        if specs.iter().any(|s| matches_type(val, s)) {
            return Ok(true);
        }
        if let Bson::Array(arr) = val {
            for e in arr {
                if specs.iter().any(|s| matches_type(e, s)) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

// --- $size --------------------------------------------------------------

fn op_size(values: &[Option<Bson>], size: &Bson) -> R {
    let n = match size {
        Bson::Int32(n) => *n as i64,
        Bson::Int64(n) => *n,
        _ => return Err(Fallback), // Python raises QueryError -> Python
    };
    if n < 0 {
        return Ok(false);
    }
    let n = n as usize;
    for v in values {
        if let Some(Bson::Array(arr)) = v {
            if arr.len() == n {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// --- $elemMatch ---------------------------------------------------------

fn op_elem_match(values: &[Option<Bson>], cond: &Bson) -> R {
    let Bson::Document(condd) = cond else {
        return Ok(false); // Python: non-mapping condition -> False
    };
    let scalar_form = is_operator_dict(condd);
    for v in values {
        let Some(Bson::Array(arr)) = v else { continue };
        for elem in arr {
            if scalar_form {
                if field_matches(&[Some(elem.clone())], cond)? {
                    return Ok(true);
                }
            } else if let Bson::Document(ed) = elem {
                // Python's $elemMatch recurses with no vars (empty scope).
                if matches(ed, condd, &Document::new())? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

// --- $mod ---------------------------------------------------------------

fn as_int(b: &Bson) -> Option<i64> {
    match b {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        _ => None,
    }
}

fn op_mod(values: &[Option<Bson>], spec: &Bson) -> R {
    let arr = spec.as_array().ok_or(Fallback)?;
    if arr.len() != 2 {
        return Err(Fallback);
    }
    let (Some(div), Some(rem)) = (as_int(&arr[0]), as_int(&arr[1])) else {
        return Err(Fallback); // float divisor/remainder -> Python
    };
    if div <= 0 {
        return Err(Fallback); // div==0 / negative modulo semantics -> Python
    }
    let check = |val: &Bson| -> Result<Option<bool>, Fallback> {
        match val {
            Bson::Int32(_) | Bson::Int64(_) => {
                let n = as_int(val).unwrap();
                Ok(Some(n.rem_euclid(div) == rem))
            }
            // Python computes `bool % div` (bool is an int subclass): True->1,
            // False->0, so a bool value DOES participate in $mod.
            Bson::Boolean(b) => Ok(Some(i64::from(*b).rem_euclid(div) == rem)),
            // Python would compute float/decimal mod; we don't -> Python.
            Bson::Double(_) | Bson::Decimal128(_) => Err(Fallback),
            _ => Ok(None), // non-numeric: Python's `v % div` raises -> no match
        }
    };
    for v in values {
        let Some(val) = v else { continue };
        if let Some(true) = check(val)? {
            return Ok(true);
        }
        if let Bson::Array(arr) = val {
            for e in arr {
                if let Some(true) = check(e)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

// --- $bits* -------------------------------------------------------------

fn resolve_bitmask(arg: &Bson) -> Result<u64, Fallback> {
    match arg {
        Bson::Boolean(_) => Err(Fallback),
        Bson::Int32(n) if *n >= 0 => Ok(*n as u64),
        Bson::Int64(n) if *n >= 0 => Ok(*n as u64),
        Bson::Int32(_) | Bson::Int64(_) => Err(Fallback), // negative mask -> Python
        Bson::Array(a) => {
            let mut mask = 0u64;
            for bit in a {
                match bit {
                    Bson::Int32(p) if (0..64).contains(p) => mask |= 1 << *p,
                    Bson::Int64(p) if (0..64).contains(p) => mask |= 1 << *p,
                    _ => return Err(Fallback),
                }
            }
            Ok(mask)
        }
        _ => Err(Fallback),
    }
}

fn op_bits(values: &[Option<Bson>], arg: &Bson, pred: fn(u64, u64) -> bool) -> R {
    let mask = resolve_bitmask(arg)?;
    for v in values {
        let val = match v {
            Some(Bson::Int32(n)) => *n as i64,
            Some(Bson::Int64(n)) => *n,
            _ => continue, // bool/non-int values are skipped (matches Python)
        };
        if val < 0 {
            return Err(Fallback); // two's-complement-infinite semantics -> Python
        }
        if pred(val as u64, mask) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn m(doc: Document, query: Document) -> bool {
        matches(&doc, &query, &Document::new()).expect("should not fall back")
    }

    #[test]
    fn equality_and_paths() {
        assert!(m(doc! {"a": 1}, doc! {"a": 1}));
        assert!(!m(doc! {"a": 1}, doc! {"a": 2}));
        assert!(m(doc! {"a": {"b": {"c": 5}}}, doc! {"a.b.c": 5}));
        assert!(m(doc! {"tags": ["red", "blue"]}, doc! {"tags": "red"}));
        assert!(m(doc! {"vals": [10, 20, 30]}, doc! {"vals.1": 20}));
    }

    #[test]
    fn comparison_and_in() {
        assert!(m(doc! {"age": 30}, doc! {"age": {"$gt": 20}}));
        assert!(!m(doc! {"age": 30}, doc! {"age": {"$lt": 30}}));
        assert!(m(doc! {"a": 2}, doc! {"a": {"$in": [1, 2, 3]}}));
        assert!(m(doc! {"a": 4}, doc! {"a": {"$nin": [1, 2, 3]}}));
    }

    #[test]
    fn null_and_exists() {
        assert!(m(doc! {}, doc! {"a": Bson::Null}));
        assert!(m(doc! {"a": Bson::Null}, doc! {"a": {"$exists": true}}));
        assert!(m(doc! {}, doc! {"a": {"$exists": false}}));
    }

    #[test]
    fn bool_distinct_from_int() {
        assert!(!m(doc! {"x": true}, doc! {"x": 1}));
        assert!(!m(doc! {"x": 1}, doc! {"x": true}));
    }

    #[test]
    fn expr_now_handled() {
        // $expr with supported operators is handled in Rust now (was a fallback).
        assert!(m(
            doc! {"a": 5, "b": 3},
            doc! {"$expr": {"$gt": ["$a", "$b"]}}
        ));
        assert!(!m(
            doc! {"a": 1, "b": 3},
            doc! {"$expr": {"$gt": ["$a", "$b"]}}
        ));
    }

    #[test]
    fn regex_falls_back() {
        assert!(matches(
            &doc! {"n": "x"},
            &doc! {"n": Bson::RegularExpression(bson::Regex {
            pattern: "x".into(), options: "".into() })},
            &Document::new()
        )
        .is_err());
    }
}
