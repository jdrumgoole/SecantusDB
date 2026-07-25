//! `updateDescription` diff — Rust port of
//! `secantus.diff.compute_update_description`, the sixth leaf engine (used to
//! build the change-stream `$v: 2` update events). Returns
//! `{updatedFields, removedFields, truncatedArrays}` for a pre->post image.
//!
//! Value equality reuses the expression engine's Python-`==` semantics
//! (`expressions::py_eq`); a Decimal128 / exotic value anywhere defers the whole
//! diff to pure Python.

use std::collections::BTreeSet;

use bson::{Bson, Document};

use crate::expressions;

#[derive(Debug)]
pub struct Fallback;

type R<T> = Result<T, Fallback>;

struct Acc {
    updated: Document,
    removed: Vec<Bson>,
    truncated: Vec<Bson>,
    disambiguated: Document,
}

/// mongod 6.1+ `disambiguatedPaths`: any reported path containing a
/// numeric-string FIELD name (a dict key like "1" that a reader could
/// mistake for an array index) maps to its typed segment list — Int32
/// for real array indices, String for field names. Mirrors
/// `diff._record_ambiguous` in the Python engine.
fn record_ambiguous(path: &str, segments: &[Bson], acc: &mut Acc) {
    let ambiguous = segments.iter().any(|s| match s {
        Bson::String(k) => !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    });
    if ambiguous {
        acc.disambiguated
            .insert(path.to_string(), Bson::Array(segments.to_vec()));
    }
}

