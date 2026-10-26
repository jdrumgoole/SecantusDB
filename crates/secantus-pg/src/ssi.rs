//! SERIALIZABLE: PostgreSQL 15's Serializable Snapshot Isolation
//! (`predicate.c`), transcribed (RP-061).
//!
//! A serializable transaction is snapshot isolation plus a watch for the
//! one shape every serialization anomaly takes: two consecutive
//! read/write antidependencies, `Tin -> Tpivot -> Tout`, with `Tout`
//! committing first. Reads leave SIREAD ("predicate") locks on what they
//! read -- a tuple, a page, a whole relation, promoted from finer to
//! coarser past PostgreSQL's limits -- and a write that meets another
//! transaction's lock records an rw-conflict in; a read that meets a
//! version another transaction wrote, and cannot see, records one out.
//! Each new conflict is checked for that shape (`OnConflict_
//! CheckForSerializationFailure`), and a commit checks once more
//! (`PreCommit_CheckForSerializationFailure`); the transaction PostgreSQL
//! would cancel gets its 40001, with PostgreSQL's reason code.
//!
//! One state per cluster (a store), as PostgreSQL keeps one in shared
//! memory; the lists keep PostgreSQL's orders, which decide which conflict
//! is found first. Not modelled: summarizing old transactions into the
//! SLRU (PostgreSQL does it only when its fixed table of transactions is
//! full; this one has no fixed size), parallel query, and two-phase commit.
//! Only writes by serializable transactions are tracked for conflict-out:
//! a version another isolation level wrote is one no conflict can come
//! from (PostgreSQL finds no `SERIALIZABLEXID` for its xid either), but its
//! visibility is not known here, so a tuple a READ COMMITTED transaction
//! inserted counts as visible to every reader.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

const FLAG_COMMITTED: u32 = 0x01;
const FLAG_PREPARED: u32 = 0x02;
const FLAG_ROLLED_BACK: u32 = 0x04;
const FLAG_DOOMED: u32 = 0x08;
const FLAG_CONFLICT_OUT: u32 = 0x10;
const FLAG_READ_ONLY: u32 = 0x20;
const FLAG_DEFERRABLE_WAITING: u32 = 0x40;
const FLAG_RO_SAFE: u32 = 0x80;
const FLAG_RO_UNSAFE: u32 = 0x100;
const FLAG_SUMMARY_CONFLICT_IN: u32 = 0x200;
const FLAG_SUMMARY_CONFLICT_OUT: u32 = 0x400;

/// `InvalidSerCommitSeqNo`.
const INVALID_SEQ: u64 = u64::MAX;
/// `FirstNormalSerCommitSeqNo`.
const FIRST_NORMAL_SEQ: u64 = 2;

/// `max_pred_locks_per_transaction`, `max_pred_locks_per_relation`,
/// `max_pred_locks_per_page`: PostgreSQL's defaults.
const MAX_PRED_LOCKS_PER_XACT: i64 = 64;
const MAX_PRED_LOCKS_PER_RELATION: i64 = -2;
const MAX_PRED_LOCKS_PER_PAGE: i64 = 2;

/// A predicate lock's target: a relation (a table or an index), a page of
/// it, or a tuple. `db` tells databases apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Target {
    Relation {
        db: u64,
        rel: i64,
    },
    Page {
        db: u64,
        rel: i64,
        blk: u32,
    },
    Tuple {
        db: u64,
        rel: i64,
        blk: u32,
        off: u16,
    },
}

impl Target {
    /// `GetParentPredicateLockTag`.
    fn parent(self) -> Option<Target> {
        match self {
            Target::Relation { .. } => None,
            Target::Page { db, rel, .. } => Some(Target::Relation { db, rel }),
            Target::Tuple { db, rel, blk, .. } => Some(Target::Page { db, rel, blk }),
        }
    }

    fn rel(self) -> (u64, i64) {
        match self {
            Target::Relation { db, rel }
            | Target::Page { db, rel, .. }
            | Target::Tuple { db, rel, .. } => (db, rel),
        }
    }

    /// `TargetTagIsCoveredBy(self, covering)`.
    fn covered_by(self, covering: Target) -> bool {
        match (self, covering) {
            (
                Target::Page { db, rel, .. } | Target::Tuple { db, rel, .. },
                Target::Relation { db: d, rel: r },
            ) => db == d && rel == r,
            (
                Target::Tuple { db, rel, blk, .. },
                Target::Page {
                    db: d,
                    rel: r,
                    blk: b,
                },
            ) => db == d && rel == r && blk == b,
            _ => false,
        }
    }

    /// `MaxPredicateChildLocks`.
    fn max_children(self) -> i64 {
        match self {
            Target::Relation { .. } => {
                if MAX_PRED_LOCKS_PER_RELATION < 0 {
                    MAX_PRED_LOCKS_PER_XACT / (-MAX_PRED_LOCKS_PER_RELATION) - 1
                } else {
                    MAX_PRED_LOCKS_PER_RELATION
                }
            }
            Target::Page { .. } => MAX_PRED_LOCKS_PER_PAGE,
            Target::Tuple { .. } => 0,
        }
    }
}

/// A transaction's snapshot, as `GetSnapshotData` takes it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Snap {
    pub xmin: u64,
    pub xmax: u64,
    pub xip: Vec<u64>,
}

/// The error a 40001 carries: its detail (`Reason code: ...`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Failure(pub String);

impl Failure {
    pub(crate) const MESSAGE: &'static str =
        "could not serialize access due to read/write dependencies among transactions";
    pub(crate) const HINT: &'static str = "The transaction might succeed if retried.";

