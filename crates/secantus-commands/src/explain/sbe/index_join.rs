//! A `$lookup` on `localField` / `foreignField` whose foreign field leads an
//! index, which mongod 8.2.11 runs as an indexed loop join (`EQ_LOOKUP`,
//! strategy `IndexedLoopJoin`): the stages `buildIndexJoinLookupStage`
//! (`stage_builder/sbe/gen_lookup.cpp`) builds, measured on 8.2.11 and run
//! here over the documents of both collections and the index's entries
//! (RM-683).
//!
//! The outer side is the local rows, each with the set of its join keys
//! (`buildKeySetForLocal`, the program the hash join shares). For each row
//! the inner side `unwind`s that set into its values, makes the intervals
//! of the index to seek for each (a value, and for an array value the
//! array's first element, `undefined` for an empty one, unless that is a
//! key already) as the low and high KeyStrings of `ks`, `ixseek`s each
//! interval, drops the documents a multikey index returns twice
//! (`unique_roaring`), `seek`s the documents, keeps those whose own keys
//! hold one of the set's (a `filter` of `traverseF`, as the index may hold
//! one key for values that are not equal: `undefined`, `null` and the empty
//! array, or a hash) and groups them into an array, which an empty array
//! replaces when there is none (`union` of the group and a constant, under
//! a `limit 1`).
//!
//! The index is chosen as mongod chooses it (`determineLookupStrategy`,
//! `planner_analysis.cpp`; `lookup_joins`, `explain/mod.rs`). The slots are
//! taken in the order the builder takes them: the key set, the unwound
//! value and its position, the array branch, the value branch, the union's
//! output, (a hashed index's `shardHash`,) the two KeyStrings, the
//! `ixseek`'s record id, index ident, key and snapshot id and the key
//! pattern's slot of the environment, the `seek`'s record and record id,
//! then the group's spill, its output, the array, the constant, the
//! union's output and the result.

use super::expr::{BinOp, Env, Expr, Slot, Value};
use super::group::Slots;
use super::lookup::{frames_taken, limit_one, outer_keys};
use super::stage::{Agg, AggFn, Counted, Row, Stage};
use bson::{Bson, Document};
use std::sync::Arc;

/// The index a `$lookup` seeks: its name and key pattern, and whether it
/// is multikey.
#[derive(Clone, Debug)]
pub(crate) struct ForeignIndex {
    pub name: String,
    pub key: Document,
    pub multikey: bool,
    /// A unique index on one field: a seek of one key reads no entry past
    /// it.
    pub unique: bool,
    /// The index's collation is the query's: it can be seeked. A collated
    /// index is still the index the plan names, and a hash join runs.
    pub seekable: bool,
}

/// What the join runs over, and the programs it evaluates.
#[derive(Debug)]
pub(crate) struct Foreign {
    docs: Vec<Document>,
    /// The index's entries, each document's keys: the KeyString of the
    /// whole key, and the record id, in index order.
    entries: Vec<(Vec<u8>, i64)>,
    first_desc: bool,
    first_hashed: bool,
    multikey: bool,
    unique_point: bool,
    set: Slot,
    unwound: Slot,
    array_slot: Slot,
    array_value: Expr,
    array_filter: Expr,
    record: Slot,
    filter: Expr,
    matched: Slot,
}

/// The key pattern as the environment prints it: `{"b" : 1, "x" : -1}`.
fn pattern_text(key: &Document) -> String {
    let parts: Vec<String> = key
        .iter()
        .map(|(k, v)| {
            let v = match v {
                Bson::String(s) => format!("\"{s}\""),
                Bson::Int32(n) => n.to_string(),
                Bson::Int64(n) => n.to_string(),
                Bson::Double(d) => format!("{d}"),
                other => other.to_string(),
            };
            format!("\"{k}\" : {v}")
        })
        .collect();
    format!("{{{}}}", parts.join(", "))
}

