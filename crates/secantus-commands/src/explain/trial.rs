//! mongod's trial of candidate plans, counted.
//!
//! When more than one plan can answer a query, mongod runs them side by
//! side over the collection, one unit of work each in turn, until one has
//! handed over the documents the trial asks for or has run out. Each plan
//! is then scored (`plan_ranker.cpp`, measured on 8.2.11 with
//! `allPlansExecution`):
//!
//! ```text
//! 1 + advanced / works + (1 if it ran out) + ε for each of: no FETCH,
//! no SORT, no index intersection; ε = min(1 / (10 · works), 1e-4)
//! ```
//!
//! and the best kept. Here each candidate is stepped with the stage rules
//! of `PlanStage::work`, over the documents the caller holds, and what each
//! stage counted during the trial is kept for `allPlansExecution`.

use super::exec::{entries, fixed_extra, merge_head, Entry, Env, Head, Scan, Work};
use super::node::{num, Node, Stats};
use bson::Document;
use secantus_core::heap::Heap;
use std::collections::HashMap;
use std::collections::{BTreeSet, VecDeque};

/// Each index's entries, by name and direction.
type Entries = HashMap<(String, bool), Vec<Entry>>;

/// A stage being stepped one work at a time.
struct Stage<'a> {
    node: &'a Node,
    inputs: Vec<Stage<'a>>,
    scan: Option<Scan>,
    pos: usize,
    seen: BTreeSet<usize>,
    /// A DISTINCT_SCAN: the values it has handed over.
    prefixes: BTreeSet<Vec<Vec<u8>>>,
    /// A sort: what its input returned, and once that ended, the output.
    pending: Vec<usize>,
    buffer: Option<Vec<usize>>,
    /// A SORT_MERGE's children without a head; an AND_SORTED's children
    /// still to reach its target.
    queue: Option<VecDeque<usize>>,
    heads: Heap<Head>,
    /// An AND_SORTED's target: the child that named it, and the RecordId.
    target: Option<(usize, usize)>,
    count: i64,
    /// The stage has reported that it is done.
    ended: bool,
    returned: i64,
    docs: i64,
    extra: Document,
}

impl<'a> Stage<'a> {
    /// `None`: a stage the trial does not step.
    fn new(node: &'a Node, env: &Env, cache: &mut Entries) -> Option<Stage<'a>> {
        let inputs: Vec<&Node> = match node {
            Node::IxScan(_) | Node::CollScan { .. } | Node::DistinctScan(..) => vec![],
            Node::Fetch { input, .. }
            | Node::Sort { input, .. }
            | Node::Projection { input, .. }
            | Node::Skip(_, input)
            | Node::Limit(_, input)
            | Node::ReturnKey(input)
            | Node::Subplan(input) => vec![input],
            Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
                inputs.iter().collect()
            }
            _ => return None,
        };
        let scan = match node {
            Node::IxScan(ix) | Node::DistinctScan(ix, _, _) => {
                let all = cache
                    .entry((ix.index.name.clone(), ix.backward))
                    .or_insert_with(|| entries(&ix.index, ix.backward, env));
                Some(Scan::over(ix, all))
            }
            _ => None,
        };
        let mut extra = Document::new();
        if let Node::AndSorted(inputs) = node {
            for i in 0..inputs.len() {
                extra.insert(format!("failedAnd_{i}"), 0_i32);
            }
        }
        Some(Stage {
            node,
            inputs: inputs
                .into_iter()
                .map(|n| Stage::new(n, env, cache))
                .collect::<Option<_>>()?,
            scan,
            pos: 0,
            seen: BTreeSet::new(),
            prefixes: BTreeSet::new(),
            pending: Vec::new(),
            buffer: None,
            queue: None,
            heads: Heap::new(),
            target: None,
            count: 0,
            ended: false,
            returned: 0,
            docs: 0,
            extra,
        })
    }

    fn work(&mut self, env: &Env) -> Work {
        let out = self.step(env);
        match out {
            Work::Advanced(_) => self.returned += 1,
            Work::Eof => self.ended = true,
            Work::NeedTime => {}
        }
        out
    }

    /// `PlanStage::isEOF()`, which the score reads: a stage that passes
    /// documents along is done when its input is, a limit when it has
    /// handed over its documents, a merge when no child has any left.
    fn is_eof(&self) -> bool {
        match self.node {
            Node::Fetch { .. }
            | Node::Projection { .. }
            | Node::Skip(..)
            | Node::ReturnKey(_)
            | Node::Subplan(_) => self.ended || self.inputs[0].is_eof(),
            Node::Limit(n, _) => self.ended || self.count >= *n || self.inputs[0].is_eof(),
            Node::Or(_) => self.ended || self.pos >= self.inputs.len(),
            Node::SortMerge { .. } => {
                self.ended
                    || (self.queue.as_ref().is_some_and(VecDeque::is_empty)
                        && self.heads.is_empty())
            }
            _ => self.ended,
        }
    }

