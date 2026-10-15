//! Putting a query's documents in the order mongod returns them.
//!
//! A query with no sort still comes back in an order: that of the index
//! mongod read, or of the collection. The same order decides which document
//! a `limit: 1` delete removes and which one an update without `multi`
//! changes. The store's index router picks its own way to the documents, so
//! its order is its own.
//!
//! Here the store is asked only WHICH documents match
//! (`Storage::matching_rows`), and the order comes from the plan mongod runs
//! ([`super::plan`]) walked over those documents with mongod's stage rules
//! ([`super::exec`]): an index scan returns its entries by key and then by
//! insertion, a backward scan the reverse, a multikey scan a document where
//! its first key in the bounds falls, and an `$or` its branches one after
//! another.
//!
//! A sort is the store's own comparison, as it was. The plan says only how
//! documents that compare EQUAL are left, which is the order they reached
//! the sort in. So a sort with no two documents equal needs no plan.
//!
//! The plan orders and never selects: a document the store matched and the
//! plan does not reach is still returned, after the rest.
//!
//! Two cases cost nothing beyond the read. The store reports the one index
//! its router read (`MatchedRows::via`); when that is the index the plan
//! scans, forward, and each document has one entry in it, the rows are
//! already in the plan's order. And a plan that scans the collection is the
//! order the rows come in.

use super::exec::{self, Env};
use super::geo;
use super::node::Node;
use super::plan::{self, Query};
use super::{catalog, parsed, resolve_hint};
use crate::storage::{RawHint, Storage, StorageError};
use bson::{Bson, Document};
use secantus_core::collation::Collation;
use std::cell::OnceCell;
use std::collections::BTreeSet;
use std::rc::Rc;

/// One read of a collection.
pub(crate) struct Ask<'a> {
    pub db: &'a str,
    pub coll: &'a str,
    pub filter: &'a Document,
    pub sort: Option<&'a Document>,
    pub hint: Option<RawHint<'a>>,
    pub collation: Option<&'a Collation>,
    pub vars: &'a Document,
    /// How many documents the caller takes from the front (`skip` plus
    /// `limit`); 0 is all of them. It is part of which plan mongod keeps.
    pub limit: i64,
    /// The rest of what the read asked for, printed with a `$near` mongod
    /// cannot plan.
    pub asked: crate::near::Asked<'a>,
}

/// The largest collection read whole to count what each candidate plan
/// examines. Past it the predicate's index is kept, as it is when it is
/// selective.
const TRIAL_COLLECTION: usize = 2000;

type Rows = Vec<(i64, Vec<u8>)>;

/// The stage under the ones that only pass documents along.
fn access(node: &Node) -> &Node {
    match node {
        Node::Projection { input, .. }
        | Node::Skip(_, input)
        | Node::Limit(_, input)
        | Node::ReturnKey(input)
        | Node::Subplan(input) => access(input),
        other => other,
    }
}

/// Whether the plan sorts what its scan returns.
fn has_sort(node: &Node) -> bool {
    match access(node) {
        Node::Sort { .. } => true,
        Node::Fetch { input, .. } => has_sort(input),
        _ => false,
    }
}

/// Whether the plan has an `$or` of scans. Its branches' filters say which
/// scan returns a document, so they are run, over whole documents.
fn has_or(node: &Node) -> bool {
    match access(node) {
        Node::Or(_) => true,
        Node::AndSorted(_) => false,
        Node::Fetch { input, .. } | Node::Sort { input, .. } => has_or(input),
        Node::SortMerge { inputs, .. } => inputs.iter().any(has_or),
        _ => false,
    }
}

/// The top-level fields the plan's index scans read, into `out`.
fn key_fields(node: &Node, out: &mut BTreeSet<String>) {
    match access(node) {
        Node::IxScan(ix) => {
            for field in ix.index.key.keys() {
                out.insert(field.split('.').next().unwrap_or(field).to_string());
            }
        }
        Node::Fetch { input, .. } | Node::Sort { input, .. } => key_fields(input, out),
        Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
            inputs.iter().for_each(|n| key_fields(n, out))
        }
        _ => {}
    }
}

