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
//! This is that executor over this store: the plan `explain` derives, run
//! stage by stage the way mongod's stages run, over the store's real entries
//! and documents, each pulled as the batch needs it. Each batch reads in the
//! call that asks for it, through the store's per-call session -- a
//! transaction's, inside one -- and at its end every stage YIELDS: what it
//! read ahead is dropped and only its position is kept.
//!
//! The stages:
//!
//! - COLLSCAN, either way;
//! - IXSCAN of any plain index, either way: ascending and descending fields
//!   (entry format 6 keeps a descending field in mongod's order), collated
//!   ones (their entries hold the collation's sort keys, as mongod's do), the
//!   `_id` index (`_id` key format 2 keeps every `_id` in mongod's order),
//!   bounds or `min` / `max`. A multikey scan returns each
//!   document once, remembering across batches what it returned, as mongod's
//!   does; it passes over the whole-array keys this store writes besides
//!   mongod's, which it tells from an element that is an array by the
//!   document's own keys. A DISTINCT_SCAN is the same scan, one entry a run;
//! - FETCH, which reads each document as it is now, and drops an entry held
//!   over a yield whose document no longer has that key (mongod's
//!   `isIndexKeyConsistent`);
//! - SKIP below a FETCH (keys examined, documents never read);
//! - OR, each child in turn, each document once;
//! - SORT_MERGE, one head held per child (across batches, as mongod holds
//!   them), the least returned first, each document once;
//! - a SORT of index keys below the FETCH: every key read and sorted for the
//!   first batch, each document fetched as a batch reaches it.
//!
//! - GEO_NEAR_2D / GEO_NEAR_2DSPHERE: a `$near`'s ring search, its rings,
//!   queue and increment kept between batches, each ring reading the
//!   collection as it is when the search reaches it, and the queue handing
//!   out the documents it buffered as it buffered them (RM-347).
//!
//! What does not stream reads as before ([`super::order`]): a sort of whole
//! documents (mongod reads it whole for the first batch too), plans that
//! intersect, and a geo read other than a `$near`, or a text read.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use bson::{doc, Bson, Document};
use secantus_core::collation::Collation;
use secantus_core::geoorder::near::{Candidate, Measure, NearSearch, Source};
use secantus_core::geoorder::FieldBounds;
use secantus_core::heap::{Heap, HeapOrder};
use secantus_core::sortkey::{self, COMPOUND_SEP};

use super::exec::{contiguous, outside_of, ranges, reversed_key, Ranges};
use super::node::{num, IxScan, Node, Stats, WorkCounts};
use crate::storage::{Storage, StorageError};

/// How many index entries or documents one read of the store takes.
const CHUNK: usize = 256;

/// An index scan's first read of the store: it doubles on each read after,
/// up to [`CHUNK`]. mongod reads one key per unit of work and stops at its
/// bounds; reading ahead in whole chunks made a short range read and
/// decode 256 entries to return ten (RM-365).
const FIRST_CHUNK: usize = 16;

/// What one stage did as it ran, counted as mongod's `PlanStage` counts its
/// `CommonStats` (RM-359): a profile entry's `execStats`.
///
/// A stage that hands a work down to its input (a FETCH, a SKIP, an OR, a
/// SORT reading its input) does not count it: its input does, and the
/// stage's works are its inputs' and its own (`own`). Its own are the works
/// that call no input: a scan's every one, a SORT's or a SORT_MERGE's
/// results, and the work that finds the input already at its end.
#[derive(Clone, Copy, Default)]
struct Tally {
    own: i64,
    advanced: i64,
    /// A work of the stage returned EOF (one ends a stage's run).
    eof_work: bool,
    /// Works past that one that found the stage at its end again.
    eof_again: i64,
    keys: i64,
    docs: i64,
    seeks: i64,
    dups_tested: i64,
    dups_dropped: i64,
    /// The time spent in the stage's calls, on a coarse clock.
    millis: i64,
}

/// Milliseconds on a coarse clock, as cheap to read as mongod's fast clock
/// source, which times each stage's works (`executionTimeMillisEstimate`).
fn coarse_ms() -> i64 {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        let clock = libc::CLOCK_MONOTONIC_COARSE;
        #[cfg(target_os = "macos")]
        let clock = libc::CLOCK_MONOTONIC_RAW_APPROX;
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid timespec for the call to fill.
        unsafe { libc::clock_gettime(clock, &mut ts) };
        // `time_t` and `c_long` are i64 on a 64-bit target, narrower on others.
        #[allow(clippy::unnecessary_cast)]
        let ms = ts.tv_sec as i64 * 1000 + ts.tv_nsec as i64 / 1_000_000;
        ms
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis() as i64
    }
}

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

fn undecodable(e: impl std::fmt::Display) -> StorageError {
    StorageError::Internal(format!("undecodable document: {e}"))
}

/// One batch: the documents (unprojected), each one's RecordId and (when
/// asked for) the index key it was returned at, whether the read reached
/// the end, and what it examined to fill this batch.
pub(crate) struct Batch {
    pub docs: Vec<Vec<u8>>,
    pub rids: Vec<i64>,
    pub keys: Vec<Option<Document>>,
    pub exhausted: bool,
    pub keys_examined: i64,
    pub docs_examined: i64,
}

/// A read made whole ([`Stream::read_all`]).
#[derive(Default)]
pub(crate) struct ReadAll {
    pub docs: Vec<Vec<u8>>,
    pub rids: Vec<i64>,
    pub keys: Vec<Option<Document>>,
    pub keys_examined: i64,
    pub docs_examined: i64,
}

/// What a read beyond the plan asks of the stream.
#[derive(Default)]
pub(crate) struct Options<'a> {
    /// The key each document was returned at (`returnKey`, `{$meta:
    /// "indexKey"}`).
    pub keys: bool,
    /// `min` / `max`: where the hinted scan starts and stops.
    pub min: Option<&'a Document>,
    pub max: Option<&'a Document>,
    /// The command's collation, which a sort of index keys compares under.
    pub collation: Option<&'a Collation>,
    /// A blocking sort's memory budget and whether it may spill to disk
    /// (`allowDiskUse`), held to as mongod's `SORT` holds to it (RM-315).
    pub sort_budget: Option<(secantus_core::memlimit::Budgets, bool)>,
}

/// What a scan knows of its index.
struct IndexMeta {
    name: String,
    fields: Vec<String>,
    dirs: Vec<i32>,
    collation: Option<Collation>,
    sparse: bool,
    multikey: bool,
    /// The `_id` index: each entry one `id_key`, not a compound key.
    id: bool,
}

/// The index entry a document was found at: the index, and each field's
/// ascending key.
#[derive(Clone)]
struct EntryKey {
    meta: Arc<IndexMeta>,
    parts: Vec<Vec<u8>>,
}

/// A document on its way up the plan (mongod's working set member): its
/// RecordId, the entry that found it, and the document once fetched.
struct Member {
    rid: i64,
    doc: Option<Vec<u8>>,
    entry: Option<EntryKey>,
    /// Found before the last yield and not fetched since.
    stale: bool,
}

/// One result, with what a caller may want of it.
struct Found {
    doc: Vec<u8>,
    rid: i64,
    entry: Option<EntryKey>,
}

/// The plan's state between batches.
struct Cx<'a> {
    storage: &'a dyn Storage,
    db: &'a str,
    coll: &'a str,
    vars: &'a Document,
    keys: i64,
    docs: i64,
    /// How many more results the batch takes (0: as many as its bytes
    /// allow): what a FETCH may read ahead.
    want: usize,
}

impl Cx<'_> {
    /// The document behind `rid` as it is now, `None` when it is gone.
    fn read(&self, rid: i64) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(mut found) = self.storage.records_by_id(self.db, self.coll, &[rid])? else {
            return Err(StorageError::Internal("this store cannot stream".into()));
        };
        Ok(found.pop().flatten())
    }

    fn matches(&self, blob: &[u8], filter: &Document) -> Result<bool, StorageError> {
        let raw = bson::RawDocument::from_bytes(blob).map_err(undecodable)?;
        secantus_core::query::matches_raw(raw, filter, self.vars, None).map_err(fault)
    }
}

/// Whether `part`, one field's ascending key, is one of mongod's keys for
/// `doc` in that field of `meta`'s index (`secantus_core::index_keys`).
fn mongod_key(meta: &IndexMeta, field: usize, doc: &Document, part: &[u8]) -> bool {
    let keys = secantus_core::index_keys::field_keys(doc, &meta.fields[field], meta.sparse);
    keys.mongod
        .iter()
        .any(|v| sortkey::encode_value(v, meta.collation.as_ref()).is_ok_and(|k| k == part))
}

/// Whether every field of `entry` is one of mongod's keys for `blob`.
fn holds_entry(entry: &EntryKey, blob: &[u8]) -> Result<bool, StorageError> {
    if entry.meta.id {
        return Ok(true);
    }
    let doc = secantus_core::bsondoc::from_bytes(blob).map_err(undecodable)?;
    Ok((0..entry.parts.len()).all(|i| mongod_key(&entry.meta, i, &doc, &entry.parts[i])))
}

/// An entry read from the index: its stored key, RecordId, each field's
/// ascending key and each field's key in SCAN order.
struct Entry {
    raw: Vec<u8>,
    rid: i64,
    asc: Vec<Vec<u8>>,
    scan: Vec<Vec<u8>>,
}

/// How a scan is bounded.
enum Limits {
    /// The plan's index bounds, per field, in scan order.
    Ranges {
        ranges: Vec<Ranges>,
        contiguous: bool,
    },
    /// `min` (inclusive) to `max` (exclusive), as stored keys.
    MinMax {
        min: Option<Vec<u8>>,
        max: Option<Vec<u8>>,
    },
}

/// An index scan's bounds and position.
struct IndexScan {
    meta: Arc<IndexMeta>,
    /// Last entry first.
    backward: bool,
    limits: Limits,
    /// Where the scan starts, as the entries hold it.
    start: Vec<u8>,
    done: bool,
    /// The last entry the scan consumed: where the next read resumes.
    last: Option<(Vec<u8>, i64)>,
    /// Entries read ahead of the position, dropped at a yield.
    buf: VecDeque<Entry>,
    /// The store has nothing after `buf`.
    end: bool,
    /// A multikey scan's documents returned so far.
    seen: HashSet<i64>,
    /// A DISTINCT_SCAN: the first entry of each run of this many leading
    /// keys, and the last such run returned.
    distinct: Option<usize>,
    run: Option<Vec<Vec<u8>>>,
    /// A COUNT_SCAN: mongod's counts the call that finds its end as a key
    /// examined, whether or not the index holds a key past the bounds
    /// (`CountScan::doWork` counts before it looks; measured 8.2.11: a
    /// count of `{x: {$gt: 3}}` over keys 0 to 11 examines 9).
    count_scan: bool,
    /// The end has been counted.
    ended: bool,
    /// How many entries the next read of the store takes.
    chunk: usize,
    /// Every RecordId read from the store, when asked for ([`scan_reach`]).
    read: Option<Vec<i64>>,
    t: Tally,
}

/// What one unit of an index scan's work did.
enum Step {
    Entry(Member),
    Seek,
    End,
}

/// Where an index scan stood before it produced an entry: what a FETCH
/// that read ahead rewinds it to at a yield, so the next batch reads on from
/// the last entry handed out, as mongod's scan does.
struct Snap {
    last: Option<(Vec<u8>, i64)>,
    done: bool,
    run: Option<Vec<Vec<u8>>>,
    /// The batch's keys examined at that point.
    keys: i64,
    /// The scan's own tally at that point.
    t: Tally,
}

impl IndexScan {
    fn snapshot(&self, keys: i64) -> Snap {
        Snap {
            last: self.last.clone(),
            done: self.done,
            run: self.run.clone(),
            keys,
            t: self.t,
        }
    }

