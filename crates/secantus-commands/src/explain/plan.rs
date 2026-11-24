//! Choosing the plan mongod would report for a query.
//!
//! This reproduces the planner's RULES: which predicates bound which index,
//! what is left for the fetch to recheck, when an index serves the sort or
//! covers the projection. When several plans could serve one query and the
//! caller holds the collection, mongod's trial between them is counted
//! (`super::trial`): one plan per index a predicate bounds, and a walk of
//! each index that returns the sort when none of those does. Plans the
//! trial scores alike, and every choice made without the collection, fall
//! to a fixed order of preference (`preference`).

use super::bounds::{intersect, leaf_bounds, union, Ctx, Interval, Tight};
use super::exec::Env;
use super::node::{IndexInfo, IxScan, Node, Stats};
use bson::{doc, Bson, Document};
use secantus_core::collation::Collation;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

thread_local! {
    /// Whether the operation running on this thread times its planning,
    /// and the time it has spent (`None`: it has planned nothing yet):
    /// the `planningTimeMicros` of its `Slow query` line (RM-023).
    static PLANNING: std::cell::Cell<(bool, Option<std::time::Duration>)> =
        const { std::cell::Cell::new((false, None)) };
    /// How deep in the planner the thread is: a plan made inside another
    /// is part of the outer one's time.
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Whether the operation has made its planning's interrupt checks
    /// ([`planning_checks`]): it plans many times, mongod once.
    static CHECKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// An operation begins on this thread: it has planned nothing.
pub(crate) fn begin_operation() {
    CHECKED.with(|c| c.set(false));
}

/// The interrupt checks mongod's planning makes, once per operation, from
/// the outermost call into the planner (RM-360): one per index scan the
/// access planner builds (`QueryPlannerAccess::makeLeafNode`) and one when
/// it is done (`PrepareExecutionHelper::prepare`), or the express
/// executor's one; none for a walk of a whole index (`IxScan::whole`); none for a `RECORD_STORE_FAST_COUNT`, a `DISTINCT_SCAN`
/// without a query, or an IDHACK, which mongod builds without its planner
/// (an upsert's or a `count`'s `_id` lookup; measured 8.2.11, 2026-11-08
/// and 2026-11-09). A write's are its write plan's ([`write`]). A failed check trips the
/// command (`deadline::tripped`). A plan with more than one candidate
/// ([`candidates`]) has the leaves of all of mongod's enumerated plans,
/// which this planner does not build, so its count is not mongod's.
fn planning_checks(node: &Node, distinct_without_query: bool) {
    if DEPTH.with(std::cell::Cell::get) != 1 || CHECKED.with(|c| c.replace(true)) {
        return;
    }
    fn leaves(node: &Node) -> Option<usize> {
        match node {
            Node::CollScan { .. } => Some(0),
            // A walk of the whole index is built without a check.
            Node::IxScan(scan) | Node::CountScan(scan) => Some(usize::from(!scan.whole)),
            Node::Fetch { input, .. }
            | Node::Sort { input, .. }
            | Node::Skip(_, input)
            | Node::Limit(_, input)
            | Node::Projection { input, .. }
            | Node::ReturnKey(input)
            | Node::SortKeyGenerator(input)
            | Node::Count(input)
            | Node::Update(input)
            | Node::Delete { input, .. } => leaves(input),
            _ => None,
        }
    }
    /// An `_id` lookup mongod answers with IDHACK, which it builds without
    /// its planner (`isIdHackEligibleQuery`): an upsert's, a `count`'s.
    fn id_hack(node: &Node) -> bool {
        match node {
            Node::IdHack(_) => true,
            Node::Count(input)
            | Node::Update(input)
            | Node::Delete { input, .. }
            | Node::Projection { input, .. } => id_hack(input),
            _ => false,
        }
    }
    use secantus_core::deadline::{check_for_interrupt, unmatched};
    let checks = match node {
        _ if id_hack(node) => Some(0),
        Node::FastCount => Some(0),
        Node::DistinctScan(..) if distinct_without_query => Some(0),
        Node::Projection { input, .. }
            if distinct_without_query && matches!(**input, Node::DistinctScan(..)) =>
        {
            Some(0)
        }
        Node::Express { .. } | Node::ExpressWrite { .. } => Some(1),
        other => leaves(other).map(|n| n + 1),
    };
    match checks {
        Some(n) => {
            for _ in 0..n {
                let _ = check_for_interrupt();
            }
        }
        None => unmatched(),
    }
}

/// Times one call into the planner, when the outermost.
#[must_use = "the planning is timed while it lives"]
pub(crate) struct Timing(Option<std::time::Instant>);

/// Times a plan its caller settles without the planner, as mongod plans
/// it all the same: an `EOF` for a collection that is not there, a
/// `RECORD_STORE_FAST_COUNT` for a `count` with no query. Its line and
/// entry carry the `planningTimeMicros` (measured 8.2.11, RM-363).
pub(crate) fn planned_apart() -> Timing {
    plans_nothing();
    timing()
}

/// The operation makes no planning checks (RM-360): its plan was settled
/// without the planner ([`planned_apart`]), or it is a `getMore`, which
/// runs the plan its cursor has and plans nothing on mongod.
pub(crate) fn plans_nothing() {
    CHECKED.with(|c| c.set(true));
}

fn timing() -> Timing {
    let depth = DEPTH.with(|d| d.replace(d.get() + 1));
    let armed = PLANNING.with(|p| p.get().0);
    Timing((depth == 0 && armed).then(std::time::Instant::now))
}

impl Drop for Timing {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        if let Some(began) = self.0 {
            PLANNING.with(|p| {
                let (armed, spent) = p.get();
                p.set((armed, Some(spent.unwrap_or_default() + began.elapsed())));
            });
        }
    }
}

/// Times the operation's planning until it drops ([`PlanningScope::micros`]).
pub(crate) struct PlanningScope((bool, Option<std::time::Duration>));

/// Start timing the planning of the operation running on this thread.
pub(crate) fn planning_scope() -> PlanningScope {
    PlanningScope(PLANNING.with(|p| p.replace((true, None))))
}

impl PlanningScope {
    /// The planning time so far, in microseconds; `None` when nothing was
    /// planned.
    pub(crate) fn micros(&self) -> Option<i64> {
        planning_spent().map(|d| d.as_micros() as i64)
    }
}

impl Drop for PlanningScope {
    fn drop(&mut self) {
        PLANNING.with(|p| p.set(self.0));
    }
}

/// The planning time spent so far by the operation running on this thread.
pub(crate) fn planning_spent() -> Option<std::time::Duration> {
    PLANNING.with(|p| p.get().1)
}

/// The index a query asked for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hint {
    None,
    /// `{$natural: ±1}`: true is backward.
    Natural(bool),
    Index(String),
}

/// Everything about a query the plan depends on.
#[derive(Clone)]
pub(crate) struct Query<'a> {
    pub canonical: &'a Document,
    pub sort: Option<&'a Document>,
    pub projection: Option<&'a Document>,
    pub skip: i64,
    pub limit: i64,
    pub hint: Hint,
    pub collation: Option<&'a Collation>,
    pub min_max: bool,
    pub return_key: bool,
    pub show_record_id: bool,
    pub batch_size: bool,
    /// The command reads index keys only (`count`).
    pub keys_only: bool,
    /// A field the command wants from the index itself (`distinct`'s key).
    pub wanted: Option<&'a str>,
    /// Whether the query implies an index's partial filter.
    pub implies: &'a dyn Fn(&Document) -> bool,
    /// The query is an aggregate's front that mongod runs in SBE: planned
    /// by the classic planner without index intersections, and cached by
    /// what its winner read (measured 8.2.11, 2026-11-08: `{a: 3, b: 5}`
    /// over `a_1` and `b_1` tries the two scans and no `AND_SORTED`).
    pub sbe: bool,
    /// What the caller can say of the collection: mongod runs its candidate
    /// plans against the data and keeps the one that finishes first, and
    /// with this the planner can count what each would read.
    pub trial: Option<&'a Trial<'a>>,
}

/// The collection, for counting what a candidate plan reads.
pub(crate) struct Trial<'a> {
    /// How many documents it holds, when the caller counted them.
    pub total: Option<usize>,
    /// How many of them the query matches, when the caller knows.
    pub matched: Option<usize>,
    /// The documents themselves, read only when a count needs them.
    pub whole: &'a dyn Whole,
    /// The plans the last counted trial ran, as `explain` lists them.
    pub ranked: RefCell<Vec<Ranked>>,
    /// mongod's plan cache, which a read consults before any trial and
    /// writes after one (`super::plan_cache`). `explain` passes none: it
    /// neither reads nor writes the cache.
    pub cache: Option<&'a dyn CacheHook>,
    /// The query's active cache entry ran in place of a trial and did no
    /// worse than it allowed: mongod runs it under a CACHED_PLAN stage
    /// (`CachedPlanStage`), which a replanned query's does not keep.
    pub cached_ran: std::cell::Cell<bool>,
}

/// What the plan cache holds for a query, as the planner asks it.
pub(crate) struct Lookup {
    pub key: super::cache_key::CacheKey,
    /// The active entry's plan (its `signature`) and the works it took.
    pub active: Option<(String, i64)>,
    /// The query is a branch of a rooted `$or`, planned by the subplanner.
    pub branch: bool,
    /// The active entry's plan was already tried for this read: take it as
    /// it is.
    pub trusted: bool,
}

/// How a trial's winner is written to the cache
/// (`ConditionalClassicPlanCacheWriter::Mode`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CacheMode {
    Always,
    /// Not when the best plans tie or the winner returned nothing
    /// (a branch of a rooted `$or`).
    Sometimes,
}

/// A finished trial, for the plan cache.
pub(crate) struct Decision<'p> {
    /// The plans, best first, with their scores and what they counted.
    pub ranked: Vec<(&'p Node, f64, &'p Stats)>,
    /// The winner's `works`; for a query SBE runs, the keys and documents
    /// it read.
    pub works: i64,
}

/// The plan cache as the planner sees it.
pub(crate) trait CacheHook {
    /// The cache's entry for the query `q` plans. `None`: a query mongod
    /// does not cache.
    fn lookup(&self, q: &Query) -> Option<Lookup>;
    /// A cached plan did worse than its works allowed: the entry goes
    /// inactive (`PlanCache::deactivate`).
    fn deactivate(&self, key: &super::cache_key::CacheKey);
    /// A trial's winner for the query (`PlanCache::set`).
    fn store(&self, l: &Lookup, q: &Query, d: &Decision, mode: CacheMode);
    /// The plan the read runs for the query, cached or tried.
    fn chose(&self, l: &Lookup, signature: String);
}

/// The query solution as mongod hashes it (`QuerySolution::hash`), which
/// `explain`'s `isCached` compares with the cached one: every stage of the
/// plan under whatever the command puts on top -- fetch, sort, skip, limit
/// and projection included -- and the index and direction each scan reads,
/// but no value (measured 8.2.11: the plan cached for `{a: {$lt: 5}}` is
/// `isCached` for `{a: {$lt: 6}}`, and not for it with a skip and limit).
pub(crate) fn solution(node: &Node) -> String {
    let mut node = node;
    while let Node::Count(input) | Node::Update(input) | Node::Delete { input, .. } = node {
        node = input;
    }
    fn walk(node: &Node, out: &mut String) {
        let doc = node.to_doc(None);
        out.push_str(doc.get_str("stage").unwrap_or("?"));
        if let Node::Sort {
            on_keys,
            keeps_metadata,
            ..
        } = node
        {
            out.push_str(if *on_keys || *keeps_metadata {
                ":default"
            } else {
                ":simple"
            });
        }
        if let Ok(name) = doc.get_str("indexName") {
            out.push(':');
            out.push_str(name);
            out.push(':');
            out.push_str(doc.get_str("direction").unwrap_or(""));
        }
        let inputs: Vec<&Node> = match node {
            Node::Fetch { input, .. }
            | Node::Sort { input, .. }
            | Node::Skip(_, input)
            | Node::Limit(_, input)
            | Node::Projection { input, .. }
            | Node::ReturnKey(input)
            | Node::SortKeyGenerator(input)
            | Node::Subplan(input) => vec![input],
            Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
                inputs.iter().collect()
            }
            _ => vec![],
        };
        if !inputs.is_empty() {
            out.push('(');
            for (i, n) in inputs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                walk(n, out);
            }
            out.push(')');
        }
    }
    let mut out = String::new();
    walk(node, &mut out);
    out
}

