//! Streaming reads: a `find` cursor that reads on as `getMore` asks.
//!
//! mongod's executor SAVES its position between batches and each `getMore`
//! RESTORES it on a fresh snapshot, fetching every document as it is now:
//! a collection scan resumes after its RecordId, an index scan after its
//! `(key, RecordId)`. So a document inserted ahead of the position appears,
//! one deleted ahead of it does not, and an index key moved behind the cursor
//! is not returned while one moved ahead is returned again (measured 8.2.11,
//! 2026-10-24; RM-080). Its counts are the batch's own, with no lookahead:
//! the scan stops when the batch is full, and finds the end only when a
//! later batch reads to it.
//!
//! This is that executor over this store: the plan `explain` derives, its
//! index scan run with [`super::exec`]'s rules (bounds, seeks, which keys
//! count) over the index's real entries, read a chunk at a time from the
//! saved position. Each batch reads in the call that asks for it, through
//! the store's per-call session -- a transaction's, inside one.
//!
//! What streams: a COLLSCAN, and a FETCH over an IXSCAN of an ascending,
//! uncollated index that is not multikey and not `_id_` (whose keys are the
//! documents' own `id_key`s, not index-table entries), under SKIP, LIMIT and a
//! projection. An index that is not multikey holds exactly mongod's keys
//! (this store also writes whole-array keys for an array, and keys an empty
//! array by the array where mongod keys `undefined`); a descending field's
//! inverted bytes do not reverse string prefixes; a collated index keys
//! strings its own way. Those, sorts that block, and every other plan read
//! as before ([`super::order`]).

use std::collections::VecDeque;

use bson::Document;
use secantus_core::sortkey::{split_compound, COMPOUND_SEP};

use super::exec::{contiguous, outside_of, ranges, Ranges};
use super::node::{IxScan, Node};
use crate::storage::{Storage, StorageError};

/// How many index entries or documents one read of the store takes.
const CHUNK: usize = 256;

/// A filter's failure on a document, as the store reports one
/// (`secantus_storage::query_fault` through the adapter).
fn fault(f: secantus_core::fallback::Fallback) -> StorageError {
    match f.as_mongo() {
        Some((code, errmsg)) => StorageError::WriteError {
            code,
            errmsg: errmsg.to_string(),
            exec: f.is_exec() || (f.folded_verdict() == Some(false) && !f.is_bare()),
        },
        None => StorageError::WriteError {
            code: 2,
            errmsg: "query uses a construct the Rust server does not support".to_string(),
            exec: false,
        },
    }
}

/// One batch: the documents (unprojected), whether the read reached the
/// end, and what it examined to fill this batch.
pub(crate) struct Batch {
    pub docs: Vec<Vec<u8>>,
    pub exhausted: bool,
    pub keys_examined: i64,
    pub docs_examined: i64,
}

/// A read in progress: the plan and where it stands.
pub(crate) struct Stream {
    db: String,
    coll: String,
    access: Access,
    /// The filter of the FETCH, or of the collection scan.
    filter: Option<Document>,
    vars: Document,
    /// Results still to skip, and still to return.
    skip: i64,
    limit: Option<i64>,
    /// Documents read for a batch that did not fit in it: the next batch's
    /// first (mongod stashes them the same way).
    stashed: VecDeque<Vec<u8>>,
    done: bool,
    keys: i64,
    docs: i64,
}

enum Access {
    /// A collection scan, after this RecordId.
    Coll {
        after: Option<i64>,
    },
    Index(IndexScan),
}

/// An index scan's bounds and position.
struct IndexScan {
    name: String,
    width: usize,
    ranges: Vec<Ranges>,
    contiguous: bool,
    /// Where the scan starts: the low ends of the first intervals.
    start: Vec<u8>,
    done: bool,
    /// The last entry the scan consumed: where the next read resumes.
    last: Option<(Vec<u8>, i64)>,
    /// Entries still to skip before any is fetched (a SKIP below the FETCH).
    key_skip: i64,
}

