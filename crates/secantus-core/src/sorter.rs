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
//! the last runs are merged as they are read. Each run, and a sort that
//! never spills, is sorted by `std::sort`, which is not stable: documents
//! whose keys compare equal come in the C++ library's order ([`sort_held`]),
//! so a spilled sort can answer ties in another order than one held in
//! memory, as mongod's does. The merges keep equal keys in the order of
//! their runs.
//!
//! What a document counts depends on the stage that sorts it, all measured
//! against 8.2.11 by the same bisection:
//!
//! | sort | document counts |
//! | --- | --- |
//! | the query layer's (`find`, a pipeline's leading `$sort`) | its BSON + 16 |
//! | `$sort` over documents read from the collection | its BSON + 96 |
//! | `$sort` after `$addFields` / `$set` / `$unwind` | its storage ([`crate::docstore`], RM-408) |
//! | `$bucketAuto` | its BSON + 333 |
//!
//! (The storage model gives the +224 and +240 once measured here for a
//! short field an `$addFields` added and an `$unwind` of strings set: a
//! 128-byte field cache, and what the unwound value holds beyond its
//! BSON.)
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
    /// The query layer's `SORT` of index keys (`type: "default"`, below
    /// the FETCH), or over a covered projection: working-set members, each
    /// held as [`member_doc`] and sized by its caller ([`sort_index_keys`],
    /// [`sort_owned_objs`]).
    Member,
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
            // Never added: a member's size is its caller's.
            SortSource::Member => 0,
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
    /// Each document's size by its place in the sort's input, when the
    /// stages before it left a storage mongod sizes otherwise than as read
    /// (`memUsageForSorter`, [`crate::docstore`]): it stands for the
    /// document's and its overhead.
    pub sizes: Option<&'a [usize]>,
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
    /// Each record's place in the sort's input, in the order written: not
    /// in the file, which holds what mongod's does, but kept so a caller
    /// can carry what it keeps beside each document through the sort.
    positions: Vec<i64>,
    /// An SBE sort's row slots ([`sbe_slots`]), and each record's kept
    /// fields as places in them, in the order its document held them: the
    /// row holds the values in its slots' order, and the document comes
    /// back as it went in.
    slots: Option<Vec<String>>,
    orders: Vec<Vec<usize>>,
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

/// The slots of an SBE sort's row of kept values, by name
/// (`PlanStageSlots::getRequiredNamesInOrder` sorts them, and the scan
/// makes their slots in that order), or `None` when the stages after it
/// read the whole document, which the row then holds as its one value.
fn sbe_slots(sizing: &Sizing) -> Option<Vec<String>> {
    let fields = sizing.fields?;
    let mut names: BTreeSet<&String> = fields.iter().collect();
    names.extend(sizing.added.into_iter().flatten());
    Some(names.into_iter().cloned().collect())
}

/// One record as this sorter's mongod counterpart serializes it: the key
/// as a `Value` (an array of them for a compound sort), then the document
/// as that sorter keeps it -- the query layer's as BSON, a pipeline
/// `$sort`'s as a `Document`, `$bucketAuto`'s inside the `{output,
/// generatedSortKey}` document it sorts. SBE's sorter sorts rows: the key's
/// values, then the kept fields' in `slots` (RM-506).
fn write_record(
    (keys, d, pos): &Held,
    sizing: &Sizing,
    slots: Option<&[String]>,
    out: &mut Vec<u8>,
) -> Result<(), spill_file::Error> {
    if sizing.source == SortSource::Sbe {
        spill_file::write_sbe_row(&keys.iter().map(Some).collect::<Vec<_>>(), out)?;
        return match slots {
            Some(names) => {
                spill_file::write_sbe_row(&names.iter().map(|n| d.get(n)).collect::<Vec<_>>(), out)
            }
            None => spill_file::write_sbe_row(&[Some(&Bson::Document(d.clone()))], out),
        };
    }
    match keys.as_slice() {
        [one] => spill_file::write_value(one, out)?,
        many => spill_file::write_value(&Bson::Array(many.to_vec()), out)?,
    }
    match sizing.source {
        SortSource::QueryLayer => spill_file::write_bson(d, out),
        SortSource::Sbe => unreachable!("an SBE row is written above"),
        SortSource::BucketAuto => spill_file::write_document(
            &doc! { "output": kept(d, sizing), "generatedSortKey": *pos },
            out,
        ),
        SortSource::Stored | SortSource::Modified | SortSource::Unwound => {
            spill_file::write_document(&kept(d, sizing), out)
        }
        SortSource::Member => match d.get(MEMBER_FIELD) {
            Some(Bson::Binary(b)) => {
                out.extend_from_slice(&b.bytes);
                Ok(())
            }
            _ => Err(spill_file::Error {
                what: "write",
                detail: "a working-set member that is not one".into(),
            }),
        },
    }
}

fn read_record(
    at: &mut spill_file::Cursor,
    arity: usize,
    source: SortSource,
    sbe: Option<(&[String], &[usize])>,
) -> Result<Held, spill_file::Error> {
    if source == SortSource::Sbe {
        return read_sbe_record(at, arity, sbe);
    }
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
        SortSource::QueryLayer => (spill_file::read_bson(at)?, 0),
        SortSource::Sbe => unreachable!("an SBE row is read above"),
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
        SortSource::Member => (member_doc(spill_file::read_member(at)?), 0),
    };
    Ok((keys, d, pos))
}