    fn reason(r: &str) -> Failure {
        Failure(format!("Reason code: {r}"))
    }
}

type Res<T> = Result<T, Failure>;

#[derive(Debug, Default, Clone, Copy)]
struct LocalLock {
    held: bool,
    child_locks: i64,
}

/// `SERIALIZABLEXACT`.
#[derive(Debug)]
struct Sxact {
    /// `SeqNo.lastCommitBeforeSnapshot`.
    last_commit_before_snapshot: u64,
    /// `SeqNo.earliestOutConflictCommit`.
    earliest_out_conflict_commit: u64,
    prepare_seq: u64,
    commit_seq: u64,
    top_xid: Option<u64>,
    finished_before: u64,
    snap: Snap,
    flags: u32,
    /// The conflicts this one reads before (`outConflicts`), and is read
    /// before by (`inConflicts`), by id, in the order recorded.
    out_conflicts: Vec<usize>,
    in_conflicts: Vec<usize>,
    /// `possibleUnsafeConflicts`: a read-only transaction's writers, a
    /// writer's read-only transactions.
    possible_unsafe: Vec<usize>,
    /// Its predicate locks, in the order taken (`predicateLocks`).
    locks: Vec<Target>,
    /// `LocalPredicateLockHash`.
    local: HashMap<Target, LocalLock>,
    did_write: bool,
    /// On `FinishedSerializableTransactions`.
    finished: bool,
}

impl Sxact {
    fn is(&self, f: u32) -> bool {
        self.flags & f != 0
    }
}

/// `RWConflictData`: `out` reads before `in_` writes.
#[derive(Debug, Clone, Copy)]
struct Conflict {
    out: usize,
    in_: usize,
}

/// One tracked tuple version: the serializable transactions that inserted
/// and deleted it, by xid, and the row it holds (an index scan examines
/// the versions its key range reaches).
#[derive(Debug, Clone, Default)]
struct Version {
    xmin: Option<u64>,
    xmax: Option<u64>,
    doc: Option<bson::Document>,
}

#[derive(Default)]
pub(crate) struct State {
    sxacts: Vec<Option<Sxact>>,
    /// `PredXact->activeList`, in creation order.
    active: Vec<usize>,
    conflicts: Vec<Option<Conflict>>,
    free_conflicts: Vec<usize>,
    /// `PredicateLockTargetHash`: each target's holders, in order.
    targets: HashMap<Target, Vec<usize>>,
    /// `SerializableXidHash`.
    xids: HashMap<u64, usize>,
    last_commit_seq: u64,
    can_partial_clear_through: u64,
    have_partial_cleared_through: u64,
    global_xmin: Option<u64>,
    global_xmin_count: u64,
    writable_count: u64,
    finished_list: VecDeque<usize>,
    /// The versions serializable transactions wrote, by relation and ctid.
    versions: HashMap<(u64, i64), BTreeMap<i64, Version>>,
    /// Each backend's transaction.
    by_pid: HashMap<i32, usize>,
    /// The cluster's next xid, as the caller last gave it
    /// (`finishedBefore` is read from it).
    next_xid: u64,
}

/// The clusters' states, by the store's address.
fn states() -> &'static Mutex<HashMap<usize, State>> {
    static S: OnceLock<Mutex<HashMap<usize, State>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Run `f` on cluster `key`'s state; `next_xid` is the cluster's next
/// xid.
pub(crate) fn with<T>(key: usize, next_xid: u64, f: impl FnOnce(&mut State) -> T) -> T {
    let mut all = states().lock().unwrap_or_else(|e| e.into_inner());
    let st = all.entry(key).or_insert_with(|| State {
        last_commit_seq: FIRST_NORMAL_SEQ - 1,
        ..Default::default()
    });
    st.next_xid = next_xid;
    f(st)
}

/// Forget a cluster (its store closed).
pub(crate) fn forget(key: usize) {
    states()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
}

impl State {
    fn sx(&self, i: usize) -> &Sxact {
        self.sxacts[i].as_ref().expect("a live sxact")
    }

    fn sx_mut(&mut self, i: usize) -> &mut Sxact {
        self.sxacts[i].as_mut().expect("a live sxact")
    }

    fn conflict(&self, c: usize) -> Conflict {
        self.conflicts[c].expect("a live conflict")
    }

    /// The backend's serializable transaction, if it has one.
    pub(crate) fn mine(&self, pid: i32) -> Option<usize> {
        self.by_pid.get(&pid).copied()
    }

    // ---------------------------------------------------------------- lists

    /// `RWConflictExists`.
    fn conflict_exists(&self, reader: usize, writer: usize) -> bool {
        let (r, w) = (self.sx(reader), self.sx(writer));
        if r.is(FLAG_DOOMED)
            || w.is(FLAG_DOOMED)
            || r.out_conflicts.is_empty()
            || w.in_conflicts.is_empty()
        {
            return false;
        }
        r.out_conflicts
            .iter()
            .any(|&c| self.conflict(c).in_ == writer)
    }

    fn new_conflict(&mut self, out: usize, in_: usize) -> usize {
        let c = Conflict { out, in_ };
        match self.free_conflicts.pop() {
            Some(i) => {
                self.conflicts[i] = Some(c);
                i
            }
            None => {
                self.conflicts.push(Some(c));
                self.conflicts.len() - 1
            }
        }
    }

    /// `SetRWConflict`.
    fn set_conflict(&mut self, reader: usize, writer: usize) {
        let c = self.new_conflict(reader, writer);
        self.sx_mut(reader).out_conflicts.push(c);
        self.sx_mut(writer).in_conflicts.push(c);
    }

