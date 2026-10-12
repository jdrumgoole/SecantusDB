//! The match expression as mongod parses it, where that goes beyond
//! `secantus_core::canonical_match`.

use super::bounds::{cmp, sort_key};
use bson::{doc, Bson, Document};

/// mongod's rewrite of a filter that is ONLY an `$or`, before planning: a
/// one-branch `$or` is its branch, and an `$or` of plain equalities on ONE
/// field is that field's `$in`.
fn normalize_or(filter: Document) -> Document {
    let Some(Bson::Array(arms)) = filter.get("$or").filter(|_| filter.len() == 1) else {
        return filter;
    };
    if let [Bson::Document(only)] = arms.as_slice() {
        return only.clone();
    }
    let mut field: Option<&str> = None;
    let mut values = Vec::new();
    for arm in arms {
        let Bson::Document(d) = arm else {
            return filter.clone();
        };
        let [(k, v)] = d.iter().collect::<Vec<_>>()[..] else {
            return filter.clone();
        };
        if k.starts_with('$') || field.is_some_and(|f| f != k) {
            return filter.clone();
        }
        let value = match v {
            Bson::Document(op) if op.keys().any(|key| key.starts_with('$')) => {
                match (op.len(), op.get("$eq")) {
                    (1, Some(eq)) => eq.clone(),
                    _ => return filter.clone(),
                }
            }
            Bson::RegularExpression(_) | Bson::Array(_) => return filter.clone(),
            other => other.clone(),
        };
        field = Some(k);
        values.push(value);
    }
    match field {
        Some(f) => doc! { f: { "$in": values } },
        None => filter.clone(),
    }
}

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

/// One operator document, rewritten as mongod prints it.
fn operator(d: Document) -> Document {
    // `{$exists: false}` is `{$not: {$exists: true}}`.
    if d.len() == 1 && d.get("$exists").is_some_and(|v| !crate::util::truthy(v)) {
        return doc! { "$not": { "$exists": true } };
    }
    // An equality to a regex is the regex.
    if let (1, Some(Bson::RegularExpression(re))) = (d.len(), d.get("$eq")) {
        let mut out = doc! { "$regex": re.pattern.clone() };
        if !re.options.is_empty() {
            out.insert("$options", re.options.clone());
        }
        return out;
    }
    let mut out = Document::new();
    for (k, v) in d {
        let v = match (k.as_str(), v) {
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
                let mut pairs: Vec<(String, Bson)> = operator(inner).into_iter().collect();
                pairs.sort_by_key(|(op, _)| rank(op));
                Bson::Document(pairs.into_iter().collect())
            }
            // The branches of `$or` and `$nor` sort like `$and`'s children.
            ("$or" | "$nor", Bson::Array(items)) => {
                let mut branches: Vec<Bson> = items.into_iter().map(tidy).collect();
                branches.sort_by_key(branch_rank);
                Bson::Array(branches)
            }
            (_, other) => tidy(other),
        };
        out.insert(k, v);
    }
    out
}

fn tidy(value: Bson) -> Bson {
    match value {
        Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => {
            Bson::Document(operator(d))
        }
        Bson::Document(d) => Bson::Document(d.into_iter().map(|(k, v)| (k, tidy(v))).collect()),
        Bson::Array(a) => Bson::Array(a.into_iter().map(tidy).collect()),
        other => other,
    }
}

/// The match expression as mongod parses and normalises it.
pub(crate) fn parsed_query(filter: &Document) -> Document {
    let canonical = secantus_core::canonical_match(&Bson::Document(normalize_or(filter.clone())));
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
    match tidy(Bson::Document(canonical)) {
        Bson::Document(d) => d,
        _ => Document::new(),
    }
}

/// The parsed branches of a filter that is only an `$or`, in the order the
/// query gave them. (The parsed query itself lists them sorted.)
pub(crate) fn or_branches(filter: &Document) -> Option<Vec<Document>> {
    let filter = normalize_or(filter.clone());
    if filter.len() != 1 {
        return None;
    }
    let branches = filter.get_array("$or").ok()?;
    Some(
        branches
            .iter()
            .filter_map(Bson::as_document)
            .map(parsed_query)
            .collect(),
    )
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
