//! `$jsonSchema` -- a port of mongod's `JSONSchemaParser`.
//!
//! mongod does not interpret a schema while it matches: it TRANSLATES the
//! schema, once, into an ordinary match-expression tree (`$_internalSchema*`
//! nodes, `$exists`, `$gte`, `$regex`, `$and` / `$or` / `$not`), refusing a
//! malformed schema while it does so, and then matches that tree. Both halves
//! matter to what a client sees:
//!
//! - the ERRORS come from the translation, in its order: every keyword of a
//!   level is checked for being known before any is translated, then the
//!   scalar keywords, the array ones, `encrypt`, the object ones and the
//!   logical ones, each refusing a bad argument with its own code and message;
//! - the ANSWERS come from the tree, whose paths are dotted paths (so
//!   `required: ["a.b"]` is `{"a.b": {$exists: true}}`) and whose nodes each
//!   walk a path with their own array rules (`$_internalSchemaType` does not
//!   look inside a leaf array; `$gte` does).
//!
//! So this module is the same two steps: [`parse`] builds a [`Node`] tree the
//! way `json_schema_parser.cpp` does, and [`Node::eval`] matches a document
//! the way `matcher_single_element_match.cpp` evaluates each node. Sources:
//! mongod r8.2.11, `src/mongo/db/matcher/schema/json_schema_parser.cpp` and
//! `src/mongo/db/exec/matcher/`; every message below is verbatim from there
//! and was measured against 8.2.11 by `tools/probes/query_shapes.py`
//! (2026-10-22, RM-268 / RM-269).

use std::collections::BTreeSet;

use bson::{Bson, Document};

use crate::decimal::{self, Dec};
use crate::fallback::Fallback;
use crate::query::{bson_type_name, field_matches, resolve_path, Cand};
use crate::regexutil::{self, CompiledRegex};

type R = Result<bool, Fallback>;

/// The path an `ExpressionWithPlaceholder` binds: the nested schema of
/// `items`, `additionalItems`, `patternProperties` and `additionalProperties`
/// is translated against this one field, and matched against `{i: <value>}`.
const PLACEHOLDER: &str = "i";

/// Every keyword the parser knows (`keywordMap`).
const KEYWORDS: [&str; 31] = [
    "additionalItems",
    "additionalProperties",
    "allOf",
    "anyOf",
    "bsonType",
    "dependencies",
    "description",
    "encrypt",
    "encryptMetadata",
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

/// Keywords JSON Schema has and mongod refuses by name.
const UNSUPPORTED: [&str; 6] = ["$ref", "$schema", "default", "definitions", "format", "id"];

/// The BSON type codes, by `$type` alias (`findBSONTypeAlias`).
const BSON_ALIASES: [(&str, i32); 21] = [
    ("double", 1),
    ("string", 2),
    ("object", 3),
    ("array", 4),
    ("binData", 5),
    ("undefined", 6),
    ("objectId", 7),
    ("bool", 8),
    ("date", 9),
    ("null", 10),
    ("regex", 11),
    ("dbPointer", 12),
    ("javascript", 13),
    ("symbol", 14),
    ("javascriptWithScope", 15),
    ("int", 16),
    ("timestamp", 17),
    ("long", 18),
    ("decimal", 19),
    ("minKey", -1),
    ("maxKey", 127),
];

/// The names `type` takes (`kJsonSchemaTypeAliasMap`); `number` is handled
/// before either map is consulted.
const JSON_ALIASES: [(&str, i32); 5] = [
    ("array", 4),
    ("boolean", 8),
    ("null", 10),
    ("object", 3),
    ("string", 2),
];

const NUMBER_CODES: [i32; 4] = [1, 16, 18, 19];

fn err(code: i32, msg: impl Into<String>) -> Fallback {
    Fallback::mongo(code, msg)
}

/// The BSON type code of a value.
fn type_code(v: &Bson) -> i32 {
    if let Some((t, _)) = crate::bsondoc::as_raw_text(v) {
        return i32::from(t);
    }
    if crate::bsondoc::as_invalid_utf8(v).is_some() {
        return 2;
    }
    match v {
        Bson::Double(_) => 1,
        Bson::String(_) => 2,
        Bson::Document(_) => 3,
        Bson::Array(_) => 4,
        Bson::Binary(_) => 5,
        Bson::Undefined => 6,
        Bson::ObjectId(_) => 7,
        Bson::Boolean(_) => 8,
        Bson::DateTime(_) => 9,
        Bson::Null => 10,
        Bson::RegularExpression(_) => 11,
        Bson::DbPointer(_) => 12,
        Bson::JavaScriptCode(_) => 13,
        Bson::Symbol(_) => 14,
        Bson::JavaScriptCodeWithScope(_) => 15,
        Bson::Int32(_) => 16,
        Bson::Timestamp(_) => 17,
        Bson::Int64(_) => 18,
        Bson::Decimal128(_) => 19,
        Bson::MinKey => -1,
        Bson::MaxKey => 127,
    }
}

fn is_number(v: &Bson) -> bool {
    NUMBER_CODES.contains(&type_code(v))
}

/// `MatcherTypeSet`: some BSON types, and whether every number is one.
#[derive(Debug, Clone, Default)]
pub(crate) struct TypeSet {
    all_numbers: bool,
    types: BTreeSet<i32>,
}

impl TypeSet {
    fn single(code: i32) -> Self {
        TypeSet {
            all_numbers: false,
            types: BTreeSet::from([code]),
        }
    }

    fn numbers() -> Self {
        TypeSet {
            all_numbers: true,
            types: BTreeSet::new(),
        }
    }

    fn has_type(&self, code: i32) -> bool {
        (self.all_numbers && NUMBER_CODES.contains(&code)) || self.types.contains(&code)
    }

    fn is_single_type(&self) -> bool {
        if self.all_numbers {
            self.types.is_empty()
        } else {
            self.types.len() == 1
        }
    }

    fn is_empty(&self) -> bool {
        !self.all_numbers && self.types.is_empty()
    }
}

/// Which of the two length bounds a node checks.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Bound {
    Min,
    Max,
}

impl Bound {
    fn holds(self, have: usize, limit: i64) -> bool {
        let have = have as i64;
        match self {
            Bound::Min => have >= limit,
            Bound::Max => have <= limit,
        }
    }
}