/// `Ordering::getBits`: bit `i` set when key field `i` is descending.
fn ordering_bits(key: &Document) -> i32 {
    key.values()
        .enumerate()
        .filter(|(_, v)| crate::util::as_i64(v).is_some_and(|n| n < 0))
        .fold(0, |bits, (i, _)| bits | (1 << i))
}

fn is_desc(v: &Bson) -> bool {
    crate::util::as_i64(v).is_some_and(|n| n < 0)
}

/// The index's entries over `docs`: for each document the keys its fields
/// give (a hashed field's is its hash), a combination of one per field,
/// each once; sorted as the index is, by KeyString and then record id.
fn index_entries(docs: &[Document], key: &Document) -> Option<Vec<(Vec<u8>, i64)>> {
    let mut entries = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        let per_field: Vec<Vec<Bson>> = key
            .iter()
            .map(|(path, spec)| {
                if secantus_core::hashed::is_hashed_spec(spec) {
                    let v = secantus_core::get_path_values(d, path).0;
                    vec![secantus_core::hashed::index_key(v.first().copied(), None)]
                } else {
                    secantus_core::index_keys::field_keys(d, path, false).mongod
                }
            })
            .collect();
        let mut combos: Vec<Vec<&Bson>> = vec![vec![]];
        for keys in &per_field {
            combos = combos
                .into_iter()
                .flat_map(|c| {
                    keys.iter().map(move |k| {
                        let mut c = c.clone();
                        c.push(k);
                        c
                    })
                })
                .collect();
        }
        let mut seen: Vec<Vec<u8>> = Vec::new();
        for combo in combos {
            let values: Vec<(&Bson, bool)> = combo
                .iter()
                .zip(key.values())
                .map(|(k, spec)| (*k, is_desc(spec)))
                .collect();
            // A key the KeyString encoder cannot write (a decimal) is not
            // one this plan can seek.
            let bytes = crate::key_string::encode_key(&values, None).ok()?;
            if !seen.contains(&bytes) {
                seen.push(bytes.clone());
                entries.push((bytes, i as i64 + 1));
            }
        }
    }
    entries.sort();
    Some(entries)
}

/// The foreign field's filter (`buildForeignMatches`): the `traverseF`s
/// over its path that hold of a document one of whose keys the local set
/// has. The frames are numbered as the lowering numbers them, outermost
/// first, from `first`.
fn foreign_filter(path: &[String], record: Slot, set: Slot, first: u32) -> Expr {
    fn level(i: usize, path: &[String], object: Expr, set: Slot, next: &mut u32) -> Expr {
        let last = i + 1 == path.len();
        let bind = (!last).then(|| {
            *next += 1;
            *next
        });
        *next += 1;
        let lambda = *next;
        let get = Expr::fill_empty(
            Expr::func(
                "getField",
                vec![object, Expr::Const(Some(Bson::String(path[i].clone())))],
            ),
            Expr::null(),
        );
        let input = match bind {
            Some(frame) => Expr::let_(
                frame,
                get,
                Expr::if_(
                    Expr::TypeMatch(Box::new(Expr::local(frame, false)), 24),
                    Expr::local(frame, true),
                    Expr::nothing(),
                ),
            ),
            None => get,
        };
        let body = if last {
            Expr::func("isMember", vec![Expr::local(lambda, true), Expr::Slot(set)])
        } else {
            Expr::if_(
                Expr::fill_empty(
                    Expr::func("isObject", vec![Expr::local(lambda, false)]),
                    Expr::Const(Some(Bson::Boolean(true))),
                ),
                level(i + 1, path, Expr::local(lambda, true), set, next),
                Expr::Const(Some(Bson::Boolean(false))),
            )
        };
        Expr::func(
            "traverseF",
            vec![
                input,
                Expr::Lambda {
                    frame: lambda,
                    body: Box::new(body),
                },
                Expr::Const(Some(Bson::Boolean(last))),
            ],
        )
    }
    let mut next = first - 1;
    level(0, path, Expr::Slot(record), set, &mut next)
}

