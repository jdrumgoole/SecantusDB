//! Update application — Rust port of `secantus.update.apply_update`, the third
//! leaf engine. Same graceful-fallback design as the query matcher: handle the
//! common, deterministic operators byte-for-byte and return `Fallback` for
//! anything whose Python semantics we don't reproduce, so the shim runs the
//! pure-Python `apply_update` instead (which also raises the right errors).
//!
//! Handled: replacement-style updates, `$set`, `$setOnInsert`, `$unset`,
//! `$inc`, `$mul`, `$push` (incl. the `$each` modifier form with `$position` /
//! `$slice` / `$sort` — `1`/`-1` whole-element or `{field: dir}`, BSON-order),
//! `$pop`, `$rename`, `$bit`, `$min`/`$max` (full BSON cross-type order via
//! `order::bson_lt` — the direct `_bson_lt` port, covering bool / Decimal128 /
//! NaN / Binary / Timestamp / Regex / Min-MaxKey and the decoded exotic text
//! types; a missing field is set, an explicit-null is compared as rank 2; only
//! a DBPointer operand still defers),
//! `$addToSet` (incl. `$each`), `$pull` (query semantics: element-value predicate /
//! sub-document match / equality, via `query::matches`), `$pullAll` (literal
//! equality via `expressions::py_eq`), plus `_id` immutability.
//! Deferred to Python: pipeline (array) updates, positional operators
//! (`$`/`$[]`/`$[id]`) and array filters, `$currentDate` (non-deterministic), a
//! `$push` `$sort` over elements outside the sortable subset, a `$min`/`$max`
//! against a non-sortable operand (bool / Decimal128 / NaN / exotic — whose full
//! order Python's `_bson_lt` handles), Decimal128 / non-numeric arithmetic, and
//! every error condition (so Python raises the exact `UpdateError`).

use std::collections::HashMap;

use bson::{doc, Bson, Document};

use crate::decimal;
use crate::numeric::{as_float_like, as_int_like, int_promoted_to_bson, is_int64};
use crate::paths::{self, get_path, has_path};
use crate::{expressions, query};

pub use crate::fallback::Fallback;

type R<T> = Result<T, Fallback>;

fn is_positional_token(part: &str) -> bool {
    part == "$" || part == "$[]" || (part.starts_with("$[") && part.ends_with("]"))
}

fn has_positional(path: &str) -> bool {
    path.split('.').any(is_positional_token)
}

/// Two dotted `$rename` paths are equal or one is an ancestor of the other
/// (mongod: source and target must not be on the same path). Mirrors
/// `update._rename_same_path`.
fn rename_same_path(a: &str, b: &str) -> bool {
    let ap: Vec<&str> = a.split('.').collect();
    let bp: Vec<&str> = b.split('.').collect();
    let n = ap.len().min(bp.len());
    ap[..n] == bp[..n]
}

/// True if walking `path` against `doc` passes through an array element — mongod
/// forbids a `$rename` source/destination from being an array element (this
/// previously silently corrupted the array). Mirrors
/// `update._rename_traverses_array`.
fn rename_traverses_array(doc: &Document, path: &str) -> bool {
    let mut parts = path.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(mut cur) = doc.get(first) else {
        return false;
    };
    for part in parts {
        match cur {
            // Only a numeric index into an array is the forbidden "array
            // element"; a positional token ($ / $[] / $[id]) is not (and is
            // already deferred above via has_positional).
            Bson::Array(_) => return !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()),
            Bson::Document(d) => match d.get(part) {
                Some(v) => cur = v,
                None => return false,
            },
            _ => return false,
        }
    }
    false
}

// --- positional / arrayFilters path expansion ---------------------------
//
// Mirrors `update._expand_path` / `_walk_positional` / `_index_array_filters` /
// `find_positional_matches`. A target path that contains a positional token
// (`$`, `$[]`, `$[ident]`) is expanded against the current document into the
// concrete index paths the operator then writes to.

/// An arrayFilter identifier: begins with a lowercase ASCII letter, then ASCII
/// alphanumerics (mirrors Python's `^[a-z][a-zA-Z0-9]*$`).
fn is_valid_af_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// Every arrayFilter identifier referenced by a `$[<id>]` token in an update
/// path (mirrors `_array_filter_referenced_identifiers`).
fn referenced_af_identifiers(update: &Document) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for value in update.values() {
        if let Bson::Document(payload) = value {
            for path in payload.keys() {
                let mut rest = path.as_str();
                while let Some(start) = rest.find("$[") {
                    let after = &rest[start + 2..];
                    if let Some(end) = after.find(']') {
                        out.insert(after[..end].to_string());
                        rest = &after[end + 1..];
                    } else {
                        break;
                    }
                }
            }
        }
    }
    out
}

/// The ordered, de-duplicated arrayFilter identifiers a filter references — the
/// top-level field name (before the first `.`) of each non-`$` key, recursing
/// through `$and`/`$or`/`$nor` sub-clauses. Mirrors `_extract_af_identifiers`
/// (the `$expr`/no-identifier distinction only matters for Python's exact error
/// code, so the bool isn't tracked here — an empty result defers regardless).
pub fn extract_af_identifiers(f: &Document) -> Vec<String> {
    fn walk(m: &Document, idents: &mut Vec<String>, seen: &mut std::collections::HashSet<String>) {
        for (key, value) in m {
            if key == "$and" || key == "$or" || key == "$nor" {
                if let Bson::Array(subs) = value {
                    for sub in subs {
                        if let Bson::Document(d) = sub {
                            walk(d, idents, seen);
                        }
                    }
                }
            } else if !key.starts_with('$') {
                let ident = key.split('.').next().unwrap_or(key).to_string();
                if seen.insert(ident.clone()) {
                    idents.push(ident);
                }
            }
        }
    }
    let mut idents = Vec::new();
    let mut seen = std::collections::HashSet::new();
    walk(f, &mut idents, &mut seen);
    idents
}

/// Whether the arrayFilters are valid per mongod (mirrors
/// `_validate_array_filters`): each references exactly one identifier (empty /
/// `$expr` / two-or-more all defer), well-formed (bad name defers), unique (dup
/// defers), and used by a `$[id]` path (unused defers). Invalid → the whole
/// update defers so Python raises the exact code. The identifier may nest inside
/// `$and`/`$or`/`$nor`, matching mongod.
fn array_filters_valid(filters: &[Document], update: &Document) -> bool {
    if filters.is_empty() {
        return true;
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut identifiers: Vec<String> = Vec::new();
    for f in filters {
        let found = extract_af_identifiers(f);
        if found.len() != 1 {
            return false; // 0 (empty/$expr) or >=2 distinct -> Python raises (9/224)
        }
        let ident = &found[0];
        if !is_valid_af_ident(ident) {
            return false; // bad identifier -> Python raises code 2
        }
        if !seen.insert(ident.clone()) {
            return false; // duplicate identifier -> Python raises code 9
        }
        identifiers.push(ident.clone());
    }
    let referenced = referenced_af_identifiers(update);
    identifiers.iter().all(|id| referenced.contains(id))
}

/// Map each arrayFilter identifier to its filter document (the identifier may
/// nest inside `$and`/`$or`/`$nor`). First entry wins per identifier, as in Python.
fn index_array_filters(filters: &[Document]) -> HashMap<String, &Document> {
    let mut out: HashMap<String, &Document> = HashMap::new();
    for f in filters {
        for name in extract_af_identifiers(f) {
            out.entry(name).or_insert(f);
        }
    }
    out
}

/// The first matching array index for each top-level array field referenced by a
/// dotted equality clause in `filter` — the resolution for the `$` positional
/// operator. Mirrors `update.find_positional_matches`.
pub fn find_positional_matches(doc: &Document, filter: &Document) -> Document {
    let mut array_paths: HashMap<&str, Document> = HashMap::new();
    for (key, value) in filter {
        if key.starts_with('$') || !key.contains('.') {
            continue;
        }
        let (top, rest) = key.split_once('.').unwrap();
        if matches!(doc.get(top), Some(Bson::Array(_))) {
            array_paths
                .entry(top)
                .or_default()
                .insert(rest.to_string(), value.clone());
        }
    }
    let mut out = Document::new();
    for (path, sub_filter) in array_paths {
        if let Some(Bson::Array(arr)) = doc.get(path) {
            for (i, elem) in arr.iter().enumerate() {
                let elem_doc = match elem {
                    Bson::Document(d) => d.clone(),
                    other => {
                        let mut d = Document::new();
                        d.insert("_", other.clone());
                        d
                    }
                };
                if query::matches(&elem_doc, &sub_filter, &Document::new(), None).unwrap_or(false) {
                    out.insert(path.to_string(), i as i64);
                    break;
                }
            }
        }
    }
    out
}

/// Expand `path` (which may contain positional tokens) into the concrete index
/// paths to write, walking `doc`. Paths with no positional token return as-is.
fn expand_path(
    doc: &Document,
    path: &str,
    filters: &HashMap<String, &Document>,
    pos: &Document,
) -> R<Vec<String>> {
    let parts: Vec<&str> = path.split('.').collect();
    if !parts.iter().any(|p| is_positional_token(p)) {
        return Ok(vec![path.to_string()]);
    }
    let mut out = Vec::new();
    let mut prefix: Vec<String> = Vec::new();
    walk_positional(
        &Bson::Document(doc.clone()),
        &parts,
        &mut prefix,
        &mut out,
        filters,
        pos,
    )?;
    Ok(out)
}

fn walk_positional(
    cur: &Bson,
    remaining: &[&str],
    prefix: &mut Vec<String>,
    out: &mut Vec<String>,
    filters: &HashMap<String, &Document>,
    pos: &Document,
) -> R<()> {
    let Some((head, rest)) = remaining.split_first() else {
        out.push(prefix.join("."));
        return Ok(());
    };
    let head = *head;
    if head == "$" {
        let Bson::Array(arr) = cur else { return Ok(()) };
        let path_so_far = prefix.join(".");
        let idx = pos.get(&path_so_far).and_then(as_int_like);
        let idx = match idx {
            Some(i) if i >= 0 && (i as usize) < arr.len() => i as usize,
            // Unresolvable `$` — Python raises; we defer (server → BadValue).
            _ => return Err(Fallback::Defer),
        };
        prefix.push(idx.to_string());
        walk_positional(&arr[idx], rest, prefix, out, filters, pos)?;
        prefix.pop();
    } else if head == "$[]" {
        let Bson::Array(arr) = cur else { return Ok(()) };
        for (i, elem) in arr.iter().enumerate() {
            prefix.push(i.to_string());
            walk_positional(elem, rest, prefix, out, filters, pos)?;
            prefix.pop();
        }
    } else if head.starts_with("$[") && head.ends_with(']') {
        let name = &head[2..head.len() - 1];
        let Bson::Array(arr) = cur else { return Ok(()) };
        let sub = filters.get(name).ok_or(Fallback::Defer)?;
        for (i, elem) in arr.iter().enumerate() {
            let mut elem_doc = Document::new();
            elem_doc.insert(name.to_string(), elem.clone());
            if query::matches(&elem_doc, sub, &Document::new(), None)? {
                prefix.push(i.to_string());
                walk_positional(elem, rest, prefix, out, filters, pos)?;
                prefix.pop();
            }
        }
    } else if let Bson::Document(d) = cur {
        let child = d.get(head).cloned().unwrap_or(Bson::Null);
        prefix.push(head.to_string());
        walk_positional(&child, rest, prefix, out, filters, pos)?;
        prefix.pop();
    } else if let Bson::Array(a) = cur {
        if head.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(idx) = head.parse::<usize>() {
                if idx < a.len() {
                    prefix.push(head.to_string());
                    walk_positional(&a[idx], rest, prefix, out, filters, pos)?;
                    prefix.pop();
                }
            }
        }
    }
    Ok(())
}

