//! A backend's system caches (RP-225): the catalog cache (`catcache.c`
//! behind `syscache.c`), the relation cache (`relcache.c`) and the other
//! per-backend caches PostgreSQL 15.19 reads the catalogs through, kept as
//! the SETS of what each backend has loaded, so that a lookup PostgreSQL
//! answers from its cache counts nothing and one it misses counts the
//! catalog scan the miss makes.
//!
//! This server's catalogs are computed, not scanned, so nothing here reads
//! them: [`crate::catalog_reads`] decides which lookups a statement makes,
//! and this module decides which of them are misses and counts each in the
//! cumulative statistics (`pg_stat_sys_tables`, `pg_stat_sys_indexes`):
//!
//! - a syscache miss is an index scan of the cache's index returning the
//!   rows the key matches, 0 or 1 (`SearchCatCacheMiss`), and leaves an
//!   entry, positive or negative, that the next search finds;
//! - `AMOID` and `AMNAME` are always heap scans of `pg_am`
//!   (`IndexScanOK`), returning the one row they find;
//! - a list search (`SearchCatCacheList`) is one index scan returning every
//!   row the partial key matches, and enters each as a member the exact
//!   searches then find;
//! - a relation cache entry, once built, holds its descriptor, index list
//!   and extended-statistics list until an invalidation drops it;
//! - `LookupOpclassInfo`'s cache and the tablespace cache are never
//!   invalidated.
//!
//! Invalidation follows `inval.c`: a change to a catalog row invalidates
//! the entries with that row's key in each cache over the catalog, and every
//! list of those caches (`CatCacheInvalidate`); a relation's change drops
//! its relation cache entry. The changing backend applies its own messages
//! at once (`CommandCounterIncrement`); the others read them from the
//! shared queue when they next accept invalidations, which a committed
//! transaction's messages reach only at commit. A backend that falls too
//! far behind the queue resets every cache (`InvalidateSystemCaches`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::pgstat::Stats;

/// One of PostgreSQL 15's system caches (`cacheinfo` in `syscache.c`) this
/// model reads through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Cache {
    AggFnoid,
    AmOid,
    AmopOpid,
    AmopStrategy,
    AmprocNum,
    AttName,
    AttNum,
    AuthName,
    AuthOid,
    CastSourceTarget,
    ClaOid,
    CollNameEncNsp,
    CollOid,
    DatabaseOid,
    IndexRelid,
    LangName,
    LangOid,
    NamespaceName,
    NamespaceOid,
    OperNameNsp,
    OperOid,
    ProcNameArgsNsp,
    ProcOid,
    PublicationNamespaceMap,
    PublicationRelMap,
    RangeType,
    RelNameNsp,
    RelOid,
    RuleRelName,
    StatRelAttInh,
    TableSpaceOid,
    TypeNameNsp,
    TypeOid,
}

impl Cache {
    /// `(catalog, index)` the cache reads: its misses scan the index.
    pub(crate) fn relations(self) -> (i64, i64) {
        use Cache::*;
        match self {
            AggFnoid => (2600, 2650),
            AmOid => (2601, 2652),
            AmopOpid => (2602, 2654),
            AmopStrategy => (2602, 2653),
            AmprocNum => (2603, 2655),
            AttName => (1249, 2658),
            AttNum => (1249, 2659),
            AuthName => (1260, 2676),
            AuthOid => (1260, 2677),
            CastSourceTarget => (2605, 2661),
            ClaOid => (2616, 2687),
            CollNameEncNsp => (3456, 3164),
            CollOid => (3456, 3085),
            DatabaseOid => (1262, 2672),
            IndexRelid => (2610, 2679),
            LangName => (2612, 2681),
            LangOid => (2612, 2682),
            NamespaceName => (2615, 2684),
            NamespaceOid => (2615, 2685),
            OperNameNsp => (2617, 2689),
            OperOid => (2617, 2688),
            ProcNameArgsNsp => (1255, 2691),
            ProcOid => (1255, 2690),
            PublicationNamespaceMap => (6237, 6239),
            PublicationRelMap => (6106, 6113),
            RangeType => (3541, 3542),
            RelNameNsp => (1259, 2663),
            RelOid => (1259, 2662),
            RuleRelName => (2618, 2693),
            StatRelAttInh => (2619, 2696),
            TableSpaceOid => (1213, 2697),
            TypeNameNsp => (1247, 2704),
            TypeOid => (1247, 2703),
        }
    }

