//! A streamed join whose right side is past the bound (batch 66).
//!
//! `stream_join` holds every right side in memory, within
//! `join_inner_bytes`; past it the statement used to fall back to the
//! materialised path, which holds EVERY side. Here such a right side is a
//! grace hash join instead: its rows are written to `PARTITIONS` temporary
//! files by the hash of their join key, the left side's rows to the same
//! partitions by theirs, and each partition is then joined on its own --
//! its right rows in memory, its left rows read back a batch at a time -- by
//! the code every other join uses (`join_rows_core`). A partition still past
//! the bound is partitioned again under another hash, `MAX_DEPTH` times.
//!
//! The rows, and their ORDER, are the materialised path's. Each left row is
//! numbered as it arrives and each right row as it is read, the numbers ride
//! through the join as two hidden keys, and the joined rows are sorted on
//! them (`external_sort`, spilled past its run size) before they are handed
//! on: each left row in arrival order with its matching right rows in theirs,
//! then the right rows nothing matched (their left number is NULL, which
//! sorts last).
//!
//! Only a join with column equalities in its ON (`equi`) partitions. A right
//! row whose key the hash does not model (`join_hash_key` is `None` for a
//! non-NULL value) would have to meet every left row, so the join declines
//! before any row is handed on. A LEFT row like that joins every partition
//! (`every`); for a LEFT / FULL join it is NULL-extended only when no
//! partition matched it.

use super::*;
use std::hash::{Hash, Hasher};
use std::io::{BufReader, BufWriter, Read, Seek, Write};

/// The files a side is split into, at each level.
const PARTITIONS: usize = 32;
/// How many times a partition past the bound is split again.
const MAX_DEPTH: u32 = 3;
/// The hidden keys numbering the left and the right rows.
pub(crate) const LSEQ: &str = "\u{1f}grace_l";
pub(crate) const RSEQ: &str = "\u{1f}grace_r";
const BATCH: usize = 256;

/// Rows written to an anonymous temporary file (removed when closed), each
/// its length and its BSON; nothing is created until the first row.
#[derive(Default)]
pub(crate) struct Spool {
    w: Option<BufWriter<std::fs::File>>,
    pub(crate) bytes: usize,
    pub(crate) rows: usize,
}

impl Spool {
    pub(crate) fn push(&mut self, d: &Document) -> PgWireResult<()> {
        let bytes = bson::to_vec(d).map_err(|e| spill_err(e.to_string()))?;
        let len = u32::try_from(bytes.len()).map_err(|_| spill_err("a row is too large"))?;
        if self.w.is_none() {
            let f = tempfile::tempfile().map_err(|e| spill_err(e.to_string()))?;
            self.w = Some(BufWriter::with_capacity(64 << 10, f));
        }
        let w = self.w.as_mut().expect("created above");
        w.write_all(&len.to_le_bytes())
            .and_then(|()| w.write_all(&bytes))
            .map_err(|e| spill_err(e.to_string()))?;
        self.bytes += bytes.len();
        self.rows += 1;
        Ok(())
    }

    /// Read the rows back from the start, keeping the spool to read again
    /// (nothing is written after the first read).
    pub(crate) fn reread(&mut self) -> PgWireResult<SpoolReader> {
        let Some(w) = self.w.as_mut() else {
            return Ok(SpoolReader { r: None });
        };
        w.flush().map_err(|e| spill_err(e.to_string()))?;
        let mut f = w
            .get_ref()
            .try_clone()
            .map_err(|e| spill_err(e.to_string()))?;
        f.rewind().map_err(|e| spill_err(e.to_string()))?;
        Ok(SpoolReader {
            r: Some(BufReader::with_capacity(64 << 10, f)),
        })
    }

    /// Read the rows back, in the order written.
    pub(crate) fn reader(self) -> PgWireResult<SpoolReader> {
        let r = match self.w {
            None => None,
            Some(w) => {
                let mut f = w.into_inner().map_err(|e| spill_err(e.to_string()))?;
                f.rewind().map_err(|e| spill_err(e.to_string()))?;
                Some(BufReader::with_capacity(64 << 10, f))
            }
        };
        Ok(SpoolReader { r })
    }
}

pub(crate) struct SpoolReader {
    r: Option<BufReader<std::fs::File>>,
}

