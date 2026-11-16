//! `$jsonSchema` in a query shape: the match expression tree mongod's
//! `JSONSchemaParser` builds from the schema (`json_schema_parser.cpp`,
//! 8.2.11), serialized as it stands -- unoptimized, so every `$and` the
//! parser makes is kept, and each restriction keeps its
//! `{$or: [{$not: <type>}, <restriction>]}` in the parser's order.
//!
//! The `explain` translation (`explain/json_schema.rs`) prints the
//! optimized tree instead; this module follows the parser function by
//! function. A schema mongod refuses answers `None` (mongod parses no
//! shape for it), as does the `encrypt` family, which a query may not use.
//! Checked against mongod 8.2.11's `$queryStats` shapes, 2026-11-16
//! (`tests/mongod_live/test_query_shape_hash.py`'s `JSON_SCHEMAS`).

use bson::{doc, Bson, Document};

use super::{field_path, lit, number, scalar_rep, serialize, Node};

/// The placeholder mongod names an array element or property by.
const PLACEHOLDER: &str = "i";

/// The node `{$jsonSchema: schema}` stands for.
pub(super) fn parse(schema: &Document) -> Option<Node> {
    parse_at(None, schema)
}

/// A `MatcherTypeSet`: `allNumbers` and the BSON type codes.
#[derive(Clone)]
struct Types {
    numbers: bool,
    codes: Vec<i32>,
}

impl Types {
    fn single(code: i32) -> Self {
        Types {
            numbers: false,
            codes: vec![code],
        }
    }

    fn all_numbers() -> Self {
        Types {
            numbers: true,
            codes: vec![],
        }
    }

    fn is_single(&self) -> bool {
        (self.numbers && self.codes.is_empty()) || (!self.numbers && self.codes.len() == 1)
    }

    fn has(&self, code: i32) -> bool {
        self.codes.contains(&code) || (self.numbers && matches!(code, 1 | 16 | 18 | 19))
    }

    /// As `$_internalSchemaType` prints it: `"number"`, then the codes in
    /// order. Not a literal: the set is part of the shape.
    fn print(&self) -> Bson {
        let mut codes = self.codes.clone();
        codes.sort_unstable();
        let mut out = Vec::new();
        if self.numbers {
            out.push(Bson::String("number".into()));
        }
        out.extend(codes.into_iter().map(Bson::Int32));
        Bson::Array(out)
    }
}

fn leaf(path: &str, op: &str, value: Bson) -> Node {
    Node::Leaf {
        path: path.to_string(),
        rhs: doc! { op: value },
    }
}

fn always_true() -> Node {
    Node::Raw(doc! { "$alwaysTrue": lit(Bson::Int32(1)) })
}

fn always_false() -> Node {
    Node::Raw(doc! { "$alwaysFalse": lit(Bson::Int32(1)) })
}

fn type_node(path: &str, types: &Types) -> Node {
    leaf(path, "$_internalSchemaType", types.print())
}

fn exists(path: &str) -> Node {
    leaf(path, "$exists", lit(Bson::Boolean(true)))
}

fn object_match(path: &str, sub: &Node) -> Node {
    leaf(
        path,
        "$_internalSchemaObjectMatch",
        Bson::Document(serialize(sub, true)),
    )
}

/// `makeRestriction`: `restriction` holds only where the value has the
/// type it restricts.
fn restriction(restricts: &Types, path: &str, expr: Node, stated: Option<&Types>) -> Node {
    if let Some(stated) = stated.filter(|t| t.is_single()) {
        // `NumberInt` stands in for "number".
        let code = if stated.numbers { 16 } else { stated.codes[0] };
        return if restricts.has(code) {
            expr
        } else {
            always_true()
        };
    }
    Node::Or(vec![Node::Not(Box::new(type_node(path, restricts))), expr])
}

/// `parseIntegerElementToNonNegativeLong`.
fn non_negative_integer(v: &Bson) -> Option<()> {
    let n = number(v)?;
    (n >= 0.0 && n.fract() == 0.0).then_some(())
}