/// An entry of the index, split into its fields' keys.
struct Entry {
    parts: Vec<Vec<u8>>,
    raw: Vec<u8>,
    rid: i64,
    /// A whole-array key, which this store writes and mongod does not.
    foreign: bool,
}

/// The entries of one batch's reads, a chunk at a time.
struct Source<'a> {
    storage: &'a dyn Storage,
    db: &'a str,
    coll: &'a str,
    scan: &'a mut IndexScan,
    buf: VecDeque<Entry>,
    end: bool,
}

impl Source<'_> {
    /// Whether an entry is at the front, reading the next chunk if need be.
    fn fill(&mut self) -> Result<bool, StorageError> {
        if self.buf.is_empty() && !self.end {
            self.refill()?;
        }
        Ok(!self.buf.is_empty())
    }

    fn refill(&mut self) -> Result<(), StorageError> {
        let (from, after) = match &self.scan.last {
            Some((raw, rid)) => (raw.clone(), Some(*rid)),
            None => (self.scan.start.clone(), None),
        };
        let Some(rows) = self.storage.index_entries_from(
            self.db,
            self.coll,
            &self.scan.name,
            &from,
            after,
            CHUNK,
        )?
        else {
            return Err(StorageError::Internal(
                "this store cannot stream an index scan".into(),
            ));
        };
        if rows.len() < CHUNK {
            self.end = true;
        }
        for (raw, rid) in rows {
            let parts: Vec<Vec<u8>> = split_compound(&raw, self.scan.width)
                .ok_or_else(|| {
                    StorageError::Internal(format!(
                        "index {} holds an entry this server cannot read",
                        self.scan.name
                    ))
                })?
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect();
            let foreign = parts
                .iter()
                .any(|p| p.first() == Some(&secantus_core::sortkey::RANK_ARRAY));
            self.buf.push_back(Entry {
                parts,
                raw,
                rid,
                foreign,
            });
        }
        Ok(())
    }

    /// Consume the front entry: the scan's position moves past it.
    fn pop(&mut self) -> Option<i64> {
        let e = self.buf.pop_front()?;
        self.scan.last = Some((e.raw, e.rid));
        Some(e.rid)
    }
}

/// What one unit of an index scan's work did.
enum Step {
    Entry(i64),
    Seek,
    End,
}

/// [`super::exec::Scan`]'s work over the store's entries: an entry inside
/// the bounds, a key that sends the scan seeking (counted; the keys it
/// seeks past are not), or the end.
fn work_entry(src: &mut Source, keys: &mut i64) -> Result<Step, StorageError> {
    loop {
        if src.scan.done {
            return Ok(Step::End);
        }
        if !src.fill()? {
            src.scan.done = true;
            return Ok(Step::End);
        }
        let (foreign, outside) = {
            let e = &src.buf[0];
            (e.foreign, outside_of(&src.scan.ranges, &e.parts))
        };
        if foreign {
            src.pop();
            continue;
        }
        let Some((field, ahead)) = outside else {
            *keys += 1;
            let rid = src.pop().unwrap_or_default();
            return Ok(Step::Entry(rid));
        };
        if src.scan.contiguous {
            // Before the run: not examined. After it: the scan is over.
            if ahead {
                src.pop();
                continue;
            }
            src.scan.done = true;
            return Ok(Step::End);
        }
        *keys += 1;
        if !ahead && field == 0 {
            src.scan.done = true;
            return Ok(Step::End);
        }
        // Seek: past every key that fails the same way.
        let prefix = src.buf[0].parts[..field].to_vec();
        src.pop();
        while src.fill()? {
            let (foreign, same, fails_alike) = {
                let other = &src.buf[0];
                (
                    other.foreign,
                    other.parts[..field] == prefix[..],
                    matches!(outside_of(&src.scan.ranges, &other.parts), Some((f, true)) if f == field),
                )
            };
            if foreign {
                src.pop();
                continue;
            }
            if !same || (ahead && !fails_alike) {
                break;
            }
            src.pop();
        }
        return Ok(Step::Seek);
    }
}

