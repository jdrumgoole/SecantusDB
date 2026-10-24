//! mongod's `MatchDetails::elemMatchKey`: the array position a positional
//! projection (`{"arr.$": 1}`) reads.
//!
//! mongod matches the query against the document with details requested, and
//! a leaf that matched through an array ELEMENT -- not the array as a whole --
//! records that element's position in the first array on its path. Of the
//! leaves of a conjunction, the last to record one wins, in the order the
//! optimizer has sorted them: by match type, then by path
//! (`matchExpressionComparator`). `$or` (of more than one clause), `$nor` and
//! every negation record nothing. So the position can come from an array other
//! than the one projected: `find({"o.k": 2}, {"arr.$": 1})` returns the `arr`
//! element at `o`'s matching position (all measured 8.2.11, 2026-10-24).
//!
//! The position a leaf records is the first element at which it matches; here
//! that is found by testing the leaf with the array replaced by each element in
//! turn, which is the same element-by-element walk the leaf makes.

use bson::{Bson, Document};

use crate::fallback::Fallback;

/// A leaf of the optimized conjunction: its rank in mongod's `MatchType`
/// order, its path, the one-clause filter that tests it, and whether it can
/// record a position at all.
struct Leaf {
    rank: u8,
    path: String,
    filter: Document,
    records: bool,
    elem_match: bool,
}

// `MatchType`'s order (matcher/expression.h).
const ELEM_MATCH_OBJECT: u8 = 2;
const ELEM_MATCH_VALUE: u8 = 3;
const SIZE: u8 = 4;
const EQ: u8 = 5;
const LTE: u8 = 6;
const LT: u8 = 7;
const GT: u8 = 8;
const GTE: u8 = 9;
const REGEX: u8 = 10;
const MOD: u8 = 11;
const EXISTS: u8 = 12;
const MATCH_IN: u8 = 13;
const BITS_ALL_SET: u8 = 14;
const BITS_ALL_CLEAR: u8 = 15;
const BITS_ANY_SET: u8 = 16;
const BITS_ANY_CLEAR: u8 = 17;
const NOT: u8 = 18;
const NOR: u8 = 19;
const TYPE_OPERATOR: u8 = 20;
const OTHER: u8 = 30;

fn leaf(rank: u8, path: &str, test: Bson) -> Leaf {
    let mut filter = Document::new();
    filter.insert(path.to_string(), test);
    let records = !matches!(rank, SIZE | NOT | NOR | OTHER);
    Leaf {
        rank,
        path: path.to_string(),
        filter,
        records,
        elem_match: matches!(rank, ELEM_MATCH_OBJECT | ELEM_MATCH_VALUE),
    }
}

fn op(name: &str, arg: &Bson) -> Bson {
    let mut d = Document::new();
    d.insert(name.to_string(), arg.clone());
    Bson::Document(d)
}

/// The leaf an equality to `value` is: a regular expression matches, anything
/// else compares.
fn equality(path: &str, value: &Bson) -> Leaf {
    match value {
        Bson::RegularExpression(_) => leaf(REGEX, path, value.clone()),
        Bson::Document(d) if d.keys().next().is_some_and(|k| k == "$elemMatch") => {
            elem_match(path, d.get("$elemMatch").unwrap_or(&Bson::Null))
        }
        _ => leaf(EQ, path, op("$eq", value)),
    }
}

fn elem_match(path: &str, arg: &Bson) -> Leaf {
    let value_form = matches!(arg, Bson::Document(d) if d.keys().next().is_some_and(|k| {
        k.starts_with('$')
            && !matches!(k.as_str(), "$and" | "$or" | "$nor" | "$where" | "$expr" | "$text")
    }));
    let rank = if value_form {
        ELEM_MATCH_VALUE
    } else {
        ELEM_MATCH_OBJECT
    };
    leaf(rank, path, op("$elemMatch", arg))
}

fn is_operator_dict(d: &Document) -> bool {
    d.keys()
        .next()
        .is_some_and(|k| k.starts_with('$') && !matches!(k.as_str(), "$ref" | "$id" | "$db"))
}

/// The leaves of `filter`'s conjunction, flattened as the optimizer flattens
/// it: a nested `$and` and a one-clause `$or` join their parent.
fn leaves(filter: &Document, out: &mut Vec<Leaf>) {
    for (key, value) in filter {
        match key.as_str() {
            "$and" => {
                for sub in value.as_array().into_iter().flatten() {
                    if let Bson::Document(d) = sub {
                        leaves(d, out);
                    }
                }
            }
            "$or" => match value.as_array().map(Vec::as_slice) {
                Some([Bson::Document(only)]) => leaves(only, out),
                _ => out.push(leaf(OTHER, "", Bson::Null)),
            },
            "$nor" => out.push(leaf(NOR, "", Bson::Null)),
            "$comment" => {}
            k if k.starts_with('$') => out.push(leaf(OTHER, "", Bson::Null)),
            path => field_leaves(path, value, out),
        }
    }
}

