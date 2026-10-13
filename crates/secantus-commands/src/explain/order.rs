//! Putting a query's documents in the order mongod returns them.
//!
//! A query with no sort still comes back in an order: that of the index
//! mongod read, or of the collection. The same order decides which document
//! a `limit: 1` delete removes and which one an update without `multi`
//! changes. The store's index router picks its own way to the documents, so
//! its order is its own.
//!
//! Here the store is asked only WHICH documents match
//! (`Storage::matching_rows`, in insertion order), and the order comes from
//! the plan mongod runs ([`super::plan`]) walked over those documents with
//! mongod's stage rules ([`super::exec`]): an index scan returns its entries
//! by key and then by insertion, a backward scan the reverse, a multikey
//! scan a document where its first key in the bounds falls, an `$or` its
//! branches one after another, and a sort the plan cannot read from an index
//! keeps equal documents in the order they arrived.
//!
//! The plan orders and never selects: a document the store matched and the
//! plan does not reach is still returned, after the rest.

use super::exec::{self, Env};
use super::node::Node;
use super::plan::{self, Query};
use super::{catalog, parsed, resolve_hint};
use crate::storage::{RawHint, Storage, StorageError};
use crate::util::as_i64;
use bson::Document;
use secantus_core::collation::Collation;

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
}

/// The largest collection read whole to count what each candidate plan
/// examines. Past it the predicate's index is kept, as it is when it is
/// selective.
const TRIAL_COLLECTION: usize = 2000;

fn has_sort(node: &Node) -> bool {
    match node {
        Node::Sort { .. } => true,
        Node::Fetch { input, .. }
        | Node::Projection { input, .. }
        | Node::Skip(_, input)
        | Node::Limit(_, input)
        | Node::ReturnKey(input)
        | Node::Subplan(input) => has_sort(input),
        _ => false,
    }
}

/// Whether running `node` needs the documents decoded: an index scan reads
/// their keys, and a filter under an `$or` says which branch returns one.
fn reads_documents(node: &Node) -> bool {
    match node {
        Node::IxScan(_) | Node::Or(_) | Node::SortMerge { .. } => true,
        Node::Fetch { input, .. }
        | Node::Sort { input, .. }
        | Node::Projection { input, .. }
        | Node::Skip(_, input)
        | Node::Limit(_, input)
        | Node::ReturnKey(input)
        | Node::Subplan(input) => reads_documents(input),
        _ => false,
    }
}

struct Walk<'a> {
    storage: &'a dyn Storage,
    blobs: Vec<&'a [u8]>,
    env: Env<'a>,
    collation: Option<&'a Collation>,
}

impl Walk<'_> {
    fn sorted(&self, rows: Vec<usize>, pattern: &Document) -> Result<Vec<usize>, StorageError> {
        let blobs: Vec<&[u8]> = rows.iter().map(|&r| self.blobs[r]).collect();
        let order = self
            .storage
            .sort_permutation(&blobs, pattern, self.collation)?;
        Ok(order.into_iter().map(|at| rows[at]).collect())
    }

    /// The rows `node` returns, in order. Paging is the caller's.
    fn rows(&self, node: &Node) -> Result<Vec<usize>, StorageError> {
        Ok(match node {
            Node::CollScan { backward, .. } => {
                let all = 0..self.blobs.len();
                if *backward {
                    all.rev().collect()
                } else {
                    all.collect()
                }
            }
            Node::IxScan(ix) => exec::scan_rows(ix, &self.env),
            Node::Fetch { filter, input } => {
                let mut rows = self.rows(input)?;
                if filter.is_some() && !self.env.docs.is_empty() {
                    rows.retain(|&r| self.env.matches(r, filter));
                }
                rows
            }
            Node::Or(inputs) => self.union(inputs)?,
            Node::SortMerge { pattern, inputs } => self.sorted(self.union(inputs)?, pattern)?,
            Node::Sort { pattern, input, .. } => self.sorted(self.rows(input)?, pattern)?,
            Node::Projection { input, .. } | Node::Skip(_, input) | Node::Limit(_, input) => {
                self.rows(input)?
            }
            Node::ReturnKey(input) | Node::Subplan(input) => self.rows(input)?,
            _ => (0..self.blobs.len()).collect(),
        })
    }

    /// Each input's rows after the last one's, a row once.
    fn union(&self, inputs: &[Node]) -> Result<Vec<usize>, StorageError> {
        let mut seen = vec![false; self.blobs.len()];
        let mut out = Vec::new();
        for input in inputs {
            for row in self.rows(input)? {
                if !std::mem::replace(&mut seen[row], true) {
                    out.push(row);
                }
            }
        }
        Ok(out)
    }
}

