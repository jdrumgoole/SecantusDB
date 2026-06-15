//! Shared dotted-path read helpers (mirror `secantus.paths`' read side).
//! Used by both the update engine and the expression evaluator.

use bson::{Bson, Document};

pub fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Resolve a dotted path to a value, walking into documents and (by numeric
/// index) arrays. `None` for a missing path. Matches `secantus.paths.get_path`.
pub fn get_path<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    let parts: Vec<&str> = path.split('.').collect();
    get_in_doc(doc, &parts)
}

fn get_in_doc<'a>(d: &'a Document, parts: &[&str]) -> Option<&'a Bson> {
    let child = d.get(parts[0])?;
    if parts.len() == 1 {
        Some(child)
    } else {
        get_in_bson(child, &parts[1..])
    }
}

fn get_in_bson<'a>(cur: &'a Bson, parts: &[&str]) -> Option<&'a Bson> {
    match cur {
        Bson::Document(d) => get_in_doc(d, parts),
        Bson::Array(arr) => {
            if !is_digits(parts[0]) {
                return None;
            }
            let idx: usize = parts[0].parse().ok()?;
            let child = arr.get(idx)?;
            if parts.len() == 1 {
                Some(child)
            } else {
                get_in_bson(child, &parts[1..])
            }
        }
        _ => None,
    }
}

pub fn has_path(doc: &Document, path: &str) -> bool {
    get_path(doc, path).is_some()
}

/// Hard cap on a numeric path index that would grow a list (mirrors
/// `secantus.paths._MAX_LIST_GROW_INDEX`). Exceeding it is `Err(())` so callers
/// can defer to Python (which raises `PathError`).
const MAX_LIST_GROW_INDEX: usize = 100_000;

/// Set `value` at a dotted path, creating intermediate documents, growing lists
/// for a trailing numeric index. `Err(())` only when the list-growth cap is
/// exceeded. Mirrors `secantus.paths.set_path`.
pub fn set_path(doc: &mut Document, path: &str, value: Bson) -> Result<(), ()> {
    let parts: Vec<&str> = path.split('.').collect();
    set_in_doc(doc, &parts, value)
}

fn set_in_doc(d: &mut Document, parts: &[&str], value: Bson) -> Result<(), ()> {
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

fn set_in_bson(cur: &mut Bson, parts: &[&str], value: Bson) -> Result<(), ()> {
    match cur {
        Bson::Document(d) => set_in_doc(d, parts, value),
        Bson::Array(arr) => set_in_array(arr, parts, value),
        _ => Ok(()), // non-container intermediate -> Python walk returns None -> no-op
    }
}

fn set_in_array(arr: &mut Vec<Bson>, parts: &[&str], value: Bson) -> Result<(), ()> {
    let head = parts[0];
    if !is_digits(head) {
        return Ok(()); // non-digit into list -> no-op
    }
    let idx: usize = head.parse().map_err(|_| ())?;
    if parts.len() == 1 {
        if idx > MAX_LIST_GROW_INDEX {
            return Err(()); // Python raises PathError -> let Python do it
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

/// Remove the value at a dotted path. On a list element, the slot is set to
/// null (matching `secantus.paths.unset_path`), not removed.
pub fn unset_path(doc: &mut Document, path: &str) {
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
                    arr[idx] = Bson::Null;
                }
            } else if idx < arr.len() {
                unset_in_bson(&mut arr[idx], &parts[1..]);
            }
        }
        _ => {}
    }
}
