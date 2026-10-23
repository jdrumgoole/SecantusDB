//! The system columns of a stored table -- `ctid`, `xmin`, `cmin`, `xmax`,
//! `cmax` -- kept as PostgreSQL keeps them (RP-069, decided by Joe on
//! 2026-10-22: persist the per-row state).
//!
//! Each row carries hidden fields ([`secantus_pgcatalog::SYSTEM_FIELDS`])
//! the writer stamps on it: the xid of the transaction that wrote this
//! version (`xmin`), the command within it (`cmin` / `cmax`, one field, as
//! PostgreSQL's one `t_cid` is), a live row's `xmax` of 0, and the tuple's
//! place in the heap PostgreSQL would have built (`ctid`).
//!
//! The heap is PostgreSQL's layout, computed rather than stored: 8 kB pages
//! with a 24-byte header, each tuple taking its MAXALIGNed length
//! (`heap_compute_data_size`: a 23-byte header plus null bitmap, then each
//! attribute at its type's alignment, a varlena of up to 126 bytes with a
//! one-byte header and no alignment) and a 4-byte line pointer. An INSERT
//! goes on the last page while it fits, else a new one; an UPDATE's new
//! version on its old page while it fits there, else the same way. Space a
//! DELETE or UPDATE leaves is not reused (no VACUUM runs here), and a
//! TRUNCATE starts the heap again. Each table's pages -- free bytes and next
//! line pointer -- are kept in this process, keyed by the store and the
//! table's oid (which a rename keeps and a new table never reuses), and
//! derived from the rows' ctids when a process first touches the table.
//!
//! What is not modelled: TOAST (a tuple over 2 kB has its largest values
//! moved out of line here, where PostgreSQL first compresses them), HOT
//! pruning, and a fillfactor below 100.

use bson::{Bson, Document};
use pgwire::error::PgWireResult;
use secantus_pgcatalog::TableDef;

use crate::PgHandler;

const PAGE_SPACE: u32 = 8192 - 24;
const LINE_POINTER: u32 = 4;
/// `MaxHeapTuplesPerPage` for 8 kB pages.
const MAX_TUPLES: u32 = 291;
/// `TOAST_TUPLE_THRESHOLD`.
const TOAST_THRESHOLD: usize = 2032;
/// An out-of-line value's pointer (`TOAST_POINTER_SIZE`, short header).
const TOAST_POINTER: usize = 18;

fn maxalign(n: usize) -> usize {
    (n + 7) & !7
}

fn align(n: usize, a: char) -> usize {
    let w = match a {
        'c' => 1,
        's' => 2,
        'i' => 4,
        _ => 8,
    };
    n.div_ceil(w) * w
}

/// A type's `(typlen, typalign)`: a built-in's as `pg_type` records it, a
/// user enum's 4 / `i`, any other user type a varlena aligned `d`.
fn layout(pg_type: &str) -> (i64, char) {
    let t = pg_type.trim();
    if t.ends_with("[]") {
        return (-1, 'i');
    }
    let Some(oid) = secantus_pgplan::pgtypes::oid_of_name(t) else {
        return if secantus_pgplan::is_user_enum(t) {
            (4, 'i')
        } else {
            (-1, 'd')
        };
    };
    let len = crate::pg_type_facts::builtin(oid, "typlen")
        .and_then(|v| match v {
            Bson::Int32(i) => Some(i64::from(i)),
            Bson::Int64(i) => Some(i),
            _ => None,
        })
        .unwrap_or(-1);
    let al = crate::pg_type_facts::builtin(oid, "typalign")
        .and_then(|v| match v {
            Bson::String(s) => s.chars().next(),
            _ => None,
        })
        .unwrap_or('i');
    (len, al)
}

/// A value's text, for a varlena's length.
fn text_of(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::Decimal128(d) => d.to_string(),
        Bson::Double(f) => f.to_string(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Binary(b) => String::from_utf8_lossy(&b.bytes).into_owned(),
        other => other.to_string(),
    }
}

