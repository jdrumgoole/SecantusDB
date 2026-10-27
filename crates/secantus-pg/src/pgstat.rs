//! The cumulative statistics system (RP-070): the counters behind
//! `pg_stat_*_tables`, `pg_stat_*_indexes`, `pg_stat_*_functions`,
//! `pg_stat_xact_*`, `pg_stat_database` and `pg_statio_*`, kept as
//! PostgreSQL 15.19 keeps them (decided by Joe, 2026-10-22: model the
//! counters; 2026-10-24: model a buffer cache for the block counts).
//!
//! Each backend counts what it does into its own pending counts
//! (`pgstat_relation.c`): a scan's tuples straight away, a write's tuples in
//! the transaction level it happened in. A level ends as
//! `AtEOXact_PgStat_Relations` / `AtEOSubXact_PgStat_Relations` end it --
//! every attempted write counted, and on commit an insert a live tuple and an
//! update or delete a dead one, on abort the inserts and updates dead. The
//! pending counts reach the shared ones as the backend goes idle
//! (`pgstat_report_stat`): at once when a second has passed since its last
//! flush, else after ten seconds idle, or at the next idle point after
//! `pg_stat_force_next_flush()`. A reader keeps the values it first read of
//! an object until its transaction ends (`stats_fetch_consistency =
//! cache`). The shared counts are written to the store at a clean shutdown
//! and read back, once, at startup: a crash loses them, as it does
//! PostgreSQL's.
//!
//! The block counters come from a model of the shared buffer pool: every
//! page a scan, a fetch or a write touches is looked up in a pool of
//! `shared_buffers` pages, a hit when it is there and a read (which loads
//! it, evicting the least recently used) when it is not. PostgreSQL also
//! touches visibility-map, free-space-map and catalog pages, which this
//! server does not, so the counts follow the mechanism rather than its
//! numbers.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use bson::{doc, Bson, Document};

/// `PGSTAT_MIN_INTERVAL`: the least time between two flushes.
const MIN_INTERVAL: Duration = Duration::from_millis(1000);
/// `PGSTAT_IDLE_INTERVAL`: how long an idle backend holds counts it could
/// not flush.
const IDLE_INTERVAL: Duration = Duration::from_millis(10_000);
/// `PGSTAT_MAX_INTERVAL`: counts pending this long are flushed regardless.
const MAX_INTERVAL: Duration = Duration::from_millis(60_000);

/// Where the shared counts are kept across a clean shutdown.
const STATS_NAMESPACE: &str = "__secantus_pg__";
const STATS_COLLECTION: &str = "pg_stat";

/// At startup: the counts a clean shutdown wrote, read and removed -- a
/// crash after this leaves none, so its counts start from nothing, as
/// PostgreSQL's do (`pgstat_restore_stats` / `pgstat_discard_stats`).
pub(crate) fn restore_store(
    storage: &std::sync::Arc<secantus_storage::Storage>,
) -> Result<(), String> {
    let key = std::sync::Arc::as_ptr(storage) as usize;
    with_shared(key, |s| *s = Shared::default());
    if !storage
        .collection_exists(STATS_NAMESPACE, STATS_COLLECTION)
        .map_err(|e| e.to_string())?
    {
        return Ok(());
    }
    let docs = storage
        .find_matching(STATS_NAMESPACE, STATS_COLLECTION, &Document::new())
        .map_err(|e| e.to_string())?;
    for bytes in docs {
        let d: Document = crate::decode_doc(&bytes).map_err(|e| e.to_string())?;
        with_shared(key, |s| restore(s, &d));
    }
    storage
        .delete_matching(
            STATS_NAMESPACE,
            STATS_COLLECTION,
            &Document::new(),
            0,
            &Document::new(),
            None,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// At a clean shutdown: every backend's counts folded in, and the shared
/// ones written for the next start (`pgstat_write_statsfile`).
pub(crate) fn save_store(
    storage: &std::sync::Arc<secantus_storage::Storage>,
) -> Result<(), String> {
    let key = std::sync::Arc::as_ptr(storage) as usize;
    Stats::flush_all(key);
    let d = Stats::dump(key);
    Stats::forget_store(key);
    // The shared invalidation queue goes with the server (RP-225).
    crate::catcache::forget_store(key);
    if !storage
        .collection_exists(STATS_NAMESPACE, STATS_COLLECTION)
        .map_err(|e| e.to_string())?
    {
        storage
            .create_collection(STATS_NAMESPACE, STATS_COLLECTION)
            .map_err(|e| e.to_string())?;
    }
    storage
        .delete_matching(
            STATS_NAMESPACE,
            STATS_COLLECTION,
            &Document::new(),
            0,
            &Document::new(),
            None,
        )
        .map_err(|e| e.to_string())?;
    let bytes = crate::encode_doc(&d).map_err(|e| e.to_string())?;
    storage
        .insert(STATS_NAMESPACE, STATS_COLLECTION, vec![bytes], true)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// A relation's counts as one backend accumulates them
/// (`PgStat_TableCounts`).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct RelCounts {
    pub numscans: i64,
    pub tuples_returned: i64,
    pub tuples_fetched: i64,
    pub tuples_inserted: i64,
    pub tuples_updated: i64,
    pub tuples_deleted: i64,
    pub tuples_hot_updated: i64,
    pub truncdropped: bool,
    pub delta_live_tuples: i64,
    pub delta_dead_tuples: i64,
    pub changed_tuples: i64,
    pub blocks_fetched: i64,
    pub blocks_hit: i64,
}

impl RelCounts {
    fn is_empty(&self) -> bool {
        *self == RelCounts::default()
    }
}

/// One transaction level's writes to a relation (`PgStat_TableXactStatus`).
#[derive(Debug, Default, Clone, Copy)]
struct XactRel {
    inserted: i64,
    updated: i64,
    deleted: i64,
    truncdropped: bool,
    /// The counts a TRUNCATE in this level set aside, restored if the level
    /// aborts.
    saved: Option<(i64, i64, i64)>,
}

/// A relation's shared counts (`PgStat_StatTabEntry`).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct RelEntry {
    pub numscans: i64,
    pub tuples_returned: i64,
    pub tuples_fetched: i64,
    pub tuples_inserted: i64,
    pub tuples_updated: i64,
    pub tuples_deleted: i64,
    pub tuples_hot_updated: i64,
    pub live_tuples: i64,
    pub dead_tuples: i64,
    pub changes_since_analyze: i64,
    pub inserts_since_vacuum: i64,
    pub blocks_fetched: i64,
    pub blocks_hit: i64,
    pub vacuum_timestamp: Option<SystemTime>,
    pub vacuum_count: i64,
    pub autovac_vacuum_timestamp: Option<SystemTime>,
    pub autovac_vacuum_count: i64,
    pub analyze_timestamp: Option<SystemTime>,
    pub analyze_count: i64,
    pub autovac_analyze_timestamp: Option<SystemTime>,
    pub autovac_analyze_count: i64,
}

/// A function's counts (`PgStat_FunctionCounts`), times in microseconds.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct FuncCounts {
    pub calls: i64,
    pub total_time: i64,
    pub self_time: i64,
}

