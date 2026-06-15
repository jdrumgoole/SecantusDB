//! Projection — Rust port of `secantus.projection.apply_projection`, the fifth
//! leaf engine (`find()`'s `projection` argument). Same graceful-fallback
//! design: reproduce the common inclusion / exclusion / `$slice` / `$elemMatch`
//! shapes, and defer the whole call to Python for anything else (mixed
//! inclusion/exclusion which Python raises on, nested-document specs, unusual
//! `$slice` argument types, or a `$elemMatch` sub-filter the matcher defers).

use bson::{Bson, Document};

use crate::{paths, query};

#[derive(Debug)]
pub struct Fallback;

type R<T> = Result<T, Fallback>;

fn elem_match_spec(v: &Bson) -> Option<&Bson> {
    if let Bson::Document(d) = v {
        if d.len() == 1 {
            if let Some(sub) = d.get("$elemMatch") {
                return Some(sub);
            }
        }
    }
    None
}

fn slice_spec(v: &Bson) -> Option<&Bson> {
    if let Bson::Document(d) = v {
        if d.len() == 1 {
            if let Some(arg) = d.get("$slice") {
                return Some(arg);
            }
        }
    }
    None
}

/// Truthiness of an inclusion/exclusion spec value. Only the literal 0/1 /
/// bool / double forms are reproduced; anything else (nested doc, string, …)
/// defers (Python's quirky `bool()` semantics).
fn spec_truthy(v: &Bson) -> R<bool> {
    match v {
        Bson::Int32(n) => Ok(*n != 0),
        Bson::Int64(n) => Ok(*n != 0),
        Bson::Boolean(b) => Ok(*b),
        Bson::Double(d) => Ok(*d != 0.0),
        _ => Err(Fallback),
    }
}

/// Whether an `_id` spec value means "drop `_id`" (Python compares `== 0`,
/// which is true for `0` / `false` / `0.0`).
fn is_drop_id(v: &Bson) -> bool {
    matches!(v, Bson::Int32(0) | Bson::Int64(0) | Bson::Boolean(false))
        || matches!(v, Bson::Double(d) if *d == 0.0)
}

fn as_slice_int(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        Bson::Double(d) if d.is_finite() => Some(*d as i64),
        _ => None,
    }
}

fn apply_slice(value: Bson, slice_arg: &Bson) -> R<Bson> {
    let Bson::Array(a) = &value else {
        return Ok(value); // non-array passes through unchanged
    };
    let len = a.len() as i64;
    if let Some(n) = as_slice_int(slice_arg) {
        let out = if n >= 0 {
            a[..n.min(len).max(0) as usize].to_vec()
        } else {
            a[(len + n).max(0) as usize..].to_vec()
        };
        return Ok(Bson::Array(out));
    }
    if let Bson::Array(args) = slice_arg {
        if args.len() == 2 {
            let (Some(raw_skip), Some(limit)) = (as_slice_int(&args[0]), as_slice_int(&args[1]))
            else {
                return Err(Fallback);
            };
            let skip = if raw_skip < 0 {
                (len + raw_skip).max(0)
            } else {
                raw_skip.min(len)
            };
            let tail = &a[skip as usize..];
            let tlen = tail.len() as i64;
            let out = if limit >= 0 {
                tail[..limit.min(tlen).max(0) as usize].to_vec()
            } else {
                tail[(tlen + limit).max(0) as usize..].to_vec()
            };
            return Ok(Bson::Array(out));
        }
    }
    Err(Fallback) // unrecognised $slice argument shape -> Python
}

/// First array element under `path` matching `sub_filter` (`$elemMatch`
/// projection). `Ok(None)` for no match / non-array.
fn first_match(doc: &Document, path: &str, sub_filter: &Document) -> R<Option<Bson>> {
    let Some(Bson::Array(arr)) = paths::get_path(doc, path) else {
        return Ok(None);
    };
    let empty = Document::new();
    for elem in arr {
        let hit = match elem {
            Bson::Document(ed) => {
                query::matches(ed, sub_filter, &empty, None).map_err(|_| Fallback)?
            }
            scalar => {
                // matches({"_": elem}, {"_": sub_filter})
                let mut wrapper = Document::new();
                wrapper.insert("_".to_string(), scalar.clone());
                let mut q = Document::new();
                q.insert("_".to_string(), Bson::Document(sub_filter.clone()));
                query::matches(&wrapper, &q, &empty, None).map_err(|_| Fallback)?
            }
        };
        if hit {
            return Ok(Some(elem.clone()));
        }
    }
    Ok(None)
}

