//! `$group` over a collection scan, built as mongod 8.2.11's
//! `SlotBasedStageBuilder::buildGroup` builds it (`gen_group.cpp`, with the
//! accumulators of `gen_accumulator.cpp`).
//!
//! The scan may carry a filter, and a skip and a limit, in front of the
//! group (the query the pipeline's leading stages make; `filter.rs`).
//!
//! Slots come from one counter, in the order the builder takes them: a
//! skip's constant when a limit sits directly over it, the scan's record
//! and record id, its field slots (the group's top-level fields, sorted,
//! then the filter's, sorted), the limit's or lone skip's constant, the
//! dotted path's traversal, the spill slots, the group keys, the
//! accumulators, the finalize projection and the result. An expression that
//! is already a slot takes none, and a projection of nothing but slots is
//! not built at all (`SbBuilder::makeProject`).
//!
//! Built here: an `_id` that is a field path, `$$ROOT`, a constant, or an
//! object of those; accumulator arguments that are field paths or `$$ROOT`
//! (and numeric constants for `$sum`); at most one distinct dotted path,
//! since with two mongod orders their slots by a hash table salted per
//! process. A skip or limit only where the group reads no field (with
//! one, mongod puts a dependency projection among them that is not built
//! here). Also an `_id` part that is `$mod` of a field by a number, and a
//! `$push` / `$addToSet` of an object of fields (RM-693). Anything else
//! returns `None`.

use super::expr::{BinOp, Expr, Slot, NULLISH_MASK};
use super::filter::Filter;
use super::stage::{Agg, AggFn, Stage};
use super::Plan;
use bson::{doc, Bson, Document};
use std::collections::BTreeSet;

/// `internalQueryMaxPushBytes` / `internalQueryMaxAddToSetBytes`.
const CAPPED_BYTES: i32 = 104_857_600;

/// An argument as the builder generates it, before slots are known.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Arg {
    /// `$$ROOT` / `$$CURRENT`: the scan's record slot.
    Root,
    /// A top-level field: its scan slot.
    Field(String),
    /// A dotted path: the traversal's slot.
    Path(Vec<String>),
    /// A constant.
    Const(Bson),
    /// `{$mod: [<field>, <numeric constant>]}` in the `_id` (RM-693).
    Mod(String, Bson),
    /// An object of top-level fields, as a `$push` / `$addToSet` argument:
    /// `newObj` over their scan slots (RM-693).
    Obj(Vec<(String, String)>),
}

/// An `_id` part: [`arg_of`], or a `$mod` of a field by a number.
fn id_arg_of(v: &Bson) -> Option<Arg> {
    if let Bson::Document(d) = v {
        if let (1, Some(Bson::Array(a))) = (d.len(), d.get("$mod")) {
            return match (a.as_slice(), a.first().and_then(arg_of)) {
                ([_, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)], Some(Arg::Field(f))) => {
                    Some(Arg::Mod(f, a[1].clone()))
                }
                _ => None,
            };
        }
    }
    arg_of(v)
}

/// A `$push` / `$addToSet` argument: [`arg_of`], or an object of fields.
fn collect_arg_of(v: &Bson) -> Option<Arg> {
    match v {
        Bson::Document(d) if !d.is_empty() && !d.keys().any(|k| k.starts_with('$')) => {
            let children: Option<Vec<(String, String)>> = d
                .iter()
                .map(|(k, v)| match arg_of(v)? {
                    Arg::Field(f) => Some((k.clone(), f)),
                    _ => None,
                })
                .collect();
            Some(Arg::Obj(children?))
        }
        other => arg_of(other),
    }
}

pub(super) fn arg_of(v: &Bson) -> Option<Arg> {
    match v {
        Bson::String(s) if s == "$$ROOT" || s == "$$CURRENT" => Some(Arg::Root),
        Bson::String(s) if s.starts_with("$$") => None,
        Bson::String(s) if s.starts_with('$') => {
            let parts: Vec<String> = s[1..].split('.').map(str::to_string).collect();
            if parts.iter().any(String::is_empty) {
                return None;
            }
            Some(if parts.len() == 1 {
                Arg::Field(parts.into_iter().next()?)
            } else {
                Arg::Path(parts)
            })
        }
        // `$const` is how a desugared stage (`$count`) writes a literal.
        Bson::Document(d) if d.len() == 1 => match d.get("$literal").or_else(|| d.get("$const")) {
            Some(lit) if is_scalar(lit) => Some(Arg::Const(lit.clone())),
            _ => None,
        },
        other if is_scalar(other) => Some(Arg::Const(other.clone())),
        _ => None,
    }
}

