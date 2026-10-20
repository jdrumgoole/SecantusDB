//! The match expression as mongod parses it, where that goes beyond
//! `secantus_core::canonical_match`.

use super::bounds::{cmp, sort_key};
use bson::{doc, Bson, Document};

/// Where mongod sorts an operator among its siblings (the comparison part
/// of `secantus_core`'s match-type ranks).
fn rank(op: &str) -> i32 {
    match op {
        "$and" => 0,
        "$or" => 1,
        "$nor" => 2,
        "$elemMatch" => 3,
        "$size" => 5,
        "$eq" => 6,
        "$lte" => 7,
        "$lt" => 8,
        "$gt" => 9,
        "$gte" => 10,
        "$regex" => 11,
        "$mod" => 12,
        "$exists" => 13,
        "$in" => 14,
        "$not" => 19,
        "$type" => 20,
        _ => 99,
    }
}

/// A branch's place among its siblings: its operator's rank, then its path.
fn branch_rank(branch: &Bson) -> (i32, String) {
    let Some((key, value)) = branch.as_document().and_then(|d| d.iter().next()) else {
        return (99, String::new());
    };
    if key.starts_with('$') {
        return (rank(key), String::new());
    }
    let op = value
        .as_document()
        .and_then(|d| d.keys().next())
        .map_or(99, |op| rank(op));
    (op, key.clone())
}

/// One operator document, rewritten as mongod prints it. `sorted_in` is
/// whether an `$in` lists its values sorted and once each, as `parsedQuery`
/// does; mongod's query-tree text (`debug_string`) keeps them as written.
fn operator(d: Document, sorted_in: bool) -> Document {
    // `{$exists: false}` is `{$not: {$exists: true}}`.
    if d.len() == 1 && d.get("$exists").is_some_and(|v| !crate::util::truthy(v)) {
        return doc! { "$not": { "$exists": true } };
    }
    // `{$in: [x]}` is `{$eq: x}`, under a `$not` as at the top.
    if let (1, Some(Bson::Array(items))) = (d.len(), d.get("$in")) {
        if let [one] = items.as_slice() {
            if !matches!(one, Bson::RegularExpression(_)) {
                return doc! { "$eq": one.clone() };
            }
        }
    }
    let mut out = Document::new();
    for (k, v) in d {
        let v = match (k.as_str(), v) {
            // `$not` of a regex is the regex's own clause.
            ("$not", Bson::RegularExpression(re)) => {
                let mut clause = doc! { "$regex": re.pattern.clone() };
                if !re.options.is_empty() {
                    clause.insert("$options", re.options.clone());
                }
                Bson::Document(clause)
            }
            // A BinData bit mask is the positions of its set bits, byte by
            // byte from the first, as a number's is.
            (
                "$bitsAllSet" | "$bitsAllClear" | "$bitsAnySet" | "$bitsAnyClear",
                Bson::Binary(mask),
            ) => Bson::Array(
                mask.bytes
                    .iter()
                    .enumerate()
                    .flat_map(|(i, byte)| {
                        (0..8)
                            .filter(move |bit| byte >> bit & 1 == 1)
                            .map(move |bit| Bson::Int32((i * 8 + bit) as i32))
                    })
                    .collect(),
            ),
            // `$type`'s codes are whole numbers, however they were written.
            ("$type", Bson::Array(items)) => Bson::Array(
                items
                    .into_iter()
                    .map(|c| match c {
                        Bson::Double(d) if d.fract() == 0.0 => Bson::Int32(d as i32),
                        Bson::Int64(n) => i32::try_from(n).map_or(Bson::Int64(n), Bson::Int32),
                        other => other,
                    })
                    .collect(),
            ),
            ("$in", Bson::Array(items)) if !sorted_in => Bson::Array(items),
            // `$in` lists its values once each, in sort order.
            ("$in", Bson::Array(mut items)) => {
                items.sort_by(cmp);
                items.dedup_by(|a, b| sort_key(a) == sort_key(b));
                Bson::Array(items)
            }
            // `$mod`'s divisor and remainder are 64-bit.
            ("$mod", Bson::Array(items)) => Bson::Array(
                items
                    .into_iter()
                    .map(|n| crate::util::as_i64(&n).map_or(n, Bson::Int64))
                    .collect(),
            ),
            // The value form of `$elemMatch` sorts its operators.
            ("$elemMatch", Bson::Document(inner)) if inner.keys().all(|x| x.starts_with('$')) => {
                let mut pairs: Vec<(String, Bson)> =
                    operator(inner, sorted_in).into_iter().collect();
                pairs.sort_by_key(|(op, _)| rank(op));
                Bson::Document(pairs.into_iter().collect())
            }
            // The branches of `$or` and `$nor` sort like `$and`'s children.
            ("$or" | "$nor", Bson::Array(items)) => {
                let mut branches: Vec<Bson> =
                    items.into_iter().map(|b| tidy(b, sorted_in)).collect();
                branches.sort_by_key(branch_rank);
                Bson::Array(branches)
            }
            (_, other) => tidy(other, sorted_in),
        };
        out.insert(k, v);
    }
    out
}

