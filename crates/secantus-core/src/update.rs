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

#[derive(Debug)]
pub struct Fallback;

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
fn extract_af_identifiers(f: &Document) -> Vec<String> {
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
            _ => return Err(Fallback),
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
        let sub = filters.get(name).ok_or(Fallback)?;
        for (i, elem) in arr.iter().enumerate() {
            let mut elem_doc = Document::new();
            elem_doc.insert(name.to_string(), elem.clone());
            if query::matches(&elem_doc, sub, &Document::new(), None).map_err(|_| Fallback)? {
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
    paths::set_path(doc, path, value).map_err(|_| Fallback)
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
        return Err(Fallback);
    }
    // Decimal dominates the widening order (int32 < int64 < double < decimal),
    // so either side being decimal puts the whole operation in the decimal
    // domain — computed exactly at decimal128's 34 digits, quantum preserved.
    if matches!(current, Bson::Decimal128(_)) || matches!(operand, Bson::Decimal128(_)) {
        let (a, b) = (
            decimal::from_bson(current).ok_or(Fallback)?,
            decimal::from_bson(operand).ok_or(Fallback)?,
        );
        let r = if mul {
            decimal::mul(&a, &b)
        } else {
            decimal::add(&a, &b)
        };
        return decimal::to_bson(&r.ok_or(Fallback)?).ok_or(Fallback);
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
        return int_promoted_to_bson(r.ok_or(Fallback)?, wide).ok_or(Fallback);
    }
    // Float path: any non-numeric operand (current/operand) makes Python raise.
    let a = as_float_like(current).ok_or(Fallback)?;
    let b = as_float_like(operand).ok_or(Fallback)?;
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
fn current_or_zero(result: &Document, path: &str) -> R<Bson> {
    match get_path(result, path) {
        None => Ok(Bson::Int32(0)),
        Some(Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)) => {
            Ok(get_path(result, path).expect("just matched").clone())
        }
        // Bson::Boolean lands here deliberately: it is not numeric for arithmetic.
        Some(_) => Err(Fallback),
    }
}

// --- operator application ----------------------------------------------

fn payload_doc(payload: &Bson) -> R<&Document> {
    match payload {
        Bson::Document(d) => Ok(d),
        _ => Err(Fallback),
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
            _ => return Err(Fallback), // unknown modifier -> Python raises
        }
    }
    let each = match m.get("$each") {
        Some(Bson::Array(a)) => a,
        _ => return Err(Fallback),
    };
    match m.get("$position") {
        None => arr.extend(each.iter().cloned()),
        Some(p) => {
            // A bool $position is a parse error in mongod (code 2), not index 1
            // — `as_int_like` would coerce it, so guard first.
            if matches!(p, Bson::Boolean(_)) {
                return Err(Fallback);
            }
            let n = as_int_like(p).ok_or(Fallback)?;
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
            return Err(Fallback);
        }
        let n = as_int_like(s).ok_or(Fallback)?;
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
            let dir = dir_pm1(spec).ok_or(Fallback)?;
            if !arr.iter().all(crate::order::is_sortable) {
                return Err(Fallback);
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
                .map(|(f, d)| dir_pm1(d).map(|di| (f, di)).ok_or(Fallback))
                .collect::<R<Vec<_>>>()?;
            for (field, _) in &fields {
                if !arr
                    .iter()
                    .all(|e| crate::order::is_sortable(&key_of(e, field)))
                {
                    return Err(Fallback);
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
        _ => return Err(Fallback), // non-int / non-doc $sort -> Python raises
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
            crate::query::matches(&d, &q, &Document::new(), None).map_err(|_| Fallback)
        }
        Bson::Document(c) => match element {
            Bson::Document(ed) => {
                crate::query::matches(ed, c, &Document::new(), None).map_err(|_| Fallback)
            }
            _ => Ok(false),
        },
        _ => {
            let d = doc! { "__e": element.clone() };
            let q = doc! { "__e": criterion.clone() };
            crate::query::matches(&d, &q, &Document::new(), None).map_err(|_| Fallback)
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
                    let new = arith(&cur, factor, true)?;
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
                        Some(_) => return Err(Fallback), // $push on non-array -> Python raises
                    };
                    push_apply(&mut a, value)?;
                    set_path(result, &cpath, Bson::Array(a))?;
                }
            }
        }
        "$pop" => {
            for (path, dir) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
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
                        if matches!(dir, Bson::Boolean(_)) {
                            return Err(Fallback);
                        }
                        match as_int_like(dir) {
                            Some(1) => {
                                a.pop();
                            }
                            Some(-1) => {
                                a.remove(0);
                            }
                            _ => return Err(Fallback),
                        }
                        set_path(result, &cpath, Bson::Array(a))?;
                    }
                }
            }
        }
        "$rename" => {
            for (old, new) in payload {
                let new = match new {
                    Bson::String(s) => s.as_str(),
                    _ => return Err(Fallback),
                };
                // $rename doesn't support positional tokens (mongod rejects);
                // defer the rare case to keep semantics exact.
                if has_positional(old) || has_positional(new) {
                    return Err(Fallback);
                }
                if old == "_id" || new == "_id" {
                    return Err(Fallback); // immutable _id -> Python raises
                }
                // mongod validation (Python raises 56 / 2; the Rust server renders
                // BadValue). These previously silently corrupted the array or
                // created a bad field.
                if old.is_empty() || new.is_empty() {
                    return Err(Fallback); // empty path -> Python raises 56
                }
                if old == new || rename_same_path(old, new) {
                    return Err(Fallback); // differ / same path -> Python raises 2
                }
                if rename_traverses_array(result, old) || rename_traverses_array(result, new) {
                    return Err(Fallback); // array element -> Python raises 2
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
                let ops = match ops {
                    Bson::Document(d) if !d.is_empty() => d,
                    _ => return Err(Fallback), // empty / non-doc -> Python raises
                };
                let mut parsed: Vec<(&str, i64)> = Vec::with_capacity(ops.len());
                for (bit_op, mask_b) in ops {
                    let op_s = bit_op.as_str();
                    if !matches!(op_s, "and" | "or" | "xor") {
                        return Err(Fallback); // unknown sub-op -> Python raises
                    }
                    let mask = match mask_b {
                        Bson::Int32(n) => *n as i64,
                        Bson::Int64(n) => *n,
                        _ => return Err(Fallback), // non-integer mask -> Python raises
                    };
                    parsed.push((op_s, mask));
                }
                for cpath in expand_path(result, path, filters, pos)? {
                    let mut cur = match get_path(result, &cpath) {
                        None | Some(Bson::Null) => 0i64,
                        Some(Bson::Int32(n)) => *n as i64,
                        Some(Bson::Int64(n)) => *n,
                        Some(_) => return Err(Fallback), // $bit on non-integer -> Python raises
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
                    // BSON cross-type order via `order::bson_lt` — the direct
                    // `_bson_lt` port, which (unlike the `$sort` comparator)
                    // needs no transitivity and so covers bool / Decimal128 /
                    // NaN / Binary / Timestamp / Regex / Min-MaxKey and the
                    // decoded exotic text types. Only a DBPointer (Python's
                    // type-name tiebreak) still defers.
                    let should_set = match get_path(result, &cpath) {
                        None => true,
                        Some(current) => {
                            let lt = if want_less {
                                crate::order::bson_lt(value, current) // value < current
                            } else {
                                crate::order::bson_lt(current, value) // current < value
                            };
                            match lt {
                                Some(l) => l,
                                None => return Err(Fallback),
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
                        _ => return Err(Fallback), // $each not an array -> Python raises
                    },
                    _ => vec![value.clone()],
                };
                for cpath in expand_path(result, path, filters, pos)? {
                    let mut a = match get_path(result, &cpath).cloned() {
                        None | Some(Bson::Null) => Vec::new(),
                        Some(Bson::Array(a)) => a,
                        Some(_) => return Err(Fallback), // non-array -> Python raises
                    };
                    for item in &items {
                        // mongod's `$addToSet` membership test is field-ORDER-
                        // sensitive for documents: `{y: 2, x: 1}` is a different
                        // value from `{x: 1, y: 2}` and gets appended. `py_eq`
                        // mirrors Python's `==`, which compares documents
                        // order-INsensitively, so defer whenever a document is
                        // involved and let the Python engine — which walks the
                        // pairs in order — decide. Scalars keep the fast path.
                        // Two cases `py_eq` gets wrong, both verified against
                        // mongod 6.0.16:
                        //   * documents — membership is field-ORDER-sensitive, so
                        //     `{y:2,x:1}` is appended alongside `{x:1,y:2}`;
                        //     `py_eq` mirrors Python `==`, which ignores order.
                        //   * booleans — `true` is a distinct type from `1`, so
                        //     `$addToSet: true` into `[1, 2]` yields `[1, 2, true]`
                        //     (and `1` into `[true]` yields `[true, 1]`); Python's
                        //     `==` says `1 == True`, so `py_eq` skips the append.
                        // Defer both to the Python engine, whose equality ranks
                        // bool separately and walks document pairs in order.
                        let tricky = |v: &Bson| matches!(v, Bson::Document(_) | Bson::Boolean(_));
                        if tricky(item) || a.iter().any(tricky) {
                            return Err(Fallback);
                        }
                        let mut present = false;
                        for e in &a {
                            if expressions::py_eq(e, item).map_err(|_| Fallback)? {
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
                        Some(_) => return Err(Fallback),
                        None => {}
                    }
                }
            }
        }
        "$pullAll" => {
            for (path, values) in payload {
                let Bson::Array(vals) = values else {
                    return Err(Fallback); // non-array arg -> Python raises
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
                                    if expressions::py_eq(&e, v).map_err(|_| Fallback)? {
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
                        Some(_) => return Err(Fallback),
                        None => {}
                    }
                }
            }
        }
        // $currentDate (non-deterministic) and unknown ops -> Python.
        _ => return Err(Fallback),
    }
    Ok(())
}

/// Apply an operator/replacement update document. `Err(Fallback)` => defer to
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
    if update.is_empty() {
        return Ok(doc.clone());
    }
    if !array_filters_valid(array_filters, update) {
        return Err(Fallback); // invalid arrayFilters -> Python raises the exact code
    }
    let has_op = update.keys().any(|k| k.starts_with('$'));
    if has_op {
        if !update.keys().all(|k| k.starts_with('$')) {
            return Err(Fallback); // mixing operators with fields -> Python raises
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
                return Err(Fallback);
            }
        }
        Ok(result)
    } else {
        // Replacement-style: the update is the new doc, with _id preserved.
        let mut new = update.clone();
        if let Some(orig) = doc.get("_id") {
            match new.get("_id") {
                Some(v) if v != orig => return Err(Fallback), // changed _id -> Python raises
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

/// mongod's numeric domain for `$inc` / `$mul`: int32 / int64 / double /
/// decimal. Bool is deliberately excluded — mongod refuses `$inc` by `true`.
fn is_arith_numeric(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

/// A scalar as mongod renders it inside an error message.
fn render_scalar(v: &Bson) -> String {
    match v {
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".to_string(),
        Bson::String(s) => format!("\"{s}\""),
        Bson::ObjectId(o) => format!("ObjectId('{o}')"),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(d) => d.to_string(),
        Bson::Decimal128(d) => d.to_string(),
        other => format!("{other:?}"),
    }
}

/// The `{_id: …}` prefix mongod puts in the non-numeric-field message. It is the
/// *document's* `_id`, not the field being incremented.
fn render_doc_id(doc: &Document) -> String {
    match doc.get("_id") {
        Some(id) => format!("{{_id: {}}}", render_scalar(id)),
        None => "{}".to_string(),
    }
}

#[cfg(test)]
mod tests {
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