    /// `IndexScanOK`: `pg_am` is always read by a heap scan.
    fn heap_scan(self) -> bool {
        matches!(self, Cache::AmOid)
    }

    /// The caches over one catalog, for an invalidation of its rows.
    fn over(catalog: i64) -> impl Iterator<Item = Cache> {
        ALL.iter()
            .copied()
            .filter(move |c| c.relations().0 == catalog)
    }
}

const ALL: [Cache; 33] = [
    Cache::AggFnoid,
    Cache::AmOid,
    Cache::AmopOpid,
    Cache::AmopStrategy,
    Cache::AmprocNum,
    Cache::AttName,
    Cache::AttNum,
    Cache::AuthName,
    Cache::AuthOid,
    Cache::CastSourceTarget,
    Cache::ClaOid,
    Cache::CollNameEncNsp,
    Cache::CollOid,
    Cache::DatabaseOid,
    Cache::IndexRelid,
    Cache::LangName,
    Cache::LangOid,
    Cache::NamespaceName,
    Cache::NamespaceOid,
    Cache::OperNameNsp,
    Cache::OperOid,
    Cache::ProcNameArgsNsp,
    Cache::ProcOid,
    Cache::PublicationNamespaceMap,
    Cache::PublicationRelMap,
    Cache::RangeType,
    Cache::RelNameNsp,
    Cache::RelOid,
    Cache::RuleRelName,
    Cache::StatRelAttInh,
    Cache::TableSpaceOid,
    Cache::TypeNameNsp,
    Cache::TypeOid,
];

/// One column of a cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum K {
    O(i64),
    N(String),
}

impl From<i64> for K {
    fn from(v: i64) -> Self {
        K::O(v)
    }
}

impl From<&str> for K {
    fn from(v: &str) -> Self {
        K::N(v.to_string())
    }
}

impl From<String> for K {
    fn from(v: String) -> Self {
        K::N(v)
    }
}

pub(crate) type Key = Vec<K>;

/// An invalidation message (`SharedInvalidationMessage`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Inval {
    /// A catalog row with this key in `cache` changed: the entry goes, and
    /// with it every list of the cache.
    Cat(Cache, Key),
    /// Every entry of `cache` whose key starts with this prefix (the rows
    /// of one relation in `pg_attribute` or `pg_statistic`), and the
    /// cache's lists.
    CatPrefix(Cache, Key),
    /// A relation changed: its relation cache entry is rebuilt on next use.
    Rel(i64),
    /// Everything (`InvalidateSystemCaches`).
    Reset,
}

/// What one statement left behind for the next time this backend runs it:
/// the scans its executor makes of catalog relations, counted again on
/// every run, valid while nothing was invalidated since.
#[derive(Debug, Clone)]
pub(crate) struct Memo {
    generation: u64,
    catalog_version: u64,
    pub(crate) scans: Vec<Scan>,
}

/// A scan counted against a catalog relation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Scan {
    /// An index scan of `index` returning `n` live rows.
    Index(i64, i64),
    /// A sequential scan of `rel` returning `n` rows.
    Seq(i64, i64),
}

