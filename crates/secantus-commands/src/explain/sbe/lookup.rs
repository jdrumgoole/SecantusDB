//! A `$lookup` on `localField` / `foreignField` mongod 8.2.11 pushes into
//! SBE and runs as a hash join (`EQ_LOOKUP`, strategy `HashJoin`): the
//! stages `buildLookupHashJoin` (`sbe_stage_builder_lookup.cpp`) builds,
//! measured on 8.2.11 and run here over the documents of both collections
//! (RM-683).
//!
//! The outer side is the query's plan with a `project` of the local
//! field's join keys (an `arrayToSet` of the field's value, or of
//! `[null]` for none, and of what a dotted path traverses). The inner side
//! is a scan of the foreign collection with, for each record, a correlated
//! program that yields the keys of its foreign field: one value per
//! component of the path, an `unwind` per component after the first, and a
//! `union` of each element of an array value and the array itself; a
//! `group` collects them into a set. `hash_lookup` builds its table from
//! those and, for each outer row, collects the records that share a key;
//! `(s ?: [])` and a `makeBsonObj` set the result into the document.
//!
//! Slots are taken in the order the builder takes them: the outer key, the
//! foreign record and record id, the keys of each path component, then the
//! union's unwind and its output, the branch's output, the group's spill
//! and output, the lookup's and the result. A frame id is taken twice per
//! path component of the local field before the field's own are, and
//! three more for each component but the last.
//!
//! Built for a foreign collection that exists and has no index on the
//! foreign field, or does not exist (`NonExistentForeignCollection`); a
//! foreign field an index leads is an indexed loop join (`index_join.rs`);
//! the other strategies are not built here (`None`).

use super::expr::{BinOp, Expr, Slot};
use super::group::{scan, Front, Scanned, Slots};
use super::index_join::{self, ForeignIndex};
use super::stage::{Agg, AggFn, Stage};
use super::Plan;
use bson::{doc, Bson, Document};
use std::collections::BTreeSet;

/// One `$lookup` stage of the pushed pipeline.
pub(crate) struct Join {
    /// The foreign collection's namespace, as `foreignCollection` names it.
    pub namespace: String,
    pub local_field: String,
    pub foreign_field: String,
    pub as_field: String,
    /// The foreign collection's UUID, and its documents in record order;
    /// `None` when it does not exist.
    pub foreign: Option<(String, Vec<Document>)>,
    /// The index the strategy is an indexed loop join over; `None`: no
    /// index qualifies, and the foreign collection is scanned.
    pub index: Option<ForeignIndex>,
    /// No index is seeked and no hash table built: the foreign collection
    /// is scanned for each local row (`NestedLoopJoin`).
    pub nested: bool,
}

/// A frame id source: ids are taken in the order the program is built.
struct Frames(u32);

impl Frames {
    fn next(&mut self) -> u32 {
        self.0 += 1;
        self.0
    }
}

fn mv(frame: u32) -> Expr {
    Expr::local(frame, true)
}

/// The levels of a dotted path below the first, printed outermost first:
/// frames `first`, `second` and the lambda's `arg` are taken before the
/// body's own.
fn path_below(read: Expr, rest: &[String], frames: &mut Frames) -> Expr {
    let first = frames.next();
    let second = frames.next();
    let arg = frames.next();
    let traverse = Expr::func(
        "traverseP",
        vec![
            Expr::local(first, false),
            Expr::Lambda {
                frame: arg,
                body: Box::new(if rest.len() == 1 {
                    Expr::func(
                        "getField",
                        vec![mv(arg), Expr::Const(Some(Bson::String(rest[0].clone())))],
                    )
                } else {
                    path_below(
                        Expr::func(
                            "getField",
                            vec![mv(arg), Expr::Const(Some(Bson::String(rest[0].clone())))],
                        ),
                        &rest[1..],
                        frames,
                    )
                }),
            },
            Expr::Const(Some(Bson::Int32(1))),
        ],
    );
    Expr::let_(
        first,
        read,
        Expr::let_(
            second,
            traverse,
            Expr::if_(
                Expr::func("isArray", vec![mv(first)]),
                Expr::func("unwindArray", vec![mv(second)]),
                Expr::func("makeOwn", vec![mv(second)]),
            ),
        ),
    )
}