// --- path write helpers (shared impl in crate::paths) -------------------

/// `paths::set_path` with its list-growth-cap error mapped to our `Fallback`.
fn set_path(doc: &mut Document, path: &str, value: Bson) -> R<()> {
    // A dotted path that runs THROUGH a non-document cannot be created --
    // mongod answers `PathNotViable` (28). `paths::set_path` returns silently
    // in that case (as Python's did), so the update reported success and wrote
    // nothing. Defer: the Python engine raises the exact error, and the
    // standalone Rust server names it via `path_not_viable_error`.
    if paths::path_block(doc, path).is_some() {
        return Err(Fallback::Defer);
    }
    paths::set_path(doc, path, value).map_err(|_| Fallback::Defer)
}

use crate::paths::unset_path;

// --- arithmetic ($inc / $mul) -------------------------------------------

/// `current <op> operand` with Python's numeric semantics. `mul=false` adds.
fn arith(current: &Bson, operand: &Bson, mul: bool) -> R<Bson> {
    // A bool `$inc`/`$mul` argument is NOT a number for mongod (it errors with
    // "Cannot increment/multiply with non-numeric argument", code 14) — but
    // `as_int_like` treats bool as 0/1, which would silently compute. Defer so
    // the operand is rejected instead. (The Python server raises code 14; the
    // Rust server surfaces a generic BadValue — the standing update error-code
    // gap. String / null operands already fall through to Fallback below.)
    if matches!(operand, Bson::Boolean(_)) {
        return Err(Fallback::Defer);
    }
    // Decimal dominates the widening order (int32 < int64 < double < decimal),
    // so either side being decimal puts the whole operation in the decimal
    // domain — computed exactly at decimal128's 34 digits, quantum preserved.
    if matches!(current, Bson::Decimal128(_)) || matches!(operand, Bson::Decimal128(_)) {
        let (a, b) = (
            decimal::from_bson(current).ok_or(Fallback::Defer)?,
            decimal::from_bson(operand).ok_or(Fallback::Defer)?,
        );
        let r = if mul {
            decimal::mul(&a, &b)
        } else {
            decimal::add(&a, &b)
        };
        return decimal::to_bson(&r.ok_or(Fallback::Defer)?).ok_or(Fallback::Defer);
    }
    if let (Some(a), Some(b)) = (as_int_like(current), as_int_like(operand)) {
        let r = if mul {
            a.checked_mul(b)
        } else {
            a.checked_add(b)
        };
        // MongoDB promotes the result to int64 if either operand is already
        // int64 (or a 32-bit result would overflow) — matching `numerics.bson_*`.
        let wide = is_int64(current) || is_int64(operand);
        return int_promoted_to_bson(r.ok_or(Fallback::Defer)?, wide).ok_or(Fallback::Defer);
    }
    // Float path: any non-numeric operand (current/operand) makes Python raise.
    let a = as_float_like(current).ok_or(Fallback::Defer)?;
    let b = as_float_like(operand).ok_or(Fallback::Defer)?;
    Ok(Bson::Double(if mul { a * b } else { a + b }))
}

/// Current value of a field for $inc/$mul. A *missing* field is treated as
/// int 0 (mongod applies the delta), but a field present with an explicit
/// `null` is a TypeMismatch (code 14) — mongod refuses to coerce a present
/// non-numeric value to 0. We defer that (and any other present value that
/// `arith` can't handle) to the Python oracle so it raises the exact coded
/// error; the Rust *server* surfaces a generic BadValue (the documented
/// error-code gap).
/// The existing value `$inc` / `$mul` will operate on, or `Fallback`.
///
/// Defers for EVERY non-numeric type, not just null. mongod answers TypeMismatch
/// (14) for `$inc` against a string, bool, array or document; the Python engine
/// raises exactly that, so deferring keeps the two engines in step. Previously
/// only `Null` deferred, which meant a bool reached `arith` and silently
/// incremented (Python treats `bool` as an `int` subclass) — the parity suite
/// caught the divergence the moment the Python side started refusing.
/// A double or Decimal128 zero, of either sign. NOT an int -- an int zero
/// promotes under a double multiplier and then follows IEEE (see `$mul`).
fn is_zero_number(v: &Bson) -> bool {
    match v {
        Bson::Double(d) => *d == 0.0,
        Bson::Decimal128(_) => crate::decimal::from_bson(v)
            .map(|d| d.is_zero())
            .unwrap_or(false),
        _ => false,
    }
}

fn current_or_zero(result: &Document, path: &str) -> R<Bson> {
    match get_path(result, path) {
        None => Ok(Bson::Int32(0)),
        Some(Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)) => {
            Ok(get_path(result, path).expect("just matched").clone())
        }
        // Bson::Boolean lands here deliberately: it is not numeric for arithmetic.
        Some(_) => Err(Fallback::Defer),
    }
}

// --- operator application ----------------------------------------------

fn payload_doc(payload: &Bson) -> R<&Document> {
    match payload {
        Bson::Document(d) => Ok(d),
        _ => Err(Fallback::Defer),
    }
}

/// Apply one `$push` value to `arr`: a plain value is appended; the `{$each: […]}`
/// modifier form appends each element, honouring `$position` and `$slice`. Mirrors
/// the pure `_apply_push`. **`$sort` defers** (Python's BSON-order array sort), as
/// does a non-integer `$position`/`$slice`, a non-array `$each`, or an unknown
/// modifier — the pure engine either sorts or raises there.
fn push_apply(arr: &mut Vec<Bson>, value: &Bson) -> R<()> {
    let m = match value {
        Bson::Document(d) if d.contains_key("$each") => d,
        _ => {
            arr.push(value.clone());
            return Ok(());
        }
    };
    for k in m.keys() {
        match k.as_str() {
            "$each" | "$position" | "$slice" | "$sort" => {}
            _ => return Err(Fallback::Defer), // unknown modifier -> Python raises
        }
    }
    let each = match m.get("$each") {
        Some(Bson::Array(a)) => a,
        // mongod names the type here, and words it differently from the
        // `$addToSet` sibling above: `$push` keeps the colon before the type and
        // answers code 2 where `$addToSet` answers 14. Both verbatim from an
        // 8.2.11 probe (2026-09-06). This used to defer, which on the standalone
        // Rust server told the client the server could not do `$push`.
        Some(v) => {
            return Err(Fallback::mongo(
                2,
                format!(
                    "The argument to $each in $push must be an array but it was of type: {}",
                    crate::query::bson_type_name(v)
                ),
            ));
        }
        None => return Err(Fallback::Defer),
    };
    match m.get("$position") {
        None => arr.extend(each.iter().cloned()),
        Some(p) => {
            // A bool $position is a parse error in mongod (code 2), not index 1
            // — `as_int_like` would coerce it, so guard first.
            if matches!(p, Bson::Boolean(_)) {
                return Err(Fallback::Defer);
            }
            let n = as_int_like(p).ok_or(Fallback::Defer)?;
            let idx = if n >= 0 {
                (n as usize).min(arr.len())
            } else {
                (arr.len() as i128 + n).max(0) as usize
            };
            for (off, e) in each.iter().enumerate() {
                arr.insert(idx + off, e.clone());
            }
        }
    }
    // mongod order: $position insert, then $sort the whole array, then $slice.
    if let Some(spec) = m.get("$sort") {
        push_sort(arr, spec)?;
    }
    if let Some(s) = m.get("$slice") {
        // A bool $slice is a parse error in mongod (code 2), not "keep 1".
        if matches!(s, Bson::Boolean(_)) {
            return Err(Fallback::Defer);
        }
        let n = as_int_like(s).ok_or(Fallback::Defer)?;
        if n == 0 {
            arr.clear();
        } else if n > 0 {
            arr.truncate(n as usize);
        } else {
            let keep = (-n) as usize;
            if arr.len() > keep {
                arr.drain(0..arr.len() - keep);
            }
        }
    }
    Ok(())
}

/// `$push` `$sort`: `1`/`-1` sorts whole elements in BSON order; a `{field: dir}`
/// doc sorts (stably, field-by-field, in reverse spec order) by those paths. Any
/// element outside the sortable subset defers to Python (same `order::cmp` /
/// `is_sortable` contract as `$sortArray`). Mirrors `_push_sort`.
fn push_sort(arr: &mut [Bson], spec: &Bson) -> R<()> {
    // Key an element for a `{field: dir}` sort: a document element keys off the
    // field path (missing -> null); a scalar element keys off itself.
    fn key_of(e: &Bson, field: &str) -> Bson {
        match e {
            Bson::Document(d) => crate::paths::get_path(d, field)
                .cloned()
                .unwrap_or(Bson::Null),
            other => other.clone(),
        }
    }
    // A valid sort direction is exactly 1 or -1, as an int or a whole double
    // (mongod accepts `1.0`); a bool, a fractional double, or any other value
    // defers so Python raises code 2. (`as_int_like` treats bool as 0/1, which
    // would wrongly sort a `{field: true}` direction — so use this stricter form.)
    fn dir_pm1(b: &Bson) -> Option<i128> {
        let v = match b {
            Bson::Int32(n) => *n as i128,
            Bson::Int64(n) => *n as i128,
            Bson::Double(d) if d.fract() == 0.0 => *d as i128,
            _ => return None,
        };
        (v == 1 || v == -1).then_some(v)
    }
    match spec {
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => {
            let dir = dir_pm1(spec).ok_or(Fallback::Defer)?;
            if !arr.iter().all(crate::order::is_sortable) {
                return Err(Fallback::Defer);
            }
            if dir == -1 {
                arr.sort_by(|a, b| crate::order::cmp(b, a));
            } else {
                arr.sort_by(crate::order::cmp);
            }
        }
        Bson::Document(spec_doc) => {
            let fields: Vec<(&String, i128)> = spec_doc
                .iter()
                .map(|(f, d)| dir_pm1(d).map(|di| (f, di)).ok_or(Fallback::Defer))
                .collect::<R<Vec<_>>>()?;
            for (field, _) in &fields {
                if !arr
                    .iter()
                    .all(|e| crate::order::is_sortable(&key_of(e, field)))
                {
                    return Err(Fallback::Defer);
                }
            }
            // Stable field-by-field, applied in reverse spec order (Python parity).
            for (field, dir) in fields.iter().rev() {
                arr.sort_by(|a, b| {
                    let (ka, kb) = (key_of(a, field), key_of(b, field));
                    if *dir == -1 {
                        crate::order::cmp(&kb, &ka)
                    } else {
                        crate::order::cmp(&ka, &kb)
                    }
                });
            }
        }
        _ => return Err(Fallback::Defer), // non-int / non-doc $sort -> Python raises
    }
    Ok(())
}

