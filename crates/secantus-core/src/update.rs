//! Update application — Rust port of `secantus.update.apply_update`, the third
//! leaf engine. Same graceful-fallback design as the query matcher: handle the
//! common, deterministic operators byte-for-byte and return `Fallback` for
//! anything whose Python semantics we don't reproduce, so the shim runs the
//! pure-Python `apply_update` instead (which also raises the right errors).
//!
//! Handled: replacement-style updates, `$set`, `$setOnInsert`, `$unset`,
//! `$inc`, `$mul`, `$push` (incl. the `$each` modifier form with `$position` /
//! `$slice` / `$sort` — `1`/`-1` whole-element or `{field: dir}`, BSON-order),
//! `$pop`, `$rename`, `$bit`, `$min`/`$max` (Python `<` for numeric / string /
//! date pairs — cross-type defers), `$addToSet` (incl. `$each`), `$pull` (query
//! semantics: element-value predicate / sub-document match / equality, via
//! `query::matches`), `$pullAll` (literal equality via `expressions::py_eq`),
//! plus `_id` immutability.
//! Deferred to Python: pipeline (array) updates, positional operators
//! (`$`/`$[]`/`$[id]`) and array filters, `$currentDate` (non-deterministic), a
//! `$push` `$sort` over elements outside the sortable subset, a `$min`/`$max`
//! comparison Python's `<` would raise (cross-type / Decimal128 / ObjectId /
//! arrays), Decimal128 / non-numeric arithmetic, and every error condition (so
//! Python raises the exact `UpdateError`).

use std::cmp::Ordering;
use std::collections::HashMap;

use bson::{doc, Bson, Document};

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

// --- positional / arrayFilters path expansion ---------------------------
//
// Mirrors `update._expand_path` / `_walk_positional` / `_index_array_filters` /
// `find_positional_matches`. A target path that contains a positional token
// (`$`, `$[]`, `$[ident]`) is expanded against the current document into the
// concrete index paths the operator then writes to.

/// Map each arrayFilter identifier (`{"x.score": {...}}` → `x`) to its filter
/// document. First entry wins per identifier, as in Python.
fn index_array_filters(filters: &[Document]) -> HashMap<String, &Document> {
    let mut out: HashMap<String, &Document> = HashMap::new();
    for f in filters {
        for key in f.keys() {
            let name = key.split('.').next().unwrap_or(key).to_string();
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
    // Decimal128 has no Python arithmetic support (raises) -> defer.
    if matches!(current, Bson::Decimal128(_)) || matches!(operand, Bson::Decimal128(_)) {
        return Err(Fallback);
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
fn current_or_zero(result: &Document, path: &str) -> R<Bson> {
    match get_path(result, path) {
        None => Ok(Bson::Int32(0)),
        Some(Bson::Null) => Err(Fallback),
        Some(v) => Ok(v.clone()),
    }
}

// --- operator application ----------------------------------------------

fn payload_doc(payload: &Bson) -> R<&Document> {
    match payload {
        Bson::Document(d) => Ok(d),
        _ => Err(Fallback),
    }
}

/// Compare two BSON values the way Python's `<` / `>` would for `$min` / `$max`:
/// `Some(Ordering)` only for the operand pairs Python compares *without raising*
/// — both numeric (int / long / double / bool; exact when integral, else `f64`
/// with a 2^53 safety bound so a large int can't lose precision), both strings
/// (UTF-8 byte order == code-point order), or both dates. Any other pair
/// (cross-type, Decimal128, ObjectId, arrays, NaN, …) returns `None`, which
/// defers `$min`/`$max` to the Python oracle, whose `<` would raise or use an
/// ordering not reproduced here.
fn py_cmp(a: &Bson, b: &Bson) -> Option<Ordering> {
    fn as_int(v: &Bson) -> Option<i64> {
        match v {
            Bson::Int32(n) => Some(*n as i64),
            Bson::Int64(n) => Some(*n),
            Bson::Boolean(x) => Some(*x as i64),
            _ => None,
        }
    }
    fn as_f(v: &Bson) -> Option<f64> {
        match v {
            Bson::Double(d) => Some(*d),
            _ => as_int(v).map(|i| i as f64),
        }
    }
    // Both integral (incl. bool) -> exact i64 comparison.
    if let (Some(x), Some(y)) = (as_int(a), as_int(b)) {
        return Some(x.cmp(&y));
    }
    let numeric = |v: &Bson| {
        matches!(
            v,
            Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Boolean(_)
        )
    };
    if numeric(a) && numeric(b) {
        // One side is a double; refuse if an integer side can't be represented
        // exactly as f64 (else the compare could diverge from Python's exact one).
        for v in [a, b] {
            if let Some(i) = as_int(v) {
                if i.unsigned_abs() >= (1u64 << 53) {
                    return None;
                }
            }
        }
        return as_f(a)?.partial_cmp(&as_f(b)?); // NaN -> None (defer)
    }
    match (a, b) {
        (Bson::String(x), Bson::String(y)) => Some(x.cmp(y)),
        (Bson::DateTime(x), Bson::DateTime(y)) => {
            Some(x.timestamp_millis().cmp(&y.timestamp_millis()))
        }
        _ => None,
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
    match spec {
        Bson::Int32(_) | Bson::Int64(_) => {
            let dir = as_int_like(spec).ok_or(Fallback)?;
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
                .map(|(f, d)| as_int_like(d).map(|di| (f, di)).ok_or(Fallback))
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
                        match as_int_like(dir) {
                            Some(1) => {
                                a.pop();
                            }
                            Some(-1) => {
                                a.remove(0);
                            }
                            _ => continue, // other direction -> no change
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
                    // Python treats an absent *or null* field as "no current" and
                    // always sets (`current is None`); else it compares.
                    let should_set = match get_path(result, &cpath) {
                        None | Some(Bson::Null) => true,
                        Some(current) => {
                            let ord = py_cmp(value, current).ok_or(Fallback)?;
                            if want_less {
                                ord == Ordering::Less
                            } else {
                                ord == Ordering::Greater
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
                    // (element-value predicate / sub-document match / equality); a
                    // non-array field is a no-op.
                    if let Some(Bson::Array(a)) = get_path(result, &cpath).cloned() {
                        let mut kept = Vec::with_capacity(a.len());
                        for e in a {
                            if !pull_matches(&e, criterion)? {
                                kept.push(e);
                            }
                        }
                        set_path(result, &cpath, Bson::Array(kept))?;
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
                    // equality, not predicates); non-array field is a no-op.
                    if let Some(Bson::Array(a)) = get_path(result, &cpath).cloned() {
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
                    new.insert("_id".to_string(), orig.clone());
                }
            }
        }
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
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
    fn replacement_preserves_id() {
        assert_eq!(
            upd(doc! {"_id": 7, "a": 1}, doc! {"b": 2}),
            doc! {"b": 2, "_id": 7}
        );
    }

    #[test]
    fn fallbacks() {
        // _id change, mixing, cross-type $min, pipeline-only ops -> Fallback.
        assert!(apply_update(&doc! {"_id": 1}, &doc! {"$set": {"_id": 2}}, false).is_err());
        assert!(apply_update(&doc! {"a": 1}, &doc! {"$set": {"a": 1}, "b": 2}, false).is_err());
        // Numeric $min now computes; a cross-type compare (Python `<` raises) defers.
        assert!(apply_update(&doc! {"a": 1}, &doc! {"$min": {"a": "x"}}, false).is_err());
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
