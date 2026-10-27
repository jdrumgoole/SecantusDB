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
use bson::{doc, Bson, Document};
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

/// Whether the plan sorts whole documents in memory: a sort above its
/// FETCH, not one of index keys below it.
fn sorts_documents(node: &Node) -> bool {
    match access(node) {
        Node::Sort { on_keys, .. } => !on_keys,
        Node::Fetch { input, .. } => sorts_documents(input),
        _ => false,
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
    /// The trial needs whole documents and runs every filter: some index
    /// is partial (it tests a filter of its own), the query is a rooted
    /// `$or`, whose branches are each tried against their own filter, or it
    /// sorts, and a sort counts the bytes of what it holds.
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

/// The indexes `ask` may be planned over: the collection's, or the ones an
/// index filter set for its shape leaves.
pub(crate) fn ask_catalog(
    storage: &dyn Storage,
    ask: &Ask,
) -> Result<Vec<std::rc::Rc<super::IndexInfo>>, StorageError> {
    let catalog = catalog(storage, ask.db, ask.coll, None)?;
    Ok(crate::index_filters::apply(
        storage,
        ask.db,
        ask.coll,
        ask.filter,
        ask.sort,
        ask.asked.projection,
        ask.collation,
        catalog,
    )
    .0)
}

/// The collection's indexes as the planner sees them, before any index
/// filter: what mongod's plan cache discriminates by and is cleared with.
fn catalog_of(storage: &dyn Storage, ask: &Ask) -> Result<Vec<Rc<super::IndexInfo>>, StorageError> {
    catalog(storage, ask.db, ask.coll, None)
}

/// mongod's plan cache as one read of a command consults and writes it
/// (`super::plan_cache`).
struct ReadCache<'a> {
    storage: &'a dyn Storage,
    place: super::plan_cache::Place<'a>,
    ask: &'a Ask<'a>,
    /// The query as parsed, which a rooted `$or`'s branches are found in.
    canonical: Document,
    catalog: Vec<Rc<super::IndexInfo>>,
    index_filter_set: bool,
    /// The read has consulted and written the cache already: it now only
    /// reads back the plans chosen.
    read_only: std::cell::Cell<bool>,
    /// The plan the read chose for each key (the query's, and a rooted
    /// `$or`'s branches').
    chosen: std::cell::RefCell<std::collections::HashMap<String, String>>,
}

impl<'a> ReadCache<'a> {
    /// `None` for a read that keeps no plan cache: one made to learn
    /// about the plan, or of a collection with no UUID.
    fn new(
        storage: &'a dyn Storage,
        ask: &'a Ask<'a>,
        canonical: Document,
        catalog: Vec<Rc<super::IndexInfo>>,
    ) -> Result<Option<ReadCache<'a>>, StorageError> {
        if !ask.asked.cached {
            return Ok(None);
        }
        let Ok(uuid) = storage.collection_uuid(ask.db, ask.coll) else {
            return Ok(None);
        };
        let index_filter_set = crate::index_filters::apply(
            storage,
            ask.db,
            ask.coll,
            ask.filter,
            ask.sort,
            ask.asked.projection,
            ask.collation,
            Vec::new(),
        )
        .1;
        Ok(Some(ReadCache {
            storage,
            place: super::plan_cache::Place {
                store: store_address(storage),
                db: ask.db,
                coll: ask.coll,
                uuid,
                catalog: super::plan_cache::fingerprint(storage, ask.db, ask.coll, &catalog)?,
            },
            ask,
            canonical,
            catalog,
            index_filter_set,
            read_only: std::cell::Cell::new(false),
            chosen: Default::default(),
        }))
    }
}