/// Whether an array element should be removed by `$pull` under mongod's query
/// semantics (verified three-way vs mongod 6.0): a criterion of only
/// `$`-operators is an element-value predicate; any other document criterion is a
/// sub-document match against the element (a scalar element never matches); a
/// scalar criterion is BSON-aware equality. Mirrors `_pull_matches`. A construct
/// the query engine can't evaluate exactly (regex / collation edge) defers.
fn pull_matches(element: &Bson, criterion: &Bson) -> R<bool> {
    match criterion {
        Bson::Document(c) if !c.is_empty() && c.keys().all(|k| k.starts_with('$')) => {
            let d = doc! { "__e": element.clone() };
            let q = doc! { "__e": criterion.clone() };
            crate::query::matches(&d, &q, &Document::new(), None)
        }
        Bson::Document(c) => match element {
            Bson::Document(ed) => crate::query::matches(ed, c, &Document::new(), None),
            _ => Ok(false),
        },
        _ => {
            let d = doc! { "__e": element.clone() };
            let q = doc! { "__e": criterion.clone() };
            crate::query::matches(&d, &q, &Document::new(), None)
        }
    }
}

fn apply_op(
    result: &mut Document,
    op: &str,
    payload: &Bson,
    filters: &HashMap<String, &Document>,
    pos: &Document,
) -> R<()> {
    let payload = payload_doc(payload)?;
    match op {
        "$set" | "$setOnInsert" => {
            for (path, value) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
                    set_path(result, &cpath, value.clone())?;
                }
            }
        }
        "$unset" => {
            for path in payload.keys() {
                for cpath in expand_path(result, path, filters, pos)? {
                    unset_path(result, &cpath);
                }
            }
        }
        "$inc" => {
            for (path, delta) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
                    let cur = current_or_zero(result, &cpath)?;
                    let new = arith(&cur, delta, false)?;
                    set_path(result, &cpath, new)?;
                }
            }
        }
        "$mul" => {
            for (path, factor) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
                    let cur = current_or_zero(result, &cpath)?;
                    let mut new = arith(&cur, factor, true)?;
                    // A stored DOUBLE or DECIMAL zero keeps its own sign:
                    // mongod's `$mul` leaves `0.0` as `0.0` and `-0.0` as
                    // `-0.0` whatever the multiplier, where IEEE would flip it
                    // (`0.0 * -1` is `-0.0`). Measured across 15 shapes on
                    // 8.2.11 (2026-09-05): negative, positive, zero and
                    // non-finite multipliers, over double / int / int64 /
                    // decimal. A non-zero RESULT (`0.0 * inf` is NaN) still
                    // writes, and an INT zero promotes and follows IEEE, so the
                    // rule is narrow -- stored zero, zero result.
                    if is_zero_number(&cur) && is_zero_number(&new) {
                        new = cur.clone();
                    }
                    set_path(result, &cpath, new)?;
                }
            }
        }
        "$push" => {
            for (path, value) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
                    let mut a = match get_path(result, &cpath).cloned() {
                        None | Some(Bson::Null) => Vec::new(),
                        Some(Bson::Array(a)) => a,
                        Some(other) => {
                            return Err(Fallback::mongo(
                                2,
                                format!(
                                    "The field '{cpath}' must be an array but is of type {} \
                                     in document {}",
                                    crate::query::bson_type_name(&other),
                                    render_doc_id(result)
                                ),
                            )
                            .exec());
                        }
                    };
                    push_apply(&mut a, value)?;
                    set_path(result, &cpath, Bson::Array(a))?;
                }
            }
        }
        "$pop" => {
            for (path, dir) in payload {
                // mongod validates the DIRECTION before it looks at the field,
                // through the same numeric ladder as `$size` and the `$bits*`
                // mask. This deferred for every one of those cases, which on
                // the standalone server reads as "$pop is not supported".
                let dir_int = match crate::query::coerce_int64_argument(dir, path) {
                    None => {
                        return Err(Fallback::mongo(
                            9,
                            format!(
                                "Expected a number in: {path}: {}",
                                crate::query::bson_value_repr(dir)
                            ),
                        ));
                    }
                    Some(Err(e)) => return Err(e),
                    Some(Ok(n)) => n,
                };
                if dir_int != 1 && dir_int != -1 {
                    // mongod reports the COERCED integer, not the argument as
                    // written: `Decimal128("1E+2")` is "found: 100", `2.000` is
                    // "found: 2", and `Decimal128("-0")` is "found: 0" -- which
                    // is the shape that caught this, since it is the only one
                    // whose own rendering ("-0") differs from its value.
                    return Err(Fallback::mongo(
                        9,
                        format!("$pop expects 1 or -1, found: {dir_int}"),
                    ));
                }
                for cpath in expand_path(result, path, filters, pos)? {
                    // A PRESENT non-array is an ERROR on mongod ("Path 'a'
                    // contains an element of non-array type 'int'", code 14); a
                    // missing field or an empty array are no-ops. The `if let`
                    // below silently skips all three, so an invalid update
                    // reported success. Defer so Python raises the exact error.
                    if let Some(v) = get_path(result, &cpath) {
                        if !matches!(v, Bson::Array(_)) {
                            return Err(Fallback::mongo(
                                14,
                                format!(
                                    "Path '{cpath}' contains an element of non-array type '{}'",
                                    crate::query::bson_type_name(v)
                                ),
                            )
                            .exec());
                        }
                    }
                    if let Some(Bson::Array(a)) = get_path(result, &cpath) {
                        if a.is_empty() {
                            continue;
                        }
                        let mut a = a.clone();
                        // A bool direction is "not a number" and any value other
                        // than ±1 is "$pop expects 1 or -1" — both mongod errors
                        // (code 9). `as_int_like` would coerce `true` to 1, and
                        // the old `_ => continue` silently no-op'd a bad value;
                        // defer so the Python oracle raises the exact error.
                        if dir_int == 1 {
                            a.pop();
                        } else {
                            a.remove(0);
                        }
                        set_path(result, &cpath, Bson::Array(a))?;
                    }
                }
            }
        }
        "$rename" => {
            for (old, new) in payload {
                let new = match new {
                    // `Code` is a `String` variant's neighbour in BSON but a
                    // distinct type; mongod refuses it, as it does every
                    // non-string.
                    Bson::String(s) => s.as_str(),
                    other => {
                        return Err(Fallback::mongo(
                            2,
                            format!(
                                "The 'to' field for $rename must be a string: {old}: {}",
                                crate::query::bson_value_repr(other)
                            ),
                        ));
                    }
                };
                // $rename doesn't support positional tokens (mongod rejects);
                // defer the rare case to keep semantics exact.
                if has_positional(old) || has_positional(new) {
                    return Err(Fallback::Defer);
                }
                if old.is_empty() || new.is_empty() {
                    return Err(Fallback::mongo(56, "An empty update path is not valid."));
                }
                if old == "_id" || new == "_id" {
                    return Err(Fallback::Defer); // immutable _id -> Python raises
                }
                // mongod validation (Python raises 56 / 2; the Rust server renders
                // BadValue). These previously silently corrupted the array or
                // created a bad field.
                if old.is_empty() || new.is_empty() {
                    return Err(Fallback::Defer); // empty path -> Python raises 56
                }
                if old == new || rename_same_path(old, new) {
                    // Named rather than deferred: on the standalone server a
                    // defer reports "a construct the Rust server does not
                    // support" for an ordinary bad argument. Probed 8.2.11.
                    return Err(Fallback::mongo(
                        2,
                        format!(
                            "The source and target field for $rename must differ: {old}: \"{new}\""
                        ),
                    ));
                }
                if rename_traverses_array(result, old) || rename_traverses_array(result, new) {
                    return Err(Fallback::Defer); // array element -> Python raises 2
                }
                if has_path(result, old) {
                    let value = get_path(result, old).unwrap().clone();
                    unset_path(result, old);
                    set_path(result, new, value)?;
                }
            }
        }
        "$bit" => {
            for (path, ops) in payload {
                // `{field: {and|or|xor: <int mask>, ...}}` — mongod applies every
                // listed operation to the field in order (e.g. (v & X) | Y).
                // mongod separates "not a document" from "an EMPTY document",
                // with two texts. The unbalanced braces in both are its own.
                let ops = match ops {
                    Bson::Document(d) if d.is_empty() => {
                        return Err(Fallback::mongo(
                            2,
                            "You must pass in at least one bitwise operation. The format is: \
                             {$bit: {field: {and/or/xor: #}}",
                        ));
                    }
                    Bson::Document(d) => d,
                    other => {
                        return Err(Fallback::mongo(
                            2,
                            format!(
                                "The $bit modifier is not compatible with a {}. You must pass \
                                 in an embedded document: {{$bit: {{field: {{and/or/xor: #}}}}",
                                crate::query::bson_type_name(other)
                            ),
                        ));
                    }
                };
                let mut parsed: Vec<(&str, i64)> = Vec::with_capacity(ops.len());
                for (bit_op, mask_b) in ops {
                    let op_s = bit_op.as_str();
                    if !matches!(op_s, "and" | "or" | "xor") {
                        return Err(Fallback::mongo(
                            2,
                            format!(
                                "The $bit modifier only supports 'and', 'or', and 'xor', not \
                                 '{op_s}' which is an unknown operator: {{{op_s}: {}}}",
                                crate::query::bson_value_repr(mask_b)
                            ),
                        ));
                    }
                    let mask = match mask_b {
                        Bson::Int32(n) => *n as i64,
                        Bson::Int64(n) => *n,
                        _ => return Err(Fallback::Defer), // non-integer mask -> Python raises
                    };
                    parsed.push((op_s, mask));
                }
                for cpath in expand_path(result, path, filters, pos)? {
                    let mut cur = match get_path(result, &cpath) {
                        None | Some(Bson::Null) => 0i64,
                        Some(Bson::Int32(n)) => *n as i64,
                        Some(Bson::Int64(n)) => *n,
                        Some(other) => {
                            return Err(Fallback::mongo(
                                2,
                                format!(
                                    "Cannot apply $bit to a value of non-integral type.\
                                     _id: {} has the field {cpath} of non-integer type {}",
                                    crate::query::bson_value_repr(
                                        result.get("_id").unwrap_or(&Bson::Null)
                                    ),
                                    crate::query::bson_type_name(other)
                                ),
                            )
                            .exec());
                        }
                    };
                    for (bit_op, mask) in &parsed {
                        cur = match *bit_op {
                            "and" => cur & mask,
                            "or" => cur | mask,
                            _ => cur ^ mask,
                        };
                    }
                    // Python computes a plain int -> bson encodes it as int32 when
                    // it fits, else int64. Match that so the BSON subtype agrees.
                    let val = match i32::try_from(cur) {
                        Ok(v) => Bson::Int32(v),
                        Err(_) => Bson::Int64(cur),
                    };
                    set_path(result, &cpath, val)?;
                }
            }
        }
        "$min" | "$max" => {
            let want_less = op == "$min"; // $min sets when value < current
            for (path, value) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
                    // A *missing* field is set unconditionally; a present field
                    // (incl. an explicit null, rank 2) is compared by MongoDB's
                    // SORT order, which is what `$min` / `$max` follow.
                    //
                    // NOT `order::bson_lt`, whose comment used to claim it
                    // "covers NaN": it keeps IEEE semantics, where every NaN
                    // comparison is false. mongod's sort order places NaN BELOW
                    // -Infinity, so `{$min: {a: NaN}}` over `a: 5` sets the
                    // field — and this left it untouched, a wrong value in a
                    // WRITE path (probed 8.2.11, 2026-09-03).
                    //
                    // The two orders genuinely differ and both are mongod's:
                    // the RANGE operators exclude NaN entirely, while sorting
                    // places it first. `sortkey::encode_value` already encodes
                    // that correctly, so this defers to it rather than
                    // re-deriving the rule.
                    let should_set = match get_path(result, &cpath) {
                        None => true,
                        Some(current) => {
                            let (lhs, rhs) = if want_less {
                                (value, current)
                            } else {
                                (current, value)
                            };
                            match (
                                crate::sortkey::encode_value(lhs, None),
                                crate::sortkey::encode_value(rhs, None),
                            ) {
                                (Ok(a), Ok(b)) => a < b,
                                // Anything the sort key cannot encode falls
                                // back to the comparison order, as before.
                                _ => match crate::order::bson_lt(lhs, rhs) {
                                    Some(l) => l,
                                    None => return Err(Fallback::Defer),
                                },
                            }
                        }
                    };
                    if should_set {
                        set_path(result, &cpath, value.clone())?;
                    }
                }
            }
        }
        "$addToSet" => {
            for (path, value) in payload {
                // `$each` adds each element (deduped); otherwise the value itself.
                let items: Vec<Bson> = match value {
                    Bson::Document(d) if d.contains_key("$each") => match d.get("$each") {
                        Some(Bson::Array(a)) => a.clone(),
                        other => {
                            return Err(Fallback::mongo(
                                14,
                                format!(
                                    "The argument to $each in $addToSet must be an array but \
                                     it was of type {}",
                                    crate::query::bson_type_name(other.unwrap_or(&Bson::Null))
                                ),
                            ));
                        }
                    },
                    _ => vec![value.clone()],
                };
                for cpath in expand_path(result, path, filters, pos)? {
                    let mut a = match get_path(result, &cpath).cloned() {
                        None | Some(Bson::Null) => Vec::new(),
                        Some(Bson::Array(a)) => a,
                        Some(other) => {
                            return Err(Fallback::mongo(
                                2,
                                format!(
                                    "Cannot apply $addToSet to non-array field. Field named \
                                     '{cpath}' has non-array type {}",
                                    crate::query::bson_type_name(&other)
                                ),
                            )
                            .exec());
                        }
                    };
                    for item in &items {
                        let mut present = false;
                        for e in &a {
                            if addtoset_eq(e, item)? {
                                present = true;
                                break;
                            }
                        }
                        if !present {
                            a.push(item.clone());
                        }
                    }
                    set_path(result, &cpath, Bson::Array(a))?;
                }
            }
        }
        "$pull" => {
            for (path, criterion) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
                    // Remove elements matching the criterion under query semantics
                    // (element-value predicate / sub-document match / equality). A
                    // missing field is a no-op; a present but non-array target
                    // defers so Python raises code 2.
                    match get_path(result, &cpath).cloned() {
                        Some(Bson::Array(a)) => {
                            let mut kept = Vec::with_capacity(a.len());
                            for e in a {
                                if !pull_matches(&e, criterion)? {
                                    kept.push(e);
                                }
                            }
                            set_path(result, &cpath, Bson::Array(kept))?;
                        }
                        Some(_) => {
                            // Both $pull and $pullAll report it as $pull.
                            return Err(Fallback::mongo(
                                2,
                                "Cannot apply $pull to a non-array value",
                            )
                            .exec());
                        }
                        None => {}
                    }
                }
            }
        }
        "$pullAll" => {
            for (path, values) in payload {
                let Bson::Array(vals) = values else {
                    return Err(Fallback::Defer); // non-array arg -> Python raises
                };
                for cpath in expand_path(result, path, filters, pos)? {
                    // Remove every element equal to any listed value (literal
                    // equality, not predicates). Missing field: no-op; present
                    // non-array target defers so Python raises code 2.
                    match get_path(result, &cpath).cloned() {
                        Some(Bson::Array(a)) => {
                            let mut kept = Vec::with_capacity(a.len());
                            for e in a {
                                let mut drop = false;
                                for v in vals {
                                    if expressions::py_eq(&e, v)? {
                                        drop = true;
                                        break;
                                    }
                                }
                                if !drop {
                                    kept.push(e);
                                }
                            }
                            set_path(result, &cpath, Bson::Array(kept))?;
                        }
                        Some(_) => {
                            // Both $pull and $pullAll report it as $pull.
                            return Err(Fallback::mongo(
                                2,
                                "Cannot apply $pull to a non-array value",
                            )
                            .exec());
                        }
                        None => {}
                    }
                }
            }
        }
        // $currentDate (non-deterministic) and unknown ops -> Python.
        _ => return Err(Fallback::Defer),
    }
    Ok(())
}

