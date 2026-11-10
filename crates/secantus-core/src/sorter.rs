//! A blocking sort held to its memory budget, spilling as mongod 8.2's
//! external sorter does (RM-315).
//!
//! mongod's `NoLimitSorter` (`sorter_template_defs.h`, 8.2.1) adds each
//! key and document to memory, counting `key.memUsageForSorter() +
//! value.memUsageForSorter()`, and once that passes its budget it sorts what
//! it holds and writes it to a spill file as one sorted run -- or, with
//! `allowDiskUse: false`, refuses with 292. Its budget is the knob's less
//! the memory it keeps for reading runs back: a tenth of the knob, at most
//! 1 MiB, in whole file iterators (136 bytes each; measured 8.2.11 by
//! bisecting the knob over several document sizes, 2026-10-26). At the end
//! it spills what is left and merges the runs, `max(knob / 64 KiB, 2)` at a
//! time, into that many runs, each merge pass writing every record again;
//! the last runs are merged as they are read. Equal keys keep the order of
//! the runs, which keep the order the documents came in, so a spilled sort
//! answers in the same order as one held in memory.
//!
//! What a document counts depends on the stage that sorts it, all measured
//! against 8.2.11 by the same bisection:
//!
//! | sort | document counts |
//! | --- | --- |
//! | the query layer's (`find`, a pipeline's leading `$sort`) | its BSON + 16 |
//! | `$sort` over documents read from the collection | its BSON + 96 |
//! | `$sort` after `$addFields` / `$set` / `$unset` | its BSON + 224 |
//! | `$sort` after `$unwind` | its BSON + 240 |
//! | `$bucketAuto` | its BSON + 333 |
//!
//! A key counts as a classic `Value`: one field's value, or an array of
//! them for a compound sort. A pipeline's documents carry only the fields
//! the stages after them read, as mongod's dependency analysis leaves them.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use bson::{doc, Bson, Document};

use crate::fallback::Fallback;
use crate::{memlimit, spill_file};

type R<T> = Result<T, Fallback>;

/// `sizeof(FileIterator<Key, Value>)` on mongod 8.2.11 (arm64), measured.
const FILE_ITERATOR: usize = 136;
/// `kSortedFileBufferSize`.
const SORTED_FILE_BUFFER: usize = spill_file::CHUNK;

/// Which of mongod's sorts this is, for what a document counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortSource {
    /// The query layer's `SORT` stage.
    QueryLayer,
    /// `$sort` over documents as they were read.
    Stored,
    /// `$sort` over documents an `$addFields` / `$set` / `$unset` changed.
    Modified,
    /// `$sort` over documents an `$unwind` made.
    Unwound,
    /// `$bucketAuto`'s sort of its group keys.
    BucketAuto,
    /// A sort mongod runs in SBE (one a `$group` or `$lookup` is pushed
    /// down behind): rows of SBE values, the key's and the fields kept.
    Sbe,
}

impl SortSource {
    fn overhead(self) -> usize {
        match self {
            SortSource::QueryLayer => 16,
            SortSource::Stored => 96,
            SortSource::Modified => 224,
            SortSource::Unwound => 240,
            SortSource::BucketAuto => 333,
            SortSource::Sbe => 32,
        }
    }

    /// `sizeof(FileIterator<Key, Value>)` for this sorter's key and value
    /// types (measured 8.2.11 on arm64).
    fn file_iterator(self) -> usize {
        match self {
            SortSource::Sbe => 152,
            _ => FILE_ITERATOR,
        }
    }
}

/// How a sort counts its documents.
#[derive(Debug, Clone, Copy)]
pub struct Sizing<'a> {
    pub source: SortSource,
    /// The top-level fields mongod keeps of each document; `None`: all.
    pub fields: Option<&'a BTreeSet<String>>,
    /// The knob that budgets this sort.
    pub budget: usize,
    /// Fields a stage before the sort added, which mongod counts as the
    /// document read rather than as written (`$addFields`, `$set`).
    pub added: Option<&'a BTreeSet<String>>,
}