/// A database's counts (`PgStat_StatDBEntry`, what a backend adds to it
/// beside its relations' sums).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct DbEntry {
    pub xact_commit: i64,
    pub xact_rollback: i64,
    pub blocks_fetched: i64,
    pub blocks_hit: i64,
    pub tuples_returned: i64,
    pub tuples_fetched: i64,
    pub tuples_inserted: i64,
    pub tuples_updated: i64,
    pub tuples_deleted: i64,
    pub deadlocks: i64,
    pub sessions: i64,
    pub sessions_abandoned: i64,
    pub sessions_fatal: i64,
    pub sessions_killed: i64,
    /// Milliseconds.
    pub session_time: f64,
    pub active_time: f64,
    pub idle_in_transaction_time: f64,
    pub stat_reset_timestamp: Option<SystemTime>,
}

impl DbEntry {
    fn add(&mut self, o: &DbEntry) {
        self.xact_commit += o.xact_commit;
        self.xact_rollback += o.xact_rollback;
        self.blocks_fetched += o.blocks_fetched;
        self.blocks_hit += o.blocks_hit;
        self.tuples_returned += o.tuples_returned;
        self.tuples_fetched += o.tuples_fetched;
        self.tuples_inserted += o.tuples_inserted;
        self.tuples_updated += o.tuples_updated;
        self.tuples_deleted += o.tuples_deleted;
        self.deadlocks += o.deadlocks;
        self.sessions += o.sessions;
        self.sessions_abandoned += o.sessions_abandoned;
        self.sessions_fatal += o.sessions_fatal;
        self.sessions_killed += o.sessions_killed;
        self.session_time += o.session_time;
        self.active_time += o.active_time;
        self.idle_in_transaction_time += o.idle_in_transaction_time;
    }
}

/// The shared buffer pool's residency: which pages are loaded, the least
/// recently used first out.
#[derive(Debug, Default)]
struct Buffers {
    /// Each loaded page and the stamp of its last use.
    loaded: HashMap<(i64, i64), u64>,
    /// `(stamp, page)` in use order; an entry whose stamp is stale was
    /// used again later.
    order: VecDeque<(u64, (i64, i64))>,
    clock: u64,
}

impl Buffers {
    /// Touch `page`: true for a hit, false for a read that loaded it.
    fn touch(&mut self, page: (i64, i64), capacity: usize) -> bool {
        self.clock += 1;
        let stamp = self.clock;
        let hit = self.loaded.insert(page, stamp).is_some();
        self.order.push_back((stamp, page));
        while self.loaded.len() > capacity.max(16) {
            let Some((s, p)) = self.order.pop_front() else {
                break;
            };
            if self.loaded.get(&p) == Some(&s) {
                self.loaded.remove(&p);
            }
        }
        // Keep the order queue from growing without bound.
        if self.order.len() > 4 * capacity.max(16) {
            let loaded = &self.loaded;
            self.order.retain(|(s, p)| loaded.get(p) == Some(s));
        }
        hit
    }

    /// Forget a relation's pages (DROP, TRUNCATE): its buffers are
    /// invalidated.
    fn drop_relation(&mut self, rel: i64) {
        self.loaded.retain(|(r, _), _| *r != rel);
    }
}

/// One store's shared statistics.
#[derive(Debug, Default)]
struct Shared {
    /// `(database oid, relation oid)`.
    rels: HashMap<(i64, i64), RelEntry>,
    /// `(database oid, function oid)`.
    funcs: HashMap<(i64, i64), FuncCounts>,
    dbs: HashMap<i64, DbEntry>,
    buffers: Buffers,
    /// Each backend's pending counts, by pid.
    backends: HashMap<i32, Backend>,
    /// Counts a PREPARE TRANSACTION set aside, by gid: applied when the
    /// prepared transaction is committed or rolled back.
    prepared: HashMap<String, (i64, Vec<HashMap<i64, XactRel>>)>,
}