/// The plan's structure: how it reaches the documents -- the scans, the
/// indexes they read and in which direction, and per key field the kind of
/// bounds (`P`oint, `A`ll, a range closed below `L`, above `H`, or both
/// `C`) -- without the values, and without the stages a query's other
/// parts put over it (fetch, sort, projection, skip, limit). Two queries of
/// one shape planned alike have one signature: it stands for mongod's
/// cached index tags, from which mongod rebuilds the rest of the plan.
pub(crate) fn signature(node: &Node) -> String {
    fn kind(i: &Interval) -> char {
        if i.is_point() {
            'P'
        } else if i.is_all() {
            'A'
        } else {
            let low = !matches!(i.lo, Bson::MinKey)
                && !matches!(&i.lo, Bson::Double(d) if d.is_infinite());
            let high = !matches!(i.hi, Bson::MaxKey)
                && !matches!(&i.hi, Bson::Double(d) if d.is_infinite());
            match (low, high) {
                (true, true) => 'C',
                (true, false) => 'L',
                (false, true) => 'H',
                (false, false) => 'O',
            }
        }
    }
    fn scan(ix: &IxScan, out: &mut String) {
        out.push_str(&ix.index.name);
        out.push(if ix.backward { '<' } else { '>' });
        for field in &ix.bounds {
            let kinds: BTreeSet<char> = field.iter().map(kind).collect();
            out.extend(kinds);
            out.push(';');
        }
    }
    fn walk(node: &Node, out: &mut String) {
        let inputs: Vec<&Node> = match node {
            // What the rest of the query puts over the access.
            Node::Fetch { input, .. }
            | Node::Sort { input, .. }
            | Node::Skip(_, input)
            | Node::Limit(_, input)
            | Node::Projection { input, .. }
            | Node::ReturnKey(input)
            | Node::SortKeyGenerator(input)
            | Node::Count(input)
            | Node::Update(input)
            | Node::Delete { input, .. } => return walk(input, out),
            Node::CollScan { backward, .. } => {
                out.push_str(if *backward { "COLLSCAN<" } else { "COLLSCAN>" });
                vec![]
            }
            Node::IxScan(ix) => {
                out.push_str("IXSCAN:");
                scan(ix, out);
                vec![]
            }
            Node::DistinctScan(ix, width, fetching) => {
                out.push_str(&format!("DISTINCT_SCAN{width}{fetching}:"));
                scan(ix, out);
                vec![]
            }
            Node::CountScan(ix) => {
                out.push_str("COUNT_SCAN:");
                scan(ix, out);
                vec![]
            }
            Node::Or(inputs) => {
                out.push_str("OR");
                inputs.iter().collect()
            }
            Node::AndSorted(inputs) => {
                out.push_str("AND_SORTED");
                inputs.iter().collect()
            }
            Node::SortMerge { inputs, .. } => {
                out.push_str("SORT_MERGE");
                inputs.iter().collect()
            }
            Node::Subplan(input) => {
                out.push_str("SUBPLAN");
                vec![input]
            }
            other => {
                out.push_str(other.to_doc(None).get_str("stage").unwrap_or("?"));
                vec![]
            }
        };
        if !inputs.is_empty() {
            out.push('(');
            for (i, n) in inputs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                walk(n, out);
            }
            out.push(')');
        }
    }
    let mut out = String::new();
    walk(node, &mut out);
    out
}

/// One plan mongod tried: its tree, its score, and what its stages counted
/// during the trial.
pub(crate) struct Ranked {
    pub plan: Node,
    pub score: f64,
    pub stats: Stats,
}

/// A collection's documents, for candidate plans to be run over.
pub(crate) trait Whole {
    /// The documents `plans` read in their first `works` works each: every
    /// document of a small collection, or of a large one those reached
    /// (`Docs::reach`). `None`: the collection is not to hand.
    fn docs(&self, plans: &[&Node], works: i64) -> Option<Docs<'_>>;
}

/// The documents a trial runs over.
pub(crate) struct Docs<'a> {
    pub docs: Cow<'a, [Document]>,
    /// Per document, whether the query matches it (`Env::known`).
    pub known: Option<Cow<'a, [bool]>>,
    pub collation: Option<&'a Collation>,
    /// The works these documents serve: within that many, each plan reads
    /// only documents held here, so it does what it would over the whole
    /// collection. `None`: any number (they are the whole collection, or
    /// every scan came to its end).
    pub reach: Option<i64>,
    /// How many documents the collection holds; `None` when not counted,
    /// which serves a trial shorter than `internalQueryPlanEvaluationWorks`.
    pub total: Option<usize>,
    /// The collection is clustered (`Env::clustered`).
    pub clustered: bool,
}

impl Docs<'_> {
    pub(crate) fn env(&self) -> Env<'_> {
        Env {
            docs: &self.docs,
            collation: self.collation,
            skip: 0,
            limit: 0,
            multi: true,
            upsert: false,
            returns: false,
            known: self.known.as_deref(),
            query: None,
            clustered: self.clustered,
        }
    }
}

/// The works a trial over a large collection first reads for: a trial that
/// ends at 101 documents seldom takes more.
const FIRST_REACH: i64 = 512;

/// `go` run over the documents `plans` read, as many as it turns out to
/// need. `go` answers with the works it took; when that reached what the
/// documents serve, it is run again over eight times as many, so its answer
/// is the one the whole collection gives. `None`: the collection is not to
/// hand, or `go` could not run.
fn over_reach<T>(
    whole: &dyn Whole,
    plans: &[&Node],
    mut go: impl FnMut(&Docs) -> Option<(T, i64)>,
) -> Option<T> {
    let mut works = FIRST_REACH;
    loop {
        let docs = whole.docs(plans, works)?;
        let (out, used) = go(&docs)?;
        let short = docs.reach.is_some_and(|r| used >= r);
        // A trial as long as `internalQueryPlanEvaluationWorks` may have
        // been cut short by an uncounted collection's budget.
        let uncounted = docs.total.is_none() && used >= super::trial::max_works(0);
        if !(short || uncounted) {
            return Some(out);
        }
        works = works.saturating_mul(8).max(used.saturating_add(1));
    }
}

/// The scan a query reads through, before sort, paging and projection.
#[derive(Clone)]
struct Access {
    scan: Node,
    fetch_filter: Option<Document>,
    /// The scan reads index keys, so documents need a FETCH.
    indexed: bool,
    /// The one index read, when there is exactly one.
    index: Option<Arc<IndexInfo>>,
    /// The scan already returns documents in the sort order.
    sorted: bool,
    subplan: bool,
}

#[derive(Clone)]
struct Leaf {
    path: String,
    pred: Document,
    /// Which clause of the query this came from.
    clause: usize,
    /// Derived from inside an `$elemMatch`: bounds only, the clause stays.
    derived: bool,
}

/// The conjuncts of a canonical match expression.
pub(crate) fn clauses(canonical: &Document) -> Vec<Document> {
    if canonical.is_empty() {
        return vec![];
    }
    if canonical.len() == 1 {
        if let Ok(parts) = canonical.get_array("$and") {
            return parts
                .iter()
                .filter_map(Bson::as_document)
                .cloned()
                .collect();
        }
    }
    vec![canonical.clone()]
}

/// One clause, or an `$and` of several, as mongod prints a filter.
///
/// A twin `$expr` derived (`$_internalExprEq` ...) is never printed: the
/// `$expr` beside it rechecks it. But mongod drops it from the `$and` after
/// building it, so a filter left with ONE clause that way is still printed
/// as an `$and` (measured 8.2.11, 2026-10-16). A twin the bounds decided
/// never reaches here, and leaves no `$and` behind.
pub(crate) fn assemble(parts: Vec<Document>) -> Option<Document> {
    let before = parts.len();
    let parts: Vec<Document> = parts.into_iter().filter(|c| !is_expr_twin(c)).collect();
    match parts.len() {
        0 => None,
        1 if parts.len() == before => parts.into_iter().next(),
        _ => Some(doc! { "$and": parts.into_iter().map(Bson::Document).collect::<Vec<_>>() }),
    }
}

/// A predicate `$expr` derived for the planner: `{path: {$_internalExprEq:
/// v}}` and its range siblings, or an `$or` of them.
pub(crate) fn is_expr_twin(clause: &Document) -> bool {
    let Some((key, value)) = clause.iter().next().filter(|_| clause.len() == 1) else {
        return false;
    };
    match (key.as_str(), value) {
        ("$or", Bson::Array(branches)) => branches.iter().all(|b| match b {
            Bson::Document(d) => match d.get("$and") {
                Some(Bson::Array(parts)) => parts
                    .iter()
                    .all(|p| p.as_document().is_some_and(is_expr_twin)),
                _ => is_expr_twin(d),
            },
            _ => false,
        }),
        (path, Bson::Document(pred)) if !path.starts_with('$') => {
            pred.len() == 1 && pred.keys().all(|k| k.starts_with("$_internalExpr"))
        }
        _ => false,
    }
}

fn leaves_of(parts: &[Document]) -> Vec<Leaf> {
    let mut out = Vec::new();
    for (i, clause) in parts.iter().enumerate() {
        let Some((path, value)) = clause.iter().next() else {
            continue;
        };
        if path.starts_with('$') {
            continue;
        }
        let Some(pred) = value.as_document() else {
            continue;
        };
        // An object-form `$elemMatch` bounds the paths under the array.
        if let Ok(inner) = pred.get_document("$elemMatch") {
            if inner.keys().all(|k| !k.starts_with('$')) {
                for (sub, sub_pred) in inner {
                    if let Some(p) = sub_pred.as_document() {
                        out.push(Leaf {
                            path: format!("{path}.{sub}"),
                            pred: p.clone(),
                            clause: i,
                            derived: true,
                        });
                    }
                }
                continue;
            }
        }
        out.push(Leaf {
            path: path.clone(),
            pred: pred.clone(),
            clause: i,
            derived: false,
        });
    }
    out
}

fn collation_differs(index: &IndexInfo, query: Option<&Collation>) -> bool {
    match (&index.collation, query) {
        (None, None) => false,
        (Some(a), Some(b)) => !a.same(b),
        _ => true,
    }
}

/// What one index does for a conjunction.
struct IndexUse {
    index: Arc<IndexInfo>,
    bounds: Vec<Vec<Interval>>,
    /// Clauses the bounds decide outright.
    covered: BTreeSet<usize>,
    /// Clauses rechecked against the key.
    on_key: BTreeSet<usize>,
    /// Clauses whose predicate set bounds: mongod's index tags.
    tagged: BTreeSet<usize>,
    /// The leading field is bounded by a predicate.
    leads: bool,
    bounded_fields: usize,
}

fn use_index(index: &Arc<IndexInfo>, leaves: &[Leaf], parts: &[Document], q: &Query) -> IndexUse {
    let collated = collation_differs(index, q.collation);
    let mut bounds = Vec::new();
    let mut covered = BTreeSet::new();
    let mut on_key = BTreeSet::new();
    let mut tagged = BTreeSet::new();
    let mut leads = false;
    let mut bounded_fields = 0;
    let wildcard_path = index.wildcard.as_ref().and_then(|w| w.path.clone());
    for (pos, (field, _)) in index.fields().into_iter().enumerate() {
        // A wildcard index's `$_path` is one string, the queried path.
        if let (0, Some(path)) = (pos, &wildcard_path) {
            let mut list = vec![Interval::point(Bson::String(path.clone()))];
            // Present anywhere: the path, and every path below it.
            if leaves
                .iter()
                .any(|l| l.path == *path && l.pred.get("$exists").is_some_and(truthy))
            {
                list.push(Interval::new(
                    Bson::String(format!("{path}.")),
                    true,
                    Bson::String(format!("{path}/")),
                    false,
                ));
            }
            bounds.push(list);
            continue;
        }
        // A geo field is bounded by the covering of a geo predicate alone.
        if index.geo.as_ref().is_some_and(|g| g.geo_field() == field) {
            let found = geo_leaves(index, leaves).find_map(|l| {
                let (op, arg) = l.pred.iter().next()?;
                let op = if op == "$within" {
                    "$geoWithin"
                } else {
                    op.as_str()
                };
                let list = geo_intervals(index.geo.as_ref()?.predicate_bounds(op, arg)?)?;
                Some((list, l.clause))
            });
            if let Some((list, clause)) = found {
                tagged.insert(clause);
                bounded_fields += 1;
                if pos == 0 {
                    leads = true;
                }
                bounds.push(list);
            } else {
                bounds.push(vec![Interval::all()]);
            }
            continue;
        }
        let multikey = !index.multikey_paths[pos].is_empty();
        let ctx = Ctx {
            multikey,
            // A wildcard index holds no key for a missing path.
            sparse: index.sparse || wildcard_path.is_some(),
            collated,
            plain_strings: index.collation.is_none(),
            keys_only: q.keys_only,
        };
        let mut list: Option<Vec<Interval>> = None;
        for leaf in leaves.iter().filter(|l| l.path == field) {
            if wildcard_path.is_some() && !wildcard_fits(&leaf.pred) {
                continue;
            }
            // A multikey field takes the bounds of ONE predicate: two
            // predicates may be satisfied by different array elements.
            if multikey && !leaf.derived && list.is_some() {
                continue;
            }
            let found = if index.is_hashed(pos) {
                super::bounds::hashed_leaf_bounds(&leaf.pred, ctx, index.collation.as_ref())
            } else {
                leaf_bounds(&leaf.pred, ctx)
            };
            let Some((part, tight)) = found else {
                continue;
            };
            list = Some(match list {
                Some(have) => intersect(&have, &part),
                None => part,
            });
            if leaf.derived {
                continue;
            }
            tagged.insert(leaf.clause);
            match tight {
                Tight::Exact => {
                    covered.insert(leaf.clause);
                }
                Tight::IndexFilter => {
                    on_key.insert(leaf.clause);
                }
                Tight::FetchFilter => {}
            }
        }
        if list.is_some() {
            bounded_fields += 1;
            // A wildcard entry is led by `$_path`: its path's field leads.
            if pos == 0 || (pos == 1 && wildcard_path.is_some()) {
                leads = true;
            }
        }
        bounds.push(list.unwrap_or_else(|| vec![Interval::all()]));
    }
    // An index whose leading field has no predicate is read end to end.
    if !leads {
        return IndexUse {
            index: index.clone(),
            bounds: vec![vec![Interval::all()]; index.key.len()],
            covered: BTreeSet::new(),
            on_key: BTreeSet::new(),
            tagged: BTreeSet::new(),
            leads,
            bounded_fields: 0,
        };
    }
    // A clause the partial filter states is true of every indexed document.
    if let Some(partial) = &index.partial {
        let stated = clauses(&secantus_core::canonical_match(&Bson::Document(
            partial.clone(),
        )));
        for (i, clause) in parts.iter().enumerate() {
            if stated.contains(clause) {
                covered.insert(i);
            }
        }
    }
    IndexUse {
        index: index.clone(),
        bounds,
        covered,
        on_key,
        tagged,
        leads,
        bounded_fields,
    }
}

/// The index a plan reads and the clauses of `q.canonical` it assigned to
/// that index -- mongod's index tags, from which it builds the cache
/// entry's `PlanCacheIndexTree`. `None` unless the plan reads one index
/// (a scan, or the scans of a merge over points of it) and every clause is
/// one mongod tags as this planner does (no `$elemMatch`).
pub(crate) fn index_tags(q: &Query, winner: &Node) -> Option<(Arc<IndexInfo>, BTreeSet<usize>)> {
    let index = match access_of_plan(winner)? {
        Node::IxScan(ix) => ix.index.clone(),
        Node::SortMerge { inputs, .. } => {
            let mut indexes = inputs.iter().map(|n| match n {
                Node::IxScan(ix) => Some(ix.index.clone()),
                _ => None,
            });
            let first = indexes.next()??;
            if !indexes.all(|i| i.is_some_and(|i| i.name == first.name)) {
                return None;
            }
            first
        }
        _ => return None,
    };
    let parts = clauses(q.canonical);
    let leaves = leaves_of(&parts);
    if leaves.iter().any(|l| l.derived) {
        return None;
    }
    Some((index.clone(), use_index(&index, &leaves, &parts, q).tagged))
}

