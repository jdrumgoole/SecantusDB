//! Projection — Rust port of `secantus.projection.apply_projection`, the fifth
//! leaf engine (`find()`'s `projection` argument). Same graceful-fallback
//! design: reproduce the common inclusion / exclusion / `$slice` / `$elemMatch`
//! shapes, and defer the whole call to Python for anything else (mixed
//! inclusion/exclusion which Python raises on, nested-document specs, unusual
//! `$slice` argument types, or a `$elemMatch` sub-filter the matcher defers).

use bson::{Bson, Document, RawDocument};

use crate::{paths, query};

#[derive(Debug)]
pub struct Fallback;

type R<T> = Result<T, Fallback>;

/// Raw-BSON projection fast path for the common case: a **pure top-level
/// inclusion** spec (`{a: 1, b: 1}`, optionally with `_id: 0`) — no dotted
/// paths, no operators (`$slice` / `$elemMatch` / `$meta`), no positional, no
/// exclusion. For that shape the output has only `_id` (unless dropped) plus the
/// included fields, so we decode ONLY those from `raw` instead of materialising
/// the whole document (the return-path materialization,
/// `tasks/rust-perf-findings.md`). The result is byte-identical to
/// `apply_projection(&raw.to_document(), spec, None)` — including field order:
/// `_id` first, then the included fields in sorted order (`apply_projection`'s
/// inclusion path emits them through a `BTreeMap`). Returns `None` for any spec
/// this fast path doesn't cover, so the caller falls back to the full
/// `apply_projection` on a decoded document.
pub fn apply_projection_raw(raw: &RawDocument, spec: &Document) -> Option<Document> {
    if spec.is_empty() {
        return None; // whole-doc copy — caller/splice handles it
    }
    let mut include_fields: Vec<&str> = Vec::new();
    let mut include_id = true;
    for (k, v) in spec {
        if k.contains('.') || k.ends_with(".$") {
            return None; // dotted / positional -> full projection
        }
        // Only canonical inclusion scalars; a document value is an operator
        // ($slice/$elemMatch/$meta), and a string / null / exotic `_id` spec has
        // special rules — defer all of those.
        let truthy = match v {
            Bson::Int32(n) => *n != 0,
            Bson::Int64(n) => *n != 0,
            Bson::Double(d) => *d != 0.0,
            Bson::Boolean(b) => *b,
            _ => return None,
        };
        if k == "_id" {
            include_id = truthy;
        } else if truthy {
            include_fields.push(k.as_str());
        } else {
            return None; // exclusion of a real field -> not pure inclusion
        }
    }
    if include_fields.is_empty() {
        return None; // `_id`-only / degenerate specs -> let the full path handle
    }
    // Match `apply_projection`'s inclusion order: `_id` first (when kept and
    // present), then the included fields sorted (its SpecTree is a BTreeMap).
    include_fields.sort_unstable();
    include_fields.dedup();

    let mut out = Document::new();
    if include_id {
        if let Ok(Some(v)) = raw.get("_id") {
            out.insert("_id".to_string(), Bson::try_from(v).ok()?);
        }
    }
    for f in include_fields {
        // A field absent from the document is simply omitted (matching
        // `include_doc`'s `doc.get(key)` guard).
        if let Ok(Some(v)) = raw.get(f) {
            out.insert(f.to_string(), Bson::try_from(v).ok()?);
        }
    }
    Some(out)
}

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

/// A `{$meta: <arg>}` projection value. SecantusDB doesn't compute any metadata,
/// so a recognized `$meta` field is *omitted* from the projected doc (partial —
/// graceful degradation). The two error cases (an unknown argument → Location17308,
/// `textScore` without a `$text` query → Location40218) are validated at parse
/// time in the command layer (`find::projection_meta_error`), which owns the
/// `CommandError` type; here we just recognize and drop the field.
pub fn meta_spec(v: &Bson) -> Option<&str> {
    if let Bson::Document(d) = v {
        if d.len() == 1 {
            if let Some(Bson::String(arg)) = d.get("$meta") {
                return Some(arg.as_str());
            }
        }
    }
    None
}

/// The `$meta` keywords mongod recognizes. Anything else is a Location17308.
pub const META_KEYWORDS: &[&str] = &[
    "textScore",
    "indexKey",
    "recordId",
    "sortKey",
    "searchScore",
    "searchHighlights",
    "geoNearDistance",
    "geoNearPoint",
    "vectorSearchScore",
];

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

