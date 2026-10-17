//! mongod's lock manager: hierarchical resource locks with acquisition
//! counting, the substrate for `serverStatus.locks` (RM-034) and for
//! transaction-held collection locks that `drop` / `createIndexes` /
//! `renameCollection` / `collMod` wait on (RM-084).
//!
//! The model is mongod's (mongo/db/concurrency): resources are `Global`,
//! `Database` and `Collection` (plus the special ones mongod reports,
//! `Mutex`, `Metadata`, `oplog`, `ReplicationStateTransition`,
//! `MultiDocumentTransactionsBarrier`), and a lock is taken in one of four
//! modes — intent-shared (`r`), intent-exclusive (`w`), shared (`R`) or
//! exclusive (`W`). A request waits while a holder has a mode the conflict
//! table rejects; intent modes exist so a hierarchy is locked top-down (a
//! collection `w` is taken holding the database's and the global's `w`).
//! Every acquisition is counted per resource and mode, and a wait also
//! counts `acquireWaitCount` and `timeAcquiringMicros` — exactly what
//! `serverStatus.locks` reports. The section is generated from these
//! counters, never a table of plausible counts.
//!
//! Locks are held either for one statement (a `find` takes `Global.r`,
//! `Database.r`, `Collection.r` and lets them go at the end) or to the end
//! of a transaction (a statement inside a transaction keeps the
//! collection's `r` or `w` until commit/abort — what a concurrent `drop`
//! then waits on, answering 50 at its `maxTimeMS`). Waits poll the
//! request's deadline (`secantus_core::deadline`), so a spent budget ends
//! the wait with `MaxTimeMsExpired`; with no budget armed the wait is
//! unbounded, as mongod's is without `maxTimeMS`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use secantus_core::deadline::MaxTimeMsExpired;

/// The four lock modes, mongod's letters for them, weakest first.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum Mode {
    /// Intent shared — mongod's `r`.
    Is,
    /// Intent exclusive — mongod's `w`.
    Ix,
    /// Shared — mongod's `R`.
    S,
    /// Exclusive — mongod's `W`.
    X,
}

impl Mode {
    /// mongod's one-letter name, as `serverStatus.locks` shows it.
    pub fn letter(self) -> &'static str {
        match self {
            Mode::Is => "r",
            Mode::Ix => "w",
            Mode::S => "R",
            Mode::X => "W",
        }
    }

    /// mongod's conflict table: a request of `self` cannot be granted while
    /// a holder has `held`. Everything self-compatible but X.
    pub fn conflicts_with(self, held: Mode) -> bool {
        use Mode::*;
        matches!(
            (self, held),
            (S, Ix) | (S, X) | (X, Is) | (X, Ix) | (X, S) | (X, X) | (Ix, S) | (Ix, X)
        )
    }
}

/// What a lock is taken on. `Database` and `Collection` carry the name;
/// `serverStatus.locks` groups the counts under the resource kind, as
/// mongod does.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Resource {
    Global,
    Database(String),
    Collection(String),
    Mutex,
    Metadata,
    Oplog,
    ReplicationStateTransition,
    MdtBarrier,
}

impl Resource {
    /// The resource kind, as `serverStatus.locks` keys it.
    pub fn kind(&self) -> &'static str {
        match self {
            Resource::Global => "Global",
            Resource::Database(_) => "Database",
            Resource::Collection(_) => "Collection",
            Resource::Mutex => "Mutex",
            Resource::Metadata => "Metadata",
            Resource::Oplog => "oplog",
            Resource::ReplicationStateTransition => "ReplicationStateTransition",
            Resource::MdtBarrier => "MultiDocumentTransactionsBarrier",
        }
    }
}

/// Who holds a lock: a transaction (held to its end) or one statement.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Owner {
    /// A multi-document transaction; its locks release at commit/abort.
    Transaction(u64),
    /// One statement; released when the statement ends.
    Statement(u64),
}

#[derive(Default)]
struct Head {
    /// Every hold, to release precisely and to tell whom a waiter blocks on.
    holders: Vec<(Owner, Mode)>,
}

impl Head {
    fn blocks(&self, owner: Owner, mode: Mode) -> bool {
        self.holders
            .iter()
            .any(|(o, m)| *o != owner && mode.conflicts_with(*m))
    }
}

/// Per-resource, per-mode counters, the shape `serverStatus.locks` shows.
#[derive(Default)]
struct Stats {
    acquire_count: HashMap<Mode, u64>,
    acquire_wait_count: HashMap<Mode, u64>,
    time_acquiring_micros: HashMap<Mode, u64>,
}