/// Apply an operator/replacement update document. `Err(Fallback::Defer)` => defer to
/// the pure-Python `apply_update` (which also raises the right errors).
pub fn apply_update(doc: &Document, update: &Document, is_upsert: bool) -> R<Document> {
    apply_update_with(doc, update, is_upsert, &[], &Document::new())
}

/// Like [`apply_update`] but with `arrayFilters` (the `$[ident]` filter docs) and
/// `positional_matches` (the `$` resolution from the query, via
/// [`find_positional_matches`]) threaded in, so positional update operators
/// (`$` / `$[]` / `$[ident]`) resolve to concrete array indices. Mirrors
/// `update.apply_update(..., array_filters=, positional_matches=)`.
pub fn apply_update_with(
    doc: &Document,
    update: &Document,
    is_upsert: bool,
    array_filters: &[Document],
    positional_matches: &Document,
) -> R<Document> {
    // NO empty short-circuit: `update: {}` is a replacement with an empty
    // document, so the stored doc is reduced to its `_id` (probed mongod
    // 6.0.16, which reports `nModified: 1` for it). Returning `doc.clone()`
    // here silently kept every field the client asked to drop. An empty
    // *pipeline* is the genuine no-op, and is a different entry point.
    if !array_filters_valid(array_filters, update) {
        return Err(Fallback::Defer); // invalid arrayFilters -> Python raises the exact code
    }
    // An update whose operators touch overlapping paths is rejected by mongod
    // (code 40) rather than applied. Defer so the Python engine raises the exact
    // error; the Rust server names it via `path_conflict_error` (it has no
    // Python to fall back to).
    match update_path_fault(update) {
        // A path conflict still defers: the Python engine raises the exact
        // error, and on the Rust server `path_conflict_error` recovers it in
        // the storage layer. The other two name themselves -- a defer has no
        // Python behind it there. All are PARSE errors, so they stay bare (no
        // executor wrapper).
        Some(UpdatePathFault::Conflict { .. }) => return Err(Fallback::Defer),
        Some(_) => {
            let (code, message) = update_spec_error(update).expect("a fault was just observed");
            return Err(Fallback::mongo(code, message));
        }
        None => {}
    }
    let has_op = is_operator_form(update);
    if has_op {
        if !update.keys().all(|k| k.starts_with('$')) {
            return Err(Fallback::Defer); // mixing operators with fields -> Python raises
        }
        let filters = index_array_filters(array_filters);
        let mut result = doc.clone();
        for (op, payload) in update {
            if op == "$setOnInsert" && !is_upsert {
                continue;
            }
            apply_op(&mut result, op, payload, &filters, positional_matches)?;
        }
        // _id is immutable: a changed _id is an error (let Python raise).
        if let Some(orig) = doc.get("_id") {
            if result.get("_id") != Some(orig) {
                return Err(Fallback::Defer);
            }
        }
        Ok(result)
    } else {
        // A `$`-prefixed TOP-LEVEL key in a replacement is mongod's
        // `DollarPrefixedFieldName` (52), and it is an EXECUTION-time error, not
        // a parse-time one: with no matching document the statement is a silent
        // no-op (`n: 0`), and an UPSERT inserts the document verbatim, `$`-key
        // and all (probed 8.2.11, 2026-09-06). So it fires only on a real
        // replacement, which `is_upsert` distinguishes -- the upsert path calls
        // us with the seed document it is about to insert.
        if !is_upsert {
            if let Some(field) = replacement_dollar_field(update) {
                return Err(Fallback::mongo(52, replacement_dollar_error(field)).exec());
            }
        }
        // Replacement-style: the update is the new doc, with _id preserved.
        let mut new = update.clone();
        if let Some(orig) = doc.get("_id") {
            match new.get("_id") {
                Some(v) if v != orig => return Err(Fallback::Defer), // changed _id -> Python raises
                _ => {
                    // `_id` leads the stored document, as it does in mongod.
                    // `insert` on a Document APPENDS when the key is absent,
                    // and BSON preserves field order on the wire, so the
                    // client got back bytes that differed from mongod's.
                    // mongo-php-library's CodecCollectionFunctionalTest
                    // compares raw BSON and caught exactly that.
                    let mut ordered = Document::new();
                    ordered.insert("_id".to_string(), orig.clone());
                    for (k, v) in new.iter() {
                        if k != "_id" {
                            ordered.insert(k.clone(), v.clone());
                        }
                    }
                    new = ordered;
                }
            }
        }
        Ok(new)
    }
}

/// Returns the offending path and the conflict point when two operators target
/// overlapping paths.
///
/// mongod refuses an update whose operators touch paths that are equal, or where
/// one is a prefix of the other: `{$set: {a: 2}, $inc: {"a.b": 1}}` cannot be
/// applied because `$set` replaces the very subtree `$inc` wants to walk into.
/// Siblings and disjoint paths are fine. Mirrors
/// `update._conflicting_update_paths`.
///
/// `$rename` claims BOTH ends -- it writes one and removes the other -- except
/// when they are equal, which mongod reports with its own dedicated error.
pub fn conflicting_update_paths(update: &Document) -> Option<(String, String)> {
    match update_path_fault(update) {
        Some(UpdatePathFault::Conflict { offending, at }) => Some((offending, at)),
        _ => None,
    }
}

/// Set `value` at a dotted `path`, creating the intermediate documents.
///
/// The public face of `paths::set_path`, which the storage layer needs when it
/// seeds an upsert from the filter's equalities: mongod builds the nesting, so
/// `{"a.b.c": 5}` seeds `{a: {b: {c: 5}}}` and NOT a literal dotted key. A
/// literal one is a document mongod cannot produce, and it does not match the
/// query that created it -- which made the same upsert, run twice, insert two
/// documents.
///
/// Returns `Fallback::Defer` for a path the pure engine will not build (only
/// the list-growth cap), rather than `paths`' bare `Result<(), ()>`.
pub fn set_document_path(doc: &mut Document, path: &str, value: Bson) -> R<()> {
    paths::set_path(doc, path, value).map_err(|()| Fallback::Defer)
}