fn eq(a: &Bson, b: &Bson) -> R<bool> {
    expressions::py_eq(a, b).map_err(|_| Fallback)
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Diff two documents field-by-field. Split out from [`walk`] so the top-level
/// call (and the nested doc-vs-doc case) operate on `&Document` directly, without
/// wrapping each side in an owned `Bson::Document` clone.
fn walk_docs(a: &Document, b: &Document, path: &str, segments: &[Bson], acc: &mut Acc) -> R<()> {
    // sorted union of keys (Python `sorted(pre_keys | post_keys)`)
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    for key in keys {
        let cp = child_path(path, key);
        let mut cs = segments.to_vec();
        cs.push(Bson::String(key.clone()));
        match (a.get(key), b.get(key)) {
            (Some(_), None) => {
                acc.removed.push(Bson::String(cp.clone()));
                record_ambiguous(&cp, &cs, acc);
            }
            (None, Some(bv)) => {
                acc.updated.insert(cp.clone(), bv.clone());
                record_ambiguous(&cp, &cs, acc);
            }
            (Some(av), Some(bv)) => walk(av, bv, &cp, &cs, acc)?,
            (None, None) => unreachable!(),
        }
    }
    Ok(())
}

fn walk(pre: &Bson, post: &Bson, path: &str, segments: &[Bson], acc: &mut Acc) -> R<()> {
    match (pre, post) {
        (Bson::Document(a), Bson::Document(b)) => walk_docs(a, b, path, segments, acc),
        (Bson::Array(a), Bson::Array(b)) => {
            if eq(pre, post)? {
                return Ok(());
            }
            // Post longer than pre -> wholesale replace (can't encode appends).
            if b.len() > a.len() {
                acc.updated.insert(path.to_string(), post.clone());
                record_ambiguous(path, segments, acc);
                return Ok(());
            }
            for i in 0..b.len() {
                let cp = child_path(path, &i.to_string());
                let mut cs = segments.to_vec();
                cs.push(Bson::Int32(i as i32));
                walk(&a[i], &b[i], &cp, &cs, acc)?;
            }
            if b.len() < a.len() {
                let mut entry = Document::new();
                entry.insert("field".to_string(), Bson::String(path.to_string()));
                entry.insert("newSize".to_string(), Bson::Int32(b.len() as i32));
                acc.truncated.push(Bson::Document(entry));
                record_ambiguous(path, segments, acc);
            }
            Ok(())
        }
        _ => {
            if !eq(pre, post)? {
                acc.updated.insert(path.to_string(), post.clone());
                record_ambiguous(path, segments, acc);
            }
            Ok(())
        }
    }
}

/// `{updatedFields, removedFields, truncatedArrays}` for `pre` -> `post`.
/// `Err(Fallback)` => defer to the pure-Python implementation.
pub fn compute_update_description(pre: &Document, post: &Document) -> R<Document> {
    let mut acc = Acc {
        updated: Document::new(),
        removed: Vec::new(),
        truncated: Vec::new(),
        disambiguated: Document::new(),
    };
    walk_docs(pre, post, "", &[], &mut acc)?;
    let mut out = Document::new();
    out.insert("updatedFields".to_string(), Bson::Document(acc.updated));
    out.insert("removedFields".to_string(), Bson::Array(acc.removed));
    out.insert("truncatedArrays".to_string(), Bson::Array(acc.truncated));
    if !acc.disambiguated.is_empty() {
        // Only stamped when an ambiguous path exists (mirrors Python).
        out.insert(
            "disambiguatedPaths".to_string(),
            Bson::Document(acc.disambiguated),
        );
    }
    Ok(out)
}

/// Apply a `$v: 2` `updateDescription` (`{updatedFields, removedFields,
/// truncatedArrays}`) to `doc`, returning the post-image. The inverse of
/// [`compute_update_description`] and the keystone of oplog replay (PITR): it
/// rolls a document forward without re-running the original update operators.
/// Mirrors `secantus.diff.apply_update_description`.
///
/// `disambiguatedPaths` is intentionally not consulted — every path is applied
/// against the real pre-image, whose container types (map vs array) already
/// resolve the numeric-key vs array-index ambiguity that field exists to flag
/// for a blind reader. Order matches Python: updates, then removals, then array
/// truncations.
pub fn apply_update_description(mut doc: Document, diff: &Document) -> R<Document> {
    if let Ok(updated) = diff.get_document("updatedFields") {
        for (path, value) in updated {
            crate::paths::set_path(&mut doc, path.as_str(), value.clone()).map_err(|_| Fallback)?;
        }
    }
    if let Ok(removed) = diff.get_array("removedFields") {
        for p in removed {
            if let Bson::String(path) = p {
                crate::paths::unset_path(&mut doc, path);
            }
        }
    }
    if let Ok(truncated) = diff.get_array("truncatedArrays") {
        for entry in truncated {
            let Bson::Document(e) = entry else { continue };
            let Ok(field) = e.get_str("field") else {
                continue;
            };
            let new_size = e
                .get_i32("newSize")
                .map(|n| n as usize)
                .or_else(|_| e.get_i64("newSize").map(|n| n as usize));
            let Ok(new_size) = new_size else { continue };
            // Clone-truncate-set: release the immutable `get_path` borrow before
            // the mutable `set_path`.
            let shorter = match crate::paths::get_path(&doc, field) {
                Some(Bson::Array(arr)) if new_size < arr.len() => {
                    let mut a = arr.clone();
                    a.truncate(new_size);
                    Some(a)
                }
                _ => None,
            };
            if let Some(a) = shorter {
                crate::paths::set_path(&mut doc, field, Bson::Array(a)).map_err(|_| Fallback)?;
            }
        }
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn d(pre: Document, post: Document) -> Document {
        compute_update_description(&pre, &post).expect("should not fall back")
    }

    #[test]
    fn updated_added_removed() {
        let out = d(doc! {"a": 1, "b": 2}, doc! {"a": 9, "c": 3});
        assert_eq!(
            out.get_document("updatedFields").unwrap(),
            &doc! {"a": 9, "c": 3}
        );
        assert_eq!(
            out.get_array("removedFields").unwrap(),
            &vec![Bson::String("b".into())]
        );
    }

    #[test]
    fn nested_leaf_only() {
        let out = d(doc! {"a": {"b": 1, "c": 2}}, doc! {"a": {"b": 1, "c": 9}});
        assert_eq!(out.get_document("updatedFields").unwrap(), &doc! {"a.c": 9});
    }

    #[test]
    fn numeric_bridge_no_change() {
        // 1 == 1.0 -> no update emitted.
        let out = d(doc! {"a": 1}, doc! {"a": 1.0});
        assert!(out.get_document("updatedFields").unwrap().is_empty());
    }

    #[test]
    fn array_truncation() {
        let out = d(doc! {"a": [1, 2, 3, 4]}, doc! {"a": [1, 9, 3]});
        assert_eq!(out.get_document("updatedFields").unwrap(), &doc! {"a.1": 9});
        let trunc = out.get_array("truncatedArrays").unwrap();
        assert_eq!(
            trunc,
            &vec![Bson::Document(doc! {"field": "a", "newSize": 3})]
        );
    }

    #[test]
    fn array_growth_wholesale() {
        let out = d(doc! {"a": [1, 2]}, doc! {"a": [1, 2, 3]});
        assert_eq!(
            out.get_document("updatedFields").unwrap(),
            &doc! {"a": [1, 2, 3]}
        );
    }

    /// `apply_update_description` is the exact inverse of `compute`: rolling the
    /// pre-image forward by the computed diff reproduces the post-image. Covers
    /// scalar change, nested add, field removal, and array truncation in one go.
    #[test]
    fn apply_inverts_compute() {
        let cases = [
            (doc! {"a": 1}, doc! {"a": 2}),
            (doc! {"a": 1, "b": 2}, doc! {"a": 1}), // removal
            (doc! {"a": {"b": 1}}, doc! {"a": {"b": 1, "c": 9}}), // nested add
            (doc! {"a": [1, 2, 3]}, doc! {"a": [1, 9]}), // truncate + change
            (doc! {"a": [1, 2]}, doc! {"a": [1, 2, 3]}), // growth
            (
                doc! {"x": 1, "y": [1, 2, 3], "z": {"d": 5}, "gone": true},
                doc! {"x": 2, "y": [1, 9], "z": {"d": 5, "e": 7}},
            ),
        ];
        for (pre, post) in cases {
            let diff = compute_update_description(&pre, &post).expect("compute");
            let rolled = apply_update_description(pre.clone(), &diff).expect("apply");
            assert_eq!(rolled, post, "roundtrip failed for pre={pre:?}");
        }
    }
}