/// For a plan that reads an `OR` of index scans beside other clauses
/// (`{$or: [{a: 3}, {b: 5}], c: {$lt: 3}}` over `a_1` and `b_1`): which
/// clause of `q.canonical` is the `$or`, and the index each of its branches
/// was assigned -- mongod's index tags under the `OR`. `None` unless the
/// query is a conjunction holding one `$or`, every branch is one predicate
/// on the leading field of exactly one scanned index, and no scanned index
/// holds a field of another clause (mongod could push that clause into the
/// branches, under an `OrPushdownTag`).
pub(crate) fn or_tags(q: &Query, winner: &Node) -> Option<(usize, Vec<Arc<IndexInfo>>)> {
    let mut node = winner;
    let inputs = loop {
        match node {
            Node::Fetch { input, .. }
            | Node::Sort { input, .. }
            | Node::Skip(_, input)
            | Node::Limit(_, input)
            | Node::Projection { input, .. } => node = input,
            Node::Or(inputs) => break inputs,
            _ => return None,
        }
    };
    let mut scanned: Vec<Arc<IndexInfo>> = Vec::new();
    for input in inputs {
        let scan = match input {
            Node::Fetch { input, .. } => input.as_ref(),
            other => other,
        };
        let Node::IxScan(ix) = scan else { return None };
        scanned.push(ix.index.clone());
    }
    let parts = clauses(q.canonical);
    if parts.len() < 2 {
        return None;
    }
    let mut ors = parts
        .iter()
        .enumerate()
        .filter(|(_, p)| p.contains_key("$or"));
    let (at, or) = ors.next()?;
    if ors.next().is_some() {
        return None;
    }
    let outer: BTreeSet<&str> = parts
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != at)
        .flat_map(|(_, p)| p.keys().map(String::as_str))
        .collect();
    if scanned
        .iter()
        .any(|ix| ix.fields().iter().any(|(f, _)| outer.contains(*f)))
    {
        return None;
    }
    let mut tags = Vec::new();
    for branch in or.get_array("$or").ok()? {
        let branch = branch.as_document()?;
        let (path, _) = branch.iter().next().filter(|_| branch.len() == 1)?;
        let mut by = scanned.iter().filter(|ix| ix.fields()[0].0 == path);
        let one = by.next()?;
        if by.any(|ix| ix.name != one.name) {
            return None;
        }
        tags.push(one.clone());
    }
    Some((at, tags))
}

/// The access under what the rest of the query puts over it.
fn access_of_plan(node: &Node) -> Option<&Node> {
    match node {
        Node::Fetch { input, .. }
        | Node::Sort { input, .. }
        | Node::Skip(_, input)
        | Node::Limit(_, input)
        | Node::Projection { input, .. } => access_of_plan(input),
        Node::IxScan(_) | Node::SortMerge { .. } => Some(node),
        _ => None,
    }
}

/// Whether a scan of `index` under `bounds` returns the sort order, and in
/// which direction (`true` is backward).
fn serves_sort(index: &IndexInfo, bounds: &[Vec<Interval>], sort: &Document) -> Option<bool> {
    let fields = index.fields();
    let pinned = |pos: usize| matches!(bounds[pos].as_slice(), [one] if one.is_point());
    let mut pos = 0;
    let mut backward: Option<bool> = None;
    for (name, dir) in sort {
        let dir = crate::util::as_i64(dir)?;
        // A field that holds arrays sorts by its least or greatest element,
        // and a scan meets a document at the first key inside its bounds:
        // the two agree only when the bounds leave no key out.
        if let Some(at) = index.position(name) {
            let whole = matches!(bounds[at].as_slice(), [one] if one.is_all());
            if !index.multikey_paths[at].is_empty() && !whole {
                return None;
            }
        }
        // Sorting on a field every result shares is no sort at all.
        if index.position(name).is_some_and(pinned) {
            continue;
        }
        while pos < fields.len() && fields[pos].0 != name && pinned(pos) {
            pos += 1;
        }
        if pos < fields.len() && fields[pos].0 == name {
            // The keys of a hashed field are hashes: they order nothing.
            if index.is_hashed(pos) {
                return None;
            }
            let this = (fields[pos].1 < 0) != (dir < 0);
            if backward.is_some_and(|b| b != this) {
                return None;
            }
            backward = Some(this);
            pos += 1;
        } else {
            return None;
        }
    }
    Some(backward.unwrap_or(false))
}

fn sort_of<'a>(q: &'a Query) -> Option<&'a Document> {
    q.sort.filter(|s| !s.is_empty())
}

/// Whether an index may be read at all for this query.
fn eligible(index: &IndexInfo, q: &Query) -> bool {
    (index.plain
        || index.has_hashed()
        || index.geo.is_some()
        || index.wildcard.as_ref().is_some_and(|w| w.path.is_some()))
        && index.partial.as_ref().is_none_or(|p| (q.implies)(p))
}

/// The `$geoWithin` / `$geoIntersects` leaves on a geo index's geo field.
fn geo_leaves<'l>(index: &IndexInfo, leaves: &'l [Leaf]) -> impl Iterator<Item = &'l Leaf> {
    let field = index.geo.as_ref().map(|g| g.geo_field().to_string());
    leaves.iter().filter(move |l| {
        !l.derived
            && field.as_deref() == Some(l.path.as_str())
            && l.pred
                .keys()
                .next()
                .is_some_and(|k| matches!(k.as_str(), "$geoWithin" | "$within" | "$geoIntersects"))
    })
}

/// The intervals of a geo field's bounds, as mongod prints them: cells as
/// integers, a `2d` index's geohashes as `BinData(128, ...)`.
fn geo_intervals(bounds: secantus_core::geoorder::FieldBounds) -> Option<Vec<Interval>> {
    use secantus_core::geoorder::FieldBounds;
    match bounds {
        FieldBounds::Cells(list) => Some(
            list.into_iter()
                .map(|(lo, hi)| Interval::new(Bson::Int64(lo), true, Bson::Int64(hi), true))
                .collect(),
        ),
        FieldBounds::Hashes(list) => {
            let bin = |h: u64| {
                Bson::Binary(bson::Binary {
                    subtype: bson::spec::BinarySubtype::UserDefined(128),
                    bytes: h.to_be_bytes().to_vec(),
                })
            };
            Some(
                list.into_iter()
                    .map(|(lo, hi)| Interval::new(bin(lo), true, bin(hi), true))
                    .collect(),
            )
        }
        _ => None,
    }
}

/// `nodeIsSupportedByWildcardIndex`, and a wildcard index's sparseness: an
/// equality to null, which it holds no key for, or to a non-empty object
/// or array, which it keys by their leaves, is not one it serves.
fn wildcard_fits(pred: &Document) -> bool {
    let Some((op, v)) = pred.iter().next().filter(|_| pred.len() == 1) else {
        return false;
    };
    let one = |v: &Bson, op: &str| match v {
        Bson::Null => false,
        Bson::Document(d) => d.is_empty() && (op == "$eq" || op == "$lte"),
        Bson::Array(a) => a.is_empty() && op == "$eq",
        _ => true,
    };
    match op.as_str() {
        "$eq" | "$gt" | "$gte" | "$lt" | "$lte" => one(v, op),
        "$in" => v
            .as_array()
            .is_some_and(|items| items.iter().all(|x| one(x, "$eq"))),
        "$exists" => truthy(v),
        "$type" | "$regex" => true,
        _ => false,
    }
}

/// What mongod expands a wildcard index to for one queried path
/// (`expandWildcardIndexEntry`): the entry `{$_path: 1, <path>: 1}`, multikey
/// where the index holds a metadata key for the path or a prefix of it.
fn wildcard_entry(index: &Arc<IndexInfo>, path: &str) -> Arc<IndexInfo> {
    let base = index.wildcard.as_ref().expect("a wildcard index");
    let mut prefix = String::new();
    let mut arrays = Vec::new();
    for part in path.split('.') {
        if !prefix.is_empty() {
            prefix.push('.');
        }
        prefix.push_str(part);
        if base.arrays.contains(&prefix) {
            arrays.push(prefix.clone());
        }
    }
    let dir = index.key.values().next().cloned().unwrap_or(Bson::Int32(1));
    let mut key = Document::new();
    key.insert("$_path", 1);
    key.insert(path, dir);
    Arc::new(IndexInfo {
        name: index.name.clone(),
        key,
        unique: false,
        sparse: false,
        partial: index.partial.clone(),
        multikey_paths: vec![vec![], arrays],
        version: index.version,
        collation: index.collation.clone(),
        plain: false,
        geo: None,
        wildcard: Some(Arc::new(super::node::Wildcard {
            projection: base.projection.clone(),
            arrays: base.arrays.clone(),
            path: Some(path.to_string()),
        })),
    })
}

/// The uses of one index for a conjunction: a wildcard index is one use
/// per queried path it serves, as mongod plans it.
fn uses_of(
    index: &Arc<IndexInfo>,
    leaves: &[Leaf],
    parts: &[Document],
    q: &Query,
) -> Vec<IndexUse> {
    let Some(wild) = index.wildcard.as_ref().filter(|_| index.is_wildcard_base()) else {
        return vec![use_index(index, leaves, parts, q)];
    };
    let mut paths: Vec<&str> = Vec::new();
    for leaf in leaves {
        if wild.projection.covers(&leaf.path)
            && wildcard_fits(&leaf.pred)
            && !paths.contains(&leaf.path.as_str())
        {
            paths.push(&leaf.path);
        }
    }
    // The expansion is over the paths in `FieldRef` order, which is the
    // order its plans are tried in (measured 8.2.11: a tie goes to the
    // path that sorts first, not the first in the query).
    paths.sort_by(|a, b| a.split('.').cmp(b.split('.')));
    paths
        .into_iter()
        .map(|path| use_index(&wildcard_entry(index, path), leaves, parts, q))
        .filter(|u| u.leads)
        .collect()
}

fn scan_of(u: &IndexUse, parts: &[Document], backward: bool) -> IxScan {
    IxScan {
        index: u.index.clone(),
        bounds: u.bounds.clone(),
        backward,
        filter: assemble(u.on_key.iter().map(|i| parts[*i].clone()).collect()),
        no_bounds: false,
        whole: !u.leads,
    }
}

/// What the FETCH over one index rechecks, in mongod's order for it: by
/// path, not the canonical order (`secantus_core::residual_key`) -- unless
/// the index bounds no predicate at all (a hint to an unrelated index), when
/// mongod 8.2.1 keeps the canonical order (RM-422). Predicates tagged with
/// the index come first (RM-439).
fn residual(u: &IndexUse, parts: &[Document]) -> Option<Document> {
    let mut rest: Vec<(usize, Document)> = parts
        .iter()
        .enumerate()
        .filter(|(i, _)| !u.covered.contains(i) && !u.on_key.contains(i))
        .map(|(i, c)| (i, c.clone()))
        .collect();
    if !u.covered.is_empty() || !u.on_key.is_empty() || !u.tagged.is_empty() {
        // `sortUsingTags`: a predicate tagged with the index (one that set
        // bounds but the bounds do not decide, an inexact `$exists`) sorts
        // ahead of the untagged ones, which follow by path (RM-439).
        rest.sort_by_key(|(i, c)| (!u.tagged.contains(i), secantus_core::residual_key(c)));
    }
    assemble(rest.into_iter().map(|(_, c)| c).collect())
}

/// When the leading fields are a few points and the sort follows them, mongod
/// scans each point and merges: one scan per combination of points, in the
/// order the scan meets them (a field read against its direction meets its
/// points greatest first).
fn explode(u: &IndexUse, sort: &Document, parts: &[Document]) -> Option<Node> {
    let fields = u.index.fields();
    let mut probe = u.bounds.clone();
    let mut exploded = Vec::new();
    let mut count = 1;
    for pos in 0..u.bounds.len() {
        let list = &u.bounds[pos];
        // `explodeForSort`: only the leading fields whose bounds are points
        // explode; a range ends them (measured 8.2.11: `{b: {$in: [1, 2,
        // 3]}, a: {$gt: 10}}` sorted on `b` over `a_1_b_1` sorts the scan's
        // keys, it does not merge three scans).
        if !list.iter().all(Interval::is_point) {
            break;
        }
        if list.len() < 2 {
            continue;
        }
        if count * list.len() > 200 {
            return None;
        }
        count *= list.len();
        probe[pos] = vec![list[0].clone()];
        exploded.push(pos);
        let Some(backward) = serves_sort(&u.index, &probe, sort) else {
            continue;
        };
        let mut combos: Vec<Vec<Vec<Interval>>> = vec![u.bounds.clone()];
        for &at in &exploded {
            let mut points = u.bounds[at].clone();
            if (fields[at].1 < 0) != backward {
                points.reverse();
            }
            combos = combos
                .into_iter()
                .flat_map(|c| {
                    points.iter().map(move |point| {
                        let mut next = c.clone();
                        next[at] = vec![point.clone()];
                        next
                    })
                })
                .collect();
        }
        let inputs = combos
            .into_iter()
            .map(|bounds| {
                Node::IxScan(IxScan {
                    bounds,
                    ..scan_of(u, parts, backward)
                })
            })
            .collect();
        return Some(Node::SortMerge {
            pattern: sort.clone(),
            inputs,
        });
    }
    None
}

/// How much the fixed order of preference likes an index for a query: more
/// bounded fields, then the field a `distinct` wants, more point bounds,
/// the sort, a covered projection, a shorter key. It decides between
/// candidates mongod's trial scores alike, and stands in for the trial
/// when the collection is not counted.
type Preference = (usize, bool, usize, bool, bool, usize);

