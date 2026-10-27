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
    /// mongod's name for the mode in a lock error (`IX`).
    pub fn name(self) -> &'static str {
        match self {
            Mode::Is => "IS",
            Mode::Ix => "IX",
            Mode::S => "S",
            Mode::X => "X",
        }
    }

    /// mongod's one-letter name, as `serverStatus.locks` shows it.
    pub fn letter(self) -> &'static str {
        match self {
            Mode::Is => "r",
            Mode::Ix => "w",
            Mode::S => "R",
            Mode::X => "W",
        }
    }

    /// mongod's conflict table (`lock_manager_defs.h`): a request of
    /// `self` cannot be granted while a holder has `held`. It is
    /// symmetric: IS goes with all but X, IX with IS and IX, S with IS and
    /// S, X with nothing. A read outside a transaction takes only the
    /// global IS (`statement_rounds`), so it never meets a collection's X.
    pub fn conflicts_with(self, held: Mode) -> bool {
        use Mode::*;
        matches!(
            (self, held),
            (Is, X) | (S, Ix) | (S, X) | (X, Is) | (X, Ix) | (X, S) | (X, X) | (Ix, S) | (Ix, X)
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
    /// The resource as mongod names it in a lock error: `{Collection :
    /// db.c}` (measured 8.2.11).
    pub fn described(&self) -> String {
        match self {
            Resource::Collection(ns) => format!("{{Collection : {ns}}}"),
            Resource::Database(db) => format!("{{Database : {db}}}"),
            // mongod names the global lock by its id: "Unable to acquire IX
            // lock on '{Global : 0}'" (measured 8.2.11, 2026-10-20).
            Resource::Global => "{Global : 0}".to_string(),
            other => format!("{{{} : {}}}", other.kind(), other.kind()),
        }
    }

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
    /// The requests waiting, first come first: `(ticket, owner, mode)`.
    queue: std::collections::VecDeque<(u64, Owner, Mode)>,
}

impl Head {
    fn blocks(&self, owner: Owner, mode: Mode) -> bool {
        self.holders
            .iter()
            .any(|(o, m)| *o != owner && mode.conflicts_with(*m))
    }

    fn holds(&self, owner: Owner) -> bool {
        self.holders.iter().any(|(o, _)| *o == owner)
    }

    /// Whether the waiter with `ticket` would be granted now, as mongod's
    /// lock head grants its conflict queue: in order, each request
    /// compatible with the holds and with those granted before it, up to
    /// the first that is not, which every later request waits behind.
    fn grantable(&self, ticket: u64) -> bool {
        let mut granted: Vec<(Owner, Mode)> = Vec::new();
        for (t, owner, mode) in &self.queue {
            let blocked = self.blocks(*owner, *mode)
                || granted
                    .iter()
                    .any(|(o, m)| o != owner && mode.conflicts_with(*m));
            if blocked {
                return false;
            }
            if *t == ticket {
                return true;
            }
            granted.push((*owner, *mode));
        }
        false
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
    next_ticket: AtomicU64,
    /// `fsync: {lock: true}`'s hold: how many locks are outstanding, and the
    /// owner holding the global lock in S while any are.
    fsync: Mutex<(u32, Option<Owner>)>,
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
            next_ticket: AtomicU64::new(1),
            fsync: Mutex::new((0, None)),
        }
    }

    /// `fsync: {lock: true}`: the first lock takes the global lock in S under
    /// an owner of its own, waiting out the writers that hold it, so later
    /// writes (global IX) wait while reads (global IS) go on; each further
    /// lock only counts. Answers the new count. mongod 8.2.11's mechanism,
    /// nested locks included.
    pub fn fsync_lock(&self) -> u32 {
        let mut held = self.fsync.lock().unwrap_or_else(|e| e.into_inner());
        if held.0 == 0 {
            let owner = self.statement_owner();
            // No deadline: the lock waits as long as the writers it follows.
            let _ = self.acquire(owner, &Resource::Global, Mode::S);
            held.1 = Some(owner);
        }
        held.0 += 1;
        held.0
    }

    /// `fsyncUnlock`: one lock fewer, and the global lock let go with the
    /// last. `None` when no lock is held.
    pub fn fsync_unlock(&self) -> Option<u32> {
        let mut held = self.fsync.lock().unwrap_or_else(|e| e.into_inner());
        if held.0 == 0 {
            return None;
        }
        held.0 -= 1;
        if held.0 == 0 {
            if let Some(owner) = held.1.take() {
                self.release(owner);
            }
        }
        Some(held.0)
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
        // First come first served (RM-138): a request that finds others
        // waiting queues behind them, so a `drop` waiting on live writers is
        // not overtaken by every writer that comes after it. An owner that
        // already holds the resource converts in place, as mongod's do, and
        // never queues behind a waiter that waits on it.
        {
            let head = inner.heads.entry(resource.clone()).or_default();
            if !head.blocks(owner, mode) && (head.queue.is_empty() || head.holds(owner)) {
                head.holders.push((owner, mode));
                drop(inner);
                self.count(resource, mode, None);
                note_taken(resource);
                return Ok(());
            }
        }
        let converting = inner
            .heads
            .get(resource)
            .is_some_and(|head| head.holds(owner));
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        if !converting {
            if let Some(head) = inner.heads.get_mut(resource) {
                head.queue.push_back((ticket, owner, mode));
            }
        }
        loop {
            let head = inner.heads.entry(resource.clone()).or_default();
            let ready = if converting {
                !head.blocks(owner, mode)
            } else {
                head.grantable(ticket)
            };
            if ready {
                head.queue.retain(|(t, _, _)| *t != ticket);
                head.holders.push((owner, mode));
                drop(inner);
                // Those queued behind may now be grantable beside it.
                self.changed.notify_all();
                self.count(resource, mode, Some(began.elapsed()));
                note_taken(resource);
                return Ok(());
            }
            // A kill ends the wait as a spent budget does: mongod 8.2.11
            // interrupts a `drop` or `createIndexes` waiting on a
            // transaction's lock with 11601 (RM-313).
            if secantus_core::deadline::expired() || secantus_core::deadline::interrupted() {
                head.queue.retain(|(t, _, _)| *t != ticket);
                drop(inner);
                self.changed.notify_all();
                self.count(resource, mode, Some(began.elapsed()));
                return Err(MaxTimeMsExpired);
            }
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
        inner
            .heads
            .retain(|_, h| !h.holders.is_empty() || !h.queue.is_empty());
        drop(inner);
        if any {
            self.changed.notify_all();
        }
    }

    /// The `serverStatus.locks` document: per resource kind, its per-mode
    /// counts. Modes with no acquisitions are omitted, and
    /// `acquireWaitCount` / `timeAcquiringMicros` appear only for modes
    /// that waited — mongod's rendering.
    pub fn server_status_locks(&self) -> Vec<ResourceReport> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // mongod's `ResourceType` order, which `serverStatus.locks` keeps.
        const ORDER: [&str; 8] = [
            "MultiDocumentTransactionsBarrier",
            "ReplicationStateTransition",
            "Global",
            "Database",
            "Collection",
            "Metadata",
            "Mutex",
            "oplog",
        ];
        let mut kinds: Vec<&'static str> = inner.stats.keys().copied().collect();
        kinds.sort_by_key(|k| ORDER.iter().position(|o| o == k).unwrap_or(ORDER.len()));
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
        note_operation_lock(resource.kind(), mode, waited.is_some());
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

/// What the rounds of a statement depend on, read from the store before
/// it runs.
pub trait Facts {
    /// The collection exists (a view counts).
    fn exists(&self, coll: &str) -> bool;
    /// The collection holds no document.
    fn is_empty(&self, coll: &str) -> bool;
    /// The key patterns of the collection's indexes.
    fn index_keys(&self, coll: &str) -> Vec<bson::Document>;
    /// The names of the database's collections and views.
    fn collections(&self) -> Vec<String>;
}

/// [`Facts`] off a server's storage; with none (unit tests), every
/// collection exists and is empty.
pub struct StorageFacts<'a> {
    pub storage: Option<&'a std::sync::Arc<dyn crate::storage::Storage>>,
    pub db: &'a str,
}

impl Facts for StorageFacts<'_> {
    fn exists(&self, coll: &str) -> bool {
        self.storage
            .is_none_or(|s| s.collection_exists(self.db, coll).unwrap_or(true))
    }

    fn is_empty(&self, coll: &str) -> bool {
        self.storage
            .is_none_or(|s| matches!(s.collection_count_at_most(self.db, coll, 0), Ok(Some(0))))
    }

    fn index_keys(&self, coll: &str) -> Vec<bson::Document> {
        self.storage
            .and_then(|s| s.list_indexes(self.db, coll).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|ix| ix.get_document("key").ok().cloned())
            .collect()
    }

    fn collections(&self) -> Vec<String> {
        self.storage
            .and_then(|s| s.list_collections(self.db).ok())
            .unwrap_or_default()
    }
}