/// The memory a sorter keeps aside for reading its runs back
/// (`MergeableSorter::setMaxMemoryUsageBytes`).
fn reserved(budget: usize, iterator: usize) -> usize {
    let requested = (budget as f64 * 0.1) as usize;
    let bytes = if requested < 1 << 20 {
        requested.max(iterator)
    } else {
        1 << 20
    };
    iterator * (bytes / iterator)
}

/// What one key counts: a classic `Value`, an array of them for a compound
/// sort.
pub fn key_size(keys: &[Bson]) -> usize {
    match keys {
        [one] => memlimit::classic_size(one),
        many => 16 + 40 + many.iter().map(memlimit::classic_size).sum::<usize>(),
    }
}

fn doc_size(doc: &Document, sizing: &Sizing) -> usize {
    let kept = |k: &str| {
        sizing.fields.is_none_or(|f| f.contains(k)) && sizing.added.is_none_or(|a| !a.contains(k))
    };
    let body = if sizing.source == SortSource::Sbe {
        doc.iter()
            .filter(|(k, _)| kept(k))
            .map(|(_, v)| memlimit::sbe_size(v))
            .sum::<usize>()
    } else if sizing.fields.is_none() && sizing.added.is_none() {
        memlimit::bson_len(doc)
    } else {
        let mut kept_doc = Document::new();
        for (k, v) in doc.iter().filter(|(k, _)| kept(k)) {
            kept_doc.insert(k.clone(), v.clone());
        }
        memlimit::bson_len(&kept_doc)
    };
    body + sizing.source.overhead()
}

fn sized_key(keys: &[Bson], source: SortSource) -> usize {
    if source == SortSource::Sbe {
        keys.iter().map(memlimit::sbe_size).sum()
    } else {
        key_size(keys)
    }
}

fn refused(budget: usize) -> Fallback {
    Fallback::mongo(
        292,
        format!(
            "Sort exceeded memory limit of {budget} bytes, but did not opt in to external \
             sorting."
        ),
    )
}

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// A key and its document as the sorter holds them, with the document's
/// position in the input (`$bucketAuto` writes it into each record).
type Held = (Vec<Bson>, Document, i64);

fn io_error(path: &std::path::Path, e: spill_file::Error) -> Fallback {
    Fallback::mongo(
        1,
        format!(
            "the sort could not {} its spill file {}: {}",
            e.what,
            path.display(),
            e.detail
        ),
    )
}

/// One sorted run on disk. Its file goes when it is dropped.
struct Run {
    path: PathBuf,
    records: usize,
    /// How many values each key holds: one is written as itself, more as
    /// an array of them.
    arity: usize,
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "secantus: could not remove sort spill file {}: {e}",
                    self.path.display()
                );
            }
        }
    }
}

