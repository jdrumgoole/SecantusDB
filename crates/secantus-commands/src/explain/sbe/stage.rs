//! SBE stages (`exec/sbe/stages/`): `scan`, `filter`, `limit` /
//! `limitskip`, `project` and `group` (`HashAggStage`), run over a collection's documents with the statistics
//! each one keeps (`PlanStageStats` / `SpecificStats`), and printed as
//! mongod's `debugPrint` prints them.

use super::corr;
use super::expr::{compare, Env, Expr, Slot, Value};
use bson::{Bson, Document};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

/// An accumulator of a `group` stage: the function `HashAggStage` steps
/// with each row, and what it prints.
#[derive(Clone, Debug)]
pub(crate) enum AggFn {
    /// `count()`.
    Count,
    /// `sum(e)`.
    Sum,
    /// `aggDoubleDoubleSum(e)`.
    DoubleDoubleSum,
    /// `first(e)` / `last(e)`.
    First,
    Last,
    /// `min(e)` / `max(e)`.
    Min,
    Max,
    /// `addToArrayCapped(e, cap)` / `addToSetCapped(e, cap)`.
    AddToArray(i32),
    AddToSet(i32),
    /// `addToSet(e)`: a `$lookup`'s foreign key set (`aggSetUnion` merges
    /// its spills).
    AddToSetUncapped,
}

impl AggFn {
    pub fn name(&self) -> &'static str {
        match self {
            AggFn::Count => "count",
            AggFn::Sum => "sum",
            AggFn::DoubleDoubleSum => "aggDoubleDoubleSum",
            AggFn::First => "first",
            AggFn::Last => "last",
            AggFn::Min => "min",
            AggFn::Max => "max",
            AggFn::AddToArray(_) => "addToArrayCapped",
            AggFn::AddToSet(_) => "addToSetCapped",
            AggFn::AddToSetUncapped => "addToSet",
        }
    }

    /// The accumulator's state after one more row whose argument is `v`.
    pub(super) fn step(&self, state: Value, v: Value) -> Value {
        match self {
            AggFn::Count => Some(Bson::Int64(match state {
                Some(Bson::Int64(n)) => n + 1,
                _ => 1,
            })),
            AggFn::Sum | AggFn::DoubleDoubleSum => {
                let total = match state {
                    Some(Bson::Array(s)) => s.into_iter().next(),
                    other => other,
                };
                let next = match (total, v) {
                    (t, None) => t,
                    (None, Some(x)) if super::expr::as_f64(&x).is_some() => Some(x),
                    (None, Some(_)) => None,
                    (Some(t), Some(x)) => super::expr::add(&t, &x).or(Some(t)),
                };
                let next = next.or(Some(Bson::Int32(0)));
                if matches!(self, AggFn::DoubleDoubleSum) {
                    next.map(|t| Bson::Array(vec![t]))
                } else {
                    next
                }
            }
            AggFn::First => match state {
                Some(s) => Some(s),
                None => v,
            },
            AggFn::Last => v.or(state),
            AggFn::Min | AggFn::Max => match (state, v) {
                (s, None) => s,
                (None, v) => v,
                (s, v) => {
                    let o = compare(&v, &s);
                    let better = if matches!(self, AggFn::Min) {
                        o == Ordering::Less
                    } else {
                        o == Ordering::Greater
                    };
                    if better {
                        v
                    } else {
                        s
                    }
                }
            },
            AggFn::AddToSetUncapped => {
                let mut items = match state {
                    Some(Bson::Array(a)) => a,
                    _ => vec![],
                };
                if let Some(x) = v {
                    if !items
                        .iter()
                        .any(|y| secantus_core::order::cmp(y, &x) == Ordering::Equal)
                    {
                        items.push(x);
                    }
                }
                Some(Bson::Array(items))
            }
            AggFn::AddToArray(_) | AggFn::AddToSet(_) => {
                let mut items = match state {
                    Some(Bson::Array(mut s)) => match s.first_mut() {
                        Some(Bson::Array(a)) => std::mem::take(a),
                        _ => vec![],
                    },
                    _ => vec![],
                };
                if let Some(x) = v {
                    let set = matches!(self, AggFn::AddToSet(_));
                    if !set
                        || !items
                            .iter()
                            .any(|y| secantus_core::order::cmp(y, &x) == Ordering::Equal)
                    {
                        items.push(x);
                    }
                }
                let size = items.len() as i64;
                Some(Bson::Array(vec![Bson::Array(items), Bson::Int64(size)]))
            }
        }
    }
}

