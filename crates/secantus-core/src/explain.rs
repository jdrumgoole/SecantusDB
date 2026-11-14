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

use bson::{doc, Bson, Document};

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
        // mongod's enum order for the twins `$expr` derives (8.2.11): only
        // `$_internalExprEq` beside `$_internalExprLt` was measured.
        "$_internalExprEq" => 22,
        "$_internalExprGt" => 23,
        "$_internalExprGte" => 24,
        "$_internalExprLt" => 25,
        "$_internalExprLte" => 26,
        // What `$jsonSchema` parses into, after every other match type, in
        // mongod's enum order (the order its tree sort uses).
        "$_internalSchemaAllElemMatchFromIndex" => 30,
        "$_internalSchemaAllowedProperties" => 31,
        "$_internalSchemaCond" => 32,
        "$_internalSchemaEq" => 33,
        "$_internalSchemaFmod" => 34,
        "$_internalSchemaMatchArrayIndex" => 35,
        "$_internalSchemaMaxItems" => 36,
        "$_internalSchemaMaxLength" => 37,
        "$_internalSchemaMaxProperties" => 38,
        "$_internalSchemaMinItems" => 39,
        "$_internalSchemaMinLength" => 40,
        "$_internalSchemaMinProperties" => 41,
        "$_internalSchemaObjectMatch" => 42,
        "$_internalSchemaRootDocEq" => 43,
        "$_internalSchemaType" => 44,
        "$_internalSchemaUniqueItems" => 45,
        "$_internalSchemaXor" => 46,
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
pub fn clause_rank(clause: &Document) -> (i32, String, u8) {
    rank(clause)
}

fn rank(clause: &Document) -> (i32, String, u8) {
    let Some((key, value)) = clause.iter().next() else {
        return (match_type_rank(""), String::new(), 0);
    };
    if key.starts_with('$') {
        // Two `$expr`s: one whose argument is not a document (`"$q"`) sorts
        // before one whose argument is, whatever order they were written in
        // (measured 8.2.11, 2026-10-16).
        let doc_arg = u8::from(key == "$expr" && matches!(value, Bson::Document(_)));
        return (match_type_rank(key), String::new(), doc_arg);
    }
    let op = match value {
        Bson::Document(d) => d.keys().next().cloned().unwrap_or_default(),
        _ => String::new(),
    };
    (match_type_rank(&op), key.clone(), 0)
}

/// The order mongod gives the clauses a FETCH stage rechecks: by path, the
/// operators that have none (`$or`, `$expr`, ...) first, and then by match
/// type -- not the canonical order, which puts the type first (measured
/// 8.2.11, 2026-10-16: `{z: 1, a: {$gt: 1}, b: 2}` over an index on `z`
/// rechecks `a` before `b`). A stable sort on this keeps ties in canonical
/// order.
pub fn residual_key(clause: &Document) -> (String, i32) {
    let (rank, path, _) = rank(clause);
    // A `$not` is a tree node of its own, with no path: `{c: {$not: ...}}`
    // sorts with `$or` and `$expr`, ahead of every field (measured).
    let negated = clause
        .values()
        .next()
        .and_then(Bson::as_document)
        .is_some_and(|p| p.contains_key("$not"));
    (if negated { String::new() } else { path }, rank)
}

/// `path` equal to `value`, where a regex given as a value (bare, in an
/// `$in` of one, or in an `$all`) is a pattern match, `{$regex: ...}`. A
/// regex given to `$eq` itself stays an equality to that regex, which is how
/// mongod 8.2.11 parses each (measured 2026-10-20).
fn equal_to(path: &str, value: &Bson) -> Document {
    if let Bson::RegularExpression(re) = value {
        let mut clause = Document::new();
        clause.insert("$regex", re.pattern.clone());
        if !re.options.is_empty() {
            clause.insert("$options", re.options.clone());
        }
        return one(path, Bson::Document(clause));
    }
    one(path, wrap("$eq", value.clone()))
}