/// Whether an index scan can stream (see the module's doc).
fn streams(ix: &IxScan) -> bool {
    // The `_id` index is the `id_key` each document row holds, not entries
    // of the index table this reads.
    ix.index.name != "_id_"
        && !ix.no_bounds
        && ix.filter.is_none()
        && !ix.backward
        && ix.index.plain
        && ix.index.collation.is_none()
        && !ix.index.is_multikey()
        && ix.index.fields().iter().all(|(_, dir)| *dir == 1)
}

impl Stream {
    /// A read of `plan` over `db.coll`, when it is one that streams. Each
    /// document is matched against the command's own `filter`, not the plan's
    /// residual filter: the plan's is a rewrite for `explain`, and the
    /// command's is what a `find` answers and validates (an index's bounds
    /// only narrow which documents are fetched, so the counts are the same).
    pub(crate) fn of_plan(
        plan: &Node,
        db: &str,
        coll: &str,
        filter: &Document,
        vars: &Document,
    ) -> Option<Stream> {
        let mut node = plan;
        // The caller projects each batch.
        if let Node::Projection { input, .. } = node {
            node = input;
        }
        let mut limit = None;
        if let Node::Limit(n, input) = node {
            limit = Some(*n);
            node = input;
        }
        let mut skip = 0;
        if let Node::Skip(n, input) = node {
            skip = *n;
            node = input;
        }
        let (access, _plan_filter) = match node {
            Node::CollScan {
                filter,
                backward: false,
            } => (Access::Coll { after: None }, filter.clone()),
            Node::Fetch { filter, input } => {
                // mongod skips index entries below the FETCH when nothing is
                // filtered after it: the skipped keys are examined, their
                // documents never read (`FETCH <- SKIP <- IXSCAN`).
                let (key_skip, input) = match input.as_ref() {
                    Node::Skip(n, inner) => (*n, inner.as_ref()),
                    other => (0, other),
                };
                match input {
                    Node::IxScan(ix) if streams(ix) => {
                        let ranges: Vec<Ranges> = ix
                            .bounds
                            .iter()
                            .map(|list| ranges(list, false, None))
                            .collect();
                        let mut start = Vec::new();
                        for (i, list) in ranges.iter().enumerate() {
                            if i > 0 {
                                start.extend_from_slice(COMPOUND_SEP);
                            }
                            start.extend_from_slice(
                                list.first().map_or(&[][..], |r| r.0.as_slice()),
                            );
                        }
                        let done = ranges.iter().any(Vec::is_empty);
                        (
                            Access::Index(IndexScan {
                                name: ix.index.name.clone(),
                                width: ix.index.key.len(),
                                contiguous: contiguous(&ix.bounds),
                                ranges,
                                start,
                                done,
                                last: None,
                                key_skip,
                            }),
                            filter.clone(),
                        )
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };
        Some(Stream {
            db: db.to_string(),
            coll: coll.to_string(),
            access,
            filter: (!filter.is_empty()).then(|| filter.clone()),
            vars: vars.clone(),
            skip,
            limit,
            stashed: VecDeque::new(),
            done: false,
            keys: 0,
            docs: 0,
        })
    }

    /// Up to `want` documents that pass the filter (`want` 0: no count
    /// limit), read on from where the last batch stopped.
    fn read(&mut self, storage: &dyn Storage, want: usize) -> Result<Vec<Vec<u8>>, StorageError> {
        let mut out = Vec::new();
        let full = |out: &Vec<Vec<u8>>| want > 0 && out.len() >= want;
        match &mut self.access {
            Access::Coll { after } => {
                while !full(&out) {
                    let Some(rows) = storage.records_after(&self.db, &self.coll, *after, CHUNK)?
                    else {
                        return Err(StorageError::Internal("this store cannot stream".into()));
                    };
                    let end = rows.len() < CHUNK;
                    let mut stopped = false;
                    for (rid, blob) in rows {
                        *after = Some(rid);
                        self.docs += 1;
                        let filter = self.filter.as_ref();
                        let keep = match filter {
                            None => true,
                            Some(f) => {
                                let raw = bson::RawDocument::from_bytes(&blob).map_err(|e| {
                                    StorageError::Internal(format!("undecodable document: {e}"))
                                })?;
                                secantus_core::query::matches_raw(raw, f, &self.vars, None)
                                    .map_err(fault)?
                            }
                        };
                        if keep {
                            out.push(blob);
                            if full(&out) {
                                stopped = true;
                                break;
                            }
                        }
                    }
                    if end && !stopped {
                        self.done = true;
                        break;
                    }
                }
            }
            Access::Index(scan) => {
                let mut src = Source {
                    storage,
                    db: &self.db,
                    coll: &self.coll,
                    scan,
                    buf: VecDeque::new(),
                    end: false,
                };
                'batch: while !full(&out) {
                    // The entries this batch is sure to fetch: no more than it
                    // can still take, so none is read and lost.
                    let room = if want == 0 {
                        CHUNK
                    } else {
                        (want - out.len()).min(CHUNK)
                    };
                    let mut rids = Vec::with_capacity(room);
                    while rids.len() < room {
                        match work_entry(&mut src, &mut self.keys)? {
                            Step::Entry(_) if src.scan.key_skip > 0 => src.scan.key_skip -= 1,
                            Step::Entry(rid) => rids.push(rid),
                            Step::Seek => {}
                            Step::End => break,
                        }
                    }
                    if rids.is_empty() {
                        self.done = true;
                        break;
                    }
                    let Some(found) = storage.records_by_id(&self.db, &self.coll, &rids)? else {
                        return Err(StorageError::Internal("this store cannot stream".into()));
                    };
                    for blob in found.into_iter().flatten() {
                        self.docs += 1;
                        let keep = match &self.filter {
                            None => true,
                            Some(f) => {
                                let raw = bson::RawDocument::from_bytes(&blob).map_err(|e| {
                                    StorageError::Internal(format!("undecodable document: {e}"))
                                })?;
                                secantus_core::query::matches_raw(raw, f, &self.vars, None)
                                    .map_err(fault)?
                            }
                        };
                        if keep {
                            out.push(blob);
                            if full(&out) {
                                break 'batch;
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// The next batch: up to `want` results (0: as many as fit) within
    /// `max_bytes`, after SKIP and LIMIT, with what it examined.
    pub(crate) fn next_batch(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        max_bytes: usize,
    ) -> Result<Batch, StorageError> {
        self.keys = 0;
        self.docs = 0;
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 0usize;
        let left = |out: &Vec<Vec<u8>>, limit: Option<i64>| -> usize {
            let by_limit = limit.map(|l| l.max(0) as usize);
            let by_want = (want > 0).then(|| want - out.len());
            match (by_limit, by_want) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => 0,
            }
        };
        'fill: loop {
            if want > 0 && out.len() >= want {
                break;
            }
            if self.limit == Some(0) {
                self.done = true;
                break;
            }
            let mut fresh: VecDeque<Vec<u8>> = std::mem::take(&mut self.stashed);
            if fresh.is_empty() {
                if self.done {
                    break;
                }
                // With no count to fill, a chunk at a time until the bytes
                // are spent; skipped results are read too.
                let ask = match left(&out, self.limit) {
                    0 => CHUNK,
                    n => n + self.skip.max(0) as usize,
                };
                fresh = self.read(storage, ask)?.into();
                if fresh.is_empty() {
                    self.done = true;
                    break;
                }
            }
            while let Some(blob) = fresh.pop_front() {
                if self.skip > 0 {
                    self.skip -= 1;
                    continue;
                }
                if !out.is_empty() && bytes + blob.len() > max_bytes {
                    fresh.push_front(blob);
                    self.stashed = fresh;
                    break 'fill;
                }
                bytes += blob.len();
                out.push(blob);
                if let Some(l) = &mut self.limit {
                    *l -= 1;
                    if *l == 0 {
                        self.done = true;
                        self.stashed.clear();
                        break 'fill;
                    }
                }
                if want > 0 && out.len() >= want {
                    self.stashed = fresh;
                    break 'fill;
                }
            }
        }
        let exhausted = self.done && self.stashed.is_empty();
        Ok(Batch {
            docs: out,
            exhausted,
            keys_examined: self.keys,
            docs_examined: self.docs,
        })
    }
}