/// What building one join gives: the plan so far with the join over it,
/// the slot of its result and the frame ids it used.
pub(super) struct Joined {
    pub root: Stage,
    pub result: Slot,
    pub frames: u32,
}

/// Everything `build` needs of the join and the plan under it.
pub(super) struct Inputs<'a> {
    pub node: u32,
    pub root: Stage,
    pub current: Slot,
    pub local: &'a [String],
    pub foreign: &'a [String],
    pub frame_base: u32,
    pub object: &'static str,
    pub spec: Expr,
    pub uuid: &'a str,
    pub docs: &'a [Document],
    pub index: &'a ForeignIndex,
}

/// The indexed loop join of one `$lookup` over `root`.
pub(super) fn build(
    i: Inputs<'_>,
    slots: &mut Slots,
    env: &mut Vec<(Slot, String)>,
) -> Option<Joined> {
    let Inputs {
        node,
        root,
        current,
        local,
        foreign,
        frame_base,
        object,
        spec,
        uuid,
        docs,
        index,
    } = i;
    let key = &index.key;
    let first_spec = key.values().next().cloned().unwrap_or(Bson::Int32(1));
    let first_hashed = secantus_core::hashed::is_hashed_spec(&first_spec);
    let first_desc = is_desc(&first_spec);
    let bits = ordering_bits(key);

    // The local keys: `arrayToSet` of the field's values.
    let set = slots.next();
    let local_frames = frames_taken(local.len());
    let outer = Stage::Project {
        node,
        binds: vec![(set, outer_keys(current, local, frame_base))],
        input: Box::new(root),
    };
    // The value stream: each key of the set, and the intervals it seeks.
    let unwound = slots.next();
    let position = slots.next();
    let array_slot = slots.next();
    let value_slot = slots.next();
    let union_out = slots.next();
    let array_value = Expr::fill_empty(
        Expr::func(
            "getElement",
            vec![Expr::Slot(unwound), Expr::Const(Some(Bson::Int32(0)))],
        ),
        Expr::Const(Some(Bson::Undefined)),
    );
    let array_filter = Expr::bin(
        BinOp::LogicAnd,
        Expr::func("isArray", vec![Expr::Slot(unwound)]),
        Expr::Not(Box::new(Expr::func(
            "isMember",
            vec![Expr::Slot(array_slot), Expr::Slot(set)],
        ))),
    );
    let array_branch = Stage::Filter {
        node,
        predicate: array_filter.clone(),
        input: Box::new(Stage::Project {
            node,
            binds: vec![(array_slot, array_value.clone())],
            input: Box::new(limit_one(node)),
        }),
    };
    let value_branch = Stage::Project {
        node,
        binds: vec![(value_slot, Expr::Slot(unwound))],
        input: Box::new(limit_one(node)),
    };
    let mut values = Stage::Union {
        node,
        inputs: vec![
            (vec![array_slot], array_branch),
            (vec![value_slot], value_branch),
        ],
        outputs: vec![union_out],
    };
    let mut bound = union_out;
    if first_hashed {
        let hashed = slots.next();
        values = Stage::Project {
            node,
            binds: vec![(hashed, Expr::func("shardHash", vec![Expr::Slot(union_out)]))],
            input: Box::new(values),
        };
        bound = hashed;
    }
    let (low, high) = (slots.next(), slots.next());
    let ks = |discriminator: i64| {
        Expr::func(
            "ks",
            vec![
                Expr::Int64(1),
                Expr::Const(Some(Bson::Int32(bits))),
                Expr::Slot(bound),
                Expr::Int64(discriminator),
            ],
        )
    };
    let intervals = Stage::Project {
        node,
        binds: vec![(low, ks(1)), (high, ks(2))],
        input: Box::new(values),
    };
    let unwind = Stage::Unwind {
        node,
        input_slot: set,
        out: unwound,
        index: position,
        preserve: true,
        input: Box::new(limit_one(node)),
    };
    let bounds = Stage::Nlj {
        node,
        projects: vec![],
        correlated: vec![unwound],
        outer: Box::new(unwind),
        inner: Box::new(intervals),
    };
    let (record_id, ident, index_key, snapshot) =
        (slots.next(), slots.next(), slots.next(), slots.next());
    // The key pattern is a slot of the environment, which a second join
    // over the same index shares.
    let text = pattern_text(key);
    let pattern = match env.iter().find(|(_, t)| *t == text) {
        Some((slot, _)) => *slot,
        None => {
            let slot = slots.next();
            env.push((slot, text));
            slot
        }
    };
    let ixseek = Stage::IxSeekBounds {
        node,
        low,
        high,
        record_id,
        ident,
        key: index_key,
        snapshot,
        index: index.name.clone(),
        uuid: uuid.to_string(),
    };
    let mut seeks = Stage::Nlj {
        node,
        projects: vec![],
        correlated: vec![low, high],
        outer: Box::new(bounds),
        inner: Box::new(ixseek),
    };
    if index.multikey {
        seeks = Stage::UniqueRoaring {
            node,
            slot: record_id,
            input: Box::new(seeks),
        };
    }
    let (record, seek_id) = (slots.next(), slots.next());
    let fetch = Stage::Nlj {
        node,
        projects: vec![],
        correlated: vec![record_id, snapshot, ident, index_key, pattern],
        outer: Box::new(seeks),
        inner: Box::new(Stage::LimitLit {
            node,
            limit: 1,
            input: Box::new(Stage::Seek {
                node,
                seek_record_id: record_id,
                record,
                record_id: seek_id,
                snapshot,
                ident,
                key: index_key,
                key_pattern: pattern,
                fields: vec![],
                uuid: uuid.to_string(),
            }),
        }),
    };
    // The frames: the filter's creation takes two for each component of
    // the path, and the lowering numbers its binders from there.
    let after_local = frame_base + local_frames;
    let first_frame = after_local + 2 * foreign.len() as u32 + 1;
    let filter = foreign_filter(foreign, record, set, first_frame);
    let filtered = Stage::Filter {
        node,
        predicate: filter.clone(),
        input: Box::new(fetch),
    };
    const CAP: i32 = 104_857_600;
    let spill = slots.next();
    let grouped = slots.next();
    let array_out = slots.next();
    let empty = slots.next();
    let matched = slots.next();
    let result = slots.next();
    let group = Stage::Group {
        node,
        keys: vec![],
        aggs: vec![Agg {
            slot: grouped,
            func: AggFn::AddToArray(CAP),
            arg: Some(Expr::Slot(record)),
            spill,
            merging: Expr::func(
                "aggConcatArraysCapped",
                vec![Expr::Slot(spill), Expr::Const(Some(Bson::Int32(CAP)))],
            ),
        }],
        input: Box::new(filtered),
    };
    let found = Stage::Project {
        node,
        binds: vec![(
            array_out,
            Expr::func(
                "getElement",
                vec![Expr::Slot(grouped), Expr::Const(Some(Bson::Int32(0)))],
            ),
        )],
        input: Box::new(group),
    };
    let none = Stage::Project {
        node,
        binds: vec![(empty, Expr::Const(Some(Bson::Array(vec![]))))],
        input: Box::new(limit_one(node)),
    };
    let right = Stage::LimitLit {
        node,
        limit: 1,
        input: Box::new(Stage::Union {
            node,
            inputs: vec![(vec![array_out], found), (vec![empty], none)],
            outputs: vec![matched],
        }),
    };
    let foreign_data = Arc::new(Foreign {
        docs: docs.to_vec(),
        entries: index_entries(docs, key)?,
        first_desc,
        first_hashed,
        multikey: index.multikey,
        unique_point: index.unique && key.len() == 1,
        set,
        unwound,
        array_slot,
        array_value,
        array_filter,
        record,
        filter,
        matched,
    });
    let join = Stage::IndexJoin {
        node,
        projects: vec![current],
        correlated: vec![set],
        outer: Box::new(outer),
        inner: Box::new(right),
        foreign: foreign_data,
    };
    let project = Stage::Project {
        node,
        binds: vec![(
            result,
            Expr::func(object, vec![spec, Expr::Slot(current), Expr::Slot(matched)]),
        )],
        input: Box::new(join),
    };
    Some(Joined {
        root: project,
        result,
        frames: local_frames + 4 * foreign.len() as u32 - 1,
    })
}

