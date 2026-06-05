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