/// One accumulator: `slot = fn(arg)`.
#[derive(Clone, Debug)]
pub(crate) struct Agg {
    pub slot: Slot,
    pub func: AggFn,
    pub arg: Option<Expr>,
    /// The spill slot and merging expression `HashAggStage` prints for it.
    pub spill: Slot,
    pub merging: Expr,
}

/// An SBE stage tree.
#[derive(Clone, Debug)]
pub(crate) enum Stage {
    /// `ScanStage` over a collection: the record and record id slots, and
    /// the top-level fields it reads into slots.
    Scan {
        node: u32,
        record: Slot,
        record_id: Slot,
        fields: Vec<(String, Slot)>,
        uuid: String,
    },
    /// `FilterStage`: the rows its predicate holds of.
    Filter {
        node: u32,
        predicate: Expr,
        input: Box<Stage>,
    },
    /// `LimitSkipStage`: each amount and the slot its constant is in.
    /// Printed `limit` when it only limits, else `limitskip`.
    LimitSkip {
        node: u32,
        limit: Option<(Slot, i64)>,
        skip: Option<(Slot, i64)>,
        input: Box<Stage>,
    },
    /// `ProjectStage`.
    Project {
        node: u32,
        binds: Vec<(Slot, Expr)>,
        input: Box<Stage>,
    },
    /// `SimpleIndexScanStage` (`ixseek`) over one interval of a
    /// single-field index, as a `COUNT_SCAN` builds it: it reads no key into
    /// a slot, only each entry's record id. `low` / `high` are the seek
    /// KeyStrings with their discriminators; `path` and `descending` are the
    /// index's key field, a top-level one.
    IxSeek {
        node: u32,
        record_id: Slot,
        low: Vec<u8>,
        high: Vec<u8>,
        path: String,
        descending: bool,
        index: String,
        uuid: String,
        /// Under a fetch (RM-695): the key, snapshot id and index ident
        /// slots it also reads each entry into.
        fetch: Option<(Slot, Slot, Slot)>,
    },
    /// `ScanStage` as a `seek` of the record whose id is in
    /// `seek_record_id`, checked against the index entry (snapshot, ident,
    /// key and key pattern slots), reading top-level `fields`: the inner
    /// side of the `nlj` a `FETCH` over an index scan builds (RM-695).
    Seek {
        node: u32,
        seek_record_id: Slot,
        record: Slot,
        record_id: Slot,
        snapshot: Slot,
        ident: Slot,
        key: Slot,
        key_pattern: Slot,
        fields: Vec<(String, Slot)>,
        uuid: String,
    },
    /// `SortStage`: the key slots and each one's direction (`true`:
    /// descending), the slots it outputs, and a top-k limit's slot and
    /// amount.
    Sort {
        node: u32,
        keys: Vec<(Slot, bool)>,
        outputs: Vec<Slot>,
        limit: Option<(Slot, i64)>,
        input: Box<Stage>,
    },
    /// `HashAggStage`.
    Group {
        node: u32,
        keys: Vec<Slot>,
        aggs: Vec<Agg>,
        input: Box<Stage>,
    },
    /// `CoScanStage`: one empty row per pull, never at EOF.
    Coscan { node: u32 },
    /// `LimitSkipStage` with a literal limit (`limit 1ll`).
    LimitLit {
        node: u32,
        limit: i64,
        input: Box<Stage>,
    },
    /// `UnwindStage`: each element of the array in `input_slot` into
    /// `out`, its position into `index`; a value that is no array, or an
    /// empty one, passes once when `preserve`.
    Unwind {
        node: u32,
        input_slot: Slot,
        out: Slot,
        index: Slot,
        preserve: bool,
        input: Box<Stage>,
    },
    /// `BranchStage`: `then_` where the predicate holds of the correlated
    /// row, else `else_`; each branch's output slots become `outputs`.
    Branch {
        node: u32,
        predicate: Expr,
        then_slots: Vec<Slot>,
        else_slots: Vec<Slot>,
        outputs: Vec<Slot>,
        then_: Box<Stage>,
        else_: Box<Stage>,
    },
    /// `UnionStage`: each input's rows in turn, an input's slots read into
    /// `outputs`.
    Union {
        node: u32,
        inputs: Vec<(Vec<Slot>, Stage)>,
        outputs: Vec<Slot>,
    },
    /// `LoopJoinStage` (`nlj inner`): the inner side runs once per outer
    /// row, reading `correlated` from it.
    Nlj {
        node: u32,
        projects: Vec<Slot>,
        correlated: Vec<Slot>,
        outer: Box<Stage>,
        inner: Box<Stage>,
    },
    /// `HashLookupStage`: `inner` yields each foreign record (`inner_value`)
    /// with its set of join keys (`inner_key`); each `outer` row, with its
    /// own keys in `outer_key`, gets the array of the records it matches in
    /// `out` (`addToArray`).
    HashLookup {
        node: u32,
        outer_key: Slot,
        inner_key: Slot,
        inner_value: Slot,
        out: Slot,
        outer: Box<Stage>,
        inner: Box<Stage>,
        /// The foreign collection's documents in record order.
        foreign: Vec<Document>,
    },
}