/// Per-mode counter values, `(mode letter, count)` pairs — mongod's letters.
pub type ModeCounts = Vec<(&'static str, u64)>;

/// One resource kind's counters, the `serverStatus.locks` shape.
pub struct ResourceReport {
    pub kind: &'static str,
    pub acquire_count: ModeCounts,
    pub acquire_wait_count: ModeCounts,
    pub time_acquiring_micros: ModeCounts,
}

#[derive(Default)]
struct Inner {
    heads: HashMap<Resource, Head>,
    stats: HashMap<&'static str, Stats>,
}

/// The server-wide lock manager, one per server: built into the server's
/// `Shared` and reached from handlers through `CommandContext`, like the
/// transaction registry. (`OnceLock` process-wide singletons are the other
/// convention; this one is per-server because its counters ARE the
/// server's `serverStatus.locks`, and two servers in one process — the
/// gauges run them embedded — must not count each other's locks.)
pub struct LockManager {
    inner: Mutex<Inner>,
    /// Waiters sleep on this; a release broadcasts. One condvar for every
    /// resource: lock waits are rare, broadcasts are cheap, and the
    /// backlog's lock-fairness item is the place queue order would come
    /// from if a workload ever needs it.
    changed: Condvar,
    next_owner: AtomicU64,
}

impl Default for LockManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LockManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            changed: Condvar::new(),
            next_owner: AtomicU64::new(1),
        }
    }

    /// A fresh statement owner.
    pub fn statement_owner(&self) -> Owner {
        Owner::Statement(self.next_owner.fetch_add(1, Ordering::Relaxed))
    }

    /// A fresh transaction owner (a transaction keeps its locks to its
    /// end; the registry hands one out per open transaction).
    pub fn transaction_owner(&self) -> Owner {
        Owner::Transaction(self.next_owner.fetch_add(1, Ordering::Relaxed))
    }

    /// Take `mode` on `resource` for `owner`, waiting out conflicts until
    /// they clear or the request's deadline spends. Counts the acquisition,
    /// and the wait when there was one; a wait that ends on the deadline
    /// counts as a wait but leaves no hold.
    pub fn acquire(
        &self,
        owner: Owner,
        resource: &Resource,
        mode: Mode,
    ) -> Result<(), MaxTimeMsExpired> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let began = Instant::now();
        let mut waited = false;
        loop {
            let head = inner.heads.entry(resource.clone()).or_default();
            if !head.blocks(owner, mode) {
                head.holders.push((owner, mode));
                let wait = waited.then(|| began.elapsed());
                drop(inner);
                self.count(resource, mode, wait);
                return Ok(());
            }
            if secantus_core::deadline::expired() {
                drop(inner);
                self.count(resource, mode, Some(began.elapsed()));
                return Err(MaxTimeMsExpired);
            }
            waited = true;
            let (guard, _) = self
                .changed
                .wait_timeout(inner, Duration::from_millis(5))
                .unwrap_or_else(|e| e.into_inner());
            inner = guard;
        }
    }

    /// Release every hold `owner` has, waking waiters.
    pub fn release(&self, owner: Owner) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut any = false;
        for head in inner.heads.values_mut() {
            let before = head.holders.len();
            head.holders.retain(|(o, _)| *o != owner);
            any |= head.holders.len() != before;
        }
        inner.heads.retain(|_, h| !h.holders.is_empty());
        drop(inner);
        if any {
            self.changed.notify_all();
        }
    }

    /// The `serverStatus.locks` document: per resource kind, its per-mode
    /// counts. Modes with no acquisitions are omitted, and
    /// `acquireWaitCount` / `timeAcquiringMicros` appear only for modes
    /// that waited — mongod's rendering.
    /// The `serverStatus.locks` document: per resource kind, its per-mode
    /// counts. Modes with no acquisitions are omitted, and
    /// `acquireWaitCount` / `timeAcquiringMicros` appear only for modes
    /// that waited — mongod's rendering.
    pub fn server_status_locks(&self) -> Vec<ResourceReport> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut kinds: Vec<&'static str> = inner.stats.keys().copied().collect();
        kinds.sort();
        kinds
            .into_iter()
            .map(|kind| {
                let s = &inner.stats[kind];
                let mut modes: Vec<Mode> = s.acquire_count.keys().copied().collect();
                modes.sort();
                let acquire_count = modes
                    .iter()
                    .map(|m| (m.letter(), s.acquire_count[m]))
                    .collect();
                let acquire_wait_count = modes
                    .iter()
                    .filter_map(|m| s.acquire_wait_count.get(m).map(|n| (m.letter(), *n)))
                    .collect();
                let time_acquiring_micros = modes
                    .iter()
                    .filter_map(|m| s.time_acquiring_micros.get(m).map(|n| (m.letter(), *n)))
                    .collect();
                ResourceReport {
                    kind,
                    acquire_count,
                    acquire_wait_count,
                    time_acquiring_micros,
                }
            })
            .collect()
    }

    fn count(&self, resource: &Resource, mode: Mode, waited: Option<Duration>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let s = inner.stats.entry(resource.kind()).or_default();
        *s.acquire_count.entry(mode).or_default() += 1;
        if let Some(d) = waited {
            *s.acquire_wait_count.entry(mode).or_default() += 1;
            *s.time_acquiring_micros.entry(mode).or_default() += d.as_micros() as u64;
        }
    }
}