/// A `numeric`'s stored bytes after its varlena header: the 2-byte short
/// header and its base-10000 digits, leading and trailing zero groups gone.
fn numeric_bytes(text: &str) -> usize {
    let t = text.trim().trim_start_matches(['-', '+']);
    if !t
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_digit() || b == b'.')
    {
        return 2; // NaN, Infinity
    }
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    let int = int.trim_start_matches('0');
    let frac = frac.trim_end_matches('0');
    // Groups of four digits about the decimal point.
    let int_groups = int.len().div_ceil(4);
    let frac_groups = frac.len().div_ceil(4);
    let digits: Vec<char> = {
        let pad_int = (4 - int.len() % 4) % 4;
        let mut s: String = "0".repeat(pad_int);
        s.push_str(int);
        s.push_str(frac);
        s.push_str(&"0".repeat((4 - frac.len() % 4) % 4));
        s.chars().collect()
    };
    let mut groups: Vec<&[char]> = digits.chunks(4).collect();
    debug_assert_eq!(groups.len(), int_groups + frac_groups);
    while groups.first().is_some_and(|g| g.iter().all(|c| *c == '0')) {
        groups.remove(0);
    }
    while groups.last().is_some_and(|g| g.iter().all(|c| *c == '0')) {
        groups.pop();
    }
    2 + 2 * groups.len()
}

/// The bytes a varlena value holds, header aside.
fn varlena_data(pg_type: &str, typmod: i32, v: &Bson) -> usize {
    match (pg_type, v) {
        (_, Bson::Binary(b)) => b.bytes.len(),
        ("numeric", v) => numeric_bytes(&text_of(v)),
        // A `char(n)` is stored padded to n characters.
        ("bpchar", v) => {
            let s = text_of(v);
            let n = usize::try_from(typmod - 4).unwrap_or(0);
            let chars = s.chars().count();
            s.len() + n.saturating_sub(chars)
        }
        (_, Bson::Array(items)) => {
            // ARR_OVERHEAD for one dimension, then 4-byte-headed elements.
            24 + items
                .iter()
                .map(|i| align(4 + text_of(i).len(), 'i'))
                .sum::<usize>()
        }
        (_, v) => text_of(v).len(),
    }
}

/// The length `heap_form_tuple` gives a row of `def`: header and null
/// bitmap, then each column in attnum order.
pub(crate) fn tuple_len(def: &TableDef, doc: &Document) -> usize {
    // Every attnum counts toward the bitmap, a dropped column's (NULL) too.
    let natts = def
        .columns
        .iter()
        .enumerate()
        .map(|(i, _)| def.attnum(i) as usize)
        .max()
        .unwrap_or(0);
    let has_dropped = natts > def.columns.len();
    let values: Vec<(&secantus_pgcatalog::Column, Option<&Bson>)> = def
        .columns
        .iter()
        .map(|c| {
            let v = doc.get(c.field()).filter(|v| !matches!(v, Bson::Null));
            (c, v)
        })
        .collect();
    let has_nulls = has_dropped || values.iter().any(|(_, v)| v.is_none());
    let bitmap = if has_nulls { natts.div_ceil(8) } else { 0 };
    let hoff = maxalign(23 + bitmap);
    let mut varlenas: Vec<usize> = Vec::new();
    let mut off = 0usize;
    for (c, v) in &values {
        let Some(v) = v else {
            continue;
        };
        let (len, al) = layout(&c.pg_type);
        if len > 0 {
            off = align(off, al) + len as usize;
        } else if len == -2 {
            off += text_of(v).len() + 1;
        } else {
            let data = varlena_data(&c.pg_type, c.typmod, v);
            if data < 127 {
                off += 1 + data;
            } else {
                off = align(off, al) + 4 + data;
                varlenas.push(4 + data);
            }
        }
    }
    let mut total = hoff + off;
    // TOAST: the largest values go out of line until the tuple fits.
    varlenas.sort_unstable_by(|a, b| b.cmp(a));
    for v in varlenas {
        if total <= TOAST_THRESHOLD {
            break;
        }
        total = total - v + TOAST_POINTER;
    }
    maxalign(total)
}

/// One table's pages: `(free bytes, line pointers used)`, and the space a
/// new tuple must leave free on a page (`fillfactor`). `disordered` once a
/// tuple's place no longer follows the rows' stored order -- an UPDATE's new
/// version, a rewrite -- so that a sequential scan must sort by ctid
/// (RP-192); until then the stored order is the heap's.
struct Heap {
    pages: Vec<(u32, u32)>,
    reserve: u32,
    disordered: bool,
}

impl Heap {
    fn fits(page: (u32, u32), need: u32) -> bool {
        page.0 >= need + LINE_POINTER && page.1 < MAX_TUPLES
    }