/// A valid projection `$slice` argument (mongod): a number, or a `[skip, limit]`
/// pair with a positive numeric limit. Returns `Some((skip, limit))` for the pair
/// form, `Some((n, i64::MIN))` sentinel for the scalar form, else `None` (Python
/// raises 28667 / 28724). The scalar sentinel keeps the two forms distinguishable.
fn slice_bounds(slice_arg: &Bson) -> Option<(i64, i64)> {
    if let Some(n) = as_slice_int(slice_arg) {
        return Some((n, i64::MIN));
    }
    if let Bson::Array(args) = slice_arg {
        if args.len() == 2 {
            if let (Some(skip), Some(limit)) = (as_slice_int(&args[0]), as_slice_int(&args[1])) {
                if limit > 0 {
                    return Some((skip, limit));
                }
            }
        }
    }
    None
}

fn apply_slice(value: Bson, slice_arg: &Bson) -> R<Bson> {
    // Validate the argument shape up front (mongod parse-time) so an invalid
    // $slice on a non-array field also defers to Python.
    let Some((first, limit)) = slice_bounds(slice_arg) else {
        return Err(Fallback);
    };
    let Bson::Array(a) = &value else {
        return Ok(value); // non-array passes through unchanged
    };
    let len = a.len() as i64;
    if limit == i64::MIN {
        // Scalar form: first / last `first` elements.
        let out = if first >= 0 {
            a[..first.min(len).max(0) as usize].to_vec()
        } else {
            a[(len + first).max(0) as usize..].to_vec()
        };
        return Ok(Bson::Array(out));
    }
    // [skip, limit] with a positive limit.
    let skip = if first < 0 {
        (len + first).max(0)
    } else {
        first.min(len)
    };
    let tail = &a[skip as usize..];
    let out = tail[..(limit as usize).min(tail.len())].to_vec();
    Ok(Bson::Array(out))
}

/// Build the per-element predicate a positional `arr.$` projects against, from the
/// query's clauses on `array_path`. Returns `(doc_pred, value_pred)` — a
/// sub-document match (from `arr.sub` clauses and an `arr: {$elemMatch: E}` clause)
/// and an optional direct value/operator predicate (from `arr: <value|ops>`). None
/// when the query has no clause on `array_path` (mongod errors Location51246).
/// Mirrors `projection._positional_element_predicate`.
fn positional_predicate(
    query: Option<&Document>,
    array_path: &str,
) -> Option<(Document, Option<Bson>)> {
    let query = query?;
    let mut doc_pred = Document::new();
    let mut value_pred: Option<Bson> = None;
    let prefix = format!("{array_path}.");
    let mut found = false;
    for (key, val) in query {
        if key == array_path {
            found = true;
            match val {
                Bson::Document(d) if d.len() == 1 && d.contains_key("$elemMatch") => {
                    if let Some(Bson::Document(em)) = d.get("$elemMatch") {
                        for (k, v) in em {
                            doc_pred.insert(k.clone(), v.clone());
                        }
                    }
                }
                other => value_pred = Some(other.clone()),
            }
        } else if let Some(sub) = key.strip_prefix(&prefix) {
            found = true;
            doc_pred.insert(sub.to_string(), val.clone());
        }
    }
    if !found {
        return None;
    }
    Some((doc_pred, value_pred))
}

/// Validate a positional projection up-front (mongod validates at parse time, so
/// an invalid `arr.$` errors even when nothing matches). `Err(Fallback)` for the
/// error cases (>1 positional / exclusion / array field not in the query) — the
/// find handler surfaces it as `BadValue`, and the pure-Python oracle raises the
/// exact Location code (31276 / 31395 / 51246).
pub fn validate_projection(spec: &Document, query: Option<&Document>) -> R<()> {
    let positional: Vec<&String> = spec.keys().filter(|k| k.ends_with(".$")).collect();
    if positional.is_empty() {
        return Ok(());
    }
    if positional.len() > 1 {
        return Err(Fallback);
    }
    let key = positional[0];
    if !spec_truthy(&spec[key])? {
        return Err(Fallback);
    }
    let array_path = &key[..key.len() - 2];
    if positional_predicate(query, array_path).is_none() {
        return Err(Fallback);
    }
    Ok(())
}