/// A backend's pending statistics.
#[derive(Debug, Default)]
struct Backend {
    db: i64,
    rels: HashMap<i64, RelCounts>,
    funcs: HashMap<i64, FuncCounts>,
    dbc: DbEntry,
    /// The transaction's levels: the top one, then one per savepoint.
    levels: Vec<HashMap<i64, XactRel>>,
    last_flush: Option<Instant>,
    pending_since: Option<Instant>,
    /// Set when the backend went idle holding counts it could not flush:
    /// they go out once it has been idle `IDLE_INTERVAL`.
    idle_since: Option<Instant>,
    force_next: bool,
    /// What this backend's reads saw first in its transaction.
    snapshot: HashMap<(u8, i64), Snap>,
    snapshot_taken: Option<SystemTime>,
    /// When the backend's current state began, for the session times.
    state_since: Option<Instant>,
    state: SessionState,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum SessionState {
    #[default]
    Idle,
    Active,
    IdleInTransaction,
}

#[derive(Debug, Clone)]
enum Snap {
    Rel(Option<RelEntry>),
    Func(Option<FuncCounts>),
    Db(Option<DbEntry>),
}

fn registry() -> &'static Mutex<HashMap<usize, Shared>> {
    static REG: OnceLock<Mutex<HashMap<usize, Shared>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_shared<R>(store: usize, f: impl FnOnce(&mut Shared) -> R) -> R {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    f(reg.entry(store).or_default())
}

fn backend(shared: &mut Shared, pid: i32, db: i64) -> &mut Backend {
    let b = shared.backends.entry(pid).or_default();
    if b.levels.is_empty() {
        b.levels.push(HashMap::new());
    }
    b.db = db;
    b
}

/// Fold a backend's pending counts into the shared ones
/// (`pgstat_relation_flush_cb`, `pgstat_database_flush_cb`).
fn flush_backend(shared: &mut Shared, pid: i32, now: Instant) {
    let Some(b) = shared.backends.get_mut(&pid) else {
        return;
    };
    let db = b.db;
    let rels = std::mem::take(&mut b.rels);
    let funcs = std::mem::take(&mut b.funcs);
    let mut dbc = std::mem::take(&mut b.dbc);
    // A shared catalog's counts are kept under database 0, and so are the
    // tuples they add to a database's (`pg_stat_database`'s row for shared
    // objects).
    let mut shared_dbc = DbEntry::default();
    b.last_flush = Some(now);
    b.pending_since = None;
    b.idle_since = None;
    b.force_next = false;
    for (rel, c) in rels {
        let is_shared = crate::catalog_facts::is_shared_relation(rel);
        let e = shared.rels.entry((rel_db(db, rel), rel)).or_default();
        e.numscans += c.numscans;
        e.tuples_returned += c.tuples_returned;
        e.tuples_fetched += c.tuples_fetched;
        e.tuples_inserted += c.tuples_inserted;
        e.tuples_updated += c.tuples_updated;
        e.tuples_deleted += c.tuples_deleted;
        e.tuples_hot_updated += c.tuples_hot_updated;
        if c.truncdropped {
            e.live_tuples = 0;
            e.dead_tuples = 0;
            e.inserts_since_vacuum = 0;
        }
        e.live_tuples = (e.live_tuples + c.delta_live_tuples).max(0);
        e.dead_tuples = (e.dead_tuples + c.delta_dead_tuples).max(0);
        e.changes_since_analyze += c.changed_tuples;
        e.inserts_since_vacuum += c.tuples_inserted;
        e.blocks_fetched += c.blocks_fetched;
        e.blocks_hit += c.blocks_hit;
        let dbc = if is_shared { &mut shared_dbc } else { &mut dbc };
        dbc.tuples_returned += c.tuples_returned;
        dbc.tuples_fetched += c.tuples_fetched;
        dbc.tuples_inserted += c.tuples_inserted;
        dbc.tuples_updated += c.tuples_updated;
        dbc.tuples_deleted += c.tuples_deleted;
        dbc.blocks_fetched += c.blocks_fetched;
        dbc.blocks_hit += c.blocks_hit;
    }
    for (f, c) in funcs {
        let e = shared.funcs.entry((db, f)).or_default();
        e.calls += c.calls;
        e.total_time += c.total_time;
        e.self_time += c.self_time;
    }
    shared.dbs.entry(db).or_default().add(&dbc);
    if shared_dbc != DbEntry::default() {
        shared.dbs.entry(0).or_default().add(&shared_dbc);
    }
}

/// The database a relation's counts are kept under: 0 for a shared
/// catalog, which every database reads.
fn rel_db(db: i64, rel: i64) -> i64 {
    if crate::catalog_facts::is_shared_relation(rel) {
        0
    } else {
        db
    }
}

/// Flush every other backend that has been idle long enough with counts
/// pending -- its idle timer would have fired by now.
fn flush_idle(shared: &mut Shared, now: Instant) {
    let due: Vec<i32> = shared
        .backends
        .iter()
        .filter(|(_, b)| {
            b.idle_since
                .is_some_and(|t| now.duration_since(t) >= IDLE_INTERVAL)
        })
        .map(|(p, _)| *p)
        .collect();
    for pid in due {
        flush_backend(shared, pid, now);
    }
}

fn has_pending(b: &Backend) -> bool {
    b.rels.values().any(|c| !c.is_empty()) || !b.funcs.is_empty() || b.dbc != DbEntry::default()
}

/// The handle a session holds: its store, pid and database.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stats {
    pub store: usize,
    pub pid: i32,
    pub db: i64,
}

/// What `stats_fetch_consistency` asks of a reader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Consistency {
    None,
    Cache,
    Snapshot,
}

