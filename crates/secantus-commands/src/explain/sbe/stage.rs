//! SBE stages (`exec/sbe/stages/`): `scan`, `filter`, `limit` /
//! `limitskip`, `project` and `group` (`HashAggStage`), run over a collection's documents with the statistics
//! each one keeps (`PlanStageStats` / `SpecificStats`), and printed as
//! mongod's `debugPrint` prints them.

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
        }
    }

    /// The accumulator's state after one more row whose argument is `v`.
    fn step(&self, state: Value, v: Value) -> Value {
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
    /// `HashAggStage`.
    Group {
        node: u32,
        keys: Vec<Slot>,
        aggs: Vec<Agg>,
        input: Box<Stage>,
    },
}

/// What running a stage counted (`CommonStats` and the stage's own).
#[derive(Clone, Debug, Default)]
pub(crate) struct Counted {
    pub returned: i64,
    pub reads: i64,
    /// A filter's `numTested`.
    pub tested: i64,
    /// The stage was asked for a row after its last one.
    pub eof: bool,
    pub input: Option<Box<Counted>>,
}

type Row = HashMap<Slot, Value>;

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
        }
    }
}

impl Counted {
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
            Stage::Filter { .. } => "filter",
            Stage::LimitSkip { skip: None, .. } => "limit",
            Stage::LimitSkip { .. } => "limitskip",
            Stage::Project { .. } => "project",
            Stage::Group { .. } => "group",
        }
    }

    pub fn input(&self) -> Option<&Stage> {
        match self {
            Stage::Scan { .. } => None,
            Stage::Filter { input, .. }
            | Stage::LimitSkip { input, .. }
            | Stage::Project { input, .. }
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