/// What running a stage counted (`CommonStats` and the stage's own).
#[derive(Clone, Debug, Default)]
pub(crate) struct Counted {
    pub returned: i64,
    pub reads: i64,
    /// A filter's `numTested`.
    pub tested: i64,
    /// An index scan's `keysExamined` and `seeks`.
    pub keys: i64,
    pub seeks: i64,
    /// The stage was asked for a row after its last one.
    pub eof: bool,
    pub input: Option<Box<Counted>>,
    /// The counts of a stage with several inputs, or of one run again for
    /// each row of the stage it is correlated with (`corr.rs`), in order.
    pub kids: Vec<Counted>,
    /// How often the stage was opened and closed, where it is not once.
    pub opens: Option<i64>,
    pub closes: Option<i64>,
    /// A `nlj`'s inner side opens and closes; a `branch`'s opens of each
    /// side.
    pub inner_opens: i64,
    pub inner_closes: i64,
    pub then_opens: i64,
    pub else_opens: i64,
    /// A `branch`'s side that is open now, which stays open while it is
    /// re-opened on the same side.
    pub side: Option<usize>,
}

pub(super) type Row = HashMap<Slot, Value>;

/// A group key: the key slots' values under BSON comparison.
#[derive(Clone, Debug)]
struct Key(Vec<Value>);

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Key {}
impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        for (a, b) in self.0.iter().zip(&other.0) {
            match compare(a, b) {
                Ordering::Equal => continue,
                o => return o,
            }
        }
        self.0.len().cmp(&other.0.len())
    }
}

fn eval(e: &Expr, row: &Row) -> Value {
    let slots = |s: Slot| row.get(&s).cloned().flatten();
    let mut env = Env {
        slots: &slots,
        locals: Vec::new(),
    };
    e.eval(&mut env)
}

impl Stage {
    pub fn node(&self) -> u32 {
        match self {
            Stage::Scan { node, .. }
            | Stage::Filter { node, .. }
            | Stage::LimitSkip { node, .. }
            | Stage::Project { node, .. }
            | Stage::IxSeek { node, .. }
            | Stage::Sort { node, .. }
            | Stage::Coscan { node }
            | Stage::LimitLit { node, .. }
            | Stage::Unwind { node, .. }
            | Stage::Branch { node, .. }
            | Stage::Union { node, .. }
            | Stage::Nlj { node, .. }
            | Stage::Seek { node, .. }
            | Stage::HashLookup { node, .. }
            | Stage::Group { node, .. } => *node,
        }
    }