impl Consistency {
    pub(crate) fn from_setting(v: Option<&str>) -> Self {
        match v.map(str::to_ascii_lowercase).as_deref() {
            Some("none") => Consistency::None,
            Some("snapshot") => Consistency::Snapshot,
            _ => Consistency::Cache,
        }
    }
}

impl Stats {
    fn with<R>(&self, f: impl FnOnce(&mut Shared) -> R) -> R {
        with_shared(self.store, f)
    }

    fn mine<R>(&self, f: impl FnOnce(&mut Backend) -> R) -> R {
        let (pid, db) = (self.pid, self.db);
        self.with(|s| f(backend(s, pid, db)))
    }

    // ------------------------------------------------------------ counting

    fn rel(&self, rel: i64, f: impl FnOnce(&mut RelCounts)) {
        if rel == 0 {
            return;
        }
        self.mine(|b| {
            if b.pending_since.is_none() {
                b.pending_since = Some(Instant::now());
            }
            f(b.rels.entry(rel).or_default())
        });
    }

    fn xact(&self, rel: i64, f: impl FnOnce(&mut XactRel)) {
        if rel == 0 {
            return;
        }
        self.mine(|b| {
            if b.pending_since.is_none() {
                b.pending_since = Some(Instant::now());
            }
            let level = b.levels.last_mut().expect("a level");
            f(level.entry(rel).or_default())
        });
    }

    /// A sequential scan began (`pgstat_count_heap_scan`) and returned
    /// `tuples` (`pgstat_count_heap_getnext`).
    pub(crate) fn seq_scan(&self, rel: i64, tuples: i64) {
        self.rel(rel, |c| {
            c.numscans += 1;
            c.tuples_returned += tuples;
        });
    }

    /// An index scan began on `index`, returning `entries` index entries
    /// whose `fetched` heap tuples were fetched.
    pub(crate) fn index_scan(&self, index: i64, entries: i64, fetched: i64) {
        self.rel(index, |c| {
            c.numscans += 1;
            c.tuples_returned += entries;
            c.tuples_fetched += fetched;
        });
    }

    pub(crate) fn inserted(&self, rel: i64, n: i64) {
        self.xact(rel, |x| x.inserted += n);
    }

    pub(crate) fn updated(&self, rel: i64, n: i64, hot: i64) {
        self.xact(rel, |x| x.updated += n);
        if hot > 0 {
            self.rel(rel, |c| c.tuples_hot_updated += hot);
        }
    }

    pub(crate) fn deleted(&self, rel: i64, n: i64) {
        self.xact(rel, |x| x.deleted += n);
    }

    /// TRUNCATE or a DROP of the relation in this level
    /// (`pgstat_count_truncate`).
    pub(crate) fn truncated(&self, rel: i64) {
        self.xact(rel, |x| {
            if !x.truncdropped {
                x.saved = Some((x.inserted, x.updated, x.deleted));
                x.truncdropped = true;
            }
            x.inserted = 0;
            x.updated = 0;
            x.deleted = 0;
        });
        self.with(|s| s.buffers.drop_relation(rel));
    }

    /// Pages `blocks` of `rel` touched (`pgstat_count_buffer_read` /
    /// `_hit`), as the shared buffer pool holds them: under one lock, a
    /// scan's or a statement's batch.
    pub(crate) fn pages(&self, rel: i64, blocks: impl IntoIterator<Item = i64>, capacity: usize) {
        if rel == 0 {
            return;
        }
        let (pid, db) = (self.pid, self.db);
        self.with(|s| {
            let (mut fetched, mut hit) = (0, 0);
            for b in blocks {
                fetched += 1;
                if s.buffers.touch((rel, b), capacity) {
                    hit += 1;
                }
            }
            if fetched == 0 {
                return;
            }
            let be = backend(s, pid, db);
            if be.pending_since.is_none() {
                be.pending_since = Some(Instant::now());
            }
            let c = be.rels.entry(rel).or_default();
            c.blocks_fetched += fetched;
            c.blocks_hit += hit;
        });
    }

    /// A function call that took `total` microseconds, `own` of them its
    /// own (`pgstat_end_function_usage`).
    pub(crate) fn function_call(&self, func: i64, total: i64, own: i64) {
        self.mine(|b| {
            if b.pending_since.is_none() {
                b.pending_since = Some(Instant::now());
            }
            let f = b.funcs.entry(func).or_default();
            f.calls += 1;
            f.total_time += total;
            f.self_time += own;
        });
    }

    pub(crate) fn deadlock(&self) {
        self.mine(|b| b.dbc.deadlocks += 1);
    }

    // ------------------------------------------------- transaction levels

    /// A savepoint was set: a new level.
    pub(crate) fn sub_begin(&self) {
        self.mine(|b| b.levels.push(HashMap::new()));
    }

    /// Levels above `depth` (counting the top level as 1) end: committed
    /// into the level below (RELEASE), or aborted (ROLLBACK TO, which then
    /// also empties the level at `depth`).
    pub(crate) fn sub_end(&self, depth: usize, commit: bool) {
        self.mine(|b| {
            while b.levels.len() > depth.max(1) {
                let top = b.levels.pop().expect("a level");
                if commit {
                    let parent = b.levels.last_mut().expect("a level");
                    for (rel, x) in top {
                        let p = parent.entry(rel).or_default();
                        if x.truncdropped {
                            if !p.truncdropped {
                                p.saved = Some((p.inserted, p.updated, p.deleted));
                                p.truncdropped = true;
                            }
                            p.inserted = x.inserted;
                            p.updated = x.updated;
                            p.deleted = x.deleted;
                        } else {
                            p.inserted += x.inserted;
                            p.updated += x.updated;
                            p.deleted += x.deleted;
                        }
                    }
                } else {
                    abort_level(b, top);
                }
            }
            if !commit && depth > 1 {
                if let Some(level) = b.levels.pop() {
                    abort_level(b, level);
                }
                b.levels.push(HashMap::new());
            }
        });
    }

