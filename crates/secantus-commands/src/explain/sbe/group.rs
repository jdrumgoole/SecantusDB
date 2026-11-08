//! `$group` over a collection scan, built as mongod 8.2.11's
//! `SlotBasedStageBuilder::buildGroup` builds it (`gen_group.cpp`, with the
//! accumulators of `gen_accumulator.cpp`).
//!
//! Slots come from one counter, in the order the builder takes them: the
//! scan's record and record id, its field slots (top-level fields, sorted),
//! the dotted path's traversal, the spill slots, the group keys, the
//! accumulators, the finalize projection and the result. An expression that
//! is already a slot takes none, and a projection of nothing but slots is
//! not built at all (`SbBuilder::makeProject`).
//!
//! Built here: an `_id` that is a field path, `$$ROOT`, a constant, or an
//! object of those; accumulator arguments that are field paths or `$$ROOT`
//! (and numeric constants for `$sum`); at most one distinct dotted path,
//! since with two mongod orders their slots by a hash table salted per
//! process. Anything else returns `None`.

use super::expr::{BinOp, Expr, Slot, NULLISH_MASK};
use super::stage::{Agg, AggFn, Stage};
use super::Plan;
use bson::{doc, Bson, Document};
use std::collections::BTreeSet;

/// `internalQueryMaxPushBytes` / `internalQueryMaxAddToSetBytes`.
const CAPPED_BYTES: i32 = 104_857_600;

/// An argument as the builder generates it, before slots are known.
#[derive(Clone, Debug, PartialEq)]
enum Arg {
    /// `$$ROOT` / `$$CURRENT`: the scan's record slot.
    Root,
    /// A top-level field: its scan slot.
    Field(String),
    /// A dotted path: the traversal's slot.
    Path(Vec<String>),
    /// A constant.
    Const(Bson),
}

fn arg_of(v: &Bson) -> Option<Arg> {
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
                .map(|(k, v)| Some((k.clone(), arg_of(v)?)))
                .collect();
            let children = children?;
            // An object of constants is folded into one constant before
            // SBE sees it.
            if children.iter().all(|(_, a)| matches!(a, Arg::Const(_))) {
                return None;
            }
            Id::Object(children)
        }
        other => Id::One(arg_of(other)?),
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
                let a = arg_of(value)?;
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
struct Slots(Slot);

impl Slots {
    fn next(&mut self) -> Slot {
        self.0 += 1;
        self.0
    }
}

/// `count()` is `$sum` of the int32 constant 1 (and `{$count: {}}`).
fn is_count(acc: &Acc) -> bool {
    acc.op == "$count" || matches!(acc.arg, Some(Arg::Const(Bson::Int32(1))))
}

pub(super) fn build(group: &Document, uuid: &str, last: bool) -> Option<Plan> {
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
        .filter_map(|a| match a {
            Arg::Field(f) => Some(f.clone()),
            Arg::Path(p) => Some(p[0].clone()),
            _ => None,
        })
        .collect();
    let whole_document = args.iter().any(|a| matches!(a, Arg::Root));
    // Node 2 is the dependency projection pushdown puts under the group
    // when it needs a finite set of fields; it builds no SBE stage.
    let node: u32 = if !fields.is_empty() && !whole_document {
        3
    } else {
        2
    };

    let mut slots = Slots(0);
    let record = slots.next();
    let record_id = slots.next();
    let scan_fields: Vec<(String, Slot)> = fields.into_iter().map(|f| (f, slots.next())).collect();
    let field_slot = |f: &str| scan_fields.iter().find(|(n, _)| n == f).map(|(_, s)| *s);
    let mut root = Stage::Scan {
        node: 1,
        record,
        record_id,
        fields: scan_fields.clone(),
        uuid: uuid.to_string(),
    };

    // The dotted path: `traverseP` over its top-level field, one lambda per
    // level, framed from 1 + (levels - 1) since generating it took those.
    let mut path_slot = None;
    if let Some(path) = paths.first() {
        let top = field_slot(&path[0])?;
        let first_frame = path.len() as u32;
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
        query_plan: doc! {
            "stage": "GROUP",
            "planNodeId": node as i32,
            "inputStage": {
                "stage": "COLLSCAN",
                "planNodeId": 1,
                "filter": {},
                "direction": "forward",
            },
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
