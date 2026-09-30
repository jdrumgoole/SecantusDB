//! `errInfo.details` for a document that failed its collection's validator.
//!
//! mongod explains a 121 `Document failed validation` with a structure that
//! names every rule the document broke -- per keyword for `$jsonSchema`, per
//! clause for a query-operator validator. Drivers' `errorResponse` tests read
//! it, and it is the only thing that tells a user WHY a write was refused. The
//! Rust server sent `{operatorName: "$jsonSchema"}` and nothing else for every
//! `$jsonSchema` failure (measured 8.2.11, 2026-09-30).
//!
//! The shapes are transcribed from mongod 8.2's `doc_validation_error.cpp`, and
//! the ORDER of `$jsonSchema` rules from `json_schema_parser.cpp`'s `_parse`,
//! which builds one AND of: the scalar keywords (`pattern`, `maxLength`,
//! `minLength`, `multipleOf`, `maximum`, `minimum`), the array keywords
//! (`minItems`, `maxItems`, `uniqueItems`, `items`, `additionalItems`), the
//! object keywords (`properties`, `patternProperties` / `additionalProperties`,
//! `required`, `minProperties`, `maxProperties`, `dependencies`), the logical
//! ones (`allOf`, `anyOf`, `oneOf`, `not`, `enum`), and -- below the top level
//! -- the type restriction LAST. Every scalar / array / object keyword is a
//! restriction on its own type: it cannot fail for a value of another type.
//!
//! Which document FAILS is still decided by the matcher; this module only
//! explains a failure already established. `tools/probes/
//! validation_error_details.py` compares the output with mongod exactly.

use bson::{doc, Bson, Document};
use secantus_core::query::bson_type_name;

/// The explanation for `doc` failing `validator`.
pub(crate) fn explain(validator: &Document, doc: &Document) -> Document {
    let node = parse_filter(validator);
    explain_node(&node, doc, false).unwrap_or_else(|| doc! {"operatorName": "validator"})
}

// --- query-operator validators ----------------------------------------------

enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Nor(Vec<Node>),
    Leaf {
        field: String,
        op: String,
        spec: Document,
        filter: Document,
    },
    Not {
        inner: Box<Node>,
        filter: Document,
    },
    Expr(Bson),
    JsonSchema(Document),
}

/// A filter document as mongod's match-expression tree: each top-level clause
/// -- and each operator of a multi-operator field -- is its own child of an
/// implicit `$and`, which a single child replaces.
fn parse_filter(filter: &Document) -> Node {
    let mut clauses = Vec::new();
    for (key, value) in filter {
        match key.as_str() {
            "$and" | "$or" | "$nor" => {
                let children: Vec<Node> = value
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Bson::as_document)
                            .map(parse_filter)
                            .collect()
                    })
                    .unwrap_or_default();
                clauses.push(match key.as_str() {
                    "$and" => Node::And(children),
                    "$or" => Node::Or(children),
                    _ => Node::Nor(children),
                });
            }
            "$expr" => clauses.push(Node::Expr(value.clone())),
            "$jsonSchema" => {
                if let Bson::Document(s) = value {
                    clauses.push(Node::JsonSchema(s.clone()));
                }
            }
            "$comment" => {}
            _ => clauses.extend(parse_field(key, value)),
        }
    }
    if clauses.len() == 1 {
        clauses.pop().unwrap()
    } else {
        Node::And(clauses)
    }
}

fn is_operator_doc(value: &Bson) -> bool {
    matches!(value, Bson::Document(d) if !d.is_empty() && d.keys().all(|k| k.starts_with('$')))
}