/// The record a [`write_record`] wrote for an SBE sort: `sbe` holds the
/// row's slots and this record's fields as places in them, in its order.
fn read_sbe_record(
    at: &mut spill_file::Cursor,
    arity: usize,
    sbe: Option<(&[String], &[usize])>,
) -> Result<Held, spill_file::Error> {
    let bad = |detail: &str| spill_file::Error {
        what: "read",
        detail: detail.into(),
    };
    let keys = spill_file::read_sbe_row(at)?
        .into_iter()
        .collect::<Option<Vec<Bson>>>()
        .filter(|k| k.len() == arity)
        .ok_or_else(|| bad("a record whose key is not the sort's"))?;
    let mut values = spill_file::read_sbe_row(at)?;
    let d = match sbe {
        Some((names, order)) => {
            if values.len() != names.len() {
                return Err(bad("a row that is not the sort's"));
            }
            let mut d = Document::new();
            for &i in order {
                if let Some(v) = values.get_mut(i).and_then(Option::take) {
                    d.insert(names[i].clone(), v);
                }
            }
            d
        }
        None => match (values.pop(), values.is_empty()) {
            (Some(Some(Bson::Document(d))), true) => d,
            _ => return Err(bad("a row without its document")),
        },
    };
    Ok((keys, d, 0))
}

/// `NoLimitSorter::sort`: what the sorter holds, in the order it came,
/// sorted by `std::sort` on the keys alone -- before each spill, and once
/// at the end when it never spilled. `std::sort` is not stable, so the
/// order of documents whose keys compare equal is the C++ library's:
/// libstdc++'s introsort ([`crate::stl::sort`]) on the Linux build (RM-402:
/// the runs of up to 21 documents over seven keys a sort after `$project`
/// spills store mongod 8.2.12's 122253 bytes only in that order), libc++'s
/// ([`crate::stl::libcxx::sort`]) on the macOS build (RM-526).
fn sort_held(held: &mut Vec<Held>, cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering) {
    let order = std_sort_order(held.len(), &|a, b| cmp(&held[a].0, &held[b].0));
    let mut taken: Vec<Option<Held>> = std::mem::take(held).into_iter().map(Some).collect();
    held.extend(
        order
            .into_iter()
            .map(|i| taken[i].take().expect("each position once")),
    );
}

/// The order mongod's `std::sort` leaves `n` items in, given as positions
/// in the order they came: `cmp(a, b)` compares the items at positions `a`
/// and `b` by their sort keys alone. Items whose keys compare equal come
/// where the build's C++ library puts them: libstdc++'s introsort
/// ([`crate::stl::sort`]) on the Linux build, mongod's exemplar, and
/// libc++'s ([`crate::stl::libcxx::sort`]) on the macOS build (RM-526).
///
/// Every sort mongod runs through its sorter without a limit -- a
/// `$sort`, and a `find`'s `SORT` stage (RM-312) -- leaves ties this way.
pub fn std_sort_order(n: usize, cmp: &dyn Fn(usize, usize) -> Ordering) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    // The algorithms move positions, so sorting them moves no item.
    let less = |a: &usize, b: &usize| cmp(*a, *b) == Ordering::Less;
    if crate::stl::LIBCXX {
        crate::stl::libcxx::sort(&mut order, &less);
    } else {
        crate::stl::sort(&mut order, &less);
    }
    order
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
        positions: Vec::new(),
        slots: (sizing.source == SortSource::Sbe)
            .then(|| sbe_slots(sizing))
            .flatten(),
        orders: Vec::new(),
    };
    for item in items {
        run.arity = item.0.len();
        run.positions.push(item.2);
        if let Some(names) = &run.slots {
            run.orders.push(
                item.1
                    .keys()
                    .filter_map(|k| names.binary_search(k).ok())
                    .collect(),
            );
        }
        write_record(&item, sizing, run.slots.as_deref(), out.buffer())
            .map_err(|e| io_error(&path, e))?;
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
struct Reader<'a> {
    input: spill_file::Reader,
    path: PathBuf,
    positions: std::slice::Iter<'a, i64>,
    slots: Option<&'a [String]>,
    orders: std::slice::Iter<'a, Vec<usize>>,
    arity: usize,
    source: SortSource,
}

impl<'a> Reader<'a> {
    fn open(run: &'a Run, source: SortSource) -> R<Reader<'a>> {
        Ok(Reader {
            input: spill_file::Reader::open(&run.path).map_err(|e| io_error(&run.path, e))?,
            path: run.path.clone(),
            positions: run.positions.iter(),
            slots: run.slots.as_deref(),
            orders: run.orders.iter(),
            arity: run.arity,
            source,
        })
    }

    fn next(&mut self) -> R<Option<Held>> {
        let Some(&pos) = self.positions.next() else {
            return Ok(None);
        };
        let (arity, source) = (self.arity, self.source);
        let sbe = match self.slots {
            Some(names) => {
                let order = self.orders.next().ok_or_else(|| {
                    io_error(
                        &self.path,
                        spill_file::Error {
                            what: "read",
                            detail: "a record without its fields' order".into(),
                        },
                    )
                })?;
                Some((names, order.as_slice()))
            }
            None => None,
        };
        let (keys, d, _) = self
            .input
            .record(|at| read_record(at, arity, source, sbe))
            .map_err(|e| io_error(&self.path, e))?;
        Ok(Some((keys, d, pos)))
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
/// `limit`: a top-k sorter's, which its merge iterator stops at, so each
/// merged run holds no more than that many (mongod 8.2.11: a limit of 50
/// over seven runs of 43 writes merged runs of 50, 50, 50, 42, then 50, 50;
/// RM-399).
fn merge_spills(
    runs: Vec<Run>,
    target: usize,
    parallel: usize,
    cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering,
    sizing: &Sizing,
    limit: Option<usize>,
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
            if let Some(k) = limit {
                merged.truncate(k);
            }
            next.push(write_run(merged.into_iter(), sizing)?);
        }
        runs = next;
    }
    Ok(runs)
}

/// mongod 8.2's `TopKSorter` (`sorter_template_defs.h`): a sort that keeps
/// only its first `k`, held to its budget as [`blocking_sort`]'s is, spilling
/// what it holds as a sorted run whenever it passes it (RM-399).
///
/// It adds each document while it holds fewer than `k`, counting its bytes
/// as sorted, and once it holds `k` keeps them as a heap, a better document
/// replacing the worst (its bytes then count in memory, not as sorted). At
/// each spill it moves a cutoff from what the runs so far hold -- the worst
/// key it has kept once `k` were kept at or above it, or the median of a
/// spill once `k` were kept at or below that -- and turns away any document
/// not better than it. Its merges stop each merged run at `k`.
///
/// Measured against mongod 8.2.11 over 300 documents of {_id, k, s: 1000
/// x's, n}, sorted both ways over `n` in and out of input order, limits 2 to
/// 250, under a 50000-byte budget: every spill, record and byte matched.
struct TopK<'a> {
    k: usize,
    cmp: &'a dyn Fn(&[Bson], &[Bson]) -> Ordering,
    sizing: &'a Sizing<'a>,
    effective: usize,
    iterators: usize,
    parallel: usize,
    /// What it holds, with what each counts; a heap of the worst first once
    /// it holds `k`.
    data: Vec<(Held, usize)>,
    used: usize,
    runs: Vec<Run>,
    cutoff: Option<Vec<Bson>>,
    worst_seen: Option<Vec<Bson>>,
    worst_count: usize,
    last_median: Option<Vec<Bson>>,
    median_count: usize,
    /// Bytes counted as sorted: those of each document added while it held
    /// fewer than `k`.
    sorted: usize,
    /// What it held at each spill while taking its input (SBE's count).
    spilled: usize,
}

