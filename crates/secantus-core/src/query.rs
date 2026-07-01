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
//! is matched with `re.search` semantics (unanchored `is_match`; options
//! `i`/`m`/`s`/`x` map to flags; other flag chars are ignored, mirroring
//! Python's `_re_flags`). The linear `regex` crate is tried first (the fast
//! path for almost every pattern); patterns it can't compile — backreferences,
//! lookaround (e.g. the `^(?!system\.)` pymongo emits for listCollections), etc.
//! — fall back to the backtracking `fancy-regex`. Only patterns neither engine
//! compiles, or those over the 1000-char cap, signal `Fallback` (defer to
//! Python `re`). Known divergence from Python/PCRE on the linear path: the
//! crate's `$` matches only the end of the haystack, not before a trailing `\n`
//! (backlog §7).

use std::cmp::Ordering;

use bson::{Bson, Document};

use crate::collation::{self, Collation};
use crate::{expressions, numeric, regexutil};

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
fn resolve_path<'a>(doc: &'a Document, path: &str) -> Vec<Option<&'a Bson>> {
    // Borrow throughout — never clone the document or the values it reaches. The
    // root is always a document, so seed by resolving the first path component
    // against it directly; later components fan out over the borrowed values.
    let mut parts = path.split('.');
    let first = parts.next().unwrap_or("");
    let mut current: Vec<Option<&Bson>> = vec![doc.get(first)];
    for part in parts {
        let mut nxt: Vec<Option<&Bson>> = Vec::new();
        for cur in &current {
            match cur {
                Some(Bson::Document(d)) => nxt.push(d.get(part)),
                Some(Bson::Array(arr)) => {
                    if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
                        let idx: Result<usize, _> = part.parse();
                        nxt.push(idx.ok().and_then(|i| arr.get(i)));
                    } else {
                        for elem in arr {
                            if let Bson::Document(ed) = elem {
                                nxt.push(ed.get(part));
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

/// Field-level query operators this engine recognises (incl. the geo/regex
/// sub-operators handled specially). Used only for error messages — see
/// [`first_unknown_operator`].
const KNOWN_FIELD_OPS: &[&str] = &[
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$exists",
    "$not",
    "$type",
    "$size",
    "$all",
    "$elemMatch",
    "$mod",
    "$bitsAllSet",
    "$bitsAnySet",
    "$bitsAllClear",
    "$bitsAnyClear",
    "$geoWithin",
    "$geoIntersects",
    "$near",
    "$nearSphere",
    "$regex",
    "$options",
    "$geometry",
    "$center",
    "$centerSphere",
    "$box",
    "$polygon",
    "$minDistance",
    "$maxDistance",
];

/// The first unrecognised field-level operator in a filter (recursing through
/// `$and`/`$or`/`$nor`), e.g. `$badOperator` for `{x: {$badOperator: 1}}`. Lets
/// the command layer build mongod's "unknown operator"-style message (which the
/// drivers' error-document tests assert names the offending operator) instead of
/// a generic one. `None` when no field operator is unrecognised.
pub fn first_unknown_operator(filter: &Document) -> Option<String> {
    for (k, v) in filter.iter() {
        if k == "$and" || k == "$or" || k == "$nor" {
            if let Bson::Array(arr) = v {
                for sub in arr {
                    if let Bson::Document(d) = sub {
                        if let Some(op) = first_unknown_operator(d) {
                            return Some(op);
                        }
                    }
                }
            }
            continue;
        }
        if k.starts_with('$') {
            continue; // other document-level operators ($expr/$text/$comment/...)
        }
        if let Bson::Document(d) = v {
            if is_operator_dict(d) {
                for op in d.keys() {
                    if !KNOWN_FIELD_OPS.contains(&op.as_str()) {
                        return Some(op.clone());
                    }
                }
            }
        }
    }
    None
}

fn field_matches(values: &[Option<&Bson>], cond: &Bson, coll: Option<&Collation>) -> R {
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
                    // `$maxDistance` / `$minDistance` are sibling modifiers of the
                    // legacy 2d `$near` / `$nearSphere` form; consumed with them.
                    "$maxDistance" | "$minDistance"
                        if d.contains_key("$near") || d.contains_key("$nearSphere") =>
                    {
                        continue
                    }
                    "$near" => {
                        if !crate::geo::op_geo_near(
                            values,
                            arg,
                            d.get("$maxDistance"),
                            d.get("$minDistance"),
                            false,
                        )? {
                            return Ok(false);
                        }
                    }
                    "$nearSphere" => {
                        if !crate::geo::op_geo_near(
                            values,
                            arg,
                            d.get("$maxDistance"),
                            d.get("$minDistance"),
                            true,
                        )? {
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

/// `$regex` / bare-regex matching, mirroring `secantus.query._op_regex`:
/// `re.search` over each string value (and over string elements of array
/// values). `pattern` is a `String` or a BSON `RegularExpression`; `options`
/// is the optional sibling `$options` string. A non-string pattern/options, or
/// a pattern neither regex engine can compile, signals `Fallback`.
fn op_regex(values: &[Option<&Bson>], pattern: &Bson, options: Option<&Bson>) -> R {
    let re = regexutil::compile(pattern, options).map_err(|_| Fallback)?;
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

fn op_matches(values: &[Option<&Bson>], op: &str, arg: &Bson, coll: Option<&Collation>) -> R {
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
        // The legacy 2d *sibling* `$maxDistance`/`$minDistance` form is handled in
        // `field_matches` (it needs the parent condition dict); here (e.g. under
        // `$elemMatch`/`$not`) only the self-contained list / GeoJSON form is seen.
        "$near" => crate::geo::op_geo_near(values, arg, None, None, false),
        "$nearSphere" => crate::geo::op_geo_near(values, arg, None, None, true),
        // Anything unknown -> Python (Python raises QueryError for genuinely-
        // unknown operators). $regex/$options are intercepted in `field_matches`
        // (they share a condition dict).
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

fn eq_with_array(values: &[Option<&Bson>], expected: &Bson, coll: Option<&Collation>) -> R {
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

/// Element-wise array equality (same length, each pair `eq_scalar`-equal).
/// A `Document`/exotic element keeps `eq_scalar`'s `Fallback`, so arrays of such
/// elements still defer to Python rather than diverge.
fn array_eq(a: &[Bson], b: &[Bson], coll: Option<&Collation>) -> R {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (x, y) in a.iter().zip(b.iter()) {
        if !eq_scalar(x, y, coll)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Embedded-document equality. mongod (and the Python oracle) compare embedded
/// documents ORDER-SENSITIVELY: `{w:21,h:14}` does not match a stored `{h:14,
/// w:21}`. So compare key/value pairs pairwise in field order.
fn doc_eq(a: &Document, b: &Document, coll: Option<&Collation>) -> R {
    if a.len() != b.len() {
        return Ok(false);
    }
    for ((ka, va), (kb, vb)) in a.iter().zip(b.iter()) {
        if ka != kb || !eq_scalar(va, vb, coll)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn eq_scalar(v: &Bson, expected: &Bson, coll: Option<&Collation>) -> R {
    // Array equality: `{field: [a, b, c]}` matches when the stored value is an
    // array equal element-by-element (same length, each pair `eq_scalar`-equal).
    // The "field-is-an-array-containing-this-array" nested case is handled by the
    // caller (`eq_with_array`), which also tries each element against `expected`.
    if let Bson::Array(exp) = expected {
        return match v {
            Bson::Array(val) => array_eq(val, exp, coll),
            _ => Ok(false),
        };
    }
    // Embedded-document equality: `{field: {a: 1}}` (a non-operator subdoc)
    // matches a stored document equal field-by-field. Python compares decoded
    // dicts with `==` (key-based, order-insensitive), so mirror that here.
    if let Bson::Document(exp) = expected {
        return match v {
            Bson::Document(val) => doc_eq(val, exp, coll),
            _ => Ok(false),
        };
    }
    // Symbol / JS-Code (with or without scope) match by value — mongod compares
    // them directly (mongo-node-driver's "handles BSON type inserts" queries on
    // a Symbol / Code value). Cross-type (Symbol vs String) and ordering keep
    // deferring via the `is_exotic` checks below / in the comparison path.
    match (v, expected) {
        (Bson::Symbol(a), Bson::Symbol(b)) => return Ok(a == b),
        (Bson::JavaScriptCode(a), Bson::JavaScriptCode(b)) => return Ok(a == b),
        (Bson::JavaScriptCodeWithScope(a), Bson::JavaScriptCodeWithScope(b)) => {
            return Ok(a.code == b.code && a.scope == b.scope)
        }
        _ => {}
    }
    // Regex / exotic expected -> special semantics we don't reproduce: defer.
    if matches!(expected, Bson::RegularExpression(_)) || is_exotic(expected) {
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
    values: &[Option<&Bson>],
    target: &Bson,
    coll: Option<&Collation>,
    pred: fn(Ordering) -> bool,
) -> R {
    for v in values {
        let Some(val) = v else { continue };
        if let Bson::Array(arr) = val {
            // Multikey field: match if any element satisfies the bound. mongod
            // does NOT compare the array-as-a-whole against a scalar bound (an
            // array always out-ranks a number in BSON type order, which would
            // make every array field match), so the whole-array compare is
            // deliberately skipped here.
            for e in arr {
                if let Some(o) = compare_values(e, target, coll)? {
                    if pred(o) {
                        return Ok(true);
                    }
                }
            }
        } else if let Some(o) = compare_values(val, target, coll)? {
            if pred(o) {
                return Ok(true);
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

/// mongod's BSON type-alias string for a value — fills `consideredType` in
/// document-validation failure details (`crud::validation_failure_details`).
/// Mirrors `query.bson_type_name`. The Rust `Bson` enum is disjoint, so a
/// straight match suffices (Python orders `bool`/`Int64` ahead of `int` only
/// because they're `int` subclasses there). Unknown variants → `"object"`,
/// matching Python's default.
pub fn bson_type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Boolean(_) => "bool",
        Bson::Int64(_) => "long",
        Bson::Int32(_) => "int",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal",
        Bson::String(_) => "string",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::Binary(_) => "binData",
        Bson::Array(_) => "array",
        _ => "object",
    }
}

fn op_type(values: &[Option<&Bson>], spec: &Bson) -> R {
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
fn op_all(values: &[Option<&Bson>], required: &Bson) -> R {
    let Bson::Array(required) = required else {
        return Err(Fallback); // Python raises QueryError on a non-array $all
    };
    // A regex element matches array elements as a *pattern* (not by equality),
    // mirroring `query._op_all`; a non-regex element matches by `py_eq`. A regex
    // the engine can't compile still defers via `op_regex`.
    for v in values {
        let Some(Bson::Array(arr)) = v else { continue };
        let mut all_present = true;
        for r in required {
            let mut found = false;
            for e in arr {
                let matched = match r {
                    Bson::RegularExpression(_) => op_regex(&[Some(e)], r, None)?,
                    _ => expressions::py_eq(e, r).map_err(|_| Fallback)?,
                };
                if matched {
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

fn op_size(values: &[Option<&Bson>], size: &Bson) -> R {
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

fn op_elem_match(values: &[Option<&Bson>], cond: &Bson) -> R {
    let Bson::Document(condd) = cond else {
        return Ok(false); // Python: non-mapping condition -> False
    };
    let scalar_form = is_operator_dict(condd);
    for v in values {
        let Some(Bson::Array(arr)) = v else { continue };
        for elem in arr {
            if scalar_form {
                // Python's $elemMatch passes no collation to the inner match.
                if field_matches(&[Some(elem)], cond, None)? {
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

fn op_mod(values: &[Option<&Bson>], spec: &Bson) -> R {
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

fn op_bits(values: &[Option<&Bson>], arg: &Bson, pred: fn(u64, u64) -> bool) -> R {
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
    fn regex_lookaround_and_backref_via_fancy() {
        // Backreferences / lookaround aren't compilable by the linear `regex`
        // crate, so they fall back to fancy-regex and evaluate (no Fallback).
        let m = |doc: Document, q: Document| {
            matches(&doc, &q, &Document::new(), None).expect("fancy-regex should evaluate")
        };
        // backreference
        assert!(m(doc! {"x": "aa"}, doc! {"x": {"$regex": r"(a)\1"}}));
        assert!(!m(doc! {"x": "ab"}, doc! {"x": {"$regex": r"(a)\1"}}));
        // negative lookahead — the listCollections `^(?!system\.)` shape
        assert!(m(
            doc! {"x": "systemcoll"},
            doc! {"x": {"$regex": r"^(?!system\.)"}}
        ));
        assert!(!m(
            doc! {"x": "system.foo"},
            doc! {"x": {"$regex": r"^(?!system\.)"}}
        ));
        // lookbehind, and flags applied through the inline-prefix path
        assert!(m(
            doc! {"x": "xyzabc"},
            doc! {"x": {"$regex": r"(?<=xyz)abc"}}
        ));
        assert!(m(
            doc! {"x": "FOObar"},
            doc! {"x": {"$regex": r"foo(?!baz)", "$options": "i"}}
        ));
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