impl SpoolReader {
    pub(crate) fn next_row(&mut self) -> PgWireResult<Option<Document>> {
        let Some(r) = self.r.as_mut() else {
            return Ok(None);
        };
        let mut len = [0u8; 4];
        match r.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(spill_err(e.to_string())),
        }
        let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
        r.read_exact(&mut buf)
            .map_err(|e| spill_err(e.to_string()))?;
        decode_doc(&buf)
            .map(Some)
            .map_err(|e| spill_err(e.to_string()))
    }

    /// Up to `n` rows; empty at the end.
    pub(crate) fn next_batch(&mut self, n: usize) -> PgWireResult<Vec<Document>> {
        let mut out = Vec::with_capacity(n.min(BATCH));
        while out.len() < n {
            match self.next_row()? {
                Some(d) => out.push(d),
                None => break,
            }
        }
        Ok(out)
    }
}

/// `(left number, right number, offset, length)`; a NULL number sorts last.
type IndexEntry = (i64, i64, u64, u32);

fn seq_key(d: &Document, f: &str) -> i64 {
    match d.get(f) {
        Some(Bson::Int64(n)) => *n,
        _ => i64::MAX,
    }
}

fn ops_budget(right: &GraceRight) -> usize {
    right.budget
}

/// Joined rows spooled in arrival order, read back in `(LSEQ, RSEQ)` order.
#[derive(Default)]
struct OrderSpool {
    w: Option<BufWriter<std::fs::File>>,
    at: u64,
    index: Vec<IndexEntry>,
}

impl OrderSpool {
    fn push(&mut self, d: &Document) -> PgWireResult<()> {
        let bytes = bson::to_vec(d).map_err(|e| spill_err(e.to_string()))?;
        let len = u32::try_from(bytes.len()).map_err(|_| spill_err("a row is too large"))?;
        if self.w.is_none() {
            let f = tempfile::tempfile().map_err(|e| spill_err(e.to_string()))?;
            self.w = Some(BufWriter::with_capacity(64 << 10, f));
        }
        let w = self.w.as_mut().expect("created above");
        w.write_all(&bytes).map_err(|e| spill_err(e.to_string()))?;
        self.index
            .push((seq_key(d, LSEQ), seq_key(d, RSEQ), self.at, len));
        self.at += u64::from(len);
        Ok(())
    }

    /// Every row so far, in arrival order (for the external sort).
    fn drain(&mut self) -> PgWireResult<Vec<Document>> {
        let index = std::mem::take(&mut self.index);
        let Some(w) = self.w.take() else {
            return Ok(Vec::new());
        };
        let mut r = OrderedRows::open(w, index)?;
        let out = r.next_batch(usize::MAX)?;
        self.at = 0;
        Ok(out)
    }

    fn into_sorted(mut self) -> PgWireResult<OrderedRows> {
        self.index.sort_by_key(|e| (e.0, e.1));
        match self.w.take() {
            Some(w) => OrderedRows::open(w, self.index),
            None => Ok(OrderedRows {
                f: None,
                index: Vec::new(),
                next: 0,
                pos: 0,
            }),
        }
    }
}

struct OrderedRows {
    f: Option<BufReader<std::fs::File>>,
    index: Vec<IndexEntry>,
    next: usize,
    /// Where the reader stands, to skip a seek for a row that follows.
    pos: u64,
}

impl OrderedRows {
    fn open(w: BufWriter<std::fs::File>, index: Vec<IndexEntry>) -> PgWireResult<Self> {
        let mut f = w.into_inner().map_err(|e| spill_err(e.to_string()))?;
        f.rewind().map_err(|e| spill_err(e.to_string()))?;
        Ok(OrderedRows {
            f: Some(BufReader::with_capacity(64 << 10, f)),
            index,
            next: 0,
            pos: 0,
        })
    }

    fn next_batch(&mut self, n: usize) -> PgWireResult<Vec<Document>> {
        let mut out = Vec::new();
        let Some(f) = self.f.as_mut() else {
            return Ok(out);
        };
        while out.len() < n && self.next < self.index.len() {
            let (_, _, at, len) = self.index[self.next];
            self.next += 1;
            if at != self.pos {
                f.seek(std::io::SeekFrom::Start(at))
                    .map_err(|e| spill_err(e.to_string()))?;
            }
            let mut buf = vec![0u8; len as usize];
            f.read_exact(&mut buf)
                .map_err(|e| spill_err(e.to_string()))?;
            self.pos = at + u64::from(len);
            out.push(decode_doc(&buf).map_err(|e| spill_err(e.to_string()))?);
        }
        Ok(out)
    }
}

