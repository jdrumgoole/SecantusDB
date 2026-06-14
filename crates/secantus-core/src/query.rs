//! Query matching — Rust port of `secantus.query.matches` (the field-level and
//! document-level operators), the second leaf engine of the rewrite.
//!
//! Design: faithful where it's cheap to be, **fall back to Python otherwise**.
//! Any construct this module can't reproduce byte-for-byte signals `Fallback`,
//! which the Python shim turns into "run the pure-Python matcher instead". That
//! keeps the port strictly correct: the operators handled here match the Python
//! implementation exactly (pinned by `tests/test_rust_query_parity.py`), and
//! everything else (`$jsonSchema`, geo, structural/compound equality, exotic
//! BSON types) defers to Python. `$expr` is handled via the Rust expression
//! evaluator; `$all` via its Python-`==`. A `collation` is threaded through
//! string comparisons and handled for the ASCII-safe cases (see
//! `crate::collation`), deferring non-ASCII / `numericOrdering` to Python.
//!
//! **Regex** (`$regex` + `$options`, and a bare BSON `RegularExpression` value)
//! is matched with the `regex` crate (`re.search` semantics → unanchored
//! `is_match`; options `i`/`m`/`s`/`x` map to the builder flags; other flag
//! chars are ignored, mirroring Python's `_re_flags`). Patterns the crate can't
//! compile — backreferences, lookaround, `\Z`, etc. — signal `Fallback` (defer
//! to Python `re`), as do patterns over the 1000-char cap. Known divergence
//! from Python/PCRE: the crate's `$` matches only the end of the haystack, not
//! before a trailing `\n` (backlog §7).

use std::cmp::Ordering;

use bson::{Bson, Document};
use regex::RegexBuilder;

use crate::collation::{self, Collation};
use crate::{expressions, numeric};

/// Signal that the pure-Python matcher must handle this query/value.
#[derive(Debug)]
pub struct Fallback;

type R = Result<bool, Fallback>;