/// The plan with nothing in it that tests a document. Every row walked
/// already matches the query, so outside an `$or` a filter passes them all,
/// and a partial index holds them all: the plan then needs only the fields
/// its indexes are keyed on.
fn untested(node: &Node) -> Node {
    match access(node) {
        Node::IxScan(ix) => {
            let mut scan = ix.clone();
            scan.filter = None;
            if scan.index.partial.is_some() {
                let mut index = (*scan.index).clone();
                index.partial = None;
                scan.index = Rc::new(index);
            }
            Node::IxScan(scan)
        }
        Node::Fetch { input, .. } => Node::Fetch {
            filter: None,
            input: Box::new(untested(input)),
        },
        Node::Sort {
            pattern,
            limit,
            on_keys,
            input,
        } => Node::Sort {
            pattern: pattern.clone(),
            limit: *limit,
            on_keys: *on_keys,
            input: Box::new(untested(input)),
        },
        Node::SortMerge { pattern, inputs } => Node::SortMerge {
            pattern: pattern.clone(),
            inputs: inputs.iter().map(untested).collect(),
        },
        other => other.clone(),
    }
}

/// Only `fields` of a stored document.
fn decode_fields(blob: &[u8], fields: &BTreeSet<String>) -> Option<Document> {
    let raw = bson::RawDocument::from_bytes(blob).ok()?;
    let mut out = Document::new();
    for elem in raw {
        let (key, val) = elem.ok()?;
        if fields.contains(key) {
            out.insert(key.to_string(), Bson::try_from(val).ok()?);
        }
    }
    Some(out)
}

/// The rows `node` returns, in the order they reach a sort or the client:
/// a sort is passed through, since the store does the sorting.
fn reached(node: &Node, env: &Env, count: usize) -> Vec<usize> {
    let union = |inputs: &[Node]| {
        let mut seen = vec![false; count];
        let mut out = Vec::new();
        for input in inputs {
            for row in reached(input, env, count) {
                if !std::mem::replace(&mut seen[row], true) {
                    out.push(row);
                }
            }
        }
        out
    };
    match access(node) {
        Node::CollScan { backward: true, .. } => (0..count).rev().collect(),
        Node::IxScan(ix) => exec::scan_rows(ix, env),
        Node::Fetch { filter, input } => {
            let mut rows = reached(input, env, count);
            if filter.is_some() {
                rows.retain(|&r| env.matches(r, filter));
            }
            rows
        }
        Node::Sort { input, .. } => reached(input, env, count),
        Node::Or(inputs) => union(inputs),
        and @ Node::AndSorted(_) => exec::rows(and, env),
        merge @ Node::SortMerge { .. } => exec::rows(merge, env),
        _ => (0..count).collect(),
    }
}

/// Whether the plan merges sorted scans. Which of two equal documents a
/// merge hands over first depends on every document it holds, so all of
/// them are placed, not only those that compare equal.
fn has_merge(node: &Node) -> bool {
    match access(node) {
        Node::SortMerge { .. } => true,
        Node::Fetch { input, .. } | Node::Sort { input, .. } => has_merge(input),
        Node::Or(inputs) => inputs.iter().any(has_merge),
        _ => false,
    }
}

/// Whether a stage of the plan runs a filter.
fn filtered(node: &Node) -> bool {
    match access(node) {
        Node::IxScan(ix) => ix.filter.is_some(),
        Node::Fetch { filter, input } => filter.is_some() || filtered(input),
        Node::Sort { input, .. } => filtered(input),
        Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
            inputs.iter().any(filtered)
        }
        Node::CollScan { filter, .. } => filter.is_some(),
        _ => false,
    }
}