impl<'a> TopK<'a> {
    fn new(
        k: usize,
        cmp: &'a dyn Fn(&[Bson], &[Bson]) -> Ordering,
        sizing: &'a Sizing<'a>,
        effective: usize,
        iterators: usize,
        parallel: usize,
    ) -> Self {
        TopK {
            k,
            cmp,
            sizing,
            effective,
            iterators,
            parallel,
            data: Vec::new(),
            used: 0,
            runs: Vec::new(),
            cutoff: None,
            worst_seen: None,
            worst_count: 0,
            last_median: None,
            median_count: 0,
            sorted: 0,
            spilled: 0,
        }
    }

    /// mongod's `less` on keys alone.
    fn less(&self, a: &[Bson], b: &[Bson]) -> bool {
        (self.cmp)(a, b) == Ordering::Less
    }

    /// The order the heap keeps: by key, then by arrival, so equal keys
    /// answer in the order they came, as the whole sort's do.
    fn later(&self, a: &Held, b: &Held) -> bool {
        (self.cmp)(&a.0, &b.0).then(a.2.cmp(&b.2)) == Ordering::Greater
    }

    fn sift_down(&mut self, mut i: usize) {
        let n = self.data.len();
        loop {
            let (l, r) = (2 * i + 1, 2 * i + 2);
            let mut m = i;
            if l < n && self.later(&self.data[l].0, &self.data[m].0) {
                m = l;
            }
            if r < n && self.later(&self.data[r].0, &self.data[m].0) {
                m = r;
            }
            if m == i {
                return;
            }
            self.data.swap(i, m);
            i = m;
        }
    }

    fn add(&mut self, item: Held, size: usize) -> R<()> {
        if self.data.len() < self.k {
            if self.cutoff.as_ref().is_some_and(|c| !self.less(&item.0, c)) {
                return Ok(());
            }
            self.data.push((item, size));
            self.used += size;
            self.sorted += size;
            if self.data.len() == self.k {
                for i in (0..self.k / 2).rev() {
                    self.sift_down(i);
                }
            }
        } else {
            if !self.less(&item.0, &self.data[0].0 .0) {
                return Ok(());
            }
            self.used = self.used + size - self.data[0].1;
            self.data[0] = (item, size);
            self.sift_down(0);
        }
        if self.used > self.effective {
            self.spilled += self.used;
            self.spill()?;
        }
        Ok(())
    }

    fn sort(&mut self) {
        let cmp = self.cmp;
        self.data
            .sort_by(|(a, _), (b, _)| cmp(&a.0, &b.0).then(a.2.cmp(&b.2)));
    }

    /// `TopKSorter::updateCutoff`, over what it holds, sorted.
    fn update_cutoff(&mut self) {
        let Some(((worst, _, _), _)) = self.data.last() else {
            return;
        };
        if self.worst_count == 0
            || self
                .worst_seen
                .as_ref()
                .is_some_and(|w| self.less(w, worst))
        {
            self.worst_seen = Some(worst.clone());
        }
        if self.median_count == 0 {
            self.last_median = Some(self.data[self.data.len() / 2].0 .0.clone());
        }
        self.worst_count += self.data.len();
        let median = self.last_median.as_ref().expect("a median");
        self.median_count += self
            .data
            .iter()
            .take_while(|((k, _, _), _)| !self.less(median, k))
            .count();
        if self.worst_count >= self.k {
            let w = self.worst_seen.as_ref().expect("a worst");
            if self.cutoff.as_ref().is_none_or(|c| self.less(w, c)) {
                self.cutoff = self.worst_seen.clone();
            }
            self.worst_count = 0;
        }
        if self.median_count >= self.k {
            let m = self.last_median.as_ref().expect("a median");
            if self.cutoff.as_ref().is_none_or(|c| self.less(m, c)) {
                self.cutoff = self.last_median.clone();
            }
            self.median_count = 0;
        }
    }