    /// Run the stage over `docs` (the collection in record order), as a
    /// consumer that stops after `demand` rows pulls it: the rows it returns
    /// and what each stage under it counted. A stage stopped by its demand
    /// was never asked past its last row, so it is not at EOF.
    pub fn run(&self, docs: &[Document], demand: Option<usize>) -> (Vec<Row>, Counted) {
        match self {
            Stage::Scan {
                record,
                record_id,
                fields,
                ..
            } => {
                let take = demand.map_or(docs.len(), |d| d.min(docs.len()));
                let eof = demand.is_none_or(|d| docs.len() < d);
                let rows: Vec<Row> = docs[..take]
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        let mut row = Row::new();
                        row.insert(*record, Some(Bson::Document(d.clone())));
                        row.insert(*record_id, Some(Bson::Int64(i as i64 + 1)));
                        for (name, slot) in fields {
                            row.insert(*slot, d.get(name).cloned());
                        }
                        row
                    })
                    .collect();
                let n = rows.len() as i64;
                (
                    rows,
                    Counted {
                        returned: n,
                        reads: n,
                        eof,
                        input: None,
                        ..Counted::default()
                    },
                )
            }
            Stage::IxSeek {
                record_id,
                low,
                high,
                path,
                descending,
                ..
            } => {
                // The index holds one entry per document, its key the
                // KeyString of the field's value (null when missing), in
                // key and then record id order. The seek lands on the
                // first entry after `low` and reads on until one is past
                // `high` or the index ends: one read more than it returns.
                let mut entries: Vec<(Vec<u8>, i64)> = Vec::new();
                for (i, d) in docs.iter().enumerate() {
                    let v = d.get(path).cloned().unwrap_or(Bson::Null);
                    let Ok(key) = crate::key_string::encode_key(&[(&v, *descending)], None) else {
                        continue;
                    };
                    entries.push((key, i as i64 + 1));
                }
                entries.sort();
                let found: Vec<i64> = entries
                    .into_iter()
                    .filter(|(k, _)| {
                        k.as_slice() > low.as_slice() && k.as_slice() < high.as_slice()
                    })
                    .map(|(_, id)| id)
                    .collect();
                let take = demand.map_or(found.len(), |d| d.min(found.len()));
                let eof = demand.is_none_or(|d| found.len() < d);
                let rows: Vec<Row> = found[..take]
                    .iter()
                    .map(|id| {
                        let mut row = Row::new();
                        row.insert(*record_id, Some(Bson::Int64(*id)));
                        row
                    })
                    .collect();
                let n = rows.len() as i64;
                (
                    rows,
                    Counted {
                        returned: n,
                        reads: n + i64::from(eof),
                        keys: n,
                        seeks: 1,
                        eof,
                        ..Counted::default()
                    },
                )
            }
            Stage::Filter {
                predicate, input, ..
            } => {
                // The filter pulls a row at a time: run what is under it
                // in full, then count only as far as it read.
                let (rows, mut below) = input.run(docs, None);
                let mut out = Vec::new();
                let mut tested = 0;
                for row in rows {
                    if demand.is_some_and(|d| out.len() >= d) {
                        break;
                    }
                    tested += 1;
                    if eval(predicate, &row) == Some(Bson::Boolean(true)) {
                        out.push(row);
                    }
                }
                let exhausted = tested == below.returned;
                let eof = exhausted && demand.is_none_or(|d| out.len() < d);
                below.trim(tested, eof);
                let n = out.len() as i64;
                (
                    out,
                    Counted {
                        returned: n,
                        tested,
                        eof,
                        input: Some(Box::new(below)),
                        ..Counted::default()
                    },
                )
            }
            Stage::LimitSkip {
                limit, skip, input, ..
            } => {
                let skip = skip.map_or(0, |(_, n)| n.max(0) as usize);
                let limit = limit.map(|(_, n)| n.max(0) as usize);
                let mine = match (limit, demand) {
                    (Some(l), Some(d)) => Some(l.min(d)),
                    (l, d) => l.or(d),
                };
                let (rows, below) = input.run(docs, mine.map(|m| m + skip));
                let out: Vec<Row> = rows
                    .into_iter()
                    .skip(skip)
                    .take(mine.unwrap_or(usize::MAX))
                    .collect();
                // At its limit it answers EOF without asking what is under it.
                let eof = below.eof || limit.is_some_and(|l| out.len() >= l);
                let n = out.len() as i64;
                (
                    out,
                    Counted {
                        returned: n,
                        eof: eof && demand.is_none_or(|d| (n as usize) < d),
                        input: Some(Box::new(below)),
                        ..Counted::default()
                    },
                )
            }
            Stage::Project { binds, input, .. } => {
                let (mut rows, below) = input.run(docs, demand);
                for row in &mut rows {
                    for (slot, e) in binds {
                        let v = eval(e, row);
                        row.insert(*slot, v);
                    }
                }
                let n = rows.len() as i64;
                (
                    rows,
                    Counted {
                        returned: n,
                        eof: below.eof,
                        input: Some(Box::new(below)),
                        ..Counted::default()
                    },
                )
            }
            Stage::Sort {
                keys, limit, input, ..
            } => {
                // A blocking stage: it reads everything under it, then
                // returns its rows in key order (ties in input order).
                let (mut rows, below) = input.run(docs, None);
                rows.sort_by(|a, b| {
                    for (slot, desc) in keys {
                        let x = a.get(slot).cloned().flatten();
                        let y = b.get(slot).cloned().flatten();
                        let o = compare(&x, &y);
                        let o = if *desc { o.reverse() } else { o };
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    Ordering::Equal
                });
                let cap = limit.map_or(usize::MAX, |(_, n)| n.max(0) as usize);
                rows.truncate(cap.min(demand.unwrap_or(usize::MAX)));
                let n = rows.len() as i64;
                (
                    rows,
                    Counted {
                        returned: n,
                        eof: true,
                        input: Some(Box::new(below)),
                        ..Counted::default()
                    },
                )
            }
            Stage::Group {
                keys, aggs, input, ..
            } => {
                let (rows, below) = input.run(docs, None);
                // `HashAggStage` keeps its groups in a hash table: one per
                // distinct key, none over no rows.
                let mut table: BTreeMap<Key, (usize, Vec<Value>)> = BTreeMap::new();
                for row in &rows {
                    let key = Key(keys.iter().map(|k| row.get(k).cloned().flatten()).collect());
                    let next = table.len();
                    let entry = table
                        .entry(key)
                        .or_insert_with(|| (next, vec![None; aggs.len()]));
                    for (state, agg) in entry.1.iter_mut().zip(aggs) {
                        let v = agg.arg.as_ref().and_then(|a| eval(a, row));
                        *state = agg.func.step(state.take(), v);
                    }
                }
                let mut groups: Vec<(Key, (usize, Vec<Value>))> = table.into_iter().collect();
                groups.sort_by_key(|(_, (seen, _))| *seen);
                let out: Vec<Row> = groups
                    .into_iter()
                    .map(|(key, (_, states))| {
                        let mut row = Row::new();
                        for (slot, v) in keys.iter().zip(key.0) {
                            row.insert(*slot, v);
                        }
                        for (agg, v) in aggs.iter().zip(states) {
                            row.insert(agg.slot, v);
                        }
                        row
                    })
                    .collect();
                let n = out.len() as i64;
                (
                    out,
                    Counted {
                        returned: n,
                        eof: true,
                        input: Some(Box::new(below)),
                        ..Counted::default()
                    },
                )
            }
            Stage::HashLookup { .. } => corr::hash_lookup(self, docs, demand),
            Stage::Nlj { outer, inner, .. } if fetch_of(inner).is_some() => {
                fetch_join(outer, inner, docs, demand)
            }
            Stage::Seek { .. } => unreachable!("a seek runs under its nlj"),
            Stage::Coscan { .. }
            | Stage::LimitLit { .. }
            | Stage::Unwind { .. }
            | Stage::Branch { .. }
            | Stage::Union { .. }
            | Stage::Nlj { .. } => {
                // Only run inside a `hash_lookup`, once per outer row.
                let mut counted = Counted::default();
                let rows = corr::exec(self, &Row::new(), &[], &mut counted);
                (rows, counted)
            }
        }
    }
}