fn parse_field(field: &str, value: &Bson) -> Vec<Node> {
    let Bson::Document(ops) = value else {
        let op = if matches!(value, Bson::RegularExpression(_)) {
            "$regex"
        } else {
            "$eq"
        };
        let spec = doc! {field: value.clone()};
        return vec![Node::Leaf {
            field: field.to_string(),
            op: op.to_string(),
            filter: spec.clone(),
            spec,
        }];
    };
    if !is_operator_doc(value) {
        let spec = doc! {field: value.clone()};
        return vec![Node::Leaf {
            field: field.to_string(),
            op: "$eq".into(),
            filter: spec.clone(),
            spec,
        }];
    }
    let mut out = Vec::new();
    for (op, arg) in ops {
        match op.as_str() {
            // `$options` belongs to `$regex`, and is echoed with it.
            "$options" => {}
            "$regex" => {
                let mut inner = doc! {"$regex": arg.clone()};
                if let Some(o) = ops.get("$options") {
                    inner.insert("$options", o.clone());
                }
                let spec = doc! {field: inner};
                out.push(Node::Leaf {
                    field: field.to_string(),
                    op: "$regex".into(),
                    filter: spec.clone(),
                    spec,
                });
            }
            "$not" => {
                let mut inner_nodes = parse_field(field, arg);
                let inner = if inner_nodes.len() == 1 {
                    inner_nodes.pop().unwrap()
                } else {
                    Node::And(inner_nodes)
                };
                out.push(Node::Not {
                    inner: Box::new(inner),
                    filter: doc! {field: {"$not": arg.clone()}},
                });
            }
            _ => {
                let spec = doc! {field: {op.clone(): arg.clone()}};
                out.push(Node::Leaf {
                    field: field.to_string(),
                    op: op.clone(),
                    filter: spec.clone(),
                    spec,
                });
            }
        }
    }
    out
}

fn filter_matches(doc: &Document, filter: &Document) -> bool {
    secantus_core::query::matches(doc, filter, &Document::new(), None).unwrap_or(false)
}

fn node_matches(node: &Node, doc: &Document) -> bool {
    match node {
        Node::And(c) => c.iter().all(|n| node_matches(n, doc)),
        Node::Or(c) => c.iter().any(|n| node_matches(n, doc)),
        Node::Nor(c) => !c.iter().any(|n| node_matches(n, doc)),
        Node::Leaf { filter, .. } | Node::Not { filter, .. } => filter_matches(doc, filter),
        Node::Expr(e) => filter_matches(doc, &doc! {"$expr": e.clone()}),
        Node::JsonSchema(s) => filter_matches(doc, &doc! {"$jsonSchema": s.clone()}),
    }
}

/// Why `node` fails for `doc` -- or, when `inverted`, why it MATCHES (the
/// child of a `$nor` / `$not`). `None` when there is nothing to explain.
fn explain_node(node: &Node, doc: &Document, inverted: bool) -> Option<Document> {
    if node_matches(node, doc) != inverted {
        return None;
    }
    Some(match node {
        Node::And(children) => {
            let key = if inverted {
                "clausesSatisfied"
            } else {
                "clausesNotSatisfied"
            };
            let items = indexed(children.iter().map(|c| explain_node(c, doc, inverted)));
            doc! {"operatorName": "$and", key: items}
        }
        Node::Or(children) => {
            let key = if inverted {
                "clausesSatisfied"
            } else {
                "clausesNotSatisfied"
            };
            let items = indexed(children.iter().map(|c| explain_node(c, doc, inverted)));
            doc! {"operatorName": "$or", key: items}
        }
        Node::Nor(children) => {
            let key = if inverted {
                "clausesNotSatisfied"
            } else {
                "clausesSatisfied"
            };
            let items = indexed(children.iter().map(|c| explain_node(c, doc, !inverted)));
            doc! {"operatorName": "$nor", key: items}
        }
        Node::Not { inner, .. } => {
            let mut d = doc! {"operatorName": "$not"};
            if let Some(child) = explain_node(inner, doc, !inverted) {
                d.insert("details", child);
            }
            d
        }
        Node::Leaf {
            field, op, spec, ..
        } => leaf_details(doc, field, op, spec, inverted),
        Node::Expr(e) => {
            let result = secantus_core::expressions::evaluate(doc, e, &Document::new())
                .unwrap_or(Bson::Null);
            let reason = if inverted {
                "expression did match"
            } else {
                "expression did not match"
            };
            doc! {
                "operatorName": "$expr",
                "specifiedAs": {"$expr": e.clone()},
                "reason": reason,
                "expressionResult": result,
            }
        }
        Node::JsonSchema(schema) => explain_json_schema(schema, doc),
    })
}