    fn spill(&mut self) -> R<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        if !memlimit::disk_allowed() {
            return Err(refused(self.sizing.budget));
        }
        self.sort();
        self.update_cutoff();
        let held = std::mem::take(&mut self.data);
        self.runs
            .push(write_run(held.into_iter().map(|(h, _)| h), self.sizing)?);
        self.used = 0;
        if self.runs.len() >= self.iterators {
            let all = std::mem::take(&mut self.runs);
            self.runs = merge_spills(
                all,
                self.iterators / 2,
                self.parallel,
                self.cmp,
                self.sizing,
                Some(self.k),
            )?;
        }
        Ok(())
    }

    fn done(mut self) -> R<Vec<Held>> {
        if self.runs.is_empty() {
            self.sort();
            return Ok(self.data.into_iter().map(|(h, _)| h).collect());
        }
        self.spill()?;
        let runs = std::mem::take(&mut self.runs);
        let runs = merge_spills(
            runs,
            self.parallel,
            self.parallel,
            self.cmp,
            self.sizing,
            Some(self.k),
        )?;
        let mut out = Vec::with_capacity(self.k);
        merge_runs(&runs, self.sizing.source, self.cmp, |item| {
            if out.len() < self.k {
                out.push(item);
            }
            Ok(())
        })?;
        Ok(out)
    }
}

/// Sort `items` by `cmp` (stable), held to `sizing.budget`: spilled to disk
/// when the command allows it, refused (292) when it does not. `limit`: a
/// `$limit` mongod folds into the sort, which then keeps only that many, as
/// its top-k sorter ([`TopK`]) does.
pub fn blocking_sort(
    items: Vec<(Vec<Bson>, Document)>,
    cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering,
    sizing: &Sizing,
    limit: Option<usize>,
) -> R<Vec<(Vec<Bson>, Document)>> {
    Ok(sort_items(items, cmp, sizing, limit)?
        .into_iter()
        .map(|(k, d, _)| (k, d))
        .collect())
}

/// [`blocking_sort`], giving each item's place in `items` in the order the
/// sort leaves them, for a caller that sorts its own copy of the items.
pub fn blocking_sort_order(
    items: Vec<(Vec<Bson>, Document)>,
    cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering,
    sizing: &Sizing,
) -> R<Vec<usize>> {
    Ok(sort_items(items, cmp, sizing, None)?
        .into_iter()
        .map(|(_, _, pos)| pos as usize)
        .collect())
}

/// [`blocking_sort`], each item with its place in `items`.
fn sort_items(
    items: Vec<(Vec<Bson>, Document)>,
    cmp: &dyn Fn(&[Bson], &[Bson]) -> Ordering,
    sizing: &Sizing,
    limit: Option<usize>,
) -> R<Vec<Held>> {
    let budget = sizing.budget;
    let reserve = reserved(budget, sizing.source.file_iterator());
    let effective = budget.saturating_sub(reserve);
    let size = |(k, d, pos): &Held| {
        sized_key(k, sizing.source)
            + match sizing.sizes {
                Some(sizes) => sizes[*pos as usize],
                None => doc_size(d, sizing),
            }
    };
    let items = items
        .into_iter()
        .enumerate()
        .map(|(i, (k, d))| (k, d, i as i64));
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

    let iterators = (reserve / sizing.source.file_iterator()).max(1);
    let parallel = (budget / SORTED_FILE_BUFFER).max(2);

    match limit {
        Some(k) if k <= 1 => {
            // A limit of one never goes near its budget. Its sorter
            // (sorter.cpp's `LimitOneSorter`) counts the keys it saw but no
            // bytes: mongod 8.2.11 reports `totalDataSizeSorted: 0` for it
            // (RM-374).
            let mut items: Vec<Held> = items.collect();
            items.sort_by(|a, b| cmp(&a.0, &b.0));
            items.truncate(k);
            record(0);
            return Ok(items);
        }
        Some(k) => {
            let mut top = TopK::new(k, cmp, sizing, effective, iterators, parallel);
            for item in items {
                crate::deadline::check()?;
                let s = size(&item);
                top.add(item, s)?;
            }
            record(if sbe { top.spilled } else { top.sorted });
            return top.done();
        }
        None => {}
    }

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
        sort_held(held, cmp);
        runs.push(write_run(std::mem::take(held).into_iter(), sizing)?);
        if runs.len() >= iterators {
            let all = std::mem::take(runs);
            *runs = merge_spills(all, iterators / 2, parallel, cmp, sizing, None)?;
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
        sort_held(&mut held, cmp);
        return Ok(held);
    }
    spill(&mut held, &mut runs)?;
    let runs = merge_spills(runs, parallel, parallel, cmp, sizing, None)?;
    let mut out = Vec::with_capacity(n as usize);
    merge_runs(&runs, sizing.source, cmp, |item| {
        out.push(item);
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

/// The snapshot a member was read in, as `WorkingSetMember::serialize`
/// writes it: a number mongod draws from a process-wide counter as each
/// storage transaction opens a snapshot, which this server does not keep.
/// It moves only what the run compresses to (`spilledDataStorageSize`).
const SNAPSHOT_ID: u64 = 0;

/// Where a [`SortSource::Member`] item's document holds the member's
/// serialized bytes.
const MEMBER_FIELD: &str = "m";

/// A serialized working-set member, as the sorter holds it.
fn member_doc(bytes: Vec<u8>) -> Document {
    let mut d = Document::new();
    d.insert(
        MEMBER_FIELD,
        Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Generic,
            bytes,
        }),
    );
    d
}

/// A working-set member of mongod's `SORT` of index keys: its sort key's
/// values, and the index entry it was found at.
pub struct IndexKeyMember<'a> {
    pub sort_key: Vec<Bson>,
    /// The index's key pattern, as it was created.
    pub key_pattern: &'a Document,
    /// The values of the index key it was found at, in the index's order.
    pub index_key: Vec<Bson>,
    /// The index's place among those the plan's scans registered with its
    /// working set (`WorkingSet::registerIndexIdent`), from 0.
    pub index_id: u32,
    pub rid: i64,
}

