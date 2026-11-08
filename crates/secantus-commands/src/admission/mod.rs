//! mongod's admission control: the ticket pools an operation is admitted
//! through before it runs (RM-355).
//!
//! A port of mongod 8.2.11's `TicketHolder` (`util/concurrency/
//! ticketholder.cpp`) and `AdmissionContext` (`admission_context.cpp`): a
//! pool of tickets an operation of normal priority takes, waiting in a queue
//! while none is free, and an exempt priority that bypasses the pool and is
//! only counted. Each pool keeps mongod's statistics, per priority, which
//! `serverStatus.queues` prints; each operation keeps its own admissions and
//! the time it queued, which its `Slow query` line prints.
//!
//! The pools built on it: [`ingress`], mongod's ingress admission control.
//! Execution control's read and write pools are still to come (RM-355).

pub mod ingress;

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use bson::{doc, Document};

use crate::CommandError;

/// An admission's priority (mongod's `AdmissionContext::Priority`): a
/// normal one takes a ticket from the pool, waiting for one if it must; an
/// exempt one bypasses the pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Exempt = 0,
    Normal = 1,
}

/// One operation's admissions through one pool (mongod's
/// `AdmissionContext`): how many tickets it has taken, how long it has
/// queued for them, and whether it holds one now.
#[derive(Debug)]
pub struct AdmissionContext {
    admissions: AtomicI32,
    exempted_admissions: AtomicI32,
    priority: AtomicU8,
    total_time_queued_micros: AtomicI64,
    /// When it began waiting in a queue, while it is.
    queueing_since: Mutex<Option<Instant>>,
    holding_ticket: AtomicBool,
}

impl Default for AdmissionContext {
    fn default() -> Self {
        AdmissionContext {
            admissions: AtomicI32::new(0),
            exempted_admissions: AtomicI32::new(0),
            priority: AtomicU8::new(Priority::Normal as u8),
            total_time_queued_micros: AtomicI64::new(0),
            queueing_since: Mutex::new(None),
            holding_ticket: AtomicBool::new(false),
        }
    }
}

impl AdmissionContext {
    /// The tickets this operation has taken.
    pub fn admissions(&self) -> i32 {
        self.admissions.load(Ordering::Relaxed)
    }

    /// The exempt tickets among them.
    pub fn exempted_admissions(&self) -> i32 {
        self.exempted_admissions.load(Ordering::Relaxed)
    }

    /// The time it has spent in a queue, in microseconds, not counting a
    /// wait still under way.
    pub fn total_time_queued_micros(&self) -> i64 {
        self.total_time_queued_micros.load(Ordering::Relaxed)
    }