/// Is this an operator update, or a replacement document?
///
/// mongod decides on the **first key alone** (probed 8.2.11, 2026-09-06), and
/// then complains in that form's vocabulary:
///
/// ```text
/// {$set: {a: 1}, z: 2}   ->  9  Unknown modifier: z
/// {z: 2, $set: {a: 1}}   -> 52  The dollar ($) prefixed field '$set' ... is not
///                               allowed in the context of an update's
///                               replacement document.
/// ```
///
/// This used to ask `keys().any(|k| k.starts_with('$'))`, which made the second
/// one an operator update too and answered 9 for it. An empty update is a
/// replacement (of nothing), which is how `{}` reduces a document to its `_id`.
pub fn is_operator_form(update: &Document) -> bool {
    update.keys().next().is_some_and(|k| k.starts_with('$'))
}

/// The FIRST top-level `$`-prefixed key of a replacement document.
///
/// Only the TOP level: mongod 8.x stores `{a: {$bad: 1}}` and `{a: [{$bad: 1}]}`
/// happily, and stores a dotted key like `{"a.b": 1}` literally too. Probed
/// 8.2.11 (2026-09-06).
pub fn replacement_dollar_field(update: &Document) -> Option<&str> {
    update
        .keys()
        .find(|k| k.starts_with('$'))
        .map(String::as_str)
}

/// mongod's `DollarPrefixedFieldName` (52) message for a replacement document.
pub fn replacement_dollar_error(field: &str) -> String {
    format!(
        "The dollar ($) prefixed field '{field}' in '{field}' is not allowed in the \
         context of an update's replacement document. Consider using an aggregation \
         pipeline with $replaceWith."
    )
}

/// What is wrong with an update's operator paths, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePathFault {
    /// A path is empty, or has an empty component. mongod's `EmptyFieldName`
    /// (56), carrying the message it uses for this shape.
    Empty(String),
    /// Two operators target overlapping paths -- mongod's code 40.
    Conflict { offending: String, at: String },
    /// A top-level key that is not an update modifier mongod knows -- either an
    /// unrecognised `$`-operator or a bare field among the operators. mongod
    /// has ONE message for both and names the offending key without a `$` for
    /// the bare one. Its `FailedToParse` (9).
    UnknownModifier(String),
}

/// Update modifiers mongod accepts. Mirrors `secantus.update._KNOWN_UPDATE_OPS`
/// and the `KNOWN_UPDATE_OPS` the command crate used to keep privately.
pub const KNOWN_UPDATE_OPS: [&str; 15] = [
    "$set",
    "$setOnInsert",
    "$unset",
    "$currentDate",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$push",
    "$addToSet",
    "$pull",
    "$pullAll",
    "$pop",
    "$rename",
    "$bit",
];

/// The `(code, message)` mongod answers for a malformed update SPEC, or `None`
/// if the spec's operators and paths are well formed. The parse-time half of
/// update validation: mongod reports every one of these even when the filter
/// matches nothing (probed 8.2.11, 2026-09-06), so the command layer runs it
/// before going near a document.
pub fn update_spec_error(update: &Document) -> Option<(i32, String)> {
    match update_path_fault(update)? {
        UpdatePathFault::UnknownModifier(k) => Some((
            9,
            format!(
                "Unknown modifier: {k}. Expected a valid update modifier or \
                 pipeline-style update specified as an array"
            ),
        )),
        UpdatePathFault::Empty(message) => Some((56, message)),
        UpdatePathFault::Conflict { offending, at } => Some((
            40,
            format!("Updating the path '{offending}' would create a conflict at '{at}'"),
        )),
    }
}

/// The FIRST thing wrong with an update's operator paths, in document order.
///
/// The empty-path and conflict checks share one walk because mongod interleaves
/// them and the first offender wins (probed 8.2.11, 2026-09-06):
///
/// ```text
/// {$inc: {"": 1},  $set: {a: 1, "a.b": 1}}   -> 56, the empty path
/// {$set: {a: 1, "a.b": 1},  $inc: {"": 1}}   -> 40, the conflict
/// ```
///
/// Running emptiness as a separate earlier pass answers 56 for both.
///
/// The empty-path half is new in 2026-09: before it, BOTH servers accepted
/// `{$set: {"": 1}}` and stored a document with an empty field name -- one
/// mongod cannot produce, and which the query that created it then fails to
/// match. That is the "user-supplied path used as a dict key" shape
/// `CLAUDE.md` calls out.
pub fn update_path_fault(update: &Document) -> Option<UpdatePathFault> {
    if !is_operator_form(update) {
        // A replacement's fields are DATA, not paths -- including any
        // `$`-prefixed one, whose refusal is execution-time (see
        // `replacement_dollar_field`), not parse-time.
        return None;
    }
    let mut seen: Vec<Vec<String>> = Vec::new();
    for (op, payload) in update.iter() {
        // The operator's NAME is checked before its paths, and a bare field
        // among the operators is reached in document order like anything else:
        // `{$nope: {a: 1}, $set: {"": 1}}` is 9 and `{$set: {"": 1}, z: 2}` is
        // 56. Checking names in a pass of their own gets the second one wrong.
        if !op.starts_with('$') || !KNOWN_UPDATE_OPS.contains(&op.as_str()) {
            return Some(UpdatePathFault::UnknownModifier(op.clone()));
        }
        let Bson::Document(fields) = payload else {
            continue;
        };
        for (field, value) in fields.iter() {
            let mut paths = vec![field.clone()];
            if op == "$rename" {
                if let Some(dest) = value.as_str() {
                    if dest != field {
                        paths.push(dest.to_string());
                    }
                }
            }
            // Check every path of this field against paths claimed EARLIER, then
            // claim them all. A `$rename`'s source and destination must not be
            // compared with each other: mongod gives an overlapping pair its own
            // error ("must not be on the same path", code 2), not a code-40
            // conflict.
            let mut claimed: Vec<Vec<String>> = Vec::new();
            for path in paths {
                if path.is_empty() {
                    return Some(UpdatePathFault::Empty(
                        "An empty update path is not valid.".to_string(),
                    ));
                }
                let parts: Vec<String> = path.split('.').map(str::to_string).collect();
                if parts.iter().any(String::is_empty) {
                    return Some(UpdatePathFault::Empty(format!(
                        "The update path '{path}' contains an empty field name, \
                         which is not allowed."
                    )));
                }
                for prev in &seen {
                    let n = parts.len().min(prev.len());
                    if parts[..n] == prev[..n] {
                        let shorter = if prev.len() <= parts.len() {
                            prev.join(".")
                        } else {
                            parts.join(".")
                        };
                        return Some(UpdatePathFault::Conflict {
                            offending: path,
                            at: shorter,
                        });
                    }
                }
                claimed.push(parts);
            }
            seen.extend(claimed);
        }
    }
    None
}

/// mongod's message for a path conflict, if this update has one.
pub fn path_conflict_error(update: &Document) -> Option<String> {
    conflicting_update_paths(update).map(|(offending, at)| {
        format!("Updating the path '{offending}' would create a conflict at '{at}'")
    })
}

/// The exact mongod error for a `$inc` / `$mul` the engine refused on type
/// grounds, or `None` if this update fails for some other (deferrable) reason.
///
/// The engine's `Fallback` is deliberately opaque — it means "run the Python
/// engine", which is right on the Python server but useless on the standalone
/// Rust server, where a defer has nowhere to go and surfaced as a generic
/// `BadValue` (2). mongod answers `TypeMismatch` (14) here. So this mirrors the
/// `query::json_schema_keyword_error` pattern: a standalone validator that
/// names the errors we *can* name, leaving `Fallback` for the ones we can't.
///
/// Messages are verbatim from a mongod 6.0.16 probe and match
/// `secantus.update`'s wording exactly, both shapes:
///
/// * non-numeric operand — `Cannot increment with non-numeric argument: {n: "x"}`
/// * non-numeric field   — `Cannot apply $inc to a value of non-numeric type.
///   {_id: 1} has the field 'n' of non-numeric type string`
pub fn arith_type_error(doc: &Document, update: &Document) -> Option<String> {
    for (op, payload) in update.iter() {
        let verb = match op.as_str() {
            "$inc" => "increment",
            "$mul" => "multiply",
            _ => continue,
        };
        let Bson::Document(fields) = payload else {
            continue;
        };
        for (path, operand) in fields.iter() {
            // mongod validates the whole update before touching a document, so
            // the operand check fires first and wins over the field check.
            if !is_arith_numeric(operand) {
                return Some(format!(
                    "Cannot {verb} with non-numeric argument: {{{path}: {}}}",
                    render_scalar(operand)
                ));
            }
            // Positional / arrayFilter paths expand per document; leave those to
            // the normal defer rather than guess at the concrete path.
            if path.contains("$[") || path.contains(".$") {
                continue;
            }
            // A *missing* field is fine — mongod treats it as 0 and applies the
            // delta. Only a present, non-numeric field is a TypeMismatch.
            if let Some(current) = get_path(doc, path) {
                if !is_arith_numeric(current) {
                    let leaf = path.rsplit('.').next().unwrap_or(path);
                    return Some(format!(
                        "Cannot apply {op} to a value of non-numeric type. \
                         {} has the field '{leaf}' of non-numeric type {}",
                        render_doc_id(doc),
                        query::bson_type_name(current)
                    ));
                }
            }
        }
    }
    None
}

/// The exact mongod error for an `$inc` / `$mul` that overflows int64, or
/// `None` if this update fails for some other reason.
///
/// Same "re-run a narrower check" shape as [`arith_type_error`], and for the
/// same reason: the overflow is discovered deep inside `arith`, which knows
/// neither the operator name nor the document's `_id`, and mongod's message
/// names both. Without this the site could only `Fallback::Defer`, and a defer
/// on the standalone Rust server has no Python behind it -- so five real
/// overflow shapes told the client
/// `query uses a construct the Rust server does not support`, i.e. that the
/// server cannot do `$inc`, when it can and it was the RESULT that did not fit.
///
/// mongod fails the write here rather than widening to a double the way the
/// *aggregation* operators do. Message verbatim from a mongod 8.2.11 probe
/// (2026-09-06) -- `Failed to apply $inc operations to current value
/// ((NumberLong)9223372036854775807) for document {_id: 1}` -- and identical to
/// `secantus.update._arith_or_overflow`'s.
pub fn arith_overflow_error(doc: &Document, update: &Document) -> Option<String> {
    for (op, payload) in update.iter() {
        let mul = match op.as_str() {
            "$inc" => false,
            "$mul" => true,
            _ => continue,
        };
        let Bson::Document(fields) = payload else {
            continue;
        };
        for (path, operand) in fields.iter() {
            // Positional / arrayFilter paths expand per document; leave those to
            // the normal defer rather than guess at the concrete path.
            if path.contains("$[") || path.contains(".$") {
                continue;
            }
            // A missing field is an implicit int 0, which cannot overflow.
            let Some(current) = get_path(doc, path) else {
                continue;
            };
            // Decimal128 has its own (much wider) domain and its own path in
            // `arith`; only the integral one can overflow into this message.
            if matches!(current, Bson::Decimal128(_)) || matches!(operand, Bson::Decimal128(_)) {
                continue;
            }
            let (Some(a), Some(b)) = (as_int_like(current), as_int_like(operand)) else {
                continue;
            };
            let r = if mul {
                a.checked_mul(b)
            } else {
                a.checked_add(b)
            };
            let wide = is_int64(current) || is_int64(operand);
            // `None` from either step is the overflow: past i128 (unreachable
            // from two BSON integers, but cheap to be exact about) or past the
            // int64 the result must be encoded into.
            if r.is_none() || int_promoted_to_bson(r.unwrap(), wide).is_none() {
                return Some(format!(
                    "Failed to apply {op} operations to current value ({}) for document {}",
                    render_arith_operand(current),
                    render_doc_id(doc)
                ));
            }
        }
    }
    None
}

