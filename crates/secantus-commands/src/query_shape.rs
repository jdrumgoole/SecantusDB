//! mongod's query shape and its `queryShapeHash` (RM-358).
//!
//! mongod 8.2 prints `queryShapeHash` on the `Slow query` line, the profile
//! entry and the `explain` of a `find`, a `distinct` and an `aggregate`: the
//! SHA-256 of the command's *representative* query shape, in which every
//! literal is replaced by a value of its type (`1` for any number, `"?"`
//! for a string, ...). Transcribed from 8.2.1's `query_shape/`
//! (`find_cmd_shape.cpp`, `distinct_cmd_shape.cpp`, `agg_cmd_shape.cpp`,
//! `query_shape.cpp`, `serialization_options.cpp`), the match expression
//! parser and serializers (`expression_parser.cpp`, `expression_leaf.cpp`,
//! `expression_tree.cpp`, `expression_array.cpp`), the projection AST
//! serializer and the aggregation expressions' `serialize`, and checked
//! against mongod's `explain` (`tests/mongod_live/test_query_shape_hash.py`).
//!
//! The shape is the expression as mongod PARSES it, before it optimizes it:
//! an `$and`'s children keep the order they were written in, `{$in: [x]}`
//! stays an `$in`, an `$or` of equalities is not rewritten to an `$in`.
//!
//! - `find` hashes a buffer of its own -- `"find"`, a word of option bits,
//!   the namespace, the min, max, sort, collation and `let` -- followed by
//!   the filter and the projection.
//! - `distinct` hashes `"distinct"`, a 16-bit word, the namespace, the key,
//!   the collation, then the query.
//! - `aggregate` hashes its shape's BSON: `{cmdNs, collation, let, command,
//!   pipeline, allowDiskUse}`.
//!
//! Where a query uses something this module does not shape (a geo or text
//! predicate, `$where`, `$jsonSchema`, an aggregation operator or stage
//! outside the lists here), [`hash`] answers `None` and no hash is printed:
//! a wrong hash would be worse than none (`tasks/backlog-rust-mongodb.md`
//! names what is left).

use bson::{doc, oid::ObjectId, spec::BinarySubtype, Binary, Bson, DateTime, Document, Timestamp};
use sha2::{Digest, Sha256};

/// `queryShapeHash` of `request`, run against database `db`: uppercase hex
/// of the SHA-256, or `None` for a command mongod gives no query shape or a
/// query this module cannot shape. An `explain` has the shape of the
/// command it explains.
pub(crate) fn hash(db: &str, request: &Document) -> Option<String> {
    let name = request.keys().next()?.as_str();
    if name == "explain" {
        return hash(db, request.get_document("explain").ok()?);
    }
    // mongod keeps no query shape for a read of an internal database or a
    // system collection (measured 8.2.1, 2026-11-07).
    let coll = request.get_str(name).ok()?;
    if matches!(db, "admin" | "local" | "config") || coll.starts_with("system.") {
        return None;
    }
    let digest = match name {
        "find" => find(db, request)?,
        "distinct" => distinct(db, request)?,
        "aggregate" => aggregate(db, request)?,
        _ => return None,
    };
    Some(digest.iter().map(|b| format!("{b:02X}")).collect())
}

fn bytes(d: &Document) -> Option<Vec<u8>> {
    secantus_core::bsondoc::to_vec(d).ok()
}

fn optional_doc(request: &Document, key: &str) -> Option<Option<Document>> {
    match request.get(key) {
        None | Some(Bson::Null) => Some(None),
        Some(Bson::Document(d)) => Some(Some(d.clone())),
        Some(_) => None,
    }
}

// ------------------------------------------------------------ commands --

/// The options `FindCmdShapeComponents::optionalArgumentsEncoding` packs,
/// in its order: two bits each (set, and true), then skip and limit.
const FIND_FLAGS: [&str; 8] = [
    "singleBatch",
    "allowDiskUse",
    "returnKey",
    "showRecordId",
    "tailable",
    "awaitData",
    "mirrored",
    "oplogReplay",
];

fn find(db: &str, request: &Document) -> Option<[u8; 32]> {
    let coll = request.get_str("find").ok()?;
    let filter = filter_shape(&optional_doc(request, "filter")?.unwrap_or_default())?;
    let projection = match optional_doc(request, "projection")? {
        Some(p) => projection(&p, Mode::Find)?,
        None => Document::new(),
    };
    let sort = match optional_doc(request, "sort")? {
        Some(s) => sort_shape(&s)?,
        None => Document::new(),
    };
    let min = flat_literals(&optional_doc(request, "min")?.unwrap_or_default())?;
    let max = flat_literals(&optional_doc(request, "max")?.unwrap_or_default())?;
    let collation = optional_doc(request, "collation")?.unwrap_or_default();
    let let_shape = let_shape(&optional_doc(request, "let")?.unwrap_or_default())?;

    let mut word: u32 = 0;
    for key in FIND_FLAGS {
        if let Some(v) = request.get(key) {
            word |= if v.as_bool()? { 0b11 } else { 0b10 };
        }
        word <<= 2;
    }
    word |= u32::from(request.contains_key("skip"));
    word |= u32::from(request.contains_key("limit")) << 1;
    // The low bit: the command names its collection, not its UUID.
    let word = (word << 1) | 1;

    let mut buf = b"find".to_vec();
    buf.extend_from_slice(&word.to_le_bytes());
    buf.extend_from_slice(format!("{db}.{coll}").as_bytes());
    for part in [
        &min,
        &max,
        &sort,
        &collation,
        &let_shape,
        &filter,
        &projection,
    ] {
        buf.extend_from_slice(&bytes(part)?);
    }
    Some(Sha256::digest(&buf).into())
}

