//! The values a `distinct` mongod answers from a DISTINCT_SCAN.
//!
//! When mongod's plan for a `distinct` is a DISTINCT_SCAN ([`super::plan::distinct`]),
//! the values come from the index, not from the documents (measured 8.2.11,
//! 2026-10-16):
//!
//! - a covered scan reads each value from its key: a document without the
//!   field gives `null`, an empty array `undefined`, and a multikey index
//!   gives the array's elements, each of which the command then unwinds one
//!   level more, so `[8, [9]]` gives `8` and `9` where the document gives
//!   `8` and `[9]`;
//! - a scan through a collated index cannot read values back from its
//!   collation keys, so it fetches the FIRST document of each key, in index
//!   order, and reads the field from that: a null key whose first document
//!   lacks the field gives nothing.
//!
//! The scan is the explain module's own walk ([`super::exec`]) over the
//! documents the query matches; the caller sorts and de-duplicates.

use super::exec::{self, Env};
use super::node::Node;
use super::order::Ask;
use super::plan::{self, Query};
use super::{parsed, resolve_hint};
use crate::storage::{Storage, StorageError};
use bson::{Bson, Document};

/// The values, in the order the scan meets them, when mongod's plan for
/// `distinct` on `key` is a DISTINCT_SCAN. `None`: it is not, and the values
/// are the documents'.
pub(crate) fn scan_values(
    storage: &dyn Storage,
    ask: &Ask,
    key: &str,
) -> Result<Option<Vec<Bson>>, StorageError> {
    let catalog = super::order::ask_catalog(storage, ask)?;
    if catalog.is_empty() {
        return Ok(None);
    }
    let canonical = parsed::parsed_query(ask.filter);
    let implied = |partial: &Document| storage.query_implies_partial(ask.filter, partial);
    let Ok(hint) = resolve_hint(ask.hint, &catalog) else {
        return Ok(None);
    };
    let query = Query {
        canonical: &canonical,
        sort: None,
        projection: None,
        skip: 0,
        limit: 0,
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
    let node = plan::distinct(&query, &catalog, key, !ask.filter.is_empty());
    let scan = match &node {
        Node::Projection { input, .. } => input.as_ref(),
        other => other,
    };
    let Node::DistinctScan(ix, width, fetching) = scan else {
        return Ok(None);
    };
    let Some(matched) = storage.matching_rows(
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
    // The entries of one key are in RecordId order.
    let mut rows = matched.rows;
    rows.sort_unstable_by_key(|(recordid, _)| *recordid);
    let mut docs = Vec::with_capacity(rows.len());
    for (_, blob) in &rows {
        docs.push(
            secantus_core::bsondoc::from_bytes(blob.as_slice())
                .map_err(|e| StorageError::Internal(format!("decode: {e}")))?,
        );
    }
    let env = Env {
        docs: &docs,
        collation: ask.collation,
        skip: 0,
        limit: 0,
        multi: false,
        upsert: false,
        returns: false,
        known: None,
    };
    let mut values = Vec::new();
    for (row, value) in exec::distinct_entries(ix, *width, &env) {
        if *fetching {
            let (found, _) = secantus_core::get_path_values(&docs[row], key);
            for v in found {
                unwound(v.clone(), &mut values);
            }
        } else {
            unwound(value, &mut values);
        }
    }
    Ok(Some(values))
}

/// `value`, or an array's elements.
fn unwound(value: Bson, out: &mut Vec<Bson>) {
    match value {
        Bson::Array(items) => out.extend(items),
        other => out.push(other),
    }
}
