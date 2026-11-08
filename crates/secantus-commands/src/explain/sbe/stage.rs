//! SBE stages (`exec/sbe/stages/`): `scan`, `project` and `group`
//! (`HashAggStage`), run over a collection's documents with the statistics
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
            Stage::Scan { node, .. } | Stage::Project { node, .. } | Stage::Group { node, .. } => {
                *node
            }
        }
    }

    /// Run the stage over `docs` (the collection in record order): the rows
    /// it returns and what each stage under it counted.
    pub fn run(&self, docs: &[Document]) -> (Vec<Row>, Counted) {
        match self {
            Stage::Scan {
                record,
                record_id,
                fields,
                ..
            } => {
                let rows: Vec<Row> = docs
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
                        input: None,
                    },
                )
            }
            Stage::Project { binds, input, .. } => {
                let (mut rows, below) = input.run(docs);
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
                        reads: 0,
                        input: Some(Box::new(below)),
                    },
                )
            }
            Stage::Group {
                keys, aggs, input, ..
            } => {
                let (rows, below) = input.run(docs);
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
                        reads: 0,
                        input: Some(Box::new(below)),
                    },
                )
            }
        }
    }
}

impl Stage {
    /// The name `executionStages` gives the stage.
    pub fn name(&self) -> &'static str {
        match self {
            Stage::Scan { .. } => "scan",
            Stage::Project { .. } => "project",
            Stage::Group { .. } => "group",
        }
    }

    pub fn input(&self) -> Option<&Stage> {
        match self {
            Stage::Scan { .. } => None,
            Stage::Project { input, .. } | Stage::Group { input, .. } => Some(input),
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