    /// When it began waiting, if it is waiting now.
    pub fn start_queueing_time(&self) -> Option<Instant> {
        *self
            .queueing_since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_holding_ticket(&self) -> bool {
        self.holding_ticket.load(Ordering::Relaxed)
    }

    pub fn priority(&self) -> Priority {
        if self.priority.load(Ordering::Relaxed) == Priority::Exempt as u8 {
            Priority::Exempt
        } else {
            Priority::Normal
        }
    }

    /// Admit at `priority` until the scope drops (mongod's
    /// `ScopedAdmissionPriority`). An exempt operation cannot be demoted.
    pub fn scoped_priority(self: &Arc<Self>, priority: Priority) -> ScopedPriority {
        let original = self.priority();
        let applied = if original == Priority::Exempt {
            Priority::Exempt
        } else {
            priority
        };
        self.priority.store(applied as u8, Ordering::Relaxed);
        ScopedPriority {
            ctx: self.clone(),
            original,
        }
    }
}

/// Restores an operation's priority when dropped.
#[must_use = "the priority is restored when this guard drops"]
pub struct ScopedPriority {
    ctx: Arc<AdmissionContext>,
    original: Priority,
}

impl Drop for ScopedPriority {
    fn drop(&mut self) {
        self.ctx
            .priority
            .store(self.original as u8, Ordering::Relaxed);
    }
}

/// A pool's statistics for one priority (mongod's
/// `TicketHolder::QueueStats`).
#[derive(Debug, Default)]
struct QueueStats {
    added_to_queue: AtomicI64,
    removed_from_queue: AtomicI64,
    started_processing: AtomicI64,
    finished_processing: AtomicI64,
    total_time_processing_micros: AtomicI64,
    canceled: AtomicI64,
    new_admissions: AtomicI64,
    total_time_queued_micros: AtomicI64,
    total_delinquent_acquisitions: AtomicI64,
    total_acquisition_delinquency_millis: AtomicI64,
    max_acquisition_delinquency_millis: AtomicI64,
}

impl QueueStats {
    /// mongod's `_appendQueueStats`, field for field and type for type
    /// (measured 8.2.11: every count a long, but `totalDelinquentAcquisitions`
    /// an int).
    fn to_document(&self) -> Document {
        let get = |a: &AtomicI64| a.load(Ordering::Relaxed);
        let added = get(&self.added_to_queue);
        let removed = get(&self.removed_from_queue);
        let started = get(&self.started_processing);
        let finished = get(&self.finished_processing);
        doc! {
            "addedToQueue": added,
            "removedFromQueue": removed,
            "queueLength": (added - removed).max(0),
            "startedProcessing": started,
            "processing": (started - finished).max(0),
            "finishedProcessing": finished,
            "totalTimeProcessingMicros": get(&self.total_time_processing_micros),
            "canceled": get(&self.canceled),
            "newAdmissions": get(&self.new_admissions),
            "totalTimeQueuedMicros": get(&self.total_time_queued_micros),
            "totalDelinquentAcquisitions": get(&self.total_delinquent_acquisitions) as i32,
            "totalAcquisitionDelinquencyMillis": get(&self.total_acquisition_delinquency_millis),
            "maxAcquisitionDelinquencyMillis": get(&self.max_acquisition_delinquency_millis),
        }
    }
}

/// mongod's `TicketHolder` with its immediate resize policy: `outof`
/// tickets, of which `available` are free. Resizing moves both at once, so
/// `available` goes below zero when the pool shrinks under operations that
/// hold tickets, and no one is admitted until enough have been returned.
///
/// As in mongod, a ticket is taken and returned with an atomic on
/// `available` alone; only an operation that has to wait takes the
/// `sleep` lock, and a release wakes one only when one is waiting.
#[derive(Debug)]
pub struct TicketHolder {
    available: AtomicI32,
    /// What a waiter sleeps on, and `freed` wakes it.
    sleep: Mutex<()>,
    freed: Condvar,
    outof: AtomicI32,
    max_queue_depth: AtomicI32,
    /// Operations waiting for a ticket now.
    waiters: AtomicI32,
    resize_lock: Mutex<()>,
    /// An acquisition held longer than this is delinquent; `None` for a
    /// pool that does not track delinquency (mongod's ingress pool, which
    /// it constructs with no callback).
    delinquent_after: Option<Duration>,
    exempt: QueueStats,
    normal: QueueStats,
}

/// A ticket taken from a [`TicketHolder`]. Returned to it on drop, the
/// release counted in the pool's statistics.
#[must_use = "the ticket is returned when it drops"]
#[derive(Debug)]
pub struct Ticket {
    holder: Arc<TicketHolder>,
    ctx: Arc<AdmissionContext>,
    priority: Priority,
    acquired: Instant,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.holder.release(self);
    }
}

/// How long a waiter sleeps before it checks for interruption again:
/// mongod's 500 ms, jittered by up to a fifth either way so that waiters do
/// not wake together.
fn next_wake() -> Duration {
    const BASE_MS: i64 = 500;
    let offset = rand::random_range(-(BASE_MS / 5)..=(BASE_MS / 5));
    Duration::from_millis((BASE_MS + offset) as u64)
}