/// What a statement does to the data, read from the command name the way
/// mongod's dispatch classifies it: reads take intent-shared, writes
/// intent-exclusive, and DDL takes the collection exclusively.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Read,
    Write,
    Ddl,
}

/// mongod's class for a command (mongo/db/commands / the lock-taking
/// patterns in each command's implementation).
pub fn classify(name: &str) -> Class {
    match name {
        "find" | "getMore" | "count" | "distinct" | "aggregate" | "collStats" | "dbStats"
        | "listCollections" | "listIndexes" | "indexStats" | "dataSize" | "storageDetails" => {
            Class::Read
        }
        // `createIndexes` takes the collection exclusively: mongod's
        // non-concurrent build blocks writers, and an open transaction's
        // collection lock must block the build (RM-084's decision names it
        // among the waiters; an `IX` would be compatible with the `IX` a
        // writing transaction holds and never wait).
        "drop" | "create" | "collMod" | "renameCollection" | "dropDatabase" | "dropIndexes"
        | "dropIndexesForUUID" | "convertToCapped" | "createIndexes" => Class::Ddl,
        _ => Class::Write,
    }
}

/// The lock chain a statement takes: mongod's hierarchy, global first, then
/// the database, then the collection when the command names one — reads
/// intent-shared all the way, writes intent-exclusive, DDL's collection lock
/// exclusive. `renameCollection` names a second namespace in `to`. Commands
/// that address no database (the pure-global admin family: `hello`,
/// `serverStatus`, `isMaster`, `ping`, …) take the global level alone, as
/// mongod's Global lock is the one every operation passes through.
pub fn statement_chain(doc: &bson::Document, db: &str) -> Vec<(Resource, Mode)> {
    use Mode::*;
    let Some(name) = doc.keys().next().map(String::as_str) else {
        return vec![(Resource::Global, Is)];
    };
    let class = classify(name);
    let intent = match class {
        Class::Read => Is,
        Class::Write | Class::Ddl => Ix,
    };
    // The pure-global family: no $db addressing beyond the administrative.
    const GLOBAL_ONLY: &[&str] = &[
        "hello",
        "isMaster",
        "ismaster",
        "serverStatus",
        "ping",
        "whatsmyuri",
        "features",
        "hostInfo",
        "connectionStatus",
        "logout",
        "saslStart",
        "saslContinue",
        "authenticate",
        "listDatabases",
        "replSetGetStatus",
        "replSetGetConfig",
    ];
    let mut chain = vec![(Resource::Global, intent)];
    if GLOBAL_ONLY.contains(&name) {
        return chain;
    }
    let coll = doc.get_str(name).ok().filter(|s| !s.is_empty());
    // `renameCollection` (an admin command) names QUALIFIED namespaces in
    // its arguments — the source in the command name, the target in `to`;
    // every other collection-taking command names one in the `$db` it runs
    // against.
    let ns_of = |c: &str| {
        if name == "renameCollection" {
            c.to_string()
        } else {
            format!("{db}.{c}")
        }
    };
    let db_mode = if matches!(name, "createDatabase" | "dropDatabase") {
        X
    } else {
        intent
    };
    let db_level = coll
        .map(|c| ns_of(c).split('.').next().unwrap_or(db).to_string())
        .unwrap_or_else(|| db.to_string());
    chain.push((Resource::Database(db_level), db_mode));
    if let Some(coll) = coll {
        let mode = if class == Class::Ddl { X } else { intent };
        chain.push((Resource::Collection(ns_of(coll)), mode));
    }
    if name == "renameCollection" {
        if let Ok(to) = doc.get_str("to") {
            chain.push((Resource::Collection(to.to_string()), X));
        }
    }
    chain
}

/// Take a chain in order; a link that fails (the deadline spent mid-chain)
/// releases what the chain took, leaving no stray holds.
pub fn acquire_chain(
    lm: &LockManager,
    owner: Owner,
    chain: &[(Resource, Mode)],
) -> Result<(), secantus_core::deadline::MaxTimeMsExpired> {
    for (res, mode) in chain {
        if lm.acquire(owner, res, *mode).is_err() {
            lm.release(owner);
            return Err(secantus_core::deadline::MaxTimeMsExpired);
        }
    }
    Ok(())
}

/// A statement's holds, released on drop (the statement's end — every path,
/// error replies included).
pub struct Guard {
    lm: std::sync::Arc<LockManager>,
    owner: Owner,
}

