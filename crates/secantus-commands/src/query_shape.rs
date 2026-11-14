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
//! Many nodes serialize their own way, each transcribed from its mongod
//! serializer and checked against mongod 8.2.11: the operators with named
//! arguments (a constant argument as `{$const: <representative>}`, where
//! an `ExpressionNary`'s is bare), geo predicates (representatives that are
//! valid shapes), and the stages mongod desugars into others (`$bucket`,
//! `$setWindowFields`, `$fill`, `$densify`, `$documents`, `$score`).
//! `$rankFusion` and `$scoreFusion` are the pipelines mongod desugars them
//! into (`document_source_rank_fusion.cpp`,
//! `document_source_score_fusion.cpp`).
//!
//! Where a query uses something this module does not shape (`$jsonSchema`,
//! a hybrid search over `$search`, an option no one has measured), [`hash`]
//! answers `None` and no hash is printed: a wrong hash would be worse than
//! none (`tasks/backlog-rust-mongodb.md` names what is left).

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
    let coll = match request.get(name)? {
        Bson::String(coll) => coll.as_str(),
        // A collectionless `aggregate: 1` runs on `<db>.$cmd.aggregate`.
        one if name == "aggregate" && number(one) == Some(1.0) => "$cmd.aggregate",
        _ => return None,
    };
    if matches!(db, "admin" | "local" | "config") || coll.starts_with("system.") {
        return None;
    }
    let digest = match name {
        "find" => find(db, request)?,
        "distinct" => distinct(db, request)?,
        "aggregate" => aggregate(db, coll, request)?,
        _ => return None,
    };
    Some(digest.iter().map(|b| format!("{b:02X}")).collect())
}

/// A query as `$queryStats` keeps it: its shape twice -- with
/// representative literals, which the entry's `keyHash` covers, and with
/// debug type strings, which `$queryStats` prints -- and its
/// `queryShapeHash`.
pub(crate) struct StatsShape {
    pub representative: Document,
    pub debug: Document,
    pub hash: String,
}

/// The shape a `find`, `distinct`, `count` or `aggregate` against database
/// `db` is recorded under in the query stats store (RM-215). Unlike
/// [`hash`] this shapes a read of an internal database or a system
/// collection too: mongod 8.2.11 records those.
pub(crate) fn stats_shape(db: &str, request: &Document) -> Option<StatsShape> {
    let (name, coll) = stats_namespace(request)?;
    let digest = match name {
        "find" => find(db, request)?,
        "distinct" => distinct(db, request)?,
        "count" => count(db, request)?,
        "aggregate" => aggregate(db, coll, request)?,
        _ => return None,
    };
    let representative = shape_doc(db, coll, name, request)?;
    let debug = with_debug(|| shape_doc(db, coll, name, request))?;
    Some(StatsShape {
        representative,
        debug,
        hash: digest.iter().map(|b| format!("{b:02X}")).collect(),
    })
}

/// The command's name and the collection its shape names.
fn stats_namespace(request: &Document) -> Option<(&str, &str)> {
    let name = request.keys().next()?.as_str();
    let coll = match request.get(name)? {
        Bson::String(coll) => coll.as_str(),
        one if name == "aggregate" && number(one) == Some(1.0) => "$cmd.aggregate",
        _ => return None,
    };
    Some((name, coll))
}

/// The debug shape [`stats_shape`] keeps, made again: `$queryStats` prints
/// it under [`with_hmac`] when `transformIdentifiers` asks.
pub(crate) fn debug_shape(db: &str, request: &Document) -> Option<Document> {
    let (name, coll) = stats_namespace(request)?;
    with_debug(|| shape_doc(db, coll, name, request))
}

/// A literal outside the shape -- a `$queryStats` key's `comment`,
/// `maxTimeMS` or `batchSize` -- as its representative, or its debug type
/// string when `debug`.
pub(crate) fn literal(v: &Bson, debug_form: bool) -> Option<Bson> {
    if debug_form {
        with_debug(|| rep(v))
    } else {
        rep(v)
    }
}

/// The query shape as a document (`Shape::toBson`): `cmdNs`, a non-empty
/// collation, then what the command's own components append.
fn shape_doc(db: &str, coll: &str, name: &str, request: &Document) -> Option<Document> {
    let mut shape = doc! { "cmdNs": { "db": ident(db), "coll": ident(coll) } };
    if let Some(c) = optional_doc(request, "collation")?.filter(|c| !c.is_empty()) {
        shape.insert("collation", c);
    }
    match name {
        "find" => {
            // `FindCmdShapeComponents::appendTo`.
            if let Some(l) = optional_doc(request, "let")? {
                shape.insert("let", let_shape(&l)?);
            }
            shape.insert("command", "find");
            let filter = optional_doc(request, "filter")?.unwrap_or_default();
            shape.insert("filter", filter_shape(&filter)?);
            if let Some(p) = find_projection(request)? {
                shape.insert("projection", p);
            }
            for key in ["max", "min"] {
                if let Some(d) = optional_doc(request, key)?.filter(|d| !d.is_empty()) {
                    shape.insert(key, flat_literals(&d)?);
                }
            }
            if let Some(d) = optional_doc(request, "sort")?.filter(|d| !d.is_empty()) {
                shape.insert("sort", sort_shape(&d)?);
            }
            for key in ["limit", "skip"] {
                if request.contains_key(key) {
                    shape.insert(key, rep(&Bson::Int64(1))?);
                }
            }
            for key in FIND_FLAGS {
                if let Some(v) = request.get(key) {
                    shape.insert(key, v.as_bool()?);
                }
            }
        }
        "distinct" => {
            shape.insert("command", "distinct");
            shape.insert("key", field_path(request.get_str("key").ok()?));
            let query = optional_doc(request, "query")?.unwrap_or_default();
            let query = filter_shape(&query)?;
            if !query.is_empty() {
                shape.insert("query", query);
            }
        }
        "count" => {
            shape.insert("command", "count");
            let query = optional_doc(request, "query")?.unwrap_or_default();
            let query = filter_shape(&query)?;
            if !query.is_empty() {
                shape.insert("query", query);
            }
            for key in ["limit", "skip"] {
                if request.contains_key(key) {
                    shape.insert(key, rep(&Bson::Int64(1))?);
                }
            }
        }
        "aggregate" => {
            if let Some(l) = optional_doc(request, "let")? {
                shape.insert("let", let_shape(&l)?);
            }
            shape.insert("command", "aggregate");
            shape.insert(
                "pipeline",
                pipeline_shape(db, coll, request.get_array("pipeline").ok()?)?,
            );
            if let Some(v) = request.get("allowDiskUse") {
                shape.insert("allowDiskUse", v.as_bool()?);
            }
        }
        _ => return None,
    }
    Some(shape)
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

/// A `find`'s projection shape, if it has one. `showRecordId` asks it
/// for the record id, ahead of an included `_id` (measured 8.2.11).
fn find_projection(request: &Document) -> Option<Option<Document>> {
    let mut shown = match optional_doc(request, "projection")?.filter(|p| !p.is_empty()) {
        Some(p) => Some(projection(&p, Mode::Find)?),
        None => None,
    };
    if request.get("showRecordId").and_then(Bson::as_bool) == Some(true) {
        let p = shown.get_or_insert_with(Document::new);
        // Under `transformIdentifiers` both names are HMACs (measured
        // 8.2.11: `$recordId` as one identifier).
        let id_key = field_path("_id");
        let id = (p.keys().last() == Some(&id_key))
            .then(|| p.remove(&id_key))
            .flatten();
        p.insert(field_path("$recordId"), doc! { "$meta": "recordId" });
        if let Some(id) = id {
            p.insert(id_key, id);
        }
    }
    Some(shown)
}

fn find(db: &str, request: &Document) -> Option<[u8; 32]> {
    let coll = request.get_str("find").ok()?;
    let filter = filter_shape(&optional_doc(request, "filter")?.unwrap_or_default())?;
    let projection = find_projection(request)?.unwrap_or_default();
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

fn aggregate(db: &str, coll: &str, request: &Document) -> Option<[u8; 32]> {
    let shape = shape_doc(db, coll, "aggregate", request)?;
    Some(Sha256::digest(bytes(&shape)?).into())
}

/// `CountCmdShape::sha256Hash`: `"count"`, a 16-bit options word, the
/// namespace, the collation, a byte of skip (bit 0) and limit (bit 1),
/// then the representative query.
fn count(db: &str, request: &Document) -> Option<[u8; 32]> {
    let coll = request.get_str("count").ok()?;
    let query = filter_shape(&optional_doc(request, "query")?.unwrap_or_default())?;
    let collation = optional_doc(request, "collation")?.unwrap_or_default();
    let mut buf = b"count".to_vec();
    buf.extend_from_slice(&0_i16.to_le_bytes());
    buf.extend_from_slice(format!("{db}.{coll}").as_bytes());
    buf.extend_from_slice(&bytes(&collation)?);
    buf.push(
        u8::from(request.contains_key("skip")) | (u8::from(request.contains_key("limit")) << 1),
    );
    buf.extend_from_slice(&bytes(&query)?);
    Some(Sha256::digest(&buf).into())
}

// ------------------------------------------------------------ literals --

thread_local! {
    /// Shape literals as mongod's debug type strings (`?number`) rather
    /// than representative values: how `$queryStats` prints a shape.
    static DEBUG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn debug() -> bool {
    DEBUG.with(std::cell::Cell::get)
}

/// Run `f` with the shape's literals serialized as debug type strings
/// (`kToDebugTypeString`).
fn with_debug<R>(f: impl FnOnce() -> R) -> R {
    let was = DEBUG.with(|d| d.replace(true));
    let out = f();
    DEBUG.with(|d| d.set(was));
    out
}

// --------------------------------------------------------- identifiers --

thread_local! {
    /// The key `$queryStats`' `transformIdentifiers` gave: while it is set,
    /// every identifier in the shape is printed as its HMAC-SHA-256.
    static HMAC_KEY: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with each identifier the shape names -- a field path's
/// components, a namespace, a variable -- serialized as its HMAC-SHA-256
/// under `key`, base64 (`SerializationOptions::transformIdentifiers` with
/// `$queryStats`' `sha256HmacStringDataHasher`).
pub(crate) fn with_hmac<R>(key: &[u8], f: impl FnOnce() -> R) -> R {
    let was = HMAC_KEY.with(|k| k.replace(Some(key.to_vec())));
    let out = f();
    HMAC_KEY.with(|k| *k.borrow_mut() = was);
    out
}

/// `serializeIdentifier`: `s`, or its HMAC under [`with_hmac`].
pub(crate) fn ident(s: &str) -> String {
    HMAC_KEY.with(|k| match k.borrow().as_deref() {
        None => s.to_string(),
        Some(key) => {
            use base64::Engine;
            use hmac::Mac;
            let mut mac =
                hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
            mac.update(s.as_bytes());
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        }
    })
}

fn transforming() -> bool {
    HMAC_KEY.with(|k| k.borrow().is_some())
}

/// `serializeFieldPath`: each dotted component as an identifier.
pub(crate) fn field_path(path: &str) -> String {
    if !transforming() {
        return path.to_string();
    }
    path.split('.').map(ident).collect::<Vec<_>>().join(".")
}

/// The variables mongod defines itself (`Variables::kBuiltinVarNameToId`),
/// and `CURRENT`, which names `ROOT` until something rebinds it.
const BUILTIN_VARIABLES: &[&str] = &[
    "ROOT",
    "CURRENT",
    "REMOVE",
    "NOW",
    "CLUSTER_TIME",
    "JS_SCOPE",
    "IS_MR",
    "SEARCH_META",
    "USER_ROLES",
];

/// `ExpressionFieldPath::serialize` of a `$path` or `$$variable.path`
/// string: `$$CURRENT.a` is `$a`; under [`with_hmac`] a builtin variable
/// keeps its name and the rest of the path is transformed.
fn path_expression(s: &str) -> String {
    let Some(rest) = s.strip_prefix("$$") else {
        return format!("${}", field_path(&s[1..]));
    };
    let (var, tail) = match rest.split_once('.') {
        Some((var, tail)) => (var, Some(tail)),
        None => (rest, None),
    };
    match tail {
        Some(tail) if var == "CURRENT" => format!("${}", field_path(tail)),
        _ if !transforming() => s.to_string(),
        None if BUILTIN_VARIABLES.contains(&var) => s.to_string(),
        Some(tail) if BUILTIN_VARIABLES.contains(&var) => format!("$${var}.{}", field_path(tail)),
        _ => format!("$${}", field_path(rest)),
    }
}

/// mongod's debug type string of one value's type (`debugTypeString`),
/// an array's by its elements: `[]` empty, `?array<T>` of one type, and
/// `?array<>` of more than one.
fn debug_type(v: &Bson, top: bool) -> String {
    match v {
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => "?number",
        Bson::String(_) | Bson::Symbol(_) => "?string",
        Bson::Document(_) => "?object",
        Bson::Array(items) if top => {
            let first = items.first().map(|f| debug_type(f, false));
            return match first {
                None => "[]".into(),
                Some(t) if items.iter().all(|i| debug_type(i, false) == t) => {
                    format!("?array<{t}>")
                }
                Some(_) => "?array<>".into(),
            };
        }
        Bson::Array(_) => "?array",
        Bson::Binary(_) => "?binData",
        Bson::ObjectId(_) => "?objectId",
        Bson::Boolean(_) => "?bool",
        Bson::DateTime(_) => "?date",
        Bson::Null => "?null",
        Bson::RegularExpression(_) => "?regex",
        Bson::JavaScriptCode(_) => "?javascript",
        Bson::JavaScriptCodeWithScope(_) => "?javascriptWithScope",
        Bson::Timestamp(_) => "?timestamp",
        Bson::MinKey => "?minKey",
        Bson::MaxKey => "?maxKey",
        Bson::Undefined => "?undefined",
        Bson::DbPointer(_) => "?dbPointer",
    }
    .into()
}

/// A literal as the shape being built serializes it: its representative,
/// or under [`with_debug`] the debug type string of that representative --
/// mongod prints a debug shape by re-parsing the representative one.
fn lit(representative: Bson) -> Bson {
    if debug() {
        Bson::String(debug_type(&representative, true))
    } else {
        representative
    }
}

/// mongod's literal of `v`: its representative value
/// (`defaultLiteralOfType`), or its debug type string (see [`lit`]).
fn rep(v: &Bson) -> Option<Bson> {
    Some(lit(representative(v)?))
}

/// mongod's representative value of `v` (`defaultLiteralOfType`).
fn representative(v: &Bson) -> Option<Bson> {
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
        Bson::DbPointer(_) => dbpointer_rep()?,
    })
}

/// mongod's representative DBPointer, `BSONDBRef("?.?", OID::max())`. The
/// bson crate has no constructor for one, so it is decoded from its bytes.
fn dbpointer_rep() -> Option<Bson> {
    let mut element = vec![0x0c, 0];
    element.extend_from_slice(&4_i32.to_le_bytes());
    element.extend_from_slice(b"?.?\0");
    element.extend_from_slice(&[0xff; 12]);
    let mut raw = i32::try_from(element.len() + 5)
        .ok()?
        .to_le_bytes()
        .to_vec();
    raw.extend_from_slice(&element);
    raw.push(0);
    secantus_core::bsondoc::from_bytes(&raw)
        .ok()?
        .get("")
        .cloned()
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
    d.iter()
        .map(|(k, v)| Some((field_path(k), rep(v)?)))
        .collect()
}

/// A sort: each direction as 1 or -1; a `$meta` key is named
/// `$computed<position>`, as mongod names the field it computes.
fn sort_shape(d: &Document) -> Option<Document> {
    d.iter()
        .enumerate()
        .map(|(at, (k, v))| {
            Some(match v {
                Bson::Document(meta) if meta.contains_key("$meta") => {
                    (format!("$computed{at}"), Bson::Document(meta.clone()))
                }
                other => (
                    field_path(k),
                    Bson::Int32(if number(other)? < 0.0 { -1 } else { 1 }),
                ),
            })
        })
        .collect()
}

fn let_shape(d: &Document) -> Option<Document> {
    d.iter()
        .map(|(k, v)| Some((field_path(k), expr(v, true)?)))
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
        "$alwaysTrue" | "$alwaysFalse" => Node::Raw(doc! { op: lit(Bson::Int32(1)) }),
        "$expr" => Node::Raw(doc! { "$expr": expr(v, true)? }),
        "$where" => {
            if !matches!(v, Bson::String(_) | Bson::JavaScriptCode(_)) {
                return None;
            }
            Node::Raw(doc! { "$where": lit(Bson::JavaScriptCode("return ?;".into())) })
        }
        // mongod parses `$sampleRate` into the expression it stands for.
        "$sampleRate" => {
            number(v)?;
            Node::Raw(doc! { "$expr": { "$lt": [{ "$rand": {} }, lit(Bson::Int32(1))] } })
        }
        "$text" => {
            let d = v.as_document()?;
            const KEYS: [&str; 4] = [
                "$search",
                "$language",
                "$caseSensitive",
                "$diacriticSensitive",
            ];
            if d.keys().any(|k| !KEYS.contains(&k.as_str())) {
                return None;
            }
            d.get_str("$search").ok()?;
            Node::Raw(doc! { "$text": {
                "$search": "?",
                "$language": "?",
                "$caseSensitive": true,
                "$diacriticSensitive": true,
            } })
        }
        _ => return None,
    }))
}