/// The document as the sort keeps it: only the fields mongod's dependency
/// analysis leaves it, and those a stage before it added.
fn kept(doc: &Document, sizing: &Sizing) -> Document {
    if sizing.fields.is_none() {
        return doc.clone();
    }
    doc.iter()
        .filter(|(k, _)| {
            sizing.fields.is_none_or(|f| f.contains(k.as_str()))
                || sizing.added.is_some_and(|a| a.contains(k.as_str()))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// One record as this sorter's mongod counterpart serializes it: the key
/// as a `Value` (an array of them for a compound sort), then the document
/// as that sorter keeps it -- the query layer's (and SBE's) as BSON, a
/// pipeline `$sort`'s as a `Document`, `$bucketAuto`'s inside the
/// `{output, generatedSortKey}` document it sorts.
fn write_record(
    (keys, d, pos): &Held,
    sizing: &Sizing,
    out: &mut Vec<u8>,
) -> Result<(), spill_file::Error> {
    match keys.as_slice() {
        [one] => spill_file::write_value(one, out)?,
        many => spill_file::write_value(&Bson::Array(many.to_vec()), out)?,
    }
    match sizing.source {
        SortSource::QueryLayer => spill_file::write_bson(d, out),
        SortSource::Sbe => spill_file::write_bson(&kept(d, sizing), out),
        SortSource::BucketAuto => spill_file::write_document(
            &doc! { "output": kept(d, sizing), "generatedSortKey": *pos },
            out,
        ),
        SortSource::Stored | SortSource::Modified | SortSource::Unwound => {
            spill_file::write_document(&kept(d, sizing), out)
        }
    }
}

fn read_record(
    at: &mut spill_file::Cursor,
    arity: usize,
    source: SortSource,
) -> Result<Held, spill_file::Error> {
    let key = spill_file::read_value(at)?;
    let keys = match (arity, key) {
        (1, k) => vec![k],
        (_, Bson::Array(k)) if k.len() == arity => k,
        _ => {
            return Err(spill_file::Error {
                what: "read",
                detail: "a record whose key is not the sort's".into(),
            })
        }
    };
    let (d, pos) = match source {
        SortSource::QueryLayer | SortSource::Sbe => (spill_file::read_bson(at)?, 0),
        SortSource::BucketAuto => {
            let mut w = spill_file::read_document(at)?;
            match (w.remove("output"), w.remove("generatedSortKey")) {
                (Some(Bson::Document(d)), Some(Bson::Int64(pos))) => (d, pos),
                _ => {
                    return Err(spill_file::Error {
                        what: "read",
                        detail: "a $bucketAuto record without its document".into(),
                    })
                }
            }
        }
        SortSource::Stored | SortSource::Modified | SortSource::Unwound => {
            (spill_file::read_document(at)?, 0)
        }
    };
    Ok((keys, d, pos))
}

/// Write sorted `items` as a run, counted as one spill.
fn write_run(items: impl Iterator<Item = Held>, sizing: &Sizing) -> R<Run> {
    let bucket_auto = sizing.source == SortSource::BucketAuto;
    let dir = memlimit::spill_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        io_error(
            &dir,
            spill_file::Error {
                what: "create the directory for",
                detail: e.to_string(),
            },
        )
    })?;
    let path = dir.join(format!(
        "secantus-sort-{}-{}.spill",
        std::process::id(),
        NEXT_FILE.fetch_add(1, AtomicOrdering::Relaxed)
    ));
    let mut out = spill_file::Writer::create(&path).map_err(|e| io_error(&path, e))?;
    // Registered before anything is written, so a failure below removes it.
    let mut run = Run {
        path: path.clone(),
        records: 0,
        arity: 1,
    };
    for item in items {
        run.arity = item.0.len();
        write_record(&item, sizing, out.buffer()).map_err(|e| io_error(&path, e))?;
        out.added().map_err(|e| io_error(&path, e))?;
        run.records += 1;
    }
    let written = out.done().map_err(|e| io_error(&path, e))?;
    if bucket_auto {
        memlimit::record_bucket_auto_spill(
            run.records as i64,
            written.bytes,
            written.stored,
            written.chunks,
        );
    } else {
        memlimit::record_sort_spill(
            run.records as i64,
            written.bytes,
            written.stored,
            written.chunks,
        );
    }
    Ok(run)
}

/// A run being read back.
struct Reader {
    input: spill_file::Reader,
    path: PathBuf,
    left: usize,
    arity: usize,
    source: SortSource,
}

impl Reader {
    fn open(run: &Run, source: SortSource) -> R<Reader> {
        Ok(Reader {
            input: spill_file::Reader::open(&run.path).map_err(|e| io_error(&run.path, e))?,
            path: run.path.clone(),
            left: run.records,
            arity: run.arity,
            source,
        })
    }

    fn next(&mut self) -> R<Option<Held>> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        let (arity, source) = (self.arity, self.source);
        self.input
            .record(|at| read_record(at, arity, source))
            .map(Some)
            .map_err(|e| io_error(&self.path, e))
    }
}

/// Merge `runs` (in order) into one stream, equal keys in run order.
fn merge_runs(
    runs: &[Run],
    source: SortSource,
    cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering,
    mut emit: impl FnMut(Held) -> R<()>,
) -> R<()> {
    let mut readers = runs
        .iter()
        .map(|r| Reader::open(r, source))
        .collect::<R<Vec<_>>>()?;
    let mut heads = Vec::with_capacity(readers.len());
    for r in readers.iter_mut() {
        heads.push(r.next()?);
    }
    loop {
        crate::deadline::check()?;
        let mut least: Option<usize> = None;
        for (i, h) in heads.iter().enumerate() {
            let Some((k, _, _)) = h else { continue };
            match least {
                None => least = Some(i),
                Some(l) => {
                    let (lk, _, _) = heads[l].as_ref().expect("a head");
                    if cmp(k, lk) == Ordering::Less {
                        least = Some(i);
                    }
                }
            }
        }
        let Some(i) = least else { return Ok(()) };
        let item = heads[i].take().expect("a head");
        heads[i] = readers[i].next()?;
        emit(item)?;
    }
}

