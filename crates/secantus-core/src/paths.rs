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
                        // An element that does not have the field sorts as
                        // null, as it is indexed: one without the key, and
                        // one that is not a document at all. `p.q` over
                        // `[{q: 3}, {r: 1}]` and over `[5, {q: 3}]` is null
                        // ascending. A numeric component is an index first,
                        // and adds no null for the elements it passes over.
                        let named = !is_digits(part);
                        for elem in arr {
                            match elem {
                                Bson::Document(d) => match d.get(part) {
                                    Some(v) => next.push((v, false)),
                                    None if named => next.push((&MISSING_IN_ELEMENT, false)),
                                    None => {}
                                },
                                _ if named => next.push((&MISSING_IN_ELEMENT, false)),
                                _ => {}
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

/// What an array element that lacks the next path component stands for.
static MISSING_IN_ELEMENT: Bson = Bson::Null;

/// Why mongod refuses to build a sort key for one document.
#[derive(Debug, PartialEq)]
pub enum SortKeyRefusal<'a> {
    /// Two sort paths reach two different arrays of the document: 2
    /// "cannot sort with keys that are parallel arrays".
    ParallelArrays,
    /// A path component is a valid INDEX of the array it is applied to while
    /// an element of that array is a document carrying that exact key: 16746
    /// (see [`ambiguous_sort_message`]). The component and the array.
    Ambiguous(&'a str, &'a Vec<Bson>),
}

/// mongod's message for [`SortKeyRefusal::ParallelArrays`].
pub const PARALLEL_ARRAYS_MESSAGE: &str = "cannot sort with keys that are parallel arrays";

/// What mongod refuses to sort `doc` by under the sort paths `paths`, or
/// `None`.
///
/// mongod builds a sort key with its B-tree key generator
/// (`BtreeKeyGenerator::_getKeysWithArray`), so it refuses what that refuses,
/// and this is a port of its walk. At each level every path still in play is
/// followed through documents until it ends or meets an array; all the paths
/// that meet one must meet the SAME array (empty or not), or it is the
/// parallel-arrays refusal. That array's elements are then walked in turn
/// with what is left of those paths. A component after an array that is a
/// valid index of it also resolves POSITIONALLY, for every element, so `a.0.c`
/// and `a.c` over `[{c: [1]}, {c: [2]}]` meet two arrays at the second
/// element; an element that is a document carrying that component as a key
/// as well is the ambiguity refusal. An element that is itself an array is
/// walked as nothing but the positional reading. The two refusals are raised
/// in the walk's order -- level by level, path by path -- so `{a.0, a.x,
/// a.y}` over `[{"0": 1, x: [1], y: [2]}]` is 16746 and `{a.x, a.y, a.0}` is
/// 2. Measured over 40 shapes against 8.2.1, 2026-11-08 (RM-332); the
/// ambiguity half over 19 against 8.2.11, 2026-09-09.
///
/// mongod refuses only for a SORT -- the same paths in a `find` filter, a
/// `$group` `_id`, a projection or a `$sortArray` resolve happily.
pub fn sort_key_refusal<'a>(doc: &'a Document, paths: &[&'a str]) -> Option<SortKeyRefusal<'a>> {
    let fields: Vec<Option<KeyPath<'a>>> = paths
        .iter()
        .map(|p| {
            Some(KeyPath {
                path: p,
                positional: None,
            })
        })
        .collect();
    refusal_walk(Container::Doc(doc), &fields)
}

/// How many of `paths` meet an array in `doc`: the first array along the
/// path, or the value it ends at being one (empty or not).
///
/// The `$top` / `$bottom` accumulators decide by this count whether to build
/// their key as a `$sort` does (see [`sort_key_refusal`]): with fewer than
/// two, never; with two or more, SBE walks the key generator and the classic
/// engine refuses outright, whether or not the arrays are one array.
pub fn sort_paths_meeting_arrays(doc: &Document, paths: &[&str]) -> usize {
    paths
        .iter()
        .filter(|p| matches!(along_path(Container::Doc(doc), p).0, Some(Bson::Array(_))))
        .count()
}

/// What one sort path still has to resolve at a level of [`refusal_walk`].
#[derive(Clone, Copy)]
struct KeyPath<'a> {
    path: &'a str,
    positional: Option<Positional<'a>>,
}

/// The positional reading of a path's next component over the array the
/// level walks (mongod's `PositionalPathInfo`): where it leads, and what is
/// left after it.
#[derive(Clone, Copy)]
struct Positional<'a> {
    array: &'a Vec<Bson>,
    dotted: Option<&'a Bson>,
    rest: &'a str,
}