/// A match-expression node, as `json_schema_parser.cpp` builds them.
pub(crate) enum Node {
    AlwaysTrue,
    AlwaysFalse,
    And(Vec<Node>),
    Or(Vec<Node>),
    /// `oneOf`: exactly one child matches.
    Xor(Vec<Node>),
    Not(Box<Node>),
    /// `if` / `then` / `else` (the `dependencies` keyword).
    Cond(Box<Node>, Box<Node>, Box<Node>),
    /// An ordinary query operator on a path (`$exists`, `$gte`, `$lt`,
    /// `$regex`), matched exactly as the same filter would be.
    Leaf {
        path: String,
        cond: Bson,
    },
    /// `$_internalSchemaType`: the value AT the path, never inside a leaf array.
    Type {
        path: String,
        types: TypeSet,
    },
    /// `$_internalSchemaObjectMatch`: the value at the path is a document the
    /// child matches.
    ObjectMatch {
        path: String,
        child: Box<Node>,
    },
    /// `$_internalSchemaFmod` (`multipleOf`), computed in Decimal128.
    Fmod {
        path: String,
        divisor: Dec,
    },
    /// `$_internalSchemaMin/MaxLength`, in UTF-8 code points.
    StrLength {
        path: String,
        bound: Bound,
        len: i64,
    },
    /// `$_internalSchemaMin/MaxItems`.
    ArrayItems {
        path: String,
        bound: Bound,
        len: i64,
    },
    /// `$_internalSchemaUniqueItems`.
    UniqueItems {
        path: String,
    },
    /// `$_internalSchemaMatchArrayIndex` (one position of a tuple `items`).
    MatchArrayIndex {
        path: String,
        index: usize,
        child: Box<Node>,
    },
    /// `$_internalSchemaAllElemMatchFromIndex` (`items`, `additionalItems`).
    AllElemFromIndex {
        path: String,
        index: usize,
        child: Box<Node>,
    },
    /// `$_internalSchemaEq` (`enum` under a path), ignoring field order.
    Eq {
        path: String,
        value: Bson,
    },
    /// `$_internalSchemaRootDocEq` (`enum` at the top level).
    RootDocEq(Bson),
    /// `$_internalSchemaMin/MaxProperties`, on the document being matched.
    NumProperties {
        bound: Bound,
        count: i64,
    },
    /// `$_internalSchemaAllowedProperties`, on the document being matched.
    AllowedProperties {
        properties: Vec<String>,
        patterns: Vec<(CompiledRegex, Node)>,
        otherwise: Box<Node>,
    },
    /// `$_internalSchemaBinDataSubType` (`encrypt`).
    BinDataSubType {
        path: Option<String>,
        subtype: u8,
    },
    /// `$_internalSchemaBinDataEncryptedType` (`encrypt.bsonType`).
    EncryptedType {
        path: Option<String>,
        types: TypeSet,
    },
}

/// Translate a `$jsonSchema` argument, or refuse it as mongod does.
pub(crate) fn parse_top(schema: &Bson) -> Result<Node, Fallback> {
    let Bson::Document(d) = schema else {
        return Err(err(14, "$jsonSchema must be an object"));
    };
    parse(None, d)
}