/// Where the plan puts each document of the whole collection it returns,
/// by RecordId. `None`: the collection is too large to read for it, or a
/// document does not decode.
fn merge_ranks(
    storage: &dyn Storage,
    ask: &Ask,
    plan: &Node,
) -> Result<Option<std::collections::HashMap<i64, usize>>, StorageError> {
    let Some(all) = storage.collection_at_most(ask.db, ask.coll, TRIAL_COLLECTION)? else {
        return Ok(None);
    };
    let docs: Option<Vec<Document>> = all
        .iter()
        .map(|(_, blob)| secantus_core::bsondoc::from_bytes(blob.as_slice()).ok())
        .collect();
    let Some(docs) = docs else {
        return Ok(None);
    };
    let env = Env {
        docs: &docs,
        collation: ask.collation,
        skip: 0,
        limit: 0,
        multi: true,
        upsert: false,
        returns: false,
        known: None,
    };
    Ok(Some(
        reached(plan, &env, docs.len())
            .into_iter()
            .enumerate()
            .map(|(rank, at)| (all[at].0, rank))
            .collect(),
    ))
}

/// Whether rows the router read through index `via` are in the plan's
/// order as they stand: the plan scans that index forward, and a document
/// has one entry inside the scan.
fn already_in_order(plan: &Node, via: &str) -> bool {
    let scan = match access(plan) {
        Node::Fetch { input, .. } => access(input),
        other => other,
    };
    let Node::IxScan(ix) = scan else {
        return false;
    };
    if ix.index.name != via || ix.backward {
        return false;
    }
    if !ix.index.is_multikey() {
        return true;
    }
    // A multikey index: each field is one value that is in the entries as
    // mongod holds it (a null or an array is not, see `values_at`).
    ix.bounds.iter().all(|list| {
        matches!(list.as_slice(), [one] if one.is_point()
            && !matches!(one.lo, Bson::Null | Bson::Undefined | Bson::Array(_) | Bson::Document(_)))
    })
}

/// The whole of a small collection, read the first time a count needs it.
///
/// Which documents match is known, so the candidates' filters are not run
/// and the documents need only their indexed fields. A partial index tests
/// a filter of its own, on whole documents.
struct Lazy<'a> {
    storage: &'a dyn Storage,
    ask: &'a Ask<'a>,
    rows: &'a Rows,
    /// Some index is partial.
    partial: bool,
    /// The top-level fields the indexes are keyed on.
    fields: BTreeSet<String>,
    loaded: OnceCell<Option<Loaded>>,
}

/// A collection's documents, and for each whether the query matches it
/// (absent when a partial index has its own filter to run).
type Loaded = (Vec<Document>, Option<Vec<bool>>);

impl Lazy<'_> {
    fn read(&self) -> Option<Loaded> {
        let all = self
            .storage
            .collection_at_most(self.ask.db, self.ask.coll, TRIAL_COLLECTION)
            .ok()??;
        let docs: Option<Vec<Document>> = all
            .iter()
            .map(|(_, blob)| {
                if self.partial {
                    secantus_core::bsondoc::from_bytes(blob.as_slice()).ok()
                } else {
                    decode_fields(blob, &self.fields)
                }
            })
            .collect();
        let matched: BTreeSet<i64> = self.rows.iter().map(|(recordid, _)| *recordid).collect();
        let known: Vec<bool> = all.iter().map(|(r, _)| matched.contains(r)).collect();
        Some((docs?, (!self.partial).then_some(known)))
    }
}

impl plan::Whole for Lazy<'_> {
    fn env(&self) -> Option<Env<'_>> {
        let (docs, known) = self.loaded.get_or_init(|| self.read()).as_ref()?;
        Some(Env {
            docs,
            collation: self.ask.collation,
            skip: 0,
            limit: 0,
            multi: true,
            upsert: false,
            returns: false,
            known: known.as_deref(),
        })
    }
}