/// `MergeableSorter::_mergeSpills`: merge the runs `parallel` at a time
/// until no more than `target` are left, each merge written as a new run.
fn merge_spills(
    runs: Vec<Run>,
    target: usize,
    parallel: usize,
    cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering,
    sizing: &Sizing,
) -> R<Vec<Run>> {
    let target = target.max(1);
    let mut runs = runs;
    while runs.len() > target {
        let mut next = Vec::new();
        for chunk in runs.chunks(parallel) {
            let mut merged: Vec<Held> = Vec::new();
            // Read the chunk through a writer of its own: mongod writes each
            // merged chunk as a run, a single run included.
            merge_runs(chunk, sizing.source, cmp, |item| {
                merged.push(item);
                Ok(())
            })?;
            next.push(write_run(merged.into_iter(), sizing)?);
        }
        runs = next;
    }
    Ok(runs)
}

/// Sort `items` by `cmp` (stable), held to `sizing.budget`: spilled to disk
/// when the command allows it, refused (292) when it does not. `limit`: a
/// `$limit` mongod folds into the sort, which then keeps only that many.
pub fn blocking_sort(
    items: Vec<(Vec<Bson>, Document)>,
    cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering,
    sizing: &Sizing,
    limit: Option<usize>,
) -> R<Vec<(Vec<Bson>, Document)>> {
    let budget = sizing.budget;
    let reserve = reserved(budget, sizing.source.file_iterator());
    let effective = budget.saturating_sub(reserve);
    let size = |(k, d, _): &Held| sized_key(k, sizing.source) + doc_size(d, sizing);
    let items = items
        .into_iter()
        .enumerate()
        .map(|(i, (k, d))| (k, d, i as i64));
    let unheld = |held: Vec<Held>| held.into_iter().map(|(k, d, _)| (k, d)).collect();
    let n = items.len() as i64;
    let bucket_auto = sizing.source == SortSource::BucketAuto;
    // mongod 8.2.11's slot-based sort adds its bytes to the counters only
    // as it spills while taking its input: what it held at each such spill.
    // Its last run, written when the input ends, and a sort that never
    // spills, add none (RM-398).
    let sbe = sizing.source == SortSource::Sbe;
    let record = |bytes: usize| {
        if !bucket_auto {
            memlimit::record_sort(n, bytes as i64);
        }
    };

    if let Some(k) = limit {
        // mongod's top-k sorter keeps only the k first; a limit of one never
        // goes near its budget. Its sorter for a limit of one (sorter.cpp's
        // `LimitOneSorter`) counts the keys it saw but no bytes: mongod
        // 8.2.11 reports `totalDataSizeSorted: 0` for it (RM-374).
        let mut items: Vec<Held> = items.collect();
        items.sort_by(|a, b| cmp(&a.0, &b.0));
        items.truncate(k);
        let held: usize = items.iter().map(size).sum();
        record(if k == 1 || sbe { 0 } else { held });
        if k > 1 && held > effective && !memlimit::disk_allowed() {
            return Err(refused(budget));
        }
        return Ok(unheld(items));
    }

    let iterators = (reserve / sizing.source.file_iterator()).max(1);
    let parallel = (budget / SORTED_FILE_BUFFER).max(2);
    let mut held: Vec<Held> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut used = 0usize;
    let mut total = 0usize;
    let mut spilled = 0usize;
    let spill = |held: &mut Vec<Held>, runs: &mut Vec<Run>| -> R<()> {
        if held.is_empty() {
            return Ok(());
        }
        if !memlimit::disk_allowed() {
            return Err(refused(budget));
        }
        held.sort_by(|a, b| cmp(&a.0, &b.0));
        runs.push(write_run(std::mem::take(held).into_iter(), sizing)?);
        if runs.len() >= iterators {
            let all = std::mem::take(runs);
            *runs = merge_spills(all, iterators / 2, parallel, cmp, sizing)?;
        }
        Ok(())
    };
    for item in items {
        crate::deadline::check()?;
        let s = size(&item);
        used += s;
        total += s;
        held.push(item);
        if used > effective {
            spill(&mut held, &mut runs)?;
            spilled += used;
            used = 0;
        }
    }
    record(if sbe { spilled } else { total });
    if runs.is_empty() {
        held.sort_by(|a, b| cmp(&a.0, &b.0));
        return Ok(unheld(held));
    }
    spill(&mut held, &mut runs)?;
    let runs = merge_spills(runs, parallel, parallel, cmp, sizing)?;
    let mut out = Vec::with_capacity(n as usize);
    merge_runs(&runs, sizing.source, cmp, |(k, d, _)| {
        out.push((k, d));
        Ok(())
    })?;
    Ok(out)
}