    /// `SetPossibleUnsafeConflict`.
    fn set_possible_unsafe(&mut self, ro: usize, active: usize) {
        let c = self.new_conflict(active, ro);
        self.sx_mut(active).possible_unsafe.push(c);
        self.sx_mut(ro).possible_unsafe.push(c);
    }

    /// `ReleaseRWConflict`: off both its lists.
    fn release_conflict(&mut self, c: usize) {
        let Conflict { out, in_ } = self.conflict(c);
        for s in [out, in_] {
            if let Some(Some(x)) = self.sxacts.get_mut(s) {
                x.out_conflicts.retain(|&y| y != c);
                x.in_conflicts.retain(|&y| y != c);
                x.possible_unsafe.retain(|&y| y != c);
            }
        }
        self.conflicts[c] = None;
        self.free_conflicts.push(c);
    }

    /// `FlagSxactUnsafe`.
    fn flag_unsafe(&mut self, sx: usize) {
        self.sx_mut(sx).flags |= FLAG_RO_UNSAFE;
        for c in self.sx(sx).possible_unsafe.clone() {
            self.release_conflict(c);
        }
    }

    // ------------------------------------------------------------ snapshots

    /// `GetSerializableTransactionSnapshotInt`: register backend `pid`'s
    /// transaction at its snapshot. `None` when a read-only transaction
    /// opts out (no writable serializable transaction runs).
    pub(crate) fn register(&mut self, pid: i32, snap: Snap, read_only: bool) -> Option<usize> {
        if let Some(sx) = self.mine(pid) {
            return Some(sx);
        }
        if read_only && self.writable_count == 0 {
            return None;
        }
        let sx = Sxact {
            last_commit_before_snapshot: self.last_commit_seq,
            earliest_out_conflict_commit: 0,
            prepare_seq: INVALID_SEQ,
            commit_seq: INVALID_SEQ,
            top_xid: None,
            finished_before: 0,
            snap: snap.clone(),
            flags: 0,
            out_conflicts: Vec::new(),
            in_conflicts: Vec::new(),
            possible_unsafe: Vec::new(),
            locks: Vec::new(),
            local: HashMap::new(),
            did_write: false,
            finished: false,
        };
        let id = match self.sxacts.iter().position(Option::is_none) {
            Some(i) => {
                self.sxacts[i] = Some(sx);
                i
            }
            None => {
                self.sxacts.push(Some(sx));
                self.sxacts.len() - 1
            }
        };
        self.active.push(id);
        if read_only {
            self.sx_mut(id).flags |= FLAG_READ_ONLY;
            // Every concurrent read/write transaction is a possible
            // conflict; if all commit without a conflict out to an earlier
            // one, the snapshot is safe.
            for other in self.active.clone() {
                if other == id {
                    continue;
                }
                let o = self.sx(other);
                if !o.is(FLAG_COMMITTED) && !o.is(FLAG_DOOMED) && !o.is(FLAG_READ_ONLY) {
                    self.set_possible_unsafe(id, other);
                }
            }
            if self.sx(id).possible_unsafe.is_empty() {
                self.active.retain(|&x| x != id);
                self.sxacts[id] = None;
                return None;
            }
        } else {
            self.writable_count += 1;
        }
        match self.global_xmin {
            None => {
                self.global_xmin = Some(snap.xmin);
                self.global_xmin_count = 1;
            }
            Some(x) if x == snap.xmin => self.global_xmin_count += 1,
            Some(_) => {}
        }
        self.by_pid.insert(pid, id);
        Some(id)
    }

    /// `RegisterPredicateLockingXid`: the transaction's xid, once it has
    /// one.
    pub(crate) fn set_xid(&mut self, sx: usize, xid: u64) {
        if self.sx(sx).top_xid.is_some() {
            return;
        }
        self.sx_mut(sx).top_xid = Some(xid);
        self.xids.insert(xid, sx);
    }

    // ------------------------------------------------------------ locking

    /// `SerializationNeededForRead`, its side effect included: a
    /// transaction flagged RO_SAFE drops its locks and stops checking.
    fn needed_for_read(&mut self, pid: i32) -> Option<usize> {
        let sx = self.mine(pid)?;
        if self.sx(sx).is(FLAG_RO_SAFE) {
            self.release(pid, false, true);
            return None;
        }
        Some(sx)
    }

    fn lock_exists(&self, sx: usize, t: Target) -> bool {
        self.sx(sx).local.get(&t).is_some_and(|l| l.held)
    }

    /// `CoarserLockCovers`.
    fn coarser_covers(&self, sx: usize, t: Target) -> bool {
        let mut t = t;
        while let Some(p) = t.parent() {
            t = p;
            if self.lock_exists(sx, t) {
                return true;
            }
        }
        false
    }

    /// `CreatePredicateLock`.
    fn create_lock(&mut self, sx: usize, t: Target) {
        let holders = self.targets.entry(t).or_default();
        if !holders.contains(&sx) {
            holders.push(sx);
            self.sx_mut(sx).locks.push(t);
        }
    }

    fn remove_lock(&mut self, sx: usize, t: Target) {
        if let Some(h) = self.targets.get_mut(&t) {
            h.retain(|&x| x != sx);
            if h.is_empty() {
                self.targets.remove(&t);
            }
        }
        if let Some(Some(s)) = self.sxacts.get_mut(sx) {
            s.locks.retain(|&x| x != t);
        }
    }