fn distinct(db: &str, request: &Document) -> Option<[u8; 32]> {
    let coll = request.get_str("distinct").ok()?;
    let key = request.get_str("key").ok()?;
    let query = filter_shape(&optional_doc(request, "query")?.unwrap_or_default())?;
    let collation = optional_doc(request, "collation")?.unwrap_or_default();
    let mut buf = b"distinct".to_vec();
    // A 16-bit options word: 0 for a command naming its collection.
    buf.extend_from_slice(&0_i16.to_le_bytes());
    buf.extend_from_slice(format!("{db}.{coll}").as_bytes());
    buf.push(0);
    buf.extend_from_slice(key.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&bytes(&collation)?);
    buf.extend_from_slice(&bytes(&query)?);
    Some(Sha256::digest(&buf).into())
}

fn aggregate(db: &str, request: &Document) -> Option<[u8; 32]> {
    let coll = request.get_str("aggregate").ok()?;
    let mut pipeline = Vec::new();
    for stage in request.get_array("pipeline").ok()? {
        pipeline.extend(
            stage_shape(stage.as_document()?)?
                .into_iter()
                .map(Bson::Document),
        );
    }
    let mut shape = doc! { "cmdNs": { "db": db, "coll": coll } };
    if let Some(c) = optional_doc(request, "collation")?.filter(|c| !c.is_empty()) {
        shape.insert("collation", c);
    }
    if let Some(l) = optional_doc(request, "let")? {
        shape.insert("let", let_shape(&l)?);
    }
    shape.insert("command", "aggregate");
    shape.insert("pipeline", pipeline);
    if let Some(v) = request.get("allowDiskUse") {
        shape.insert("allowDiskUse", v.as_bool()?);
    }
    Some(Sha256::digest(bytes(&shape)?).into())
}

// ------------------------------------------------------------ literals --

/// mongod's representative value of `v` (`defaultLiteralOfType`).
fn rep(v: &Bson) -> Option<Bson> {
    match v {
        Bson::Array(items) => Some(Bson::Array(array_rep(items.iter())?)),
        other => scalar_rep(other),
    }
}

fn scalar_rep(v: &Bson) -> Option<Bson> {
    Some(match v {
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => Bson::Int32(1),
        Bson::String(_) | Bson::Symbol(_) => Bson::String("?".into()),
        Bson::Document(_) => Bson::Document(doc! { "?": "?" }),
        // An array inside an array is the empty array.
        Bson::Array(_) => Bson::Array(vec![]),
        Bson::Binary(_) => Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![],
        }),
        Bson::ObjectId(_) => Bson::ObjectId(ObjectId::from_bytes([0xff; 12])),
        Bson::Boolean(_) => Bson::Boolean(true),
        Bson::DateTime(_) => Bson::DateTime(DateTime::from_millis(0)),
        Bson::Null => Bson::Null,
        // `BSONRegEx("/\?/")`: the C++ literal's `\?` is a `?`.
        Bson::RegularExpression(_) => Bson::RegularExpression(bson::Regex {
            pattern: "/?/".into(),
            options: String::new(),
        }),
        Bson::JavaScriptCode(_) => Bson::JavaScriptCode("return ?;".into()),
        Bson::JavaScriptCodeWithScope(_) => {
            Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope {
                code: "return ?;".into(),
                scope: Document::new(),
            })
        }
        Bson::Timestamp(_) => Bson::Timestamp(Timestamp {
            time: 0,
            increment: 0,
        }),
        Bson::MinKey => Bson::MinKey,
        Bson::MaxKey => Bson::MaxKey,
        Bson::Undefined => Bson::Undefined,
        // `BSONDBRef("?.?", OID::max())`, which the bson crate cannot build.
        Bson::DbPointer(_) => return None,
    })
}

/// The BSON type byte, which mongod compares to tell one-type arrays from
/// mixed ones.
fn type_byte(v: &Bson) -> u8 {
    v.element_type() as u8
}

/// An array literal's representative: empty, one value of its one type, or
/// mongod's canonical mixed array `[2, "or more types"]`.
fn array_rep<'a>(items: impl Iterator<Item = &'a Bson>) -> Option<Vec<Bson>> {
    let mut first: Option<&Bson> = None;
    for v in items {
        match first {
            None => first = Some(v),
            Some(f) if type_byte(f) != type_byte(v) => {
                return Some(vec![Bson::Int32(2), Bson::String("or more types".into())])
            }
            Some(_) => {}
        }
    }
    Some(match first {
        None => vec![],
        Some(v) => vec![scalar_rep(v)?],
    })
}