    /// An INSERT's test: room for the tuple and the fillfactor's reserve
    /// (`RelationGetBufferForTuple`'s `saveFreeSpace`).
    fn fits_new(&self, page: (u32, u32), need: u32) -> bool {
        page.0 >= need + LINE_POINTER + self.reserve && page.1 < MAX_TUPLES
    }

    fn put(&mut self, block: usize, need: u32) -> i64 {
        let page = &mut self.pages[block];
        page.0 -= need + LINE_POINTER;
        page.1 += 1;
        (block as i64) * 65536 + i64::from(page.1)
    }

    /// A new tuple: the last page while it fits, else a new one.
    fn insert(&mut self, need: u32) -> i64 {
        let last = self.pages.len().checked_sub(1);
        match last {
            Some(b) if self.fits_new(self.pages[b], need) => self.put(b, need),
            _ => {
                self.pages.push((PAGE_SPACE, 0));
                self.put(self.pages.len() - 1, need)
            }
        }
    }

    /// A new version of the tuple at `old`: its own page while it fits.
    fn update(&mut self, old: Option<i64>, need: u32) -> i64 {
        self.disordered = true;
        if let Some(block) = old
            .filter(|t| *t >= 0)
            .and_then(|t| usize::try_from(t / 65536).ok())
        {
            if block < self.pages.len() && Self::fits(self.pages[block], need) {
                return self.put(block, need);
            }
        }
        self.insert(need)
    }
}

/// How many command ids a statement other than a row write consumes in
/// PostgreSQL 15.19, measured inside a transaction block: the catalog
/// changes a DDL statement makes advance the command counter as many times
/// as its steps do. A statement this table does not list counts one.
fn ddl_command_ids(stmt: &secantus_pgplan::Statement) -> u32 {
    use secantus_pgplan::Statement as S;
    match stmt {
        S::Select(_)
        | S::SelectConstant(_)
        | S::Truncate { .. }
        | S::Set { .. }
        | S::Show { .. }
        | S::Transaction(_) => 0,
        S::CreateTable(def, _) => {
            let serials = def.columns.iter().filter(|c| c.sequence.is_some()).count() as u32;
            let pk = def.columns.iter().any(|c| c.pk);
            1 + if pk { 2 } else { 0 } + 4 * serials
        }
        S::CreateIndex(_) => 2,
        S::CreateView { .. } => 2,
        S::DropTable(_) => 3,
        _ => 1,
    }
}

impl PgHandler {
    fn heap_key(&self, table: &str) -> String {
        format!("heap:{}", self.relation_oid(table).unwrap_or(0))
    }

    /// A table's pages as its stored rows show them: each live tuple's
    /// length on its block, the block's line pointers up to the highest
    /// offset in use, and whether the places follow the stored order. What
    /// a process knows from the rows when it first touches a table (after a
    /// restart: a dead tuple's space is then free, as after a VACUUM).
    fn derive_heap(&self, table: &str) -> (Vec<(u32, u32)>, bool) {
        let Some(def) = self.lookup(table) else {
            return (Vec::new(), false);
        };
        let raw = self
            .storage
            .find_matching(self.db(), table, &Document::new())
            .unwrap_or_default();
        let mut pages: Vec<(u32, u32)> = Vec::new();
        let mut last = -1;
        let mut disordered = false;
        for row in raw.iter().filter_map(|b| crate::decode_doc(b).ok()) {
            let Some(ctid) = ctid_of(&row) else {
                continue;
            };
            disordered |= ctid < last;
            last = ctid;
            let (block, offset) = ((ctid / 65536) as usize, (ctid % 65536) as u32);
            if pages.len() <= block {
                pages.resize(block + 1, (PAGE_SPACE, 0));
            }
            let need = tuple_len(&def, &row) as u32 + LINE_POINTER;
            let page = &mut pages[block];
            page.0 = page.0.saturating_sub(need);
            page.1 = page.1.max(offset);
        }
        (pages, disordered)
    }

    /// `BLCKSZ * (100 - fillfactor) / 100`: the bytes an INSERT leaves free
    /// on a page of `table`.
    fn heap_reserve(&self, table: &str) -> u32 {
        let ff = self
            .lookup(table)
            .and_then(|d| {
                d.extra.get_array("reloptions").ok().and_then(|a| {
                    a.iter().find_map(|v| {
                        v.as_str()?
                            .strip_prefix("fillfactor=")
                            .and_then(|n| n.trim().parse::<u32>().ok())
                    })
                })
            })
            .unwrap_or(100)
            .clamp(10, 100);
        8192 * (100 - ff) / 100
    }

