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
//! one-byte header and no alignment) and a 4-byte line pointer. The pages,
//! the free space map and their rules are [`crate::heap_page`] and
//! [`crate::fsm`] (RP-193): a new tuple goes where `RelationGetBufferForTuple`
//! puts it, an UPDATE's new version on its old page while it fits there (a
//! heap-only tuple when no indexed field changed), a page is pruned when a
//! scan meets it short of space, and VACUUM removes what no snapshot can
//! see. Deleted rows reach the heap through the store's delete observer;
//! whether their deleter committed is settled from the store once it is
//! older than every snapshot still held. A TRUNCATE starts the heap again.
//! Each table's heap is kept in this process, keyed by the store and the
//! table's oid (which a rename keeps and a new table never reuses), and
//! derived from the rows' ctids when a process first touches the table --
//! so a restart loses the dead tuples, the free space map and the target
//! blocks, which PostgreSQL keeps (RP-193).
//!
//! What is not modelled: TOAST (a tuple over 2 kB has its largest values
//! moved out of line here, where PostgreSQL first compresses them).

use bson::{Bson, Document};
use pgwire::error::PgWireResult;
use secantus_pgcatalog::TableDef;

use crate::heap_page::{tid, untid, Heap, Lp, Page, LINE_POINTER, PAGE_SPACE};
use crate::PgHandler;

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
            server_len(&s) + n.saturating_sub(chars)
        }
        (_, Bson::Array(items)) => {
            // ARR_OVERHEAD for one dimension, then 4-byte-headed elements.
            24 + items
                .iter()
                .map(|i| align(4 + server_len(&text_of(i)), 'i'))
                .sum::<usize>()
        }
        (_, v) => server_len(&text_of(v)),
    }
}

/// A text's length in the server encoding's bytes, as the tuple holds it
/// (RP-064).
fn server_len(s: &str) -> usize {
    secantus_pgplan::server_encoding::bytes(s).len()
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

/// A column's type as a tuple lays it out: its name and typmod, and its
/// `(typlen, typalign)`, looked up once.
#[derive(Debug, Clone)]
pub(crate) struct Laid {
    pub(crate) pg_type: String,
    typmod: i32,
    len: i64,
    align: char,
}

impl Laid {
    pub(crate) fn of(pg_type: &str, typmod: i32) -> Self {
        let (len, align) = layout(pg_type);
        Laid {
            pg_type: pg_type.to_string(),
            typmod,
            len,
            align,
        }
    }
}

/// The length `index_form_tuple` gives an index tuple of these
/// attributes, `types` each one's layout (`None` for an expression's,
/// sized by its value): an 8-byte header, 16 with a null bitmap, then each
/// attribute aligned as its type is.
pub(crate) fn index_tuple_len(types: &[Option<Laid>], values: &[Bson]) -> usize {
    let has_nulls = values.iter().any(|v| matches!(v, Bson::Null));
    let hoff = if has_nulls { maxalign(8 + 4) } else { 8 };
    let mut off = 0usize;
    for (t, v) in types.iter().zip(values) {
        if matches!(v, Bson::Null) {
            continue;
        }
        let guessed;
        let l = match t {
            Some(l) => l,
            None => {
                guessed = Laid::of(value_type(v), -1);
                &guessed
            }
        };
        if l.len > 0 {
            off = align(off, l.align) + l.len as usize;
        } else if l.len == -2 {
            off += text_of(v).len() + 1;
        } else {
            let data = varlena_data(&l.pg_type, l.typmod, v);
            if data < 127 {
                off += 1 + data;
            } else {
                off = align(off, l.align) + 4 + data;
            }
        }
    }
    maxalign(hoff + off)
}

/// The type an expression's stored value has, for its index tuple's size.
fn value_type(v: &Bson) -> &'static str {
    match v {
        Bson::Int32(_) => "int4",
        Bson::Int64(_) => "int8",
        Bson::Double(_) => "float8",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "timestamptz",
        _ => "text",
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

/// A row a DELETE removed, as the store's delete path reported it: its
/// table, place, `_id` and tuple length (or the row, when the length needs
/// a definition the statement did not name).
struct Deleted {
    table: String,
    ctid: i64,
    id: Bson,
    len: Option<u32>,
    row: Option<Document>,
    /// The row, kept while a serializable transaction runs here: SSI
    /// tracks the version for other transactions' conflicts out.
    ssi: Option<Document>,
}

thread_local! {
    /// What this thread's deletes removed since the statement began.
    static DELETED: std::cell::RefCell<Vec<Deleted>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The definitions a delete's length is computed by, the statement's
    /// own table's among them.
    static DELETE_DEFS: std::cell::RefCell<std::collections::HashMap<String, TableDef>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// Nonzero while a table's rows are replaced wholesale (a rewrite, a
    /// TRUNCATE): those removals are the old heap's, not dead tuples.
    static DELETES_SUPPRESSED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The store's delete observer (installed once per thread).
fn observe_delete(_db: &str, coll: &str, blob: &[u8]) {
    if coll.starts_with("__") || DELETES_SUPPRESSED.with(std::cell::Cell::get) > 0 {
        return;
    }
    let Ok(doc) = crate::decode_doc(blob) else {
        return;
    };
    let Some(ctid) = ctid_of(&doc) else {
        return;
    };
    let id = doc.get("_id").cloned().unwrap_or(Bson::Null);
    let def = DELETE_DEFS.with(|d| d.borrow().get(coll).cloned());
    let ssi = crate::serializable::SSI_CAPTURE
        .with(std::cell::Cell::get)
        .then(|| doc.clone());
    let (len, row) = match def {
        Some(def) if !crate::partition::is_partitioned(&def) => {
            (Some(tuple_len(&def, &doc) as u32), None)
        }
        _ => (None, Some(doc)),
    };
    DELETED.with(|d| {
        d.borrow_mut().push(Deleted {
            table: coll.to_string(),
            ctid,
            id,
            len,
            row,
            ssi,
        })
    });
}

/// Run `f` with the deletes it makes not taken as dead tuples.
pub(crate) fn without_dead_tuples<T>(f: impl FnOnce() -> T) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            DELETES_SUPPRESSED.with(|c| c.set(c.get() - 1));
        }
    }
    DELETES_SUPPRESSED.with(|c| c.set(c.get() + 1));
    let _restore = Restore;
    f()
}

