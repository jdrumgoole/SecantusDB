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
//! Not reproduced: the rewrite of a user `$match` into the oplog filter
//! (RM-452), the counts of a read that starts before the end of the oplog
//! (RM-453), and the change stream's `queryShapeHash` (RM-454).

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
enum M {
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
            M::Leaf(..) => &[],
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
    /// its child -- then `sortTree`: children first, each list stably.
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
            out.sort_by(M::compare);
            out
        };
        match self {
            M::And(c) => {
                let mut c = flat(c, true);
                if c.len() == 1 {
                    c.pop().expect("one")
                } else {
                    M::And(c)
                }
            }
            M::Or(c) => {
                let mut c = flat(c, false);
                if c.len() == 1 {
                    c.pop().expect("one")
                } else {
                    M::Or(c)
                }
            }
            M::Nor(c) => {
                let mut c: Vec<M> = c.into_iter().map(M::normalized).collect();
                c.sort_by(M::compare);
                M::Nor(c)
            }
            M::Not(c) => M::Not(Box::new(c.normalized())),
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
            M::Not(child) => {
                let inner = child.to_doc();
                let (path, pred) = inner.into_iter().next().expect("a leaf");
                doc! { path: { "$not": pred } }
            }
            M::ElemObject(path, child) => doc! { path.clone(): { "$elemMatch": child.to_doc() } },
        }
    }
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

/// The entries that are an event of the stream: its writes and commands,
/// and -- unless `showExpandedEvents` -- only of the original event types.
fn events(scope: &Scope, n: &Names, spec: &Spec) -> M {
    let entries = M::Or(vec![commands(scope, n, spec), writes(&n.ns)]);
    if spec.expanded {
        return entries;
    }
    let command = |field: &str| M::And(vec![op_is("c"), exists(field)]);
    M::And(vec![
        entries,
        M::Or(vec![
            command("o.drop"),
            command("o.dropDatabase"),
            command("o.renameCollection"),
            leaf(
                "op",
                Leaf::In,
                Bson::Array(vec!["d".into(), "i".into(), "u".into()]),
            ),
        ]),
    ])
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
fn filters(scope: &Scope, spec: &Spec, start: Timestamp) -> (Document, Document) {
    let n = Names::of(scope, spec.system);
    let mut any = Vec::new();
    if spec.expanded {
        if let Some(views) = &n.views {
            any.push(writes(views));
        }
    }
    any.push(events(scope, &n, spec));
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
    let mut unwind = vec![events(scope, &n, spec)];
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

/// The time a resume token resumes from: this server's own token, or
/// mongod's.
fn token_time(token: &Document) -> Option<Timestamp> {
    let hex = token.get_str("_data").ok()?;
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| {
            hex.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
        })
        .collect();
    bytes
        .and_then(|b| secantus_core::bsondoc::from_bytes(&b).ok())
        .and_then(|d| d.get_timestamp("t").ok())
        .or_else(|| crate::resume_keystring::cluster_time(token))
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
    let (front, rest) = match run.first().filter(|s| s.contains_key("$match")) {
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
            out.push(doc! { "$match": { "$and": all } });
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
    let (start, token) = match (spec.resume(), raw.get_timestamp("startAtOperationTime")) {
        (Some((_, token)), _) => (
            token_time(token).unwrap_or(Timestamp {
                time: 0,
                increment: 0,
            }),
            token.clone(),
        ),
        (None, Ok(ts)) => {
            let seq = match storage.seq_for_timestamp(ts) {
                0 => storage.oplog_tail_seq(),
                seq => seq - 1,
            };
            (ts, at_seq(seq))
        }
        (None, Err(_)) => {
            let now = storage.peek_cluster_time();
            (
                Timestamp {
                    time: now.time,
                    increment: now.increment + 1,
                },
                at_seq(storage.oplog_tail_seq()),
            )
        }
    };
    let (filter, unwind) = filters(&scope, &spec, start);
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
        "parsedQuery": filter,
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
    if wanted {
        let mut collscan = doc! { "stage": "COLLSCAN", "filter": scan.get("filter").cloned() };
        idle_counters(&mut collscan);
        collscan.insert("direction", "forward");
        collscan.insert("minRecord", min_record);
        collscan.insert("docsExamined", 0_i32);
        let mut projected = doc! { "isCached": false, "stage": "PROJECTION_SIMPLE" };
        idle_counters(&mut projected);
        projected.insert("transformBy", transform_by);
        projected.insert("inputStage", collscan);
        let mut stats = doc! {
            "executionSuccess": true,
            "nReturned": 0_i32,
            "executionTimeMillis": 0_i32,
            "totalKeysExamined": 0_i32,
            "totalDocsExamined": 0_i32,
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
    stages.extend(stages_after_read(&spec, &db, &pipeline[1..], &token));
    if wanted {
        for stage in &mut stages {
            stage.insert("nReturned", 0_i64);
            stage.insert("executionTimeMillisEstimate", 0_i64);
        }
    }
    let mut reply = doc! {
        "explainVersion": "1",
        "stages": stages.into_iter().map(Bson::Document).collect::<Vec<_>>(),
    };
    reply.insert("serverInfo", server_info(ctx));
    reply.insert("serverParameters", server_parameters());
    reply.insert("command", echo);
    reply.insert("ok", 1.0);
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes(scope: &Scope, raw: Document) -> (String, String) {
        let spec = Spec::of(&raw);
        let (filter, _) = filters(
            scope,
            &spec,
            Timestamp {
                time: 1,
                increment: 1,
            },
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
        let inner = doc! { "s": 3_i64, "t": Timestamp { time: 7, increment: 2 }, "n": "", "k": {} };
        let bytes = secantus_core::bsondoc::to_vec(&inner).expect("bytes");
        let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
        assert_eq!(
            token_time(&doc! { "_data": hex }),
            Some(Timestamp {
                time: 7,
                increment: 2
            })
        );
    }
}