    /// Place tuples in `table`'s heap: `f` gets the heap and returns the
    /// places. The heap's free space is not transactional in PostgreSQL --
    /// a rolled-back row's place stays taken, an aborted tuple still on its
    /// page, and ROLLBACK TO leaves it so too -- so it is kept outside any
    /// transaction: in this process, per store and table, never committed
    /// to the store (a commit there would be another session's commit to a
    /// REPEATABLE READ snapshot). One writer at a time, as a page's buffer
    /// lock makes it.
    fn with_heap<T>(&self, table: &str, f: impl FnOnce(&mut Heap) -> T) -> PgWireResult<T> {
        type Heaps = std::collections::HashMap<(usize, String), (Vec<(u32, u32)>, bool)>;
        static HEAPS: std::sync::OnceLock<std::sync::Mutex<Heaps>> = std::sync::OnceLock::new();
        let key = (
            std::sync::Arc::as_ptr(&self.storage) as usize,
            self.heap_key(table),
        );
        let reserve = self.heap_reserve(table);
        let known = HEAPS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&key);
        // Read before the lock is taken: deriving reads the store.
        let derived = (!known).then(|| self.derive_heap(table));
        let mut all = HEAPS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = all
            .entry(key)
            .or_insert_with(|| derived.unwrap_or_default());
        let mut heap = Heap {
            pages: std::mem::take(&mut state.0),
            reserve,
            disordered: state.1,
        };
        let out = f(&mut heap);
        *state = (heap.pages, heap.disordered);
        Ok(out)
    }

    /// A TRUNCATE starts the heap again.
    pub(crate) fn reset_heap(&self, table: &str) -> PgWireResult<()> {
        self.with_heap(table, |h| {
            h.pages.clear();
            h.disordered = false;
        })
    }

    /// Must a sequential scan of `table` sort its stored rows by ctid to
    /// read them in heap order? Only once a tuple has moved (see `Heap`). A
    /// partitioned table's rows are in its leaves' heaps, each with places
    /// of its own, so a ctid does not order them across leaves.
    pub(crate) fn heap_scan_disordered(&self, table: &str) -> bool {
        if table.starts_with("__") || Self::virtual_table(table).is_some() {
            return false;
        }
        match self.lookup(table) {
            Some(def) if !crate::partition::is_partitioned(&def) => {}
            _ => return false,
        }
        self.with_heap(table, |h| h.disordered).unwrap_or(false)
    }

    fn pid(&self) -> i32 {
        self.backend_pid.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The writing transaction's xid (assigned now, as PostgreSQL assigns
    /// it at the first write) and the command's id.
    fn writer(&self) -> (i64, i64) {
        let pid = self.pid();
        let xid =
            secantus_pgplan::snapshots::ask_pid(pid, secantus_pgplan::snapshots::XidOp::Assign)
                .and_then(|x| x.parse::<u64>().ok())
                .unwrap_or(u64::from(crate::xids::FROZEN_XID));
        let cid = crate::xids::command_id(&self.storage, pid);
        // An xid is 32 bits (the low half of the xid8).
        ((xid & 0xFFFF_FFFF) as i64, i64::from(cid))
    }

    /// The heap a row of `table` lives in: a partitioned table's rows are
    /// in their leaf partition's, each partition being a relation of its
    /// own in PostgreSQL.
    fn heap_of(&self, table: &str, def: &TableDef, row: &Document) -> PgWireResult<String> {
        if crate::partition::is_partitioned(def) {
            if let Some(leaf) = self.leaf_for(def, row)? {
                return Ok(leaf);
            }
        }
        Ok(table.to_string())
    }

    /// Stamp new rows of `table`: this transaction's xid and command, and
    /// each tuple's place at the end of its heap.
    pub(crate) fn stamp_inserted(
        &self,
        table: &str,
        def: &TableDef,
        rows: &mut [Document],
    ) -> PgWireResult<()> {
        let mut versions: Vec<(Option<i64>, Document)> =
            rows.iter().map(|r| (None, r.clone())).collect();
        self.stamp_versions(table, def, &mut versions)?;
        for (r, (_, v)) in rows.iter_mut().zip(versions) {
            *r = v;
        }
        Ok(())
    }

    /// Stamp row versions: `(old ctid, row)`, a new row with no old one
    /// going to the end of its heap, a new version of an updated one on its
    /// old page while it fits.
    fn stamp_versions(
        &self,
        table: &str,
        def: &TableDef,
        rows: &mut [(Option<i64>, Document)],
    ) -> PgWireResult<()> {
        if rows.is_empty() || table.starts_with("__") {
            return Ok(());
        }
        let (xmin, cid) = self.writer();
        // By heap, in the rows' order within each.
        let mut heaps: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, (_, r)) in rows.iter().enumerate() {
            let h = self.heap_of(table, def, r)?;
            match heaps.iter_mut().find(|(n, _)| *n == h) {
                Some((_, v)) => v.push(i),
                None => heaps.push((h, vec![i])),
            }
        }
        for (heap_table, idx) in heaps {
            let needs: Vec<(Option<i64>, u32)> = idx
                .iter()
                .map(|i| (rows[*i].0, tuple_len(def, &rows[*i].1) as u32))
                .collect();
            let ctids = self.with_heap(&heap_table, |heap| {
                needs
                    .iter()
                    .map(|(old, n)| match old {
                        Some(_) => heap.update(*old, *n),
                        None => heap.insert(*n),
                    })
                    .collect::<Vec<_>>()
            })?;
            for (i, ctid) in idx.into_iter().zip(ctids) {
                stamp(&mut rows[i].1, xmin, cid, ctid);
            }
        }
        Ok(())
    }

    /// `VACUUM FULL` / `CLUSTER`: the rows, in `rows`' order, copied to a
    /// new heap (`rewrite_heap_tuple`): each keeps its xmin and command, and
    /// takes the next place.
    pub(crate) fn rewrite_heap(
        &self,
        table: &str,
        def: &TableDef,
        rows: &mut [Document],
    ) -> PgWireResult<()> {
        if table.starts_with("__") {
            return Ok(());
        }
        let mut heaps: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            let h = self.heap_of(table, def, r)?;
            match heaps.iter_mut().find(|(n, _)| *n == h) {
                Some((_, v)) => v.push(i),
                None => heaps.push((h, vec![i])),
            }
        }
        self.reset_heap(table)?;
        for (heap_table, idx) in heaps {
            let needs: Vec<u32> = idx
                .iter()
                .map(|i| tuple_len(def, &rows[*i]) as u32)
                .collect();
            let ctids = self.with_heap(&heap_table, |heap| {
                heap.pages.clear();
                // CLUSTER's order is an index's, not the stored one.
                heap.disordered = true;
                needs.iter().map(|n| heap.insert(*n)).collect::<Vec<_>>()
            })?;
            for (i, ctid) in idx.into_iter().zip(ctids) {
                rows[i].insert("__sys_ctid", Bson::Int64(ctid));
            }
        }
        Ok(())
    }

    /// Give back the place a row took that was never written: an INSERT
    /// ... ON CONFLICT whose arbiter found the key, which PostgreSQL checks
    /// BEFORE inserting (`ExecCheckIndexConstraints`), so no tuple is made.
    pub(crate) fn release_inserted(
        &self,
        table: &str,
        def: &TableDef,
        row: &Document,
    ) -> PgWireResult<()> {
        let Some(ctid) = ctid_of(row) else {
            return Ok(());
        };
        let need = tuple_len(def, row) as u32;
        let (block, offset) = ((ctid / 65536) as usize, (ctid % 65536) as u32);
        let heap_table = self.heap_of(table, def, row)?;
        self.with_heap(&heap_table, |heap| {
            if block + 1 == heap.pages.len() && heap.pages[block].1 == offset {
                let page = &mut heap.pages[block];
                page.0 += need + LINE_POINTER;
                page.1 -= 1;
                if page.1 == 0 {
                    heap.pages.pop();
                }
            }
        })
    }

    /// Stamp new versions of updated rows: `(old ctid, new row)` each.
    pub(crate) fn stamp_updated(
        &self,
        table: &str,
        def: &TableDef,
        rows: &mut [(Option<i64>, Document)],
    ) -> PgWireResult<()> {
        // A row whose old place is unknown (written before ctids were kept)
        // is placed as a new one is.
        for (old, _) in rows.iter_mut() {
            if old.is_none() {
                *old = Some(-1);
            }
        }
        self.stamp_versions(table, def, rows)
    }
}