/// mongod 8.2's `SORT` of index keys (`type: "default"`, below the FETCH)
/// over `members` in the order they came, run for what it counts: each
/// member counts its [`member_size`], the sort is held to `budget` -- past
/// it, refused (292) without disk use, and with it spilled to runs of the
/// members as `WorkingSetMember::serialize` writes them (RM-475), merged
/// as [`blocking_sort`]'s are. `descending`: each sort field's direction.
/// `limit`: the sort's own, which keeps that many (its top-k sorter).
///
/// The caller orders the members itself; this answers only what the
/// sorter counts and spills, into [`memlimit::report`].
pub fn sort_index_keys(
    members: Vec<IndexKeyMember>,
    descending: &[bool],
    budget: usize,
    limit: Option<usize>,
) -> R<()> {
    let mut sizes = Vec::with_capacity(members.len());
    let mut items = Vec::with_capacity(members.len());
    for m in members {
        sizes.push(memlimit::index_key_len(&m.index_key) + MEMBER);
        let mut bytes = Vec::new();
        spill_file::write_index_key_member(
            m.key_pattern,
            &m.index_key,
            m.index_id,
            SNAPSHOT_ID,
            m.rid,
            &mut bytes,
        )
        .map_err(|e| Fallback::mongo(1, format!("the sort could not {}: {}", e.what, e.detail)))?;
        items.push((m.sort_key, member_doc(bytes)));
    }
    sort_members(items, &sizes, descending, budget, limit)
}

/// A document `PROJECTION_COVERED` made of an index scan's member, as the
/// `SORT` above it holds it: an `OWNED_OBJ` working-set member, its keys
/// and RecordId dropped (`ProjectionStageCovered::transform`).
pub struct OwnedObjMember {
    pub sort_key: Vec<Bson>,
    /// The document, its fields in the index's order (see
    /// [`crate::projection::covered_document`]).
    pub doc: Document,
}

/// The snapshot id of a document a stage built rather than read
/// (`WorkingSetMember::resetDocument` with a default `SnapshotId`).
const NO_SNAPSHOT: u64 = 0;

/// What one [`OwnedObjMember`] counts beside its sort key
/// (`WorkingSetMember::getMemUsage`): its document's
/// `Document::getApproximateSize` and nothing else, having no RecordId and
/// no index key. mongod 8.2.11 counts 132 for an int sort key over
/// `{x: <int>}` (RM-473).
pub fn owned_obj_size(doc: &Document) -> usize {
    memlimit::bson_len(doc) + memlimit::DOCUMENT_OVERHEAD
}

/// mongod 8.2's `SORT` over a covered projection (`SORT <-
/// PROJECTION_COVERED <- IXSCAN`, `type: "default"`), run for what it
/// counts: each member its sort key and [`owned_obj_size`], held to
/// `budget` and spilled, past it with disk use, as
/// `WorkingSetMember::serialize` writes an `OWNED_OBJ` member. As
/// [`sort_index_keys`], the caller orders the members itself.
pub fn sort_owned_objs(
    members: Vec<OwnedObjMember>,
    descending: &[bool],
    budget: usize,
    limit: Option<usize>,
) -> R<()> {
    let mut sizes = Vec::with_capacity(members.len());
    let mut items = Vec::with_capacity(members.len());
    for m in members {
        sizes.push(owned_obj_size(&m.doc));
        let mut bytes = Vec::new();
        spill_file::write_owned_obj_member(&m.doc, NO_SNAPSHOT, &mut bytes).map_err(|e| {
            Fallback::mongo(1, format!("the sort could not {}: {}", e.what, e.detail))
        })?;
        items.push((m.sort_key, member_doc(bytes)));
    }
    sort_members(items, &sizes, descending, budget, limit)
}

/// Run working-set members, each held as [`member_doc`] and counting its
/// size in `sizes` beside its key, through a query-layer sorter.
fn sort_members(
    items: Vec<(Vec<Bson>, Document)>,
    sizes: &[usize],
    descending: &[bool],
    budget: usize,
    limit: Option<usize>,
) -> R<()> {
    let cmp = |a: &[Bson], b: &[Bson]| {
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            let c = crate::order::cmp(x, y);
            let c = if descending.get(i).copied().unwrap_or(false) {
                c.reverse()
            } else {
                c
            };
            if c != Ordering::Equal {
                return c;
            }
        }
        Ordering::Equal
    };
    let sizing = Sizing {
        source: SortSource::Member,
        fields: None,
        budget,
        added: None,
        sizes: Some(sizes),
    };
    sort_items(items, &cmp, &sizing, limit)?;
    Ok(())
}

/// `docs` sorted by the `$sort` spec `spec`, held to the budget (see
/// [`blocking_sort`], which says what order ties come in).
pub fn sort_documents(
    docs: Vec<Document>,
    spec: &Bson,
    sizing: &Sizing,
    limit: Option<usize>,
) -> R<Vec<Document>> {
    Ok(sort_documents_placed(docs, spec, sizing, limit)?.0)
}