impl TicketHolder {
    pub fn new(tickets: i32, max_queue_depth: i32, delinquent_after: Option<Duration>) -> Self {
        TicketHolder {
            available: AtomicI32::new(tickets),
            sleep: Mutex::new(()),
            freed: Condvar::new(),
            outof: AtomicI32::new(tickets),
            max_queue_depth: AtomicI32::new(max_queue_depth),
            waiters: AtomicI32::new(0),
            resize_lock: Mutex::new(()),
            delinquent_after,
            exempt: QueueStats::default(),
            normal: QueueStats::default(),
        }
    }

    /// The tickets in the pool.
    pub fn outof(&self) -> i32 {
        self.outof.load(Ordering::Relaxed)
    }

    /// The tickets free now; below zero after a shrink.
    pub fn available(&self) -> i32 {
        self.available.load(Ordering::SeqCst)
    }

    /// The tickets taken.
    pub fn used(&self) -> i32 {
        self.outof() - self.available()
    }

    /// Operations of normal priority waiting for a ticket now.
    pub fn queued(&self) -> i64 {
        let added = self.normal.added_to_queue.load(Ordering::Relaxed);
        let removed = self.normal.removed_from_queue.load(Ordering::Relaxed);
        (added - removed).max(0)
    }

    pub fn set_max_queue_depth(&self, depth: i32) {
        self.max_queue_depth.store(depth, Ordering::Relaxed);
    }

    /// mongod's immediate resize: the pool becomes `size` tickets at once,
    /// waking the waiters when tickets have become free.
    pub fn resize(&self, size: i32) {
        let _resizing = self.resize_lock.lock().unwrap_or_else(|e| e.into_inner());
        let old = self.outof.swap(size, Ordering::Relaxed);
        let delta = size - old;
        let before = self.available.fetch_add(delta, Ordering::SeqCst);
        if before <= 0 && before + delta > 0 {
            self.wake(true);
        }
    }

    /// Wake one waiter, or all of them. Taken under the waiters' lock, so a
    /// waiter between its last look at the pool and its sleep cannot miss it.
    fn wake(&self, all: bool) {
        let _sleep = self.sleep.lock().unwrap_or_else(|e| e.into_inner());
        if all {
            self.freed.notify_all();
        } else {
            self.freed.notify_one();
        }
    }

    /// Take a ticket from the pool if one is free (mongod's
    /// `_tryAcquireNormalPriorityTicket`).
    fn take(&self) -> bool {
        let mut available = self.available.load(Ordering::SeqCst);
        loop {
            if available <= 0 {
                return false;
            }
            match self.available.compare_exchange_weak(
                available,
                available - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(now) => available = now,
            }
        }
    }

    /// Take a ticket without waiting: an exempt operation always gets one,
    /// a normal one when the pool has one free.
    pub fn try_acquire(self: &Arc<Self>, ctx: &Arc<AdmissionContext>) -> Option<Ticket> {
        let priority = ctx.priority();
        if priority == Priority::Exempt {
            self.count_acquisition(ctx, &self.exempt, priority);
            return Some(self.make_ticket(ctx, priority, false));
        }
        let ticket = self.try_acquire_normal(ctx)?;
        self.count_acquisition(ctx, &self.normal, priority);
        Some(ticket)
    }

