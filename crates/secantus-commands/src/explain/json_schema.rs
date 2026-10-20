//! `$jsonSchema` as mongod parses it: the match expressions each keyword
//! stands for, which `parsedQuery` and the planner's query tree print in
//! its place.
//!
//! Transcribed from what mongod 8.2.11 prints for 31 schemas (measured
//! 2026-10-20). A property's keywords hold only where they apply: a numeric
//! bound is `{$or: [<bound>, <not a number>]}`, a string bound the same with
//! the string type, and so on; and a property the schema does not require
//! is also satisfied when it is absent. `required` adds `$exists`, a nested
//! `properties` matches inside the object (`$_internalSchemaObjectMatch`),
//! and the combinators become `$or` / `$and` / `$_internalSchemaXor`.
//! A keyword not transcribed here leaves the schema echoed as written.

use bson::{doc, Bson, Document};

/// The filter `schema` stands for, or `None` when it uses a keyword this
/// transcription does not cover.
pub(crate) fn translate(schema: &Document) -> Option<Document> {
    Some(and_of(object_clauses(schema)?))
}

/// `{path: {op: value}}`.
fn on(path: &str, op: &str, value: Bson) -> Document {
    let mut inner = Document::new();
    inner.insert(op, value);
    let mut out = Document::new();
    out.insert(path, inner);
    out
}

fn and_of(clauses: Vec<Document>) -> Document {
    match clauses.len() {
        0 => Document::new(),
        1 => clauses.into_iter().next().expect("one"),
        _ => doc! { "$and": clauses.into_iter().map(Bson::Document).collect::<Vec<_>>() },
    }
}

fn or_of(clauses: Vec<Document>) -> Document {
    match clauses.len() {
        1 => clauses.into_iter().next().expect("one"),
        _ => doc! { "$or": clauses.into_iter().map(Bson::Document).collect::<Vec<_>>() },
    }
}

/// `{path: {$not: {op: value}}}`: the field is not of the types `codes`.
fn not_type(path: &str, codes: Vec<Bson>) -> Document {
    on(
        path,
        "$not",
        Bson::Document(doc! { "$_internalSchemaType": codes }),
    )
}

fn absent(path: &str) -> Document {
    on(path, "$not", Bson::Document(doc! { "$exists": true }))
}

/// A `type` / `bsonType` value as mongod lists it: BSON codes, the alias
/// `number` kept by name.
fn type_codes(value: &Bson, json: bool) -> Option<Vec<Bson>> {
    let names: Vec<&str> = match value {
        Bson::String(s) => vec![s.as_str()],
        Bson::Array(items) => items.iter().map(Bson::as_str).collect::<Option<_>>()?,
        _ => return None,
    };
    names
        .into_iter()
        .map(|name| {
            Some(match name {
                "number" => Bson::String("number".into()),
                "string" => Bson::Int32(2),
                "object" => Bson::Int32(3),
                "array" => Bson::Int32(4),
                "boolean" if json => Bson::Int32(8),
                "null" => Bson::Int32(10),
                _ if json => return None,
                other => Bson::Int32(bson_type_code(other)?),
            })
        })
        .collect()
}