    fn step(&mut self, env: &Env) -> Work {
        match self.node {
            Node::IxScan(_) => match self.scan.as_mut() {
                Some(scan) => scan.work(env),
                None => Work::Eof,
            },
            Node::DistinctScan(_, width, _) => {
                let (Some(scan), prefixes) = (self.scan.as_mut(), &mut self.prefixes) else {
                    return Work::Eof;
                };
                match scan.next_distinct(*width, prefixes) {
                    Some(row) => Work::Advanced(row),
                    None => {
                        self.count = 1;
                        Work::Eof
                    }
                }
            }
            Node::CollScan { filter, backward } => {
                if self.pos >= env.docs.len() {
                    return Work::Eof;
                }
                let row = if *backward {
                    env.docs.len() - 1 - self.pos
                } else {
                    self.pos
                };
                self.pos += 1;
                self.docs += 1;
                if env.matches(row, filter) {
                    Work::Advanced(row)
                } else {
                    Work::NeedTime
                }
            }
            Node::Fetch { filter, .. } => match self.inputs[0].work(env) {
                Work::Advanced(row) => {
                    self.docs += 1;
                    if env.matches(row, filter) {
                        Work::Advanced(row)
                    } else {
                        Work::NeedTime
                    }
                }
                other => other,
            },
            Node::Projection { .. } | Node::ReturnKey(_) | Node::Subplan(_) => {
                self.inputs[0].work(env)
            }
            Node::Skip(n, _) => match self.inputs[0].work(env) {
                Work::Advanced(_) if self.count < *n => {
                    self.count += 1;
                    Work::NeedTime
                }
                other => other,
            },
            Node::Limit(n, _) => {
                if self.count >= *n {
                    return Work::Eof;
                }
                let out = self.inputs[0].work(env);
                if matches!(out, Work::Advanced(_)) {
                    self.count += 1;
                }
                out
            }
            // A blocking sort takes one work per work of its input, the one
            // that ends it included, then one per document it hands over,
            // then one to report that it is done. Which order it hands them
            // over in does not change the counts.
            Node::Sort { limit, .. } => {
                if self.buffer.is_none() {
                    match self.inputs[0].work(env) {
                        Work::Advanced(row) => self.pending.push(row),
                        Work::NeedTime => {}
                        Work::Eof => {
                            let mut all = std::mem::take(&mut self.pending);
                            if let Some(n) = limit {
                                all.truncate((*n).max(0) as usize);
                            }
                            self.buffer = Some(all);
                        }
                    }
                    return Work::NeedTime;
                }
                let next = self.buffer.as_ref().and_then(|b| b.get(self.pos).copied());
                self.pos += 1;
                next.map_or(Work::Eof, Work::Advanced)
            }
            Node::Or(_) => {
                if self.pos >= self.inputs.len() {
                    return Work::Eof;
                }
                match self.inputs[self.pos].work(env) {
                    Work::Advanced(row) if !self.seen.insert(row) => Work::NeedTime,
                    Work::Eof => {
                        self.pos += 1;
                        if self.pos < self.inputs.len() {
                            Work::NeedTime
                        } else {
                            Work::Eof
                        }
                    }
                    other => other,
                }
            }
            // `AndSortedStage::doWork`: the first child names a target; each
            // other child is worked until it reaches it. One that passes it
            // names a new target, and the old target's child failed.
            Node::AndSorted(_) => {
                let Some((node, target)) = self.target else {
                    return match self.inputs[0].work(env) {
                        Work::Advanced(row) => {
                            self.target = Some((0, row));
                            self.queue = Some((1..self.inputs.len()).collect());
                            Work::NeedTime
                        }
                        other => other,
                    };
                };
                let mut queue = self.queue.take().unwrap_or_default();
                let Some(&child) = queue.front() else {
                    self.target = None;
                    return Work::Advanced(target);
                };
                let out = match self.inputs[child].work(env) {
                    Work::Advanced(row) if row == target => {
                        queue.pop_front();
                        if queue.is_empty() {
                            self.target = None;
                            Work::Advanced(target)
                        } else {
                            Work::NeedTime
                        }
                    }
                    Work::Advanced(row) if row > target => {
                        let key = format!("failedAnd_{node}");
                        let n = self
                            .extra
                            .get(&key)
                            .and_then(crate::util::as_i64)
                            .unwrap_or(0);
                        self.extra.insert(key, num(n + 1));
                        self.target = Some((child, row));
                        queue = (0..self.inputs.len()).filter(|&c| c != child).collect();
                        Work::NeedTime
                    }
                    Work::Advanced(_) | Work::NeedTime => Work::NeedTime,
                    Work::Eof => Work::Eof,
                };
                self.queue = Some(queue);
                out
            }
            // `MergeSortStage::doWork`: a child with no head is worked; else
            // the least head is handed over and its child goes to the back
            // of the line.
            Node::SortMerge { pattern, .. } => {
                let mut queue = self
                    .queue
                    .take()
                    .unwrap_or_else(|| (0..self.inputs.len()).collect());
                let out = if queue.is_empty() && self.heads.is_empty() {
                    Work::Eof
                } else if let Some(&child) = queue.front() {
                    match self.inputs[child].work(env) {
                        Work::Advanced(row) => {
                            if self.seen.insert(row) {
                                queue.pop_front();
                                self.heads.push(merge_head(pattern, env, row, child));
                            }
                        }
                        Work::Eof => {
                            queue.pop_front();
                        }
                        Work::NeedTime => {}
                    }
                    Work::NeedTime
                } else {
                    match self.heads.pop() {
                        Some(top) => {
                            queue.push_back(top.child);
                            Work::Advanced(top.row)
                        }
                        None => Work::Eof,
                    }
                };
                self.queue = Some(queue);
                out
            }
            _ => Work::Eof,
        }
    }