fn set(doc: &mut Document, path: &str, value: Bson) -> R<()> {
    paths::set_path(doc, path, value).map_err(|_| Fallback)
}

/// Apply a projection spec to a document. `Err(Fallback)` => defer to the
/// pure-Python `apply_projection` (which also raises the mixed-mode error).
pub fn apply_projection(doc: &Document, spec: &Document) -> R<Document> {
    if spec.is_empty() {
        return Ok(doc.clone());
    }

    // Separate $slice specs (neutral modifiers) from the inclusion/exclusion set.
    let mut slice_specs: Vec<(&str, &Bson)> = Vec::new();
    let mut spec_main: Vec<(&str, &Bson)> = Vec::new();
    for (k, v) in spec {
        match slice_spec(v) {
            Some(arg) => slice_specs.push((k, arg)),
            None => spec_main.push((k, v)),
        }
    }

    let id_spec: Option<&Bson> = spec_main.iter().find(|(k, _)| *k == "_id").map(|(_, v)| *v);
    let non_id: Vec<(&str, &Bson)> = spec_main
        .iter()
        .copied()
        .filter(|(k, _)| *k != "_id")
        .collect();

    if non_id.is_empty() {
        // The spec is at most an `_id` entry plus `$slice` modifiers.
        // mongod's rules (oracle-pinned, mirrored in projection.py):
        //   * non-zero `_id` (incl. null and "") => INCLUSION: only `_id`
        //     plus any $slice'd fields survive;
        //   * numeric zero / false => whole doc minus `_id`;
        //   * no `_id` key => whole doc ($slice applied in place).
        if let Some(id_v) = id_spec {
            if !is_drop_id(id_v) {
                let mut result = Document::new();
                if let Some(idv) = doc.get("_id") {
                    result.insert("_id", idv.clone());
                }
                for (path, slice_arg) in &slice_specs {
                    if let Some(current) = paths::get_path(doc, path).cloned() {
                        let sliced = apply_slice(current, slice_arg)?;
                        set(&mut result, path, sliced)?;
                    }
                }
                return Ok(result);
            }
        }
        let mut result = doc.clone();
        if id_spec.is_some() {
            result.remove("_id");
        }
        apply_slices(&mut result, &slice_specs)?;
        return Ok(result);
    }

    // Inclusion vs exclusion: elemMatch counts as truthy (inclusion).
    let mut truthy: Vec<bool> = Vec::new();
    for (_, v) in &non_id {
        truthy.push(if elem_match_spec(v).is_some() {
            true
        } else {
            spec_truthy(v)?
        });
    }
    let inclusion = if truthy.iter().all(|&t| t) {
        true
    } else if !truthy.iter().any(|&t| t) {
        false
    } else {
        return Err(Fallback); // mixed inclusion/exclusion -> Python raises
    };

    if inclusion {
        let mut result = Document::new();
        let include_id = match id_spec {
            None => true,
            Some(v) => spec_truthy(v)?,
        };
        if include_id {
            if let Some(id) = doc.get("_id") {
                result.insert("_id".to_string(), id.clone());
            }
        }
        // Plain inclusion paths go through a path trie so dotted
        // segments fan over array elements (mirrors projection.py's
        // _include_doc; oracle-pinned semantics).
        let plain: Vec<&str> = non_id
            .iter()
            .filter(|(_, v)| elem_match_spec(v).is_none())
            .map(|(k, _)| *k)
            .collect();
        if !plain.is_empty() {
            let tree = spec_tree(&plain);
            for (k, v) in include_doc(doc, &tree) {
                result.insert(k, v);
            }
        }
        for (path, value) in &non_id {
            if let Some(sub) = elem_match_spec(value) {
                let Bson::Document(subf) = sub else {
                    return Err(Fallback);
                };
                if let Some(first) = first_match(doc, path, subf)? {
                    set(&mut result, path, Bson::Array(vec![first]))?;
                }
            }
        }
        // $slice implicitly includes its path in inclusion mode.
        for (path, slice_arg) in &slice_specs {
            if !paths::has_path(&result, path) {
                if let Some(extracted) = paths::get_path(doc, path) {
                    set(&mut result, path, extracted.clone())?;
                }
            }
            if let Some(current) = paths::get_path(&result, path).cloned() {
                let sliced = apply_slice(current, slice_arg)?;
                set(&mut result, path, sliced)?;
            }
        }
        return Ok(result);
    }

    // Exclusion mode: trie-walk so dotted unsets map over array
    // elements (non-document elements survive untouched).
    let mut result = doc.clone();
    let all_paths: Vec<&str> = non_id.iter().map(|(k, _)| *k).collect();
    exclude_doc(&mut result, &spec_tree(&all_paths));
    if id_spec.map(is_drop_id).unwrap_or(false) {
        result.remove("_id");
    }
    apply_slices(&mut result, &slice_specs)?;
    Ok(result)
}