/// The `seek` under the `limit 1` an `nlj`'s inner side is when it
/// fetches the documents of an index seek (RM-695).
fn fetch_of(inner: &Stage) -> Option<&Stage> {
    let Stage::LimitLit { input, .. } = inner else {
        return None;
    };
    matches!(**input, Stage::Seek { .. }).then_some(&**input)
}

/// An `nlj` of an index seek and a `limit 1` `seek` of each entry's
/// document: each outer entry opens the inner side once, whose limit
/// returns the one document the seek reads and stops it before it is
/// asked past it; both are closed once, if opened at all.
fn fetch_join(
    outer: &Stage,
    inner: &Stage,
    docs: &[Document],
    demand: Option<usize>,
) -> (Vec<Row>, Counted) {
    let (keys, below) = outer.run(docs, None);
    let Some(Stage::Seek {
        seek_record_id,
        record,
        record_id,
        fields,
        ..
    }) = fetch_of(inner)
    else {
        unreachable!("checked by the caller")
    };
    let mut out = Vec::new();
    for mut row in keys {
        if demand.is_some_and(|d| out.len() >= d) {
            break;
        }
        let Some(Some(Bson::Int64(id))) = row.get(seek_record_id).cloned() else {
            continue;
        };
        let Some(d) = usize::try_from(id - 1).ok().and_then(|i| docs.get(i)) else {
            continue;
        };
        row.insert(*record, Some(Bson::Document(d.clone())));
        row.insert(*record_id, Some(Bson::Int64(id)));
        for (name, slot) in fields {
            row.insert(*slot, d.get(name).cloned());
        }
        out.push(row);
    }
    let n = out.len() as i64;
    let closes = n.min(1);
    let seek = Counted {
        returned: n,
        reads: n,
        opens: Some(n),
        closes: Some(closes),
        ..Counted::default()
    };
    let limit = Counted {
        returned: n,
        opens: Some(n),
        closes: Some(closes),
        eof: n > 0,
        input: Some(Box::new(seek)),
        ..Counted::default()
    };
    let eof = below.eof && demand.is_none_or(|d| out.len() < d);
    (
        out,
        Counted {
            returned: n,
            eof,
            kids: vec![below, limit],
            inner_opens: n,
            inner_closes: closes,
            ..Counted::default()
        },
    )
}