    /// The transaction ended (`AtEOXact_PgStat`).
    pub(crate) fn end_xact(&self, commit: bool) {
        self.mine(|b| {
            let mut levels = std::mem::take(&mut b.levels);
            // Inner levels left open end with the transaction.
            while levels.len() > 1 {
                let top = levels.pop().expect("a level");
                if commit {
                    let parent = levels.last_mut().expect("a level");
                    for (rel, x) in top {
                        let p = parent.entry(rel).or_default();
                        p.inserted += x.inserted;
                        p.updated += x.updated;
                        p.deleted += x.deleted;
                        p.truncdropped |= x.truncdropped;
                    }
                } else {
                    abort_level(b, top);
                }
            }
            if let Some(top) = levels.pop() {
                if commit {
                    commit_level(b, top);
                } else {
                    abort_level(b, top);
                }
            }
            b.levels.push(HashMap::new());
            if commit {
                b.dbc.xact_commit += 1;
            } else {
                b.dbc.xact_rollback += 1;
            }
            b.snapshot.clear();
            b.snapshot_taken = None;
        });
    }

    /// PREPARE TRANSACTION: the transaction's levels wait under its gid.
    pub(crate) fn prepare(&self, gid: &str) {
        let (pid, db) = (self.pid, self.db);
        self.with(|s| {
            let b = backend(s, pid, db);
            let levels = std::mem::take(&mut b.levels);
            b.levels.push(HashMap::new());
            b.snapshot.clear();
            s.prepared.insert(gid.to_string(), (db, levels));
        });
    }

    /// COMMIT / ROLLBACK PREPARED: the prepared transaction's counts end
    /// with it, in this backend.
    pub(crate) fn finish_prepared(&self, gid: &str, commit: bool) {
        let (pid, db) = (self.pid, self.db);
        self.with(|s| {
            let Some((_, levels)) = s.prepared.remove(gid) else {
                return;
            };
            let b = backend(s, pid, db);
            for level in levels {
                if commit {
                    commit_level(b, level);
                } else {
                    abort_level(b, level);
                }
            }
        });
    }

    /// The counts as they stand, for [`Stats::rewind`]: a statement this
    /// server runs again after a write conflict counts once, as the one
    /// PostgreSQL re-checks row by row does.
    pub(crate) fn checkpoint(&self) -> Checkpoint {
        self.mine(|b| Checkpoint {
            rels: b.rels.clone(),
            levels: b.levels.clone(),
        })
    }

    pub(crate) fn rewind(&self, cp: Checkpoint) {
        self.mine(|b| {
            b.rels = cp.rels;
            b.levels = cp.levels;
        });
    }

    // ------------------------------------------------------------- flushing

    /// `pg_stat_force_next_flush()`.
    pub(crate) fn force_next_flush(&self) {
        self.mine(|b| b.force_next = true);
    }

    /// The backend goes idle (`pgstat_report_stat(false)`): flush if
    /// allowed, else hold the counts for the idle timer.
    pub(crate) fn report(&self, in_block: bool) {
        let (pid, db) = (self.pid, self.db);
        let now = Instant::now();
        self.with(|s| {
            flush_idle(s, now);
            let b = backend(s, pid, db);
            b.idle_since = None;
            if in_block {
                return;
            }
            if !has_pending(b) {
                b.force_next = false;
                return;
            }
            let force = b.force_next
                || b.pending_since
                    .is_some_and(|t| now.duration_since(t) >= MAX_INTERVAL);
            let early = b
                .last_flush
                .is_some_and(|t| now.duration_since(t) < MIN_INTERVAL);
            if early && !force {
                b.idle_since = Some(now);
                return;
            }
            flush_backend(s, pid, now);
        });
    }

    /// A message arrived: the idle timer stops.
    pub(crate) fn wake(&self) {
        self.mine(|b| b.idle_since = None);
    }

    /// The session's state for the session times (`pgstat_report_activity`).
    pub(crate) fn set_state(&self, active: bool, in_block: bool) {
        self.mine(|b| {
            let now = Instant::now();
            if let Some(since) = b.state_since {
                let ms = now.duration_since(since).as_secs_f64() * 1000.0;
                match b.state {
                    SessionState::Active => b.dbc.active_time += ms,
                    SessionState::IdleInTransaction => b.dbc.idle_in_transaction_time += ms,
                    SessionState::Idle => {}
                }
            }
            b.state = if active {
                SessionState::Active
            } else if in_block {
                SessionState::IdleInTransaction
            } else {
                SessionState::Idle
            };
            b.state_since = Some(now);
        });
    }

    /// A session began (`pgstat_report_connect`).
    pub(crate) fn connect(&self) {
        self.mine(|b| {
            b.dbc.sessions += 1;
            b.state_since = Some(Instant::now());
        });
    }

    /// The session ends (`pgstat_shutdown_hook`): its time, how it ended,
    /// and a last flush.
    pub(crate) fn disconnect(&self, session_ms: f64, how: Disconnect) {
        let (pid, db) = (self.pid, self.db);
        self.set_state(false, false);
        self.with(|s| {
            let b = backend(s, pid, db);
            b.dbc.session_time += session_ms;
            match how {
                Disconnect::Normal => {}
                Disconnect::Abandoned => b.dbc.sessions_abandoned += 1,
                Disconnect::Killed => b.dbc.sessions_killed += 1,
            }
            flush_backend(s, pid, Instant::now());
            s.backends.remove(&pid);
        });
    }