fn tidy(value: Bson, sorted_in: bool) -> Bson {
    match value {
        Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => {
            Bson::Document(operator(d, sorted_in))
        }
        Bson::Document(d) => Bson::Document(
            d.into_iter()
                .map(|(k, v)| (k, tidy(v, sorted_in)))
                .collect(),
        ),
        Bson::Array(a) => Bson::Array(a.into_iter().map(|v| tidy(v, sorted_in)).collect()),
        other => other,
    }
}

/// `$alwaysTrue` matches everything: mongod drops it from an `$and`, and an
/// `$and` left with one child is that child.
fn drop_always_true(filter: Document) -> Document {
    let Some(Bson::Array(children)) = filter.get("$and").filter(|_| filter.len() == 1) else {
        return if filter.len() == 1 && filter.contains_key("$alwaysTrue") {
            Document::new()
        } else {
            filter
        };
    };
    let kept: Vec<Bson> = children
        .iter()
        .filter(|c| {
            !c.as_document()
                .is_some_and(|d| d.len() == 1 && d.contains_key("$alwaysTrue"))
        })
        .cloned()
        .collect();
    match kept.len() {
        0 => Document::new(),
        1 => kept[0].as_document().cloned().unwrap_or_default(),
        n if n == children.len() => filter,
        _ => doc! { "$and": kept },
    }
}

/// The match expression as mongod parses and normalises it.
pub(crate) fn parsed_query(filter: &Document) -> Document {
    parsed(filter, true)
}

/// The match expression as mongod's query-tree text prints it: as
/// `parsedQuery`, but with each `$in`'s values as they were written.
pub(crate) fn parsed_query_as_written(filter: &Document) -> Document {
    parsed(filter, false)
}

fn parsed(filter: &Document, sorted_in: bool) -> Document {
    let canonical = drop_always_true(secantus_core::canonical_match(&Bson::Document(
        filter.clone(),
    )));
    // `{a: {$nin: []}}` excludes nothing, and mongod says so its own way.
    if let [(_, Bson::Document(pred))] = canonical.iter().collect::<Vec<_>>()[..] {
        let empty_nin = pred
            .get_document("$not")
            .ok()
            .and_then(|n| n.get_array("$in").ok())
            .is_some_and(Vec::is_empty);
        if pred.len() == 1 && empty_nin {
            return doc! { "$nor": [{ "$alwaysFalse": 1 }] };
        }
    }
    match tidy(Bson::Document(canonical), sorted_in) {
        Bson::Document(d) => optimize(d, sorted_in),
        _ => Document::new(),
    }
}

/// mongod's optimisation of the parsed tree (8.2.11, measured 2026-10-20):
/// an `$and` or `$or` takes in the children of a nested one of its kind and
/// keeps one of each repeated child (`1` and `1.0` are one); the equalities
/// of an `$or` on one path become that path's `$in` (a regex value joins it
/// as a pattern); and a node left with one child is that child, a lone
/// `$in` of one value being its equality.
fn optimize(node: Document, sorted_in: bool) -> Document {
    let Some((key, Bson::Array(children))) = node.iter().next().filter(|_| node.len() == 1) else {
        return node;
    };
    let kind = key.clone();
    if kind != "$and" && kind != "$or" {
        return node;
    }
    let mut flat: Vec<Document> = Vec::new();
    for child in children.iter().filter_map(Bson::as_document) {
        let child = optimize(child.clone(), sorted_in);
        match child.get(&kind) {
            Some(Bson::Array(inner)) if child.len() == 1 => {
                flat.extend(inner.iter().filter_map(Bson::as_document).cloned());
            }
            _ => flat.push(child),
        }
    }
    if kind == "$or" {
        flat = group_equalities(flat, sorted_in);
    }
    let mut kept: Vec<Document> = Vec::new();
    for child in flat {
        if !kept.iter().any(|k| same_clause(k, &child)) {
            kept.push(child);
        }
    }
    if kept.len() == 1 {
        let only = kept.pop().expect("one");
        return if kind == "$or" { single_in(only) } else { only };
    }
    kept.sort_by_key(secantus_core::clause_rank);
    doc! { kind: kept.into_iter().map(Bson::Document).collect::<Vec<_>>() }
}

