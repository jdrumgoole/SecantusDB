//! Update application — Rust port of `secantus.update.apply_update`, the third
//! leaf engine. Same graceful-fallback design as the query matcher: handle the
//! common, deterministic operators byte-for-byte and return `Fallback` for
//! anything whose Python semantics we don't reproduce, so the shim runs the
//! pure-Python `apply_update` instead (which also raises the right errors).
//!
//! Handled: replacement-style updates, `$set`, `$setOnInsert`, `$unset`,
//! `$inc`, `$mul`, `$push`, `$pop`, `$rename`, plus `_id` immutability.
//! Deferred to Python: pipeline (array) updates, positional operators
//! (`$`/`$[]`/`$[id]`) and array filters, `$currentDate` (non-deterministic),
//! `$min`/`$max`/`$pull`/`$addToSet`/`$bit` (Python comparison/`==` semantics),
//! Decimal128 / non-numeric arithmetic, and every error condition (so Python
//! raises the exact `UpdateError`).

use std::collections::HashMap;

use bson::{Bson, Document};

use crate::numeric::{as_float_like, as_int_like, int_to_bson};
use crate::paths::{self, get_path, has_path};
use crate::query;

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
        return int_to_bson(r.ok_or(Fallback)?).ok_or(Fallback);
    }
    // Float path: any non-numeric operand (current/operand) makes Python raise.
    let a = as_float_like(current).ok_or(Fallback)?;
    let b = as_float_like(operand).ok_or(Fallback)?;
    Ok(Bson::Double(if mul { a * b } else { a + b }))
}

/// Current value of a field for $inc/$mul: missing or explicit null -> int 0.
fn current_or_zero(result: &Document, path: &str) -> Bson {
    match get_path(result, path) {
        None | Some(Bson::Null) => Bson::Int32(0),
        Some(v) => v.clone(),
    }
}

// --- operator application ----------------------------------------------

fn payload_doc(payload: &Bson) -> R<&Document> {
    match payload {
        Bson::Document(d) => Ok(d),
        _ => Err(Fallback),
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
                    let cur = current_or_zero(result, &cpath);
                    let new = arith(&cur, delta, false)?;
                    set_path(result, &cpath, new)?;
                }
            }
        }
        "$mul" => {
            for (path, factor) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
                    let cur = current_or_zero(result, &cpath);
                    let new = arith(&cur, factor, true)?;
                    set_path(result, &cpath, new)?;
                }
            }
        }
        "$push" => {
            for (path, value) in payload {
                for cpath in expand_path(result, path, filters, pos)? {
                    match get_path(result, &cpath).cloned() {
                        None | Some(Bson::Null) => {
                            set_path(result, &cpath, Bson::Array(vec![value.clone()]))?;
                        }
                        Some(Bson::Array(mut a)) => {
                            a.push(value.clone());
                            set_path(result, &cpath, Bson::Array(a))?;
                        }
                        Some(_) => return Err(Fallback), // $push on non-array -> Python raises
                    }
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
        // $currentDate (non-deterministic), $min/$max/$pull/$addToSet/$bit
        // (Python comparison/== semantics), and unknown ops -> Python.
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
    fn replacement_preserves_id() {
        assert_eq!(
            upd(doc! {"_id": 7, "a": 1}, doc! {"b": 2}),
            doc! {"b": 2, "_id": 7}
        );
    }

    #[test]
    fn fallbacks() {
        // _id change, mixing, $min, pipeline-only ops -> Fallback.
        assert!(apply_update(&doc! {"_id": 1}, &doc! {"$set": {"_id": 2}}, false).is_err());
        assert!(apply_update(&doc! {"a": 1}, &doc! {"$set": {"a": 1}, "b": 2}, false).is_err());
        assert!(apply_update(&doc! {"a": 1}, &doc! {"$min": {"a": 0}}, false).is_err());
        // Bare `apply_update` (no positional_matches) can't resolve `$` -> defer.
        assert!(apply_update(&doc! {"a": [1]}, &doc! {"$set": {"a.$": 9}}, false).is_err());
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