/// The rounds a statement OUTSIDE a transaction takes, where mongod 8.2.11
/// was measured (2026-10-17, `serverStatus.locks` around one run of each);
/// `None` for a command not measured, which takes [`statement_chain`].
/// mongod lets a round go before it takes the next, and holds the last
/// while the statement runs:
///
/// - a read (`find`, `getMore`, `aggregate` without `$out` / `$merge`,
///   `count`, `distinct`, `listCollections`, `listIndexes`,
///   `listDatabases`, `collStats`) takes the global IS alone -- reads are
///   lock-free below it;
/// - `insert` a round of IX on the state transition, global, database and
///   collection locks, three when it creates the collection; `update` and
///   `delete` a round per statement; `findAndModify` a round without the
///   collection, then a full one;
/// - `create` a write round, and a round that reads the collection when it
///   exists already; `drop` / `dropIndexes` a write round and then one with
///   the collection in X, or the database round alone when there is no
///   collection; `dropDatabase` the database in X;
/// - `hello`, `ping`, `serverStatus` and `killCursors` take nothing.
pub fn statement_rounds(
    doc: &bson::Document,
    db: &str,
    facts: &dyn Facts,
) -> Option<Vec<Vec<(Resource, Mode)>>> {
    let exists = |c: &str| facts.exists(c);
    use Mode::*;
    let name = doc.keys().next().map(String::as_str)?;
    let coll = doc.get_str(name).ok().filter(|s| !s.is_empty());
    let write_round = |coll: Option<&str>| {
        let mut r = vec![
            (Resource::ReplicationStateTransition, Ix),
            (Resource::Global, Ix),
            (Resource::Database(db.to_string()), Ix),
        ];
        if let Some(c) = coll {
            r.push((Resource::Collection(format!("{db}.{c}")), Ix));
        }
        r
    };
    let statements = |key: &str| match doc.get(key) {
        Some(bson::Bson::Array(items)) => items.len().max(1),
        _ => 1,
    };
    let read = || Some(vec![vec![(Resource::Global, Is)]]);
    match name {
        // `fsync` and `fsyncUnlock` manage the global lock themselves.
        "hello" | "isMaster" | "ismaster" | "ping" | "serverStatus" | "killCursors" | "fsync"
        | "fsyncUnlock" => Some(Vec::new()),
        // The server's own state, no data: no lock at all (measured 8.2.11,
        // 2026-10-24, in valid forms).
        "buildInfo"
        | "buildinfo"
        | "connPoolStats"
        | "connectionStatus"
        | "dropConnections"
        | "features"
        | "getClusterParameter"
        | "getCmdLineOpts"
        | "getDefaultRWConcern"
        | "getLog"
        | "getParameter"
        | "hostInfo"
        | "listCommands"
        | "top"
        | "whatsmyuri" => Some(Vec::new()),
        // `listDatabases` takes one more global IS for each database whose
        // size it reads, as it reads it (`global_reads`).
        "find" | "getMore" | "count" | "distinct" | "listCollections" | "listIndexes"
        | "listDatabases" | "collStats" | "rolesInfo" | "usersInfo" => read(),
        // The older read path: the replication state transition lock first.
        "dataSize"
        | "planCacheClear"
        | "planCacheClearFilters"
        | "planCacheListFilters"
        | "planCacheSetFilter" => {
            let ns = if name == "dataSize" {
                coll.map(str::to_string)
            } else {
                coll.map(|c| format!("{db}.{c}"))
            };
            let mut round = vec![
                (Resource::ReplicationStateTransition, Ix),
                (Resource::Global, Is),
            ];
            if let Some(ns) = ns {
                let in_db = ns.split('.').next().unwrap_or(db).to_string();
                round.push((Resource::Database(in_db), Is));
                round.push((Resource::Collection(ns), Is));
            }
            Some(vec![round])
        }
        // `filemd5` reads `<root>.chunks` the same way.
        "filemd5" => {
            let root = doc.get_str("root").unwrap_or("fs");
            Some(vec![vec![
                (Resource::ReplicationStateTransition, Ix),
                (Resource::Global, Is),
                (Resource::Database(db.to_string()), Is),
                (Resource::Collection(format!("{db}.{root}.chunks")), Is),
            ]])
        }
        // What the explained command reads with, or one write round for a
        // write, which is planned and not run.
        "explain" => {
            let inner = doc.get_document("explain").ok()?;
            let verb = inner.keys().next().map(String::as_str)?;
            let target = inner.get_str(verb).ok().filter(|s| !s.is_empty());
            match verb {
                "update" | "delete" | "findAndModify" | "findandmodify" => {
                    Some(vec![write_round(target)])
                }
                _ => read(),
            }
        }
        // Twice the transition and global locks, then the database in S
        // with each of its collections in IS.
        "dbHash" => {
            let opening = vec![
                (Resource::ReplicationStateTransition, Ix),
                (Resource::Global, Is),
            ];
            let mut hashing = opening.clone();
            hashing.push((Resource::Database(db.to_string()), S));
            hashing.extend(
                facts
                    .collections()
                    .into_iter()
                    .map(|c| (Resource::Collection(format!("{db}.{c}")), Is)),
            );
            Some(vec![opening, hashing])
        }
        "aggregate" => {
            let writes = matches!(doc.get("pipeline"), Some(bson::Bson::Array(stages))
            if stages.iter().any(|s| s.as_document().is_some_and(|d| {
                d.contains_key("$out") || d.contains_key("$merge")
            })));
            if !writes {
                return read();
            }
            let merges = matches!(doc.get("pipeline"), Some(bson::Bson::Array(stages))
                if stages.last().and_then(bson::Bson::as_document)
                    .is_some_and(|d| d.contains_key("$merge")));
            if merges {
                // The global IS twice; the writes take their rounds as they
                // go (`write_rounds`).
                return Some(vec![
                    vec![(Resource::Global, Is)],
                    vec![(Resource::Global, Is)],
                ]);
            }
            out_rounds(doc, db, coll, facts)
        }
        "insert" => {
            let c = coll?;
            let n = if exists(c) { 1 } else { 3 };
            Some((0..n).map(|_| write_round(Some(c))).collect())
        }
        "update" | "delete" => {
            let c = coll?;
            let key = if name == "update" {
                "updates"
            } else {
                "deletes"
            };
            Some((0..statements(key)).map(|_| write_round(Some(c))).collect())
        }
        "findAndModify" | "findandmodify" => {
            let c = coll?;
            Some(vec![write_round(None), write_round(Some(c))])
        }
        "create" => {
            let c = coll?;
            let mut r = vec![write_round(Some(c))];
            if exists(c) {
                r.push(vec![
                    (Resource::ReplicationStateTransition, Ix),
                    (Resource::Global, Is),
                    (Resource::Database(db.to_string()), Is),
                    (Resource::Collection(format!("{db}.{c}")), Is),
                ]);
            }
            Some(r)
        }
        "drop" | "dropIndexes" | "deleteIndexes" => {
            let c = coll?;
            if !exists(c) {
                return Some(vec![write_round(None)]);
            }
            let mut exclusive = write_round(None);
            exclusive.push((Resource::Collection(format!("{db}.{c}")), X));
            Some(vec![write_round(Some(c)), exclusive])
        }
        "dropDatabase" => Some(vec![vec![
            (Resource::ReplicationStateTransition, Ix),
            (Resource::Global, Ix),
            (Resource::Database(db.to_string()), X),
        ]]),
        "createIndexes" => {
            let c = coll?;
            let global = || vec![(Resource::Global, Is)];
            if !exists(c) {
                // It creates the collection: two write rounds.
                return Some(vec![global(), write_round(Some(c)), write_round(Some(c))]);
            }
            let have = facts.index_keys(c);
            let asked: Vec<bson::Document> = match doc.get("indexes") {
                Some(bson::Bson::Array(specs)) => specs
                    .iter()
                    .filter_map(|s| s.as_document()?.get_document("key").ok().cloned())
                    .collect(),
                _ => Vec::new(),
            };
            if !asked.is_empty() && asked.iter().all(|k| have.contains(k)) {
                // Every index is there already: nothing is built.
                return Some(vec![global(), write_round(Some(c))]);
            }
            // A build: the collection in X while it is set up and committed,
            // IX while it runs; an empty collection builds in one step.
            let mut x = write_round(None);
            x.push((Resource::Collection(format!("{db}.{c}")), X));
            let mut rounds = vec![global(), global()];
            rounds.extend((0..2).map(|_| vec![(Resource::ReplicationStateTransition, Ix)]));
            if facts.is_empty(c) {
                rounds.push(x);
                rounds.push(write_round(Some(c)));
                return Some(rounds);
            }
            // Over documents the build takes its set-up and commit X rounds
            // itself (`exclusive_round`), and holds no collection lock while
            // it scans, so the collection takes writes (RM-187): the round
            // held through the build is the hierarchy above the collection,
            // its collection IX counted in a round of its own.
            rounds.push(x);
            rounds.extend((0..4).map(|_| write_round(Some(c))));
            rounds.push(vec![(Resource::Collection(format!("{db}.{c}")), Ix)]);
            rounds.push(write_round(None));
            Some(rounds)
        }
        "dbStats" => {
            let mut round = vec![
                (Resource::ReplicationStateTransition, Ix),
                (Resource::Global, Is),
                (Resource::Database(db.to_string()), Is),
            ];
            // IS on each collection and view.
            round.extend(
                facts
                    .collections()
                    .into_iter()
                    .map(|c| (Resource::Collection(format!("{db}.{c}")), Is)),
            );
            Some(vec![round])
        }
        _ => None,
    }
}