    /// Back to `snap`: the entries produced since are read again by the
    /// next batch, and a multikey scan forgets it returned them.
    fn rewind(&mut self, snap: Snap, unread: &[i64]) {
        self.last = snap.last;
        self.done = snap.done;
        self.run = snap.run;
        self.t = snap.t;
        self.buf.clear();
        self.end = false;
        if self.meta.multikey {
            for rid in unread {
                self.seen.remove(rid);
            }
        }
    }

    /// Whether an entry is at the front, reading the next chunk if need be.
    fn fill(&mut self, cx: &Cx) -> Result<bool, StorageError> {
        if self.buf.is_empty() && !self.end {
            self.refill(cx)?;
        }
        Ok(!self.buf.is_empty())
    }

    fn refill(&mut self, cx: &Cx) -> Result<(), StorageError> {
        let (from, after) = match &self.last {
            Some((raw, rid)) => (raw.clone(), Some(*rid)),
            None => (self.start.clone(), None),
        };
        let Some(rows) = cx.storage.index_entries_from(
            cx.db,
            cx.coll,
            &self.meta.name,
            &from,
            after,
            self.chunk,
            self.backward,
        )?
        else {
            return Err(StorageError::Internal(
                "this store cannot stream an index scan".into(),
            ));
        };
        if rows.len() < self.chunk {
            self.end = true;
        }
        self.chunk = (self.chunk * 2).min(CHUNK);
        let unreadable = || {
            StorageError::Internal(format!(
                "index {} holds an entry this server cannot read",
                self.meta.name
            ))
        };
        for (raw, rid) in rows {
            if let Some(read) = self.read.as_mut() {
                read.push(rid);
            }
            let asc: Vec<Vec<u8>> = if self.meta.id {
                vec![raw.clone()]
            } else {
                sortkey::split_directed(&raw, &self.meta.dirs)
                    .ok_or_else(unreadable)?
                    .into_iter()
                    .zip(&self.meta.dirs)
                    .map(|(part, dir)| sortkey::ascending_part(part, *dir))
                    .collect()
            };
            let scan = asc
                .iter()
                .zip(&self.meta.dirs)
                .map(|(part, dir)| {
                    if (*dir < 0) != self.backward {
                        reversed_key(part)
                    } else {
                        part.clone()
                    }
                })
                .collect();
            self.buf.push_back(Entry {
                raw,
                rid,
                asc,
                scan,
            });
        }
        Ok(())
    }

    /// Consume the front entry: the scan's position moves past it.
    fn pop(&mut self) -> Option<Entry> {
        let e = self.buf.pop_front()?;
        self.last = Some((e.raw.clone(), e.rid));
        Some(e)
    }

    /// Where the front entry stands against the bounds: inside (`None`),
    /// or the first field outside and whether a later interval of it could
    /// still be reached.
    fn outside(&self) -> Option<(usize, bool)> {
        let e = &self.buf[0];
        match &self.limits {
            Limits::Ranges { ranges, .. } => outside_of(ranges, &e.scan),
            Limits::MinMax { min, max } => {
                let (low, high) = if self.backward {
                    (max, min)
                } else {
                    (min, max)
                };
                // Walking up, a key at or past `max` ends the scan; walking
                // down, a key below `min` does. `max` itself is never in.
                let past = if self.backward {
                    high.as_ref().is_some_and(|m| e.raw < *m)
                } else {
                    high.as_ref().is_some_and(|m| e.raw >= *m)
                };
                let before = if self.backward {
                    low.as_ref().is_some_and(|m| e.raw >= *m)
                } else {
                    low.as_ref().is_some_and(|m| e.raw < *m)
                };
                if past {
                    Some((0, false))
                } else if before {
                    Some((0, true))
                } else {
                    None
                }
            }
        }
    }

    /// Whether the front entry is a key of mongod's: not a whole-array key
    /// this store writes besides them. Only an array-ranked key in a
    /// multikey index can be one; the document says.
    fn foreign(&self, cx: &Cx) -> Result<bool, StorageError> {
        if !self.meta.multikey {
            return Ok(false);
        }
        let e = &self.buf[0];
        let arrays: Vec<usize> = e
            .asc
            .iter()
            .enumerate()
            .filter(|(_, p)| p.first() == Some(&sortkey::RANK_ARRAY))
            .map(|(i, _)| i)
            .collect();
        if arrays.is_empty() {
            return Ok(false);
        }
        let Some(blob) = cx.read(e.rid)? else {
            return Ok(true);
        };
        let doc = secantus_core::bsondoc::from_bytes(blob.as_slice()).map_err(undecodable)?;
        Ok(!arrays
            .iter()
            .all(|&i| mongod_key(&self.meta, i, &doc, &e.asc[i])))
    }

    /// [`super::exec::Scan`]'s work over the store's entries: an entry
    /// inside the bounds, a key that sends the scan seeking (counted; the
    /// keys it seeks past are not), or the end.
    fn work(&mut self, cx: &mut Cx) -> Result<Step, StorageError> {
        loop {
            if self.done {
                return Ok(Step::End);
            }
            if !self.fill(cx)? {
                self.done = true;
                return Ok(Step::End);
            }
            if self.foreign(cx)? {
                self.pop();
                continue;
            }
            let Some((field, ahead)) = self.outside() else {
                // A DISTINCT_SCAN seeks past the rest of the run it returned.
                if let Some(width) = self.distinct {
                    let front = &self.buf[0].scan;
                    let run = &front[..width.min(front.len())];
                    if self.run.as_deref() == Some(run) {
                        self.pop();
                        continue;
                    }
                    self.run = Some(run.to_vec());
                }
                cx.keys += 1;
                self.t.keys += 1;
                self.t.own += 1;
                let e = self.pop().expect("an entry is at the front");
                // A DISTINCT_SCAN returns each value, whichever document
                // holds it: a document is returned once by any other scan.
                if self.meta.multikey && self.distinct.is_none() {
                    self.t.dups_tested += 1;
                    if !self.seen.insert(e.rid) {
                        // mongod's scan spends the work and needs more time.
                        self.t.dups_dropped += 1;
                        continue;
                    }
                }
                self.t.advanced += 1;
                return Ok(Step::Entry(Member {
                    rid: e.rid,
                    doc: None,
                    entry: Some(EntryKey {
                        meta: self.meta.clone(),
                        parts: e.asc,
                    }),
                    stale: false,
                }));
            };
            let contiguous = match &self.limits {
                Limits::Ranges { contiguous, .. } => *contiguous,
                Limits::MinMax { .. } => true,
            };
            if contiguous {
                // Before the run: not examined. After it: the scan is over.
                if ahead {
                    self.pop();
                    continue;
                }
                self.done = true;
                return Ok(Step::End);
            }
            cx.keys += 1;
            self.t.keys += 1;
            if !ahead && field == 0 {
                self.done = true;
                return Ok(Step::End);
            }
            // Seek: past every key that fails the same way. The work that
            // examined the key seeks, and needs more time.
            self.t.own += 1;
            self.t.seeks += 1;
            let prefix = self.buf[0].scan[..field].to_vec();
            self.pop();
            while self.fill(cx)? {
                if self.foreign(cx)? {
                    self.pop();
                    continue;
                }
                let same = self.buf[0].scan[..field] == prefix[..];
                let fails_alike = matches!(self.outside(), Some((f, true)) if f == field);
                if !same || (ahead && !fails_alike) {
                    break;
                }
                self.pop();
            }
            return Ok(Step::Seek);
        }
    }

    fn next(&mut self, cx: &mut Cx) -> Result<Option<Member>, StorageError> {
        // A scan at its end is worked no more (mongod's executor stops).
        if self.t.eof_work {
            return Ok(None);
        }
        let started = coarse_ms();
        // The first work seeks to the start of the bounds.
        if self.t.own == 0 {
            self.t.seeks += 1;
        }
        let out = loop {
            match self.work(cx) {
                Ok(Step::Entry(m)) => break Ok(Some(m)),
                Ok(Step::Seek) => continue,
                Ok(Step::End) => {
                    if self.count_scan && !self.ended {
                        self.ended = true;
                        cx.keys += 1;
                        self.t.keys += 1;
                    }
                    // The work that finds the end.
                    self.t.own += 1;
                    self.t.eof_work = true;
                    break Ok(None);
                }
                Err(e) => break Err(e),
            }
        };
        self.t.millis += coarse_ms() - started;
        out
    }
}

/// A collection scan's position.
struct CollScan {
    /// After this RecordId (before it, backward).
    after: Option<i64>,
    backward: bool,
    filter: Option<Document>,
    buf: VecDeque<(i64, Vec<u8>)>,
    end: bool,
    t: Tally,
}

impl CollScan {
    fn new(backward: bool, filter: Option<Document>, end: bool) -> CollScan {
        CollScan {
            after: None,
            backward,
            filter,
            buf: VecDeque::new(),
            end,
            t: Tally::default(),
        }
    }

    /// Each document is a work, advanced when it matches; the end is one
    /// more.
    fn next(&mut self, cx: &mut Cx) -> Result<Option<Member>, StorageError> {
        if self.t.eof_work {
            return Ok(None);
        }
        let started = coarse_ms();
        let out = self.scan(cx);
        match &out {
            Ok(Some(_)) => self.t.advanced += 1,
            Ok(None) => {
                self.t.own += 1;
                self.t.eof_work = true;
            }
            Err(_) => {}
        }
        self.t.millis += coarse_ms() - started;
        out
    }

    fn scan(&mut self, cx: &mut Cx) -> Result<Option<Member>, StorageError> {
        loop {
            if self.buf.is_empty() {
                if self.end {
                    return Ok(None);
                }
                let Some(rows) =
                    cx.storage
                        .records_after(cx.db, cx.coll, self.after, CHUNK, self.backward)?
                else {
                    return Err(StorageError::Internal("this store cannot stream".into()));
                };
                self.end = rows.len() < CHUNK;
                self.buf.extend(rows);
                if self.buf.is_empty() {
                    return Ok(None);
                }
            }
            let (rid, blob) = self.buf.pop_front().expect("a row is buffered");
            self.after = Some(rid);
            cx.docs += 1;
            self.t.docs += 1;
            self.t.own += 1;
            let keep = match &self.filter {
                None => true,
                Some(f) => cx.matches(&blob, f)?,
            };
            if keep {
                return Ok(Some(Member {
                    rid,
                    doc: Some(blob),
                    entry: None,
                    stale: false,
                }));
            }
        }
    }
}

/// An entry a FETCH read ahead: the member, its document as read (`None`:
/// gone), and where the scan stood before producing it.
struct Ahead {
    member: Member,
    blob: Option<Vec<u8>>,
    snap: Snap,
}

/// A SORT_MERGE's head: a child's next document and its sort key.
struct Head {
    key: Vec<Vec<u8>>,
    member: Member,
    child: usize,
}

impl HeapOrder for Head {
    fn heap_less(&self, other: &Self) -> bool {
        self.key > other.key
    }
}

/// A SORT_MERGE: one head per child, the least returned first.
struct Merge {
    /// The sort pattern: each field, and whether it is descending.
    pattern: Vec<(String, bool)>,
    inputs: Vec<Stage>,
    /// The children without a head, in the order they are worked.
    queue: Option<VecDeque<usize>>,
    heads: Heap<Head>,
    seen: HashSet<i64>,
    /// mongod tests each result for a duplicate only when a scan below can
    /// return one: over a multikey index.
    dedup: bool,
    t: Tally,
}

/// A document's key under a sort pattern and a collation: each field's
/// value (null when missing) as the collation orders it.
fn collated_key(
    pattern: &[(String, bool)],
    blob: &[u8],
    collation: &Collation,
) -> Result<Vec<Vec<u8>>, StorageError> {
    let doc = secantus_core::bsondoc::from_bytes(blob).map_err(undecodable)?;
    Ok(pattern
        .iter()
        .map(|(field, descending)| {
            let value = secantus_core::get_path(&doc, field)
                .cloned()
                .unwrap_or(Bson::Null);
            let asc = sortkey::encode_sort_value(&value, Some(collation)).unwrap_or_default();
            if *descending {
                reversed_key(&asc)
            } else {
                asc
            }
        })
        .collect())
}