    /// What the stage and those under it counted, shaped as the plan.
    fn stats(&self) -> Stats {
        let mut extra = self.extra.clone();
        fixed_extra(self.node, &mut extra);
        let keys = match (self.node, &self.scan) {
            // The key that ends the scan is examined in the work that ends it.
            (Node::DistinctScan(..), Some(scan)) => {
                self.prefixes.len() as i64 + i64::from(self.count > 0 && scan.key_beyond())
            }
            (_, Some(scan)) => scan.keys(),
            _ => 0,
        };
        let docs = super::exec::fetched(self.node, self.returned, &mut extra).unwrap_or(self.docs);
        Stats {
            n_returned: self.returned,
            keys,
            docs,
            extra,
            children: self.inputs.iter().map(Stage::stats).collect(),
        }
    }
}

/// Whether the plan holds a stage of the kind `pick` names.
fn holds(node: &Node, pick: &dyn Fn(&Node) -> bool) -> bool {
    if pick(node) {
        return true;
    }
    match node {
        Node::Fetch { input, .. }
        | Node::Sort { input, .. }
        | Node::Projection { input, .. }
        | Node::Skip(_, input)
        | Node::Limit(_, input)
        | Node::ReturnKey(input)
        | Node::Subplan(input) => holds(input, pick),
        Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
            inputs.iter().any(|n| holds(n, pick))
        }
        _ => false,
    }
}

/// Each plan's score after mongod's trial over `env`, in the order given,
/// with what its stages counted during the trial. `need`: the documents a
/// plan hands over to end the trial. `None`: a plan holds a stage this
/// does not step.
pub(crate) fn run(plans: &[Node], env: &Env, need: i64) -> Option<Vec<(f64, Stats)>> {
    // An index's entries, built once for every scan of it.
    let mut cache = Entries::new();
    let mut stages: Vec<Stage> = plans
        .iter()
        .map(|p| Stage::new(p, env, &mut cache))
        .collect::<Option<_>>()?;
    let max_works = 10_000i64.max((env.docs.len() as f64 * 0.3) as i64);
    let mut works = 0i64;
    while works < max_works {
        works += 1;
        let mut done = false;
        for stage in stages.iter_mut() {
            match stage.work(env) {
                Work::Advanced(_) => done |= stage.returned >= need,
                Work::Eof => done = true,
                Work::NeedTime => {}
            }
        }
        if done {
            break;
        }
    }
    let epsilon = (1.0 / (10.0 * works as f64)).min(1e-4);
    Some(
        plans
            .iter()
            .zip(&stages)
            .map(|(plan, stage)| {
                let fetches = holds(plan, &|n| matches!(n, Node::Fetch { .. }));
                let sorts = holds(plan, &|n| matches!(n, Node::Sort { .. }));
                let intersects = holds(plan, &|n| matches!(n, Node::AndSorted(_)));
                let mut score = 1.0 + stage.returned as f64 / works as f64;
                for absent in [!fetches, !sorts, !intersects] {
                    if absent {
                        score += epsilon;
                    }
                }
                if stage.is_eof() {
                    score += 1.0;
                }
                (score, stage.stats())
            })
            .collect(),
    )
}
