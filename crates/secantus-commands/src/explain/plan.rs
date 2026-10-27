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
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

thread_local! {
    /// Whether the operation running on this thread times its planning,
    /// and the time it has spent (`None`: it has planned nothing yet):
    /// the `planningTimeMicros` of its `Slow query` line (RM-023).
    static PLANNING: std::cell::Cell<(bool, Option<std::time::Duration>)> =
        const { std::cell::Cell::new((false, None)) };
    /// How deep in the planner the thread is: a plan made inside another
    /// is part of the outer one's time.
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Times one call into the planner, when the outermost.
struct Timing(Option<std::time::Instant>);

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
    /// The query is an aggregate's front: a projection that keeps the sort's
    /// fields runs below a blocking sort.
    pub pipeline: bool,
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
    /// The winner's `works`.
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
        if let Node::Sort { on_keys, .. } = node {
            out.push_str(if *on_keys { ":default" } else { ":simple" });
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

/// A collection's documents, for a plan to be run over.
pub(crate) trait Whole {
    fn env(&self) -> Option<Env<'_>>;
}

/// The scan a query reads through, before sort, paging and projection.
#[derive(Clone)]
struct Access {
    scan: Node,
    fetch_filter: Option<Document>,
    /// The scan reads index keys, so documents need a FETCH.
    indexed: bool,
    /// The one index read, when there is exactly one.
    index: Option<Rc<IndexInfo>>,
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
    index: Rc<IndexInfo>,
    bounds: Vec<Vec<Interval>>,
    /// Clauses the bounds decide outright.
    covered: BTreeSet<usize>,
    /// Clauses rechecked against the key.
    on_key: BTreeSet<usize>,
    /// The leading field is bounded by a predicate.
    leads: bool,
    bounded_fields: usize,
}

fn use_index(index: &Rc<IndexInfo>, leaves: &[Leaf], parts: &[Document], q: &Query) -> IndexUse {
    let collated = collation_differs(index, q.collation);
    let mut bounds = Vec::new();
    let mut covered = BTreeSet::new();
    let mut on_key = BTreeSet::new();
    let mut leads = false;
    let mut bounded_fields = 0;
    for (pos, (field, _)) in index.fields().into_iter().enumerate() {
        let multikey = !index.multikey_paths[pos].is_empty();
        let ctx = Ctx {
            multikey,
            sparse: index.sparse,
            collated,
            plain_strings: index.collation.is_none(),
            keys_only: q.keys_only,
        };
        let mut list: Option<Vec<Interval>> = None;
        for leaf in leaves.iter().filter(|l| l.path == field) {
            // A multikey field takes the bounds of ONE predicate: two
            // predicates may be satisfied by different array elements.
            if multikey && !leaf.derived && list.is_some() {
                continue;
            }
            let Some((part, tight)) = leaf_bounds(&leaf.pred, ctx) else {
                continue;
            };
            list = Some(match list {
                Some(have) => intersect(&have, &part),
                None => part,
            });
            if leaf.derived {
                continue;
            }
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
            if pos == 0 {
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
        leads,
        bounded_fields,
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
    index.plain && index.partial.as_ref().is_none_or(|p| (q.implies)(p))
}

fn scan_of(u: &IndexUse, parts: &[Document], backward: bool) -> IxScan {
    IxScan {
        index: u.index.clone(),
        bounds: u.bounds.clone(),
        backward,
        filter: assemble(u.on_key.iter().map(|i| parts[*i].clone()).collect()),
        no_bounds: false,
    }
}

/// What the FETCH over one index rechecks, in mongod's order for it: by
/// path, not the canonical order (`secantus_core::residual_key`).
fn residual(u: &IndexUse, parts: &[Document]) -> Option<Document> {
    let mut rest: Vec<Document> = parts
        .iter()
        .enumerate()
        .filter(|(i, _)| !u.covered.contains(i) && !u.on_key.contains(i))
        .map(|(_, c)| c.clone())
        .collect();
    rest.sort_by_key(secantus_core::residual_key);
    assemble(rest)
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
    catalog: &[Rc<IndexInfo>],
) -> Vec<IndexUse> {
    let uses: Vec<IndexUse> = catalog
        .iter()
        .filter(|ix| eligible(ix, q))
        .map(|ix| use_index(ix, leaves, parts, q))
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
fn index_access(parts: &[Document], q: &Query, catalog: &[Rc<IndexInfo>]) -> Option<Access> {
    let leaves = leaves_of(parts);
    let hinted = match &q.hint {
        Hint::Index(name) => catalog.iter().find(|ix| &ix.name == name),
        _ => None,
    };
    let chosen: IndexUse = if let Some(index) = hinted {
        use_index(index, &leaves, parts, q)
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
/// a branch reads only an index on a field the branch itself names, and
/// the outer clauses are rechecked above the `OR`, not in a branch.
fn or_access(
    branches: &[Bson],
    outer: &[Document],
    q: &Query,
    catalog: &[Rc<IndexInfo>],
) -> Option<(Node, Option<Document>)> {
    let plain = Query {
        sort: None,
        projection: None,
        hint: Hint::None,
        ..q.clone()
    };
    // Per index, in catalog order: the single-predicate branches reading it,
    // each its scan, whether its bounds decide it, and the branch.
    type Branch = (IxScan, bool, Document);
    let mut shared: Vec<(usize, Vec<Branch>)> = Vec::new();
    let mut own = Vec::new();
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
        let usable: Vec<Rc<IndexInfo>> = if outer.is_empty() {
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
        // The outer clauses are rechecked above the `OR`.
        if !outer.is_empty() {
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
            .and_then(|ix| catalog.iter().position(|c| Rc::ptr_eq(c, ix)));
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
                match all.as_slice() {
                    [Bson::Document(one)] => one.clone(),
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
        return scans.pop();
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
        return Some((merged, None));
    }
    Some((Node::Or(inputs), None))
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

fn access(q: &Query, catalog: &[Rc<IndexInfo>]) -> Access {
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
    let mut only_or = false;
    // A filter that is only an `$or` is planned branch by branch.
    if let [only] = parts.as_slice() {
        if let Ok(parsed) = only.get_array("$or") {
            if q.hint == Hint::None {
                if let Some((scan, fetch_filter)) = or_access(parsed, &[], q, catalog) {
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
        if let Some((scan, recheck)) = or_access(branches, &rest, q, catalog) {
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

/// The walks of whole indexes that return the documents in the sort order,
/// each one checked against the query, in catalog order.
fn sort_walks(
    q: &Query,
    catalog: &[Rc<IndexInfo>],
    filter: Option<Document>,
    subplan: bool,
) -> Vec<Access> {
    let Some(sort) = sort_of(q) else {
        return Vec::new();
    };
    let full = |ix: &Rc<IndexInfo>| vec![vec![Interval::all()]; ix.key.len()];
    catalog
        .iter()
        .filter_map(|ix| {
            let usable = eligible(ix, q) && !ix.sparse && !collation_differs(ix, q.collation);
            let backward = usable.then(|| serves_sort(ix, &full(ix), sort)).flatten()?;
            Some(Access {
                scan: Node::IxScan(IxScan {
                    backward,
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
    catalog: &[Rc<IndexInfo>],
    filter: Option<Document>,
    subplan: bool,
) -> Option<Access> {
    sort_walks(q, catalog, filter, subplan).into_iter().next()
}

/// The plans mongod tries for a conjunction, in the order it lists them:
/// one per index a predicate bounds, and when none of those returns the
/// sort, a walk of every index that does. Each with its standing in the
/// fixed preference; a walk has none.
fn candidates(
    parts: &[Document],
    q: &Query,
    catalog: &[Rc<IndexInfo>],
    filter: Option<Document>,
) -> Vec<(Access, Option<Preference>)> {
    if q.hint != Hint::None || q.min_max || q.keys_only || q.wanted.is_some() {
        return Vec::new();
    }
    let leaves = leaves_of(parts);
    // An index whose leading field holds arrays takes the bounds of one
    // predicate on it, and mongod tries one plan per predicate (measured
    // 8.2.11: `{t: {$gt: 3, $lt: 9}}` over a multikey `t_1` is two plans,
    // `[-inf, 9)` and `(3, inf]`, in the order of the parsed query).
    let mut uses: Vec<IndexUse> = Vec::new();
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for u in candidate_uses(&leaves, parts, q, catalog) {
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
    let mut out: Vec<(Access, Option<Preference>)> = uses
        .into_iter()
        .map(|u| {
            let standing = preference(&u, parts, q);
            (access_of(u, parts, q), Some(standing))
        })
        .collect();
    if out.is_empty() {
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
    for group in &groups {
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
    for x in 0..singles {
        for y in x + 1..singles {
            if !in_order(&out[x].0) || !in_order(&out[y].0) {
                continue;
            }
            // Two scans of one index are intersected only all together,
            // above (an `$all` over a multikey index).
            let same = match (&out[x].0.index, &out[y].0.index) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                _ => false,
            };
            if same {
                continue;
            }
            let both: BTreeSet<usize> = covered[x].union(&covered[y]).copied().collect();
            if covered.iter().any(|c| c.is_superset(&both)) {
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
                if let Some((scan, recheck)) = or_access(branches, &rest, q, catalog) {
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
    out
}

/// Whether mongod has more than one plan to try for the query, and so
/// which it reads depends on its trial.
pub(crate) fn contested(q: &Query, catalog: &[Rc<IndexInfo>]) -> bool {
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
    catalog: &[Rc<IndexInfo>],
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
    // (`internalQueryCacheEvictionRatio`); a branch of an `$or` takes it
    // as it is. A plan that does worse sends the entry inactive and the
    // query to a trial whose winner is cached whatever happens.
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
                    || trial.whole.env().is_none_or(|env| {
                        super::trial::within(&plans[at], &env, trial_need(q), works * 10)
                            != Some(false)
                    });
                if fits {
                    trial.ranked.borrow_mut().clear();
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
        let d = Decision {
            ranked,
            works: outcome.works,
        };
        cache.store(l, q, &d, mode);
    }
    Some(found.swap_remove(at).0)
}

/// The documents mongod's trial asks a plan for
/// (`trial_period::getTrialPeriodNumToReturn`): the query's limit, at most
/// 101.
fn trial_need(q: &Query) -> i64 {
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
    let env = trial.whole.env()?;
    // Counted where the plan hands documents over, past any skip (measured
    // 8.2.11: `skip: 1, limit: 2` ends the trial at two documents).
    let outcome = super::trial::run(&plans, &env, trial_need(q))?;
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
    if index.is_multikey() || projection.is_empty() {
        return false;
    }
    let mut id_out = false;
    if projection.keys().all(|f| f == "_id") {
        return false;
    }
    for (field, value) in projection {
        if matches!(value, Bson::Document(_) | Bson::Array(_) | Bson::String(_)) {
            return false;
        }
        if field == "_id" && !truthy(value) {
            id_out = true;
            continue;
        }
        if !truthy(value) || index.position(field).is_none() {
            return false;
        }
    }
    id_out || index.position("_id").is_some()
}

fn projection_name(projection: &Document, covered: bool) -> &'static str {
    let general = projection
        .iter()
        .any(|(f, v)| f.contains('.') || matches!(v, Bson::Document(_) | Bson::Array(_)));
    if general {
        "PROJECTION_DEFAULT"
    } else if covered {
        "PROJECTION_COVERED"
    } else {
        "PROJECTION_SIMPLE"
    }
}

/// The single equality a point lookup needs: `{path: {$eq: scalar}}`.
fn point_lookup(canonical: &Document) -> Option<(&str, &Bson)> {
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
pub(crate) fn count(q: &Query, catalog: &[Rc<IndexInfo>]) -> Node {
    let _timing = timing();
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
pub(crate) fn distinct(q: &Query, catalog: &[Rc<IndexInfo>], key: &str, asked: bool) -> Node {
    let _timing = timing();
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
        let hinted = |ix: &&Rc<IndexInfo>| match &q.hint {
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
    catalog: &[Rc<IndexInfo>],
    delete: bool,
    multi: bool,
    upsert: bool,
    returns: bool,
) -> Node {
    let _timing = timing();
    if let (Hint::None, Some(("_id", value))) = (&q.hint, point_lookup(q.canonical)) {
        let id_index = catalog.iter().find(|ix| ix.name == "_id_");
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
    let sorted = returns && sort_of(q).is_some();
    let read = Query {
        sort: q.sort.filter(|_| returns),
        projection: None,
        skip: 0,
        limit: i64::from(sorted),
        ..q.clone()
    };
    let input = if never(q) {
        Node::Eof("predicateEvalsToFalse")
    } else {
        match wrap(access(&read, catalog), &read) {
            // The one document a findAndModify sorts for: mongod labels it so.
            Node::Sort {
                pattern,
                limit,
                input,
                ..
            } => Node::Sort {
                pattern,
                limit,
                on_keys: true,
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
pub(crate) fn plan(q: &Query, catalog: &[Rc<IndexInfo>]) -> Node {
    let _timing = timing();
    if never(q) {
        return Node::Eof("predicateEvalsToFalse");
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

/// Whether a projection hands on every top-level field `sort` orders by.
fn keeps(projection: &Document, sort: &Document) -> bool {
    let top = |k: &str| k.split('.').next().unwrap_or(k).to_string();
    let excluding = projection
        .iter()
        .filter(|(k, _)| *k != "_id")
        .all(|(_, v)| matches!(v, Bson::Boolean(false)));
    sort.keys().all(|k| {
        let field = top(k);
        match projection.get(&field) {
            Some(Bson::Boolean(on)) => *on,
            Some(_) => false,
            // Not named: an exclusion hands it on; an inclusion only `_id`.
            None => excluding || field == "_id",
        }
    })
}

/// Sort, paging and projection around the scan, in mongod's nesting.
fn wrap(a: Access, q: &Query) -> Node {
    let sort = sort_of(q);
    let blocking = sort.filter(|_| !a.sorted);
    let in_key = |s: &Document, ix: &IndexInfo| {
        !ix.is_multikey() && s.keys().all(|f| ix.position(f).is_some())
    };
    let covered = a.indexed
        && a.fetch_filter.is_none()
        && match (&a.index, q.projection) {
            (Some(ix), Some(p)) => covers(ix, p) && blocking.is_none_or(|s| in_key(s, ix)),
            _ => false,
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
            Node::Or(inputs) => inputs.iter().all(|i| keys_hold(i, s, in_key)),
            _ => false,
        }
    }
    let sort_on_keys = blocking.is_some_and(|s| {
        a.fetch_filter.is_none()
            && match &a.scan {
                Node::IxScan(_) => a.index.as_ref().is_some_and(|ix| in_key(s, ix)),
                or @ Node::Or(_) => keys_hold(or, s, &in_key),
                _ => false,
            }
    });
    let absorbed = (q.limit > 0).then_some(q.limit + q.skip);
    let sort_node = |input: Node, on_keys: bool| Node::Sort {
        pattern: blocking.cloned().unwrap_or_default(),
        limit: absorbed,
        on_keys,
        input: Box::new(input),
    };

    let mut node = a.scan;
    let mut skipped = q.skip == 0;
    let mut sorted = blocking.is_none();
    if a.indexed {
        if sort_on_keys {
            node = sort_node(node, true);
            sorted = true;
        }
        if !skipped && sorted && a.fetch_filter.is_none() {
            node = Node::Skip(q.skip, Box::new(node));
            skipped = true;
        }
        if q.return_key {
            node = Node::ReturnKey(Box::new(node));
        } else if !covered {
            node = Node::fetch(a.fetch_filter, node);
        }
    }
    // An aggregate's projection that keeps every field of a blocking sort
    // runs first (measured 8.2.11: `[{$sort: {x: 1}}, {$project: {x: 1}}]`
    // is `SORT <- PROJECTION_SIMPLE`; under a `$limit` the sort stays below).
    let under = q.pipeline
        && !covered
        && !sorted
        && absorbed.is_none()
        && blocking.is_some_and(|s| q.projection.is_some_and(|p| keeps(p, s)));
    let mut projected = false;
    if under {
        if let Some(p) = q.projection.filter(|p| !p.is_empty()) {
            node = Node::Projection {
                name: projection_name(p, covered),
                spec: p.clone(),
                input: Box::new(node),
            };
            projected = true;
        }
    }
    if !sorted {
        node = sort_node(node, false);
    }
    if !skipped {
        node = Node::Skip(q.skip, Box::new(node));
    }
    if let Some(p) = q.projection.filter(|p| !projected && !p.is_empty()) {
        node = Node::Projection {
            name: projection_name(p, covered),
            spec: p.clone(),
            input: Box::new(node),
        };
    } else if q.show_record_id && !projected {
        node = Node::Projection {
            name: "PROJECTION_DEFAULT",
            spec: doc! { "$recordId": { "$meta": "recordId" } },
            input: Box::new(node),
        };
    }
    if q.limit > 0 && (blocking.is_none() || absorbed.is_none()) {
        node = Node::Limit(q.limit, Box::new(node));
    }
    if a.subplan {
        node = Node::Subplan(Box::new(node));
    }
    node
}