fn bson_type_code(name: &str) -> Option<i32> {
    Some(match name {
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

const NUMBER: &str = "number";

/// The keywords about an object's own fields, in mongod's order.
const OBJECT_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "dependencies",
    "maxProperties",
    "minProperties",
    "patternProperties",
    "properties",
    "required",
];

/// The clauses a schema puts on the object at `path` (the document itself
/// at the root): every keyword about an object's fields, and the
/// combinators.
fn object_clauses(schema: &Document) -> Option<Vec<Document>> {
    let mut out = Vec::new();
    for (key, value) in schema {
        match key.as_str() {
            "title" | "description" | "$comment" => {}
            "type" | "bsonType" => {
                // At the root (or inside an object match) the value is an
                // object, so only `object` can hold.
                let codes = type_codes(value, key == "type")?;
                if !codes.contains(&Bson::Int32(3)) {
                    out.push(doc! { "$alwaysFalse": 1 });
                }
            }
            k if OBJECT_KEYWORDS.contains(&k) => out.extend(object_keyword(schema, k)?),
            "not" | "anyOf" | "allOf" | "oneOf" => {
                out.push(combinator(key, value, object_clauses)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// The clauses one object keyword puts on the object's fields.
fn object_keyword(schema: &Document, key: &str) -> Option<Vec<Document>> {
    let required: Vec<String> = match schema.get("required") {
        None => Vec::new(),
        Some(Bson::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()?,
        Some(_) => return None,
    };
    let value = schema.get(key)?;
    let mut out = Vec::new();
    match key {
        "required" => {
            out.extend(
                required
                    .iter()
                    .map(|n| on(n, "$exists", Bson::Boolean(true))),
            );
        }
        "properties" => {
            let Bson::Document(props) = value else {
                return None;
            };
            if schema.contains_key("additionalProperties")
                || schema.contains_key("patternProperties")
            {
                return Some(out);
            }
            for (name, sub) in props {
                let Bson::Document(sub) = sub else {
                    return None;
                };
                let kept = property_clauses(sub, name)?;
                if required.contains(name) {
                    out.extend(kept);
                } else if !kept.is_empty() {
                    out.push(or_of(vec![and_of(kept), absent(name)]));
                }
            }
        }
        "minProperties" | "maxProperties" => {
            let op = if key == "minProperties" {
                "$_internalSchemaMinProperties"
            } else {
                "$_internalSchemaMaxProperties"
            };
            let mut d = Document::new();
            d.insert(op, value.clone());
            out.push(d);
        }
        "additionalProperties" | "patternProperties" => {
            if key == "patternProperties" && schema.contains_key("additionalProperties") {
                return Some(out);
            }
            out.push(allowed_properties(schema)?);
        }
        "dependencies" => {
            let Bson::Document(deps) = value else {
                return None;
            };
            for (name, dep) in deps {
                let then = match dep {
                    Bson::Array(names) => and_of(
                        names
                            .iter()
                            .map(|n| n.as_str().map(|n| on(n, "$exists", Bson::Boolean(true))))
                            .collect::<Option<_>>()?,
                    ),
                    Bson::Document(sub) => and_of(object_clauses(sub)?),
                    _ => return None,
                };
                out.push(doc! { "$_internalSchemaCond": [
                    on(name, "$exists", Bson::Boolean(true)),
                    then,
                    { "$alwaysTrue": 1 },
                ] });
            }
        }
        _ => return None,
    }
    Some(out)
}

/// `not` / `anyOf` / `allOf` / `oneOf` over subschemas each translated by
/// `parts`.
fn combinator(
    key: &str,
    value: &Bson,
    parts: impl Fn(&Document) -> Option<Vec<Document>>,
) -> Option<Document> {
    if key == "not" {
        return Some(negate(and_of(parts(value.as_document()?)?)));
    }
    let Bson::Array(items) = value else {
        return None;
    };
    let translated: Vec<Document> = items
        .iter()
        .map(|s| s.as_document().and_then(&parts).map(and_of))
        .collect::<Option<_>>()?;
    Some(match key {
        "anyOf" => or_of(translated),
        "allOf" => and_of(translated),
        _ => {
            doc! { "$_internalSchemaXor": translated.into_iter().map(Bson::Document).collect::<Vec<_>>() }
        }
    })
}

/// `additionalProperties` / `patternProperties` (with `properties` naming
/// the fields they leave out).
fn allowed_properties(schema: &Document) -> Option<Document> {
    let names: Vec<Bson> = match schema.get("properties") {
        Some(Bson::Document(props)) => props.keys().map(|k| Bson::String(k.clone())).collect(),
        Some(_) => return None,
        None => Vec::new(),
    };
    let patterns: Vec<Bson> = match schema.get("patternProperties") {
        Some(Bson::Document(pats)) => pats
            .iter()
            .map(|(pattern, sub)| {
                let sub = sub.as_document()?;
                Some(Bson::Document(doc! {
                    "regex": Bson::RegularExpression(bson::Regex {
                        pattern: pattern.clone(),
                        options: String::new(),
                    }),
                    "expression": and_of(property_clauses(sub, "i")?),
                }))
            })
            .collect::<Option<_>>()?,
        Some(_) => return None,
        None => Vec::new(),
    };
    let otherwise = match schema.get("additionalProperties") {
        None | Some(Bson::Boolean(true)) => doc! { "$alwaysTrue": 1 },
        Some(Bson::Boolean(false)) => doc! { "$alwaysFalse": 1 },
        Some(Bson::Document(sub)) => and_of(property_clauses(sub, "i")?),
        Some(_) => return None,
    };
    Some(doc! { "$_internalSchemaAllowedProperties": {
        "properties": names,
        "namePlaceholder": "i",
        "patternProperties": patterns,
        "otherwise": otherwise,
    } })
}

/// A clause negated: a single predicate on a path keeps its path.
fn negate(clause: Document) -> Document {
    if let [(path, Bson::Document(pred))] = clause.iter().collect::<Vec<_>>()[..] {
        if !path.starts_with('$') {
            return on(path, "$not", Bson::Document(pred.clone()));
        }
    }
    doc! { "$nor": [clause] }
}

/// The type a restriction keyword is about.
#[derive(Clone, Copy, PartialEq)]
enum Family {
    Number,
    String,
    Array,
    Object,
}

impl Family {
    fn codes(self) -> Vec<Bson> {
        vec![match self {
            Family::Number => Bson::String(NUMBER.into()),
            Family::String => Bson::Int32(2),
            Family::Array => Bson::Int32(4),
            Family::Object => Bson::Int32(3),
        }]
    }

    fn holds(self, code: &Bson) -> bool {
        match self {
            Family::Number => {
                matches!(code, Bson::String(s) if s == NUMBER)
                    || matches!(code, Bson::Int32(1 | 16 | 18 | 19))
            }
            Family::String => code == &Bson::Int32(2),
            Family::Array => code == &Bson::Int32(4),
            Family::Object => code == &Bson::Int32(3),
        }
    }
}

/// A 15-significant-digit decimal of a double, as mongod converts a
/// `multipleOf` that is one (`2.5` is `2.50000000000000`).
fn decimal_of(v: &Bson) -> Option<Bson> {
    let text = match v {
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Decimal128(d) => d.to_string(),
        Bson::Double(d) if d.is_finite() => {
            let sci = format!("{:.14e}", d);
            let (mantissa, exp) = sci.split_once('e')?;
            let exp: i32 = exp.parse().ok()?;
            let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
            format!("{digits}E{}", exp - 14)
        }
        _ => return None,
    };
    text.parse::<bson::Decimal128>().ok().map(Bson::Decimal128)
}

/// The clauses a property's schema puts on the field `path`. A restriction
/// keyword holds only where the field is of its type, unless the schema
/// declares that one type (then it holds outright) or one other type (then
/// it is dropped), as mongod's parser decides.
fn property_clauses(schema: &Document, path: &str) -> Option<Vec<Document>> {
    let declared: Option<Vec<Bson>> = match (schema.get("type"), schema.get("bsonType")) {
        (Some(t), _) => Some(type_codes(t, true)?),
        (None, Some(t)) => Some(type_codes(t, false)?),
        (None, None) => None,
    };
    let restrict = |family: Family, clause: Document| -> Option<Document> {
        match declared.as_deref() {
            Some([one]) if family.holds(one) => Some(clause),
            Some([_]) => None,
            _ => Some(or_of(vec![clause, not_type(path, family.codes())])),
        }
    };
    let mut out = Vec::new();
    // mongod reads the keywords in its own order, not the schema's.
    let mut keys: Vec<&String> = schema.keys().collect();
    keys.sort();
    for key in keys {
        let value = schema.get(key)?;
        match key.as_str() {
            "title" | "description" | "$comment" | "exclusiveMinimum" | "exclusiveMaximum"
            | "additionalItems" => {}
            "type" | "bsonType" => {
                if key == "bsonType" && schema.contains_key("type") {
                    continue;
                }
                out.push(on(
                    path,
                    "$_internalSchemaType",
                    Bson::Array(declared.clone()?),
                ));
            }
            "minimum" | "maximum" => {
                let lower = key == "minimum";
                let excl = schema
                    .get_bool(if lower {
                        "exclusiveMinimum"
                    } else {
                        "exclusiveMaximum"
                    })
                    .unwrap_or(false);
                let op = match (lower, excl) {
                    (true, false) => "$gte",
                    (true, true) => "$gt",
                    (false, false) => "$lte",
                    (false, true) => "$lt",
                };
                out.extend(restrict(Family::Number, on(path, op, value.clone())));
            }
            "minLength" | "maxLength" => {
                let op = if key == "minLength" {
                    "$_internalSchemaMinLength"
                } else {
                    "$_internalSchemaMaxLength"
                };
                out.extend(restrict(Family::String, on(path, op, value.clone())));
            }
            "pattern" => {
                let pattern = value.as_str()?;
                out.extend(restrict(
                    Family::String,
                    on(path, "$regex", Bson::String(pattern.to_string())),
                ));
            }
            "multipleOf" => {
                let fmod = Bson::Array(vec![decimal_of(value)?, decimal_of(&Bson::Int32(0))?]);
                out.extend(restrict(
                    Family::Number,
                    on(path, "$_internalSchemaFmod", fmod),
                ));
            }
            "enum" => {
                let items = value.as_array()?;
                out.push(or_of(
                    items
                        .iter()
                        .map(|v| on(path, "$_internalSchemaEq", v.clone()))
                        .collect(),
                ));
            }
            "minItems" | "maxItems" | "uniqueItems" => {
                let op = match key.as_str() {
                    "minItems" => "$_internalSchemaMinItems",
                    "maxItems" => "$_internalSchemaMaxItems",
                    _ => "$_internalSchemaUniqueItems",
                };
                out.extend(restrict(Family::Array, on(path, op, value.clone())));
            }
            "items" => match value {
                Bson::Document(sub) => {
                    let inner = and_of(property_clauses(sub, "i")?);
                    out.extend(restrict(
                        Family::Array,
                        on(
                            path,
                            "$_internalSchemaAllElemMatchFromIndex",
                            Bson::Array(vec![Bson::Int32(0), Bson::Document(inner)]),
                        ),
                    ));
                }
                Bson::Array(subs) => {
                    // One restriction around every position's match.
                    let mut positions = Vec::new();
                    for (i, sub) in subs.iter().enumerate() {
                        let inner = and_of(property_clauses(sub.as_document()?, "i")?);
                        positions.push(on(
                            path,
                            "$_internalSchemaMatchArrayIndex",
                            Bson::Document(doc! {
                                "index": i as i32,
                                "namePlaceholder": "i",
                                "expression": inner,
                            }),
                        ));
                    }
                    if !positions.is_empty() {
                        out.extend(restrict(Family::Array, and_of(positions)));
                    }
                    if let Some(Bson::Document(more)) = schema.get("additionalItems") {
                        let inner = and_of(property_clauses(more, "i")?);
                        out.extend(restrict(
                            Family::Array,
                            on(
                                path,
                                "$_internalSchemaAllElemMatchFromIndex",
                                Bson::Array(vec![
                                    Bson::Int32(subs.len() as i32),
                                    Bson::Document(inner),
                                ]),
                            ),
                        ));
                    }
                }
                _ => return None,
            },
            k if OBJECT_KEYWORDS.contains(&k) => {
                // Each object keyword is matched inside the object on its own
                // (`required` also steers which properties may be absent).
                let inner = and_of(object_keyword(schema, k)?);
                if inner.is_empty() {
                    continue;
                }
                out.extend(restrict(
                    Family::Object,
                    on(path, "$_internalSchemaObjectMatch", Bson::Document(inner)),
                ));
            }
            "not" | "anyOf" | "allOf" | "oneOf" => {
                out.push(combinator(key, value, |s| property_clauses(s, path))?);
            }
            _ => return None,
        }
    }
    Some(out)
}