fn preference(u: &IndexUse, parts: &[Document], q: &Query) -> Preference {
    let sorts = sort_of(q).is_some_and(|s| {
        !collation_differs(&u.index, q.collation) && serves_sort(&u.index, &u.bounds, s).is_some()
    });
    let covers = q
        .projection
        .is_some_and(|p| residual(u, parts).is_none() && covers(&u.index, p));
    let points = u
        .bounds
        .iter()
        .filter(|list| matches!(list.as_slice(), [one] if one.is_point()))
        .count();
    let holds = q.wanted.is_some_and(|f| u.index.position(f).is_some());
    (
        u.bounded_fields,
        holds,
        points,
        sorts,
        covers,
        usize::MAX - u.index.key.len(),
    )
}

/// Every index a predicate of the conjunction bounds, in catalog order.
fn candidate_uses(
    leaves: &[Leaf],
    parts: &[Document],
    q: &Query,
    catalog: &[Arc<IndexInfo>],
) -> Vec<IndexUse> {
    let uses: Vec<IndexUse> = catalog
        .iter()
        .filter(|ix| {
            // A geo index is read for a geo predicate on its field only.
            eligible(ix, q) && (ix.geo.is_none() || geo_leaves(ix, leaves).next().is_some())
                || ix.is_wildcard_base() && ix.partial.as_ref().is_none_or(|p| (q.implies)(p))
        })
        .flat_map(|ix| uses_of(ix, leaves, parts, q))
        .filter(|u| u.leads)
        .collect();
    // A unique index pinned to one key by its predicates is the plan, with
    // nothing to try against it (measured 8.2.11: `{_id: 3, a: 3}` with
    // `a_1` reports no rejected plans).
    let unique_point = |u: &IndexUse| {
        u.index.unique
            && !u.index.is_multikey()
            && u.bounds
                .iter()
                .all(|list| matches!(list.as_slice(), [one] if one.is_point()))
    };
    if uses.iter().any(unique_point) {
        uses.into_iter().filter(unique_point).collect()
    } else {
        uses
    }
}

/// The index access for a conjunction, if any index is worth reading.
fn index_access(parts: &[Document], q: &Query, catalog: &[Arc<IndexInfo>]) -> Option<Access> {
    let leaves = leaves_of(parts);
    let hinted = match &q.hint {
        Hint::Index(name) => catalog.iter().find(|ix| &ix.name == name),
        _ => None,
    };
    let chosen: IndexUse = if let Some(index) = hinted {
        let mut uses = uses_of(index, &leaves, parts, q);
        match uses.len() {
            0 => use_index(index, &leaves, parts, q),
            _ => {
                let best = uses.iter().map(|u| preference(u, parts, q)).max()?;
                let at = uses.iter().position(|u| preference(u, parts, q) == best)?;
                uses.swap_remove(at)
            }
        }
    } else {
        let mut uses = candidate_uses(&leaves, parts, q, catalog);
        let best = uses.iter().map(|u| preference(u, parts, q)).max()?;
        let at = uses.iter().position(|u| preference(u, parts, q) == best)?;
        uses.swap_remove(at)
    };
    Some(access_of(chosen, parts, q))
}

/// The access through one index: its scan, and what the fetch rechecks.
fn access_of(chosen: IndexUse, parts: &[Document], q: &Query) -> Access {
    let sort = sort_of(q);
    let fetch_filter = residual(&chosen, parts);
    let index = chosen.index.clone();
    if q.min_max {
        // A sort the index gives walks it that way between `min` and `max`
        // (mongod 8.2.11: `IXSCAN ... backward` for the reverse of the key).
        let backward = sort
            .filter(|_| !collation_differs(&index, q.collation))
            .and_then(|s| serves_sort(&index, &chosen.bounds, s));
        return Access {
            scan: Node::IxScan(IxScan {
                no_bounds: true,
                backward: backward.unwrap_or(false),
                ..IxScan::full(index.clone())
            }),
            fetch_filter,
            indexed: true,
            index: Some(index),
            sorted: backward.is_some(),
            subplan: false,
        };
    }
    // An index keyed by another collation holds strings in another order.
    if let Some(s) = sort.filter(|_| !collation_differs(&index, q.collation)) {
        if let Some(backward) = serves_sort(&index, &chosen.bounds, s) {
            return Access {
                scan: Node::IxScan(scan_of(&chosen, parts, backward)),
                fetch_filter,
                indexed: true,
                index: Some(index),
                sorted: true,
                subplan: false,
            };
        }
        if let Some(merge) = explode(&chosen, s, parts) {
            return Access {
                scan: merge,
                fetch_filter,
                indexed: true,
                index: Some(index),
                sorted: true,
                subplan: false,
            };
        }
    }
    Access {
        scan: Node::IxScan(scan_of(&chosen, parts, false)),
        fetch_filter,
        indexed: true,
        index: Some(index),
        sorted: false,
        subplan: false,
    }
}

/// An `$or` whose every branch has an index, and the filter its fetch
/// rechecks when the whole `$or` is one scan.
///
/// mongod sorts the branches by the index each reads. Branches that are one
/// predicate on an index's leading field share ONE scan of that index, their
/// bounds joined; a branch of several predicates keeps a scan of its own.
/// The scans come out index by index, except that the last index's shared
/// scan follows every branch that has its own.
/// The scans of an `$or`, one per branch (or one per index the
/// single-predicate branches share), and what the documents they return are
/// rechecked against. `outer` holds the clauses beside the `$or`, which
/// mongod pushes down into each branch (measured 8.2.11: `{$or: [{a: 3},
/// {c: 5}], b: 2}` reads `a_1_b_1` with `b` bounded for the first branch);
/// a branch reads only an index on a field the branch itself names. An
/// outer clause every branch's bounds decide is rechecked nowhere (8.2.11:
/// `{$or: [{y: 0}, {y: 1, x: {$gt: 3}}], x: {$lt: 10}}` over `y_1_x_1`
/// reads `PROJECTION_DEFAULT <- OR <- IXSCAN`, no FETCH); any other is
/// rechecked above the `OR`, not in a branch. The third value says, per
/// outer clause, whether it is rechecked.
fn or_access(
    branches: &[Bson],
    outer: &[Document],
    q: &Query,
    catalog: &[Arc<IndexInfo>],
) -> Option<(Node, Option<Document>, Vec<bool>)> {
    // The subplanner plans each branch in the classic engine, whatever
    // runs the query (measured 8.2.11, 2026-11-08: a rooted `$or` behind a
    // `$group` caches its branch with `worksType: "works"`).
    let plain = Query {
        sort: None,
        projection: None,
        hint: Hint::None,
        sbe: false,
        ..q.clone()
    };
    // Per index, in catalog order: the single-predicate branches reading it,
    // each its scan, whether its bounds decide it, and the branch.
    type Branch = (IxScan, bool, Document);
    let mut shared: Vec<(usize, Vec<Branch>)> = Vec::new();
    let mut own = Vec::new();
    let mut rechecked = vec![false; outer.len()];
    for branch in branches {
        let doc = branch.as_document()?;
        let mut parts = clauses(doc);
        let own_parts = parts.len();
        let named: BTreeSet<String> = parts
            .iter()
            .flat_map(|p| p.keys().filter(|k| !k.starts_with('$')).cloned())
            .collect();
        parts.extend(outer.iter().cloned());
        let combined = if outer.is_empty() {
            doc.clone()
        } else {
            assemble(parts.clone())?
        };
        let usable: Vec<Arc<IndexInfo>> = if outer.is_empty() {
            catalog.to_vec()
        } else {
            catalog
                .iter()
                .filter(|ix| ix.fields().iter().any(|(f, _)| named.contains(*f)))
                .cloned()
                .collect()
        };
        // mongod plans each branch on its own, with its multi-planner when
        // several plans could answer it (the subplanner, 8.2.11): the
        // branch's own trial, which `explain` does not list.
        let alone = Query {
            canonical: &combined,
            limit: 0,
            skip: 0,
            ..plain.clone()
        };
        let tried_alone = plain.trial.and_then(|trial| {
            let kept = trial.ranked.replace(Vec::new());
            let found = tried(&parts, &alone, &usable, trial, assemble(parts.clone()));
            *trial.ranked.borrow_mut() = kept;
            found
        });
        let mut access = match tried_alone {
            Some(found) => found,
            None => index_access(&parts, &plain, &usable)?,
        };
        // An outer clause this branch's bounds leave to a filter is
        // rechecked above the `OR`.
        if !outer.is_empty() {
            let mut left: Vec<Document> = access.fetch_filter.iter().flat_map(clauses).collect();
            if let Node::IxScan(ix) = &access.scan {
                left.extend(ix.filter.iter().flat_map(clauses));
            }
            if !matches!(access.scan, Node::IxScan(_)) {
                rechecked.iter_mut().for_each(|r| *r = true);
            }
            for (k, clause) in outer.iter().enumerate() {
                if left.contains(clause) {
                    rechecked[k] = true;
                }
            }
            access.fetch_filter = access.fetch_filter.and_then(|f| {
                assemble(
                    clauses(&f)
                        .into_iter()
                        .filter(|c| !outer.contains(c))
                        .collect(),
                )
            });
        }
        let at = access
            .index
            .as_ref()
            .and_then(|ix| catalog.iter().position(|c| Arc::ptr_eq(c, ix)));
        match (access.scan, at) {
            (Node::IxScan(ix), Some(at))
                if own_parts == 1
                    && named.contains(ix.index.fields()[0].0)
                    && ix
                        .bounds
                        .iter()
                        .skip(1)
                        .all(|l| l.len() == 1 && l[0].is_all()) =>
            {
                let exact = access.fetch_filter.is_none() && ix.filter.is_none();
                match shared.iter_mut().find(|(i, _)| *i == at) {
                    Some((_, list)) => list.push((ix, exact, doc.clone())),
                    None => shared.push((at, vec![(ix, exact, doc.clone())])),
                }
            }
            (scan, at) => own.push((
                at.unwrap_or(usize::MAX),
                match access.fetch_filter {
                    Some(f) => Node::fetch(Some(f), scan),
                    None => scan,
                },
            )),
        }
    }
    shared.sort_by_key(|(at, _)| *at);
    // A branch with a scan of its own comes in the order of the index it
    // reads too (measured 8.2.11), not the order it was written in.
    own.sort_by_key(|(at, _)| *at);
    let own: Vec<Node> = own.into_iter().map(|(_, node)| node).collect();
    let mut scans: Vec<(Node, Option<Document>)> = shared
        .into_iter()
        .map(|(_, list)| {
            let inexact: Vec<Bson> = list
                .iter()
                .filter(|(_, exact, _)| !exact)
                .map(|(_, _, doc)| Bson::Document(doc.clone()))
                .collect();
            let filter = (!inexact.is_empty()).then(|| {
                let all: Vec<Bson> = list
                    .iter()
                    .map(|(_, _, doc)| Bson::Document(doc.clone()))
                    .collect();
                // A lone branch of a larger `$or` keeps the `$or` around its
                // clause, as the filter of its FETCH (measured 8.2.11,
                // 2026-11-23: `{$or: [{a: null}, {b: 2}]}` filters `{$or:
                // [{a: {$eq: null}}]}`).
                match all.as_slice() {
                    [Bson::Document(one)] if branches.len() == 1 => one.clone(),
                    _ => doc! { "$or": all },
                }
            });
            let mut list = list.into_iter().map(|(ix, _, _)| ix);
            let mut scan = list.next().expect("a group holds a scan");
            for other in list {
                let mut joined = std::mem::take(&mut scan.bounds[0]);
                joined.extend(other.bounds[0].iter().cloned());
                scan.bounds[0] = union(joined);
            }
            scan.filter = None;
            (Node::IxScan(scan), filter)
        })
        .collect();
    if own.is_empty() && scans.len() == 1 {
        return scans.pop().map(|(scan, filter)| (scan, filter, rechecked));
    }
    let fetched = |(scan, filter): (Node, Option<Document>)| match filter {
        Some(f) => Node::fetch(Some(f), scan),
        None => scan,
    };
    let last = scans.pop().map(fetched);
    let mut inputs: Vec<Node> = scans.into_iter().map(fetched).collect();
    inputs.extend(own);
    inputs.extend(last);
    if let Some(merged) = sort_of(q).and_then(|s| merge_branches(&inputs, s, q)) {
        return Some((merged, None, rechecked));
    }
    Some((Node::Or(inputs), None, rechecked))
}

/// The `$or`'s branches merged in the sort's order, when every branch's
/// scan gives it: mongod 8.2.11 plans `SORT_MERGE` over the branches (each
/// walked the way that gives the sort, a FETCH above one that filters) in
/// place of the `OR` and the blocking `SORT` above it.
fn merge_branches(inputs: &[Node], sort: &Document, q: &Query) -> Option<Node> {
    let mut merged = Vec::with_capacity(inputs.len());
    for input in inputs {
        let mut input = input.clone();
        let ix = match &mut input {
            Node::IxScan(ix) => ix,
            Node::Fetch { input, .. } => match input.as_mut() {
                Node::IxScan(ix) => ix,
                _ => return None,
            },
            _ => return None,
        };
        if collation_differs(&ix.index, q.collation) {
            return None;
        }
        ix.backward = serves_sort(&ix.index, &ix.bounds, sort)?;
        merged.push(input);
    }
    Some(Node::SortMerge {
        pattern: sort.clone(),
        inputs: merged,
    })
}