/// An `aggregate` that ends in `$out` (mongod 8.2.12 on Linux x86-64, the
/// exemplar build): a round that reads the source, six of the global IS
/// while the pipeline and its target are checked, the temporary collection
/// and the target in X twice -- three times when the target exists and is
/// replaced -- and two write rounds. The macOS arm64 build reads one more
/// global IS: a time-based plan yield that its Linux build does not make
/// (with `internalQueryExecYieldPeriodMS` off, macOS reads six too; RM-185).
/// `$merge` writes a round per document, which is not known before the
/// pipeline runs: it takes them as it writes (`write_rounds`).
fn out_rounds(
    doc: &bson::Document,
    db: &str,
    source: Option<&str>,
    facts: &dyn Facts,
) -> Option<Vec<Vec<(Resource, Mode)>>> {
    use Mode::*;
    let stages = doc.get_array("pipeline").ok()?;
    let last = stages.last()?.as_document()?;
    let (out_db, target) = match last.get("$out")? {
        bson::Bson::String(t) => (db.to_string(), t.clone()),
        bson::Bson::Document(d) => (
            d.get_str("db").unwrap_or(db).to_string(),
            d.get_str("coll").ok()?.to_string(),
        ),
        _ => return None,
    };
    let src = source?;
    let target_res = Resource::Collection(format!("{out_db}.{target}"));
    let write = |mode: Mode| {
        vec![
            (Resource::ReplicationStateTransition, Ix),
            (Resource::Global, Ix),
            (Resource::Database(out_db.clone()), Ix),
            (target_res.clone(), mode),
        ]
    };
    let mut rounds = vec![vec![
        (Resource::ReplicationStateTransition, Ix),
        (Resource::Global, Is),
        (Resource::Database(db.to_string()), Is),
        (Resource::Collection(format!("{db}.{src}")), Is),
    ]];
    rounds.extend((0..6).map(|_| vec![(Resource::Global, Is)]));
    let replaced = out_db == db && facts.exists(&target);
    rounds.extend((0..if replaced { 3 } else { 2 }).map(|_| write(X)));
    rounds.extend((0..2).map(|_| write(Ix)));
    Some(rounds)
}

