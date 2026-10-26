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
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use bson::{doc, Bson, Document};

use crate::fallback::Fallback;
use crate::memlimit;

type R<T> = Result<T, Fallback>;

/// `sizeof(FileIterator<Key, Value>)` on mongod 8.2.11 (arm64), measured.
const FILE_ITERATOR: usize = 136;
/// `kSortedFileBufferSize`.
const SORTED_FILE_BUFFER: usize = 64 << 10;

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

/// `Value::serializeForSorter`: the bytes one value takes in a spill.
fn value_bytes(v: &Bson) -> usize {
    1 + match v {
        Bson::Null | Bson::Undefined | Bson::MinKey | Bson::MaxKey => 0,
        Bson::ObjectId(_) => 12,
        Bson::Int32(_) => 4,
        Bson::Int64(_) | Bson::Double(_) | Bson::DateTime(_) | Bson::Timestamp(_) => 8,
        Bson::Decimal128(_) => 16,
        Bson::Boolean(_) => 1,
        Bson::String(x) | Bson::Symbol(x) | Bson::JavaScriptCode(x) => 4 + x.len(),
        Bson::Binary(b) => 1 + 4 + b.bytes.len(),
        Bson::RegularExpression(r) => r.pattern.len() + 1 + r.options.len() + 1,
        Bson::Document(d) => document_bytes(d),
        Bson::Array(a) => 4 + a.iter().map(value_bytes).sum::<usize>(),
        Bson::JavaScriptCodeWithScope(c) => 4 + c.code.len() + memlimit::bson_len(&c.scope),
        Bson::DbPointer(_) => 12 + 33,
    }
}

/// `Document::serializeForSorter`: its fields, then its metadata (a byte
/// when it has none).
fn document_bytes(d: &Document) -> usize {
    4 + d
        .iter()
        .map(|(k, v)| k.len() + 1 + value_bytes(v))
        .sum::<usize>()
        + 1
}