/// A member's key under a sort pattern (each field, and whether it is
/// descending), from the entry that found it.
fn pattern_key(pattern: &[(String, bool)], m: &Member) -> Vec<Vec<u8>> {
    let Some(entry) = &m.entry else {
        return Vec::new();
    };
    pattern
        .iter()
        .map(|(field, descending)| {
            let asc = entry
                .meta
                .fields
                .iter()
                .position(|f| f == field)
                .map(|at| entry.parts[at].clone())
                .unwrap_or_else(|| sortkey::encode_value(&Bson::Null, None).unwrap_or_default());
            if *descending {
                reversed_key(&asc)
            } else {
                asc
            }
        })
        .collect()
}

impl Merge {
    fn key_of(&self, m: &Member) -> Vec<Vec<u8>> {
        pattern_key(&self.pattern, m)
    }

    /// mongod's `isEOF`: every child at its end and every head handed out,
    /// which a SORT_MERGE knows without a work.
    fn is_eof(&self) -> bool {
        self.queue.as_ref().is_some_and(VecDeque::is_empty) && self.heads.is_empty()
    }

    /// Each child's work is the child's; each head handed out is a work of
    /// the merge's own.
    fn next(&mut self, cx: &mut Cx) -> Result<Option<Member>, StorageError> {
        if self.is_eof() {
            if !self.t.eof_work {
                self.t.eof_work = true;
                self.t.own += 1;
            }
            return Ok(None);
        }
        let mut queue = self
            .queue
            .take()
            .unwrap_or_else(|| (0..self.inputs.len()).collect());
        while let Some(&child) = queue.front() {
            let next = match self.inputs[child].next(cx) {
                Ok(next) => next,
                Err(e) => {
                    self.queue = Some(queue);
                    return Err(e);
                }
            };
            match next {
                Some(m) => {
                    if self.dedup {
                        self.t.dups_tested += 1;
                    }
                    if !self.seen.insert(m.rid) {
                        if self.dedup {
                            self.t.dups_dropped += 1;
                        }
                    } else {
                        queue.pop_front();
                        let key = self.key_of(&m);
                        self.heads.push(Head {
                            key,
                            member: m,
                            child,
                        });
                    }
                }
                None => {
                    queue.pop_front();
                }
            }
        }
        let top = self.heads.pop();
        if let Some(top) = &top {
            queue.push_back(top.child);
            self.t.own += 1;
            self.t.advanced += 1;
        }
        self.queue = Some(queue);
        Ok(top.map(|t| t.member))
    }
}

/// A `$near`'s search: mongod's `GeoNear2DStage` / `GeoNear2DSphereStage`,
/// run as the batches ask.
struct NearStage {
    search: NearSearch<Vec<u8>>,
    measure: Measure,
    /// The command's filter, which each document the search hands out is
    /// matched against (the rest of the query is a filter over what the
    /// stage returns).
    filter: Option<Document>,
    /// The collection as this batch's rings read it: read when the batch
    /// first reaches a ring, dropped at its end.
    held: Option<Vec<Candidate<Vec<u8>>>>,
    t: Tally,
}

/// The collection as a `$near` ring reads it: every document as it is now,
/// read once a batch (nothing writes to it while a batch reads).
struct NearSource<'s, 'c, 'a> {
    cx: &'c Cx<'a>,
    measure: &'s Measure,
    held: &'s mut Option<Vec<Candidate<Vec<u8>>>>,
}

impl Source<Vec<u8>> for NearSource<'_, '_, '_> {
    type Error = StorageError;

    fn candidates(&mut self) -> Result<&[Candidate<Vec<u8>>], StorageError> {
        if self.held.is_none() {
            let Some(rows) =
                self.cx
                    .storage
                    .collection_at_most(self.cx.db, self.cx.coll, usize::MAX)?
            else {
                return Err(StorageError::Internal("this store cannot stream".into()));
            };
            let mut out = Vec::with_capacity(rows.len());
            for (rid, blob) in rows {
                let doc = secantus_core::bsondoc::from_bytes(&blob).map_err(undecodable)?;
                out.push(self.measure.candidate(rid, &doc, true, blob));
            }
            *self.held = Some(out);
        }
        Ok(self.held.as_deref().unwrap_or(&[]))
    }
}

/// A stage of the plan, as the stream runs it.
enum Stage {
    /// A point read through a unique index (`EXPRESS_IXSCAN`, `IDHACK`):
    /// the one document whose key is the value, one key and one document
    /// examined when it is there, none when it is not.
    Point {
        filter: Document,
        done: bool,
        t: Tally,
    },
    Coll(CollScan),
    Ix(Box<IndexScan>),
    /// Reads each document: `counted` unless the plan is covered. The
    /// topmost FETCH over an index scan reads its documents a chunk at a
    /// time (`ahead`), no more than the batch can still take, and gives back
    /// at a yield what it did not hand out.
    Fetch {
        filter: Option<Document>,
        input: Box<Stage>,
        counted: bool,
        ahead: Option<VecDeque<Ahead>>,
        t: Tally,
    },
    Skip {
        left: i64,
        input: Box<Stage>,
        t: Tally,
    },
    Or {
        inputs: Vec<Stage>,
        pos: usize,
        seen: HashSet<i64>,
        t: Tally,
    },
    Merge(Box<Merge>),
    /// A blocking sort of index keys below the FETCH: everything is read
    /// and sorted for the first batch, and each document fetched as a batch
    /// reaches it (mongod 8.2.11: one deleted since is not returned, one
    /// whose key moved is dropped, one changed otherwise is as it is now).
    SortKeys {
        pattern: Vec<(String, bool)>,
        limit: Option<i64>,
        input: Box<Stage>,
        sorted: Option<VecDeque<Member>>,
        /// The command's collation: the keys' values compare under it, as
        /// mongod's sort compares them whatever the index's collation.
        collation: Option<Collation>,
        t: Tally,
    },
    /// A blocking sort of whole documents above the FETCH: every document
    /// read, and examined, for the first batch, then sorted -- documents
    /// that compare equal in the order they came -- and handed out as each
    /// batch asks, as they were read (mongod's `SORT` holds them).
    SortDocs {
        pattern: Document,
        limit: Option<i64>,
        input: Box<Stage>,
        sorted: Option<VecDeque<Member>>,
        collation: Option<Collation>,
        budget: Option<(secantus_core::memlimit::Budgets, bool)>,
        t: Tally,
        /// What its sorter counted (`totalDataSizeSorted`, the spills).
        report: Option<Box<secantus_core::memlimit::SpillReport>>,
    },
    Near(Box<NearStage>),
}

/// Sort `all` by `pattern` as mongod's `SORT` does, holding it to its
/// memory budget (a sort past it without `allowDiskUse` is refused, 292)
/// and keeping documents that compare equal in the order they came; with
/// what its sorter counted, when it ran one.
fn sort_members(
    cx: &Cx,
    all: Vec<Member>,
    pattern: &Document,
    limit: Option<i64>,
    collation: Option<&Collation>,
    budget: Option<(secantus_core::memlimit::Budgets, bool)>,
) -> Result<
    (
        VecDeque<Member>,
        Option<secantus_core::memlimit::SpillReport>,
    ),
    StorageError,
> {
    let top = limit.filter(|n| *n > 0).map(|n| n as usize);
    let mut report = None;
    if let Some((budgets, disk)) = budget {
        let before = secantus_core::memlimit::report();
        let _disk = secantus_core::memlimit::allow_disk_use(disk);
        let _budgets = secantus_core::memlimit::with_budgets(budgets);
        let decoded: Vec<Document> = all
            .iter()
            .filter_map(|m| m.doc.as_deref())
            .map(|b| secantus_core::bsondoc::from_bytes(b).map_err(undecodable))
            .collect::<Result<_, _>>()?;
        let sizing = secantus_core::sorter::Sizing {
            source: secantus_core::sorter::SortSource::QueryLayer,
            fields: None,
            budget: budgets.sort,
            added: None,
        };
        secantus_core::sorter::sort_documents(
            decoded,
            &Bson::Document(pattern.clone()),
            &sizing,
            top,
        )
        .map_err(|f| match f.as_mongo() {
            Some((code, msg)) => StorageError::WriteError {
                code,
                errmsg: msg.to_string(),
                exec: true,
            },
            None => fault(f),
        })?;
        report = Some(secantus_core::memlimit::report().less(&before));
    }
    let blobs: Vec<&[u8]> = all
        .iter()
        .map(|m| m.doc.as_deref().unwrap_or(&[]))
        .collect();
    let (order, _) = cx.storage.sort_permutation(&blobs, pattern, collation)?;
    let mut slots: Vec<Option<Member>> = all.into_iter().map(Some).collect();
    let mut out: VecDeque<Member> = order
        .into_iter()
        .filter_map(|at| slots.get_mut(at).and_then(Option::take))
        .collect();
    if let Some(n) = top {
        out.truncate(n);
    }
    Ok((out, report))
}

/// A stage that hands its works down reached its end: in the work that
/// found its input's end, which is the input's own unless the input knew
/// it was at its end without a work (a SORT_MERGE).
fn ended(t: &mut Tally, input: &Stage) {
    if !t.eof_work {
        t.eof_work = true;
        if !input.tally().eof_work {
            t.own += 1;
        }
    }
}

/// A SORT's next result: each one handed out is a work of its own, and so
/// is the one that finds none left.
fn sorted_next(sorted: &mut Option<VecDeque<Member>>, t: &mut Tally) -> Option<Member> {
    let next = sorted.as_mut().and_then(VecDeque::pop_front);
    if next.is_some() {
        t.own += 1;
    } else if !t.eof_work {
        t.own += 1;
        t.eof_work = true;
    }
    next
}

impl Stage {
    /// The stage's tally; `None` for a scan, which keeps its own.
    fn tally_mut(&mut self) -> &mut Tally {
        match self {
            Stage::Point { t, .. }
            | Stage::Fetch { t, .. }
            | Stage::Skip { t, .. }
            | Stage::Or { t, .. }
            | Stage::SortKeys { t, .. }
            | Stage::SortDocs { t, .. } => t,
            Stage::Merge(m) => &mut m.t,
            Stage::Coll(s) => &mut s.t,
            Stage::Ix(s) => &mut s.t,
            Stage::Near(n) => &mut n.t,
        }
    }

    fn tally(&self) -> &Tally {
        match self {
            Stage::Point { t, .. }
            | Stage::Fetch { t, .. }
            | Stage::Skip { t, .. }
            | Stage::Or { t, .. }
            | Stage::SortKeys { t, .. }
            | Stage::SortDocs { t, .. } => t,
            Stage::Merge(m) => &m.t,
            Stage::Coll(s) => &s.t,
            Stage::Ix(s) => &s.t,
            Stage::Near(n) => &n.t,
        }
    }

    /// A scan below reads a multikey index.
    fn multikey(&self) -> bool {
        match self {
            Stage::Ix(s) => s.meta.multikey,
            Stage::Fetch { input, .. } | Stage::Skip { input, .. } => input.multikey(),
            _ => false,
        }
    }

    /// mongod's `isEOF` for the stage: a scan, an OR or a SORT once a work
    /// found its end; a FETCH or a SKIP when its input is; a SORT_MERGE
    /// once every child is at its end and every head handed out.
    fn is_eof(&self) -> bool {
        match self {
            Stage::Point { done, .. } => *done,
            Stage::Coll(_) | Stage::Ix(_) | Stage::SortKeys { .. } | Stage::SortDocs { .. } => {
                self.tally().eof_work
            }
            Stage::Fetch { input, ahead, .. } => {
                ahead.as_ref().is_none_or(VecDeque::is_empty) && input.is_eof()
            }
            Stage::Skip { input, .. } => input.is_eof(),
            Stage::Or { inputs, pos, .. } => *pos >= inputs.len(),
            Stage::Merge(m) => m.is_eof(),
            Stage::Near(n) => n.search.is_eof(),
        }
    }