thread_local! {
    /// Whether the operation this thread runs has been granted the global
    /// lock, in any mode: mongod's `Locker::wasGlobalLockTaken`.
    static TOOK_GLOBAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn note_taken(resource: &Resource) {
    if matches!(resource, Resource::Global) {
        TOOK_GLOBAL.with(|t| t.set(true));
    }
}

thread_local! {
    /// The locks the operation running on this thread has taken, by
    /// resource kind and mode, and how many of them waited: mongod's
    /// per-operation `lockStats`, which its `Slow query` line and profile
    /// entry print as `locks` (RM-023).
    static OP_LOCKS: std::cell::RefCell<Vec<(&'static str, Mode, u64, u64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn note_operation_lock(kind: &'static str, mode: Mode, waited: bool) {
    OP_LOCKS.with(|l| {
        let mut l = l.borrow_mut();
        match l.iter_mut().find(|(k, m, _, _)| *k == kind && *m == mode) {
            Some(row) => {
                row.2 += 1;
                row.3 += u64::from(waited);
            }
            None => l.push((kind, mode, 1, u64::from(waited))),
        }
    });
}

/// mongod's `ResourceType` order, which `serverStatus.locks` and an
/// operation's `locks` keep.
const KIND_ORDER: [&str; 8] = [
    "MultiDocumentTransactionsBarrier",
    "ReplicationStateTransition",
    "Global",
    "Database",
    "Collection",
    "Metadata",
    "Mutex",
    "oplog",
];

/// The locks the operation running on this thread has taken so far, as
/// mongod prints an operation's `locks`: per resource kind, in its order,
/// `acquireCount` by mode (`r`, `w`, `R`, `W`), and `acquireWaitCount` for
/// the modes that waited. `extra_global_is`: the global IS mongod takes
/// after the line, to read the operation's storage statistics, which its
/// profile entry counts.
pub fn operation_locks(extra_global_is: bool) -> bson::Document {
    let mut rows = OP_LOCKS.with(|l| l.borrow().clone());
    if extra_global_is {
        match rows
            .iter_mut()
            .find(|(k, m, _, _)| *k == "Global" && *m == Mode::Is)
        {
            Some(row) => row.2 += 1,
            None => rows.push(("Global", Mode::Is, 1, 0)),
        }
    }
    let mut kinds: Vec<&'static str> = rows.iter().map(|r| r.0).collect();
    kinds.sort_by_key(|k| {
        KIND_ORDER
            .iter()
            .position(|o| o == k)
            .unwrap_or(KIND_ORDER.len())
    });
    kinds.dedup();
    let mut out = bson::Document::new();
    for kind in kinds {
        let mut modes: Vec<&(&'static str, Mode, u64, u64)> =
            rows.iter().filter(|r| r.0 == kind).collect();
        modes.sort_by_key(|r| r.1);
        let mut acquire = bson::Document::new();
        let mut waits = bson::Document::new();
        for (_, mode, n, waited) in modes {
            acquire.insert(mode.letter(), *n as i64);
            if *waited > 0 {
                waits.insert(mode.letter(), *waited as i64);
            }
        }
        let mut stats = bson::doc! { "acquireCount": acquire };
        if !waits.is_empty() {
            stats.insert("acquireWaitCount", waits);
        }
        out.insert(kind, stats);
    }
    out
}

/// An operation starts on this thread: it has taken no global lock yet.
pub fn begin_operation() {
    TOOK_GLOBAL.with(|t| t.set(false));
    OP_LOCKS.with(|l| l.borrow_mut().clear());
}

/// Whether the operation this thread runs has taken the global lock.
pub fn took_global() -> bool {
    TOOK_GLOBAL.with(std::cell::Cell::get)
}

/// The global lock in IS that mongod takes to read an operation's storage
/// statistics for its `Slow query` line (`CurOp::completeAndLogOperation`),
/// for an operation that took the global lock: taken and let go, one more
/// global IS in `serverStatus.locks` per line (measured 8.2.11, 2026-10-24;
/// RM-299).
pub fn log_lock(lm: &LockManager) {
    let owner = lm.statement_owner();
    // Refused only by a spent deadline behind a conflicting holder; mongod
    // likewise gives up on this lock (after 500 ms) and writes the line
    // without the statistics, so there is nothing to report.
    if lm.acquire(owner, &Resource::Global, Mode::Is).is_ok() {
        lm.release(owner);
    }
}

thread_local! {
    /// The lock manager of the statement this thread is running, for the
    /// writes a statement makes partway through (`$merge`'s, a round per
    /// document), which the rounds taken before it runs cannot know.
    static CURRENT: std::cell::RefCell<Option<std::sync::Arc<LockManager>>> =
        const { std::cell::RefCell::new(None) };
}

/// Makes `lm` the running statement's lock manager until dropped.
pub struct CurrentScope(Option<std::sync::Arc<LockManager>>);

/// Enter a statement: [`write_rounds`] takes its locks from `lm`.
pub fn enter(lm: Option<&std::sync::Arc<LockManager>>) -> CurrentScope {
    CurrentScope(CURRENT.with(|c| c.replace(lm.cloned())))
}

impl Drop for CurrentScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// `n` global IS from the running statement's lock manager, each taken and
/// let go: what `listDatabases` takes reading each database's size.
pub fn global_reads(n: usize) {
    let Some(lm) = CURRENT.with(|c| c.borrow().clone()) else {
        return;
    };
    for _ in 0..n {
        let owner = lm.statement_owner();
        if lm.acquire(owner, &Resource::Global, Mode::Is).is_err() {
            return;
        }
        lm.release(owner);
    }
}

/// The running statement's lock manager, if one is wired.
pub fn current() -> Option<std::sync::Arc<LockManager>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// `n` write rounds on `db.coll` from the running statement's lock manager,
/// as mongod takes one per document a `$merge` writes: all but the last
/// taken and let go, the last held until the returned guard drops. `None`
/// when `n` is 0 or no statement is running.
pub fn write_rounds(db: &str, coll: &str, n: usize) -> Option<Guard> {
    let lm = CURRENT.with(|c| c.borrow().clone())?;
    if n == 0 {
        return None;
    }
    let round = vec![
        (Resource::ReplicationStateTransition, Mode::Ix),
        (Resource::Global, Mode::Ix),
        (Resource::Database(db.to_string()), Mode::Ix),
        (Resource::Collection(format!("{db}.{coll}")), Mode::Ix),
    ];
    for _ in 1..n {
        let owner = lm.statement_owner();
        if acquire_chain(&lm, owner, &round).is_err() {
            return None;
        }
        lm.release(owner);
    }
    let owner = lm.statement_owner();
    acquire_chain(&lm, owner, &round).ok()?;
    Some(Guard {
        lm,
        owner,
        // Not the statement's own chain: whose the statement is stays as it was.
        previous_owner: STATEMENT_OWNER.with(std::cell::Cell::get),
    })
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

thread_local! {
    /// The owner of the running statement's locks (`Guard::statement`).
    static STATEMENT_OWNER: std::cell::Cell<Option<Owner>> = const { std::cell::Cell::new(None) };
}

/// Holds taken by the running statement on top of its own chain, let go
/// (and only they) on drop.
pub struct ExtraHold {
    lm: std::sync::Arc<LockManager>,
    owner: Owner,
    chain: Vec<(Resource, Mode)>,
}

impl Drop for ExtraHold {
    fn drop(&mut self) {
        self.lm.release_holds(self.owner, &self.chain);
    }
}

/// mongod's exclusive round on `db.coll`, taken by the running statement
/// on top of its own locks and let go when the hold drops: what an index
/// build takes to set up and to commit (RM-187), waiting out the writers
/// and the open transactions that hold the collection. `None` when no
/// statement lock manager is in place; an error when the deadline spends
/// while it waits.
pub fn exclusive_round(
    db: &str,
    coll: &str,
) -> Option<Result<ExtraHold, secantus_core::deadline::MaxTimeMsExpired>> {
    let lm = CURRENT.with(|c| c.borrow().clone())?;
    let owner = STATEMENT_OWNER.with(std::cell::Cell::get)?;
    let chain = vec![
        (Resource::ReplicationStateTransition, Mode::Ix),
        (Resource::Global, Mode::Ix),
        (Resource::Database(db.to_string()), Mode::Ix),
        (Resource::Collection(format!("{db}.{coll}")), Mode::X),
    ];
    for (i, (res, mode)) in chain.iter().enumerate() {
        if lm.acquire(owner, res, *mode).is_err() {
            lm.release_holds(owner, &chain[..i]);
            return Some(Err(secantus_core::deadline::MaxTimeMsExpired));
        }
    }
    Some(Ok(ExtraHold { lm, owner, chain }))
}

impl LockManager {
    /// Let go of one hold of each `(resource, mode)` in `chain` by `owner`
    /// -- the latest taken -- leaving its other holds as they are.
    pub fn release_holds(&self, owner: Owner, chain: &[(Resource, Mode)]) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for (res, mode) in chain {
            if let Some(head) = inner.heads.get_mut(res) {
                if let Some(i) = head
                    .holders
                    .iter()
                    .rposition(|(o, m)| *o == owner && m == mode)
                {
                    head.holders.remove(i);
                }
            }
        }
        inner
            .heads
            .retain(|_, h| !h.holders.is_empty() || !h.queue.is_empty());
        drop(inner);
        self.changed.notify_all();
    }
}

/// A statement's holds, released on drop (the statement's end — every path,
/// error replies included).
pub struct Guard {
    lm: std::sync::Arc<LockManager>,
    owner: Owner,
    previous_owner: Option<Owner>,
}

impl Guard {
    /// Take the statement's chain for one request; `None` when the server
    /// has no lock manager wired (unit tests, off-server contexts) — locking
    /// is best-effort there, as the other `Option<Arc<_>>` services are.
    pub fn statement(
        lm: Option<&std::sync::Arc<LockManager>>,
        doc: &bson::Document,
        db: &str,
        facts: &dyn Facts,
    ) -> Option<Result<Self, crate::CommandError>> {
        let lm = lm?;
        let mut rounds =
            statement_rounds(doc, db, facts).unwrap_or_else(|| vec![statement_chain(doc, db)]);
        let chain = rounds.pop().unwrap_or_default();
        // The rounds before the last are taken and let go, as mongod does.
        for round in &rounds {
            let owner = lm.statement_owner();
            if acquire_chain(lm, owner, round).is_err() {
                return Some(Err(crate::CommandError::max_time_expired()));
            }
            lm.release(owner);
        }
        let owner = lm.statement_owner();
        match acquire_chain(lm, owner, &chain) {
            Ok(()) => Some(Ok(Self {
                lm: lm.clone(),
                owner,
                previous_owner: STATEMENT_OWNER.with(|o| o.replace(Some(owner))),
            })),
            Err(_) => Some(Err(crate::CommandError::max_time_expired())),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        STATEMENT_OWNER.with(|o| o.set(self.previous_owner));
        self.lm.release(self.owner);
    }
}

#[cfg(test)]
mod tests {
    /// `fsync: {lock: true}` holds the global lock in S: a read's IS is
    /// granted, a write's IX waits until the last of the nested locks is
    /// released, and an unlock with none held is refused.
    #[test]
    fn an_fsync_lock_holds_writes_until_the_last_unlock() {
        let lm = std::sync::Arc::new(LockManager::new());
        assert_eq!(lm.fsync_unlock(), None);
        assert_eq!(lm.fsync_lock(), 1);
        assert_eq!(lm.fsync_lock(), 2);
        let reader = lm.statement_owner();
        assert!(lm.acquire(reader, &Resource::Global, Mode::Is).is_ok());
        lm.release(reader);
        let (tx, rx) = std::sync::mpsc::channel();
        let writer_lm = lm.clone();
        let writer = std::thread::spawn(move || {
            let owner = writer_lm.statement_owner();
            let _ = writer_lm.acquire(owner, &Resource::Global, Mode::Ix);
            tx.send(()).unwrap();
            writer_lm.release(owner);
        });
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(lm.fsync_unlock(), Some(1));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(lm.fsync_unlock(), Some(0));
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        writer.join().unwrap();
    }

    #[test]
    fn a_waiting_exclusive_request_is_not_overtaken() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let lm = Arc::new(LockManager::new());
        let coll = Resource::Collection("db.c".into());
        let writer = lm.statement_owner();
        lm.acquire(writer, &coll, Mode::Ix).unwrap();
        // A `drop` waits on the writer.
        let dropper = lm.statement_owner();
        let dropped = Arc::new(AtomicBool::new(false));
        let d = {
            let (lm, coll, dropped) = (lm.clone(), coll.clone(), dropped.clone());
            std::thread::spawn(move || {
                lm.acquire(dropper, &coll, Mode::X).unwrap();
                dropped.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                lm.release(dropper);
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        // A later writer queues behind the drop instead of overtaking it.
        let late = lm.statement_owner();
        let late_in = Arc::new(AtomicBool::new(false));
        let w = {
            let (lm, coll, late_in, dropped) =
                (lm.clone(), coll.clone(), late_in.clone(), dropped.clone());
            std::thread::spawn(move || {
                lm.acquire(late, &coll, Mode::Ix).unwrap();
                assert!(
                    dropped.load(Ordering::SeqCst),
                    "the late writer overtook the drop"
                );
                late_in.store(true, Ordering::SeqCst);
                lm.release(late);
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(!late_in.load(Ordering::SeqCst));
        // The writer already holding the collection is not queued behind it.
        lm.acquire(writer, &coll, Mode::Ix).unwrap();
        lm.release(writer);
        d.join().unwrap();
        w.join().unwrap();
        assert!(late_in.load(Ordering::SeqCst));
    }

    use super::*;
    use secantus_core::deadline;

    #[test]
    fn the_conflict_table_is_mongod_s() {
        use Mode::*;
        // IS is compatible with all but X.
        for m in [Is, Ix, S] {
            assert!(!Is.conflicts_with(m), "IS vs {m:?}");
        }
        assert!(Is.conflicts_with(X));
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