/// Whether every key a local document's `path` gives is one the plan can
/// seek: a value, and each element of an array, that the KeyString encoder
/// writes.
pub(crate) fn seekable_keys(docs: &[Document], path: &str) -> bool {
    docs.iter().all(|d| {
        secantus_core::get_path_values(d, path)
            .0
            .into_iter()
            .all(|v| {
                let writes = |x: &Bson| crate::key_string::encode_key(&[(x, false)], None).is_ok();
                writes(v)
                    && match v {
                        Bson::Array(items) => items.iter().all(writes),
                        _ => true,
                    }
            })
    })
}

fn eval(e: &Expr, row: &Row) -> Value {
    let slots = |s: Slot| row.get(&s).cloned().flatten();
    let mut env = Env {
        slots: &slots,
        locals: Vec::new(),
    };
    e.eval(&mut env)
}

/// What the join's inner side did for the rows it ran for.
#[derive(Default)]
struct Tally {
    /// Local rows.
    rows: i64,
    /// Keys of the sets (rows unwound).
    values: i64,
    /// Values whose first element is an interval of its own.
    arrays: i64,
    /// Intervals seeked, entries read, and records found by them.
    seeks: i64,
    keys: i64,
    /// Records after the duplicates are dropped, and the rows that keep a
    /// document.
    unique: i64,
    kept: i64,
    /// Rows with a record, and with a document kept.
    fetching: i64,
    with_match: i64,
}