impl Counted {
    /// What a stage that was never opened counted: nothing, down to its
    /// inputs.
    pub fn blank(stage: &Stage) -> Counted {
        Counted {
            opens: Some(0),
            closes: Some(0),
            kids: stage.inputs().into_iter().map(Counted::blank).collect(),
            ..Counted::default()
        }
    }

    /// What the stages under this one counted, in the order its stage
    /// lists its inputs.
    pub fn children(&self) -> Vec<&Counted> {
        if self.kids.is_empty() {
            self.input.as_deref().into_iter().collect()
        } else {
            self.kids.iter().collect()
        }
    }

    /// What a scan counted when its consumer read only its first `n` rows:
    /// it produced those, and is at EOF only if it was asked past them.
    fn trim(&mut self, n: i64, eof: bool) {
        self.returned = n;
        if self.reads > n {
            self.reads = n;
        }
        self.eof = eof;
    }
}

impl Stage {
    /// The name `executionStages` gives the stage.
    pub fn name(&self) -> &'static str {
        match self {
            Stage::Scan { .. } => "scan",
            Stage::IxSeek { .. } => "ixseek",
            Stage::Filter { .. } => "filter",
            Stage::LimitSkip { skip: None, .. } => "limit",
            Stage::LimitSkip { .. } => "limitskip",
            Stage::Project { .. } => "project",
            Stage::Sort { .. } => "sort",
            Stage::Group { .. } => "group",
            Stage::Coscan { .. } => "coscan",
            Stage::LimitLit { .. } => "limit",
            Stage::Unwind { .. } => "unwind",
            Stage::Branch { .. } => "branch",
            Stage::Union { .. } => "union",
            Stage::Nlj { .. } => "nlj",
            Stage::Seek { .. } => "seek",
            Stage::HashLookup { .. } => "hash_lookup",
        }
    }

    /// The stage's inputs, in the order its statistics list them.
    pub fn inputs(&self) -> Vec<&Stage> {
        match self {
            Stage::Branch { then_, else_, .. } => vec![then_, else_],
            Stage::Union { inputs, .. } => inputs.iter().map(|(_, s)| s).collect(),
            Stage::Nlj { outer, inner, .. } | Stage::HashLookup { outer, inner, .. } => {
                vec![outer, inner]
            }
            other => other.input().into_iter().collect(),
        }
    }

    pub fn input(&self) -> Option<&Stage> {
        match self {
            Stage::Scan { .. }
            | Stage::IxSeek { .. }
            | Stage::Coscan { .. }
            | Stage::Branch { .. }
            | Stage::Union { .. }
            | Stage::Nlj { .. }
            | Stage::Seek { .. }
            | Stage::HashLookup { .. } => None,
            Stage::LimitLit { input, .. } | Stage::Unwind { input, .. } => Some(input),
            Stage::Filter { input, .. }
            | Stage::LimitSkip { input, .. }
            | Stage::Project { input, .. }
            | Stage::Sort { input, .. }
            | Stage::Group { input, .. } => Some(input),
        }
    }
}

