//! The session's side of the cumulative statistics (RP-070): its handle on
//! [`crate::pgstat`], the hooks the executor calls, and the
//! `pg_stat_get_*` functions the statistics views are defined over.

use std::collections::HashMap;

use bson::Bson;
use pgwire::error::PgWireResult;

use crate::pgstat::{Consistency, Disconnect, Stats};
use crate::PgHandler;

/// A `void` function's value: the empty string, not NULL.
const VOID: Bson = Bson::String(String::new());

/// `shared_buffers`' default, in 8 kB pages.
const DEFAULT_BUFFERS: usize = 16_384;

fn time(t: Option<std::time::SystemTime>) -> Bson {
    t.map_or(Bson::Null, |t| {
        Bson::DateTime(bson::DateTime::from_system_time(t))
    })
}

impl PgHandler {
    /// This session's handle on its store's statistics.
    pub(crate) fn stats(&self) -> Stats {
        let db = *self.stat_db_oid.get_or_init(|| {
            self.databases
                .lookup(&self.storage, self.db())
                .ok()
                .flatten()
                .map_or(0, |d| d.oid)
        });
        Stats {
            store: std::sync::Arc::as_ptr(&self.storage) as usize,
            pid: self.backend_pid.load(std::sync::atomic::Ordering::Relaxed),
            db,
        }
    }