/// The outer side's join keys: the field's value as a set of keys.
pub(super) fn outer_keys(record: Slot, path: &[String], base: u32) -> Expr {
    let n = path.len() as u32;
    // `2n` frame ids are taken before the program's own, the let's the last.
    let frame = base + 2 * n;
    let mut frames = Frames(frame);
    let head = Expr::func(
        "getField",
        vec![
            Expr::Slot(record),
            Expr::Const(Some(Bson::String(path[0].clone()))),
        ],
    );
    let (value, own) = if path.len() == 1 {
        (head, false)
    } else {
        (path_below(head, &path[1..], &mut frames), true)
    };
    let take = |own: bool| {
        if own {
            Expr::func("makeOwn", vec![mv(frame)])
        } else {
            mv(frame)
        }
    };
    Expr::func(
        "arrayToSet",
        vec![Expr::let_(
            frame,
            value,
            Expr::if_(
                Expr::fill_empty(
                    Expr::func("isArray", vec![Expr::local(frame, false)]),
                    Expr::Const(Some(Bson::Boolean(false))),
                ),
                Expr::if_(
                    Expr::func("isArrayEmpty", vec![Expr::local(frame, false)]),
                    Expr::Const(Some(Bson::Array(vec![Bson::Null]))),
                    take(own),
                ),
                Expr::func("newArray", vec![Expr::fill_empty(take(own), Expr::null())]),
            ),
        )],
    )
}

pub(super) fn limit_one(node: u32) -> Stage {
    Stage::LimitLit {
        node,
        limit: 1,
        input: Box::new(Stage::Coscan { node }),
    }
}

/// The frames a local field of `n` components takes.
pub(super) fn frames_taken(n: usize) -> u32 {
    (5 * n as u32).saturating_sub(3).max(2)
}

