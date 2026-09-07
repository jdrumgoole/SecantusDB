//! mongod's normalised match expression — what `explain` reports as
//! `queryPlanner.parsedQuery` and as a stage's `filter`.
//!
//! mongod does NOT echo the filter you sent. It echoes the `MatchExpression`
//! tree *after* normalisation: bare equality grows an explicit `$eq`, several
//! top-level fields become an `$and` whose children are SORTED by mongod's
//! internal match-type ordinal, `$ne` becomes `$not`/`$eq`, `$type` becomes
//! numeric BSON codes, and so on.
//!
//! A port of `secantus/explain.py`'s `canonical_match`, rule for rule. The Rust
//! server used to answer `parsedQuery` with the raw filter, which diverged from
//! mongod on 44 of the 56 shapes in `tools/probes/explain_shapes.py` while the
//! Python server matched on all 56.
//!
//! Every rule was measured against mongod 8.2.11; where one was not measured
//! the input passes through unchanged, deliberately — a half-right normaliser
//! reads as authoritative while being wrong.

use bson::{Bson, Document};

/// The order `$and`'s children come back in: mongod's `MatchExpression` type
/// ordinal, then the path. So `{a: {$gt: 1}, b: 2}` reports `b`'s equality
/// FIRST. Derived pairwise against 8.2.11 rather than from the enum in
/// mongod's source, which disagrees about where `$not` sits.
fn match_type_rank(op: &str) -> i32 {
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
        "$bitsAllSet" => 15,
        "$bitsAllClear" => 16,
        "$bitsAnySet" => 17,
        "$bitsAnyClear" => 18,
        "$not" => 19,
        "$type" => 20,
        "$expr" => 21,
        "$_internalExprEq" => 22,
        // Anything unmeasured sorts AFTER everything measured, rather than
        // colliding at rank 0 with `$and`.
        _ => 99,
    }
}

/// `$type`'s string aliases as mongod renders them back: a numeric BSON code.
/// `"number"` is the one alias with no single code, and mongod echoes the
/// STRING for it rather than expanding to the four numeric ones.
fn type_alias_code(alias: &str) -> Option<i32> {
    Some(match alias {
        "double" => 1,
        "string" => 2,
        "object" => 3,
        "array" => 4,
        "binData" => 5,
        "undefined" => 6,
        "objectId" => 7,
        "bool" => 8,
        "date" => 9,
        "null" => 10,
        "regex" => 11,
        "dbPointer" => 12,
        "javascript" => 13,
        "symbol" => 14,
        "javascriptWithScope" => 15,
        "int" => 16,
        "timestamp" => 17,
        "long" => 18,
        "decimal" => 19,
        "minKey" => -1,
        "maxKey" => 127,
        _ => return None,
    })
}

const BITS_OPS: [&str; 4] = [
    "$bitsAllSet",
    "$bitsAllClear",
    "$bitsAnySet",
    "$bitsAnyClear",
];

fn is_operator_doc(value: &Bson) -> bool {
    matches!(value, Bson::Document(d) if d.keys().any(|k| k.starts_with('$')))
}

/// A bitwise operator's argument as mongod echoes it: set-bit positions. So
/// `$bitsAllSet: 1` parses back as `$bitsAllSet: [0]`.
fn bit_positions(arg: &Bson) -> Bson {
    let n = match arg {
        Bson::Int32(v) => i64::from(*v),
        Bson::Int64(v) => *v,
        // A position list or BinData is already in the echoed form; a bool is
        // not an integer here, as everywhere else in this engine.
        _ => return arg.clone(),
    };
    if n < 0 {
        return arg.clone();
    }
    let mut out = Vec::new();
    for i in 0..64 {
        if n >> i & 1 == 1 {
            out.push(Bson::Int32(i));
        }
    }
    Bson::Array(out)
}