fn access(q: &Query, catalog: &[Arc<IndexInfo>]) -> Access {
    let always = q.canonical.len() == 1 && q.canonical.contains_key("$alwaysTrue");
    // Through `assemble`, which leaves out the twins `$expr` derives.
    let whole = || {
        (!q.canonical.is_empty() && !always)
            .then(|| assemble(clauses(q.canonical)))
            .flatten()
    };
    let collscan = |subplan: bool, backward: bool| Access {
        scan: Node::CollScan {
            filter: whole(),
            backward,
        },
        fetch_filter: None,
        indexed: false,
        index: None,
        sorted: false,
        subplan,
    };
    if let Hint::Natural(backward) = q.hint {
        return collscan(false, backward);
    }
    let parts = clauses(q.canonical);
    // A `$near` is the geo index's ring search (RM-578).
    if let Some(found) = geo_near_access(&parts, q, catalog) {
        return found;
    }
    let mut only_or = false;
    // A filter that is only an `$or` is planned branch by branch.
    if let [only] = parts.as_slice() {
        if let Ok(parsed) = only.get_array("$or") {
            if q.hint == Hint::None {
                if let Some((scan, fetch_filter, _)) = or_access(parsed, &[], q, catalog) {
                    let sorted = matches!(scan, Node::SortMerge { .. });
                    return Access {
                        scan,
                        fetch_filter,
                        indexed: true,
                        index: None,
                        sorted,
                        subplan: true,
                    };
                }
                // A branch has no index: the whole `$or` is one filter.
                only_or = true;
            }
        }
    }
    // Several plans could answer it: mongod tries them against each other.
    if let Some(trial) = q.trial {
        if let Some(found) = tried(&parts, q, catalog, trial, whole()) {
            return found;
        }
    }
    if let Some(found) = index_access(&parts, q, catalog) {
        return found;
    }
    // An `$or` beside other clauses: the branches' indexes, the rest rechecked.
    for (i, clause) in parts.iter().enumerate() {
        let Ok(branches) = clause.get_array("$or") else {
            continue;
        };
        let mut rest: Vec<Document> = parts
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, c)| c.clone())
            .collect();
        if let Some((scan, recheck, kept)) = or_access(branches, &rest, q, catalog) {
            let mut kept = kept.into_iter();
            rest.retain(|_| kept.next().unwrap_or(true));
            rest.extend(recheck);
            let sorted = matches!(scan, Node::SortMerge { .. });
            return Access {
                scan,
                fetch_filter: assemble(rest),
                indexed: true,
                index: None,
                sorted,
                subplan: false,
            };
        }
    }
    // No predicate has an index: one that returns the sort order still helps.
    if let Some(found) = sort_provider(q, catalog, whole(), only_or) {
        return found;
    }
    collscan(only_or, false)
}

/// mongod's plan of a `$near` / `$nearSphere`: the ring search of a geo
/// index whose leading field the predicate names, GEO_NEAR_2D or
/// GEO_NEAR_2DSPHERE, the query's other clauses a FETCH's filter above it
/// (measured 8.2.11: `FETCH {z: {$eq: 1}} <- GEO_NEAR_2D`). The stage
/// returns documents, so there is no FETCH without them. A `2d` index is
/// preferred to a `2dsphere` one, as the order's search prefers it
/// (`geo::check_near_hint` has the same compatibility rule); a hint
/// restricts the choice to the index it names.
fn geo_near_access(parts: &[Document], q: &Query, catalog: &[Arc<IndexInfo>]) -> Option<Access> {
    let near_clause = |c: &Document| {
        c.len() == 1
            && c.values().next().is_some_and(|v| {
                matches!(v, Bson::Document(d) if d.contains_key("$near") || d.contains_key("$nearSphere"))
            })
    };
    let at = parts.iter().position(near_clause)?;
    let found = crate::near::find_near(&parts[at]).ok()??;
    let leaves = leaves_of(parts);
    let mut candidates: Vec<(&Arc<IndexInfo>, bool, BTreeSet<usize>)> = catalog
        .iter()
        .filter(|ix| match &q.hint {
            Hint::Index(name) => &ix.name == name,
            _ => true,
        })
        .filter_map(|ix| {
            let geo = ix.geo.as_ref().filter(|g| g.geo_field() == found.field)?;
            let sphere = geo.is_sphere();
            if if sphere {
                !found.near.spherical
            } else {
                found.near.geojson
            } {
                return None;
            }
            // The fields before the geo one are bounded by the query's
            // predicates, and so are those after it that the stage's scans
            // can decide: the near stage reads them as its bounds.
            let collated = collation_differs(ix, q.collation);
            let mut absorbed: BTreeSet<usize> = BTreeSet::new();
            for (pos, (field, _)) in ix.fields().into_iter().enumerate() {
                if pos == geo.geo_at {
                    continue;
                }
                let ctx = Ctx {
                    multikey: !ix.multikey_paths[pos].is_empty(),
                    sparse: ix.sparse,
                    collated,
                    plain_strings: ix.collation.is_none(),
                    keys_only: q.keys_only,
                };
                let mut bounded = false;
                for leaf in leaves.iter().filter(|l| l.path == field && !l.derived) {
                    if let Some((_, tight)) = leaf_bounds(&leaf.pred, ctx) {
                        bounded = true;
                        if tight != Tight::FetchFilter {
                            absorbed.insert(leaf.clause);
                        }
                    }
                }
                if !bounded && pos < geo.geo_at {
                    return None;
                }
            }
            Some((ix, sphere, absorbed))
        })
        .collect();
    candidates.sort_by_key(|(_, sphere, _)| *sphere);
    let (index, sphere, absorbed) = candidates.into_iter().next()?;
    let rest: Vec<Document> = parts
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != at && !absorbed.contains(i))
        .map(|(_, c)| c.clone())
        .collect();
    let fetch_filter = if rest.is_empty() {
        None
    } else {
        assemble(rest)
    };
    Some(Access {
        scan: Node::GeoNear {
            index: index.clone(),
            sphere,
            clause: parts[at].clone(),
        },
        indexed: fetch_filter.is_some(),
        fetch_filter,
        index: None,
        sorted: false,
        subplan: false,
    })
}

/// Why mongod's planner finds no GEO_NEAR plan for a `$near`, which it
/// words two ways (measured 8.2.11, 2026-11-24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NearRefusal {
    /// "unable to find index for $geoNear query": no index of the
    /// collection is relevant to the query, since none has a predicate on
    /// its leading field (or the one with a geo field leading is the wrong
    /// kind).
    NoIndex,
    /// "No query solutions": an index is relevant, but no predicate can be
    /// assigned to its leading field to bound the scan (`$ne`, `$nin`,
    /// `$not`, `$nor` or an `$or`, a predicate on a path it keys that
    /// cannot bound it).
    NoSolutions,
}

impl NearRefusal {
    /// The reason mongod gives.
    pub(crate) fn message(self) -> &'static str {
        match self {
            NearRefusal::NoIndex => "unable to find index for $geoNear query",
            NearRefusal::NoSolutions => "No query solutions",
        }
    }
}

/// Whether a predicate of the query is one mongod's planner counts as
/// relevant to an index keying its path: the operators it never tags an
/// index for are `$size`, the bit tests and an object-form `$elemMatch`.
fn near_relevant(pred: &Document) -> bool {
    let Some(op) = pred.keys().next() else {
        return false;
    };
    match op.as_str() {
        "$size" | "$bitsAllSet" | "$bitsAllClear" | "$bitsAnySet" | "$bitsAnyClear" | "$where"
        | "$text" => false,
        "$elemMatch" => pred
            .get_document("$elemMatch")
            .is_ok_and(|inner| inner.keys().any(|k| k.starts_with('$'))),
        _ => true,
    }
}

/// Whether anything under `clause` is a predicate relevant to `path`.
fn near_mentions(clause: &Bson, path: &str) -> bool {
    match clause {
        Bson::Document(d) => d.iter().any(|(k, v)| {
            if k == path {
                return match v {
                    Bson::Document(pred)
                        if pred.keys().next().is_some_and(|o| o.starts_with('$')) =>
                    {
                        pred.iter().any(|(op, arg)| {
                            let one = doc! { op.clone(): arg.clone() };
                            near_relevant(&one)
                        })
                    }
                    _ => true,
                };
            }
            near_mentions(v, path)
        }),
        Bson::Array(list) => list.iter().any(|c| near_mentions(c, path)),
        _ => false,
    }
}

/// mongod's refusal of a `$near` (or `$geoNear`) on `found`'s field when no
/// index can plan it, or `None` when one can. A geo index whose geo field
/// does not lead is relevant only when a predicate of the query bounds its
/// LEADING field (the fields between are free; measured over
/// `{z: 1, t: 1, pts: "2dsphere"}`, where `z` alone plans and `t` alone
/// does not); when only negations, an `$or` or the like touch it, mongod
/// reports [`NearRefusal::NoSolutions`], and a hint overrides all of it
/// (the caller's affair).
pub(crate) fn near_refusal(
    canonical: &Document,
    field: &str,
    near: &secantus_core::geoshape::Near,
    catalog: &[Arc<IndexInfo>],
    implies: &dyn Fn(&Document) -> bool,
    collation: Option<&Collation>,
) -> Option<NearRefusal> {
    let parts = clauses(canonical);
    // A query that matches nothing by its form (`$in: []`) is planned as
    // EOF, whatever its indexes.
    if parts.iter().any(|c| c.contains_key("$alwaysFalse")) {
        return None;
    }
    let leaves = leaves_of(&parts);
    let mut refusal = NearRefusal::NoIndex;
    let mut any = false;
    for ix in catalog {
        let Some(geo) = ix.geo.as_ref().filter(|g| g.geo_field() == field) else {
            continue;
        };
        if if geo.is_sphere() {
            !near.spherical
        } else {
            near.geojson
        } {
            continue;
        }
        any = true;
        if ix.partial.as_ref().is_some_and(|p| !implies(p)) {
            continue;
        }
        if geo.geo_at == 0 {
            return None;
        }
        let lead = ix.fields()[0].0;
        let ctx = Ctx {
            multikey: !ix.multikey_paths[0].is_empty(),
            sparse: ix.sparse,
            collated: collation_differs(ix, collation),
            plain_strings: ix.collation.is_none(),
            keys_only: false,
        };
        let mut relevant = false;
        for leaf in leaves.iter().filter(|l| l.path == lead && !l.derived) {
            // A sparse index is never read for what `$expr` derives.
            if ix.sparse
                && leaf
                    .pred
                    .keys()
                    .next()
                    .is_some_and(|k| k.starts_with("$_internalExpr"))
            {
                continue;
            }
            let negated = leaf.pred.keys().next().is_some_and(|k| k == "$not");
            if !negated && leaf_bounds(&leaf.pred, ctx).is_some() {
                return None;
            }
            relevant |= near_relevant(&leaf.pred);
        }
        // An `$or` or `$nor` over the leading field is relevant to the
        // index, though it bounds nothing.
        relevant |= parts.iter().any(|c| {
            c.keys().next().is_some_and(|k| k == "$or" || k == "$nor")
                && near_mentions(&Bson::Document(c.clone()), lead)
        });
        if relevant {
            refusal = NearRefusal::NoSolutions;
        }
    }
    any.then_some(refusal)
}

/// The walks of whole indexes that return the documents in the sort order,
/// each one checked against the query, in catalog order.
fn sort_walks(
    q: &Query,
    catalog: &[Arc<IndexInfo>],
    filter: Option<Document>,
    subplan: bool,
) -> Vec<Access> {
    let Some(sort) = sort_of(q) else {
        return Vec::new();
    };
    let full = |ix: &Arc<IndexInfo>| vec![vec![Interval::all()]; ix.key.len()];
    catalog
        .iter()
        .filter_map(|ix| {
            let usable = eligible(ix, q)
                && ix.geo.is_none()
                && !ix.sparse
                && !collation_differs(ix, q.collation);
            let backward = usable.then(|| serves_sort(ix, &full(ix), sort)).flatten()?;
            Some(Access {
                scan: Node::IxScan(IxScan {
                    backward,
                    whole: true,
                    ..IxScan::full(ix.clone())
                }),
                fetch_filter: filter.clone(),
                indexed: true,
                index: Some(ix.clone()),
                sorted: true,
                subplan,
            })
        })
        .collect()
}

/// The first walk of a whole index that returns the sort order.
fn sort_provider(
    q: &Query,
    catalog: &[Arc<IndexInfo>],
    filter: Option<Document>,
    subplan: bool,
) -> Option<Access> {
    sort_walks(q, catalog, filter, subplan).into_iter().next()
}

