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

use bson::{Bson, Document};

use crate::numeric::{as_float_like, as_int_like, int_to_bson};
use crate::paths::{get_path, has_path, is_digits};

#[derive(Debug)]
pub struct Fallback;

type R<T> = Result<T, Fallback>;

const MAX_LIST_GROW_INDEX: usize = 100_000;

fn is_positional_token(part: &str) -> bool {
    part == "$" || part == "$[]" || (part.starts_with("$[") && part.ends_with("]"))
}

fn has_positional(path: &str) -> bool {
    path.split('.').any(is_positional_token)
}

// --- path write helpers (read helpers live in crate::paths) -------------

fn set_path(doc: &mut Document, path: &str, value: Bson) -> R<()> {
    let parts: Vec<&str> = path.split('.').collect();
    set_in_doc(doc, &parts, value)
}

fn set_in_doc(d: &mut Document, parts: &[&str], value: Bson) -> R<()> {
    let head = parts[0];
    if parts.len() == 1 {
        d.insert(head.to_string(), value);
        return Ok(());
    }
    if !d.contains_key(head) {
        d.insert(head.to_string(), Bson::Document(Document::new()));
    }
    set_in_bson(d.get_mut(head).unwrap(), &parts[1..], value)
}

fn set_in_bson(cur: &mut Bson, parts: &[&str], value: Bson) -> R<()> {
    match cur {
        Bson::Document(d) => set_in_doc(d, parts, value),
        Bson::Array(arr) => set_in_array(arr, parts, value),
        _ => Ok(()), // non-container intermediate -> Python walk returns None -> no-op
    }
}

fn set_in_array(arr: &mut Vec<Bson>, parts: &[&str], value: Bson) -> R<()> {
    let head = parts[0];
    if !is_digits(head) {
        return Ok(()); // non-digit into list -> no-op
    }
    let idx: usize = head.parse().map_err(|_| Fallback)?;
    if parts.len() == 1 {
        if idx > MAX_LIST_GROW_INDEX {
            return Err(Fallback); // Python raises PathError -> let Python do it
        }
        while arr.len() <= idx {
            arr.push(Bson::Null);
        }
        arr[idx] = value;
        Ok(())
    } else if idx < arr.len() {
        set_in_bson(&mut arr[idx], &parts[1..], value)
    } else {
        Ok(()) // intermediate index out of range -> no-op
    }
}

fn unset_path(doc: &mut Document, path: &str) {
    let parts: Vec<&str> = path.split('.').collect();
    unset_in_doc(doc, &parts);
}

fn unset_in_doc(d: &mut Document, parts: &[&str]) {
    let head = parts[0];
    if parts.len() == 1 {
        d.remove(head);
        return;
    }
    if let Some(child) = d.get_mut(head) {
        unset_in_bson(child, &parts[1..]);
    }
}

fn unset_in_bson(cur: &mut Bson, parts: &[&str]) {
    match cur {
        Bson::Document(d) => unset_in_doc(d, parts),
        Bson::Array(arr) => {
            if !is_digits(parts[0]) {
                return;
            }
            let Ok(idx) = parts[0].parse::<usize>() else {
                return;
            };
            if parts.len() == 1 {
                if idx < arr.len() {
                    arr[idx] = Bson::Null; // Python sets the slot to None, doesn't remove
                }
            } else if idx < arr.len() {
                unset_in_bson(&mut arr[idx], &parts[1..]);
            }
        }
        _ => {}
    }
}

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

fn apply_op(result: &mut Document, op: &str, payload: &Bson) -> R<()> {
    let payload = payload_doc(payload)?;
    // Any positional token in a path -> Python's positional/arrayFilters path.
    for key in payload.keys() {
        if has_positional(key) {
            return Err(Fallback);
        }
    }
    match op {
        "$set" | "$setOnInsert" => {
            for (path, value) in payload {
                set_path(result, path, value.clone())?;
            }
        }
        "$unset" => {
            for path in payload.keys() {
                unset_path(result, path);
            }
        }
        "$inc" => {
            for (path, delta) in payload {
                let cur = current_or_zero(result, path);
                let new = arith(&cur, delta, false)?;
                set_path(result, path, new)?;
            }
        }
        "$mul" => {
            for (path, factor) in payload {
                let cur = current_or_zero(result, path);
                let new = arith(&cur, factor, true)?;
                set_path(result, path, new)?;
            }
        }
        "$push" => {
            for (path, value) in payload {
                match get_path(result, path).cloned() {
                    None | Some(Bson::Null) => {
                        set_path(result, path, Bson::Array(vec![value.clone()]))?;
                    }
                    Some(Bson::Array(mut a)) => {
                        a.push(value.clone());
                        set_path(result, path, Bson::Array(a))?;
                    }
                    Some(_) => return Err(Fallback), // $push on non-array -> Python raises
                }
            }
        }
        "$pop" => {
            for (path, dir) in payload {
                if let Some(Bson::Array(a)) = get_path(result, path) {
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
                    set_path(result, path, Bson::Array(a))?;
                }
            }
        }
        "$rename" => {
            for (old, new) in payload {
                let new = match new {
                    Bson::String(s) => s.as_str(),
                    _ => return Err(Fallback),
                };
                if has_positional(new) {
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
    if update.is_empty() {
        return Ok(doc.clone());
    }
    let has_op = update.keys().any(|k| k.starts_with('$'));
    if has_op {
        if !update.keys().all(|k| k.starts_with('$')) {
            return Err(Fallback); // mixing operators with fields -> Python raises
        }
        let mut result = doc.clone();
        for (op, payload) in update {
            if op == "$setOnInsert" && !is_upsert {
                continue;
            }
            apply_op(&mut result, op, payload)?;
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
        // _id change, mixing, positional, $min, pipeline-only ops -> Fallback.
        assert!(apply_update(&doc! {"_id": 1}, &doc! {"$set": {"_id": 2}}, false).is_err());
        assert!(apply_update(&doc! {"a": 1}, &doc! {"$set": {"a": 1}, "b": 2}, false).is_err());
        assert!(apply_update(&doc! {"a": [1]}, &doc! {"$set": {"a.$": 9}}, false).is_err());
        assert!(apply_update(&doc! {"a": 1}, &doc! {"$min": {"a": 0}}, false).is_err());
    }
}