fn spill_err(e: impl std::fmt::Display) -> PgWireError {
    PgHandler::user_error("XX000", format!("could not spill a join partition: {e}"))
}

fn partition_of(key: &[String], depth: u32) -> usize {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    depth.hash(&mut h);
    key.hash(&mut h);
    (h.finish() % PARTITIONS as u64) as usize
}

/// A right side split into partitions while it was read.
pub(crate) struct GraceRight {
    parts: Vec<Spool>,
    /// Rows with a NULL key: they match nothing, but a RIGHT / FULL join
    /// still returns each.
    null_keys: Spool,
    next_seq: i64,
    /// The bytes one partition may hold in memory.
    budget: usize,
}

impl GraceRight {
    pub(crate) fn new(budget: usize) -> Self {
        GraceRight {
            parts: (0..PARTITIONS).map(|_| Spool::default()).collect(),
            null_keys: Spool::default(),
            next_seq: 0,
            budget,
        }
    }

    /// Add the next right rows. `false`: a row's key is one the hash does
    /// not model, so this join cannot be partitioned.
    pub(crate) fn add(
        &mut self,
        rows: Vec<Document>,
        right_key: &dyn Fn(&Document) -> KeyClass,
    ) -> PgWireResult<bool> {
        for mut d in rows {
            d.insert(RSEQ, Bson::Int64(self.next_seq));
            self.next_seq += 1;
            match right_key(&d) {
                KeyClass::Key(k) => self.parts[partition_of(&k, 0)].push(&d)?,
                KeyClass::Null => self.null_keys.push(&d)?,
                KeyClass::Other => return Ok(false),
            }
        }
        Ok(true)
    }
}

/// How a row's join key partitions it.
pub(crate) enum KeyClass {
    Key(Vec<String>),
    /// NULL: it matches nothing.
    Null,
    /// A value the hash does not model: it is compared with every row of
    /// the other side.
    Other,
}

type Feed<'f> = &'f mut dyn FnMut(Sink<'_>) -> PgWireResult<()>;

/// What a grace join needs of the join it runs: the two sides' keys, the
/// in-memory join of one partition (`join(right rows, left feed, emit,
/// as_left)`, where `as_left` asks for a LEFT join's NULL extension
/// whatever the join's own kind) and which sides an outer join keeps.
pub(crate) struct GraceOps<'a> {
    pub(crate) left_key: &'a dyn Fn(&Document) -> KeyClass,
    pub(crate) right_key: &'a dyn Fn(&Document) -> KeyClass,
    #[allow(clippy::type_complexity)]
    pub(crate) join: &'a dyn Fn(Vec<Document>, Feed<'_>, Sink<'_>, bool) -> PgWireResult<()>,
    pub(crate) outer_left: bool,
    pub(crate) outer_right: bool,
}

/// The parts of a `JoinNode::Join` its partition joins need.
pub(crate) struct JoinSpec<'a> {
    pub(crate) kind: secantus_pgplan::joins::JoinKind,
    pub(crate) on: Option<&'a secantus_pgplan::ColumnExpr>,
    pub(crate) equi: &'a [(String, String)],
    pub(crate) merged: &'a [(String, String, String)],
    /// The sides' keys, each with its hidden number appended.
    pub(crate) left_keys: Vec<String>,
    pub(crate) right_keys: Vec<String>,
}

/// A general join's key class over `keys` (`join_hash_key`).
pub(crate) fn general_key(d: &Document, keys: &[&String]) -> KeyClass {
    match join_hash_key(d, keys) {
        Some(k) => KeyClass::Key(k),
        None if keys
            .iter()
            .any(|k| matches!(d.get(k.as_str()), Some(Bson::Null) | None)) =>
        {
            KeyClass::Null
        }
        None => KeyClass::Other,
    }
}

type Sink<'s> = &'s mut dyn FnMut(Vec<Document>) -> PgWireResult<bool>;