/// The plan for the query `front` over the collection whose UUID is
/// `uuid` with each of `joins` over it, in order; `last`: nothing classic
/// follows, so the result is a BSON object. `None` when it is outside what
/// is built here.
pub(super) fn build(uuid: &str, front: &Front, joins: &[Join], last: bool) -> Option<Plan> {
    if joins.is_empty() || front.seek.is_some() {
        return None;
    }
    let mut slots = Slots(0);
    let Scanned {
        mut root,
        record,
        mut env,
        mut query_plan,
        ..
    } = scan(front, uuid, BTreeSet::new(), &mut slots)?;
    let mut frame_base = front.filter.as_ref().map_or(0, |f| f.frames());
    let mut node = front.nodes();
    let mut current = record;
    for (i, join) in joins.iter().enumerate() {
        node += 1;
        let local: Vec<String> = join.local_field.split('.').map(str::to_string).collect();
        let foreign: Vec<String> = join.foreign_field.split('.').map(str::to_string).collect();
        if local.iter().chain(&foreign).any(String::is_empty) || join.as_field.contains('.') {
            return None;
        }
        let object = if last && i + 1 == joins.len() {
            "makeBsonObj"
        } else {
            "makeObj"
        };
        let spec = Expr::SetSpec(vec![join.as_field.clone()]);
        let strategy;
        let result;
        let mut index_fields: Option<&ForeignIndex> = None;
        match &join.foreign {
            None => {
                strategy = "NonExistentForeignCollection";
                let empty = slots.next();
                result = slots.next();
                root = Stage::Project {
                    node,
                    binds: vec![(empty, Expr::Const(Some(Bson::Array(vec![]))))],
                    input: Box::new(root),
                };
                root = Stage::Project {
                    node,
                    binds: vec![(
                        result,
                        Expr::func(object, vec![spec, Expr::Slot(current), Expr::Slot(empty)]),
                    )],
                    input: Box::new(root),
                };
            }
            Some((foreign_uuid, docs)) if join.index.as_ref().is_some_and(|ix| ix.seekable) => {
                let index = join.index.as_ref()?;
                strategy = "IndexedLoopJoin";
                let joined = index_join::build(
                    index_join::Inputs {
                        node,
                        root,
                        current,
                        local: &local,
                        foreign: &foreign,
                        frame_base,
                        object,
                        spec,
                        uuid: foreign_uuid,
                        docs,
                    },
                    index,
                    &mut slots,
                    &mut env,
                )?;
                root = joined.root;
                result = joined.result;
                frame_base += joined.frames;
                index_fields = Some(index);
            }
            Some((foreign_uuid, docs)) if join.nested => {
                strategy = "NestedLoopJoin";
                index_fields = join.index.as_ref();
                let joined = index_join::build_nested(
                    index_join::Inputs {
                        node,
                        root,
                        current,
                        local: &local,
                        foreign: &foreign,
                        frame_base,
                        object,
                        spec,
                        uuid: foreign_uuid,
                        docs,
                    },
                    &mut slots,
                );
                root = joined.root;
                result = joined.result;
                frame_base += joined.frames;
            }
            Some((foreign_uuid, docs)) => {
                strategy = "HashJoin";
                index_fields = join.index.as_ref();
                let key = slots.next();
                let rec = slots.next();
                let rid = slots.next();
                // The keys of the foreign field: a value per path component,
                // an `unwind` of the value before it for each after the first.
                let mut value = slots.next();
                let mut left = Stage::Project {
                    node,
                    binds: vec![(
                        value,
                        Expr::fill_empty(
                            Expr::func(
                                "getField",
                                vec![
                                    Expr::Slot(rec),
                                    Expr::Const(Some(Bson::String(foreign[0].clone()))),
                                ],
                            ),
                            Expr::null(),
                        ),
                    )],
                    input: Box::new(limit_one(node)),
                };
                for name in &foreign[1..] {
                    let elem = slots.next();
                    let index = slots.next();
                    let next = slots.next();
                    left = Stage::Unwind {
                        node,
                        input_slot: value,
                        out: elem,
                        index,
                        preserve: true,
                        input: Box::new(left),
                    };
                    let pick = Expr::if_(
                        Expr::bin(
                            BinOp::LogicOr,
                            Expr::func("isObject", vec![Expr::Slot(elem)]),
                            Expr::Not(Box::new(Expr::func("isArray", vec![Expr::Slot(value)]))),
                        ),
                        Expr::fill_empty(
                            Expr::func(
                                "getField",
                                vec![
                                    Expr::Slot(elem),
                                    Expr::Const(Some(Bson::String(name.clone()))),
                                ],
                            ),
                            Expr::null(),
                        ),
                        Expr::nothing(),
                    );
                    left = Stage::Project {
                        node,
                        binds: vec![(next, pick)],
                        input: Box::new(left),
                    };
                    value = next;
                }
                let unwound = slots.next();
                let position = slots.next();
                let union = slots.next();
                let branch = slots.next();
                let spill = slots.next();
                let set = slots.next();
                let joined = slots.next();
                let filled = slots.next();
                result = slots.next();
                let right = Stage::Branch {
                    node,
                    predicate: Expr::func("isArray", vec![Expr::Slot(value)]),
                    then_slots: vec![union],
                    else_slots: vec![value],
                    outputs: vec![branch],
                    then_: Box::new(Stage::Union {
                        node,
                        inputs: vec![
                            (
                                vec![unwound],
                                Stage::Unwind {
                                    node,
                                    input_slot: value,
                                    out: unwound,
                                    index: position,
                                    preserve: true,
                                    input: Box::new(limit_one(node)),
                                },
                            ),
                            (vec![value], limit_one(node)),
                        ],
                        outputs: vec![union],
                    }),
                    else_: Box::new(limit_one(node)),
                };
                let keys = Stage::Group {
                    node,
                    keys: vec![],
                    aggs: vec![Agg {
                        slot: set,
                        func: AggFn::AddToSetUncapped,
                        arg: Some(Expr::Slot(branch)),
                        spill,
                        merging: Expr::func("aggSetUnion", vec![Expr::Slot(spill)]),
                    }],
                    input: Box::new(Stage::Nlj {
                        node,
                        projects: vec![],
                        correlated: vec![value],
                        outer: Box::new(left),
                        inner: Box::new(right),
                    }),
                };
                let inner = Stage::Nlj {
                    node,
                    projects: vec![rec],
                    correlated: vec![rec],
                    outer: Box::new(Stage::Scan {
                        node,
                        record: rec,
                        record_id: rid,
                        fields: vec![],
                        uuid: foreign_uuid.clone(),
                    }),
                    inner: Box::new(keys),
                };
                let outer = Stage::Project {
                    node,
                    binds: vec![(key, outer_keys(current, &local, frame_base))],
                    input: Box::new(root),
                };
                frame_base += frames_taken(local.len());
                root = Stage::HashLookup {
                    node,
                    outer_key: key,
                    inner_key: set,
                    inner_value: rec,
                    out: joined,
                    outer: Box::new(outer),
                    inner: Box::new(inner),
                    foreign: docs.clone(),
                };
                root = Stage::Project {
                    node,
                    binds: vec![(
                        filled,
                        Expr::fill_empty(
                            Expr::Slot(joined),
                            Expr::Const(Some(Bson::Array(vec![]))),
                        ),
                    )],
                    input: Box::new(root),
                };
                root = Stage::Project {
                    node,
                    binds: vec![(
                        result,
                        Expr::func(object, vec![spec, Expr::Slot(current), Expr::Slot(filled)]),
                    )],
                    input: Box::new(root),
                };
            }
        }
        current = result;
        let mut eq = doc! {
            "stage": "EQ_LOOKUP",
            "planNodeId": node as i32,
            "foreignCollection": join.namespace.clone(),
            "localField": join.local_field.clone(),
            "foreignField": join.foreign_field.clone(),
            "asField": join.as_field.clone(),
            "strategy": strategy,
        };
        if let Some(ix) = index_fields {
            eq.insert("indexName", ix.name.clone());
            eq.insert("indexKeyPattern", ix.key.clone());
        }
        eq.insert("scanDirection", "forward");
        eq.insert("inputStage", query_plan);
        query_plan = eq;
    }
    Some(Plan {
        root,
        result: current,
        env,
        query_plan,
        eof: false,
    })
}

