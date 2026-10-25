//! Where a session's work meets SSI (`ssi.rs`, RP-061): the transaction's
//! registration at its snapshot, its reads' predicate locks and conflicts
//! out, its writes' conflicts in, and its commit and end -- each where
//! PostgreSQL's executor and access methods call into `predicate.c`.

use bson::Document;
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};

use crate::btree_model::Shape;
use crate::ssi::{self, Failure, Snap};
use crate::{xids, PgHandler};

thread_local! {
    /// The session on this thread runs a serializable transaction that
    /// SSI tracks: deletes keep the rows they remove (`heap::observe_delete`).
    pub(crate) static SSI_CAPTURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A 40001 with PostgreSQL's message, detail and hint.
fn failure(f: Failure) -> PgWireError {
    let mut info = ErrorInfo::new("ERROR".into(), "40001".into(), Failure::MESSAGE.into());
    info.detail = Some(f.0);
    info.hint = Some(Failure::HINT.into());
    PgWireError::UserError(Box::new(info))
}

/// A filter's conditions, field by field, through its `$and`s: `None` for
/// one holding anything else at the top (an `$or`). The storage's own
/// markers (`__us__...`) are not the WHERE's and are left out.
fn flat_conditions(filter: &Document) -> Option<Vec<(String, bson::Bson)>> {
    let mut out = Vec::new();
    let mut docs = vec![filter];
    while let Some(doc) = docs.pop() {
        for (k, v) in doc {
            if k == "$and" {
                for item in v.as_array()? {
                    docs.push(item.as_document()?);
                }
            } else if k == "$or" {
                out.push(or_range(v.as_array()?)?);
            } else if k.starts_with('$') {
                return None;
            } else if !k.starts_with("__us") {
                out.push((k.clone(), v.clone()));
            }
        }
    }
    Some(out)
}

/// An `$or` the lowering of a comparison on a timestamp makes -- the range,
/// beside the storage's spelling of an infinity (`"-infinity"`) and of a
/// value finer than its milliseconds (a `__us_` marker) -- as the one
/// range on its field it is: the union of its branches' bounds.
fn or_range(branches: &[bson::Bson]) -> Option<(String, bson::Bson)> {
    let mut field: Option<String> = None;
    let mut ops: Vec<(String, bson::Bson)> = Vec::new();
    let mut stack: Vec<&bson::Bson> = branches.iter().collect();
    while let Some(b) = stack.pop() {
        let d = b.as_document()?;
        if let Ok(inner) = d.get_array("$or") {
            if d.len() == 1 {
                stack.extend(inner.iter());
                continue;
            }
        }
        if d.keys().any(|k| k.starts_with("__us")) {
            continue;
        }
        if d.len() != 1 {
            return None;
        }
        let (f, v) = d.iter().next()?;
        if f.starts_with('$') {
            return None;
        }
        if field.get_or_insert_with(|| f.clone()) != f {
            return None;
        }
        match v {
            bson::Bson::String(s) if s == "infinity" || s == "-infinity" => {}
            bson::Bson::Document(o) if o.keys().all(|k| k.starts_with('$')) => {
                for (op, x) in o {
                    ops.push((op.clone(), x.clone()));
                }
            }
            other => ops.push(("$eq".into(), other.clone())),
        }
    }
    // A branch with no bound on a side leaves that side unbounded.
    let lower = ops
        .iter()
        .all(|(o, _)| matches!(o.as_str(), "$gt" | "$gte" | "$eq"));
    let upper = ops
        .iter()
        .all(|(o, _)| matches!(o.as_str(), "$lt" | "$lte" | "$eq"));
    let mut merged = Document::new();
    if lower {
        if let Some((o, x)) = ops
            .iter()
            .filter(|(o, _)| o != "$lt" && o != "$lte")
            .min_by(|a, b| secantus_core::order::cmp(&a.1, &b.1))
        {
            merged.insert(if o == "$gt" { "$gt" } else { "$gte" }, x.clone());
        }
    }
    if upper {
        if let Some((o, x)) = ops
            .iter()
            .filter(|(o, _)| o != "$gt" && o != "$gte")
            .max_by(|a, b| secantus_core::order::cmp(&a.1, &b.1))
        {
            merged.insert(if o == "$lt" { "$lt" } else { "$lte" }, x.clone());
        }
    }
    if merged.is_empty() {
        return None;
    }
    Some((field?, bson::Bson::Document(merged)))
}

/// A row as the store holds it: a key spelled with a dot (`_id.room_id`, a
/// compound primary key's column before the store nests it) nested, so a
/// filter's path reaches it.
fn nested(doc: &Document) -> Document {
    let mut out = Document::new();
    for (k, v) in doc {
        match k.split_once('.') {
            Some((head, rest)) if !head.is_empty() && !rest.is_empty() => {
                let mut inner = match out.remove(head) {
                    Some(bson::Bson::Document(d)) => d,
                    _ => Document::new(),
                };
                inner.insert(rest, v.clone());
                out.insert(head, bson::Bson::Document(nested(&inner)));
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

/// One end of an index range: the key, and whether it is included.
type Bound = Option<(bson::Bson, bool)>;

/// The index a SERIALIZABLE scan reads, as the planner chose it.
enum ScanIndex {
    Btree(std::sync::Arc<crate::btree::IndexShape>),
    Hash(crate::hash_index::HashShape),
}

impl PgHandler {
    fn ssi_key(&self) -> usize {
        std::sync::Arc::as_ptr(&self.storage) as usize
    }

    fn ssi_pid(&self) -> i32 {
        self.backend_pid.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The database, as a predicate lock target names it.
    fn ssi_db(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.db().hash(&mut h);
        h.finish()
    }

    fn ssi<T>(&self, f: impl FnOnce(&mut ssi::State) -> T) -> T {
        ssi::with(self.ssi_key(), xids::next_xid(&self.storage), f)
    }

    fn setting_is(&self, name: &str, value: &str) -> bool {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .is_some_and(|v| v == value)
    }

    /// `GetSerializableTransactionSnapshot`: a SERIALIZABLE transaction
    /// takes its snapshot (once; a read-only one with no writable
    /// serializable transaction running opts out). A READ ONLY DEFERRABLE
    /// one waits for a safe snapshot (`GetSafeSnapshot`): until every
    /// concurrent read/write transaction has finished, retrying on a new
    /// snapshot when one of them made this one unsafe; it then runs with
    /// no predicate locking at all.
    pub(crate) fn ssi_begin(
        &self,
        handle: &mut secantus_storage::UserTransactionHandle,
    ) -> PgWireResult<()> {
        if !self.setting_is("transaction_isolation", "serializable") {
            return Ok(());
        }
        if self
            .ssi_registered
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(());
        }
        let pid = self.ssi_pid();
        let read_only = self.setting_is("transaction_read_only", "on");
        let deferrable = read_only && self.setting_is("transaction_deferrable", "on");
        loop {
            let (xmin, xmax, xip) = xids::snapshot_of(&self.storage, pid);
            let tracked = self
                .ssi(|s| s.register(pid, Snap { xmin, xmax, xip }, read_only))
                .is_some();
            if !tracked || !deferrable {
                SSI_CAPTURE.with(|c| c.set(tracked));
                return Ok(());
            }
            self.ssi(|s| s.set_deferrable_waiting(pid, true));
            let safe = loop {
                match self.ssi(|s| s.deferrable_state(pid)) {
                    None => break true,
                    Some((true, false)) => break true,
                    Some((_, true)) => break false,
                    Some(_) => {}
                }
                if let Err(e) = self.check_cancel() {
                    self.ssi(|s| s.release(pid, false, false));
                    return Err(e);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            self.ssi(|s| s.set_deferrable_waiting(pid, false));
            // Safe: no checks from here on. Unsafe: try a new snapshot.
            self.ssi(|s| s.release(pid, false, safe));
            if safe {
                SSI_CAPTURE.with(|c| c.set(false));
                return Ok(());
            }
            self.storage
                .refresh_user_snapshot(handle)
                .map_err(|e| Self::storage_err("could not refresh the snapshot", e))?;
            xids::rehold_snapshot(&self.storage, pid);
        }
    }

    /// Does SSI track this session's transaction?
    /// `ssi_begin` for a read outside the statement's execution (a
    /// subquery read while planning), with the snapshot hold the first
    /// statement takes.
    pub(crate) fn ssi_begin_reading(
        &self,
        handle: &mut secantus_storage::UserTransactionHandle,
    ) -> PgWireResult<()> {
        if !self.setting_is("transaction_isolation", "serializable") {
            return Ok(());
        }
        xids::hold_snapshot(&self.storage, self.ssi_pid());
        self.ssi_begin(handle)
    }

    pub(crate) fn ssi_tracking(&self) -> bool {
        let pid = self.ssi_pid();
        self.ssi(|s| s.tracking(pid))
    }

    /// `PreCommit_CheckForSerializationFailure`.
    pub(crate) fn ssi_pre_commit(&self) -> PgWireResult<()> {
        let pid = self.ssi_pid();
        self.ssi(|s| s.pre_commit(pid)).map_err(failure)
    }

    /// `ReleasePredicateLocks` as the transaction ends: committed, or
    /// not (a rollback, or the end of a transaction whose commit was
    /// already released).
    pub(crate) fn ssi_end(&self, committed: bool) {
        self.ssi_registered
            .store(false, std::sync::atomic::Ordering::Relaxed);
        SSI_CAPTURE.with(|c| c.set(false));
        let pid = self.ssi_pid();
        self.ssi(|s| s.release(pid, committed, false));
    }

    /// The transaction's xid, registered once it has one.
    fn ssi_xid(&self) {
        let pid = self.ssi_pid();
        if let Some(xid) = xids::xid_of(&self.storage, pid) {
            self.ssi(|s| {
                if let Some(sx) = s.mine(pid) {
                    s.set_xid(sx, xid);
                }
            });
        }
    }

    /// A relation SSI watches: a stored table's or index's, not a temporary
    /// one (`PredicateLockingNeededForRelation`).
    fn ssi_relation(&self, table: &str) -> Option<i64> {
        if table.starts_with("__") {
            return None;
        }
        let def = self.lookup(table)?;
        if def.temp {
            return None;
        }
        self.relation_oid(table)
    }

    /// A scan of `table` through `filter`, as PostgreSQL's plan reads it:
    /// by a sequential scan, or by an index (a plain or a bitmap index
    /// scan), the cheapest as `costsize` prices them for a table with no
    /// statistics, with `enable_*` honoured.
    pub(crate) fn ssi_scan(&self, table: &str, filter: &Document) -> PgWireResult<()> {
        if !self.ssi_tracking() {
            return Ok(());
        }
        let Some(rel) = self.ssi_relation(table) else {
            return Ok(());
        };
        match self.ssi_scan_index(table, filter) {
            Some(ScanIndex::Btree(shape)) => self.ssi_index_scan(table, rel, &shape, filter),
            Some(ScanIndex::Hash(shape)) => self.ssi_hash_scan(table, rel, &shape, filter),
            None => self.ssi_seq_scan(table),
        }
    }

    /// A hash index scan of `table` through `shape` (`_hash_first`): the
    /// primary page of each key's bucket, locked; then each heap tuple of
    /// those keys, as an index scan fetches them.
    fn ssi_hash_scan(
        &self,
        table: &str,
        rel: i64,
        shape: &crate::hash_index::HashShape,
        filter: &Document,
    ) -> PgWireResult<()> {
        let pid = self.ssi_pid();
        let db = self.ssi_db();
        let conds = flat_conditions(filter).unwrap_or_default();
        let mut values: Vec<bson::Bson> = Vec::new();
        for (k, v) in &conds {
            if k != &shape.field {
                continue;
            }
            match v {
                bson::Bson::Document(ops) if ops.keys().all(|o| o.starts_with('$')) => {
                    if let Some(x) = ops.get("$eq") {
                        values.push(x.clone());
                    }
                    if let Some(bson::Bson::Array(xs)) = ops.get("$in") {
                        values.extend(xs.iter().cloned());
                    }
                }
                other => values.push(other.clone()),
            }
        }
        let buckets: Vec<u32> = values
            .iter()
            .filter_map(|v| shape.hash_value(v))
            .map(|h| self.with_hash_meta(table, shape, |m| m.bucket(h)))
            .collect();
        let index_filter = bson::doc! { shape.field.clone(): { "$in": values } };
        let rows = self
            .storage
            .find_matching(self.db(), table, &index_filter)
            .map_err(|e| Self::storage_err("could not read", e))?;
        let mut tuples: Vec<(i64, Option<u64>)> = rows
            .iter()
            .filter_map(|b| {
                let d = crate::decode_doc(b).ok()?;
                let ctid = crate::heap::ctid_of(&d)?;
                let xmin = d
                    .get("__sys_xmin")
                    .and_then(crate::bson_i64)
                    .map(|x| x as u64);
                Some((ctid, xmin))
            })
            .collect();
        tuples.sort_unstable();
        let empty = Document::new();
        let examined = |_: i64, doc: Option<&Document>| {
            doc.is_some_and(|d| {
                secantus_core::query::matches(d, &index_filter, &empty, None).unwrap_or(false)
            })
        };
        self.ssi(|s| {
            for b in &buckets {
                s.lock_page(pid, db, shape.oid, crate::hash_index::bucket_page(*b));
            }
            let any = !tuples.is_empty() || s.has_versions(db, rel, &examined);
            s.scan_conflicts_out(pid, db, rel, &examined, any)?;
            for (ctid, xmin) in &tuples {
                s.lock_tuple(pid, db, rel, *ctid, *xmin);
            }
            Ok(())
        })
        .map_err(failure)
    }

    /// The index PostgreSQL's cheapest path for `filter` over `table`
    /// reads, or `None` for a sequential scan.
    fn ssi_scan_index(&self, table: &str, filter: &Document) -> Option<ScanIndex> {
        use secantus_pgplan::{costsize, hashagg};
        if filter.is_empty() {
            return None;
        }
        let count = self
            .storage
            .count_matching(self.db(), table, &Document::new(), None)
            .ok()?;
        let pt = self.planner_table(table, count)?;
        let flat: Document =
            flat_conditions(filter)?
                .into_iter()
                .fold(Document::new(), |mut d, (k, v)| {
                    // Two conditions on one field: both, in an `$and`.
                    match d.remove(&k) {
                        Some(prev) => {
                            let both = vec![
                                bson::Bson::Document(bson::doc! { k.clone(): prev }),
                                bson::Bson::Document(bson::doc! { k.clone(): v }),
                            ];
                            let mut and = d.get_array("$and").cloned().unwrap_or_default();
                            and.extend(both);
                            d.insert("$and", and);
                        }
                        None => {
                            d.insert(k, v);
                        }
                    }
                    d
                });
        let quals = costsize::conjuncts(&flat)?;
        let rel = costsize::Rel {
            pages: pt.pages,
            tuples: pt.tuples,
        };
        let nd = |f: &str| {
            if pt.unique.iter().any(|u| u == f) {
                pt.tuples
            } else if pt
                .def
                .columns
                .iter()
                .any(|c| c.field() == f && c.pg_type == "bool")
            {
                2.0
            } else {
                pt.tuples.min(200.0)
            }
        };
        let rows = (pt.tuples * hashagg::filter_selectivity(&flat, &nd)?)
            .round()
            .max(1.0);
        let mut indexes = self.cost_indexes_with(table, &pt.def, count as u64, false);
        let full = indexes.len();
        let partial = self.ssi_partial_indexes(table, &pt, filter, &nd);
        indexes.extend(partial.iter().map(|(ix, _)| ix.clone()));
        let hashes = self.hash_shapes(table);
        for h in &hashes {
            let pages = self.with_hash_meta(table, h, |m| m.pages());
            indexes.push(costsize::Index {
                field: h.field.clone(),
                unique: false,
                pages,
                height: 0,
                columns: vec![h.field.clone()],
                correlation: 0.0,
                visible: 0.0,
                tuples: None,
                predicate: None,
                hash: true,
            });
        }
        // `disable_cost`, added to a path's startup when its kind is off.
        const DISABLE_COST: f64 = 1.0e10;
        let off = |name: &str| self.setting_is(name, "off");
        let mut best: Option<(costsize::ScanKind, (f64, f64))> = None;
        for (kind, (mut startup, mut total)) in costsize::scan_paths(
            &rel,
            &indexes,
            &quals,
            &|f| costsize::ColSel::Ndistinct(nd(f)),
            rows,
        ) {
            let disabled = match kind {
                costsize::ScanKind::Seq => off("enable_seqscan"),
                costsize::ScanKind::Bitmap(_) => off("enable_bitmapscan"),
                costsize::ScanKind::Index(_) => off("enable_indexscan"),
            };
            if disabled {
                startup += DISABLE_COST;
                total += DISABLE_COST;
            }
            if best.is_none_or(|(_, (bs, bt))| total < bt || (total == bt && startup < bs)) {
                best = Some((kind, (startup, total)));
            }
        }
        let (costsize::ScanKind::Bitmap(i) | costsize::ScanKind::Index(i)) = best?.0 else {
            return None;
        };
        if i >= full + partial.len() {
            return hashes
                .into_iter()
                .nth(i - full - partial.len())
                .map(ScanIndex::Hash);
        }
        if i >= full {
            return partial
                .into_iter()
                .nth(i - full)
                .map(|(_, shape)| ScanIndex::Btree(shape));
        }
        let columns = &indexes.get(i)?.columns;
        self.btree_shapes(table)
            .into_iter()
            .find(|s| s.is_plain() && s.key_fields() == columns.as_slice())
            .map(ScanIndex::Btree)
    }

    /// `table`'s partial B-trees whose predicate `filter` implies
    /// (`check_index_predicates`: each of its conjuncts is one of the
    /// filter's, which is sound and not complete), costed as `plancat`
    /// sees them: `estimate_rel_size` of the index (its pages, and its
    /// tuples from `pg_class` at that density, or from its columns' widths
    /// when `pg_class` has none past the metapage), and the selectivity of
    /// the predicate's conjuncts its leading column's quals do not imply.
    fn ssi_partial_indexes(
        &self,
        table: &str,
        pt: &crate::PlannerTable,
        filter: &Document,
        nd: &dyn Fn(&str) -> f64,
    ) -> Vec<(
        secantus_pgplan::costsize::Index,
        std::sync::Arc<crate::btree::IndexShape>,
    )> {
        use secantus_pgplan::{costsize, hashagg};
        let Some(conds) = flat_conditions(filter) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for shape in self.btree_shapes(table) {
            let Some(pred) = shape.plain_predicate() else {
                continue;
            };
            let Some(pred) = flat_conditions(pred) else {
                continue;
            };
            if pred.is_empty() || !pred.iter().all(|p| conds.contains(p)) {
                continue;
            }
            let columns = shape.key_fields().to_vec();
            let Some(lead) = columns.first().cloned() else {
                continue;
            };
            let Ok(Some((pages, height))) =
                self.with_btree(table, &shape, |t| t.root.map(|_| (t.pages(), t.height())))
            else {
                continue;
            };
            let stats = self.relstats(&Self::index_relstats_key(table, &shape.name));
            let relpages = stats
                .as_ref()
                .and_then(|d| d.get("relpages"))
                .and_then(crate::bson_i64)
                .unwrap_or(0);
            let reltuples = stats
                .as_ref()
                .and_then(|d| d.get("reltuples"))
                .and_then(bson::Bson::as_f64)
                .unwrap_or(-1.0);
            // The metapage is discounted from both.
            let cur = f64::from(pages.saturating_sub(1));
            let relpages = (relpages - 1).max(0);
            let density = if reltuples >= 0.0 && relpages > 0 {
                reltuples / relpages as f64
            } else {
                let width: i64 = columns
                    .iter()
                    .map(|f| Self::column_width(&pt.def, f))
                    .sum::<i64>()
                    + 24
                    + 4;
                ((8192 - 24) / width.max(1)) as f64
            };
            let tuples = (density * cur).round().min(pt.tuples);
            let rest: Document = pred
                .iter()
                .filter(|(f, _)| f != &lead)
                .map(|(f, v)| (f.clone(), v.clone()))
                .collect();
            let selectivity = if rest.is_empty() {
                1.0
            } else {
                match hashagg::filter_selectivity(&rest, nd) {
                    Some(s) => s,
                    None => continue,
                }
            };
            let unique = shape.unique() && columns.len() == 1;
            out.push((
                costsize::Index {
                    field: lead,
                    unique,
                    pages: f64::from(pages),
                    height,
                    columns,
                    correlation: 0.0,
                    visible: 0.0,
                    tuples: Some(tuples),
                    predicate: Some(selectivity),
                    hash: false,
                },
                shape,
            ));
        }
        out
    }

    /// An index scan of `table` through `shape` (`_bt_first` onwards): the
    /// leaf pages it reads, locked; then each heap tuple the index's
    /// conditions reach, its versions through
    /// `HeapCheckForSerializableConflictOut` and the visible one locked
    /// (`heap_hot_search_buffer`).
    fn ssi_index_scan(
        &self,
        table: &str,
        rel: i64,
        shape: &crate::btree::IndexShape,
        filter: &Document,
    ) -> PgWireResult<()> {
        let pid = self.ssi_pid();
        let db = self.ssi_db();
        let keys = shape.key_fields().to_vec();
        let conds = flat_conditions(filter).unwrap_or_default();
        // The conditions on the index's columns: what its scan applies.
        let on_index: Vec<bson::Bson> = conds
            .iter()
            .filter(|(k, _)| keys.iter().any(|f| f == k))
            .map(|(k, v)| bson::Bson::Document(bson::doc! { k.clone(): v.clone() }))
            .collect();
        let index_filter = if on_index.is_empty() {
            Document::new()
        } else {
            bson::doc! { "$and": on_index }
        };
        // Each primitive scan's bounds on the leading column: an IN list's
        // values each, a comparison's range.
        let lead = keys.first();
        let mut lo: Option<(bson::Bson, bool)> = None;
        let mut hi: Option<(bson::Bson, bool)> = None;
        let mut list: Option<Vec<bson::Bson>> = None;
        for (k, v) in &conds {
            if Some(k) != lead {
                continue;
            }
            match v {
                bson::Bson::Document(ops) if ops.keys().all(|o| o.starts_with('$')) => {
                    for (op, x) in ops {
                        match op.as_str() {
                            "$eq" => {
                                lo = Some((x.clone(), true));
                                hi = Some((x.clone(), true));
                            }
                            "$gt" => lo = Some((x.clone(), false)),
                            "$gte" => lo = Some((x.clone(), true)),
                            "$lt" => hi = Some((x.clone(), false)),
                            "$lte" => hi = Some((x.clone(), true)),
                            "$in" => list = x.as_array().cloned(),
                            _ => {}
                        }
                    }
                }
                other => {
                    lo = Some((other.clone(), true));
                    hi = Some((other.clone(), true));
                }
            }
        }
        let ranges: Vec<(Bound, Bound)> = match list {
            Some(mut vals) => {
                vals.sort_by(|a, b| shape.cmp_attr(0, a, b));
                vals.into_iter()
                    .map(|v| (Some((v.clone(), true)), Some((v, true))))
                    .collect()
            }
            None => vec![(lo, hi)],
        };
        let pages: Vec<Option<Vec<u32>>> = self.with_btree(table, shape, |t| {
            ranges
                .iter()
                .map(|(lo, hi)| {
                    t.scan_pages(
                        shape,
                        lo.as_ref().map(|(v, i)| (v, *i)),
                        hi.as_ref().map(|(v, i)| (v, *i)),
                    )
                })
                .collect()
        })?;
        // The heap tuples the index's conditions reach, visible to the scan.
        let rows = self
            .storage
            .find_matching(self.db(), table, &index_filter)
            .map_err(|e| Self::storage_err("could not read", e))?;
        let mut tuples: Vec<(i64, Option<u64>)> = rows
            .iter()
            .filter_map(|b| {
                let d = crate::decode_doc(b).ok()?;
                let ctid = crate::heap::ctid_of(&d)?;
                let xmin = d
                    .get("__sys_xmin")
                    .and_then(crate::bson_i64)
                    .map(|x| x as u64);
                Some((ctid, xmin))
            })
            .collect();
        tuples.sort_unstable();
        let empty = Document::new();
        let examined = |_: i64, doc: Option<&Document>| {
            doc.is_some_and(|d| {
                secantus_core::query::matches(d, &index_filter, &empty, None).unwrap_or(false)
            })
        };
        self.ssi(|s| {
            for p in &pages {
                match p {
                    Some(blocks) => {
                        for b in blocks {
                            s.lock_page(pid, db, shape.oid, *b);
                        }
                    }
                    None => s.lock_relation(pid, db, shape.oid),
                }
            }
            let any = !tuples.is_empty() || s.has_versions(db, rel, &examined);
            s.scan_conflicts_out(pid, db, rel, &examined, any)?;
            for (ctid, xmin) in &tuples {
                s.lock_tuple(pid, db, rel, *ctid, *xmin);
            }
            Ok(())
        })
        .map_err(failure)
    }

    /// A sequential scan of `table` (`heap_beginscan`'s relation lock, then
    /// each tuple it meets through `HeapCheckForSerializableConflictOut`).
    pub(crate) fn ssi_seq_scan(&self, table: &str) -> PgWireResult<()> {
        let pid = self.ssi_pid();
        if !self.ssi_tracking() {
            return Ok(());
        }
        let Some(rel) = self.ssi_relation(table) else {
            return Ok(());
        };
        let db = self.ssi_db();
        // Did the scan meet a tuple at all: a version SSI tracks, or a row.
        let tuples = self.ssi(|s| s.has_versions(db, rel, &|_, _| true))
            || self
                .storage
                .count_matching(self.db(), table, &Document::new(), None)
                .is_ok_and(|n| n > 0);
        self.ssi(|s| {
            s.lock_relation(pid, db, rel);
            s.scan_conflicts_out(pid, db, rel, &|_, _| true, tuples)
        })
        .map_err(failure)
    }

    /// A write of `table`: an insert (`old` none), or an update or delete
    /// of the version at `old` (`CheckForSerializableConflictIn`).
    pub(crate) fn ssi_write(&self, table: &str, old: Option<i64>) -> PgWireResult<()> {
        if !self.ssi_tracking() {
            return Ok(());
        }
        let Some(rel) = self.ssi_relation(table) else {
            return Ok(());
        };
        self.ssi_xid();
        let pid = self.ssi_pid();
        let db = self.ssi_db();
        let blk = old.map(|c| (c / 65536) as u32);
        self.ssi(|s| s.conflict_in(pid, db, rel, old, blk))
            .map_err(failure)
    }

    /// A version this transaction made of a row of `table`: `ctid`
    /// inserted, or deleted.
    pub(crate) fn ssi_version(
        &self,
        table: &str,
        ctid: i64,
        inserted: bool,
        doc: Option<&Document>,
    ) {
        let doc = doc.map(nested);
        let doc = doc.as_ref();
        if !self.ssi_tracking() {
            return;
        }
        let Some(rel) = self.ssi_relation(table) else {
            return;
        };
        self.ssi_xid();
        let pid = self.ssi_pid();
        let db = self.ssi_db();
        self.ssi(|s| s.note_version(pid, db, rel, ctid, inserted, doc.cloned()));
    }

    /// An index tuple goes into `leaf` of index `index` (`_bt_doinsert`'s
    /// check on the page it lands on).
    pub(crate) fn ssi_index_insert(&self, index: i64, leaf: u32) -> PgWireResult<()> {
        if !self.ssi_tracking() {
            return Ok(());
        }
        self.ssi_xid();
        let pid = self.ssi_pid();
        let db = self.ssi_db();
        self.ssi(|s| s.conflict_in(pid, db, index, None, Some(leaf)))
            .map_err(failure)
    }

    /// [`Self::ssi_index_insert`] of a unique index, held until the
    /// statement ends. `_bt_doinsert` checks the leaf only once
    /// `_bt_check_unique` has passed, after waiting out an inserter of the
    /// same key, which here is the write conflict the statement ends in.
    pub(crate) fn ssi_defer_unique(&self, index: i64, leaf: u32) {
        if self.ssi_tracking() {
            self.ssi_unique_pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((index, leaf));
        }
    }

    /// The statement's end for [`Self::ssi_defer_unique`]: a write conflict
    /// drops the checks (the statement waits and runs again, and makes them
    /// anew); any other outcome makes them, and their failure is the
    /// statement's.
    pub(crate) fn ssi_flush_unique<T>(&self, out: PgWireResult<T>) -> PgWireResult<T> {
        let pending = std::mem::take(
            &mut *self
                .ssi_unique_pending
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        if pending.is_empty() || matches!(&out, Err(e) if Self::is_write_conflict(e)) {
            return out;
        }
        for (index, leaf) in pending {
            self.ssi_index_insert(index, leaf)?;
        }
        out
    }

    /// `PredicateLockPageSplit`: page `old` of index `index` split, its
    /// right half on `new` -- whatever the isolation of the splitter.
    pub(crate) fn ssi_page_split(&self, index: i64, old: u32, new: u32) {
        let db = self.ssi_db();
        self.ssi(|s| s.page_split(db, index, old, new));
    }

    /// `CheckTableForSerializableConflictIn`: a TRUNCATE or DROP of `table`.
    pub(crate) fn ssi_table_write(&self, table: &str) -> PgWireResult<()> {
        if !self.ssi_tracking() {
            return Ok(());
        }
        let Some(rel) = self.ssi_relation(table) else {
            return Ok(());
        };
        let pid = self.ssi_pid();
        let db = self.ssi_db();
        self.ssi(|s| s.table_conflict_in(pid, db, rel))
            .map_err(failure)
    }
}