fn indexed(items: impl Iterator<Item = Option<Document>>) -> Vec<Bson> {
    items
        .enumerate()
        .filter_map(|(i, d)| d.map(|d| Bson::Document(doc! {"index": i as i32, "details": d})))
        .collect()
}

/// `(normal, inverted)` reasons, from `doc_validation_error.cpp`.
fn leaf_reasons(op: &str) -> (&'static str, &'static str) {
    match op {
        "$in" => (
            "no matching value found in array",
            "matching value found in array",
        ),
        "$nin" => (
            "matching value found in array",
            "no matching value found in array",
        ),
        "$ne" => ("comparison succeeded", "comparison failed"),
        "$regex" => (
            "regular expression did not match",
            "regular expression did match",
        ),
        "$size" => (
            "array length was not equal to given size",
            "array length was equal to given size",
        ),
        "$all" => (
            "array did not contain all specified values",
            "array did contain all specified values",
        ),
        "$mod" => (
            "$mod did not evaluate to expected remainder",
            "$mod did evaluate to expected remainder",
        ),
        "$elemMatch" => (
            "array did not satisfy the child predicate",
            "array did satisfy the child predicate",
        ),
        "$type" => ("type did not match", "type did match"),
        "$bitsAllSet" | "$bitsAllClear" | "$bitsAnySet" | "$bitsAnyClear" => (
            "bitwise operator failed to match",
            "bitwise operator matched successfully",
        ),
        _ => ("comparison failed", "comparison succeeded"),
    }
}

fn leaf_details(
    doc: &Document,
    field: &str,
    op: &str,
    spec: &Document,
    inverted: bool,
) -> Document {
    let mut d = doc! {"operatorName": op, "specifiedAs": spec.clone()};
    if op == "$exists" {
        let wants = spec
            .get_document(field)
            .ok()
            .and_then(|o| o.get("$exists"))
            .is_some_and(|v| {
                !matches!(v, Bson::Boolean(false) | Bson::Null) && v.as_i32() != Some(0)
            });
        // `$exists: true` fails on a missing path; `false` on a present one.
        let reason = match (wants, inverted) {
            (true, false) | (false, true) => "path does not exist",
            _ => "path does exist",
        };
        d.insert("reason", reason);
        return d;
    }
    // The array-shaped operators consider the array itself; the rest the
    // elements of an array they traverse.
    let whole = matches!(op, "$size" | "$all" | "$elemMatch");
    let Some(values) = path_values(doc, field, !whole) else {
        d.insert("reason", "field was missing");
        return d;
    };
    let (normal, inv) = leaf_reasons(op);
    d.insert("reason", if inverted { inv } else { normal });
    append_considered(&mut d, &values);
    if op == "$type" {
        append_considered_types(&mut d, &values);
    }
    d
}

fn append_considered(d: &mut Document, values: &[Bson]) {
    if values.len() == 1 {
        d.insert("consideredValue", values[0].clone());
    } else {
        d.insert("consideredValues", values.to_vec());
    }
}

fn append_considered_types(d: &mut Document, values: &[Bson]) {
    let mut types: Vec<&str> = values.iter().map(bson_type_name).collect();
    types.sort_unstable();
    types.dedup();
    match types.len() {
        0 => {}
        1 => {
            d.insert("consideredType", types[0]);
        }
        _ => {
            d.insert("consideredTypes", types);
        }
    }
}

/// The values a leaf considers at `path`, or `None` when the path does not
/// exist. With `traverse`, an array at the end of the path contributes its
/// ELEMENTS (mongod's `kTraverseOmitArray`); arrays along the way are walked
/// into their document elements either way.
fn path_values(doc: &Document, path: &str, traverse: bool) -> Option<Vec<Bson>> {
    fn walk(v: &Bson, parts: &[&str], traverse: bool, out: &mut Vec<Bson>, found: &mut bool) {
        if parts.is_empty() {
            *found = true;
            match v {
                Bson::Array(a) if traverse => out.extend(a.iter().cloned()),
                other => out.push(other.clone()),
            }
            return;
        }
        match v {
            Bson::Document(d) => {
                if let Some(next) = d.get(parts[0]) {
                    walk(next, &parts[1..], traverse, out, found);
                }
            }
            Bson::Array(a) => {
                for e in a {
                    if matches!(e, Bson::Document(_)) {
                        walk(e, parts, traverse, out, found);
                    }
                }
            }
            _ => {}
        }
    }
    let parts: Vec<&str> = path.split('.').collect();
    let mut out = Vec::new();
    let mut found = false;
    walk(
        &Bson::Document(doc.clone()),
        &parts,
        traverse,
        &mut out,
        &mut found,
    );
    found.then_some(out)
}