    fn stat_setting(&self, name: &str) -> Option<String> {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    /// `track_counts`: off stops the table and index counters.
    fn tracking(&self) -> bool {
        !matches!(
            self.stat_setting("track_counts").as_deref(),
            Some("off" | "false" | "0" | "no")
        )
    }

    fn consistency(&self) -> Consistency {
        Consistency::from_setting(self.stat_setting("stats_fetch_consistency").as_deref())
    }

    /// `shared_buffers`, in pages.
    pub(crate) fn buffer_pages(&self) -> usize {
        match self.stat_setting("shared_buffers") {
            Some(v) => {
                let v = v.trim().to_ascii_lowercase();
                let (num, unit) = v
                    .find(|c: char| !c.is_ascii_digit())
                    .map_or((v.as_str(), ""), |i| (&v[..i], v[i..].trim()));
                let n: usize = num.parse().unwrap_or(DEFAULT_BUFFERS);
                match unit {
                    "kb" => n / 8,
                    "mb" => n * 128,
                    "gb" => n * 131_072,
                    _ => n,
                }
            }
            None => DEFAULT_BUFFERS,
        }
    }

    // ----------------------------------------------------- executor hooks

    /// Rows of `table` written: inserted (no old version) or updated, `hot`
    /// of the updates heap-only.
    pub(crate) fn stat_writes(&self, table: &str, inserted: i64, updated: i64, hot: i64) {
        if !self.tracking() || table.starts_with("__") {
            return;
        }
        let rel = self.relation_oid(table).unwrap_or(0);
        let s = self.stats();
        if inserted > 0 {
            s.inserted(rel, inserted);
        }
        if updated > 0 {
            s.updated(rel, updated, hot);
        }
    }

    pub(crate) fn stat_deletes(&self, table: &str, n: i64) {
        if !self.tracking() || table.starts_with("__") || n == 0 {
            return;
        }
        let rel = self.relation_oid(table).unwrap_or(0);
        self.stats().deleted(rel, n);
    }

    /// Pages of `table` touched (each a read or a hit, as the pool holds
    /// it).
    pub(crate) fn stat_pages(&self, table: &str, blocks: impl IntoIterator<Item = i64>) {
        if !self.tracking() || table.starts_with("__") {
            return;
        }
        let rel = self.relation_oid(table).unwrap_or(0);
        self.stats().pages(rel, blocks, self.buffer_pages());
    }

    /// Every page of `table`'s heap is touched by a sequential scan.
    pub(crate) fn stat_heap_pages(&self, table: &str) {
        if !self.tracking() || table.starts_with("__") || !self.has_heap(table) {
            return;
        }
        let blocks = self.heap_blocks(table).unwrap_or(0);
        self.stat_pages(table, 0..i64::from(blocks));
    }

    /// The oid of `table`'s index `index` (the storage's `_id_` is its
    /// primary key), read once per catalog version.
    fn stat_index_oid(&self, table: &str, index: &str) -> i64 {
        type Key = (u64, u64, String, String);
        thread_local! {
            static MEMO: std::cell::RefCell<HashMap<Key, i64>> = std::cell::RefCell::new(HashMap::new());
        }
        let name = if index == "_id_" {
            format!("{}_pkey", secantus_pgplan::schemas::split_key(table).1)
        } else {
            index.to_string()
        };
        let key: Key = (
            crate::catalog_cache()
                .version
                .load(std::sync::atomic::Ordering::SeqCst),
            if self.block_wrote_catalog() {
                self.session_serial
            } else {
                0
            },
            table.to_string(),
            name.clone(),
        );
        if let Some(o) = MEMO.with(|m| m.borrow().get(&key).copied()) {
            return o;
        }
        let oid = self
            .index_relations()
            .iter()
            .find(|ix| {
                ix.table.name == table && secantus_pgplan::schemas::split_key(&ix.name).1 == name
            })
            .map_or(0, |ix| ix.oid);
        MEMO.with(|m| {
            let mut m = m.borrow_mut();
            if m.len() > 4096 {
                m.clear();
            }
            m.insert(key, oid);
        });
        oid
    }

    /// The B-tree levels above the leaves of an index on `table`, from the
    /// heap's tuples, recounted when the heap grows a page.
    fn stat_btree_levels(&self, table: &str) -> i64 {
        thread_local! {
            static MEMO: std::cell::RefCell<HashMap<(String, u32), i64>> = std::cell::RefCell::new(HashMap::new());
        }
        let blocks = self.heap_blocks(table).unwrap_or(0);
        let key = (table.to_string(), blocks);
        if let Some(l) = MEMO.with(|m| m.borrow().get(&key).copied()) {
            return l;
        }
        let levels = btree_levels(self.heap_tuple_count(table) as f64);
        MEMO.with(|m| {
            let mut m = m.borrow_mut();
            if m.len() > 4096 {
                m.clear();
            }
            m.insert(key, levels);
        });
        levels
    }

    /// A query's scan of `table` under `filter` returned `rows`: a
    /// sequential scan, which reads every visible tuple, or an index scan,
    /// which returns the entries the index bounds give -- whichever the
    /// storage planner runs, as `EXPLAIN` shows it.
    pub(crate) fn stat_scan(&self, table: &str, filter: &bson::Document, rows: &[Vec<u8>]) {
        if !self.tracking() || table.starts_with("__") || !self.has_heap(table) {
            return;
        }
        match self.storage.explain_plan(self.db(), table, filter) {
            Ok(secantus_storage::ExplainPlan::IxScan { index_name, .. }) => {
                let oid = self.stat_index_oid(table, &index_name);
                let ctids: Vec<i64> = rows.iter().filter_map(|b| raw_ctid(b)).collect();
                self.stat_index_scan(table, oid, &ctids);
            }
            _ => {
                let total = if filter.is_empty() {
                    rows.len()
                } else {
                    self.storage
                        .count_matching(self.db(), table, &bson::Document::new(), None)
                        .unwrap_or(rows.len())
                };
                self.stat_seq_scan(table, total as i64);
            }
        }
    }

    /// A scan that streams its rows, planned here and counted when it
    /// ends -- on whichever thread reads it.
    pub(crate) fn scan_stat(&self, table: &str, filter: &bson::Document) -> Option<ScanStat> {
        if !self.tracking() || table.starts_with("__") || !self.has_heap(table) {
            return None;
        }
        let rel = self.relation_oid(table).unwrap_or(0);
        let index = match self.storage.explain_plan(self.db(), table, filter) {
            Ok(secantus_storage::ExplainPlan::IxScan { index_name, .. }) => {
                Some(self.stat_index_oid(table, &index_name))
            }
            _ => None,
        };
        let total = if index.is_some() {
            0
        } else {
            self.storage
                .count_matching(self.db(), table, &bson::Document::new(), None)
                .unwrap_or(0) as i64
        };
        Some(ScanStat {
            stats: self.stats(),
            rel,
            index,
            blocks: self.heap_blocks(table).unwrap_or(0),
            capacity: self.buffer_pages(),
            levels: if index.is_some() {
                self.stat_btree_levels(table)
            } else {
                0
            },
            total,
        })
    }

    /// A write's scan of `table` under `filter`, its rows not in hand: a
    /// sequential one needs only the table's size, an index one the rows it
    /// reaches.
    pub(crate) fn stat_scan_filter(&self, table: &str, filter: &bson::Document) {
        if !self.tracking() || table.starts_with("__") || !self.has_heap(table) {
            return;
        }
        if self.collection_scan(table, filter) {
            let total = self
                .storage
                .count_matching(self.db(), table, &bson::Document::new(), None)
                .unwrap_or(0);
            self.stat_seq_scan(table, total as i64);
            return;
        }
        let rows = self
            .storage
            .find_matching(self.db(), table, filter)
            .unwrap_or_default();
        self.stat_scan(table, filter, &rows);
    }

    pub(crate) fn stat_seq_scan(&self, table: &str, tuples: i64) {
        if !self.tracking() || table.starts_with("__") {
            return;
        }
        let rel = self.relation_oid(table).unwrap_or(0);
        self.stats().seq_scan(rel, tuples);
        self.stat_heap_pages(table);
    }

    /// An index scan of `index` on `table` returned `n` entries and
    /// fetched as many live heap tuples, at `ctids`.
    pub(crate) fn stat_index_scan(&self, table: &str, index_oid: i64, ctids: &[i64]) {
        if !self.tracking() || table.starts_with("__") || index_oid == 0 {
            return;
        }
        let s = self.stats();
        let n = ctids.len() as i64;
        s.index_scan(index_oid, n, n);
        let capacity = self.buffer_pages();
        // The descent: the metapage, then a page a level down to the leaf.
        s.pages(index_oid, 0..=self.stat_btree_levels(table), capacity);
        let mut blocks: Vec<i64> = ctids.iter().map(|c| c / 65536).collect();
        blocks.dedup();
        self.stat_pages(table, blocks);
    }

    /// A VACUUM (`analyze` false) or ANALYZE of `table` reports what it
    /// found: the live rows, and the dead tuples left on the heap.
    pub(crate) fn stat_maintained(&self, table: &str, analyze: bool) -> PgWireResult<()> {
        if !self.tracking() || !self.has_heap(table) {
            return Ok(());
        }
        let live = self.table_docs(table)?.len() as i64;
        let dead = self.dead_tuple_count(table);
        let rel = self.relation_oid(table).unwrap_or(0);
        let s = self.stats();
        if analyze {
            s.report_analyze(rel, live, dead);
        } else {
            s.report_vacuum(rel, live, dead);
        }
        Ok(())
    }

    pub(crate) fn stat_truncated(&self, table: &str) {
        if table.starts_with("__") {
            return;
        }
        let rel = self.relation_oid(table).unwrap_or(0);
        self.stats().truncated(rel);
    }

    /// A user function's call ended: counted when `track_functions` covers
    /// its language (`pl`: the procedural ones; `all`: SQL too).
    pub(crate) fn stat_function_call(&self, oid: i64, language: &str, total: i64, own: i64) {
        let track = self.stat_setting("track_functions").unwrap_or_default();
        let counted = match track.to_ascii_lowercase().as_str() {
            "all" => true,
            "pl" => !matches!(language, "sql" | "c" | "internal"),
            _ => false,
        };
        if counted {
            self.stats().function_call(oid, total, own);
        }
    }

    /// The session ends (`pgstat_shutdown_hook`), once: an open block
    /// aborts, and its counts and times go out with how it ended.
    pub(crate) fn stat_end_session(&self, in_block: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        if self.stat_ended.swap(true, Relaxed) {
            return;
        }
        let s = self.stats();
        if in_block {
            s.end_xact(false);
        }
        let how = if self.backend.terminate.load(Relaxed) {
            Disconnect::Killed
        } else if self.stat_abandoned.load(Relaxed) {
            Disconnect::Abandoned
        } else {
            Disconnect::Normal
        };
        s.disconnect(
            self.stat_session_start.elapsed().as_secs_f64() * 1000.0,
            how,
        );
    }

    // ------------------------------------------------------------ the SQL

    /// `pg_stat_*`: a counter read, a flush forced, a snapshot or a counter
    /// reset.
    pub(crate) fn pgstat_call(&self, name: &str, args: &[Bson]) -> PgWireResult<Bson> {
        let s = self.stats();
        let c = self.consistency();
        let oid = || -> i64 {
            match args.first() {
                Some(Bson::Null) | None => -1,
                Some(v) => crate::bson_i64(v)
                    .or_else(|| secantus_pgplan::regclass_oid(v))
                    .unwrap_or_else(|| match v {
                        Bson::String(t) => t
                            .trim()
                            .parse()
                            .unwrap_or_else(|_| self.relation_oid(t.trim()).unwrap_or(-1)),
                        _ => -1,
                    }),
            }
        };
        let int = |v: i64| Bson::Int64(v);
        match name {
            "pg_stat_force_next_flush" => {
                s.force_next_flush();
                return Ok(VOID);
            }
            "pg_stat_clear_snapshot" => {
                s.clear_snapshot();
                return Ok(VOID);
            }
            "pg_stat_get_snapshot_timestamp" => return Ok(time(s.snapshot_timestamp())),
            "pg_stat_reset" => {
                s.reset_database();
                return Ok(VOID);
            }
            "pg_stat_reset_single_table_counters" | "pg_stat_reset_single_function_counters" => {
                let o = oid();
                if o >= 0 {
                    s.reset_single(name.ends_with("function_counters"), o);
                }
                return Ok(VOID);
            }
            _ => {}
        }
        let o = oid();
        if o < 0 {
            return Ok(Bson::Null);
        }
        if let Some(field) = name.strip_prefix("pg_stat_get_db_") {
            let e = s.db_entry(c, o).unwrap_or_default();
            return Ok(match field {
                "numbackends" => Bson::Int32(s.numbackends(o)),
                "xact_commit" => int(e.xact_commit),
                "xact_rollback" => int(e.xact_rollback),
                "blocks_fetched" => int(e.blocks_fetched),
                "blocks_hit" => int(e.blocks_hit),
                "tuples_returned" => int(e.tuples_returned),
                "tuples_fetched" => int(e.tuples_fetched),
                "tuples_inserted" => int(e.tuples_inserted),
                "tuples_updated" => int(e.tuples_updated),
                "tuples_deleted" => int(e.tuples_deleted),
                "deadlocks" => int(e.deadlocks),
                // No standby, so no recovery conflicts; nothing spills to
                // temporary files here.
                "conflict_all" | "temp_files" | "temp_bytes" => int(0),
                // Data checksums are off (`initdb` without `-k`).
                "checksum_failures" | "checksum_last_failure" => Bson::Null,
                // `track_io_timing` is off.
                "blk_read_time" | "blk_write_time" => Bson::Double(0.0),
                "session_time" => Bson::Double(e.session_time),
                "active_time" => Bson::Double(e.active_time),
                "idle_in_transaction_time" => Bson::Double(e.idle_in_transaction_time),
                "sessions" => int(e.sessions),
                "sessions_abandoned" => int(e.sessions_abandoned),
                "sessions_fatal" => int(e.sessions_fatal),
                "sessions_killed" => int(e.sessions_killed),
                "stat_reset_time" => time(e.stat_reset_timestamp),
                _ => Bson::Null,
            });
        }
        if let Some(field) = name.strip_prefix("pg_stat_get_xact_") {
            if let Some(f) = field.strip_prefix("function_") {
                let Some(e) = s.xact_func(o) else {
                    return Ok(Bson::Null);
                };
                return Ok(match f {
                    "calls" => int(e.calls),
                    "total_time" => Bson::Double(e.total_time as f64 / 1000.0),
                    _ => Bson::Double(e.self_time as f64 / 1000.0),
                });
            }
            let e = s.xact_rel(o).unwrap_or_default();
            return Ok(int(match field {
                "numscans" => e.numscans,
                "tuples_returned" => e.tuples_returned,
                "tuples_fetched" => e.tuples_fetched,
                "tuples_inserted" => e.tuples_inserted,
                "tuples_updated" => e.tuples_updated,
                "tuples_deleted" => e.tuples_deleted,
                "tuples_hot_updated" => e.tuples_hot_updated,
                "blocks_fetched" => e.blocks_fetched,
                _ => e.blocks_hit,
            }));
        }
        if let Some(f) = name.strip_prefix("pg_stat_get_function_") {
            let Some(e) = s.func_entry(c, o) else {
                return Ok(Bson::Null);
            };
            return Ok(match f {
                "calls" => int(e.calls),
                "total_time" => Bson::Double(e.total_time as f64 / 1000.0),
                _ => Bson::Double(e.self_time as f64 / 1000.0),
            });
        }
        let e = s.rel_entry(c, o).unwrap_or_default();
        Ok(match name.trim_start_matches("pg_stat_get_") {
            "numscans" => int(e.numscans),
            "tuples_returned" => int(e.tuples_returned),
            "tuples_fetched" => int(e.tuples_fetched),
            "tuples_inserted" => int(e.tuples_inserted),
            "tuples_updated" => int(e.tuples_updated),
            "tuples_deleted" => int(e.tuples_deleted),
            "tuples_hot_updated" => int(e.tuples_hot_updated),
            "live_tuples" => int(e.live_tuples),
            "dead_tuples" => int(e.dead_tuples),
            "mod_since_analyze" => int(e.changes_since_analyze),
            "ins_since_vacuum" => int(e.inserts_since_vacuum),
            "blocks_fetched" => int(e.blocks_fetched),
            "blocks_hit" => int(e.blocks_hit),
            "last_vacuum_time" => time(e.vacuum_timestamp),
            "last_autovacuum_time" => time(e.autovac_vacuum_timestamp),
            "last_analyze_time" => time(e.analyze_timestamp),
            "last_autoanalyze_time" => time(e.autovac_analyze_timestamp),
            "vacuum_count" => int(e.vacuum_count),
            "autovacuum_count" => int(e.autovac_vacuum_count),
            "analyze_count" => int(e.analyze_count),
            "autoanalyze_count" => int(e.autovac_analyze_count),
            _ => Bson::Null,
        })
    }
}

/// A B-tree's levels above its leaves for `rows` entries, as a freshly
/// built one has them: a leaf holds about 367 int4 entries, an inner page
/// about 286 downlinks.
fn btree_levels(rows: f64) -> i64 {
    let mut pages = (rows / 367.0).ceil().max(1.0);
    let mut levels = 1;
    while pages > 1.0 {
        pages = (pages / 286.0).ceil();
        levels += 1;
    }
    levels
}

/// A streamed scan's statistics, settled when it ends.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanStat {
    stats: Stats,
    rel: i64,
    index: Option<i64>,
    blocks: u32,
    capacity: usize,
    levels: i64,
    total: i64,
}

impl ScanStat {
    /// The scan ended having returned `returned` rows: a sequential scan
    /// read the heap's visible tuples and every page; an index scan
    /// returned its entries through the index's descent.
    pub(crate) fn finish(&self, returned: i64) {
        match self.index {
            Some(ix) => {
                self.stats.index_scan(ix, returned, returned);
                self.stats.pages(ix, 0..=self.levels, self.capacity);
            }
            None => {
                self.stats.seq_scan(self.rel, self.total);
                self.stats
                    .pages(self.rel, 0..i64::from(self.blocks), self.capacity);
            }
        }
    }
}

/// A stored row's ctid, read from its bytes without decoding the row.
fn raw_ctid(bytes: &[u8]) -> Option<i64> {
    match bson::RawDocument::from_bytes(bytes)
        .ok()?
        .get("__sys_ctid")
        .ok()??
    {
        bson::RawBsonRef::Int64(v) => Some(v),
        bson::RawBsonRef::Int32(v) => Some(i64::from(v)),
        _ => None,
    }
}