/// `min` and `max`: each value a literal (`shapifyFlatObj`).
fn flat_literals(d: &Document) -> Option<Document> {
    d.iter().map(|(k, v)| Some((k.clone(), rep(v)?))).collect()
}

fn sort_shape(d: &Document) -> Option<Document> {
    d.iter()
        .map(|(k, v)| {
            let v = match v {
                Bson::Document(meta) if meta.contains_key("$meta") => Bson::Document(meta.clone()),
                other => Bson::Int32(if number(other)? < 0.0 { -1 } else { 1 }),
            };
            Some((k.clone(), v))
        })
        .collect()
}

fn let_shape(d: &Document) -> Option<Document> {
    d.iter()
        .map(|(k, v)| Some((k.clone(), expr(v, true)?)))
        .collect()
}

/// A number's value, of any width.
fn number(v: &Bson) -> Option<f64> {
    match v {
        Bson::Double(d) => Some(*d),
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        _ => None,
    }
}

/// `BSONElement::trueValue`.
fn true_value(v: &Bson) -> bool {
    match v {
        Bson::Boolean(b) => *b,
        Bson::Null | Bson::Undefined => false,
        Bson::Double(d) => *d != 0.0,
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Decimal128(d) => d.to_string().parse::<f64>() != Ok(0.0),
        _ => true,
    }
}

// --------------------------------------------------- match expressions --

/// A match expression as mongod's parser builds it.
enum Node {
    /// A path's predicate: `{path: rhs}`.
    Leaf {
        path: String,
        rhs: Document,
    },
    And(Vec<Node>),
    Or(Vec<Node>),
    Nor(Vec<Node>),
    Not(Box<Node>),
    /// `{path: {$elemMatch: {<operators>}}}`.
    ElemValue {
        path: String,
        subs: Vec<Node>,
    },
    /// `{path: {$elemMatch: <query>}}`.
    ElemObject {
        path: String,
        sub: Box<Node>,
    },
    /// A pathless predicate, serialized as it stands.
    Raw(Document),
}

/// The representative shape of a filter (`MatchExpression::serialize`).
pub(crate) fn filter_shape(filter: &Document) -> Option<Document> {
    Some(serialize(&parse(filter)?, true))
}

/// The operators that take no path (`retrievePathlessParser`), which make
/// an `$elemMatch` a query on the element rather than on its value.
const PATHLESS: &[&str] = &[
    "$and",
    "$or",
    "$nor",
    "$expr",
    "$where",
    "$text",
    "$comment",
    "$alwaysTrue",
    "$alwaysFalse",
    "$jsonSchema",
    "$sampleRate",
];

fn is_expression_doc(v: &Bson) -> bool {
    let Some(d) = v.as_document() else {
        return false;
    };
    match d.keys().next() {
        Some(k) if k == "$ref" => !d.contains_key("$id"),
        Some(k) => k.starts_with('$'),
        None => false,
    }
}

fn parse(obj: &Document) -> Option<Node> {
    let mut root = Vec::new();
    for (k, v) in obj {
        if k.starts_with('$') {
            if let Some(node) = pathless(k, v)? {
                root.push(node);
            }
        } else if is_expression_doc(v) {
            parse_sub(k, v.as_document()?, &mut root)?;
        } else if let Bson::RegularExpression(re) = v {
            root.push(regex_leaf(k, &re.options));
        } else {
            root.push(comparison(k, "$eq", v)?);
        }
    }
    Some(if root.len() == 1 {
        root.pop()?
    } else {
        Node::And(root)
    })
}

/// A pathless operator: `Some(None)` for one the parser drops (`$comment`).
fn pathless(op: &str, v: &Bson) -> Option<Option<Node>> {
    let list = |v: &Bson| -> Option<Vec<Node>> {
        let items = v.as_array().filter(|a| !a.is_empty())?;
        items.iter().map(|i| parse(i.as_document()?)).collect()
    };
    Some(Some(match op {
        "$and" => Node::And(list(v)?),
        "$or" => Node::Or(list(v)?),
        "$nor" => Node::Nor(list(v)?),
        "$comment" => return Some(None),
        "$alwaysTrue" | "$alwaysFalse" => Node::Raw(doc! { op: 1 }),
        "$expr" => Node::Raw(doc! { "$expr": expr(v, true)? }),
        _ => return None,
    }))
}

fn parse_sub(path: &str, sub: &Document, root: &mut Vec<Node>) -> Option<()> {
    for (op, v) in sub {
        if let Some(node) = sub_field(path, op, v, sub)? {
            root.push(node);
        }
    }
    Some(())
}

fn leaf(path: &str, op: &str, value: Bson) -> Node {
    Node::Leaf {
        path: path.to_string(),
        rhs: doc! { op: value },
    }
}

fn regex_leaf(path: &str, options: &str) -> Node {
    // `"\\?"` and `"i"`: a representative that parses as a regex.
    let mut rhs = doc! { "$regex": "\\?" };
    if !options.is_empty() {
        rhs.insert("$options", "i");
    }
    Node::Leaf {
        path: path.to_string(),
        rhs,
    }
}