/// What a level reads its components from. An array is read only through a
/// positional reading (its keys are its canonical indexes).
#[derive(Clone, Copy)]
enum Container<'a> {
    Doc(&'a Document),
    Arr(&'a Vec<Bson>),
    Nothing,
}

impl<'a> Container<'a> {
    fn get(self, name: &str) -> Option<&'a Bson> {
        match self {
            Container::Doc(d) => d.get(name),
            Container::Arr(a) => index_component(name).and_then(|i| a.get(i)),
            Container::Nothing => None,
        }
    }
}

fn split_component(path: &str) -> (&str, &str) {
    path.split_once('.').unwrap_or((path, ""))
}

/// mongod's `extractElementAtPathOrArrayAlongPath`: follow `path` through
/// documents, stopping at the first array; the value and what is left of the
/// path.
fn along_path<'a>(c: Container<'a>, path: &'a str) -> (Option<&'a Bson>, &'a str) {
    let (head, rest) = split_component(path);
    match c.get(head) {
        None => (None, rest),
        Some(v @ Bson::Array(_)) => (Some(v), rest),
        Some(v) if rest.is_empty() => (Some(v), rest),
        Some(Bson::Document(d)) => along_path(Container::Doc(d), rest),
        Some(_) => (None, rest),
    }
}

fn refusal_walk<'a>(
    obj: Container<'a>,
    fields: &[Option<KeyPath<'a>>],
) -> Option<SortKeyRefusal<'a>> {
    let mut array: Option<&'a Vec<Bson>> = None;
    let mut through: Vec<(usize, &'a str)> = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let Some(field) = field else { continue };
        let (head, _) = split_component(field.path);
        let (found, rest) = if obj.get(head).is_some() {
            if let Some(p) = field.positional {
                return Some(SortKeyRefusal::Ambiguous(head, p.array));
            }
            along_path(obj, field.path)
        } else if let Some(p) = field.positional {
            (p.dotted, p.rest)
        } else {
            (None, "")
        };
        if let Some(Bson::Array(a)) = found {
            match array {
                None => array = Some(a),
                Some(seen) if !std::ptr::eq(seen, a) => {
                    return Some(SortKeyRefusal::ParallelArrays);
                }
                Some(_) => {}
            }
            through.push((i, rest));
        }
    }
    let array = array?;
    let mut next: Vec<Option<KeyPath<'a>>> = vec![None; fields.len()];
    let mut live = false;
    for (i, rest) in through {
        if rest.is_empty() {
            continue;
        }
        let (part, tail) = split_component(rest);
        let positional = index_component(part).and_then(|n| array.get(n)).map(|elt| {
            let (dotted, rest) = if tail.is_empty() {
                (Some(elt), "")
            } else {
                match elt {
                    Bson::Document(d) => along_path(Container::Doc(d), tail),
                    Bson::Array(a) => along_path(Container::Arr(a), tail),
                    _ => (None, ""),
                }
            };
            Positional {
                array,
                dotted,
                rest,
            }
        });
        next[i] = Some(KeyPath {
            path: rest,
            positional,
        });
        live = true;
    }
    if !live {
        return None;
    }
    array.iter().find_map(|elem| {
        let obj = match elem {
            Bson::Document(d) => Container::Doc(d),
            _ => Container::Nothing,
        };
        refusal_walk(obj, &next)
    })
}