/// One backend's caches.
#[derive(Debug, Default)]
pub(crate) struct Backend {
    entries: HashSet<(Cache, Key)>,
    lists: HashSet<(Cache, Key)>,
    /// Relations whose descriptor this backend built (or read from the
    /// init file): their `RelationBuildDesc` is done.
    relcache: HashSet<i64>,
    /// Relations whose index list (`RelationGetIndexList`) is loaded.
    index_lists: HashSet<i64>,
    /// Relations whose extended-statistics list is loaded.
    statext_lists: HashSet<i64>,
    /// Relations whose publication descriptor is built.
    pubdescs: HashSet<i64>,
    /// `LookupOpclassInfo`'s cache: never invalidated.
    pub(crate) opclasses: HashSet<i64>,
    /// `spccache.c`: never invalidated by a relation's change.
    pub(crate) tablespaces: HashSet<i64>,
    /// `typcache.c`: each type's `TCFLAGS_CHECKED_*` flags.
    pub(crate) typcache: HashMap<i64, u32>,
    /// `baseSearchPathValid`: the path setting and role it was computed
    /// for.
    pub(crate) search_path: Option<String>,
    /// Bumped whenever an invalidation drops anything: a memo from before
    /// is stale.
    generation: u64,
    /// Where this backend is in its store's invalidation queue.
    inval_pos: u64,
    /// `QUEUE_SERIAL` when this backend last read the queue.
    serial_seen: u64,
    /// Messages of this backend's open transaction, sent at commit.
    pending: Vec<Inval>,
    /// Catalog index entries its open transaction left dead.
    pending_dead: Vec<(i64, Key)>,
    memo: HashMap<u64, Memo>,
    pub(crate) started: bool,
}

impl Backend {
    // ------------------------------------------------------------ searches

    /// `SearchCatCache`: a hit counts nothing; a miss scans the cache's
    /// index (or `pg_am`'s heap) for the `found` rows (0 or 1) and leaves
    /// an entry. True when it missed.
    pub(crate) fn search(&mut self, st: &Counter, cache: Cache, key: Key, found: i64) -> bool {
        if self.entries.contains(&(cache, key.clone())) {
            return false;
        }
        let (catalog, index) = cache.relations();
        if cache.heap_scan() {
            st.seq(catalog, found);
        } else {
            st.index_key(index, &key, found);
        }
        self.entries.insert((cache, key));
        true
    }

    /// Whether `search` would hit.
    pub(crate) fn cached(&self, cache: Cache, key: &Key) -> bool {
        self.entries.contains(&(cache, key.clone()))
    }

    /// `SearchCatCacheList`: a miss is one index scan returning every
    /// `member` (each a full key), which the exact searches then find.
    pub(crate) fn search_list(
        &mut self,
        st: &Counter,
        cache: Cache,
        key: Key,
        members: impl FnOnce() -> Vec<Key>,
    ) -> bool {
        if self.lists.contains(&(cache, key.clone())) {
            return false;
        }
        let members = members();
        st.index_key(cache.relations().1, &key, members.len() as i64);
        for m in members {
            self.entries.insert((cache, m));
        }
        self.lists.insert((cache, key));
        true
    }

    /// Whether `rel`'s relation cache entry is built.
    pub(crate) fn has_relation(&self, rel: i64) -> bool {
        self.relcache.contains(&rel)
    }

    /// Mark `rel`'s descriptor built; true if it was not.
    pub(crate) fn build_relation(&mut self, rel: i64) -> bool {
        self.relcache.insert(rel)
    }

    /// `RelationGetIndexList`: true when the list must be read.
    pub(crate) fn load_index_list(&mut self, rel: i64) -> bool {
        self.index_lists.insert(rel)
    }

    /// `RelationBuildPublicationDesc`: true when it must be built.
    pub(crate) fn build_pubdesc(&mut self, rel: i64) -> bool {
        self.pubdescs.insert(rel)
    }

    /// `RelationGetStatExtList`: true when the list must be read.
    pub(crate) fn load_statext_list(&mut self, rel: i64) -> bool {
        self.statext_lists.insert(rel)
    }