/// Entry point. `Ok(b)` is the match result; `Err(Fallback)` means defer to
/// Python (the query uses something not ported yet). `vars` carries user vars
/// for `$expr`; `coll` is the active collation (or `None`).
pub fn matches(doc: &Document, query: &Document, vars: &Document, coll: Option<&Collation>) -> R {
    for (k, v) in query.iter() {
        if !match_clause(doc, k, v, vars, coll)? {
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

fn match_clause(
    doc: &Document,
    key: &str,
    cond: &Bson,
    vars: &Document,
    coll: Option<&Collation>,
) -> R {
    match key {
        "$and" => {
            let arr = cond.as_array().ok_or(Fallback)?;
            for c in arr {
                if !matches(doc, as_doc(c)?, vars, coll)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "$or" => {
            let arr = cond.as_array().ok_or(Fallback)?;
            for c in arr {
                if matches(doc, as_doc(c)?, vars, coll)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "$nor" => {
            let arr = cond.as_array().ok_or(Fallback)?;
            for c in arr {
                if matches(doc, as_doc(c)?, vars, coll)? {
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
        _ => field_matches(&resolve_path(doc, key), cond, coll),
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

fn field_matches(values: &[Option<Bson>], cond: &Bson, coll: Option<&Collation>) -> R {
    match cond {
        // A bare BSON regex literal: `{field: /pat/flags}` matches as a pattern.
        Bson::RegularExpression(_) => op_regex(values, cond, None),
        Bson::Document(d) if is_operator_dict(d) => {
            for (op, arg) in d.iter() {
                match op.as_str() {
                    // `$options` is a sibling modifier of `$regex`, consumed below.
                    "$options" if d.contains_key("$regex") => continue,
                    "$regex" => {
                        if !op_regex(values, arg, d.get("$options"))? {
                            return Ok(false);
                        }
                    }
                    _ => {
                        if !op_matches(values, op, arg, coll)? {
                            return Ok(false);
                        }
                    }
                }
            }
            Ok(true)
        }
        _ => eq_with_array(values, cond, coll),
    }
}

/// Hard cap on user-supplied regex pattern length, mirroring
/// `secantus.query._MAX_REGEX_PATTERN_LEN`. Over the cap, defer to Python
/// (which raises a `QueryError`).
const MAX_REGEX_PATTERN_LEN: usize = 1000;

/// `$regex` / bare-regex matching, mirroring `secantus.query._op_regex`:
/// `re.search` over each string value (and over string elements of array
/// values). `pattern` is a `String` or a BSON `RegularExpression`; `options`
/// is the optional sibling `$options` string. Anything the `regex` crate can't
/// compile, or a non-string pattern/options, signals `Fallback`.
fn op_regex(values: &[Option<Bson>], pattern: &Bson, options: Option<&Bson>) -> R {
    let re = build_regex(pattern, options)?;
    for v in values {
        match v {
            Some(Bson::String(s)) if re.is_match(s) => return Ok(true),
            Some(Bson::Array(arr)) => {
                for e in arr {
                    if let Bson::String(s) = e {
                        if re.is_match(s) {
                            return Ok(true);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

fn build_regex(pattern: &Bson, options: Option<&Bson>) -> Result<regex::Regex, Fallback> {
    let (pat, embedded_flags): (&str, &str) = match pattern {
        Bson::String(s) => (s.as_str(), ""),
        Bson::RegularExpression(r) => (r.pattern.as_str(), r.options.as_str()),
        _ => return Err(Fallback),
    };
    let opt_flags: &str = match options {
        None => "",
        Some(Bson::String(s)) => s.as_str(),
        Some(_) => return Err(Fallback),
    };
    if pat.len() > MAX_REGEX_PATTERN_LEN {
        return Err(Fallback);
    }
    // i/m/s/x map to builder flags; any other flag char is ignored, mirroring
    // Python's `_MONGO_FLAG_MAP.get(c, 0)`.
    let (mut ci, mut ml, mut dotall, mut ext) = (false, false, false, false);
    for c in embedded_flags.chars().chain(opt_flags.chars()) {
        match c {
            'i' => ci = true,
            'm' => ml = true,
            's' => dotall = true,
            'x' => ext = true,
            _ => {}
        }
    }
    RegexBuilder::new(pat)
        .case_insensitive(ci)
        .multi_line(ml)
        .dot_matches_new_line(dotall)
        .ignore_whitespace(ext)
        .build()
        .map_err(|_| Fallback)
}

fn op_matches(values: &[Option<Bson>], op: &str, arg: &Bson, coll: Option<&Collation>) -> R {
    match op {
        "$eq" => eq_with_array(values, arg, coll),
        "$ne" => Ok(!eq_with_array(values, arg, coll)?),
        "$gt" => cmp_op(values, arg, coll, |o| o == Ordering::Greater),
        "$gte" => cmp_op(values, arg, coll, |o| o != Ordering::Less),
        "$lt" => cmp_op(values, arg, coll, |o| o == Ordering::Less),
        "$lte" => cmp_op(values, arg, coll, |o| o != Ordering::Greater),
        "$in" => {
            let arr = arg.as_array().ok_or(Fallback)?;
            for cand in arr {
                if eq_with_array(values, cand, coll)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "$nin" => {
            let arr = arg.as_array().ok_or(Fallback)?;
            for cand in arr {
                if eq_with_array(values, cand, coll)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "$exists" => {
            let present = values.iter().any(|v| v.is_some());
            Ok(present == truthy(arg)?)
        }
        "$not" => Ok(!field_matches(values, arg, coll)?),
        "$type" => op_type(values, arg),
        "$size" => op_size(values, arg),
        "$all" => op_all(values, arg),
        "$elemMatch" => op_elem_match(values, arg),
        "$mod" => op_mod(values, arg),
        "$bitsAllSet" => op_bits(values, arg, |v, m| v & m == m),
        "$bitsAnySet" => op_bits(values, arg, |v, m| v & m != 0),
        "$bitsAllClear" => op_bits(values, arg, |v, m| v & m == 0),
        "$bitsAnyClear" => op_bits(values, arg, |v, m| v & m != m),
        "$geoWithin" => crate::geo::op_geo_within(values, arg),
        "$geoIntersects" => crate::geo::op_geo_intersects(values, arg),
        "$near" => crate::geo::op_geo_near(values, arg, false),
        "$nearSphere" => crate::geo::op_geo_near(values, arg, true),
        // $center (Shapely 64-gon) and anything unknown -> Python (Python raises
        // QueryError for genuinely-unknown operators). $regex/$options are
        // intercepted in `field_matches` (they share a condition dict).
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

fn eq_with_array(values: &[Option<Bson>], expected: &Bson, coll: Option<&Collation>) -> R {
    for v in values {
        match v {
            None => {
                if matches!(expected, Bson::Null) {
                    return Ok(true);
                }
            }
            Some(val) => {
                if eq_scalar(val, expected, coll)? {
                    return Ok(true);
                }
                if let Bson::Array(arr) = val {
                    for e in arr {
                        if eq_scalar(e, expected, coll)? {
                            return Ok(true);
                        }
                    }
                }
            }
        }
    }
    Ok(false)
}

fn eq_scalar(v: &Bson, expected: &Bson, coll: Option<&Collation>) -> R {
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
    // Collation-aware string equality (defers to Python on non-ASCII /
    // numericOrdering); without a collation, plain byte equality.
    if let (Bson::String(a), Bson::String(b)) = (v, expected) {
        return match coll {
            Some(c) => collation::equal(a, b, c).ok_or(Fallback),
            None => Ok(a == b),
        };
    }
    Ok(match (v, expected) {
        (Bson::Null, Bson::Null) => true,
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

fn cmp_op(
    values: &[Option<Bson>],
    target: &Bson,
    coll: Option<&Collation>,
    pred: fn(Ordering) -> bool,
) -> R {
    for v in values {
        let Some(val) = v else { continue };
        if let Some(o) = compare_values(val, target, coll)? {
            if pred(o) {
                return Ok(true);
            }
        }
        if let Bson::Array(arr) = val {
            for e in arr {
                if let Some(o) = compare_values(e, target, coll)? {
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
fn compare_values(
    a: &Bson,
    b: &Bson,
    coll: Option<&Collation>,
) -> Result<Option<Ordering>, Fallback> {
    // Python compares bool as int (bool is an int subclass) for $gt/$lt; rather
    // than reproduce that quirk, defer any bool operand to Python.
    if matches!(a, Bson::Boolean(_)) || matches!(b, Bson::Boolean(_)) {
        return Err(Fallback);
    }
    if let (Some(na), Some(nb)) = (numeric::classify(a), numeric::classify(b)) {
        return Ok(numeric::cmp(&na, &nb));
    }
    // Collation-aware string ordering (defers on non-ASCII / numericOrdering).
    if let (Some(c), Bson::String(x), Bson::String(y)) = (coll, a, b) {
        return Ok(Some(collation::compare(x, y, c).ok_or(Fallback)?));
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

/// `$all`: some array value contains, for every required element, a matching
/// element. Element equality uses Python `==` (`expressions::py_eq` — numeric
/// bridge + bool-as-int), matching `secantus.query._op_all`. Regex elements
/// (which Python matches as patterns) defer to Python.
fn op_all(values: &[Option<Bson>], required: &Bson) -> R {
    let Bson::Array(required) = required else {
        return Err(Fallback); // Python raises QueryError on a non-array $all
    };
    if required
        .iter()
        .any(|r| matches!(r, Bson::RegularExpression(_)))
    {
        return Err(Fallback);
    }
    for v in values {
        let Some(Bson::Array(arr)) = v else { continue };
        let mut all_present = true;
        for r in required {
            let mut found = false;
            for e in arr {
                if expressions::py_eq(e, r).map_err(|_| Fallback)? {
                    found = true;
                    break;
                }
            }
            if !found {
                all_present = false;
                break;
            }
        }
        if all_present {
            return Ok(true);
        }
    }
    Ok(false)
}

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
                // Python's $elemMatch passes no collation to the inner match.
                if field_matches(&[Some(elem.clone())], cond, None)? {
                    return Ok(true);
                }
            } else if let Bson::Document(ed) = elem {
                // Python's $elemMatch recurses with no vars and no collation.
                if matches(ed, condd, &Document::new(), None)? {
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
        matches(&doc, &query, &Document::new(), None).expect("should not fall back")
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

    fn re(pattern: &str, options: &str) -> Bson {
        Bson::RegularExpression(bson::Regex {
            pattern: pattern.into(),
            options: options.into(),
        })
    }

    #[test]
    fn regex_dollar_operator() {
        // $regex string form, unanchored search.
        assert!(m(doc! {"item": "paper"}, doc! {"item": {"$regex": "^p"}}));
        assert!(!m(
            doc! {"item": "journal"},
            doc! {"item": {"$regex": "^p"}}
        ));
        assert!(m(
            doc! {"item": "abc123"},
            doc! {"item": {"$regex": "[0-9]+"}}
        ));
        // $options: i (case-insensitive), s (dotall), m (multiline).
        assert!(m(
            doc! {"item": "Paper"},
            doc! {"item": {"$regex": "^p", "$options": "i"}}
        ));
        assert!(m(
            doc! {"x": "a\nb"},
            doc! {"x": {"$regex": "^b", "$options": "m"}}
        ));
        assert!(m(
            doc! {"x": "foobar"},
            doc! {"x": {"$regex": "o.b", "$options": "s"}}
        ));
        // array element matches.
        assert!(m(
            doc! {"tags": ["red", "blank"]},
            doc! {"tags": {"$regex": "^bl"}}
        ));
        assert!(!m(
            doc! {"tags": ["red", "blank"]},
            doc! {"tags": {"$regex": "^z"}}
        ));
    }

    #[test]
    fn regex_bare_literal() {
        assert!(m(doc! {"x": "hello"}, doc! {"x": re("^h", "")}));
        assert!(m(doc! {"x": "Hello"}, doc! {"x": re("^h", "i")}));
        assert!(!m(doc! {"x": "Hello"}, doc! {"x": re("^h", "")}));
    }

    #[test]
    fn regex_uncompilable_defers() {
        // A backreference is unsupported by the `regex` crate -> Fallback.
        assert!(matches(
            &doc! {"x": "aa"},
            &doc! {"x": {"$regex": r"(a)\1"}},
            &Document::new(),
            None,
        )
        .is_err());
    }

    #[test]
    fn collation_ascii_case_insensitive() {
        let coll = Collation {
            strength: 2,
            case_level: false,
            numeric_ordering: false,
        };
        assert!(matches(
            &doc! {"n": "PING"},
            &doc! {"n": "ping"},
            &Document::new(),
            Some(&coll)
        )
        .unwrap());
        // non-ASCII under a case-insensitive collation defers to Python
        assert!(matches(
            &doc! {"n": "café"},
            &doc! {"n": "CAFÉ"},
            &Document::new(),
            Some(&coll)
        )
        .is_err());
    }
}