/// The command ids a statement consumes: a row write (INSERT, UPDATE,
/// DELETE, MERGE, COPY FROM) one, whether or not it wrote a row
/// (`ExecInitModifyTable` takes it); DDL its measured count.
pub(crate) fn command_ids(stmt: &secantus_pgplan::Statement) -> u32 {
    use secantus_pgplan::Statement as S;
    match stmt {
        S::Insert(_) | S::Update(_) | S::Delete(_) | S::CopyFrom(_) | S::RuleRewrite(_) => 1,
        _ => ddl_command_ids(stmt),
    }
}

/// The hidden system fields on a row version.
pub(crate) fn stamp(doc: &mut Document, xmin: i64, cid: i64, ctid: i64) {
    doc.insert("__sys_xmin", Bson::Int64(xmin));
    doc.insert("__sys_cid", Bson::Int64(cid));
    doc.insert("__sys_xmax", Bson::Int64(0));
    doc.insert("__sys_ctid", Bson::Int64(ctid));
}

/// Stored rows in heap order, as a sequential scan reads them: by ctid,
/// a row without one (written before ctids were kept) first.
pub(crate) fn in_ctid_order(mut rows: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    rows.sort_by_cached_key(|b| {
        bson::RawDocument::from_bytes(b)
            .ok()
            .and_then(|d| d.get_i64("__sys_ctid").ok())
            .unwrap_or(-1)
    });
    rows
}