    // ------------------------------------------------------------ the memo

    pub(crate) fn memo(&self, text: u64, catalog_version: u64) -> Option<&Memo> {
        self.memo
            .get(&text)
            .filter(|m| m.generation == self.generation && m.catalog_version == catalog_version)
    }

    pub(crate) fn remember(&mut self, text: u64, catalog_version: u64, scans: Vec<Scan>) {
        if self.memo.len() > 4096 {
            self.memo.clear();
        }
        self.memo.insert(
            text,
            Memo {
                generation: self.generation,
                catalog_version,
                scans,
            },
        );
    }

    // -------------------------------------------------------- invalidation

    fn apply(&mut self, msg: &Inval) {
        let before = (
            self.entries.len(),
            self.lists.len(),
            self.relcache.len(),
            self.index_lists.len(),
            self.statext_lists.len(),
        );
        match msg {
            Inval::Cat(cache, key) => {
                self.entries.remove(&(*cache, key.clone()));
                self.lists.retain(|(c, _)| c != cache);
            }
            Inval::CatPrefix(cache, prefix) => {
                self.entries
                    .retain(|(c, k)| c != cache || !k.starts_with(prefix));
                self.lists.retain(|(c, _)| c != cache);
            }
            Inval::Rel(rel) => {
                self.relcache.remove(rel);
                self.index_lists.remove(rel);
                self.statext_lists.remove(rel);
                self.pubdescs.remove(rel);
            }
            Inval::Reset => {
                self.entries.clear();
                self.lists.clear();
                // Every relation cache entry is rebuilt; the init file's
                // own are reread with it.
                self.relcache.clear();
                self.index_lists.clear();
                self.statext_lists.clear();
                self.pubdescs.clear();
                self.typcache.clear();
                self.search_path = None;
            }
        }
        // A namespace's change makes the search path recomputed
        // (`NamespaceCallback`).
        if matches!(msg, Inval::Cat(Cache::NamespaceName | Cache::NamespaceOid, _) | Inval::Reset)
        {
            self.search_path = None;
        }
        // A type's change drops its type cache entry (`TypeCacheTypCallback`);
        // an operator-class change drops them all (`TypeCacheOpcCallback`).
        match msg {
            // `TypeCacheOpcCallback`: every entry's operator flags.
            Inval::Cat(Cache::ClaOid, _) => self.typcache.clear(),
            _ => {}
        }
        let after = (
            self.entries.len(),
            self.lists.len(),
            self.relcache.len(),
            self.index_lists.len(),
            self.statext_lists.len(),
        );
        if before != after || matches!(msg, Inval::Reset) {
            self.generation += 1;
        }
    }
}

/// Where a backend's counts go: its statistics handle, unless
/// `track_counts` is off.
pub(crate) struct Counter {
    pub(crate) stats: Option<Stats>,
    /// The store whose dead catalog index entries a scan meets.
    pub(crate) store: usize,
}

impl Counter {
    pub(crate) fn index(&self, index: i64, rows: i64) {
        if let Some(s) = &self.stats {
            s.index_scan(index, rows, rows);
        }
    }

    /// An index scan of `index` whose keys start with `key`: the live rows
    /// it fetches, and the entries of rows a committed change left dead,
    /// which it returns once and then marks killed (`_bt_killitems`), so
    /// that no later scan returns them.
    pub(crate) fn index_key(&self, index: i64, key: &Key, rows: i64) {
        let dead = take_dead(self.store, index, key);
        if let Some(s) = &self.stats {
            s.index_scan(index, rows + dead, rows);
        }
    }

    pub(crate) fn seq(&self, rel: i64, rows: i64) {
        if let Some(s) = &self.stats {
            s.seq_scan(rel, rows);
        }
    }

    pub(crate) fn scan(&self, scan: Scan) {
        match scan {
            Scan::Index(i, n) => self.index(i, n),
            Scan::Seq(r, n) => self.seq(r, n),
        }
    }
}