    fn next(&mut self, cx: &mut Cx) -> Result<Option<Member>, StorageError> {
        match self {
            // The scans time their own calls.
            Stage::Coll(scan) => return scan.next(cx),
            Stage::Ix(scan) => return scan.next(cx),
            Stage::Merge(merge) => {
                let started = coarse_ms();
                let out = merge.next(cx);
                merge.t.millis += coarse_ms() - started;
                return out;
            }
            _ => {}
        }
        let started = coarse_ms();
        let out = self.step(cx);
        let t = self.tally_mut();
        if let Ok(Some(_)) = &out {
            t.advanced += 1;
        }
        t.millis += coarse_ms() - started;
        out
    }

    /// One result of the stage's, counted by [`Self::next`].
    fn step(&mut self, cx: &mut Cx) -> Result<Option<Member>, StorageError> {
        match self {
            Stage::Point { filter, done, t } => {
                if *done {
                    return Ok(None);
                }
                *done = true;
                let Some(matched) = cx
                    .storage
                    .matching_rows(cx.db, cx.coll, filter, None, None, cx.vars)?
                else {
                    return Err(StorageError::Internal("this store cannot stream".into()));
                };
                let Some((rid, blob)) = matched.rows.into_iter().next() else {
                    return Ok(None);
                };
                cx.keys += 1;
                cx.docs += 1;
                t.keys += 1;
                t.docs += 1;
                Ok(Some(Member {
                    rid,
                    doc: Some(blob),
                    entry: None,
                    stale: false,
                }))
            }
            Stage::Coll(_) | Stage::Ix(_) | Stage::Merge(_) => {
                unreachable!("counted by Stage::next")
            }
            Stage::Near(n) => {
                let NearStage {
                    search,
                    measure,
                    filter,
                    held,
                    t,
                } = n.as_mut();
                let (keys, docs) = (search.keys_examined, search.docs_examined);
                let out = {
                    let mut source = NearSource {
                        cx: &*cx,
                        measure,
                        held,
                    };
                    loop {
                        t.own += 1;
                        let Some((rid, blob)) = search.next(&mut source)? else {
                            t.eof_work = true;
                            break None;
                        };
                        if let Some(f) = filter.as_ref() {
                            if !source.cx.matches(&blob, f)? {
                                continue;
                            }
                        }
                        break Some(Member {
                            rid,
                            doc: Some(blob),
                            entry: None,
                            stale: false,
                        });
                    }
                };
                let keys = search.keys_examined - keys;
                let docs = search.docs_examined - docs;
                cx.keys += keys;
                cx.docs += docs;
                t.keys += keys;
                t.docs += docs;
                Ok(out)
            }
            Stage::Fetch {
                filter,
                input,
                counted,
                ahead,
                t,
            } => {
                // Its input at its end: the work that finds it there. The
                // FETCH a covered plan has here is no stage of mongod's: the
                // projection above works its input whatever it is at (its
                // SORT_MERGE spends a work finding itself at its end,
                // measured 8.2.11).
                if *counted && ahead.as_ref().is_none_or(VecDeque::is_empty) && input.is_eof() {
                    ended(t, input);
                    return Ok(None);
                }
                loop {
                    if let (Some(queue), Stage::Ix(scan)) = (ahead.as_mut(), input.as_mut()) {
                        if queue.is_empty() {
                            let room = match cx.want {
                                0 => CHUNK,
                                n => n.min(CHUNK),
                            };
                            let mut members = Vec::with_capacity(room);
                            while members.len() < room {
                                let snap = scan.snapshot(cx.keys);
                                match scan.next(cx)? {
                                    Some(m) => members.push((m, snap)),
                                    None => break,
                                }
                            }
                            if members.is_empty() {
                                ended(t, input);
                                return Ok(None);
                            }
                            let rids: Vec<i64> = members.iter().map(|(m, _)| m.rid).collect();
                            let Some(found) = cx.storage.records_by_id(cx.db, cx.coll, &rids)?
                            else {
                                return Err(StorageError::Internal(
                                    "this store cannot stream".into(),
                                ));
                            };
                            for ((member, snap), blob) in members.into_iter().zip(found) {
                                queue.push_back(Ahead { member, blob, snap });
                            }
                        }
                        let Some(Ahead {
                            mut member, blob, ..
                        }) = queue.pop_front()
                        else {
                            return Ok(None);
                        };
                        let Some(blob) = blob else { continue };
                        if *counted {
                            cx.docs += 1;
                            t.docs += 1;
                        }
                        if let Some(f) = filter.as_ref() {
                            if !cx.matches(&blob, f)? {
                                continue;
                            }
                        }
                        member.doc = Some(blob);
                        return Ok(Some(member));
                    }
                    let Some(mut m) = input.next(cx)? else {
                        // The projection above a covered plan works its
                        // input again, which finds itself at its end.
                        if !*counted && !input.tally().eof_work {
                            input.next(cx)?;
                        }
                        ended(t, input);
                        return Ok(None);
                    };
                    if m.doc.is_none() {
                        let Some(blob) = cx.read(m.rid)? else {
                            continue;
                        };
                        // An entry held over a yield is fetched only if the
                        // document still has its key (mongod drops it).
                        if m.stale {
                            if let Some(entry) = &m.entry {
                                if !holds_entry(entry, &blob)? {
                                    continue;
                                }
                            }
                        }
                        if *counted {
                            cx.docs += 1;
                            t.docs += 1;
                        }
                        m.doc = Some(blob);
                    }
                    if let (Some(f), Some(blob)) = (filter.as_ref(), m.doc.as_ref()) {
                        if !cx.matches(blob, f)? {
                            continue;
                        }
                    }
                    return Ok(Some(m));
                }
            }
            Stage::Skip { left, input, t } => {
                if input.is_eof() {
                    ended(t, input);
                    return Ok(None);
                }
                while *left > 0 {
                    if input.next(cx)?.is_none() {
                        ended(t, input);
                        return Ok(None);
                    }
                    *left -= 1;
                }
                let next = input.next(cx)?;
                if next.is_none() {
                    ended(t, input);
                }
                Ok(next)
            }
            Stage::Or {
                inputs,
                pos,
                seen,
                t,
            } => {
                while *pos < inputs.len() {
                    match inputs[*pos].next(cx)? {
                        Some(m) => {
                            // mongod's OR tests every result for a duplicate.
                            t.dups_tested += 1;
                            if seen.insert(m.rid) {
                                return Ok(Some(m));
                            }
                            t.dups_dropped += 1;
                        }
                        None => {
                            *pos += 1;
                            if *pos == inputs.len() {
                                // The last child's end is the OR's.
                                ended(t, &inputs[*pos - 1]);
                            }
                        }
                    }
                }
                Ok(None)
            }
            Stage::SortKeys {
                pattern,
                limit,
                input,
                sorted,
                collation,
                t,
            } => {
                if sorted.is_none() {
                    let mut all: Vec<(Vec<Vec<u8>>, Member)> = Vec::new();
                    while let Some(m) = input.next(cx)? {
                        let key = match collation {
                            // The values the keys hold, compared under the
                            // collation: read from the document as it is
                            // at the sort (a key is not its value here),
                            // which is no FETCH of mongod's, so not counted.
                            Some(c) => match cx.read(m.rid)? {
                                Some(blob) => collated_key(pattern, &blob, c)?,
                                None => pattern_key(pattern, &m),
                            },
                            None => pattern_key(pattern, &m),
                        };
                        all.push((key, m));
                    }
                    all.sort_by(|a, b| a.0.cmp(&b.0));
                    if let Some(n) = limit.filter(|n| *n > 0) {
                        all.truncate(n as usize);
                    }
                    *sorted = Some(all.into_iter().map(|(_, m)| m).collect());
                }
                Ok(sorted_next(sorted, t))
            }
            Stage::SortDocs {
                pattern,
                limit,
                input,
                sorted,
                collation,
                budget,
                t,
                report,
            } => {
                if sorted.is_none() {
                    let mut all: Vec<Member> = Vec::new();
                    while let Some(mut m) = input.next(cx)? {
                        if m.doc.is_none() {
                            // Read to sort, by a stage below that did not.
                            match cx.read(m.rid)? {
                                Some(blob) => m.doc = Some(blob),
                                None => continue,
                            }
                        }
                        all.push(m);
                    }
                    let (out, counted) =
                        sort_members(cx, all, pattern, *limit, collation.as_ref(), *budget)?;
                    *sorted = Some(out);
                    *report = counted.map(Box::new);
                }
                Ok(sorted_next(sorted, t))
            }
        }
    }

    /// The end of a batch: what was read ahead is dropped, positions and
    /// held heads kept (the heads to be checked when fetched). A FETCH that
    /// read ahead rewinds its scan, and answers the batch's keys examined
    /// before it read the entries it gives back.
    fn yield_now(&mut self) -> Option<i64> {
        if let Stage::Fetch {
            input,
            ahead: Some(queue),
            ..
        } = self
        {
            if let (Some(first), Stage::Ix(scan)) = (queue.pop_front(), input.as_mut()) {
                let keys = first.snap.keys;
                let unread: Vec<i64> = std::iter::once(first.member.rid)
                    .chain(queue.drain(..).map(|a| a.member.rid))
                    .collect();
                scan.rewind(first.snap, &unread);
                return Some(keys);
            }
        }
        self.yield_children();
        None
    }

    fn yield_children(&mut self) {
        match self {
            Stage::Point { .. } => {}
            // What the rings read is read again by the next ring; the queue
            // keeps what it buffered.
            Stage::Near(n) => n.held = None,
            Stage::Coll(scan) => {
                scan.buf.clear();
                scan.end = false;
            }
            Stage::Ix(scan) => {
                scan.buf.clear();
                scan.end = false;
            }
            Stage::Fetch { input, .. } | Stage::Skip { input, .. } => input.yield_children(),
            Stage::Or { inputs, .. } => inputs.iter_mut().for_each(Stage::yield_children),
            Stage::Merge(merge) => {
                merge.inputs.iter_mut().for_each(Stage::yield_children);
                for h in merge.heads.iter_mut() {
                    h.member.stale = h.member.doc.is_none();
                }
            }
            Stage::SortKeys { input, sorted, .. } => {
                input.yield_children();
                for m in sorted.iter_mut().flatten() {
                    m.stale = m.doc.is_none();
                }
            }
            // The sorted documents are held as they were read.
            Stage::SortDocs { input, .. } => input.yield_children(),
        }
    }

    /// Whether the plan sorts in memory: mongod's `hasSortStage`.
    fn sorts(&self) -> bool {
        match self {
            Stage::SortKeys { .. } | Stage::SortDocs { .. } => true,
            Stage::Point { .. } | Stage::Coll(_) | Stage::Ix(_) | Stage::Near(_) => false,
            Stage::Fetch { input, .. } | Stage::Skip { input, .. } => input.sorts(),
            Stage::Or { inputs, .. } => inputs.iter().any(Stage::sorts),
            Stage::Merge(merge) => merge.inputs.iter().any(Stage::sorts),
        }
    }
}