/// `rows` in the order `order` names them.
fn placed(rows: Rows, order: Vec<usize>) -> Rows {
    let mut slots: Vec<Option<(i64, Vec<u8>)>> = rows.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|r| slots.get_mut(r).and_then(Option::take))
        .collect()
}

/// `rows` as a blocking sort leaves them: sorted by `ask.sort` when there
/// is one, documents that compare equal in the order they came.
fn sorted_over(storage: &dyn Storage, rows: Rows, ask: &Ask) -> Result<Rows, StorageError> {
    let Some(sort) = ask.sort else {
        return Ok(rows);
    };
    let blobs: Vec<&[u8]> = rows.iter().map(|(_, b)| b.as_slice()).collect();
    let (order, _) = storage.sort_permutation(&blobs, sort, ask.collation)?;
    Ok(placed(rows, order))
}

/// Whether a sort is one the planner reads: every key a field, every value
/// exactly 1 or -1. mongod takes `-1.5` for a descending sort and then does
/// not read it from an index.
pub(crate) fn plain_sort(sort: &Document) -> bool {
    !sort.is_empty()
        && sort.iter().all(|(k, v)| {
            !k.starts_with('$')
                && match v {
                    Bson::Int32(n) => n.abs() == 1,
                    Bson::Int64(n) => n.abs() == 1,
                    Bson::Double(d) => d.abs() == 1.0,
                    _ => false,
                }
        })
}