// ------------------------------------------------------- the shared queue

/// How many messages the queue keeps (`MAXNUMMESSAGES`): a backend further
/// behind resets its caches.
const QUEUE_SIZE: usize = 4096;

#[derive(Debug, Default)]
struct Queue {
    /// The position of `messages[0]`.
    base: u64,
    messages: VecDeque<(i32, Inval)>,
    /// Catalog index entries whose rows a committed transaction deleted or
    /// updated out of place, not yet killed: `(index, the entry's key)`.
    dead: Vec<(i64, Key)>,
}

/// Take the dead entries of `index` a scan whose keys start with `key`
/// returns.
fn take_dead(store: usize, index: i64, key: &Key) -> i64 {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    let Some(q) = q.get_mut(&store) else {
        return 0;
    };
    if q.dead.is_empty() {
        return 0;
    }
    let before = q.dead.len();
    q.dead
        .retain(|(i, k)| !(*i == index && k.starts_with(key)));
    (before - q.dead.len()) as i64
}

fn queues() -> &'static Mutex<HashMap<usize, Queue>> {
    static Q: OnceLock<Mutex<HashMap<usize, Queue>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Every queue's end, summed: a backend that saw this value has nothing to
/// read.
static QUEUE_SERIAL: AtomicU64 = AtomicU64::new(0);

fn queue_end(store: usize) -> u64 {
    let q = queues().lock().unwrap_or_else(|e| e.into_inner());
    q.get(&store)
        .map_or(0, |q| q.base + q.messages.len() as u64)
}

impl Backend {
    /// A new backend starts at the queue's end (`SharedInvalBackendInit`).
    pub(crate) fn attach(&mut self, store: usize) {
        self.serial_seen = QUEUE_SERIAL.load(Ordering::Acquire);
        self.inval_pos = queue_end(store);
    }

    /// `AcceptInvalidationMessages`: apply what other backends sent since
    /// this one last looked.
    pub(crate) fn accept(&mut self, store: usize, pid: i32) {
        let serial = QUEUE_SERIAL.load(Ordering::Acquire);
        if serial == self.serial_seen {
            return;
        }
        self.serial_seen = serial;
        let msgs: Option<Vec<Inval>> = {
            let q = queues().lock().unwrap_or_else(|e| e.into_inner());
            let Some(q) = q.get(&store) else {
                return;
            };
            let end = q.base + q.messages.len() as u64;
            if self.inval_pos >= end {
                return;
            }
            if self.inval_pos < q.base {
                None
            } else {
                let from = (self.inval_pos - q.base) as usize;
                let out = q
                    .messages
                    .iter()
                    .skip(from)
                    .filter(|(p, _)| *p != pid)
                    .map(|(_, m)| m.clone())
                    .collect();
                self.inval_pos = end;
                Some(out)
            }
        };
        match msgs {
            Some(msgs) => {
                for m in &msgs {
                    self.apply(m);
                }
            }
            None => {
                // Fell off the queue: everything is reset.
                self.inval_pos = queue_end(store);
                self.apply(&Inval::Reset);
            }
        }
    }

    /// This backend changed the catalogs: its own caches see it at once,
    /// the others at commit.
    pub(crate) fn local(&mut self, msgs: Vec<Inval>) {
        for m in &msgs {
            self.apply(m);
        }
        self.pending.extend(msgs);
    }

    /// Catalog index entries this backend's change left dead: killable once
    /// its transaction commits.
    pub(crate) fn left_dead(&mut self, entries: Vec<(i64, Key)>) {
        self.pending_dead.extend(entries);
    }

