//! Choosing the plan mongod would report for a query.
//!
//! This reproduces the planner's RULES: which predicates bound which index,
//! what is left for the fetch to recheck, when an index serves the sort or
//! covers the projection. It does not reproduce mongod's trial runs. When
//! several indexes could serve one query the choice below is a fixed order
//! of preference, which agrees with mongod on the common shapes and is not
//! claimed to agree on the rest (`tasks/backlog.md`).

use super::bounds::{intersect, leaf_bounds, Ctx, Interval, Tight};
use super::node::{IndexInfo, IxScan, Node};
use bson::{doc, Bson, Document};
use secantus_core::collation::Collation;
use std::collections::BTreeSet;
use std::rc::Rc;

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
    /// Whether the query implies an index's partial filter.
    pub implies: &'a dyn Fn(&Document) -> bool,
}

/// The scan a query reads through, before sort, paging and projection.
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
            return parts.iter().filter_map(Bson::as_document).cloned().collect();
        }
    }
    vec![canonical.clone()]
}

/// One clause, or an `$and` of several, as mongod prints a filter.
pub(crate) fn assemble(parts: Vec<Document>) -> Option<Document> {
    match parts.len() {
        0 => None,
        1 => parts.into_iter().next(),
        _ => Some(doc! { "$and": parts.into_iter().map(Bson::Document).collect::<Vec<_>>() }),
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
        let stated = clauses(&secantus_core::canonical_match(&Bson::Document(partial.clone())));
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

fn residual(u: &IndexUse, parts: &[Document]) -> Option<Document> {
    assemble(
        parts
            .iter()
            .enumerate()
            .filter(|(i, _)| !u.covered.contains(i) && !u.on_key.contains(i))
            .map(|(_, c)| c.clone())
            .collect(),
    )
}

/// When the leading fields are a few points and the sort follows them, mongod
/// scans each point and merges: one scan per combination of points.
fn explode(u: &IndexUse, sort: &Document, parts: &[Document]) -> Option<Node> {
    let mut combos: Vec<Vec<Vec<Interval>>> = vec![u.bounds.clone()];
    for pos in 0..u.bounds.len() {
        let list = &u.bounds[pos];
        if list.len() < 2 || !list.iter().all(Interval::is_point) {
            continue;
        }
        if combos.len() * list.len() > 200 {
            return None;
        }
        combos = combos
            .into_iter()
            .flat_map(|c| {
                list.iter().map(move |point| {
                    let mut next = c.clone();
                    next[pos] = vec![point.clone()];
                    next
                })
            })
            .collect();
        if let Some(backward) = serves_sort(&u.index, &combos[0], sort) {
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
    }
    None
}

/// The index access for a conjunction, if any index is worth reading.
fn index_access(parts: &[Document], q: &Query, catalog: &[Rc<IndexInfo>]) -> Option<Access> {
    let leaves = leaves_of(parts);
    let sort = sort_of(q);
    let hinted = match &q.hint {
        Hint::Index(name) => catalog.iter().find(|ix| &ix.name == name),
        _ => None,
    };
    let chosen: IndexUse = if let Some(index) = hinted {
        use_index(index, &leaves, parts, q)
    } else {
        let mut uses: Vec<IndexUse> = catalog
            .iter()
            .filter(|ix| eligible(ix, q))
            .map(|ix| use_index(ix, &leaves, parts, q))
            .filter(|u| u.leads)
            .collect();
        if uses.is_empty() {
            return None;
        }
        // The fixed order of preference among several usable indexes.
        let score = |u: &IndexUse| {
            let sorts = sort.is_some_and(|s| serves_sort(&u.index, &u.bounds, s).is_some());
            let covers = q
                .projection
                .is_some_and(|p| residual(u, parts).is_none() && covers(&u.index, p));
            (u.bounded_fields, sorts, covers, usize::MAX - u.index.key.len())
        };
        let best = uses.iter().map(score).max()?;
        let at = uses.iter().position(|u| score(u) == best)?;
        uses.swap_remove(at)
    };
    let fetch_filter = residual(&chosen, parts);
    let index = chosen.index.clone();
    if q.min_max {
        return Some(Access {
            scan: Node::IxScan(IxScan {
                no_bounds: true,
                ..IxScan::full(index.clone())
            }),
            fetch_filter,
            indexed: true,
            index: Some(index),
            sorted: false,
            subplan: false,
        });
    }
    if let Some(s) = sort {
        if let Some(backward) = serves_sort(&index, &chosen.bounds, s) {
            return Some(Access {
                scan: Node::IxScan(scan_of(&chosen, parts, backward)),
                fetch_filter,
                indexed: true,
                index: Some(index),
                sorted: true,
                subplan: false,
            });
        }
        if let Some(merge) = explode(&chosen, s, parts) {
            return Some(Access {
                scan: merge,
                fetch_filter,
                indexed: true,
                index: Some(index),
                sorted: true,
                subplan: false,
            });
        }
    }
    Some(Access {
        scan: Node::IxScan(scan_of(&chosen, parts, false)),
        fetch_filter,
        indexed: true,
        index: Some(index),
        sorted: false,
        subplan: false,
    })
}

/// An `$or` whose every branch has an index: one scan per branch.
fn or_access(branches: &[Bson], q: &Query, catalog: &[Rc<IndexInfo>]) -> Option<Node> {
    let plain = Query {
        sort: None,
        projection: None,
        hint: Hint::None,
        ..q.clone()
    };
    let mut inputs = Vec::new();
    for branch in branches {
        let parts = clauses(branch.as_document()?);
        let access = index_access(&parts, &plain, catalog)?;
        inputs.push(match access.fetch_filter {
            Some(f) => Node::fetch(Some(f), access.scan),
            None => access.scan,
        });
    }
    Some(Node::Or(inputs))
}

fn access(q: &Query, catalog: &[Rc<IndexInfo>]) -> Access {
    let always = q.canonical.len() == 1 && q.canonical.contains_key("$alwaysTrue");
    let whole = || (!q.canonical.is_empty() && !always).then(|| q.canonical.clone());
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
    // A filter that is only an `$or` is planned branch by branch.
    if let [only] = parts.as_slice() {
        if let Ok(branches) = only.get_array("$or") {
            if q.hint == Hint::None {
                return match or_access(branches, q, catalog) {
                    Some(scan) => Access {
                        scan,
                        fetch_filter: None,
                        indexed: true,
                        index: None,
                        sorted: false,
                        subplan: true,
                    },
                    None => collscan(true, false),
                };
            }
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
        if let Some(scan) = or_access(branches, q, catalog) {
            let rest = parts
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, c)| c.clone())
                .collect();
            return Access {
                scan,
                fetch_filter: assemble(rest),
                indexed: true,
                index: None,
                sorted: false,
                subplan: false,
            };
        }
    }
    // No predicate has an index: one that returns the sort order still helps.
    if let Some(sort) = sort_of(q) {
        let full = |ix: &Rc<IndexInfo>| vec![vec![Interval::all()]; ix.key.len()];
        let found = catalog.iter().find_map(|ix| {
            let usable = eligible(ix, q)
                && !ix.sparse
                && ix.partial.is_none()
                && !collation_differs(ix, q.collation);
            let backward = usable.then(|| serves_sort(ix, &full(ix), sort)).flatten()?;
            Some((ix.clone(), backward))
        });
        if let Some((index, backward)) = found {
            return Access {
                scan: Node::IxScan(IxScan {
                    backward,
                    ..IxScan::full(index.clone())
                }),
                fetch_filter: whole(),
                indexed: true,
                index: Some(index),
                sorted: true,
                subplan: false,
            };
        }
    }
    collscan(false, false)
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

/// The plan for `distinct` on `key`.
pub(crate) fn distinct(q: &Query, catalog: &[Rc<IndexInfo>], key: &str) -> Node {
    if never(q) {
        return Node::Eof("predicateEvalsToFalse");
    }
    let bare = Query {
        sort: None,
        projection: None,
        skip: 0,
        limit: 0,
        ..q.clone()
    };
    let express = plan(&bare, catalog);
    if matches!(express, Node::Express { .. }) || matches!(q.hint, Hint::Natural(_)) {
        return express;
    }
    let scan_of_key = |ix: IxScan, width: usize| Node::Projection {
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
        input: Box::new(Node::DistinctScan(ix, width)),
    };
    let parts = clauses(q.canonical);
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
    let found = access(&bare, catalog);
    let position = found.index.as_ref().and_then(|ix| ix.position(key));
    match (&found.scan, position, &found.fetch_filter) {
        (Node::IxScan(ix), Some(pos), None)
            if ix.filter.is_none() && !collation_differs(&ix.index, q.collation) =>
        {
            scan_of_key(ix.clone(), pos + 1)
        }
        _ => wrap(found, &bare),
    }
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
    // A sort on fields the index holds runs on the keys, below the fetch.
    let sort_on_keys = blocking.is_some_and(|s| {
        a.fetch_filter.is_none()
            && matches!(a.scan, Node::IxScan(_))
            && a.index.as_ref().is_some_and(|ix| in_key(s, ix))
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
    if !sorted {
        node = sort_node(node, false);
    }
    if !skipped {
        node = Node::Skip(q.skip, Box::new(node));
    }
    if let Some(p) = q.projection.filter(|p| !p.is_empty()) {
        node = Node::Projection {
            name: projection_name(p, covered),
            spec: p.clone(),
            input: Box::new(node),
        };
    } else if q.show_record_id {
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