/// The plans mongod tries for a conjunction, in the order it lists them:
/// one per index a predicate bounds, and when none of those returns the
/// sort, a walk of every index that does. Each with its standing in the
/// fixed preference; a walk has none.
///
/// A hint leaves only the hinted index's plans: its scans and their
/// intersection, never a walk for the sort or another index (measured
/// 8.2.1: `{s: {$gte: "a", $lte: "b"}}` hinted to a multikey `s_1` is
/// still two plans, `["", "b"]` winning over `["a", {})`).
fn candidates(
    parts: &[Document],
    q: &Query,
    catalog: &[Arc<IndexInfo>],
    filter: Option<Document>,
) -> Vec<(Access, Option<Preference>)> {
    if q.min_max || q.keys_only || q.wanted.is_some() {
        return Vec::new();
    }
    let hinted = match &q.hint {
        Hint::None => None,
        Hint::Index(name) => match catalog.iter().find(|ix| &ix.name == name) {
            Some(index) => Some(index),
            None => return Vec::new(),
        },
        Hint::Natural(_) => return Vec::new(),
    };
    let leaves = leaves_of(parts);
    // An index whose leading field holds arrays takes the bounds of one
    // predicate on it, and mongod tries one plan per predicate (measured
    // 8.2.11: `{t: {$gt: 3, $lt: 9}}` over a multikey `t_1` is two plans,
    // `[-inf, 9)` and `(3, inf]`, in the order of the parsed query).
    let mut uses: Vec<IndexUse> = Vec::new();
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    let found = match hinted {
        Some(index) => uses_of(index, &leaves, parts, q)
            .into_iter()
            .filter(|u| u.leads)
            .collect(),
        None => candidate_uses(&leaves, parts, q, catalog),
    };
    for u in found {
        let index = u.index.clone();
        let lead = index.fields()[0].0.to_string();
        let ctx_multikey = !index.multikey_paths[0].is_empty();
        let on_lead: Vec<usize> = leaves
            .iter()
            .enumerate()
            .filter(|(_, l)| l.path == lead && !l.derived)
            .map(|(i, _)| i)
            .collect();
        if !ctx_multikey || on_lead.len() < 2 {
            uses.push(u);
            continue;
        }
        let start = uses.len();
        for &chosen in &on_lead {
            let kept: Vec<Leaf> = leaves
                .iter()
                .enumerate()
                .filter(|(i, l)| *i == chosen || l.path != lead || l.derived)
                .map(|(_, l)| l.clone())
                .collect();
            let one = use_index(&index, &kept, parts, q);
            if one.leads {
                uses.push(one);
            }
        }
        if uses.len() - start > 1 {
            groups.push(start..uses.len());
        }
    }
    let covered: Vec<BTreeSet<usize>> = uses.iter().map(|u| u.covered.clone()).collect();
    // What a hashed index decides is nothing outright (its bounds are the
    // hashes, rechecked on the document); it is intersected with the index of
    // another predicate all the same (measured 8.2.11, 2026-11-23), by the
    // predicates it was tagged with.
    let tagged: Vec<BTreeSet<usize>> = uses.iter().map(|u| u.tagged.clone()).collect();
    let mut out: Vec<(Access, Option<Preference>)> = uses
        .into_iter()
        .map(|u| {
            let standing = preference(&u, parts, q);
            (access_of(u, parts, q), Some(standing))
        })
        .collect();
    if out.is_empty() {
        if hinted.is_some() {
            return out;
        }
        // No predicate has an index: the walks that return the sort are
        // the candidates, unless an `$or` is planned on its own.
        if parts.iter().any(|p| p.contains_key("$or")) {
            return out;
        }
        return sort_walks(q, catalog, filter, false)
            .into_iter()
            .map(|w| (w, None))
            .collect();
    }
    // Two scans that each return documents in RecordId order (every field
    // a point, the index holding a document once) are also intersected,
    // pair by pair in catalog order, unless one index already decides every
    // predicate the two do (measured 8.2.11: `{a: 1, b: 1}` over `a_1` and
    // `b_1` adds `AND_SORTED`; with `a_1_b_1` beside them it does not).
    let in_order = |a: &Access| match &a.scan {
        Node::IxScan(ix) => {
            // `IndexScanNode::sortedByDiskLoc`: one key, multikey or not
            // (measured 8.2.11: `{a: 4, t: 2}` over `a_1` and a multikey
            // `t_1` adds `AND_SORTED`).
            ix.filter.is_none()
                && ix
                    .bounds
                    .iter()
                    .all(|list| matches!(list.as_slice(), [one] if one.is_point()))
        }
        _ => false,
    };
    let singles = out.len();
    // The point scans one index makes of several predicates on its arrays
    // are intersected all together (measured 8.2.11: `{t: {$all: [3, 4,
    // 5]}}` adds one AND_SORTED of three scans of `t_1`).
    let points = |a: &Access| match &a.scan {
        Node::IxScan(ix) => {
            ix.filter.is_none()
                && ix
                    .bounds
                    .iter()
                    .all(|list| matches!(list.as_slice(), [one] if one.is_point()))
        }
        _ => false,
    };
    // A front SBE runs is planned with no index intersection (measured
    // 8.2.11, 2026-11-08: `{t: {$all: [1, 2]}, a: {$lt: 50}}` behind a
    // `$group`, `t_1` multikey, tries the two `t_1` scans and `a_1`).
    let intersect = !q.sbe;
    for group in groups.iter().filter(|_| intersect) {
        if !group.clone().all(|at| points(&out[at].0)) {
            continue;
        }
        let scans = group.clone().map(|at| out[at].0.scan.clone()).collect();
        out.push((
            Access {
                scan: Node::AndSorted(scans),
                fetch_filter: assemble(parts.to_vec()),
                indexed: true,
                index: None,
                sorted: false,
                subplan: false,
            },
            None,
        ));
    }
    if hinted.is_some() {
        return out;
    }
    for x in (0..singles).filter(|_| intersect) {
        for y in x + 1..singles {
            if !in_order(&out[x].0) || !in_order(&out[y].0) {
                continue;
            }
            // Two scans of one index are intersected only all together,
            // above (an `$all` over a multikey index).
            let same = match (&out[x].0.index, &out[y].0.index) {
                // (A wildcard index's entries for two paths are one index.)
                (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.name == b.name,
                _ => false,
            };
            if same {
                continue;
            }
            // A hashed index decides nothing outright, so the predicates it
            // was tagged with stand in for the ones it decides, for every
            // index (measured 8.2.11, 2026-11-23: `{a: 3, b: 0}` over
            // `a_1`, `b_1` and `{b: 1, a: "hashed"}` adds no AND_SORTED).
            let decided = if out
                .iter()
                .any(|(a, _)| a.index.as_ref().is_some_and(|ix| ix.has_hashed()))
            {
                &tagged
            } else {
                &covered
            };
            let both: BTreeSet<usize> = decided[x].union(&decided[y]).copied().collect();
            if decided.iter().any(|c| c.is_superset(&both)) {
                continue;
            }
            let scans = vec![out[x].0.scan.clone(), out[y].0.scan.clone()];
            out.push((
                Access {
                    scan: Node::AndSorted(scans),
                    fetch_filter: assemble(parts.to_vec()),
                    indexed: true,
                    index: None,
                    sorted: false,
                    subplan: false,
                },
                None,
            ));
        }
    }
    // An `$or` beside other clauses is also read branch by branch, the
    // other clauses pushed down into each branch and rechecked above the
    // `OR`, and mongod tries that plan against the others (measured 8.2.11:
    // `{$or: [{a: 3, b: 2}, {c: 5}, {d: 1, b: 3}], d: {$ne: 5}}` keeps the
    // branches' `a_1_b_1`, `d_1_b_1` and `c_1` over `d_1_b_1` alone).
    // mongod offers every combination of the branches' indexes; this offers
    // the one each branch's own trial picks.
    if parts.len() > 1 {
        if let Some(at) = parts.iter().position(|p| p.contains_key("$or")) {
            if let Ok(branches) = parts[at].get_array("$or") {
                let mut rest: Vec<Document> = parts
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != at)
                    .map(|(_, c)| c.clone())
                    .collect();
                if let Some((scan, recheck, kept)) = or_access(branches, &rest, q, catalog) {
                    let mut kept = kept.into_iter();
                    rest.retain(|_| kept.next().unwrap_or(true));
                    rest.extend(recheck);
                    let sorted = matches!(scan, Node::SortMerge { .. });
                    out.push((
                        Access {
                            scan,
                            fetch_filter: assemble(rest),
                            indexed: true,
                            index: None,
                            sorted,
                            subplan: false,
                        },
                        None,
                    ));
                }
            }
        }
    }
    if sort_of(q).is_some() && !out.iter().any(|(a, _)| a.sorted) {
        out.extend(
            sort_walks(q, catalog, filter, false)
                .into_iter()
                .map(|w| (w, None)),
        );
    }
    // mongod's planning checks once per scan of every plan it enumerates,
    // and its enumerator is not this one (RM-360).
    if out.len() > 1 {
        secantus_core::deadline::unmatched();
    }
    out
}

/// Whether every plan mongod tries for the query reads through one index
/// (a scan, or an OR of scans of a contained `$or`): no index
/// intersection, and no rooted `$or`, whose branches are each tried apart.
pub(crate) fn single_index_candidates(q: &Query, catalog: &[Arc<IndexInfo>]) -> bool {
    let parts = clauses(q.canonical);
    if matches!(parts.as_slice(), [only] if only.contains_key("$or")) {
        return false;
    }
    candidates(&parts, q, catalog, None)
        .iter()
        .all(|(a, _)| !matches!(a.scan, Node::AndSorted(_)))
}

/// Whether mongod has more than one plan to try for the query, and so
/// which it reads depends on its trial.
pub(crate) fn contested(q: &Query, catalog: &[Arc<IndexInfo>]) -> bool {
    let parts = clauses(q.canonical);
    if let [only] = parts.as_slice() {
        // An `$or` is planned branch by branch, each with its own trial
        // where several plans could answer it (`or_access`).
        if let Ok(branches) = only.get_array("$or") {
            return branches.iter().filter_map(Bson::as_document).any(|b| {
                let alone = Query {
                    canonical: b,
                    sort: None,
                    projection: None,
                    limit: 0,
                    skip: 0,
                    hint: Hint::None,
                    ..q.clone()
                };
                candidates(&clauses(b), &alone, catalog, None).len() > 1
            });
        }
    }
    candidates(&parts, q, catalog, None).len() > 1
}

/// The plan mongod keeps for a conjunction after trying its candidates
/// against each other. `None`: there is one candidate or none, or the
/// collection is not to hand to count over.
fn tried(
    parts: &[Document],
    q: &Query,
    catalog: &[Arc<IndexInfo>],
    trial: &Trial,
    filter: Option<Document>,
) -> Option<Access> {
    let mut found = candidates(parts, q, catalog, filter);
    if found.len() < 2 {
        return None;
    }
    let plans: Vec<Node> = found.iter().map(|(a, _)| wrap(a.clone(), q)).collect();
    // mongod looks the query up in its plan cache before any trial: an
    // active entry's plan runs in place of the trial, as long as it does
    // no worse than ten times the works it was cached with
    // (`internalQueryCacheEvictionRatio`) -- in SBE, ten times the keys and
    // documents it read; a branch of an `$or` takes it as it is. A plan
    // that does worse sends the entry inactive and the query to a trial
    // whose winner is cached whatever happens.
    let looked = trial.cache.and_then(|c| c.lookup(q).map(|l| (c, l)));
    let mut mode = CacheMode::Always;
    if let Some((cache, l)) = &looked {
        if l.branch {
            mode = CacheMode::Sometimes;
        }
        if let Some((sig, works)) = &l.active {
            if let Some(at) = plans.iter().position(|p| signature(p) == *sig) {
                let fits = l.branch
                    || l.trusted
                    || over_reach(trial.whole, &[&plans[at]], |docs| {
                        let within = if q.sbe {
                            super::trial::within_reads
                        } else {
                            super::trial::within
                        };
                        match within(&plans[at], &docs.env(), trial_need(q), works * 10) {
                            Some((fits, used)) => Some((Some(fits), used)),
                            None => Some((None, 0)),
                        }
                    })
                    .flatten()
                        != Some(false);
                if fits {
                    trial.ranked.borrow_mut().clear();
                    trial.cached_ran.set(!l.branch && !l.trusted);
                    cache.chose(l, sig.clone());
                    return Some(found.swap_remove(at).0);
                }
                cache.deactivate(&l.key);
                mode = CacheMode::Always;
            }
        }
    }
    // Without the cache, a walk of the sort's index against the scan of the
    // predicate's is decided without reading the documents when it can be.
    let shortcut = |found: &mut Vec<(Access, Option<Preference>)>| {
        if let [(scan, Some(_)), (walk, None)] = found.as_slice() {
            if let Some(walk_wins) = counted(scan, walk, q, trial) {
                return Some(found.swap_remove(usize::from(walk_wins)).0);
            }
        }
        None
    };
    if looked.is_none() {
        if let Some(found) = shortcut(&mut found) {
            return Some(found);
        }
    }
    let Some((at, outcome)) = pick(plans.clone(), q, trial) else {
        // The collection is not to hand to run the trial over.
        return shortcut(&mut found);
    };
    if let Some((cache, l)) = &looked {
        cache.chose(l, signature(&plans[at]));
        let ranked = outcome
            .order
            .iter()
            .map(|&i| (&plans[i], outcome.scores[i], &outcome.stats[i]))
            .collect();
        // SBE keeps what the winner read in the trial: its keys and
        // documents (`worksType: "reads"`).
        let works = if q.sbe {
            let (keys, docs) = outcome.stats[at].totals();
            keys + docs
        } else {
            outcome.works
        };
        let d = Decision { ranked, works };
        cache.store(l, q, &d, mode);
    }
    Some(found.swap_remove(at).0)
}

/// The documents mongod's trial asks a plan for
/// (`trial_period::getTrialPeriodNumToReturn`): the query's limit, at most
/// 101.
pub(crate) fn trial_need(q: &Query) -> i64 {
    if q.limit > 0 {
        q.limit.min(101)
    } else {
        101
    }
}

/// Which of `plans` mongod's trial keeps: the best score, between equals
/// the fixed preference, then the first. What each plan did is recorded
/// on `trial` for `explain`, which lists the plans as they were tried, the
/// winner swapped with the first.
fn pick(plans: Vec<Node>, q: &Query, trial: &Trial) -> Option<(usize, super::trial::Outcome)> {
    tried_by(plans, trial_need(q), trial, false)
}

/// Which of `plans`, the candidates mongod tries for a `$group` it answers
/// from a DISTINCT_SCAN, its trial keeps, recorded on `trial` as [`pick`]
/// records them. `None`: the collection is not to hand.
pub(crate) fn pick_for_group(plans: Vec<Node>, trial: &Trial) -> Option<usize> {
    tried_by(plans, 101, trial, true).map(|(at, _)| at)
}

fn tried_by(
    plans: Vec<Node>,
    need: i64,
    trial: &Trial,
    group: bool,
) -> Option<(usize, super::trial::Outcome)> {
    let all: Vec<&Node> = plans.iter().collect();
    // Counted where the plan hands documents over, past any skip (measured
    // 8.2.11: `skip: 1, limit: 2` ends the trial at two documents).
    let outcome = over_reach(trial.whole, &all, |docs| {
        let total = docs.total.unwrap_or(0);
        let outcome = super::trial::run(&plans, &docs.env(), need, total, group)?;
        let works = outcome.works;
        Some((outcome, works))
    })?;
    // The best score, mongod's tie-breaking bonuses included; between
    // plans those leave equal, the first.
    let at = outcome.order[0];
    let mut ranked: Vec<Ranked> = plans
        .into_iter()
        .enumerate()
        .map(|(i, plan)| Ranked {
            plan,
            score: outcome.base_scores[i],
            stats: outcome.stats[i].clone(),
        })
        .collect();
    ranked.swap(0, at);
    *trial.ranked.borrow_mut() = ranked;
    Some((at, outcome))
}

