//! `updateDescription` diff — Rust port of
//! `secantus.diff.compute_update_description`, the sixth leaf engine (used to
//! build the change-stream `$v: 2` update events). Returns
//! `{updatedFields, removedFields, truncatedArrays}` for a pre->post image.
//!
//! Value equality reuses the expression engine's Python-`==` semantics
//! (`expressions::py_eq`); a Decimal128 / exotic value anywhere defers the whole
//! diff to pure Python.

use std::collections::{BTreeMap, BTreeSet};

use bson::{Bson, Document};

use crate::expressions;

pub use crate::fallback::Fallback;

type R<T> = Result<T, Fallback>;

struct Acc {
    updated: Document,
    removed: Vec<Bson>,
    truncated: Vec<Bson>,
    /// Array paths the UPDATE touched element-wise, mapped to the indices past
    /// the old end that may be reported (`None` = any, an append knows every
    /// index it wrote). `None` for the whole field means "no operator update":
    /// pipeline updates and callers with no spec diff the values instead.
    /// See this module's docs.
    elementwise: Option<BTreeMap<String, Option<BTreeSet<i64>>>>,
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
    expressions::py_eq(a, b)
}

/// Would storing `b` where `a` is leave the document unchanged?
///
/// The CHANGE-DETECTION twin of `eq`, and deliberately not the same predicate.
/// mongod answers the two questions differently:
///
/// * EQUALITY calls them the same -- `{$eq: [0.0, -0.0]}` is true, `$cmp` is 0,
///   and `find({a: -0.0})` matches a stored `0.0`;
/// * CHANGE detection does not -- `{$set: {a: -0.0}}` over `a: 0.0` writes and
///   puts the field in a change stream's `updatedFields`, and so does a numeric
///   TYPE change: `int 1` -> `double 1.0` reports `{a: 1.0}` with
///   `nModified: 1`.
///
/// Both probed against 8.2.11 (2026-09-05). Folding this into `eq` would have
/// been the tempting one-line fix and would have broken `$eq` and query
/// matching -- one predicate cannot serve both questions.
fn same_stored_value(a: &Bson, b: &Bson) -> R<bool> {
    if !eq(a, b)? {
        return Ok(false);
    }
    Ok(same_encoding(a, b))
}

/// Do two `eq`-equal values encode to the same BSON? Distinguishes a signed
/// zero (by bit pattern) and a numeric type change (by variant), recursing so
/// that either nested in an array or a subdocument counts just the same.
fn same_encoding(a: &Bson, b: &Bson) -> bool {
    match (a, b) {
        (Bson::Double(x), Bson::Double(y)) => x.to_bits() == y.to_bits(),
        (Bson::Array(x), Bson::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| same_encoding(p, q))
        }
        (Bson::Document(x), Bson::Document(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && same_encoding(v1, v2))
        }
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
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
            if same_stored_value(pre, post)? {
                return Ok(());
            }
            // mongod reports an array by the OPERATION that changed it, not by
            // diffing the values. Mirrors `_walk` in src/secantus/diff.py.
            let Some(ew) = acc.elementwise.as_ref() else {
                // Pipeline update (or no spec): diff the values. This is the one
                // shape where mongod really does emit `truncatedArrays`.
                for i in 0..b.len() {
                    let cp = child_path(path, &i.to_string());
                    let mut cs = segments.to_vec();
                    cs.push(Bson::Int32(i as i32));
                    if i >= a.len() {
                        acc.updated.insert(cp.clone(), b[i].clone());
                        record_ambiguous(&cp, &cs, acc);
                        continue;
                    }
                    walk(&a[i], &b[i], &cp, &cs, acc)?;
                }
                if b.len() < a.len() {
                    let mut entry = Document::new();
                    entry.insert("field".to_string(), Bson::String(path.to_string()));
                    entry.insert("newSize".to_string(), Bson::Int32(b.len() as i32));
                    acc.truncated.push(Bson::Document(entry));
                    record_ambiguous(path, segments, acc);
                }
                return Ok(());
            };
            let beyond = ew.get(path);
            if beyond.is_none() || b.len() < a.len() {
                // Not element-wise, or it shrank: mongod resends the array.
                acc.updated.insert(path.to_string(), post.clone());
                record_ambiguous(path, segments, acc);
                return Ok(());
            }
            let allowed = beyond.and_then(|v| v.clone());
            for i in 0..b.len() {
                let cp = child_path(path, &i.to_string());
                let mut cs = segments.to_vec();
                cs.push(Bson::Int32(i as i32));
                if i >= a.len() {
                    // An append reports every index it wrote; an indexed `$set`
                    // reports only the one it named.
                    if let Some(named) = allowed.as_ref() {
                        if !named.contains(&(i as i64)) {
                            continue;
                        }
                    }
                    acc.updated.insert(cp.clone(), b[i].clone());
                    record_ambiguous(&cp, &cs, acc);
                    continue;
                }
                walk(&a[i], &b[i], &cp, &cs, acc)?;
            }
            Ok(())
        }
        _ => {
            if !same_stored_value(pre, post)? {
                acc.updated.insert(path.to_string(), post.clone());
                record_ambiguous(path, segments, acc);
            }
            Ok(())
        }
    }
}