    /// `DecrementParentLocks`.
    fn decrement_parents(&mut self, sx: usize, t: Target) {
        let mut t = t;
        while let Some(p) = t.parent() {
            t = p;
            let s = self.sx_mut(sx);
            let Some(l) = s.local.get_mut(&t) else {
                continue;
            };
            l.child_locks -= 1;
            if l.child_locks < 0 {
                l.child_locks = 0;
            }
            if l.child_locks == 0 && !l.held {
                s.local.remove(&t);
            }
        }
    }

    /// `DeleteChildTargetLocks`.
    fn delete_children(&mut self, sx: usize, t: Target) {
        for old in self.sx(sx).locks.clone() {
            if old.covered_by(t) {
                self.remove_lock(sx, old);
                self.sx_mut(sx).local.remove(&old);
                self.decrement_parents(sx, old);
            }
        }
    }

    /// `CheckAndPromotePredicateLockRequest`.
    fn promote(&mut self, sx: usize, t: Target) -> bool {
        let mut promotion = None;
        let mut t = t;
        while let Some(p) = t.parent() {
            t = p;
            let l = self.sx_mut(sx).local.entry(t).or_insert(LocalLock {
                held: false,
                child_locks: 0,
            });
            l.child_locks += 1;
            if l.child_locks > t.max_children() {
                promotion = Some(t);
            }
        }
        match promotion {
            Some(p) => {
                self.acquire(sx, p);
                true
            }
            None => false,
        }
    }

    /// `PredicateLockAcquire`.
    fn acquire(&mut self, sx: usize, t: Target) {
        if self.lock_exists(sx, t) || self.coarser_covers(sx, t) {
            return;
        }
        let l = self.sx_mut(sx).local.entry(t).or_default();
        l.held = true;
        self.create_lock(sx, t);
        if !self.promote(sx, t) && !matches!(t, Target::Tuple { .. }) {
            self.delete_children(sx, t);
        }
    }

    /// `PredicateLockRelation`.
    pub(crate) fn lock_relation(&mut self, pid: i32, db: u64, rel: i64) {
        if let Some(sx) = self.needed_for_read(pid) {
            self.acquire(sx, Target::Relation { db, rel });
        }
    }

    /// `PredicateLockPage`.
    pub(crate) fn lock_page(&mut self, pid: i32, db: u64, rel: i64, blk: u32) {
        if let Some(sx) = self.needed_for_read(pid) {
            self.acquire(sx, Target::Page { db, rel, blk });
        }
    }

    /// `PredicateLockTID` of a heap tuple whose inserter is `xmin`.
    pub(crate) fn lock_tuple(&mut self, pid: i32, db: u64, rel: i64, ctid: i64, xmin: Option<u64>) {
        let Some(sx) = self.needed_for_read(pid) else {
            return;
        };
        // A tuple it wrote, it holds a write lock on.
        if xmin.is_some() && xmin == self.sx(sx).top_xid {
            return;
        }
        if self.lock_exists(sx, Target::Relation { db, rel }) {
            return;
        }
        let (blk, off) = ((ctid / 65536) as u32, (ctid % 65536) as u16);
        self.acquire(sx, Target::Tuple { db, rel, blk, off });
    }

    // ----------------------------------------------------- conflicts out

    /// `CheckForSerializableConflictOutNeeded`: a doomed transaction fails
    /// at the next tuple it reads.
    pub(crate) fn conflict_out_needed(&mut self, pid: i32) -> Res<Option<usize>> {
        let Some(sx) = self.needed_for_read(pid) else {
            return Ok(None);
        };
        if self.sx(sx).is(FLAG_DOOMED) {
            return Err(Failure::reason(
                "Canceled on identification as a pivot, during conflict out checking.",
            ));
        }
        Ok(Some(sx))
    }

    /// `XidIsConcurrent`.
    fn concurrent(&self, sx: usize, xid: u64) -> bool {
        let s = &self.sx(sx).snap;
        if xid < s.xmin {
            return false;
        }
        if xid >= s.xmax {
            return true;
        }
        s.xip.contains(&xid)
    }

    /// `CheckForSerializableConflictOut`.
    fn conflict_out(&mut self, me: usize, xid: u64) -> Res<()> {
        if self.sx(me).is(FLAG_DOOMED) {
            return Err(Failure::reason(
                "Canceled on identification as a pivot, during conflict out checking.",
            ));
        }
        if Some(xid) == self.sx(me).top_xid {
            return Ok(());
        }
        let Some(&other) = self.xids.get(&xid) else {
            // Not serializable, or not important.
            return Ok(());
        };
        if other == me || self.sx(other).is(FLAG_DOOMED) {
            return Ok(());
        }
        if self.sx(other).is(FLAG_SUMMARY_CONFLICT_OUT) {
            if !self.sx(other).is(FLAG_PREPARED) {
                self.sx_mut(other).flags |= FLAG_DOOMED;
                return Ok(());
            }
            return Err(Failure::reason("Canceled on conflict out to old pivot."));
        }
        // A read-only transaction appears to run before a committed writer
        // with no conflict out to one that committed earlier.
        let (m, o) = (self.sx(me), self.sx(other));
        if m.is(FLAG_READ_ONLY)
            && o.is(FLAG_COMMITTED)
            && !o.is(FLAG_SUMMARY_CONFLICT_OUT)
            && (!o.is(FLAG_CONFLICT_OUT)
                || m.last_commit_before_snapshot < o.earliest_out_conflict_commit)
        {
            return Ok(());
        }
        if !self.concurrent(me, xid) {
            return Ok(());
        }
        if self.conflict_exists(me, other) {
            return Ok(());
        }
        self.flag_conflict(me, other, me)
    }