/// An index scan of `ix` from its start, when one streams.
fn index_scan(ix: &IxScan, opts: &Options) -> Result<Option<IndexScan>, StorageError> {
    // The `_id` index's entries are the documents' `_id` keys, in mongod's
    // order since `_id` key format 2.
    let id = ix.index.name == "_id_";
    let fields = ix.index.fields();
    let dirs: Vec<i32> = fields
        .iter()
        .map(|(_, d)| if *d < 0 { -1 } else { 1 })
        .collect();
    let collation = ix.index.collation.clone();
    let meta = Arc::new(IndexMeta {
        name: ix.index.name.clone(),
        fields: fields.iter().map(|(f, _)| f.to_string()).collect(),
        dirs: dirs.clone(),
        collation: collation.clone(),
        sparse: ix.index.sparse,
        multikey: ix.index.is_multikey(),
        id,
    });
    // Each field's stored key for a value, as the entries hold it.
    let stored = |v: &Bson, dir: i32| -> Option<Vec<u8>> {
        if id {
            return sortkey::encode_id_key(v).ok();
        }
        sortkey::encode_value_directed(v, dir, collation.as_ref()).ok()
    };
    let join = |values: &[(Bson, i32)]| -> Option<Vec<u8>> {
        let mut out = Vec::new();
        for (i, (v, dir)) in values.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(COMPOUND_SEP);
            }
            out.extend(stored(v, *dir)?);
        }
        Some(out)
    };
    let (limits, start, done) = if ix.no_bounds {
        // `min` / `max` name every field of the index, in its order.
        let bound = |d: Option<&Document>| -> Result<Option<Vec<u8>>, ()> {
            let Some(d) = d else { return Ok(None) };
            if d.keys()
                .map(String::as_str)
                .ne(meta.fields.iter().map(String::as_str))
            {
                return Err(());
            }
            let values: Vec<(Bson, i32)> = d.values().cloned().zip(dirs.iter().copied()).collect();
            join(&values).map(Some).ok_or(())
        };
        let (Ok(min), Ok(max)) = (bound(opts.min), bound(opts.max)) else {
            return Ok(None);
        };
        let start = if ix.backward {
            max.clone().unwrap_or_default()
        } else {
            min.clone().unwrap_or_default()
        };
        // Walking down from `max`, the store's seek starts past every entry
        // of that key; with no `max`, at the end of the index.
        let start = if ix.backward && max.is_none() {
            vec![0xFF]
        } else {
            start
        };
        (Limits::MinMax { min, max }, start, false)
    } else {
        let ranges: Vec<Ranges> = ix
            .bounds
            .iter()
            .zip(&dirs)
            .map(|(list, dir)| ranges(list, (*dir < 0) != ix.backward, collation.as_ref()))
            .collect();
        // The first entry the scan can need: each field's first interval
        // start in the scan's order (the low ends, or the high ends for a
        // field read against its direction), as the entries hold it.
        let mut firsts: Vec<(Bson, i32)> = Vec::with_capacity(dirs.len());
        for (list, dir) in ix.bounds.iter().zip(&dirs) {
            let reversed = (*dir < 0) != ix.backward;
            let pick = list
                .iter()
                .map(|iv| if reversed { &iv.hi } else { &iv.lo })
                .min_by_key(|v| super::exec::directed(v, reversed, collation.as_ref()));
            let Some(v) = pick else {
                firsts.clear();
                break;
            };
            firsts.push((v.clone(), *dir));
        }
        let done = ranges.iter().any(Vec::is_empty) || firsts.len() != dirs.len();
        let start = if done {
            Vec::new()
        } else {
            match join(&firsts) {
                Some(s) => s,
                None => return Ok(None),
            }
        };
        (
            Limits::Ranges {
                ranges,
                contiguous: contiguous(&ix.bounds),
            },
            start,
            done,
        )
    };
    Ok(Some(IndexScan {
        meta,
        backward: ix.backward,
        limits,
        start,
        done,
        last: None,
        buf: VecDeque::new(),
        end: false,
        seen: HashSet::new(),
        distinct: None,
        run: None,
        count_scan: false,
        ended: false,
        chunk: FIRST_CHUNK,
        read: None,
        t: Tally::default(),
    }))
}

/// The RecordIds of every entry a scan of `ix` reads from the store in its
/// first `works` units of work -- inside the bounds, sought past or
/// ending it -- and whether it came to its end within them: what a trial of
/// the plan over a large collection needs of it ([`super::order`]).
/// `width`: a DISTINCT_SCAN of that many leading fields. `None`: the index
/// does not stream.
pub(crate) fn scan_reach(
    storage: &dyn Storage,
    db: &str,
    coll: &str,
    ix: &IxScan,
    width: Option<usize>,
    works: i64,
) -> Result<Option<(Vec<i64>, bool)>, StorageError> {
    if !ix.index.plain {
        return Ok(None);
    }
    let Some(mut scan) = index_scan(ix, &Options::default())? else {
        return Ok(None);
    };
    scan.distinct = width;
    scan.read = Some(Vec::new());
    let vars = Document::new();
    let mut cx = Cx {
        storage,
        db,
        coll,
        vars: &vars,
        keys: 0,
        docs: 0,
        want: 0,
    };
    let mut ended = false;
    for _ in 0..works {
        if matches!(scan.work(&mut cx)?, Step::End) {
            ended = true;
            break;
        }
    }
    Ok(Some((scan.read.take().unwrap_or_default(), ended)))
}

/// Whether an index scan can stream (see the module's doc).
fn streams(ix: &IxScan) -> bool {
    ix.filter.is_none() && ix.index.plain
}

/// The stage `node` is, when it streams. `top`: the topmost FETCH, which
/// matches the command's own filter (`command`); the others match their own.
fn stage_of(
    node: &Node,
    command: Option<&Document>,
    opts: &Options,
    covered: bool,
) -> Result<Option<Stage>, StorageError> {
    let scan = |ix: &IxScan| -> Result<Option<Stage>, StorageError> {
        if !streams(ix) {
            return Ok(None);
        }
        Ok(index_scan(ix, opts)?.map(|s| Stage::Ix(Box::new(s))))
    };
    let stage = match node {
        Node::Express { index, value } => {
            let Some(field) = index.key.keys().next() else {
                return Ok(None);
            };
            Some(Stage::Point {
                filter: doc! { field.clone(): { "$eq": value.clone() } },
                done: false,
                t: Tally::default(),
            })
        }
        Node::IdHack(value) => Some(Stage::Point {
            filter: doc! { "_id": { "$eq": value.clone() } },
            done: false,
            t: Tally::default(),
        }),
        Node::CollScan { backward, .. } => Some(Stage::Coll(CollScan::new(
            *backward,
            command.cloned(),
            false,
        ))),
        Node::Fetch { filter, input } => {
            let Some(input) = stage_of(input, None, opts, false)? else {
                return Ok(None);
            };
            let filter = match command {
                Some(c) => Some(c.clone()),
                None => filter.clone(),
            };
            Some(Stage::Fetch {
                filter,
                input: Box::new(input),
                counted: true,
                ahead: None,
                t: Tally::default(),
            })
        }
        // mongod skips index entries below the FETCH when nothing is
        // filtered after it: the skipped keys are examined, their
        // documents never read (`FETCH <- SKIP <- IXSCAN`).
        Node::Skip(n, input) if command.is_none() => {
            let Some(input) = stage_of(input, None, opts, false)? else {
                return Ok(None);
            };
            Some(Stage::Skip {
                left: *n,
                input: Box::new(input),
                t: Tally::default(),
            })
        }
        Node::IxScan(ix) => {
            let Some(s) = scan(ix)? else { return Ok(None) };
            // A scan at the top of the plan (covered, or a RETURN_KEY's)
            // answers from the keys and examines no document. This store's
            // keys are not the values they stand for, so each document is
            // read to answer, and not counted.
            if command.is_some() || covered {
                Some(Stage::Fetch {
                    filter: None,
                    input: Box::new(s),
                    counted: false,
                    ahead: None,
                    t: Tally::default(),
                })
            } else {
                Some(s)
            }
        }
        // The first document of each key a `$group` answers from
        // (`$groupByDistinctScan`), covered or fetched.
        Node::DistinctScan(ix, width, _) => {
            if !streams(ix) {
                return Ok(None);
            }
            let Some(mut s) = index_scan(ix, opts)? else {
                return Ok(None);
            };
            s.distinct = Some(*width);
            Some(Stage::Fetch {
                filter: None,
                input: Box::new(Stage::Ix(Box::new(s))),
                counted: !covered,
                ahead: None,
                t: Tally::default(),
            })
        }
        Node::Or(inputs) => {
            let mut stages = Vec::with_capacity(inputs.len());
            for input in inputs {
                let Some(s) = stage_of(input, None, opts, false)? else {
                    return Ok(None);
                };
                stages.push(s);
            }
            Some(Stage::Or {
                inputs: stages,
                pos: 0,
                seen: HashSet::new(),
                t: Tally::default(),
            })
        }
        Node::Sort {
            pattern,
            limit,
            on_keys: true,
            input,
        } => {
            let pattern: Vec<(String, bool)> = pattern
                .iter()
                .map(|(k, v)| (k.clone(), crate::util::as_i64(v).unwrap_or(1) < 0))
                .collect();
            // Each scan below holds every sort field: the keys sort.
            fn holds(node: &Node, pattern: &[(String, bool)]) -> bool {
                match node {
                    Node::IxScan(ix) => pattern.iter().all(|(f, _)| ix.index.position(f).is_some()),
                    Node::Or(inputs) => inputs.iter().all(|i| holds(i, pattern)),
                    _ => false,
                }
            }
            if !holds(input, &pattern) {
                return Ok(None);
            }
            let Some(input) = stage_of(input, None, opts, false)? else {
                return Ok(None);
            };
            Some(Stage::SortKeys {
                pattern,
                limit: *limit,
                input: Box::new(input),
                sorted: None,
                collation: opts.collation.cloned(),
                t: Tally::default(),
            })
        }
        Node::SortMerge { pattern, inputs } => {
            let pattern: Vec<(String, bool)> = pattern
                .iter()
                .map(|(k, v)| (k.clone(), crate::util::as_i64(v).unwrap_or(1) < 0))
                .collect();
            let mut stages = Vec::with_capacity(inputs.len());
            for input in inputs {
                // Each child is an index scan holding every sort field (a
                // FETCH above one that filters), so its heads are compared
                // by their keys, as mongod's are.
                let ix = match input {
                    Node::IxScan(ix) => ix,
                    Node::Fetch { input: inner, .. } => match inner.as_ref() {
                        Node::IxScan(ix) => ix,
                        _ => return Ok(None),
                    },
                    _ => return Ok(None),
                };
                if !pattern.iter().all(|(f, _)| ix.index.position(f).is_some()) {
                    return Ok(None);
                }
                let Some(s) = stage_of(input, None, opts, false)? else {
                    return Ok(None);
                };
                stages.push(s);
            }
            let dedup = stages.iter().any(Stage::multikey);
            Some(Stage::Merge(Box::new(Merge {
                pattern,
                inputs: stages,
                queue: None,
                heads: Heap::new(),
                seen: HashSet::new(),
                dedup,
                t: Tally::default(),
            })))
        }
        _ => None,
    };
    Ok(stage)
}

/// A read in progress: the plan and where it stands.
pub(crate) struct Stream {
    db: String,
    coll: String,
    root: Stage,
    vars: Document,
    /// Results still to skip, and still to return.
    skip: i64,
    limit: Option<i64>,
    /// Results read for a batch that did not fit in it: the next batch's
    /// first (mongod stashes them the same way).
    stashed: VecDeque<Found>,
    done: bool,
    keys: i64,
    docs: i64,
    /// The key each result was found at is asked for.
    want_keys: bool,
    /// A COUNT_SCAN: results are index entries, counted without their
    /// documents.
    keys_only: bool,
    /// What the run's stages at the top did, which the stream does for them
    /// (RM-359): `skip` at its start, the LIMIT reached, and the work that
    /// found it so; the yields between batches.
    skip_given: i64,
    limit_reached: bool,
    limit_eof_work: bool,
    /// No batch has been read yet: `find`'s, whose executor works its
    /// LIMIT once more after the last result, where a `getMore`'s asks
    /// whether the plan is at its end first (measured 8.2.11).
    first: bool,
    yields: Yields,
    /// The yields the last batch's profile entry reports.
    shown: Yields,
    /// A write's read ([`Stream::pull_for_write`]): the error that ended it
    /// part-way, kept apart so the results matched before it are returned.
    failed: Option<Option<StorageError>>,
    /// The keys and documents a trial examined before the first batch
    /// ([`Stream::buffer_trial`]), which that batch counts.
    carried: (i64, i64),
    /// The trial's winner ran out, and has not been worked since: `find`'s
    /// first batch, past what the trial buffered, works it once more and
    /// finds it at its end (measured 8.2.11: the FETCH over an IXSCAN works
    /// once more than the scan).
    ran_out: bool,
}