    // -------------------------------------------------------------- reading

    /// `pg_stat_clear_snapshot()`.
    pub(crate) fn clear_snapshot(&self) {
        self.mine(|b| {
            b.snapshot.clear();
            b.snapshot_taken = None;
        });
    }

    /// `pg_stat_get_snapshot_timestamp()`.
    pub(crate) fn snapshot_timestamp(&self) -> Option<SystemTime> {
        self.mine(|b| b.snapshot_taken)
    }

    fn read<T: Clone>(
        &self,
        consistency: Consistency,
        key: (u8, i64),
        fetch: impl FnOnce(&Shared, i64) -> Snap,
        unwrap: impl Fn(&Snap) -> Option<T>,
    ) -> Option<T> {
        let (pid, db) = (self.pid, self.db);
        let now = Instant::now();
        self.with(|s| {
            flush_idle(s, now);
            if consistency != Consistency::None {
                if let Some(v) = s.backends.get(&pid).and_then(|b| b.snapshot.get(&key)) {
                    return unwrap(v);
                }
            }
            let v = fetch(s, db);
            let out = unwrap(&v);
            if consistency != Consistency::None {
                let b = backend(s, pid, db);
                b.snapshot.insert(key, v);
                b.snapshot_taken.get_or_insert_with(SystemTime::now);
            }
            out
        })
    }

    /// A relation's shared counts, as this reader sees them.
    pub(crate) fn rel_entry(&self, c: Consistency, rel: i64) -> Option<RelEntry> {
        self.read(
            c,
            (0, rel),
            |s, db| Snap::Rel(s.rels.get(&(rel_db(db, rel), rel)).cloned()),
            |v| match v {
                Snap::Rel(e) => e.clone(),
                _ => None,
            },
        )
    }

    pub(crate) fn func_entry(&self, c: Consistency, func: i64) -> Option<FuncCounts> {
        self.read(
            c,
            (1, func),
            |s, db| Snap::Func(s.funcs.get(&(db, func)).copied()),
            |v| match v {
                Snap::Func(e) => *e,
                _ => None,
            },
        )
    }

    pub(crate) fn db_entry(&self, c: Consistency, dboid: i64) -> Option<DbEntry> {
        self.read(
            c,
            (2, dboid),
            |s, _| Snap::Db(s.dbs.get(&dboid).cloned()),
            |v| match v {
                Snap::Db(e) => e.clone(),
                _ => None,
            },
        )
    }

    /// The backend's own counts not yet flushed, its open transaction's
    /// writes included (`pg_stat_get_xact_*`).
    pub(crate) fn xact_rel(&self, rel: i64) -> Option<RelCounts> {
        self.mine(|b| {
            let mut c = b.rels.get(&rel).copied();
            for level in &b.levels {
                if let Some(x) = level.get(&rel) {
                    let e = c.get_or_insert_with(RelCounts::default);
                    e.tuples_inserted += x.inserted;
                    e.tuples_updated += x.updated;
                    e.tuples_deleted += x.deleted;
                }
            }
            c
        })
    }

    pub(crate) fn xact_func(&self, func: i64) -> Option<FuncCounts> {
        self.mine(|b| b.funcs.get(&func).copied())
    }

    // -------------------------------------------- VACUUM, ANALYZE, resets

    /// `pgstat_report_vacuum`: straight to the shared counts, an
    /// autovacuum worker's under its own times and count.
    pub(crate) fn report_vacuum(&self, rel: i64, live: i64, dead: i64) {
        let db = self.db;
        let auto = crate::autovacuum::in_worker();
        self.with(|s| {
            let e = s.rels.entry((db, rel)).or_default();
            e.live_tuples = live;
            e.dead_tuples = dead;
            e.inserts_since_vacuum = 0;
            if auto {
                e.autovac_vacuum_timestamp = Some(SystemTime::now());
                e.autovac_vacuum_count += 1;
            } else {
                e.vacuum_timestamp = Some(SystemTime::now());
                e.vacuum_count += 1;
            }
        });
    }

    /// `pgstat_report_analyze`.
    pub(crate) fn report_analyze(&self, rel: i64, live: i64, dead: i64) {
        let db = self.db;
        let auto = crate::autovacuum::in_worker();
        self.with(|s| {
            let e = s.rels.entry((db, rel)).or_default();
            e.live_tuples = live;
            e.dead_tuples = dead;
            e.changes_since_analyze = 0;
            if auto {
                e.autovac_analyze_timestamp = Some(SystemTime::now());
                e.autovac_analyze_count += 1;
            } else {
                e.analyze_timestamp = Some(SystemTime::now());
                e.analyze_count += 1;
            }
        });
    }

    /// `pg_stat_reset()`: every counter of this database.
    pub(crate) fn reset_database(&self) {
        let db = self.db;
        self.with(|s| {
            s.rels.retain(|(d, _), _| *d != db);
            s.funcs.retain(|(d, _), _| *d != db);
            s.dbs.insert(
                db,
                DbEntry {
                    stat_reset_timestamp: Some(SystemTime::now()),
                    ..DbEntry::default()
                },
            );
        });
    }

    /// `pg_stat_reset_single_table_counters` /
    /// `_function_counters`: the object's counters, and the database's
    /// reset time.
    pub(crate) fn reset_single(&self, function: bool, oid: i64) {
        let db = self.db;
        self.with(|s| {
            if function {
                s.funcs.remove(&(db, oid));
            } else {
                s.rels.remove(&(rel_db(db, oid), oid));
            }
            s.dbs.entry(db).or_default().stat_reset_timestamp = Some(SystemTime::now());
        });
    }