/// The documents `ask` matches with their RecordIds, in mongod's order.
/// `None`: the store does not offer the matched set for this collection, or
/// the sort is one this does not read; the caller reads as it did before.
pub(crate) fn ordered_rows(storage: &dyn Storage, ask: &Ask) -> Result<Option<Rows>, StorageError> {
    if let Some(sort) = ask.sort {
        if !plain_sort(sort) {
            return Ok(None);
        }
    }
    let near = crate::near::find_near(ask.filter)?;
    let mut near_index = None;
    if let Some(near) = &near {
        near_index = geo::check_near_hint(storage, ask.db, ask.coll, ask.hint, near, |why| {
            near.unplannable(ask.db, ask.coll, ask.asked, why)
        })?;
        near.check_index(storage, ask.db, ask.coll, ask.asked)?;
    }
    // A hint to a geo index: the store reads such an index its own way, so
    // the matched set is asked for unhinted and ordered (and, for a scan of
    // the whole index, chosen) here.
    let hinted_geo = match &near {
        Some(_) => None,
        None => geo::hinted_geo(storage, ask.db, ask.coll, ask.hint)?,
    };
    let store_hint = if near_index.is_some() || hinted_geo.is_some() {
        None
    } else {
        ask.hint
    };
    let Some(matched) = storage.matching_rows(
        ask.db,
        ask.coll,
        ask.filter,
        store_hint,
        ask.collation,
        ask.vars,
    )?
    else {
        return Ok(None);
    };
    let (mut rows, mut via) = (matched.rows, matched.via);
    // A `$near` is the order: mongod's ring search, nearest first, and a
    // sort over what it returns.
    if let Some(near) = &near {
        if via.take().is_some() {
            rows.sort_unstable_by_key(|(recordid, _)| *recordid);
        }
        let index = match near_index {
            Some(ix) => Some(ix),
            None => geo::near_index(storage, ask.db, ask.coll, &near.field, &near.near)?,
        };
        rows = match index {
            Some(ix) => {
                match geo::near_rows(storage, ask.db, ask.coll, ask.filter, &ix, near, rows)? {
                    Ok(rows) => rows,
                    Err(rows) => near.nearest_first(rows),
                }
            }
            None => near.nearest_first(rows),
        };
        return Ok(Some(sorted_over(storage, rows, ask)?));
    }
    if let Some(index) = hinted_geo {
        rows.sort_unstable_by_key(|(recordid, _)| *recordid);
        let scan = geo::scan(storage, ask.db, ask.coll, ask.filter, Some(index), true)?;
        if let Some(ordered) = scan.and_then(|scan| scan.order(rows.clone())) {
            return Ok(Some(sorted_over(storage, ordered, ask)?));
        }
        // Not a scan this reads: as the store reads it.
        return Ok(None);
    }
    if rows.len() < 2 {
        return Ok(Some(rows));
    }
    // The sort is the store's. Past it, the plan says only how the documents
    // that compare equal are left; `tied` marks those.
    let mut sorted: Option<(Vec<usize>, Vec<bool>)> = None;
    if let Some(sort) = ask.sort {
        if via.take().is_some() {
            rows.sort_unstable_by_key(|(recordid, _)| *recordid);
        }
        let blobs: Vec<&[u8]> = rows.iter().map(|(_, b)| b.as_slice()).collect();
        let (order, tied) = storage.sort_permutation(&blobs, sort, ask.collation)?;
        if !tied.iter().any(|t| *t) {
            return Ok(Some(placed(rows, order)));
        }
        sorted = Some((order, tied));
    }

    let catalog = catalog(storage, ask.db, ask.coll, None)?;
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let hint = match resolve_hint(ask.hint, &catalog) {
        Ok(h) => h,
        // The store took the hint and the planner does not know it: read as
        // before.
        Err(_) => return Ok(None),
    };
    let query = |trial| Query {
        canonical: &canonical,
        sort: ask.sort,
        projection: None,
        skip: 0,
        limit: ask.limit,
        hint: hint.clone(),
        collation: ask.collation,
        min_max: false,
        return_key: false,
        show_record_id: false,
        batch_size: false,
        keys_only: false,
        wanted: None,
        implies: &implied,
        trial,
    };
    let mut plan = plan::plan(&query(None), &catalog);

    // A geo predicate no other index serves, and no hint: a scan of the
    // geo index.
    let elsewhere = !matches!(access(&plan), Node::CollScan { .. })
        || ask
            .hint
            .is_some_and(|h| !matches!(h, Bson::Document(d) if d.is_empty()));
    if let Some(scan) = geo::scan(storage, ask.db, ask.coll, ask.filter, None, elsewhere)? {
        rows.sort_unstable_by_key(|(recordid, _)| *recordid);
        if let Some(ordered) = scan.order(rows.clone()) {
            return Ok(Some(sorted_over(storage, ordered, ask)?));
        }
    }

    // Several plans could answer it, or the predicate's index leaves a sort
    // and a walk of another index returns it: which one mongod keeps depends
    // on its trial over the collection.
    if has_sort(&plan) || plan::contested(&query(None), &catalog) {
        let total = storage.collection_count_at_most(ask.db, ask.coll, TRIAL_COLLECTION)?;
        if let Some(total) = total {
            let whole = Lazy {
                storage,
                ask,
                rows: &rows,
                partial: catalog.iter().any(|ix| ix.partial.is_some()),
                fields: catalog
                    .iter()
                    .flat_map(|ix| ix.key.keys())
                    .map(|field| field.split('.').next().unwrap_or(field).to_string())
                    .collect(),
                loaded: OnceCell::new(),
            };
            let trial = plan::Trial {
                total,
                matched: Some(rows.len()),
                whole: &whole,
                ranked: Default::default(),
            };
            plan = plan::plan(&query(Some(&trial)), &catalog);
        }
    }

    if let Some(via) = &via {
        if already_in_order(&plan, via) {
            return Ok(Some(rows));
        }
        rows.sort_unstable_by_key(|(recordid, _)| *recordid);
    }
    // A scan of the collection is the order the rows are in.
    if sorted.is_none() {
        if let Node::CollScan { backward, .. } = access(&plan) {
            if *backward {
                rows.reverse();
            }
            return Ok(Some(rows));
        }
    }

    // The rows the plan has to place: all of them, or under a sort the ones
    // that compare equal to a neighbour.
    // The caller takes `limit` documents from the front, so only the runs
    // of equal documents that start before that are worth placing.
    let cut = match (&sorted, ask.limit) {
        (Some((order, tied)), limit) if limit > 0 => {
            let mut end = (limit as usize).min(order.len());
            while end < order.len() && tied[end] {
                end += 1;
            }
            end
        }
        _ => rows.len(),
    };
    let merged = has_merge(&plan);
    let cut = if merged { rows.len() } else { cut };
    let placing: Vec<usize> = match &sorted {
        None => (0..rows.len()).collect(),
        Some(_) if merged => (0..rows.len()).collect(),
        Some((order, tied)) => {
            let mut some: Vec<usize> = (0..cut)
                .filter(|&at| tied[at] || tied.get(at + 1).copied().unwrap_or(false))
                .map(|at| order[at])
                .collect();
            some.sort_unstable();
            some
        }
    };
    // A merge whose scans admit documents a filter then rejects holds those
    // in its heap too, and they move where equal documents come out: it is
    // run over the whole collection, filters and all.
    let whole_merge = if merged && filtered(&plan) {
        merge_ranks(storage, ask, &plan)?
    } else {
        None
    };
    let rank = if let Some(ranks) = whole_merge {
        let mut rank = vec![usize::MAX; rows.len()];
        for &row in &placing {
            rank[row] = ranks
                .get(&rows[row].0)
                .copied()
                .unwrap_or(usize::MAX / 2 + row);
        }
        rank
    } else {
        // Whole documents when an `$or` runs its branches' filters; otherwise
        // the fields the scans are keyed on, and none at all without a scan.
        let whole = has_or(&plan);
        let plan = if whole { plan } else { untested(&plan) };
        let mut fields = BTreeSet::new();
        key_fields(&plan, &mut fields);
        let mut docs: Vec<Document> = Vec::new();
        if whole || !fields.is_empty() {
            docs.reserve(placing.len());
            for &row in &placing {
                let blob = rows[row].1.as_slice();
                let decoded = if whole {
                    secantus_core::bsondoc::from_bytes(blob).ok()
                } else {
                    decode_fields(blob, &fields)
                };
                match decoded {
                    Some(d) => docs.push(d),
                    // A document that does not decode: read as before.
                    None => return Ok(None),
                }
            }
        }
        let env = Env {
            docs: &docs,
            collation: ask.collation,
            skip: 0,
            limit: 0,
            multi: true,
            upsert: false,
            returns: false,
            known: None,
        };
        // Where the plan puts each row among those it places. One it does not
        // reach goes after the rest, in insertion order: the plan never selects.
        let mut rank = vec![usize::MAX; rows.len()];
        let mut next = 0;
        for at in reached(&plan, &env, placing.len()) {
            let row = placing[at];
            if rank[row] == usize::MAX {
                rank[row] = next;
                next += 1;
            }
        }
        for &row in &placing {
            if rank[row] == usize::MAX {
                rank[row] = next;
                next += 1;
            }
        }
        rank
    };
    let order = match sorted {
        None => {
            let mut order = placing;
            order.sort_unstable_by_key(|&row| rank[row]);
            order
        }
        Some((mut order, tied)) => {
            // Each run of equal documents, in the order the plan left them.
            let mut start = 0;
            while start < cut {
                let mut end = start + 1;
                while end < order.len() && tied[end] {
                    end += 1;
                }
                order[start..end].sort_unstable_by_key(|&row| rank[row]);
                start = end;
            }
            order
        }
    };
    Ok(Some(placed(rows, order)))
}