/// What mongod 8.2.11's `SORT` of index keys (`type: "default"`, below the
/// FETCH) counts of each working-set member it sorts: its sort key, the
/// index key it carries, and 32 bytes beside them (65 for an int sort key
/// over a `{y: 1, x: 1}` key of two ints, measured over the key's types,
/// its width and the sort's arity, 2026-11-09).
const MEMBER: usize = 32;

/// What one working-set member counts in a sort of index keys: `sort_key`
/// its sort key's values, `index_key` the values of the index key it was
/// found at.
pub fn member_size(sort_key: &[Bson], index_key: &[Bson]) -> usize {
    key_size(sort_key) + memlimit::index_key_len(index_key) + MEMBER
}

/// Count a sort of index keys as mongod's sorter counts it, `sizes` each
/// member's [`member_size`] in sorted order, and hold it to `budget` as
/// [`blocking_sort`] holds a sort of documents: refused (292) past it
/// without disk use. `limit`: the sort's own, which keeps that many.
pub fn count_members(sizes: &[usize], budget: usize, limit: Option<usize>) -> R<()> {
    let effective = budget.saturating_sub(reserved(budget, FILE_ITERATOR));
    let n = sizes.len() as i64;
    if let Some(k) = limit {
        // The top-k sorter keeps the k first, and its sorter of one counts
        // no bytes (as `blocking_sort`'s, RM-374).
        let held: usize = sizes.iter().take(k).sum();
        memlimit::record_sort(n, if k == 1 { 0 } else { held as i64 });
        if k > 1 && held > effective && !memlimit::disk_allowed() {
            return Err(refused(budget));
        }
        return Ok(());
    }
    let total: usize = sizes.iter().sum();
    if total > effective && !memlimit::disk_allowed() {
        return Err(refused(budget));
    }
    memlimit::record_sort(n, total as i64);
    Ok(())
}