type Heaps = std::collections::HashMap<(usize, String), Heap>;

fn heaps() -> &'static std::sync::Mutex<Heaps> {
    static HEAPS: std::sync::OnceLock<std::sync::Mutex<Heaps>> = std::sync::OnceLock::new();
    HEAPS.get_or_init(Default::default)
}

/// The xids still running, and the removal horizon (`GlobalVisState`): no
/// tuple a transaction from it on deleted may be removed yet.
fn visibility() -> (std::collections::HashSet<u64>, u64) {
    let running: std::collections::HashSet<u64> =
        crate::xids::running().into_iter().map(|(_, x)| x).collect();
    (running, crate::xids::horizon())
}

/// Forget backend `pid`'s target blocks (its session ended).
pub(crate) fn forget_backend(pid: i32) {
    for heap in heaps()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values_mut()
    {
        heap.targets.remove(&pid);
    }
}

impl PgHandler {
    /// A heap's key: its store, then its database and table oid (each
    /// database draws its oids from a counter of its own, so an oid alone
    /// names a table in every database at once -- RP-233).
    fn heap_key(&self, table: &str) -> (usize, String) {
        (
            std::sync::Arc::as_ptr(&self.storage) as usize,
            format!(
                "heap:{}:{}",
                self.db(),
                self.relation_oid(table).unwrap_or(0)
            ),
        )
    }