fn type_set(v: &Bson, json_type: bool) -> Option<Types> {
    let names: Vec<&str> = match v {
        Bson::String(s) => vec![s.as_str()],
        Bson::Array(items) => items.iter().map(|i| i.as_str()).collect::<Option<_>>()?,
        _ => return None,
    };
    let mut types = Types {
        numbers: false,
        codes: vec![],
    };
    let mut seen: Vec<&str> = Vec::new();
    for name in names {
        if name == "integer" || seen.contains(&name) {
            return None;
        }
        seen.push(name);
        if name == "number" {
            types.numbers = true;
            continue;
        }
        let code = if json_type {
            match name {
                "array" => 4,
                "boolean" => 8,
                "null" => 10,
                "object" => 3,
                "string" => 2,
                _ => return None,
            }
        } else {
            super::type_code(name)?
        };
        if !types.codes.contains(&code) {
            types.codes.push(code);
        }
    }
    (types.numbers || !types.codes.is_empty()).then_some(types)
}

/// An `$_internalSchemaEq` / `$_internalSchemaRootDocEq` value: each
/// scalar inside it a literal, objects and arrays kept as they stand.
fn deep(v: &Bson) -> Option<Bson> {
    Some(match v {
        Bson::Document(d) => Bson::Document(deep_doc(d)?),
        Bson::Array(items) => Bson::Array(items.iter().map(deep).collect::<Option<_>>()?),
        other => lit(scalar_rep(other)?),
    })
}

fn deep_doc(d: &Document) -> Option<Document> {
    d.iter().map(|(k, v)| Some((k.clone(), deep(v)?))).collect()
}

/// Whether `node` names the placeholder. The representative shape always
/// prints it; mongod prints the debug shape by re-parsing that, and the
/// re-parse (`ExpressionWithPlaceholder::make`) finds the placeholder in
/// the expression's paths, so an expression that names none prints `""`.
fn names_placeholder(node: &Node) -> bool {
    match node {
        Node::Leaf { path, .. } | Node::ElemValue { path, .. } | Node::ElemObject { path, .. } => {
            path == PLACEHOLDER || path.starts_with("i.")
        }
        Node::And(kids) | Node::Or(kids) | Node::Nor(kids) => kids.iter().any(names_placeholder),
        Node::Not(child) => names_placeholder(child),
        Node::Raw(_) => false,
    }
}

/// `_parse`: the schema at `path` (`None` at the top level).
fn parse_at(path: Option<&str>, schema: &Document) -> Option<Node> {
    const KEYWORDS: &[&str] = &[
        "additionalItems",
        "additionalProperties",
        "allOf",
        "anyOf",
        "bsonType",
        "dependencies",
        "description",
        "enum",
        "exclusiveMaximum",
        "exclusiveMinimum",
        "items",
        "maxItems",
        "maxLength",
        "maxProperties",
        "maximum",
        "minItems",
        "minLength",
        "minProperties",
        "minimum",
        "multipleOf",
        "not",
        "oneOf",
        "pattern",
        "patternProperties",
        "properties",
        "required",
        "title",
        "type",
        "uniqueItems",
    ];
    // Unknown keywords, `$ref` and the other unsupported ones, and the
    // `encrypt` keywords (not allowed in a query) are all refused.
    if schema.keys().any(|k| !KEYWORDS.contains(&k.as_str())) {
        return None;
    }
    for meta in ["description", "title"] {
        if schema
            .get(meta)
            .is_some_and(|v| !matches!(v, Bson::String(_)))
        {
            return None;
        }
    }
    let stated = match (schema.get("type"), schema.get("bsonType")) {
        (Some(_), Some(_)) => return None,
        (Some(t), None) => Some(type_set(t, true)?),
        (None, Some(t)) => Some(type_set(t, false)?),
        (None, None) => None,
    };
    let stated = stated.as_ref();
    let mut and = Vec::new();
    scalar_keywords(schema, path, stated, &mut and)?;
    array_keywords(schema, path, stated, &mut and)?;
    object_keywords(schema, path, stated, &mut and)?;
    logical_keywords(schema, path, &mut and)?;
    match (path, stated) {
        (None, Some(t)) if !t.has(3) => return Some(always_false()),
        (Some(p), Some(t)) => and.push(type_node(p, t)),
        _ => {}
    }
    Some(Node::And(and))
}