/// Whether mongod keeps the walk of the sort's index over `found`, the scan
/// of the predicate's index that then has to sort, counted without the
/// documents. `None`: the shapes need the documents to count.
///
/// A scan of one run of keys in an index that holds a document once, with
/// nothing left to recheck, reads a key for each document the query
/// matches, and one more work to find the run over. The sort then takes a
/// work for each document it hands over and one to report it is done. A
/// walk of such an index reads a key for each document of the collection,
/// and one work more. The plan that runs out first is kept; the walk when
/// both run out together, since it does not sort.
fn counted(found: &Access, walk: &Access, q: &Query, trial: &Trial) -> Option<bool> {
    let need = q.skip + if q.limit > 0 { q.limit } else { 101 };
    let once = |a: &Access| matches!(&a.scan, Node::IxScan(ix) if !ix.index.is_multikey() && ix.filter.is_none());
    let one_run = matches!(&found.scan, Node::IxScan(ix) if {
        let mut open = false;
        ix.bounds.iter().all(|list| match list.as_slice() {
            [one] => {
                let fits = !open || one.is_all();
                open |= !one.is_point();
                fits
            }
            _ => false,
        })
    });
    let whole = walk
        .index
        .as_ref()
        .is_some_and(|ix| !ix.sparse && ix.partial.is_none());
    let counted = once(found) && once(walk) && one_run && whole && found.fetch_filter.is_none();
    let matched = trial
        .matched
        .map(|m| m as i64)
        .filter(|m| counted && *m < need)?;
    let (walk_works, sort_works) = (trial.total? as i64 + 1, 2 * matched + 2);
    Some(walk_works <= sort_works)
}

fn truthy(v: &Bson) -> bool {
    match v {
        Bson::Boolean(b) => *b,
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Double(n) => *n != 0.0,
        _ => false,
    }
}

/// Whether the index holds every field an inclusion projection returns.
fn covers(index: &IndexInfo, projection: &Document) -> bool {
    covers_with(projection, &|field| index.holds_value(field))
}

/// mongod's `QuerySolutionNode::fetched` for the scan an access reads: a
/// FETCH is, and an OR or a SORT_MERGE when every branch is (mongod
/// 8.2.11: `OR <- [FETCH <- IXSCAN, FETCH <- IXSCAN]` takes no FETCH above
/// it).
fn fetched(node: &Node) -> bool {
    match node {
        Node::Fetch { .. } => true,
        Node::Or(inputs) | Node::SortMerge { inputs, .. } => {
            !inputs.is_empty() && inputs.iter().all(fetched)
        }
        _ => false,
    }
}

/// mongod's `QuerySolutionNode::hasField`: whether everything `node`
/// returns carries `field`. A fetched branch carries every field, an index
/// scan the fields its index holds, and an OR or a SORT_MERGE a field each
/// of its branches carries (mongod 8.2.11 answers `{_id: 0, x: 1}` from an
/// `OR` of `FETCH <- IXSCAN w_1` and `IXSCAN y_1_x_1` with no FETCH above
/// it).
fn has_field(node: &Node, field: &str) -> bool {
    match node {
        Node::IxScan(ix) => ix.index.holds_value(field),
        Node::Fetch { .. } => true,
        Node::Or(inputs) | Node::SortMerge { inputs, .. } => {
            !inputs.is_empty() && inputs.iter().all(|i| has_field(i, field))
        }
        _ => false,
    }
}

/// Whether what a scan returns carries every field an inclusion projection
/// returns, `holds` saying which fields it carries.
fn covers_with(projection: &Document, holds: &dyn Fn(&str) -> bool) -> bool {
    if projection.is_empty() {
        return false;
    }
    let mut id_out = false;
    // `{_id: 0}` alone is an exclusion projection, never answered from the
    // keys; `{_id: 1}` is, by an index that holds `_id` (mongod 8.2.11:
    // `find({y: 0}, {_id: 1})` over `{y: 1, _id: 1}` is
    // `PROJECTION_COVERED <- IXSCAN`, RM-573).
    if projection.iter().all(|(f, v)| f == "_id" && !truthy(v)) {
        return false;
    }
    for (field, value) in projection {
        // A computed field is answered from the keys when every field it
        // reads is (mongod 8.2.11: `{k: {$add: ["$x", 1]}}` over `{y: 1,
        // x: 1}` is `PROJECTION_DEFAULT <- IXSCAN`, RM-570).
        let expression = match value {
            Bson::Document(d) => d.len() == 1 && d.keys().all(|k| k.starts_with('$')),
            Bson::String(s) => s.starts_with('$'),
            _ => false,
        };
        if expression {
            let mut reads = std::collections::BTreeSet::new();
            if !secantus_core::expression_fields(value, &mut reads)
                || !reads.iter().all(|f| holds(f))
            {
                return false;
            }
            continue;
        }
        if matches!(value, Bson::Document(_) | Bson::Array(_) | Bson::String(_)) {
            return false;
        }
        if field == "_id" && !truthy(value) {
            id_out = true;
            continue;
        }
        if !truthy(value) || !holds(field) {
            return false;
        }
    }
    id_out || holds("_id")
}

/// mongod's name for a projection: `PROJECTION_DEFAULT` for one that
/// computes a value (a literal, a string, an operator) or names a dotted or
/// positional path, `PROJECTION_COVERED` for one that answers from the keys
/// of one index scan, else `PROJECTION_SIMPLE`.
fn projection_name(projection: &Document, covered: bool) -> &'static str {
    let general = projection
        .iter()
        .any(|(f, v)| f.contains('.') || !secantus_core::projection::is_flag_value(v));
    if general {
        "PROJECTION_DEFAULT"
    } else if covered {
        "PROJECTION_COVERED"
    } else {
        "PROJECTION_SIMPLE"
    }
}

/// The single equality a point lookup needs: `{path: {$eq: scalar}}`.
pub(super) fn point_lookup(canonical: &Document) -> Option<(&str, &Bson)> {
    if canonical.len() != 1 {
        return None;
    }
    let (path, pred) = canonical.iter().next()?;
    let pred = pred.as_document()?;
    if path.starts_with('$') || pred.len() != 1 {
        return None;
    }
    let value = pred.get("$eq")?;
    let scalar = !matches!(
        value,
        Bson::Null | Bson::Undefined | Bson::Array(_) | Bson::RegularExpression(_)
    );
    scalar.then_some((path.as_str(), value))
}

fn never(q: &Query) -> bool {
    q.canonical.len() == 1 && q.canonical.contains_key("$alwaysFalse")
}

/// A FETCH that rechecks nothing is dropped where no document is needed.
fn unfetched(node: Node) -> Node {
    match node {
        Node::Fetch {
            filter: None,
            input,
        } => *input,
        Node::Subplan(inner) => Node::Subplan(Box::new(unfetched(*inner))),
        other => other,
    }
}

/// The plan for `count`.
pub(crate) fn count(q: &Query, catalog: &[Arc<IndexInfo>]) -> Node {
    let _timing = timing();
    let node = count_plan(q, catalog);
    planning_checks(&node, false);
    node
}

fn count_plan(q: &Query, catalog: &[Arc<IndexInfo>]) -> Node {
    if q.canonical.is_empty() && q.hint == Hint::None {
        return Node::FastCount;
    }
    if never(q) {
        return Node::Count(Box::new(Node::Eof("predicateEvalsToFalse")));
    }
    if let (Hint::None, Some(("_id", value))) = (&q.hint, point_lookup(q.canonical)) {
        return Node::Count(Box::new(Node::IdHack(value.clone())));
    }
    let bare = Query {
        sort: None,
        projection: None,
        skip: 0,
        limit: 0,
        keys_only: true,
        ..q.clone()
    };
    let node = match unfetched(wrap(access(&bare, catalog), &bare)) {
        Node::IxScan(ix) if ix.filter.is_none() && ix.single_range().is_some() => {
            Node::CountScan(ix)
        }
        other => other,
    };
    Node::Count(Box::new(node))
}

/// The plan for `distinct` on `key`. `asked`: the command gave a query, empty
/// or not once normalised.
///
/// mongod 8.2.11's rules (measured 2026-10-16):
/// - a query whose normal form is empty (`{$and: [{}]}`) is still a query:
///   nothing bounds an index, and it scans the collection;
/// - with a query, an index on which the key is multikey serves no
///   DISTINCT_SCAN: its keys are the array's elements, where the query
///   selects documents;
/// - an index with a collation holds collation keys, which cannot be read
///   back as values: its DISTINCT_SCAN fetches the first document of each
///   key, and nothing is projected.
pub(crate) fn distinct(q: &Query, catalog: &[Arc<IndexInfo>], key: &str, asked: bool) -> Node {
    let _timing = timing();
    let node = distinct_plan(q, catalog, key, asked);
    planning_checks(&node, !asked);
    node
}

fn distinct_plan(q: &Query, catalog: &[Arc<IndexInfo>], key: &str, asked: bool) -> Node {
    if never(q) {
        return Node::Eof("predicateEvalsToFalse");
    }
    let bare = Query {
        sort: None,
        projection: None,
        skip: 0,
        limit: 0,
        wanted: Some(key),
        ..q.clone()
    };
    let express = plan(&bare, catalog);
    if matches!(express, Node::Express { .. }) || matches!(q.hint, Hint::Natural(_)) {
        return express;
    }
    let scan_of_key = |ix: IxScan, width: usize| {
        if ix.index.collation.is_some() {
            return Node::DistinctScan(ix, width, true);
        }
        Node::Projection {
            name: if key.contains('.') {
                "PROJECTION_DEFAULT"
            } else {
                "PROJECTION_COVERED"
            },
            spec: if key == "_id" {
                doc! { "_id": 1 }
            } else {
                doc! { "_id": 0, key: 1 }
            },
            input: Box::new(Node::DistinctScan(ix, width, false)),
        }
    };
    let parts = clauses(q.canonical);
    if parts.is_empty() && asked {
        return wrap(access(&bare, catalog), &bare);
    }
    if parts.is_empty() {
        let hinted = |ix: &&Arc<IndexInfo>| match &q.hint {
            Hint::Index(name) => &ix.name == name,
            _ => true,
        };
        let led = catalog.iter().filter(hinted).find(|ix| {
            ix.plain
                && ix.partial.is_none()
                && ix.position(key) == Some(0)
                && !collation_differs(ix, q.collation)
        });
        return match led {
            Some(ix) => scan_of_key(IxScan::full(ix.clone()), 1),
            None => wrap(access(&bare, catalog), &bare),
        };
    }
    let distinct_of = |found: Access| {
        let position = found.index.as_ref().and_then(|ix| ix.position(key));
        match (&found.scan, position, &found.fetch_filter) {
            (Node::IxScan(ix), Some(pos), None)
                if ix.filter.is_none()
                    && !collation_differs(&ix.index, q.collation)
                    && ix.index.multikey_paths.get(pos).is_none_or(Vec::is_empty) =>
            {
                scan_of_key(ix.clone(), pos + 1)
            }
            _ => wrap(found, &bare),
        }
    };
    // Several indexes a predicate bounds: mongod tries each, the one that
    // holds the key as a DISTINCT_SCAN.
    if let (Hint::None, Some(trial)) = (&q.hint, q.trial) {
        let leaves = leaves_of(&parts);
        let uses = candidate_uses(&leaves, &parts, &bare, catalog);
        if uses.len() > 1 {
            let mut plans: Vec<Node> = uses
                .into_iter()
                .map(|u| distinct_of(access_of(u, &parts, &bare)))
                .collect();
            if let Some((at, _)) = pick(plans.clone(), &bare, trial) {
                return plans.swap_remove(at);
            }
        }
    }
    distinct_of(access(&bare, catalog))
}

/// The plan for an update or a delete. `returns`: findAndModify, which hands
/// one document back and honours a sort.
pub(crate) fn write(
    q: &Query,
    catalog: &[Arc<IndexInfo>],
    delete: bool,
    multi: bool,
    upsert: bool,
    returns: bool,
) -> Node {
    let _timing = timing();
    let node = write_plan(q, catalog, delete, multi, upsert, returns);
    planning_checks(&node, false);
    node
}

fn write_plan(
    q: &Query,
    catalog: &[Arc<IndexInfo>],
    delete: bool,
    multi: bool,
    upsert: bool,
    returns: bool,
) -> Node {
    // An `_id` point write skips the planner only under the `_id` index's
    // own collation: under another, a number's bounds still read the index
    // (UPDATE, FETCH, `IXSCAN { _id: 1 }`) and a string's do not (COLLSCAN),
    // an upsert included (measured 8.2.11, 2026-11-09, RM-376).
    if let (Hint::None, Some(("_id", value))) = (&q.hint, point_lookup(q.canonical)) {
        let id_index = catalog
            .iter()
            .find(|ix| ix.name == "_id_" && !collation_differs(ix, q.collation));
        match id_index {
            Some(_) if upsert => {
                return Node::Update(Box::new(Node::IdHack(value.clone())));
            }
            Some(index) => {
                return Node::ExpressWrite {
                    index: index.clone(),
                    value: value.clone(),
                    delete,
                }
            }
            None => {}
        }
    }
    // A findAndModify's sort, or an update's (8.0): a SORT of one document
    // over the read, or a read in the sort's order and a LIMIT of one
    // (measured 8.2.11, 2026-11-09, RM-376).
    let sorted = sort_of(q).is_some();
    let read = Query {
        sort: q.sort,
        projection: None,
        skip: 0,
        limit: i64::from(sorted),
        ..q.clone()
    };
    let input = if never(q) {
        Node::Eof("predicateEvalsToFalse")
    } else {
        match wrap(access(&read, catalog), &read) {
            // The one document a write sorts for: mongod labels it so.
            Node::Sort {
                pattern,
                limit,
                keeps_metadata,
                input,
                ..
            } => Node::Sort {
                pattern,
                limit,
                on_keys: true,
                keeps_metadata,
                input,
            },
            other => other,
        }
    };
    let node = if delete {
        Node::Delete {
            batched: multi,
            input: Box::new(input),
        }
    } else {
        Node::Update(Box::new(input))
    };
    match q.projection.filter(|p| returns && !p.is_empty()) {
        Some(p) => Node::Projection {
            name: "PROJECTION_DEFAULT",
            spec: p.clone(),
            input: Box::new(node),
        },
        None => node,
    }
}