fn comparison(path: &str, op: &str, v: &Bson) -> Option<Node> {
    if matches!(v, Bson::Undefined) || (op != "$eq" && matches!(v, Bson::RegularExpression(_))) {
        return None;
    }
    Some(leaf(path, op, rep(v)?))
}

fn sub_field(path: &str, op: &str, v: &Bson, context: &Document) -> Option<Option<Node>> {
    Some(Some(match op {
        "$not" => match v {
            Bson::RegularExpression(re) => Node::Not(Box::new(regex_leaf(path, &re.options))),
            Bson::Document(d) if !d.is_empty() => {
                let mut inner = Vec::new();
                parse_sub(path, d, &mut inner)?;
                Node::Not(Box::new(Node::And(inner)))
            }
            _ => return None,
        },
        "$eq" | "$lt" | "$lte" | "$gt" | "$gte" => comparison(path, op, v)?,
        "$ne" if matches!(v, Bson::RegularExpression(_)) => return None,
        "$ne" => Node::Not(Box::new(comparison(path, "$eq", v)?)),
        "$in" | "$nin" => {
            let node = leaf(path, "$in", Bson::Array(in_rep(v.as_array()?)?));
            if op == "$in" {
                node
            } else {
                Node::Not(Box::new(node))
            }
        }
        "$exists" => {
            let node = leaf(path, "$exists", Bson::Boolean(true));
            if true_value(v) {
                node
            } else {
                Node::Not(Box::new(node))
            }
        }
        "$size" => {
            let n = number(v)?;
            if n < 0.0 || n.fract() != 0.0 {
                return None;
            }
            leaf(path, "$size", Bson::Int32(1))
        }
        "$mod" => {
            if v.as_array().map(Vec::len) != Some(2) {
                return None;
            }
            leaf(
                path,
                "$mod",
                Bson::Array(vec![Bson::Int32(1), Bson::Int32(1)]),
            )
        }
        "$type" => leaf(path, "$type", Bson::Array(type_set(v)?)),
        "$options" if context.contains_key("$regex") => return Some(None),
        "$regex" => {
            let options = match (context.get("$options"), v) {
                (Some(Bson::String(o)), _) => o.clone(),
                (None, Bson::RegularExpression(re)) => re.options.clone(),
                (None, Bson::String(_)) => String::new(),
                _ => return None,
            };
            regex_leaf(path, &options)
        }
        "$elemMatch" => {
            let d = v.as_document()?;
            let first = d.keys().next();
            let value_form =
                is_expression_doc(v) && first.is_some_and(|k| !PATHLESS.contains(&k.as_str()));
            if value_form {
                let mut subs = Vec::new();
                parse_sub("", d, &mut subs)?;
                Node::ElemValue {
                    path: path.to_string(),
                    subs,
                }
            } else {
                Node::ElemObject {
                    path: path.to_string(),
                    sub: Box::new(parse(d)?),
                }
            }
        }
        "$all" => all(path, v.as_array()?)?,
        "$bitsAllSet" | "$bitsAllClear" | "$bitsAnySet" | "$bitsAnyClear" => {
            leaf(path, op, Bson::Array(bits_rep(v)?))
        }
        _ => return None,
    }))
}

fn all(path: &str, items: &[Bson]) -> Option<Node> {
    let elem_match = |v: &Bson| {
        v.as_document()
            .and_then(|d| d.keys().next())
            .is_some_and(|k| k == "$elemMatch")
    };
    let mut kids = Vec::new();
    if items.first().is_some_and(elem_match) {
        for item in items {
            let d = item.as_document().filter(|_| elem_match(item))?;
            kids.push(sub_field(path, "$elemMatch", d.get("$elemMatch")?, d)??);
        }
        return Some(Node::And(kids));
    }
    for item in items {
        match item {
            Bson::RegularExpression(re) => kids.push(regex_leaf(path, &re.options)),
            Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => {
                return None
            }
            other => kids.push(comparison(path, "$eq", other)?),
        }
    }
    if kids.is_empty() {
        return Some(Node::Raw(doc! { "$alwaysFalse": 1 }));
    }
    Some(Node::And(kids))
}

/// `$in`'s representative: the first value of each canonical type (a
/// number of any width is one type), with a regex standing for its
/// regexes, then that list as an array literal.
fn in_rep(items: &[Bson]) -> Option<Vec<Bson>> {
    let mut kinds: Vec<u8> = Vec::new();
    let mut first: Option<&Bson> = None;
    let mut regex = false;
    for v in items {
        let kind = match v {
            Bson::RegularExpression(_) => {
                regex = true;
                continue;
            }
            Bson::Undefined => return None,
            Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => {
                return None
            }
            Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => 0,
            Bson::Symbol(_) => type_byte(&Bson::String(String::new())),
            other => type_byte(other),
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
            first.get_or_insert(v);
        }
    }
    Some(match (kinds.len() + usize::from(regex), first) {
        (0, _) => vec![],
        (1, Some(v)) => vec![scalar_rep(v)?],
        (1, None) => vec![scalar_rep(&Bson::RegularExpression(bson::Regex {
            pattern: String::new(),
            options: String::new(),
        }))?],
        _ => vec![Bson::Int32(2), Bson::String("or more types".into())],
    })
}