/// The yields a run went through: a cursor's between its batches, a
/// write's around each document it writes.
#[derive(Clone, Copy, Default)]
pub(crate) struct Yields {
    pub saved: i64,
    pub restored: i64,
}

impl Stream {
    /// A read of `plan` over `db.coll`, when it is one that streams. Each
    /// document is matched against the command's own `filter` at the
    /// topmost FETCH (or the scan of the collection), not the plan's
    /// residual filter there: the plan's is a rewrite for `explain`, and the
    /// command's is what a `find` answers and validates (an index's bounds
    /// only narrow which documents are fetched, so the counts are the same).
    /// A FETCH below an OR keeps its own filter, which decides what the OR
    /// passes on and so remembers.
    pub(crate) fn of_plan(
        plan: &Node,
        db: &str,
        coll: &str,
        filter: &Document,
        vars: &Document,
    ) -> Result<Option<Stream>, StorageError> {
        Stream::of_plan_with(plan, db, coll, filter, vars, &Options::default())
    }

    /// [`Self::of_plan`], with what the read asks beyond the plan.
    pub(crate) fn of_plan_with(
        plan: &Node,
        db: &str,
        coll: &str,
        filter: &Document,
        vars: &Document,
        opts: &Options,
    ) -> Result<Option<Stream>, StorageError> {
        // A LIMIT over a SKIP, and the projection at any level of them (a
        // covered plan's is below its LIMIT): the caller projects each batch.
        let mut node = plan;
        let mut covered = false;
        let (mut limit, mut skip) = (None, 0);
        loop {
            match node {
                Node::Projection { name, input, .. } => {
                    covered = *name == "PROJECTION_COVERED";
                    node = input;
                }
                Node::ReturnKey(input) | Node::Subplan(input) => node = input,
                Node::Limit(n, input) if limit.is_none() && skip == 0 => {
                    limit = Some(*n);
                    node = input;
                }
                Node::Skip(n, input) if skip == 0 => {
                    skip = *n;
                    node = input;
                }
                _ => break,
            }
        }
        let command = (!filter.is_empty()).then(|| filter.clone());
        // A blocking sort of documents at the top sorts what the plan below
        // it returns, whose topmost FETCH matches the command's filter.
        // A sort over what is no index scan sorts whole documents, whatever
        // type mongod prints for it (a `findAndModify`'s SORT over a
        // COLLSCAN is `default`).
        let of_keys = |n: &Node| match n {
            Node::IxScan(_) => true,
            Node::Or(inputs) => inputs.iter().all(|i| matches!(i, Node::IxScan(_))),
            _ => false,
        };
        if let Node::Sort {
            pattern,
            limit: sort_limit,
            on_keys,
            input,
        } = node
        {
            if *on_keys && of_keys(input) {
                // A sort of index keys below the FETCH: `stage_of`'s.
            } else {
                let Some(mut below) = Stream::of_plan_with(input, db, coll, filter, vars, opts)?
                else {
                    return Ok(None);
                };
                let inner = std::mem::replace(
                    &mut below.root,
                    Stage::Coll(CollScan::new(false, None, true)),
                );
                below.root = Stage::SortDocs {
                    pattern: pattern.clone(),
                    limit: *sort_limit,
                    input: Box::new(inner),
                    sorted: None,
                    collation: opts.collation.cloned(),
                    budget: opts.sort_budget,
                    t: Tally::default(),
                    report: None,
                };
                below.skip = skip;
                below.skip_given = skip;
                below.limit = limit;
                return Ok(Some(below));
            }
        }
        // The topmost FETCH (or the collection scan) matches the command's
        // filter; a bare scan at the top is wrapped in a reading FETCH.
        let top = match node {
            Node::Fetch { .. } | Node::CollScan { .. } => Some(command.clone().unwrap_or_default()),
            _ => None,
        };
        let root = match top {
            Some(f) => stage_of(node, Some(&f), opts, covered)?,
            None => stage_of(node, None, opts, covered).map(|s| {
                s.map(|s| match s {
                    already @ Stage::Fetch { .. } => already,
                    other => Stage::Fetch {
                        filter: None,
                        input: Box::new(other),
                        counted: false,
                        ahead: None,
                        t: Tally::default(),
                    },
                })
            })?,
        };
        let Some(mut root) = root else {
            return Ok(None);
        };
        if let Stage::Fetch { input, ahead, .. } = &mut root {
            if matches!(input.as_ref(), Stage::Ix(_)) {
                *ahead = Some(VecDeque::new());
            }
        }
        // An empty filter matches every document: nothing to run.
        if command.is_none() {
            clear_top_filter(&mut root);
        }
        Ok(Some(Stream {
            db: db.to_string(),
            coll: coll.to_string(),
            root,
            vars: vars.clone(),
            skip,
            limit,
            stashed: VecDeque::new(),
            done: false,
            keys: 0,
            docs: 0,
            want_keys: opts.keys,
            keys_only: false,
            skip_given: skip,
            limit_reached: false,
            limit_eof_work: false,
            first: true,
            yields: Yields::default(),
            shown: Yields::default(),
            failed: None,
            carried: (0, 0),
            ran_out: false,
        }))
    }