/// The bytes a key and its document take in a spill (the sort's
/// `spilledBytes`): the key as a `Value` (an array of them for a compound
/// sort) and the document as this sorter keeps it -- a classic `Document`,
/// the query layer's working-set member (its BSON), or `$bucketAuto`'s
/// wrapper around the document.
fn spilled_bytes(keys: &[Bson], doc: &Document, sizing: &Sizing) -> usize {
    let key = match keys {
        [one] => value_bytes(one),
        many => 1 + 4 + many.iter().map(value_bytes).sum::<usize>(),
    };
    let kept = || -> Document {
        doc.iter()
            .filter(|(k, _)| {
                sizing.fields.is_none_or(|f| f.contains(k.as_str()))
                    || sizing.added.is_some_and(|a| a.contains(k.as_str()))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    let value = match sizing.source {
        SortSource::QueryLayer => memlimit::bson_len(doc),
        SortSource::Sbe => memlimit::bson_len(&kept()),
        SortSource::BucketAuto => {
            // `{output: <document>, generatedSortKey: <long>}`.
            4 + 7 + 1 + document_bytes(&kept()) + 17 + 9 + 1
        }
        SortSource::Stored | SortSource::Modified | SortSource::Unwound => document_bytes(&kept()),
    };
    key + value
}

fn io_error(what: &str, path: &std::path::Path, e: impl std::fmt::Display) -> Fallback {
    Fallback::mongo(
        1,
        format!(
            "the sort could not {what} its spill file {}: {e}",
            path.display()
        ),
    )
}

/// One sorted run on disk. Its file goes when it is dropped.
struct Run {
    path: PathBuf,
    records: usize,
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

/// Write sorted `items` as a run, counted as one spill.
fn write_run(items: impl Iterator<Item = (Vec<Bson>, Document)>, sizing: &Sizing) -> R<Run> {
    let bucket_auto = sizing.source == SortSource::BucketAuto;
    let dir = memlimit::spill_dir();
    std::fs::create_dir_all(&dir).map_err(|e| io_error("create the directory for", &dir, e))?;
    let path = dir.join(format!(
        "secantus-sort-{}-{}.bson",
        std::process::id(),
        NEXT_FILE.fetch_add(1, AtomicOrdering::Relaxed)
    ));
    let file = File::create(&path).map_err(|e| io_error("create", &path, e))?;
    let mut run = Run {
        path: path.clone(),
        records: 0,
    };
    let mut out = BufWriter::new(file);
    let mut bytes = 0i64;
    for (keys, d) in items {
        bytes += spilled_bytes(&keys, &d, sizing) as i64;
        let mut buf = Vec::new();
        doc! { "k": keys, "d": d }
            .to_writer(&mut buf)
            .map_err(|e| io_error("encode a record for", &path, e))?;
        out.write_all(&buf)
            .map_err(|e| io_error("write", &path, e))?;
        run.records += 1;
    }
    out.flush().map_err(|e| io_error("write", &path, e))?;
    let file = out
        .into_inner()
        .map_err(|e| io_error("write", &path, e.error()))?;
    file.sync_all().map_err(|e| io_error("write", &path, e))?;
    let stored = file
        .metadata()
        .map_err(|e| io_error("measure", &path, e))?
        .len() as i64;
    if bucket_auto {
        memlimit::record_bucket_auto_spill(run.records as i64, bytes, stored);
    } else {
        memlimit::record_sort_spill(run.records as i64, bytes, stored);
    }
    Ok(run)
}

/// A run being read back.
struct Reader {
    input: BufReader<File>,
    path: PathBuf,
    left: usize,
}

impl Reader {
    fn open(run: &Run) -> R<Reader> {
        let file = File::open(&run.path).map_err(|e| io_error("open", &run.path, e))?;
        Ok(Reader {
            input: BufReader::new(file),
            path: run.path.clone(),
            left: run.records,
        })
    }

    fn next(&mut self) -> R<Option<(Vec<Bson>, Document)>> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        let mut len = [0u8; 4];
        self.input
            .read_exact(&mut len)
            .map_err(|e| io_error("read", &self.path, e))?;
        let size = usize::try_from(i32::from_le_bytes(len))
            .ok()
            .filter(|n| *n >= 5)
            .ok_or_else(|| io_error("read", &self.path, "a record with a bad length"))?;
        let mut bytes = vec![0u8; size];
        bytes[..4].copy_from_slice(&len);
        self.input
            .read_exact(&mut bytes[4..])
            .map_err(|e| io_error("read", &self.path, e))?;
        let mut record =
            crate::bsondoc::from_bytes(&bytes).map_err(|e| io_error("read", &self.path, e))?;
        let keys = match record.remove("k") {
            Some(Bson::Array(k)) => k,
            _ => return Err(io_error("read", &self.path, "a record without its key")),
        };
        let d = match record.remove("d") {
            Some(Bson::Document(d)) => d,
            _ => {
                return Err(io_error(
                    "read",
                    &self.path,
                    "a record without its document",
                ))
            }
        };
        Ok(Some((keys, d)))
    }
}

/// Merge `runs` (in order) into one stream, equal keys in run order.
fn merge_runs(
    runs: &[Run],
    cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering,
    mut emit: impl FnMut((Vec<Bson>, Document)) -> R<()>,
) -> R<()> {
    let mut readers = runs.iter().map(Reader::open).collect::<R<Vec<_>>>()?;
    let mut heads = Vec::with_capacity(readers.len());
    for r in readers.iter_mut() {
        heads.push(r.next()?);
    }
    loop {
        crate::deadline::check()?;
        let mut least: Option<usize> = None;
        for (i, h) in heads.iter().enumerate() {
            let Some((k, _)) = h else { continue };
            match least {
                None => least = Some(i),
                Some(l) => {
                    let (lk, _) = heads[l].as_ref().expect("a head");
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
            let mut merged: Vec<(Vec<Bson>, Document)> = Vec::new();
            // Read the chunk through a writer of its own: mongod writes each
            // merged chunk as a run, a single run included.
            merge_runs(chunk, cmp, |item| {
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
    let size = |(k, d): &(Vec<Bson>, Document)| sized_key(k, sizing.source) + doc_size(d, sizing);
    let n = items.len() as i64;
    let bucket_auto = sizing.source == SortSource::BucketAuto;
    let record = |bytes: usize| {
        if !bucket_auto {
            memlimit::record_sort(n, bytes as i64);
        }
    };

    if let Some(k) = limit {
        // mongod's top-k sorter keeps only the k first; a limit of one never
        // goes near its budget.
        let mut items = items;
        items.sort_by(|a, b| cmp(&a.0, &b.0));
        items.truncate(k);
        let held: usize = items.iter().map(size).sum();
        record(held);
        if k > 1 && held > effective && !memlimit::disk_allowed() {
            return Err(refused(budget));
        }
        return Ok(items);
    }

    let iterators = (reserve / sizing.source.file_iterator()).max(1);
    let parallel = (budget / SORTED_FILE_BUFFER).max(2);
    let mut held: Vec<(Vec<Bson>, Document)> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut used = 0usize;
    let mut total = 0usize;
    let spill = |held: &mut Vec<(Vec<Bson>, Document)>, runs: &mut Vec<Run>| -> R<()> {
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
            used = 0;
        }
    }
    record(total);
    if runs.is_empty() {
        held.sort_by(|a, b| cmp(&a.0, &b.0));
        return Ok(held);
    }
    spill(&mut held, &mut runs)?;
    let runs = merge_spills(runs, parallel, parallel, cmp, sizing)?;
    let mut out = Vec::with_capacity(n as usize);
    merge_runs(&runs, cmp, |item| {
        out.push(item);
        Ok(())
    })?;
    Ok(out)
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
    }
}