#[cfg(test)]
mod tests {
    use super::{frames_taken, outer_keys};
    use crate::explain::sbe::print::listed;

    /// The `projections` text of the outer key of a `$lookup` on `a`
    /// (mongod 8.2.11, measured 2026-11-26).
    #[test]
    fn a_one_component_local_field_prints_as_mongod_does() {
        let text = listed(&outer_keys(1, &["a".to_string()], 0));
        assert_eq!(
            text,
            "arrayToSet(\n    let [\n        l2.0 = getField(s1, \"a\") \n    ] \n    in \n        \
             if (isArray(l2.0) ?: false) \n        then \n            if isArrayEmpty(l2.0) \n            \
             then [null] \n            else move(l2.0) \n        \n        \
             else newArray((move(l2.0) ?: null)) \n) "
        );
    }

    /// A dotted local field of `n` components takes `2n` frame ids before
    /// its own and three for each component after the first (measured: one
    /// join on `a.x` and one on `a` number `l4` and `l9`).
    #[test]
    fn a_dotted_local_field_takes_frames() {
        assert_eq!(
            [1, 2, 3].map(frames_taken),
            [2, 7, 12],
            "frames taken by local fields of one, two and three components"
        );
        let two = listed(&outer_keys(1, &["a".to_string(), "x".to_string()], 0));
        assert!(two.contains("l4.0 = \n"), "{two}");
        assert!(two.contains("lambda(l7.0)"), "{two}");
    }
}