    /// `HeapCheckForSerializableConflictOut` over one tracked version, for
    /// the reader `me`.
    fn check_version(&mut self, me: usize, v: &Version) -> Res<()> {
        let state = |x: u64| -> Option<bool> {
            // Some(committed) for a tracked writer, None for an untracked
            // one (taken as committed long ago).
            self.xids.get(&x).map(|&s| self.sx(s).is(FLAG_COMMITTED))
        };
        let my_xid = self.sx(me).top_xid;
        let visible_xid = |x: Option<u64>| -> bool {
            match x {
                None => true,
                Some(x) if Some(x) == my_xid => true,
                Some(x) => match state(x) {
                    None => true,
                    Some(committed) => committed && !self.concurrent(me, x),
                },
            }
        };
        let xmin_in_progress = v
            .xmin
            .is_some_and(|x| Some(x) != my_xid && state(x) == Some(false))
            || v.xmin.is_some_and(|x| Some(x) == my_xid);
        let inserted_visible = visible_xid(v.xmin);
        let deleted_visible = v.xmax.is_some() && visible_xid(v.xmax);
        let visible = inserted_visible && !deleted_visible;
        let txn_xmin = self.sx(me).snap.xmin;
        let xid = if xmin_in_progress {
            // HEAPTUPLE_INSERT_IN_PROGRESS.
            v.xmin
        } else if v.xmax.is_none() {
            // HEAPTUPLE_LIVE.
            if visible {
                return Ok(());
            }
            v.xmin
        } else {
            let xmax = v.xmax.expect("checked");
            let deleter_committed = Some(xmax) != my_xid && state(xmax) != Some(false);
            if deleter_committed && xmax < txn_xmin {
                // HEAPTUPLE_DEAD.
                return Ok(());
            }
            // HEAPTUPLE_RECENTLY_DEAD or HEAPTUPLE_DELETE_IN_PROGRESS.
            let x = if visible { Some(xmax) } else { v.xmin };
            match x {
                Some(x) if x < txn_xmin => return Ok(()),
                None => return Ok(()),
                _ => x,
            }
        };
        let Some(xid) = xid else {
            return Ok(());
        };
        if Some(xid) == my_xid || xid < txn_xmin {
            return Ok(());
        }
        self.conflict_out(me, xid)
    }

    /// The tuples a scan examines, in ctid order: each tracked version of
    /// relation `rel` that `examined` keeps (every one, for a sequential
    /// scan), through `HeapCheckForSerializableConflictOut`. `tuples` says
    /// whether the scan met any tuple at all, the condition for a doomed
    /// transaction's failure.
    pub(crate) fn scan_conflicts_out(
        &mut self,
        pid: i32,
        db: u64,
        rel: i64,
        examined: &dyn Fn(i64, Option<&bson::Document>) -> bool,
        tuples: bool,
    ) -> Res<()> {
        if !tuples {
            return Ok(());
        }
        let Some(me) = self.conflict_out_needed(pid)? else {
            return Ok(());
        };
        let versions: Vec<Version> = match self.versions.get(&(db, rel)) {
            None => return Ok(()),
            Some(m) => m
                .iter()
                .filter(|(c, v)| examined(**c, v.doc.as_ref()))
                .map(|(_, v)| v.clone())
                .collect(),
        };
        for v in &versions {
            self.check_version(me, v)?;
        }
        Ok(())
    }

    /// Is any tracked version of `rel` one `examined` keeps?
    pub(crate) fn has_versions(
        &self,
        db: u64,
        rel: i64,
        examined: &dyn Fn(i64, Option<&bson::Document>) -> bool,
    ) -> bool {
        self.versions
            .get(&(db, rel))
            .is_some_and(|m| m.iter().any(|(c, v)| examined(*c, v.doc.as_ref())))
    }

    // ------------------------------------------------------ conflicts in

    /// `CheckTargetForConflictsIn`.
    fn target_conflicts_in(&mut self, me: usize, t: Target) -> Res<()> {
        let Some(holders) = self.targets.get(&t).cloned() else {
            return Ok(());
        };
        let my_xmin = self.sx(me).snap.xmin;
        let mut mine = false;
        for sx in holders {
            if sx == me {
                // A write lock on the tuple makes our SIREAD lock redundant.
                if matches!(t, Target::Tuple { .. }) {
                    mine = true;
                }
                continue;
            }
            let o = self.sx(sx);
            if !o.is(FLAG_DOOMED)
                && (!o.is(FLAG_COMMITTED) || my_xmin < o.finished_before)
                && !self.conflict_exists(sx, me)
            {
                self.flag_conflict(sx, me, me)?;
            }
        }
        if mine {
            self.remove_lock(me, t);
            self.sx_mut(me).local.remove(&t);
            self.decrement_parents(me, t);
        }
        Ok(())
    }

    /// `CheckForSerializableConflictIn`: a write of tuple `ctid` (an update
    /// or delete) on page `blk` of relation `rel`, either optional (an
    /// insert names neither, an index insert only its page).
    pub(crate) fn conflict_in(
        &mut self,
        pid: i32,
        db: u64,
        rel: i64,
        ctid: Option<i64>,
        blk: Option<u32>,
    ) -> Res<()> {
        let Some(me) = self.mine(pid) else {
            return Ok(());
        };
        if self.sx(me).is(FLAG_DOOMED) {
            return Err(Failure::reason(
                "Canceled on identification as a pivot, during conflict in checking.",
            ));
        }
        self.sx_mut(me).did_write = true;
        if let Some(c) = ctid {
            let (blk, off) = ((c / 65536) as u32, (c % 65536) as u16);
            self.target_conflicts_in(me, Target::Tuple { db, rel, blk, off })?;
        }
        if let Some(blk) = blk {
            self.target_conflicts_in(me, Target::Page { db, rel, blk })?;
        }
        self.target_conflicts_in(me, Target::Relation { db, rel })
    }