// --- $jsonSchema ------------------------------------------------------------

fn explain_json_schema(schema: &Document, doc: &Document) -> Document {
    // A top-level type that excludes "object" matches nothing at all; mongod
    // reports the whole schema rather than a rule.
    let top_type = schema
        .get("bsonType")
        .map(|t| ("bsonType", t))
        .or_else(|| schema.get("type").map(|t| ("type", t)));
    if let Some((kw, t)) = top_type {
        if !type_matches(kw, t, &Bson::Document(doc.clone())) {
            return doc! {
                "operatorName": "$jsonSchema",
                "specifiedAs": {"$jsonSchema": schema.clone()},
                "reason": "expression always evaluates to false",
            };
        }
    }
    let rules = explain_value(schema, &Bson::Document(doc.clone()), true);
    doc! {"operatorName": "$jsonSchema", "schemaRulesNotSatisfied": rules}
}

fn schema_matches(schema: &Document, value: &Bson, top: bool) -> bool {
    explain_value(schema, value, top).is_empty()
}

fn as_f64(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(d) => Some(*d),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        _ => None,
    }
}

fn is_number(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
    )
}

fn as_count(v: Option<&Bson>) -> Option<i64> {
    match v? {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        Bson::Double(d) if d.fract() == 0.0 => Some(*d as i64),
        _ => None,
    }
}

/// Whether `value` has the type a `type` / `bsonType` keyword names.
fn type_matches(kw: &str, spec: &Bson, value: &Bson) -> bool {
    let names: Vec<&str> = match spec {
        Bson::String(s) => vec![s.as_str()],
        Bson::Array(a) => a.iter().filter_map(Bson::as_str).collect(),
        _ => return true,
    };
    names.iter().any(|n| {
        let alias = if kw == "type" {
            match *n {
                "boolean" => "bool",
                other => other,
            }
        } else {
            n
        };
        filter_matches(&doc! {"v": value.clone()}, &doc! {"v": {"$type": alias}})
            && !(matches!(value, Bson::Array(_)) && alias != "array")
    })
}

fn bson_equal(a: &Bson, b: &Bson) -> bool {
    secantus_core::order::is_comparable(a)
        && secantus_core::order::is_comparable(b)
        && secantus_core::order::cmp(a, b) == std::cmp::Ordering::Equal
        && std::mem::discriminant(a) == std::mem::discriminant(b)
        || (is_number(a) && is_number(b) && as_f64(a) == as_f64(b))
}