impl PgHandler {
    /// Join `left` (fed by `feed_left`) to the partitioned `right`, handing
    /// the joined rows to `sink` in the materialised path's order.
    pub(crate) fn grace_join(
        &self,
        ops: &GraceOps<'_>,
        right: GraceRight,
        feed_left: &mut dyn FnMut(Sink<'_>) -> PgWireResult<()>,
        sink: Sink<'_>,
    ) -> PgWireResult<()> {
        let outer_left = ops.outer_left;
        let order = [LSEQ, RSEQ].map(|f| OrderKey {
            field: f.to_string(),
            ascending: true,
            nulls: Nulls::Last,
            expr: None,
        });
        // The joined rows go to a spool, each indexed by its two numbers
        // and its place in the file; the index alone is sorted, and the rows
        // read back in its order (batch 67: an external sort wrote and
        // merged the whole rows again). Past `budget` of index the rows
        // move to the external sort after all.
        let mut out = crate::external_sort::RunBuilder::new(&order, None, BATCH);
        let mut ordered = OrderSpool::default();
        let index_cap = (ops_budget(&right) / std::mem::size_of::<IndexEntry>()).max(1 << 16);
        let mut sorting = false;
        let mut push_out = |docs: Vec<Document>| -> PgWireResult<bool> {
            if !sorting && ordered.index.len() + docs.len() > index_cap {
                sorting = true;
                for d in ordered.drain()? {
                    let n = stream_join::doc_bytes(&d);
                    out.push(d, n)
                        .map_err(|e| Self::user_error("XX000", format!("could not sort: {e}")))?;
                }
            }
            for d in docs {
                if sorting {
                    let n = stream_join::doc_bytes(&d);
                    out.push(d, n)
                        .map_err(|e| Self::user_error("XX000", format!("could not sort: {e}")))?;
                } else {
                    ordered.push(&d)?;
                }
            }
            Ok(true)
        };
        // The left side, numbered and partitioned. A NULL key matches
        // nothing: a LEFT / FULL join NULL-extends it at once.
        let mut lparts: Vec<Spool> = (0..PARTITIONS).map(|_| Spool::default()).collect();
        let mut every = Spool::default();
        let mut every_seqs: HashSet<i64> = HashSet::new();
        let mut lseq = 0i64;
        feed_left(&mut |docs| {
            self.check_cancel()?;
            let mut nulls = Vec::new();
            for mut d in docs {
                d.insert(LSEQ, Bson::Int64(lseq));
                match (ops.left_key)(&d) {
                    KeyClass::Key(k) => lparts[partition_of(&k, 0)].push(&d)?,
                    KeyClass::Null => {
                        if outer_left {
                            nulls.push(d);
                        }
                    }
                    KeyClass::Other => {
                        every_seqs.insert(lseq);
                        every.push(&d)?;
                    }
                }
                lseq += 1;
            }
            if !nulls.is_empty() {
                (ops.join)(
                    Vec::new(),
                    &mut |s| s(std::mem::take(&mut nulls)).map(|_| ()),
                    &mut push_out,
                    false,
                )?;
            }
            Ok(true)
        })?;
        // `every` is read once per partition, so it stays in memory: these
        // are the rare left rows of a type the hash does not model.
        let every_rows = every.reader()?.next_batch(usize::MAX)?;
        let mut every_matched: HashSet<i64> = HashSet::new();
        let GraceRight {
            parts,
            null_keys,
            budget,
            ..
        } = right;
        for (rpart, lpart) in parts.into_iter().zip(lparts) {
            self.join_spilled(
                ops,
                rpart,
                lpart,
                &every_rows,
                &every_seqs,
                &mut every_matched,
                budget,
                0,
                &mut push_out,
            )?;
        }
        // A left row of `every` nothing matched, NULL-extended.
        if outer_left {
            let unmatched: Vec<Document> = every_rows
                .iter()
                .filter(
                    |d| !matches!(d.get(LSEQ), Some(Bson::Int64(n)) if every_matched.contains(n)),
                )
                .cloned()
                .collect();
            if !unmatched.is_empty() {
                let mut unmatched = Some(unmatched);
                (ops.join)(
                    Vec::new(),
                    &mut |s| match unmatched.take() {
                        Some(rows) => s(rows).map(|_| ()),
                        None => Ok(()),
                    },
                    &mut push_out,
                    true,
                )?;
            }
        }
        // The right rows with a NULL key, for a RIGHT / FULL join.
        if ops.outer_right {
            let mut r = null_keys.reader()?;
            loop {
                let rows = r.next_batch(BATCH)?;
                if rows.is_empty() {
                    break;
                }
                (ops.join)(rows, &mut |_| Ok(()), &mut push_out, false)?;
            }
        }
        drop(push_out);
        if !sorting {
            let mut rows = ordered.into_sorted()?;
            loop {
                self.check_cancel()?;
                let mut docs = rows.next_batch(BATCH)?;
                if docs.is_empty() {
                    return Ok(());
                }
                for d in docs.iter_mut() {
                    d.remove(LSEQ);
                    d.remove(RSEQ);
                }
                if !sink(docs)? {
                    return Ok(());
                }
            }
        }
        let mut sorted = out
            .finish(0, None, None)
            .map_err(|e| Self::user_error("XX000", format!("could not sort: {e}")))?;
        loop {
            self.check_cancel()?;
            let mut docs = sorted
                .next_batch()
                .map_err(|e| Self::user_error("XX000", format!("could not sort: {e}")))?;
            if docs.is_empty() {
                return Ok(());
            }
            for d in docs.iter_mut() {
                d.remove(LSEQ);
                d.remove(RSEQ);
            }
            if !sink(docs)? {
                return Ok(());
            }
        }
    }

    /// One partition: split again while its right rows are past `budget`,
    /// else joined with them in memory.
    #[allow(clippy::too_many_arguments)]
    fn join_spilled(
        &self,
        ops: &GraceOps<'_>,
        right: Spool,
        left: Spool,
        every: &[Document],
        every_seqs: &HashSet<i64>,
        every_matched: &mut HashSet<i64>,
        budget: usize,
        depth: u32,
        out: Sink<'_>,
    ) -> PgWireResult<()> {
        self.check_cancel()?;
        let (outer_left, outer_right) = (ops.outer_left, ops.outer_right);
        // Nothing to return: no right rows (and no left row to NULL-extend),
        // or no left rows at all (and no right row to).
        if (right.rows == 0 && (left.rows == 0 || !outer_left))
            || (left.rows == 0 && every.is_empty() && !outer_right)
        {
            return Ok(());
        }
        if right.bytes > budget && right.rows > 1 && depth < MAX_DEPTH {
            let split =
                |spool: Spool, key: &dyn Fn(&Document) -> KeyClass| -> PgWireResult<Vec<Spool>> {
                    let mut parts: Vec<Spool> = (0..PARTITIONS).map(|_| Spool::default()).collect();
                    let mut r = spool.reader()?;
                    while let Some(d) = r.next_row()? {
                        // Every row here has a key (see `GraceRight::add`).
                        let k = match key(&d) {
                            KeyClass::Key(k) => k,
                            _ => Vec::new(),
                        };
                        parts[partition_of(&k, depth + 1)].push(&d)?;
                    }
                    Ok(parts)
                };
            let rows = right.rows;
            let rparts = split(right, ops.right_key)?;
            let lparts = split(left, ops.left_key)?;
            // One key's rows cannot be split: a split that left every right
            // row in one part is joined as it stands.
            let depth = if rparts.iter().any(|p| p.rows == rows) {
                MAX_DEPTH
            } else {
                depth + 1
            };
            for (r, l) in rparts.into_iter().zip(lparts) {
                self.join_spilled(
                    ops,
                    r,
                    l,
                    every,
                    every_seqs,
                    every_matched,
                    budget,
                    depth,
                    out,
                )?;
            }
            return Ok(());
        }
        let rrows = right.reader()?.next_batch(usize::MAX)?;
        let feed_every = !rrows.is_empty() && !every.is_empty();
        let mut lr = left.reader()?;
        let mut feed = |s: Sink<'_>| -> PgWireResult<()> {
            loop {
                let rows = lr.next_batch(BATCH)?;
                if rows.is_empty() {
                    break;
                }
                if !s(rows)? {
                    return Ok(());
                }
            }
            if feed_every {
                s(every.to_vec())?;
            }
            Ok(())
        };
        (ops.join)(
            rrows,
            &mut feed,
            &mut |docs| {
                let mut kept = Vec::with_capacity(docs.len());
                for d in docs {
                    let l = match d.get(LSEQ) {
                        Some(Bson::Int64(n)) if every_seqs.contains(n) => Some(*n),
                        _ => None,
                    };
                    match (l, d.get(RSEQ)) {
                        // An `every` row NULL-extended here may match in another
                        // partition: decided after all of them.
                        (Some(_), None | Some(Bson::Null)) => continue,
                        (Some(n), _) => {
                            every_matched.insert(n);
                        }
                        _ => {}
                    }
                    kept.push(d);
                }
                out(kept)
            },
            false,
        )
    }
}