    /// A table's heap as its stored rows show it, what a process knows when
    /// it first touches a table: each live tuple's length on its block, the
    /// line pointers up to the highest offset in use -- one between them
    /// that no row holds being dead -- and whether the places follow the
    /// stored order. (After a restart, then, a dead tuple's space is free,
    /// its line pointer not, and the free space map is empty.)
    fn derive_heap(&self, table: &str) -> Heap {
        // A heap a checkpoint wrote, made true to the rows (RP-193).
        if let Some(saved) = self.saved_heap(table) {
            let rows = self.heap_row_places(table);
            return crate::heap_store::reconcile(saved, &rows);
        }
        let mut heap = Heap::default();
        let Some(def) = self.lookup(table) else {
            return heap;
        };
        // A partition's rows are in its tree's root, under the root's fields.
        let (def, rows) = match self.partition_rows(&def) {
            Ok(Some((root, rows))) => (root, rows),
            _ => {
                let rows = self
                    .storage
                    .find_matching(self.db(), table, &Document::new())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|b| crate::decode_doc(b).ok())
                    .collect();
                (def, rows)
            }
        };
        let mut last = -1;
        let mut held: std::collections::HashMap<u32, Vec<u16>> = Default::default();
        for row in rows {
            let Some((block, off)) = ctid_of(&row).and_then(untid) else {
                continue;
            };
            let ctid = tid(block, off);
            heap.disordered |= ctid < last;
            last = ctid;
            if heap.pages.len() <= block as usize {
                heap.pages.resize(block as usize + 1, Page::default());
            }
            let page = &mut heap.pages[block as usize];
            page.free += tuple_len(&def, &row) as u32;
            page.nlp = page.nlp.max(off);
            held.entry(block).or_default().push(off);
        }
        for (b, page) in heap.pages.iter_mut().enumerate() {
            let used = page.free;
            page.free = PAGE_SPACE.saturating_sub(used + u32::from(page.nlp) * LINE_POINTER);
            let have = held.remove(&(b as u32)).unwrap_or_default();
            for off in 1..=page.nlp {
                if !have.contains(&off) {
                    page.lps.insert(off, Lp::Dead);
                }
            }
        }
        heap
    }

    /// `BLCKSZ * (100 - fillfactor) / 100`: the bytes an INSERT leaves free
    /// on a page of `table`.
    fn heap_reserve(&self, table: &str) -> u32 {
        let ff = self
            .reloption(table, "fillfactor")
            .and_then(|n| n.trim().parse::<u32>().ok())
            .unwrap_or(100)
            .clamp(10, 100);
        8192 * (100 - ff) / 100
    }

    /// A table's storage parameter, as `WITH (...)` set it.
    fn reloption(&self, table: &str, name: &str) -> Option<String> {
        let def = self.lookup(table)?;
        let opts = def.extra.get_array("reloptions").ok()?;
        opts.iter().find_map(|v| {
            let (k, val) = v.as_str()?.split_once('=')?;
            (k.trim() == name).then(|| val.trim().to_string())
        })
    }

    /// Work on `table`'s heap. The heap is not transactional in PostgreSQL
    /// -- a rolled-back row's place stays taken, an aborted tuple still on
    /// its page, and ROLLBACK TO leaves it so too -- so it is kept outside
    /// any transaction: in this process, per store and table, never
    /// committed to the store (a commit there would be another session's
    /// commit to a REPEATABLE READ snapshot). One writer at a time, as a
    /// page's buffer lock makes it.
    fn with_heap<T>(&self, table: &str, f: impl FnOnce(&mut Heap) -> T) -> PgWireResult<T> {
        self.heap_access(table, true, f)
    }

    /// [`Self::with_heap`] for a look that changes nothing: the heap is not
    /// marked for the next checkpoint.
    fn with_heap_read<T>(&self, table: &str, f: impl FnOnce(&mut Heap) -> T) -> PgWireResult<T> {
        self.heap_access(table, false, f)
    }

    fn heap_access<T>(
        &self,
        table: &str,
        writes: bool,
        f: impl FnOnce(&mut Heap) -> T,
    ) -> PgWireResult<T> {
        let key = self.heap_key(table);
        let reserve = self.heap_reserve(table);
        let known = heaps()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&key);
        // Read before the lock is taken: deriving reads the store.
        let derived = (!known).then(|| self.derive_heap(table));
        let (running, _) = visibility();
        let mut all = heaps().lock().unwrap_or_else(|e| e.into_inner());
        let heap = all
            .entry(key)
            .or_insert_with(|| derived.unwrap_or_default());
        heap.reserve = reserve;
        heap.forget_ended(&|x| running.contains(&x));
        heap.dirty |= writes;
        Ok(f(heap))
    }

    /// A new relation file (a TRUNCATE): an empty heap and free space map.
    pub(crate) fn reset_heap(&self, table: &str) -> PgWireResult<()> {
        self.with_heap(table, |h| *h = Heap::default())
    }

    /// Must a sequential scan of `table` sort its stored rows by ctid to
    /// read them in heap order? Only once a tuple has moved (see `Heap`). A
    /// partitioned table's rows are in its leaves' heaps, each with places
    /// of its own, so a ctid does not order them across leaves.
    pub(crate) fn heap_scan_disordered(&self, table: &str) -> bool {
        if !self.has_heap(table) {
            return false;
        }
        self.with_heap_read(table, |h| h.disordered)
            .unwrap_or(false)
    }

    /// A stored table with a heap of its own (not a catalog, a view or a
    /// partitioned parent).
    pub(crate) fn has_heap(&self, table: &str) -> bool {
        if table.starts_with("__") || Self::virtual_table(table).is_some() {
            return false;
        }
        matches!(self.lookup(table), Some(def) if !crate::partition::is_partitioned(&def))
    }

    fn pid(&self) -> i32 {
        self.backend_pid.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The writing transaction's xid8 (assigned now, as PostgreSQL assigns
    /// it at the first write) and the command's id.
    /// Whether `row` is a version the running command wrote: its xmin is
    /// this transaction's xid and its cmin this command's.
    pub(crate) fn written_by_this_command(&self, row: &Document) -> bool {
        let (xid, cid) = self.writer();
        let field = |k: &str| match row.get(k) {
            Some(Bson::Int64(v)) => Some(*v),
            Some(Bson::Int32(v)) => Some(i64::from(*v)),
            _ => None,
        };
        field("__sys_xmin") == Some((xid & 0xFFFF_FFFF) as i64) && field("__sys_cid") == Some(cid)
    }

    /// The running command's `(xid, command id)`.
    pub(crate) fn command_writer(&self) -> (u64, i64) {
        self.writer()
    }

    fn writer(&self) -> (u64, i64) {
        let pid = self.pid();
        let xid =
            secantus_pgplan::snapshots::ask_pid(pid, secantus_pgplan::snapshots::XidOp::Assign)
                .and_then(|x| x.parse::<u64>().ok())
                .unwrap_or(u64::from(crate::xids::FROZEN_XID));
        let cid = crate::xids::command_id(&self.storage, pid);
        (xid, i64::from(cid))
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

    /// The fields an index of `table` reads -- its keys, an expression's
    /// columns, a partial index's predicate -- which an UPDATE must leave
    /// unchanged to be HOT (`INDEX_ATTR_BITMAP_ALL`).
    fn indexed_fields(&self, table: &str, def: &TableDef) -> Vec<String> {
        let Ok(indexes) = self.storage.list_indexes(self.db(), table) else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        let mut add = |f: &str| {
            if !out.iter().any(|o| o == f) {
                out.push(f.to_string());
            }
        };
        for ix in &indexes {
            if let Ok(key) = ix.get_document("key") {
                for (f, _) in key {
                    add(f);
                }
            }
            if let Ok(partial) = ix.get_document("partialFilterExpression") {
                let mut stack = vec![Bson::Document(partial.clone())];
                while let Some(v) = stack.pop() {
                    match v {
                        Bson::Document(d) => {
                            for (k, v) in d {
                                if !k.starts_with('$') {
                                    add(&k);
                                }
                                stack.push(v);
                            }
                        }
                        Bson::Array(a) => stack.extend(a),
                        _ => {}
                    }
                }
            }
            // A predicate kept as SQL (RP-236) reads the columns it names.
            let predicate_sql = ix.get_str("sqlPredicate").ok().or_else(|| {
                ix.get_document("options")
                    .ok()
                    .and_then(|o| o.get_str("sqlPredicate").ok())
            });
            if let Some(sql) = predicate_sql {
                for c in &def.columns {
                    if sql
                        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                        .any(|w| w.eq_ignore_ascii_case(&c.name))
                    {
                        add(&c.field());
                    }
                }
            }
            if let Some((exprs, _)) = Self::index_expressions(ix) {
                for c in &def.columns {
                    let named = exprs.iter().any(|e| {
                        e.split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                            .any(|w| w.eq_ignore_ascii_case(&c.name))
                    });
                    if named {
                        add(&c.field());
                    }
                }
            }
        }
        out
    }

    /// Stamp new rows of `table`: this transaction's xid and command, and
    /// each tuple's place, as `RelationGetBufferForTuple` finds it.
    pub(crate) fn stamp_inserted(
        &self,
        table: &str,
        def: &TableDef,
        rows: &mut [Document],
    ) -> PgWireResult<()> {
        let mut versions: Vec<(Option<Document>, Document)> =
            rows.iter().map(|r| (None, r.clone())).collect();
        self.stamp_versions(table, def, &mut versions)?;
        for (r, (_, v)) in rows.iter_mut().zip(versions) {
            *r = v;
        }
        Ok(())
    }

    /// Stamp new versions of updated rows: `(old row, new row)` each, the
    /// old row as it was stored (its place, its values).
    pub(crate) fn stamp_updated(
        &self,
        table: &str,
        def: &TableDef,
        rows: &mut [(Option<Document>, Document)],
    ) -> PgWireResult<()> {
        self.stamp_versions(table, def, rows)
    }

    /// Stamp row versions: `(old row, row)`. A new row (no old one) goes
    /// where an INSERT's would; a new version of an updated one on its old
    /// page while it fits, as a heap-only tuple when no indexed field
    /// changed.
    fn stamp_versions(
        &self,
        table: &str,
        def: &TableDef,
        rows: &mut [(Option<Document>, Document)],
    ) -> PgWireResult<()> {
        if rows.is_empty() || table.starts_with("__") {
            return Ok(());
        }
        let (xid, cid) = self.writer();
        let pid = self.pid();
        let indexed = if rows.iter().any(|(old, _)| old.is_some()) {
            self.indexed_fields(table, def)
        } else {
            Vec::new()
        };
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
            struct Version {
                old: Option<(Option<i64>, u32, bool, Bson)>,
                len: u32,
            }
            let versions: Vec<Version> = idx
                .iter()
                .map(|i| {
                    let (old, new) = &rows[*i];
                    let len = tuple_len(def, new) as u32;
                    let old = old.as_ref().map(|o| {
                        let hot = indexed.iter().all(|f| o.get(f) == new.get(f));
                        (
                            ctid_of(o),
                            tuple_len(def, o) as u32,
                            hot,
                            o.get("_id").cloned().unwrap_or(Bson::Null),
                        )
                    });
                    Version { old, len }
                })
                .collect();
            // The UPDATE's scan read the old rows' pages first.
            let pages: Vec<i64> = versions
                .iter()
                .filter_map(|v| v.old.as_ref().and_then(|o| o.0))
                .collect();
            if !pages.is_empty() {
                self.prune_pages(&heap_table, Some(&pages))?;
            }
            // What each version is, for the statistics (RP-070): an
            // insert, or an update -- heap-only when it stayed on its page
            // with no indexed field changed.
            let olds: Vec<Option<(Option<i64>, bool)>> = versions
                .iter()
                .map(|v| v.old.as_ref().map(|o| (o.0, o.2)))
                .collect();
            // SSI's check of each write, before it is made: an insert
            // against the table, an update against its old version too.
            for v in &versions {
                self.ssi_write(&heap_table, v.old.as_ref().and_then(|o| o.0))?;
            }
            let (ctids, accessed) = self.with_heap(&heap_table, |heap| {
                let ctids = versions
                    .into_iter()
                    .map(|v| match v.old {
                        Some((old, old_len, hot, id)) => {
                            heap.update(pid, old, old_len, v.len, hot, xid, id)
                        }
                        None => heap.insert(pid, xid, v.len),
                    })
                    .collect::<Vec<_>>();
                (ctids, std::mem::take(&mut heap.accessed))
            })?;
            let (mut inserted, mut updated, mut hot) = (0, 0, 0);
            for (old, ctid) in olds.iter().zip(&ctids) {
                match old {
                    None => inserted += 1,
                    Some((old_ctid, indexed_same)) => {
                        updated += 1;
                        if *indexed_same && old_ctid.is_some_and(|o| o / 65536 == ctid / 65536) {
                            hot += 1;
                        }
                    }
                }
            }
            // The buffers the writes read, as `RelationGetBufferForTuple`,
            // `heap_update` and the free space map read them (RP-239).
            self.stat_pages(&heap_table, accessed);
            self.stat_writes(&heap_table, inserted, updated, hot);
            // Every version that is not heap-only takes a tuple in each of
            // the table's B-trees (RP-196).
            // With the old version, so an index can tell its key was left
            // unchanged (`indexUnchanged`, which allows bottom-up deletion).
            let indexed: Vec<(&Document, Option<&Document>, i64)> = idx
                .iter()
                .zip(olds.iter().zip(&ctids))
                .filter(|(_, (old, ctid))| match old {
                    None => true,
                    Some((old_ctid, same)) => {
                        !(*same && old_ctid.is_some_and(|o| o / 65536 == **ctid / 65536))
                    }
                })
                .map(|(i, (_, ctid))| (&rows[*i].1, rows[*i].0.as_ref(), *ctid))
                .collect();
            self.btree_insert_versions(&heap_table, &indexed)?;
            // The versions this transaction made, for others' conflicts
            // out: each new one, and each old one it deleted.
            for (i, ctid) in idx.iter().zip(&ctids) {
                let (old, new) = &rows[*i];
                if let Some(old) = old {
                    if let Some(c) = ctid_of(old) {
                        self.ssi_version(&heap_table, c, false, Some(old));
                    }
                }
                self.ssi_version(&heap_table, *ctid, true, Some(new));
            }
            for (i, ctid) in idx.into_iter().zip(ctids) {
                // An xid is 32 bits (the low half of the xid8).
                stamp(&mut rows[i].1, (xid & 0xFFFF_FFFF) as i64, cid, ctid);
            }
        }
        Ok(())
    }

    /// `VACUUM FULL` / `CLUSTER`: the rows, in `rows`' order, copied to a
    /// new heap (`rewrite_heap_tuple`, which fills each page in turn and
    /// keeps no free space map): each keeps its xmin and command, and takes
    /// the next place.
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
                *heap = Heap {
                    reserve: heap.reserve,
                    ..Heap::default()
                };
                // CLUSTER's order is an index's, not the stored one.
                heap.disordered = true;
                let ctids = needs
                    .iter()
                    .map(|n| {
                        let (b, o) = heap.place(i32::MIN, *n, false);
                        tid(b, o)
                    })
                    .collect::<Vec<_>>();
                heap.targets.clear();
                // A rewrite writes its new heap past the buffers.
                heap.accessed.clear();
                heap.fsm.touched.clear();
                ctids
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
        let len = tuple_len(def, row) as u32;
        let heap_table = self.heap_of(table, def, row)?;
        self.with_heap(&heap_table, |heap| heap.release(ctid, len))
    }

    /// The rows the statement's deletes removed (since `mark`, what
    /// [`Self::watch_deletes`] returned) are dead tuples of its
    /// transaction's: each page pruned first, as the DELETE's scan read it.
    /// `checked`: the statement succeeded, so SSI checks its deletes (a
    /// failed one's are rolled back).
    pub(crate) fn note_deleted(&self, mark: usize, checked: bool) -> PgWireResult<()> {
        let deleted: Vec<Deleted> = DELETED.with(|d| {
            let mut d = d.borrow_mut();
            let from = mark.min(d.len());
            d.drain(from..).collect()
        });
        if deleted.is_empty() {
            return Ok(());
        }
        let (xid, _) = self.writer();
        // Each heap's dead tuples: `(ctid, length, _id)`.
        type Dead = Vec<(i64, u32, Bson)>;
        let mut by_heap: Vec<(String, Dead)> = Vec::new();
        // SSI's check of each delete, and the version it leaves.
        let mut ssi_deleted: Vec<(String, i64, Option<Document>)> = Vec::new();
        for d in deleted {
            let Some(def) = self.lookup(&d.table) else {
                continue;
            };
            let (heap_table, len) = match (d.len, &d.row) {
                (Some(len), _) => (d.table.clone(), len),
                (None, Some(row)) => (
                    self.heap_of(&d.table, &def, row)?,
                    tuple_len(&def, row) as u32,
                ),
                (None, None) => continue,
            };
            if checked && d.ssi.is_some() {
                ssi_deleted.push((heap_table.clone(), d.ctid, d.ssi.clone()));
            }
            match by_heap.iter_mut().find(|(t, _)| *t == heap_table) {
                Some((_, v)) => v.push((d.ctid, len, d.id)),
                None => by_heap.push((heap_table, vec![(d.ctid, len, d.id)])),
            }
        }
        for (table, ctid, doc) in &ssi_deleted {
            self.ssi_write(table, Some(*ctid))?;
            self.ssi_version(table, *ctid, false, doc.as_ref());
        }
        for (heap_table, tuples) in by_heap {
            let ctids: Vec<i64> = tuples.iter().map(|(c, _, _)| *c).collect();
            self.prune_pages(&heap_table, Some(&ctids))?;
            self.stat_deletes(&heap_table, tuples.len() as i64);
            // `heap_delete` reads each row's page (RP-239).
            self.stat_pages(&heap_table, ctids.iter().map(|c| c / 65536));
            self.with_heap(&heap_table, |heap| {
                for (ctid, len, id) in tuples {
                    heap.delete(ctid, len, xid, id);
                }
            })?;
        }
        Ok(())
    }

    /// Before a statement: the store reports its deletes here, with the
    /// definition of the table it names. Returns the mark
    /// [`Self::note_deleted`] takes (a statement a trigger runs is inside
    /// another's).
    pub(crate) fn watch_deletes(&self, table: Option<&str>) -> usize {
        thread_local! {
            static ON_THIS_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        if !ON_THIS_THREAD.with(std::cell::Cell::get) {
            secantus_storage::set_delete_observer(Some(Box::new(observe_delete)));
            ON_THIS_THREAD.with(|c| c.set(true));
        }
        let mark = DELETED.with(|d| d.borrow().len());
        // The outermost statement: definitions an earlier one held may have
        // changed since.
        if mark == 0 {
            DELETE_DEFS.with(|defs| defs.borrow_mut().clear());
        }
        if let Some(t) = table.filter(|t| self.has_heap(t)) {
            if let Some(def) = self.lookup(t) {
                DELETE_DEFS.with(|defs| defs.borrow_mut().insert(t.to_string(), def));
            }
        }
        mark
    }

    /// The TIDs of `table`'s heap whose HOT chains are dead to every
    /// snapshot, for bottom-up index deletion (RP-196): each ended deleter
    /// settled first, as [`Self::prune_pages`] settles them, without a
    /// prune. Taken before the indexes are written, so no B-tree is held
    /// while the heap is read.
    pub(crate) fn dead_index_tids(
        &self,
        table: &str,
    ) -> PgWireResult<std::collections::HashSet<i64>> {
        if !self.has_heap(table) {
            return Ok(Default::default());
        }
        let (running, horizon) = visibility();
        let is_running = |x: u64| running.contains(&x);
        let candidates: Vec<(u32, Vec<(u16, Bson)>)> = self.with_heap(table, |heap| {
            (0..heap.nblocks())
                .map(|b| (b, heap.unsettled(b, horizon, &is_running)))
                .filter(|(_, u)| !u.is_empty())
                .collect()
        })?;
        let ids: Vec<Bson> = candidates
            .iter()
            .flat_map(|(_, u)| u.iter().map(|(_, id)| id.clone()))
            .collect();
        let present: std::collections::HashMap<String, i64> = if ids.is_empty() {
            Default::default()
        } else {
            self.storage
                .find_matching(self.db(), table, &bson::doc! { "_id": { "$in": ids } })
                .map_err(|e| Self::storage_err("could not read", e))?
                .iter()
                .filter_map(|raw| {
                    let d = crate::decode_doc(raw).ok()?;
                    Some((d.get("_id")?.to_string(), ctid_of(&d)?))
                })
                .collect()
        };
        self.with_heap(table, |heap| {
            for (block, unsettled) in candidates {
                let outcome: Vec<(u16, bool)> = unsettled
                    .iter()
                    .map(|(off, id)| {
                        let here = present.get(&id.to_string()) == Some(&tid(block, *off));
                        (*off, !here)
                    })
                    .collect();
                heap.settle(block, &outcome);
            }
            heap.vacuumable_tids(horizon, &is_running)
        })
    }

    /// `heap_page_prune_opt` on the pages a read of `table` met: every
    /// page (a sequential scan, `ctids` None) or the given rows' pages.
    /// A tuple whose deleter has ended is settled first -- committed if its
    /// row is gone from the store or has moved, rolled back if it is still
    /// here. Only a deleter older than the removal horizon is settled: it
    /// ended before any snapshot still held was taken, this statement's
    /// own among them, so the store's answer here is its outcome.
    pub(crate) fn prune_pages(&self, table: &str, ctids: Option<&[i64]>) -> PgWireResult<()> {
        if !self.has_heap(table) {
            return Ok(());
        }
        let (running, horizon) = visibility();
        let is_running = |x: u64| running.contains(&x);
        // The pages a prune could act on, and their unsettled tuples.
        let candidates: Vec<(u32, Vec<(u16, Bson)>)> = self.with_heap(table, |heap| {
            let blocks: Vec<u32> = match ctids {
                None => heap.prunable_blocks(),
                Some(c) => {
                    let mut b: Vec<u32> = c.iter().filter_map(|c| untid(*c).map(|t| t.0)).collect();
                    b.sort_unstable();
                    b.dedup();
                    b
                }
            };
            blocks
                .into_iter()
                .filter(|b| heap.may_prune(*b, horizon))
                .map(|b| (b, heap.unsettled(b, horizon, &is_running)))
                .collect()
        })?;
        if candidates.is_empty() {
            return Ok(());
        }
        let ids: Vec<Bson> = candidates
            .iter()
            .flat_map(|(_, u)| u.iter().map(|(_, id)| id.clone()))
            .collect();
        let present: std::collections::HashMap<String, i64> = if ids.is_empty() {
            Default::default()
        } else {
            self.storage
                .find_matching(self.db(), table, &bson::doc! { "_id": { "$in": ids } })
                .map_err(|e| Self::storage_err("could not read", e))?
                .iter()
                .filter_map(|raw| {
                    let d = crate::decode_doc(raw).ok()?;
                    Some((d.get("_id")?.to_string(), ctid_of(&d)?))
                })
                .collect()
        };
        self.with_heap(table, |heap| {
            for (block, unsettled) in candidates {
                let outcome: Vec<(u16, bool)> = unsettled
                    .iter()
                    .map(|(off, id)| {
                        let here = present.get(&id.to_string()) == Some(&tid(block, *off));
                        (*off, !here)
                    })
                    .collect();
                heap.settle(block, &outcome);
                heap.prune_opt(block, horizon, &is_running);
            }
        })
    }

    /// Plain VACUUM of `table`'s heap (`heap_vacuum_rel`): each page
    /// pruned, its dead tuples removed, its free space recorded, the empty
    /// pages at the end truncated away. Its scan reads every row, which
    /// settles every deleter.
    pub(crate) fn vacuum_heap(&self, table: &str) -> PgWireResult<()> {
        let Some(def) = self.lookup(table) else {
            return Ok(());
        };
        let heaps: Vec<String> = if crate::partition::is_partitioned(&def) {
            self.leaf_partitions(table)?
        } else if self.has_heap(table) {
            vec![table.to_string()]
        } else {
            Vec::new()
        };
        if heaps.is_empty() {
            return Ok(());
        }
        let rows: Vec<Document> = self
            .storage
            .find_matching(self.db(), table, &Document::new())
            .map_err(|e| Self::storage_err("could not read", e))?
            .iter()
            .filter_map(|raw| crate::decode_doc(raw).ok())
            .collect();
        // A table without a primary key has no index the row id's `_id_`
        // stands for.
        let has_indexes = def.columns.iter().any(|c| c.pk)
            || self
                .storage
                .list_indexes(self.db(), table)
                .is_ok_and(|ix| ix.iter().any(|i| i.get_str("name") != Ok("_id_")));
        let truncate = self.reloption(table, "vacuum_truncate").is_none_or(|v| {
            !matches!(
                v.to_ascii_lowercase().as_str(),
                "false" | "off" | "no" | "0"
            )
        });
        let (running, horizon) = visibility();
        for heap_table in heaps {
            let mut live: std::collections::HashMap<i64, u32> = Default::default();
            for row in &rows {
                if self.heap_of(table, &def, row)? != heap_table {
                    continue;
                }
                if let Some(ctid) = ctid_of(row) {
                    live.insert(ctid, tuple_len(&def, row) as u32);
                }
            }
            let index_pass = self.with_heap(&heap_table, |heap| {
                heap.settle_all(&|c| live.contains_key(&c), &|x| running.contains(&x));
                heap.vacuum(
                    horizon,
                    &|x| running.contains(&x),
                    &live,
                    has_indexes,
                    truncate,
                )
            })?;
            // The index pass: each B-tree loses the dead TIDs, and records
            // its pages and the TIDs left (`btvacuumcleanup`, RP-196).
            let visited = match index_pass {
                Some(dead) => {
                    let dead: std::collections::HashSet<i64> = dead.into_iter().collect();
                    self.btree_bulk_delete(&heap_table, &dead)?
                }
                None => self.btree_cleanup(&heap_table)?,
            };
            for (index, pages, tuples) in visited {
                self.record_index_size(&heap_table, &index, pages, tuples as f64)?;
            }
        }
        Ok(())
    }

    /// The partitions of `table` that hold rows, at any depth.
    pub(crate) fn leaf_partitions(&self, table: &str) -> PgWireResult<Vec<String>> {
        let mut out = Vec::new();
        let mut todo = vec![table.to_string()];
        while let Some(t) = todo.pop() {
            for p in self.partitions_of(&t)? {
                if crate::partition::is_partitioned(&p) {
                    todo.push(p.name);
                } else {
                    out.push(p.name);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// The pages `table`'s heap has (`RelationGetNumberOfBlocks`).
    /// The heap's tuples a VACUUM or ANALYZE counts dead: deleted (by a
    /// committed or a running transaction) or inserted by one that rolled
    /// back, still on their pages.
    pub(crate) fn dead_tuple_count(&self, table: &str) -> i64 {
        if !self.has_heap(table) {
            return 0;
        }
        self.with_heap_read(table, |heap| {
            heap.pages
                .iter()
                .flat_map(|p| p.lps.values())
                .filter(|lp| matches!(lp, Lp::Normal(t) if t.xmax.is_some() || t.xmin_aborted))
                .count() as i64
        })
        .unwrap_or(0)
    }

    /// The tuples on the heap's pages, live or dead: what a B-tree over
    /// the table holds an entry for until VACUUM.
    pub(crate) fn heap_tuple_count(&self, table: &str) -> i64 {
        if !self.has_heap(table) {
            return 0;
        }
        self.with_heap_read(table, |heap| {
            heap.pages
                .iter()
                .flat_map(|p| p.lps.values())
                .filter(|lp| matches!(lp, Lp::Normal(_)))
                .count() as i64
        })
        .unwrap_or(0)
    }

    /// The heap's heap-only tuples, which no index points at.
    pub(crate) fn heap_only_tids(&self, table: &str) -> std::collections::HashSet<i64> {
        if !self.has_heap(table) {
            return Default::default();
        }
        self.with_heap_read(table, |heap| {
            heap.pages
                .iter()
                .enumerate()
                .flat_map(|(b, p)| {
                    p.lps.iter().filter_map(move |(o, lp)| match lp {
                        Lp::Normal(t) if t.heap_only => Some(tid(b as u32, *o)),
                        _ => None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    pub(crate) fn heap_blocks(&self, table: &str) -> PgWireResult<u32> {
        self.with_heap_read(table, |h| h.nblocks())
    }

    /// ANALYZE updates the relation's statistics in place, which
    /// invalidates every backend's target block.
    pub(crate) fn heap_invalidate(&self, table: &str) -> PgWireResult<()> {
        if !self.has_heap(table) {
            return Ok(());
        }
        self.with_heap(table, |h| h.targets.clear())
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

/// The blocks `rows` of `def` fill when inserted one after another into an
/// empty heap, as `initdb` lays out a catalog (RP-214, Joe 2026-10-24):
/// each tuple takes its MAXALIGNed length and a 4-byte line pointer on a
/// page whose header is 24 bytes, a catalog's fillfactor being 100, and at
/// most `MaxHeapTuplesPerPage` (291) tuples go on a page.
pub(crate) fn fresh_blocks(def: &TableDef, rows: &[Document]) -> u32 {
    const PAGE: usize = 8192;
    const HEADER: usize = 24;
    const MAX_TUPLES: usize = 291;
    let mut blocks = 0u32;
    let (mut free, mut on_page) = (0usize, MAX_TUPLES);
    for r in rows {
        let need = maxalign(tuple_len(def, r)) + 4;
        if on_page == MAX_TUPLES || need > free {
            blocks += 1;
            free = PAGE - HEADER;
            on_page = 0;
        }
        free -= need.min(free);
        on_page += 1;
    }
    blocks
}

impl PgHandler {
    /// The heap the last checkpoint wrote for `table`, if any (RP-193).
    fn saved_heap(&self, table: &str) -> Option<Heap> {
        let oid = self.relation_oid(table)?;
        let docs = self
            .storage
            .find_matching(
                self.db(),
                crate::heap_store::COLLECTION,
                &bson::doc! { "_id": oid.to_string() },
            )
            .ok()?;
        let d = crate::decode_doc(docs.first()?).ok()?;
        Some(crate::heap_store::heap_from(&d))
    }

    /// Each stored row of `table`'s place and length, `(block, offset,
    /// length)`.
    fn heap_row_places(&self, table: &str) -> Vec<(u32, u16, u32)> {
        let Some(def) = self.lookup(table) else {
            return Vec::new();
        };
        let (def, rows) = match self.partition_rows(&def) {
            Ok(Some((root, rows))) => (root, rows),
            _ => {
                let rows = self
                    .storage
                    .find_matching(self.db(), table, &Document::new())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|b| crate::decode_doc(b).ok())
                    .collect();
                (def, rows)
            }
        };
        rows.iter()
            .filter_map(|row| {
                let (b, o) = ctid_of(row).and_then(untid)?;
                Some((b, o, tuple_len(&def, row) as u32))
            })
            .collect()
    }

    /// Start (`Some(use_fsm)`) or end (`None`) this backend's bulk insert
    /// into `table`'s heap (`GetBulkInsertState`, RP-239): COPY keeps the
    /// free space map, CREATE TABLE AS does not (`TABLE_INSERT_SKIP_FSM`),
    /// and each holds the page it writes, which it does not read again.
    pub(crate) fn bulk_insert(&self, table: &str, use_fsm: Option<bool>) {
        if table.starts_with("__") || !self.has_heap(table) {
            return;
        }
        let pid = self.pid();
        let _ = self.with_heap(table, |heap| match use_fsm {
            Some(use_fsm) => {
                heap.bulk.insert(
                    pid,
                    crate::heap_page::Bulk {
                        use_fsm,
                        current: None,
                    },
                );
            }
            None => {
                heap.bulk.remove(&pid);
            }
        });
    }

    /// Write this store's changed heaps (RP-193): at most once a second
    /// from the end of a message cycle, every one at a clean shutdown.
    ///
    /// On a thread of its own, as PostgreSQL's checkpointer is a process of
    /// its own: run in the message cycle, its writes -- durable, and slow on
    /// a disk that syncs -- held back the answer of whichever statement came
    /// due, a cancelled one's included (0.7 s on CI).
    pub(crate) fn checkpoint_heaps(&self, force: bool) {
        if force {
            flush_store(&self.storage, true);
            return;
        }
        let store = std::sync::Arc::as_ptr(&self.storage) as usize;
        if !crate::heap_store::due(store, false) {
            return;
        }
        let storage = self.storage.clone();
        CHECKPOINTS_LIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let spawned = std::thread::Builder::new()
            .name("secantusd-pg checkpointer".into())
            .spawn(move || {
                let running = CHECKPOINTER.lock().unwrap_or_else(|e| e.into_inner());
                flush_changed(&storage, false);
                // The store's last handle may be waiting at shutdown.
                drop(storage);
                drop(running);
                CHECKPOINTS_LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            });
        if let Err(e) = spawned {
            CHECKPOINTS_LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            eprintln!("secantusd-pg: could not start a checkpoint: {e}");
        }
    }

    /// A table is dropped: its heap leaves this process. The saved copy
    /// stays -- a DROP rolled back reads it back -- and an oid is never
    /// drawn twice in a database, so nothing else finds it.
    pub(crate) fn forget_heap(&self, table: &str) {
        let key = self.heap_key(table);
        heaps()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }
}

/// A database is gone (DROP DATABASE): every heap of its tables (RP-233).
pub(crate) fn forget_database(storage: &std::sync::Arc<secantus_storage::Storage>, db: &str) {
    let store = std::sync::Arc::as_ptr(storage) as usize;
    let prefix = format!("heap:{db}:");
    heaps()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(s, k), _| !(*s == store && k.starts_with(&prefix)));
}

/// Write `storage`'s changed heaps, each in its database's saved-heap
/// collection, outside any user transaction. A write that fails is reported
/// and the heap left marked, for the next checkpoint.
pub(crate) fn flush_store(storage: &std::sync::Arc<secantus_storage::Storage>, force: bool) {
    // A clean shutdown's: after every background checkpoint has ended and
    // let go of the store, which then closes.
    if force {
        while CHECKPOINTS_LIVE.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    // After a checkpoint still running, whose writes this one follows.
    let _running = CHECKPOINTER.lock().unwrap_or_else(|e| e.into_inner());
    let store = std::sync::Arc::as_ptr(storage) as usize;
    if !crate::heap_store::due(store, force) {
        return;
    }
    flush_changed(storage, force);
}

/// One checkpoint at a time: a background one, or the clean shutdown's.
static CHECKPOINTER: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Background checkpoints started and not yet ended.
static CHECKPOINTS_LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `flush_store`'s writes, once due.
fn flush_changed(storage: &std::sync::Arc<secantus_storage::Storage>, force: bool) {
    let store = std::sync::Arc::as_ptr(storage) as usize;
    // `(key, database, oid, document)` of every changed heap.
    let changed: Vec<((usize, String), String, i64, Document)> = {
        let mut all = heaps().lock().unwrap_or_else(|e| e.into_inner());
        all.iter_mut()
            .filter(|((s, _), h)| *s == store && h.dirty)
            .filter_map(|(key, h)| {
                let rest = key.1.strip_prefix("heap:")?;
                let (db, oid) = rest.rsplit_once(':')?;
                let oid: i64 = oid.parse().ok()?;
                h.dirty = false;
                Some((
                    key.clone(),
                    db.to_string(),
                    oid,
                    crate::heap_store::heap_doc(oid, h),
                ))
            })
            .collect()
    };
    for (key, db, oid, d) in changed {
        if oid == 0 {
            continue;
        }
        let bytes = match crate::encode_doc(&d) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("secantusd-pg: could not encode the heap of table {oid} in {db}: {e}");
                continue;
            }
        };
        let written = storage.outside_user_transaction(|| -> secantus_storage::Result<()> {
            if !storage.collection_exists(&db, crate::heap_store::COLLECTION)? {
                storage.create_collection(&db, crate::heap_store::COLLECTION)?;
            }
            storage.delete_matching(
                &db,
                crate::heap_store::COLLECTION,
                &bson::doc! { "_id": oid.to_string() },
                0,
                &Document::new(),
                None,
            )?;
            storage.insert(
                &db,
                crate::heap_store::COLLECTION,
                vec![bytes.clone()],
                true,
            )?;
            Ok(())
        });
        if let Err(e) = written {
            eprintln!("secantusd-pg: could not write the heap of table {oid} in {db}: {e}");
            if let Some(h) = heaps()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&key)
            {
                h.dirty = true;
            }
        }
    }
    // The indexes' pages, on a checkpoint of their own (RP-196).
    crate::btree::flush_store(storage, force);
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
        let mut heap = Heap::default();
        let ctids: Vec<i64> = (1..=200).map(|_| heap.insert(1, 800, need)).collect();
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
        let mut heap = Heap::default();
        let first = heap.insert(1, 800, 32);
        heap.insert(1, 800, 32);
        assert!(!heap.disordered);
        assert_eq!(
            heap.update(1, Some(first), 32, 32, true, 800, Bson::Int32(1)),
            3
        );
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
