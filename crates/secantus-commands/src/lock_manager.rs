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
    /// The owners waiting for a lock now, and the mode each waits for:
    /// mongod's `Locker::hasLockPending`.
    pending: HashMap<Owner, Mode>,
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
    /// mongod's execution admission control, once the server wires it
    /// (`admission::execution`): an owner takes a ticket from its read or
    /// write pool as it takes the global lock (RM-355).
    execution: std::sync::OnceLock<std::sync::Arc<ExecutionControl>>,
    /// The execution ticket each owner holds, and whether from the read
    /// pool: mongod's `Locker::_ticket`, one per owner.
    tickets: Mutex<HashMap<Owner, (crate::admission::Ticket, bool)>>,
}

use crate::admission::execution::ExecutionControl;

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
            execution: std::sync::OnceLock::new(),
            tickets: Mutex::new(HashMap::new()),
        }
    }

    /// Wire in the server's execution control (once).
    pub fn set_execution(&self, control: std::sync::Arc<ExecutionControl>) -> bool {
        self.execution.set(control).is_ok()
    }

    /// The server's execution control, when wired.
    pub fn execution(&self) -> Option<&std::sync::Arc<ExecutionControl>> {
        self.execution.get()
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
    ///
    /// The global lock is taken with an execution ticket, before it, as
    /// mongod's `Locker::lockGlobal` takes one (RM-355): an owner without
    /// one takes one from the read pool for IS or S and from the write pool
    /// for IX, waiting for it when the pool has none free, and holds it
    /// until it lets the global lock go. X takes none. An error that ends
    /// the ticket's wait other than the deadline is kept for
    /// [`take_ticket_error`].
    pub fn acquire(
        &self,
        owner: Owner,
        resource: &Resource,
        mode: Mode,
    ) -> Result<(), MaxTimeMsExpired> {
        let ticketed = if matches!(resource, Resource::Global) {
            self.take_ticket(owner, mode)?
        } else {
            false
        };
        let taken = self.acquire_lock(owner, resource, mode);
        if taken.is_err() && ticketed {
            self.drop_ticket(owner);
        }
        taken
    }

    fn acquire_lock(
        &self,
        owner: Owner,
        resource: &Resource,
        mode: Mode,
    ) -> Result<(), MaxTimeMsExpired> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let began = Instant::now();
        let ticket_first = takes_flow_control_ticket(&inner, owner, resource, mode);
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
                if ticket_first {
                    note_flow_control_ticket();
                }
                self.count(resource, mode, None);
                note_taken(resource);
                // mongod's `Locker::_lockBegin`: a lock granted at once is
                // not granted to an operation that was killed or ran out of
                // time, and the check counts (RM-360) -- a re-take by the
                // owner already holding it (`recursiveCount` > 1) too, as
                // its acquisition counts in the line's `locks` (r8.2.11
                // `locker.cpp`: only a lock its unit of work still holds to
                // its end is re-used without either).
                if let Err(e) = secantus_core::deadline::check_for_interrupt() {
                    self.release_one(owner, resource);
                    return Err(e);
                }
                return Ok(());
            }
        }
        let converting = inner
            .heads
            .get(resource)
            .is_some_and(|head| head.holds(owner));
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        // mongod checks for interrupt each time a waiting request wakes
        // (`CondVarLockGrantNotification::wait`), which its timing decides;
        // this wait polls on its own clock, so the count is not mongod's.
        secantus_core::deadline::unmatched();
        if !converting {
            if let Some(head) = inner.heads.get_mut(resource) {
                head.queue.push_back((ticket, owner, mode));
            }
        }
        inner.pending.insert(owner, mode);
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
                inner.pending.remove(&owner);
                drop(inner);
                if ticket_first {
                    note_flow_control_ticket();
                }
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
                inner.pending.remove(&owner);
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

    /// Release `owner`'s latest hold on `resource`, waking waiters.
    fn release_one(&self, owner: Owner, resource: &Resource) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(head) = inner.heads.get_mut(resource) {
            if let Some(at) = head.holders.iter().rposition(|(o, _)| *o == owner) {
                head.holders.remove(at);
            }
        }
        let global_gone = matches!(resource, Resource::Global) && !holds_global(&inner, owner);
        drop(inner);
        if global_gone {
            self.drop_ticket(owner);
        }
        self.changed.notify_all();
    }

    /// Release every hold `owner` has, waking waiters, and its execution
    /// ticket with its global lock.
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
        self.drop_ticket(owner);
        if any {
            self.changed.notify_all();
        }
    }

    /// Take `owner`'s execution ticket for the global lock in `mode` ahead
    /// of the lock itself, when `resource` is the global lock and it needs
    /// one: a caller that bounds the lock's wait does not bound the
    /// ticket's.
    pub fn take_global_ticket(
        &self,
        owner: Owner,
        resource: &Resource,
        mode: Mode,
    ) -> Result<(), MaxTimeMsExpired> {
        if matches!(resource, Resource::Global) {
            self.take_ticket(owner, mode)?;
        }
        Ok(())
    }

    /// `globalLock.currentQueue`'s readers and writers: the operations
    /// waiting for an execution ticket, and those holding one with a lock
    /// request pending (mongod's `Locker::getClientState`), each a reader
    /// or a writer by the pool its ticket is from, or by the mode it waits
    /// for when it holds none.
    pub fn current_queue(&self) -> (i64, i64) {
        let (mut readers, mut writers) = match self.execution.get() {
            Some(control) => (
                control.holder(true).queued(),
                control.holder(false).queued(),
            ),
            None => (0, 0),
        };
        let pending: Vec<(Owner, Mode)> = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.pending.iter().map(|(o, m)| (*o, *m)).collect()
        };
        let tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        for (owner, mode) in pending {
            let reader = tickets
                .get(&owner)
                .map_or(matches!(mode, Mode::Is | Mode::S), |(_, r)| *r);
            if reader {
                readers += 1;
            } else {
                writers += 1;
            }
        }
        (readers, writers)
    }

    /// Return `owner`'s execution ticket while it keeps its locks (mongod's
    /// `Locker::releaseTicket`): what a transaction does between its
    /// statements, taking one again with its next statement's global lock.
    pub fn release_ticket(&self, owner: Owner) {
        self.drop_ticket(owner);
    }

    fn drop_ticket(&self, owner: Owner) {
        let ticket = self
            .tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&owner);
        // Returned outside the map's lock: a release may wake a waiter.
        drop(ticket);
    }

    fn has_ticket(&self, owner: Owner) -> bool {
        self.tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&owner)
    }

    /// Whether this thread's operation holds an execution ticket under
    /// another owner, which a global lock it takes now rides on: mongod
    /// keeps one locker per operation, and its global locks after the
    /// first take no ticket of their own.
    fn rides(&self) -> bool {
        let me = self as *const LockManager as usize;
        TICKETED.with(|t| {
            let mut t = t.borrow_mut();
            let tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            t.retain(|(lm, o)| *lm != me || tickets.contains_key(o));
            t.iter().any(|(lm, _)| *lm == me)
        })
    }

    /// Take `owner`'s execution ticket for a global lock in `mode`, when it
    /// needs one (see [`acquire`](Self::acquire)): `true` when it took one.
    fn take_ticket(&self, owner: Owner, mode: Mode) -> Result<bool, MaxTimeMsExpired> {
        let Some(control) = self.execution.get() else {
            return Ok(false);
        };
        if mode == Mode::X || self.has_ticket(owner) || self.rides() {
            return Ok(false);
        }
        let reader = matches!(mode, Mode::Is | Mode::S);
        let holder = control.holder(reader);
        let ctx = EXEC_ADMISSION
            .with(|c| c.borrow().clone())
            .unwrap_or_default();
        let mut interrupted = || {
            // mongod's waiter checks its operation each time it wakes.
            secantus_core::deadline::count_check();
            if secantus_core::deadline::interrupted() {
                return Err(crate::CommandError::new(
                    11601,
                    "Interrupted",
                    "operation was interrupted",
                ));
            }
            if secantus_core::deadline::expired() {
                return Err(crate::CommandError::max_time_expired());
            }
            Ok(())
        };
        // An owner that already holds locks takes its ticket again (a
        // transaction's next statement): mongod's `reacquireTicket`, which
        // gives up rather than wait on a ticket that a request queued on
        // one of its locks may hold.
        let reacquiring = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.heads.values().any(|h| h.holds(owner))
        };
        // The whole acquisition is the operation's wait for execution
        // admission (`storageExecutionMicros`), a ticket that was free at
        // once included.
        let started = std::time::Instant::now();
        let taken = if reacquiring {
            self.reacquire(owner, mode, holder, &ctx, &mut interrupted)
        } else {
            holder.wait_for_ticket(&ctx, &mut interrupted)
        };
        ctx.record_acquiring(started.elapsed());
        match taken {
            Ok(ticket) => {
                self.tickets
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(owner, (ticket, reader));
                let me = self as *const LockManager as usize;
                TICKETED.with(|t| t.borrow_mut().push((me, owner)));
                Ok(true)
            }
            Err(e) => {
                if e.code != MaxTimeMsExpired::CODE {
                    TICKET_ERROR.with(|t| *t.borrow_mut() = Some(e));
                }
                Err(MaxTimeMsExpired)
            }
        }
    }

    /// mongod's `Locker::reacquireTicket`: a ticket at once if one is free;
    /// else, while none of `owner`'s locks has a request queued on it,
    /// waiting for one 100 ms at a time; and when one has, 24 LockTimeout.
    fn reacquire(
        &self,
        owner: Owner,
        mode: Mode,
        holder: &std::sync::Arc<crate::admission::TicketHolder>,
        ctx: &std::sync::Arc<crate::admission::AdmissionContext>,
        interrupted: &mut dyn FnMut() -> Result<(), crate::CommandError>,
    ) -> Result<crate::admission::Ticket, crate::CommandError> {
        if let Some(ticket) = holder.try_acquire(ctx) {
            return Ok(ticket);
        }
        loop {
            let contended = {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner
                    .heads
                    .iter()
                    .find(|(_, h)| h.holds(owner) && !h.queue.is_empty())
                    .map(|(r, _)| r.described())
            };
            interrupted()?;
            if let Some(resource) = contended {
                // mongod prints the lock mode's number (`LockMode`).
                let number = match mode {
                    Mode::Is => 1,
                    Mode::Ix => 2,
                    Mode::S => 3,
                    Mode::X => 4,
                };
                return Err(crate::CommandError::new(
                    24,
                    "LockTimeout",
                    format!(
                        "Unable to acquire ticket with mode '{number}' due to detected lock \
                         conflict for resource {resource}"
                    ),
                ));
            }
            let until = Some(std::time::Instant::now() + Duration::from_millis(100));
            if let Some(ticket) = holder.wait_for_ticket_until(ctx, until, interrupted)? {
                return Ok(ticket);
            }
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
    /// The names of the server's databases.
    fn databases(&self) -> Vec<String>;
    /// The names of `db`'s collections as mongod's catalog holds them: its
    /// views left out, and `system.views` in when it has one.
    fn collections_of(&self, db: &str) -> Vec<String>;
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

    fn databases(&self) -> Vec<String> {
        self.storage
            .and_then(|s| s.list_databases().ok())
            .unwrap_or_default()
    }

    fn collections_of(&self, db: &str) -> Vec<String> {
        self.storage
            .and_then(|s| crate::views::catalog_collections(s.as_ref(), db).ok())
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
///   collection, and a full one inside it;
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
                // A collectionless aggregate (`aggregate: 1`: `$documents`,
                // `$currentOp`, `$listLocalSessions`) takes no lock of its
                // own (measured 8.2.11, 2026-11-09).
                if collectionless_without_reads(doc) {
                    return Some(Vec::new());
                }
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
        // The full round is taken inside the first, which it holds: the
        // write takes one flow control ticket where `drop`'s two rounds
        // take two (measured 8.2.11).
        "findAndModify" | "findandmodify" => {
            let c = coll?;
            let mut nested = write_round(None);
            nested.extend(write_round(Some(c)));
            Some(vec![nested])
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
        // A round on each database it reads, and inside it one on each of
        // the database's collections -- or the one named, whether or not it
        // exists -- as it validates it (measured 8.2.11: a view takes none
        // and `system.views` one; a database that does not exist takes its
        // own round alone). The request has parsed (`parse_before_locks`).
        "validateDBMetadata" => {
            let named = |key: &str| doc.get_str(key).ok().map(str::to_string);
            let dbs = match named("db") {
                Some(db) => vec![db],
                None => facts.databases(),
            };
            let existing = facts.databases();
            let read = |db: &str, coll: Option<&str>| {
                let mut r = vec![
                    (Resource::ReplicationStateTransition, Ix),
                    (Resource::Global, Is),
                    (Resource::Database(db.to_string()), Is),
                ];
                if let Some(c) = coll {
                    r.push((Resource::Collection(format!("{db}.{c}")), Is));
                }
                r
            };
            let mut rounds = Vec::new();
            for db in &dbs {
                rounds.push(read(db, None));
                if !existing.contains(db) {
                    continue;
                }
                let colls = match named("collection") {
                    Some(c) => vec![c],
                    None => facts.collections_of(db),
                };
                rounds.extend(colls.iter().map(|c| read(db, Some(c))));
            }
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

/// Stages that read a collection or the oplog, at any depth of a pipeline.
const READING_STAGES: [&str; 6] = [
    "$lookup",
    "$unionWith",
    "$graphLookup",
    "$out",
    "$merge",
    "$changeStream",
];

/// Whether `doc` is an `aggregate` on no collection whose pipeline reads
/// none either: mongod's `runAggregate` takes no collection or global lock
/// for a collectionless namespace (`<db>.$cmd.aggregate`), only what its
/// stages read.
fn collectionless_without_reads(doc: &bson::Document) -> bool {
    fn reads(value: &bson::Bson) -> bool {
        match value {
            bson::Bson::Document(d) => d
                .iter()
                .any(|(k, v)| READING_STAGES.contains(&k.as_str()) || reads(v)),
            bson::Bson::Array(items) => items.iter().any(reads),
            _ => false,
        }
    }
    !matches!(doc.get("aggregate"), Some(bson::Bson::String(_)))
        && doc.get("pipeline").is_some_and(|p| !reads(p))
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

/// Whether `owner` holds the global lock in any mode.
fn holds_global(inner: &Inner, owner: Owner) -> bool {
    inner
        .heads
        .get(&Resource::Global)
        .is_some_and(|h| h.holds(owner))
}

thread_local! {
    /// The running operation's execution admissions (mongod's
    /// `ExecutionAdmissionContext`), which its tickets are counted in.
    static EXEC_ADMISSION: std::cell::RefCell<Option<std::sync::Arc<crate::admission::AdmissionContext>>> =
        const { std::cell::RefCell::new(None) };
    /// The owners whose execution tickets this thread's operation took,
    /// by lock manager (see `LockManager::rides`).
    static TICKETED: std::cell::RefCell<Vec<(usize, Owner)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Why the last ticket wait failed, when it was not the deadline.
    static TICKET_ERROR: std::cell::RefCell<Option<crate::CommandError>> =
        const { std::cell::RefCell::new(None) };
}

/// The error that ended this thread's last ticket wait, when it was not
/// the deadline (a kill's 11601, a full queue's 433, a transaction's
/// LockTimeout): what a failed [`LockManager::acquire`] answers instead of
/// 50.
pub fn take_ticket_error() -> Option<crate::CommandError> {
    TICKET_ERROR.with(|t| t.borrow_mut().take())
}

/// Returns a transaction's execution ticket when its statement ends, the
/// transaction keeping its locks.
pub struct TicketStash {
    lm: std::sync::Arc<LockManager>,
    owner: Owner,
}

impl TicketStash {
    pub fn new(lm: std::sync::Arc<LockManager>, owner: Owner) -> Self {
        TicketStash { lm, owner }
    }
}

impl Drop for TicketStash {
    fn drop(&mut self) {
        self.lm.release_ticket(self.owner);
    }
}

/// The running operation's execution admissions, when a command runs on
/// this thread.
pub fn execution_admission() -> Option<std::sync::Arc<crate::admission::AdmissionContext>> {
    EXEC_ADMISSION.with(|c| c.borrow().clone())
}

/// Makes `ctx` the running operation's execution admissions until dropped.
pub struct AdmissionScope(Option<std::sync::Arc<crate::admission::AdmissionContext>>);

/// Count the tickets this thread's operation takes in `ctx`.
pub fn admission_scope(ctx: std::sync::Arc<crate::admission::AdmissionContext>) -> AdmissionScope {
    AdmissionScope(EXEC_ADMISSION.with(|c| c.replace(Some(ctx))))
}

impl Drop for AdmissionScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        EXEC_ADMISSION.with(|c| *c.borrow_mut() = previous);
    }
}

/// Run `wait` with the running statement's execution ticket returned, and
/// take one again after: what mongod's `getMore` does while it waits for
/// data to arrive, its locks and ticket let go. A ticket that cannot be had again
/// (a kill, a spent budget) leaves the statement without one, which its own
/// interrupt checks then end.
pub fn without_ticket<R>(wait: impl FnOnce() -> R) -> R {
    let held = CURRENT
        .with(|c| c.borrow().clone())
        .zip(STATEMENT_OWNER.with(std::cell::Cell::get));
    let Some((lm, owner)) = held else {
        return wait();
    };
    let mode = match lm
        .tickets
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&owner)
    {
        Some((_, true)) => Mode::Is,
        Some((_, false)) => Mode::Ix,
        None => return wait(),
    };
    lm.drop_ticket(owner);
    let out = wait();
    if lm.take_ticket(owner, mode).is_err() {
        take_ticket_error();
    }
    out
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
    /// The flow control tickets the operation running on this thread has
    /// taken: mongod's per-operation `flowControl.acquireCount` (RM-357).
    static FLOW_CONTROL: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// Whether `owner`'s request for `mode` on `resource` takes a flow control
/// ticket, as mongod's `Locker::getFlowControlTicket` decides: the global
/// lock in IX, by an owner not holding it already (a nested acquisition
/// takes none), outside a multi-document transaction (which does not
/// take part in flow control: a transaction's statement counts `{}`,
/// measured 8.2.11). This server is always a replica set's primary, and
/// mongod's flow control is on there by default. Its lag is a single
/// member's, never past the threshold, so a ticket is always free: each
/// one is counted and none is waited for.
fn takes_flow_control_ticket(inner: &Inner, owner: Owner, resource: &Resource, mode: Mode) -> bool {
    matches!(resource, Resource::Global)
        && mode == Mode::Ix
        && matches!(owner, Owner::Statement(_))
        && !inner.heads.get(resource).is_some_and(|h| h.holds(owner))
}

fn note_flow_control_ticket() {
    FLOW_CONTROL.with(|n| n.set(n.get() + 1));
}

/// The flow control tickets the operation running on this thread has taken
/// so far: what mongod prints as `flowControl: {acquireCount: n}` on a
/// write's line and entry (measured 8.2.11: an insert's 1, one more for
/// each write round it takes apart -- 3 for an insert that creates its
/// collection, one per statement of an `update`).
pub fn flow_control_tickets() -> i64 {
    FLOW_CONTROL.with(std::cell::Cell::get)
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
    secantus_core::deadline::begin_operation();
    crate::explain::begin_operation();
    TOOK_GLOBAL.with(|t| t.set(false));
    TICKETED.with(|t| t.borrow_mut().clear());
    TICKET_ERROR.with(|t| *t.borrow_mut() = None);
    OP_LOCKS.with(|l| l.borrow_mut().clear());
    FLOW_CONTROL.with(|n| n.set(0));
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
        chain: round,
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
        let global_gone = chain.iter().any(|(r, _)| matches!(r, Resource::Global))
            && !holds_global(&inner, owner);
        drop(inner);
        if global_gone {
            self.drop_ticket(owner);
        }
        self.changed.notify_all();
    }
}

/// A statement's holds, released on drop (the statement's end — every path,
/// error replies included).
pub struct Guard {
    lm: std::sync::Arc<LockManager>,
    owner: Owner,
    previous_owner: Option<Owner>,
    chain: Vec<(Resource, Mode)>,
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
        let refused = || take_ticket_error().unwrap_or_else(crate::CommandError::max_time_expired);
        for round in &rounds {
            let owner = lm.statement_owner();
            if acquire_chain(lm, owner, round).is_err() {
                return Some(Err(refused()));
            }
            lm.release(owner);
        }
        let owner = lm.statement_owner();
        match acquire_chain(lm, owner, &chain) {
            Ok(()) => Some(Ok(Self {
                lm: lm.clone(),
                owner,
                previous_owner: STATEMENT_OWNER.with(|o| o.replace(Some(owner))),
                chain,
            })),
            Err(_) => Some(Err(refused())),
        }
    }
}

impl Guard {
    /// Let the statement yield (`deadline::check`): at each yield mongod
    /// releases the statement's locks and takes them again, each grant
    /// counted and checked for interrupt (RM-360). The re-take is counted
    /// and checked here; the locks stay held across it, so a request
    /// waiting on them is not let in by the yield as mongod's is (RM-360).
    pub fn yields(&self) -> Option<secantus_core::deadline::YieldGuard> {
        if self.chain.is_empty() {
            return None;
        }
        let lm = self.lm.clone();
        let chain = self.chain.clone();
        Some(secantus_core::deadline::yield_with(std::rc::Rc::new(
            move || lm.count_retake(&chain),
        )))
    }
}

impl LockManager {
    /// Count a re-take of `chain` as a yield's (see [`Guard::yields`]).
    fn count_retake(&self, chain: &[(Resource, Mode)]) -> Result<(), MaxTimeMsExpired> {
        for (resource, mode) in chain {
            self.count(resource, *mode, None);
            note_taken(resource);
            secantus_core::deadline::check_for_interrupt()?;
        }
        Ok(())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        STATEMENT_OWNER.with(|o| o.set(self.previous_owner));
        let kept = HELD_TO_END.with(|h| match h.borrow_mut().as_mut() {
            Some(held) => {
                held.push((self.lm.clone(), self.owner));
                true
            }
            None => false,
        });
        if !kept {
            self.lm.release(self.owner);
        }
    }
}

/// Statement holds kept past their statement's end: whose, by which owner.
type KeptHolds = Vec<(std::sync::Arc<LockManager>, Owner)>;

thread_local! {
    /// The statement holds a [`HeldToEnd`] scope keeps past their
    /// statement's end, latest last.
    static HELD_TO_END: std::cell::RefCell<Option<KeptHolds>> =
        const { std::cell::RefCell::new(None) };
}

/// While alive, a statement's locks outlast the statement: they are let go
/// when this drops. A retryable write keeps its session's row in
/// `config.transactions` in the write's own unit of work, under the write's
/// locks, as mongod's `onWriteOpCompletedOnPrimary` does.
pub struct HeldToEnd {
    previous: Option<KeptHolds>,
}

/// Keep the statement locks taken from here until the scope drops.
pub fn hold_to_end() -> HeldToEnd {
    HeldToEnd {
        previous: HELD_TO_END.with(|h| h.replace(Some(Vec::new()))),
    }
}

impl HeldToEnd {
    /// A write round on `db.coll` inside the held statement's locks, kept
    /// with them: the write of the session's row. The transition and global
    /// locks are taken again by their holder, so the round takes no flow
    /// control ticket of its own (measured 8.2.11: a retryable insert takes
    /// one, as any insert). Nothing when no statement lock is held.
    pub fn write_round(&self, db: &str, coll: &str) -> Result<(), MaxTimeMsExpired> {
        let Some((lm, owner)) = HELD_TO_END.with(|h| h.borrow().as_ref()?.last().cloned()) else {
            return Ok(());
        };
        let round = [
            (Resource::ReplicationStateTransition, Mode::Ix),
            (Resource::Global, Mode::Ix),
            (Resource::Database(db.to_string()), Mode::Ix),
            (Resource::Collection(format!("{db}.{coll}")), Mode::Ix),
        ];
        for (res, mode) in &round {
            lm.acquire(owner, res, *mode)?;
        }
        Ok(())
    }
}

impl Drop for HeldToEnd {
    fn drop(&mut self) {
        let held = HELD_TO_END.with(|h| h.replace(self.previous.take()));
        for (lm, owner) in held.into_iter().flatten().rev() {
            lm.release(owner);
        }
    }
}

/// The replication state transition lock in IX, taken and let go, that a
/// retryable write takes beside its rounds: one per command, whether or not
/// it writes, with no global lock (measured 8.2.11: a retryable insert
/// counts three of it to the two global IX of its write and its session's
/// row; an `update` matching nothing two to its one).
pub fn retryable_write_transition_lock(lm: Option<&std::sync::Arc<LockManager>>) {
    let Some(lm) = lm else { return };
    let owner = lm.statement_owner();
    // Refused only by a spent deadline behind a stepdown's X, which this
    // server never takes.
    if lm
        .acquire(owner, &Resource::ReplicationStateTransition, Mode::Ix)
        .is_ok()
    {
        lm.release(owner);
    }
}

/// The locks a write's wait for its journal takes: the transition lock in
/// IX and the global lock in IS, held while the journal is flushed and let
/// go (measured 8.2.11: `j: true` and `w: "majority"` each add one of each
/// to a write's `locks`, `w: 1` none). Taken whether or not the write wrote,
/// as mongod's are for a duplicate key.
pub fn journal_wait(lm: Option<&std::sync::Arc<LockManager>>) -> Option<JournalWait> {
    let lm = lm?;
    let owner = lm.statement_owner();
    let chain = [
        (Resource::ReplicationStateTransition, Mode::Ix),
        (Resource::Global, Mode::Is),
    ];
    acquire_chain(lm, owner, &chain).ok()?;
    Some(JournalWait {
        lm: lm.clone(),
        owner,
    })
}

/// The locks mongod's `ReplicationCoordinatorImpl::getLatestWriteOpTime`
/// reads the oplog's latest entry under: the global lock in IS, with the
/// transition lock in IX beside it -- the same pair as a journal wait's,
/// one more of each in the line's `locks` per read (measured 8.2.11: an
/// `update` that matched nothing reads it twice; RM-360).
pub fn latest_write_optime_locks(lm: Option<&std::sync::Arc<LockManager>>) -> Option<JournalWait> {
    journal_wait(lm)
}

/// The journal wait's locks, let go on drop.
pub struct JournalWait {
    lm: std::sync::Arc<LockManager>,
    owner: Owner,
}

impl Drop for JournalWait {
    fn drop(&mut self) {
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

    /// RM-357: a flow control ticket for each global IX a statement takes
    /// without holding the global lock already; none for a nested one, a
    /// transaction's or an intent-shared one.
    #[test]
    fn a_ticket_for_each_global_ix_taken_apart() {
        use super::{begin_operation, flow_control_tickets, LockManager, Mode, Resource};
        let lm = LockManager::new();
        begin_operation();
        let o = lm.statement_owner();
        lm.acquire(o, &Resource::Global, Mode::Ix).unwrap();
        lm.acquire(o, &Resource::Global, Mode::Ix).unwrap();
        assert_eq!(flow_control_tickets(), 1);
        lm.release(o);
        let o = lm.statement_owner();
        lm.acquire(o, &Resource::Global, Mode::Ix).unwrap();
        lm.release(o);
        assert_eq!(flow_control_tickets(), 2);
        let t = lm.transaction_owner();
        lm.acquire(t, &Resource::Global, Mode::Ix).unwrap();
        lm.release(t);
        let r = lm.statement_owner();
        lm.acquire(r, &Resource::Global, Mode::Is).unwrap();
        lm.release(r);
        assert_eq!(flow_control_tickets(), 2);
        begin_operation();
        assert_eq!(flow_control_tickets(), 0);
    }

    /// RM-357: `findAndModify` takes its full round inside the first, so
    /// its write takes one ticket.
    #[test]
    fn find_and_modify_nests_its_rounds() {
        use super::{statement_rounds, Resource, StorageFacts};
        let facts = StorageFacts {
            storage: None,
            db: "db",
        };
        let rounds = statement_rounds(&bson::doc! {"findAndModify": "c"}, "db", &facts).unwrap();
        assert_eq!(rounds.len(), 1);
        let globals = rounds[0]
            .iter()
            .filter(|(r, _)| *r == Resource::Global)
            .count();
        assert_eq!(globals, 2);
        assert_eq!(
            rounds[0].last().unwrap().0,
            Resource::Collection("db.c".into())
        );
    }

    /// RM-357: under `hold_to_end` a statement's locks outlast it, and a
    /// write round taken inside them is nested: no ticket of its own, and
    /// all of it let go when the scope ends.
    #[test]
    fn held_statement_locks_take_the_session_row_inside_them() {
        use super::{
            begin_operation, flow_control_tickets, hold_to_end, Guard, LockManager, Mode, Resource,
            StorageFacts,
        };
        let lm = std::sync::Arc::new(LockManager::new());
        let facts = StorageFacts {
            storage: None,
            db: "db",
        };
        begin_operation();
        let held = hold_to_end();
        let doc = bson::doc! {"insert": "c", "documents": [{}]};
        drop(
            Guard::statement(Some(&lm), &doc, "db", &facts)
                .unwrap()
                .unwrap(),
        );
        held.write_round("config", "transactions").unwrap();
        assert_eq!(flow_control_tickets(), 1);
        // The collection is still held in IX: an X waits.
        let other = lm.statement_owner();
        let coll = Resource::Collection("db.c".into());
        {
            // The budget bounds this wait only: a grant to an operation
            // whose budget is spent fails its interrupt check (RM-360).
            let _deadline = secantus_core::deadline::arm(Some(20));
            assert!(lm.acquire(other, &coll, Mode::X).is_err());
        }
        drop(held);
        assert!(lm.acquire(other, &coll, Mode::X).is_ok());
        assert!(lm
            .acquire(
                other,
                &Resource::Collection("config.transactions".into()),
                Mode::X
            )
            .is_ok());
        lm.release(other);
    }

    /// A catalog of two databases, `a` (collections `x`, `y`) and `b` (`z`).
    struct Catalog;

    impl super::Facts for Catalog {
        fn exists(&self, _: &str) -> bool {
            true
        }
        fn is_empty(&self, _: &str) -> bool {
            true
        }
        fn index_keys(&self, _: &str) -> Vec<bson::Document> {
            Vec::new()
        }
        fn collections(&self) -> Vec<String> {
            Vec::new()
        }
        fn databases(&self) -> Vec<String> {
            vec!["a".into(), "b".into()]
        }
        fn collections_of(&self, db: &str) -> Vec<String> {
            match db {
                "a" => vec!["x".into(), "y".into()],
                "b" => vec!["z".into()],
                _ => Vec::new(),
            }
        }
    }

    /// Each round's global IS and collection IS, totalled.
    fn reads(rounds: &[Vec<(super::Resource, super::Mode)>]) -> (usize, Vec<String>) {
        use super::{Mode, Resource};
        let globals = rounds
            .iter()
            .flatten()
            .filter(|lock| **lock == (Resource::Global, Mode::Is))
            .count();
        let colls = rounds
            .iter()
            .flatten()
            .filter_map(|(r, _)| match r {
                Resource::Collection(ns) => Some(ns.clone()),
                _ => None,
            })
            .collect();
        (globals, colls)
    }

    #[test]
    fn validate_db_metadata_reads_each_database_and_collection() {
        use super::statement_rounds;
        let api = bson::doc! {"version": "1"};
        let rounds = |doc: bson::Document| statement_rounds(&doc, "a", &Catalog).unwrap();
        // Every database, and each of its collections.
        let all = rounds(bson::doc! {"validateDBMetadata": 1, "apiParameters": api.clone()});
        assert_eq!(
            reads(&all),
            (5, vec!["a.x".into(), "a.y".into(), "b.z".into()])
        );
        // One database.
        let one =
            rounds(bson::doc! {"validateDBMetadata": 1, "db": "a", "apiParameters": api.clone()});
        assert_eq!(reads(&one), (3, vec!["a.x".into(), "a.y".into()]));
        // One collection in every database, whether or not it is there.
        let named = rounds(bson::doc! {
            "validateDBMetadata": 1, "collection": "q", "apiParameters": api.clone()
        });
        assert_eq!(reads(&named), (4, vec!["a.q".into(), "b.q".into()]));
        // A database that does not exist: its own round alone.
        let missing = rounds(bson::doc! {
            "validateDBMetadata": 1, "db": "nosuch", "collection": "q", "apiParameters": api
        });
        assert_eq!(reads(&missing), (1, vec![]));
    }

    /// A collectionless aggregate takes no lock unless a stage reads a
    /// collection (measured 8.2.11, RM-357).
    #[test]
    fn a_collectionless_aggregate_takes_no_lock() {
        use super::statement_rounds;
        let rounds = |pipeline: bson::Bson, target: bson::Bson| {
            statement_rounds(
                &bson::doc! {"aggregate": target, "pipeline": pipeline},
                "a",
                &Catalog,
            )
            .unwrap()
        };
        let docs = bson::bson!([{"$documents": [{"a": 1}]}, {"$facet": {"x": [{"$match": {}}]}}]);
        assert!(rounds(docs.clone(), bson::Bson::Int32(1)).is_empty());
        assert_eq!(rounds(docs, "x".into()).len(), 1);
        let lookup = bson::bson!([
            {"$documents": [{"a": 1}]},
            {"$facet": {"x": [{"$lookup": {"from": "x", "pipeline": [], "as": "j"}}]}},
        ]);
        assert_eq!(rounds(lookup, bson::Bson::Int32(1)).len(), 1);
    }

    // ------------------------------------------- execution tickets (RM-355)

    /// A lock manager whose pools hold `reads` and `writes` tickets.
    fn ticketed(reads: i32, writes: i32) -> std::sync::Arc<LockManager> {
        use crate::admission::execution::{
            ExecutionControl, READ_TRANSACTIONS, WRITE_TRANSACTIONS,
        };
        let lm = std::sync::Arc::new(LockManager::new());
        let values = move |name: &str| match name {
            READ_TRANSACTIONS => Some(bson::Bson::Int32(reads)),
            WRITE_TRANSACTIONS => Some(bson::Bson::Int32(writes)),
            _ => None,
        };
        lm.set_execution(ExecutionControl::build(&values, 8, None));
        lm
    }

    fn used(lm: &LockManager, reader: bool) -> i32 {
        lm.execution().unwrap().holder(reader).used()
    }

    #[test]
    fn the_global_lock_takes_a_ticket_and_returns_it_with_the_lock() {
        let lm = ticketed(5, 5);
        super::begin_operation();
        let reader = lm.statement_owner();
        lm.acquire(reader, &Resource::Global, Mode::Is).unwrap();
        assert_eq!((used(&lm, true), used(&lm, false)), (1, 0));
        lm.release(reader);
        assert_eq!(used(&lm, true), 0);
        // IX takes a write ticket; X none, mongod's exclusive lock needing
        // no admission.
        let writer = lm.statement_owner();
        lm.acquire(writer, &Resource::Global, Mode::Ix).unwrap();
        assert_eq!(used(&lm, false), 1);
        lm.release(writer);
        let exclusive = lm.statement_owner();
        lm.acquire(exclusive, &Resource::Global, Mode::X).unwrap();
        assert_eq!((used(&lm, true), used(&lm, false)), (0, 0));
        lm.release(exclusive);
        // Only the global lock takes one.
        let coll = lm.statement_owner();
        lm.acquire(coll, &Resource::Collection("d.c".into()), Mode::Ix)
            .unwrap();
        assert_eq!(used(&lm, false), 0);
        lm.release(coll);
    }

    #[test]
    fn a_second_global_lock_of_one_operation_rides_on_its_ticket() {
        let lm = ticketed(5, 5);
        super::begin_operation();
        let ctx = std::sync::Arc::new(crate::admission::AdmissionContext::default());
        let _scope = super::admission_scope(ctx.clone());
        let outer = lm.statement_owner();
        lm.acquire(outer, &Resource::Global, Mode::Ix).unwrap();
        let inner = lm.statement_owner();
        lm.acquire(inner, &Resource::Global, Mode::Is).unwrap();
        assert_eq!((used(&lm, true), used(&lm, false)), (0, 1));
        lm.release(inner);
        lm.release(outer);
        // One admission, counted in the operation's context; the next
        // global lock, after the first was let go, is a second.
        assert_eq!(ctx.admissions(), 1);
        let again = lm.statement_owner();
        lm.acquire(again, &Resource::Global, Mode::Is).unwrap();
        lm.release(again);
        assert_eq!(ctx.admissions(), 2);
    }

    #[test]
    fn writers_beyond_the_pool_wait_for_a_ticket() {
        let lm = ticketed(5, 1);
        let held = lm.statement_owner();
        lm.acquire(held, &Resource::Global, Mode::Ix).unwrap();
        let (lm2, done) = (
            lm.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let flag = done.clone();
        let waiter = std::thread::spawn(move || {
            super::begin_operation();
            let owner = lm2.statement_owner();
            lm2.acquire(owner, &Resource::Global, Mode::Ix).unwrap();
            flag.store(true, Ordering::SeqCst);
            lm2.release(owner);
        });
        while lm.execution().unwrap().holder(false).queued() == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!done.load(Ordering::SeqCst));
        // A reader is not held up by the writers' pool.
        let reader = lm.statement_owner();
        lm.acquire(reader, &Resource::Global, Mode::Is).unwrap();
        lm.release(reader);
        lm.release(held);
        waiter.join().unwrap();
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(used(&lm, false), 0);
    }

    #[test]
    fn a_transaction_returns_its_ticket_between_statements() {
        let lm = ticketed(5, 5);
        super::begin_operation();
        let txn = lm.transaction_owner();
        let chain = [
            (Resource::Global, Mode::Ix),
            (Resource::Collection("d.c".into()), Mode::Ix),
        ];
        acquire_chain(&lm, txn, &chain).unwrap();
        assert_eq!(used(&lm, false), 1);
        // The statement ends: the locks stay, the ticket goes.
        drop(TicketStash::new(lm.clone(), txn));
        assert_eq!(used(&lm, false), 0);
        // The next statement takes one again.
        super::begin_operation();
        acquire_chain(&lm, txn, &chain).unwrap();
        assert_eq!(used(&lm, false), 1);
        lm.release(txn);
        assert_eq!(used(&lm, false), 0);
    }

    #[test]
    fn a_transaction_gives_up_a_ticket_a_waiter_on_its_locks_may_hold() {
        // mongod's `reacquireTicket`: with the pool empty and a request
        // queued on one of the transaction's locks, 24 LockTimeout.
        let lm = ticketed(5, 1);
        let txn = lm.transaction_owner();
        let coll = Resource::Collection("d.c".into());
        super::begin_operation();
        acquire_chain(
            &lm,
            txn,
            &[(Resource::Global, Mode::Ix), (coll.clone(), Mode::Ix)],
        )
        .unwrap();
        lm.release_ticket(txn);
        // A drop takes the one write ticket and waits on the collection.
        let lm2 = lm.clone();
        let dropper = std::thread::spawn(move || {
            super::begin_operation();
            let owner = lm2.statement_owner();
            let _budget = secantus_core::deadline::arm(Some(2_000));
            let r = acquire_chain(
                &lm2,
                owner,
                &[
                    (Resource::Global, Mode::Ix),
                    (Resource::Collection("d.c".into()), Mode::X),
                ],
            );
            lm2.release(owner);
            r.is_ok()
        });
        while !lm
            .inner
            .lock()
            .unwrap()
            .heads
            .get(&coll)
            .is_some_and(|h| !h.queue.is_empty())
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        super::begin_operation();
        assert!(lm.acquire(txn, &Resource::Global, Mode::Ix).is_err());
        let e = super::take_ticket_error().unwrap();
        assert_eq!(
            (e.code, e.errmsg.as_str()),
            (
                24,
                "Unable to acquire ticket with mode '2' due to detected lock conflict for \
                 resource {Collection : d.c}"
            )
        );
        // The transaction aborts, and the drop goes through.
        lm.release(txn);
        assert!(dropper.join().unwrap());
    }
}
