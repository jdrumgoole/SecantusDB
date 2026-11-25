//! `explain` of a `$changeStream` (RM-076): the read of the oplog mongod
//! runs for it, and the internal stages it expands into.
//!
//! mongod 8.2 explains a change stream as a `$cursor` over `local.oplog.rs`
//! -- a `PROJECTION_SIMPLE` of the oplog fields over a forward `COLLSCAN`
//! from the start time -- whose filter its change-stream filter builders
//! make from the scope and the options, then `internalUnwindTransaction`,
//! `internalTransform`, `$_internalChangeStreamCheckInvalidate`,
//! `$_internalChangeStreamCheckResumability`, the `$match` on the event
//! types, the pre- and post-image stages, the user's stages, and
//! `$_internalChangeStreamEnsureResumeTokenPresent` for a resumed stream.
//!
//! The filter is built here as mongod builds it -- a tree of match
//! expressions, one builder per kind of oplog entry -- then optimized
//! (nested `$and` / `$or` flattened) and sorted by mongod's
//! `sortTree` comparator (match type, then path, then children), which is
//! what `parsedQuery` prints. Measured against mongod 8.2.11 for the three
//! scopes and every option that changes the filter
//! (`tests/mongod_live/test_change_stream_explain.py`).
//!
//! A `$match` on `operationType` that follows the stream -- with the
//! `$match` on the event types a stream without `showExpandedEvents`
//! starts with -- is rewritten into predicates on the oplog entry and
//! joins the read's filter and the unwind's (RM-452), as mongod's
//! `change_stream_rewrite_helpers.cpp` rewrites it.
//!
//! So are a `$match` on `documentKey`, (inexactly) on `fullDocument`, on
//! `ns` and `to`, and an `$expr` on `operationType` (RM-698). Not
//! reproduced: the rewrites of `updateDescription`,
//! `fullDocumentBeforeChange`, the renamed `clusterTime` / `lsid` /
//! `txnNumber`, and of an `$expr` on any field but `operationType`
//! (RM-698), and the
//! change stream's `queryShapeHash` (RM-454). The unwind's filter goes through
//! mongod's Boolean simplifier (`simplify.rs`, RM-699).

use bson::{doc, Bson, Document, Timestamp};

use super::{cache_key, hoist, server_info, server_parameters, shape, Verbosity};
use crate::{CommandContext, HandlerResult};

/// What a change stream watches.
enum Scope<'a> {
    Coll { db: &'a str, coll: &'a str },
    Db(&'a str),
    Cluster,
}

/// The `$changeStream` options that shape what mongod runs.
struct Spec<'a> {
    raw: &'a Document,
    expanded: bool,
    system: bool,
    migration: bool,
    full_document: &'a str,
    before_change: &'a str,
}

impl<'a> Spec<'a> {
    fn of(raw: &'a Document) -> Self {
        let flag = |k: &str| raw.get_bool(k).unwrap_or(false);
        Spec {
            raw,
            expanded: flag("showExpandedEvents"),
            system: flag("showSystemEvents"),
            migration: flag("showMigrationEvents"),
            full_document: raw.get_str("fullDocument").unwrap_or("default"),
            before_change: raw.get_str("fullDocumentBeforeChange").unwrap_or("off"),
        }
    }

    fn resume(&self) -> Option<(&'static str, &'a Document)> {
        ["resumeAfter", "startAfter"]
            .into_iter()
            .find_map(|k| self.raw.get_document(k).ok().map(|t| (k, t)))
    }

    /// Whether the stream reads images from the pre-image collection,
    /// which reads the oplog fields that find them.
    fn reads_images(&self) -> bool {
        self.before_change != "off" || matches!(self.full_document, "whenAvailable" | "required")
    }
}

// ------------------------------------------------------------- filter --

/// A match expression's type, in mongod's `MatchExpression::MatchType`
/// order, which `sortTree` sorts by first.
#[derive(Clone, Copy)]
enum Leaf {
    Eq,
    Lte,
    Lt,
    Gt,
    Gte,
    Regex,
    Exists,
    In,
    Type,
}

impl Leaf {
    fn rank(self) -> u8 {
        match self {
            Leaf::Eq => 5,
            Leaf::Lte => 6,
            Leaf::Lt => 7,
            Leaf::Gt => 8,
            Leaf::Gte => 9,
            Leaf::Regex => 10,
            Leaf::Exists => 12,
            Leaf::In => 13,
            Leaf::Type => 20,
        }
    }

    fn op(self) -> &'static str {
        match self {
            Leaf::Eq => "$eq",
            Leaf::Lte => "$lte",
            Leaf::Lt => "$lt",
            Leaf::Gt => "$gt",
            Leaf::Gte => "$gte",
            Leaf::Regex => "$regex",
            Leaf::Exists => "$exists",
            Leaf::In => "$in",
            Leaf::Type => "$type",
        }
    }
}

/// One node of the oplog filter.
#[derive(Clone)]
enum M {
    True,
    False,
    And(Vec<M>),
    Or(Vec<M>),
    Nor(Vec<M>),
    Leaf(String, Leaf, Bson),
    Not(Box<M>),
    ElemObject(String, Box<M>),
    /// `$expr` of an aggregation expression, as `parsedQuery` prints it.
    Expr(Document),
}

impl M {
    fn rank(&self) -> u8 {
        match self {
            M::And(_) => 0,
            M::Or(_) => 1,
            M::ElemObject(..) => 2,
            M::Leaf(_, leaf, _) => leaf.rank(),
            M::Not(_) => 18,
            M::Nor(_) => 19,
            M::Expr(_) => 23,
            M::False => 24,
            M::True => 25,
        }
    }

    fn path(&self) -> &str {
        match self {
            M::Leaf(p, ..) | M::ElemObject(p, _) => p,
            _ => "",
        }
    }

    fn children(&self) -> &[M] {
        match self {
            M::And(c) | M::Or(c) | M::Nor(c) => c,
            M::Not(c) | M::ElemObject(_, c) => std::slice::from_ref(&**c),
            M::Leaf(..) | M::Expr(_) | M::True | M::False => &[],
        }
    }

    /// mongod's `matchExpressionComparator`.
    fn compare(&self, other: &M) -> std::cmp::Ordering {
        self.rank()
            .cmp(&other.rank())
            .then_with(|| self.path().as_bytes().cmp(other.path().as_bytes()))
            .then_with(|| {
                let (a, b) = (self.children(), other.children());
                a.iter()
                    .zip(b)
                    .map(|(x, y)| x.compare(y))
                    .find(|o| o.is_ne())
                    .unwrap_or_else(|| a.len().cmp(&b.len()))
            })
    }

    /// The optimizer's flattening -- an `$and` in an `$and` (an `$or` in an
    /// `$or`) gives up its children to it, and a one-child `$and` / `$or` is
    /// its child -- its folding of `$alwaysTrue` / `$alwaysFalse`, its
    /// merging of an `$or`'s equalities on one path into an `$in`, then
    /// `sortTree`: children first, each list stably.
    fn normalized(self) -> M {
        let flat = |children: Vec<M>, and: bool| {
            let mut out = Vec::new();
            for c in children.into_iter().map(M::normalized) {
                match c {
                    M::And(g) if and => out.extend(g),
                    M::Or(g) if !and => out.extend(g),
                    other => out.push(other),
                }
            }
            out
        };
        let single = |mut c: Vec<M>, empty: M, wrap: fn(Vec<M>) -> M| match c.len() {
            0 => empty,
            1 => c.pop().expect("one"),
            _ => {
                c.sort_by(M::compare);
                wrap(c)
            }
        };
        match self {
            M::And(c) => {
                let c = flat(c, true);
                if c.iter().any(|m| matches!(m, M::False)) {
                    return M::False;
                }
                let c = c.into_iter().filter(|m| !matches!(m, M::True)).collect();
                single(c, M::True, M::And)
            }
            M::Or(c) => {
                let c = flat(c, false);
                if c.iter().any(|m| matches!(m, M::True)) {
                    return M::True;
                }
                let c = c.into_iter().filter(|m| !matches!(m, M::False)).collect();
                single(merge_equalities(c), M::False, M::Or)
            }
            M::Nor(c) => {
                let c: Vec<M> = c.into_iter().map(M::normalized).collect();
                if c.iter().any(|m| matches!(m, M::True)) {
                    return M::False;
                }
                let mut c: Vec<M> = c.into_iter().filter(|m| !matches!(m, M::False)).collect();
                match c.len() {
                    0 => M::True,
                    1 => M::Not(Box::new(c.pop().expect("one"))).normalized(),
                    _ => {
                        c.sort_by(M::compare);
                        M::Nor(c)
                    }
                }
            }
            M::Not(c) => match c.normalized() {
                M::True => M::False,
                M::False => M::True,
                c => M::Not(Box::new(c)),
            },
            M::ElemObject(p, c) => M::ElemObject(p, Box::new(c.normalized())),
            leaf => leaf,
        }
    }

    /// As `parsedQuery` prints it.
    fn to_doc(&self) -> Document {
        let list = |c: &[M]| Bson::Array(c.iter().map(|m| Bson::Document(m.to_doc())).collect());
        match self {
            M::And(c) => doc! { "$and": list(c) },
            M::Or(c) => doc! { "$or": list(c) },
            M::Nor(c) => doc! { "$nor": list(c) },
            M::Leaf(path, leaf, value) => doc! { path.clone(): { leaf.op(): value.clone() } },
            M::True => doc! { "$alwaysTrue": 1 },
            M::False => doc! { "$alwaysFalse": 1 },
            M::Expr(e) => doc! { "$expr": e.clone() },
            // A `$not` of a tree prints as the `$nor` of it.
            M::Not(child) if !matches!(**child, M::Leaf(..)) => doc! { "$nor": [child.to_doc()] },
            M::Not(child) => {
                let inner = child.to_doc();
                let (path, pred) = inner.into_iter().next().expect("a leaf");
                doc! { path: { "$not": pred } }
            }
            M::ElemObject(path, child) => doc! { path.clone(): { "$elemMatch": child.to_doc() } },
        }
    }
}