/// The constants printed and compared here: no containers, no types whose
/// constant text is not reproduced.
fn is_scalar(v: &Bson) -> bool {
    matches!(
        v,
        Bson::Null | Bson::Boolean(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)
    ) || matches!(v, Bson::String(s) if !s.starts_with('$') && !s.contains('`'))
}

/// The `_id` of the group.
enum Id {
    One(Arg),
    Object(Vec<(String, Arg)>),
}

/// One accumulator of the spec.
struct Acc {
    name: String,
    op: String,
    arg: Option<Arg>,
}

fn parse(group: &Document) -> Option<(Id, Vec<Acc>)> {
    let id = match group.get("_id")? {
        Bson::Document(d) if !d.keys().any(|k| k.starts_with('$')) => {
            if d.is_empty() {
                return None;
            }
            let children: Option<Vec<(String, Arg)>> = d
                .iter()
                .map(|(k, v)| Some((k.clone(), id_arg_of(v)?)))
                .collect();
            let children = children?;
            // An object of constants is folded into one constant before
            // SBE sees it.
            if children.iter().all(|(_, a)| matches!(a, Arg::Const(_))) {
                return None;
            }
            Id::Object(children)
        }
        other => Id::One(id_arg_of(other)?),
    };
    let mut accs = Vec::new();
    for (name, spec) in group {
        if name == "_id" {
            continue;
        }
        let Bson::Document(spec) = spec else {
            return None;
        };
        let (op, value) = spec.iter().next().filter(|_| spec.len() == 1)?;
        let arg = match op.as_str() {
            "$count" => match value {
                Bson::Document(d) if d.is_empty() => None,
                _ => return None,
            },
            "$sum" | "$avg" | "$first" | "$last" | "$min" | "$max" | "$push" | "$addToSet" => {
                let a = if matches!(op.as_str(), "$push" | "$addToSet") {
                    collect_arg_of(value)?
                } else {
                    arg_of(value)?
                };
                let numeric = matches!(
                    &a,
                    Arg::Const(Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))
                );
                if matches!(a, Arg::Const(_)) && !(op == "$sum" && numeric) {
                    return None;
                }
                Some(a)
            }
            _ => return None,
        };
        accs.push(Acc {
            name: name.clone(),
            op: op.clone(),
            arg,
        });
    }
    Some((id, accs))
}

/// The slot counter (`IdGenerator`: the first id is 1).
pub(super) struct Slots(pub(super) Slot);

impl Slots {
    pub(super) fn next(&mut self) -> Slot {
        self.0 += 1;
        self.0
    }
}

/// `count()` is `$sum` of the int32 constant 1 (and `{$count: {}}`).
fn is_count(acc: &Acc) -> bool {
    acc.op == "$count" || matches!(acc.arg, Some(Arg::Const(Bson::Int32(1))))
}

/// The query in front of the group: its filter (the `parsedQuery`, and
/// the program built from it) and its paging (0: none).
pub(crate) struct Front {
    pub parsed: Document,
    pub filter: Option<Filter>,
    pub skip: i64,
    pub limit: i64,
    /// The query is a `COUNT_SCAN` of an index rather than a collection
    /// scan (RM-072): a group that reads no field over an index scan whose
    /// documents need no fetch.
    pub seek: Option<Seek>,
}

/// A `COUNT_SCAN` as `buildCountScan` builds it: an `ixseek` between two
/// KeyStrings of a single-field index.
pub(crate) struct Seek {
    /// The seek keys, with their discriminators.
    pub low: Vec<u8>,
    pub high: Vec<u8>,
    /// The index's (top-level) key field and its direction.
    pub path: String,
    pub descending: bool,
    pub index: String,
    /// The index's key pattern, as the plan's environment prints it.
    pub key_pattern: String,
    /// The `COUNT_SCAN` (or, under a fetch, `IXSCAN`) node of `queryPlan`,
    /// less its stage and node id.
    pub described: Document,
    /// The filter of the `FETCH` over the index scan, when the documents
    /// are fetched (RM-695): `None` for a `COUNT_SCAN`.
    pub fetch_filter: Option<Document>,
}

