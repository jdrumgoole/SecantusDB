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
//! Not reproduced: the rewrites of the other event fields and of `$expr`
//! (RM-698), the Boolean simplifier mongod runs over the unwind's filter
//! (RM-699), and the change stream's `queryShapeHash` (RM-454).

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
            M::Leaf(..) | M::True | M::False => &[],
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
        let many = children
            .iter()
            .filter(|m| mergeable(m) && m.path() == p)
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
        out.push(M::Leaf(p, Leaf::In, Bson::Array(vs)));
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
    /// The equalities, and whether the list held a regex.
    In(String, Vec<Bson>, bool),
    /// `{$exists: true}`; `false` parses as its `$not`.
    Exists(String),
    /// Any other predicate on a path: its operator and operand.
    Other(String, String, Bson),
    /// `$expr`, `$where`, `$text`, ... -- nothing on a path.
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
                    "$in" => match arg {
                        Bson::Array(a) if a.is_empty() => U::False,
                        Bson::Array(a) => U::In(
                            path.to_string(),
                            a.iter()
                                .filter(|b| !matches!(b, Bson::RegularExpression(_)))
                                .cloned()
                                .collect(),
                            a.iter().any(|b| matches!(b, Bson::RegularExpression(_))),
                        ),
                        _ => U::Opaque,
                    },
                    "$exists" => U::Exists(path.to_string()),
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

/// mongod's `matchRewriteOperationType`.
fn rewrite_operation_type(u: &U, path: &str) -> Option<M> {
    // A subfield of `operationType` is always missing: the predicate is
    // what it says of a missing field.
    if path != "operationType" {
        let on_missing = match u {
            U::Eq(_, v) => matches!(v, Bson::Null),
            U::In(_, vs, _) => vs.iter().any(|v| matches!(v, Bson::Null)),
            U::Other(_, op, v) => matches!(op.as_str(), "$gte" | "$lte") && matches!(v, Bson::Null),
            _ => false,
        };
        return Some(if on_missing { M::True } else { M::False });
    }
    let named = |v: &Bson| match v {
        Bson::String(s) => op_type_rewrite(s).map(|m| (s.clone(), m)),
        _ => None,
    };
    match u {
        U::Eq(_, v) => Some(named(v).map_or(M::False, |(_, m)| m)),
        U::In(_, _, true) => None,
        U::In(_, vs, false) if vs.is_empty() => Some(M::False),
        U::In(_, vs, false) => {
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
/// rewrites: `operationType`. `None` is no rewrite.
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
            _ => None,
        },
        U::Opaque | U::False => None,
    }
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
    (
        M::And(oplog).normalized().to_doc(),
        M::And(unwind).normalized().to_doc(),
    )
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
        &Document::new(),
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
