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

/// The array index `part` names, or `None` when it names no index at all.
///
/// Canonical digits only: mongod reads `x.0` as an index and `x.00` as a field
/// name, so `"00"` gets no index reading.
pub fn index_component(part: &str) -> Option<usize> {
    if !is_digits(part) || (part.len() > 1 && part.starts_with('0')) {
        return None;
    }
    part.parse::<usize>().ok()
}

/// `(value, indexed)` pairs for a SORT path, `indexed` per value.
///
/// The same walk as [`get_path_values`] -- both readings of a numeric component
/// over an array -- but it reports, per value, whether the LAST component was
/// consumed as an array INDEX. mongod descends one level into an array-valued
/// sort key reached by a field name and does NOT descend one reached by an
/// index, so `{x: [[5]]}` sorted by `x.0` ranks among the ARRAYS (its key is
/// `[5]`) while `{x: [{y: [1, 2]}]}` sorted by `x.y` ranks by `1`.
///
/// Both servers descended in every case, so `x.0` over `[[5]]` sorted as the
/// NUMBER 5: wrong order, and wrong RESULTS under a `limit`. Measured against
/// 8.2.11 over twelve shapes in both sort directions, 2026-09-09.
///
/// Mirrors `secantus.ordering._sort_path_values`.
pub fn sort_path_values<'a>(doc: &'a Document, path: &str) -> Vec<(&'a Bson, bool)> {
    let mut current: Vec<(&Bson, bool)> = Vec::new();
    let mut first = true;
    for part in path.split('.') {
        let mut next: Vec<(&Bson, bool)> = Vec::new();
        if first {
            if let Some(v) = doc.get(part) {
                next.push((v, false));
            }
            first = false;
        } else {
            for (cur, _) in &current {
                match cur {
                    Bson::Document(d) => {
                        if let Some(v) = d.get(part) {
                            next.push((v, false));
                        }
                    }
                    Bson::Array(arr) => {
                        if is_digits(part) {
                            if let Some(v) = part.parse::<usize>().ok().and_then(|i| arr.get(i)) {
                                next.push((v, true));
                            }
                        }
                        for elem in arr {
                            if let Bson::Document(d) = elem {
                                if let Some(v) = d.get(part) {
                                    next.push((v, false));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        current = next;
    }
    current
}

/// The `(component, array)` mongod refuses to sort by, or `None`.
///
/// A component is ambiguous when it is a VALID INDEX of the array it is applied
/// to *and* some element of that array is a document carrying that exact key.
/// Both halves are load-bearing -- `x.1` over `[{"1": 5}]` is fine (index 1 is
/// past the end, so only the field reading exists) and `x.0` over `[{"00": 5}]`
/// is fine (`"00"` is not the key `"0"`). The element carrying the key need not
/// be the one at that index: `[{a: 5}, {"0": 6}]` sorted by `x.0` is refused.
/// Measured over 19 shapes on 8.2.11, 2026-09-09.
///
/// mongod refuses only for a SORT -- the same path in a `find` filter, a
/// `$group` `_id` or a projection resolves to both readings happily.
/// Mirrors `secantus.ordering._check_sort_path_ambiguity`.
pub fn ambiguous_sort_path<'a>(
    doc: &'a Document,
    path: &'a str,
) -> Option<(&'a str, &'a Vec<Bson>)> {
    let parts: Vec<&str> = path.split('.').collect();
    ambiguity_walk(doc.get(parts[0])?, &parts[1..])
}

fn ambiguity_walk<'a>(current: &'a Bson, rest: &[&'a str]) -> Option<(&'a str, &'a Vec<Bson>)> {
    let (part, tail) = rest.split_first()?;
    match current {
        Bson::Document(d) => d.get(*part).and_then(|v| ambiguity_walk(v, tail)),
        Bson::Array(items) => {
            let index = index_component(part).filter(|i| *i < items.len());
            // The FIELD reading walks every element, which is also how a
            // non-numeric component descends through an array.
            let named: Vec<&Bson> = items
                .iter()
                .filter_map(|e| match e {
                    Bson::Document(d) => d.get(*part),
                    _ => None,
                })
                .collect();
            if index.is_some() && !named.is_empty() {
                return Some((part, items));
            }
            if let Some(i) = index {
                if let Some(hit) = ambiguity_walk(&items[i], tail) {
                    return Some(hit);
                }
            }
            named.into_iter().find_map(|v| ambiguity_walk(v, tail))
        }
        _ => None,
    }
}

/// mongod's message for the refusal [`ambiguous_sort_path`] detects.
pub fn ambiguous_sort_message(part: &str, items: &[Bson]) -> String {
    let rendered: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(i, v)| format!("{i}: {}", crate::query::bson_value_repr(v)))
        .collect();
    format!(
        "Ambiguous field name found in array (do not use numeric field names in \
         embedded elements in an array), field: '{part}' for array: {{ {} }}",
        rendered.join(", ")
    )
}

/// Every value reachable at `path`, descending into arrays, plus whether any
/// component was resolved by walking array elements.
///
/// MongoDB's index-key generation walks a dotted path *through* an array:
/// `prices.owner` over `{"prices": [{"owner": 1}, {"owner": 2}]}` yields
/// `[1, 2]`, and the covering index is multikey. `get_path` deliberately
/// doesn't do this (it reads a numeric component as a positional index), which
/// is right for `$set` / projection and wrong for index keys.
/// Mirrors `secantus.paths.get_path_values`.
pub fn get_path_values<'a>(doc: &'a Document, path: &str) -> (Vec<&'a Bson>, bool) {
    let mut current: Vec<&Bson> = Vec::new();
    let mut descended = false;
    let mut first = true;
    for part in path.split('.') {
        let mut next: Vec<&Bson> = Vec::new();
        if first {
            if let Some(v) = doc.get(part) {
                next.push(v);
            }
            first = false;
        } else {
            for cur in &current {
                match cur {
                    Bson::Document(d) => {
                        if let Some(v) = d.get(part) {
                            next.push(v);
                        }
                    }
                    Bson::Array(arr) => {
                        descended = true;
                        if is_digits(part) {
                            if let Some(v) = part.parse::<usize>().ok().and_then(|i| arr.get(i)) {
                                next.push(v);
                            }
                        }
                        // mongod matches the component against each element
                        // too, so `a.0` finds both the positional element and
                        // any element carrying a literal "0" key.
                        for elem in arr {
                            if let Bson::Document(d) = elem {
                                if let Some(v) = d.get(part) {
                                    next.push(v);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        current = next;
    }
    (current, descended)
}

/// What stops `path` from being created against `doc`, or `None` if it can be:
/// `(container_key, container_value, field)`, where `field` is the component
/// that cannot be created. mongod refuses these with `PathNotViable` (28).
/// Mirrors `secantus.paths.path_block`.
///
/// Creatable, so `None`: a missing intermediate (mongod makes the
/// sub-document), an out-of-range array index (mongod pads with nulls), and the
/// leaf itself whatever its current type.
pub fn path_block<'a>(doc: &'a Document, path: &str) -> Option<(Option<String>, &'a Bson, String)> {
    let parts: Vec<&str> = path.split('.').collect();
    // The first component is resolved against the document; from then on we
    // walk Bson values. A missing first component, or a single-component path
    // (the leaf itself), is creatable either way.
    let (mut last_key, mut cur): (Option<String>, &Bson) = match doc.get(parts[0]) {
        Some(v) if parts.len() > 1 => (Some(parts[0].to_string()), v),
        _ => return None,
    };
    for (i, part) in parts.iter().enumerate().skip(1) {
        let is_leaf = i == parts.len() - 1;
        match cur {
            Bson::Document(d) => {
                if !d.contains_key(*part) || is_leaf {
                    return None;
                }
                last_key = Some((*part).to_string());
                cur = d.get(*part).unwrap();
            }
            Bson::Array(a) => {
                if !is_digits(part) {
                    return Some((last_key, cur, (*part).to_string()));
                }
                let idx: usize = match part.parse() {
                    Ok(v) => v,
                    Err(_) => return Some((last_key, cur, (*part).to_string())),
                };
                if idx >= a.len() || is_leaf {
                    return None;
                }
                last_key = Some((*part).to_string());
                cur = &a[idx];
            }
            _ => return Some((last_key, cur, (*part).to_string())),
        }
    }
    None
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
        // A non-container intermediate is a silent no-op HERE because this is
        // the shared setter, used by projection / aggregation as well. On the
        // UPDATE path mongod refuses it (`PathNotViable`, 28) -- see
        // `path_block`, which `update::set_path` consults first.
        _ => Ok(()),
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

#[cfg(test)]
mod tests {
    use bson::doc;

    /// mongod refuses to create a field under a non-document (`PathNotViable`,
    /// 28); `set_path` returned silently instead, so the update reported
    /// success and wrote nothing. Cases probed against mongod 6.0.16.
    #[test]
    fn path_block_names_what_is_in_the_way() {
        let scalar = doc! {"n": 5};
        let (key, _, field) = super::path_block(&scalar, "n.x").unwrap();
        assert_eq!((key.as_deref(), field.as_str()), (Some("n"), "x"));

        let nested = doc! {"a": {"b": 7}};
        let (key, _, field) = super::path_block(&nested, "a.b.c").unwrap();
        assert_eq!((key.as_deref(), field.as_str()), (Some("b"), "c"));

        // An array addressed by a non-numeric component.
        let arr = doc! {"a": [1]};
        let (key, _, field) = super::path_block(&arr, "a.x").unwrap();
        assert_eq!((key.as_deref(), field.as_str()), (Some("a"), "x"));

        // Descending into a scalar array ELEMENT.
        let (key, _, field) = super::path_block(&arr, "a.0.x").unwrap();
        assert_eq!((key.as_deref(), field.as_str()), (Some("0"), "x"));
    }

    #[test]
    fn creatable_paths_are_not_blocked() {
        // A missing intermediate: mongod makes the sub-document.
        assert!(super::path_block(&doc! {}, "n.x").is_none());
        assert!(super::path_block(&doc! {"a": {}}, "a.b.c").is_none());
        // An out-of-range index: mongod pads with nulls.
        assert!(super::path_block(&doc! {"a": [1]}, "a.4").is_none());
        // The leaf itself is overwritten, whatever its current type.
        assert!(super::path_block(&doc! {"n": 5}, "n").is_none());
        assert!(super::path_block(&doc! {"a": {"b": 7}}, "a.b").is_none());
    }
}