fn counts(n: i64, opens: i64, closes: i64, eof: bool, input: Option<Counted>) -> Counted {
    Counted {
        returned: n,
        opens: Some(opens),
        closes: Some(closes),
        eof,
        input: input.map(Box::new),
        ..Counted::default()
    }
}

/// A `limit 1` over a `coscan` that ran `opens` times: the limit is at its
/// end when it was asked again.
fn limit_coscan(opens: i64, closes: i64, eof: bool) -> Counted {
    counts(
        opens,
        opens,
        closes,
        eof,
        Some(counts(opens, opens, closes, false, None)),
    )
}

/// The statistics of the inner side's stages, in the shape the stage tree
/// is built in (`build`).
fn inner_counts(inner: &Stage, t: &Tally, shape: (bool, bool, bool), cut: bool) -> Counted {
    let (multikey, hashed, unique_point) = shape;
    let l = t.rows;
    if l == 0 {
        return Counted::blank(inner);
    }
    let (with_record, without) = (t.fetching, l - t.with_match);
    let more = t.unique;
    // Stages of the values: the unwound keys, and the intervals.
    let unwind = counts(t.values, l, l, true, Some(limit_coscan(l, l, true)));
    let branch = |returned: i64| {
        counts(
            returned,
            t.values,
            t.values,
            true,
            Some(limit_coscan(t.values, t.values, true)),
        )
    };
    let array_branch = {
        let mut f = counts(t.arrays, t.values, t.values, true, Some(branch(t.values)));
        f.tested = t.values;
        f
    };
    let bounds = t.values + t.arrays;
    let union = Counted {
        kids: vec![array_branch, branch(t.values)],
        ..counts(bounds, t.values, l, true, None)
    };
    let mut intervals = counts(bounds, t.values, l, true, Some(union));
    if hashed {
        intervals = counts(bounds, t.values, l, true, Some(intervals));
    }
    let _ = &intervals;
    let value_join = Counted {
        kids: vec![unwind, intervals],
        inner_opens: t.values,
        inner_closes: l,
        ..counts(bounds, l, l, true, None)
    };
    let ixseek = Counted {
        keys: t.keys,
        seeks: t.seeks,
        reads: if unique_point {
            t.seeks
        } else {
            t.keys + t.seeks
        },
        ..counts(t.keys, t.seeks, l, true, None)
    };
    let seek_join = Counted {
        kids: vec![value_join, ixseek],
        inner_opens: t.seeks,
        inner_closes: l,
        ..counts(t.keys, l, l, true, None)
    };
    let outer = if multikey {
        Counted {
            dups_tested: t.keys,
            dups_dropped: t.keys - more,
            ..counts(more, l, l, true, Some(seek_join))
        }
    } else {
        seek_join
    };
    let seek = Counted {
        reads: more,
        ..counts(more, more, with_record, false, None)
    };
    let fetch_inner = counts(more, more, with_record, more > 0, Some(seek));
    let fetch = Counted {
        kids: vec![outer, fetch_inner],
        inner_opens: more,
        inner_closes: with_record,
        ..counts(more, l, l, true, None)
    };
    let filter = Counted {
        tested: more,
        ..counts(t.kept, l, l, true, Some(fetch))
    };
    let exhausted = t.with_match < l;
    let group = counts(t.with_match, l, l, exhausted, Some(filter));
    let found = counts(t.with_match, l, l, exhausted, Some(group));
    let empty = counts(
        without,
        without,
        without,
        false,
        Some(counts(
            without,
            without,
            without,
            false,
            Some(counts(without, without, without, false, None)),
        )),
    );
    let union = Counted {
        kids: vec![found, empty],
        ..counts(l, l, 1, false, None)
    };
    // Each run's limit is asked again after its row, but for the last
    // when the consumer stopped there.
    counts(l, l, 1, l > 1 || !cut, Some(union))
}