impl Agg {
    /// The accumulator as `group` prints it: `fn(arg)`.
    pub fn call(&self) -> Expr {
        let mut args: Vec<Expr> = self.arg.iter().cloned().collect();
        if let AggFn::AddToArray(cap) | AggFn::AddToSet(cap) = self.func {
            args.push(Expr::Const(Some(Bson::Int32(cap))));
        }
        Expr::Func(self.func.name(), args)
    }
}

#[cfg(test)]
mod tests {
    use super::super::expr::{BinOp, Expr};
    use super::Stage;
    use bson::{doc, Bson};

    /// `[{$match: {a: {$gt: 1}}}, {$limit: 2}, ...]` over `a` 0..5: the
    /// filter stops after the second match, so it tested four rows and the
    /// scan read four, neither at EOF; the limit is (mongod 8.2.11,
    /// measured 2026-11-08).
    #[test]
    fn a_limit_stops_what_is_under_it() {
        let docs: Vec<_> = (0..5).map(|i| doc! { "a": i }).collect();
        let scan = Stage::Scan {
            node: 1,
            record: 1,
            record_id: 2,
            fields: vec![("a".into(), 3)],
            uuid: String::new(),
        };
        let predicate = Expr::fill_empty(
            Expr::bin(BinOp::Gt, Expr::Slot(3), Expr::Const(Some(Bson::Int32(1)))),
            Expr::Const(Some(Bson::Boolean(false))),
        );
        let filter = Stage::Filter {
            node: 1,
            predicate,
            input: Box::new(scan),
        };
        let limit = Stage::LimitSkip {
            node: 2,
            limit: Some((4, 2)),
            skip: None,
            input: Box::new(filter),
        };
        let (rows, top) = limit.run(&docs, None);
        assert_eq!(rows.len(), 2);
        assert!(top.eof);
        let f = top.input.as_deref().unwrap();
        assert_eq!((f.returned, f.tested, f.eof), (2, 4, false));
        let s = f.input.as_deref().unwrap();
        assert_eq!((s.returned, s.reads, s.eof), (4, 4, false));
    }
}