/// Every rule of `schema` that `value` breaks, in mongod's order. `top` is the
/// root schema, which has no path and so no type restriction of its own.
fn explain_value(schema: &Document, value: &Bson, top: bool) -> Vec<Bson> {
    let mut out: Vec<Bson> = Vec::new();
    let mut push = |d: Document| out.push(Bson::Document(d));

    // Scalar keywords -- strings and numbers only.
    if let Bson::String(s) = value {
        if let Some(Bson::String(p)) = schema.get("pattern") {
            if !filter_matches(
                &doc! {"v": value.clone()},
                &doc! {"v": {"$regex": p.clone()}},
            ) {
                push(doc! {
                    "operatorName": "pattern",
                    "specifiedAs": {"pattern": p.clone()},
                    "reason": "regular expression did not match",
                    "consideredValue": value.clone(),
                });
            }
        }
        let len = s.chars().count() as i64;
        for (kw, fails) in [("maxLength", true), ("minLength", false)] {
            if let Some(n) = as_count(schema.get(kw)) {
                if (fails && len > n) || (!fails && len < n) {
                    push(doc! {
                        "operatorName": kw,
                        "specifiedAs": {kw: schema.get(kw).unwrap().clone()},
                        "reason": "specified string length was not satisfied",
                        "consideredValue": value.clone(),
                    });
                }
            }
        }
    }
    if is_number(value) {
        if let (Some(m), Some(v)) = (schema.get("multipleOf").and_then(as_f64), as_f64(value)) {
            if m != 0.0 && (v / m).fract() != 0.0 {
                push(doc! {
                    "operatorName": "multipleOf",
                    "specifiedAs": {"multipleOf": schema.get("multipleOf").unwrap().clone()},
                    "reason": "considered value is not a multiple of the specified value",
                    "consideredValue": value.clone(),
                });
            }
        }
        for (kw, excl, op, op_excl) in [
            ("maximum", "exclusiveMaximum", "$lte", "$lt"),
            ("minimum", "exclusiveMinimum", "$gte", "$gt"),
        ] {
            let Some(bound) = schema.get(kw) else {
                continue;
            };
            let exclusive = schema.get_bool(excl).unwrap_or(false);
            let cmp_op = if exclusive { op_excl } else { op };
            if !filter_matches(
                &doc! {"v": value.clone()},
                &doc! {"v": {cmp_op: bound.clone()}},
            ) {
                let mut spec = doc! {kw: bound.clone()};
                if let Some(e) = schema.get(excl) {
                    spec.insert(excl, e.clone());
                }
                push(doc! {
                    "operatorName": kw,
                    "specifiedAs": spec,
                    "reason": "comparison failed",
                    "consideredValue": value.clone(),
                });
            }
        }
    }

    // Array keywords.
    if let Bson::Array(items) = value {
        let n = items.len() as i64;
        for (kw, too_many) in [("minItems", false), ("maxItems", true)] {
            if let Some(limit) = as_count(schema.get(kw)) {
                if (too_many && n > limit) || (!too_many && n < limit) {
                    push(doc! {
                        "operatorName": kw,
                        "specifiedAs": {kw: schema.get(kw).unwrap().clone()},
                        "reason": "array did not match specified length",
                        "consideredValue": value.clone(),
                        "numberOfItems": n as i32,
                    });
                }
            }
        }
        if schema.get_bool("uniqueItems").unwrap_or(false) {
            let dup = items.iter().enumerate().find_map(|(i, a)| {
                items[..i]
                    .iter()
                    .any(|b| bson_equal(a, b))
                    .then(|| a.clone())
            });
            if let Some(dup) = dup {
                push(doc! {
                    "operatorName": "uniqueItems",
                    "specifiedAs": {"uniqueItems": true},
                    "reason": "found a duplicate item",
                    "consideredValue": value.clone(),
                    "duplicatedValue": dup,
                });
            }
        }
        match schema.get("items") {
            Some(Bson::Document(item_schema)) => {
                if let Some((i, details)) = items.iter().enumerate().find_map(|(i, it)| {
                    let d = explain_value(item_schema, it, false);
                    (!d.is_empty()).then_some((i, d))
                }) {
                    push(doc! {
                        "operatorName": "items",
                        "reason": "At least one item did not match the sub-schema",
                        "itemIndex": i as i32,
                        "details": details,
                    });
                }
            }
            Some(Bson::Array(schemas)) => {
                let failing: Vec<Bson> = schemas
                    .iter()
                    .zip(items.iter())
                    .enumerate()
                    .filter_map(|(i, (s, it))| {
                        let s = s.as_document()?;
                        let d = explain_value(s, it, false);
                        (!d.is_empty())
                            .then(|| Bson::Document(doc! {"itemIndex": i as i32, "details": d}))
                    })
                    .collect();
                if !failing.is_empty() {
                    push(doc! {"operatorName": "items", "details": failing});
                }
                if let Some(extra) = items.get(schemas.len()..).filter(|e| !e.is_empty()) {
                    match schema.get("additionalItems") {
                        Some(Bson::Boolean(false)) => push(doc! {
                            "operatorName": "additionalItems",
                            "specifiedAs": {"additionalItems": false},
                            "reason": "found additional items",
                            "additionalItems": extra.to_vec(),
                        }),
                        Some(Bson::Document(add)) => {
                            if let Some((i, details)) =
                                extra.iter().enumerate().find_map(|(i, it)| {
                                    let d = explain_value(add, it, false);
                                    (!d.is_empty()).then_some((i + schemas.len(), d))
                                })
                            {
                                push(doc! {
                                    "operatorName": "additionalItems",
                                    "reason": "At least one additional item did not match the sub-schema",
                                    "itemIndex": i as i32,
                                    "details": details,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Object keywords.
    if let Bson::Document(obj) = value {
        explain_object(schema, obj, &mut push);
    }

    // Logical keywords, then `enum`.
    let sub_schemas = |kw: &str| -> Vec<Document> {
        schema
            .get_array(kw)
            .map(|a| a.iter().filter_map(Bson::as_document).cloned().collect())
            .unwrap_or_default()
    };
    let all_of = sub_schemas("allOf");
    if !all_of.is_empty() {
        let failing = indexed_schemas(&all_of, value, top, |d| !d.is_empty());
        if !failing.is_empty() {
            push(doc! {"operatorName": "allOf", "schemasNotSatisfied": failing});
        }
    }
    let any_of = sub_schemas("anyOf");
    if !any_of.is_empty() && !any_of.iter().any(|s| schema_matches(s, value, top)) {
        let all = indexed_schemas(&any_of, value, top, |_| true);
        push(doc! {"operatorName": "anyOf", "schemasNotSatisfied": all});
    }
    let one_of = sub_schemas("oneOf");
    if !one_of.is_empty() {
        let matching: Vec<i32> = one_of
            .iter()
            .enumerate()
            .filter(|(_, s)| schema_matches(s, value, top))
            .map(|(i, _)| i as i32)
            .collect();
        match matching.len() {
            0 => {
                let all = indexed_schemas(&one_of, value, top, |_| true);
                push(doc! {"operatorName": "oneOf", "schemasNotSatisfied": all});
            }
            1 => {}
            _ => push(doc! {
                "operatorName": "oneOf",
                "reason": "more than one subschema matched",
                "matchingSchemaIndexes": matching,
            }),
        }
    }
    if let Ok(not) = schema.get_document("not") {
        if schema_matches(not, value, top) {
            push(doc! {"operatorName": "not", "reason": "child expression matched"});
        }
    }
    if let Ok(options) = schema.get_array("enum") {
        if !options.iter().any(|o| bson_equal(o, value)) {
            push(doc! {
                "operatorName": "enum",
                "specifiedAs": {"enum": options.clone()},
                "reason": "value was not found in enum",
                "consideredValue": value.clone(),
            });
        }
    }

    // The type restriction, last -- and only below the root.
    if !top {
        for kw in ["bsonType", "type"] {
            if let Some(t) = schema.get(kw) {
                if !type_matches(kw, t, value) {
                    push(doc! {
                        "operatorName": kw,
                        "specifiedAs": {kw: t.clone()},
                        "reason": "type did not match",
                        "consideredValue": value.clone(),
                        "consideredType": bson_type_name(value),
                    });
                }
            }
        }
    }
    out
}

fn indexed_schemas(
    schemas: &[Document],
    value: &Bson,
    top: bool,
    keep: impl Fn(&Vec<Bson>) -> bool,
) -> Vec<Bson> {
    schemas
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let d = explain_value(s, value, top);
            keep(&d).then(|| Bson::Document(doc! {"index": i as i32, "details": d}))
        })
        .collect()
}

fn explain_object(schema: &Document, obj: &Document, push: &mut impl FnMut(Document)) {
    let properties = schema.get_document("properties").ok();
    if let Some(props) = properties {
        let failing: Vec<Bson> = props
            .iter()
            .filter_map(|(name, sub)| {
                let sub = sub.as_document()?;
                let v = obj.get(name)?;
                let d = explain_value(sub, v, false);
                (!d.is_empty()).then(|| Bson::Document(doc! {"propertyName": name, "details": d}))
            })
            .collect();
        if !failing.is_empty() {
            push(doc! {"operatorName": "properties", "propertiesNotSatisfied": failing});
        }
    }
    let patterns = schema.get_document("patternProperties").ok();
    if let Some(patterns) = patterns {
        let mut failing = Vec::new();
        for (name, v) in obj {
            for (pattern, sub) in patterns {
                let Some(sub) = sub.as_document() else {
                    continue;
                };
                let hit = filter_matches(
                    &doc! {"k": name.clone()},
                    &doc! {"k": {"$regex": pattern.clone()}},
                );
                if hit {
                    let d = explain_value(sub, v, false);
                    if !d.is_empty() {
                        failing.push(Bson::Document(doc! {
                            "propertyName": name,
                            "regexMatched": pattern.clone(),
                            "details": d,
                        }));
                    }
                }
            }
        }
        if !failing.is_empty() {
            push(doc! {"operatorName": "patternProperties", "details": failing});
        }
    }
    if let Some(additional) = schema.get("additionalProperties") {
        let is_additional = |name: &str| {
            properties.is_none_or(|p| !p.contains_key(name))
                && patterns.is_none_or(|p| {
                    !p.keys().any(|pat| {
                        filter_matches(&doc! {"k": name}, &doc! {"k": {"$regex": pat.clone()}})
                    })
                })
        };
        match additional {
            Bson::Boolean(false) => {
                let extra: Vec<&String> = obj.keys().filter(|k| is_additional(k)).collect();
                if !extra.is_empty() {
                    push(doc! {
                        "operatorName": "additionalProperties",
                        "specifiedAs": {"additionalProperties": false},
                        "additionalProperties": extra.into_iter().cloned().collect::<Vec<_>>(),
                    });
                }
            }
            Bson::Document(sub) => {
                if let Some((name, d)) =
                    obj.iter()
                        .filter(|(k, _)| is_additional(k))
                        .find_map(|(k, v)| {
                            let d = explain_value(sub, v, false);
                            (!d.is_empty()).then(|| (k.clone(), d))
                        })
                {
                    push(doc! {
                        "operatorName": "additionalProperties",
                        "reason": "at least one additional property did not match the subschema",
                        "failingProperty": name,
                        "details": d,
                    });
                }
            }
            _ => {}
        }
    }
    if let Ok(required) = schema.get_array("required") {
        let missing: Vec<Bson> = required
            .iter()
            .filter(|r| r.as_str().is_some_and(|n| !obj.contains_key(n)))
            .cloned()
            .collect();
        if !missing.is_empty() {
            push(doc! {
                "operatorName": "required",
                "specifiedAs": {"required": required.clone()},
                "missingProperties": missing,
            });
        }
    }
    let n = obj.len() as i64;
    for (kw, too_many) in [("minProperties", false), ("maxProperties", true)] {
        if let Some(limit) = as_count(schema.get(kw)) {
            if (too_many && n > limit) || (!too_many && n < limit) {
                push(doc! {
                    "operatorName": kw,
                    "specifiedAs": {kw: schema.get(kw).unwrap().clone()},
                    "reason": "specified number of properties was not satisfied",
                    "numberOfProperties": n as i32,
                });
            }
        }
    }
    if let Ok(deps) = schema.get_document("dependencies") {
        let mut failing = Vec::new();
        for (prop, dep) in deps {
            if !obj.contains_key(prop) {
                continue;
            }
            match dep {
                Bson::Array(names) => {
                    let missing: Vec<Bson> = names
                        .iter()
                        .filter(|n| n.as_str().is_some_and(|n| !obj.contains_key(n)))
                        .cloned()
                        .collect();
                    if !missing.is_empty() {
                        failing.push(Bson::Document(doc! {
                            "conditionalProperty": prop,
                            "missingProperties": missing,
                        }));
                    }
                }
                Bson::Document(sub) => {
                    let d = explain_value(sub, &Bson::Document(obj.clone()), true);
                    if !d.is_empty() {
                        failing.push(Bson::Document(
                            doc! {"conditionalProperty": prop, "details": d},
                        ));
                    }
                }
                _ => {}
            }
        }
        if !failing.is_empty() {
            push(doc! {"operatorName": "dependencies", "failingDependencies": failing});
        }
    }
}

/// Expected values are mongod 8.2.11's, measured 2026-09-30.
#[cfg(test)]
mod tests {
    use super::explain;
    use bson::{bson, doc};

    #[test]
    fn schema_rules_come_in_mongods_order_not_the_schemas() {
        let v =
            doc! {"$jsonSchema": {"required": ["a"], "properties": {"b": {"bsonType": "string"}}}};
        assert_eq!(
            explain(&v, &doc! {"_id": 1, "b": 3}),
            doc! {"operatorName": "$jsonSchema", "schemaRulesNotSatisfied": [
                {"operatorName": "properties", "propertiesNotSatisfied": [
                    {"propertyName": "b", "details": [{
                        "operatorName": "bsonType", "specifiedAs": {"bsonType": "string"},
                        "reason": "type did not match", "consideredValue": 3, "consideredType": "int",
                    }]},
                ]},
                {"operatorName": "required", "specifiedAs": {"required": ["a"]}, "missingProperties": ["a"]},
            ]}
        );
    }

    #[test]
    fn a_keyword_restricts_only_its_own_type() {
        let v = doc! {"$jsonSchema": {"properties": {"a": {"bsonType": "int", "minimum": 5}}}};
        let d = explain(&v, &doc! {"_id": 2, "a": "s"});
        let rules = d.get_array("schemaRulesNotSatisfied").unwrap();
        let detail = rules[0]
            .as_document()
            .unwrap()
            .get_array("propertiesNotSatisfied")
            .unwrap()[0]
            .as_document()
            .unwrap()
            .get_array("details")
            .unwrap()
            .clone();
        assert_eq!(
            detail.len(),
            1,
            "minimum does not apply to a string: {detail:?}"
        );
    }

    #[test]
    fn items_report_the_first_failing_index() {
        let v = doc! {"$jsonSchema": {"properties": {"a": {"items": {"bsonType": "int"}}}}};
        let d = explain(&v, &doc! {"_id": 1, "a": [1, "x", 2]});
        let item = &d.get_array("schemaRulesNotSatisfied").unwrap()[0]
            .as_document()
            .unwrap()
            .get_array("propertiesNotSatisfied")
            .unwrap()[0];
        assert_eq!(
            item.as_document().unwrap().get_array("details").unwrap()[0],
            bson!({
                "operatorName": "items",
                "reason": "At least one item did not match the sub-schema",
                "itemIndex": 1,
                "details": [{"operatorName": "bsonType", "specifiedAs": {"bsonType": "int"},
                    "reason": "type did not match", "consideredValue": "x", "consideredType": "string"}],
            })
        );
    }

    #[test]
    fn query_clauses_flatten_per_operator_under_one_and() {
        assert_eq!(
            explain(&doc! {"a": {"$gt": 1, "$lt": 5}}, &doc! {"_id": 1, "a": 9}),
            doc! {"operatorName": "$and", "clausesNotSatisfied": [{"index": 1, "details": {
                "operatorName": "$lt", "specifiedAs": {"a": {"$lt": 5}},
                "reason": "comparison failed", "consideredValue": 9,
            }}]}
        );
        assert_eq!(
            explain(&doc! {"a": 5}, &doc! {"_id": 2}),
            doc! {"operatorName": "$eq", "specifiedAs": {"a": 5}, "reason": "field was missing"}
        );
    }

    #[test]
    fn nor_and_not_report_what_matched() {
        assert_eq!(
            explain(&doc! {"$nor": [{"a": 1}]}, &doc! {"_id": 1, "a": 1}),
            doc! {"operatorName": "$nor", "clausesSatisfied": [{"index": 0, "details": {
                "operatorName": "$eq", "specifiedAs": {"a": 1},
                "reason": "comparison succeeded", "consideredValue": 1,
            }}]}
        );
        assert_eq!(
            explain(&doc! {"a": {"$not": {"$gt": 5}}}, &doc! {"_id": 1, "a": 9}),
            doc! {"operatorName": "$not", "details": {
                "operatorName": "$gt", "specifiedAs": {"a": {"$gt": 5}},
                "reason": "comparison succeeded", "consideredValue": 9,
            }}
        );
    }
}