/// `nlj` of the local rows and the index seeks of each (`IndexJoin`).
pub(super) fn run(stage: &Stage, docs: &[Document], demand: Option<usize>) -> (Vec<Row>, Counted) {
    let Stage::IndexJoin {
        outer,
        inner,
        foreign: f,
        ..
    } = stage
    else {
        return (Vec::new(), Counted::default());
    };
    let (rows, outer_counted) = outer.run(docs, demand);
    let mut t = Tally {
        rows: rows.len() as i64,
        ..Tally::default()
    };
    let mut out = Vec::with_capacity(rows.len());
    for mut row in rows {
        let set = match row.get(&f.set).cloned().flatten() {
            Some(Bson::Array(a)) => a,
            _ => Vec::new(),
        };
        let set_value = Some(Bson::Array(set.clone()));
        // The intervals of each key of the set.
        let mut bounds: Vec<Bson> = Vec::new();
        for w in &set {
            let mut r = Row::new();
            r.insert(f.set, set_value.clone());
            r.insert(f.unwound, Some(w.clone()));
            let first = eval(&f.array_value, &r);
            r.insert(f.array_slot, first.clone());
            if eval(&f.array_filter, &r) == Some(Bson::Boolean(true)) {
                if let Some(first) = first {
                    bounds.push(first);
                    t.arrays += 1;
                }
            }
            bounds.push(w.clone());
        }
        t.values += set.len() as i64;
        // Seek each interval of the index.
        let mut found: Vec<i64> = Vec::new();
        for b in &bounds {
            t.seeks += 1;
            let value = if f.first_hashed {
                secantus_core::hashed::index_key(Some(b), None)
            } else {
                b.clone()
            };
            let Ok(mut prefix) = crate::key_string::encode_key(&[(&value, f.first_desc)], None)
            else {
                continue;
            };
            prefix.pop();
            let from = f
                .entries
                .partition_point(|(k, _)| k.as_slice() < prefix.as_slice());
            for (k, rid) in &f.entries[from..] {
                if !k.starts_with(&prefix) {
                    break;
                }
                t.keys += 1;
                found.push(*rid);
            }
        }
        let reached = found.len();
        if f.multikey {
            let mut seen = std::collections::HashSet::new();
            found.retain(|rid| seen.insert(*rid));
        }
        let _ = reached;
        t.unique += found.len() as i64;
        t.fetching += i64::from(!found.is_empty());
        let mut kept: Vec<Bson> = Vec::new();
        for rid in &found {
            let Some(d) = usize::try_from(rid - 1).ok().and_then(|i| f.docs.get(i)) else {
                continue;
            };
            let mut r = Row::new();
            r.insert(f.set, set_value.clone());
            r.insert(f.record, Some(Bson::Document(d.clone())));
            if eval(&f.filter, &r) == Some(Bson::Boolean(true)) {
                kept.push(Bson::Document(d.clone()));
            }
        }
        t.kept += kept.len() as i64;
        t.with_match += i64::from(!kept.is_empty());
        row.insert(f.matched, Some(Bson::Array(kept)));
        out.push(row);
    }
    let cut = demand.is_some_and(|d| out.len() >= d);
    let hashed = f.first_hashed;
    let below = inner_counts(inner, &t, (f.multikey, hashed, f.unique_point), cut);
    let l = t.rows;
    (
        out,
        Counted {
            returned: l,
            eof: outer_counted.eof,
            kids: vec![outer_counted, below],
            inner_opens: l,
            inner_closes: i64::from(l > 0),
            ..Counted::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{foreign_filter, index_entries, ordering_bits, pattern_text};
    use crate::explain::sbe::print::listed;
    use bson::doc;

    /// The filter on a foreign field of one component (mongod 8.2.11,
    /// measured 2026-11-27): the `isMember` lambda is the first frame.
    #[test]
    fn a_one_component_filter_prints_as_mongod_does() {
        let text = listed(&foreign_filter(&["b".to_string()], 16, 3, 5));
        assert_eq!(
            text,
            "traverseF((getField(s16, \"b\") ?: null), lambda(l5.0) { isMember(move(l5.0), s3) }, true) "
        );
    }

    /// A dotted field numbers its binders outermost first: the `let` of a
    /// level, then its lambda (`l9` to `l13` for `b.c.d` after four frames).
    #[test]
    fn a_dotted_filter_numbers_frames_outermost_first() {
        let path: Vec<String> = ["b", "c", "d"].map(String::from).to_vec();
        let text = listed(&foreign_filter(&path, 16, 3, 9));
        for frame in [
            "l9.0 = ",
            "lambda(l10.0)",
            "l11.0 = ",
            "lambda(l12.0)",
            "lambda(l13.0)",
        ] {
            assert!(text.contains(frame), "{frame} in {text}");
        }
    }

    #[test]
    fn a_key_pattern_prints_and_orders_as_mongod_does() {
        let key = doc! { "b": 1, "x": -1, "y": "hashed" };
        assert_eq!(
            pattern_text(&key),
            "{\"b\" : 1, \"x\" : -1, \"y\" : \"hashed\"}"
        );
        assert_eq!(ordering_bits(&key), 2);
    }

    /// A document gives an entry for each distinct key of an array, and
    /// the entries are in KeyString order, then record id.
    #[test]
    fn the_index_holds_each_key_of_a_document_once() {
        let docs = vec![doc! { "b": [2, 2, 1] }, doc! { "b": 1 }, doc! {}];
        let entries = index_entries(&docs, &doc! { "b": 1 }).expect("encodable");
        let ids: Vec<i64> = entries.iter().map(|(_, id)| *id).collect();
        // null (the third document), 1 (the first, then the second), 2.
        assert_eq!(ids, vec![3, 1, 2, 1]);
    }
}