/// The clause list one `field: <value>` entry expands to.
fn field_clauses(path: &str, value: &Bson) -> Vec<Document> {
    if !is_operator_doc(value) {
        // A bare value -- including a whole sub-document, which is an equality
        // against that document and NOT a nested query.
        return vec![equal_to(path, value)];
    }
    let Bson::Document(spec) = value else {
        return vec![one(path, wrap("$eq", value.clone()))];
    };
    // A `$near` is one node with its `$maxDistance` / `$minDistance`
    // (measured 8.2.11: `{loc: {$nearSphere: [0, 0], $maxDistance: 0.25}}`
    // is its own `parsedQuery`), not a clause per operator.
    if spec.contains_key("$near") || spec.contains_key("$nearSphere") {
        return vec![one(path, value.clone())];
    }
    let mut out: Vec<Document> = Vec::new();
    for (op, arg) in spec.iter() {
        match op.as_str() {
            "$ne" => out.push(one(path, wrap("$not", wrap("$eq", arg.clone())))),
            "$nin" => out.push(one(path, wrap("$not", wrap("$in", arg.clone())))),
            "$in" => match arg {
                Bson::Array(items) if items.is_empty() => {
                    out.push(one("$alwaysFalse", Bson::Int32(1)))
                }
                Bson::Array(items) if items.len() == 1 => out.push(equal_to(path, &items[0])),
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
                        other => out.push(equal_to(path, other)),
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
            // mongod optimises the expression and derives index-usable twins
            // from it (`crate::expr_rewrite`).
            "$expr" => match crate::expr_rewrite::expr_clauses(value) {
                crate::expr_rewrite::ExprClauses::Constant(true) => {}
                crate::expr_rewrite::ExprClauses::Constant(false) => {
                    out.push(one("$alwaysFalse", Bson::Int32(1)))
                }
                crate::expr_rewrite::ExprClauses::Rewritten { expr, derived } => {
                    out.push(one("$expr", expr));
                    for twin in derived {
                        match twin.iter().next() {
                            // An `$in` twin is an ordinary `$in` clause.
                            Some((path, Bson::Document(p))) if p.contains_key("$in") => {
                                out.extend(field_clauses(path, &Bson::Document(p.clone())))
                            }
                            _ => out.push(twin),
                        }
                    }
                }
            },
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
/// `$expr` is optimised and gains mongod's index-usable twins
/// (`crate::expr_rewrite`). `$jsonSchema` is echoed here and translated by
/// the command layer (`secantus-commands`' `explain::json_schema`).
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

/// `filter` as mongod parses and optimises it for a pipeline `$match`
/// (`DocumentSourceMatch`: `MatchExpression::optimize`, with no
/// `sortTree`), which is how explain lists a `$match` the query does not
/// take. Unlike `canonical_match` (the query's `parsedQuery`) nothing is
/// sorted: clauses keep the order they were written in, and the tree is
/// reshaped only as mongod 8.2's optimiser does it
/// (`ListOfMatchExpression::getOptimizer`, `expression_tree.cpp`):
///
/// * an `$and` takes in the children of each `$and` among its children, and
///   puts them AFTER its own (`{c: 1, $and: [{b: 1, a: 1}]}` is `[c, b, a]`),
///   and an `$or` the same with `$or` children. `$all` and an `$expr` with
///   index-usable twins (`[twin, $expr]`) parse to an `$and` of their own,
///   and so go last too;
/// * a constant is dropped where it changes nothing (`$alwaysTrue` from an
///   `$and`, `$alwaysFalse` from an `$or` or a `$nor`), and decides the node
///   where it decides the answer;
/// * a node of one child is that child, and a `$nor` of one is its `$not`;
/// * duplicates stay, and a `$nor` among other clauses stays a `$nor`;
/// * an `$or`'s equalities and regexes on one path become that path's `$in`
///   (`or_as_in`).
///
/// Leaves are parsed as `canonical_match` parses them. Measured on 8.2.11,
/// 2026-11-14.
pub fn match_as_written(filter: &Bson) -> Document {
    let Bson::Document(doc) = filter else {
        return Document::new();
    };
    written(doc)
}

/// One document's clauses, as the `$and` they parse to, optimised.
fn written(doc: &Document) -> Document {
    let mut children: Vec<Document> = Vec::new();
    for (key, value) in doc {
        match key.as_str() {
            "$comment" => {}
            "$and" | "$or" | "$nor" => {
                let subs = match value {
                    Bson::Array(subs) => subs.iter().filter_map(Bson::as_document).map(written),
                    _ => {
                        children.push(one(key, value.clone()));
                        continue;
                    }
                };
                children.push(optimized(key, subs.collect()));
            }
            "$expr" => match crate::expr_rewrite::expr_clauses(value) {
                // A constant `$expr` optimises to an empty `$and` (`{}`), or
                // to `$alwaysFalse`.
                crate::expr_rewrite::ExprClauses::Constant(true) => children.push(Document::new()),
                crate::expr_rewrite::ExprClauses::Constant(false) => {
                    children.push(one("$alwaysFalse", Bson::Int32(1)))
                }
                crate::expr_rewrite::ExprClauses::Rewritten { expr, derived } => {
                    // mongod's `ExprMatchExpression` optimises to an `$and`
                    // of what `RewriteExpr` derives (one twin, or an `$and`
                    // of several) and then itself, so one twin lists ahead
                    // of the `$expr` and several after it.
                    let mut twins: Vec<Document> = Vec::new();
                    for twin in derived {
                        match twin.iter().next() {
                            Some((path, Bson::Document(p))) if p.contains_key("$in") => {
                                twins.extend(field_clauses(path, &Bson::Document(p.clone())))
                            }
                            _ => twins.push(twin),
                        }
                    }
                    let expr = one("$expr", expr);
                    children.push(if twins.is_empty() {
                        expr
                    } else {
                        optimized("$and", vec![optimized("$and", twins), expr])
                    });
                }
            },
            other if other.starts_with('$') => children.push(one(other, value.clone())),
            path => children.extend(field_written(path, value)),
        }
    }
    // A document of one clause parses to that clause, with no `$and` to
    // drop a lone `$alwaysTrue` (measured 8.2.11).
    if children.len() == 1 {
        return children.pop().expect("one clause");
    }
    optimized("$and", children)
}

/// Whether an `$elemMatch` argument is its value form, which applies its
/// operators to the elements themselves: its FIRST field is an operator,
/// and not one of the pathless ones that take a whole document (mongod's
/// `parseElemMatch`).
fn elem_match_is_value(arg: &Document) -> bool {
    arg.keys().next().is_some_and(|k| {
        k.starts_with('$')
            && !matches!(
                k.as_str(),
                "$and"
                    | "$or"
                    | "$nor"
                    | "$expr"
                    | "$where"
                    | "$text"
                    | "$comment"
                    | "$jsonSchema"
                    | "$alwaysTrue"
                    | "$alwaysFalse"
            )
            && !k.starts_with("$_internal")
    })
}

/// The clauses one `field: <value>` entry parses to, in the order written.
fn field_written(path: &str, value: &Bson) -> Vec<Document> {
    let Bson::Document(spec) = value else {
        return field_clauses(path, value);
    };
    if !is_operator_doc(value) || spec.contains_key("$near") || spec.contains_key("$nearSphere") {
        return field_clauses(path, value);
    }
    let mut out = Vec::new();
    for (op, arg) in spec {
        match (op.as_str(), arg) {
            ("$options", _) => {}
            ("$regex", _) => {
                let mut clause = doc! { "$regex": arg.clone() };
                if let Some(options) = spec.get("$options") {
                    clause.insert("$options", options.clone());
                }
                out.extend(field_clauses(path, &Bson::Document(clause)));
            }
            // `$all` is an `$and` of its members.
            ("$all", Bson::Array(items)) => {
                let members = items
                    .iter()
                    .map(|member| match member {
                        Bson::Document(d) if d.len() == 1 && d.contains_key("$elemMatch") => {
                            let inner = d.get("$elemMatch").cloned().unwrap_or(Bson::Null);
                            elem_match(path, &inner)
                        }
                        other => equal_to(path, other),
                    })
                    .collect();
                out.push(if items.is_empty() {
                    one("$alwaysFalse", Bson::Int32(1))
                } else {
                    optimized("$and", members)
                });
            }
            ("$elemMatch", _) => out.push(elem_match(path, arg)),
            // A `$not` of several operators is the `$not` of their `$and`.
            ("$not", Bson::Document(inner))
                if inner.keys().filter(|k| *k != "$options").count() > 1 =>
            {
                out.push(not_of(optimized(
                    "$and",
                    field_written(path, &Bson::Document(inner.clone())),
                )))
            }
            // `$nin: []` is the `$not` of an `$alwaysFalse`, which has no
            // path to print it under.
            ("$nin", Bson::Array(items)) if items.is_empty() => out.push(doc! {
                "$nor": [{ "$alwaysFalse": 1 }]
            }),
            _ => out.extend(field_clauses(path, &Bson::Document(one(op, arg.clone())))),
        }
    }
    out
}

fn elem_match(path: &str, arg: &Bson) -> Document {
    match arg {
        Bson::Document(d) if !elem_match_is_value(d) => {
            one(path, wrap("$elemMatch", Bson::Document(written(d))))
        }
        other => one(path, wrap("$elemMatch", other.clone())),
    }
}

fn is_always(clause: &Document, which: &str) -> bool {
    clause.len() == 1 && clause.contains_key(which)
}

fn trivially_true(clause: &Document) -> bool {
    clause.is_empty() || is_always(clause, "$alwaysTrue")
}

/// The children of `clause` when it is a node of `kind` (`{kind: [...]}`).
fn node_children(clause: &Document, kind: &str) -> Option<Vec<Document>> {
    match clause.get(kind) {
        Some(Bson::Array(items)) if clause.len() == 1 => Some(
            items
                .iter()
                .filter_map(Bson::as_document)
                .cloned()
                .collect(),
        ),
        _ => None,
    }
}

/// An `$and` / `$or` / `$nor` of these (already optimised) children,
/// optimised as mongod's `ListOfMatchExpression::getOptimizer` does.
fn optimized(kind: &str, children: Vec<Document>) -> Document {
    let mut children = if kind == "$nor" {
        children
    } else {
        let mut kept = Vec::new();
        let mut absorbed = Vec::new();
        for child in children {
            match node_children(&child, kind) {
                Some(grand) => absorbed.extend(grand),
                None => kept.push(child),
            }
        }
        kept.extend(absorbed);
        kept
    };
    children.retain(|c| match kind {
        "$and" => !trivially_true(c),
        _ => !is_always(c, "$alwaysFalse"),
    });
    if children.is_empty() {
        return if kind == "$or" {
            one("$alwaysFalse", Bson::Int32(1))
        } else {
            Document::new()
        };
    }
    if children.len() == 1 {
        let only = children.pop().expect("one child");
        if kind != "$nor" {
            return only;
        }
        if !trivially_true(&only) {
            return not_of(only);
        }
        children.push(only);
    }
    for child in &children {
        match kind {
            "$and" if is_always(child, "$alwaysFalse") => {
                return one("$alwaysFalse", Bson::Int32(1))
            }
            "$or" if trivially_true(child) => return Document::new(),
            "$nor" if trivially_true(child) => return one("$alwaysFalse", Bson::Int32(1)),
            _ => {}
        }
    }
    if kind == "$or" {
        return or_as_in(children);
    }
    one(
        kind,
        Bson::Array(children.into_iter().map(Bson::Document).collect()),
    )
}

/// The `$not` of `clause`: under its path where it has one, else a `$nor`
/// of it (`NotMatchExpression`'s serialisation).
fn not_of(clause: Document) -> Document {
    match clause.iter().next() {
        Some((path, pred)) if clause.len() == 1 && !path.starts_with('$') => {
            one(path, wrap("$not", pred.clone()))
        }
        _ => one("$nor", Bson::Array(vec![Bson::Document(clause)])),
    }
}

/// The value an `$or` branch may give an `$in` on its path: an equality to
/// anything but a regex, or a regex.
fn in_member(branch: &Document) -> Option<(String, Bson)> {
    let [(path, Bson::Document(pred))] = branch.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    if path.starts_with('$') {
        return None;
    }
    if let (1, Some(v)) = (pred.len(), pred.get("$eq")) {
        return (!matches!(v, Bson::RegularExpression(_))).then(|| (path.clone(), v.clone()));
    }
    let pattern = match pred.get("$regex")? {
        Bson::String(s) => s.clone(),
        Bson::RegularExpression(re) => re.pattern.clone(),
        _ => return None,
    };
    let options = pred.get_str("$options").unwrap_or_default();
    (pred.len() == 1 + usize::from(pred.contains_key("$options"))).then(|| {
        (
            path.clone(),
            Bson::RegularExpression(bson::Regex {
                pattern,
                options: options.to_string(),
            }),
        )
    })
}

/// mongod's rewrite of an `$or`'s equalities and regexes on one path into
/// that path's `$in`. The `$or` lists the new `$in`s, then the branches no
/// `$in` could take in the order written, then the equalities and regexes
/// that were alone on their path.
///
/// mongod gathers them in an `stdx::unordered_map` keyed by path, and lists
/// the `$in`s and the lone equalities in its iteration order, which changes
/// from one mongod process to the next (measured 8.2.11, 2026-11-14: two
/// runs listed `{$or: [{b: 1}, {a: 2}]}` as `[b, a]` and `[a, b]`). There is
/// no order to match there; this lists each group in first-written order.
fn or_as_in(children: Vec<Document>) -> Document {
    let mut paths: Vec<(String, Vec<(Document, Bson)>)> = Vec::new();
    let mut others: Vec<Document> = Vec::new();
    for child in children {
        let Some((path, value)) = in_member(&child) else {
            others.push(child);
            continue;
        };
        match paths.iter_mut().find(|(p, _)| *p == path) {
            Some((_, members)) => members.push((child, value)),
            None => paths.push((path, vec![(child, value)])),
        }
    }
    let mut ins: Vec<Document> = Vec::new();
    let mut alone: Vec<Document> = Vec::new();
    for (path, mut members) in paths {
        if members.len() == 1 {
            alone.push(members.pop().expect("one").0);
        } else {
            let values = members.into_iter().map(|(_, v)| v).collect();
            ins.push(one(&path, wrap("$in", Bson::Array(values))));
        }
    }
    others.extend(alone);
    if ins.is_empty() {
        return one(
            "$or",
            Bson::Array(others.into_iter().map(Bson::Document).collect()),
        );
    }
    if others.is_empty() && ins.len() == 1 {
        return ins.pop().expect("one");
    }
    ins.extend(others);
    one(
        "$or",
        Bson::Array(ins.into_iter().map(Bson::Document).collect()),
    )
}

/// mongod's blocking-sort memory budget, reported verbatim as the `SORT`
/// stage's `memLimit` (`internalQueryMaxBlockingSortMemoryUsageBytes`, 100 MiB
/// — the default, which is also what `serverParameters` reports).
pub const SORT_MEM_LIMIT_BYTES: i64 = 104_857_600;

/// `PROJECTION_SIMPLE` or `PROJECTION_DEFAULT` for this spec.
///
/// mongod uses the fast path only for a flat inclusion / exclusion list; a
/// dotted path or any operator (`$elemMatch` / `$slice` / a computed
/// expression) drops it to the general one. Probed 8.2.11.
fn projection_stage_name(projection: &Document) -> &'static str {
    for (field, value) in projection.iter() {
        if field.contains('.') || matches!(value, Bson::Document(_) | Bson::Array(_)) {
            return "PROJECTION_DEFAULT";
        }
    }
    "PROJECTION_SIMPLE"
}

/// Wrap the scan node `base` in mongod's query-shape stages.
///
/// The nesting is mongod's, measured on 8.2.11 — it is not the order the
/// command's fields are written in:
///
/// * a sort the index cannot serve becomes a blocking `SORT` directly above the
///   scan, and it ABSORBS the limit (as `limitAmount`, counting the skipped
///   documents), so no separate `LIMIT` stage appears;
/// * `SKIP` sits above the scan (or above the `SORT`);
/// * the projection sits above the skip;
/// * a `LIMIT` not absorbed by a sort is the OUTERMOST stage, above the
///   projection.
///
/// A port of `secantus.explain.build_stage_tree`. The Rust server reported the
/// bare scan node, so `{filter: …, limit: 3}` came back as a `COLLSCAN` where
/// mongod reports a `LIMIT` above one.
pub fn build_stage_tree(
    base: Document,
    sort: Option<&Document>,
    sort_served_by_index: bool,
    projection: Option<&Document>,
    skip: i64,
    limit: i64,
) -> Document {
    let mut node = base;
    let skip_n = skip.max(0);
    // A negative `limit` is the driver's "single batch" flag, not a smaller
    // limit; mongod reports its magnitude.
    let limit_n = limit.abs();

    let blocking_sort = sort.is_some_and(|s| !s.is_empty()) && !sort_served_by_index;
    let mut absorbed = false;
    if blocking_sort {
        let mut sort_stage = Document::new();
        sort_stage.insert("stage", "SORT");
        sort_stage.insert(
            "sortPattern",
            Bson::Document(sort.cloned().unwrap_or_default()),
        );
        sort_stage.insert("memLimit", SORT_MEM_LIMIT_BYTES);
        if limit_n > 0 {
            // The sort must retain everything the skip will later discard.
            sort_stage.insert("limitAmount", limit_n + skip_n);
            absorbed = true;
        }
        sort_stage.insert("type", "simple");
        sort_stage.insert("inputStage", Bson::Document(node));
        node = sort_stage;
    }
    if skip_n > 0 {
        let mut skip_stage = Document::new();
        skip_stage.insert("stage", "SKIP");
        skip_stage.insert("skipAmount", skip_n);
        skip_stage.insert("inputStage", Bson::Document(node));
        node = skip_stage;
    }
    if let Some(proj) = projection.filter(|p| !p.is_empty()) {
        let mut proj_stage = Document::new();
        proj_stage.insert("stage", projection_stage_name(proj));
        proj_stage.insert("transformBy", Bson::Document(proj.clone()));
        proj_stage.insert("inputStage", Bson::Document(node));
        node = proj_stage;
    }
    if limit_n > 0 && !absorbed {
        let mut limit_stage = Document::new();
        limit_stage.insert("stage", "LIMIT");
        limit_stage.insert("limitAmount", limit_n);
        limit_stage.insert("inputStage", Bson::Document(node));
        node = limit_stage;
    }
    node
}

#[cfg(test)]
mod match_as_written_tests {
    use super::match_as_written;
    use bson::{doc, Bson, Document};

    fn written(filter: Document) -> Document {
        match_as_written(&Bson::Document(filter))
    }

    /// The shapes mongod 8.2.11 lists for a pipeline `$match` (measured
    /// 2026-11-14), before the command layer's tidying.
    #[test]
    fn keeps_the_written_order_and_reshapes_as_mongod() {
        let cases = [
            (
                doc! { "d": { "$gt": 0 }, "c": 1 },
                doc! { "$and": [{ "d": { "$gt": 0 } }, { "c": { "$eq": 1 } }] },
            ),
            // An `$and`'s `$and` children go after its own.
            (
                doc! { "$and": [{ "b": 1 }, { "$and": [{ "z": 1 }, { "a": 1 }] }], "c": 1 },
                doc! { "$and": [
                    { "c": { "$eq": 1 } }, { "b": { "$eq": 1 } },
                    { "z": { "$eq": 1 } }, { "a": { "$eq": 1 } },
                ] },
            ),
            // An `$and` of one is that one, in its place.
            (
                doc! { "c": 1, "$and": [{ "b": 1 }], "d": 1 },
                doc! { "$and": [{ "c": { "$eq": 1 } }, { "b": { "$eq": 1 } }, { "d": { "$eq": 1 } }] },
            ),
            // Duplicates stay.
            (
                doc! { "$and": [{ "b": 1 }, { "a": 1 }, { "b": 1 }] },
                doc! { "$and": [{ "b": { "$eq": 1 } }, { "a": { "$eq": 1 } }, { "b": { "$eq": 1 } }] },
            ),
            // A `$nor` among other clauses stays one; a `$nor` of one is its `$not`.
            (
                doc! { "z": 1, "$nor": [{ "b": 1 }, { "a": 1 }] },
                doc! { "$and": [
                    { "z": { "$eq": 1 } },
                    { "$nor": [{ "b": { "$eq": 1 } }, { "a": { "$eq": 1 } }] },
                ] },
            ),
            (
                doc! { "c": 1, "$nor": [{ "b": 1 }], "d": 1 },
                doc! { "$and": [
                    { "c": { "$eq": 1 } }, { "b": { "$not": { "$eq": 1 } } }, { "d": { "$eq": 1 } },
                ] },
            ),
            (
                doc! { "$nor": [{ "b": 1, "a": 2 }] },
                doc! { "$nor": [{ "$and": [{ "b": { "$eq": 1 } }, { "a": { "$eq": 2 } }] }] },
            ),
            // `$all` is an `$and`, so it goes last.
            (
                doc! { "t": { "$all": [3, 1] }, "a": 1 },
                doc! { "$and": [{ "a": { "$eq": 1 } }, { "t": { "$eq": 3 } }, { "t": { "$eq": 1 } }] },
            ),
            // An `$or`'s equalities on one path are an `$in`, listed first;
            // an equality alone on its path goes last.
            (
                doc! { "$or": [{ "y": { "$gt": 1 } }, { "x": 2 }, { "x": 1 }] },
                doc! { "$or": [{ "x": { "$in": [2, 1] } }, { "y": { "$gt": 1 } }] },
            ),
            (
                doc! { "$or": [{ "x": 1 }, { "z": { "$gt": 1 } }, { "z": { "$gt": 1 } }] },
                doc! { "$or": [
                    { "z": { "$gt": 1 } }, { "z": { "$gt": 1 } }, { "x": { "$eq": 1 } },
                ] },
            ),
            // An `$or` of `$alwaysFalse` and one clause is that clause.
            (
                doc! { "$or": [{ "a": { "$in": [] } }, { "b": { "$gt": 1 } }] },
                doc! { "b": { "$gt": 1 } },
            ),
            // `$nin: []` is the `$not` of an `$alwaysFalse`.
            (
                doc! { "c": 1, "a": { "$nin": [] } },
                doc! { "$and": [{ "c": { "$eq": 1 } }, { "$nor": [{ "$alwaysFalse": 1 }] }] },
            ),
            // A `$not` of two operators is the `$not` of their `$and`.
            (
                doc! { "a": { "$not": { "$lt": 9, "$gt": 3 } } },
                doc! { "$nor": [{ "$and": [{ "a": { "$lt": 9 } }, { "a": { "$gt": 3 } }] }] },
            ),
            // An `$elemMatch` document whose first field is a path is a query.
            (
                doc! { "t": { "$elemMatch": { "b": 1, "$and": [{ "q": 1 }], "a": { "$gt": 0 } } } },
                doc! { "t": { "$elemMatch": { "$and": [
                    { "b": { "$eq": 1 } }, { "q": { "$eq": 1 } }, { "a": { "$gt": 0 } },
                ] } } },
            ),
            (
                doc! { "b": 1, "$comment": "x", "$alwaysTrue": 1, "a": 1 },
                doc! { "$and": [{ "b": { "$eq": 1 } }, { "a": { "$eq": 1 } }] },
            ),
            (doc! { "$alwaysTrue": 1 }, doc! { "$alwaysTrue": 1 }),
            (doc! { "$and": [{ "$alwaysTrue": 1 }] }, doc! {}),
            (
                doc! { "$or": [{ "$alwaysTrue": 1 }] },
                doc! { "$alwaysTrue": 1 },
            ),
            (doc! { "$expr": true }, doc! {}),
            (doc! { "$or": [{ "b": 1 }, { "$expr": true }] }, doc! {}),
            (
                doc! { "$nor": [{ "$expr": true }] },
                doc! { "$alwaysFalse": 1 },
            ),
            (
                doc! { "c": 1, "a": { "$in": [] } },
                doc! { "$alwaysFalse": 1 },
            ),
        ];
        for (filter, want) in cases {
            assert_eq!(written(filter.clone()), want, "{filter}");
        }
    }
}