/// The sort key that reads rows in heap order (`in_ctid_order`), for a
/// streamed scan's bounded-memory sort.
pub(crate) fn ctid_order_key() -> secantus_pgplan::OrderKey {
    secantus_pgplan::OrderKey {
        field: "__sys_ctid".to_string(),
        ascending: true,
        nulls: secantus_pgplan::Nulls::First,
        jsonb: false,
        expr: None,
    }
}

/// A stored row's ctid, if it was stamped.
pub(crate) fn ctid_of(doc: &Document) -> Option<i64> {
    match doc.get("__sys_ctid")? {
        Bson::Int64(v) => Some(*v),
        Bson::Int32(v) => Some(i64::from(*v)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secantus_pgcatalog::Column;

    /// `(a int, b text)` with 100-byte text: PostgreSQL 15.19 puts 58 such
    /// tuples on a page (a=70 at (1,12), a=200 at (3,26)).
    #[test]
    fn tuples_fill_pages_as_postgresql_does() {
        let def = TableDef::new(
            "t",
            vec![
                Column::new("a", "int4", false),
                Column::new("b", "text", false),
            ],
        );
        let doc = bson::doc! {"a": 1, "b": "x".repeat(100)};
        let need = tuple_len(&def, &doc) as u32;
        let mut heap = Heap {
            pages: Vec::new(),
            reserve: 0,
            disordered: false,
        };
        let ctids: Vec<i64> = (1..=200).map(|_| heap.insert(need)).collect();
        let tid = |b: i64, o: i64| b * 65536 + o;
        assert_eq!(ctids[69], tid(1, 12));
        assert_eq!(ctids[199], tid(3, 26));
    }

    /// `(a int)`: 226 tuples a page (28 bytes, MAXALIGNed to 32, plus 4).
    #[test]
    fn small_tuples() {
        let def = TableDef::new("t", vec![Column::new("a", "int4", false)]);
        assert_eq!(tuple_len(&def, &bson::doc! {"a": 1}), 32);
        assert_eq!(numeric_bytes("1.50"), 6);
        assert_eq!(numeric_bytes("0"), 2);
    }

    /// An UPDATE moves a tuple out of the stored order; a scan then sorts
    /// by ctid, a row without one first.
    #[test]
    fn an_update_disorders_the_heap() {
        let mut heap = Heap {
            pages: Vec::new(),
            reserve: 0,
            disordered: false,
        };
        let first = heap.insert(32);
        heap.insert(32);
        assert!(!heap.disordered);
        assert_eq!(heap.update(Some(first), 32), 3);
        assert!(heap.disordered);
        let row = |ctid: Option<i64>| {
            let mut d = bson::doc! {"a": 1};
            if let Some(c) = ctid {
                d.insert("__sys_ctid", c);
            }
            bson::to_vec(&d).unwrap()
        };
        let sorted = in_ctid_order(vec![row(Some(3)), row(Some(2)), row(None)]);
        assert_eq!(sorted, vec![row(None), row(Some(2)), row(Some(3))]);
    }
}