impl Guard {
    /// Take the statement's chain for one request; `None` when the server
    /// has no lock manager wired (unit tests, off-server contexts) — locking
    /// is best-effort there, as the other `Option<Arc<_>>` services are.
    pub fn statement(
        lm: Option<&std::sync::Arc<LockManager>>,
        doc: &bson::Document,
        db: &str,
    ) -> Option<Result<Self, crate::CommandError>> {
        let lm = lm?;
        let chain = statement_chain(doc, db);
        let owner = lm.statement_owner();
        match acquire_chain(lm, owner, &chain) {
            Ok(()) => Some(Ok(Self {
                lm: lm.clone(),
                owner,
            })),
            Err(_) => Some(Err(crate::CommandError::max_time_expired())),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.lm.release(self.owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secantus_core::deadline;

    #[test]
    fn the_conflict_table_is_mongod_s() {
        use Mode::*;
        // IS is compatible with everything, including X.
        for m in [Is, Ix, S, X] {
            assert!(!Is.conflicts_with(m), "IS vs {m:?}");
        }
        assert!(Ix.conflicts_with(S));
        assert!(Ix.conflicts_with(X));
        assert!(!Ix.conflicts_with(Ix));
        assert!(S.conflicts_with(Ix));
        assert!(S.conflicts_with(X));
        assert!(!S.conflicts_with(S));
        // X conflicts with every mode.
        for m in [Is, Ix, S, X] {
            assert!(X.conflicts_with(m), "X vs {m:?}");
        }
    }

    #[test]
    fn an_owner_never_blocks_itself() {
        let lm = LockManager::new();
        let o = lm.statement_owner();
        lm.acquire(o, &Resource::Collection("db.c".into()), Mode::X)
            .unwrap();
        // Re-entrant at a non-conflicting mode (a statement's own chain).
        lm.acquire(o, &Resource::Collection("db.c".into()), Mode::Is)
            .unwrap();
        lm.release(o);
    }

    #[test]
    fn uncontended_acquire_counts_without_a_wait() {
        let lm = LockManager::new();
        let owner = lm.statement_owner();
        lm.acquire(owner, &Resource::Global, Mode::Ix).unwrap();
        lm.release(owner);
        let rows = lm.server_status_locks();
        let row = &rows[0];
        assert_eq!(row.kind, "Global");
        assert_eq!(row.acquire_count, [("w", 1)]);
        assert!(row.acquire_wait_count.is_empty());
        assert!(row.time_acquiring_micros.is_empty());
    }

    #[test]
    fn a_conflicting_holder_is_waited_on_and_counted() {
        let lm = std::sync::Arc::new(LockManager::new());
        let a = lm.statement_owner();
        let b = lm.statement_owner();
        let res = Resource::Collection("db.c".into());
        lm.acquire(a, &res, Mode::X).unwrap();
        let lm2 = lm.clone();
        let t = std::thread::spawn(move || {
            let _g = deadline::arm(Some(10_000));
            lm2.acquire(b, &res, Mode::X)
        });
        // Let the waiter actually block before the release: if release won
        // the race, the acquisition would be uncontended and waitCount 0.
        std::thread::sleep(Duration::from_millis(100));
        lm.release(a);
        assert_eq!(t.join().unwrap(), Ok(()));
        let rows = lm.server_status_locks();
        let row = rows.iter().find(|r| r.kind == "Collection").unwrap();
        assert_eq!(row.acquire_count, [("W", 2)]);
        assert_eq!(row.acquire_wait_count, [("W", 1)]);
        assert_eq!(row.time_acquiring_micros.len(), 1);
    }

    #[test]
    fn an_expired_deadline_ends_the_wait_with_code_50() {
        let lm = LockManager::new();
        let a = lm.statement_owner();
        let b = lm.statement_owner();
        let res = Resource::Collection("db.c".into());
        lm.acquire(a, &res, Mode::X).unwrap();
        let _g = deadline::arm(Some(30));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(lm.acquire(b, &res, Mode::X), Err(MaxTimeMsExpired));
        lm.release(a);
        let rows = lm.server_status_locks();
        let row = rows.iter().find(|r| r.kind == "Collection").unwrap();
        assert_eq!(row.acquire_wait_count, [("W", 1)]);
    }

    #[test]
    fn counts_group_under_the_resource_kind_not_the_name() {
        let lm = LockManager::new();
        let o = lm.statement_owner();
        lm.acquire(o, &Resource::Collection("db.a".into()), Mode::Is)
            .unwrap();
        lm.acquire(o, &Resource::Collection("db.b".into()), Mode::Is)
            .unwrap();
        lm.release(o);
        let rows = lm.server_status_locks();
        let row = rows.iter().find(|r| r.kind == "Collection").unwrap();
        assert_eq!(row.acquire_count, [("r", 2)]);
    }
}