/// The optimizer's `$in` of an `$or`'s equalities (and `$in`s) on one
/// path, where there are two or more of them; the values sorted, once each.
fn merge_equalities(children: Vec<M>) -> Vec<M> {
    let mergeable = |m: &M| match m {
        M::Leaf(_, Leaf::Eq, v) => !matches!(v, Bson::RegularExpression(_)),
        M::Leaf(_, Leaf::In, _) => true,
        _ => false,
    };
    let mut paths: Vec<String> = Vec::new();
    for c in children.iter().filter(|m| mergeable(m)) {
        let p = c.path().to_string();
        // Two equalities make the `$in`; `$in`s alone stay as they are.
        let many = children
            .iter()
            .filter(|m| matches!(m, M::Leaf(_, Leaf::Eq, _)) && mergeable(m) && m.path() == p)
            .count()
            > 1;
        if many && !paths.contains(&p) {
            paths.push(p);
        }
    }
    if paths.is_empty() {
        return children;
    }
    let mut out = Vec::new();
    let mut values: Vec<(String, Vec<Bson>)> =
        paths.iter().map(|p| (p.clone(), Vec::new())).collect();
    for c in children {
        match c {
            M::Leaf(p, kind, v) if paths.contains(&p) && matches!(kind, Leaf::Eq | Leaf::In) => {
                let slot = &mut values.iter_mut().find(|(q, _)| *q == p).expect("a path").1;
                match (kind, v) {
                    (Leaf::In, Bson::Array(a)) => slot.extend(a),
                    (_, v) => slot.push(v),
                }
            }
            other => out.push(other),
        }
    }
    for (p, mut vs) in values {
        vs.sort_by(secantus_core::order::cmp);
        vs.dedup();
        // An `$in` of one value is its `$eq`.
        out.push(if vs.len() == 1 {
            M::Leaf(p, Leaf::Eq, vs.pop().expect("one"))
        } else {
            M::Leaf(p, Leaf::In, Bson::Array(vs))
        });
    }
    out
}

fn leaf(path: &str, kind: Leaf, value: impl Into<Bson>) -> M {
    M::Leaf(path.to_string(), kind, value.into())
}

/// A `$regex`, which `parsedQuery` prints as its pattern's string.
fn regex(path: &str, pattern: &str) -> M {
    leaf(path, Leaf::Regex, pattern)
}

fn exists(path: &str) -> M {
    leaf(path, Leaf::Exists, true)
}

fn op_is(op: &str) -> M {
    leaf("op", Leaf::Eq, op)
}

fn not_true(path: &str) -> M {
    M::Not(Box::new(leaf(path, Leaf::Eq, true)))
}