/// A read's consultation of mongod's plan cache: the query mongod plans --
/// its skip, limit and projection as the command gave them -- through the
/// cache, as mongod plans it before reading anything, whatever the read
/// then finds. `rows`: what the query matches. The hook it hands back reads
/// the plans it chose: the order is then worked out with them.
fn consult<'a>(
    storage: &'a dyn Storage,
    ask: &'a Ask<'a>,
    rows: &Rows,
) -> Result<Option<ReadCache<'a>>, StorageError> {
    if !ask.asked.cached || ask.hint.is_some() {
        return Ok(None);
    }
    let catalog = ask_catalog(storage, ask)?;
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let query = |trial| Query {
        canonical: &canonical,
        sort: ask.sort,
        projection: ask.asked.projection,
        skip: ask.asked.skip,
        limit: ask.asked.limit,
        hint: plan::Hint::None,
        collation: ask.collation,
        min_max: false,
        return_key: false,
        show_record_id: false,
        batch_size: false,
        keys_only: false,
        wanted: None,
        pipeline: false,
        implies: &implied,
        trial,
    };
    // One plan is all mongod has: it caches nothing.
    if !plan::contested(&query(None), &catalog) {
        return Ok(None);
    }
    let full = catalog_of(storage, ask)?;
    let Some(hook) = ReadCache::new(storage, ask, canonical.clone(), full)? else {
        return Ok(None);
    };
    let whole = Lazy {
        storage,
        ask,
        rows,
        partial: catalog.iter().any(|ix| ix.partial.is_some())
            || (canonical.len() == 1 && canonical.contains_key("$or"))
            || ask.sort.is_some_and(|s| !s.is_empty()),
        fields: catalog
            .iter()
            .flat_map(|ix| ix.key.keys())
            .map(|field| field.split('.').next().unwrap_or(field).to_string())
            .collect(),
        loaded: OnceCell::new(),
    };
    let trial = plan::Trial {
        total: None,
        matched: Some(rows.len()),
        whole: &whole,
        ranked: Default::default(),
        cache: Some(&hook),
    };
    let _chosen = plan::plan(&query(Some(&trial)), &catalog);
    hook.read_only.set(true);
    Ok(Some(hook))
}

/// A plan as the plan cache's debug information holds it: the plan's stage
/// statistics, where a projection keeps no spec (mongod 8.2.11's
/// `$planCacheStats` prints `transformBy: {}`).
fn cached_stages(mut stage: Document) -> Document {
    if stage
        .get_str("stage")
        .is_ok_and(|s| s.starts_with("PROJECTION_"))
    {
        stage.insert("transformBy", Document::new());
    }
    if let Some(Bson::Document(input)) = stage.get("inputStage").cloned() {
        stage.insert("inputStage", cached_stages(input));
    }
    if let Ok(inputs) = stage.get_array("inputStages") {
        let inputs: Vec<Bson> = inputs
            .iter()
            .map(|i| match i {
                Bson::Document(d) => Bson::Document(cached_stages(d.clone())),
                other => other.clone(),
            })
            .collect();
        stage.insert("inputStages", inputs);
    }
    stage
}

/// A store's identity within the process, for the caches kept per store.
pub(crate) fn store_address(storage: &dyn Storage) -> usize {
    storage as *const dyn Storage as *const () as usize
}

/// Whether mongod caches the plan of a query (`shouldCacheQuery`): not a
/// hinted one, nor one that matches nothing by its form, nor an empty
/// query with no sort.
fn cacheable(q: &Query) -> bool {
    if q.hint != plan::Hint::None || q.min_max {
        return false;
    }
    if q.canonical.len() == 1 && q.canonical.contains_key("$alwaysFalse") {
        return false;
    }
    !(q.canonical.is_empty() && q.sort.is_none_or(|s| s.is_empty()))
}