fn scalar_keywords(
    schema: &Document,
    path: Option<&str>,
    stated: Option<&Types>,
    and: &mut Vec<Node>,
) -> Option<()> {
    if let Some(v) = schema.get("pattern") {
        v.as_str()?;
        and.push(match path {
            None => always_true(),
            Some(p) => restriction(
                &Types::single(2),
                p,
                leaf(p, "$regex", lit(Bson::String("\\?".into()))),
                stated,
            ),
        });
    }
    for (keyword, op) in [
        ("maxLength", "$_internalSchemaMaxLength"),
        ("minLength", "$_internalSchemaMinLength"),
    ] {
        if let Some(v) = schema.get(keyword) {
            non_negative_integer(v)?;
            and.push(match path {
                None => always_true(),
                Some(p) => restriction(
                    &Types::single(2),
                    p,
                    leaf(p, op, lit(Bson::Int32(1))),
                    stated,
                ),
            });
        }
    }
    if let Some(v) = schema.get("multipleOf") {
        if number(v)? <= 0.0 {
            return None;
        }
        and.push(match path {
            None => always_true(),
            Some(p) => restriction(
                &Types::all_numbers(),
                p,
                leaf(
                    p,
                    "$_internalSchemaFmod",
                    Bson::Array(vec![lit(Bson::Int32(1)), lit(Bson::Int32(1))]),
                ),
                stated,
            ),
        });
    }
    for (bound, exclusive, inclusive_op, exclusive_op) in [
        ("maximum", "exclusiveMaximum", "$lte", "$lt"),
        ("minimum", "exclusiveMinimum", "$gte", "$gt"),
    ] {
        let excl = match schema.get(exclusive) {
            None => false,
            Some(Bson::Boolean(b)) => *b,
            Some(_) => return None,
        };
        let Some(v) = schema.get(bound) else {
            if schema.contains_key(exclusive) {
                return None;
            }
            continue;
        };
        number(v)?;
        and.push(match path {
            None => always_true(),
            Some(p) => {
                let op = if excl { exclusive_op } else { inclusive_op };
                restriction(
                    &Types::all_numbers(),
                    p,
                    leaf(p, op, lit(Bson::Int32(1))),
                    stated,
                )
            }
        });
    }
    Some(())
}

fn array_keywords(
    schema: &Document,
    path: Option<&str>,
    stated: Option<&Types>,
    and: &mut Vec<Node>,
) -> Option<()> {
    let array = Types::single(4);
    for (keyword, op) in [
        ("minItems", "$_internalSchemaMinItems"),
        ("maxItems", "$_internalSchemaMaxItems"),
    ] {
        if let Some(v) = schema.get(keyword) {
            non_negative_integer(v)?;
            and.push(match path {
                None => always_true(),
                Some(p) => restriction(&array, p, leaf(p, op, lit(Bson::Int32(1))), stated),
            });
        }
    }
    if let Some(v) = schema.get("uniqueItems") {
        let Bson::Boolean(unique) = v else {
            return None;
        };
        and.push(match path {
            Some(p) if *unique => restriction(
                &array,
                p,
                leaf(p, "$_internalSchemaUniqueItems", Bson::Boolean(true)),
                stated,
            ),
            _ => always_true(),
        });
    }
    let mut start_of_additional = None;
    if let Some(items) = schema.get("items") {
        match items {
            Bson::Array(subschemas) => {
                let mut indexed = Vec::new();
                for sub in subschemas {
                    let expr = parse_at(Some(PLACEHOLDER), sub.as_document()?)?;
                    let placeholder = if !super::debug() || names_placeholder(&expr) {
                        PLACEHOLDER
                    } else {
                        ""
                    };
                    if let Some(p) = path {
                        indexed.push(leaf(
                            p,
                            "$_internalSchemaMatchArrayIndex",
                            Bson::Document(doc! {
                                "index": lit(Bson::Int32(1)),
                                // `serializeFieldPathFromString`, where
                                // `$_internalSchemaAllowedProperties`
                                // appends its placeholder as it stands.
                                "namePlaceholder": field_path(placeholder),
                                "expression": serialize(&expr, true),
                            }),
                        ));
                    }
                }
                start_of_additional = Some(());
                and.push(match path {
                    None => always_true(),
                    Some(p) => restriction(&array, p, Node::And(indexed), stated),
                });
            }
            Bson::Document(sub) => {
                let expr = parse_at(Some(PLACEHOLDER), sub)?;
                and.push(match path {
                    None => always_true(),
                    Some(p) => restriction(&array, p, from_index(p, &expr), stated),
                });
            }
            _ => return None,
        }
    }
    if let Some(v) = schema.get("additionalItems") {
        let otherwise = match v {
            Bson::Boolean(true) => always_true(),
            Bson::Boolean(false) => always_false(),
            Bson::Document(sub) => parse_at(Some(PLACEHOLDER), sub)?,
            _ => return None,
        };
        if start_of_additional.is_some() {
            and.push(match path {
                None => always_true(),
                Some(p) => restriction(&array, p, from_index(p, &otherwise), stated),
            });
        }
    }
    Some(())
}