/// The type-tagged rendering mongod puts in an overflow message:
/// `(NumberLong)9223372036854775807`. Only a long can reach it -- an int32 that
/// outgrows its width widens to long rather than overflowing.
fn render_arith_operand(v: &Bson) -> String {
    match v {
        Bson::Int64(n) => format!("(NumberLong){n}"),
        _ => render_scalar(v),
    }
}

/// The exact mongod error for an update that would create a field under a
/// non-document, or `None` if this update fails for some other reason.
///
/// Same purpose as [`arith_type_error`]: a bare `Fallback` becomes a generic
/// `BadValue` (2) on the standalone Rust server, where mongod answers
/// `PathNotViable` (28). Message verbatim from a mongod 6.0.16 probe —
/// `Cannot create field 'x' in element {n: 5}`, naming the component that
/// cannot be created and the thing standing in its way.
///
/// `$unset` is skipped: it does not create, and mongod lets it walk a
/// non-viable path as a no-op.
pub fn path_not_viable_error(doc: &Document, update: &Document) -> Option<String> {
    for (op, payload) in update.iter() {
        if op == "$unset" || !op.starts_with('$') {
            continue;
        }
        let Bson::Document(fields) = payload else {
            continue;
        };
        for path in fields.keys() {
            // Positional / arrayFilter paths expand per document; leave those
            // to the normal defer rather than guess at the concrete path.
            if path.contains("$[") || path.contains(".$") {
                continue;
            }
            if let Some((key, container, field)) = paths::path_block(doc, path) {
                let element = match key {
                    Some(k) => format!("{{{k}: {}}}", render_value(container)),
                    None => "{}".to_string(),
                };
                return Some(format!(
                    "Cannot create field '{field}' in element {element}"
                ));
            }
        }
    }
    None
}

/// `render_scalar` extended to arrays and sub-documents. mongod spaces the
/// brackets — `[ 1 ]`, `{ a: 1 }` — in the `PathNotViable` message.
fn render_value(v: &Bson) -> String {
    match v {
        Bson::Document(d) if d.is_empty() => "{}".to_string(),
        Bson::Document(d) => {
            let inner: Vec<String> = d
                .iter()
                .map(|(k, x)| format!("{k}: {}", render_value(x)))
                .collect();
            format!("{{ {} }}", inner.join(", "))
        }
        Bson::Array(a) if a.is_empty() => "[]".to_string(),
        Bson::Array(a) => {
            let inner: Vec<String> = a.iter().map(render_value).collect();
            format!("[ {} ]", inner.join(", "))
        }
        _ => render_scalar(v),
    }
}