/// `_parse`: one schema level, at `path` (`None` at the top).
fn parse(path: Option<&str>, schema: &Document) -> Result<Node, Fallback> {
    // Every keyword of this level is checked for being known before any is
    // translated, so an unknown keyword wins over a bad argument beside it.
    for key in schema.keys() {
        if KEYWORDS.contains(&key.as_str()) {
            continue;
        }
        if UNSUPPORTED.contains(&key.as_str()) {
            return Err(err(
                9,
                format!("$jsonSchema keyword '{key}' is not currently supported"),
            ));
        }
        return Err(err(9, format!("Unknown $jsonSchema keyword: {key}")));
    }
    for kw in ["description", "title"] {
        if schema
            .get(kw)
            .is_some_and(|v| !matches!(v, Bson::String(_)))
        {
            return Err(err(
                14,
                format!("$jsonSchema keyword '{kw}' must be of type string"),
            ));
        }
    }
    let type_elt = schema.get("type");
    let bson_type_elt = schema.get("bsonType");
    let encrypt_elt = schema.get("encrypt");
    match (type_elt, bson_type_elt, encrypt_elt) {
        (Some(_), Some(_), _) => {
            return Err(err(
                9,
                "Cannot specify both $jsonSchema keywords 'type' and 'bsonType'",
            ))
        }
        (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => {
            let kw = if type_elt.is_some() {
                "type"
            } else {
                "bsonType"
            };
            return Err(err(
                9,
                format!(
                    "$jsonSchema keyword 'encrypt' cannot be used in conjunction with '{kw}', \
                     'encrypt' implies type 'bsonType::BinData'"
                ),
            ));
        }
        _ => {}
    }
    let stated: Option<TypeSet> = if let Some(t) = type_elt {
        Some(parse_type("type", t, &JSON_ALIASES)?)
    } else if let Some(t) = bson_type_elt {
        Some(parse_type("bsonType", t, &BSON_ALIASES)?)
    } else if encrypt_elt.is_some() {
        Some(TypeSet::single(5))
    } else {
        None
    };
    let mut and = Vec::new();
    let st = stated.as_ref();
    scalar_keywords(schema, path, st, &mut and)?;
    array_keywords(schema, path, st, &mut and)?;
    encryption_keywords(schema, path, &mut and)?;
    object_keywords(schema, path, st, &mut and)?;
    logical_keywords(schema, path, &mut and)?;
    if let Some(types) = stated {
        match path {
            // A top-level schema that requires something other than an
            // object matches nothing: only objects are stored.
            None if !types.has_type(3) => return Ok(Node::AlwaysFalse),
            None => {}
            Some(p) => and.push(Node::Type {
                path: p.to_string(),
                types,
            }),
        }
    }
    Ok(Node::And(and))
}

/// `parseTypeSet` + `fromStringAliases`.
fn parse_type(kw: &str, elt: &Bson, aliases: &[(&str, i32)]) -> Result<TypeSet, Fallback> {
    let integer = || err(9, "$jsonSchema type 'integer' is not currently supported.");
    let mut names: BTreeSet<&str> = BTreeSet::new();
    match elt {
        Bson::String(s) => {
            if s == "integer" {
                return Err(integer());
            }
            names.insert(s);
        }
        Bson::Array(items) => {
            for item in items {
                let Bson::String(s) = item else {
                    return Err(err(
                        14,
                        format!("$jsonSchema keyword '{kw}' array elements must be strings"),
                    ));
                };
                if s == "integer" {
                    return Err(integer());
                }
                if !names.insert(s) {
                    return Err(err(
                        9,
                        format!("$jsonSchema keyword '{kw}' has duplicate value: {s}"),
                    ));
                }
            }
        }
        _ => {
            return Err(err(
                14,
                format!(
                    "$jsonSchema keyword '{kw}' must be either a string or an array of strings"
                ),
            ))
        }
    }
    // The aliases are a SET, so an unknown one is reported in sorted order.
    let mut types = TypeSet::default();
    for name in names {
        if name == "number" {
            types.all_numbers = true;
        } else if let Some((_, code)) = aliases.iter().find(|(a, _)| *a == name) {
            types.types.insert(*code);
        } else {
            return Err(err(2, format!("Unknown type name alias: {name}")));
        }
    }
    if types.is_empty() {
        return Err(err(
            9,
            format!("$jsonSchema keyword '{kw}' must name at least one type"),
        ));
    }
    Ok(types)
}

/// What a restriction keyword applies to.
enum Applies {
    Numbers,
    Type(i32),
}

impl Applies {
    fn types(&self) -> TypeSet {
        match self {
            Applies::Numbers => TypeSet::numbers(),
            Applies::Type(code) => TypeSet::single(*code),
        }
    }
}

/// `makeRestriction`: a restriction keyword (`maxItems`, `minimum`, ...) holds
/// for any value of another type. When the schema states a single type the
/// restriction either always applies or never does; otherwise it is
/// `(NOT type) OR restriction`.
fn restrict(applies: Applies, path: &str, expr: Node, stated: Option<&TypeSet>) -> Node {
    if let Some(stated) = stated.filter(|s| s.is_single_type()) {
        let code = if stated.all_numbers {
            16
        } else {
            *stated.types.iter().next().expect("single type")
        };
        return if applies.types().has_type(code) {
            expr
        } else {
            Node::AlwaysTrue
        };
    }
    Node::Or(vec![
        Node::Not(Box::new(Node::Type {
            path: path.to_string(),
            types: applies.types(),
        })),
        expr,
    ])
}

fn leaf(path: &str, op: &str, arg: Bson) -> Node {
    let mut cond = Document::new();
    cond.insert(op, arg);
    Node::Leaf {
        path: path.to_string(),
        cond: Bson::Document(cond),
    }
}

fn exists(path: &str) -> Node {
    leaf(path, "$exists", Bson::Boolean(true))
}

fn object_match(path: &str, child: Node) -> Node {
    Node::ObjectMatch {
        path: path.to_string(),
        child: Box::new(child),
    }
}

/// mongod's rendering of a keyword element (`toString(true, true)`).
fn elt_repr(kw: &str, v: &Bson) -> String {
    format!("{kw}: {}", crate::query::bson_value_repr(v))
}

/// `parseIntegerElementToNonNegativeLong`.
fn non_negative_long(kw: &str, v: &Bson) -> Result<i64, Fallback> {
    let n = match v {
        Bson::Int32(n) => *n as i64,
        Bson::Int64(n) => *n,
        Bson::Double(d) => {
            if d.is_nan() {
                return Err(err(
                    9,
                    format!("Expected an integer, but found NaN in: {}", elt_repr(kw, v)),
                ));
            }
            if *d >= 9_223_372_036_854_775_808.0 || *d < -9_223_372_036_854_775_808.0 {
                return Err(err(
                    9,
                    format!("Cannot represent as a 64-bit integer: {}", elt_repr(kw, v)),
                ));
            }
            if d.fract() != 0.0 {
                return Err(err(9, format!("Expected an integer: {}", elt_repr(kw, v))));
            }
            *d as i64
        }
        Bson::Decimal128(_) => decimal::decimal_to_i64_exact(v).ok_or_else(|| {
            err(
                9,
                format!("Cannot represent as a 64-bit integer: {}", elt_repr(kw, v)),
            )
        })?,
        _ => return Err(err(9, format!("Expected a number in: {}", elt_repr(kw, v)))),
    };
    if n < 0 {
        return Err(err(
            9,
            format!("Expected a non-negative number in: {}", elt_repr(kw, v)),
        ));
    }
    Ok(n)
}

/// `translateScalarKeywords`: `pattern`, `maxLength`, `minLength`,
/// `multipleOf`, `maximum`, `minimum`, in that order.
fn scalar_keywords(
    schema: &Document,
    path: Option<&str>,
    stated: Option<&TypeSet>,
    and: &mut Vec<Node>,
) -> Result<(), Fallback> {
    if let Some(pattern) = schema.get("pattern") {
        let Bson::String(pat) = pattern else {
            return Err(err(14, "$jsonSchema keyword 'pattern' must be a string"));
        };
        and.push(match path {
            None => Node::AlwaysTrue,
            Some(p) => {
                if let Some(msg) = regexutil::pcre_compile_error(pat) {
                    return Err(err(51091, format!("Regular expression is invalid: {msg}")));
                }
                let expr = leaf(p, "$regex", Bson::String(pat.clone()));
                restrict(Applies::Type(2), p, expr, stated)
            }
        });
    }
    for (kw, bound) in [("maxLength", Bound::Max), ("minLength", Bound::Min)] {
        if let Some(v) = schema.get(kw) {
            let len = non_negative_long(kw, v)?;
            and.push(match path {
                None => Node::AlwaysTrue,
                Some(p) => {
                    let expr = Node::StrLength {
                        path: p.to_string(),
                        bound,
                        len,
                    };
                    restrict(Applies::Type(2), p, expr, stated)
                }
            });
        }
    }
    if let Some(m) = schema.get("multipleOf") {
        if !is_number(m) {
            return Err(err(14, "$jsonSchema keyword 'multipleOf' must be a number"));
        }
        let divisor = decimal::from_bson(m).ok_or(Fallback::Defer)?;
        let negative = match &divisor {
            Dec::Fin { sign, .. } | Dec::Inf(sign) => *sign < 0,
            Dec::Nan => false,
        };
        if negative || divisor.is_zero() {
            return Err(err(
                9,
                "$jsonSchema keyword 'multipleOf' must have a positive value",
            ));
        }
        and.push(match path {
            None => Node::AlwaysTrue,
            Some(p) => {
                // The node's own constructor refuses these two.
                match divisor {
                    Dec::Nan => return Err(err(2, "divisor cannot be NaN")),
                    Dec::Inf(_) => return Err(err(2, "divisor cannot be infinite")),
                    Dec::Fin { .. } => {}
                }
                let expr = Node::Fmod {
                    path: p.to_string(),
                    divisor,
                };
                restrict(Applies::Numbers, p, expr, stated)
            }
        });
    }
    for (kw, excl_kw, op, excl_op) in [
        ("maximum", "exclusiveMaximum", "$lte", "$lt"),
        ("minimum", "exclusiveMinimum", "$gte", "$gt"),
    ] {
        let Some(bound) = schema.get(kw) else {
            if schema.contains_key(excl_kw) {
                return Err(err(
                    9,
                    format!("$jsonSchema keyword '{kw}' must be a present if {excl_kw} is present"),
                ));
            }
            continue;
        };
        let exclusive = match schema.get(excl_kw) {
            None => false,
            Some(Bson::Boolean(b)) => *b,
            Some(_) => {
                return Err(err(
                    14,
                    format!("$jsonSchema keyword '{excl_kw}' must be a boolean"),
                ))
            }
        };
        if !is_number(bound) {
            return Err(err(
                14,
                format!("$jsonSchema keyword '{kw}' must be a number"),
            ));
        }
        and.push(match path {
            None => Node::AlwaysTrue,
            Some(p) => {
                let expr = leaf(p, if exclusive { excl_op } else { op }, bound.clone());
                restrict(Applies::Numbers, p, expr, stated)
            }
        });
    }
    Ok(())
}

/// `translateArrayKeywords`: `minItems`, `maxItems`, `uniqueItems`, then
/// `items` and `additionalItems`.
fn array_keywords(
    schema: &Document,
    path: Option<&str>,
    stated: Option<&TypeSet>,
    and: &mut Vec<Node>,
) -> Result<(), Fallback> {
    for (kw, bound) in [("minItems", Bound::Min), ("maxItems", Bound::Max)] {
        if let Some(v) = schema.get(kw) {
            let len = non_negative_long(kw, v)?;
            and.push(match path {
                None => Node::AlwaysTrue,
                Some(p) => {
                    let expr = Node::ArrayItems {
                        path: p.to_string(),
                        bound,
                        len,
                    };
                    restrict(Applies::Type(4), p, expr, stated)
                }
            });
        }
    }
    if let Some(v) = schema.get("uniqueItems") {
        let Bson::Boolean(unique) = v else {
            return Err(err(
                14,
                "$jsonSchema keyword 'uniqueItems' must be a boolean",
            ));
        };
        and.push(match path {
            Some(p) if *unique => {
                let expr = Node::UniqueItems {
                    path: p.to_string(),
                };
                restrict(Applies::Type(4), p, expr, stated)
            }
            _ => Node::AlwaysTrue,
        });
    }
    // The index `additionalItems` applies from: set only by a TUPLE `items`.
    let mut additional_from: Option<usize> = None;
    if let Some(items) = schema.get("items") {
        match items {
            Bson::Array(subs) => {
                let mut tuple = Vec::new();
                for (index, sub) in subs.iter().enumerate() {
                    let Bson::Document(sd) = sub else {
                        return Err(err(
                            14,
                            format!(
                                "$jsonSchema keyword 'items' requires that each element of the \
                                 array is an object, but found a {}",
                                bson_type_name(sub)
                            ),
                        ));
                    };
                    let child = parse(Some(PLACEHOLDER), sd)?;
                    if let Some(p) = path {
                        tuple.push(Node::MatchArrayIndex {
                            path: p.to_string(),
                            index,
                            child: Box::new(child),
                        });
                    }
                }
                additional_from = Some(subs.len());
                and.push(match path {
                    None => Node::AlwaysTrue,
                    Some(p) => restrict(Applies::Type(4), p, Node::And(tuple), stated),
                });
            }
            Bson::Document(sd) => {
                let child = parse(Some(PLACEHOLDER), sd)?;
                and.push(match path {
                    None => Node::AlwaysTrue,
                    Some(p) => {
                        let expr = Node::AllElemFromIndex {
                            path: p.to_string(),
                            index: 0,
                            child: Box::new(child),
                        };
                        restrict(Applies::Type(4), p, expr, stated)
                    }
                });
            }
            other => {
                return Err(err(
                    14,
                    format!(
                        "$jsonSchema keyword 'items' must be an array or an object, not {}",
                        bson_type_name(other)
                    ),
                ))
            }
        }
    }
    if let Some(extra) = schema.get("additionalItems") {
        let otherwise = match extra {
            Bson::Boolean(true) => Node::AlwaysTrue,
            Bson::Boolean(false) => Node::AlwaysFalse,
            Bson::Document(sd) => parse(Some(PLACEHOLDER), sd)?,
            other => {
                return Err(err(
                    14,
                    format!(
                        "$jsonSchema keyword 'additionalItems' must be either an object or a \
                         boolean, but got a {}",
                        bson_type_name(other)
                    ),
                ))
            }
        };
        if let Some(index) = additional_from {
            and.push(match path {
                None => Node::AlwaysTrue,
                Some(p) => {
                    let expr = Node::AllElemFromIndex {
                        path: p.to_string(),
                        index,
                        child: Box::new(otherwise),
                    };
                    restrict(Applies::Type(4), p, expr, stated)
                }
            });
        }
    }
    Ok(())
}

/// The fields an `encrypt` or `encryptMetadata` object may hold (the IDL
/// structs are strict).
fn encryption_fields(kw: &str, d: &Document, allowed: &[&str]) -> Result<(), Fallback> {
    for (name, value) in d {
        if !allowed.contains(&name.as_str()) {
            return Err(err(
                40415,
                format!("BSON field '{kw}.{name}' is an unknown field."),
            ));
        }
        match name.as_str() {
            "algorithm" => {
                let Bson::String(s) = value else {
                    return Err(err(
                        14,
                        format!(
                            "BSON field '{kw}.algorithm' is the wrong type '{}', expected type \
                             'string'",
                            bson_type_name(value)
                        ),
                    ));
                };
                if !matches!(
                    s.as_str(),
                    "AEAD_AES_256_CBC_HMAC_SHA_512-Deterministic"
                        | "AEAD_AES_256_CBC_HMAC_SHA_512-Random"
                ) {
                    return Err(err(
                        2,
                        format!("Enumeration value '{s}' for field '{kw}.algorithm' is not a valid value."),
                    ));
                }
            }
            "keyId" => match value {
                // A JSON pointer to the key (`JSONPointer`'s constructor).
                Bson::String(ptr) => {
                    if ptr.is_empty() {
                        return Err(err(51064, "Empty JSONPointers are not supported"));
                    }
                    if !ptr.starts_with('/') {
                        return Err(err(51065, "JSONPointer must start with a '/'"));
                    }
                    let mut chars = ptr.chars().peekable();
                    while let Some(c) = chars.next() {
                        if c == '~' && !matches!(chars.peek(), Some('0' | '1')) {
                            return Err(err(
                                51063,
                                "JSONPointer cannot contain unescaped ~ character",
                            ));
                        }
                    }
                }
                Bson::Array(keys) => {
                    for key in keys {
                        match key {
                            Bson::Binary(b) if b.subtype == bson::spec::BinarySubtype::Uuid => {
                                if b.bytes.len() != 16 {
                                    return Err(err(
                                        207,
                                        "uuid must be a 16-byte binary field with UUID (4) subtype",
                                    ));
                                }
                            }
                            Bson::Binary(b) => {
                                return Err(err(
                                    51084,
                                    format!(
                                        "Encryption schema 'keyId' array elements must have \
                                         BinData type UUID, found {}",
                                        u8::from(b.subtype)
                                    ),
                                ))
                            }
                            other => {
                                return Err(err(
                                    51088,
                                    format!(
                                        "Encryption schema 'keyId' array elements must have \
                                         type BinData, found {}",
                                        bson_type_name(other)
                                    ),
                                ))
                            }
                        }
                    }
                }
                other => {
                    return Err(err(
                        51085,
                        format!(
                            "Expected either string or array of UUID for EncryptSchemaKeyId, \
                             found {}",
                            bson_type_name(other)
                        ),
                    ))
                }
            },
            _ => {}
        }
    }
    Ok(())
}

/// `translateEncryptionKeywords`.
fn encryption_keywords(
    schema: &Document,
    path: Option<&str>,
    and: &mut Vec<Node>,
) -> Result<(), Fallback> {
    let encrypt = schema.get("encrypt");
    let metadata = schema.get("encryptMetadata");
    if encrypt.is_some() && metadata.is_some() {
        return Err(err(
            9,
            "Cannot specify both $jsonSchema keywords 'encrypt' and 'encryptMetadata'",
        ));
    }
    if let Some(m) = metadata {
        let Bson::Document(md) = m else {
            return Err(err(
                14,
                "$jsonSchema keyword 'encryptMetadata' must be an object ",
            ));
        };
        if md.is_empty() {
            return Err(err(
                9,
                "$jsonSchema keyword 'encryptMetadata' cannot be an empty object ",
            ));
        }
        encryption_fields("encryptMetadata", md, &["algorithm", "keyId"])?;
    }
    if let Some(e) = encrypt {
        let Bson::Document(ed) = e else {
            return Err(err(14, "$jsonSchema keyword 'encrypt' must be an object "));
        };
        encryption_fields("encrypt", ed, &["bsonType", "algorithm", "keyId"])?;
        let types = match ed.get("bsonType") {
            Some(t) => Some(encrypt_bson_type(t)?),
            None => None,
        };
        and.push(Node::BinDataSubType {
            path: path.map(str::to_string),
            subtype: 6,
        });
        if let Some(types) = types {
            and.push(Node::EncryptedType {
                path: path.map(str::to_string),
                types,
            });
        }
    }
    Ok(())
}

/// `BSONTypeSet::parseFromBSON`, the type list of `encrypt.bsonType`.
fn encrypt_bson_type(t: &Bson) -> Result<TypeSet, Fallback> {
    let names: Vec<&Bson> = match t {
        Bson::Array(a) => a.iter().collect(),
        single => vec![single],
    };
    let mut types = TypeSet::default();
    for name in names {
        let Bson::String(s) = name else {
            return Err(err(
                14,
                "$jsonSchema keyword 'bsonType' must be either a string or an array of strings",
            ));
        };
        if s == "number" {
            types.all_numbers = true;
        } else if let Some((_, code)) = BSON_ALIASES.iter().find(|(a, _)| a == s) {
            types.types.insert(*code);
        } else {
            return Err(err(2, format!("Unknown type name alias: {s}")));
        }
    }
    Ok(types)
}

/// `parseRequired`.
fn parse_required(v: &Bson) -> Result<Vec<String>, Fallback> {
    let Bson::Array(names) = v else {
        return Err(err(
            14,
            format!(
                "$jsonSchema keyword 'required' must be an array, but found an element of type {}",
                bson_type_name(v)
            ),
        ));
    };
    let mut seen = BTreeSet::new();
    for name in names {
        let Bson::String(s) = name else {
            return Err(err(
                14,
                format!(
                    "$jsonSchema keyword 'required' must be an array of strings, but found an \
                     element of type: {}",
                    bson_type_name(name)
                ),
            ));
        };
        if !seen.insert(s.clone()) {
            return Err(err(
                9,
                "$jsonSchema keyword 'required' array cannot contain duplicate values",
            ));
        }
    }
    if seen.is_empty() {
        return Err(err(
            9,
            "$jsonSchema keyword 'required' cannot be an empty array",
        ));
    }
    // A set: sorted, as `translateRequired` sorts it.
    Ok(seen.into_iter().collect())
}

/// `translateObjectKeywords`: `required` (parsed first, translated after),
/// `properties`, `patternProperties` / `additionalProperties`,
/// `minProperties`, `maxProperties`, `dependencies`.
fn object_keywords(
    schema: &Document,
    path: Option<&str>,
    stated: Option<&TypeSet>,
    and: &mut Vec<Node>,
) -> Result<(), Fallback> {
    // Under a path, an object keyword checks the document AT the path, and
    // holds for anything that is not one.
    let under_path = |expr: Node| match path {
        None => expr,
        Some(p) => restrict(Applies::Type(3), p, object_match(p, expr), stated),
    };
    let required = match schema.get("required") {
        Some(v) => parse_required(v)?,
        None => Vec::new(),
    };
    let mut property_names = Vec::new();
    if let Some(props) = schema.get("properties") {
        let Bson::Document(pd) = props else {
            return Err(err(
                14,
                "$jsonSchema keyword 'properties' must be an object",
            ));
        };
        let mut each = Vec::new();
        for (name, sub) in pd {
            let Bson::Document(sd) = sub else {
                return Err(err(
                    14,
                    format!("Nested schema for $jsonSchema property '{name}' must be an object"),
                ));
            };
            let nested = parse(Some(name), sd)?;
            property_names.push(name.clone());
            each.push(if required.contains(name) {
                nested
            } else {
                // Absent, or matching its schema.
                Node::Or(vec![Node::Not(Box::new(exists(name))), nested])
            });
        }
        and.push(under_path(Node::And(each)));
    }
    let pattern_props = schema.get("patternProperties");
    let additional = schema.get("additionalProperties");
    if pattern_props.is_some() || additional.is_some() {
        let mut patterns = Vec::new();
        if let Some(pp) = pattern_props {
            let Bson::Document(ppd) = pp else {
                return Err(err(
                    14,
                    "$jsonSchema keyword 'patternProperties' must be an object",
                ));
            };
            for (pat, sub) in ppd {
                let Bson::Document(sd) = sub else {
                    return Err(err(
                        14,
                        format!(
                            "$jsonSchema keyword 'patternProperties' has property '{pat}' which \
                             is not an object"
                        ),
                    ));
                };
                patterns.push((pat.clone(), parse(Some(PLACEHOLDER), sd)?));
            }
        }
        let otherwise = match additional {
            None | Some(Bson::Boolean(true)) => Node::AlwaysTrue,
            Some(Bson::Boolean(false)) => Node::AlwaysFalse,
            Some(Bson::Document(sd)) => parse(Some(PLACEHOLDER), sd)?,
            Some(_) => {
                return Err(err(
                    14,
                    "$jsonSchema keyword 'additionalProperties' must be an object or a boolean",
                ))
            }
        };
        // The node's constructor compiles the patterns, after both are parsed.
        let mut compiled = Vec::new();
        for (pat, child) in patterns {
            if let Some(msg) = regexutil::pcre_compile_error(&pat) {
                return Err(err(2, format!("Invalid regular expression: {msg}")));
            }
            let rx = regexutil::compile(&Bson::String(pat), None).map_err(|_| Fallback::Defer)?;
            compiled.push((rx, child));
        }
        and.push(under_path(Node::AllowedProperties {
            properties: property_names,
            patterns: compiled,
            otherwise: Box::new(otherwise),
        }));
    }
    if !required.is_empty() {
        and.push(under_path(Node::And(
            required.iter().map(|name| exists(name)).collect(),
        )));
    }
    for (kw, bound) in [("minProperties", Bound::Min), ("maxProperties", Bound::Max)] {
        if let Some(v) = schema.get(kw) {
            let count = non_negative_long(kw, v)?;
            and.push(under_path(Node::NumProperties { bound, count }));
        }
    }
    if let Some(deps) = schema.get("dependencies") {
        let Bson::Document(dd) = deps else {
            return Err(err(
                14,
                "$jsonSchema keyword 'dependencies' must be an object",
            ));
        };
        // The "if": the property exists (in the document AT the path).
        let present = |name: &str| match path {
            None => exists(name),
            Some(p) => object_match(p, exists(name)),
        };
        let mut each = Vec::new();
        for (name, dep) in dd {
            let then = match dep {
                Bson::Document(sd) => parse(path, sd)?,
                Bson::Array(names) => {
                    if names.is_empty() {
                        return Err(err(
                            9,
                            format!(
                                "property '{name}' in $jsonSchema keyword 'dependencies' cannot \
                                 be an empty array"
                            ),
                        ));
                    }
                    let mut seen = BTreeSet::new();
                    let mut all = Vec::new();
                    for n in names {
                        let Bson::String(s) = n else {
                            return Err(err(
                                14,
                                format!(
                                    "array '{name}' in $jsonSchema keyword 'dependencies' can \
                                     only contain strings, but found element of type: {}",
                                    bson_type_name(n)
                                ),
                            ));
                        };
                        if !seen.insert(s.as_str()) {
                            return Err(err(
                                9,
                                format!(
                                    "array '{name}' in $jsonSchema keyword 'dependencies' \
                                     contains duplicate element: {s}"
                                ),
                            ));
                        }
                        all.push(present(s));
                    }
                    Node::And(all)
                }
                // A title or description among the dependencies is allowed.
                Bson::String(_) if name == "title" || name == "description" => continue,
                _ => {
                    return Err(err(
                        14,
                        format!(
                            "property '{name}' in $jsonSchema keyword 'dependencies' must be \
                             either an object or an array"
                        ),
                    ))
                }
            };
            each.push(Node::Cond(
                Box::new(present(name)),
                Box::new(then),
                Box::new(Node::AlwaysTrue),
            ));
        }
        and.push(Node::And(each));
    }
    Ok(())
}

/// `translateLogicalKeywords`: `allOf`, `anyOf`, `oneOf`, `not`, `enum`.
fn logical_keywords(
    schema: &Document,
    path: Option<&str>,
    and: &mut Vec<Node>,
) -> Result<(), Fallback> {
    for kw in ["allOf", "anyOf", "oneOf"] {
        let Some(v) = schema.get(kw) else {
            continue;
        };
        let Bson::Array(subs) = v else {
            return Err(err(
                14,
                format!("$jsonSchema keyword '{kw}' must be an array"),
            ));
        };
        if subs.is_empty() {
            return Err(err(
                2,
                format!("$jsonSchema keyword '{kw}' must be a non-empty array"),
            ));
        }
        let mut children = Vec::new();
        for sub in subs {
            let Bson::Document(sd) = sub else {
                return Err(err(
                    14,
                    format!(
                        "$jsonSchema keyword '{kw}' must be an array of objects, but found an \
                         element of type {}",
                        bson_type_name(sub)
                    ),
                ));
            };
            children.push(parse(path, sd)?);
        }
        and.push(match kw {
            "allOf" => Node::And(children),
            "anyOf" => Node::Or(children),
            _ => Node::Xor(children),
        });
    }
    if let Some(v) = schema.get("not") {
        let Bson::Document(sd) = v else {
            return Err(err(
                14,
                format!(
                    "$jsonSchema keyword 'not' must be an object, but found an element of type {}",
                    bson_type_name(v)
                ),
            ));
        };
        and.push(Node::Not(Box::new(parse(path, sd)?)));
    }
    if let Some(v) = schema.get("enum") {
        let Bson::Array(values) = v else {
            return Err(err(
                14,
                format!(
                    "$jsonSchema keyword 'enum' must be an array, but found an element of type {}",
                    bson_type_name(v)
                ),
            ));
        };
        if values.is_empty() {
            return Err(err(
                9,
                "$jsonSchema keyword 'enum' cannot be an empty array",
            ));
        }
        let mut seen = BTreeSet::new();
        let mut any = Vec::new();
        for value in values {
            if !seen.insert(unordered_key(value)?) {
                return Err(err(
                    9,
                    "$jsonSchema keyword 'enum' array cannot contain duplicate values.",
                ));
            }
            match path {
                // At the top level only an object can equal a stored document.
                None if matches!(value, Bson::Document(_)) => {
                    any.push(Node::RootDocEq(value.clone()))
                }
                None => {}
                Some(p) => any.push(Node::Eq {
                    path: p.to_string(),
                    value: value.clone(),
                }),
            }
        }
        and.push(if any.is_empty() {
            Node::AlwaysFalse
        } else {
            Node::Or(any)
        });
    }
    Ok(())
}

/// A key equal for exactly the values mongod's `UnorderedFieldsBSONElement
/// Comparator` calls equal: numbers by value across types, documents as their
/// fields sorted by name (recursively), arrays in order.
fn unordered_key(v: &Bson) -> Result<Vec<u8>, Fallback> {
    let mut out = Vec::new();
    push_unordered_key(v, &mut out)?;
    Ok(out)
}

fn push_unordered_key(v: &Bson, out: &mut Vec<u8>) -> Result<(), Fallback> {
    let child = |v: &Bson, out: &mut Vec<u8>| -> Result<(), Fallback> {
        let key = unordered_key(v)?;
        out.extend((key.len() as u32).to_be_bytes());
        out.extend(key);
        Ok(())
    };
    match v {
        Bson::Document(d) => {
            out.push(b'd');
            let mut fields: Vec<(&String, &Bson)> = d.iter().collect();
            fields.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            for (k, fv) in fields {
                out.extend((k.len() as u32).to_be_bytes());
                out.extend_from_slice(k.as_bytes());
                child(fv, out)?;
            }
        }
        Bson::Array(a) => {
            out.push(b'a');
            for e in a {
                child(e, out)?;
            }
        }
        other => {
            out.push(b'v');
            out.extend(crate::sortkey::encode_value(other, None).map_err(|_| Fallback::Defer)?);
        }
    }
    Ok(())
}

/// Whether `enum` / `uniqueItems` call two values equal.
pub(crate) fn values_equal(a: &Bson, b: &Bson) -> bool {
    matches!((unordered_key(a), unordered_key(b)), (Ok(x), Ok(y)) if x == y)
}

/// Whether `value` is a multiple of `divisor` as `multipleOf` computes it: in
/// Decimal128, a double converted at 15 significant digits.
pub(crate) fn multiple_of(value: &Bson, divisor: &Bson) -> bool {
    if !is_number(value) || !is_number(divisor) {
        return false;
    }
    match (decimal::from_bson(value), decimal::from_bson(divisor)) {
        (Some(v), Some(d @ Dec::Fin { .. })) => decimal::rem(&v, &d).is_some_and(|r| r.is_zero()),
        _ => false,
    }
}

/// The document an `ExpressionWithPlaceholder` is matched against.
fn placeholder_doc(v: &Bson) -> Document {
    let mut d = Document::new();
    d.insert(PLACEHOLDER, v.clone());
    d
}

/// The values a path reaches WITHOUT looking inside a leaf array
/// (`LeafArrayBehavior::kNoTraversal`); arrays met before the leaf are
/// traversed.
fn at_path<'a>(doc: &'a Document, path: &str) -> Vec<&'a Bson> {
    resolve_path(doc, path)
        .into_iter()
        .filter_map(|c| c.value)
        .collect()
}