fn from_index(path: &str, expr: &Node) -> Node {
    leaf(
        path,
        "$_internalSchemaAllElemMatchFromIndex",
        Bson::Array(vec![
            lit(Bson::Int32(1)),
            Bson::Document(serialize(expr, true)),
        ]),
    )
}

/// The node for an object restriction: `expr` inside `path`, or `expr`
/// itself at the top level.
fn on_object(path: Option<&str>, expr: Node, stated: Option<&Types>) -> Node {
    match path {
        None => expr,
        Some(p) => restriction(&Types::single(3), p, object_match(p, &expr), stated),
    }
}

fn object_keywords(
    schema: &Document,
    path: Option<&str>,
    stated: Option<&Types>,
    and: &mut Vec<Node>,
) -> Option<()> {
    let mut required: Vec<&str> = Vec::new();
    if let Some(v) = schema.get("required") {
        for name in v.as_array()? {
            let name = name.as_str()?;
            if required.contains(&name) {
                return None;
            }
            required.push(name);
        }
        if required.is_empty() {
            return None;
        }
    }
    let properties = match schema.get("properties") {
        None => None,
        Some(v) => Some(v.as_document()?),
    };
    if let Some(props) = properties {
        let mut kids = Vec::new();
        for (name, sub) in props {
            let nested = parse_at(Some(name), sub.as_document()?)?;
            kids.push(if required.contains(&name.as_str()) {
                nested
            } else {
                Node::Or(vec![Node::Not(Box::new(exists(name))), nested])
            });
        }
        and.push(on_object(path, Node::And(kids), stated));
    }
    let pattern = schema.get("patternProperties");
    let additional = schema.get("additionalProperties");
    if pattern.is_some() || additional.is_some() {
        let mut names: Vec<Bson> = properties
            .map(|p| p.keys().map(|k| Bson::String(k.clone())).collect())
            .unwrap_or_default();
        names.dedup();
        let mut patterns = Vec::new();
        if let Some(pattern) = pattern {
            for (regex, sub) in pattern.as_document()? {
                let expr = parse_at(Some(PLACEHOLDER), sub.as_document()?)?;
                let raw = Bson::RegularExpression(bson::Regex {
                    pattern: regex.clone(),
                    options: String::new(),
                });
                patterns.push(Bson::Document(doc! {
                    "regex": lit(scalar_rep(&raw)?),
                    "expression": serialize(&expr, true),
                }));
            }
        }
        let otherwise = match additional {
            None | Some(Bson::Boolean(true)) => always_true(),
            Some(Bson::Boolean(false)) => always_false(),
            Some(Bson::Document(sub)) => parse_at(Some(PLACEHOLDER), sub)?,
            Some(_) => return None,
        };
        let allowed = Node::Raw(doc! { "$_internalSchemaAllowedProperties": {
            "properties": lit(super::representative(&Bson::Array(names))?),
            "namePlaceholder": PLACEHOLDER,
            "patternProperties": patterns,
            "otherwise": serialize(&otherwise, true),
        } });
        and.push(on_object(path, allowed, stated));
    }
    if !required.is_empty() {
        required.sort_unstable();
        let kids = required.iter().map(|name| exists(name)).collect();
        and.push(on_object(path, Node::And(kids), stated));
    }
    for (keyword, op) in [
        ("minProperties", "$_internalSchemaMinProperties"),
        ("maxProperties", "$_internalSchemaMaxProperties"),
    ] {
        if let Some(v) = schema.get(keyword) {
            non_negative_integer(v)?;
            let expr = Node::Raw(doc! { op: lit(Bson::Int32(1)) });
            and.push(on_object(path, expr, stated));
        }
    }
    if let Some(v) = schema.get("dependencies") {
        let mut kids = Vec::new();
        for (name, dependency) in v.as_document()? {
            let then = match dependency {
                Bson::Document(sub) => parse_at(path, sub)?,
                Bson::Array(names) => {
                    if names.is_empty() {
                        return None;
                    }
                    let mut seen: Vec<&str> = Vec::new();
                    let mut all = Vec::new();
                    for n in names {
                        let n = n.as_str()?;
                        if seen.contains(&n) {
                            return None;
                        }
                        seen.push(n);
                        all.push(dependency_exists(path, n));
                    }
                    Node::And(all)
                }
                Bson::String(_) if name == "title" || name == "description" => continue,
                _ => return None,
            };
            let cond = Bson::Array(vec![
                Bson::Document(serialize(&dependency_exists(path, name), true)),
                Bson::Document(serialize(&then, true)),
                Bson::Document(serialize(&always_true(), true)),
            ]);
            kids.push(Node::Raw(doc! { "$_internalSchemaCond": cond }));
        }
        and.push(Node::And(kids));
    }
    Some(())
}