/// `$type`'s argument as mongod echoes it: a sorted list of BSON codes.
fn type_codes(spec: &Bson) -> Bson {
    let values: Vec<Bson> = match spec {
        Bson::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    let mut mapped: Vec<Bson> = Vec::with_capacity(values.len());
    for v in &values {
        match v {
            Bson::String(s) => match type_alias_code(s) {
                Some(code) => mapped.push(Bson::Int32(code)),
                // `"number"` has no single code; mongod keeps the alias.
                None => mapped.push(v.clone()),
            },
            other => mapped.push(other.clone()),
        }
    }
    let strings: Vec<Bson> = mapped
        .iter()
        .filter(|v| matches!(v, Bson::String(_)))
        .cloned()
        .collect();
    let mut numeric: Vec<Bson> = mapped
        .iter()
        .filter(|v| !matches!(v, Bson::String(_)))
        .cloned()
        .collect();
    if !strings.is_empty() && !numeric.is_empty() {
        // Not measured (mongod may interleave); pass the input order through
        // rather than assert an order nobody probed.
        return Bson::Array(mapped);
    }
    if !strings.is_empty() {
        return Bson::Array(strings);
    }
    numeric.sort_by_key(bson_as_i64);
    Bson::Array(numeric)
}

fn bson_as_i64(v: &Bson) -> i64 {
    match v {
        Bson::Int32(n) => i64::from(*n),
        Bson::Int64(n) => *n,
        Bson::Double(d) => *d as i64,
        _ => i64::MAX,
    }
}

fn one(path: &str, value: Bson) -> Document {
    let mut d = Document::new();
    d.insert(path.to_string(), value);
    d
}

fn wrap(op: &str, value: Bson) -> Bson {
    Bson::Document(one(op, value))
}

/// Sort key for one `$and` child: (match-type ordinal, path).
fn rank(clause: &Document) -> (i32, String) {
    let Some((key, value)) = clause.iter().next() else {
        return (match_type_rank(""), String::new());
    };
    if key.starts_with('$') {
        return (match_type_rank(key), String::new());
    }
    let op = match value {
        Bson::Document(d) => d.keys().next().cloned().unwrap_or_default(),
        _ => String::new(),
    };
    (match_type_rank(&op), key.clone())
}

/// The clause list one `field: <value>` entry expands to.
fn field_clauses(path: &str, value: &Bson) -> Vec<Document> {
    if !is_operator_doc(value) {
        // A bare value -- including a whole sub-document, which is an equality
        // against that document and NOT a nested query.
        return vec![one(path, wrap("$eq", value.clone()))];
    }
    let Bson::Document(spec) = value else {
        return vec![one(path, wrap("$eq", value.clone()))];
    };
    let mut out: Vec<Document> = Vec::new();
    for (op, arg) in spec.iter() {
        match op.as_str() {
            "$ne" => out.push(one(path, wrap("$not", wrap("$eq", arg.clone())))),
            "$nin" => out.push(one(path, wrap("$not", wrap("$in", arg.clone())))),
            "$in" => match arg {
                Bson::Array(items) if items.is_empty() => {
                    out.push(one("$alwaysFalse", Bson::Int32(1)))
                }
                Bson::Array(items) if items.len() == 1 => {
                    out.push(one(path, wrap("$eq", items[0].clone())))
                }
                _ => out.push(one(path, wrap("$in", arg.clone()))),
            },
            "$all" => {
                let Bson::Array(items) = arg else {
                    out.push(one(path, wrap("$all", arg.clone())));
                    continue;
                };
                // `$all` is AND-of-equalities, and the `$elemMatch` form is
                // AND-of-elemMatches. An empty `$all` matches nothing.
                if items.is_empty() {
                    out.push(one("$alwaysFalse", Bson::Int32(1)));
                }
                for member in items {
                    match member {
                        Bson::Document(d) if d.len() == 1 && d.contains_key("$elemMatch") => {
                            let inner = d.get("$elemMatch").cloned().unwrap_or(Bson::Null);
                            out.push(one(
                                path,
                                wrap("$elemMatch", Bson::Document(canonical_match(&inner))),
                            ));
                        }
                        other => out.push(one(path, wrap("$eq", other.clone()))),
                    }
                }
            }
            "$elemMatch" => {
                if is_operator_doc(arg) {
                    // The VALUE form (`{$elemMatch: {$gt: 1}}`) applies the
                    // operators to the elements themselves, so it is not a
                    // sub-document query.
                    out.push(one(path, wrap("$elemMatch", arg.clone())));
                } else {
                    out.push(one(
                        path,
                        wrap("$elemMatch", Bson::Document(canonical_match(arg))),
                    ));
                }
            }
            "$type" => out.push(one(path, wrap("$type", type_codes(arg)))),
            "$regex" => {
                // `$options` belongs to the same clause as the `$regex` it
                // modifies -- mongod keeps the pair together.
                let mut clause = Document::new();
                clause.insert("$regex".to_string(), arg.clone());
                if let Some(options) = spec.get("$options") {
                    clause.insert("$options".to_string(), options.clone());
                }
                out.push(one(path, Bson::Document(clause)));
            }
            // Consumed by the `$regex` arm above.
            "$options" => continue,
            other if BITS_OPS.contains(&other) => {
                out.push(one(path, wrap(other, bit_positions(arg))))
            }
            other => out.push(one(path, wrap(other, arg.clone()))),
        }
    }
    out
}

/// Re-open an already-canonical document into its clause list.
fn clauses_of_canonical(doc: &Document) -> Vec<Document> {
    if doc.len() == 1 {
        if let Some(Bson::Array(children)) = doc.get("$and") {
            return children
                .iter()
                .filter_map(|c| match c {
                    Bson::Document(d) => Some(d.clone()),
                    _ => None,
                })
                .collect();
        }
    }
    if doc.is_empty() {
        vec![]
    } else {
        vec![doc.clone()]
    }
}

/// `$nor`'s per-child negation: `{a: {$eq: 1}}` -> `{a: {$not: {$eq: 1}}}`.
fn negate(clause: &Document) -> Vec<Document> {
    let mut out = Vec::new();
    for sub in clauses_of_canonical(clause) {
        let Some((path, value)) = sub.iter().next() else {
            continue;
        };
        if path.starts_with('$') {
            out.push(one("$nor", Bson::Array(vec![Bson::Document(sub.clone())])));
        } else {
            out.push(one(path, wrap("$not", value.clone())));
        }
    }
    out
}

/// Flatten a filter document into mongod's list of `$and` children.
fn clauses(filter: &Document) -> Vec<Document> {
    let mut out: Vec<Document> = Vec::new();
    for (key, value) in filter.iter() {
        // mongod drops `$comment` from the parsed tree entirely.
        if key == "$comment" {
            continue;
        }
        match key.as_str() {
            "$and" => {
                if let Bson::Array(subs) = value {
                    for sub in subs {
                        if let Bson::Document(d) = sub {
                            out.extend(clauses(d));
                        }
                    }
                }
            }
            "$nor" => {
                let children = canonical_children(value);
                if children.len() >= 2 {
                    // Kept whole while it is the query's only clause; the
                    // caller decomposes it if anything else joins it.
                    out.push(one(
                        "$nor",
                        Bson::Array(children.into_iter().map(Bson::Document).collect()),
                    ));
                } else {
                    for child in &children {
                        out.extend(negate(child));
                    }
                }
            }
            "$or" => {
                let children = canonical_children(value);
                if children.len() == 1 {
                    out.extend(clauses_of_canonical(&children[0]));
                } else {
                    out.push(one(
                        "$or",
                        Bson::Array(children.into_iter().map(Bson::Document).collect()),
                    ));
                }
            }
            other if other.starts_with('$') => out.push(one(other, value.clone())),
            other => out.extend(field_clauses(other, value)),
        }
    }
    out
}

fn canonical_children(value: &Bson) -> Vec<Document> {
    let Bson::Array(subs) = value else {
        return vec![];
    };
    subs.iter()
        .filter(|s| matches!(s, Bson::Document(_)))
        .map(canonical_match)
        .collect()
}

/// mongod's normalised form of `filter`, as `parsedQuery` reports it.
///
/// Anything unmeasured passes through unchanged rather than being guessed at.
/// The known residue against 8.2.11 is `$expr` (which mongod splits into an
/// `$expr` clause plus an `$_internalExprEq` index-usable twin) and
/// `$jsonSchema`; both are echoed as sent.
pub fn canonical_match(filter: &Bson) -> Document {
    let Bson::Document(doc) = filter else {
        return Document::new();
    };
    if doc.is_empty() {
        return Document::new();
    }
    let found = clauses(doc);
    if found
        .iter()
        .any(|c| c.len() == 1 && c.contains_key("$alwaysFalse"))
    {
        return one("$alwaysFalse", Bson::Int32(1));
    }
    if found.is_empty() {
        return Document::new();
    }
    if found.len() == 1 {
        return found.into_iter().next().expect("length checked");
    }
    // A `$nor` survives as a node only when it is the whole query. As soon as
    // it shares an `$and` with anything else, mongod merges it in as one `$not`
    // per child rather than nesting it (probed 8.2.11).
    let mut expanded: Vec<Document> = Vec::new();
    for clause in &found {
        if clause.len() == 1 {
            if let Some(Bson::Array(children)) = clause.get("$nor") {
                for child in children {
                    if let Bson::Document(d) = child {
                        expanded.extend(negate(d));
                    }
                }
                continue;
            }
        }
        expanded.push(clause.clone());
    }
    // Stable, like Python's `sorted`: equal ranks keep their input order.
    expanded.sort_by_key(rank);
    one(
        "$and",
        Bson::Array(expanded.into_iter().map(Bson::Document).collect()),
    )
}
