//! An ORDER BY for a streamed portal (`portal_stream`) with bounded memory.
//!
//! The rows are read in scan order and gathered into runs of at most
//! `RUN_BYTES`; each run is sorted (`compare_rows`, the materialised path's
//! comparison) and, when more than one is needed, written to an anonymous
//! temporary file. The runs are then merged, ties going to the earlier run,
//! so the result is the stable sort of the scan -- what the materialised
//! path gives. Memory is one run plus one buffered reader per run.
//!
//! With a LIMIT the server keeps only the first `OFFSET + LIMIT` rows seen
//! so far in order (a top-k), and nothing is written to disk.

use super::*;
use std::io::{BufReader, BufWriter, Read, Seek, Write};

/// The bytes of rows sorted in memory before a run is written out.
const RUN_BYTES: usize = 16 << 20;
/// A LIMIT whose rows are kept in memory however large the table.
const TOP_K_MAX: usize = 100_000;

/// Sort the rows `scan` hands over by `order` and pass them to `emit` in
/// batches of `batch`, after `skip` rows and at most `limit`. `scan` calls
/// its sink with each batch of stored rows and stops when the sink answers
/// `false`; `emit` answers `false` to stop. Errors are messages.
pub(crate) fn sorted_rows(
    scan: impl FnOnce(&mut dyn FnMut(Vec<Vec<u8>>) -> bool) -> Result<(), String>,
    order: &[OrderKey],
    skip: usize,
    limit: Option<usize>,
    batch: usize,
    mut emit: impl FnMut(Vec<Document>) -> bool,
) -> Result<(), String> {
    let mut sorted = sort_runs(scan, order, skip, limit, batch)?;
    loop {
        let docs = sorted.next_batch()?;
        if docs.is_empty() || !emit(docs) {
            return Ok(());
        }
    }
}

/// Read every row `scan` hands over into sorted runs (see `sorted_rows`)
/// and return the merge, which hands the rows out a batch at a time.
pub(crate) fn sort_runs(
    scan: impl FnOnce(&mut dyn FnMut(Vec<Vec<u8>>) -> bool) -> Result<(), String>,
    order: &[OrderKey],
    skip: usize,
    limit: Option<usize>,
    batch: usize,
) -> Result<Sorted, String> {
    let cap = limit.map(|l| l.saturating_add(skip));
    let top_k = cap.filter(|c| *c <= TOP_K_MAX);
    let mut chunk: Vec<Document> = Vec::new();
    let mut bytes = 0usize;
    let mut runs: Vec<std::fs::File> = Vec::new();
    let mut failed: Option<String> = None;
    {
        let mut sink = |blobs: Vec<Vec<u8>>| -> bool {
            for blob in blobs {
                bytes += blob.len();
                match decode_doc(&blob) {
                    Ok(d) => chunk.push(d),
                    Err(e) => {
                        failed = Some(format!("could not decode a row: {e}"));
                        return false;
                    }
                }
            }
            if let Some(k) = top_k {
                // Keep the first k in order: the earlier rows stay ahead of
                // equal later ones, as a stable sort keeps them.
                if chunk.len() >= k.max(batch) * 2 {
                    chunk.sort_by(|a, b| compare_rows(a, b, order));
                    chunk.truncate(k);
                }
                return true;
            }
            if bytes >= RUN_BYTES {
                chunk.sort_by(|a, b| compare_rows(a, b, order));
                match spill(std::mem::take(&mut chunk)) {
                    Ok(f) => runs.push(f),
                    Err(e) => {
                        failed = Some(e);
                        return false;
                    }
                }
                bytes = 0;
            }
            true
        };
        scan(&mut sink)?;
    }
    if let Some(e) = failed {
        return Err(e);
    }
    chunk.sort_by(|a, b| compare_rows(a, b, order));
    if let Some(k) = top_k {
        chunk.truncate(k);
    }
    let order: Arc<[OrderKey]> = order.to_vec().into();
    let mut sorted = Sorted {
        order: order.clone(),
        skip,
        left: limit,
        batch,
        mem: Vec::new().into_iter(),
        readers: Vec::new(),
        heap: std::collections::BinaryHeap::new(),
    };
    if runs.is_empty() {
        sorted.mem = chunk.into_iter();
        return Ok(sorted);
    }
    if !chunk.is_empty() {
        runs.push(spill(chunk)?);
    }
    for mut f in runs {
        f.rewind()
            .map_err(|e| format!("could not read a sort run: {e}"))?;
        sorted.readers.push(BufReader::with_capacity(64 << 10, f));
    }
    for run in 0..sorted.readers.len() {
        if let Some(doc) = read_one(&mut sorted.readers[run])? {
            sorted.heap.push(Head {
                doc,
                run,
                order: order.clone(),
            });
        }
    }
    Ok(sorted)
}