/// The bit positions of a bit test as an array literal: empty, or `[1]`.
fn bits_rep(v: &Bson) -> Option<Vec<Bson>> {
    let any = match v {
        Bson::Array(items) => {
            for i in items {
                let n = number(i)?;
                if n < 0.0 || n.fract() != 0.0 {
                    return None;
                }
            }
            !items.is_empty()
        }
        Bson::Binary(b) => b.bytes.iter().any(|byte| *byte != 0),
        other => {
            let n = number(other)?;
            if n < 0.0 || n.fract() != 0.0 {
                return None;
            }
            n != 0.0
        }
    };
    Some(if any { vec![Bson::Int32(1)] } else { vec![] })
}

/// `$type`'s set as mongod prints it: `"number"` first if it was asked
/// for, then the type codes in order.
fn type_set(v: &Bson) -> Option<Vec<Bson>> {
    let items: Vec<&Bson> = match v {
        Bson::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut numbers = false;
    let mut codes: Vec<i32> = Vec::new();
    for item in items {
        let code = match item {
            Bson::String(s) if s == "number" => {
                numbers = true;
                continue;
            }
            Bson::String(s) => type_code(s)?,
            other => {
                let n = number(other)?;
                if n.fract() != 0.0 {
                    return None;
                }
                n as i32
            }
        };
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes.sort_unstable();
    let mut out = Vec::new();
    if numbers {
        out.push(Bson::String("number".into()));
    }
    out.extend(codes.into_iter().map(Bson::Int32));
    Some(out)
}

fn type_code(alias: &str) -> Option<i32> {
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

fn serialize(node: &Node, include_path: bool) -> Document {
    let list = |kids: &[Node]| -> Bson {
        Bson::Array(
            kids.iter()
                .map(|k| Bson::Document(serialize(k, include_path)))
                .collect(),
        )
    };
    match node {
        Node::Leaf { path, rhs } => {
            if include_path {
                doc! { path: rhs.clone() }
            } else {
                rhs.clone()
            }
        }
        Node::ElemValue { path, .. } | Node::ElemObject { path, .. } => {
            if include_path {
                doc! { path: right_hand_side(node) }
            } else {
                right_hand_side(node)
            }
        }
        Node::And(kids) if kids.is_empty() => Document::new(),
        Node::And(kids) => doc! { "$and": list(kids) },
        Node::Or(kids) if kids.is_empty() => doc! { "$alwaysFalse": 1 },
        Node::Or(kids) => doc! { "$or": list(kids) },
        Node::Nor(kids) => doc! { "$nor": list(kids) },
        Node::Raw(d) => d.clone(),
        Node::Not(child) => not(child, include_path),
    }
}

fn not(child: &Node, include_path: bool) -> Document {
    if matches!(child, Node::And(kids) if kids.is_empty()) {
        return doc! { "$alwaysFalse": 1 };
    }
    if !include_path {
        let inner = match child {
            Node::And(kids) => merged(kids),
            other => serialize(other, false),
        };
        return doc! { "$not": inner };
    }
    let target = match child {
        Node::And(kids) if kids.len() == 1 => &kids[0],
        other => other,
    };
    match target {
        Node::Leaf { path, .. } | Node::ElemValue { path, .. } | Node::ElemObject { path, .. }
            if !path.starts_with('$') =>
        {
            doc! { path: { "$not": right_hand_side(target) } }
        }
        other => doc! { "$nor": [serialize(other, true)] },
    }
}

fn merged(kids: &[Node]) -> Document {
    let mut out = Document::new();
    for k in kids {
        out.extend(serialize(k, false));
    }
    out
}

fn right_hand_side(node: &Node) -> Document {
    match node {
        Node::Leaf { rhs, .. } => rhs.clone(),
        Node::ElemValue { subs, .. } => doc! { "$elemMatch": merged(subs) },
        Node::ElemObject { sub, .. } => doc! { "$elemMatch": serialize(sub, true) },
        other => serialize(other, true),
    }
}

// ----------------------------------------------- aggregation expressions --

/// The operators whose `serialize` is `ExpressionNary`'s: `{$op: [args]}`,
/// a constant argument as its bare representative.
const NARY: &[&str] = &[
    "$abs",
    "$add",
    "$allElementsTrue",
    "$and",
    "$anyElementTrue",
    "$arrayElemAt",
    "$arrayToObject",
    "$binarySize",
    "$bitNot",
    "$bsonSize",
    "$ceil",
    "$cmp",
    "$concat",
    "$concatArrays",
    "$cond",
    "$divide",
    "$eq",
    "$exp",
    "$first",
    "$floor",
    "$gt",
    "$gte",
    "$ifNull",
    "$in",
    "$indexOfArray",
    "$indexOfBytes",
    "$indexOfCP",
    "$isArray",
    "$isNumber",
    "$last",
    "$ln",
    "$log",
    "$log10",
    "$lt",
    "$lte",
    "$mod",
    "$multiply",
    "$ne",
    "$not",
    "$objectToArray",
    "$or",
    "$pow",
    "$range",
    "$reverseArray",
    "$round",
    "$setDifference",
    "$setEquals",
    "$setIntersection",
    "$setIsSubset",
    "$setUnion",
    "$size",
    "$slice",
    "$split",
    "$sqrt",
    "$strLenBytes",
    "$strLenCP",
    "$strcasecmp",
    "$substrBytes",
    "$substrCP",
    "$subtract",
    "$toLower",
    "$toUpper",
    "$trunc",
    "$tsIncrement",
    "$tsSecond",
    "$type",
    "$sum",
    "$avg",
    "$max",
    "$min",
    "$stdDevPop",
    "$stdDevSamp",
];

fn operator(d: &Document) -> Option<(&str, &Bson)> {
    let (k, v) = d.iter().next()?;
    k.starts_with('$').then_some((k.as_str(), v))
}

/// Whether `v` parses to an `ExpressionConstant`.
fn is_constant(v: &Bson) -> bool {
    match v {
        Bson::String(s) => !s.starts_with('$'),
        Bson::Document(d) => operator(d).is_some_and(|(op, _)| op == "$literal"),
        Bson::Array(_) => false,
        _ => true,
    }
}

/// Whether `v` and everything in it are constants
/// (`selfAndChildrenAreConstant`).
fn all_constant(v: &Bson) -> bool {
    match v {
        Bson::Document(d) if operator(d).is_some() => is_constant(v),
        Bson::Document(d) => d.values().all(all_constant),
        Bson::Array(a) => a.iter().all(all_constant),
        other => is_constant(other),
    }
}

/// The value an all-constant expression evaluates to.
fn constant_value(v: &Bson) -> Bson {
    match v {
        Bson::Document(d) if operator(d).is_some_and(|(op, _)| op == "$literal") => {
            d.get("$literal").cloned().unwrap_or(Bson::Null)
        }
        Bson::Document(_) => Bson::Document(Document::new()),
        Bson::Array(a) => Bson::Array(a.iter().map(constant_value).collect()),
        other => other.clone(),
    }
}

/// `ExpressionConstant::serializeConstant`.
fn constant(v: &Bson, wrap: bool) -> Option<Bson> {
    let r = rep(v)?;
    Some(if wrap {
        Bson::Document(doc! { "$const": r })
    } else {
        r
    })
}

/// An aggregation expression's representative shape.
fn expr(v: &Bson, wrap: bool) -> Option<Bson> {
    match v {
        Bson::String(s) if s.starts_with('$') => (s.len() > 1).then(|| v.clone()),
        Bson::Document(d) if operator(d).is_some() => {
            let (op, arg) = operator(d)?;
            if d.len() != 1 {
                return None;
            }
            if op == "$literal" {
                return constant(arg, wrap);
            }
            if !NARY.contains(&op) {
                return None;
            }
            let args: Vec<&Bson> = match arg {
                Bson::Array(a) => a.iter().collect(),
                other => vec![other],
            };
            let out = args
                .into_iter()
                .map(|a| {
                    if is_constant(a) {
                        constant(&constant_value(a), false)
                    } else {
                        expr(a, true)
                    }
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Bson::Document(doc! { op: out }))
        }
        Bson::Document(_) | Bson::Array(_) if all_constant(v) => constant(&constant_value(v), wrap),
        Bson::Document(d) => d
            .iter()
            .map(|(k, x)| Some((k.clone(), expr(x, true)?)))
            .collect::<Option<Document>>()
            .map(Bson::Document),
        Bson::Array(a) => a
            .iter()
            .map(|x| expr(x, true))
            .collect::<Option<Vec<_>>>()
            .map(Bson::Array),
        other => constant(other, wrap),
    }
}

// --------------------------------------------------------- projections --

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// A `find`'s projection.
    Find,
    /// `$project`.
    Project,
    /// `$addFields` / `$set`: every value an expression.
    AddFields,
}

enum PNode {
    Sub(Vec<(String, PNode)>),
    Bool(bool),
    Slice(Bson),
    ElemMatch(Document),
    Expr(Bson),
}

#[derive(Default)]
struct Kind {
    inclusion: bool,
    exclusion: bool,
}

fn child<'a>(tree: &'a mut Vec<(String, PNode)>, name: &str) -> &'a mut Vec<(String, PNode)> {
    let at = match tree.iter().position(|(k, _)| k == name) {
        Some(at) => at,
        None => {
            tree.push((name.to_string(), PNode::Sub(Vec::new())));
            tree.len() - 1
        }
    };
    if !matches!(tree[at].1, PNode::Sub(_)) {
        tree[at].1 = PNode::Sub(Vec::new());
    }
    match &mut tree[at].1 {
        PNode::Sub(sub) => sub,
        _ => unreachable!("made a sub-tree above"),
    }
}

fn build(
    spec: &Document,
    mode: Mode,
    top: bool,
    tree: &mut Vec<(String, PNode)>,
    kind: &mut Kind,
) -> Option<()> {
    for (key, v) in spec {
        let (positional, key) = match key.strip_suffix(".$") {
            Some(k) if mode == Mode::Find => (true, k),
            _ => (false, key.as_str()),
        };
        let mut parts: Vec<&str> = key.split('.').collect();
        let last = parts.pop()?;
        let mut node = &mut *tree;
        for p in parts {
            node = child(node, p);
        }
        if positional {
            kind.inclusion = true;
            node.push((format!("{last}.$"), PNode::Bool(true)));
            continue;
        }
        let value = match v {
            Bson::Boolean(_)
            | Bson::Int32(_)
            | Bson::Int64(_)
            | Bson::Double(_)
            | Bson::Decimal128(_)
                if mode != Mode::AddFields =>
            {
                let on = true_value(v);
                if !(top && key == "_id") {
                    if on {
                        kind.inclusion = true;
                    } else {
                        kind.exclusion = true;
                    }
                }
                PNode::Bool(on)
            }
            Bson::Document(d) if operator(d).is_some() => match (mode, operator(d)?.0) {
                (Mode::Find, "$slice") => {
                    PNode::Slice(d.get("$slice").cloned().unwrap_or(Bson::Null))
                }
                (Mode::Find, "$elemMatch") => {
                    kind.inclusion = true;
                    PNode::ElemMatch(d.get_document("$elemMatch").ok()?.clone())
                }
                _ => {
                    kind.inclusion = true;
                    PNode::Expr(v.clone())
                }
            },
            Bson::Document(d) if !d.is_empty() => {
                let sub = child(node, last);
                build(d, mode, false, sub, kind)?;
                continue;
            }
            Bson::Document(_) => return None,
            other => {
                kind.inclusion = true;
                PNode::Expr(other.clone())
            }
        };
        match node.iter().position(|(k, _)| k == last) {
            Some(at) => node[at].1 = value,
            None => node.push((last.to_string(), value)),
        }
    }
    Some(())
}

fn emit(tree: &[(String, PNode)]) -> Option<Document> {
    tree.iter()
        .map(|(k, v)| {
            let value = match v {
                PNode::Sub(sub) => Bson::Document(emit(sub)?),
                PNode::Bool(b) => Bson::Boolean(*b),
                PNode::Slice(Bson::Array(_)) => Bson::Array(vec![Bson::Int32(1), Bson::Int32(1)]),
                PNode::Slice(_) => Bson::Int32(1),
                PNode::ElemMatch(q) => Bson::Document(doc! { "$elemMatch": filter_shape(q)? }),
                PNode::Expr(e) => expr(e, true)?,
            };
            let value = match v {
                PNode::Slice(_) => Bson::Document(doc! { "$slice": value }),
                _ => value,
            };
            Some((k.clone(), value))
        })
        .collect()
}

fn projection(spec: &Document, mode: Mode) -> Option<Document> {
    let mut tree = Vec::new();
    let mut kind = Kind::default();
    build(spec, mode, true, &mut tree, &mut kind)?;
    if kind.inclusion && kind.exclusion && mode != Mode::AddFields {
        return None;
    }
    let has_id = tree.iter().any(|(k, _)| k == "_id");
    match mode {
        Mode::Find if kind.inclusion && !has_id => tree.push(("_id".into(), PNode::Bool(true))),
        Mode::Project if !has_id && kind.inclusion => {
            tree.insert(0, ("_id".into(), PNode::Bool(true)));
        }
        Mode::Project if !has_id => tree.push(("_id".into(), PNode::Bool(true))),
        _ => {}
    }
    emit(&tree)
}

// -------------------------------------------------------------- stages --

const ACCUMULATORS: &[&str] = &[
    "$sum",
    "$avg",
    "$min",
    "$max",
    "$first",
    "$last",
    "$push",
    "$addToSet",
    "$stdDevPop",
    "$stdDevSamp",
];

fn count_of_one() -> Document {
    doc! { "$sum": { "$const": 1 } }
}

/// A stage's representative shape, as the stages mongod parses it into
/// (`$count` is a `$group` and a `$project`).
fn stage_shape(stage: &Document) -> Option<Vec<Document>> {
    if stage.len() != 1 {
        return None;
    }
    let (name, spec) = stage.iter().next()?;
    let name = name.as_str();
    Some(match name {
        "$match" => vec![doc! { "$match": filter_shape(spec.as_document()?)? }],
        "$limit" | "$skip" => {
            number(spec)?;
            vec![doc! { name: 1 }]
        }
        "$sort" => vec![doc! { "$sort": sort_shape(spec.as_document()?)? }],
        "$count" => {
            let field = spec.as_str()?;
            vec![
                doc! { "$group": { "_id": { "$const": Bson::Null }, field: count_of_one() } },
                doc! { "$project": { field: true, "_id": false } },
            ]
        }
        "$group" => {
            let spec = spec.as_document()?;
            let mut out = doc! { "_id": expr(spec.get("_id")?, true)? };
            for (k, v) in spec.iter().filter(|(k, _)| *k != "_id") {
                let (op, arg) = operator(v.as_document()?)?;
                let acc = match op {
                    "$count" => count_of_one(),
                    _ if ACCUMULATORS.contains(&op) => doc! { op: expr(arg, true)? },
                    _ => return None,
                };
                out.insert(k.clone(), acc);
            }
            vec![doc! { "$group": out }]
        }
        "$project" => vec![doc! { "$project": projection(spec.as_document()?, Mode::Project)? }],
        "$addFields" | "$set" => {
            vec![doc! { name: projection(spec.as_document()?, Mode::AddFields)? }]
        }
        "$unset" => {
            let fields: Vec<&str> = match spec {
                Bson::String(s) => vec![s.as_str()],
                Bson::Array(a) => a.iter().map(Bson::as_str).collect::<Option<_>>()?,
                _ => return None,
            };
            let excluded: Document = fields
                .into_iter()
                .map(|f| (f.to_string(), Bson::Int32(0)))
                .collect();
            vec![doc! { "$project": projection(&excluded, Mode::Project)? }]
        }
        "$unwind" => {
            let mut out = Document::new();
            match spec {
                Bson::String(path) => {
                    out.insert("path", path.clone());
                }
                Bson::Document(d) => {
                    out.insert("path", d.get_str("path").ok()?);
                    if d.get("preserveNullAndEmptyArrays").is_some_and(true_value) {
                        out.insert("preserveNullAndEmptyArrays", true);
                    }
                    if let Some(index) = d.get("includeArrayIndex") {
                        out.insert("includeArrayIndex", index.as_str()?);
                    }
                }
                _ => return None,
            }
            vec![doc! { "$unwind": out }]
        }
        "$replaceRoot" => {
            let root = spec.as_document()?.get("newRoot")?;
            vec![doc! { "$replaceRoot": { "newRoot": expr(root, true)? } }]
        }
        "$replaceWith" => vec![doc! { "$replaceRoot": { "newRoot": expr(spec, true)? } }],
        "$sample" => {
            number(spec.as_document()?.get("size")?)?;
            vec![doc! { "$sample": { "size": 1 } }]
        }
        "$sortByCount" => vec![
            doc! { "$group": { "_id": expr(spec, true)?, "count": count_of_one() } },
            doc! { "$sort": { "count": -1 } },
        ],
        "$lookup" => {
            let d = spec.as_document()?;
            let mut keys: Vec<&str> = d.keys().map(String::as_str).collect();
            keys.sort_unstable();
            if keys != ["as", "foreignField", "from", "localField"] {
                return None;
            }
            vec![doc! { "$lookup": {
                "from": d.get_str("from").ok()?,
                "as": d.get_str("as").ok()?,
                "localField": d.get_str("localField").ok()?,
                "foreignField": d.get_str("foreignField").ok()?,
            } }]
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hashes mongod 8.2.1 printed for these commands in database
    /// `qsh` (`explain`, 2026-11-07).
    #[test]
    fn hashes_match_mongods() {
        let find = doc! { "find": "c", "filter": { "x": { "$gt": 5 } } };
        assert_eq!(
            hash("qsh", &find).as_deref(),
            Some("F50F92F1AF9EA65463AAB3B676868D96E47EF9F1B3DEF87DF0888D0B0E4DCFCC")
        );
        let distinct = doc! { "distinct": "c", "key": "x", "query": { "x": { "$gt": 5 } } };
        assert_eq!(
            hash("qsh", &distinct).as_deref(),
            Some("F207FE3A93F54D86CE9804A6945C77EF507CD94256961254553C04B23993E7C2")
        );
        let aggregate = doc! {
            "aggregate": "c", "pipeline": [{ "$match": { "x": { "$gt": 5 } } }], "cursor": {}
        };
        assert_eq!(
            hash("qsh", &aggregate).as_deref(),
            Some("5A676B3B7E5DB490E031C8D55A26F49DA3A474FE04D1B6B81F87438A69911ADB")
        );
    }

    #[test]
    fn the_shape_is_the_parse_not_the_optimized_tree() {
        assert_eq!(
            filter_shape(&doc! { "y": 5, "x": { "$in": [5] } }),
            Some(doc! { "$and": [{ "y": { "$eq": 1 } }, { "x": { "$in": [1] } }] })
        );
        assert_eq!(
            filter_shape(&doc! { "x": { "$ne": 3, "$exists": false } }),
            Some(doc! { "$and": [
                { "x": { "$not": { "$eq": 1 } } },
                { "x": { "$not": { "$exists": true } } },
            ] })
        );
        assert_eq!(
            filter_shape(&doc! { "x": { "$in": [3, "a"] } }),
            Some(doc! { "x": { "$in": [2, "or more types"] } })
        );
    }

    #[test]
    fn what_it_cannot_shape_has_no_hash() {
        assert_eq!(
            hash("d", &doc! { "find": "c", "filter": { "$where": "1" } }),
            None
        );
        assert_eq!(
            hash(
                "d",
                &doc! { "aggregate": "c", "pipeline": [{ "$facet": {} }], "cursor": {} }
            ),
            None
        );
        assert_eq!(hash("d", &doc! { "count": "c" }), None);
        let find = doc! { "find": "c", "filter": { "x": 1 } };
        assert_eq!(hash("admin", &find), None);
        assert_eq!(hash("d", &doc! { "find": "system.profile" }), None);
        assert_eq!(
            hash("d", &doc! { "explain": find.clone() }),
            hash("d", &find)
        );
    }
}