impl plan::CacheHook for ReadCache<'_> {
    fn lookup(&self, q: &Query) -> Option<plan::Lookup> {
        let top = *q.canonical == self.canonical;
        // A rooted `$or`'s branch, which the subplanner plans and caches
        // as a query of its own.
        let branch = !top
            && self.canonical.len() == 1
            && self.canonical.get_array("$or").is_ok_and(|branches| {
                branches
                    .iter()
                    .any(|b| b.as_document() == Some(q.canonical))
            });
        if !(top || branch) || !cacheable(q) {
            return None;
        }
        let parts = super::cache_key::Parts {
            sort: if top { q.sort } else { None },
            projection: if top { self.ask.asked.projection } else { None },
            collation: q.collation,
            distinct: if top { self.ask.asked.distinct } else { None },
            sbe: false,
        };
        let filter = if top { self.ask.filter } else { q.canonical };
        let implies = |partial: &Document| self.storage.query_implies_partial(filter, partial);
        let ix = super::cache_key::Indexability {
            catalog: &self.catalog,
            collation: q.collation,
            implies: &implies,
        };
        let key = super::cache_key::key_for(q.canonical, &parts, &ix)?;
        // Read back, the plan this read chose; else the active entry's.
        let active = if self.read_only.get() {
            self.chosen
                .borrow()
                .get(&key.key)
                .map(|sig| (sig.clone(), 0))
        } else {
            super::plan_cache::get(&self.place, &key)
                .filter(|e| e.active)
                .map(|e| (e.signature, e.works))
        };
        Some(plan::Lookup {
            key,
            active,
            branch,
            trusted: self.read_only.get(),
        })
    }

    fn chose(&self, l: &plan::Lookup, signature: String) {
        if !self.read_only.get() {
            self.chosen
                .borrow_mut()
                .insert(l.key.key.clone(), signature);
        }
    }

    fn deactivate(&self, key: &super::cache_key::CacheKey) {
        if !self.read_only.get() {
            super::plan_cache::deactivate(&self.place, key);
        }
    }

    fn store(&self, l: &plan::Lookup, q: &Query, d: &plan::Decision, mode: plan::CacheMode) {
        if self.read_only.get() {
            return;
        }
        let Some(&(winner, best, winner_stats)) = d.ranked.first() else {
            return;
        };
        if mode == plan::CacheMode::Sometimes {
            let tie = d
                .ranked
                .get(1)
                .is_some_and(|(_, second, _)| (best - second).abs() < 1e-10);
            if tie || winner_stats.n_returned == 0 {
                return;
            }
        }
        let mut created_from = if l.branch {
            doc! { "query": q.canonical.clone(), "sort": {}, "projection": {} }
        } else {
            let projection: Document = self
                .ask
                .asked
                .projection
                .map(|p| {
                    p.iter()
                        .filter(|(k, _)| !k.starts_with('$'))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            doc! {
                "query": self.ask.filter.clone(),
                "sort": self.ask.sort.cloned().unwrap_or_default(),
                "projection": projection,
            }
        };
        if let Some(c) = q.collation {
            created_from.insert("collation", c.spec().clone());
        }
        if let (false, Some(key)) = (l.branch, self.ask.asked.distinct) {
            created_from.insert("distinct", doc! { "key": key });
        }
        let creation = d
            .ranked
            .iter()
            .map(|(node, _, stats)| {
                let (keys, docs) = stats.totals();
                doc! {
                    "nReturned": super::node::num(stats.n_returned),
                    "executionTimeMillisEstimate": 0_i32,
                    "totalKeysExamined": super::node::num(keys),
                    "totalDocsExamined": super::node::num(docs),
                    "executionStages": cached_stages(node.to_doc(Some(stats))),
                }
            })
            .collect();
        let e = super::cache_key::parse(q.canonical);
        let command_shape = e
            .map(|e| {
                super::cache_key::command_shape(
                    &e,
                    if l.branch { None } else { q.sort },
                    if l.branch {
                        None
                    } else {
                        self.ask.asked.projection
                    },
                    q.collation,
                )
            })
            .unwrap_or_default();
        let signature = plan::signature(winner);
        let solution = plan::solution(winner);
        super::plan_cache::set(
            &self.place,
            super::plan_cache::Entry {
                key: l.key.key.clone(),
                shape_hash: l.key.shape_hash,
                key_hash: l.key.key_hash,
                command_shape,
                active: false,
                works: d.works,
                reads: false,
                created: bson::DateTime::now(),
                created_from,
                solution_hash: super::plan_cache::solution_hash(&solution),
                signature,
                solution,
                cached_plan: cached_stages(winner.to_doc(None)),
                creation,
                scores: d.ranked.iter().map(|(_, s, _)| *s).collect(),
                index_filter_set: self.index_filter_set,
            },
        );
    }
}

/// A command that reads no rows in order (`count`, an `update` of many
/// documents, a `delete` of many) still plans its query as mongod does, and
/// so consults and writes the plan cache: the read is planned here and its
/// rows dropped. Nothing is read when one plan is all mongod has: a query no
/// two plans contest, or a count a COUNT_SCAN answers.
pub(crate) fn through_cache(
    storage: &dyn Storage,
    ask: &Ask,
    count: bool,
) -> Result<(), StorageError> {
    if !ask.asked.cached || ask.hint.is_some() || ask.filter.is_empty() {
        return Ok(());
    }
    let catalog = ask_catalog(storage, ask)?;
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let query = Query {
        canonical: &canonical,
        sort: ask.sort,
        projection: None,
        skip: 0,
        limit: ask.limit,
        hint: plan::Hint::None,
        collation: ask.collation,
        min_max: false,
        return_key: false,
        show_record_id: false,
        batch_size: false,
        keys_only: false,
        wanted: None,
        pipeline: false,
        implies: &implied,
        trial: None,
    };
    if !plan::contested(&query, &catalog) {
        return Ok(());
    }
    let count_scan = |n: &Node| match n {
        Node::Count(input) => matches!(**input, Node::CountScan(_)),
        other => matches!(other, Node::CountScan(_)),
    };
    if count && count_scan(&plan::count(&query, &catalog)) {
        return Ok(());
    }
    // What the read finds is not used: only the plan it chose is kept.
    ordered_rows(storage, ask).map(|_| ())
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

/// Whether a filter names a geo query operator anywhere.
fn mentions_geo(filter: &Document) -> bool {
    fn walk(v: &Bson) -> bool {
        match v {
            Bson::Document(d) => d.iter().any(|(k, v)| {
                matches!(
                    k.as_str(),
                    "$geoWithin" | "$geoIntersects" | "$within" | "$near" | "$nearSphere"
                ) || walk(v)
            }),
            Bson::Array(items) => items.iter().any(walk),
            _ => false,
        }
    }
    filter.iter().any(|(k, v)| {
        matches!(k.as_str(), "$geoWithin" | "$geoIntersects" | "$within") || walk(v)
    })
}

/// What a `find` asks of its plan beyond its query (`min` / `max`,
/// `returnKey`, `showRecordId`).
#[derive(Clone, Copy, Default)]
pub(crate) struct StreamFlags {
    pub min_max: bool,
    pub return_key: bool,
    pub show_record_id: bool,
}

/// The plan mongod runs for a `find` that a streaming cursor can read
/// (`stream::Stream`), or `None` to read it as before: a collection that
/// streams, no geo predicate or `$near`, a plan nothing contests (choosing
/// among several needs mongod's trial over the collection) and no
/// in-memory sort. Under a collation the plan is the collated one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_plan(
    storage: &dyn Storage,
    ask: &Ask,
    skip: i64,
    limit: i64,
    projection: Option<&Document>,
    batch_size: bool,
    flags: StreamFlags,
) -> Result<Option<Node>, StorageError> {
    if !storage.can_stream(ask.db, ask.coll)? {
        return Ok(None);
    }
    if ask.sort.is_some_and(|s| !plain_sort(s)) {
        return Ok(None);
    }
    if crate::near::find_near(ask.filter)?.is_some()
        || geo::hinted_geo(storage, ask.db, ask.coll, ask.hint)?.is_some()
    {
        return Ok(None);
    }
    let catalog = ask_catalog(storage, ask)?;
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let Ok(hint) = resolve_hint(ask.hint, &catalog) else {
        return Ok(None);
    };
    let query = Query {
        canonical: &canonical,
        sort: ask.sort,
        projection,
        skip,
        limit,
        hint,
        collation: ask.collation,
        min_max: flags.min_max,
        return_key: flags.return_key,
        show_record_id: flags.show_record_id,
        batch_size,
        keys_only: false,
        wanted: None,
        pipeline: false,
        implies: &implied,
        trial: None,
    };
    if !flags.min_max && plan::contested(&query, &catalog) {
        return Ok(None);
    }
    let plan = plan::plan(&query, &catalog);
    // A sort of whole documents the plan does in memory reads them all for
    // the first batch, on mongod too: that read is the one made before. A
    // sort of index keys below the FETCH streams (`stream::Stage::SortKeys`).
    if sorts_documents(&plan) {
        return Ok(None);
    }
    // An `$or` with a geo branch is read with each branch's geo scan
    // (RM-151), which this planner does not plan: read as before.
    if matches!(plan, Node::Subplan(_)) && mentions_geo(ask.filter) {
        return Ok(None);
    }
    // A geo index that would order a scan of the collection.
    let elsewhere = !matches!(access(&plan), Node::CollScan { .. })
        || ask
            .hint
            .is_some_and(|h| !matches!(h, Bson::Document(d) if d.is_empty()));
    if geo::scan(storage, ask.db, ask.coll, ask.filter, None, elsewhere)?.is_some() {
        return Ok(None);
    }
    Ok(Some(plan))
}

/// The documents `ask` matches with their RecordIds, in mongod's order.
/// `None`: the store does not offer the matched set for this collection, or
/// the sort is one this does not read; the caller reads as it did before.
pub(crate) fn ordered_rows(storage: &dyn Storage, ask: &Ask) -> Result<Option<Rows>, StorageError> {
    // `local.oplog.rs` is read in the oplog's own order, which its read path
    // gives, `$natural: -1` reversed: no plan stands behind it, and ordering
    // its entries here lost the reversal (`find().sort({$natural: -1})`
    // answered the oldest entry, where mongod answers the newest).
    if ask.db == "local" && ask.coll == "oplog.rs" {
        return Ok(None);
    }
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
    // mongod plans the query, through its plan cache, before it reads.
    let hook = consult(storage, ask, &rows)?;
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

    let catalog = ask_catalog(storage, ask)?;
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
        pipeline: false,
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
    if ask.hint.is_none() && ask.filter.contains_key("$or") {
        let mut inserted = rows.clone();
        inserted.sort_unstable_by_key(|(recordid, _)| *recordid);
        if let Some(ordered) = geo_or_rows(storage, ask, &inserted)? {
            return Ok(Some(sorted_over(storage, ordered, ask)?));
        }
    }
    if let Some(scan) = geo::scan(storage, ask.db, ask.coll, ask.filter, None, elsewhere)? {
        rows.sort_unstable_by_key(|(recordid, _)| *recordid);
        if let Some(ordered) = scan.order(rows.clone()) {
            return Ok(Some(sorted_over(storage, ordered, ask)?));
        }
    }

    // Several plans could answer it, or the predicate's index leaves a sort
    // and a walk of another index returns it: which one mongod keeps depends
    // on its trial over the collection.
    let sorts = has_sort(&plan);
    if sorts || plan::contested(&query(None), &catalog) {
        // The count serves only the walk against a sort, which can be
        // decided without reading the documents; otherwise reading them
        // finds a collection too large on its own.
        let total = if sorts {
            storage.collection_count_at_most(ask.db, ask.coll, TRIAL_COLLECTION)?
        } else {
            None
        };
        if total.is_some() || !sorts {
            let whole = Lazy {
                storage,
                ask,
                rows: &rows,
                partial: catalog.iter().any(|ix| ix.partial.is_some())
                    || (canonical.len() == 1 && canonical.contains_key("$or"))
                    || ask.sort.is_some_and(|s| !s.is_empty()),
                fields: catalog
                    .iter()
                    .flat_map(|ix| ix.key.keys())
                    .map(|field| field.split('.').next().unwrap_or(field).to_string())
                    .collect(),
                loaded: OnceCell::new(),
            };
            // The plans the read chose through the plan cache.
            let trial = plan::Trial {
                total,
                matched: Some(rows.len()),
                whole: &whole,
                ranked: Default::default(),
                cache: hook.as_ref().map(|h| h as &dyn plan::CacheHook),
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

/// An `$or` whose branches each read an index, a geo predicate among them:
/// mongod scans each branch's index and merges the scans in the catalog
/// order of the indexes they read, each document once (measured 8.2.11: an
/// `_id` branch comes before a geo branch, and two geo branches on one
/// index come in the order written). `None` for any other read, or a
/// branch no index serves (then the whole `$or` is a collection scan).
fn geo_or_rows(
    storage: &dyn Storage,
    ask: &Ask,
    rows: &Rows,
) -> Result<Option<Rows>, StorageError> {
    let branches = match ask.filter.get_array("$or") {
        Ok(b) if ask.filter.len() == 1 => b,
        _ => return Ok(None),
    };
    let names: Vec<String> = storage
        .list_indexes(ask.db, ask.coll)?
        .iter()
        .filter_map(|spec| spec.get_str("name").ok().map(str::to_string))
        .collect();
    let position = |name: &str| names.iter().position(|n| n == name);
    let decoded: Vec<Option<Document>> = rows
        .iter()
        .map(|(_, blob)| secantus_core::bsondoc::from_bytes(blob).ok())
        .collect();
    let mut any_geo = false;
    let mut scans: Vec<(usize, Rows)> = Vec::new();
    for branch in branches {
        let Some(b) = branch.as_document() else {
            return Ok(None);
        };
        let matching: Rows = rows
            .iter()
            .zip(&decoded)
            .filter(|(_, d)| {
                d.as_ref().is_some_and(|d| {
                    secantus_core::query::matches(d, b, ask.vars, ask.collation).unwrap_or(false)
                })
            })
            .map(|(row, _)| row.clone())
            .collect();
        if let Some(scan) = geo::scan(storage, ask.db, ask.coll, b, None, false)? {
            let (Some(at), Some(ordered)) = (position(scan.index_name()), scan.order(matching))
            else {
                return Ok(None);
            };
            any_geo = true;
            scans.push((at, ordered));
            continue;
        }
        let sub = Ask {
            db: ask.db,
            coll: ask.coll,
            filter: b,
            sort: None,
            hint: None,
            collation: ask.collation,
            vars: ask.vars,
            limit: 0,
            asked: Default::default(),
        };
        let Some(at) = plan_indexes(storage, &sub)
            .first()
            .and_then(|n| position(n))
        else {
            return Ok(None);
        };
        let Some(ordered) = ordered_rows(storage, &sub)? else {
            return Ok(None);
        };
        let keep: BTreeSet<i64> = matching.iter().map(|(r, _)| *r).collect();
        scans.push((
            at,
            ordered
                .into_iter()
                .filter(|(r, _)| keep.contains(r))
                .collect(),
        ));
    }
    if !any_geo {
        return Ok(None);
    }
    scans.sort_by_key(|(at, _)| *at);
    let mut seen = BTreeSet::new();
    let mut out: Rows = Vec::with_capacity(rows.len());
    for (_, list) in scans {
        for row in list {
            if seen.insert(row.0) {
                out.push(row);
            }
        }
    }
    out.extend(rows.iter().filter(|(r, _)| !seen.contains(r)).cloned());
    Ok(Some(out))
}

/// The plan mongod runs for `ask`, without a trial of the candidates (a
/// fixed order of preference stands in); `None` for a collection with no
/// index, or a hint the planner does not know.
fn untried_plan(storage: &dyn Storage, ask: &Ask) -> Option<Node> {
    let catalog = ask_catalog(storage, ask).ok()?;
    if catalog.is_empty() {
        return None;
    }
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let hint = resolve_hint(ask.hint, &catalog).ok()?;
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
        pipeline: false,
        implies: &implied,
        trial: None,
    };
    Some(plan::plan(&query, &catalog))
}

/// Whether the plan for `ask` sorts in memory (a blocking `SORT`) rather
/// than reading its sort from an index. With no index at all to plan over,
/// it does.
pub(crate) fn sorts_in_memory(storage: &dyn Storage, ask: &Ask) -> bool {
    ask.sort.is_some_and(|s| !s.is_empty())
        && untried_plan(storage, ask).is_none_or(|plan| has_sort(&plan))
}

/// The names of the indexes the plan for `ask` reads. A cursor left open
/// over that plan is killed when one of them is dropped, as mongod's is.
pub(crate) fn plan_indexes(storage: &dyn Storage, ask: &Ask) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(plan) = untried_plan(storage, ask) {
        index_names(&plan, &mut names);
    }
    names
}

/// The one index scan the plan reads its documents through, under the
/// stages that fetch and sort them; `None` for any other plan.
fn the_scan(node: &Node) -> Option<&super::node::IxScan> {
    match access(node) {
        Node::IxScan(ix) => Some(ix),
        Node::Fetch { input, .. } | Node::Sort { input, .. } => the_scan(input),
        _ => None,
    }
}

/// mongod's `{$meta: "indexKey"}` for each of `docs`, the documents the
/// plan for `ask` returns: the key its index scan met the document at, its
/// fields in the key pattern's order (measured 8.2.11: `{z: 1, a: -1}`
/// gives `{z: .., a: ..}`). For a document an array gives several keys,
/// the first in the bounds in the scan's direction (`{t: [2, 12]}` read by
/// `t >= 3` is `{t: 12}`). `None` when the plan reads no single index scan
/// -- a collection scan, an `_id` lookup, several scans -- where mongod
/// adds nothing.
pub(crate) fn index_keys(
    storage: &dyn Storage,
    ask: &Ask,
    docs: &[Document],
) -> Option<Vec<Document>> {
    let plan = untried_plan(storage, ask)?;
    let ix = the_scan(&plan)?;
    if !ix.index.plain {
        return None;
    }
    let collation = ix.index.collation.as_ref();
    let fields = ix.index.fields();
    let within = |at: usize, v: &Bson| {
        if ix.no_bounds {
            return true;
        }
        ix.bounds.get(at).is_none_or(|list| {
            list.iter().any(|i| {
                let lo = super::bounds::cmp(&i.lo, v);
                let hi = super::bounds::cmp(v, &i.hi);
                (lo.is_lt() || (i.lo_incl && lo.is_eq()))
                    && (hi.is_lt() || (i.hi_incl && hi.is_eq()))
            })
        })
    };
    Some(
        docs.iter()
            .map(|doc| {
                let mut key = Document::new();
                for (at, (field, dir)) in fields.iter().enumerate() {
                    let values = exec::values_at(doc, field).0;
                    let reversed = (*dir < 0) != ix.backward;
                    let inside: Vec<&Bson> = values.iter().filter(|v| within(at, v)).collect();
                    let pool: Vec<&Bson> = if inside.is_empty() {
                        values.iter().collect()
                    } else {
                        inside
                    };
                    let first = pool
                        .into_iter()
                        .min_by_key(|v| exec::directed(v, reversed, collation))
                        .cloned()
                        .unwrap_or(Bson::Null);
                    key.insert(field.to_string(), first);
                }
                key
            })
            .collect(),
    )
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
    let Ok(catalog) = ask_catalog(storage, ask) else {
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
        pipeline: false,
        implies: &implied,
        trial: None,
    };
    let node = match uses {
        Uses::Find => plan::plan(&query, &catalog),
        Uses::Count => plan::count(&query, &catalog),
        Uses::Distinct(key) => plan::distinct(&query, &catalog, key, !ask.filter.is_empty()),
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
        Node::IxScan(ix) | Node::CountScan(ix) | Node::DistinctScan(ix, _, _) => {
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

/// How many documents mongod's batched multi-delete collects before it
/// writes them, last-read first, as one `applyOps` (measured 8.2.11).
pub(crate) const DELETE_BATCH: usize = 10;

/// Run `f`, an unbounded delete through `storage`, removing its matches in
/// mongod's batches: [`DELETE_BATCH`] at a time, each in reverse.
pub(crate) fn in_delete_batches<T>(storage: &dyn Storage, f: impl FnOnce() -> T) -> T {
    let mut f = Some(f);
    let mut out = None;
    storage.with_reversed_delete_batches(DELETE_BATCH, &mut || {
        if let Some(run) = f.take() {
            out = Some(run());
        }
    });
    match out {
        Some(v) => v,
        None => (f.take().expect("the delete ran or it did not"))(),
    }
}

/// Every RecordId `ask` matches, in mongod's order, for a delete that
/// removes them all: the order its batches are formed in. `None`: the
/// store's own order is the plan's (a whole collection read in storage
/// order, or the one index both read), or nothing matched.
pub(crate) fn delete_order(
    storage: &dyn Storage,
    ask: &Ask,
) -> Result<Option<Vec<i64>>, StorageError> {
    if ask.filter.is_empty() && ask.hint.is_none() {
        return Ok(None);
    }
    if store_order_is_the_plans(storage, ask)? {
        return Ok(None);
    }
    let Some(rows) = ordered_rows(storage, ask)? else {
        return Ok(None);
    };
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        rows.into_iter().map(|(recordid, _)| recordid).collect(),
    ))
}

/// Whether the store, left to pick, already takes the document mongod's
/// plan reads first, so the matches need not be read twice (RM-056).
///
/// That holds for one predicate, an equality or range on a plain scalar,
/// over a field exactly one index leads, when that index is a plain
/// single-field index holding each document once: mongod scans it, and
/// the store's router reads the same index, both by key and then by
/// RecordId. A null, an array, a document, a collation, a hint or a
/// second candidate index each leaves the order to be worked out.
fn store_order_is_the_plans(storage: &dyn Storage, ask: &Ask) -> Result<bool, StorageError> {
    if ask.sort.is_some_and(|s| !s.is_empty()) || ask.hint.is_some() || ask.collation.is_some() {
        return Ok(false);
    }
    let [(field, value)] = ask.filter.iter().collect::<Vec<_>>()[..] else {
        return Ok(false);
    };
    let plain = |v: &Bson| {
        matches!(
            v,
            Bson::Int32(_)
                | Bson::Int64(_)
                | Bson::Double(_)
                | Bson::Decimal128(_)
                | Bson::String(_)
                | Bson::Boolean(_)
                | Bson::DateTime(_)
                | Bson::ObjectId(_)
                | Bson::Timestamp(_)
        ) && !matches!(v, Bson::Double(d) if d.is_nan())
    };
    let simple = field != "_id"
        && !field.starts_with('$')
        && match value {
            Bson::Document(ops) => {
                !ops.is_empty()
                    && ops.iter().all(|(op, v)| {
                        matches!(op.as_str(), "$eq" | "$gt" | "$gte" | "$lt" | "$lte") && plain(v)
                    })
            }
            v => plain(v),
        };
    if !simple {
        return Ok(false);
    }
    let catalog = ask_catalog(storage, ask)?;
    let mut leading = catalog.iter().filter(|ix| ix.position(field) == Some(0));
    let (Some(index), None) = (leading.next(), leading.next()) else {
        return Ok(false);
    };
    Ok(index.key.len() == 1
        && index.plain
        && !index.sparse
        && index.partial.is_none()
        && index.collation.is_none()
        && !index.is_multikey()
        && crate::util::as_i64(index.key.get(field).unwrap_or(&Bson::Null)) == Some(1))
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
    if store_order_is_the_plans(storage, ask)? {
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