    /// A `$near`'s read over `db.coll`: the ring search `measure` makes,
    /// bounded on the index's other fields by `base`, each document it
    /// returns matched against `filter`, after `skip`, up to `limit` (0: no
    /// limit).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn of_near(
        db: &str,
        coll: &str,
        filter: &Document,
        vars: &Document,
        measure: Measure,
        base: Vec<FieldBounds>,
        skip: i64,
        limit: i64,
    ) -> Stream {
        let root = Stage::Near(Box::new(NearStage {
            search: NearSearch::new(measure.clone(), base),
            measure,
            filter: (!filter.is_empty()).then(|| filter.clone()),
            held: None,
            t: Tally::default(),
        }));
        Stream {
            db: db.to_string(),
            coll: coll.to_string(),
            root,
            vars: vars.clone(),
            skip: skip.max(0),
            limit: (limit > 0).then_some(limit),
            stashed: VecDeque::new(),
            done: false,
            keys: 0,
            docs: 0,
            want_keys: false,
            keys_only: false,
            skip_given: skip.max(0),
            limit_reached: false,
            limit_eof_work: false,
            first: true,
            yields: Yields::default(),
            shown: Yields::default(),
            failed: None,
            carried: (0, 0),
            ran_out: false,
        }
    }

    /// The read of a `count`'s plan below its COUNT stage (`input`), when it
    /// streams: a COUNT_SCAN reads index entries alone, anything else as a
    /// `find` of the same plan reads.
    pub(crate) fn of_count(
        input: &Node,
        db: &str,
        coll: &str,
        filter: &Document,
        vars: &Document,
        opts: &Options,
    ) -> Result<Option<Stream>, StorageError> {
        let Node::CountScan(ix) = input else {
            return Stream::of_plan_with(input, db, coll, filter, vars, opts);
        };
        if !streams(ix) {
            return Ok(None);
        }
        let Some(mut scan) = index_scan(ix, opts)? else {
            return Ok(None);
        };
        scan.count_scan = true;
        Ok(Some(Stream {
            db: db.to_string(),
            coll: coll.to_string(),
            root: Stage::Ix(Box::new(scan)),
            vars: vars.clone(),
            skip: 0,
            limit: None,
            stashed: VecDeque::new(),
            done: false,
            keys: 0,
            docs: 0,
            want_keys: false,
            keys_only: true,
            skip_given: 0,
            limit_reached: false,
            limit_eof_work: false,
            first: true,
            yields: Yields::default(),
            shown: Yields::default(),
            failed: None,
            carried: (0, 0),
            ran_out: false,
        }))
    }

    /// Run a `count` over the read: how many results there are after
    /// `skip`, up to `limit` (0: no limit), and what reading them examined.
    /// mongod's COUNT stops reading at its limit.
    pub(crate) fn count(
        &mut self,
        storage: &dyn Storage,
        skip: i64,
        limit: i64,
    ) -> Result<(i64, i64, i64), StorageError> {
        self.skip = skip.max(0);
        self.skip_given = self.skip;
        self.limit = (limit > 0).then_some(limit);
        let (mut n, mut keys, mut docs) = (0i64, 0i64, 0i64);
        loop {
            let batch = self.batch(storage, CHUNK, usize::MAX, false)?;
            n += batch.docs.len() as i64;
            keys += batch.keys_examined;
            docs += batch.docs_examined;
            if batch.exhausted {
                return Ok((n, keys, docs));
            }
        }
    }

    /// Every result of the read, with each one's RecordId, and what reading
    /// them examined: a read made whole, as a write's or a `distinct`'s is.
    pub(crate) fn read_all(&mut self, storage: &dyn Storage) -> Result<ReadAll, StorageError> {
        let mut out = ReadAll::default();
        loop {
            let batch = self.batch(storage, CHUNK, usize::MAX, false)?;
            out.keys_examined += batch.keys_examined;
            out.docs_examined += batch.docs_examined;
            out.rids.extend(batch.rids);
            out.docs.extend(batch.docs);
            out.keys.extend(batch.keys);
            if batch.exhausted {
                return Ok(out);
            }
        }
    }

    /// The plan reads one index scan (or none): mongod gives a document's
    /// `{$meta: "indexKey"}` only then (measured 8.2.11).
    pub(crate) fn single_scan(&self) -> bool {
        fn scans(stage: &Stage) -> usize {
            match stage {
                Stage::Coll(_) => 0,
                Stage::Ix(_) | Stage::Point { .. } | Stage::Near(_) => 1,
                Stage::Fetch { input, .. } | Stage::Skip { input, .. } => scans(input),
                Stage::Or { inputs, .. } => inputs.iter().map(scans).sum(),
                Stage::Merge(merge) => merge.inputs.iter().map(scans).sum(),
                Stage::SortKeys { input, .. } | Stage::SortDocs { input, .. } => scans(input),
            }
        }
        scans(&self.root) <= 1
    }

    /// Whether the plan sorts in memory: mongod's `hasSortStage`, which a
    /// cursor's every batch reports.
    pub(crate) fn has_sort_stage(&self) -> bool {
        self.root.sorts()
    }

    /// Up to `want` results that pass the filter (`want` 0: no count
    /// limit), and none past the one that brings them to `budget` bytes,
    /// read on from where the last batch stopped.
    fn read(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        budget: Option<usize>,
    ) -> Result<Vec<Found>, StorageError> {
        let mut cx = Cx {
            storage,
            db: &self.db,
            coll: &self.coll,
            vars: &self.vars,
            keys: self.keys,
            docs: self.docs,
            want: 0,
        };
        let mut out = Vec::new();
        let mut bytes = 0usize;
        let result = (|| {
            while !((want > 0 && out.len() >= want) || budget.is_some_and(|b| bytes >= b)) {
                // A `getMore`'s executor asks whether the plan is at its
                // end before it works it (a SORT_MERGE knows it is).
                if !self.first && self.root.is_eof() {
                    self.done = true;
                    break;
                }
                cx.want = if want > 0 { want - out.len() } else { 0 };
                let Some(m) = self.root.next(&mut cx)? else {
                    self.done = true;
                    break;
                };
                let doc = match m.doc {
                    Some(doc) => doc,
                    // A COUNT_SCAN's result is its entry.
                    None if self.keys_only => Vec::new(),
                    None => continue,
                };
                bytes += doc.len();
                out.push(Found {
                    doc,
                    rid: m.rid,
                    entry: m.entry,
                });
            }
            Ok(())
        })();
        self.keys = cx.keys;
        self.docs = cx.docs;
        match (result, &mut self.failed) {
            // A write's read ends at the error with what it matched before.
            (Err(e), Some(failed)) => {
                *failed = Some(e);
                self.done = true;
                Ok(out)
            }
            (result, _) => result.map(|()| out),
        }
    }

    /// The next batch: up to `want` results (0: as many as fit) within
    /// `max_bytes`, after SKIP and LIMIT, with what it examined.
    pub(crate) fn next_batch(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        max_bytes: usize,
    ) -> Result<Batch, StorageError> {
        // A `getMore` restores the plan its last batch saved.
        if !self.first {
            self.yields.restored += 1;
        }
        let out = self.batch(storage, want, max_bytes, false);
        // A batch that leaves the cursor open saves the plan for the next
        // `getMore`: `find`'s profile entry counts its save, a `getMore`'s
        // is written before it (measured 8.2.11).
        let open = out.as_ref().is_ok_and(|b| !b.exhausted);
        if !self.first {
            self.shown = self.yields;
        }
        if open {
            self.yields.saved += 1;
        }
        if self.first {
            self.shown = self.yields;
        }
        self.first = false;
        out
    }

    /// The next of a write's documents, read as mongod's write stage pulls
    /// them from its plan (RM-323): up to `want` (0: no count limit), and
    /// none past the one that brings them to `max_bytes`, with what reading
    /// them examined. A read that fails part-way returns the documents it
    /// matched before the failure, with the failure, and ends there.
    pub(crate) fn pull_for_write(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        max_bytes: usize,
    ) -> (Result<Batch, StorageError>, Option<StorageError>) {
        self.failed = Some(None);
        let batch = self.batch(storage, want, max_bytes, true);
        (batch, self.failed.take().flatten())
    }

    /// What an aggregate's cursor stage buffers: up to `want` results, and
    /// no more once they hold `max_bytes` (the document that reaches it is
    /// kept, where a batch stops short of it: measured 8.2.11).
    pub(crate) fn next_buffer(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        max_bytes: usize,
    ) -> Result<Batch, StorageError> {
        self.batch(storage, want, max_bytes, true)
    }

    /// mongod's trial of the plan against the others that could answer the
    /// query, as the winner ran it (RM-349): its first `results` results
    /// past its skip read now, as the command reads them, and held for the
    /// batches that follow, which return them before they read on from
    /// where the trial stopped. A winner that `ended` in its trial read
    /// everything it returns. What the trial examined is counted by the
    /// first batch.
    pub(crate) fn buffer_trial(
        &mut self,
        storage: &dyn Storage,
        results: usize,
        ended: bool,
    ) -> Result<(), StorageError> {
        self.keys = 0;
        self.docs = 0;
        // A winner that ran out read to its end, or to its LIMIT, which a
        // trial finds at its end too: `None` reads on to the end.
        let skip = self.skip.max(0) as usize;
        let want = match (ended, self.limit) {
            (true, None) => None,
            (true, Some(limit)) => Some(limit.max(0) as usize + skip),
            (false, _) => Some(results + skip),
        };
        if want != Some(0) {
            let found = self.read(storage, want.unwrap_or(0), None)?;
            // A LIMIT the trial filled is at its end from then on.
            let past = found.len().saturating_sub(skip) as i64;
            self.limit_reached |= self.limit.is_some_and(|limit| past >= limit);
            self.stashed.extend(found);
        }
        self.done |= ended;
        self.ran_out = ended && self.done;
        self.carried = (self.keys, self.docs);
        Ok(())
    }

    /// The command ends with no batch read (`batchSize: 0`): the plan
    /// yields, and what the trial examined is the command's to count.
    pub(crate) fn save_trial(&mut self) -> (i64, i64) {
        self.ran_out = false;
        let (mut keys, docs) = std::mem::take(&mut self.carried);
        if let Some(at) = self.root.yield_now() {
            keys = at;
        }
        (keys, docs)
    }

    fn batch(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        max_bytes: usize,
        reaching: bool,
    ) -> Result<Batch, StorageError> {
        (self.keys, self.docs) = std::mem::take(&mut self.carried);
        let result = self.fill(storage, want, max_bytes, reaching);
        // The batch is over: every stage yields, keeping only its position;
        // what was read ahead is read again by the next batch, and counted
        // there.
        if let Some(keys) = self.root.yield_now() {
            self.keys = keys;
        }
        let out = result?;
        let exhausted = self.done && self.stashed.is_empty();
        let mut batch = Batch {
            docs: Vec::with_capacity(out.len()),
            rids: Vec::with_capacity(out.len()),
            keys: Vec::with_capacity(out.len()),
            exhausted,
            keys_examined: self.keys,
            docs_examined: self.docs,
        };
        for found in out {
            let key = match (&found.entry, self.want_keys) {
                (Some(entry), true) => Some(key_document(entry, &found.doc)?),
                _ => None,
            };
            batch.docs.push(found.doc);
            batch.rids.push(found.rid);
            batch.keys.push(key);
        }
        Ok(batch)
    }

    fn fill(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        max_bytes: usize,
        reaching: bool,
    ) -> Result<Vec<Found>, StorageError> {
        let mut out: Vec<Found> = Vec::new();
        let mut bytes = 0usize;
        let left = |out: &Vec<Found>, limit: Option<i64>| -> usize {
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
            let mut fresh: VecDeque<Found> = std::mem::take(&mut self.stashed);
            if fresh.is_empty() {
                if self.done {
                    // The work that finds the trial's winner at its end
                    // again: the topmost stage's own, which asks no input.
                    if self.first && std::mem::take(&mut self.ran_out) {
                        let t = self.root.tally_mut();
                        t.own += 1;
                        t.eof_again += 1;
                    }
                    break;
                }
                // With no count to fill, a chunk at a time until the bytes
                // are spent; skipped results are read too.
                let ask = match left(&out, self.limit) {
                    0 => CHUNK,
                    n => n + self.skip.max(0) as usize,
                };
                // A buffer stops reading at the document that fills it.
                let budget = (reaching && self.skip <= 0).then(|| max_bytes.saturating_sub(bytes));
                fresh = self.read(storage, ask, budget)?.into();
                if fresh.is_empty() {
                    self.done = true;
                    break;
                }
            }
            while let Some(found) = fresh.pop_front() {
                if self.skip > 0 {
                    self.skip -= 1;
                    continue;
                }
                if !reaching && !out.is_empty() && bytes + found.doc.len() > max_bytes {
                    fresh.push_front(found);
                    self.stashed = fresh;
                    break 'fill;
                }
                bytes += found.doc.len();
                out.push(found);
                if let Some(l) = &mut self.limit {
                    *l -= 1;
                    if *l == 0 {
                        self.done = true;
                        self.stashed.clear();
                        self.limit_reached = true;
                        // `find`'s executor works the LIMIT once more, which
                        // finds it at its end; a `getMore`'s does not.
                        self.limit_eof_work = self.first;
                        break 'fill;
                    }
                }
                if (want > 0 && out.len() >= want) || (reaching && bytes >= max_bytes) {
                    self.stashed = fresh;
                    break 'fill;
                }
            }
        }
        Ok(out)
    }
}

impl Stream {
    /// The run so far as mongod's profile entry gives it (`execStats`),
    /// with the yields the last batch's entry reports (RM-359). `plan` is
    /// the one the stream was made from.
    pub(crate) fn exec_stats(&self, plan: &Node) -> Option<Document> {
        let run = self.report_with(plan, self.shown)?;
        Some(exec_stats_doc(plan, &run.stats))
    }

    /// What each stage of `plan` did, yielding `y` times.
    pub(crate) fn report_with(&self, plan: &Node, y: Yields) -> Option<Report> {
        self.top(plan, y, false, false)
    }

    /// A `count`'s run: its COUNT stage over the stream's, which counted
    /// `counted` after the results it skipped. The COUNT works once more when
    /// it reaches its limit, finding itself at its end (measured 8.2.11).
    pub(crate) fn count_stats(&self, plan: &Node, counted: i64) -> Option<Document> {
        let Node::Count(input) = plan else {
            return None;
        };
        let skipped = self.skip_given - self.skip.max(0);
        let c = self.top(input, Yields::default(), false, false)?;
        let works = c.works + i64::from(self.limit_reached);
        let mut extra = Document::new();
        extra.insert("nCounted", num(counted));
        extra.insert("nSkipped", num(skipped));
        let stats = Stats {
            n_returned: 0,
            keys: 0,
            docs: 0,
            extra,
            children: vec![c.stats],
            work: Some(work_counts(
                works,
                0,
                true,
                true,
                &Tally::default(),
                Yields::default(),
                c.millis,
            )),
        };
        Some(exec_stats_doc(plan, &stats))
    }

    /// The stages the stream runs for those above the plan's first FETCH or
    /// scan: a projection passes each result on, SKIP and LIMIT are the
    /// stream's own `skip` and `limit` (as `of_plan_with` takes them).
    fn top(&self, node: &Node, y: Yields, limited: bool, skipped: bool) -> Option<Report> {
        match node {
            Node::Projection { input, .. } | Node::ReturnKey(input) | Node::Subplan(input) => {
                let c = self.top(input, y, limited, skipped)?;
                Some(pass(c, y))
            }
            Node::Limit(_, input) if !limited && !skipped => {
                let c = self.top(input, y, true, skipped)?;
                let works = c.works + i64::from(self.limit_eof_work);
                let eof = self.limit_reached || c.eof;
                let eof_work = self.limit_eof_work || c.eof_work;
                Some(Report {
                    stats: Stats {
                        n_returned: c.advanced,
                        work: Some(work_counts(
                            works,
                            c.advanced,
                            eof,
                            eof_work,
                            &Tally::default(),
                            y,
                            c.millis,
                        )),
                        children: vec![c.stats],
                        ..Default::default()
                    },
                    works,
                    advanced: c.advanced,
                    eof,
                    eof_work,
                    millis: c.millis,
                })
            }
            Node::Skip(_, input) if !skipped => {
                let c = self.top(input, y, limited, true)?;
                // Each result skipped is a work that needs more time.
                let advanced = c.advanced - (self.skip_given - self.skip.max(0));
                Some(Report {
                    stats: Stats {
                        n_returned: advanced,
                        work: Some(work_counts(
                            c.works,
                            advanced,
                            c.eof,
                            c.eof_work,
                            &Tally::default(),
                            y,
                            c.millis,
                        )),
                        children: vec![c.stats],
                        ..Default::default()
                    },
                    works: c.works,
                    advanced,
                    eof: c.eof,
                    eof_work: c.eof_work,
                    millis: c.millis,
                })
            }
            _ => report(node, &self.root, y),
        }
    }
}

/// What a stage of a run did, as its parent counts from it: its statistics
/// and its `CommonStats` (RM-359).
#[derive(Clone, Debug)]
pub(crate) struct Report {
    pub stats: Stats,
    works: i64,
    advanced: i64,
    eof: bool,
    eof_work: bool,
    millis: i64,
}

/// mongod's `CommonStats` for a stage: every work advanced, needed more
/// time, or returned EOF (no work here waits to yield).
fn work_counts(
    works: i64,
    advanced: i64,
    eof: bool,
    eof_work: bool,
    t: &Tally,
    y: Yields,
    millis: i64,
) -> WorkCounts {
    WorkCounts {
        works,
        advanced,
        need_time: works - advanced - i64::from(eof_work) - t.eof_again,
        eof,
        seeks: t.seeks,
        dups_tested: t.dups_tested,
        dups_dropped: t.dups_dropped,
        saved: y.saved,
        restored: y.restored,
        millis,
    }
}

/// A stage that passes each result of its input on: a projection's.
fn pass(c: Report, y: Yields) -> Report {
    Report {
        stats: Stats {
            n_returned: c.advanced,
            work: Some(work_counts(
                c.works,
                c.advanced,
                c.eof,
                c.eof_work,
                &Tally::default(),
                y,
                c.millis,
            )),
            children: vec![c.stats],
            ..Default::default()
        },
        ..c
    }
}