/// mongod's `regexEscapeNsForChangeStream`.
fn escaped(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if "*+|()^?[]./\\$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The database part of a cluster stream's regexes: any database but the
/// internal three.
const ANY_DB: &str = "^(?!(admin|config|local)\\.)[^.]+";

/// The regexes the builders match names by.
struct Names {
    /// A watched namespace.
    ns: String,
    /// A watched collection's name, as a command names it.
    coll: String,
    /// The `$cmd` namespace of a watched database.
    cmd_ns: String,
    /// The `system.views` namespace of a watched database.
    views: Option<String>,
}

impl Names {
    fn of(scope: &Scope, system: bool) -> Names {
        // The collections a database or cluster stream sees: none of the
        // internal ones, or, with `showSystemEvents`, the four it shows.
        let colls = if system {
            "(?!(\\$|system\\.(?!(js$|resharding\\.|buckets\\.|views$))))"
        } else {
            "(?!(\\$|system\\.))"
        };
        match scope {
            Scope::Coll { db, coll } => Names {
                ns: format!("^{}\\.{}$", escaped(db), escaped(coll)),
                coll: format!("^{}$", escaped(coll)),
                cmd_ns: format!("^{}\\.\\$cmd$", escaped(db)),
                views: None,
            },
            Scope::Db(db) => Names {
                ns: format!("^{}\\.{colls}", escaped(db)),
                coll: format!("^{colls}"),
                cmd_ns: format!("^{}\\.\\$cmd$", escaped(db)),
                views: Some(format!("^{}\\.system\\.views$", escaped(db))),
            },
            Scope::Cluster => Names {
                ns: format!("{ANY_DB}\\.{colls}"),
                coll: format!("^{colls}"),
                cmd_ns: format!("{ANY_DB}\\.\\$cmd$"),
                views: Some(format!("{ANY_DB}\\.system\\.views$")),
            },
        }
    }
}

// --------------------------------------------------------- user match --

/// A predicate of the `$match` that follows the stream, as mongod's parser
/// makes it: `$ne` / `$nin` are the `$not` of `$eq` / `$in`, an empty
/// `$in` is `$alwaysFalse`.
enum U {
    And(Vec<U>),
    Or(Vec<U>),
    Nor(Vec<U>),
    Not(Box<U>),
    Eq(String, Bson),
    /// The equalities, and the regexes the list held.
    In(String, Vec<Bson>, Vec<bson::Regex>),
    /// `{$exists: true}`; `false` parses as its `$not`.
    Exists(String),
    /// Any other predicate on a path: its operator and operand.
    Other(String, String, Bson),
    /// `$_internalExprEq`, which the `$match`'s optimizer derived from an
    /// `$expr`'s `$eq` of a field path and a constant (`RewriteExpr`).
    ExprEq(String, Bson),
    /// An `$expr`'s aggregation expression.
    Expr(Bson),
    /// `$where`, `$text`, ... -- nothing on a path.
    Opaque,
    False,
}

impl U {
    fn parse(m: &Document) -> U {
        let mut all = Vec::new();
        for (k, v) in m {
            let list = || -> Vec<U> {
                match v {
                    Bson::Array(a) => a
                        .iter()
                        .filter_map(|c| c.as_document().map(U::parse))
                        .collect(),
                    _ => Vec::new(),
                }
            };
            match k.as_str() {
                "$and" => all.push(U::And(list())),
                "$or" => all.push(U::Or(list())),
                "$nor" => all.push(U::Nor(list())),
                "$alwaysFalse" => all.push(U::False),
                "$expr" => all.push(U::Expr(v.clone())),
                k if k.starts_with('$') => all.push(U::Opaque),
                path => all.push(U::path(path, v)),
            }
        }
        if all.len() == 1 {
            all.pop().expect("one")
        } else {
            U::And(all)
        }
    }

    fn path(path: &str, v: &Bson) -> U {
        let ops = match v {
            Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => d,
            other => return U::Eq(path.to_string(), other.clone()),
        };
        let mut all = Vec::new();
        for (op, arg) in ops {
            let one = |op: &str, arg: &Bson| -> U {
                match op {
                    "$eq" => U::Eq(path.to_string(), arg.clone()),
                    "$_internalExprEq" => U::ExprEq(path.to_string(), arg.clone()),
                    "$in" => match arg {
                        Bson::Array(a) if a.is_empty() => U::False,
                        Bson::Array(a) => U::In(
                            path.to_string(),
                            a.iter()
                                .filter(|b| !matches!(b, Bson::RegularExpression(_)))
                                .cloned()
                                .collect(),
                            a.iter()
                                .filter_map(|b| match b {
                                    Bson::RegularExpression(r) => Some(r.clone()),
                                    _ => None,
                                })
                                .collect(),
                        ),
                        _ => U::Opaque,
                    },
                    "$exists" => U::Exists(path.to_string()),
                    // A `$regex` takes its `$options` with it.
                    "$regex" => match (arg, ops.get("$options")) {
                        (Bson::String(pattern), Some(Bson::String(options))) => U::Other(
                            path.to_string(),
                            op.to_string(),
                            Bson::RegularExpression(bson::Regex {
                                pattern: pattern.clone(),
                                options: options.clone(),
                            }),
                        ),
                        _ => U::Other(path.to_string(), op.to_string(), arg.clone()),
                    },
                    _ => U::Other(path.to_string(), op.to_string(), arg.clone()),
                }
            };
            all.push(match op.as_str() {
                "$ne" => U::Not(Box::new(one("$eq", arg))),
                "$nin" => U::Not(Box::new(one("$in", arg))),
                "$not" => match arg {
                    Bson::Document(d) => {
                        U::Not(Box::new(U::path(path, &Bson::Document(d.clone()))))
                    }
                    other => U::Not(Box::new(U::Other(
                        path.to_string(),
                        "$regex".into(),
                        other.clone(),
                    ))),
                },
                // `{$exists: false}` is the `$not` of `{$exists: true}`.
                "$exists" if !truthy(arg) => U::Not(Box::new(U::Exists(path.to_string()))),
                "$options" => continue,
                op => one(op, arg),
            });
        }
        if all.len() == 1 {
            all.pop().expect("one")
        } else {
            U::And(all)
        }
    }

    /// Whether the `$match`'s own optimizer folds it to `$alwaysFalse`: an
    /// empty `$in`, or an `$and` holding one.
    fn always_false(&self) -> bool {
        match self {
            U::False => true,
            U::And(c) => c.iter().any(U::always_false),
            U::Or(c) => !c.is_empty() && c.iter().all(U::always_false),
            _ => false,
        }
    }
}

fn truthy(b: &Bson) -> bool {
    !matches!(
        b,
        Bson::Boolean(false) | Bson::Null | Bson::Int32(0) | Bson::Int64(0) | Bson::Undefined
    ) && !matches!(b, Bson::Double(d) if *d == 0.0)
}

/// mongod's `kOpTypeRewriteMap`: each event type as the oplog entries that
/// make it.
fn op_type_rewrite(name: &str) -> Option<M> {
    let c = |field: &str| M::And(vec![op_is("c"), exists(field)]);
    Some(match name {
        "insert" => op_is("i"),
        "delete" => op_is("d"),
        "update" => M::And(vec![op_is("u"), M::Not(Box::new(exists("o._id")))]),
        "replace" => M::And(vec![op_is("u"), exists("o._id")]),
        "drop" => c("o.drop"),
        "create" => c("o.create"),
        "createIndexes" => M::And(vec![
            op_is("c"),
            M::Or(vec![
                exists("o.createIndexes"),
                exists("o.commitIndexBuild"),
            ]),
        ]),
        "startIndexBuild" => c("o.startIndexBuild"),
        "abortIndexBuild" => c("o.abortIndexBuild"),
        "dropIndexes" => c("o.dropIndexes"),
        "modify" => c("o.collMod"),
        "rename" => c("o.renameCollection"),
        "dropDatabase" => c("o.dropDatabase"),
        _ => return None,
    })
}

fn is_crud(name: &str) -> bool {
    matches!(name, "insert" | "update" | "replace" | "delete")
}

/// Whether a predicate on a path matches a document without it
/// (mongod's `matchesSingleElement(predicate, {})`).
fn matches_missing(u: &U) -> bool {
    match u {
        U::Eq(_, v) => matches!(v, Bson::Null),
        U::In(_, vs, _) => vs.iter().any(|v| matches!(v, Bson::Null)),
        U::Other(_, op, v) => matches!(op.as_str(), "$gte" | "$lte") && matches!(v, Bson::Null),
        _ => false,
    }
}

/// A predicate on a path, its first part renamed (mongod's
/// `cloneWithSubstitution`); `None` for a predicate this filter cannot
/// print.
fn renamed(u: &U, to: &str) -> Option<M> {
    let path = |p: &str| match p.split_once('.') {
        Some((_, rest)) => format!("{to}.{rest}"),
        None => to.to_string(),
    };
    Some(match u {
        U::Eq(p, v) => M::Leaf(path(p), Leaf::Eq, v.clone()),
        U::In(p, vs, res) if res.is_empty() => M::Leaf(path(p), Leaf::In, Bson::Array(vs.clone())),
        U::Exists(p) => M::Leaf(path(p), Leaf::Exists, Bson::Boolean(true)),
        U::Other(p, op, v) => {
            let kind = match op.as_str() {
                "$lt" => Leaf::Lt,
                "$lte" => Leaf::Lte,
                "$gt" => Leaf::Gt,
                "$gte" => Leaf::Gte,
                _ => return None,
            };
            M::Leaf(path(p), kind, v.clone())
        }
        _ => return None,
    })
}

/// `{$nor: [{op: "i"}, {op: "u"}, {op: "d"}]}`: an entry that is no CRUD
/// event.
fn not_crud() -> M {
    M::Nor(vec![op_is("i"), op_is("u"), op_is("d")])
}

/// mongod's `matchRewriteDocumentKey`: `o2` of an update or an insert, `o`
/// of a delete, and missing from every other event.
fn rewrite_document_key(u: &U) -> Option<M> {
    let mut any = Vec::new();
    if matches_missing(u) {
        any.push(not_crud());
    }
    for (op, to) in [("u", "o2"), ("d", "o"), ("i", "o2")] {
        any.push(M::And(vec![op_is(op), renamed(u, to)?]));
    }
    Some(M::Or(any))
}

/// mongod's `matchRewriteFullDocument`, inexact only: an update's
/// document may be looked up later, so every update passes; an insert's
/// or a replacement's is `o`; a delete and a command have none.
fn rewrite_full_document(u: &U) -> Option<M> {
    let mut any = vec![
        M::And(vec![op_is("u"), M::Not(Box::new(exists("o._id")))]),
        M::And(vec![
            M::Or(vec![op_is("i"), M::And(vec![op_is("u"), exists("o._id")])]),
            renamed(u, "o")?,
        ]),
    ];
    if matches_missing(u) {
        any.push(op_is("d"));
        any.push(not_crud());
    }
    Some(M::Or(any))
}

/// The commands that name a collection in a string field of `o`.
const COLL_COMMANDS: [&str; 8] = [
    "abortIndexBuild",
    "collMod",
    "commitIndexBuild",
    "create",
    "createIndexes",
    "drop",
    "dropIndexes",
    "startIndexBuild",
];

/// A predicate on one oplog field: an `$eq` of the one value, an `$in` of
/// several.
fn on(path: &str, values: &[Bson], many: bool) -> M {
    if many {
        leaf(path, Leaf::In, Bson::Array(values.to_vec()))
    } else {
        leaf(path, Leaf::Eq, values[0].clone())
    }
}

/// As [`on`], of regexes: a `$regex` of one, an `$in` of several.
fn on_regex(path: &str, patterns: &[String], many: bool) -> M {
    if many {
        let rs = patterns
            .iter()
            .map(|p| {
                Bson::RegularExpression(bson::Regex {
                    pattern: p.clone(),
                    options: String::new(),
                })
            })
            .collect();
        leaf(path, Leaf::In, Bson::Array(rs))
    } else {
        regex(path, &patterns[0])
    }
}

/// The commands that name a collection in `o`, in the order mongod's
/// `matchRewriteGenericNamespace` lists them for a regex (an `$expr`
/// sorts by nothing, so the order shows).
const REGEX_COMMANDS: [&str; 8] = [
    "drop",
    "create",
    "createIndexes",
    "commitIndexBuild",
    "startIndexBuild",
    "abortIndexBuild",
    "dropIndexes",
    "collMod",
];

/// A regex predicate's pattern and options: `{$regex}` (with its
/// `$options`) or a bare regex value.
fn regex_of(u: &U) -> Option<(String, String)> {
    match u {
        U::Eq(_, Bson::RegularExpression(r)) => Some((r.pattern.clone(), r.options.clone())),
        U::Other(_, op, v) if op == "$regex" => match v {
            Bson::String(p) => Some((p.clone(), String::new())),
            Bson::RegularExpression(r) => Some((r.pattern.clone(), r.options.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Which part of a namespace string a regex is matched against.
#[derive(Clone, Copy)]
enum Part {
    Whole,
    Db,
    Coll,
}

/// mongod's `$expr` for a regex on a namespace held as a string in
/// `field`: the string (or its part before / after the first dot) by
/// `$regexMatch`, and nothing when the field is no string.
fn ns_regex(field: &str, part: Part, (pattern, options): &(String, String)) -> M {
    let dot = doc! { "$ifNull": [
        { "$indexOfBytes": ["$$oplogField", { "$const": "." }] },
        { "$const": 0_i32 },
    ] };
    let input = match part {
        Part::Whole => Bson::String("$$oplogField".into()),
        Part::Db => Bson::Document(doc! {
            "$substrBytes": ["$$oplogField", { "$const": 0_i32 }, dot]
        }),
        Part::Coll => Bson::Document(doc! {
            "$substrBytes": [
                "$$oplogField",
                { "$add": [{ "$const": 1_i32 }, dot] },
                { "$const": -1_i32 },
            ]
        }),
    };
    let field = format!("${field}");
    M::Expr(doc! { "$let": {
        "vars": { "oplogField": { "$cond": [
            { "$eq": [{ "$type": [field.as_str()] }, { "$const": "string" }] },
            field.as_str(),
            "$$REMOVE",
        ] } },
        "in": { "$regexMatch": {
            "input": input,
            "regex": { "$const": pattern.as_str() },
            "options": { "$const": options.as_str() },
        } },
    } })
}

/// The part of a namespace a path past `ns` / `to` names, if any.
fn part_of(path: &str) -> Option<Part> {
    match path.split_once('.').map(|(_, rest)| rest) {
        Some("db") => Some(Part::Db),
        Some("coll") => Some(Part::Coll),
        _ => None,
    }
}

/// mongod's `matchRewriteGenericNamespace` for a regex on `ns.db` or
/// `ns.coll`: an `$expr` on each command field that names it, and on a
/// write's `ns`. A whole `ns` is an object, which no regex matches.
fn namespace_regex(re: &(String, String), path: &str) -> M {
    let (cmd, crud) = match part_of(path) {
        Some(Part::Coll) => {
            let mut cmd = vec![ns_regex("o.renameCollection", Part::Coll, re)];
            cmd.extend(
                REGEX_COMMANDS
                    .iter()
                    .map(|f| ns_regex(&format!("o.{f}"), Part::Whole, re)),
            );
            (cmd, ns_regex("ns", Part::Coll, re))
        }
        Some(_) => {
            let mut cmd = vec![
                M::And(vec![
                    leaf("o.dropDatabase", Leaf::Eq, 1_i32),
                    ns_regex("ns", Part::Db, re),
                ]),
                ns_regex("o.renameCollection", Part::Db, re),
            ];
            cmd.extend(REGEX_COMMANDS.iter().map(|_| ns_regex("ns", Part::Db, re)));
            (cmd, ns_regex("ns", Part::Db, re))
        }
        None => return M::False,
    };
    M::Or(vec![
        M::And(vec![M::Or(cmd), op_is("c")]),
        M::And(vec![crud, M::Not(Box::new(op_is("c")))]),
    ])
}

/// The equalities an `$in` holds besides its regexes, as mongod parses
/// them: an `$eq` of one, an `$in` of several.
fn equalities(p: &str, vs: &[Bson]) -> U {
    match vs {
        [v] => U::Eq(p.to_string(), v.clone()),
        _ => U::In(p.to_string(), vs.to_vec(), Vec::new()),
    }
}

fn regex_pair(r: &bson::Regex) -> (String, String) {
    (r.pattern.clone(), r.options.clone())
}

/// The command and write branches of a namespace rewrite.
fn branches(m: M) -> Option<(Vec<M>, Vec<M>)> {
    let M::Or(mut top) = m else { return None };
    if top.len() != 2 {
        return None;
    }
    let side = |m: M| match m {
        M::And(mut c) if c.len() == 2 => match c.swap_remove(0) {
            M::Or(v) => Some(v),
            other => Some(vec![other]),
        },
        _ => None,
    };
    let crud = side(top.pop()?)?;
    let cmd = side(top.pop()?)?;
    Some((cmd, crud))
}

/// Appends the lists' first items, then their second, and so on.
fn interleave(out: &mut Vec<M>, mut lists: Vec<std::vec::IntoIter<M>>) {
    loop {
        let before = out.len();
        out.extend(lists.iter_mut().filter_map(Iterator::next));
        if out.len() == before {
            return;
        }
    }
}

/// mongod's `matchRewriteGenericNamespace` for an `$in` on `ns` holding
/// regexes: each command field and a write's `ns` match an equality or a
/// regex, the equalities as [`rewrite_namespace`] has them and each regex
/// as [`namespace_regex`]'s `$expr`.
fn namespace_in_regex(p: &str, vs: &[Bson], res: &[bson::Regex], path: &str) -> Option<M> {
    let (mut cmd, mut crud) = (Vec::new(), Vec::new());
    let mut eq_db_drop = None;
    if !vs.is_empty() {
        let eq = equalities(p, vs);
        match rewrite_namespace(&eq, path) {
            Some(m @ M::Or(_)) => {
                let (mut c, w) = branches(m)?;
                if matches!(part_of(path), Some(Part::Db)) {
                    // `ns.db`: the `dropDatabase` branch takes the regexes too.
                    eq_db_drop = Some(c.remove(0));
                }
                cmd = c;
                crud = w;
            }
            Some(M::False) => {}
            other => return other,
        }
    }
    // Each field takes its regexes together, field by field.
    let mut drop_regexes = Vec::new();
    let (mut cmds, mut cruds) = (Vec::new(), Vec::new());
    for r in res {
        let Some((mut c, w)) = branches(namespace_regex(&regex_pair(r), path)) else {
            continue;
        };
        if matches!(part_of(path), Some(Part::Db)) {
            if let M::And(mut d) = c.remove(0) {
                drop_regexes.push(d.pop().expect("the regex"));
            }
        }
        cmds.push(c.into_iter());
        cruds.push(w.into_iter());
    }
    interleave(&mut cmd, cmds);
    interleave(&mut crud, cruds);
    if matches!(part_of(path), Some(Part::Db)) {
        let mut on_ns = Vec::new();
        if let Some(M::And(mut d)) = eq_db_drop {
            on_ns.push(d.pop().expect("the namespace"));
        }
        on_ns.extend(drop_regexes);
        cmd.insert(
            0,
            M::And(vec![leaf("o.dropDatabase", Leaf::Eq, 1_i32), M::Or(on_ns)]),
        );
    }
    if cmd.is_empty() && crud.is_empty() {
        return Some(M::False);
    }
    Some(M::Or(vec![
        M::And(vec![M::Or(cmd), op_is("c")]),
        M::And(vec![M::Or(crud), M::Not(Box::new(op_is("c")))]),
    ]))
}

/// mongod's `matchRewriteGenericNamespace` for `ns`, `$eq` and `$in`: the
/// commands' `o` fields that name the namespace, and a write's `ns`.
/// A regex is [`namespace_regex`]'s.
fn rewrite_namespace(u: &U, path: &str) -> Option<M> {
    if let Some(re) = regex_of(u) {
        return Some(namespace_regex(&re, path));
    }
    if let U::In(p, vs, res) = u {
        if !res.is_empty() {
            return namespace_in_regex(p, vs, res, path);
        }
    }
    let parts: Vec<&str> = path.split('.').collect();
    let (values, many): (Vec<Bson>, bool) = match u {
        U::Eq(_, v) => (vec![v.clone()], false),
        U::In(_, vs, _) => (vs.clone(), true),
        _ => return None,
    };
    let (mut cmd, mut crud) = (Vec::new(), Vec::new());
    match parts.as_slice() {
        ["ns"] => {
            // A whole namespace: `{db}` or `{db, coll}`, in that order.
            for v in &values {
                let Bson::Document(d) = v else { continue };
                let keys: Vec<&str> = d.keys().map(String::as_str).collect();
                let db = match d.get("db") {
                    Some(Bson::String(s)) => s.clone(),
                    _ => continue,
                };
                let cmd_ns = Bson::String(format!("{db}.$cmd"));
                match (keys.as_slice(), d.get("coll")) {
                    (["db"], _) => cmd.push(M::And(vec![
                        leaf("ns", Leaf::Eq, cmd_ns),
                        leaf("o.dropDatabase", Leaf::Eq, 1),
                    ])),
                    (["db", "coll"], Some(Bson::String(coll))) => {
                        for f in COLL_COMMANDS {
                            cmd.push(M::And(vec![
                                leaf("ns", Leaf::Eq, cmd_ns.clone()),
                                leaf(&format!("o.{f}"), Leaf::Eq, coll.as_str()),
                            ]));
                        }
                        let full = format!("{db}.{coll}");
                        cmd.push(leaf("o.renameCollection", Leaf::Eq, full.as_str()));
                        crud.push(leaf("ns", Leaf::Eq, full));
                    }
                    _ => {}
                }
            }
        }
        ["ns", field @ ("db" | "coll")] => {
            if !values.iter().all(|v| matches!(v, Bson::String(_))) {
                return None;
            }
            let names: Vec<&str> = values.iter().filter_map(Bson::as_str).collect();
            if *field == "coll" {
                let patterns: Vec<String> = names
                    .iter()
                    .map(|c| format!("{ANY_DB}\\.{}$", escaped(c)))
                    .collect();
                for f in COLL_COMMANDS {
                    cmd.push(on(&format!("o.{f}"), &values, many));
                }
                cmd.push(on_regex("o.renameCollection", &patterns, many));
                crud.push(on_regex("ns", &patterns, many));
            } else {
                let cmd_ns: Vec<Bson> = names
                    .iter()
                    .map(|d| Bson::String(format!("{d}.$cmd")))
                    .collect();
                let patterns: Vec<String> = names
                    .iter()
                    .map(|d| format!("^{}\\.(?!(\\$|system\\.))", escaped(d)))
                    .collect();
                cmd.push(M::And(vec![
                    leaf("o.dropDatabase", Leaf::Eq, 1),
                    on("ns", &cmd_ns, many),
                ]));
                for _ in COLL_COMMANDS {
                    cmd.push(on("ns", &cmd_ns, many));
                }
                cmd.push(on_regex("o.renameCollection", &patterns, many));
                crud.push(on_regex("ns", &patterns, many));
            }
        }
        // `ns` holds only `db` and `coll`: anything else is missing.
        _ => {
            return if matches_missing(u) {
                None
            } else {
                Some(M::False)
            }
        }
    }
    Some(M::Or(vec![
        M::And(vec![M::Or(cmd), op_is("c")]),
        M::And(vec![M::Or(crud), M::Not(Box::new(op_is("c")))]),
    ]))
}

/// mongod's `matchRewriteGenericNamespace` for `to`, `$eq` and `$in`: a
/// rename's `o.to`, the one oplog field holding a rename's target. A
/// whole `to` names a collection, so a `{db}` alone matches nothing. A
/// regex on `to.db` / `to.coll` is an `$expr` on `o.to`.
fn rewrite_to(u: &U, path: &str) -> Option<M> {
    if let Some(re) = regex_of(u) {
        return Some(match part_of(path) {
            Some(part) => M::And(vec![ns_regex("o.to", part, &re), op_is("c")]),
            None => M::False,
        });
    }
    if let U::In(p, vs, res) = u {
        if !res.is_empty() {
            // Each equality and each regex, as mongod's `$or` of them.
            let mut on_to = Vec::new();
            if !vs.is_empty() {
                let eq = equalities(p, vs);
                match rewrite_to(&eq, path) {
                    Some(M::And(mut c)) if c.len() == 2 => on_to.push(c.swap_remove(0)),
                    Some(M::False) => {}
                    _ => return None,
                }
            }
            if let Some(part) = part_of(path) {
                on_to.extend(res.iter().map(|r| ns_regex("o.to", part, &regex_pair(r))));
            }
            return Some(if on_to.is_empty() {
                M::False
            } else {
                M::And(vec![M::Or(on_to), op_is("c")])
            });
        }
    }
    let (values, many): (Vec<Bson>, bool) = match u {
        U::Eq(_, v) => (vec![v.clone()], false),
        U::In(_, vs, _) => (vs.clone(), true),
        _ => return None,
    };
    let to = match path.split('.').collect::<Vec<_>>().as_slice() {
        ["to"] => {
            let full: Vec<Bson> = values
                .iter()
                .filter_map(|v| {
                    let Bson::Document(d) = v else { return None };
                    let keys: Vec<&str> = d.keys().map(String::as_str).collect();
                    match (keys.as_slice(), d.get("db"), d.get("coll")) {
                        (["db", "coll"], Some(Bson::String(db)), Some(Bson::String(c))) => {
                            Some(Bson::String(format!("{db}.{c}")))
                        }
                        _ => None,
                    }
                })
                .collect();
            if full.is_empty() {
                return Some(M::False);
            }
            let many = many && full.len() > 1;
            on("o.to", &full, many)
        }
        ["to", field @ ("db" | "coll")] => {
            if !values.iter().all(|v| matches!(v, Bson::String(_))) {
                return None;
            }
            let patterns: Vec<String> = values
                .iter()
                .filter_map(Bson::as_str)
                .map(|n| {
                    if *field == "coll" {
                        format!("{ANY_DB}\\.{}$", escaped(n))
                    } else {
                        format!("^{}\\.(?!(\\$|system\\.))", escaped(n))
                    }
                })
                .collect();
            on_regex("o.to", &patterns, many)
        }
        _ => {
            return if matches_missing(u) {
                None
            } else {
                Some(M::False)
            }
        }
    };
    Some(M::And(vec![to, op_is("c")]))
}

/// mongod's `matchRewriteOperationType`.
fn rewrite_operation_type(u: &U, path: &str) -> Option<M> {
    // A subfield of `operationType` is always missing: the predicate is
    // what it says of a missing field.
    if path != "operationType" {
        return Some(if matches_missing(u) {
            M::True
        } else {
            M::False
        });
    }
    let named = |v: &Bson| match v {
        Bson::String(s) => op_type_rewrite(s).map(|m| (s.clone(), m)),
        _ => None,
    };
    match u {
        U::Eq(_, v) => Some(named(v).map_or(M::False, |(_, m)| m)),
        U::In(_, _, res) if !res.is_empty() => None,
        U::In(_, vs, _) if vs.is_empty() => Some(M::False),
        U::In(_, vs, _) => {
            let mut names: Vec<String> = vs.iter().filter_map(named).map(|(s, _)| s).collect();
            names.sort();
            names.dedup();
            names.sort_by(|a, b| is_crud(b).cmp(&is_crud(a)).then_with(|| a.cmp(b)));
            let fused = names.iter().any(|n| n == "update") && names.iter().any(|n| n == "replace");
            let mut any = Vec::new();
            for n in names {
                match n.as_str() {
                    "replace" if fused => {}
                    "update" if fused => any.push(op_is("u")),
                    n => any.extend(op_type_rewrite(n)),
                }
            }
            Some(M::Or(any))
        }
        _ => None,
    }
}

/// mongod's `rewriteMatchExpressionTree`, for the fields this server
/// rewrites: `operationType`, `documentKey` and `fullDocument`. `None` is
/// no rewrite.
fn rewrite(u: &U, inexact: bool) -> Option<M> {
    match u {
        U::And(c) => {
            let mut out = Vec::new();
            for child in c {
                match rewrite(child, inexact) {
                    Some(m) => out.push(m),
                    None if inexact => {}
                    None => return None,
                }
            }
            Some(M::And(out))
        }
        U::Or(c) => c
            .iter()
            .map(|x| rewrite(x, inexact))
            .collect::<Option<Vec<_>>>()
            .map(M::Or),
        U::Nor(c) => {
            let mut out = Vec::new();
            for child in c {
                match rewrite(child, false) {
                    Some(m) => out.push(m),
                    None if inexact => {}
                    None => return None,
                }
            }
            Some(M::Nor(out))
        }
        U::Not(c) => rewrite(c, false).map(|m| M::Not(Box::new(m))),
        U::Eq(p, _) | U::In(p, ..) | U::Exists(p) | U::Other(p, ..) => match p.split('.').next() {
            Some("operationType") => rewrite_operation_type(u, p),
            Some("documentKey") => rewrite_document_key(u),
            Some("ns") => rewrite_namespace(u, p),
            Some("to") => rewrite_to(u, p),
            Some("fullDocument") if inexact => rewrite_full_document(u),
            _ => None,
        },
        U::ExprEq(p, v) if p == "operationType" => {
            rewrite_operation_type(&U::Eq(p.clone(), v.clone()), p)
        }
        U::ExprEq(..) => None,
        U::Expr(e) => match rewrite_agg(e, inexact)? {
            Bson::Document(d) => Some(M::Expr(d)),
            _ => None,
        },
        U::Opaque | U::False => None,
    }
}

/// A field path of an aggregation expression (`"$a.b"`), not a variable.
fn field_path(b: &Bson) -> Option<&str> {
    match b {
        Bson::String(s) if s.starts_with('$') && !s.starts_with("$$") => Some(&s[1..]),
        _ => None,
    }
}

/// A constant of an aggregation expression: a literal that is not a path,
/// a variable, an operator or an array or document holding one.
fn constant(b: &Bson) -> Option<Bson> {
    match b {
        Bson::String(s) if s.starts_with('$') => None,
        // `$const` is how the optimizer prints a constant, `$literal` how a
        // user writes one.
        Bson::Document(d)
            if d.len() == 1 && (d.contains_key("$const") || d.contains_key("$literal")) =>
        {
            d.values().next().cloned()
        }
        Bson::Document(d) if d.keys().any(|k| k.starts_with('$')) => None,
        Bson::Document(d) => d.values().all(|v| constant(v).is_some()).then(|| b.clone()),
        Bson::Array(a) => a.iter().all(|v| constant(v).is_some()).then(|| b.clone()),
        other => Some(other.clone()),
    }
}

/// `$operationType` as an expression over the oplog entry (mongod's
/// `exprRewriteOperationType`).
fn operation_type_expr() -> Bson {
    let is = |op: &str| doc! { "$eq": ["$op", { "$const": op }] };
    let cmd = |field: &str, name: &str| {
        doc! { "case": { "$ne": [format!("$o.{field}"), "$$REMOVE"] }, "then": { "$const": name } }
    };
    Bson::Document(doc! { "$switch": {
        "branches": [
            { "case": is("i"), "then": { "$const": "insert" } },
            { "case": { "$and": [is("u"), { "$eq": ["$o._id", "$$REMOVE"] }] }, "then": { "$const": "update" } },
            { "case": { "$and": [is("u"), { "$ne": ["$o._id", "$$REMOVE"] }] }, "then": { "$const": "replace" } },
            { "case": is("d"), "then": { "$const": "delete" } },
            { "case": { "$ne": ["$op", { "$const": "c" }] }, "then": "$$REMOVE" },
            cmd("drop", "drop"),
            cmd("create", "create"),
            cmd("dropDatabase", "dropDatabase"),
            cmd("renameCollection", "rename"),
            cmd("createIndexes", "createIndexes"),
            cmd("commitIndexBuild", "createIndexes"),
            cmd("startIndexBuild", "startIndexBuild"),
            cmd("abortIndexBuild", "abortIndexBuild"),
            cmd("dropIndexes", "dropIndexes"),
            cmd("collMod", "modify"),
        ],
        "default": "$$REMOVE",
    } })
}

/// mongod's `rewriteAggExpressionTree`, for the field this server rewrites
/// in an expression: `operationType`. An `$and` drops a child it cannot
/// rewrite when the rewrite may be inexact; anything else needs all of its
/// paths rewritten. Constants print as `$const`, as the optimizer leaves
/// them. `None` is no rewrite.
fn rewrite_agg(e: &Bson, inexact: bool) -> Option<Bson> {
    if let Some(p) = field_path(e) {
        return (p == "operationType").then(operation_type_expr);
    }
    if let Some(c) = constant(e) {
        return Some(Bson::Document(doc! { "$const": c }));
    }
    let d = e.as_document()?;
    if d.len() != 1 {
        return None;
    }
    let (op, args) = d.iter().next()?;
    let args = match args {
        Bson::Array(a) => a.clone(),
        one => vec![one.clone()],
    };
    let out: Vec<Bson> = match op.as_str() {
        "$and" => {
            let kept: Vec<Bson> = args
                .iter()
                .filter_map(|a| rewrite_agg(a, inexact))
                .collect();
            if (kept.len() != args.len() && !inexact) || kept.is_empty() {
                return None;
            }
            kept
        }
        "$or" | "$not" | "$eq" | "$ne" | "$gt" | "$gte" | "$lt" | "$lte" | "$in" => args
            .iter()
            .map(|a| rewrite_agg(a, false))
            .collect::<Option<_>>()?,
        _ => return None,
    };
    Some(Bson::Document(doc! { op.clone(): out }))
}

/// The rewrite of the `$match` after the stream into the oplog's terms, if
/// it has one; mongod skips a `$match` its optimizer folded to a constant.
fn rewritten(user: Option<&Document>) -> Option<M> {
    let u = U::parse(user?);
    if u.always_false() {
        return None;
    }
    rewrite(&u, true)
}

/// An oplog entry that writes a document: not a no-op, not a command.
fn writes(ns_pattern: &str) -> M {
    M::And(vec![
        regex("ns", ns_pattern),
        M::Nor(vec![op_is("n"), op_is("c")]),
    ])
}

/// The commands on a watched namespace that make an event.
fn commands(scope: &Scope, n: &Names, spec: &Spec) -> M {
    let mut names = vec![
        "collMod",
        "commitIndexBuild",
        "create",
        "createIndexes",
        "drop",
        "dropIndexes",
    ];
    if spec.system {
        names.extend(["abortIndexBuild", "startIndexBuild"]);
    }
    let mut any = vec![M::And(vec![
        regex("o.to", &n.ns),
        exists("o.renameCollection"),
    ])];
    any.extend(names.iter().map(|c| regex(&format!("o.{c}"), &n.coll)));
    any.push(regex("o.renameCollection", &n.ns));
    if !matches!(scope, Scope::Coll { .. }) {
        any.push(exists("o.dropDatabase"));
    }
    M::And(vec![M::Or(any), op_is("c"), regex("ns", &n.cmd_ns)])
}

/// mongod's `buildOperationFilter`: the entries that are an event of the
/// stream, its writes and commands, and of those the ones the `$match`
/// after the stream -- the event types, without `showExpandedEvents` --
/// rewritten can accept.
fn events(scope: &Scope, n: &Names, spec: &Spec, user: Option<&M>) -> M {
    let entries = M::Or(vec![commands(scope, n, spec), writes(&n.ns)]);
    match user {
        Some(m) => M::And(vec![entries, m.clone()]),
        None => entries,
    }
}

/// The commands that end the stream: a drop or rename of the collection,
/// a drop of the database.
fn invalidations(scope: &Scope) -> Option<M> {
    match scope {
        Scope::Coll { db, coll } => {
            let ns = format!("{db}.{coll}");
            Some(M::And(vec![
                M::Or(vec![
                    M::And(vec![
                        leaf("o.to", Leaf::Eq, ns.as_str()),
                        exists("o.renameCollection"),
                    ]),
                    leaf("o.drop", Leaf::Eq, *coll),
                    leaf("o.renameCollection", Leaf::Eq, ns.as_str()),
                ]),
                leaf("ns", Leaf::Eq, format!("{db}.$cmd")),
                op_is("c"),
            ]))
        }
        Scope::Db(db) => Some(M::And(vec![
            leaf("ns", Leaf::Eq, format!("{db}.$cmd")),
            op_is("c"),
            exists("o.dropDatabase"),
        ])),
        Scope::Cluster => None,
    }
}

/// The `applyOps` of a committed transaction that touches the namespace.
fn transactions(n: &Names) -> M {
    let created = M::And(vec![
        M::Or(vec![
            regex("o.create", &n.coll),
            regex("o.createIndexes", &n.coll),
        ]),
        regex("ns", &n.cmd_ns),
    ]);
    M::And(vec![
        M::Or(vec![
            M::ElemObject("o.applyOps".into(), Box::new(created)),
            regex("o.applyOps.ns", &n.ns),
            M::Not(Box::new(leaf(
                "prevOpTime",
                Leaf::Eq,
                doc! { "ts": Timestamp { time: 0, increment: 0 }, "t": -1_i64 },
            ))),
        ]),
        op_is("c"),
        not_true("o.partialTxn"),
        not_true("o.prepare"),
        leaf("o.applyOps", Leaf::Type, Bson::Array(vec![Bson::Int32(4)])),
    ])
}

/// The sharding no-ops on the namespace.
fn sharding(n: &Names, spec: &Spec) -> M {
    let mut fields = vec![
        "refineCollectionShardKey",
        "reshardBegin",
        "reshardCollection",
        "reshardDoneCatchUp",
        "shardCollection",
    ];
    if spec.system {
        fields.extend(["migrateLastChunkFromShard", "reshardBlockingWrites"]);
    }
    M::And(vec![
        M::Or(fields.iter().map(|f| exists(&format!("o2.{f}"))).collect()),
        op_is("n"),
        regex("ns", &n.ns),
    ])
}

/// What a chunk migration wrote is left out unless the stream asks for it;
/// with `showSystemEvents`, the creates it made are kept.
fn not_from_migration(n: &Names, spec: &Spec) -> Option<M> {
    if spec.migration {
        return None;
    }
    if !spec.system {
        return Some(not_true("fromMigrate"));
    }
    Some(M::Or(vec![
        M::And(vec![
            M::Or(vec![
                regex("o.create", &n.coll),
                regex("o.createIndexes", &n.coll),
            ]),
            leaf("fromMigrate", Leaf::Eq, true),
            regex("ns", &n.cmd_ns),
        ]),
        not_true("fromMigrate"),
    ]))
}

/// The oplog filter, and the filter `internalUnwindTransaction` applies to
/// the entries a transaction holds.
fn filters(
    scope: &Scope,
    spec: &Spec,
    start: Timestamp,
    user: Option<&Document>,
) -> (Document, Document) {
    let n = Names::of(scope, spec.system);
    let user = rewritten(user);
    let user = user.as_ref();
    let mut any = Vec::new();
    if spec.expanded {
        if let Some(views) = &n.views {
            any.push(writes(views));
        }
    }
    any.push(events(scope, &n, spec, user));
    any.extend(invalidations(scope));
    any.push(transactions(&n));
    any.push(sharding(&n, spec));
    any.push(M::And(vec![
        leaf("o.commitTransaction", Leaf::Eq, 1_i32),
        op_is("c"),
    ]));
    any.push(M::And(vec![
        op_is("n"),
        regex("o2.endOfTransaction", &n.ns),
    ]));
    let mut oplog = vec![M::Or(any), leaf("ts", Leaf::Gte, start)];
    oplog.extend(not_from_migration(&n, spec));
    let mut unwind = vec![events(scope, &n, spec, user)];
    unwind.extend(not_from_migration(&n, spec));
    // The unwind's filter goes through mongod's Boolean simplifier
    // (`internalUnwindTransaction` optimises it as a query is; the oplog
    // filter is read as it is): after `operationType: "drop"` the writes
    // branch and the event-type `$or` fall away, as only a command can
    // match (RM-699).
    let unwind = M::And(unwind).normalized().to_doc();
    let unwind = match super::simplify::simplify(&unwind) {
        Some(simpler) => super::parsed::sort_tree(simpler),
        None => unwind,
    };
    (M::And(oplog).normalized().to_doc(), unwind)
}

// ------------------------------------------------------------- stages --

/// The event types a stream without `showExpandedEvents` returns.
const EVENT_TYPES: [&str; 12] = [
    "delete",
    "drop",
    "dropDatabase",
    "insert",
    "invalidate",
    "migrateChunkToNewShard",
    "rename",
    "replace",
    "reshardBegin",
    "reshardBlockingWrites",
    "reshardDoneCatchUp",
    "update",
];

/// The oplog fields the read keeps.
fn projection(spec: &Spec) -> Document {
    let mut fields = Vec::new();
    if spec.reads_images() {
        fields.extend(["applyOpsIndex", "applyOpsTs"]);
    }
    fields.extend([
        "commitTimestamp",
        "lsid",
        "multiOpType",
        "ns",
        "o",
        "o2",
        "op",
        "prevOpTime",
        "t",
        "tid",
        "ts",
        "txnNumber",
        "txnOpIndex",
        "ui",
        "wall",
    ]);
    let mut out: Document = fields
        .into_iter()
        .map(|f| (f.to_string(), Bson::Int32(1)))
        .collect();
    out.insert("_id", 0);
    out
}

/// `internalTransform`'s options: the start, the images, then the flags
/// the stream was given, in the order of mongod's spec.
fn transform_options(spec: &Spec, start: Timestamp) -> Document {
    let mut out = Document::new();
    match spec.resume() {
        Some((k, token)) => {
            out.insert(k, token.clone());
        }
        None => {
            out.insert("startAtOperationTime", start);
        }
    }
    out.insert("fullDocument", spec.full_document);
    out.insert("fullDocumentBeforeChange", spec.before_change);
    for k in [
        "allChangesForCluster",
        "showMigrationEvents",
        "showSystemEvents",
        "allowToRunOnConfigDB",
        "allowToRunOnSystemNS",
        "showExpandedEvents",
        "showRawUpdateDescription",
        "matchCollectionUUIDForUpdateLookup",
        "version",
    ] {
        if let Some(v) = spec.raw.get(k) {
            out.insert(k, v.clone());
        }
    }
    out
}

/// The time a resume token resumes from: its `clusterTime`, the first
/// element of its KeyString.
fn token_time(token: &Document) -> Option<Timestamp> {
    crate::resume_keystring::cluster_time(token)
}

/// The stages after the read, as mongod lists them: the user's `$match`es
/// move ahead of the image stages (and the user's own reshaping stages)
/// when they read nothing those write, and join the `$match` on the event
/// types.
fn stages_after_read(spec: &Spec, db: &str, user: &[Document], token: &Document) -> Vec<Document> {
    // The image stages, as the fields they write.
    let image = |field: &str| doc! { "$addFields": { field: { "$literal": "\u{0}image" } } };
    let pre = spec.before_change != "off";
    let post = spec.full_document != "default";
    let mut run: Vec<Bson> = Vec::new();
    if pre {
        run.push(Bson::Document(image("fullDocumentBeforeChange")));
    }
    if post {
        run.push(Bson::Document(image("fullDocument")));
    }
    let staged: Vec<Bson> = user.iter().cloned().map(Bson::Document).collect();
    run.extend(shape::desugared(&staged));
    let run: Vec<Document> = hoist::hoist_matches(&run)
        .into_iter()
        .filter_map(|b| match b {
            Bson::Document(d) => Some(d),
            _ => None,
        })
        .collect();
    // The user's `$match` that leads, as any `$match` after a read is listed.
    let listed_match = |stage: &Document| {
        shape::listed_stage(stage, db)
            .into_iter()
            .next()
            .and_then(|s| s.get_document("$match").ok().cloned())
    };
    // A `$match` moved ahead of a `$replaceRoot` joins the event types too
    // (measured 8.2.11, RM-536).
    let leads = |s: &&Document| s.contains_key("$match") || s.contains_key(hoist::PUSHED_MATCH);
    let (front, rest) = match run.first().filter(leads) {
        Some(stage) => (listed_match(stage), &run[1..]),
        None => (None, &run[..]),
    };
    let mut out = Vec::new();
    let types = doc! { "operationType": { "$in": EVENT_TYPES.to_vec() } };
    match (spec.expanded, front) {
        (true, Some(m)) => out.push(doc! { "$match": m }),
        (true, None) => {}
        (false, None) => out.push(doc! { "$match": types }),
        (false, Some(m)) => {
            let mut all = vec![Bson::Document(types)];
            match m.get_array("$and") {
                Ok(children) if m.len() == 1 => all.extend(children.iter().cloned()),
                _ => all.push(Bson::Document(m)),
            }
            // An `$and` holding `$alwaysFalse` is `$alwaysFalse`.
            let never = doc! { "$alwaysFalse": 1 };
            if all
                .iter()
                .any(|b| b.as_document().is_some_and(|d| *d == never))
            {
                out.push(doc! { "$match": never });
            } else {
                out.push(doc! { "$match": { "$and": all } });
            }
        }
    }
    for stage in rest {
        if *stage == image("fullDocumentBeforeChange") {
            out.push(doc! { "$changeStream": {
                "stage": "internalAddPreImage",
                "fullDocumentBeforeChange": spec.before_change,
            } });
        } else if *stage == image("fullDocument") {
            out.push(doc! { "$changeStream": {
                "stage": "$_internalChangeStreamAddPostImage",
                "fullDocument": spec.full_document,
            } });
        } else {
            out.extend(shape::listed_stage(stage, db));
        }
    }
    if spec.resume().is_some() {
        out.push(doc! { "$changeStream": {
            "stage": "$_internalChangeStreamEnsureResumeTokenPresent",
            "resumeToken": token.clone(),
        } });
    }
    out
}

/// What the explain's run of a change stream found (RM-453).
struct Run {
    /// Oplog entries at or after the start time: the scan's `docsExamined`.
    examined: usize,
    /// Of those, the entries the stream's filter keeps.
    matched: usize,
    /// The events the stream makes of them, invalidations included.
    events: Vec<Document>,
    invalidates: usize,
}

/// Run the read mongod runs for `explain` of a change stream: the oplog
/// from `start` through the filter, and the events of the stream from the
/// seq `after`, as the stream itself polls them.
fn run_read(
    storage: &dyn crate::storage::Storage,
    scope: &Scope,
    raw: &Document,
    pipeline: &[Document],
    filter: &Document,
    start: Timestamp,
    after: i64,
) -> Result<Run, crate::CommandError> {
    let fault = |e: crate::storage::StorageError| {
        crate::CommandError::new(1, "InternalError", format!("{e:?}"))
    };
    let from = doc! { "ts": { "$gte": start } };
    let examined = storage
        .find("local", "oplog.rs", &from, None, None)
        .map_err(fault)?
        .len();
    let read = doc! { "$and": [from, filter.clone()] };
    let matched = storage
        .find("local", "oplog.rs", &read, None, None)
        .map_err(fault)?
        .len();
    let cs_scope = match scope {
        Scope::Coll { db, coll } => crate::storage::ChangeStreamScope::Coll {
            db: (*db).to_string(),
            coll: (*coll).to_string(),
        },
        Scope::Db(db) => crate::storage::ChangeStreamScope::Db((*db).to_string()),
        Scope::Cluster => crate::storage::ChangeStreamScope::Cluster,
    };
    let opts = crate::storage::ChangeStreamOptions {
        full_document: raw.get_str("fullDocument").unwrap_or("default").to_string(),
        full_document_before_change: raw
            .get_str("fullDocumentBeforeChange")
            .unwrap_or("off")
            .to_string(),
        show_expanded_events: raw.get_bool("showExpandedEvents").unwrap_or(false),
        split_large_events: pipeline
            .iter()
            .any(|s| s.contains_key("$changeStreamSplitLargeEvent")),
        start_ts: raw.get_timestamp("startAtOperationTime").ok(),
        resume_skip: None,
    };
    let tail = storage.oplog_tail_seq();
    let mut position = after;
    let mut events = Vec::new();
    let mut invalidates = 0;
    while position < tail {
        let batch = storage
            .change_stream_poll(&cs_scope, &opts, position, 1000)
            .map_err(fault)?;
        for bytes in &batch.events {
            let d = secantus_core::bsondoc::from_bytes(bytes)
                .map_err(|e| crate::CommandError::new(1, "InternalError", e.to_string()))?;
            if d.get_str("operationType") == Ok("invalidate") {
                invalidates += 1;
            }
            events.push(d);
        }
        if batch.invalidated || batch.fatal.is_some() || batch.new_position <= position {
            break;
        }
        position = batch.new_position;
    }
    Ok(Run {
        examined,
        matched,
        events,
        invalidates,
    })
}

/// The events a listed stage passes on: an internal `$changeStream` stage
/// passes every event; any other stage is run over them as listed.
fn pass(events: Vec<Document>, stage: &Document) -> Vec<Document> {
    if stage.contains_key("$changeStream") {
        return events;
    }
    let mut run = stage.clone();
    run.remove("nReturned");
    run.remove("executionTimeMillisEstimate");
    match secantus_core::aggregate::apply_pipeline(
        events.clone(),
        &[Bson::Document(run)],
        &secantus_core::vars::Vars::new(),
        None,
    ) {
        Ok(out) => out,
        Err(_) => events,
    }
}

/// A plan stage's counters, as a tailable read of the oplog that found
/// nothing leaves them.
fn idle_counters(out: &mut Document) {
    for (k, v) in [
        ("nReturned", 0),
        ("executionTimeMillisEstimate", 0),
        ("works", 1),
        ("advanced", 0),
        ("needTime", 0),
        ("needYield", 0),
        ("saveState", 2),
        ("restoreState", 1),
        ("isEOF", 0),
    ] {
        out.insert(k, Bson::Int32(v));
    }
}

/// The `explain` of an aggregate whose first stage is `$changeStream`.
pub(super) fn reply(
    inner: &Document,
    pipeline: &[Document],
    echo: Document,
    verbosity: Verbosity,
    ctx: &mut CommandContext,
) -> HandlerResult {
    let db = ctx.db_name.clone();
    let empty = Document::new();
    let raw = pipeline
        .first()
        .and_then(|s| s.get_document("$changeStream").ok())
        .unwrap_or(&empty);
    let spec = Spec::of(raw);
    let scope = match inner.get("aggregate") {
        Some(Bson::String(coll)) => Scope::Coll {
            db: &db,
            coll: coll.as_str(),
        },
        _ if raw.get_bool("allChangesForCluster").unwrap_or(false) => Scope::Cluster,
        _ => Scope::Db(&db),
    };
    let storage = ctx.storage()?;
    // Where the stream starts, and the token it starts from: a resume
    // token's own time; a start time; else one past the cluster time, as a
    // new stream on mongod starts.
    let at_seq = |seq: i64| {
        let bytes = storage.high_water_mark_token(seq);
        secantus_core::bsondoc::from_bytes(&bytes).unwrap_or_default()
    };
    let (start, token, from_seq) = match (spec.resume(), raw.get_timestamp("startAtOperationTime"))
    {
        (Some((_, token)), _) => {
            let from_seq = match storage.resume_token_info(token) {
                Some(info) if info.event && !info.from_invalidate => info.position - 1,
                Some(info) => info.position,
                None => storage.oplog_tail_seq(),
            };
            (
                token_time(token).unwrap_or(Timestamp {
                    time: 0,
                    increment: 0,
                }),
                token.clone(),
                from_seq,
            )
        }
        (None, Ok(ts)) => {
            let at = crate::storage::ts_position(ts);
            (ts, at_seq(at), (at - 1).min(storage.oplog_tail_seq()))
        }
        (None, Err(_)) => {
            let now = storage.peek_cluster_time();
            let tail = storage.oplog_tail_seq();
            (
                Timestamp {
                    time: now.time,
                    increment: now.increment + 1,
                },
                at_seq(tail + 1),
                tail,
            )
        }
    };
    let after = stages_after_read(&spec, &db, &pipeline[1..], &token);
    // The `$match` that leads the stages after the read is the one mongod
    // rewrites into the oplog filter.
    let user = after.first().and_then(|s| s.get_document("$match").ok());
    let (filter, unwind) = filters(&scope, &spec, start, user);
    let transform_by = projection(&spec);
    let no_indexes = |_: &Document| false;
    let key = cache_key::key_for(
        &filter,
        &cache_key::Parts {
            sort: None,
            projection: Some(&transform_by),
            collation: None,
            distinct: None,
            sbe: false,
            subplanner: false,
        },
        &cache_key::Indexability {
            catalog: &[],
            collation: None,
            implies: &no_indexes,
        },
    );
    let min_record = ((u64::from(start.time) << 32) | u64::from(start.increment)) as i64;
    let scan = doc! {
        "stage": "COLLSCAN",
        "filter": filter.clone(),
        "direction": "forward",
        "minRecord": min_record,
    };
    let mut planner = doc! {
        "namespace": "local.oplog.rs",
        "parsedQuery": filter.clone(),
        "indexFilterSet": false,
    };
    if let Some(key) = &key {
        let shape = cache_key::hex(key.shape_hash);
        planner.insert("queryHash", shape.clone());
        planner.insert("planCacheShapeHash", shape);
        planner.insert("planCacheKey", cache_key::hex(key.key_hash));
    }
    planner.insert("optimizationTimeMillis", 0_i32);
    for k in [
        "maxIndexedOrSolutionsReached",
        "maxIndexedAndSolutionsReached",
        "maxScansToExplodeReached",
        "prunedSimilarIndexes",
    ] {
        planner.insert(k, false);
    }
    planner.insert(
        "winningPlan",
        doc! {
            "isCached": false,
            "stage": "PROJECTION_SIMPLE",
            "transformBy": transform_by.clone(),
            "inputStage": scan.clone(),
        },
    );
    planner.insert("rejectedPlans", Bson::Array(vec![]));

    let mut cursor = doc! { "queryPlanner": planner };
    let wanted = verbosity != Verbosity::QueryPlanner;
    // Under `executionStats` mongod runs the read: the scan examines every
    // oplog entry from the start time, the filter keeps the entries of the
    // stream, and each later stage counts what it passes on (RM-453).
    let counts = if wanted {
        Some(run_read(
            storage, &scope, raw, pipeline, &filter, start, from_seq,
        )?)
    } else {
        None
    };
    if let Some(run) = &counts {
        let examined = run.examined as i32;
        let matched = run.matched as i32;
        let mut collscan = doc! { "stage": "COLLSCAN", "filter": scan.get("filter").cloned() };
        idle_counters(&mut collscan);
        let busy = |d: &mut Document| {
            d.insert("nReturned", matched);
            d.insert("works", examined + 1);
            d.insert("advanced", matched);
            d.insert("needTime", examined - matched);
        };
        busy(&mut collscan);
        collscan.insert("direction", "forward");
        collscan.insert("minRecord", min_record);
        collscan.insert("docsExamined", examined);
        let mut projected = doc! { "isCached": false, "stage": "PROJECTION_SIMPLE" };
        idle_counters(&mut projected);
        busy(&mut projected);
        projected.insert("transformBy", transform_by);
        projected.insert("inputStage", collscan);
        let mut stats = doc! {
            "executionSuccess": true,
            "nReturned": matched,
            "executionTimeMillis": 0_i32,
            "totalKeysExamined": 0_i32,
            "totalDocsExamined": examined,
            "executionStages": projected,
        };
        if verbosity == Verbosity::AllPlans {
            stats.insert("allPlansExecution", Bson::Array(vec![]));
        }
        cursor.insert("executionStats", stats);
    }
    let mut stages = vec![doc! { "$cursor": cursor }];
    stages
        .push(doc! { "$changeStream": { "stage": "internalUnwindTransaction", "filter": unwind } });
    stages.push(doc! { "$changeStream": {
        "stage": "internalTransform",
        "options": transform_options(&spec, start),
    } });
    stages.push(doc! {
        "$changeStream": { "stage": "$_internalChangeStreamCheckInvalidate" },
        "$_internalChangeStreamCheckInvalidate": {},
    });
    stages.push(doc! { "$changeStream": {
        "stage": "$_internalChangeStreamCheckResumability",
        "resumeToken": token.clone(),
    } });
    stages.extend(after);
    if let Some(run) = &counts {
        let mut events = run.events.clone();
        for (i, stage) in stages.iter_mut().enumerate() {
            let n = match i {
                0 => run.matched,
                // Before `$_internalChangeStreamCheckInvalidate` adds it,
                // the stream has no invalidate event.
                1 | 2 => run.events.len() - run.invalidates,
                _ => {
                    events = pass(events, stage);
                    events.len()
                }
            };
            stage.insert("nReturned", n as i64);
            stage.insert("executionTimeMillisEstimate", 0_i64);
        }
    }
    let mut reply = doc! {
        "explainVersion": "1",
        "stages": stages.into_iter().map(Bson::Document).collect::<Vec<_>>(),
    };
    reply.insert("serverInfo", server_info(ctx));
    reply.insert("serverParameters", server_parameters(false));
    reply.insert("command", echo);
    reply.insert("ok", 1.0);
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes(scope: &Scope, raw: Document) -> (String, String) {
        hashes_after(scope, raw, None)
    }

    /// The hashes of a stream whose `$match` after it is `user`, listed as
    /// mongod lists it: joined to the event types unless
    /// `showExpandedEvents`.
    fn hashes_after(scope: &Scope, raw: Document, user: Option<Document>) -> (String, String) {
        let spec = Spec::of(&raw);
        let types = doc! { "operationType": { "$in": EVENT_TYPES.to_vec() } };
        let user = match (spec.expanded, user) {
            (true, u) => u,
            (false, None) => Some(types),
            (false, Some(u)) => Some(doc! { "$and": [types, u] }),
        };
        let (filter, _) = filters(
            scope,
            &spec,
            Timestamp {
                time: 1,
                increment: 1,
            },
            user.as_ref(),
        );
        let no = |_: &Document| false;
        let key = cache_key::key_for(
            &filter,
            &cache_key::Parts {
                sort: None,
                projection: Some(&projection(&spec)),
                collation: None,
                distinct: None,
                sbe: false,
                subplanner: false,
            },
            &cache_key::Indexability {
                catalog: &[],
                collation: None,
                implies: &no,
            },
        )
        .expect("a key");
        (cache_key::hex(key.shape_hash), cache_key::hex(key.key_hash))
    }

    /// mongod 8.2.11's `queryHash` and `planCacheKey` for each scope and
    /// each option that changes the oplog read (2026-11-09): the shape of
    /// the filter -- every node's type and path, in order -- and the
    /// projection.
    #[test]
    fn the_read_has_mongods_shape() {
        let coll = Scope::Coll { db: "d", coll: "c" };
        let db = Scope::Db("d");
        let cluster = Scope::Cluster;
        let cases: Vec<(&Scope, Document, &str, &str)> = vec![
            (&coll, doc! {}, "90A52575", "AA84D8CB"),
            (
                &coll,
                doc! { "showExpandedEvents": true },
                "95183B55",
                "1BCBD4D3",
            ),
            (
                &coll,
                doc! { "showSystemEvents": true },
                "73ABD2DF",
                "3DE5F101",
            ),
            (
                &coll,
                doc! { "showExpandedEvents": true, "showSystemEvents": true },
                "A76A9D7A",
                "D48AF3C4",
            ),
            (
                &coll,
                doc! { "fullDocumentBeforeChange": "whenAvailable" },
                "EA69A4EE",
                "6CF717EB",
            ),
            (
                &coll,
                doc! { "fullDocument": "whenAvailable" },
                "EA69A4EE",
                "6CF717EB",
            ),
            (
                &coll,
                doc! { "fullDocument": "updateLookup" },
                "90A52575",
                "AA84D8CB",
            ),
            (&coll, doc! { "showMigrationEvents": true }, "713A9926", ""),
            (&db, doc! {}, "35E72845", "723EC73B"),
            (
                &db,
                doc! { "showExpandedEvents": true },
                "C42C93B8",
                "928D00A0",
            ),
            (
                &db,
                doc! { "showSystemEvents": true },
                "E2CFA786",
                "92745AA7",
            ),
            (
                &db,
                doc! { "showExpandedEvents": true, "showSystemEvents": true },
                "EC479045",
                "51C3E84E",
            ),
            (&cluster, doc! {}, "E534C309", "FB87B203"),
            (
                &cluster,
                doc! { "showExpandedEvents": true },
                "1F64D9F9",
                "ACF8FBD9",
            ),
            (
                &cluster,
                doc! { "showSystemEvents": true },
                "93F25ABD",
                "377061D2",
            ),
            (
                &cluster,
                doc! { "showExpandedEvents": true, "showSystemEvents": true },
                "BC51E706",
                "B9D9E60B",
            ),
        ];
        for (scope, raw, query_hash, key_hash) in cases {
            let (q, k) = hashes(scope, raw.clone());
            assert_eq!(q, query_hash, "queryHash of {raw}");
            if !key_hash.is_empty() {
                assert_eq!(k, key_hash, "planCacheKey of {raw}");
            }
        }
    }

    /// mongod 8.2.11's `queryHash` of a collection stream followed by a
    /// `$match` on `operationType` (2026-11-15): the rewrite that joins the
    /// oplog filter (RM-452).
    #[test]
    fn a_match_on_the_operation_type_joins_the_read() {
        let coll = Scope::Coll { db: "d", coll: "c" };
        let cases: Vec<(Document, &str)> = vec![
            (doc! { "operationType": "insert" }, "2CFDBDA8"),
            (doc! { "operationType": "delete" }, "2CFDBDA8"),
            (doc! { "operationType": "update" }, "C2547ADC"),
            (doc! { "operationType": "replace" }, "D472C25A"),
            (doc! { "operationType": "drop" }, "C1F2BAA7"),
            (doc! { "operationType": "rename" }, "0F49F6B5"),
            (doc! { "operationType": "dropDatabase" }, "8669925F"),
            (doc! { "operationType": "create" }, "D47AF345"),
            (doc! { "operationType": "modify" }, "0A8E56F6"),
            (doc! { "operationType": "invalidate" }, "33ABFEAA"),
            (doc! { "operationType": "bogus" }, "33ABFEAA"),
            (doc! { "operationType": 1 }, "33ABFEAA"),
            (doc! { "operationType": Bson::Null }, "33ABFEAA"),
            (doc! { "operationType.a": 1 }, "33ABFEAA"),
            (doc! { "operationType.a": { "$exists": false } }, "90A52575"),
            (
                doc! { "operationType": { "$in": ["insert", "update"] } },
                "96297809",
            ),
            (
                doc! { "operationType": { "$in": ["insert", "delete"] } },
                "1D036745",
            ),
            (
                doc! { "operationType": { "$in": ["update", "replace"] } },
                "2CFDBDA8",
            ),
            (
                doc! { "operationType": { "$in": ["insert", 3] } },
                "2CFDBDA8",
            ),
            (
                doc! { "operationType": { "$in": ["drop", "createIndexes", "insert"] } },
                "DE50B65C",
            ),
            (
                doc! { "operationType": { "$in": ["createIndexes"] } },
                "6E0AA942",
            ),
            (doc! { "operationType": { "$in": [] } }, "95183B55"),
            (doc! { "operationType": { "$ne": "insert" } }, "FFF0FC73"),
            (
                doc! { "operationType": { "$nin": ["bogus", "insert"] } },
                "FFF0FC73",
            ),
            (
                doc! { "operationType": { "$not": { "$eq": "insert" } } },
                "FFF0FC73",
            ),
            (doc! { "operationType": { "$ne": "bogus" } }, "90A52575"),
            (doc! { "operationType": { "$ne": "update" } }, "D6530318"),
            (
                doc! { "operationType": { "$nin": ["update", "drop"] } },
                "E1359AA6",
            ),
            (
                doc! { "$nor": [{ "operationType": "update" }, { "operationType": "drop" }] },
                "7203AC7F",
            ),
            (
                doc! { "$nor": [{ "operationType": "insert" }, { "x": 1 }] },
                "FFF0FC73",
            ),
            (
                doc! { "$or": [{ "operationType": "insert" }, { "operationType": "delete" }] },
                "1D036745",
            ),
            (
                doc! { "$or": [{ "operationType": "insert" }, { "x": 1 }] },
                "90A52575",
            ),
            (
                doc! { "$and": [{ "operationType": "insert" }, { "operationType": "delete" }] },
                "E37C3474",
            ),
            (doc! { "operationType": "insert", "x": 1 }, "2CFDBDA8"),
            (doc! { "operationType": { "$exists": true } }, "90A52575"),
            (doc! { "operationType": { "$gt": "a" } }, "90A52575"),
            (doc! { "x": 1 }, "90A52575"),
            // RM-698: `documentKey` and, inexactly, `fullDocument`.
            (doc! { "fullDocument.a": 1 }, "193DCAB5"),
            (doc! { "documentKey._id": 1 }, "97E310BC"),
            // RM-698: a regex on the namespace, an `$expr` per field.
            (doc! { "ns.coll": { "$regex": "^c" } }, "EE5C1E5A"),
            (
                doc! { "ns.db": { "$regex": "^r", "$options": "i" } },
                "B38D21DB",
            ),
            (
                doc! { "ns.coll": { "$not": bson::Regex { pattern: "^c".into(), options: String::new() } } },
                "5CA25166",
            ),
            (doc! { "ns": { "$regex": "^c" } }, "33ABFEAA"),
            (doc! { "to.coll": { "$regex": "^c" } }, "6F8558DD"),
            // RM-698: an `$in` holding regexes, measured on mongod 8.2.11.
            (
                doc! { "ns.coll": { "$in": ["x", bson::Regex { pattern: "^c".into(), options: "iu".into() }] } },
                "7426DAC8",
            ),
            (
                doc! { "ns.db": { "$in": ["x", bson::Regex { pattern: "^r".into(), options: "u".into() }] } },
                "5E53332A",
            ),
            (
                doc! { "to.coll": { "$in": ["x", bson::Regex { pattern: "^c".into(), options: "u".into() }] } },
                "5BF65C39",
            ),
        ];
        for (user, want) in cases {
            let (q, _) = hashes_after(&coll, doc! {}, Some(user.clone()));
            assert_eq!(q, want, "queryHash after {user}");
        }
    }

    #[test]
    fn names_are_escaped_as_mongod_escapes_them() {
        let n = Names::of(
            &Scope::Coll {
                db: "a.b",
                coll: "c$d",
            },
            false,
        );
        assert_eq!(n.ns, "^a\\.b\\.c\\$d$");
        assert_eq!(n.coll, "^c\\$d$");
        assert_eq!(n.cmd_ns, "^a\\.b\\.\\$cmd$");
    }

    #[test]
    fn a_token_of_this_server_gives_its_time() {
        // A high-water mark at (7, 2): the time, then the version, the type,
        // `txnOpIndex`, `fromInvalidate` and a null uuid.
        assert_eq!(
            token_time(&doc! { "_data": "8200000007000000022B0429296E1404" }),
            Some(Timestamp {
                time: 7,
                increment: 2
            })
        );
    }
}