    /// The live sessions connected to `dboid`.
    pub(crate) fn numbackends(&self, dboid: i64) -> i32 {
        self.with(|s| s.backends.values().filter(|b| b.db == dboid).count() as i32)
    }

    /// The shared counts as one document, for the store at shutdown.
    pub(crate) fn dump(store: usize) -> Document {
        with_shared(store, |s| {
            let mut rels = Vec::new();
            for ((db, rel), e) in &s.rels {
                rels.push(Bson::Document(doc! {
                    "db": db, "rel": rel,
                    "c": [e.numscans, e.tuples_returned, e.tuples_fetched, e.tuples_inserted,
                          e.tuples_updated, e.tuples_deleted, e.tuples_hot_updated, e.live_tuples,
                          e.dead_tuples, e.changes_since_analyze, e.inserts_since_vacuum,
                          e.blocks_fetched, e.blocks_hit, e.vacuum_count, e.autovac_vacuum_count,
                          e.analyze_count, e.autovac_analyze_count],
                    "t": [ts(e.vacuum_timestamp), ts(e.autovac_vacuum_timestamp),
                          ts(e.analyze_timestamp), ts(e.autovac_analyze_timestamp)],
                }));
            }
            let mut funcs = Vec::new();
            for ((db, f), c) in &s.funcs {
                funcs.push(Bson::Document(doc! {
                    "db": db, "f": f, "c": [c.calls, c.total_time, c.self_time],
                }));
            }
            let mut dbs = Vec::new();
            for (db, e) in &s.dbs {
                dbs.push(Bson::Document(doc! {
                    "db": db,
                    "c": [e.xact_commit, e.xact_rollback, e.blocks_fetched, e.blocks_hit,
                          e.tuples_returned, e.tuples_fetched, e.tuples_inserted, e.tuples_updated,
                          e.tuples_deleted, e.deadlocks, e.sessions, e.sessions_abandoned,
                          e.sessions_fatal, e.sessions_killed],
                    "f": [e.session_time, e.active_time, e.idle_in_transaction_time],
                    "t": ts(e.stat_reset_timestamp),
                }));
            }
            doc! { "_id": "stats", "rels": rels, "funcs": funcs, "dbs": dbs }
        })
    }

    /// Fold every backend's pending counts in, for a clean shutdown.
    pub(crate) fn flush_all(store: usize) {
        with_shared(store, |s| {
            let now = Instant::now();
            let pids: Vec<i32> = s.backends.keys().copied().collect();
            for pid in pids {
                if let Some(b) = s.backends.get_mut(&pid) {
                    let levels = std::mem::take(&mut b.levels);
                    for level in levels {
                        abort_level(b, level);
                    }
                }
                flush_backend(s, pid, now);
            }
        });
    }

    /// Forget a store's state (it was closed).
    pub(crate) fn forget_store(store: usize) {
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&store);
    }
}

/// A backend's counts at one moment.
#[derive(Clone)]
pub(crate) struct Checkpoint {
    rels: HashMap<i64, RelCounts>,
    levels: Vec<HashMap<i64, XactRel>>,
}

/// How a session ended (`SessionEndType`): a FATAL error other than a
/// terminate is not told apart from a normal end here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Disconnect {
    Normal,
    /// The client went away without a Terminate (`DISCONNECT_CLIENT_EOF`).
    Abandoned,
    /// `pg_terminate_backend` (`DISCONNECT_KILLED`).
    Killed,
}

fn commit_level(b: &mut Backend, level: HashMap<i64, XactRel>) {
    for (rel, x) in level {
        let c = b.rels.entry(rel).or_default();
        c.tuples_inserted += x.inserted;
        c.tuples_updated += x.updated;
        c.tuples_deleted += x.deleted;
        if x.truncdropped {
            c.truncdropped = true;
            c.delta_live_tuples = 0;
            c.delta_dead_tuples = 0;
        }
        c.delta_live_tuples += x.inserted - x.deleted;
        c.delta_dead_tuples += x.updated + x.deleted;
        c.changed_tuples += x.inserted + x.updated + x.deleted;
    }
}

fn abort_level(b: &mut Backend, level: HashMap<i64, XactRel>) {
    for (rel, mut x) in level {
        if let Some((i, u, d)) = x.saved.take() {
            x.inserted = i;
            x.updated = u;
            x.deleted = d;
        }
        let c = b.rels.entry(rel).or_default();
        c.tuples_inserted += x.inserted;
        c.tuples_updated += x.updated;
        c.tuples_deleted += x.deleted;
        c.delta_dead_tuples += x.inserted + x.updated;
    }
}

fn ts(t: Option<SystemTime>) -> Bson {
    match t {
        Some(t) => Bson::DateTime(bson::DateTime::from_system_time(t)),
        None => Bson::Null,
    }
}