/// Dotted paths -> nested trie; a leaf is an empty subtree.
#[derive(Default)]
struct SpecTree(std::collections::BTreeMap<String, SpecTree>);

fn spec_tree(paths: &[&str]) -> SpecTree {
    let mut root = SpecTree::default();
    for p in paths {
        let mut node = &mut root;
        for seg in p.split('.') {
            node = node.0.entry(seg.to_string()).or_default();
        }
    }
    root
}

/// Inclusion projection of `doc` against a path trie (mirrors
/// projection.py's `_include_doc`): a leaf copies the whole value; an
/// interior segment recurses into documents (keeping the `{}` skeleton
/// when the leaf is absent), maps over array elements (documents
/// project — possibly to `{}` — and scalar elements drop), and drops
/// the field entirely on a scalar. Numeric segments are field names.
fn include_doc(doc: &Document, tree: &SpecTree) -> Document {
    let mut out = Document::new();
    for (key, subtree) in &tree.0 {
        let Some(val) = doc.get(key) else { continue };
        if subtree.0.is_empty() {
            out.insert(key.clone(), val.clone());
            continue;
        }
        if let Some(projected) = include_value(val, subtree) {
            out.insert(key.clone(), projected);
        }
    }
    out
}

fn include_value(val: &Bson, subtree: &SpecTree) -> Option<Bson> {
    match val {
        Bson::Document(d) => Some(Bson::Document(include_doc(d, subtree))),
        Bson::Array(arr) => Some(Bson::Array(
            arr.iter()
                .filter(|e| matches!(e, Bson::Document(_) | Bson::Array(_)))
                .filter_map(|e| include_value(e, subtree))
                .collect(),
        )),
        _ => None,
    }
}

/// Exclusion projection: unset trie leaves, recursing through
/// documents and mapping over array elements (mirrors `_exclude_doc`).
fn exclude_doc(doc: &mut Document, tree: &SpecTree) {
    for (key, subtree) in &tree.0 {
        if subtree.0.is_empty() {
            doc.remove(key);
        } else if let Some(val) = doc.get_mut(key) {
            exclude_value(val, subtree);
        }
    }
}

fn exclude_value(val: &mut Bson, subtree: &SpecTree) {
    match val {
        Bson::Document(d) => exclude_doc(d, subtree),
        Bson::Array(arr) => {
            for elem in arr.iter_mut() {
                exclude_value(elem, subtree);
            }
        }
        _ => {}
    }
}

fn apply_slices(result: &mut Document, slice_specs: &[(&str, &Bson)]) -> R<()> {
    for (path, slice_arg) in slice_specs {
        if let Some(current) = paths::get_path(result, path).cloned() {
            let sliced = apply_slice(current, slice_arg)?;
            set(result, path, sliced)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn proj(d: Document, s: Document) -> Document {
        apply_projection(&d, &s).expect("should not fall back")
    }

    #[test]
    fn inclusion() {
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": 1, "b": 2, "c": 3},
                doc! {"a": 1, "c": 1}
            ),
            doc! {"_id": 1, "a": 1, "c": 3}
        );
    }

    #[test]
    fn inclusion_without_id() {
        assert_eq!(
            proj(doc! {"_id": 1, "a": 1, "b": 2}, doc! {"a": 1, "_id": 0}),
            doc! {"a": 1}
        );
    }

    #[test]
    fn exclusion() {
        assert_eq!(
            proj(doc! {"_id": 1, "a": 1, "b": 2}, doc! {"b": 0}),
            doc! {"_id": 1, "a": 1}
        );
    }

    #[test]
    fn dotted_inclusion() {
        assert_eq!(
            proj(doc! {"_id": 1, "a": {"b": 2, "c": 3}}, doc! {"a.b": 1}),
            doc! {"_id": 1, "a": {"b": 2}}
        );
    }

    #[test]
    fn slice_and_mixed_defer() {
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": [1, 2, 3, 4]},
                doc! {"a": {"$slice": 2}}
            ),
            doc! {"_id": 1, "a": [1, 2]}
        );
        // mixed inclusion + exclusion -> defer
        assert!(apply_projection(&doc! {"a": 1, "b": 2}, &doc! {"a": 1, "b": 0}).is_err());
    }
}