fn field_leaves(path: &str, value: &Bson, out: &mut Vec<Leaf>) {
    let Bson::Document(ops) = value else {
        out.push(equality(path, value));
        return;
    };
    if !is_operator_dict(ops) {
        out.push(equality(path, value));
        return;
    }
    for (name, arg) in ops {
        let rank = match name.as_str() {
            "$eq" => EQ,
            "$lt" => LT,
            "$lte" => LTE,
            "$gt" => GT,
            "$gte" => GTE,
            "$mod" => MOD,
            "$type" => TYPE_OPERATOR,
            "$size" => SIZE,
            "$bitsAllSet" => BITS_ALL_SET,
            "$bitsAllClear" => BITS_ALL_CLEAR,
            "$bitsAnySet" => BITS_ANY_SET,
            "$bitsAnyClear" => BITS_ANY_CLEAR,
            "$ne" | "$nin" | "$not" => NOT,
            "$exists" => {
                if crate::expressions::truthy(arg) {
                    EXISTS
                } else {
                    NOT
                }
            }
            "$regex" => {
                let mut test = Document::new();
                test.insert("$regex", arg.clone());
                if let Some(options) = ops.get("$options") {
                    test.insert("$options", options.clone());
                }
                out.push(leaf(REGEX, path, Bson::Document(test)));
                continue;
            }
            "$options" => continue,
            "$elemMatch" => {
                out.push(elem_match(path, arg));
                continue;
            }
            // A one-element `$in` is optimized to the equality it is.
            "$in" => match arg.as_array().map(Vec::as_slice) {
                Some([only]) => {
                    out.push(equality(path, only));
                    continue;
                }
                _ => MATCH_IN,
            },
            // `$all` is the conjunction of its elements' equalities.
            "$all" => {
                for each in arg.as_array().into_iter().flatten() {
                    out.push(equality(path, each));
                }
                continue;
            }
            _ => OTHER,
        };
        out.push(leaf(rank, path, op(name, arg)));
    }
}

/// The first array on `path` in `doc`, as `(its path, its length)`. None when
/// the path meets a missing field or a scalar first, or names an array element
/// by position, where mongod records nothing.
fn first_array(doc: &Document, path: &str) -> Option<(String, usize)> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = doc;
    for (i, part) in parts.iter().enumerate() {
        match cur.get(*part)? {
            Bson::Array(a) => {
                let next_is_index = parts
                    .get(i + 1)
                    .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
                if next_is_index {
                    return None;
                }
                return Some((parts[..=i].join("."), a.len()));
            }
            Bson::Document(d) => cur = d,
            _ => return None,
        }
    }
    None
}

/// The position `leaf` records in `doc`: the first element of the first array
/// on its path at which it matches.
fn leaf_key(doc: &Document, leaf: &Leaf, vars: &Document) -> Result<Option<usize>, Fallback> {
    if !leaf.records {
        return Ok(None);
    }
    let Some((array_path, len)) = first_array(doc, &leaf.path) else {
        return Ok(None);
    };
    let array = crate::paths::get_path(doc, &array_path)
        .and_then(Bson::as_array)
        .cloned()
        .unwrap_or_default();
    for (i, element) in array.into_iter().enumerate().take(len) {
        // `$elemMatch` on the array itself asks for an array: the one element.
        let stand_in = if leaf.elem_match && array_path == leaf.path {
            Bson::Array(vec![element])
        } else {
            element
        };
        let mut probe = doc.clone();
        if crate::paths::set_path(&mut probe, &array_path, stand_in).is_err() {
            return Ok(None);
        }
        if crate::query::matches(&probe, &leaf.filter, vars, None)? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

/// The element position mongod's match of `filter` against `doc` records, or
/// None if it records none. `doc` is one the filter matched.
pub fn elem_match_key(
    doc: &Document,
    filter: &Document,
    vars: &Document,
) -> Result<Option<usize>, Fallback> {
    let mut all = Vec::new();
    leaves(filter, &mut all);
    // `matchExpressionComparator`: match type, then path. Stable, as the
    // optimizer's sort leaves equal leaves (`$all`'s, say) in order.
    all.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.path.cmp(&b.path)));
    let mut key = None;
    for leaf in &all {
        if let Some(k) = leaf_key(doc, leaf, vars)? {
            key = Some(k);
        }
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn key(filter: Document) -> Option<usize> {
        let d = doc! {"_id": 1, "arr": [10, 20, 30], "o": [{"k": 1}, {"k": 2}, {"k": 3}],
        "m": [[1, 2], [3, 4]]};
        elem_match_key(&d, &filter, &Document::new()).unwrap()
    }

    /// Measured 8.2.11, 2026-10-24.
    #[test]
    fn matches_mongod() {
        assert_eq!(key(doc! {"o.k": 2}), Some(1));
        assert_eq!(key(doc! {"o.k": 3, "arr": 10}), Some(2));
        assert_eq!(key(doc! {"arr": 10, "o.k": 3}), Some(2));
        assert_eq!(key(doc! {"$and": [{"arr": 30}, {"o.k": 1}]}), Some(0));
        assert_eq!(key(doc! {"$or": [{"arr": 30}, {"o.k": 1}]}), None);
        assert_eq!(key(doc! {"arr": {"$elemMatch": {"$gt": 15}}}), Some(1));
        assert_eq!(key(doc! {"o": {"$elemMatch": {"k": 3}}}), Some(2));
        assert_eq!(key(doc! {"arr": {"$in": [30, 20]}}), Some(1));
        assert_eq!(key(doc! {"arr": {"$all": [30, 20]}}), Some(1));
        assert_eq!(key(doc! {"arr": {"$size": 3}}), None);
        assert_eq!(key(doc! {"arr": {"$exists": true}}), Some(0));
        assert_eq!(key(doc! {"arr": {"$ne": 5}}), None);
        assert_eq!(key(doc! {"arr": {"$gt": 15, "$lt": 25}}), Some(1));
        assert_eq!(key(doc! {"arr": {"$gt": 25}, "o.k": {"$gte": 1}}), Some(0));
        assert_eq!(key(doc! {"m": [3, 4]}), Some(1));
        assert_eq!(key(doc! {"arr": [10, 20, 30]}), None);
        assert_eq!(key(doc! {"arr.1": 20}), None);
        assert_eq!(key(doc! {"arr": {"$type": "int"}, "o.k": 3}), Some(0));
    }
}