fn restore(s: &mut Shared, d: &Document) {
    let ints = |v: Option<&Bson>| -> Vec<i64> {
        match v {
            Some(Bson::Array(a)) => a.iter().map(|x| crate::bson_i64(x).unwrap_or(0)).collect(),
            _ => Vec::new(),
        }
    };
    let times = |v: Option<&Bson>| -> Vec<Option<SystemTime>> {
        match v {
            Some(Bson::Array(a)) => a.iter().map(time).collect(),
            _ => Vec::new(),
        }
    };
    if let Ok(rels) = d.get_array("rels") {
        for r in rels {
            let Bson::Document(r) = r else { continue };
            let key = (
                crate::bson_i64(r.get("db").unwrap_or(&Bson::Null)).unwrap_or(0),
                crate::bson_i64(r.get("rel").unwrap_or(&Bson::Null)).unwrap_or(0),
            );
            let c = ints(r.get("c"));
            let t = times(r.get("t"));
            let g = |i: usize| c.get(i).copied().unwrap_or(0);
            let tm = |i: usize| t.get(i).copied().flatten();
            s.rels.insert(
                key,
                RelEntry {
                    numscans: g(0),
                    tuples_returned: g(1),
                    tuples_fetched: g(2),
                    tuples_inserted: g(3),
                    tuples_updated: g(4),
                    tuples_deleted: g(5),
                    tuples_hot_updated: g(6),
                    live_tuples: g(7),
                    dead_tuples: g(8),
                    changes_since_analyze: g(9),
                    inserts_since_vacuum: g(10),
                    blocks_fetched: g(11),
                    blocks_hit: g(12),
                    vacuum_count: g(13),
                    autovac_vacuum_count: g(14),
                    analyze_count: g(15),
                    autovac_analyze_count: g(16),
                    vacuum_timestamp: tm(0),
                    autovac_vacuum_timestamp: tm(1),
                    analyze_timestamp: tm(2),
                    autovac_analyze_timestamp: tm(3),
                },
            );
        }
    }
    if let Ok(funcs) = d.get_array("funcs") {
        for f in funcs {
            let Bson::Document(f) = f else { continue };
            let key = (
                crate::bson_i64(f.get("db").unwrap_or(&Bson::Null)).unwrap_or(0),
                crate::bson_i64(f.get("f").unwrap_or(&Bson::Null)).unwrap_or(0),
            );
            let c = ints(f.get("c"));
            let g = |i: usize| c.get(i).copied().unwrap_or(0);
            s.funcs.insert(
                key,
                FuncCounts {
                    calls: g(0),
                    total_time: g(1),
                    self_time: g(2),
                },
            );
        }
    }
    if let Ok(dbs) = d.get_array("dbs") {
        for e in dbs {
            let Bson::Document(e) = e else { continue };
            let db = crate::bson_i64(e.get("db").unwrap_or(&Bson::Null)).unwrap_or(0);
            let c = ints(e.get("c"));
            let g = |i: usize| c.get(i).copied().unwrap_or(0);
            let f: Vec<f64> = match e.get("f") {
                Some(Bson::Array(a)) => a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect(),
                _ => Vec::new(),
            };
            let gf = |i: usize| f.get(i).copied().unwrap_or(0.0);
            s.dbs.insert(
                db,
                DbEntry {
                    xact_commit: g(0),
                    xact_rollback: g(1),
                    blocks_fetched: g(2),
                    blocks_hit: g(3),
                    tuples_returned: g(4),
                    tuples_fetched: g(5),
                    tuples_inserted: g(6),
                    tuples_updated: g(7),
                    tuples_deleted: g(8),
                    deadlocks: g(9),
                    sessions: g(10),
                    sessions_abandoned: g(11),
                    sessions_fatal: g(12),
                    sessions_killed: g(13),
                    session_time: gf(0),
                    active_time: gf(1),
                    idle_in_transaction_time: gf(2),
                    stat_reset_timestamp: time(e.get("t").unwrap_or(&Bson::Null)),
                },
            );
        }
    }
}

fn time(v: &Bson) -> Option<SystemTime> {
    match v {
        Bson::DateTime(d) => Some(d.to_system_time()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(store: usize) -> Stats {
        Stats {
            store,
            pid: 7,
            db: 5,
        }
    }

    #[test]
    fn a_commit_makes_inserts_live_and_an_abort_makes_them_dead() {
        let s = stats(0xA1);
        s.inserted(100, 3);
        s.updated(100, 1, 1);
        s.end_xact(true);
        s.inserted(100, 2);
        s.end_xact(false);
        s.force_next_flush();
        s.report(false);
        let e = s.rel_entry(Consistency::None, 100).expect("an entry");
        assert_eq!(
            (e.tuples_inserted, e.tuples_updated, e.tuples_hot_updated),
            (5, 1, 1)
        );
        assert_eq!(
            (e.live_tuples, e.dead_tuples, e.changes_since_analyze),
            (3, 3, 4)
        );
        Stats::forget_store(0xA1);
    }

    #[test]
    fn a_savepoint_rolled_back_counts_its_inserts_dead() {
        let s = stats(0xA2);
        s.inserted(100, 1);
        s.sub_begin();
        s.inserted(100, 1);
        s.sub_end(2, false);
        s.end_xact(true);
        s.force_next_flush();
        s.report(false);
        let e = s.rel_entry(Consistency::None, 100).expect("an entry");
        assert_eq!((e.tuples_inserted, e.live_tuples, e.dead_tuples), (2, 1, 1));
        Stats::forget_store(0xA2);
    }

    #[test]
    fn a_flush_within_a_second_waits() {
        let s = stats(0xA3);
        s.inserted(100, 1);
        s.end_xact(true);
        s.report(false);
        s.inserted(100, 1);
        s.end_xact(true);
        s.report(false);
        let e = s.rel_entry(Consistency::None, 100).expect("an entry");
        assert_eq!(e.tuples_inserted, 1);
        assert_eq!(s.xact_rel(100).map(|c| c.tuples_inserted), Some(1));
        Stats::forget_store(0xA3);
    }

    #[test]
    fn the_buffer_pool_reads_once_then_hits() {
        let s = stats(0xA4);
        s.pages(100, [0, 0, 0], 16);
        s.force_next_flush();
        s.report(false);
        let e = s.rel_entry(Consistency::None, 100).expect("an entry");
        assert_eq!((e.blocks_fetched, e.blocks_hit), (3, 2));
        Stats::forget_store(0xA4);
    }
}