/// `docs` sorted by the `$sort` spec `spec` (stable), held to the budget
/// (see [`blocking_sort`]).
pub fn sort_documents(
    docs: Vec<Document>,
    spec: &Bson,
    sizing: &Sizing,
    limit: Option<usize>,
) -> R<Vec<Document>> {
    let sort = crate::aggregate::sort_fields(spec)?;
    let mut keyed: Vec<(Vec<Bson>, Document)> = Vec::with_capacity(docs.len());
    for d in docs {
        keyed.push((crate::aggregate::sort_key_values(&d, &sort)?, d));
    }
    let cmp = |ka: &[Bson], kb: &[Bson]| {
        for (i, (_, rev)) in sort.iter().enumerate() {
            let c = crate::order::cmp(&ka[i], &kb[i]);
            let c = if *rev { c.reverse() } else { c };
            if c != Ordering::Equal {
                return c;
            }
        }
        Ordering::Equal
    };
    let sorted = blocking_sort(keyed, &cmp, sizing, limit)?;
    Ok(sorted.into_iter().map(|(_, d)| d).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: i32, len: usize) -> Vec<(Vec<Bson>, Document)> {
        (0..n)
            .map(|i| {
                let d = doc! { "_id": i, "k": (i * 7) % 5, "s": "x".repeat(len) };
                (vec![Bson::Int32((i * 7) % 5)], d)
            })
            .collect()
    }

    fn cmp(a: &[Bson], b: &[Bson]) -> Ordering {
        crate::order::cmp(&a[0], &b[0])
    }

    fn ids(out: &[(Vec<Bson>, Document)]) -> Vec<i32> {
        out.iter().map(|(_, d)| d.get_i32("_id").unwrap()).collect()
    }

    #[test]
    fn the_budget_is_the_knob_less_its_file_iterators() {
        // Measured on mongod 8.2.11: 100 MiB keeps 1048560 aside, 461104
        // keeps 46104.
        assert_eq!(reserved(104_857_600, 136), 1_048_560);
        assert_eq!(reserved(461_104, 136), 46_104);
        assert_eq!(reserved(0, 136), 136);
    }

    #[test]
    fn a_spilled_sort_answers_as_one_held_in_memory() {
        let whole = {
            let sizing = Sizing {
                source: SortSource::QueryLayer,
                fields: None,
                added: None,
                budget: memlimit::MAX_BYTES,
            };
            blocking_sort(items(300, 1000), &cmp, &sizing, None).unwrap()
        };
        memlimit::reset_report();
        let sizing = Sizing {
            source: SortSource::QueryLayer,
            fields: None,
            added: None,
            budget: 50_000,
        };
        let spilled = blocking_sort(items(300, 1000), &cmp, &sizing, None).unwrap();
        assert_eq!(ids(&spilled), ids(&whole));
        let report = memlimit::report();
        assert!(report.used_disk && report.sort_spills > 1, "{report:?}");
        let _off = memlimit::allow_disk_use(false);
        let e = blocking_sort(items(300, 1000), &cmp, &sizing, None).unwrap_err();
        assert_eq!(
            e.as_mongo(),
            Some((
                292,
                "Sort exceeded memory limit of 50000 bytes, but did not opt in to external \
                 sorting."
            ))
        );
    }

    #[test]
    fn spills_and_merges_count_as_mongods_do() {
        // mongod 8.2.11, 300 documents of {_id, k, s: 1000 x's, n} sorted by
        // {s: 1, n: -1} under a 50000-byte budget: 29 spills, 1200 records
        // (15 runs of 21, then merge passes to 8, 4 and 2).
        let docs: Vec<(Vec<Bson>, Document)> = (0..300)
            .map(|i| {
                let d = doc! { "_id": i, "k": i % 7, "s": "x".repeat(1000), "n": i };
                (vec![Bson::String("x".repeat(1000)), Bson::Int32(i)], d)
            })
            .collect();
        let sizing = Sizing {
            source: SortSource::QueryLayer,
            fields: None,
            added: None,
            budget: 50_000,
        };
        memlimit::reset_report();
        let cmp2 = |a: &[Bson], b: &[Bson]| {
            crate::order::cmp(&a[0], &b[0]).then(crate::order::cmp(&b[1], &a[1]))
        };
        blocking_sort(docs, &cmp2, &sizing, None).unwrap();
        let report = memlimit::report();
        assert_eq!(
            (
                report.sort_spills,
                report.sort_spilled_records,
                report.sort_total_data_size
            ),
            (29, 1200, 649_200)
        );
        assert_eq!(report.sort_spilled_bytes, 2_461_200);
        // Its file, every run in mongod's 64 KiB snappy chunks (RM-325).
        assert_eq!(report.sort_spilled_data_storage_size, 130_545);
    }

    /// Run `items` through a query-layer sort under `budget`, already in
    /// the order a collection scan reads them: `(spills, spilledBytes,
    /// spilledDataStorageSize)`.
    fn spilled(items: Vec<(Vec<Bson>, Document)>, budget: usize) -> (i64, i64, i64) {
        let sizing = Sizing {
            source: SortSource::QueryLayer,
            fields: None,
            added: None,
            budget,
        };
        let (out, report) = memlimit::captured(|| blocking_sort(items, &cmp, &sizing, None));
        out.unwrap();
        (
            report.sort_spills,
            report.sort_spilled_bytes,
            report.sort_spilled_data_storage_size,
        )
    }

    #[test]
    fn a_sort_of_index_keys_counts_its_members_as_mongods_does() {
        // mongod 8.2.11, `find({y: {$gte: 0}}).sort({x: 1})` over `{y: 1,
        // x: 1}`: 65 a member (an int key, a two-int index key and 32),
        // 137 for the sort `{x: 1, y: -1}`, 61 for a missing `x` (RM-375).
        let ints = [Bson::Int32(1), Bson::Int32(7)];
        assert_eq!(member_size(&ints[1..], &ints), 65);
        assert_eq!(member_size(&[ints[1].clone(), ints[0].clone()], &ints), 137);
        assert_eq!(
            member_size(&[Bson::Null], &[Bson::Int32(1), Bson::Null]),
            61
        );
        let counted = |sizes: &[usize], budget: usize, limit: Option<usize>| {
            let _off = memlimit::allow_disk_use(false);
            let (ran, report) = memlimit::captured(|| count_members(sizes, budget, limit));
            ran.map(|()| report.sort_total_data_size)
                .map_err(|f| f.as_mongo().map(|(code, _)| code))
        };
        // Twelve members need the budget less one 136-byte file iterator;
        // a limit of three keeps three, a limit of one counts no bytes.
        let twelve = [65; 12];
        assert_eq!(counted(&twelve, 916, None), Ok(780));
        assert_eq!(counted(&twelve, 915, None), Err(Some(292)));
        assert_eq!(counted(&twelve, 331, Some(3)), Ok(195));
        assert_eq!(counted(&twelve, 330, Some(3)), Err(Some(292)));
        assert_eq!(counted(&twelve, 1, Some(1)), Ok(0));
    }

    #[test]
    fn an_sbe_sort_counts_only_the_bytes_it_spills_while_reading() {
        // mongod 8.2.11, `[{$sort: {n: 1}}, {$group: {_id: "$k", f: {$first:
        // "$s"}}}]` over 300 documents of {_id, k, s: 1000 x's, n}: a row of
        // 1064 (the key, `k` and `s`), six spills of 43 rows while reading
        // under 50000 bytes, 274512; its last run of 42 adds nothing, and
        // nor does the same sort held in memory, or one with a limit (RM-398).
        let fields: BTreeSet<String> = ["k", "s"].iter().map(|f| f.to_string()).collect();
        let docs = || {
            (0..300)
                .map(|i| {
                    let d = doc! { "_id": i, "k": i % 7, "s": "x".repeat(1000), "n": i };
                    (vec![Bson::Int32(i)], d)
                })
                .collect::<Vec<_>>()
        };
        let counted = |budget: usize, limit: Option<usize>| {
            let sizing = Sizing {
                source: SortSource::Sbe,
                fields: Some(&fields),
                added: None,
                budget,
            };
            let (out, report) = memlimit::captured(|| blocking_sort(docs(), &cmp, &sizing, limit));
            out.unwrap();
            (report.sort_spills > 0, report.sort_total_data_size)
        };
        assert_eq!(counted(50_000, None), (true, 274_512));
        assert_eq!(counted(memlimit::MAX_BYTES, None), (false, 0));
        assert_eq!(counted(memlimit::MAX_BYTES, Some(50)), (false, 0));
    }

    #[test]
    fn a_sort_of_one_counts_no_data_sorted() {
        // mongod 8.2.11, `find({z: 0}).sort({y: 1})` over six documents:
        // `totalDataSizeSorted` 0 for a limit of 1, 134 for 2 (RM-374).
        let sizing = Sizing {
            source: SortSource::QueryLayer,
            fields: None,
            added: None,
            budget: memlimit::MAX_BYTES,
        };
        let docs = || {
            (0..6)
                .map(|i| (vec![Bson::Int32(i % 3)], doc! { "_id": i, "y": i % 3 }))
                .collect::<Vec<_>>()
        };
        let sorted = |k: usize| {
            let (out, report) =
                memlimit::captured(|| blocking_sort(docs(), &cmp, &sizing, Some(k)));
            (ids(&out.unwrap()), report.sort_total_data_size)
        };
        let (one, bytes) = sorted(1);
        assert_eq!((one, bytes), (vec![0], 0));
        let (two, bytes) = sorted(2);
        assert_eq!(two, vec![0, 3]);
        assert!(bytes > 0);
    }

    #[test]
    fn a_spill_file_takes_what_mongods_takes() {
        // Measured against mongod 8.2.1 (arm64), `$sort: {n: 1}` under a
        // 200000-byte budget: the profile's `sortSpills`,
        // `sortSpilledBytes` and `sortSpilledDataStorageSize`.
        let keyed = |d: Document| (vec![d.get("n").unwrap().clone()], d);
        let xs = (0..300).map(|i| keyed(doc! { "_id": i, "s": "x".repeat(1000), "n": i }));
        assert_eq!(spilled(xs.collect(), 200_000), (2, 310_200, 17_921));
        let ints = (0..5000).map(|i| keyed(doc! { "_id": i, "a": i * 7, "b": i % 13, "n": i }));
        assert_eq!(spilled(ints.collect(), 200_000), (2, 200_000, 94_667));
        let only = (0..9000i32).map(|i| keyed(doc! { "_id": i, "n": i64::from(i) }));
        assert_eq!(spilled(only.collect(), 200_000), (3, 306_000, 135_744));
        let dbl = (0..9000).map(|i| keyed(doc! { "_id": i, "n": f64::from(i) }));
        assert_eq!(spilled(dbl.collect(), 200_000), (3, 306_000, 109_279));
    }

    /// The smallest budget a pipeline runs under with `allowDiskUse: false`,
    /// bisected as the 8.2.11 measurements were.
    fn least_budget(pipeline: &[Bson], docs: &[Document]) -> usize {
        // A `$setWindowFields` here sorts by an index: only its own budget
        // is bisected.
        let windowed = pipeline.iter().any(|s| {
            s.as_document()
                .is_some_and(|d| d.contains_key("$setWindowFields"))
        });
        let runs = |budget: usize| {
            let budgets = memlimit::Budgets {
                sort: if windowed {
                    memlimit::MAX_BYTES
                } else {
                    budget
                },
                window_fields: budget,
                ..Default::default()
            };
            let _b = memlimit::with_budgets(budgets);
            let _d = memlimit::allow_disk_use(false);
            crate::aggregate::apply_pipeline(docs.to_vec(), pipeline, &Document::new(), None)
                .is_ok()
        };
        let (mut lo, mut hi) = (1, 100_000_000);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if runs(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }

    #[test]
    fn a_sort_after_another_stage_counts_as_mongods_does() {
        // mongod 8.2.11 over 100 documents `{_id: i, s: 2000 x's}`.
        let docs: Vec<Document> = (0..100)
            .map(|i| doc! { "_id": i, "s": "x".repeat(2000) })
            .collect();
        let sort = Bson::Document(doc! { "$sort": { "s": 1, "_id": 1 } });
        let unwound = vec![Bson::Document(doc! { "$unwind": "$s" }), sort.clone()];
        assert_eq!(least_budget(&unwound, &docs), 485_952);
        let changed = vec![
            Bson::Document(doc! { "$addFields": { "z": 1.5 } }),
            sort.clone(),
        ];
        assert_eq!(least_budget(&changed, &docs), 484_080);
        let read = vec![Bson::Document(doc! { "$limit": 1000 }), sort];
        assert_eq!(least_budget(&read, &docs), 469_920);
    }

    #[test]
    fn a_window_partition_counts_its_cache_and_functions() {
        // mongod 8.2.11 over `{_id: i, k: i % 7, n: 2i, s: (300 + i) x's}`,
        // sorted by `n` (an index serves it, so no sort is budgeted).
        let docs: Vec<Document> = (0..100)
            .map(|i| doc! { "_id": i, "k": i % 7, "n": i * 2, "s": "x".repeat(300 + i as usize) })
            .collect();
        let swf = |output: Document| {
            vec![Bson::Document(
                doc! { "$setWindowFields": { "sortBy": { "n": 1 }, "output": { "t": output } } },
            )]
        };
        // The partition and `$sum`'s own size (Linux's 176 bytes: 49126;
        // macOS arm64 counts 184): 5414201 below.
        let sum = crate::memlimit::LAYOUT.sum;
        assert_eq!(
            least_budget(&swf(doc! { "$sum": "$n" }), &docs),
            48_950 + sum
        );
        // The document answered and the next: 5643011 below.
        let uc = doc! { "$sum": "$n", "window": { "documents": ["unbounded", "current"] } };
        assert_eq!(least_budget(&swf(uc), &docs), 1_077);
        let shift = doc! { "$shift": { "output": "$n", "by": 3 } };
        assert_eq!(least_budget(&swf(shift), &docs), 2_150);
    }
}
