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

use super::bounds::Interval;
use super::exec::{entries, fixed_extra, merge_head, Entry, Env, Head, Scan, Work};
use super::node::{num, Node, Stats, WorkCounts};
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
    /// Calls to `work()`, and those that answered NEED_TIME.
    works: i64,
    need_time: i64,
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
            works: 0,
            need_time: 0,
        })
    }

    fn work(&mut self, env: &Env) -> Work {
        let out = self.step(env);
        self.works += 1;
        match out {
            Work::Advanced(_) => self.returned += 1,
            Work::Eof => self.ended = true,
            Work::NeedTime => self.need_time += 1,
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
            Node::Sort { limit, pattern, .. } => {
                if self.buffer.is_none() {
                    match self.inputs[0].work(env) {
                        Work::Advanced(row) => self.pending.push(row),
                        Work::NeedTime => {}
                        Work::Eof => {
                            let mut all = std::mem::take(&mut self.pending);
                            // What the sort held, counted when it sorts: a
                            // sort the trial does not finish reports none.
                            super::exec::sort_stats(&mut self.extra, pattern, *limit, &all, env);
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
        if matches!(self.node, Node::Sort { .. }) && !extra.contains_key("totalDataSizeSorted") {
            extra.insert("totalDataSizeSorted", 0_i32);
        }
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
        let (seeks, dups_tested, dups_dropped) =
            self.scan.as_ref().map_or((0, 0, 0), Scan::seek_counts);
        Stats {
            n_returned: self.returned,
            keys,
            docs,
            extra,
            children: self.inputs.iter().map(Stage::stats).collect(),
            work: Some(WorkCounts {
                works: self.works,
                advanced: self.returned,
                need_time: self.need_time,
                eof: self.is_eof(),
                seeks,
                dups_tested,
                dups_dropped,
                // mongod yields during a trial only when 10 ms have passed
                // (measured 8.2.11: none in 6,000 calls into three plans),
                // which a count cannot say: none here.
                saved: 0,
                restored: 0,
            }),
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

/// What mongod's trial of several plans found.
pub(crate) struct Outcome {
    /// Each plan's score, in the order the plans were given, the
    /// tie-breaking bonuses included (`$planCacheStats`'
    /// `candidatePlanScores`).
    pub scores: Vec<f64>,
    /// The same before the bonuses (`explain`'s `score`).
    pub base_scores: Vec<f64>,
    /// What each plan's stages counted during the trial.
    pub stats: Vec<Stats>,
    /// The plans best first (`PlanRankingDecision::candidateOrder`).
    pub order: Vec<usize>,
    /// The rounds of work the trial ran: each plan's `works`.
    pub works: i64,
}

/// `kBonusEpsilon`.
const BONUS: f64 = 1e-4;

/// Each plan's score after mongod's trial over `env` (`pickBestPlan` in
/// `plan_ranker_util.h`), with what its stages counted during the trial.
/// `need`: the documents a plan hands over to end the trial. `None`: a plan
/// holds a stage this does not step.
pub(crate) fn run(plans: &[Node], env: &Env, need: i64) -> Option<Outcome> {
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
    let mut scores: Vec<f64> = plans
        .iter()
        .zip(&stages)
        .map(|(plan, stage)| {
            // The tie-breakers' epsilon is set by the plan's advances
            // (`PlanScorer::calculateScore`): 1e-4 for any trial that ends
            // at 101 documents or fewer.
            let advances = stage.returned.max(1) as f64;
            let epsilon = (1.0 / (10.0 * advances)).min(BONUS);
            let bonus = |absent: bool| if absent { epsilon } else { 0.0 };
            let no_fetch = bonus(!holds(plan, &|n| matches!(n, Node::Fetch { .. })));
            let no_sort = bonus(!holds(plan, &|n| matches!(n, Node::Sort { .. })));
            let no_ixisect = bonus(!holds(plan, &|n| matches!(n, Node::AndSorted(_))));
            // Summed in mongod's order, which its doubles' last digit shows.
            let tie_breakers = no_fetch + no_sort + no_ixisect;
            let productivity = stage.returned as f64 / stage.works.max(1) as f64;
            let mut score = 1.0 + productivity + tie_breakers;
            if stage.is_eof() {
                score += 1.0;
            }
            score
        })
        .collect();
    let stats: Vec<Stats> = stages.iter().map(Stage::stats).collect();
    let base_scores = scores.clone();
    let mut order: Vec<usize> = (0..plans.len()).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
    tie_breaking_bonuses(plans, &stats, &mut scores, &order);
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
    Some(Outcome {
        scores,
        base_scores,
        stats,
        order,
        works,
    })
}

/// `addTieBreakingHeuristicsBonuses`: when the best scores tie exactly,
/// the tied plans that examined the fewest documents gain `1e-4`, those
/// whose index bounds win the index prefix heuristic `2e-4`, and a
/// DISTINCT_SCAN `1e-4`.
fn tie_breaking_bonuses(plans: &[Node], stats: &[Stats], scores: &mut [f64], order: &[usize]) {
    let top = scores[order[0]];
    let tied: Vec<usize> = order
        .iter()
        .copied()
        .take_while(|&i| scores[i] == top)
        .collect();
    if tied.len() < 2 {
        return;
    }
    // Per plan: the docs-examined, index-prefix and distinct-scan bonuses,
    // summed in that order (`TieBreakingScores::getTotalBonus`).
    let mut bonus = vec![(0.0, 0.0, 0.0); plans.len()];
    let docs: Vec<i64> = tied.iter().map(|&i| stats[i].totals().1).collect();
    let fewest = docs.iter().copied().min().unwrap_or(0);
    for (&i, &d) in tied.iter().zip(&docs) {
        if d == fewest {
            bonus[i].0 = BONUS;
        }
    }
    let solutions: Vec<&Node> = tied.iter().map(|&i| &plans[i]).collect();
    for winner in index_prefix_winners(&solutions) {
        bonus[tied[winner]].1 += 2.0 * BONUS;
    }
    for &i in &tied {
        if holds(&plans[i], &|n| matches!(n, Node::DistinctScan(..))) {
            bonus[i].2 += BONUS;
        }
    }
    for &i in &tied {
        let (docs, prefix, distinct) = bonus[i];
        scores[i] += docs + prefix + distinct;
    }
}

/// A plan stage's type and children, as `areNodesCompatible` compares them.
fn kind_and_children(node: &Node) -> (String, Vec<&Node>) {
    let kind = match node {
        Node::Sort { on_keys, .. } => {
            if *on_keys {
                "SORT_DEFAULT".to_string()
            } else {
                "SORT_SIMPLE".to_string()
            }
        }
        Node::Projection { name, .. } => name.to_string(),
        other => other
            .to_doc(None)
            .get_str("stage")
            .unwrap_or("")
            .to_string(),
    };
    let children: Vec<&Node> = match node {
        Node::Fetch { input, .. }
        | Node::Sort { input, .. }
        | Node::Projection { input, .. }
        | Node::Skip(_, input)
        | Node::Limit(_, input)
        | Node::ReturnKey(input)
        | Node::Subplan(input)
        | Node::Count(input)
        | Node::Update(input)
        | Node::Delete { input, .. } => vec![input],
        Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
            inputs.iter().collect()
        }
        _ => vec![],
    };
    (kind, children)
}

/// `isLowerBound`: a value that opens its type's range.
fn lower_bound(v: &bson::Bson, incl: bool) -> bool {
    use bson::Bson;
    match v {
        Bson::Double(d) => (d.is_infinite() || d.is_nan()) && incl,
        Bson::Int32(_) | Bson::Int64(_) => false,
        Bson::Decimal128(d) => {
            let s = d.to_string();
            (s.contains("Infinity") || s.contains("NaN")) && incl
        }
        Bson::String(s) => s.is_empty() && incl,
        Bson::DateTime(d) => d.timestamp_millis() == i64::MIN && incl,
        Bson::Timestamp(t) => t.time == 0 && t.increment == 0 && incl,
        Bson::ObjectId(o) => o.bytes() == [0u8; 12] && incl,
        Bson::Document(d) => d.is_empty() && incl,
        Bson::Array(a) => a.is_empty() && incl,
        _ => true,
    }
}

/// `isUpperBound`: a value that closes its type's range.
fn upper_bound(v: &bson::Bson, incl: bool) -> bool {
    use bson::Bson;
    match v {
        Bson::Double(d) => d.is_infinite() && incl,
        Bson::Int32(_) | Bson::Int64(_) => false,
        Bson::Decimal128(d) => d.to_string().contains("Infinity") && incl,
        Bson::String(_) => false,
        Bson::DateTime(d) => d.timestamp_millis() == i64::MAX && incl,
        Bson::Timestamp(t) => t.time == u32::MAX && t.increment == u32::MAX && incl,
        Bson::ObjectId(o) => o.bytes() == [0xffu8; 12] && incl,
        Bson::Document(d) => d.is_empty() && !incl,
        Bson::Array(a) => a.is_empty() && !incl,
        Bson::Binary(b) => b.bytes.is_empty() && !incl,
        _ => true,
    }
}

/// `isClosedInterval`, the interval held low to high: both ends of one
/// BSON type, neither the edge of that type's range. A point is closed.
fn closed(i: &Interval) -> bool {
    if std::mem::discriminant(&i.lo) != std::mem::discriminant(&i.hi) {
        return false;
    }
    if i.is_point() {
        return true;
    }
    !lower_bound(&i.lo, i.lo_incl) && !upper_bound(&i.hi, i.hi_incl)
}

/// `getIndexBoundsScore`: the longest prefix of single points, of points,
/// of closed intervals, of any bounds, then the shorter key, packed.
fn bounds_score(bounds: &[Vec<Interval>]) -> u64 {
    let (mut single, mut points, mut closed_prefix, mut length) = (0u64, 0u64, 0u64, 0u64);
    for field in bounds {
        if matches!(field.as_slice(), [one] if one.is_all()) {
            break;
        }
        if length == single && matches!(field.as_slice(), [one] if one.is_point()) {
            single += 1;
        }
        if length == points && field.iter().all(Interval::is_point) {
            points += 1;
        }
        if length == closed_prefix && field.iter().all(closed) {
            closed_prefix += 1;
        }
        length += 1;
    }
    (single << 52)
        | (points << 40)
        | (closed_prefix << 28)
        | (length << 16)
        | (u64::from(u16::MAX) - bounds.len() as u64)
}

/// `applyIndexPrefixHeuristic`: walking the tied plans side by side (they
/// must have one shape, or none wins), each set of index scans in the same
/// place gives a point to the best bounds; the plans with the most points
/// win.
fn index_prefix_winners(solutions: &[&Node]) -> Vec<usize> {
    let mut points = vec![0usize; solutions.len()];
    let mut stack: Vec<Vec<&Node>> = vec![solutions.to_vec()];
    while let Some(top) = stack.pop() {
        let shapes: Vec<(String, Vec<&Node>)> = top.iter().map(|n| kind_and_children(n)).collect();
        if shapes
            .windows(2)
            .any(|w| w[0].0 != w[1].0 || w[0].1.len() != w[1].1.len())
        {
            return Vec::new();
        }
        for child in 0..shapes[0].1.len() {
            stack.push(shapes.iter().map(|(_, c)| c[child]).collect());
        }
        let bounds: Vec<&[Vec<Interval>]> = top
            .iter()
            .filter_map(|n| match n {
                Node::IxScan(ix) | Node::DistinctScan(ix, _, _) => Some(ix.bounds.as_slice()),
                _ => None,
            })
            .collect();
        if bounds.len() == top.len() {
            let scores: Vec<u64> = bounds.iter().map(|b| bounds_score(b)).collect();
            let best = scores.iter().copied().max().unwrap_or(0);
            for (i, s) in scores.iter().enumerate() {
                if *s == best {
                    points[i] += 1;
                }
            }
        }
    }
    let best = points.iter().copied().max().unwrap_or(0);
    (0..solutions.len())
        .filter(|&i| points[i] == best)
        .collect()
}

/// mongod's `CachedPlanStage::pickBestPlan`: the cached plan is worked
/// until it hands over `need` documents or runs out, within `budget`
/// works. Whether it did; `None`: a stage this does not step.
pub(crate) fn within(plan: &Node, env: &Env, need: i64, budget: i64) -> Option<bool> {
    let mut cache = Entries::new();
    let mut stage = Stage::new(plan, env, &mut cache)?;
    for _ in 0..budget {
        match stage.work(env) {
            Work::Advanced(_) if stage.returned >= need => return Some(true),
            Work::Eof => return Some(true),
            _ => {}
        }
    }
    Some(false)
}