/// `makeDependencyExistsClause`.
fn dependency_exists(path: Option<&str>, name: &str) -> Node {
    match path {
        None => exists(name),
        Some(p) => object_match(p, &exists(name)),
    }
}

fn logical_keywords(schema: &Document, path: Option<&str>, and: &mut Vec<Node>) -> Option<()> {
    let list = |v: &Bson| -> Option<Vec<Node>> {
        let items = v.as_array().filter(|a| !a.is_empty())?;
        items
            .iter()
            .map(|i| parse_at(path, i.as_document()?))
            .collect()
    };
    if let Some(v) = schema.get("allOf") {
        and.push(Node::And(list(v)?));
    }
    if let Some(v) = schema.get("anyOf") {
        and.push(Node::Or(list(v)?));
    }
    if let Some(v) = schema.get("oneOf") {
        let kids = list(v)?
            .iter()
            .map(|k| Bson::Document(serialize(k, true)))
            .collect::<Vec<_>>();
        and.push(Node::Raw(doc! { "$_internalSchemaXor": kids }));
    }
    if let Some(v) = schema.get("not") {
        and.push(Node::Not(Box::new(parse_at(path, v.as_document()?)?)));
    }
    if let Some(v) = schema.get("enum") {
        let values = v.as_array().filter(|a| !a.is_empty())?;
        for (i, a) in values.iter().enumerate() {
            if values[..i].contains(a) {
                return None;
            }
        }
        let mut kids = Vec::new();
        for value in values {
            match path {
                None => {
                    if let Bson::Document(d) = value {
                        kids.push(Node::Raw(
                            doc! { "$_internalSchemaRootDocEq": deep_doc(d)? },
                        ));
                    }
                }
                Some(p) => {
                    // An array is printed as the object BSON stores it as.
                    let rhs = match value {
                        Bson::Array(items) => Bson::Document(
                            items
                                .iter()
                                .enumerate()
                                .map(|(i, v)| Some((i.to_string(), deep(v)?)))
                                .collect::<Option<_>>()?,
                        ),
                        other => deep(other)?,
                    };
                    kids.push(leaf(p, "$_internalSchemaEq", rhs));
                }
            }
        }
        and.push(if kids.is_empty() {
            always_false()
        } else {
            Node::Or(kids)
        });
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::super::filter_shape;
    use bson::doc;

    #[test]
    fn a_required_property_keeps_every_and_the_parser_made() {
        // mongod 8.2.11's `$queryStats` shape, re-parsed from its debug
        // form (2026-11-16).
        let schema = doc! { "required": ["x"], "properties": { "x": { "bsonType": "int" } } };
        assert_eq!(
            filter_shape(&doc! { "$jsonSchema": schema }),
            Some(doc! { "$and": [
                { "$and": [{ "$and": [{ "x": { "$_internalSchemaType": [16] } }] }] },
                { "$and": [{ "x": { "$exists": true } }] },
            ] })
        );
    }

    #[test]
    fn a_top_level_type_other_than_object_matches_nothing() {
        assert_eq!(
            filter_shape(&doc! { "$jsonSchema": { "bsonType": "int" } }),
            Some(doc! { "$alwaysFalse": 1 })
        );
    }

    #[test]
    fn an_unknown_keyword_has_no_shape() {
        assert_eq!(filter_shape(&doc! { "$jsonSchema": { "$ref": "x" } }), None);
    }
}