/// First element of `arr` matching the positional predicate, or `Ok(None)`.
fn positional_first(
    arr: Option<&Bson>,
    doc_pred: &Document,
    value_pred: Option<&Bson>,
) -> R<Option<Bson>> {
    let Some(Bson::Array(arr)) = arr else {
        return Ok(None);
    };
    let empty = Document::new();
    for elem in arr {
        if !doc_pred.is_empty() {
            match elem {
                Bson::Document(ed) => {
                    if !query::matches(ed, doc_pred, &empty, None).map_err(|_| Fallback)? {
                        continue;
                    }
                }
                _ => continue,
            }
        }
        if let Some(vp) = value_pred {
            let mut wrapper = Document::new();
            wrapper.insert("_".to_string(), elem.clone());
            let mut q = Document::new();
            q.insert("_".to_string(), vp.clone());
            if !query::matches(&wrapper, &q, &empty, None).map_err(|_| Fallback)? {
                continue;
            }
        }
        return Ok(Some(elem.clone()));
    }
    Ok(None)
}

/// Inclusion projection carrying a positional `arr.$`: the other requested fields
/// plus `array_path: [first-matching-element]`. Mirrors `projection._apply_positional`.
fn apply_positional(
    doc: &Document,
    spec_main: &[(&str, &Bson)],
    slice_specs: &[(&str, &Bson)],
    array_path: &str,
    doc_pred: &Document,
    value_pred: Option<Bson>,
) -> R<Document> {
    let mut result = Document::new();
    let id_spec = spec_main.iter().find(|(k, _)| *k == "_id").map(|(_, v)| *v);
    let include_id = match id_spec {
        None => true,
        Some(v) => spec_truthy(v)?,
    };
    if include_id {
        if let Some(id) = doc.get("_id") {
            result.insert("_id".to_string(), id.clone());
        }
    }
    let non_id: Vec<(&str, &Bson)> = spec_main
        .iter()
        .copied()
        .filter(|(k, _)| *k != "_id")
        .collect();
    // Positional forces inclusion; a companion exclusion is the mix mongod rejects.
    for (_, v) in &non_id {
        if elem_match_spec(v).is_none() && !spec_truthy(v)? {
            return Err(Fallback); // Location31254 -> Python
        }
    }
    let plain: Vec<&str> = non_id
        .iter()
        .filter(|(_, v)| elem_match_spec(v).is_none())
        .map(|(k, _)| *k)
        .collect();
    if !plain.is_empty() {
        for (k, v) in include_doc(doc, &spec_tree(&plain)) {
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
    if let Some(first) = positional_first(
        paths::get_path(doc, array_path),
        doc_pred,
        value_pred.as_ref(),
    )? {
        set(&mut result, array_path, Bson::Array(vec![first]))?;
    }
    for (path, slice_arg) in slice_specs {
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
    Ok(result)
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
pub fn apply_projection(doc: &Document, spec: &Document, query: Option<&Document>) -> R<Document> {
    if spec.is_empty() {
        return Ok(doc.clone());
    }

    // A `$meta` field is inclusion-mode in mongod, but SecantusDB doesn't compute
    // the metadata — so the field is *omitted* (partial degradation). Drop the
    // meta keys; a spec that was *only* `$meta` fields becomes an inclusion of no
    // fields (result: just `_id`, unless `_id` was excluded). Parse-time
    // validation (Location17308 / 40218) lives in `find::projection_meta_error`.
    if spec.values().any(|v| meta_spec(v).is_some()) {
        let mut stripped = Document::new();
        for (k, v) in spec {
            if meta_spec(v).is_none() {
                stripped.insert(k.clone(), v.clone());
            }
        }
        let non_meta_non_id = stripped.keys().any(|k| k != "_id");
        if !non_meta_non_id {
            let mut result = Document::new();
            let include_id = match stripped.get("_id") {
                None => true,
                Some(v) => spec_truthy(v)?,
            };
            if include_id {
                if let Some(id) = doc.get("_id") {
                    result.insert("_id".to_string(), id.clone());
                }
            }
            return Ok(result);
        }
        return apply_projection(doc, &stripped, query);
    }

    // Separate $slice specs (neutral modifiers) and positional (`arr.$`) keys from
    // the inclusion/exclusion set.
    let mut slice_specs: Vec<(&str, &Bson)> = Vec::new();
    let mut positional: Vec<(&str, &Bson)> = Vec::new();
    let mut spec_main: Vec<(&str, &Bson)> = Vec::new();
    for (k, v) in spec {
        if let Some(arg) = slice_spec(v) {
            slice_specs.push((k, arg));
        } else if k.ends_with(".$") {
            positional.push((k, v));
        } else {
            spec_main.push((k, v));
        }
    }

    if !positional.is_empty() {
        // >1 positional, an exclusion positional, or a positional whose array field
        // isn't in the query all error in mongod — defer to Python for the exact
        // Location code (31276 / 31395 / 51246).
        if positional.len() > 1 {
            return Err(Fallback);
        }
        let (pos_key, pos_val) = positional[0];
        if !spec_truthy(pos_val)? {
            return Err(Fallback);
        }
        let array_path = &pos_key[..pos_key.len() - 2]; // strip ".$"
        let Some((doc_pred, value_pred)) = positional_predicate(query, array_path) else {
            return Err(Fallback); // no query clause on the array -> 51246
        };
        return apply_positional(
            doc,
            &spec_main,
            &slice_specs,
            array_path,
            &doc_pred,
            value_pred,
        );
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
        let owned = apply_projection(&d, &s, None).expect("should not fall back");
        // Wherever the raw fast path claims a spec, it must produce the
        // byte-identical document — every inclusion projection test below thus
        // doubles as an apply_projection_raw parity check.
        let bytes = bson::to_vec(&d).unwrap();
        let raw = RawDocument::from_bytes(&bytes).unwrap();
        if let Some(fast) = apply_projection_raw(raw, &s) {
            assert_eq!(
                fast, owned,
                "apply_projection_raw != apply_projection for doc={d:?} spec={s:?}"
            );
        }
        owned
    }

    fn raw_of(d: &Document) -> Vec<u8> {
        bson::to_vec(d).unwrap()
    }

    #[test]
    fn raw_fast_path_activates_only_for_pure_inclusion() {
        let d = doc! {"_id": 1, "a": 10, "b": 20, "c": 30};
        let b = raw_of(&d);
        let raw = RawDocument::from_bytes(&b).unwrap();
        // Pure top-level inclusion -> fast path, byte-identical to owned.
        for spec in [
            doc! {"a": 1},
            doc! {"b": 1, "a": 1},
            doc! {"a": 1, "_id": 0},
        ] {
            let fast = apply_projection_raw(raw, &spec).expect("inclusion should fast-path");
            assert_eq!(fast, apply_projection(&d, &spec, None).unwrap());
        }
        // Everything else must defer (None) so the owned path runs.
        for spec in [
            doc! {"a": 0},             // exclusion
            doc! {"a": 1, "b": 0},     // mixed
            doc! {"a.x": 1},           // dotted
            doc! {"a": {"$slice": 1}}, // operator
            doc! {"_id": 1},           // _id-only
            doc! {},                   // empty
        ] {
            assert!(
                apply_projection_raw(raw, &spec).is_none(),
                "spec should defer: {spec:?}"
            );
        }
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
        assert!(apply_projection(&doc! {"a": 1, "b": 2}, &doc! {"a": 1, "b": 0}, None).is_err());
    }

    #[test]
    fn meta_only_field_omitted() {
        // A recognized-but-unsupported $meta arg: field omitted, inclusion keeps _id.
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": 1, "b": 2},
                doc! {"m": {"$meta": "indexKey"}}
            ),
            doc! {"_id": 1}
        );
    }

    #[test]
    fn meta_alongside_inclusion() {
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": 1, "b": 2},
                doc! {"a": 1, "score": {"$meta": "recordId"}}
            ),
            doc! {"_id": 1, "a": 1}
        );
    }

    #[test]
    fn meta_excludes_id() {
        assert_eq!(
            proj(
                doc! {"_id": 1, "a": 1},
                doc! {"_id": 0, "score": {"$meta": "sortKey"}}
            ),
            doc! {}
        );
    }

    #[test]
    fn meta_spec_recognizes_and_keywords() {
        assert_eq!(
            meta_spec(&bson::bson!({"$meta": "textScore"})),
            Some("textScore")
        );
        assert_eq!(meta_spec(&bson::bson!({"$meta": 5})), None);
        assert_eq!(meta_spec(&bson::bson!({"a": 1})), None);
        assert!(META_KEYWORDS.contains(&"textScore"));
        assert!(!META_KEYWORDS.contains(&"bogus"));
    }
}