// ------------------------------------------------------------------ geo --

/// mongod's representative GeoJSON coordinates for each type: valid
/// shapes, so the representative query parses.
fn geojson_coordinates(kind: &str) -> Option<Bson> {
    let p = |x: i32, y: i32| Bson::Array(vec![Bson::Int32(x), Bson::Int32(y)]);
    let line = || Bson::Array(vec![p(0, 0), p(1, 1)]);
    let polygon = || Bson::Array(vec![Bson::Array(vec![p(0, 0), p(0, 1), p(1, 1), p(0, 0)])]);
    Some(lit(match kind {
        "Point" => p(1, 1),
        "LineString" => line(),
        "Polygon" => polygon(),
        "MultiPoint" => Bson::Array(vec![p(1, 1)]),
        "MultiLineString" => Bson::Array(vec![line(), line()]),
        "MultiPolygon" => Bson::Array(vec![polygon()]),
        _ => return None,
    }))
}

/// A GeoJSON geometry: its type and `crs` as written, its coordinates (or
/// a collection's geometries) as representatives.
fn geojson(d: &Document) -> Option<Document> {
    let kind = d.get_str("type").ok()?;
    let mut out = Document::new();
    for (k, v) in d {
        let v = match k.as_str() {
            "type" | "crs" => v.clone(),
            "coordinates" => geojson_coordinates(kind)?,
            "geometries" if kind == "GeometryCollection" => Bson::Array(
                v.as_array()?
                    .iter()
                    .map(|g| Some(Bson::Document(geojson(g.as_document()?)?)))
                    .collect::<Option<_>>()?,
            ),
            _ => return None,
        };
        out.insert(k.clone(), v);
    }
    Some(out)
}

/// A legacy shape's numbers each as 1, a point written as a document as
/// `[1, 1]` (`$center`, `$centerSphere`, `$box`).
fn ones(v: &Bson) -> Option<Bson> {
    match v {
        Bson::Array(a) => a.iter().map(ones).collect::<Option<_>>().map(Bson::Array),
        Bson::Document(_) => legacy_point(v),
        other => number(other).map(|_| Bson::Int32(1)),
    }
}

/// A legacy point (`[x, y]` or `{x, y}`) as `[1, 1]`.
fn legacy_point(v: &Bson) -> Option<Bson> {
    let coords: Vec<&Bson> = match v {
        Bson::Array(a) => a.iter().collect(),
        Bson::Document(d) => d.values().collect(),
        _ => return None,
    };
    if coords.len() != 2 || coords.iter().any(|c| number(c).is_none()) {
        return None;
    }
    Some(Bson::Array(vec![Bson::Int32(1), Bson::Int32(1)]))
}

/// `$geoWithin` / `$within` / `$geoIntersects`'s shape.
fn geo_shape(op: &str, v: &Bson) -> Option<Document> {
    let d = v.as_document().filter(|d| d.len() == 1)?;
    let (shape, value) = d.iter().next()?;
    let value = match (op, shape.as_str()) {
        (_, "$geometry") => Bson::Document(geojson(value.as_document()?)?),
        ("$geoIntersects", _) => return None,
        (_, "$center" | "$centerSphere" | "$box") => lit(ones(value)?),
        (_, "$polygon") => lit(Bson::Array(vec![
            Bson::Array(vec![Bson::Int32(0), Bson::Int32(0)]),
            Bson::Array(vec![Bson::Int32(0), Bson::Int32(1)]),
            Bson::Array(vec![Bson::Int32(1), Bson::Int32(1)]),
        ])),
        _ => return None,
    };
    Some(doc! { shape.clone(): value })
}

/// `$near` / `$nearSphere` with its distances, in the order written: the
/// point as a representative, each distance as 1.
fn near(context: &Document) -> Option<Document> {
    let mut out = Document::new();
    for (k, v) in context {
        let v = match k.as_str() {
            "$near" | "$nearSphere" => match v {
                Bson::Document(d) if d.contains_key("$geometry") => {
                    let mut inner = Document::new();
                    for (ik, iv) in d {
                        let iv = match ik.as_str() {
                            "$geometry" => Bson::Document(geojson(iv.as_document()?)?),
                            "$maxDistance" | "$minDistance" => {
                                number(iv)?;
                                lit(Bson::Int32(1))
                            }
                            _ => return None,
                        };
                        inner.insert(ik.clone(), iv);
                    }
                    Bson::Document(inner)
                }
                other => lit(legacy_point(other)?),
            },
            "$maxDistance" | "$minDistance" => {
                number(v)?;
                lit(Bson::Int32(1))
            }
            _ => return None,
        };
        out.insert(k.clone(), v);
    }
    Some(out)
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
    let mut rhs = doc! { "$regex": lit(Bson::String("\\?".into())) };
    if !options.is_empty() {
        rhs.insert("$options", lit(Bson::String("i".into())));
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
            let node = leaf(path, "$in", lit(Bson::Array(in_rep(v.as_array()?)?)));
            if op == "$in" {
                node
            } else {
                Node::Not(Box::new(node))
            }
        }
        "$exists" => {
            let node = leaf(path, "$exists", lit(Bson::Boolean(true)));
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
            leaf(path, "$size", lit(Bson::Int32(1)))
        }
        "$mod" => {
            if v.as_array().map(Vec::len) != Some(2) {
                return None;
            }
            leaf(
                path,
                "$mod",
                Bson::Array(vec![lit(Bson::Int32(1)), lit(Bson::Int32(1))]),
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
        "$geoWithin" | "$within" | "$geoIntersects" => Node::Leaf {
            path: path.to_string(),
            rhs: doc! { op: geo_shape(op, v)? },
        },
        "$near" | "$nearSphere" => Node::Leaf {
            path: path.to_string(),
            rhs: near(context)?,
        },
        "$maxDistance" | "$minDistance"
            if context.contains_key("$near") || context.contains_key("$nearSphere") =>
        {
            return Some(None)
        }
        "$bitsAllSet" | "$bitsAllClear" | "$bitsAnySet" | "$bitsAnyClear" => {
            leaf(path, op, lit(Bson::Array(bits_rep(v)?)))
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
        return Some(Node::Raw(doc! { "$alwaysFalse": lit(Bson::Int32(1)) }));
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
                doc! { field_path(path): rhs.clone() }
            } else {
                rhs.clone()
            }
        }
        Node::ElemValue { path, .. } | Node::ElemObject { path, .. } => {
            if include_path {
                doc! { field_path(path): right_hand_side(node) }
            } else {
                right_hand_side(node)
            }
        }
        Node::And(kids) if kids.is_empty() => Document::new(),
        Node::And(kids) => doc! { "$and": list(kids) },
        Node::Or(kids) if kids.is_empty() => doc! { "$alwaysFalse": lit(Bson::Int32(1)) },
        Node::Or(kids) => doc! { "$or": list(kids) },
        Node::Nor(kids) => doc! { "$nor": list(kids) },
        Node::Raw(d) => d.clone(),
        Node::Not(child) => not(child, include_path),
    }
}