/// The documents `ask` matches with their RecordIds, in mongod's order.
/// `None`: the store does not offer the matched set for this collection, or
/// the sort is one this does not read; the caller reads as it did before.
pub(crate) fn ordered_rows(
    storage: &dyn Storage,
    ask: &Ask,
) -> Result<Option<Vec<(i64, Vec<u8>)>>, StorageError> {
    if let Some(sort) = ask.sort {
        let plain = sort
            .iter()
            .all(|(k, v)| !k.starts_with('$') && matches!(as_i64(v), Some(1 | -1)));
        if !plain {
            return Ok(None);
        }
    }
    let Some(rows) = storage.matching_rows(
        ask.db,
        ask.coll,
        ask.filter,
        ask.hint,
        ask.collation,
        ask.vars,
    )?
    else {
        return Ok(None);
    };
    if rows.len() < 2 {
        return Ok(Some(rows));
    }
    let catalog = catalog(storage, ask.db, ask.coll, None)?;
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let hint = match resolve_hint(ask.hint, &catalog) {
        Ok(h) => h,
        Err(_) => return Ok(Some(rows)),
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
    // A sort the predicate's index does not return: mongod also tries a walk
    // of an index that does, and which one it keeps depends on how much of
    // the collection each reads.
    if has_sort(&plan) {
        if let Some(all) = storage.collection_at_most(ask.db, ask.coll, TRIAL_COLLECTION)? {
            let whole: Option<Vec<Document>> = all
                .iter()
                .map(|blob| secantus_core::bsondoc::from_bytes(blob.as_slice()).ok())
                .collect();
            if let Some(whole) = whole {
                let env = Env {
                    docs: &whole,
                    collation: ask.collation,
                    skip: 0,
                    limit: 0,
                    multi: true,
                    upsert: false,
                    returns: false,
                };
                plan = plan::plan(&query(Some(&env)), &catalog);
            }
        }
    }
    let docs: Vec<Document> = if reads_documents(&plan) {
        let mut docs = Vec::with_capacity(rows.len());
        for (_, blob) in &rows {
            match secantus_core::bsondoc::from_bytes(blob.as_slice()) {
                Ok(d) => docs.push(d),
                // A document that does not decode: leave the order alone.
                Err(_) => return Ok(Some(rows)),
            }
        }
        docs
    } else {
        Vec::new()
    };
    let order = {
        let walk = Walk {
            storage,
            blobs: rows.iter().map(|(_, b)| b.as_slice()).collect(),
            env: Env {
                docs: &docs,
                collation: ask.collation,
                skip: 0,
                limit: 0,
                multi: true,
                upsert: false,
                returns: false,
            },
            collation: ask.collation,
        };
        let mut order = walk.rows(&plan)?;
        // The plan orders and never selects.
        let mut reached = vec![false; rows.len()];
        order.retain(|&r| r < reached.len() && !std::mem::replace(&mut reached[r], true));
        order.extend((0..rows.len()).filter(|&r| !reached[r]));
        order
    };
    let mut slots: Vec<Option<(i64, Vec<u8>)>> = rows.into_iter().map(Some).collect();
    Ok(Some(
        order.into_iter().filter_map(|r| slots[r].take()).collect(),
    ))
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
const WRITE_CANDIDATES: usize = 64;

/// The RecordIds of the first documents `ask` matches, in mongod's order,
/// for a write that changes one of them. `None`: let the store pick.
pub(crate) fn write_candidates(
    storage: &dyn Storage,
    ask: &Ask,
) -> Result<Option<Vec<i64>>, StorageError> {
    // `{_id: value}` names its document: there is no order to find.
    if ask.filter.len() == 1
        && ask.filter.get("_id").is_some_and(|v| {
            !matches!(
                v,
                bson::Bson::Document(_) | bson::Bson::RegularExpression(_)
            )
        })
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