impl Front {
    /// How many query solution nodes the query makes: the scan, then a
    /// SKIP and a LIMIT when it has them.
    pub(super) fn nodes(&self) -> u32 {
        let fetched = self.seek.as_ref().is_some_and(|s| s.fetch_filter.is_some());
        1 + u32::from(fetched) + u32::from(self.skip > 0) + u32::from(self.limit > 0)
    }
}

/// The query's SBE stages, as `buildCollScan` and `buildLimitSkip` build
/// them: what they are, the slots they took and the query solution they
/// were built from.
pub(super) struct Scanned {
    pub root: Stage,
    /// The scanned document's slot.
    pub record: Slot,
    /// The top-level fields the scan reads into slots.
    pub fields: Vec<(String, Slot)>,
    /// The constants the plan's environment binds: each `$in`'s list, the
    /// skip's and the limit's amounts, as the environment prints them.
    pub env: Vec<(Slot, String)>,
    pub query_plan: Document,
}

/// The collection scan of the collection whose UUID is `uuid`, reading
/// `fields` and then the filter's, with the filter, skip and limit of
/// `front`. Slots come from `slots` in the builder's order: a skip's
/// constant when a limit sits directly over it, the record and record id,
/// the field slots, then the limit's or lone skip's constant.
pub(super) fn scan(
    front: &Front,
    uuid: &str,
    fields: BTreeSet<String>,
    slots: &mut Slots,
) -> Option<Scanned> {
    let (skip, limit) = (front.skip.max(0), front.limit.max(0));
    if let Some(seek) = front.seek.as_ref().filter(|s| s.fetch_filter.is_some()) {
        return fetch_scan(front, seek, uuid, fields, slots);
    }
    if let Some(seek) = &front.seek {
        if front.filter.is_some() || skip > 0 || limit > 0 || !fields.is_empty() {
            return None;
        }
        let record_id = slots.next();
        let mut query_plan = doc! { "stage": "COUNT_SCAN", "planNodeId": 1 };
        query_plan.extend(seek.described.clone());
        return Some(Scanned {
            root: Stage::IxSeek {
                node: 1,
                record_id,
                low: seek.low.clone(),
                high: seek.high.clone(),
                path: seek.path.clone(),
                descending: seek.descending,
                index: seek.index.clone(),
                uuid: uuid.to_string(),
                fetch: None,
            },
            // No record is read: nothing may ask for `$$ROOT`.
            record: 0,
            fields: vec![],
            env: vec![],
            query_plan,
        });
    }
    let front_nodes = front.nodes();
    let merged_skip = (skip > 0 && limit > 0).then(|| slots.next());
    let record = slots.next();
    let record_id = slots.next();
    let mut names: Vec<String> = fields.into_iter().collect();
    if let Some(f) = &front.filter {
        for name in f.fields() {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    let scan_fields: Vec<(String, Slot)> = names.into_iter().map(|f| (f, slots.next())).collect();
    let field_slot = |f: &str| scan_fields.iter().find(|(n, _)| n == f).map(|(_, s)| *s);
    let mut root = Stage::Scan {
        node: 1,
        record,
        record_id,
        fields: scan_fields.clone(),
        uuid: uuid.to_string(),
    };
    let mut env: Vec<(Slot, String)> = Vec::new();
    if let Some(f) = &front.filter {
        let predicate = f.program(Some(record), &field_slot, &mut || slots.next())?;
        for (slot, values) in predicate.in_lists() {
            env.push((slot, super::print::in_list(&values)?));
        }
        root = Stage::Filter {
            node: 1,
            predicate,
            input: Box::new(root),
        };
    }
    let mut query_plan = doc! {
        "stage": "COLLSCAN",
        "planNodeId": 1,
        "filter": front.parsed.clone(),
        "direction": "forward",
    };
    if skip > 0 {
        query_plan = doc! {
            "stage": "SKIP",
            "planNodeId": 2,
            "skipAmount": super::number(skip),
            "inputStage": query_plan,
        };
    }
    if limit > 0 {
        query_plan = doc! {
            "stage": "LIMIT",
            "planNodeId": front_nodes as i32,
            "limitAmount": super::number(limit),
            "inputStage": query_plan,
        };
    }
    if skip > 0 || limit > 0 {
        let skip_slot = (skip > 0).then(|| merged_skip.unwrap_or_else(|| slots.next()));
        let limit_slot = (limit > 0).then(|| slots.next());
        let skip_at = skip_slot.map(|s| (s, skip));
        let limit_at = limit_slot.map(|s| (s, limit));
        env.extend(skip_at.map(|(s, n)| (s, n.to_string())));
        env.extend(limit_at.map(|(s, n)| (s, n.to_string())));
        env.sort();
        root = Stage::LimitSkip {
            node: front_nodes,
            limit: limit_at,
            skip: skip_at,
            input: Box::new(root),
        };
    }
    Some(Scanned {
        root,
        record,
        fields: scan_fields,
        env,
        query_plan,
    })
}

/// A `FETCH` with a filter over an `IXSCAN`, as `buildFetch` over
/// `buildIndexScan` builds it for a group that reads no field (RM-695):
/// an `nlj` whose outer `ixseek` reads each entry's record id, key,
/// snapshot id and index ident into slots, correlated into a `limit 1`
/// `seek` of the document that reads the filter's fields, under the
/// `filter`. Slots in the builder's order: record id, index ident, key,
/// snapshot id, the key pattern (an environment constant), then the
/// seek's record and record id and its fields.
fn fetch_scan(
    front: &Front,
    seek: &Seek,
    uuid: &str,
    fields: BTreeSet<String>,
    slots: &mut Slots,
) -> Option<Scanned> {
    let f = front.filter.as_ref()?;
    if front.skip > 0 || front.limit > 0 || !fields.is_empty() {
        return None;
    }
    let record_id = slots.next();
    let ident = slots.next();
    let key = slots.next();
    let snapshot = slots.next();
    let key_pattern = slots.next();
    let record = slots.next();
    let fetched_id = slots.next();
    let scan_fields: Vec<(String, Slot)> =
        f.fields().into_iter().map(|n| (n, slots.next())).collect();
    let field_slot = |n: &str| scan_fields.iter().find(|(m, _)| m == n).map(|(_, s)| *s);
    let predicate = f.program(None, &field_slot, &mut || slots.next())?;
    let mut env: Vec<(Slot, String)> = vec![(key_pattern, seek.key_pattern.clone())];
    for (slot, values) in predicate.in_lists() {
        env.push((slot, super::print::in_list(&values)?));
    }
    env.sort();
    let outer = Stage::IxSeek {
        node: 1,
        record_id,
        low: seek.low.clone(),
        high: seek.high.clone(),
        path: seek.path.clone(),
        descending: seek.descending,
        index: seek.index.clone(),
        uuid: uuid.to_string(),
        fetch: Some((key, snapshot, ident)),
    };
    let inner = Stage::LimitLit {
        node: 2,
        limit: 1,
        input: Box::new(Stage::Seek {
            node: 2,
            seek_record_id: record_id,
            record,
            record_id: fetched_id,
            snapshot,
            ident,
            key,
            key_pattern,
            fields: scan_fields.clone(),
            uuid: uuid.to_string(),
        }),
    };
    let root = Stage::Filter {
        node: 2,
        predicate,
        input: Box::new(Stage::Nlj {
            node: 2,
            projects: vec![],
            correlated: vec![record_id, snapshot, ident, key, key_pattern],
            outer: Box::new(outer),
            inner: Box::new(inner),
        }),
    };
    let mut ixscan = doc! { "stage": "IXSCAN", "planNodeId": 1 };
    ixscan.extend(seek.described.clone());
    let query_plan = doc! {
        "stage": "FETCH",
        "planNodeId": 2,
        "filter": seek.fetch_filter.clone()?,
        "inputStage": ixscan,
    };
    Some(Scanned {
        root,
        record,
        fields: scan_fields,
        env,
        query_plan,
    })
}

pub(super) fn build(group: &Document, uuid: &str, last: bool, front: &Front) -> Option<Plan> {
    let (id, accs) = parse(group)?;
    let mut args: Vec<&Arg> = match &id {
        Id::One(a) => vec![a],
        Id::Object(children) => children.iter().map(|(_, a)| a).collect(),
    };
    args.extend(accs.iter().filter_map(|a| a.arg.as_ref()));
    let paths: Vec<&Vec<String>> = {
        let mut seen: Vec<&Vec<String>> = Vec::new();
        for a in &args {
            if let Arg::Path(p) = a {
                if !seen.contains(&p) {
                    seen.push(p);
                }
            }
        }
        seen
    };
    if paths.len() > 1 {
        return None;
    }
    let fields: BTreeSet<String> = args
        .iter()
        .flat_map(|a| match a {
            Arg::Field(f) | Arg::Mod(f, _) => vec![f.clone()],
            Arg::Path(p) => vec![p[0].clone()],
            Arg::Obj(children) => children.iter().map(|(_, f)| f.clone()).collect(),
            _ => vec![],
        })
        .collect();
    let whole_document = args.iter().any(|a| matches!(a, Arg::Root));
    if front.seek.is_some() && (whole_document || !paths.is_empty()) {
        return None;
    }
    let projected = !fields.is_empty() && !whole_document;
    if projected && (front.skip > 0 || front.limit > 0) {
        return None;
    }
    // The query's nodes: the scan, then a SKIP and a LIMIT. Node 2 is the
    // dependency projection pushdown puts under the group when it needs a
    // finite set of fields; it builds no SBE stage.
    let node: u32 = if projected { 3 } else { front.nodes() + 1 };

    let mut slots = Slots(0);
    let Scanned {
        mut root,
        record,
        fields: scan_fields,
        env,
        query_plan,
    } = scan(front, uuid, fields, &mut slots)?;
    let field_slot = |f: &str| scan_fields.iter().find(|(n, _)| n == f).map(|(_, s)| *s);

    // The dotted path: `traverseP` over its top-level field, one lambda per
    // level, framed from 1 + (levels - 1) since generating it took those.
    let mut path_slot = None;
    if let Some(path) = paths.first() {
        let top = field_slot(&path[0])?;
        let first_frame = path.len() as u32 + front.filter.as_ref().map_or(0, Filter::frames);
        let traversal = traverse(Expr::Slot(top), &path[1..], first_frame);
        let s = slots.next();
        path_slot = Some(s);
        root = Stage::Project {
            node,
            binds: vec![(s, traversal)],
            input: Box::new(root),
        };
    }
    let gen = |a: &Arg| -> Option<Expr> {
        Some(match a {
            Arg::Root => Expr::Slot(record),
            Arg::Field(f) => Expr::Slot(field_slot(f)?),
            Arg::Path(_) => Expr::Slot(path_slot?),
            Arg::Const(c) => Expr::Const(Some(c.clone())),
            // `$mod`'s program (`ExpressionMod` in `sbe_stage_builder_expression.cpp`).
            Arg::Mod(f, divisor) => {
                let x = Expr::Slot(field_slot(f)?);
                Expr::if_(
                    Expr::fill_empty(
                        Expr::TypeMatch(Box::new(x.clone()), NULLISH_MASK),
                        Expr::Const(Some(Bson::Boolean(true))),
                    ),
                    Expr::null(),
                    Expr::if_(
                        Expr::func("isNumber", vec![x.clone()]),
                        Expr::func("mod", vec![x, Expr::Const(Some(divisor.clone()))]),
                        Expr::func(
                            "fail",
                            vec![
                                Expr::Const(Some(Bson::Int32(7157718))),
                                Expr::Const(Some(Bson::String(
                                    "$mod only supports numeric types".into(),
                                ))),
                            ],
                        ),
                    ),
                )
            }
            Arg::Obj(children) => {
                let mut args = Vec::new();
                for (name, f) in children {
                    args.push(Expr::Const(Some(Bson::String(name.clone()))));
                    args.push(Expr::Slot(field_slot(f)?));
                }
                Expr::func("newObj", args)
            }
        })
    };

    // Spill slots, per accumulator in order ($avg takes two).
    let spills: Vec<Vec<Slot>> = accs
        .iter()
        .map(|a| {
            let n = if a.op == "$avg" { 2 } else { 1 };
            (0..n).map(|_| slots.next()).collect()
        })
        .collect();

    // The group keys, and what `_id` is after the group.
    enum IdOut {
        Constant(Expr),
        Key(Slot),
        Object(Vec<(String, Slot)>),
    }
    let mut key_binds: Vec<(Slot, Expr)> = Vec::new();
    let mut keys: Vec<Slot> = Vec::new();
    let id_out = match &id {
        Id::One(Arg::Const(c)) => IdOut::Constant(Expr::Const(Some(c.clone()))),
        Id::One(a) => {
            let s = slots.next();
            key_binds.push((s, Expr::fill_empty(gen(a)?, Expr::null())));
            keys.push(s);
            IdOut::Key(s)
        }
        Id::Object(children) if children.len() == 1 => {
            let (name, a) = &children[0];
            let s = slots.next();
            let e = match a {
                Arg::Const(c) => Expr::Const(Some(c.clone())),
                other => Expr::fill_empty(gen(other)?, Expr::null()),
            };
            key_binds.push((s, e));
            keys.push(s);
            IdOut::Object(vec![(name.clone(), s)])
        }
        Id::Object(children) => {
            let mut out = Vec::new();
            for (name, a) in children {
                let s = match gen(a)? {
                    Expr::Slot(s) => s,
                    constant => {
                        let s = slots.next();
                        key_binds.push((s, constant));
                        s
                    }
                };
                if !keys.contains(&s) {
                    keys.push(s);
                }
                out.push((name.clone(), s));
            }
            IdOut::Object(out)
        }
    };
    if !key_binds.is_empty() {
        root = Stage::Project {
            node,
            binds: key_binds,
            input: Box::new(root),
        };
    }

    // The accumulators' slots, their programs and merging expressions.
    let mut aggs: Vec<Agg> = Vec::new();
    let mut outputs: Vec<Vec<Slot>> = Vec::new();
    for (acc, spill) in accs.iter().zip(&spills) {
        let x = match &acc.arg {
            Some(a) => Some(gen(a)?),
            None => None,
        };
        let mut mine = Vec::new();
        let mut push = |func: AggFn, arg: Option<Expr>, spill: Slot, merging: Expr| {
            let slot = slots.next();
            mine.push(slot);
            aggs.push(Agg {
                slot,
                func,
                arg,
                spill,
                merging,
            });
        };
        let p = |i: usize| Expr::Slot(spill[i]);
        if is_count(acc) {
            push(AggFn::Count, None, spill[0], Expr::func("sum", vec![p(0)]));
        } else {
            let x = x?;
            match acc.op.as_str() {
                "$sum" => push(
                    AggFn::DoubleDoubleSum,
                    Some(x),
                    spill[0],
                    Expr::func("aggMergeDoubleDoubleSums", vec![p(0)]),
                ),
                "$avg" => {
                    push(
                        AggFn::DoubleDoubleSum,
                        Some(x.clone()),
                        spill[0],
                        Expr::func("aggMergeDoubleDoubleSums", vec![p(0)]),
                    );
                    let not_counted = Expr::bin(
                        BinOp::LogicOr,
                        Expr::fill_empty(
                            Expr::TypeMatch(Box::new(x.clone()), NULLISH_MASK),
                            Expr::Const(Some(Bson::Boolean(true))),
                        ),
                        Expr::Not(Box::new(Expr::func("isNumber", vec![x]))),
                    );
                    let one = Expr::if_(not_counted, Expr::Int64(0), Expr::Int64(1));
                    push(
                        AggFn::Sum,
                        Some(one),
                        spill[1],
                        Expr::func("sum", vec![p(1)]),
                    );
                }
                "$first" | "$last" => {
                    let func = if acc.op == "$first" {
                        AggFn::First
                    } else {
                        AggFn::Last
                    };
                    let name = func.name();
                    push(
                        func,
                        Some(Expr::fill_empty(x, Expr::null())),
                        spill[0],
                        Expr::func(name, vec![Expr::fill_empty(p(0), Expr::null())]),
                    );
                }
                "$min" | "$max" => {
                    let func = if acc.op == "$min" {
                        AggFn::Min
                    } else {
                        AggFn::Max
                    };
                    let name = func.name();
                    let fill = |e: Expr| {
                        Expr::func(
                            "fillType",
                            vec![
                                e,
                                Expr::Const(Some(Bson::Int32(NULLISH_MASK as i32))),
                                Expr::nothing(),
                            ],
                        )
                    };
                    push(
                        func,
                        Some(fill(x)),
                        spill[0],
                        Expr::func(name, vec![fill(p(0))]),
                    );
                }
                "$push" | "$addToSet" => {
                    let (func, merge) = if acc.op == "$push" {
                        (AggFn::AddToArray(CAPPED_BYTES), "aggConcatArraysCapped")
                    } else {
                        (AggFn::AddToSet(CAPPED_BYTES), "aggSetUnionCapped")
                    };
                    push(
                        func,
                        Some(x),
                        spill[0],
                        Expr::func(
                            merge,
                            vec![p(0), Expr::Const(Some(Bson::Int32(CAPPED_BYTES)))],
                        ),
                    );
                }
                _ => return None,
            }
        }
        outputs.push(mine);
    }
    root = Stage::Group {
        node,
        keys,
        aggs,
        input: Box::new(root),
    };

    // The finalize projection: `_id` first, where it is not a key slot,
    // then each accumulator that has a finalize step.
    let mut final_binds: Vec<(Slot, Expr)> = Vec::new();
    let id_slot = match id_out {
        IdOut::Key(s) => s,
        IdOut::Constant(c) => {
            let s = slots.next();
            final_binds.push((s, c));
            s
        }
        IdOut::Object(children) => {
            let mut args = Vec::new();
            for (name, s) in children {
                args.push(Expr::Const(Some(Bson::String(name))));
                args.push(Expr::Slot(s));
            }
            let s = slots.next();
            final_binds.push((s, Expr::func("newObj", args)));
            s
        }
    };
    let mut result_args = vec![
        Expr::Const(Some(Bson::String("_id".into()))),
        Expr::Slot(id_slot),
    ];
    for (acc, out) in accs.iter().zip(&outputs) {
        let a = Expr::Slot(out[0]);
        let finalized = if is_count(acc) {
            Some(Expr::fill_empty(Expr::ConvertInt32(Box::new(a.clone())), a))
        } else {
            match acc.op.as_str() {
                "$sum" => Some(Expr::func("doubleDoubleSumFinalize", vec![a])),
                "$avg" => {
                    let count = Expr::Slot(out[1]);
                    Some(Expr::if_(
                        Expr::bin(BinOp::Eq, count.clone(), Expr::Int64(0)),
                        Expr::null(),
                        Expr::bin(
                            BinOp::Div,
                            Expr::func("doubleDoubleSumFinalize", vec![a]),
                            count,
                        ),
                    ))
                }
                "$min" | "$max" => Some(Expr::fill_empty(a, Expr::null())),
                "$push" | "$addToSet" => Some(Expr::func(
                    "getElement",
                    vec![a, Expr::Const(Some(Bson::Int32(0)))],
                )),
                _ => None,
            }
        };
        let value = match finalized {
            Some(e) => {
                let s = slots.next();
                final_binds.push((s, e));
                s
            }
            None => out[0],
        };
        result_args.push(Expr::Const(Some(Bson::String(acc.name.clone()))));
        result_args.push(Expr::Slot(value));
    }
    if !final_binds.is_empty() {
        root = Stage::Project {
            node,
            binds: final_binds,
            input: Box::new(root),
        };
    }
    let result = slots.next();
    let make = if last { "newBsonObj" } else { "newObj" };
    root = Stage::Project {
        node,
        binds: vec![(result, Expr::func(make, result_args))],
        input: Box::new(root),
    };
    Some(Plan {
        root,
        result,
        env,
        query_plan: doc! {
            "stage": "GROUP",
            "planNodeId": node as i32,
            "inputStage": query_plan,
        },
        eof: false,
    })
}

/// `traverseP` down `rest` from `input`, the lambda of each level in frame
/// `frame`, `frame + 1`, ... outermost first.
fn traverse(input: Expr, rest: &[String], frame: u32) -> Expr {
    let local = Expr::Local { frame, moved: true };
    let field = Expr::func(
        "getField",
        vec![local, Expr::Const(Some(Bson::String(rest[0].clone())))],
    );
    let body = if rest.len() == 1 {
        field
    } else {
        traverse(field, &rest[1..], frame + 1)
    };
    Expr::func(
        "traverseP",
        vec![
            input,
            Expr::Lambda {
                frame,
                body: Box::new(body),
            },
            Expr::Const(Some(Bson::Int32(1))),
        ],
    )
}