    /// Take a ticket, waiting in the queue for one if the pool has none
    /// free. `interrupted` is asked each time the wait wakes, and an error
    /// from it ends the wait with that error, as does a queue already
    /// `max_queue_depth` long (433 AdmissionQueueOverflow).
    pub fn wait_for_ticket(
        self: &Arc<Self>,
        ctx: &Arc<AdmissionContext>,
        interrupted: &mut dyn FnMut() -> Result<(), CommandError>,
    ) -> Result<Ticket, CommandError> {
        if let Some(ticket) = self.try_acquire(ctx) {
            return Ok(ticket);
        }
        let started = Instant::now();
        self.normal.added_to_queue.fetch_add(1, Ordering::Relaxed);
        *ctx.queueing_since.lock().unwrap_or_else(|e| e.into_inner()) = Some(started);
        let waited = self.perform_wait(ctx, interrupted);
        let micros = started.elapsed().as_micros() as i64;
        *ctx.queueing_since.lock().unwrap_or_else(|e| e.into_inner()) = None;
        ctx.total_time_queued_micros
            .fetch_add(micros, Ordering::Relaxed);
        self.normal
            .total_time_queued_micros
            .fetch_add(micros, Ordering::Relaxed);
        self.normal
            .removed_from_queue
            .fetch_add(1, Ordering::Relaxed);
        match waited {
            Ok(ticket) => {
                self.count_acquisition(ctx, &self.normal, ctx.priority());
                Ok(ticket)
            }
            Err(e) => {
                self.normal.canceled.fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    fn perform_wait(
        self: &Arc<Self>,
        ctx: &Arc<AdmissionContext>,
        interrupted: &mut dyn FnMut() -> Result<(), CommandError>,
    ) -> Result<Ticket, CommandError> {
        let mut waiting = false;
        let result = (|| loop {
            if self.take() {
                return Ok(self.make_ticket(ctx, Priority::Normal, true));
            }
            if !waiting {
                let before = self.waiters.fetch_add(1, Ordering::SeqCst);
                waiting = true;
                if before >= self.max_queue_depth.load(Ordering::Relaxed) {
                    return Err(CommandError::new(
                        433,
                        "AdmissionQueueOverflow",
                        "MongoDB is overloaded and cannot accept new operations. Try again later.",
                    )
                    .with_extra(doc! { "errorLabels": ["SystemOverloadedError"] }));
                }
            }
            {
                // Counted as waiting before this last look, so a ticket
                // returned after it finds this waiter to wake.
                let sleep = self.sleep.lock().unwrap_or_else(|e| e.into_inner());
                if self.take() {
                    return Ok(self.make_ticket(ctx, Priority::Normal, true));
                }
                let (sleep, _) = self
                    .freed
                    .wait_timeout(sleep, next_wake())
                    .unwrap_or_else(|e| e.into_inner());
                drop(sleep);
            }
            interrupted()?;
        })();
        if waiting {
            self.waiters.fetch_sub(1, Ordering::SeqCst);
        }
        result
    }

    fn try_acquire_normal(self: &Arc<Self>, ctx: &Arc<AdmissionContext>) -> Option<Ticket> {
        self.take()
            .then(|| self.make_ticket(ctx, Priority::Normal, true))
    }

    /// mongod's `_makeTicket`: a ticket from the pool marks the operation
    /// as holding one; an exempt ticket does not.
    fn make_ticket(
        self: &Arc<Self>,
        ctx: &Arc<AdmissionContext>,
        priority: Priority,
        from_pool: bool,
    ) -> Ticket {
        if from_pool {
            ctx.holding_ticket.store(true, Ordering::Relaxed);
        }
        Ticket {
            holder: self.clone(),
            ctx: ctx.clone(),
            priority,
            acquired: Instant::now(),
        }
    }

    fn count_acquisition(&self, ctx: &AdmissionContext, stats: &QueueStats, priority: Priority) {
        if ctx.admissions() == 0 {
            stats.new_admissions.fetch_add(1, Ordering::Relaxed);
        }
        if priority == Priority::Exempt {
            ctx.exempted_admissions.fetch_add(1, Ordering::Relaxed);
        }
        ctx.admissions.fetch_add(1, Ordering::Relaxed);
        stats.started_processing.fetch_add(1, Ordering::Relaxed);
    }

    fn release(&self, ticket: &Ticket) {
        if ticket.priority == Priority::Exempt {
            self.count_release(&self.exempt, ticket);
            return;
        }
        ticket.ctx.holding_ticket.store(false, Ordering::Relaxed);
        self.count_release(&self.normal, ticket);
        // mongod's `_releaseNormalPriorityTicket`: the wake costs a lock, so
        // it is paid only when someone waits.
        let before = self.available.fetch_add(1, Ordering::SeqCst);
        if before >= 0 && self.waiters.load(Ordering::SeqCst) > 0 {
            self.wake(false);
        }
    }

    fn count_release(&self, stats: &QueueStats, ticket: &Ticket) {
        stats.finished_processing.fetch_add(1, Ordering::Relaxed);
        let held = ticket.acquired.elapsed();
        stats
            .total_time_processing_micros
            .fetch_add(held.as_micros() as i64, Ordering::Relaxed);
        if let Some(after) = self.delinquent_after {
            if held > after {
                let ms = held.as_millis() as i64;
                stats
                    .total_delinquent_acquisitions
                    .fetch_add(1, Ordering::Relaxed);
                stats
                    .total_acquisition_delinquency_millis
                    .fetch_add(ms, Ordering::Relaxed);
                stats
                    .max_acquisition_delinquency_millis
                    .fetch_max(ms, Ordering::Relaxed);
            }
        }
    }

    /// mongod's `TicketHolder::appendStats`.
    pub fn stats(&self) -> Document {
        let available = self.available();
        let outof = self.outof();
        doc! {
            "out": outof - available,
            "available": available,
            "totalTickets": outof,
            "exempt": self.exempt.to_document(),
            "normalPriority": self.normal.to_document(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn never() -> impl FnMut() -> Result<(), CommandError> {
        || Ok(())
    }

    fn stat(holder: &TicketHolder, priority: &str, field: &str) -> i64 {
        let stats = holder.stats();
        let section = stats.get_document(priority).unwrap();
        match section.get(field).unwrap() {
            bson::Bson::Int64(n) => *n,
            bson::Bson::Int32(n) => i64::from(*n),
            other => panic!("{field}: {other:?}"),
        }
    }

    #[test]
    fn a_normal_ticket_is_taken_from_the_pool_and_returned() {
        let holder = Arc::new(TicketHolder::new(2, 10, None));
        let ctx = Arc::new(AdmissionContext::default());
        let ticket = holder.try_acquire(&ctx).unwrap();
        assert_eq!((holder.used(), holder.available()), (1, 1));
        assert!(ctx.is_holding_ticket());
        assert_eq!(ctx.admissions(), 1);
        drop(ticket);
        assert_eq!((holder.used(), holder.available()), (0, 2));
        assert!(!ctx.is_holding_ticket());
        assert_eq!(stat(&holder, "normalPriority", "startedProcessing"), 1);
        assert_eq!(stat(&holder, "normalPriority", "finishedProcessing"), 1);
        assert_eq!(stat(&holder, "normalPriority", "newAdmissions"), 1);
    }

    #[test]
    fn an_exempt_ticket_bypasses_the_pool() {
        let holder = Arc::new(TicketHolder::new(0, 10, None));
        let ctx = Arc::new(AdmissionContext::default());
        let _exempt = ctx.scoped_priority(Priority::Exempt);
        let ticket = holder.try_acquire(&ctx).unwrap();
        assert_eq!((holder.used(), holder.available()), (0, 0));
        assert!(!ctx.is_holding_ticket());
        assert_eq!((ctx.admissions(), ctx.exempted_admissions()), (1, 1));
        drop(ticket);
        assert_eq!(stat(&holder, "exempt", "finishedProcessing"), 1);
        assert_eq!(stat(&holder, "normalPriority", "startedProcessing"), 0);
    }

    #[test]
    fn a_second_admission_of_one_operation_is_not_new() {
        let holder = Arc::new(TicketHolder::new(5, 10, None));
        let ctx = Arc::new(AdmissionContext::default());
        let first = holder.try_acquire(&ctx).unwrap();
        let exempt = ctx.scoped_priority(Priority::Exempt);
        let second = holder.try_acquire(&ctx).unwrap();
        drop((second, exempt, first));
        assert_eq!(ctx.admissions(), 2);
        assert_eq!(stat(&holder, "normalPriority", "newAdmissions"), 1);
        assert_eq!(stat(&holder, "exempt", "newAdmissions"), 0);
        assert_eq!(stat(&holder, "exempt", "startedProcessing"), 1);
    }

    #[test]
    fn an_exempt_operation_cannot_be_demoted() {
        let ctx = Arc::new(AdmissionContext::default());
        let _exempt = ctx.scoped_priority(Priority::Exempt);
        {
            let _normal = ctx.scoped_priority(Priority::Normal);
            assert_eq!(ctx.priority(), Priority::Exempt);
        }
        assert_eq!(ctx.priority(), Priority::Exempt);
    }

    #[test]
    fn a_waiter_queues_until_a_ticket_is_returned() {
        let holder = Arc::new(TicketHolder::new(1, 10, None));
        let held = holder.try_acquire(&Arc::new(AdmissionContext::default()));
        let waiter_ctx = Arc::new(AdmissionContext::default());
        let (h, c) = (holder.clone(), waiter_ctx.clone());
        let waiter = std::thread::spawn(move || h.wait_for_ticket(&c, &mut never()).map(drop));
        while holder.queued() == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(waiter_ctx.start_queueing_time().is_some());
        std::thread::sleep(Duration::from_millis(20));
        drop(held);
        waiter.join().unwrap().unwrap();
        assert_eq!(holder.queued(), 0);
        assert!(waiter_ctx.total_time_queued_micros() >= 20_000);
        assert!(waiter_ctx.start_queueing_time().is_none());
        assert_eq!(stat(&holder, "normalPriority", "addedToQueue"), 1);
        assert_eq!(stat(&holder, "normalPriority", "removedFromQueue"), 1);
        assert_eq!(stat(&holder, "normalPriority", "canceled"), 0);
        assert!(stat(&holder, "normalPriority", "totalTimeQueuedMicros") >= 20_000);
    }

    #[test]
    fn an_interrupted_waiter_is_canceled() {
        let holder = Arc::new(TicketHolder::new(0, 10, None));
        let ctx = Arc::new(AdmissionContext::default());
        let mut asked = 0;
        let mut interrupted = || {
            asked += 1;
            Err(CommandError::max_time_expired())
        };
        let err = holder.wait_for_ticket(&ctx, &mut interrupted).unwrap_err();
        assert_eq!(err.code, 50);
        assert_eq!(asked, 1);
        assert_eq!(ctx.admissions(), 0);
        assert!(ctx.total_time_queued_micros() > 0);
        assert_eq!(stat(&holder, "normalPriority", "canceled"), 1);
        assert_eq!(stat(&holder, "normalPriority", "queueLength"), 0);
    }

    #[test]
    fn a_full_queue_refuses_with_admission_queue_overflow() {
        let holder = Arc::new(TicketHolder::new(0, 0, None));
        let ctx = Arc::new(AdmissionContext::default());
        let err = holder.wait_for_ticket(&ctx, &mut never()).unwrap_err();
        assert_eq!(
            (err.code, err.code_name.as_str()),
            (433, "AdmissionQueueOverflow")
        );
        assert_eq!(stat(&holder, "normalPriority", "canceled"), 1);
        assert_eq!(holder.waiters.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn contended_waiters_never_exceed_the_pool_and_all_get_through() {
        const TICKETS: usize = 2;
        let holder = Arc::new(TicketHolder::new(TICKETS as i32, 100, None));
        let now = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (h, now, peak) = (holder.clone(), now.clone(), peak.clone());
                std::thread::spawn(move || {
                    for _ in 0..40 {
                        let ctx = Arc::new(AdmissionContext::default());
                        let _t = h.wait_for_ticket(&ctx, &mut never()).unwrap();
                        let cur = now.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(cur, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_micros(100));
                        now.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= TICKETS);
        assert_eq!((holder.available(), holder.queued()), (TICKETS as i32, 0));
        assert_eq!(stat(&holder, "normalPriority", "finishedProcessing"), 320);
    }

    #[test]
    fn growing_the_pool_admits_the_waiters() {
        let holder = Arc::new(TicketHolder::new(0, 10, None));
        let done = Arc::new(AtomicUsize::new(0));
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let (h, d) = (holder.clone(), done.clone());
                std::thread::spawn(move || {
                    let ctx = Arc::new(AdmissionContext::default());
                    let _t = h.wait_for_ticket(&ctx, &mut never()).unwrap();
                    d.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        while holder.queued() < 3 {
            std::thread::sleep(Duration::from_millis(1));
        }
        holder.resize(3);
        for w in waiters {
            w.join().unwrap();
        }
        assert_eq!(done.load(Ordering::SeqCst), 3);
        assert_eq!((holder.outof(), holder.available()), (3, 3));
    }

    #[test]
    fn shrinking_below_the_tickets_held_leaves_none_available() {
        let holder = Arc::new(TicketHolder::new(2, 10, None));
        let ctx = || Arc::new(AdmissionContext::default());
        let (a, b) = (holder.try_acquire(&ctx()), holder.try_acquire(&ctx()));
        holder.resize(1);
        assert_eq!(
            (holder.outof(), holder.available(), holder.used()),
            (1, -1, 2)
        );
        assert!(holder.try_acquire(&ctx()).is_none());
        drop((a, b));
        assert_eq!(holder.available(), 1);
    }

    #[test]
    fn the_stats_have_mongods_fields_and_types() {
        let holder = TicketHolder::new(1_000_000, 1_000_000, None);
        let stats = holder.stats();
        assert_eq!(
            stats.keys().collect::<Vec<_>>(),
            [
                "out",
                "available",
                "totalTickets",
                "exempt",
                "normalPriority"
            ]
        );
        assert_eq!(
            stats.get("totalTickets"),
            Some(&bson::Bson::Int32(1_000_000))
        );
        let normal = stats.get_document("normalPriority").unwrap();
        assert_eq!(
            normal.keys().collect::<Vec<_>>(),
            [
                "addedToQueue",
                "removedFromQueue",
                "queueLength",
                "startedProcessing",
                "processing",
                "finishedProcessing",
                "totalTimeProcessingMicros",
                "canceled",
                "newAdmissions",
                "totalTimeQueuedMicros",
                "totalDelinquentAcquisitions",
                "totalAcquisitionDelinquencyMillis",
                "maxAcquisitionDelinquencyMillis",
            ]
        );
        assert_eq!(normal.get("addedToQueue"), Some(&bson::Bson::Int64(0)));
        assert_eq!(
            normal.get("totalDelinquentAcquisitions"),
            Some(&bson::Bson::Int32(0))
        );
    }

    #[test]
    fn a_pool_with_no_delinquency_interval_counts_none() {
        let holder = Arc::new(TicketHolder::new(1, 10, None));
        let t = holder.try_acquire(&Arc::new(AdmissionContext::default()));
        std::thread::sleep(Duration::from_millis(5));
        drop(t);
        assert_eq!(
            stat(&holder, "normalPriority", "totalDelinquentAcquisitions"),
            0
        );
        let tracked = Arc::new(TicketHolder::new(1, 10, Some(Duration::from_millis(1))));
        let t = tracked.try_acquire(&Arc::new(AdmissionContext::default()));
        std::thread::sleep(Duration::from_millis(5));
        drop(t);
        assert_eq!(
            stat(&tracked, "normalPriority", "totalDelinquentAcquisitions"),
            1
        );
        assert!(
            stat(
                &tracked,
                "normalPriority",
                "maxAcquisitionDelinquencyMillis"
            ) >= 5
        );
    }
}