/// [`sort_documents`], with each document's place in `docs`: what a caller
/// keeps beside each document follows it through a sort whose ties come in
/// the C++ library's order rather than in the order they came.
pub fn sort_documents_placed(
    docs: Vec<Document>,
    spec: &Bson,
    sizing: &Sizing,
    limit: Option<usize>,
) -> R<(Vec<Document>, Vec<usize>)> {
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
    let sorted = sort_items(keyed, &cmp, sizing, limit)?;
    Ok(sorted
        .into_iter()
        .map(|(_, d, pos)| (d, pos as usize))
        .unzip())
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
    fn a_spilled_sort_answers_the_keys_one_held_in_memory_does() {
        let whole = {
            let sizing = Sizing {
                sizes: None,
                source: SortSource::QueryLayer,
                fields: None,
                added: None,
                budget: memlimit::MAX_BYTES,
            };
            blocking_sort(items(300, 1000), &cmp, &sizing, None).unwrap()
        };
        memlimit::reset_report();
        let sizing = Sizing {
            sizes: None,
            source: SortSource::QueryLayer,
            fields: None,
            added: None,
            budget: 50_000,
        };
        let spilled = blocking_sort(items(300, 1000), &cmp, &sizing, None).unwrap();
        // The same keys in the same order, and the same documents; those
        // whose keys tie can come in another order, as mongod's do: each
        // run is sorted by `std::sort` on its own (`sort_held`).
        let keys =
            |out: &[(Vec<Bson>, Document)]| out.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&spilled), keys(&whole));
        let sorted_ids = |out: &[(Vec<Bson>, Document)]| {
            let mut ids = ids(out);
            ids.sort_unstable();
            ids
        };
        assert_eq!(sorted_ids(&spilled), sorted_ids(&whole));
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
    fn ties_come_in_the_order_the_builds_std_sort_leaves_them() {
        let sizing = Sizing {
            sizes: None,
            source: SortSource::Stored,
            fields: None,
            added: None,
            budget: memlimit::MAX_BYTES,
        };
        let out = blocking_sort(items(40, 1), &cmp, &sizing, None).unwrap();
        let expected: Vec<i32> = if crate::stl::LIBCXX {
            // libc++'s `std::sort` over the same 40 keys, by key alone
            // (Apple clang 21, 2026-11-13; RM-526).
            vec![
                0, 5, 25, 15, 20, 35, 30, 10, 3, 8, 13, 18, 23, 38, 33, 28, 1, 6, 11, 16, 21, 36,
                31, 26, 39, 24, 29, 19, 34, 14, 9, 4, 22, 27, 17, 7, 2, 32, 37, 12,
            ]
        } else {
            // libstdc++'s `std::sort` over the same 40 keys, by key alone
            // (g++ 15.2, 2026-11-10).
            vec![
                10, 20, 25, 15, 30, 35, 5, 0, 33, 8, 28, 13, 3, 38, 23, 18, 31, 1, 36, 26, 21, 6,
                16, 11, 39, 19, 4, 34, 9, 29, 14, 24, 12, 27, 32, 7, 17, 37, 2, 22,
            ]
        };
        assert_eq!(ids(&out), expected);
    }

    /// RM-312: a `find`'s `SORT` of 200 documents in runs of 20 equal keys
    /// leaves the equal ones as the build's `std::sort` does (libc++'s on
    /// macOS, RM-526). The first run under libstdc++ is the gap test's (`tests/backlog/
    /// test_rm_found_2026_10_25.py`), which Linux mongod answers live.
    #[test]
    fn std_sort_order_is_the_builds_std_sort() {
        let key = |i: usize| format!("{}{}", if i.is_multiple_of(2) { "a" } else { "A" }, i % 5);
        let ids: Vec<usize> = (1..=200).collect();
        let order = std_sort_order(ids.len(), &|a, b| key(ids[a]).cmp(&key(ids[b])));
        let first: Vec<usize> = order[..20].iter().map(|&at| ids[at]).collect();
        let expected: Vec<usize> = if crate::stl::LIBCXX {
            // libc++'s, as macOS mongod 8.2.11 leaves them (RM-526).
            vec![
                25, 105, 95, 115, 5, 195, 85, 125, 75, 135, 65, 145, 55, 155, 15, 185, 45, 165, 35,
                175,
            ]
        } else {
            vec![
                165, 25, 155, 55, 65, 145, 185, 75, 135, 15, 45, 85, 175, 125, 95, 195, 5, 115,
                105, 35,
            ]
        };
        assert_eq!(first, expected);
        // Every position once, sorted by key.
        let mut seen = order.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0..200).collect::<Vec<_>>());
        assert!(order.windows(2).all(|w| key(ids[w[0]]) <= key(ids[w[1]])));
    }

    #[test]
    fn each_document_keeps_its_place_through_a_spilled_sort() {
        let docs: Vec<Document> = items(300, 1000).into_iter().map(|(_, d)| d).collect();
        let sizing = Sizing {
            sizes: None,
            source: SortSource::Stored,
            fields: None,
            added: None,
            budget: 50_000,
        };
        for limit in [None, Some(50)] {
            memlimit::reset_report();
            let (sorted, placed) =
                sort_documents_placed(docs.clone(), &Bson::Document(doc! {"k": 1}), &sizing, limit)
                    .unwrap();
            assert!(memlimit::report().sort_spills > 1, "{limit:?}");
            assert_eq!(sorted.len(), placed.len());
            for (d, at) in sorted.iter().zip(&placed) {
                assert_eq!(d, &docs[*at], "{limit:?}");
            }
        }
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
            sizes: None,
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
            sizes: None,
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
        let pattern = doc! { "y": 1, "x": 1 };
        let members = |n: i32| -> Vec<IndexKeyMember> {
            (0..n)
                .map(|i| IndexKeyMember {
                    sort_key: vec![Bson::Int32(i)],
                    key_pattern: &pattern,
                    index_key: vec![Bson::Int32(i % 3), Bson::Int32(i)],
                    index_id: 0,
                    rid: i64::from(i) + 1,
                })
                .collect()
        };
        let counted = |n: i32, budget: usize, limit: Option<usize>| {
            let _off = memlimit::allow_disk_use(false);
            let (ran, report) =
                memlimit::captured(|| sort_index_keys(members(n), &[false], budget, limit));
            ran.map(|()| report.sort_total_data_size)
                .map_err(|f| f.as_mongo().map(|(code, _)| code))
        };
        // Twelve members need the budget less one 136-byte file iterator;
        // a limit of three keeps three, a limit of one counts no bytes.
        assert_eq!(counted(12, 916, None), Ok(780));
        assert_eq!(counted(12, 915, None), Err(Some(292)));
        assert_eq!(counted(12, 331, Some(3)), Ok(195));
        assert_eq!(counted(12, 330, Some(3)), Err(Some(292)));
        assert_eq!(counted(12, 1, Some(1)), Ok(0));
    }

    /// `n` documents `{_id: i, x: i, y: i % 3}` as a scan of `{y: 1, x: 1}`
    /// returns them, projected by `PROJECTION_COVERED` under `spec`, each
    /// keyed by `sort` (fields of the key, ascending).
    fn covered(n: i32, spec: &Document, sort: &[&str]) -> Vec<OwnedObjMember> {
        let pattern = doc! { "y": 1, "x": 1 };
        (0..3)
            .flat_map(|y| (0..n).filter(move |x| x % 3 == y).map(move |x| (y, x)))
            .map(|(y, x)| {
                let key = doc! { "y": y, "x": x };
                OwnedObjMember {
                    sort_key: sort.iter().map(|f| key.get(*f).unwrap().clone()).collect(),
                    doc: crate::projection::covered_document(
                        &pattern,
                        &[Bson::Int32(y), Bson::Int32(x)],
                        spec,
                    ),
                }
            })
            .collect()
    }

    #[test]
    fn a_sort_over_a_covered_projection_counts_its_documents() {
        // mongod 8.2.11, `find({y: {$gte: 0}}, <spec>).sort(<sort>)` over
        // `{y: 1, x: 1}` and twelve documents (`SORT <- PROJECTION_COVERED
        // <- IXSCAN`): each member its sort key and its document's
        // `getApproximateSize` -- 16 + 104 + 12 for `{x}` (RM-473).
        let counted = |spec: Document, sort: &[&str], descending: &[bool]| {
            let (ran, report) = memlimit::captured(|| {
                sort_owned_objs(covered(12, &spec, sort), descending, 1 << 20, None)
            });
            ran.unwrap();
            report.sort_total_data_size
        };
        assert_eq!(counted(doc! {"_id": 0, "x": 1}, &["x"], &[false]), 1584);
        assert_eq!(
            counted(doc! {"_id": 0, "x": 1, "y": 1}, &["x"], &[false]),
            1668
        );
        assert_eq!(
            counted(doc! {"_id": 0, "x": 1, "y": 1}, &["x", "y"], &[false, true]),
            2532
        );
    }

    #[test]
    fn a_sort_over_a_covered_projection_past_its_budget_spills_its_documents() {
        // mongod 8.2.11, the same sort over 2000 documents with
        // `allowDiskUse` under a 100000-byte budget: each member spilled as
        // an OWNED_OBJ working-set member, 27 bytes with its key for `{x}`,
        // 34 for `{x, y}` (RM-473). What the runs compress to is this
        // server's: mongod's members carry the snapshot ids it draws from a
        // process-wide counter (36220 and 42497 there, RM-556).
        let spilled = |spec: Document| {
            let _on = memlimit::allow_disk_use(true);
            let (ran, report) = memlimit::captured(|| {
                sort_owned_objs(covered(2000, &spec, &["x"]), &[false], 100_000, None)
            });
            ran.unwrap();
            (
                report.sort_total_data_size,
                report.sort_spills,
                report.sort_spilled_records,
                report.sort_spilled_bytes,
                report.sort_spilled_data_storage_size,
            )
        };
        assert_eq!(
            spilled(doc! {"_id": 0, "x": 1}),
            (264_000, 5, 4000, 108_000, 34_936)
        );
        let (total, spills, records, bytes, _) = spilled(doc! {"_id": 0, "x": 1, "y": 1});
        assert_eq!((total, spills, records, bytes), (278_000, 6, 4000, 136_000));
    }

    #[test]
    fn an_owned_obj_member_reads_back_as_it_was_written() {
        let mut out = Vec::new();
        spill_file::write_owned_obj_member(&doc! {"x": 7, "s": "ab"}, 0, &mut out).unwrap();
        let mut at = spill_file::Cursor::new(&out);
        assert_eq!(spill_file::read_member(&mut at).unwrap(), out);
    }

    #[test]
    fn a_sort_of_index_keys_past_its_budget_spills_its_members() {
        // mongod 8.2.11, the same sort over 2000 documents with
        // `allowDiskUse` under a 100000-byte budget: two spills of every
        // member, each written as `WorkingSetMember::serialize` writes it
        // -- 65 bytes, as it counts -- and 130000 bytes in all (RM-475).
        let pattern = doc! { "y": 1, "x": 1 };
        let members: Vec<IndexKeyMember> = (0..2000)
            .map(|i| IndexKeyMember {
                sort_key: vec![Bson::Int32(i)],
                key_pattern: &pattern,
                index_key: vec![Bson::Int32(i % 3), Bson::Int32(i)],
                index_id: 0,
                rid: i64::from(i) + 1,
            })
            .collect();
        let _on = memlimit::allow_disk_use(true);
        let (ran, report) =
            memlimit::captured(|| sort_index_keys(members, &[false], 100_000, None));
        ran.unwrap();
        assert!(report.used_disk);
        assert_eq!(report.sort_total_data_size, 130_000);
        assert_eq!(report.sort_spills, 2);
        assert_eq!(report.sort_spilled_records, 2000);
        assert_eq!(report.sort_spilled_bytes, 130_000);
    }

    #[test]
    fn an_index_key_member_reads_back_as_it_was_written() {
        let pattern = doc! { "y": 1, "x": -1 };
        let mut out = Vec::new();
        spill_file::write_index_key_member(
            &pattern,
            &[Bson::String("ab".into()), Bson::Null],
            1,
            7,
            42,
            &mut out,
        )
        .unwrap();
        // State, count, the pattern (19), the key (4 + 9 + 2 + 1), the
        // index id, the snapshot, the RecordId's format and value, no
        // metadata.
        assert_eq!(out.len(), 1 + 1 + 19 + 16 + 4 + 8 + 1 + 8 + 1);
        let mut at = spill_file::Cursor::new(&out);
        assert_eq!(spill_file::read_index_key_member(&mut at).unwrap(), out);
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
                sizes: None,
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
    fn an_sbe_sort_spills_rows_of_values() {
        // mongod 8.2.11, `[{$sort: {n: 1}}, {$group: {_id: "$k", f: {$first:
        // "$s"}}}]` over 300 documents of {_id, k, s: 1000 x's, n} under
        // 50000 bytes: 13 runs, 900 records, 927900 bytes -- 1031 a record:
        // the key's row (8 + 1 + 4) and `k`'s and `s`'s (8 + 1 + 4 + 1 + 4 +
        // 1000) (RM-506). The row holds them by name; each document comes
        // back with its fields in its own order (`s` before `k` here).
        let fields: BTreeSet<String> = ["k", "s"].iter().map(|f| f.to_string()).collect();
        let docs = (0..300)
            .map(|i| {
                let d = doc! { "_id": i, "s": "x".repeat(1000), "k": i % 7, "n": i };
                (vec![Bson::Int32(i)], d)
            })
            .collect::<Vec<_>>();
        let sizing = Sizing {
            sizes: None,
            source: SortSource::Sbe,
            fields: Some(&fields),
            added: None,
            budget: 50_000,
        };
        let (out, report) = memlimit::captured(|| blocking_sort(docs, &cmp, &sizing, None));
        let out = out.unwrap();
        assert_eq!(
            (
                report.sort_spills,
                report.sort_spilled_records,
                report.sort_spilled_bytes
            ),
            (13, 900, 927_900)
        );
        for (i, (k, d)) in out.iter().enumerate() {
            assert_eq!(k, &vec![Bson::Int32(i as i32)]);
            assert_eq!(d, &doc! { "s": "x".repeat(1000), "k": i as i32 % 7 });
            assert_eq!(d.keys().collect::<Vec<_>>(), ["s", "k"]);
        }
    }

    #[test]
    fn a_sort_of_one_counts_no_data_sorted() {
        // mongod 8.2.11, `find({z: 0}).sort({y: 1})` over six documents:
        // `totalDataSizeSorted` 0 for a limit of 1, 134 for 2 (RM-374).
        let sizing = Sizing {
            sizes: None,
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
    fn a_sort_with_a_limit_spills_as_mongods_top_k_sorter_does() {
        // mongod 8.2.11, `[{$sort: {n: way}}, {$limit: k}, {$addFields: ...}]`
        // over 300 documents of {_id, k, s: 1000 x's, n} under a 50000-byte
        // budget, `n` read in order (`i`) or not (`37 i mod 300`): the
        // profile's `sortSpills`, `sortSpilledRecords` and
        // `sortTotalDataSizeBytes` (RM-399).
        let sizing = Sizing {
            sizes: None,
            source: SortSource::QueryLayer,
            fields: None,
            added: None,
            budget: 50_000,
        };
        let counted = |shuffled: bool, way: i32, k: usize| {
            let docs = (0..300)
                .map(|i| {
                    let n = if shuffled { (i * 37) % 300 } else { i };
                    let d = doc! { "_id": i, "k": i % 7, "s": "x".repeat(1000), "n": n };
                    (vec![Bson::Int32(n)], d)
                })
                .collect();
            let by = |a: &[Bson], b: &[Bson]| {
                let c = crate::order::cmp(&a[0], &b[0]);
                if way < 0 {
                    c.reverse()
                } else {
                    c
                }
            };
            let (out, report) = memlimit::captured(|| blocking_sort(docs, &by, &sizing, Some(k)));
            let out = out.unwrap();
            // It answers the first k, whatever it spilled.
            let mut want: Vec<i32> = (0..300).collect();
            if way < 0 {
                want.reverse();
            }
            let got: Vec<i32> = out.iter().map(|(_, d)| d.get_i32("n").unwrap()).collect();
            assert_eq!(got, want[..k], "{shuffled} {way} {k}");
            (
                report.sort_spills,
                report.sort_spilled_records,
                report.sort_total_data_size,
            )
        };
        // Each document better than the last: no cutoff turns any away, and
        // the merges stop each run at the limit.
        assert_eq!(counted(false, -1, 50), (13, 592, 320_400));
        assert_eq!(counted(false, -1, 100), (13, 800, 320_400));
        assert_eq!(counted(false, -1, 200), (13, 900, 320_400));
        // Each worse: the cutoff turns the rest away after two runs.
        assert_eq!(counted(false, 1, 50), (2, 86, 91_848));
        assert_eq!(counted(false, 1, 200), (10, 645, 229_620));
        // Out of order, the median of a run becomes the cutoff too.
        assert_eq!(counted(true, 1, 50), (10, 439, 225_348));
        assert_eq!(counted(true, -1, 50), (11, 460, 232_824));
        assert_eq!(counted(true, -1, 100), (13, 711, 278_748));
        assert_eq!(counted(true, 1, 200), (13, 897, 319_332));
        // Held in memory it counts the k it added first, the documents that
        // replaced them in the heap not (mongod: 32040 for a limit of 30).
        assert_eq!(counted(false, -1, 30), (0, 0, 32_040));
        // Past the budget without disk use it is refused.
        let _off = memlimit::allow_disk_use(false);
        let docs = items(300, 1000);
        let e = blocking_sort(docs, &cmp, &sizing, Some(50)).unwrap_err();
        assert_eq!(e.as_mongo().map(|(code, _)| code), Some(292));
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