/// Sorted rows going out: the merge of the runs (or the one run kept in
/// memory), then OFFSET, LIMIT and batching.
pub(crate) struct Sorted {
    order: Arc<[OrderKey]>,
    skip: usize,
    left: Option<usize>,
    batch: usize,
    mem: std::vec::IntoIter<Document>,
    readers: Vec<BufReader<std::fs::File>>,
    heap: std::collections::BinaryHeap<Head>,
}

impl Sorted {
    fn next_row(&mut self) -> Result<Option<Document>, String> {
        if self.readers.is_empty() {
            return Ok(self.mem.next());
        }
        let Some(Head { doc, run, .. }) = self.heap.pop() else {
            return Ok(None);
        };
        if let Some(next) = read_one(&mut self.readers[run])? {
            self.heap.push(Head {
                doc: next,
                run,
                order: self.order.clone(),
            });
        }
        Ok(Some(doc))
    }

    /// The next batch of rows; empty once there are no more.
    pub(crate) fn next_batch(&mut self) -> Result<Vec<Document>, String> {
        let mut docs = Vec::new();
        while docs.len() < self.batch && self.left != Some(0) {
            let Some(d) = self.next_row()? else { break };
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            docs.push(d);
            if let Some(n) = self.left.as_mut() {
                *n -= 1;
            }
        }
        Ok(docs)
    }
}

/// A run's next row in the merge, ordered so that the max-heap pops the
/// smallest row, and of equal rows the earliest run's.
struct Head {
    doc: Document,
    run: usize,
    order: Arc<[OrderKey]>,
}

impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Head {}
impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Head {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_rows(&self.doc, &other.doc, &self.order)
            .then(self.run.cmp(&other.run))
            .reverse()
    }
}

/// A sorted run written to an anonymous temporary file (removed when the
/// file is closed), each row its length and its BSON.
fn spill(rows: Vec<Document>) -> Result<std::fs::File, String> {
    let file = tempfile::tempfile().map_err(|e| format!("could not create a sort run: {e}"))?;
    let mut w = BufWriter::with_capacity(64 << 10, file);
    for d in rows {
        let bytes = bson::to_vec(&d).map_err(|e| format!("could not write a sort run: {e}"))?;
        let len = u32::try_from(bytes.len()).map_err(|_| "a row is too large".to_string())?;
        w.write_all(&len.to_le_bytes())
            .and_then(|()| w.write_all(&bytes))
            .map_err(|e| format!("could not write a sort run: {e}"))?;
    }
    w.into_inner()
        .map_err(|e| format!("could not write a sort run: {e}"))
}

fn read_one(r: &mut BufReader<std::fs::File>) -> Result<Option<Document>, String> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(format!("could not read a sort run: {e}")),
    }
    let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
    r.read_exact(&mut buf)
        .map_err(|e| format!("could not read a sort run: {e}"))?;
    decode_doc(&buf)
        .map(Some)
        .map_err(|e| format!("could not read a sort run: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| {
                let d = bson::doc! {"k": ((i * 7919) % 101) as i32, "i": i as i32, "pad": "x".repeat(200)};
                bson::to_vec(&d).unwrap()
            })
            .collect()
    }

    fn run(n: usize, skip: usize, limit: Option<usize>) -> Vec<(i32, i32)> {
        let order = vec![OrderKey {
            field: "k".into(),
            ascending: false,
            nulls: Nulls::First,
            expr: None,
        }];
        let data = rows(n);
        let mut got = Vec::new();
        sorted_rows(
            |sink| {
                for c in data.chunks(256) {
                    if !sink(c.to_vec()) {
                        break;
                    }
                }
                Ok(())
            },
            &order,
            skip,
            limit,
            100,
            |batch| {
                got.extend(
                    batch
                        .iter()
                        .map(|d| (d.get_i32("k").unwrap(), d.get_i32("i").unwrap())),
                );
                true
            },
        )
        .unwrap();
        got
    }

    fn expected(n: usize, skip: usize, limit: Option<usize>) -> Vec<(i32, i32)> {
        let mut all: Vec<(i32, i32)> = (0..n)
            .map(|i| (((i * 7919) % 101) as i32, i as i32))
            .collect();
        // Stable: descending key, scan order among equals.
        all.sort_by_key(|a| std::cmp::Reverse(a.0));
        all.into_iter()
            .skip(skip)
            .take(limit.unwrap_or(usize::MAX))
            .collect()
    }

    #[test]
    fn in_memory_spilled_and_top_k_sorts_agree_with_a_stable_sort() {
        // 120,000 rows of ~230 bytes are ~27 MB: two runs on disk.
        for (n, skip, limit) in [
            (1000, 0, None),
            (1000, 5, Some(17)),
            (120_000, 0, None),
            (120_000, 1000, Some(5000)),
            (120_000, 3, Some(200_000)),
        ] {
            assert_eq!(
                run(n, skip, limit),
                expected(n, skip, limit),
                "{n} {skip} {limit:?}"
            );
        }
    }
}