/// mongod's numeric domain for `$inc` / `$mul`: int32 / int64 / double /
/// decimal. Bool is deliberately excluded — mongod refuses `$inc` by `true`.
fn is_arith_numeric(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

/// A scalar as mongod renders it inside an error message.
/// mongod's rendering of an offending value. This used to be a partial copy
/// that fell through to Rust's `Debug` for everything it did not name, so an
/// array printed `Array([])` where mongod prints `[]` and a document printed
/// its `Debug` form. It is now the one canonical renderer -- the same
/// consolidation the Python side needed, where FIVE copies had accumulated.
fn render_scalar(v: &Bson) -> String {
    crate::query::bson_value_repr(v)
}

/// The `{_id: …}` prefix mongod puts in the non-numeric-field message. It is the
/// *document's* `_id`, not the field being incremented.
fn render_doc_id(doc: &Document) -> String {
    match doc.get("_id") {
        Some(id) => format!("{{_id: {}}}", render_scalar(id)),
        None => "{}".to_string(),
    }
}

/// mongod's `$addToSet` membership equality.
///
/// This used to defer to the Python engine whenever a document or a bool was
/// involved, on the strength of a comment saying `py_eq` "mirrors Python's
/// `==`". `py_eq` has since grown both rules -- bool is its own BSON type, and
/// Code compares by code text -- so the only thing left to add here is document
/// and array field ORDER. Deferring is not free on the standalone server, where
/// there is no Python behind the fallback: an `$addToSet` of a bool or a
/// document answered `BadValue` instead of updating.
///
/// Probed against mongod 8.2.11 (2026-09-01):
///   * numerics unify across the width: `1`, `1.0`, `Int64(1)` and
///     `Decimal128("1")` all dedup against each other;
///   * a bool is its OWN type -- `true` into `[1]` appends, `false` into `[0]`
///     appends;
///   * documents compare field-ORDER-sensitively and RECURSIVELY -- `{y:2,x:1}`
///     is appended alongside `{x:1,y:2}`, and so is `{d:{y:2,x:1}}` beside
///     `{d:{x:1,y:2}}`;
///   * arrays are order-sensitive -- `[2,1]` appends beside `[1,2]`;
///   * `Code("ab")` and the string `"ab"` are different values;
///   * regexes compare by pattern and option SET.
fn addtoset_eq(a: &Bson, b: &Bson) -> Result<bool, Fallback> {
    match (a, b) {
        (Bson::Document(x), Bson::Document(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            for ((ka, va), (kb, vb)) in x.iter().zip(y.iter()) {
                if ka != kb || !addtoset_eq(va, vb)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Bson::Array(x), Bson::Array(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            for (ea, eb) in x.iter().zip(y.iter()) {
                if !addtoset_eq(ea, eb)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Bson::RegularExpression(x), Bson::RegularExpression(y)) => {
            Ok(crate::regexutil::regex_eq(x, y))
        }
        _ => expressions::py_eq(a, b),
    }
}

#[cfg(test)]
mod tests {
    // --- the operator-vs-replacement form decision. mongod takes the FIRST
    // key alone and then complains in that form's vocabulary (probed 8.2.11,
    // 2026-09-06). ---

    #[test]
    fn the_first_key_decides_the_form() {
        for (update, operator_form) in [
            (doc! {"$set": {"a": 1i32}}, true),
            (doc! {"$set": {"a": 1i32}, "z": 2i32}, true),
            (doc! {"z": 2i32, "$set": {"a": 1i32}}, false),
            (doc! {"y": 1i32, "z": 2i32, "$set": {"a": 1i32}}, false),
            (doc! {"_id": 1i32, "$set": {"a": 1i32}}, false),
            (doc! {"a.b": 1i32, "$set": {"a": 1i32}}, false),
            (doc! {"a": 9i32}, false),
            // An empty update is a replacement of nothing -- that is how `{}`
            // reduces a stored document to its `_id`.
            (Document::new(), false),
        ] {
            assert_eq!(
                super::is_operator_form(&update),
                operator_form,
                "{update:?}"
            );
        }
    }

    /// A replacement's `$`-prefixed TOP-LEVEL key is `DollarPrefixedFieldName`
    /// (52), and the FIRST such key is the one mongod names.
    #[test]
    fn a_dollar_key_in_a_replacement_is_refused() {
        for (update, named) in [
            (doc! {"z": 2i32, "$set": {"a": 1i32}}, "$set"),
            (doc! {"z": 2i32, "$weird": 3i32}, "$weird"),
            (doc! {"z": 1i32, "$aaa": 1i32, "$bbb": 2i32}, "$aaa"),
            (doc! {"z": 1i32, "$aaa": 1i32, "y": 2i32}, "$aaa"),
            (doc! {"_id": 1i32, "$set": {"a": 1i32}}, "$set"),
        ] {
            let err = super::apply_update(&doc! {"_id": 1i32, "a": 0i32}, &update, false)
                .expect_err("a $-prefixed replacement key is refused");
            assert_eq!(
                err.as_mongo(),
                Some((52, super::replacement_dollar_error(named).as_str())),
                "{update:?}"
            );
            assert!(err.is_exec(), "mongod wraps this one: {update:?}");
        }
    }

    /// Only the TOP level: mongod 8.x stores a nested `$`-key, and a literal
    /// dotted key, verbatim.
    #[test]
    fn only_the_top_level_of_a_replacement_is_restricted() {
        for update in [
            doc! {"a": {"$bad": 1i32}},
            doc! {"a": {"b": {"$bad": 1i32}}},
            doc! {"a": [{"$bad": 1i32}]},
            doc! {"a.b": 1i32},
            doc! {"a": 9i32},
        ] {
            let out = super::apply_update(&doc! {"_id": 1i32, "a": 0i32}, &update, false)
                .unwrap_or_else(|e| panic!("{update:?} must be stored, got {e:?}"));
            let mut want = doc! {"_id": 1i32};
            for (k, v) in update.iter() {
                want.insert(k.clone(), v.clone());
            }
            assert_eq!(out, want, "{update:?}");
        }
    }

    /// The 52 is EXECUTION-time, so the upsert-insert path must not raise it --
    /// mongod inserts the document verbatim, `$`-key and all.
    #[test]
    fn an_upsert_inserts_the_replacement_verbatim() {
        let out = super::apply_update(
            &doc! {"_id": 99i32},
            &doc! {"z": 2i32, "$set": {"a": 1i32}},
            true,
        )
        .expect("the upsert-insert path does not apply the replacement check");
        assert_eq!(out, doc! {"_id": 99i32, "z": 2i32, "$set": {"a": 1i32}});
        // Field order is mongod's: `_id` first, then the document as sent.
        assert_eq!(out.keys().collect::<Vec<_>>(), vec!["_id", "z", "$set"]);
    }

    /// The operator-form complaint stays a PARSE error: it is reported with no
    /// matching document and on an upsert, unlike the replacement-form 52.
    #[test]
    fn operator_form_still_names_the_first_bare_key() {
        assert_eq!(
            super::update_spec_error(&doc! {"$set": {"a": 1i32}, "y": 1i32, "z": 2i32})
                .map(|(c, m)| (c, m.contains("Unknown modifier: y"))),
            Some((9, true))
        );
        // A replacement is not a spec error at all -- its refusal comes later.
        assert_eq!(
            super::update_spec_error(&doc! {"z": 2i32, "$set": {"a": 1i32}}),
            None
        );
    }

    // --- update_spec_error: the three parse faults and their ORDER. Every
    // verdict measured against mongod 8.2.11 (2026-09-06). ---

    #[test]
    fn an_empty_update_path_is_rejected_for_every_operator() {
        for (op, value) in [
            ("$set", Bson::Int32(1)),
            ("$unset", Bson::String(String::new())),
            ("$inc", Bson::Int32(1)),
            ("$mul", Bson::Int32(1)),
            ("$min", Bson::Int32(1)),
            ("$max", Bson::Int32(1)),
            ("$push", Bson::Int32(1)),
            ("$addToSet", Bson::Int32(1)),
            ("$pop", Bson::Int32(1)),
            ("$bit", Bson::Document(doc! {"and": 1i32})),
        ] {
            let update = doc! {op: {"": value}};
            assert_eq!(
                super::update_spec_error(&update),
                Some((56, "An empty update path is not valid.".to_string())),
                "{op} with an empty path"
            );
        }
    }

    #[test]
    fn an_empty_path_component_is_named_wherever_it_sits() {
        for path in ["a.", ".a", "a..b", "a.b.", ".", ".."] {
            let update = doc! {"$set": {path: 1i32}};
            assert_eq!(
                super::update_spec_error(&update),
                Some((
                    56,
                    format!(
                        "The update path '{path}' contains an empty field name, \
                         which is not allowed."
                    )
                )),
                "path {path}"
            );
        }
    }

    #[test]
    fn rename_validates_both_of_its_ends() {
        assert_eq!(
            super::update_spec_error(&doc! {"$rename": {"a": "b."}})
                .unwrap()
                .0,
            56
        );
        assert_eq!(
            super::update_spec_error(&doc! {"$rename": {"a.": "b"}})
                .unwrap()
                .0,
            56
        );
        assert_eq!(
            super::update_spec_error(&doc! {"$rename": {"": "b"}}),
            Some((56, "An empty update path is not valid.".to_string()))
        );
    }

    /// The three parse checks share ONE document-order walk, and the first
    /// offender wins. Each pair below is the same two faults in both orders;
    /// running any check as a separate earlier pass gets one of them backwards.
    #[test]
    fn the_first_parse_fault_in_document_order_wins() {
        let cases: [(Document, i32); 6] = [
            (
                doc! {"$inc": {"": 1i32}, "$set": {"a": 1i32, "a.b": 1i32}},
                56,
            ),
            (
                doc! {"$set": {"a": 1i32, "a.b": 1i32}, "$inc": {"": 1i32}},
                40,
            ),
            (doc! {"$nope": {"a": 1i32}, "$set": {"": 1i32}}, 9),
            (doc! {"$set": {"": 1i32}, "$nope": {"a": 1i32}}, 56),
            (
                doc! {"$nope": {"x": 1i32}, "$set": {"a": 1i32, "a.b": 1i32}},
                9,
            ),
            (
                doc! {"$set": {"a": 1i32, "a.b": 1i32}, "$nope": {"x": 1i32}},
                40,
            ),
        ];
        for (update, code) in cases {
            assert_eq!(
                super::update_spec_error(&update).map(|(c, _)| c),
                Some(code),
                "{update:?}"
            );
        }
    }

    /// A bare field among the operators is reached in order like anything else,
    /// so an empty path ahead of it still wins.
    #[test]
    fn a_bare_field_among_operators_is_an_unknown_modifier() {
        assert_eq!(
            super::update_spec_error(&doc! {"$set": {"a": 1i32}, "z": 2i32}),
            Some((
                9,
                "Unknown modifier: z. Expected a valid update modifier or \
                 pipeline-style update specified as an array"
                    .to_string()
            ))
        );
        assert_eq!(
            super::update_spec_error(&doc! {"$set": {"": 1i32}, "z": 2i32}).map(|(c, _)| c),
            Some(56)
        );
    }

    /// A REPLACEMENT is data, not paths -- mongod really does store an empty
    /// field name for `replace_one({_id: 1}, {"": 1})`, so the walk must not
    /// touch it.
    #[test]
    fn a_replacement_document_is_not_path_validated() {
        assert_eq!(super::update_spec_error(&doc! {"": 1i32}), None);
        assert_eq!(
            super::update_spec_error(&doc! {"a": 1i32, "b.c": 2i32}),
            None
        );
        let out = super::apply_update(&doc! {"_id": 1i32, "a": 1i32}, &doc! {"": 1i32}, false)
            .expect("a replacement with an empty field name is allowed");
        assert_eq!(out, doc! {"_id": 1i32, "": 1i32});
    }

    #[test]
    fn a_well_formed_update_has_no_spec_error() {
        assert_eq!(
            super::update_spec_error(&doc! {"$set": {"a.b": 1i32}}),
            None
        );
        assert_eq!(
            super::update_spec_error(&doc! {"$set": {"a": 1i32}, "$inc": {"b": 1i32}}),
            None
        );
    }

    /// `$each` type errors: mongod words the two operators differently AND
    /// gives them different codes -- `$push` keeps the colon and answers 2,
    /// `$addToSet` drops it and answers 14. Verbatim from 8.2.11.
    #[test]
    fn each_type_errors_keep_mongods_two_wordings() {
        let err = super::apply_update(
            &doc! {"_id": 1i32, "a": [1i32]},
            &doc! {"$push": {"a": {"$each": 5i32}}},
            false,
        )
        .expect_err("a non-array $each is refused");
        assert_eq!(
            err.as_mongo(),
            Some((
                2,
                "The argument to $each in $push must be an array but it was of type: int"
            ))
        );

        let err = super::apply_update(
            &doc! {"_id": 1i32, "a": [1i32]},
            &doc! {"$addToSet": {"a": {"$each": "x"}}},
            false,
        )
        .expect_err("a non-array $each is refused");
        assert_eq!(
            err.as_mongo(),
            Some((
                14,
                "The argument to $each in $addToSet must be an array but it was of type string"
            ))
        );
    }

    // --- arith_overflow_error: messages verbatim from a mongod 8.2.11 probe
    // (2026-09-06). Before this the overflow could only defer, and the client
    // was told the server could not do `$inc`. ---

    #[test]
    fn arith_overflow_error_names_the_operator_value_and_document() {
        use bson::Bson;
        let max = doc! {"_id": 1, "n": Bson::Int64(i64::MAX)};
        assert_eq!(
            super::arith_overflow_error(&max, &doc! {"$inc": {"n": 1}}).unwrap(),
            "Failed to apply $inc operations to current value \
             ((NumberLong)9223372036854775807) for document {_id: 1}"
        );
        let min = doc! {"_id": 1, "n": Bson::Int64(i64::MIN)};
        assert_eq!(
            super::arith_overflow_error(&min, &doc! {"$inc": {"n": -1}}).unwrap(),
            "Failed to apply $inc operations to current value \
             ((NumberLong)-9223372036854775808) for document {_id: 1}"
        );
        let big = doc! {"_id": 1, "n": Bson::Int64(1i64 << 62)};
        assert_eq!(
            super::arith_overflow_error(&big, &doc! {"$mul": {"n": 4}}).unwrap(),
            "Failed to apply $mul operations to current value \
             ((NumberLong)4611686018427387904) for document {_id: 1}"
        );
        // Two int64 operands that each fit but whose sum does not.
        assert_eq!(
            super::arith_overflow_error(&big, &doc! {"$inc": {"n": Bson::Int64(1i64 << 62)}})
                .unwrap(),
            "Failed to apply $inc operations to current value \
             ((NumberLong)4611686018427387904) for document {_id: 1}"
        );
        // The document's `_id`, not the field, and rendered in mongod's form.
        let sid = doc! {"_id": "abc", "n": Bson::Int64(i64::MAX)};
        assert_eq!(
            super::arith_overflow_error(&sid, &doc! {"$inc": {"n": 1}}).unwrap(),
            "Failed to apply $inc operations to current value \
             ((NumberLong)9223372036854775807) for document {_id: \"abc\"}"
        );
    }

    #[test]
    fn arith_overflow_error_is_none_when_nothing_overflows() {
        use bson::Bson;
        let doc = doc! {"_id": 1, "n": Bson::Int64(1), "s": "x"};
        // Fits.
        assert!(super::arith_overflow_error(&doc, &doc! {"$inc": {"n": 1}}).is_none());
        // A missing field is an implicit 0 and cannot overflow.
        assert!(super::arith_overflow_error(&doc, &doc! {"$inc": {"absent": 1}}).is_none());
        // A non-numeric field is `arith_type_error`'s business, not this one.
        assert!(super::arith_overflow_error(&doc, &doc! {"$inc": {"s": 1}}).is_none());
        // Not an arithmetic operator at all.
        assert!(super::arith_overflow_error(&doc, &doc! {"$set": {"n": 1}}).is_none());
        // A double saturates to infinity rather than failing the write.
        let d = doc! {"_id": 1, "n": f64::MAX};
        assert!(super::arith_overflow_error(&d, &doc! {"$mul": {"n": 2.0}}).is_none());
    }

    // --- the parse-vs-execution classification mongod wraps on. Every verdict
    // below was measured against mongod 8.2.11 (2026-09-06): the wrapped ones
    // come back under `Plan executor error during update :: caused by ::` and
    // the bare ones do not. ---

    #[test]
    fn execution_time_update_errors_are_marked_exec() {
        for (doc, update) in [
            (doc! {"_id": 1, "a": 1}, doc! {"$push": {"a": 2}}),
            (doc! {"_id": 1, "a": 1}, doc! {"$pull": {"a": 2}}),
            (doc! {"_id": 1, "a": 1}, doc! {"$pullAll": {"a": [2]}}),
            (doc! {"_id": 1, "a": 1}, doc! {"$addToSet": {"a": 2}}),
            (doc! {"_id": 1, "a": 1}, doc! {"$pop": {"a": 1}}),
            (doc! {"_id": 1, "a": "s"}, doc! {"$bit": {"a": {"and": 1}}}),
        ] {
            let err = super::apply_update(&doc, &update, false)
                .expect_err("these all fail against the stored document");
            assert!(
                err.is_exec(),
                "{update:?} over {doc:?} must be marked execution-time, got {err:?}"
            );
        }
    }

    #[test]
    fn parse_time_update_errors_are_not_marked_exec() {
        for (doc, update) in [
            (doc! {"_id": 1, "a": 1}, doc! {"$pop": {"a": 5}}),
            (doc! {"_id": 1, "a": 1}, doc! {"$rename": {"a": "a"}}),
            (doc! {"_id": 1, "a": 1}, doc! {"$bit": {"a": 5}}),
            (
                doc! {"_id": 1, "a": [1]},
                doc! {"$addToSet": {"a": {"$each": 5}}},
            ),
        ] {
            let err = super::apply_update(&doc, &update, false)
                .expect_err("these all fail on the update spec");
            assert!(
                !err.is_exec(),
                "{update:?} over {doc:?} must stay bare, got {err:?}"
            );
        }
    }

    // --- arith_type_error: messages verbatim from a mongod 6.0.16 probe ---

    #[test]
    fn arith_type_error_names_a_non_numeric_field() {
        let doc = doc! {"_id": 1, "n": "x"};
        assert_eq!(
            super::arith_type_error(&doc, &doc! {"$inc": {"n": 1}}).unwrap(),
            "Cannot apply $inc to a value of non-numeric type. \
             {_id: 1} has the field 'n' of non-numeric type string"
        );
        assert_eq!(
            super::arith_type_error(&doc! {"_id": 1, "n": Bson::Null}, &doc! {"$inc": {"n": 1}})
                .unwrap(),
            "Cannot apply $inc to a value of non-numeric type. \
             {_id: 1} has the field 'n' of non-numeric type null"
        );
        assert_eq!(
            super::arith_type_error(&doc, &doc! {"$mul": {"n": 2}}).unwrap(),
            "Cannot apply $mul to a value of non-numeric type. \
             {_id: 1} has the field 'n' of non-numeric type string"
        );
    }

    #[test]
    fn arith_type_error_names_a_non_numeric_operand() {
        let doc = doc! {"_id": 1, "n": 1};
        assert_eq!(
            super::arith_type_error(&doc, &doc! {"$inc": {"n": "x"}}).unwrap(),
            "Cannot increment with non-numeric argument: {n: \"x\"}"
        );
        // Bool is not numeric for mongod even though it coerces elsewhere.
        assert_eq!(
            super::arith_type_error(&doc, &doc! {"$inc": {"n": true}}).unwrap(),
            "Cannot increment with non-numeric argument: {n: true}"
        );
        assert_eq!(
            super::arith_type_error(&doc, &doc! {"$mul": {"n": "x"}}).unwrap(),
            "Cannot multiply with non-numeric argument: {n: \"x\"}"
        );
    }

    #[test]
    fn arith_type_error_is_silent_when_the_update_is_fine() {
        // Numeric field, numeric operand.
        assert!(
            super::arith_type_error(&doc! {"_id": 1, "n": 1}, &doc! {"$inc": {"n": 1}}).is_none()
        );
        // A *missing* field is treated as 0 by mongod, not an error.
        assert!(super::arith_type_error(&doc! {"_id": 1}, &doc! {"$inc": {"n": 1}}).is_none());
        // Not an arithmetic operator at all.
        assert!(
            super::arith_type_error(&doc! {"_id": 1, "n": "x"}, &doc! {"$set": {"n": 2}}).is_none()
        );
        // Decimal is numeric here.
        let dec = Bson::Decimal128("2.5".parse().unwrap());
        assert!(
            super::arith_type_error(&doc! {"_id": 1, "n": dec}, &doc! {"$inc": {"n": 1}}).is_none()
        );
    }

    #[test]
    fn arith_type_error_renders_the_documents_own_id() {
        // The braces hold the doc's `_id`, not the incremented field — the bug
        // that made our message unlike any real server's.
        let oid: bson::oid::ObjectId = "60a0b0c0d0e0f00102030405".parse().unwrap();
        let msg = super::arith_type_error(&doc! {"_id": oid, "n": "x"}, &doc! {"$inc": {"n": 1}})
            .unwrap();
        assert!(
            msg.contains("{_id: ObjectId('60a0b0c0d0e0f00102030405')}"),
            "got: {msg}"
        );
        // Dotted path reports the leaf field name.
        let msg = super::arith_type_error(
            &doc! {"_id": 1, "a": {"b": "x"}},
            &doc! {"$inc": {"a.b": 1}},
        )
        .unwrap();
        assert!(msg.contains("has the field 'b'"), "got: {msg}");
    }

    use super::*;
    use bson::doc;

    fn upd(d: Document, u: Document) -> Document {
        apply_update(&d, &u, false).expect("should not fall back")
    }

    #[test]
    fn set_unset_dotted() {
        assert_eq!(
            upd(doc! {"a": 1}, doc! {"$set": {"b.c": 2}}),
            doc! {"a": 1, "b": {"c": 2}}
        );
        assert_eq!(
            upd(doc! {"a": 1, "b": 2}, doc! {"$unset": {"b": ""}}),
            doc! {"a": 1}
        );
    }

    #[test]
    fn inc_widths_and_floats() {
        assert_eq!(
            upd(doc! {"n": 5}, doc! {"$inc": {"n": 3}}),
            doc! {"n": 8i32}
        );
        assert_eq!(upd(doc! {}, doc! {"$inc": {"n": 2}}), doc! {"n": 2i32});
        assert_eq!(
            upd(doc! {"n": 1}, doc! {"$inc": {"n": 0.5}}),
            doc! {"n": 1.5}
        );
    }

    #[test]
    fn push_pop_rename() {
        assert_eq!(upd(doc! {}, doc! {"$push": {"a": 1}}), doc! {"a": [1]});
        assert_eq!(
            upd(doc! {"a": [1, 2, 3]}, doc! {"$pop": {"a": 1}}),
            doc! {"a": [1, 2]}
        );
        assert_eq!(
            upd(doc! {"a": [1, 2, 3]}, doc! {"$pop": {"a": -1}}),
            doc! {"a": [2, 3]}
        );
        assert_eq!(
            upd(doc! {"a": 1}, doc! {"$rename": {"a": "b"}}),
            doc! {"b": 1}
        );
    }

    #[test]
    fn bit_operator() {
        assert_eq!(
            upd(doc! {"b": 1i32}, doc! {"$bit": {"b": {"and": 0}}}),
            doc! {"b": 0i32}
        );
        assert_eq!(
            upd(doc! {"b": 5i32}, doc! {"$bit": {"b": {"or": 2}}}),
            doc! {"b": 7i32}
        );
        assert_eq!(
            upd(doc! {"b": 6i32}, doc! {"$bit": {"b": {"xor": 3}}}),
            doc! {"b": 5i32}
        );
        // Missing field defaults to 0.
        assert_eq!(
            upd(doc! {}, doc! {"$bit": {"b": {"or": 7}}}),
            doc! {"b": 7i32}
        );
    }

    #[test]
    fn replacement_preserves_id_first() {
        // `_id` leads, as in mongod. `doc!` comparison is order-sensitive,
        // so this pins the byte order the client sees, not just the content.
        assert_eq!(
            upd(doc! {"_id": 7, "a": 1}, doc! {"b": 2}),
            doc! {"_id": 7, "b": 2}
        );
        // Also when the replacement supplies `_id` itself, in a later slot.
        assert_eq!(
            upd(doc! {"_id": 7, "a": 1}, doc! {"b": 2, "_id": 7}),
            doc! {"_id": 7, "b": 2}
        );
    }

    #[test]
    fn fallbacks() {
        // _id change, mixing, cross-type $min, pipeline-only ops -> Fallback.
        assert!(apply_update(&doc! {"_id": 1}, &doc! {"$set": {"_id": 2}}, false).is_err());
        assert!(apply_update(&doc! {"a": 1}, &doc! {"$set": {"a": 1}, "b": 2}, false).is_err());
        // $min/$max now compare cross-type by BSON order (number < string), so a
        // string vs a number computes: $min keeps the (smaller) number.
        assert_eq!(
            apply_update(&doc! {"a": 1}, &doc! {"$min": {"a": "x"}}, false).unwrap(),
            doc! {"a": 1}
        );
        // A bool operand now computes via `order::bson_lt` (bool ranks above
        // numbers in BSON order, so $max sets it), matching Python's _bson_lt.
        assert_eq!(
            apply_update(&doc! {"a": 5}, &doc! {"$max": {"a": true}}, false).unwrap(),
            doc! {"a": true}
        );
        // Decimal128 joins the unified numeric compare: 2.5 < 3 -> $min sets it.
        let d: bson::Decimal128 = "2.5".parse().unwrap();
        assert_eq!(
            apply_update(&doc! {"a": 3}, &doc! {"$min": {"a": d}}, false).unwrap(),
            doc! {"a": d}
        );
        // NaN is unordered: Python's `5 < nan` is False, so $max keeps 5.
        assert_eq!(
            apply_update(&doc! {"a": 5}, &doc! {"$max": {"a": f64::NAN}}, false).unwrap(),
            doc! {"a": 5}
        );
        // Bare `apply_update` (no positional_matches) can't resolve `$` -> defer.
        assert!(apply_update(&doc! {"a": [1]}, &doc! {"$set": {"a.$": 9}}, false).is_err());
        // $inc / $mul on an explicit-null field -> defer so Python raises the
        // TypeMismatch (code 14). A *missing* field still applies (see
        // `inc_widths_and_floats`).
        assert!(apply_update(&doc! {"n": Bson::Null}, &doc! {"$inc": {"n": 5}}, false).is_err());
        assert!(apply_update(&doc! {"n": Bson::Null}, &doc! {"$mul": {"n": 5}}, false).is_err());
    }

    fn upd_af(d: Document, u: Document, filters: Vec<Document>, pos: Document) -> Document {
        apply_update_with(&d, &u, false, &filters, &pos).expect("should not fall back")
    }

    #[test]
    fn array_filter_all_positional() {
        // $[] touches every element.
        assert_eq!(
            upd_af(
                doc! {"g": [1, 2, 3]},
                doc! {"$inc": {"g.$[]": 10}},
                vec![],
                doc! {}
            ),
            doc! {"g": [11i32, 12i32, 13i32]}
        );
    }

    #[test]
    fn array_filter_identifier() {
        // $[e] with arrayFilters {e: {$gte: 2}} only touches elements >= 2.
        assert_eq!(
            upd_af(
                doc! {"g": [1, 2, 3]},
                doc! {"$set": {"g.$[e]": 0}},
                vec![doc! {"e": {"$gte": 2}}],
                doc! {}
            ),
            doc! {"g": [1, 0, 0]}
        );
    }

    #[test]
    fn array_filter_identifier_on_subdoc_field() {
        // $[e].score with arrayFilters {"e.score": {$lt: 50}}.
        let out = upd_af(
            doc! {"items": [{"score": 40}, {"score": 80}, {"score": 10}]},
            doc! {"$set": {"items.$[e].score": 100}},
            vec![doc! {"e.score": {"$lt": 50}}],
            doc! {},
        );
        let scores: Vec<i32> = out
            .get_array("items")
            .unwrap()
            .iter()
            .map(|e| e.as_document().unwrap().get_i32("score").unwrap())
            .collect();
        assert_eq!(scores, vec![100, 80, 100]);
    }

    #[test]
    fn positional_dollar_uses_matches() {
        // $ resolves via positional_matches (computed from the query filter).
        let pos = find_positional_matches(&doc! {"g": [5, 6, 7]}, &doc! {"g": 6});
        // find_positional_matches only fires for dotted clauses; bare {g: 6}
        // doesn't populate it, so $ stays unresolved -> here we feed it directly.
        assert!(pos.is_empty());
        let out = upd_af(
            doc! {"g": [5, 6, 7]},
            doc! {"$set": {"g.$": 60}},
            vec![],
            doc! {"g": 1i64},
        );
        assert_eq!(out, doc! {"g": [5, 60, 7]});
    }

    #[test]
    fn find_positional_matches_dotted_array_clause() {
        // {"g.x": 2} over an array of subdocs -> first matching index.
        let pos = find_positional_matches(
            &doc! {"g": [{"x": 1}, {"x": 2}, {"x": 2}]},
            &doc! {"g.x": 2},
        );
        assert_eq!(pos, doc! {"g": 1i64});
    }
}