/// The names of the indexes the plan for `ask` reads. A cursor left open
/// over that plan is killed when one of them is dropped, as mongod's is.
pub(crate) fn plan_indexes(storage: &dyn Storage, ask: &Ask) -> Vec<String> {
    let Ok(catalog) = catalog(storage, ask.db, ask.coll, None) else {
        return Vec::new();
    };
    if catalog.is_empty() {
        return Vec::new();
    }
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let Ok(hint) = resolve_hint(ask.hint, &catalog) else {
        return Vec::new();
    };
    let query = Query {
        canonical: &canonical,
        sort: ask.sort,
        projection: None,
        skip: 0,
        limit: ask.limit,
        hint,
        collation: ask.collation,
        min_max: false,
        return_key: false,
        show_record_id: false,
        batch_size: false,
        keys_only: false,
        wanted: None,
        implies: &implied,
        trial: None,
    };
    let mut names = Vec::new();
    index_names(&plan::plan(&query, &catalog), &mut names);
    names
}

/// The indexes a plan's scans read, into `out`.
fn index_names(node: &Node, out: &mut Vec<String>) {
    match access(node) {
        Node::IxScan(ix) => out.push(ix.index.name.clone()),
        Node::Express { index, .. } => out.push(index.name.clone()),
        Node::Fetch { input, .. } | Node::Sort { input, .. } => index_names(input, out),
        Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
            inputs.iter().for_each(|n| index_names(n, out))
        }
        _ => {}
    }
}