/// The plan for a `find`-shaped query over a collection that exists.
pub(crate) fn plan(q: &Query, catalog: &[Arc<IndexInfo>]) -> Node {
    let _timing = timing();
    let node = find_plan(q, catalog);
    planning_checks(&node, false);
    node
}

fn find_plan(q: &Query, catalog: &[Arc<IndexInfo>]) -> Node {
    if never(q) {
        return Node::Eof("predicateEvalsToFalse");
    }
    // An `_id` point lookup under `returnKey` is an IDHACK at any batch
    // size, under a limit or a sort, which it does not run (measured
    // 8.2.11, RM-567: `RETURN_KEY <- IDHACK`, and `RETURN_KEY <-
    // SORT_KEY_GENERATOR <- IDHACK` for a projection's `{$meta: "sortKey"}`);
    // not with a hint, a skip, `min` / `max`, `showRecordId` or a collation
    // the `_id` index does not have, which scan the index.
    if let (true, Some(("_id", value))) = (
        q.return_key && q.hint == Hint::None && q.skip == 0 && !q.min_max && !q.show_record_id,
        point_lookup(q.canonical),
    ) {
        let id_index = catalog.iter().any(|ix| {
            ix.plain
                && ix.key.len() == 1
                && ix.position("_id") == Some(0)
                && !collation_differs(ix, q.collation)
        });
        if id_index {
            let mut node = Node::IdHack(value.clone());
            let sort_key = q.projection.is_some_and(|p| {
                p.values()
                    .any(|v| secantus_core::projection::meta_spec(v) == Some("sortKey"))
            });
            if q.sort.is_some_and(|s| !s.is_empty()) && sort_key {
                node = Node::SortKeyGenerator(Box::new(node));
            }
            return Node::ReturnKey(Box::new(node));
        }
    }
    let simple = q.hint == Hint::None && q.skip == 0 && !q.min_max && !q.return_key;
    if let (true, Some((path, value))) = (simple && !q.show_record_id, point_lookup(q.canonical)) {
        let index = catalog.iter().find(|ix| {
            ix.unique
                && ix.plain
                && ix.partial.is_none()
                && ix.key.len() == 1
                && ix.position(path) == Some(0)
                && !(collation_differs(ix, q.collation)
                    && matches!(value, Bson::String(_) | Bson::Document(_)))
        });
        if let Some(index) = index {
            if q.batch_size && path == "_id" {
                return Node::IdHack(value.clone());
            }
            if !q.batch_size {
                return Node::Express {
                    index: index.clone(),
                    value: value.clone(),
                };
            }
        }
    }
    wrap(access(q, catalog), q)
}

/// Whether mongod's planner runs `projection` below a blocking sort by
/// `sort`, so the sort orders the projected documents (measured 8.2.11,
/// 2026-11-09): a projection that computes nothing -- every leaf an include
/// or exclude flag, a positional `$` and a dotted path included, where a
/// literal, an expression, `$slice`, `$elemMatch` or `$meta` keeps it above
/// -- and that hands on whole every path the sort orders by (`{a: 1}` hands
/// on `a.b`, `{"a.b": 1}` does not hand on `a`, `{"a.c": 0}` hands on `a.b`).
/// Whether a read's blocking sort keeps each member's metadata beside its
/// document. mongod's `canUseSimpleSort` (`planner_analysis.cpp`) takes the
/// `simple` sort, which discards all of it but the sort key, only when the
/// query depends on no metadata but `sortKey` and does not keep the
/// RecordId; otherwise the sort is `default` (measured 8.2.11, RM-472:
/// `showRecordId`, and a projection reading `{$meta: "recordId"}`,
/// `"indexKey"`, `"randVal"`, `"searchScore"` or `"geoNearDistance"`, sort
/// `default`; one reading only `{$meta: "sortKey"}` sorts `simple`).
/// `projection` is the one the plan runs (none under `returnKey`).
fn keeps_metadata(q: &Query, projection: Option<&Document>) -> bool {
    fn reads_metadata(v: &Bson) -> bool {
        match v {
            Bson::Document(d) => match d.get("$meta") {
                Some(Bson::String(kind)) if d.len() == 1 => kind != "sortKey",
                _ => d.values().any(reads_metadata),
            },
            Bson::Array(items) => items.iter().any(reads_metadata),
            _ => false,
        }
    }
    // `returnKey` keeps each member's index key, which the `simple` sort
    // would discard (measured 8.2.11, RM-566: `RETURN_KEY <- SORT default`).
    q.return_key || q.show_record_id || projection.is_some_and(|p| p.values().any(reads_metadata))
}

fn runs_below_sort(projection: &Document, sort: &Document) -> bool {
    fn leaves(spec: &Document, prefix: &str, out: &mut Vec<(String, bool)>) -> bool {
        spec.iter().all(|(k, v)| {
            let path = format!("{prefix}{k}");
            match v {
                v if secantus_core::projection::is_flag_value(v) => {
                    out.push((path, secantus_core::projection::flag_truthy(v)));
                    true
                }
                Bson::Document(d) if d.keys().all(|k| !k.starts_with('$')) => {
                    leaves(d, &format!("{path}."), out)
                }
                _ => false,
            }
        })
    }
    let mut flags = Vec::new();
    if !leaves(projection, "", &mut flags) {
        return false;
    }
    let within = |path: &str, of: &str| path == of || path.starts_with(&format!("{of}."));
    // `{_id: 1}` alone includes; beside an exclusion it only keeps `_id`.
    let inclusion = flags.iter().any(|(p, on)| *on && p != "_id")
        || flags.iter().all(|(p, on)| *on && p == "_id");
    sort.keys().all(|k| {
        if inclusion {
            let id_unnamed = !flags.iter().any(|(p, _)| p == "_id");
            (id_unnamed && within(k, "_id")) || flags.iter().any(|(p, on)| *on && within(k, p))
        } else {
            !flags
                .iter()
                .any(|(p, on)| !*on && (within(k, p) || within(p, k)))
        }
    })
}

/// Sort, paging and projection around the scan, in mongod's nesting.
fn wrap(a: Access, q: &Query) -> Node {
    let sort = sort_of(q);
    // `returnKey` ignores the projection: mongod plans a RETURN_KEY in its
    // place (`QueryPlannerAnalysis::analyzeDataAccess`).
    let q_projection = q.projection.filter(|_| !q.return_key);
    let blocking = sort.filter(|_| !a.sorted);
    // A field the index holds provides its sort unless its own path is
    // multikey: `IndexScanNode::computeProperties` asks each path, not the
    // index (measured 8.2.11, RM-477: `{y: 1, x: 1}`, `y` multikey, sorts
    // `{x: 1}` on the keys below the FETCH).
    let in_key = |s: &Document, ix: &IndexInfo| {
        s.keys().all(|f| {
            ix.position(f).is_some_and(|at| {
                !ix.is_hashed(at) && ix.multikey_paths.get(at).is_none_or(Vec::is_empty)
            })
        })
    };
    // A sort on fields the index holds runs on the keys, below the fetch;
    // over an `$or` of scans, when every scan's index holds them (mongod
    // 8.2.11: `FETCH <- SORT <- OR <- IXSCAN...`).
    fn keys_hold(
        node: &Node,
        s: &Document,
        in_key: &dyn Fn(&Document, &IndexInfo) -> bool,
    ) -> bool {
        match node {
            Node::IxScan(ix) => in_key(s, &ix.index),
            // A fetched branch carries every field (mongod 8.2.11's
            // `hasField`): an OR of it and index scans that hold the sort's
            // fields sorts below the FETCH (RM-575).
            Node::Fetch { .. } => true,
            Node::Or(inputs) => inputs.iter().all(|i| keys_hold(i, s, in_key)),
            _ => false,
        }
    }
    let sort_on_keys = blocking.is_some_and(|s| {
        a.fetch_filter.is_none()
            && match &a.scan {
                // An `$or` of one index's predicates is one scan of their
                // intervals, planned with no `Access::index`: its keys sort
                // below the FETCH all the same (mongod 8.2.11: `SUBPLAN <-
                // FETCH <- SORT default <- IXSCAN`, RM-557).
                Node::IxScan(ix) => a
                    .index
                    .as_ref()
                    .map_or(in_key(s, &ix.index), |ix| in_key(s, ix)),
                // An OR whose every branch fetched returns documents.
                or @ Node::Or(_) => !fetched(or) && keys_hold(or, s, &in_key),
                _ => false,
            }
    });
    // A projection the scan's keys answer needs no FETCH: one index's, or
    // an `$or`'s whose every branch carries what it returns, a fetched
    // branch carrying everything (mongod 8.2.11's `analyzeProjection`:
    // `SUBPLAN <- PROJECTION_DEFAULT <- OR <- IXSCAN...`, no FETCH).
    let covered = a.indexed
        && a.fetch_filter.is_none()
        && match (&a.index, q_projection) {
            (Some(ix), Some(p)) => covers(ix, p) && blocking.is_none_or(|s| in_key(s, ix)),
            (None, Some(p)) => {
                !fetched(&a.scan)
                    && covers_with(p, &|f| has_field(&a.scan, f))
                    && (blocking.is_none() || sort_on_keys)
            }
            _ => false,
        };
    let absorbed = (q.limit > 0).then_some(q.limit + q.skip);
    let keeps_metadata = keeps_metadata(q, q_projection);
    let sort_node = |input: Node, on_keys: bool| Node::Sort {
        pattern: blocking.cloned().unwrap_or_default(),
        limit: absorbed,
        on_keys,
        keeps_metadata,
        input: Box::new(input),
    };

    // A projection that keeps every path of a blocking sort without a limit
    // runs below it, and the sort orders what it projects (measured 8.2.11:
    // `SORT <- PROJECTION_SIMPLE <- COLLSCAN`; covered, `SORT <-
    // PROJECTION_COVERED <- IXSCAN`, the keys projected before they sort).
    let below = blocking.is_some_and(|s| {
        absorbed.is_none()
            && !q.return_key
            && !q.show_record_id
            && q_projection.is_some_and(|p| !p.is_empty() && runs_below_sort(p, s))
    });
    let keys_projected = covered && sort_on_keys && below;
    // Only one index scan's keys answer a covered projection: over an OR
    // or a SORT_MERGE of scans it is mongod's general one
    // (`produceCoveredKeyObj` wants a single leaf).
    let merged = covered && matches!(a.scan, Node::Or(_) | Node::SortMerge { .. });
    let projection = |p: &Document, input: Node| {
        if q.show_record_id {
            let mut spec = p.clone();
            spec.insert("$recordId", doc! { "$meta": "recordId" });
            return Node::Projection {
                name: "PROJECTION_DEFAULT",
                spec,
                input: Box::new(input),
            };
        }
        Node::Projection {
            name: if merged {
                "PROJECTION_DEFAULT"
            } else {
                projection_name(p, covered)
            },
            spec: p.clone(),
            input: Box::new(input),
        }
    };

    let mut node = a.scan;
    let mut skipped = q.skip == 0;
    let mut sorted = blocking.is_none();
    if a.indexed {
        if sort_on_keys && !keys_projected {
            node = sort_node(node, true);
            sorted = true;
        }
        if !skipped && sorted && a.fetch_filter.is_none() {
            node = Node::Skip(q.skip, Box::new(node));
            skipped = true;
        }
        // Under `returnKey` the keys are returned without a FETCH unless a
        // residual filter or a sort the keys do not hold reads the
        // documents (measured 8.2.11: `RETURN_KEY <- SORT <- FETCH <-
        // IXSCAN`, RM-470).
        if q.return_key {
            if a.fetch_filter.is_some() || !sorted {
                node = Node::fetch(a.fetch_filter, node);
            }
        } else if !covered && !(a.fetch_filter.is_none() && fetched(&node)) {
            // An OR whose every branch fetched returns documents already
            // (mongod 8.2.11: `SUBPLAN <- OR <- [FETCH <- IXSCAN...]`).
            node = Node::fetch(a.fetch_filter, node);
        }
    }
    let mut projected = false;
    if below && !sorted {
        if let Some(p) = q_projection {
            node = projection(p, node);
            projected = true;
        }
    }
    if !sorted {
        node = sort_node(node, keys_projected);
    }
    if !skipped {
        node = Node::Skip(q.skip, Box::new(node));
    }
    if let Some(p) = q_projection.filter(|p| !projected && !p.is_empty()) {
        node = projection(p, node);
    } else if q.show_record_id && !projected && !q.return_key {
        node = Node::Projection {
            name: "PROJECTION_DEFAULT",
            spec: doc! { "$recordId": { "$meta": "recordId" } },
            input: Box::new(node),
        };
    }
    // RETURN_KEY is the top of the plan, above its sort and skip, below a
    // limit no sort absorbed (`analyzeDataAccess`; measured 8.2.11: `RETURN_KEY
    // <- SORT <- COLLSCAN`, RM-470).
    if q.return_key {
        // A sort the index provides makes no sort keys, so a projection's
        // `{$meta: "sortKey"}` has a SORT_KEY_GENERATOR compute them
        // (measured 8.2.11: `RETURN_KEY <- SORT_KEY_GENERATOR <- IXSCAN`,
        // RM-706); a blocking SORT makes its own.
        let sort_key = q.projection.is_some_and(|p| {
            p.values()
                .any(|v| secantus_core::projection::meta_spec(v) == Some("sortKey"))
        });
        if blocking.is_none() && sort_key && q.sort.is_some_and(|s| !s.is_empty()) {
            node = super::sort_key_generator(node, None);
        }
        node = Node::ReturnKey(Box::new(node));
    }
    if q.limit > 0 && (blocking.is_none() || absorbed.is_none()) {
        node = Node::Limit(q.limit, Box::new(node));
    }
    if a.subplan {
        node = Node::Subplan(Box::new(node));
    }
    node
}
