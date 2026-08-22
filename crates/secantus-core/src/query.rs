//! Query matching — Rust port of `secantus.query.matches` (the field-level and
//! document-level operators), the second leaf engine of the rewrite.
//!
//! Design: faithful where it's cheap to be, **fall back to Python otherwise**.
//! Any construct this module can't reproduce byte-for-byte signals `Fallback`,
//! which the Python shim turns into "run the pure-Python matcher instead". That
//! keeps the port strictly correct: the operators handled here match the Python
//! implementation exactly (pinned by `tests/test_rust_query_parity.py`), and
//! everything else (geo, structural/compound equality, exotic BSON types) defers
//! to Python. `$jsonSchema` is handled for the bounded keyword subset the pure
//! server validates (`bsonType`/`type`/`enum`/numeric bounds/string length +
//! `pattern`/array + object counts + `items`/`required`/`properties`/
//! `additionalProperties`/`patternProperties`/`dependencies`/`allOf`/`anyOf`/
//! `oneOf`/`not`), deferring any shape it can't reproduce. `$expr` is handled via
//! the Rust expression
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

use bson::{Bson, Document, RawBsonRef, RawDocument};

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

/// Raw-BSON entry point: match `query` against `raw` **without** materialising
/// the whole document into an owned `Document`. Only the fields the filter
/// actually reaches are decoded (`resolve_path_raw` converts just those leaves
/// to owned `Bson`), so a selective filter over a wide document skips decoding
/// every sibling field — the scan-path materialization tax
/// (`tasks/rust-perf-findings.md`, Finding 1). Result is identical to
/// `matches(&raw.to_document(), ...)`; the parity suite pins it bool-for-bool
/// against the owned matcher. `$expr` / `$jsonSchema` need the whole document,
/// so those single clauses fall back to a full decode. Any raw-parse hiccup
/// signals `Fallback` (defer to the pure-Python matcher), never a wrong answer.
pub fn matches_raw(
    raw: &RawDocument,
    query: &Document,
    vars: &Document,
    coll: Option<&Collation>,
) -> R {
    for (k, v) in query.iter() {
        if !match_clause_raw(raw, k, v, vars, coll)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn match_clause_raw(
    raw: &RawDocument,
    key: &str,
    cond: &Bson,
    vars: &Document,
    coll: Option<&Collation>,
) -> R {
    match key {
        "$and" => {
            for c in cond.as_array().ok_or(Fallback)? {
                if !matches_raw(raw, as_doc(c)?, vars, coll)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "$or" => {
            for c in cond.as_array().ok_or(Fallback)? {
                if matches_raw(raw, as_doc(c)?, vars, coll)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "$nor" => {
            for c in cond.as_array().ok_or(Fallback)? {
                if matches_raw(raw, as_doc(c)?, vars, coll)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "$comment" => Ok(true),
        // These operators inspect the whole document, so there's nothing to save
        // by staying raw — decode once and delegate this clause to the owned
        // matcher.
        "$expr" | "$jsonSchema" => {
            let doc: Document = raw.try_into().map_err(|_| Fallback)?;
            match_clause(&doc, key, cond, vars, coll)
        }
        // $where, $text, ... -> Python.
        _ if key.starts_with('$') => Err(Fallback),
        _ => {
            let reached = resolve_path_raw(raw, key)?;
            let refs: Vec<Option<&Bson>> = reached.iter().map(Option::as_ref).collect();
            field_matches(&refs, cond, coll)
        }
    }
}

/// Raw-BSON [`resolve_path`]: the same dotted-path walk (into sub-documents by
/// key, into arrays by numeric index, fanning out over array elements for
/// non-index parts) over a [`RawDocument`], decoding **only** the reached values
/// into owned `Bson`. Returns `Fallback` on a raw-parse error so the caller
/// defers rather than guessing.
fn resolve_path_raw(raw: &RawDocument, path: &str) -> Result<Vec<Option<Bson>>, Fallback> {
    let mut parts = path.split('.');
    let first = parts.next().unwrap_or("");
    let mut current: Vec<Option<RawBsonRef>> = vec![raw.get(first).map_err(|_| Fallback)?];
    for part in parts {
        let mut nxt: Vec<Option<RawBsonRef>> = Vec::new();
        for cur in current.iter().copied() {
            match cur {
                Some(RawBsonRef::Document(d)) => nxt.push(d.get(part).map_err(|_| Fallback)?),
                Some(RawBsonRef::Array(arr)) => {
                    if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
                        let idx: usize = part.parse().map_err(|_| Fallback)?;
                        nxt.push(arr.get(idx).map_err(|_| Fallback)?);
                    } else {
                        for elem in arr {
                            if let RawBsonRef::Document(ed) = elem.map_err(|_| Fallback)? {
                                nxt.push(ed.get(part).map_err(|_| Fallback)?);
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
        .into_iter()
        .map(|o| match o {
            Some(rbr) => Bson::try_from(rbr).map(Some).map_err(|_| Fallback),
            None => Ok(None),
        })
        .collect()
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
        "$jsonSchema" => validate_json_schema(&Bson::Document(doc.clone()), cond),
        // $where, $text, ... -> Python.
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

/// mongod's `$regex` `$options` validation: a string of only `imsxu`. A
/// non-string or an unknown flag defers so the pure engine raises the exact
/// error (BadValue "$options has to be a string" / Location51108).
fn regex_options_ok(arg: &Bson) -> Result<(), Fallback> {
    let Bson::String(s) = arg else {
        return Err(Fallback);
    };
    if s.chars().any(|c| !matches!(c, 'i' | 'm' | 's' | 'x' | 'u')) {
        return Err(Fallback);
    }
    Ok(())
}

fn field_matches(values: &[Option<&Bson>], cond: &Bson, coll: Option<&Collation>) -> R {
    match cond {
        // A bare BSON regex literal: `{field: /pat/flags}` matches as a pattern.
        Bson::RegularExpression(_) => op_regex(values, cond, None),
        Bson::Document(d) if is_operator_dict(d) => {
            // Validate the $regex/$options pair up front (mongod parse-time),
            // before any operator can short-circuit the match — mirrors the pure
            // engine. A bad flag / non-string $options / $options-without-$regex
            // defers so Python raises Location51108 / BadValue.
            if let Some(opts) = d.get("$options") {
                if !d.contains_key("$regex") {
                    return Err(Fallback);
                }
                regex_options_ok(opts)?;
            }
            for (op, arg) in d.iter() {
                match op.as_str() {
                    // `$options` is a sibling modifier of `$regex`, validated above.
                    "$options" => continue,
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
        // `$gte`/`$lte: null` match null + missing (like `$eq: null`); a plain
        // BSON-order compare against null would spuriously match everything.
        "$gte" if matches!(arg, Bson::Null) => eq_with_array(values, arg, coll),
        "$gte" => cmp_op(values, arg, coll, |o| o != Ordering::Less),
        "$lt" => cmp_op(values, arg, coll, |o| o == Ordering::Less),
        "$lte" if matches!(arg, Bson::Null) => eq_with_array(values, arg, coll),
        "$lte" => cmp_op(values, arg, coll, |o| o != Ordering::Greater),
        "$in" => {
            let arr = arg.as_array().ok_or(Fallback)?;
            in_elements_ok(arr)?;
            for cand in arr {
                if in_candidate_matches(values, cand, coll)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "$nin" => {
            let arr = arg.as_array().ok_or(Fallback)?;
            in_elements_ok(arr)?;
            for cand in arr {
                if in_candidate_matches(values, cand, coll)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "$exists" => {
            let present = values.iter().any(|v| v.is_some());
            Ok(present == truthy(arg)?)
        }
        "$not" => {
            // mongod: $not needs a regex or a non-empty document (else BadValue).
            // A scalar / array / bool / empty doc defers so Python raises.
            match arg {
                Bson::RegularExpression(_) => {}
                Bson::Document(d) if !d.is_empty() => {}
                _ => return Err(Fallback),
            }
            Ok(!field_matches(values, arg, coll)?)
        }
        "$type" => op_type(values, arg),
        "$size" => op_size(values, arg),
        "$all" => op_all(values, arg),
        "$elemMatch" => {
            // mongod: $elemMatch needs an Object (else BadValue) -> defer.
            if !matches!(arg, Bson::Document(_)) {
                return Err(Fallback);
            }
            op_elem_match(values, arg)
        }
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

/// A single `$in` / `$nin` candidate. A regex candidate matches string values by
/// pattern (mongod semantics — bare equality would silently match nothing);
/// everything else is array-aware, collation-aware equality. Mirrors
/// `query._in_candidate_matches`.
/// mongod rejects a `$in` / `$nin` element that is a document with a
/// `$`-prefixed key ("cannot nest $ under $in", BadValue) — defer so Python
/// raises it. A BSON regex literal is fine (handled in `in_candidate_matches`).
fn in_elements_ok(arr: &[Bson]) -> Result<(), Fallback> {
    for el in arr {
        if let Bson::Document(d) = el {
            if d.keys().any(|k| k.starts_with('$')) {
                return Err(Fallback);
            }
        }
    }
    Ok(())
}

fn in_candidate_matches(values: &[Option<&Bson>], cand: &Bson, coll: Option<&Collation>) -> R {
    if matches!(cand, Bson::RegularExpression(_)) {
        op_regex(values, cand, None)
    } else {
        eq_with_array(values, cand, coll)
    }
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
    // NaN equals NaN for query purposes. IEEE says otherwise, and both
    // `fast_eq` and `numeric::eq` follow IEEE — correctly, because they also
    // drive ordering, where NaN must stay incomparable. Equality is the one
    // place mongod differs: it matches `{x: NaN}` against a stored NaN (probed
    // against 6.0.16). Without this a document written with `_id: NaN` is
    // accepted and then unreachable by its own key. Mirrors
    // `query.py::_eq_numeric_aware`.
    if is_nan_bson(v) && is_nan_bson(expected) {
        return Ok(true);
    }
    if let Some(r) = numeric::fast_eq(v, expected) {
        return Ok(r);
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
        // Whole-value compare. For a scalar `val` this is the ordinary compare.
        // For an array `val`: against a *scalar* bound `compare_values` returns
        // None (Python's `[..] < 2` raises -> no match), so this is harmless; but
        // against an *array* bound it returns the lexicographic ordering, so an
        // array field vs an array bound matches (mongod / Python `list < list`).
        if let Some(o) = compare_values(val, target, coll)? {
            if pred(o) {
                return Ok(true);
            }
        }
        if let Bson::Array(arr) = val {
            // Multikey field: also match if any *element* satisfies the bound
            // (a scalar-bound query against an array-valued field). The
            // whole-array compare above covers the array-bound case.
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
/// Arrays are compared element-wise / lexicographically (mirroring Python's
/// native `list < list`); a document operand or an array-vs-scalar pair is
/// not comparable (`None`). `Err(Fallback)` only for the exotic BSON types
/// (JS code, symbol, dbpointer, undefined) whose ordering we don't reproduce.
fn compare_values(
    a: &Bson,
    b: &Bson,
    coll: Option<&Collation>,
) -> Result<Option<Ordering>, Fallback> {
    // MongoDB ranks bool as its own type bracket: under range operators a bool
    // compares only with another bool, never with a number or any other type
    // (verified against mongod — `{a: {$gt: 0}}` skips a bool-valued `a`). A bool
    // operand therefore yields an ordering only when BOTH sides are bool; against
    // anything else it's not comparable (no match), mirroring Python's matcher,
    // whose bool-vs-non-bool range compare is guarded to no-match.
    if matches!(a, Bson::Boolean(_)) || matches!(b, Bson::Boolean(_)) {
        return Ok(match (a, b) {
            (Bson::Boolean(x), Bson::Boolean(y)) => Some(x.cmp(y)),
            _ => None,
        });
    }
    if let Some(r) = numeric::fast_cmp(a, b) {
        return Ok(r);
    }
    if let (Some(na), Some(nb)) = (numeric::classify(a), numeric::classify(b)) {
        return Ok(numeric::cmp(&na, &nb));
    }
    // Collation-aware string ordering (defers on non-ASCII / numericOrdering).
    if let (Some(c), Bson::String(x), Bson::String(y)) = (coll, a, b) {
        return Ok(Some(collation::compare(x, y, c).ok_or(Fallback)?));
    }
    // A document operand: Python's `<` on dicts raises TypeError — i.e. no match —
    // and mongod's range operators ($gt/$lt/…) are type-bracketed, so a
    // document-valued field never satisfies a scalar bound (and a document bound
    // never matches a scalar field). Return None (no-match) rather than deferring:
    // this is what lets `$elemMatch: {$gt: n}` over an array of sub-documents, or a
    // plain `{a: {$gt: n}}` against a document-valued `a`, no-match cleanly on the
    // Rust server instead of erroring on an otherwise-fine cross-type query.
    match (a, b) {
        // Two embedded documents order field-by-field under range operators
        // (mongod: first differing key compares as a string, else recurse into
        // the value, else the shorter document sorts first). `order::bson_lt`
        // is exactly that comparator; `None` from it means a DBPointer sub-value
        // Python resolves with a type-name tiebreak — defer that rare case.
        (Bson::Document(_), Bson::Document(_)) => {
            let ord = if crate::order::bson_lt(a, b).ok_or(Fallback)? {
                Ordering::Less
            } else if crate::order::bson_lt(b, a).ok_or(Fallback)? {
                Ordering::Greater
            } else {
                Ordering::Equal
            };
            return Ok(Some(ord));
        }
        // A document vs a non-document is a different BSON type bracket, so a
        // document-valued field never satisfies a scalar bound (and vice versa):
        // a clean no-match, not an error.
        (Bson::Document(_), _) | (_, Bson::Document(_)) => return Ok(None),
        _ => {}
    }
    // Structural array ordering: Python compares lists element-wise /
    // lexicographically (`list < list`), which mongod's range operators mirror.
    match (a, b) {
        (Bson::Array(xs), Bson::Array(ys)) => {
            for (ea, eb) in xs.iter().zip(ys.iter()) {
                // Array elements compare by FULL BSON order (type rank first),
                // NOT the type-bracketed scalar compare above — mongod ranks a
                // string element above a number element, so `[1,"x"] > [1,2]`.
                // `order::bson_lt` is that full order (a DBPointer element
                // defers, via `None`).
                if crate::order::bson_lt(ea, eb).ok_or(Fallback)? {
                    return Ok(Some(Ordering::Less));
                }
                if crate::order::bson_lt(eb, ea).ok_or(Fallback)? {
                    return Ok(Some(Ordering::Greater));
                }
                // Equal — continue to the next element pair.
            }
            // One array is a prefix of the other: the shorter sorts first.
            return Ok(Some(xs.len().cmp(&ys.len())));
        }
        // Array vs scalar / doc: Python's `[..] < 2` raises TypeError -> no match.
        (Bson::Array(_), _) | (_, Bson::Array(_)) => return Ok(None),
        _ => {}
    }
    // Exotic BSON types under a range operator. pymongo hands the Python
    // engine plain `str` for a Symbol and the str-subclass `Code` for JS code
    // (scope ignored), so the Python oracle compares those as strings —
    // including cross Symbol/Code/String pairs. A DBPointer has no ordering in
    // Python (TypeError) and undefined decodes to None: not comparable, clean
    // no-match. Under a collation the string path above would have applied
    // folding the exotic text skips, so defer that combination to Python.
    if is_exotic(a) || is_exotic(b) {
        if coll.is_some() {
            return Err(Fallback);
        }
        fn text_of(v: &Bson) -> Option<&str> {
            match v {
                Bson::String(s) | Bson::Symbol(s) | Bson::JavaScriptCode(s) => Some(s),
                Bson::JavaScriptCodeWithScope(c) => Some(&c.code),
                _ => None,
            }
        }
        return Ok(match (text_of(a), text_of(b)) {
            (Some(x), Some(y)) => Some(x.cmp(y)),
            _ => None,
        });
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
        // mongod truthiness: only false / 0 / null are falsy. An empty string,
        // array, or document is TRUTHY (unlike Python's bool()).
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
        Bson::Double(d) if d.fract() == 0.0 => Some(*d as i64), // whole-double code
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

/// `$jsonSchema` document-level validation — mirrors
/// `secantus.query._validate_json_schema`, a bounded JSON-Schema subset: exactly
/// the keywords in the pure impl's if-ladder (`bsonType` / `type` / `enum` /
/// numeric bounds / string length + `pattern` / array items + counts / object
/// `required` / `properties` / counts). Other keywords are ignored, matching the
/// Python server. Any schema shape whose result we can't reproduce faithfully — a
/// non-numeric bound, a non-int count, an uncompilable `pattern`, a non-string
/// `type`, a malformed `enum` / `required` / `properties`, or an `enum` /
/// bound comparison that defers — returns `Fallback` (Python runs instead).
/// A canonical byte-key for `uniqueItems` duplicate detection. Numerics collapse
/// cross-type by value (`sortkey::encode_value` gives them a common form), and
/// documents / arrays recurse so nested cross-type-equal numerics collide —
/// mirroring `secantus.query._unique_items_key`. Any value the encoder can't
/// represent returns `Fallback` (Python runs instead).
fn unique_items_key(value: &Bson) -> Result<Vec<u8>, Fallback> {
    let mut out = Vec::new();
    match value {
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
            out.push(b'n');
            out.extend(crate::sortkey::encode_value(value, None).map_err(|_| Fallback)?);
        }
        Bson::Document(d) => {
            out.push(b'd');
            for (k, v) in d {
                out.extend((k.len() as u32).to_be_bytes());
                out.extend_from_slice(k.as_bytes());
                let child = unique_items_key(v)?;
                out.extend((child.len() as u32).to_be_bytes());
                out.extend(child);
            }
        }
        Bson::Array(a) => {
            out.push(b'a');
            for v in a {
                let child = unique_items_key(v)?;
                out.extend((child.len() as u32).to_be_bytes());
                out.extend(child);
            }
        }
        other => {
            out.push(b's');
            out.extend(crate::sortkey::encode_value(other, None).map_err(|_| Fallback)?);
        }
    }
    Ok(out)
}

/// Every $jsonSchema keyword mongod 7.0 accepts (title / description are
/// accepted-and-ignored metadata). Mirrors `query._JSON_SCHEMA_KEYWORDS`.
const JSON_SCHEMA_KEYWORDS: &[&str] = &[
    "additionalItems",
    "additionalProperties",
    "allOf",
    "anyOf",
    "bsonType",
    "dependencies",
    "description",
    "enum",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "items",
    "maxItems",
    "maxLength",
    "maxProperties",
    "maximum",
    "minItems",
    "minLength",
    "minProperties",
    "minimum",
    "multipleOf",
    "not",
    "oneOf",
    "pattern",
    "patternProperties",
    "properties",
    "required",
    "title",
    "type",
    "uniqueItems",
];
const JSON_SCHEMA_UNSUPPORTED: &[&str] =
    &["$ref", "$schema", "default", "definitions", "format", "id"];

/// Parse-time $jsonSchema keyword validation, recursing into every sub-schema.
/// Mirrors `query._check_json_schema_keywords`; codes and messages are
/// verbatim from a mongod 7.0 probe. Returns `Some((code, codeName, errmsg))`
/// for the first violation, `None` for a clean schema. The command layer runs
/// this up-front (mongod rejects the whole query before matching a document).
pub fn json_schema_keyword_error(schema: &Bson) -> Option<(i32, &'static str, String)> {
    let Bson::Document(sch) = schema else {
        return Some((14, "TypeMismatch", "$jsonSchema must be an object".into()));
    };
    for (kw, arg) in sch.iter() {
        let kw = kw.as_str();
        if JSON_SCHEMA_UNSUPPORTED.contains(&kw) {
            return Some((
                9,
                "FailedToParse",
                format!("$jsonSchema keyword '{kw}' is not currently supported"),
            ));
        }
        if !JSON_SCHEMA_KEYWORDS.contains(&kw) {
            return Some((
                9,
                "FailedToParse",
                format!("Unknown $jsonSchema keyword: {kw}"),
            ));
        }
        if (kw == "title" || kw == "description") && !matches!(arg, Bson::String(_)) {
            return Some((
                14,
                "TypeMismatch",
                format!("$jsonSchema keyword '{kw}' must be of type string"),
            ));
        }
        if kw == "multipleOf" {
            let m = match arg {
                Bson::Int32(n) => *n as f64,
                Bson::Int64(n) => *n as f64,
                Bson::Double(d) => *d,
                _ => {
                    return Some((
                        14,
                        "TypeMismatch",
                        "$jsonSchema keyword 'multipleOf' must be a number".into(),
                    ))
                }
            };
            if m <= 0.0 {
                return Some((
                    9,
                    "FailedToParse",
                    "$jsonSchema keyword 'multipleOf' must have a positive value".into(),
                ));
            }
        }
        if kw == "exclusiveMinimum" || kw == "exclusiveMaximum" {
            if !matches!(arg, Bson::Boolean(_)) {
                return Some((
                    14,
                    "TypeMismatch",
                    format!("$jsonSchema keyword '{kw}' must be a boolean"),
                ));
            }
            let bound = if kw == "exclusiveMinimum" {
                "minimum"
            } else {
                "maximum"
            };
            if !sch.contains_key(bound) {
                return Some((
                    9,
                    "FailedToParse",
                    format!("$jsonSchema keyword '{bound}' must be a present if {kw} is present"),
                ));
            }
        }
        // Recurse into sub-schemas.
        match kw {
            "properties" | "patternProperties" => {
                if let Bson::Document(map) = arg {
                    for sub in map.values() {
                        if let Some(e) = json_schema_keyword_error(sub) {
                            return Some(e);
                        }
                    }
                }
            }
            "additionalProperties" | "additionalItems" => {
                if matches!(arg, Bson::Document(_)) {
                    if let Some(e) = json_schema_keyword_error(arg) {
                        return Some(e);
                    }
                }
            }
            "not" => {
                if let Some(e) = json_schema_keyword_error(arg) {
                    return Some(e);
                }
            }
            "items" => {
                let subs: Vec<&Bson> = match arg {
                    Bson::Array(a) => a.iter().collect(),
                    single => vec![single],
                };
                for sub in subs {
                    if let Some(e) = json_schema_keyword_error(sub) {
                        return Some(e);
                    }
                }
            }
            "allOf" | "anyOf" | "oneOf" => {
                if let Bson::Array(a) = arg {
                    for sub in a {
                        if let Some(e) = json_schema_keyword_error(sub) {
                            return Some(e);
                        }
                    }
                }
            }
            "dependencies" => {
                if let Bson::Document(map) = arg {
                    for dep in map.values() {
                        if matches!(dep, Bson::Document(_)) {
                            if let Some(e) = json_schema_keyword_error(dep) {
                                return Some(e);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn validate_json_schema(value: &Bson, schema: &Bson) -> R {
    let Bson::Document(sch) = schema else {
        return Ok(false); // Python: `not isinstance(schema, Mapping)` -> False
    };
    // bsonType (BSON type alias/code, or a list of them).
    if let Some(bt) = sch.get("bsonType") {
        let ok = match bt {
            Bson::Array(types) => types.iter().any(|t| matches_type(value, t)),
            single => matches_type(value, single),
        };
        if !ok {
            return Ok(false);
        }
    }
    // type (JSON type name, or a list of them).
    if let Some(jt) = sch.get("type") {
        let types: Vec<&Bson> = match jt {
            Bson::Array(a) => a.iter().collect(),
            single => vec![single],
        };
        let mut ok = false;
        for t in types {
            let Bson::String(name) = t else {
                return Err(Fallback); // non-string json type -> defer
            };
            if matches_json_type(value, name) {
                ok = true;
                break;
            }
        }
        if !ok {
            return Ok(false);
        }
    }
    // enum — membership via Python `==`.
    if let Some(en) = sch.get("enum") {
        let Bson::Array(items) = en else {
            return Err(Fallback); // `value not in <non-list>` -> defer
        };
        let mut found = false;
        for e in items {
            if expressions::py_eq(value, e).map_err(|_| Fallback)? {
                found = true;
                break;
            }
        }
        if !found {
            return Ok(false);
        }
    }
    // Numeric bounds — only for int/double values (Python: `int/float and not
    // bool`; Decimal128 is excluded there too). Draft-4 semantics, matching
    // mongod (probed 2026-07-17): exclusiveMinimum / exclusiveMaximum are
    // BOOLEANS sharpening minimum / maximum to a strict bound (the numeric
    // draft-6 form is rejected at parse time by the keyword validation).
    if matches!(value, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)) {
        if let Some(bound) = sch.get("minimum") {
            let ord = numeric_order(value, bound)?;
            let exclusive = sch.get("exclusiveMinimum") == Some(&Bson::Boolean(true));
            if ord == Ordering::Less || (exclusive && ord == Ordering::Equal) {
                return Ok(false);
            }
        }
        if let Some(bound) = sch.get("maximum") {
            let ord = numeric_order(value, bound)?;
            let exclusive = sch.get("exclusiveMaximum") == Some(&Bson::Boolean(true));
            if ord == Ordering::Greater || (exclusive && ord == Ordering::Equal) {
                return Ok(false);
            }
        }
        // multipleOf — Python `math.fmod(value, m) != 0`.
        if let Some(m) = sch.get("multipleOf") {
            let m = match m {
                Bson::Int32(n) => *n as f64,
                Bson::Int64(n) => *n as f64,
                Bson::Double(d) => *d,
                _ => return Err(Fallback), // rejected at parse time server-side
            };
            let v = match value {
                Bson::Int32(n) => *n as f64,
                Bson::Int64(n) => *n as f64,
                Bson::Double(d) => *d,
                _ => unreachable!(),
            };
            if (v % m) != 0.0 {
                return Ok(false);
            }
        }
    }
    // String constraints.
    if let Bson::String(s) = value {
        let len = s.chars().count() as i64; // code-point length, like Python `len`
        if let Some(min) = sch.get("minLength") {
            if len < as_schema_int(min)? {
                return Ok(false);
            }
        }
        if let Some(max) = sch.get("maxLength") {
            if len > as_schema_int(max)? {
                return Ok(false);
            }
        }
        if let Some(pat) = sch.get("pattern") {
            let re = regexutil::compile(pat, None).map_err(|_| Fallback)?;
            if !re.is_match(s) {
                return Ok(false);
            }
        }
    }
    // Array constraints.
    if let Bson::Array(arr) = value {
        let len = arr.len() as i64;
        if let Some(min) = sch.get("minItems") {
            if len < as_schema_int(min)? {
                return Ok(false);
            }
        }
        if let Some(max) = sch.get("maxItems") {
            if len > as_schema_int(max)? {
                return Ok(false);
            }
        }
        if let Some(items) = sch.get("items") {
            match items {
                // Tuple validation: position i validates against items[i];
                // elements past the tuple validate against additionalItems
                // (false = none allowed; a schema = each must match; absent =
                // anything goes). Matches the mongod probe.
                Bson::Array(tuple) => {
                    let extra = sch.get("additionalItems");
                    for (i, item) in arr.iter().enumerate() {
                        if i < tuple.len() {
                            if !validate_json_schema(item, &tuple[i])? {
                                return Ok(false);
                            }
                        } else {
                            match extra {
                                Some(Bson::Boolean(false)) => return Ok(false),
                                Some(sub @ Bson::Document(_))
                                    if !validate_json_schema(item, sub)? =>
                                {
                                    return Ok(false);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                single => {
                    for item in arr {
                        if !validate_json_schema(item, single)? {
                            return Ok(false);
                        }
                    }
                }
            }
        }
        // `uniqueItems: true` — every element must be distinct under MongoDB
        // value equality, which collapses cross-type-equal numerics recursively
        // inside sub-documents and sub-arrays ({a:1} == {a:1.0}). An element the
        // encoder can't represent defers the whole match to Python.
        if matches!(sch.get("uniqueItems"), Some(Bson::Boolean(true))) {
            let mut seen: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
            for item in arr {
                let key = unique_items_key(item)?;
                if !seen.insert(key) {
                    return Ok(false);
                }
            }
        }
    }
    // Object constraints.
    if let Bson::Document(obj) = value {
        if let Some(req) = sch.get("required") {
            let Bson::Array(keys) = req else {
                return Err(Fallback);
            };
            for k in keys {
                let Bson::String(name) = k else {
                    return Err(Fallback);
                };
                if !obj.contains_key(name) {
                    return Ok(false);
                }
            }
        }
        if let Some(props) = sch.get("properties") {
            let Bson::Document(pd) = props else {
                return Err(Fallback);
            };
            for (prop, prop_schema) in pd {
                if let Some(pv) = obj.get(prop) {
                    if !validate_json_schema(pv, prop_schema)? {
                        return Ok(false);
                    }
                }
            }
        }
        let n = obj.len() as i64;
        if let Some(min) = sch.get("minProperties") {
            if n < as_schema_int(min)? {
                return Ok(false);
            }
        }
        if let Some(max) = sch.get("maxProperties") {
            if n > as_schema_int(max)? {
                return Ok(false);
            }
        }
        // patternProperties: each key matching a pattern-regex validates against
        // its sub-schema. Compile once; also used to exclude matches from
        // "additional".
        let mut pattern_res: Vec<regexutil::CompiledRegex> = Vec::new();
        if let Some(pp) = sch.get("patternProperties") {
            let Bson::Document(pd) = pp else {
                return Err(Fallback);
            };
            for (pat, sub) in pd {
                let rx =
                    regexutil::compile(&Bson::String(pat.clone()), None).map_err(|_| Fallback)?;
                for (k, v) in obj {
                    if rx.is_match(k) && !validate_json_schema(v, sub)? {
                        return Ok(false);
                    }
                }
                pattern_res.push(rx);
            }
        }
        if let Some(ap) = sch.get("additionalProperties") {
            // "Additional" = a key not named in `properties` and not matching any
            // patternProperties regex. `false` forbids extras; a sub-schema
            // validates them.
            let named: Vec<&str> = match sch.get("properties") {
                Some(Bson::Document(pd)) => pd.keys().map(String::as_str).collect(),
                _ => Vec::new(),
            };
            let is_extra =
                |k: &str| !named.contains(&k) && !pattern_res.iter().any(|rx| rx.is_match(k));
            match ap {
                Bson::Boolean(true) => {}
                Bson::Boolean(false) => {
                    if obj.keys().any(|k| is_extra(k.as_str())) {
                        return Ok(false);
                    }
                }
                Bson::Document(_) => {
                    for (k, pv) in obj {
                        if is_extra(k.as_str()) && !validate_json_schema(pv, ap)? {
                            return Ok(false);
                        }
                    }
                }
                _ => return Err(Fallback), // non-bool/doc -> defer
            }
        }
        // dependencies: if a trigger key is present, its listed properties must all
        // be present (array form) or the whole doc must validate (schema form).
        if let Some(deps) = sch.get("dependencies") {
            let Bson::Document(dd) = deps else {
                return Err(Fallback);
            };
            for (prop, dep) in dd {
                if !obj.contains_key(prop) {
                    continue;
                }
                match dep {
                    Bson::Array(reqs) => {
                        for r in reqs {
                            let Bson::String(name) = r else {
                                return Err(Fallback);
                            };
                            if !obj.contains_key(name) {
                                return Ok(false);
                            }
                        }
                    }
                    Bson::Document(_) => {
                        if !validate_json_schema(value, dep)? {
                            return Ok(false);
                        }
                    }
                    _ => return Err(Fallback),
                }
            }
        }
    }
    // Logical combinators apply to the value regardless of its BSON type.
    if let Some(Bson::Array(subs)) = sch.get("allOf") {
        for s in subs {
            if !validate_json_schema(value, s)? {
                return Ok(false);
            }
        }
    } else if sch.contains_key("allOf") {
        return Err(Fallback);
    }
    if let Some(Bson::Array(subs)) = sch.get("anyOf") {
        let mut any = false;
        for s in subs {
            if validate_json_schema(value, s)? {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(false);
        }
    } else if sch.contains_key("anyOf") {
        return Err(Fallback);
    }
    if let Some(Bson::Array(subs)) = sch.get("oneOf") {
        let mut count = 0;
        for s in subs {
            if validate_json_schema(value, s)? {
                count += 1;
            }
        }
        if count != 1 {
            return Ok(false);
        }
    } else if sch.contains_key("oneOf") {
        return Err(Fallback);
    }
    if let Some(not_schema) = sch.get("not") {
        if validate_json_schema(value, not_schema)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// JSON-Schema `type` predicate — mirrors `query._matches_json_type`.
fn matches_json_type(value: &Bson, name: &str) -> bool {
    match name {
        "string" => matches!(value, Bson::String(_)),
        "number" => matches!(
            value,
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
        ),
        "integer" => matches!(value, Bson::Int32(_) | Bson::Int64(_)),
        "boolean" => matches!(value, Bson::Boolean(_)),
        "null" => matches!(value, Bson::Null),
        "array" => matches!(value, Bson::Array(_)),
        "object" => matches!(value, Bson::Document(_)),
        _ => false,
    }
}

/// Order a numeric value against a schema bound; the bound must itself be numeric
/// (Python `int/float < non-number` raises -> defer), and an uncomparable pair
/// (`py_order` -> `None`) also defers.
fn numeric_order(value: &Bson, bound: &Bson) -> Result<Ordering, Fallback> {
    if !matches!(bound, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)) {
        return Err(Fallback);
    }
    expressions::py_order(value, bound)
        .map_err(|_| Fallback)?
        .ok_or(Fallback)
}

/// A schema keyword that must be an integer count (`minLength` / `maxItems` /
/// ...); a non-int makes the Python `len < schema[...]` comparison raise -> defer.
fn as_schema_int(b: &Bson) -> Result<i64, Fallback> {
    match b {
        Bson::Int32(n) => Ok(*n as i64),
        Bson::Int64(n) => Ok(*n),
        _ => Err(Fallback),
    }
}

/// A valid mongod $type argument: a known alias, or a numeric code in
/// {-1, 1..19, 127} (a whole double counts). Anything else -> defer so Python
/// raises the exact error (BadValue / TypeMismatch).
fn type_spec_valid(spec: &Bson) -> bool {
    const ALIASES: [&str; 22] = [
        "double",
        "string",
        "object",
        "array",
        "binData",
        "undefined",
        "objectId",
        "bool",
        "date",
        "null",
        "regex",
        "dbPointer",
        "javascript",
        "symbol",
        "javascriptWithScope",
        "int",
        "timestamp",
        "long",
        "decimal",
        "minKey",
        "maxKey",
        "number",
    ];
    let valid_code = |c: i64| c == -1 || c == 127 || (1..=19).contains(&c);
    match spec {
        Bson::String(s) => ALIASES.contains(&s.as_str()),
        Bson::Int32(n) => valid_code(*n as i64),
        Bson::Int64(n) => valid_code(*n),
        Bson::Double(d) if d.fract() == 0.0 => valid_code(*d as i64),
        _ => false,
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
        if !type_spec_valid(s) {
            return Err(Fallback); // bad alias / code / bool / fractional -> Python raises
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
    // `$all: []` matches nothing (mongod), not everything — the vacuous
    // all-clauses-satisfied below would otherwise be true for every value.
    if required.is_empty() {
        return Ok(false);
    }
    // mongod: if any element is a $-expression doc, EVERY element must be a
    // {$elemMatch: …} clause; a mixed form or any other $-op doc is "no $
    // expressions in $all" -> defer so Python raises.
    let has_dollar =
        |e: &Bson| matches!(e, Bson::Document(d) if d.keys().any(|k| k.starts_with('$')));
    let is_elemmatch =
        |e: &Bson| matches!(e, Bson::Document(d) if d.len() == 1 && d.contains_key("$elemMatch"));
    if required.iter().any(has_dollar) && !required.iter().all(is_elemmatch) {
        return Err(Fallback);
    }
    // A regex element matches as a *pattern* (not by equality), mirroring
    // `query._op_all`; a non-regex element matches by `py_eq`. A regex the
    // engine can't compile still defers via `op_regex`. mongod treats a scalar
    // field value like a one-element array for `$all` (only `$elemMatch`
    // clauses require an actual array) — verified against mongod 7.0.12.
    for v in values {
        let Some(field) = v else { continue };
        // The elements to match each required clause against: the array's own
        // elements, or the scalar itself as a single element.
        let elems: &[Bson] = match field {
            Bson::Array(arr) => arr,
            scalar => std::slice::from_ref(scalar),
        };
        let is_array = matches!(field, Bson::Array(_));
        let mut all_present = true;
        for r in required {
            // A `{$elemMatch: {...}}` clause requires *some* element of an actual
            // array to match the sub-query — a scalar field never satisfies it.
            if let Bson::Document(rd) = r {
                if rd.len() == 1 {
                    if let Some(sub) = rd.get("$elemMatch") {
                        let ok = is_array && {
                            let arr_bson = Bson::Array(elems.to_vec());
                            op_elem_match(&[Some(&arr_bson)], sub)?
                        };
                        if !ok {
                            all_present = false;
                            break;
                        }
                        continue;
                    }
                }
            }
            let mut found = false;
            for e in elems {
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
        // An integer-valued double is accepted (mongod: `$size: 2.0` == 2). A
        // non-integer double, and any non-number (bool / string / …), are parse
        // errors mongod rejects — defer so the Rust server surfaces BadValue
        // (code 2, matching mongod's code) rather than a silent no-match.
        Bson::Double(d) if d.is_finite() && d.fract() == 0.0 => *d as i64,
        _ => return Err(Fallback),
    };
    if n < 0 {
        // mongod: "Expected a non-negative number" (code 2) — error, not
        // no-match. Fallback -> BadValue on the Rust server.
        return Err(Fallback);
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

/// The integer a value contributes to `$mod` (truncated toward zero), or None
/// if it isn't `$mod`-eligible. mongod (probed 7.0.12) truncates int / long /
/// double toward zero and **excludes bool** (bool is not a number for `$mod`).
/// Decimal128 defers to Python — `int(Decimal(...))` is exact to 34 digits,
/// which an `f64` truncation can't reproduce (the project's standing
/// Decimal128 precision-parity deferral).
fn mod_int(val: &Bson) -> Result<Option<i64>, Fallback> {
    Ok(match val {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        Bson::Double(d) => {
            if d.is_nan() || d.is_infinite() {
                None
            } else {
                Some(d.trunc() as i64)
            }
        }
        Bson::Decimal128(_) => return Err(Fallback),
        // bool and every non-numeric type: not eligible (no match), not an error.
        _ => None,
    })
}

fn op_mod(values: &[Option<&Bson>], spec: &Bson) -> R {
    let arr = spec.as_array().ok_or(Fallback)?;
    if arr.len() < 2 {
        return Err(Fallback); // "malformed mod, not enough elements" (code 2)
    }
    // The divisor is truncated toward zero too (mongod: `[2.5, 0]` divides by 2).
    let Some(div) = mod_int(&arr[0])? else {
        return Err(Fallback); // non-numeric divisor -> malformed (code 2)
    };
    if div == 0 {
        return Err(Fallback); // "divisor cannot be 0" (code 2)
    }
    let rem = as_int(&arr[1]);
    let check = |val: &Bson| -> Result<Option<bool>, Fallback> {
        match mod_int(val)? {
            // Rust's `%` on i64 is C-style truncated modulo (sign of the
            // dividend), matching mongod — `-5 % 2 == -1`. `rem == None` (a
            // float remainder in the spec) can't equal an integer result.
            Some(iv) => Ok(Some(rem.is_some() && iv % div == rem.unwrap())),
            None => Ok(None),
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
    // mongod accepts a whole-number double mask / bit position (truncated);
    // fractional / negative / bool defer to the Python oracle (which raises the
    // exact code -- mask 9, position 2).
    match arg {
        Bson::Boolean(_) => Err(Fallback),
        Bson::Int32(n) if *n >= 0 => Ok(*n as u64),
        Bson::Int64(n) if *n >= 0 => Ok(*n as u64),
        Bson::Double(d) if d.is_finite() && d.fract() == 0.0 && *d >= 0.0 => Ok(*d as u64),
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => Err(Fallback),
        Bson::Array(a) => {
            let mut mask = 0u64;
            for bit in a {
                let p = match bit {
                    Bson::Int32(p) => *p as i64,
                    Bson::Int64(p) => *p,
                    Bson::Double(d) if d.is_finite() && d.fract() == 0.0 => *d as i64,
                    _ => return Err(Fallback),
                };
                if !(0..64).contains(&p) {
                    return Err(Fallback);
                }
                mask |= 1 << p;
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

/// A float or Decimal128 NaN. Used only by equality — ordering keeps IEEE
/// semantics, where NaN is incomparable with everything including itself.
fn is_nan_bson(b: &Bson) -> bool {
    match b {
        Bson::Double(d) => d.is_nan(),
        // Decimal128's NaN is a bit pattern, not a value we can compare; parse
        // its string form, which is what the bson crate exposes portably.
        Bson::Decimal128(d) => {
            let s = d.to_string();
            s.eq_ignore_ascii_case("nan") || s.eq_ignore_ascii_case("-nan")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn m(doc: Document, query: Document) -> bool {
        let owned = matches(&doc, &query, &Document::new(), None).expect("should not fall back");
        // The raw-BSON matcher must agree with the owned one bool-for-bool
        // wherever the owned matcher succeeds — every query test here doubles as
        // a `matches_raw` parity check.
        let bytes = bson::to_vec(&doc).unwrap();
        let raw = RawDocument::from_bytes(&bytes).unwrap();
        let raw_res = matches_raw(raw, &query, &Document::new(), None)
            .expect("matches_raw fell back where matches succeeded");
        assert_eq!(
            raw_res, owned,
            "matches_raw disagreed with matches for doc={doc:?} query={query:?}"
        );
        owned
    }

    #[test]
    fn nan_equals_nan_for_equality_but_not_ordering() {
        // mongod matches `{x: NaN}` against a stored NaN (probed against
        // 6.0.16). Without this, a document written with `_id: NaN` is accepted
        // and then unreachable by its own key.
        assert!(m(doc! {"x": f64::NAN}, doc! {"x": f64::NAN}));
        assert!(m(doc! {"_id": f64::NAN}, doc! {"_id": f64::NAN}));

        // No false positives in either direction.
        assert!(!m(doc! {"x": f64::NAN}, doc! {"x": 1}));
        assert!(!m(doc! {"x": 5}, doc! {"x": f64::NAN}));
        assert!(!m(doc! {"x": f64::NAN}, doc! {"x": f64::INFINITY}));

        // Infinity compares fine under IEEE and must be untouched.
        assert!(m(doc! {"x": f64::INFINITY}, doc! {"x": f64::INFINITY}));
        assert!(!m(doc! {"x": f64::INFINITY}, doc! {"x": f64::NEG_INFINITY}));

        // The rule is confined to equality: NaN stays incomparable for ranges,
        // which is what keeps sort order IEEE-correct.
        assert!(!m(doc! {"x": f64::NAN}, doc! {"x": {"$gt": f64::NAN}}));
        assert!(!m(doc! {"x": f64::NAN}, doc! {"x": {"$lt": 0}}));
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
    fn bool_is_its_own_range_bracket() {
        // Under range operators bool compares only with bool (mongod brackets
        // bool away from numbers). A bool field never matches a numeric bound,
        // and a numeric field never matches a bool bound.
        assert!(!m(doc! {"x": true}, doc! {"x": {"$gt": 0}}));
        assert!(!m(doc! {"x": true}, doc! {"x": {"$lt": 2}}));
        assert!(!m(doc! {"x": false}, doc! {"x": {"$gte": 0}}));
        assert!(!m(doc! {"x": 5}, doc! {"x": {"$gt": false}}));
        assert!(!m(doc! {"x": 0}, doc! {"x": {"$lt": true}}));
        // bool-vs-bool still compares (True > False, False >= False).
        assert!(m(doc! {"x": true}, doc! {"x": {"$gt": false}}));
        assert!(m(doc! {"x": false}, doc! {"x": {"$gte": false}}));
        assert!(!m(doc! {"x": false}, doc! {"x": {"$gt": false}}));
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
    fn range_against_document_no_matches() {
        // mongod's range operators are type-bracketed: a document-valued field
        // never satisfies a *scalar* bound, and a scalar field never matches a
        // *document* bound (a clean no-match, not an error).
        assert!(!m(doc! {"a": {"x": 1}}, doc! {"a": {"$gt": 2}}));
        assert!(!m(doc! {"a": {"x": 1}}, doc! {"a": {"$lt": 2}}));
        assert!(!m(doc! {"a": 2}, doc! {"a": {"$gt": {"x": 1}}}));
        // Two documents in the SAME bracket DO order, field-by-field (verified
        // against mongod 7.0.12): {x:2} > {x:1}; {x:1,y:9} > {x:1} (longer wins
        // on a value tie); {y:1} > {x:1} (key "y" > "x"); {x:1} is not > {x:1}.
        assert!(m(doc! {"a": {"x": 2}}, doc! {"a": {"$gt": {"x": 1}}}));
        assert!(m(
            doc! {"a": {"x": 1, "y": 9}},
            doc! {"a": {"$gt": {"x": 1}}}
        ));
        assert!(m(doc! {"a": {"y": 1}}, doc! {"a": {"$gt": {"x": 1}}}));
        assert!(!m(doc! {"a": {"x": 1}}, doc! {"a": {"$gt": {"x": 1}}}));
        assert!(m(doc! {"a": {"x": 1}}, doc! {"a": {"$gte": {"x": 1}}}));
        assert!(m(doc! {"a": {"x": 0}}, doc! {"a": {"$lt": {"x": 1}}}));
        // The differential case: $elemMatch: {$gt: n} over an array of
        // sub-documents. Each element is a document, so every element no-matches
        // the scalar bound — the whole predicate is false, not an error.
        assert!(!m(
            doc! {"items": [{"k": 1}, {"k": 2}]},
            doc! {"items": {"$elemMatch": {"$gt": 2}}}
        ));
        // Sanity: a scalar-array element still matches via the multikey path.
        assert!(m(
            doc! {"items": [1, 2, 3]},
            doc! {"items": {"$elemMatch": {"$gt": 2}}}
        ));
    }

    #[test]
    fn array_vs_array_lexicographic_range() {
        // Verified against real mongod 6.0 with docs:
        //   {a:[1,3]} {a:[1,2]} {a:[1,2,3]} {a:5} {a:[2]}
        //   {a: {$gt:  [1,2]}} -> [1,3], [1,2,3], [2]
        //   {a: {$lt:  [1,3]}} -> [1,2], [1,2,3]
        //   {a: {$gte: [1,2]}} -> [1,3], [1,2], [1,2,3], [2]
        assert!(m(doc! {"a": [1, 3]}, doc! {"a": {"$gt": [1, 2]}}));
        assert!(!m(doc! {"a": [1, 2]}, doc! {"a": {"$gt": [1, 2]}}));
        assert!(m(doc! {"a": [1, 2, 3]}, doc! {"a": {"$gt": [1, 2]}}));
        assert!(!m(doc! {"a": 5}, doc! {"a": {"$gt": [1, 2]}}));
        assert!(m(doc! {"a": [2]}, doc! {"a": {"$gt": [1, 2]}}));

        assert!(!m(doc! {"a": [1, 3]}, doc! {"a": {"$lt": [1, 3]}}));
        assert!(m(doc! {"a": [1, 2]}, doc! {"a": {"$lt": [1, 3]}}));
        assert!(m(doc! {"a": [1, 2, 3]}, doc! {"a": {"$lt": [1, 3]}}));
        assert!(!m(doc! {"a": [2]}, doc! {"a": {"$lt": [1, 3]}}));

        assert!(m(doc! {"a": [1, 3]}, doc! {"a": {"$gte": [1, 2]}}));
        assert!(m(doc! {"a": [1, 2]}, doc! {"a": {"$gte": [1, 2]}}));
        assert!(m(doc! {"a": [1, 2, 3]}, doc! {"a": {"$gte": [1, 2]}}));
        assert!(m(doc! {"a": [2]}, doc! {"a": {"$gte": [1, 2]}}));

        // Prefix ordering: [1,2] < [1,2,3] (shorter array is Less).
        assert!(m(doc! {"a": [1, 2]}, doc! {"a": {"$lt": [1, 2, 3]}}));
        assert!(m(doc! {"a": [1, 2, 3]}, doc! {"a": {"$gt": [1, 2]}}));

        // Array-vs-scalar bound still works via the multikey element path:
        // {a:[1,3]} vs {$gt:2} matches because element 3 > 2 (the whole-array
        // compare against the scalar bound is None -> harmless no-match).
        assert!(m(doc! {"a": [1, 3]}, doc! {"a": {"$gt": 2}}));
        assert!(!m(doc! {"a": [1, 2]}, doc! {"a": {"$gt": 2}}));
    }

    #[test]
    fn array_vs_array_cross_type_element_ordering() {
        // Array elements order by FULL BSON order (type rank first), so a
        // cross-type element pair still orders — verified against mongod
        // 7.0.12: `[1, "x"]` has a string (rank 4) where `[1, 2]` has a number
        // (rank 3), so `[1, "x"] > [1, 2]`. (Previously both servers no-matched
        // this — Python's list `<` raises on str-vs-int.)
        assert!(m(doc! {"a": [1, "x"]}, doc! {"a": {"$gt": [1, 2]}}));
        assert!(!m(doc! {"a": [1, "x"]}, doc! {"a": {"$lt": [1, 2]}}));
        // A decisive difference before the cross-type element still orders.
        assert!(m(doc! {"a": [2, "x"]}, doc! {"a": {"$gt": [1, 2]}}));
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

    #[test]
    fn json_schema_unique_items_crosstype() {
        let schema = doc! {"$jsonSchema": {"properties": {"arr": {"uniqueItems": true}}}};
        // top-level cross-type-equal numerics collide
        assert!(!m(doc! {"arr": [1, 1.0]}, schema.clone()));
        assert!(m(doc! {"arr": [1, 2]}, schema.clone()));
        // nested cross-type-equal numerics collide ({a:1} == {a:1.0})
        assert!(!m(doc! {"arr": [{"a": 1}, {"a": 1.0}]}, schema.clone()));
        assert!(m(doc! {"arr": [{"a": 1}, {"a": 2}]}, schema.clone()));
        // exact duplicate documents collide
        assert!(!m(doc! {"arr": [{"a": 1}, {"a": 1}]}, schema.clone()));
        // recursively inside sub-arrays
        assert!(!m(doc! {"arr": [[1, 2], [1.0, 2.0]]}, schema.clone()));
        assert!(m(doc! {"arr": [[1, 2], [1, 3]]}, schema));
    }
}