/// The value an `$or` branch tests its path for equality with, if it is
/// one: `{p: {$eq: v}}`, or `{p: {$regex: ...}}` as a regex value.
fn equality(branch: &Document) -> Option<(String, Bson)> {
    let [(path, Bson::Document(pred))] = branch.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    if path.starts_with('$') {
        return None;
    }
    if let (1, Some(v)) = (pred.len(), pred.get("$eq")) {
        return (!matches!(v, Bson::RegularExpression(_))).then(|| (path.clone(), v.clone()));
    }
    let pattern = pred.get_str("$regex").ok()?;
    let options = pred.get_str("$options").unwrap_or_default();
    (pred.len() == 1 + usize::from(pred.contains_key("$options"))).then(|| {
        (
            path.clone(),
            Bson::RegularExpression(bson::Regex {
                pattern: pattern.to_string(),
                options: options.to_string(),
            }),
        )
    })
}

/// The `$or` branches with each path's equalities, where it has two or
/// more, folded into one `$in` in the place of the first.
fn group_equalities(branches: Vec<Document>, sorted_in: bool) -> Vec<Document> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for b in &branches {
        if let Some((path, _)) = equality(b) {
            *counts.entry(path).or_default() += 1;
        }
    }
    let mut out: Vec<Document> = Vec::new();
    let mut slot: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for b in branches {
        let Some((path, value)) = equality(&b).filter(|(p, _)| counts[p] > 1) else {
            out.push(b);
            continue;
        };
        match slot.get(&path) {
            Some(&i) => {
                if let Ok(items) = out[i]
                    .get_document_mut(&path)
                    .and_then(|p| p.get_array_mut("$in"))
                {
                    items.push(value);
                }
            }
            None => {
                slot.insert(path.clone(), out.len());
                out.push(doc! { path: { "$in": [value] } });
            }
        }
    }
    if sorted_in {
        for (path, &i) in &slot {
            if let Ok(items) = out[i]
                .get_document_mut(path)
                .and_then(|p| p.get_array_mut("$in"))
            {
                items.sort_by(cmp);
                items.dedup_by(|a, b| sort_key(a) == sort_key(b));
            }
        }
    }
    out
}

/// A clause standing alone that is an `$in` of one value: that equality.
fn single_in(clause: Document) -> Document {
    let [(path, Bson::Document(pred))] = clause.iter().collect::<Vec<_>>()[..] else {
        return clause;
    };
    match pred.get_array("$in").map(Vec::as_slice) {
        Ok([one]) if pred.len() == 1 => match one {
            Bson::RegularExpression(re) => {
                let mut r = doc! { "$regex": re.pattern.clone() };
                if !re.options.is_empty() {
                    r.insert("$options", re.options.clone());
                }
                doc! { path.clone(): r }
            }
            v => doc! { path.clone(): { "$eq": v.clone() } },
        },
        _ => clause,
    }
}

/// Whether two clauses are one predicate: the same shape, values equal as
/// mongod compares them.
fn same_clause(a: &Document, b: &Document) -> bool {
    fn same(a: &Bson, b: &Bson) -> bool {
        match (a, b) {
            (Bson::Document(x), Bson::Document(y)) => {
                x.len() == y.len()
                    && x.iter()
                        .zip(y.iter())
                        .all(|((kx, vx), (ky, vy))| kx == ky && same(vx, vy))
            }
            (Bson::Array(x), Bson::Array(y)) => {
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| same(p, q))
            }
            _ => sort_key(a) == sort_key(b),
        }
    }
    same(&Bson::Document(a.clone()), &Bson::Document(b.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_follow_mongod() {
        assert_eq!(
            parsed_query(&doc! {"a": {"$in": [3, 1, 3]}}),
            doc! {"a": {"$in": [1, 3]}}
        );
        assert_eq!(
            parsed_query(&doc! {"a": {"$exists": false}}),
            doc! {"a": {"$not": {"$exists": true}}}
        );
        assert_eq!(
            parsed_query(&doc! {"$or": [{"a": 1}, {"a": 3}]}),
            doc! {"a": {"$in": [1, 3]}}
        );
        assert_eq!(
            parsed_query(&doc! {"a": {"$nin": []}}),
            doc! {"$nor": [{"$alwaysFalse": 1}]}
        );
        assert_eq!(
            parsed_query(&doc! {"t": {"$elemMatch": {"$gt": 3, "$lt": 9}}}),
            doc! {"t": {"$elemMatch": {"$lt": 9, "$gt": 3}}}
        );
    }
}