    /// `CheckTableForSerializableConflictIn`: a TRUNCATE or DROP of the
    /// table conflicts with every lock on it.
    pub(crate) fn table_conflict_in(&mut self, pid: i32, db: u64, rel: i64) -> Res<()> {
        if self.global_xmin.is_none() {
            return Ok(());
        }
        let Some(me) = self.mine(pid) else {
            return Ok(());
        };
        self.sx_mut(me).did_write = true;
        let mut targets: Vec<Target> = self
            .targets
            .keys()
            .copied()
            .filter(|t| t.rel() == (db, rel))
            .collect();
        targets.sort();
        for t in targets {
            for sx in self.targets.get(&t).cloned().unwrap_or_default() {
                if sx != me && !self.conflict_exists(sx, me) {
                    self.flag_conflict(sx, me, me)?;
                }
            }
        }
        Ok(())
    }

    /// The version a serializable writer made: `ctid` inserted, or the old
    /// version deleted.
    pub(crate) fn note_version(
        &mut self,
        pid: i32,
        db: u64,
        rel: i64,
        ctid: i64,
        inserted: bool,
        doc: Option<bson::Document>,
    ) {
        let Some(me) = self.mine(pid) else {
            return;
        };
        let Some(xid) = self.sx(me).top_xid else {
            return;
        };
        let v = self
            .versions
            .entry((db, rel))
            .or_default()
            .entry(ctid)
            .or_default();
        if inserted {
            v.xmin = Some(xid);
        } else {
            v.xmax = Some(xid);
        }
        if v.doc.is_none() {
            v.doc = doc;
        }
    }

    /// `PredicateLockPageSplit`: the locks on the old page also cover the
    /// new one.
    pub(crate) fn page_split(&mut self, db: u64, rel: i64, old: u32, new: u32) {
        if self.global_xmin.is_none() {
            return;
        }
        let from = Target::Page { db, rel, blk: old };
        let to = Target::Page { db, rel, blk: new };
        for sx in self.targets.get(&from).cloned().unwrap_or_default() {
            self.create_lock(sx, to);
        }
    }

    // --------------------------------------------------------- the check

    /// `FlagRWConflict`, as backend `me` records it.
    fn flag_conflict(&mut self, reader: usize, writer: usize, me: usize) -> Res<()> {
        self.check_failure(reader, writer, me)?;
        self.set_conflict(reader, writer);
        Ok(())
    }

    /// `OnConflict_CheckForSerializationFailure`.
    fn check_failure(&mut self, reader: usize, writer: usize, me: usize) -> Res<()> {
        let mut failure = false;
        let w = self.sx(writer);
        if w.is(FLAG_COMMITTED) && (w.is(FLAG_CONFLICT_OUT) || w.is(FLAG_SUMMARY_CONFLICT_OUT)) {
            failure = true;
        }
        if !failure {
            if w.is(FLAG_SUMMARY_CONFLICT_OUT) {
                failure = true;
            } else {
                let r = self.sx(reader);
                for &c in &w.out_conflicts {
                    let t2 = self.sx(self.conflict(c).in_);
                    if t2.is(FLAG_PREPARED)
                        && (!r.is(FLAG_COMMITTED) || t2.prepare_seq <= r.commit_seq)
                        && (!w.is(FLAG_COMMITTED) || t2.prepare_seq <= w.commit_seq)
                        && (!r.is(FLAG_READ_ONLY)
                            || t2.prepare_seq <= r.last_commit_before_snapshot)
                    {
                        failure = true;
                        break;
                    }
                }
            }
        }
        let r = self.sx(reader);
        if !failure && w.is(FLAG_PREPARED) && !r.is(FLAG_READ_ONLY) {
            if r.is(FLAG_SUMMARY_CONFLICT_IN) {
                failure = true;
            } else {
                for &c in &r.in_conflicts {
                    let t0 = self.sx(self.conflict(c).out);
                    if !t0.is(FLAG_DOOMED)
                        && (!t0.is(FLAG_COMMITTED) || t0.commit_seq >= w.prepare_seq)
                        && (!t0.is(FLAG_READ_ONLY)
                            || t0.last_commit_before_snapshot >= w.prepare_seq)
                    {
                        failure = true;
                        break;
                    }
                }
            }
        }
        if failure {
            if me == writer {
                return Err(Failure::reason(
                    "Canceled on identification as a pivot, during write.",
                ));
            }
            if self.sx(writer).is(FLAG_PREPARED) {
                let xid = self.sx(writer).top_xid.unwrap_or(0) & 0xFFFF_FFFF;
                return Err(Failure::reason(&format!(
                    "Canceled on conflict out to pivot {xid}, during read."
                )));
            }
            self.sx_mut(writer).flags |= FLAG_DOOMED;
        }
        Ok(())
    }