/// The values a path reaches, a leaf array's elements included
/// (`LeafArrayBehavior::kTraverse`). The array itself is left out: none of
/// the nodes that use this can match one.
fn at_path_traversed<'a>(doc: &'a Document, path: &str) -> Vec<&'a Bson> {
    let mut out = Vec::new();
    for cand in resolve_path(doc, path) {
        match (cand.elements(true), cand.value) {
            (Some(items), _) => out.extend(items.iter()),
            (None, Some(v)) => out.push(v),
            (None, None) => {}
        }
    }
    out
}

fn any_value(values: Vec<&Bson>, mut test: impl FnMut(&Bson) -> R) -> R {
    for v in values {
        if test(v)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The encrypted payload's header: subtype byte, key UUID, original type.
const FLE_BLOB_HEADER: usize = 1 + 16 + 1;

impl Node {
    /// Whether `doc` matches this node.
    pub(crate) fn eval(&self, doc: &Document) -> R {
        match self {
            Node::AlwaysTrue => Ok(true),
            Node::AlwaysFalse => Ok(false),
            Node::And(children) => {
                for c in children {
                    if !c.eval(doc)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Node::Or(children) => {
                for c in children {
                    if c.eval(doc)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Node::Xor(children) => {
                let mut found = false;
                for c in children {
                    if c.eval(doc)? {
                        if found {
                            return Ok(false);
                        }
                        found = true;
                    }
                }
                Ok(found)
            }
            Node::Not(child) => Ok(!child.eval(doc)?),
            Node::Cond(cond, then, otherwise) => {
                if cond.eval(doc)? {
                    then.eval(doc)
                } else {
                    otherwise.eval(doc)
                }
            }
            Node::Leaf { path, cond } => {
                let cands: Vec<Cand> = resolve_path(doc, path);
                field_matches(&cands, cond, None, path)
            }
            Node::Type { path, types } => Ok(at_path(doc, path)
                .into_iter()
                .any(|v| types.has_type(type_code(v)))),
            Node::ObjectMatch { path, child } => any_value(at_path(doc, path), |v| match v {
                Bson::Document(d) => child.eval(d),
                _ => Ok(false),
            }),
            Node::Fmod { path, divisor } => Ok(at_path_traversed(doc, path).into_iter().any(|v| {
                is_number(v)
                    && decimal::from_bson(v)
                        .and_then(|n| decimal::rem(&n, divisor))
                        .is_some_and(|r| r.is_zero())
            })),
            Node::StrLength { path, bound, len } => {
                Ok(at_path_traversed(doc, path).into_iter().any(|v| match v {
                    Bson::String(s) => bound.holds(s.chars().count(), *len),
                    // A string that is not UTF-8: one code point per byte that
                    // is not a continuation byte, as mongod counts (RM-222).
                    v => crate::bsondoc::as_invalid_utf8(v)
                        .is_some_and(|b| bound.holds(crate::rawstr::len_cp(b), *len)),
                }))
            }
            Node::ArrayItems { path, bound, len } => {
                Ok(at_path(doc, path).into_iter().any(|v| match v {
                    Bson::Array(a) => bound.holds(a.len(), *len),
                    _ => false,
                }))
            }
            Node::UniqueItems { path } => any_value(at_path(doc, path), |v| {
                let Bson::Array(a) = v else {
                    return Ok(false);
                };
                let mut seen = BTreeSet::new();
                for e in a {
                    if !seen.insert(unordered_key(e)?) {
                        return Ok(false);
                    }
                }
                Ok(true)
            }),
            Node::MatchArrayIndex { path, index, child } => {
                any_value(at_path(doc, path), |v| match v {
                    // An array too short to have the position matches.
                    Bson::Array(a) => match a.get(*index) {
                        None => Ok(true),
                        Some(e) => child.eval(&placeholder_doc(e)),
                    },
                    _ => Ok(false),
                })
            }
            Node::AllElemFromIndex { path, index, child } => any_value(at_path(doc, path), |v| {
                let Bson::Array(a) = v else {
                    return Ok(false);
                };
                for e in a.iter().skip(*index) {
                    if !child.eval(&placeholder_doc(e))? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }),
            Node::Eq { path, value } => {
                let want = unordered_key(value)?;
                any_value(at_path(doc, path), |v| Ok(unordered_key(v)? == want))
            }
            Node::RootDocEq(value) => {
                Ok(unordered_key(&Bson::Document(doc.clone()))? == unordered_key(value)?)
            }
            Node::NumProperties { bound, count } => Ok(bound.holds(doc.len(), *count)),
            Node::AllowedProperties {
                properties,
                patterns,
                otherwise,
            } => {
                for (name, value) in doc {
                    let mut check_otherwise = true;
                    for (rx, child) in patterns {
                        if rx.is_match(name) {
                            check_otherwise = false;
                            if !child.eval(&placeholder_doc(value))? {
                                return Ok(false);
                            }
                        }
                    }
                    if check_otherwise
                        && !properties.contains(name)
                        && !otherwise.eval(&placeholder_doc(value))?
                    {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Node::BinDataSubType { path, subtype } => {
                let Some(path) = path else {
                    return Err(Fallback::Defer);
                };
                Ok(at_path_traversed(doc, path)
                    .into_iter()
                    .any(|v| matches!(v, Bson::Binary(b) if u8::from(b.subtype) == *subtype)))
            }
            Node::EncryptedType { path, types } => {
                let Some(path) = path else {
                    return Err(Fallback::Defer);
                };
                Ok(at_path_traversed(doc, path).into_iter().any(|v| match v {
                    Bson::Binary(b) if u8::from(b.subtype) == 6 => {
                        b.bytes.len() >= FLE_BLOB_HEADER
                            && matches!(b.bytes[0], 1 | 2)
                            && types.has_type(b.bytes[FLE_BLOB_HEADER - 1] as i8 as i32)
                    }
                    _ => false,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bson::{bson, doc, Bson};

    use super::parse_top;

    fn refusal(schema: Bson) -> Option<(i32, String)> {
        parse_top(&schema)
            .err()
            .and_then(|f| f.as_mongo().map(|(c, m)| (c, m.to_string())))
    }

    fn matches(schema: Bson, doc: bson::Document) -> bool {
        parse_top(&schema).unwrap().eval(&doc).unwrap()
    }

    /// Every keyword of a level is checked for being known before any is
    /// translated, so an unknown one wins over a bad argument beside it.
    #[test]
    fn unknown_keyword_wins_over_bad_argument() {
        let got = refusal(bson!({"properties": {"a": {"multipleOf": "x", "bogus": 1}}}));
        assert_eq!(got, Some((9, "Unknown $jsonSchema keyword: bogus".into())));
    }

    /// A restriction keyword at the top level has no path and is never
    /// translated, so its divisor is not checked; under a path it is.
    #[test]
    fn multiple_of_nan_refused_only_under_a_path() {
        assert_eq!(refusal(bson!({"multipleOf": f64::NAN})), None);
        let got = refusal(bson!({"properties": {"n": {"multipleOf": f64::NAN}}}));
        assert_eq!(got, Some((2, "divisor cannot be NaN".into())));
    }

    /// `required` names dotted PATHS, which traverse arrays.
    #[test]
    fn required_is_a_dotted_path() {
        let schema = bson!({"required": ["a.b"]});
        assert!(matches(schema.clone(), doc! {"a": [{"b": 1}]}));
        assert!(!matches(schema, doc! {"a.b": 1}));
    }

    /// `multipleOf` is computed in Decimal128, a double converted at 15
    /// significant digits: 0.3 is a multiple of 0.1.
    #[test]
    fn multiple_of_in_decimal() {
        let schema = bson!({"properties": {"n": {"multipleOf": 0.1}}});
        assert!(matches(schema.clone(), doc! {"n": 0.3}));
        assert!(!matches(schema, doc! {"n": 0.35}));
    }

    /// `enum` compares documents whatever their field order.
    #[test]
    fn enum_ignores_field_order() {
        let schema = bson!({"properties": {"a": {"enum": [{"x": 1, "y": 2}]}}});
        assert!(matches(schema, doc! {"a": {"y": 2, "x": 1.0}}));
    }
}