/// mongod's message for [`SortKeyRefusal::Ambiguous`].
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
pub(crate) const MAX_LIST_GROW_INDEX: usize = 100_000;

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

    /// What mongod 8.2.1 answers for a sort over each document, measured
    /// 2026-11-08 (RM-332): `P` is 2 "parallel arrays", `A` is 16746, `-`
    /// is a sort.
    #[test]
    fn sort_key_refusal_matches_mongod() {
        use super::SortKeyRefusal::{Ambiguous, ParallelArrays};
        let cases: Vec<(bson::Document, Vec<&str>, char)> = vec![
            (doc! {"a": [1, 2], "b": [3, 4]}, vec!["a", "b"], 'P'),
            (doc! {"a": [1, 2], "b": 3}, vec!["a", "b"], '-'),
            (doc! {"a": [], "b": [3, 4]}, vec!["a", "b"], 'P'),
            (doc! {"a": [], "b": []}, vec!["a", "b"], 'P'),
            (
                doc! {"a": [{"x": 1, "y": 2}, {"x": 3, "y": 0}]},
                vec!["a.x", "a.y"],
                '-',
            ),
            (doc! {"a": {"b": [1], "c": [2]}}, vec!["a.b", "a.c"], 'P'),
            (
                doc! {"a": [{"b": [1, 2], "c": [3, 4]}]},
                vec!["a.b", "a.c"],
                'P',
            ),
            (doc! {"a": [{"b": [1, 2], "c": 3}]}, vec!["a.b", "a.c"], '-'),
            (doc! {"a": [{"b": 1}], "c": [1]}, vec!["a.b", "c"], 'P'),
            (doc! {"a": [[1, 2], [3]], "b": [1]}, vec!["a", "b"], 'P'),
            (doc! {"a": [1, 2], "b": [3, 4]}, vec!["a.0", "b"], 'P'),
            (
                doc! {"a": [{"b": {"c": [1, 2], "d": [3]}}]},
                vec!["a.b.c", "a.b.d"],
                'P',
            ),
            (doc! {"a": [1], "b": 2, "c": [3]}, vec!["a", "b", "c"], 'P'),
            (doc! {"a": [{"b": 1}]}, vec!["a", "a.b"], '-'),
            (doc! {"a": {"b": [1]}}, vec!["a", "a.b"], '-'),
            (
                doc! {"a": [{"b": [1]}, {"c": [2]}]},
                vec!["a.b", "a.c"],
                '-',
            ),
            (doc! {"b": [1]}, vec!["a", "b"], '-'),
            (doc! {"a": null, "b": [1]}, vec!["a", "b"], '-'),
            (doc! {"a": [1, 2], "b": [3, 4]}, vec!["a"], '-'),
            (doc! {"a": [{"x": [1], "y": 2}]}, vec!["a.x", "a.y"], '-'),
            (
                doc! {"a": [{"x": 1}, [{"x": 2}]], "b": 1},
                vec!["a.x", "b"],
                '-',
            ),
            (doc! {"a": [[1, 2], [3, 4]]}, vec!["a.0", "a.1"], 'P'),
            (doc! {"a": [[1, 2]]}, vec!["a.0", "a.1"], '-'),
            (
                doc! {"a": [{"c": [1], "d": [2]}]},
                vec!["a.0.c", "a.0.d"],
                'P',
            ),
            (
                doc! {"a": [{"c": [1], "d": [2]}]},
                vec!["a.0.c", "a.c"],
                '-',
            ),
            (
                doc! {"a": [{"c": [1]}, {"c": [2]}]},
                vec!["a.0.c", "a.c"],
                'P',
            ),
            (
                doc! {"a": [{"c": [1]}, {"c": [2]}]},
                vec!["a.1.c", "a.c"],
                'P',
            ),
            (doc! {"a": [{"0": 1}, 2], "b": [1]}, vec!["a.0", "b"], 'P'),
            (
                doc! {"a": [{"0": 1, "x": [1], "y": [2]}]},
                vec!["a.0", "a.x", "a.y"],
                'A',
            ),
            (
                doc! {"a": [{"0": 1, "x": [1], "y": [2]}]},
                vec!["a.x", "a.y", "a.0"],
                'P',
            ),
            (doc! {"a": [5, 6], "b": 1}, vec!["a.0", "a.1"], '-'),
            (doc! {"a": [[{"x": [1]}]], "b": [2]}, vec!["a.x"], '-'),
            (doc! {"a": [[[1], [2]]]}, vec!["a.0.0", "a.0.1"], 'P'),
            (doc! {"a": [[1, 2], [3]]}, vec!["a.0", "a"], '-'),
            (
                doc! {"a": [{"x": [1]}, {"x": [2]}]},
                vec!["a.0.x", "a.1.x"],
                'P',
            ),
            // The ambiguity half (8.2.11, 2026-09-09).
            (doc! {"x": [{"a": 5}, {"0": 6}]}, vec!["x.0"], 'A'),
            (doc! {"x": [{"1": 5}]}, vec!["x.1"], '-'),
            (doc! {"x": [{"00": 5}]}, vec!["x.0"], '-'),
            (doc! {"x": [[5]]}, vec!["x.0"], '-'),
        ];
        for (doc, paths, want) in cases {
            let got = match super::sort_key_refusal(&doc, &paths) {
                None => '-',
                Some(ParallelArrays) => 'P',
                Some(Ambiguous(..)) => 'A',
            };
            assert_eq!(got, want, "{doc} sorted by {paths:?}");
        }
    }

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