    /// The transaction ended: a commit sends its messages; an abort
    /// applies them again here (the cached rows it read may be the
    /// aborted ones) and sends nothing.
    pub(crate) fn end_xact(&mut self, store: usize, pid: i32, commit: bool) {
        let pending = std::mem::take(&mut self.pending);
        let dead = std::mem::take(&mut self.pending_dead);
        if commit && !dead.is_empty() {
            let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
            q.entry(store).or_default().dead.extend(dead);
        }
        if pending.is_empty() {
            return;
        }
        if !commit {
            for m in &pending {
                self.apply(m);
            }
            return;
        }
        let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
        let q = q.entry(store).or_default();
        for m in pending {
            q.messages.push_back((pid, m));
        }
        while q.messages.len() > QUEUE_SIZE {
            q.messages.pop_front();
            q.base += 1;
        }
        QUEUE_SERIAL.fetch_add(1, Ordering::Release);
    }
}

/// Forget a store's queue (its server stopped).
pub(crate) fn forget_store(store: usize) {
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&store);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter() -> Counter {
        Counter {
            stats: None,
            store: 0,
        }
    }

    #[test]
    fn a_miss_leaves_an_entry_the_next_search_finds() {
        let mut b = Backend::default();
        let c = counter();
        assert!(b.search(&c, Cache::TypeOid, vec![K::O(23)], 1));
        assert!(!b.search(&c, Cache::TypeOid, vec![K::O(23)], 1));
        // A negative entry is an entry too.
        assert!(b.search(&c, Cache::RelNameNsp, vec!["t".into(), K::O(11)], 0));
        assert!(!b.search(&c, Cache::RelNameNsp, vec!["t".into(), K::O(11)], 0));
    }

    #[test]
    fn a_list_enters_its_members() {
        let mut b = Backend::default();
        let c = counter();
        b.search_list(&c, Cache::AmopOpid, vec![K::O(96)], || {
            vec![vec![K::O(96), "s".into(), K::O(1976)]]
        });
        assert!(b.cached(Cache::AmopOpid, &vec![K::O(96), "s".into(), K::O(1976)]));
    }

    #[test]
    fn a_row_change_drops_its_entry_and_every_list_of_the_cache() {
        let mut b = Backend::default();
        let c = counter();
        b.search(&c, Cache::ProcOid, vec![K::O(1)], 1);
        b.search_list(&c, Cache::ProcNameArgsNsp, vec!["f".into()], Vec::new);
        b.local(vec![Inval::Cat(Cache::ProcNameArgsNsp, vec!["g".into()])]);
        assert!(b.cached(Cache::ProcOid, &vec![K::O(1)]));
        assert!(b.search_list(&c, Cache::ProcNameArgsNsp, vec!["f".into()], Vec::new));
    }

    #[test]
    fn other_backends_see_a_change_at_commit() {
        let store = 0xfeed;
        let c = counter();
        let mut a = Backend::default();
        let mut b = Backend::default();
        a.attach(store);
        b.attach(store);
        b.search(&c, Cache::RelOid, vec![K::O(16384)], 1);
        b.build_relation(16384);
        a.local(vec![Inval::Rel(16384), Inval::Cat(Cache::RelOid, vec![K::O(16384)])]);
        b.accept(store, 2);
        assert!(b.has_relation(16384));
        a.end_xact(store, 1, true);
        b.accept(store, 2);
        assert!(!b.has_relation(16384));
        assert!(!b.cached(Cache::RelOid, &vec![K::O(16384)]));
        forget_store(store);
    }

    #[test]
    fn an_aborted_change_reaches_no_one_else() {
        let store = 0xfeee;
        let c = counter();
        let mut a = Backend::default();
        let mut b = Backend::default();
        a.attach(store);
        b.attach(store);
        b.build_relation(16384);
        b.search(&c, Cache::RelOid, vec![K::O(16384)], 1);
        a.local(vec![Inval::Rel(16384)]);
        a.end_xact(store, 1, false);
        b.accept(store, 2);
        assert!(b.has_relation(16384));
        forget_store(store);
    }
}