fn not(child: &Node, include_path: bool) -> Document {
    if matches!(child, Node::And(kids) if kids.is_empty()) {
        return doc! { "$alwaysFalse": lit(Bson::Int32(1)) };
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
            doc! { field_path(path): { "$not": right_hand_side(target) } }
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
    "$acos",
    "$acosh",
    "$add",
    "$allElementsTrue",
    "$and",
    "$anyElementTrue",
    "$arrayElemAt",
    "$arrayToObject",
    "$asin",
    "$asinh",
    "$atan",
    "$atan2",
    "$atanh",
    "$avg",
    "$binarySize",
    "$bitAnd",
    "$bitNot",
    "$bitOr",
    "$bitXor",
    "$bsonSize",
    "$ceil",
    "$cmp",
    "$concat",
    "$concatArrays",
    "$cond",
    "$cos",
    "$cosh",
    "$degreesToRadians",
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
    "$max",
    "$mergeObjects",
    "$min",
    "$mod",
    "$multiply",
    "$ne",
    "$not",
    "$objectToArray",
    "$or",
    "$pow",
    "$radiansToDegrees",
    "$range",
    "$reverseArray",
    "$round",
    "$setDifference",
    "$setEquals",
    "$setIntersection",
    "$setIsSubset",
    "$setUnion",
    "$sin",
    "$sinh",
    "$size",
    "$slice",
    "$split",
    "$sqrt",
    "$stdDevPop",
    "$stdDevSamp",
    "$strLenBytes",
    "$strLenCP",
    "$strcasecmp",
    "$substrBytes",
    "$substrCP",
    "$subtract",
    "$sum",
    "$tan",
    "$tanh",
    "$toLower",
    "$toUpper",
    "$trunc",
    "$tsIncrement",
    "$tsSecond",
    "$type",
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
    // A debug shape prints a constant bare.
    Some(if wrap && !debug() {
        Bson::Document(doc! { "$const": r })
    } else {
        r
    })
}

/// An aggregation expression's representative shape.
fn expr(v: &Bson, wrap: bool) -> Option<Bson> {
    match v {
        Bson::String(s) if s.starts_with('$') => {
            (s.len() > 1).then(|| Bson::String(path_expression(s)))
        }
        Bson::Document(d) if operator(d).is_some() => {
            let (op, arg) = operator(d)?;
            if d.len() != 1 {
                return None;
            }
            if op == "$literal" || op == "$const" {
                return constant(arg, wrap);
            }
            if NARY.contains(&op) && !(op == "$cond" && arg.as_document().is_some()) {
                return nary(op, arg);
            }
            operator_shape(op, arg).map(Bson::Document)
        }
        Bson::Document(_) | Bson::Array(_) if all_constant(v) => constant(&constant_value(v), wrap),
        Bson::Document(d) => d
            .iter()
            .map(|(k, x)| Some((field_path(k), expr(x, true)?)))
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

/// `ExpressionNary::serialize`: `{$op: [args]}`, a constant argument as its
/// bare representative.
fn nary(op: &str, arg: &Bson) -> Option<Bson> {
    let args: Vec<&Bson> = match arg {
        Bson::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    // A debug shape prints arguments that are all constants as one array
    // literal (measured 8.2.11: `{$add: [1, 2]}` is `{$add: "?array<?number>"}`).
    if debug() && !args.is_empty() && args.iter().all(|a| is_constant(a)) {
        let values = args.iter().map(|a| constant_value(a)).collect();
        return Some(Bson::Document(doc! { op: lit(Bson::Array(values)) }));
    }
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

/// A child expression as its parent serializes it: a constant as
/// `{$const: <representative>}`.
fn sub_expr(v: &Bson) -> Option<Bson> {
    expr(v, true)
}

/// An operator taking named arguments, serialized in mongod's field order
/// (`fields`, whatever order they were written in), each argument a child
/// expression. An unknown argument is mongod's parse error: no shape.
fn named(arg: &Bson, fields: &[&str]) -> Option<Document> {
    let d = arg.as_document()?;
    if d.keys().any(|k| !fields.contains(&k.as_str())) {
        return None;
    }
    let mut out = Document::new();
    for f in fields {
        if let Some(v) = d.get(*f) {
            out.insert(*f, sub_expr(v)?);
        }
    }
    Some(out)
}

/// The one argument of an operator that takes one, written bare or as an
/// array of one.
fn single(arg: &Bson) -> Option<&Bson> {
    match arg {
        Bson::Array(a) if a.len() == 1 => a.first(),
        Bson::Array(_) => None,
        other => Some(other),
    }
}

/// A value mongod keeps as written, wrapped as a constant (`$convert`'s
/// `to`, `$getField`'s `field`): `{$literal: v}` keeps `v`.
fn kept_constant(v: &Bson) -> Bson {
    let value = match v {
        Bson::Document(d) if operator(d).is_some_and(|(op, _)| op == "$literal") => {
            d.get("$literal").cloned().unwrap_or(Bson::Null)
        }
        other => other.clone(),
    };
    // A debug shape prints the value bare, as written -- a string that
    // would read as a field path aside.
    if debug() && !matches!(&value, Bson::String(s) if s.starts_with('$')) {
        return value;
    }
    Bson::Document(doc! { "$const": value })
}

/// `$getField`'s and `$setField`'s `field`, a constant that is also a field
/// path: kept as [`kept_constant`] keeps it, its name an identifier.
fn field_name_constant(v: &Bson) -> Bson {
    match kept_constant(v) {
        Bson::String(s) => Bson::String(field_path(&s)),
        Bson::Document(mut d) => {
            if let Some(Bson::String(s)) = d.get("$const") {
                let s = field_path(s);
                d.insert("$const", s);
            }
            Bson::Document(d)
        }
        other => other,
    }
}

/// A document whose keys are field paths, its values as written (a
/// `$top`'s `sortBy`, `SortPattern::serialize`).
fn field_paths(d: &Document) -> Document {
    d.iter().map(|(k, v)| (field_path(k), v.clone())).collect()
}

/// The date-part operators: `{$op: {date, timezone}}` whichever way the
/// argument was written (an expression, an array of one, or the document).
const DATE_PARTS: &[&str] = &[
    "$year",
    "$month",
    "$dayOfMonth",
    "$hour",
    "$minute",
    "$second",
    "$millisecond",
    "$dayOfYear",
    "$dayOfWeek",
    "$week",
    "$isoWeekYear",
    "$isoWeek",
    "$isoDayOfWeek",
];

/// The `$toX` shorthands, each a `$convert` to the type it names.
fn convert_target(op: &str) -> Option<&'static str> {
    Some(match op {
        "$toBool" => "bool",
        "$toDate" => "date",
        "$toDecimal" => "decimal",
        "$toDouble" => "double",
        "$toInt" => "int",
        "$toLong" => "long",
        "$toObjectId" => "objectId",
        "$toString" => "string",
        "$toUUID" => "uuid",
        _ => return None,
    })
}

/// `$sigmoid` as the expression mongod parses it into:
/// `1 / (1 + e^(-x))`.
fn sigmoid(x: Bson) -> Bson {
    Bson::Document(doc! { "$divide": [
        1,
        { "$add": [1, { "$exp": [{ "$multiply": [-1, x] }] }] },
    ] })
}

/// The serialization of an operator with arguments of its own form.
fn operator_shape(op: &str, arg: &Bson) -> Option<Document> {
    if DATE_PARTS.contains(&op) {
        let a = single(arg)?;
        let named_form =
            matches!(a, Bson::Document(d) if !d.keys().next().is_some_and(|k| k.starts_with('$')));
        return Some(doc! { op: if named_form {
            named(a, &["date", "timezone"])?
        } else {
            doc! { "date": sub_expr(a)? }
        } });
    }
    if let Some(to) = convert_target(op) {
        let to = if to == "uuid" {
            Bson::Document(doc! { "type": "binData", "subtype": 4 })
        } else {
            Bson::String(to.into())
        };
        let format = matches!(&to, Bson::String(s) if s == "string") || to.as_document().is_some();
        let mut out = doc! { "input": sub_expr(single(arg)?)?, "to": kept_constant(&to) };
        if format {
            // `$toString` and `$toUUID` convert with a format, a constant.
            out.insert("format", constant(&Bson::String("?".into()), true)?);
        }
        return Some(doc! { "$convert": out });
    }
    let fields: &[&str] = match op {
        "$dateToString" => &["date", "format", "timezone", "onNull"],
        "$dateFromString" => &["dateString", "timezone", "format", "onNull", "onError"],
        "$dateFromParts" => &[
            "year",
            "month",
            "day",
            "hour",
            "minute",
            "second",
            "millisecond",
            "isoWeekYear",
            "isoWeek",
            "isoDayOfWeek",
            "timezone",
        ],
        "$dateToParts" => &["date", "timezone", "iso8601"],
        "$dateAdd" | "$dateSubtract" => &["startDate", "unit", "amount", "timezone"],
        "$dateDiff" => &["startDate", "endDate", "unit", "timezone", "startOfWeek"],
        "$dateTrunc" => &["date", "unit", "binSize", "timezone", "startOfWeek"],
        "$reduce" => &["input", "initialValue", "in"],
        "$regexMatch" | "$regexFind" | "$regexFindAll" => &["input", "regex", "options"],
        "$trim" | "$ltrim" | "$rtrim" => &["input", "chars"],
        "$replaceOne" | "$replaceAll" => &["input", "find", "replacement"],
        "$maxN" | "$minN" | "$firstN" | "$lastN" => &["n", "input"],
        _ => &[],
    };
    if !fields.is_empty() {
        return Some(doc! { op: named(arg, fields)? });
    }
    Some(match op {
        "$convert" => {
            let d = arg.as_document()?;
            let mut out = named(arg, &["input", "to", "format", "onError", "onNull"])?;
            if let Some(to) = d.get("to").filter(|t| is_constant(t)) {
                let to = constant_value(to);
                if matches!(
                    to,
                    Bson::String(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)
                ) {
                    out.insert("to", kept_constant(&to));
                }
            }
            // A debug shape keeps a document's representative as it
            // re-parsed it (measured 8.2.11: `to: {"?": "?"}`).
            if let Some(to) = d
                .get("to")
                .filter(|t| debug() && t.as_document().is_some() && all_constant(t))
            {
                out.insert("to", representative(&constant_value(to))?);
            }
            doc! { "$convert": out }
        }
        "$cond" => {
            let d = arg.as_document()?;
            if d.len() != 3 {
                return None;
            }
            let args = vec![
                d.get("if")?.clone(),
                d.get("then")?.clone(),
                d.get("else")?.clone(),
            ];
            return nary(op, &Bson::Array(args))?.as_document().cloned();
        }
        "$substr" => return nary("$substrBytes", arg)?.as_document().cloned(),
        "$let" => {
            let d = arg.as_document()?;
            let vars = d.get_document("vars").ok()?;
            let vars: Document = vars
                .iter()
                .map(|(k, v)| Some((ident(k), sub_expr(v)?)))
                .collect::<Option<_>>()?;
            doc! { "$let": { "vars": vars, "in": sub_expr(d.get("in")?)? } }
        }
        "$map" | "$filter" => {
            let d = arg.as_document()?;
            let body = if op == "$map" { "in" } else { "cond" };
            let allowed: &[&str] = if op == "$map" {
                &["input", "as", "in"]
            } else {
                &["input", "as", "cond", "limit"]
            };
            if d.keys().any(|k| !allowed.contains(&k.as_str())) {
                return None;
            }
            let mut out = doc! {
                "input": sub_expr(d.get("input")?)?,
                "as": ident(d.get("as").map_or(Some("this"), Bson::as_str)?),
                body: sub_expr(d.get(body)?)?,
            };
            if let Some(limit) = d.get("limit") {
                out.insert("limit", sub_expr(limit)?);
            }
            doc! { op: out }
        }
        "$switch" => {
            let d = arg.as_document()?;
            let branches = d
                .get_array("branches")
                .ok()?
                .iter()
                .map(|b| {
                    let b = b.as_document()?;
                    Some(Bson::Document(doc! {
                        "case": sub_expr(b.get("case")?)?,
                        "then": sub_expr(b.get("then")?)?,
                    }))
                })
                .collect::<Option<Vec<_>>>()?;
            let mut out = doc! { "branches": branches };
            if let Some(default) = d.get("default") {
                out.insert("default", sub_expr(default)?);
            }
            doc! { "$switch": out }
        }
        "$getField" | "$setField" | "$unsetField" => {
            let (field, input, value) = match arg {
                Bson::Document(d) if operator(d).is_none() => {
                    (d.get("field")?, d.get("input"), d.get("value"))
                }
                other if op == "$getField" => (other, None, None),
                _ => return None,
            };
            let input = match input {
                Some(i) => sub_expr(i)?,
                None if op == "$getField" => Bson::String("$$CURRENT".into()),
                None => return None,
            };
            let mut out = doc! { "field": field_name_constant(field), "input": input };
            match op {
                "$setField" => {
                    out.insert("value", sub_expr(value?)?);
                }
                "$unsetField" => {
                    out.insert("value", "$$REMOVE");
                }
                _ => {}
            }
            doc! { if op == "$getField" { "$getField" } else { "$setField" }: out }
        }
        "$sortArray" => {
            let d = arg.as_document()?;
            doc! { "$sortArray": {
                "input": sub_expr(d.get("input")?)?,
                "sortBy": d.get("sortBy")?.clone(),
            } }
        }
        "$zip" => {
            let d = arg.as_document()?;
            let list = |v: Option<&Bson>| -> Option<Vec<Bson>> {
                match v {
                    None => Some(vec![]),
                    Some(Bson::Array(a)) => a.iter().map(sub_expr).collect(),
                    Some(_) => None,
                }
            };
            doc! { "$zip": {
                "inputs": list(d.get("inputs"))?,
                "defaults": list(d.get("defaults"))?,
                "useLongestLength": d.get("useLongestLength").is_some_and(true_value),
            } }
        }
        "$meta" => doc! { "$meta": arg.as_str()? },
        "$sigmoid" => {
            if arg.as_array().is_some() {
                return None;
            }
            return expr(&sigmoid(arg.clone()), true)?.as_document().cloned();
        }
        "$similarityCosine" | "$similarityDotProduct" | "$similarityEuclidean" => {
            let (vectors, score) = match arg {
                Bson::Array(v) => (v, false),
                Bson::Document(d) => (
                    d.get_array("vectors").ok()?,
                    d.get("score").is_some_and(true_value),
                ),
                _ => return None,
            };
            if vectors.len() != 2 {
                return None;
            }
            let vectors = vectors.iter().map(sub_expr).collect::<Option<Vec<_>>>()?;
            doc! { op: { "vectors": vectors, "score": score } }
        }
        "$rand" => doc! { "$rand": {} },
        "$toHashedIndexKey" => doc! { op: sub_expr(arg)? },
        "$median" | "$percentile" => {
            let d = arg.as_document()?;
            let mut out = doc! { "input": sub_expr(d.get("input")?)? };
            if op == "$percentile" {
                out.insert("p", d.get("p")?.clone());
            }
            out.insert("method", d.get_str("method").ok()?);
            doc! { op: out }
        }
        "$function" => {
            let d = arg.as_document()?;
            if !matches!(d.get("body")?, Bson::String(_) | Bson::JavaScriptCode(_)) {
                return None;
            }
            // The body is a bare literal, the language kept as written.
            doc! { "$function": {
                "body": "?",
                "args": sub_expr(d.get("args")?)?,
                "lang": d.get_str("lang").ok()?,
            } }
        }
        _ => return None,
    })
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
                // A `$meta` field makes a find's projection neither an
                // inclusion nor an exclusion.
                (Mode::Find, "$meta") => PNode::Expr(v.clone()),
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
                PNode::Slice(Bson::Array(_)) => {
                    Bson::Array(vec![lit(Bson::Int32(1)), lit(Bson::Int32(1))])
                }
                PNode::Slice(_) => lit(Bson::Int32(1)),
                PNode::ElemMatch(q) => Bson::Document(doc! { "$elemMatch": filter_shape(q)? }),
                PNode::Expr(e) => expr(e, true)?,
            };
            let value = match v {
                PNode::Slice(_) => Bson::Document(doc! { "$slice": value }),
                _ => value,
            };
            let key = match k.strip_suffix(".$") {
                Some(field) => format!("{}.$", field_path(field)),
                None => field_path(k),
            };
            Some((key, value))
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
    if mode == Mode::Project {
        node_order(&mut tree, kind.inclusion);
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

/// A `$project` node's order (`ProjectionNode::serialize`): `_id` if it is
/// projected the node's way, the other projected fields, then the computed
/// fields and sub-objects in the order they were added; an `_id` projected
/// the other way comes last.
fn node_order(tree: &mut [(String, PNode)], inclusion: bool) {
    let rank = |(k, v): &(String, PNode)| match v {
        PNode::Bool(b) if k == "_id" && *b == inclusion => 0,
        PNode::Bool(_) if k == "_id" => 3,
        PNode::Bool(_) => 1,
        _ => 2,
    };
    tree.sort_by_key(rank);
    for (_, v) in tree.iter_mut() {
        if let PNode::Sub(sub) = v {
            node_order(sub, inclusion);
        }
    }
}

// -------------------------------------------------------------- stages --

/// The accumulators whose argument is one expression: `{$op: <expr>}`.
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
    "$mergeObjects",
    "$concatArrays",
    "$setUnion",
];

fn count_of_one() -> Document {
    match constant(&Bson::Int32(1), true) {
        Some(one) => doc! { "$sum": one },
        None => doc! { "$sum": { "$const": 1 } },
    }
}

/// A JavaScript function's source: a bare representative string.
fn js_source(v: &Bson) -> Option<Bson> {
    matches!(v, Bson::String(_) | Bson::JavaScriptCode(_)).then(|| lit(Bson::String("?".into())))
}

/// An accumulator (`{$op: <argument>}` in a `$group`, a `$bucket`'s or
/// `$bucketAuto`'s `output`, or a window function), serialized.
fn accumulator(acc: &Bson) -> Option<Document> {
    let (op, arg) = operator(acc.as_document()?)?;
    if acc.as_document()?.len() != 1 {
        return None;
    }
    Some(match op {
        "$count" => count_of_one(),
        _ if ACCUMULATORS.contains(&op) => doc! { op: expr(arg, true)? },
        "$top" | "$bottom" | "$topN" | "$bottomN" => {
            let d = arg.as_document()?;
            let mut out = Document::new();
            if op.ends_with('N') {
                out.insert("n", sub_expr(d.get("n")?)?);
            }
            out.insert("output", sub_expr(d.get("output")?)?);
            out.insert("sortBy", field_paths(d.get_document("sortBy").ok()?));
            doc! { op: out }
        }
        "$firstN" | "$lastN" | "$maxN" | "$minN" | "$median" | "$percentile" => {
            operator_shape(op, arg)?
        }
        "$accumulator" => {
            let d = arg.as_document()?;
            let mut out = doc! {
                "init": js_source(d.get("init")?)?,
                "initArgs": match d.get("initArgs") {
                    Some(a) => sub_expr(a)?,
                    None => Bson::Document(doc! { "$const": [] }),
                },
                "accumulate": js_source(d.get("accumulate")?)?,
                "accumulateArgs": sub_expr(d.get("accumulateArgs")?)?,
                "merge": js_source(d.get("merge")?)?,
            };
            if let Some(f) = d.get("finalize") {
                out.insert("finalize", js_source(f)?);
            }
            out.insert("lang", d.get_str("lang").ok()?);
            doc! { op: out }
        }
        _ => return None,
    })
}

/// A `{name: <accumulator>}` map (`$group`'s fields, `$bucket`'s output).
fn accumulators<'a>(fields: impl Iterator<Item = (&'a String, &'a Bson)>) -> Option<Document> {
    fields
        .map(|(k, v)| Some((field_path(k), Bson::Document(accumulator(v)?))))
        .collect()
}

/// The window functions whose argument is not an accumulator's.
fn window_function(op: &str, arg: &Bson) -> Option<Document> {
    Some(match op {
        "$rank" | "$denseRank" | "$documentNumber" => {
            if !arg.as_document()?.is_empty() {
                return None;
            }
            doc! { op: {} }
        }
        "$locf" | "$linearFill" => doc! { op: sub_expr(arg)? },
        "$shift" => {
            let d = arg.as_document()?;
            let default = match d.get("default") {
                Some(v) => rep(v)?,
                None => lit(Bson::Null),
            };
            doc! { op: {
                "by": rep(d.get("by")?)?,
                "output": sub_expr(d.get("output")?)?,
                "default": default,
            } }
        }
        "$expMovingAvg" => {
            let d = arg.as_document()?;
            // `alpha`'s representative is 0.1, a valid smoothing factor.
            let (key, n) = match (d.get("N"), d.get("alpha")) {
                (Some(n), None) => ("N", rep(n)?),
                (None, Some(a)) => {
                    number(a)?;
                    ("alpha", lit(Bson::Double(0.1)))
                }
                _ => return None,
            };
            doc! { op: { key: n, "input": sub_expr(d.get("input")?)? } }
        }
        "$derivative" | "$integral" => {
            let d = arg.as_document()?;
            let mut out = doc! { "input": sub_expr(d.get("input")?)? };
            if let Some(unit) = d.get("unit") {
                out.insert("unit", unit.as_str()?);
            }
            doc! { op: out }
        }
        _ => return None,
    })
}

/// The functions that take no window.
const WINDOWLESS: &[&str] = &[
    "$rank",
    "$denseRank",
    "$documentNumber",
    "$shift",
    "$expMovingAvg",
    "$locf",
    "$linearFill",
];

/// A window's bounds: `"current"` and `"unbounded"` as written, a numeric
/// lower bound as a 64-bit 0 and a numeric upper bound as a 64-bit 1
/// (representatives that keep the window valid).
fn window(w: &Document) -> Option<Document> {
    let mut out = Document::new();
    for (k, v) in w {
        let v = match k.as_str() {
            "documents" | "range" => {
                let bounds = v.as_array().filter(|b| b.len() == 2)?;
                let bound = |b: &Bson, numeric: i64| match b {
                    Bson::String(_) => Some(b.clone()),
                    other => number(other).map(|_| lit(Bson::Int64(numeric))),
                };
                Bson::Array(vec![bound(&bounds[0], 0)?, bound(&bounds[1], 1)?])
            }
            "unit" => Bson::String(v.as_str()?.into()),
            _ => return None,
        };
        out.insert(k.clone(), v);
    }
    Some(out)
}

/// One `output` field of `$setWindowFields`: the function, then its window
/// (the whole partition when none is given).
fn window_output(spec: &Bson) -> Option<Document> {
    let d = spec.as_document()?;
    let (op, arg) = d.iter().find(|(k, _)| k.as_str() != "window")?;
    let mut out = match window_function(op, arg) {
        Some(f) => f,
        None => accumulator(&Bson::Document(doc! { op.clone(): arg.clone() }))?,
    };
    match d.get_document("window") {
        Ok(w) => {
            out.insert("window", window(w)?);
        }
        Err(_) if d.len() != 1 => return None,
        Err(_) if WINDOWLESS.contains(&op.as_str()) => {}
        Err(_) => {
            out.insert("window", doc! { "documents": ["unbounded", "unbounded"] });
        }
    }
    Some(out)
}

/// The temporary field an expression `partitionBy` is computed into.
const PARTITION_KEY: &str = "__internal_setWindowFields_partition_key";

/// `$setWindowFields` (and `$fill`'s methods) as mongod desugars it: a
/// `$sort` on the partition then the sort, then
/// `$_internalSetWindowFields`; a computed partition goes through a
/// temporary field, set before and removed after.
fn window_stages(
    partition: Option<Partition>,
    sort: Option<&Document>,
    output: Document,
) -> Option<Vec<Document>> {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut order = Document::new();
    let computed = match partition {
        None => None,
        Some(Partition::By(Bson::String(path)))
            if path.starts_with('$') && !path.starts_with("$$") =>
        {
            order.insert(field_path(&path[1..]), 1);
            return finish(
                Some(Bson::String(path_expression(path))),
                sort,
                output,
                order,
            );
        }
        Some(Partition::By(e)) => Some(sub_expr(e)?),
        Some(Partition::Fields(value)) => Some(value),
    };
    let partition = match computed {
        None => None,
        Some(value) => {
            before.push(doc! { "$addFields": { field_path(PARTITION_KEY): value } });
            order.insert(field_path(PARTITION_KEY), 1);
            after.push(
                doc! { "$project": { field_path(PARTITION_KEY): false, field_path("_id"): true } },
            );
            Some(Bson::String(path_expression(&format!("${PARTITION_KEY}"))))
        }
    };
    let mut out = before;
    out.extend(finish(partition, sort, output, order)?);
    out.extend(after);
    Some(out)
}

/// What a window stage partitions by.
enum Partition<'a> {
    /// `partitionBy`: a field path or an expression.
    By(&'a Bson),
    /// `$fill`'s `partitionByFields`, already serialized.
    Fields(Bson),
}

/// The `$sort` and `$_internalSetWindowFields` of a window stage.
fn finish(
    partition: Option<Bson>,
    sort: Option<&Document>,
    output: Document,
    mut order: Document,
) -> Option<Vec<Document>> {
    let sort = match sort {
        Some(s) => Some(sort_shape(s)?),
        None => None,
    };
    if let Some(s) = &sort {
        order.extend(s.clone());
    }
    let mut internal = Document::new();
    if let Some(p) = partition {
        internal.insert("partitionBy", p);
    }
    if let Some(s) = sort {
        internal.insert("sortBy", s);
    }
    internal.insert("output", output);
    let mut out = Vec::new();
    if !order.is_empty() {
        out.push(doc! { "$sort": order });
    }
    out.push(doc! { "$_internalSetWindowFields": internal });
    Some(out)
}

/// `$bucket`, as the `$group` over a `$switch` and the `$sort` mongod
/// parses it into.
fn bucket(spec: &Document) -> Option<Vec<Document>> {
    let group_by = spec.get("groupBy")?;
    if is_constant(group_by) {
        return None;
    }
    let group_by = expr(group_by, true)?;
    let bounds = spec.get_array("boundaries").ok()?;
    if bounds.len() < 2 {
        return None;
    }
    let branches = bounds
        .windows(2)
        .map(|pair| {
            let (lo, hi) = (lit(scalar_rep(&pair[0])?), lit(scalar_rep(&pair[1])?));
            Some(Bson::Document(doc! {
                "case": { "$and": [
                    { "$gte": [group_by.clone(), lo.clone()] },
                    { "$lt": [group_by.clone(), hi] },
                ] },
                "then": if debug() { lo } else { Bson::Document(doc! { "$const": lo }) },
            }))
        })
        .collect::<Option<Vec<_>>>()?;
    let mut switch = doc! { "branches": branches };
    if let Some(default) = spec.get("default") {
        switch.insert("default", constant(default, true)?);
    }
    let mut group = doc! { "_id": { "$switch": switch } };
    match spec.get_document("output") {
        Ok(output) => group.extend(accumulators(output.iter())?),
        Err(_) => {
            group.insert(field_path("count"), count_of_one());
        }
    }
    Some(vec![
        doc! { "$group": group },
        doc! { "$sort": { field_path("_id"): 1 } },
    ])
}

/// `$fill`: the methods as a window stage, then the values as `$addFields`.
fn fill(spec: &Document) -> Option<Vec<Document>> {
    let partition = match (spec.get("partitionBy"), spec.get("partitionByFields")) {
        (None, None) => None,
        (Some(p), None) => Some(Partition::By(p)),
        // mongod partitions by an object of the fields, which it prints
        // under `$expr`.
        (None, Some(Bson::Array(fields))) => {
            let object = fields
                .iter()
                .map(|f| {
                    let f = f
                        .as_str()
                        .filter(|f| !f.contains('.') && !f.starts_with('$'))?;
                    Some((
                        field_path(f),
                        Bson::String(path_expression(&format!("${f}"))),
                    ))
                })
                .collect::<Option<Document>>()?;
            Some(Partition::Fields(Bson::Document(doc! { "$expr": object })))
        }
        _ => return None,
    };
    let mut methods = Document::new();
    let mut values = Document::new();
    for (field, how) in spec.get_document("output").ok()? {
        let how = how.as_document()?;
        if how.len() != 1 {
            return None;
        }
        let path = Bson::String(format!("${field}"));
        if let Some(v) = how.get("value") {
            values.insert(
                field_path(field),
                nary("$ifNull", &Bson::Array(vec![path, v.clone()]))?,
            );
        } else {
            let op = match how.get_str("method").ok()? {
                "locf" => "$locf",
                "linear" => "$linearFill",
                _ => return None,
            };
            methods.insert(
                field_path(field),
                doc! { op: path_expression(&format!("${field}")) },
            );
        }
    }
    let mut out = Vec::new();
    if !methods.is_empty() {
        let sort = match spec.get("sortBy") {
            Some(s) => Some(s.as_document()?),
            None => None,
        };
        out.extend(window_stages(partition, sort, methods)?);
    }
    if !values.is_empty() {
        out.push(doc! { "$addFields": values });
    }
    Some(out)
}

/// `$densify`: a `$sort` on the partition fields and the field, then
/// `$_internalDensify`.
fn densify(spec: &Document) -> Option<Vec<Document>> {
    let field = spec.get_str("field").ok()?;
    let partitions: Vec<&str> = match spec.get("partitionByFields") {
        None => vec![],
        Some(Bson::Array(a)) => a.iter().map(Bson::as_str).collect::<Option<_>>()?,
        Some(_) => return None,
    };
    let range = spec.get_document("range").ok()?;
    let mut shaped = doc! { "step": rep(range.get("step")?)? };
    let bounds = match range.get("bounds")? {
        Bson::String(s) if s == "full" || s == "partition" => Bson::String(s.clone()),
        Bson::Array(a) if a.len() == 2 => Bson::Array(
            a.iter()
                .map(|b| scalar_rep(b).map(lit))
                .collect::<Option<_>>()?,
        ),
        _ => return None,
    };
    shaped.insert("bounds", bounds);
    if let Some(unit) = range.get("unit") {
        shaped.insert("unit", unit.as_str()?);
    }
    let mut order = Document::new();
    for p in &partitions {
        order.insert(field_path(p), 1);
    }
    order.insert(field_path(field), 1);
    Some(vec![
        doc! { "$sort": order },
        doc! { "$_internalDensify": {
            "field": field_path(field),
            "partitionByFields": partitions.iter().map(|p| field_path(p)).collect::<Vec<_>>(),
            "range": shaped,
        } },
    ])
}

/// `$merge`, with mongod's defaults filled in.
fn merge(db: &str, coll: &str, spec: &Bson) -> Option<Document> {
    let spec = match spec {
        Bson::String(coll) => doc! { "into": coll.clone() },
        Bson::Document(d) => d.clone(),
        _ => return None,
    };
    let into = match spec.get("into")? {
        Bson::String(coll) => doc! { "db": ident(db), "coll": ident(coll) },
        Bson::Document(d) => doc! {
            "db": ident(d.get_str("db").ok()?),
            "coll": ident(d.get_str("coll").ok()?),
        },
        _ => return None,
    };
    let on = match spec.get("on") {
        None => Bson::String(field_path("_id")),
        Some(Bson::String(on)) => Bson::String(field_path(on)),
        Some(Bson::Array(on)) => Bson::Array(
            on.iter()
                .map(|f| Some(Bson::String(field_path(f.as_str()?))))
                .collect::<Option<_>>()?,
        ),
        Some(_) => return None,
    };
    let mut out = doc! { "into": into, "on": on };
    match spec.get("whenMatched") {
        Some(Bson::Array(pipeline)) => {
            let mut vars = match spec.get_document("let") {
                Ok(l) => let_shape(l)?,
                Err(_) => Document::new(),
            };
            vars.insert(field_path("new"), "$$ROOT");
            out.insert("let", vars);
            out.insert("whenMatched", pipeline_shape(db, coll, pipeline)?);
        }
        Some(Bson::String(mode)) if !spec.contains_key("let") => {
            out.insert("whenMatched", mode.clone());
        }
        None if !spec.contains_key("let") => {
            out.insert("whenMatched", "merge");
        }
        _ => return None,
    }
    let not_matched = spec
        .get("whenNotMatched")
        .map_or(Some("insert"), Bson::as_str)?;
    out.insert("whenNotMatched", not_matched);
    Some(out)
}

/// `$collStats`: its sections in mongod's order, `storageStats` with its
/// defaults filled in.
fn coll_stats(spec: &Document) -> Option<Document> {
    const SECTIONS: [&str; 4] = ["latencyStats", "storageStats", "count", "queryExecStats"];
    if spec.keys().any(|k| !SECTIONS.contains(&k.as_str())) {
        return None;
    }
    let mut out = Document::new();
    for section in SECTIONS {
        let Some(v) = spec.get(section) else {
            continue;
        };
        let d = v.as_document()?;
        let flag = |key: &str, default: bool| -> Option<bool> {
            d.get(key).map_or(Some(default), Bson::as_bool)
        };
        let shaped = match section {
            "latencyStats" => match d.get("histograms") {
                None if d.is_empty() => Document::new(),
                Some(Bson::Boolean(h)) if d.len() == 1 => doc! { "histograms": *h },
                _ => return None,
            },
            "storageStats" => {
                const KEYS: [&str; 4] = ["scale", "verbose", "waitForLock", "numericOnly"];
                if d.keys().any(|k| !KEYS.contains(&k.as_str())) {
                    return None;
                }
                let mut s = Document::new();
                if let Some(scale) = d.get("scale") {
                    s.insert("scale", rep(scale)?);
                }
                s.insert("verbose", flag("verbose", false)?);
                s.insert("waitForLock", flag("waitForLock", true)?);
                s.insert("numericOnly", flag("numericOnly", false)?);
                s
            }
            _ if d.is_empty() => Document::new(),
            _ => return None,
        };
        out.insert(section, shaped);
    }
    Some(out)
}

/// `$changeStream`, with mongod's defaults filled in: where it starts (a
/// resume token, otherwise the operation time mongod picks) and the two
/// `fullDocument` modes.
fn change_stream(spec: &Document) -> Option<Document> {
    const KEYS: [&str; 7] = [
        "resumeAfter",
        "startAfter",
        "startAtOperationTime",
        "fullDocument",
        "fullDocumentBeforeChange",
        "allChangesForCluster",
        "showExpandedEvents",
    ];
    if spec.keys().any(|k| !KEYS.contains(&k.as_str())) {
        return None;
    }
    let mut out = Document::new();
    let resume = ["resumeAfter", "startAfter"]
        .into_iter()
        .find_map(|k| Some((k, spec.get(k)?)));
    match resume {
        // A token's representative is the high-water mark at
        // `Timestamp(0, 0)`, a token that parses.
        Some((key, token)) => {
            token.as_document()?.get_str("_data").ok()?;
            out.insert(key, doc! { "_data": "8200000000000000002B0229296E04" });
        }
        None => {
            out.insert(
                "startAtOperationTime",
                Timestamp {
                    time: 0,
                    increment: 0,
                },
            );
        }
    }
    out.insert(
        "fullDocument",
        spec.get("fullDocument")
            .map_or(Some("default"), Bson::as_str)?,
    );
    out.insert(
        "fullDocumentBeforeChange",
        spec.get("fullDocumentBeforeChange")
            .map_or(Some("off"), Bson::as_str)?,
    );
    for key in ["allChangesForCluster", "showExpandedEvents"] {
        if let Some(v) = spec.get(key) {
            out.insert(key, v.as_bool()?);
        }
    }
    Some(out)
}

/// `$geoNear`, its fields in mongod's order.
fn geo_near(spec: &Document) -> Option<Document> {
    let mut out = Document::new();
    if let Some(key) = spec.get("key") {
        out.insert("key", field_path(key.as_str()?));
    }
    out.insert("near", constant(spec.get("near")?, true)?);
    // Optional since 8.0: mongod 8.2.11 shapes `{near: [0, 0]}` with none.
    if let Some(f) = spec.get("distanceField") {
        out.insert("distanceField", field_path(f.as_str()?));
    }
    for k in ["maxDistance", "minDistance"] {
        if let Some(v) = spec.get(k) {
            out.insert(k, rep(v)?);
        }
    }
    let query = match spec.get("query") {
        Some(q) => filter_shape(q.as_document()?)?,
        None => Document::new(),
    };
    out.insert("query", query);
    // A literal: a debug shape prints `?bool` (measured 8.2.11).
    out.insert("spherical", lit(Bson::Boolean(true)));
    if let Some(v) = spec.get("distanceMultiplier") {
        out.insert("distanceMultiplier", rep(v)?);
    }
    if let Some(v) = spec.get("includeLocs") {
        out.insert("includeLocs", field_path(v.as_str()?));
    }
    Some(out)
}

/// A pipeline run against collection `coll` -- an `aggregate`'s own, a
/// `$facet`'s, or a `$unionWith`'s or `$lookup`'s -- each stage as the
/// stages it parses into, a leading `$rankFusion` or `$scoreFusion`
/// desugared. Those two take the collection for the
/// `$unionWith` each input pipeline after the first becomes, and must be
/// the pipeline's first stage (mongod 8.2.11 refuses one after it).
fn pipeline_shape(db: &str, coll: &str, stages: &[Bson]) -> Option<Vec<Bson>> {
    let mut out = Vec::new();
    for (at, stage) in stages.iter().enumerate() {
        let stage = stage.as_document()?;
        let shaped = match (stage.len(), stage.iter().next()) {
            (1, Some((name, _))) if at > 0 && (name == "$rankFusion" || name == "$scoreFusion") => {
                return None;
            }
            (1, Some((name, Bson::Document(spec)))) if name == "$rankFusion" => {
                rank_fusion(db, coll, spec)?
            }
            (1, Some((name, Bson::Document(spec)))) if name == "$scoreFusion" => {
                score_fusion(db, coll, spec)?
            }
            _ => stage_shape(db, coll, stage)?,
        };
        out.extend(shaped.into_iter().map(Bson::Document));
    }
    Some(out)
}

/// The shape of a stage this module builds as a user would write it.
fn built(db: &str, coll: &str, stage: Document) -> Option<Vec<Document>> {
    stage_shape(db, coll, &stage)
}

/// `FieldPath::validateFieldName`, which a `$rankFusion` or `$scoreFusion`
/// input pipeline's name must pass.
fn valid_field_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('$') && !name.contains('.') && !name.contains('\0')
}

/// A hybrid search's input pipelines, by name in `std::map` order (the
/// order mongod desugars them in), each checked as
/// `hybrid_scoring_util::isSelectionPipeline` checks it. A pipeline
/// mongod would refuse, or one using a stage this server cannot run
/// (`$search`, `$vectorSearch`), has no shape.
fn fusion_inputs(input: &Document) -> Option<std::collections::BTreeMap<&str, &Vec<Bson>>> {
    let mut out = std::collections::BTreeMap::new();
    let pipelines = input.get_document("pipelines").ok()?;
    // "A hybrid scoring stage should be run with at least one pipeline."
    if pipelines.is_empty() {
        return None;
    }
    for (name, pipeline) in pipelines {
        let stages = pipeline.as_array()?;
        if !valid_field_name(name) || stages.is_empty() {
            return None;
        }
        for stage in stages {
            let stage = stage.as_document()?;
            let (op, spec) = stage.iter().next()?;
            let selection = match op.as_str() {
                "$_internalInhibitOptimization"
                | "$limit"
                | "$match"
                | "$sample"
                | "$score"
                | "$skip"
                | "$sort" => true,
                "$geoNear" => {
                    let spec = spec.as_document()?;
                    !spec.contains_key("distanceField") && !spec.contains_key("includeLocs")
                }
                // A nested hybrid search is refused (10473002, 10473003);
                // the search stages need mongot.
                _ => false,
            };
            if !selection {
                return None;
            }
        }
        out.insert(name.as_str(), stages);
    }
    Some(out)
}

/// The first stage's name.
fn first_stage(stages: &[Bson]) -> Option<&str> {
    stages
        .first()?
        .as_document()?
        .keys()
        .next()
        .map(String::as_str)
}

/// Whether any stage is `name`.
fn has_stage(stages: &[Bson], name: &str) -> bool {
    stages
        .iter()
        .any(|s| s.as_document().is_some_and(|d| d.contains_key(name)))
}

/// Whether a `$match` filter holds a `$text` predicate
/// (`DocumentSourceMatch::containsTextOperator`).
fn has_text(filter: &Document) -> bool {
    filter.iter().any(|(k, v)| {
        k == "$text"
            || matches!(k.as_str(), "$and" | "$or" | "$nor")
                && v.as_array()
                    .is_some_and(|a| a.iter().any(|c| c.as_document().is_some_and(has_text)))
    })
}

/// Whether a pipeline's `$match` with `$text` makes it produce a `score`
/// (`Pipeline::generatesMetadataType(kScore)` over the selection stages
/// [`fusion_inputs`] admits).
fn generates_score(stages: &[Bson]) -> bool {
    stages.iter().any(|s| {
        s.as_document()
            .and_then(|d| d.get_document("$match").ok())
            .is_some_and(has_text)
    })
}

/// `hybrid_scoring_util::validateWeights`: each weight a non-negative
/// number naming an input pipeline.
fn fusion_weights(weights: &Document, names: &[&str]) -> Option<()> {
    for (name, w) in weights {
        if !names.contains(&name.as_str()) || number(w)? < 0.0 {
            return None;
        }
    }
    Some(())
}

/// `scoreDetails`, a bool defaulting to false.
fn score_details_flag(spec: &Document) -> Option<bool> {
    match spec.get("scoreDetails") {
        None => Some(false),
        Some(Bson::Boolean(b)) => Some(*b),
        Some(_) => None,
    }
}

/// `$rankFusion`, as the pipeline `DocumentSourceRankFusion::createFromBson`
/// desugars it into (mongod 8.2.11, `document_source_rank_fusion.cpp`):
/// each input pipeline, the first inline and the rest in a `$unionWith`,
/// followed by its documents' rank and reciprocal-rank score, then the
/// `$group` that fuses them, the final score and the sort on it.
fn rank_fusion(db: &str, coll: &str, spec: &Document) -> Option<Vec<Document>> {
    if spec
        .keys()
        .any(|k| !["input", "combination", "scoreDetails"].contains(&k.as_str()))
    {
        return None;
    }
    let input = spec.get_document("input").ok()?;
    if input.keys().any(|k| k != "pipelines") {
        return None;
    }
    let inputs = fusion_inputs(input)?;
    let names: Vec<&str> = inputs.keys().copied().collect();
    if let Some(c) = spec.get("combination") {
        let c = c.as_document()?;
        if c.keys().any(|k| k != "weights") {
            return None;
        }
        fusion_weights(c.get_document("weights").ok()?, &names)?;
    }
    let details = score_details_flag(spec)?;
    let mut out = Vec::new();
    for (at, (name, stages)) in inputs.iter().enumerate() {
        // `rankFusionBsonPipelineValidator`: no `$score`, and ranked -- an
        // implicitly ranked first stage or a `$sort`.
        if has_stage(stages, "$score")
            || !(first_stage(stages) == Some("$geoNear") || has_stage(stages, "$sort"))
        {
            return None;
        }
        let mut shaped: Vec<Document> = pipeline_shape(db, coll, stages)?
            .into_iter()
            .map(|b| b.as_document().cloned())
            .collect::<Option<_>>()?;
        let rank = format!("{name}_rank");
        let score = format!("{name}_score");
        shaped.extend(built(
            db,
            coll,
            doc! { "$replaceWith": { "docs": "$$ROOT" } },
        )?);
        shaped.push(doc! { "$_internalSetWindowFields": {
            "sortBy": { field_path("order"): 1 },
            "output": { field_path(&rank): { "$rank": {} } },
        } });
        shaped.extend(built(
            db,
            coll,
            doc! { "$addFields": { &score: { "$multiply": [
                { "$divide": [1, { "$add": [format!("${rank}"), 60] }] },
                1.0,
            ] } } },
        )?);
        if details {
            let value = if generates_score(stages) {
                doc! { "value": { "$meta": "score" }, "details": [] }
            } else {
                doc! { "details": [] }
            };
            shaped.extend(built(
                db,
                coll,
                doc! { "$addFields": { format!("{name}_scoreDetails"): value } },
            )?);
        }
        if at == 0 {
            out.extend(shaped);
        } else {
            let pipeline: Vec<Bson> = shaped.into_iter().map(Bson::Document).collect();
            out.push(doc! { "$unionWith": { "coll": ident(coll), "pipeline": pipeline } });
        }
    }
    let path = |name: &str, suffix: &str| format!("${name}_{suffix}");
    let mut group = doc! { "_id": "$docs._id", "docs": { "$first": "$docs" } };
    for name in &names {
        group.insert(
            format!("{name}_score"),
            doc! { "$max": { "$ifNull": [path(name, "score"), 0] } },
        );
        if details {
            group.insert(
                format!("{name}_rank"),
                doc! { "$max": { "$ifNull": [path(name, "rank"), 0] } },
            );
            group.insert(
                format!("{name}_scoreDetails"),
                doc! { "$mergeObjects": path(name, "scoreDetails") },
            );
        }
    }
    let scores: Vec<Bson> = names
        .iter()
        .map(|n| Bson::String(path(n, "score")))
        .collect();
    out.extend(built(db, coll, doc! { "$group": group })?);
    out.extend(built(
        db,
        coll,
        doc! { "$addFields": { "score": { "$add": scores.clone() } } },
    )?);
    let ranks: Document = names
        .iter()
        .map(|n| {
            let rank = path(n, "rank");
            let cond = doc! { "$cond": [
                { "$eq": [&rank, 0] },
                { "$literal": "NA" },
                &rank,
            ] };
            (format!("{n}_rank"), Bson::Document(cond))
        })
        .collect();
    out.extend(built(db, coll, doc! { "$addFields": ranks })?);
    if details {
        let calculated: Vec<Bson> = names
            .iter()
            .map(|n| {
                let rank = path(n, "rank");
                Bson::Document(doc! { "$mergeObjects": [
                    {
                        "inputPipelineName": *n,
                        "rank": &rank,
                        "weight": { "$cond": [{ "$eq": [&rank, "NA"] }, "$$REMOVE", 1.0] },
                    },
                    path(n, "scoreDetails"),
                ] })
            })
            .collect();
        out.extend(built(
            db,
            coll,
            doc! { "$addFields": { "calculatedScoreDetails": calculated } },
        )?);
        let value = doc! {
            "value": "$score",
            "description": "value output by reciprocal rank fusion algorithm",
            "details": "$calculatedScoreDetails",
        };
        out.push(doc! { "$setMetadata": { "scoreDetails": sub_expr(&Bson::Document(value))? } });
    } else {
        let score = sub_expr(&Bson::Document(doc! { "$add": scores }))?;
        out.push(doc! { "$setMetadata": { "score": score } });
    }
    out.extend(built(
        db,
        coll,
        doc! { "$sort": { "score": -1, "_id": 1 } },
    )?);
    out.extend(built(
        db,
        coll,
        doc! { "$replaceRoot": { "newRoot": "$docs" } },
    )?);
    Some(out)
}

/// `$scoreFusion`, as the pipeline
/// `DocumentSourceScoreFusion::createFromBson` desugars it into (mongod
/// 8.2.11, `document_source_score_fusion.cpp`): each input pipeline, the
/// first inline and the rest in a `$unionWith`, nesting its documents and
/// keeping its weighted, normalized score, then the `$group` and `$reduce`
/// that fuse them, the combined score and the sort on it.
fn score_fusion(db: &str, coll: &str, spec: &Document) -> Option<Vec<Document>> {
    const DOCS: &str = "_internal_scoreFusion_docs";
    const FIELDS: &str = "_internal_scoreFusion_internal_fields";
    if spec
        .keys()
        .any(|k| !["input", "combination", "scoreDetails"].contains(&k.as_str()))
    {
        return None;
    }
    let input = spec.get_document("input").ok()?;
    if input
        .keys()
        .any(|k| k != "pipelines" && k != "normalization")
    {
        return None;
    }
    let normalization = input.get_str("normalization").ok()?;
    if !["none", "sigmoid", "minMaxScaler"].contains(&normalization) {
        return None;
    }
    let inputs = fusion_inputs(input)?;
    let names: Vec<&str> = inputs.keys().copied().collect();
    // `ScoreFusionScoringOptions`: `avg` unless `method` says otherwise;
    // an `expression` only with `method: "expression"`, and never with
    // `weights`.
    let mut expression = None;
    if let Some(c) = spec.get("combination") {
        let c = c.as_document()?;
        if c.keys()
            .any(|k| !["weights", "method", "expression"].contains(&k.as_str()))
        {
            return None;
        }
        if let Some(w) = c.get("weights") {
            fusion_weights(w.as_document()?, &names)?;
        }
        match c.get("method") {
            None => {}
            Some(Bson::String(m)) if m == "avg" => {
                if c.contains_key("expression") {
                    return None;
                }
            }
            Some(Bson::String(m)) if m == "expression" => {
                if c.contains_key("weights") {
                    return None;
                }
                expression = Some(c.get("expression")?);
            }
            Some(_) => return None,
        }
    }
    let details = score_details_flag(spec)?;
    let field = |name: &str, suffix: &str| format!("{FIELDS}.{name}_{suffix}");
    let mut out = Vec::new();
    for (at, (name, stages)) in inputs.iter().enumerate() {
        // `scoreFusionBsonPipelineValidator`: scored -- a first `$match`
        // with `$text`, or a `$score`.
        let text_first = stages
            .first()
            .and_then(Bson::as_document)
            .and_then(|d| d.get_document("$match").ok())
            .is_some_and(has_text);
        if !text_first && !has_stage(stages, "$score") {
            return None;
        }
        let mut shaped: Vec<Document> = pipeline_shape(db, coll, stages)?
            .into_iter()
            .map(|b| b.as_document().cloned())
            .collect::<Option<_>>()?;
        let meta = Bson::Document(doc! { "$meta": "score" });
        let normalized = if normalization == "sigmoid" {
            Bson::Document(doc! { "$sigmoid": meta.clone() })
        } else {
            meta.clone()
        };
        shaped.extend(built(
            db,
            coll,
            doc! { "$replaceWith": { DOCS: "$$ROOT" } },
        )?);
        shaped.extend(built(
            db,
            coll,
            doc! { "$addFields": { field(name, "score"): { "$multiply": [normalized, 1.0] } } },
        )?);
        if details {
            shaped.extend(built(
                db,
                coll,
                doc! { "$addFields": { field(name, "rawScore"): meta } },
            )?);
            shaped.extend(built(
                db,
                coll,
                doc! { "$addFields": { field(name, "scoreDetails"): { "details": [] } } },
            )?);
        }
        if normalization == "minMaxScaler" {
            let score = field(name, "score");
            shaped.push(doc! { "$_internalSetWindowFields": {
                "sortBy": { field_path(&score): -1 },
                "output": { field_path(&score): {
                    "$minMaxScaler": {
                        "input": path_expression(&format!("${score}")),
                        "min": 0,
                        "max": 1,
                    },
                    "window": { "documents": ["unbounded", "unbounded"] },
                } },
            } });
        }
        if at == 0 {
            out.extend(shaped);
        } else {
            let pipeline: Vec<Bson> = shaped.into_iter().map(Bson::Document).collect();
            out.push(doc! { "$unionWith": { "coll": ident(coll), "pipeline": pipeline } });
        }
    }
    let mut push = Document::new();
    let mut initial = Document::new();
    let mut reduce_in = Document::new();
    for name in &names {
        let mut kept = vec![("score", true)];
        if details {
            kept.extend([("rawScore", true), ("scoreDetails", false)]);
        }
        for (suffix, number) in kept {
            let key = format!("{name}_{suffix}");
            let path = format!("${}", field(name, suffix));
            if number {
                push.insert(&key, doc! { "$ifNull": [path, 0] });
                initial.insert(&key, 0);
                reduce_in.insert(
                    &key,
                    doc! { "$max": [format!("$$value.{key}"), format!("$$this.{key}")] },
                );
            } else {
                push.insert(&key, path);
                initial.insert(&key, Document::new());
                reduce_in.insert(
                    &key,
                    doc! { "$mergeObjects": [format!("$$value.{key}"), format!("$$this.{key}")] },
                );
            }
        }
    }
    out.extend(built(
        db,
        coll,
        doc! { "$group": {
            "_id": format!("${DOCS}._id"),
            DOCS: { "$first": format!("${DOCS}") },
            FIELDS: { "$push": push },
        } },
    )?);
    out.extend(built(
        db,
        coll,
        doc! { "$project": {
            DOCS: 1,
            FIELDS: { "$reduce": {
                "input": format!("${FIELDS}"),
                "initialValue": initial,
                "in": reduce_in,
            } },
        } },
    )?);
    out.extend(built(
        db,
        coll,
        doc! { "$replaceRoot": { "newRoot": { "$mergeObjects": [format!("${DOCS}"), "$$ROOT"] } } },
    )?);
    out.extend(built(db, coll, doc! { "$project": { DOCS: 0 } })?);
    let scores = names.iter().map(|n| format!("${}", field(n, "score")));
    let combined = match expression {
        Some(e) => {
            let vars: Document = names
                .iter()
                .zip(scores)
                .map(|(n, s)| (n.to_string(), Bson::String(s)))
                .collect();
            doc! { "$let": { "vars": vars, "in": e.clone() } }
        }
        None => doc! { "$avg": scores.map(Bson::String).collect::<Vec<_>>() },
    };
    out.push(doc! { "$setMetadata": { "score": sub_expr(&Bson::Document(combined))? } });
    if details {
        let calculated: Vec<Bson> = names
            .iter()
            .map(|n| {
                Bson::Document(doc! { "$mergeObjects": [
                    {
                        "inputPipelineName": *n,
                        "inputPipelineRawScore": format!("${}", field(n, "rawScore")),
                        "weight": 1.0,
                        "value": format!("${}", field(n, "score")),
                    },
                    format!("${}", field(n, "scoreDetails")),
                ] })
            })
            .collect();
        out.extend(built(
            db,
            coll,
            doc! { "$addFields": { FIELDS: { "calculatedScoreDetails": calculated } } },
        )?);
        // Every string and the `combination` object are constants, whose
        // representatives do not depend on their values.
        let value = doc! {
            "value": { "$meta": "score" },
            "description": "the value calculated by combining the scores",
            "normalization": normalization,
            "combination": { "method": "average" },
            "details": format!("${FIELDS}.calculatedScoreDetails"),
        };
        out.push(doc! { "$setMetadata": { "scoreDetails": sub_expr(&Bson::Document(value))? } });
    }
    out.extend(built(
        db,
        coll,
        doc! { "$sort": { "score": { "$meta": "score" }, "_id": 1 } },
    )?);
    out.extend(built(db, coll, doc! { "$project": { FIELDS: 0 } })?);
    Some(out)
}

/// A stage's representative shape, as the stages mongod parses it into
/// (`$count` is a `$group` and a `$project`).
fn stage_shape(db: &str, coll: &str, stage: &Document) -> Option<Vec<Document>> {
    if stage.len() != 1 {
        return None;
    }
    let (name, spec) = stage.iter().next()?;
    let name = name.as_str();
    Some(match name {
        "$match" => vec![doc! { "$match": filter_shape(spec.as_document()?)? }],
        "$limit" | "$skip" => {
            number(spec)?;
            vec![doc! { name: lit(Bson::Int32(1)) }]
        }
        "$sort" => vec![doc! { "$sort": sort_shape(spec.as_document()?)? }],
        "$count" => {
            let field = spec.as_str()?;
            vec![
                doc! { "$group": { "_id": constant(&Bson::Null, true)?, field_path(field): count_of_one() } },
                doc! { "$project": { field_path(field): true, field_path("_id"): false } },
            ]
        }
        "$group" => {
            let spec = spec.as_document()?;
            let mut out = doc! { "_id": expr(spec.get("_id")?, true)? };
            out.extend(accumulators(spec.iter().filter(|(k, _)| *k != "_id"))?);
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
                    out.insert("path", path_expression(path));
                }
                Bson::Document(d) => {
                    out.insert("path", path_expression(d.get_str("path").ok()?));
                    if d.get("preserveNullAndEmptyArrays").is_some_and(true_value) {
                        out.insert("preserveNullAndEmptyArrays", lit(Bson::Boolean(true)));
                    }
                    if let Some(index) = d.get("includeArrayIndex") {
                        out.insert("includeArrayIndex", field_path(index.as_str()?));
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
            vec![doc! { "$sample": { "size": lit(Bson::Int32(1)) } }]
        }
        "$sortByCount" => vec![
            doc! { "$group": { "_id": expr(spec, true)?, field_path("count"): count_of_one() } },
            doc! { "$sort": { field_path("count"): -1 } },
        ],
        "$lookup" => {
            let d = spec.as_document()?;
            const KEYS: [&str; 6] = [
                "from",
                "as",
                "localField",
                "foreignField",
                "let",
                "pipeline",
            ];
            if d.keys().any(|k| !KEYS.contains(&k.as_str())) {
                return None;
            }
            let mut out = doc! {
                "from": ident(d.get_str("from").ok()?),
                "as": field_path(d.get_str("as").ok()?),
            };
            match (d.get("localField"), d.get("foreignField")) {
                (Some(l), Some(f)) => {
                    out.insert("localField", field_path(l.as_str()?));
                    out.insert("foreignField", field_path(f.as_str()?));
                }
                (None, None) => {}
                _ => return None,
            }
            match d.get("pipeline") {
                // A join on fields with an empty pipeline is the plain join.
                Some(Bson::Array(p)) if p.is_empty() && out.contains_key("localField") => {}
                Some(p) => {
                    let vars = match d.get("let") {
                        Some(l) => let_shape(l.as_document()?)?,
                        None => Document::new(),
                    };
                    out.insert("let", vars);
                    let from = d.get_str("from").ok()?;
                    let p = p.as_array()?;
                    out.insert("pipeline", pipeline_shape(db, from, p)?);
                    // `DocumentSourceLookUp::serializeToArray` flags a
                    // hybrid search's join (`isHybridSearchPipeline`): the
                    // hash covers it, and mongod 8.2.11's `$queryStats`
                    // key, the debug shape, does not print it (measured
                    // 2026-11-12).
                    if !debug() && (has_stage(p, "$rankFusion") || has_stage(p, "$scoreFusion")) {
                        out.insert("$_internalIsHybridSearch", true);
                    }
                }
                None if d.contains_key("let") || !out.contains_key("localField") => return None,
                None => {}
            }
            vec![doc! { "$lookup": out }]
        }
        "$facet" => {
            let facets = spec
                .as_document()?
                .iter()
                .map(|(k, p)| {
                    Some((
                        field_path(k),
                        Bson::Array(pipeline_shape(db, coll, p.as_array()?)?),
                    ))
                })
                .collect::<Option<Document>>()?;
            vec![doc! { "$facet": facets }]
        }
        "$unionWith" => {
            let (coll, pipeline) = match spec {
                Bson::String(c) => (c.as_str(), Vec::new()),
                Bson::Document(d) => {
                    if d.keys().any(|k| k != "coll" && k != "pipeline") {
                        return None;
                    }
                    let coll = d.get_str("coll").ok()?;
                    let pipeline = match d.get("pipeline") {
                        Some(p) => pipeline_shape(db, coll, p.as_array()?)?,
                        None => Vec::new(),
                    };
                    (coll, pipeline)
                }
                _ => return None,
            };
            vec![doc! { "$unionWith": { "coll": ident(coll), "pipeline": pipeline } }]
        }
        "$graphLookup" => {
            let d = spec.as_document()?;
            let mut out = doc! {
                "from": ident(d.get_str("from").ok()?),
                "as": field_path(d.get_str("as").ok()?),
                "connectToField": field_path(d.get_str("connectToField").ok()?),
                "connectFromField": field_path(d.get_str("connectFromField").ok()?),
                "startWith": sub_expr(d.get("startWith")?)?,
            };
            if let Some(f) = d.get("depthField") {
                out.insert("depthField", field_path(f.as_str()?));
            }
            if let Some(m) = d.get("maxDepth") {
                out.insert("maxDepth", rep(m)?);
            }
            if let Some(q) = d.get("restrictSearchWithMatch") {
                out.insert("restrictSearchWithMatch", filter_shape(q.as_document()?)?);
            }
            vec![doc! { "$graphLookup": out }]
        }
        "$bucket" => bucket(spec.as_document()?)?,
        "$bucketAuto" => {
            let d = spec.as_document()?;
            number(d.get("buckets")?)?;
            let mut out =
                doc! { "groupBy": sub_expr(d.get("groupBy")?)?, "buckets": lit(Bson::Int32(1)) };
            if let Some(g) = d.get("granularity") {
                out.insert("granularity", lit(Bson::String(g.as_str()?.into())));
            }
            let output = match d.get_document("output") {
                Ok(o) => accumulators(o.iter())?,
                Err(_) => doc! { field_path("count"): count_of_one() },
            };
            out.insert("output", output);
            vec![doc! { "$bucketAuto": out }]
        }
        "$redact" => vec![doc! { "$redact": sub_expr(spec)? }],
        "$setWindowFields" => {
            let d = spec.as_document()?;
            let output = d
                .get_document("output")
                .ok()?
                .iter()
                .map(|(k, v)| Some((field_path(k), Bson::Document(window_output(v)?))))
                .collect::<Option<Document>>()?;
            let sort = match d.get("sortBy") {
                Some(s) => Some(s.as_document()?),
                None => None,
            };
            window_stages(d.get("partitionBy").map(Partition::By), sort, output)?
        }
        "$fill" => fill(spec.as_document()?)?,
        "$densify" => densify(spec.as_document()?)?,
        "$out" => {
            let (out_db, coll) = match spec {
                Bson::String(c) => (db, c.as_str()),
                Bson::Document(d) if d.len() == 2 => {
                    (d.get_str("db").ok()?, d.get_str("coll").ok()?)
                }
                _ => return None,
            };
            vec![doc! { "$out": { "coll": ident(coll), "db": ident(out_db) } }]
        }
        "$merge" => vec![doc! { "$merge": merge(db, coll, spec)? }],
        "$geoNear" => vec![doc! { "$geoNear": geo_near(spec.as_document()?)? }],
        "$collStats" => vec![doc! { "$collStats": coll_stats(spec.as_document()?)? }],
        "$changeStream" => vec![doc! { "$changeStream": change_stream(spec.as_document()?)? }],
        "$changeStreamSplitLargeEvent" if spec.as_document()?.is_empty() => {
            vec![doc! { "$changeStreamSplitLargeEvent": {} }]
        }
        "$indexStats" if spec.as_document()?.is_empty() => vec![doc! { "$indexStats": {} }],
        "$planCacheStats" => {
            let d = spec.as_document()?;
            let all_hosts = match d.get("allHosts") {
                None if d.is_empty() => false,
                Some(Bson::Boolean(b)) if d.len() == 1 => *b,
                _ => return None,
            };
            vec![doc! { "$planCacheStats": { "allHosts": all_hosts } }]
        }
        "$score" => {
            let d = spec.as_document()?;
            if d.keys()
                .any(|k| !["score", "normalization", "weight"].contains(&k.as_str()))
            {
                return None;
            }
            let meta = || Bson::Document(doc! { "$meta": "score" });
            let mut out = vec![
                doc! { "$setMetadata": { "score": sub_expr(d.get("score")?)? } },
                doc! { "$replaceRoot": { "newRoot": { field_path("docs"): "$$ROOT" } } },
                doc! { "$addFields": { field_path("internal_raw_score"): meta() } },
            ];
            match d.get("normalization").map(Bson::as_str) {
                None | Some(Some("none")) => {}
                Some(Some("sigmoid")) => {
                    out.push(doc! { "$setMetadata": { "score": sub_expr(&sigmoid(meta()))? } });
                }
                Some(Some("minMaxScaler")) => {
                    const SCALED: &str = "internal_min_max_scaler_normalization_score";
                    out.push(doc! { "$_internalSetWindowFields": {
                        "sortBy": { field_path(SCALED): -1 },
                        "output": { field_path(SCALED): {
                            "$minMaxScaler": { "input": meta(), "min": 0, "max": 1 },
                            "window": { "documents": ["unbounded", "unbounded"] },
                        } },
                    } });
                    out.push(doc! { "$setMetadata": { "score": path_expression(&format!("${SCALED}")) } });
                }
                _ => return None,
            }
            // A weight of 1 is no weighting at all.
            if let Some(w) = d.get("weight").filter(|w| number(w) != Some(1.0)) {
                let weighted = Bson::Document(doc! { "$multiply": [meta(), w.clone()] });
                out.push(doc! { "$setMetadata": { "score": sub_expr(&weighted)? } });
            }
            out.push(doc! { "$replaceRoot": { "newRoot": path_expression("$docs") } });
            out
        }
        "$documents" => {
            let docs = match spec {
                Bson::Array(a) if a.iter().all(|d| d.as_document().is_some()) => {
                    constant(spec, true)?
                }
                Bson::Array(_) => return None,
                other => sub_expr(other)?,
            };
            const FIELD: &str = "_tempDocumentsField";
            vec![
                doc! { "$queue": lit(Bson::Array(vec![Bson::Document(doc! { "?": "?" })])) },
                doc! { "$project": { field_path("_id"): true, field_path(FIELD): docs } },
                doc! { "$unwind": { "path": path_expression(&format!("${FIELD}")) } },
                doc! { "$replaceRoot": { "newRoot": path_expression(&format!("${FIELD}")) } },
            ]
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

    /// The hashes mongod 8.2.11 printed for nodes with serializers of their
    /// own, in database `qsp` (`explain`, 2026-11-09).
    #[test]
    fn hashes_of_nodes_serialized_their_own_way_match_mongods() {
        let cases = [
            (
                doc! { "find": "c", "filter": { "loc": { "$geoWithin": { "$center": [[0, 0], 5] } } } },
                "6A48B45973DD72F6020CBFC7FC925B07F54553E69B6D5C60D3502EB7230ED7D6",
            ),
            (
                doc! { "find": "c", "filter": { "$where": "this.x > 1", "y": 2 } },
                "6C9E4CAEA81C35F4C4476AE50BAD5B17120C0D5FDD4FF68F6ACAF438C8B4C7BD",
            ),
            // `$function`'s body is a bare literal, its language as written.
            (
                doc! { "aggregate": "c", "pipeline": [{ "$project": { "r": { "$function": {
                    "lang": "js",
                    "args": [1, "$x"],
                    "body": Bson::JavaScriptCode("function(a,b){return a}".into()),
                } } } }], "cursor": {} },
                "E7D27803B7FD56F6A52A7308072A680E2624C8B0B8B0072E1208F5E8507F3C56",
            ),
            (
                doc! { "find": "c", "filter": { "$text": { "$search": "hello" } } },
                "BD051374B5DEFB89A655E26AF4D8F52E4ED1A1FFD63B2BED10E769D0DC43DF95",
            ),
            // A `$meta` projection makes a find's projection neither an
            // inclusion nor an exclusion; a `$meta` sort key is named
            // `$computed<position>`.
            (
                doc! {
                    "find": "c",
                    "filter": { "$text": { "$search": "hello" } },
                    "projection": { "sc": { "$meta": "textScore" } },
                    "sort": { "sc": { "$meta": "textScore" } },
                },
                "5E020697413CA45BF86AA46BBAABFD2B219734BD06147CD0E1E34CBA897F05FD",
            ),
            (
                doc! {
                    "find": "c",
                    "filter": { "$text": { "$search": "hello" } },
                    "sort": {
                        "x": 1, "sc": { "$meta": "textScore" }, "y": -1, "t": { "$meta": "textScore" },
                    },
                },
                "222CB435FEB61BCACF15FC3E64B08E5F6411AE771FE4366ED0EFF0A5C05E6DDB",
            ),
            (
                doc! { "aggregate": "c", "pipeline": [
                    { "$project": { "r": { "$dateToString": {
                        "onNull": "n", "timezone": "UTC", "format": "%Y", "date": "$t",
                    } } } },
                ], "cursor": {} },
                "8F186F95DB2C70BA0F7C3A62F6A0312491A11FCAEA66CC9C152A5B3F849E4A43",
            ),
            (
                doc! { "aggregate": "c", "pipeline": [
                    { "$facet": { "a": [{ "$match": { "x": 1 } }], "b": [{ "$count": "n" }] } },
                ], "cursor": {} },
                "ADF4EEAEE98A9534367A425CA4692922C8AC970283FDBB33C75ABB75C855E91D",
            ),
            (
                doc! { "aggregate": "c", "pipeline": [
                    { "$bucket": { "groupBy": "$x", "boundaries": [0, 5, 10], "default": "o" } },
                ], "cursor": {} },
                "C6BE87C5CF477CA6A091EBB1F7028BAEBA419445F87366E730AF367D6A298BCB",
            ),
            (
                doc! { "aggregate": "c", "pipeline": [{ "$setWindowFields": {
                    "sortBy": { "x": 1 },
                    "output": { "s": { "$sum": "$x", "window": { "documents": [-1, 1] } } },
                } }], "cursor": {} },
                "903192CA46E7D216079868135564C4D7F08F480EDA702DA5E45A7256BE853A07",
            ),
            (
                doc! { "aggregate": 1, "pipeline": [{ "$documents": [{ "x": 1 }] }], "cursor": {} },
                "D655CE1CC7706CB567B4990E45DF39D6F632F136D0E39415EB48EC12836EEDE9",
            ),
            (
                doc! { "aggregate": "c", "pipeline": [{ "$changeStream": {} }], "cursor": {} },
                "CDEE6FF81596AFA736105A1711C3B97EE2A9FDD7AB8E264ADED09578625994F8",
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(
                hash("qsp", &command).as_deref(),
                Some(expected),
                "{command}"
            );
        }
    }

    /// `{x: DBPointer("a.b", 000102...0b)}`: mongod 8.2.11's hash, the
    /// literal standing as `DBPointer("?.?", OID::max())`.
    #[test]
    fn a_dbpointer_literal_has_mongods_representative() {
        let mut element = vec![0x0c, b'x', 0];
        element.extend_from_slice(&4_i32.to_le_bytes());
        element.extend_from_slice(b"a.b\0");
        element.extend((0..12).map(|b: u8| b));
        let mut raw = i32::try_from(element.len() + 5)
            .unwrap()
            .to_le_bytes()
            .to_vec();
        raw.extend_from_slice(&element);
        raw.push(0);
        let filter = secantus_core::bsondoc::from_bytes(&raw).unwrap();
        let find = doc! { "find": "c", "filter": filter };
        assert_eq!(
            hash("qsp", &find).as_deref(),
            Some("3E78DBB5E29016964A8F49768C470F78C1DDBD2D8E3D6EB19F29A741CB19ABEA")
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

    /// `$rankFusion` and `$scoreFusion` hash as the pipelines mongod
    /// desugars them into: the hashes mongod 8.2.11 printed for these in
    /// database `q358b` (`explain`, 2026-11-12) -- input pipelines out of
    /// name order, weights, score details, each normalization, a custom
    /// combination and a `$text` input.
    #[test]
    fn hybrid_search_hashes_match_mongods() {
        let rf = Bson::Document(doc! { "$rankFusion": { "input": { "pipelines": {
            "a": [{ "$sort": { "x": 1 } }],
            "b": [{ "$sort": { "x": -1 } }],
        } } } });
        let cases = [
            // rf_sd
            (
                doc! { "aggregate": "c", "pipeline": [{ "$rankFusion": { "input": { "pipelines": { "a": [{ "$sort": { "x": 1 } }], "b": [{ "$sort": { "x": -1 } }] } }, "scoreDetails": true } }], "cursor": {  } },
                "0DB57D02ECD7B44779F4CA02AE53EFD8D5066264B8F87FA4C11610820F3D6BE4",
            ),
            // rf_w
            (
                doc! { "aggregate": "c", "pipeline": [{ "$rankFusion": { "input": { "pipelines": { "a": [{ "$sort": { "x": 1 } }], "b": [{ "$sort": { "x": -1 } }] } }, "combination": { "weights": { "a": 2 } } } }], "cursor": {  } },
                "7291ED522A85B81F6F38DD08A3A6DCE7AAB55E3C4BA357D7AA1AE3E8251AC326",
            ),
            // rf_3
            (
                doc! { "aggregate": "c", "pipeline": [{ "$rankFusion": { "input": { "pipelines": { "b": [{ "$match": { "x": { "$gt": 0 } } }, { "$sort": { "x": 1 } }, { "$limit": 5 }], "a": [{ "$sort": { "x": -1 } }, { "$skip": 1 }], "Z": [{ "$geoNear": { "near": [0, 0] } }] } } } }], "cursor": {  } },
                "4E91FE069C566F27612689C724B8A1D0F2AC1100D09A4F14747DD556AFEA0754",
            ),
            // rf_one
            (
                doc! { "aggregate": "c", "pipeline": [{ "$rankFusion": { "input": { "pipelines": { "only": [{ "$sort": { "x": 1, "y": -1 } }] } } } }], "cursor": {  } },
                "205CDCE99EBDAFC76F41A860E804737D9D5A1D08D338E110E915280DAD487715",
            ),
            // rf_text_sd
            (
                doc! { "aggregate": "c", "pipeline": [{ "$rankFusion": { "input": { "pipelines": { "a": [{ "$match": { "$text": { "$search": "hello" } } }, { "$sort": { "s": { "$meta": "textScore" } } }], "b": [{ "$sort": { "x": 1 } }] } }, "scoreDetails": true } }], "cursor": {  } },
                "89B59427C80532D4A1A3081D5B7D23DFB0D456870685BB3FBC309E2764F219FA",
            ),
            // sf_sig
            (
                doc! { "aggregate": "c", "pipeline": [{ "$scoreFusion": { "input": { "pipelines": { "a": [{ "$score": { "score": "$x" } }], "b": [{ "$score": { "score": { "$add": ["$x", 1] } } }] }, "normalization": "sigmoid" } } }], "cursor": {  } },
                "78EA92F5DDA56783B9494981CE53BE7A6E0C7247C686BC74CD808D080C80E398",
            ),
            // sf_mm
            (
                doc! { "aggregate": "c", "pipeline": [{ "$scoreFusion": { "input": { "pipelines": { "a": [{ "$score": { "score": "$x" } }] }, "normalization": "minMaxScaler" } } }], "cursor": {  } },
                "6A56C3B2A47726C95DDE639C5DB341C641749457A8E1FCD9F9D70568D16E2A51",
            ),
            // sf_expr
            (
                doc! { "aggregate": "c", "pipeline": [{ "$scoreFusion": { "input": { "pipelines": { "a": [{ "$score": { "score": "$x" } }], "b": [{ "$score": { "score": "$x" } }] }, "normalization": "none" }, "combination": { "method": "expression", "expression": { "$add": ["$$a", { "$multiply": ["$$b", 2] }] } } } }], "cursor": {  } },
                "D61769AF3672C0DD39E42A105804F93B11F2D7386A06D366D6F5091AD7649426",
            ),
            // sf_w
            (
                doc! { "aggregate": "c", "pipeline": [{ "$scoreFusion": { "input": { "pipelines": { "a": [{ "$score": { "score": "$x" } }], "b": [{ "$score": { "score": "$x" } }] }, "normalization": "none" }, "combination": { "weights": { "b": 0.5 }, "method": "avg" } } }], "cursor": {  } },
                "AC682F5060B25B9D2D3D65E47DBB660C6C347B604061B4548D5EFE422877C881",
            ),
            // sf_sd
            (
                doc! { "aggregate": "c", "pipeline": [{ "$scoreFusion": { "input": { "pipelines": { "a": [{ "$score": { "score": "$x" } }], "b": [{ "$match": { "x": 1 } }, { "$score": { "score": "$x", "normalization": "sigmoid", "weight": 0.5 } }] }, "normalization": "none" }, "scoreDetails": true } }], "cursor": {  } },
                "012F43083D247EF5FC73EC29F38767D7E1E9C9289801742E8935A36F5D048263",
            ),
            // sf_sd_expr
            (
                doc! { "aggregate": "c", "pipeline": [{ "$scoreFusion": { "input": { "pipelines": { "a": [{ "$score": { "score": "$x" } }] }, "normalization": "none" }, "scoreDetails": true, "combination": { "method": "expression", "expression": { "$add": ["$$a", 1] } } } }], "cursor": {  } },
                "0A70C8DE1BB3614C02BEF148919813B68BBF842DAB25ADE8C58E198E718FBEEF",
            ),
            // sf_text
            (
                doc! { "aggregate": "c", "pipeline": [{ "$scoreFusion": { "input": { "pipelines": { "a": [{ "$match": { "$text": { "$search": "hello" } } }] }, "normalization": "none" } } }], "cursor": {  } },
                "C885CC9CAF3B56E803995684CCE44B4873A3C5FD706AA9F475A0E77FC68B050D",
            ),
            // sf_text_sd
            (
                doc! { "aggregate": "c", "pipeline": [{ "$scoreFusion": { "input": { "pipelines": { "a": [{ "$match": { "$text": { "$search": "hello" } } }, { "$limit": 3 }] }, "normalization": "none" }, "scoreDetails": true } }], "cursor": {  } },
                "4436A83F3574B01407F25D1D0C0B3B4A465B0628F15E9786DC8C203196D77E98",
            ),
            // `$geoNear`'s `distanceField` is optional.
            (
                doc! { "aggregate": "c", "pipeline": [{ "$geoNear": { "near": [0, 0] } }], "cursor": {} },
                "63CC52833F6A590FE83AA9D389F1BFD5E530AB2C5E870774A85BD5B1D58F0C67",
            ),
            // Nested, a hybrid search runs against the collection of the
            // stage that holds it.
            (
                doc! { "aggregate": "c", "pipeline": [{ "$unionWith": { "coll": "o", "pipeline": [rf.clone()] } }], "cursor": {} },
                "83ED2D2FA9B9E84CB31C3D28BEA296F51B5ADD9D95F73A901571468480917BBB",
            ),
            (
                doc! { "aggregate": "c", "pipeline": [{ "$lookup": { "from": "o", "as": "j", "pipeline": [rf.clone()] } }], "cursor": {} },
                "9D31F3C70DE4DF920AF6AE099BD1EA37408E280FB7713572902A80E707CC22BF",
            ),
            (
                doc! { "aggregate": "c", "pipeline": [{ "$facet": { "f": [rf.clone()] } }], "cursor": {} },
                "47D1504A5E226EEFC2CC5895CECB27DC9C1E1FB0682B929C3830CBACB6908C28",
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(
                hash("q358b", &command).as_deref(),
                Some(expected),
                "{command}"
            );
        }
    }

    /// `transformIdentifiers` under the key `0123456789abcdef` twice: the
    /// key mongod 8.2.11 printed for this `find` (2026-11-14) -- each path
    /// component and the namespace as its HMAC, `_id` too.
    #[test]
    fn identifiers_transform_as_mongods() {
        let find = doc! {
            "find": "c",
            "filter": { "a": { "$gt": 1 }, "b.c": "x" },
            "projection": { "a": 1 },
            "sort": { "a": 1 },
        };
        let key = b"0123456789abcdef0123456789abcdef";
        let shape = with_hmac(key, || debug_shape("rm215r", &find)).unwrap();
        let a = "qiGHcjZFolIUi786rQ4g1MOihhI1u2WiZJZ6+dRvrsI=";
        let c = "+GIsxB8Q0QqpsXvrZxWo5jG/74lb8GWX2mvtZvLgPSk=";
        let b = "T0MsOqnZpzT9WNRha7ZJm1wuGIsixFBshsf92M9FAP4=";
        let id = "v2F7SFtXfDR4NQAoA/jCbiq80fRAMT7Hw1mpgXcw8S8=";
        assert_eq!(
            shape,
            doc! {
                "cmdNs": { "db": "JYY2uxOLLDvknJ56kp7JhXENu7H0VD3LD3Le/6Gy0ws=", "coll": c },
                "command": "find",
                "filter": { "$and": [
                    { a: { "$gt": "?number" } },
                    { format!("{b}.{c}"): { "$eq": "?string" } },
                ] },
                "projection": { a: true, id: true },
                "sort": { a: 1 },
            }
        );
        // Outside `with_hmac`, nothing is transformed.
        assert_eq!(
            debug_shape("rm215r", &find)
                .unwrap()
                .get_document("sort")
                .unwrap(),
            &doc! { "a": 1 }
        );
    }

    /// `ExpressionFieldPath::serialize`: `$$CURRENT.a` is `$a`; a builtin
    /// variable keeps its name, a user's is transformed with its path.
    #[test]
    fn a_variable_keeps_its_name_only_when_builtin() {
        let dollars = "$$";
        assert_eq!(path_expression(&format!("{dollars}CURRENT.a")), "$a");
        assert_eq!(
            path_expression(&format!("{dollars}CURRENT")),
            format!("{dollars}CURRENT")
        );
        let key = b"0123456789abcdef0123456789abcdef";
        let (v, x) = with_hmac(key, || (ident("v"), ident("x")));
        let got: Vec<String> = with_hmac(key, || {
            ["ROOT", "NOW", "ROOT.x", "v.x"]
                .iter()
                .map(|p| path_expression(&format!("{dollars}{p}")))
                .chain([path_expression("$x")])
                .collect()
        });
        assert_eq!(
            got,
            [
                format!("{dollars}ROOT"),
                format!("{dollars}NOW"),
                format!("{dollars}ROOT.{x}"),
                format!("{dollars}{v}.{x}"),
                format!("${x}"),
            ]
        );
    }

    #[test]
    fn what_it_cannot_shape_has_no_hash() {
        assert_eq!(
            hash("d", &doc! { "find": "c", "filter": { "$jsonSchema": {} } }),
            None
        );
        // mongod refuses a hybrid search with no input pipeline, and one
        // that is not the pipeline's first stage.
        let rank_fusion = doc! { "$rankFusion": { "input": { "pipelines": {} } } };
        assert_eq!(
            hash(
                "d",
                &doc! { "aggregate": "c", "pipeline": [rank_fusion], "cursor": {} }
            ),
            None
        );
        let rank_fusion = doc! { "$rankFusion": { "input": { "pipelines": { "a": [{ "$sort": { "x": 1 } }] } } } };
        let second = doc! {
            "aggregate": "c",
            "pipeline": [{ "$match": { "x": 1 } }, rank_fusion.clone()],
            "cursor": {},
        };
        assert_eq!(hash("d", &second), None);
        let first = doc! { "aggregate": "c", "pipeline": [rank_fusion], "cursor": {} };
        assert!(hash("d", &first).is_some());
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