    /// `PreCommit_CheckForSerializationFailure`.
    pub(crate) fn pre_commit(&mut self, pid: i32) -> Res<()> {
        let Some(me) = self.mine(pid) else {
            return Ok(());
        };
        if self.sx(me).is(FLAG_DOOMED) {
            return Err(Failure::reason(
                "Canceled on identification as a pivot, during commit attempt.",
            ));
        }
        for near in self.sx(me).in_conflicts.clone() {
            let pivot = self.conflict(near).out;
            let p = self.sx(pivot);
            if p.is(FLAG_COMMITTED) || p.is(FLAG_DOOMED) {
                continue;
            }
            for far in p.in_conflicts.clone() {
                let tin = self.conflict(far).out;
                let t = self.sx(tin);
                if tin == me
                    || (!t.is(FLAG_COMMITTED) && !t.is(FLAG_READ_ONLY) && !t.is(FLAG_DOOMED))
                {
                    if self.sx(pivot).is(FLAG_PREPARED) {
                        return Err(Failure::reason(
                            "Canceled on commit attempt with conflict in from prepared pivot.",
                        ));
                    }
                    self.sx_mut(pivot).flags |= FLAG_DOOMED;
                    break;
                }
            }
        }
        self.last_commit_seq += 1;
        let seq = self.last_commit_seq;
        let s = self.sx_mut(me);
        s.prepare_seq = seq;
        s.flags |= FLAG_PREPARED;
        Ok(())
    }

    // ----------------------------------------------------------- release

    /// `SetNewSxactGlobalXmin`.
    fn new_global_xmin(&mut self) {
        self.global_xmin = None;
        self.global_xmin_count = 0;
        for &i in &self.active {
            let s = self.sx(i);
            if s.is(FLAG_ROLLED_BACK) || s.is(FLAG_COMMITTED) {
                continue;
            }
            match self.global_xmin {
                Some(x) if s.snap.xmin > x => {}
                Some(x) if s.snap.xmin == x => self.global_xmin_count += 1,
                _ => {
                    self.global_xmin = Some(s.snap.xmin);
                    self.global_xmin_count = 1;
                }
            }
        }
    }

    /// `ReleasePredicateLocks`: backend `pid`'s transaction ends (committed
    /// or not), or a read-only one is found safe (`ro_safe`). `next_xid`
    /// is the cluster's next xid (`finishedBefore`).
    pub(crate) fn release_at(&mut self, pid: i32, is_commit: bool, _ro_safe: bool, next_xid: u64) {
        let Some(me) = self.mine(pid) else {
            return;
        };
        self.by_pid.remove(&pid);
        let declared_read_only = self.sx(me).is(FLAG_READ_ONLY);
        let did_write = self.sx(me).did_write;
        self.sx_mut(me).finished_before = next_xid;
        if is_commit {
            self.last_commit_seq += 1;
            let seq = self.last_commit_seq;
            let s = self.sx_mut(me);
            s.flags |= FLAG_COMMITTED;
            s.commit_seq = seq;
            // A commit without a write is read-only, after the fact.
            if !did_write {
                s.flags |= FLAG_READ_ONLY;
            }
        } else {
            let s = self.sx_mut(me);
            s.flags |= FLAG_DOOMED | FLAG_ROLLED_BACK;
            s.flags &= !FLAG_PREPARED;
        }
        if !declared_read_only {
            self.writable_count = self.writable_count.saturating_sub(1);
            if self.writable_count == 0 {
                self.can_partial_clear_through = self.last_commit_seq;
            }
        } else {
            for c in self.sx(me).possible_unsafe.clone() {
                self.release_conflict(c);
            }
        }
        let read_only_now = self.sx(me).is(FLAG_READ_ONLY);
        if is_commit && !read_only_now && self.sx(me).is(FLAG_SUMMARY_CONFLICT_OUT) {
            let s = self.sx_mut(me);
            s.earliest_out_conflict_commit = FIRST_NORMAL_SEQ;
            s.flags |= FLAG_CONFLICT_OUT;
        }
        // Conflicts out to committed transactions: kept as a flag.
        for c in self.sx(me).out_conflicts.clone() {
            let w = self.conflict(c).in_;
            let (w_committed, w_prepare, w_lcbs) = {
                let x = self.sx(w);
                (
                    x.is(FLAG_COMMITTED),
                    x.prepare_seq,
                    x.last_commit_before_snapshot,
                )
            };
            if is_commit && !read_only_now && w_committed {
                let s = self.sx_mut(me);
                if s.flags & FLAG_CONFLICT_OUT == 0 || w_prepare < s.earliest_out_conflict_commit {
                    s.earliest_out_conflict_commit = w_prepare;
                }
                s.flags |= FLAG_CONFLICT_OUT;
            }
            if !is_commit || w_committed || w_lcbs >= self.last_commit_seq {
                self.release_conflict(c);
            }
        }
        for c in self.sx(me).in_conflicts.clone() {
            let r = self.conflict(c).out;
            let x = self.sx(r);
            if !is_commit || x.is(FLAG_COMMITTED) || x.is(FLAG_READ_ONLY) {
                self.release_conflict(c);
            }
        }
        if !declared_read_only {
            // Off the concurrent read-only transactions' lists, flagging
            // them unsafe when this one has a conflict out to an earlier
            // commit.
            for c in self.sx(me).possible_unsafe.clone() {
                if self.conflicts[c].is_none() {
                    continue;
                }
                let ro = self.conflict(c).in_;
                let mine = self.sx(me);
                let unsafe_ = is_commit
                    && did_write
                    && mine.is(FLAG_CONFLICT_OUT)
                    && mine.earliest_out_conflict_commit <= self.sx(ro).last_commit_before_snapshot;
                if unsafe_ {
                    self.flag_unsafe(ro);
                } else {
                    self.release_conflict(c);
                    if self.sx(ro).possible_unsafe.is_empty() {
                        self.sx_mut(ro).flags |= FLAG_RO_SAFE;
                    }
                }
            }
        }
        let mut need_to_clear = false;
        if Some(self.sx(me).snap.xmin) == self.global_xmin {
            self.global_xmin_count = self.global_xmin_count.saturating_sub(1);
            if self.global_xmin_count == 0 {
                self.new_global_xmin();
                need_to_clear = true;
            }
        }
        if is_commit {
            self.sx_mut(me).finished = true;
            self.finished_list.push_back(me);
        } else {
            self.release_one(me, false);
        }
        if need_to_clear {
            self.clear_old();
        }
    }