thread_local! {
    /// Set while `explain` runs a command for what it says of itself: an
    /// explained plan uses no index (measured 8.2.11).
    static UNCOUNTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` without counting the indexes its plans read.
pub(crate) fn uncounted<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNCOUNTED.with(|u| u.set(self.0));
        }
    }
    let _restore = Restore(UNCOUNTED.with(|u| u.replace(true)));
    f()
}

/// What a command does with the documents a plan reads, for counting the
/// indexes it uses.
#[derive(Clone, Copy)]
pub(crate) enum Uses<'a> {
    Find,
    Count,
    Distinct(&'a str),
    /// An update (`delete` false) or a delete; `returns`: findAndModify.
    Write {
        delete: bool,
        multi: bool,
        upsert: bool,
        returns: bool,
    },
}

/// Count one use of each index the plan for `ask` reads, as mongod counts
/// one per plan it runs, however many documents or batches it returns
/// (`$indexStats`' `accesses.ops`; measured 8.2.11: two finds through
/// `a_1` are 2, a `getMore` and an `explain` count nothing).
pub(crate) fn count_index_use(storage: &dyn Storage, ask: &Ask, uses: Uses) {
    if UNCOUNTED.with(std::cell::Cell::get) {
        return;
    }
    // A read of everything in storage order reads no index.
    if ask.filter.is_empty()
        && ask.sort.is_none_or(|s| s.is_empty())
        && ask.hint.is_none()
        && !matches!(uses, Uses::Distinct(_))
    {
        return;
    }
    let Ok(catalog) = catalog(storage, ask.db, ask.coll, None) else {
        return;
    };
    if catalog.is_empty() {
        return;
    }
    // `{_id: <value>}` with no hint or sort is a lookup in the `_id` index
    // for every command (mongod's express and IDHACK plans), when the value
    // is one no collation compares differently.
    if ask.hint.is_none()
        && ask.sort.is_none_or(|s| s.is_empty())
        && ask.filter.len() == 1
        && ask.filter.get("_id").is_some_and(|v| {
            !matches!(
                v,
                Bson::Document(_)
                    | Bson::Array(_)
                    | Bson::String(_)
                    | Bson::Symbol(_)
                    | Bson::RegularExpression(_)
            )
        })
        && catalog.iter().any(|ix| ix.name == "_id_")
    {
        storage.record_index_use(ask.db, ask.coll, &["_id_".to_string()]);
        return;
    }
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let Ok(hint) = resolve_hint(ask.hint, &catalog) else {
        return;
    };
    let query = Query {
        canonical: &canonical,
        sort: ask.sort,
        projection: None,
        skip: 0,
        limit: ask.limit,
        hint,
        collation: ask.collation,
        min_max: false,
        return_key: false,
        show_record_id: false,
        batch_size: false,
        keys_only: false,
        wanted: None,
        implies: &implied,
        trial: None,
    };
    let node = match uses {
        Uses::Find => plan::plan(&query, &catalog),
        Uses::Count => plan::count(&query, &catalog),
        Uses::Distinct(key) => plan::distinct(&query, &catalog, key),
        Uses::Write {
            delete,
            multi,
            upsert,
            returns,
        } => plan::write(&query, &catalog, delete, multi, upsert, returns),
    };
    let mut names = Vec::new();
    used_indexes(&node, &mut names);
    names.dedup();
    storage.record_index_use(ask.db, ask.coll, &names);
}

/// Every index a plan reads, into `out`: its scans, and the `_id` index a
/// lookup by `_id` reads.
fn used_indexes(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::IxScan(ix) | Node::CountScan(ix) | Node::DistinctScan(ix, _) => {
            out.push(ix.index.name.clone())
        }
        Node::Express { index, .. } | Node::ExpressWrite { index, .. } => {
            out.push(index.name.clone())
        }
        Node::IdHack(_) => out.push("_id_".to_string()),
        Node::Fetch { input, .. }
        | Node::Sort { input, .. }
        | Node::Skip(_, input)
        | Node::Limit(_, input)
        | Node::Projection { input, .. }
        | Node::ReturnKey(input)
        | Node::Subplan(input)
        | Node::Count(input)
        | Node::Update(input)
        | Node::Delete { input, .. } => used_indexes(input, out),
        Node::Or(inputs) | Node::AndSorted(inputs) | Node::SortMerge { inputs, .. } => {
            inputs.iter().for_each(|n| used_indexes(n, out))
        }
        _ => {}
    }
}

/// `Storage::find_collated`, in mongod's order.
pub(crate) fn find(storage: &dyn Storage, ask: &Ask) -> Result<Vec<Vec<u8>>, StorageError> {
    match ordered_rows(storage, ask)? {
        Some(rows) => Ok(rows.into_iter().map(|(_, blob)| blob).collect()),
        None => storage.find_collated(
            ask.db,
            ask.coll,
            ask.filter,
            ask.sort,
            ask.hint,
            ask.collation,
            ask.vars,
        ),
    }
}

/// Run `f`, a write through `storage`, over the rows `order` names.
pub(crate) fn pinned<T>(
    storage: &dyn Storage,
    order: Option<Vec<i64>>,
    f: impl FnOnce() -> T,
) -> T {
    let Some(order) = order else {
        return f();
    };
    let mut f = Some(f);
    let mut out = None;
    storage.with_candidate_order(order, &mut || {
        if let Some(run) = f.take() {
            out = Some(run());
        }
    });
    match out {
        Some(v) => v,
        // A store that did not call back: run it unpinned.
        None => (f.take().expect("the write ran or it did not"))(),
    }
}

/// How many rows a write that changes one document is handed. It takes the
/// first that still matches when it runs.
const WRITE_CANDIDATES: usize = 8;

/// The RecordIds of the first documents `ask` matches, in mongod's order,
/// for a write that changes one of them. `None`: let the store pick.
pub(crate) fn write_candidates(
    storage: &dyn Storage,
    ask: &Ask,
) -> Result<Option<Vec<i64>>, StorageError> {
    // `{_id: value}` names its document: there is no order to find.
    if ask.filter.len() == 1
        && ask
            .filter
            .get("_id")
            .is_some_and(|v| !matches!(v, Bson::Document(_) | Bson::RegularExpression(_)))
    {
        return Ok(None);
    }
    let Some(rows) = ordered_rows(storage, ask)? else {
        return Ok(None);
    };
    // Nothing matched: the store looks again under its own lock, so an
    // upsert sees what was written since.
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        rows.into_iter()
            .take(WRITE_CANDIDATES)
            .map(|(recordid, _)| recordid)
            .collect(),
    ))
}