/// Operators whose effect on an array mongod reports element-wise, provided
/// they are a plain append (`$slice` / `$sort` / `$position` reorder or shrink
/// the array, and mongod then sends the whole thing).
const APPEND_OPS: &[&str] = &["$push", "$addToSet"];
const NON_APPEND_MODIFIERS: &[&str] = &["$slice", "$sort", "$position"];
/// Operators that write ONE named path. An indexed path under any of them makes
/// that array element-wise -- checked against mongod 8.2.11.
const PATH_WRITE_OPS: &[&str] = &[
    "$set",
    "$unset",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$currentDate",
    "$bit",
];

/// Array paths this update touches in a way mongod reports element-wise.
/// Mirrors `_elementwise_array_paths` in `src/secantus/diff.py`.
fn elementwise_array_paths(update: &Document) -> BTreeMap<String, Option<BTreeSet<i64>>> {
    let mut paths: BTreeMap<String, Option<BTreeSet<i64>>> = BTreeMap::new();
    for (op, spec) in update.iter() {
        let Some(spec) = spec.as_document() else {
            continue;
        };
        if APPEND_OPS.contains(&op.as_str()) {
            for (field, value) in spec.iter() {
                if let Some(d) = value.as_document() {
                    if NON_APPEND_MODIFIERS.iter().any(|m| d.contains_key(*m)) {
                        continue; // reorders or truncates -> whole array
                    }
                }
                paths.insert(field.clone(), None);
            }
        } else if PATH_WRITE_OPS.contains(&op.as_str()) {
            for field in spec.keys() {
                let parts: Vec<&str> = field.split('.').collect();
                for (i, part) in parts.iter().enumerate() {
                    if i == 0 || !part.chars().all(|c| c.is_ascii_digit()) {
                        continue;
                    }
                    let prefix = parts[..i].join(".");
                    match paths.get(&prefix) {
                        Some(None) => continue, // an append already allowed any index
                        _ => {
                            let entry =
                                paths.entry(prefix).or_insert_with(|| Some(BTreeSet::new()));
                            if let Some(set) = entry.as_mut() {
                                if let Ok(n) = part.parse::<i64>() {
                                    set.insert(n);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    paths
}

/// `{updatedFields, removedFields, truncatedArrays}` for `pre` -> `post`.
/// `Err(Fallback::Defer)` => defer to the pure-Python implementation.
pub fn compute_update_description(pre: &Document, post: &Document) -> R<Document> {
    compute_update_description_for(pre, post, None)
}

/// As above, told which update produced `post`. `update` is the operator
/// document; pass `None` for a pipeline update or when the spec is unknown, and
/// arrays are diffed by value (which is what mongod does for pipelines).
pub fn compute_update_description_for(
    pre: &Document,
    post: &Document,
    update: Option<&Document>,
) -> R<Document> {
    let mut acc = Acc {
        updated: Document::new(),
        removed: Vec::new(),
        truncated: Vec::new(),
        disambiguated: Document::new(),
        elementwise: update.map(elementwise_array_paths),
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
            crate::paths::set_path(&mut doc, path.as_str(), value.clone())
                .map_err(|_| Fallback::Defer)?;
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
                crate::paths::set_path(&mut doc, field, Bson::Array(a))
                    .map_err(|_| Fallback::Defer)?;
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
    fn a_numeric_type_change_is_a_change() {
        // This asserted the OPPOSITE, justified by `1 == 1.0 -> no update
        // emitted` -- Python's equality rule, cited instead of the server's.
        // mongod reports the change: `{$set: {a: 1.0}}` over a stored `int` 1
        // answers `updatedFields: {a: 1.0}` with `nModified: 1` and stores a
        // double. The consumer of a change stream was never told the field's
        // TYPE had changed (probed 8.2.11, 2026-09-05).
        let out = d(doc! {"a": 1}, doc! {"a": 1.0});
        assert_eq!(out.get_document("updatedFields").unwrap(), &doc! {"a": 1.0});
    }

    #[test]
    fn a_signed_zero_flip_is_a_change() {
        // Same rule, the other shape it was blind to: `0.0` and `-0.0` are
        // EQUAL for `$eq` and for query matching, and DIFFERENT for change
        // detection. Both measured on 8.2.11 (2026-09-05).
        let out = d(doc! {"a": 0.0}, doc! {"a": -0.0});
        assert_eq!(
            out.get_document("updatedFields").unwrap(),
            &doc! {"a": -0.0}
        );
        // ...including nested in an array, which the fast path used to skip.
        let nested = d(doc! {"a": [0.0]}, doc! {"a": [-0.0]});
        assert!(!nested.get_document("updatedFields").unwrap().is_empty());
    }

    #[test]
    fn an_unchanged_value_is_still_no_change() {
        // The guard against over-reporting: same value, same type, no entry.
        let out = d(doc! {"a": 1.0, "b": "x"}, doc! {"a": 1.0, "b": "x"});
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

    /// Growth used to wholesale-replace here. Measured against mongod 8.2.11 it
    /// is reported positionally -- for a pipeline update (no spec) and for a
    /// `$push` alike. Only a whole-field operator `$set` resends the array.
    #[test]
    fn array_growth_reports_appended_indices() {
        let out = d(doc! {"a": [1, 2]}, doc! {"a": [1, 2, 3]});
        assert_eq!(out.get_document("updatedFields").unwrap(), &doc! {"a.2": 3});

        let pushed = compute_update_description_for(
            &doc! {"a": [1, 2]},
            &doc! {"a": [1, 2, 3]},
            Some(&doc! {"$push": {"a": 3}}),
        )
        .unwrap();
        assert_eq!(
            pushed.get_document("updatedFields").unwrap(),
            &doc! {"a.2": 3}
        );

        let whole = compute_update_description_for(
            &doc! {"a": [1, 2]},
            &doc! {"a": [1, 2, 3]},
            Some(&doc! {"$set": {"a": [1, 2, 3]}}),
        )
        .unwrap();
        assert_eq!(
            whole.get_document("updatedFields").unwrap(),
            &doc! {"a": [1, 2, 3]}
        );
    }

    /// A shrink by an OPERATOR resends the array; the same shrink with no spec
    /// (pipeline form) reports a truncation. Both measured on 8.2.11.
    #[test]
    fn array_shrink_depends_on_the_operation() {
        let popped = compute_update_description_for(
            &doc! {"a": [1, 2, 3]},
            &doc! {"a": [1, 2]},
            Some(&doc! {"$pop": {"a": 1}}),
        )
        .unwrap();
        assert_eq!(
            popped.get_document("updatedFields").unwrap(),
            &doc! {"a": [1, 2]}
        );
        assert!(popped.get_array("truncatedArrays").unwrap().is_empty());

        let pipeline = d(doc! {"a": [1, 2, 3]}, doc! {"a": [1, 2]});
        assert_eq!(pipeline.get_array("truncatedArrays").unwrap().len(), 1);
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