    /// `ReleasePredicateLocks` at the end of a transaction.
    pub(crate) fn release(&mut self, pid: i32, is_commit: bool, ro_safe: bool) {
        let next = self.next_xid;
        self.release_at(pid, is_commit, ro_safe, next);
    }

    /// `ClearOldPredicateLocks`: the finished transactions, in commit
    /// order, no in-progress one can still conflict with.
    fn clear_old(&mut self) {
        let mut i = 0;
        while i < self.finished_list.len() {
            let f = self.finished_list[i];
            let s = self.sx(f);
            if self.global_xmin.is_none_or(|x| s.finished_before <= x) {
                // Committed before any in-progress transaction's snapshot.
                self.finished_list.remove(i);
                self.sx_mut(f).finished = false;
                self.release_one(f, false);
            } else if s.commit_seq > self.have_partial_cleared_through
                && s.commit_seq <= self.can_partial_clear_through
            {
                // Every active transaction that took its snapshot before
                // this one committed is read-only.
                let seq = s.commit_seq;
                if s.is(FLAG_READ_ONLY) {
                    self.finished_list.remove(i);
                    self.sx_mut(f).finished = false;
                    self.release_one(f, false);
                } else {
                    self.release_one(f, true);
                    i += 1;
                }
                self.have_partial_cleared_through = seq;
            } else {
                break;
            }
        }
    }

    /// `ReleaseOneSerializableXact`: its locks, and (unless `partial`) its
    /// conflicts out, its xid and itself.
    fn release_one(&mut self, sx: usize, partial: bool) {
        for t in std::mem::take(&mut self.sx_mut(sx).locks) {
            if let Some(h) = self.targets.get_mut(&t) {
                h.retain(|&x| x != sx);
                if h.is_empty() {
                    self.targets.remove(&t);
                }
            }
        }
        self.sx_mut(sx).local.clear();
        if !partial {
            for c in self.sx(sx).out_conflicts.clone() {
                self.release_conflict(c);
            }
        }
        for c in self.sx(sx).in_conflicts.clone() {
            self.release_conflict(c);
        }
        if !partial {
            let s = self.sxacts[sx].take().expect("a live sxact");
            self.active.retain(|&x| x != sx);
            for c in s.possible_unsafe {
                if self.conflicts[c].is_some() {
                    self.release_conflict(c);
                }
            }
            if let Some(xid) = s.top_xid {
                self.xids.remove(&xid);
                // Its versions are no longer anyone's concern: a committed
                // one's are old enough to be seen by every snapshot that
                // remains, a rolled-back one's are dead.
                let rolled_back = s.flags & FLAG_ROLLED_BACK != 0;
                for m in self.versions.values_mut() {
                    m.retain(|_, v| {
                        if v.xmin == Some(xid) {
                            if rolled_back {
                                return false;
                            }
                            v.xmin = None;
                        }
                        if v.xmax == Some(xid) {
                            v.xmax = None;
                        }
                        v.xmin.is_some() || v.xmax.is_some()
                    });
                }
                self.versions.retain(|_, m| !m.is_empty());
            }
        }
    }

    /// Is backend `pid` in a read-only transaction known safe, waiting
    /// for one (DEFERRABLE), or neither?
    /// `(no possible unsafe conflict left, flagged unsafe)`.
    pub(crate) fn deferrable_state(&self, pid: i32) -> Option<(bool, bool)> {
        let s = self.sx(self.mine(pid)?);
        Some((s.possible_unsafe.is_empty(), s.is(FLAG_RO_UNSAFE)))
    }

    /// `GetSafeSnapshotBlockingPids`: the backends whose read/write
    /// transactions backend `pid`'s READ ONLY DEFERRABLE one waits on for
    /// a safe snapshot, in its `possibleUnsafeConflicts` order; none
    /// unless it is waiting.
    pub(crate) fn safe_snapshot_blocking_pids(&self, pid: i32) -> Vec<i32> {
        let Some(sx) = self.mine(pid) else {
            return Vec::new();
        };
        let s = self.sx(sx);
        if !s.is(FLAG_DEFERRABLE_WAITING) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for &c in &s.possible_unsafe {
            let writer = self.conflict(c).out;
            if let Some((&p, _)) = self.by_pid.iter().find(|(_, &x)| x == writer) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Mark backend `pid`'s read-only transaction as waiting for a safe
    /// snapshot (`GetSafeSnapshot`), or no longer.
    pub(crate) fn set_deferrable_waiting(&mut self, pid: i32, on: bool) {
        if let Some(sx) = self.mine(pid) {
            let s = self.sx_mut(sx);
            if on {
                s.flags |= FLAG_DEFERRABLE_WAITING;
            } else {
                s.flags &= !FLAG_DEFERRABLE_WAITING;
            }
        }
    }

    /// Does backend `pid`'s transaction still need predicate locking?
    pub(crate) fn tracking(&self, pid: i32) -> bool {
        self.mine(pid).is_some()
    }
}