/// A scan: every work its own.
fn leaf(t: &Tally, y: Yields, extra: Document) -> Report {
    Report {
        stats: Stats {
            n_returned: t.advanced,
            keys: t.keys,
            docs: t.docs,
            extra,
            children: Vec::new(),
            work: Some(work_counts(
                t.own, t.advanced, t.eof_work, t.eof_work, t, y, t.millis,
            )),
        },
        works: t.own,
        advanced: t.advanced,
        eof: t.eof_work,
        eof_work: t.eof_work,
        millis: t.millis,
    }
}

/// A stage that hands its works down: its inputs' works and its own.
fn above(stage: &Stage, t: &Tally, children: Vec<Report>, extra: Document, y: Yields) -> Report {
    let works = children.iter().map(|c| c.works).sum::<i64>() + t.own;
    let eof = stage.is_eof();
    Report {
        stats: Stats {
            n_returned: t.advanced,
            keys: 0,
            docs: t.docs,
            extra,
            children: children.into_iter().map(|c| c.stats).collect(),
            work: Some(work_counts(
                works, t.advanced, eof, t.eof_work, t, y, t.millis,
            )),
        },
        works,
        advanced: t.advanced,
        eof,
        eof_work: t.eof_work,
        millis: t.millis,
    }
}

fn dups(t: &Tally) -> Document {
    doc! { "dupsTested": num(t.dups_tested), "dupsDropped": num(t.dups_dropped) }
}

/// What `stage`, running `node` of the plan, did.
fn report(node: &Node, stage: &Stage, y: Yields) -> Option<Report> {
    match (node, stage) {
        (Node::Projection { input, .. } | Node::ReturnKey(input) | Node::Subplan(input), _) => {
            Some(pass(report(input, stage, y)?, y))
        }
        // The FETCH that reads a fetching DISTINCT_SCAN's documents is the
        // scan's own (`isFetching`).
        (
            Node::DistinctScan(_, _, fetching),
            Stage::Fetch {
                input, t: fetch, ..
            },
        ) => {
            let Stage::Ix(scan) = input.as_ref() else {
                return None;
            };
            let mut extra = Document::new();
            super::exec::fixed_extra(node, &mut extra);
            if *fetching {
                extra.insert("docsExamined", num(fetch.docs));
            }
            let mut r = leaf(&scan.t, y, extra);
            r.stats.docs = fetch.docs;
            Some(r)
        }
        // This store reads the document a covered plan answers from: no
        // stage of mongod's.
        (
            _,
            Stage::Fetch {
                counted: false,
                input,
                ..
            },
        ) if !matches!(node, Node::Fetch { .. }) => report(node, input, y),
        (Node::Express { .. } | Node::IdHack(_), Stage::Point { t, .. }) => Some(Report {
            stats: Stats {
                n_returned: t.advanced,
                keys: t.keys,
                docs: t.docs,
                ..Default::default()
            },
            works: 1,
            advanced: t.advanced,
            eof: true,
            eof_work: true,
            millis: t.millis,
        }),
        (Node::CollScan { .. }, Stage::Coll(scan)) => Some(leaf(&scan.t, y, Document::new())),
        (Node::IxScan(_) | Node::CountScan(_), Stage::Ix(scan)) => {
            Some(leaf(&scan.t, y, Document::new()))
        }
        (Node::Fetch { input: n, .. }, Stage::Fetch { input, t, .. })
        | (Node::Skip(_, n), Stage::Skip { input, t, .. }) => {
            let c = report(n, input, y)?;
            Some(above(stage, t, vec![c], Document::new(), y))
        }
        (Node::Or(nodes), Stage::Or { inputs, t, .. }) => {
            let children = children_of(nodes, inputs, y)?;
            Some(above(stage, t, children, dups(t), y))
        }
        (Node::SortMerge { inputs: nodes, .. }, Stage::Merge(merge)) => {
            let children = children_of(nodes, &merge.inputs, y)?;
            Some(above(stage, &merge.t, children, dups(&merge.t), y))
        }
        (Node::Sort { input: n, .. }, Stage::SortKeys { input, t, .. }) => {
            let c = report(n, input, y)?;
            let mut extra = Document::new();
            super::exec::fixed_extra(node, &mut extra);
            Some(above(stage, t, vec![c], extra, y))
        }
        (
            Node::Sort { input: n, .. },
            Stage::SortDocs {
                input,
                t,
                report: counted,
                ..
            },
        ) => {
            let c = report(n, input, y)?;
            let mut extra = Document::new();
            if let Some(r) = counted {
                super::exec::sort_extra(&mut extra, r);
            }
            super::exec::fixed_extra(node, &mut extra);
            Some(above(stage, t, vec![c], extra, y))
        }
        _ => None,
    }
}

fn children_of(nodes: &[Node], stages: &[Stage], y: Yields) -> Option<Vec<Report>> {
    if nodes.len() != stages.len() {
        return None;
    }
    nodes
        .iter()
        .zip(stages)
        .map(|(n, s)| report(n, s, y))
        .collect()
}

/// A run's stages as a profile entry's `execStats`: a streamed plan is
/// never one the plan cache chose (only a contested plan is cached), and a
/// projection's `transformBy` is `{}` there, where `explain` gives the
/// projection (measured 8.2.11).
pub(crate) fn exec_stats_doc(plan: &Node, stats: &Stats) -> Document {
    fn blank_projections(stage: &mut Document) {
        if stage.contains_key("transformBy") {
            stage.insert("transformBy", Document::new());
        }
        if let Ok(input) = stage.get_document_mut("inputStage") {
            blank_projections(input);
        }
        if let Ok(inputs) = stage.get_array_mut("inputStages") {
            for input in inputs.iter_mut() {
                if let Bson::Document(d) = input {
                    blank_projections(d);
                }
            }
        }
    }
    let mut out = doc! { "isCached": false };
    out.extend(plan.to_doc(Some(stats)));
    blank_projections(&mut out);
    out
}

/// A `count` answered from the collection's record count: mongod's
/// RECORD_STORE_FAST_COUNT, one work (measured 8.2.11).
pub(crate) fn fast_count_stats(counted: i64, skipped: i64) -> Document {
    let mut extra = Document::new();
    extra.insert("nCounted", num(counted));
    extra.insert("nSkipped", num(skipped));
    let stats = Stats {
        extra,
        work: Some(work_counts(
            1,
            0,
            true,
            true,
            &Tally::default(),
            Yields::default(),
            0,
        )),
        ..Default::default()
    };
    exec_stats_doc(&Node::FastCount, &stats)
}

/// The read of a write's documents, kept for its profile entry until the
/// write is done (RM-359): the write's plan, what its read did, and when
/// the write started, on the stages' clock.
#[derive(Clone, Debug)]
pub(crate) struct WriteRun {
    pub plan: Node,
    pub read: Option<Report>,
    /// An `_id` point write's keys and documents examined.
    pub examined: i64,
    pub started_ms: i64,
}

/// What a write did, as its stage reports it.
#[derive(Clone, Copy, Default)]
pub(crate) struct Wrote {
    pub matched: i64,
    pub modified: i64,
    pub upserted: i64,
    pub deleted: i64,
    /// A `findAndModify` handed a document back.
    pub returned: bool,
}

/// mongod's `batchedDeletesTargetBatchDocs`: a BATCHED_DELETE commits, and
/// its input yields, once per this many documents (its 5 ms time target is
/// not kept here).
const DELETE_BATCH_DOCS: i64 = 10;

impl WriteRun {
    /// The write's `execStats`: its UPDATE or DELETE over the read, whose
    /// stages yield around each document written -- once per batch of a
    /// BATCHED_DELETE -- and whose works are the read's; the write returns
    /// EOF in its last work unless it hands a document back (measured
    /// 8.2.11).
    pub(crate) fn exec_stats(&self, wrote: &Wrote) -> Option<Document> {
        let millis = coarse_ms() - self.started_ms;
        let (writes, extra, input) = match &self.plan {
            Node::ExpressWrite { delete, .. } => {
                let mut extra = Document::new();
                extra.insert(
                    "nWouldModify",
                    num(if *delete { 0 } else { wrote.modified }),
                );
                extra.insert("nWouldUpsert", num(wrote.upserted));
                extra.insert("nWouldDelete", num(wrote.deleted));
                let stats = Stats {
                    n_returned: i64::from(wrote.returned),
                    keys: self.examined,
                    docs: self.examined,
                    extra,
                    ..Default::default()
                };
                return Some(exec_stats_doc(&self.plan, &stats));
            }
            Node::Update(input) => {
                let mut extra = Document::new();
                extra.insert("nMatched", num(wrote.matched));
                extra.insert("nWouldModify", num(wrote.modified));
                extra.insert("nWouldUpsert", num(wrote.upserted));
                (wrote.matched, extra, input)
            }
            Node::Delete { batched, input } => {
                let mut extra = Document::new();
                extra.insert("nWouldDelete", num(wrote.deleted));
                let yields = if *batched {
                    (wrote.deleted + DELETE_BATCH_DOCS - 1) / DELETE_BATCH_DOCS
                } else {
                    wrote.deleted
                };
                (yields, extra, input)
            }
            _ => return None,
        };
        // An `_id` read below the write (IDHACK) keeps no work counts.
        if matches!(input.as_ref(), Node::IdHack(_)) {
            return None;
        }
        let read = self.read.as_ref()?;
        let mut child = read.stats.clone();
        yielded(
            &mut child,
            Yields {
                saved: writes,
                restored: writes,
            },
        );
        let advanced = i64::from(wrote.returned);
        let stats = Stats {
            n_returned: advanced,
            extra,
            children: vec![child],
            work: Some(work_counts(
                read.works,
                advanced,
                true,
                advanced == 0,
                &Tally::default(),
                Yields::default(),
                millis.max(read.millis),
            )),
            ..Default::default()
        };
        Some(exec_stats_doc(&self.plan, &stats))
    }
}

/// Every stage of `stats` went through `y`.
fn yielded(stats: &mut Stats, y: Yields) {
    if let Some(w) = stats.work.as_mut() {
        w.saved = y.saved;
        w.restored = y.restored;
    }
    for c in &mut stats.children {
        yielded(c, y);
    }
}

/// The stages' clock now: a write's start, for its stage's time.
pub(crate) fn clock_ms() -> i64 {
    coarse_ms()
}

/// Drop the topmost filter of a plan whose command filter is empty.
fn clear_top_filter(stage: &mut Stage) {
    match stage {
        Stage::Fetch { filter, .. } => *filter = None,
        Stage::Coll(scan) => scan.filter = None,
        _ => {}
    }
}

/// The key a document was found at, as mongod's `returnKey` and `{$meta:
/// "indexKey"}` give it: each field of the index and its value in that
/// entry, the value as the document holds it (an empty array's key is
/// `undefined`). A string under the index's collation is its collation key,
/// as mongod gives it.
fn key_document(entry: &EntryKey, blob: &[u8]) -> Result<Document, StorageError> {
    let doc = secantus_core::bsondoc::from_bytes(blob).map_err(undecodable)?;
    let meta = &entry.meta;
    let mut out = Document::new();
    for (i, field) in meta.fields.iter().enumerate() {
        let part = &entry.parts[i];
        let values = if meta.id {
            doc.get("_id").cloned().into_iter().collect()
        } else {
            secantus_core::index_keys::field_keys(&doc, field, meta.sparse).mongod
        };
        let found = values.into_iter().find(|v| {
            let encoded = if meta.id {
                sortkey::encode_id_key(v)
            } else {
                sortkey::encode_value(v, meta.collation.as_ref())
            };
            encoded.is_ok_and(|k| k == *part)
        });
        let value = match (found, &meta.collation) {
            (Some(Bson::String(s)), Some(c)) => Bson::String(
                String::from_utf8_lossy(&secantus_core::collation::sort_key(&s, c)).into_owned(),
            ),
            (Some(v), _) => v,
            (None, _) => Bson::Null,
        };
        out.insert(field.clone(), value);
    }
    Ok(out)
}
