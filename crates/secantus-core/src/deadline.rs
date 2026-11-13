//! Cooperative `maxTimeMS` enforcement.
//!
//! `maxTimeMS` was parsed and validated exactly as mongod validates it, and
//! then ignored: the operation ran to completion and answered `ok`. mongod
//! aborts it and answers `50 MaxTimeMSExpired`. That is invisible on a fast
//! operation, and obvious the moment the budget is small enough for the work to
//! exceed it — measured 2026-09-28, a `createIndexes` over 100,000 documents
//! with `maxTimeMS: 1` returned `ok: 1.0` here and code 50 on mongod 8.2.11.
//!
//! A real deadline cannot be a parse-time check, because the thing being
//! bounded is elapsed time *inside* the handler. It is threaded here as a
//! THREAD-LOCAL, armed by `dispatch` around the handler call and polled from
//! the loops whose length is driven by the data — the storage scan, the
//! aggregation pipeline, the index build. Thread-local rather than a parameter
//! because the alternative is a deadline argument on every function between
//! `dispatch` and a document loop, most of which have nothing to do with time;
//! one connection thread handles one command at a time, which is what makes the
//! thread-local exact.
//!
//! Cooperative means the granularity is one poll: a single predicate evaluation
//! is never interrupted, so an operation can overrun by the cost of one
//! document. mongod's own enforcement is interrupt-point-based and has the same
//! property.
//!
//! This mirrors `src/secantus/deadline.py`, deliberately — the two servers
//! should overrun in the same places for the same reasons.

use std::cell::Cell;
use std::time::{Duration, Instant};

/// How often a polling loop actually reads the clock.
///
/// `Instant::now` is cheap but not free, and a scan calls [`check`] once per
/// document — at a million documents that is a million clock reads for a budget
/// measured in milliseconds. Polling every Nth call keeps the overhead off the
/// hot path while bounding the overrun to N documents.
pub const POLL_EVERY: u32 = 64;

/// The operation outlived its `maxTimeMS` budget.
///
/// Carries mongod's message verbatim; the command layer turns it into
/// `{ok: 0, code: 50, codeName: "MaxTimeMSExpired"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxTimeMsExpired;

impl MaxTimeMsExpired {
    pub const CODE: i32 = 50;
    pub const CODE_NAME: &'static str = "MaxTimeMSExpired";
    /// mongod's wording, measured rather than invented.
    pub const MESSAGE: &'static str = "operation exceeded time limit";
}

impl std::fmt::Display for MaxTimeMsExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(Self::MESSAGE)
    }
}

impl std::error::Error for MaxTimeMsExpired {}

#[derive(Clone, Copy)]
struct Armed {
    deadline: Instant,
}

thread_local! {
    static CURRENT: Cell<Option<Armed>> = const { Cell::new(None) };
    /// The running operation's kill flag (`killOp`), read by [`check`] even
    /// with no budget armed: a kill surfaces as the same error as a spent
    /// budget, and the command layer, which knows the operation was killed,
    /// reports it as mongod's 11601.
    static INTERRUPT: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
    static INTERRUPT_TICKS: Cell<u32> = const { Cell::new(0) };
    /// Set while an expression is evaluated that no interrupt reaches.
    static OPAQUE: Cell<u32> = const { Cell::new(0) };
    /// Whether a poll has failed in the running command (see [`tripped`]):
    /// an interrupt some caller turned into a value is still its answer.
    static TRIPPED: Cell<bool> = const { Cell::new(false) };
    /// The running operation's interrupt checks (see [`check_for_interrupt`]).
    static CHECKS: Cell<i64> = const { Cell::new(0) };
    /// When the running operation last yielded, or began (see [`check`]).
    static YIELD_MARK: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The running operation's yields (see [`check`]).
    static YIELDS: Cell<i64> = const { Cell::new(0) };
    /// What a yield does besides its check, while a statement may yield
    /// (see [`yield_with`]).
    static YIELD_HOOK: std::cell::RefCell<Option<Relock>> =
        const { std::cell::RefCell::new(None) };
    /// Calls to [`check_expression`] until it next checks.
    static EXPRESSION_TICK: Cell<u32> = const { Cell::new(EXPRESSION_CHECK_PERIOD) };
    /// Whether [`check_expression`] ticked since [`take_expression_ticked`].
    static EXPRESSION_TICKED: Cell<bool> = const { Cell::new(false) };
    /// Whether a check of the running operation was made somewhere mongod
    /// makes none, or one mongod makes is not made here (see [`unmatched`]).
    static UNMATCHED: Cell<bool> = const { Cell::new(false) };
    /// Whether the running operation's pipeline counted its checks as
    /// mongod's does (see [`pipeline_counted`]).
    static PIPELINE_COUNTED: Cell<bool> = const { Cell::new(false) };
}

/// What a statement does at a yield besides its check (see [`yield_with`]).
pub type Relock = std::rc::Rc<dyn Fn() -> Result<(), MaxTimeMsExpired>>;

/// mongod's `internalQueryExecYieldPeriodMS`: a plan executor yields --
/// and checks for interrupt -- once this long has passed since it last
/// yielded (`PlanYieldPolicy`, 8.2; `internalQueryExecYieldIterations` is
/// -1 there, so time is the only trigger).
static YIELD_PERIOD_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(10);

/// mongod's `ExpressionContext::InterruptChecker::kInterruptCheckPeriod`.
const EXPRESSION_CHECK_PERIOD: u32 = 128;

/// The yield period (`internalQueryExecYieldPeriodMS`), in milliseconds.
pub fn yield_period_ms() -> i64 {
    YIELD_PERIOD_MS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the yield period, as `setParameter` does; returns the old one.
pub fn set_yield_period_ms(ms: i64) -> i64 {
    YIELD_PERIOD_MS.swap(ms, std::sync::atomic::Ordering::Relaxed)
}

/// An operation begins on this thread: no interrupt checks, no yields, and
/// its yield clock starts now.
pub fn begin_operation() {
    CHECKS.with(|c| c.set(0));
    YIELDS.with(|c| c.set(0));
    YIELD_MARK.with(|m| m.set(Some(Instant::now())));
    INTERRUPT_TICKS.with(|t| t.set(0));
    EXPRESSION_TICK.with(|t| t.set(EXPRESSION_CHECK_PERIOD));
    UNMATCHED.with(|u| u.set(false));
    PIPELINE_COUNTED.with(|u| u.set(false));
}

/// mongod's `OperationContext::checkForInterruptNoAssert`: counts one
/// check of the running operation (its line's `numInterruptChecks`), then
/// fails if it was killed or its budget is spent. mongod makes these at
/// fixed points -- dispatch, each lock it is granted, planning, a yield,
/// a cursor's check-in -- and its count is theirs (RM-360).
pub fn check_for_interrupt() -> Result<(), MaxTimeMsExpired> {
    CHECKS.with(|c| c.set(c.get() + 1));
    if interrupted() || expired() {
        return Err(trip());
    }
    Ok(())
}

/// A [`check_for_interrupt`] whose failure its caller handles: counted, and
/// `true` when the operation was killed or its budget spent, without
/// failing the command ([`tripped`]) -- mongod's check of a cursor checked
/// back in, which kills the cursor and still answers (RM-360).
pub fn check_quietly() -> bool {
    CHECKS.with(|c| c.set(c.get() + 1));
    interrupted() || expired()
}

/// Count a check its caller makes against its own clock: an admission
/// queue's waiter, which checks each time it wakes, as mongod's
/// `TicketHolder` does (RM-360).
pub fn count_check() {
    CHECKS.with(|c| c.set(c.get() + 1));
}

/// The running operation's interrupt checks so far.
pub fn interrupt_checks() -> i64 {
    CHECKS.with(Cell::get)
}

/// The running operation's yields so far (its line's `numYields`).
pub fn yields() -> i64 {
    YIELDS.with(Cell::get)
}

/// The running operation reached a check mongod makes that this server
/// does not make in the same place yet, so its count is not mongod's and
/// its line prints none (RM-360).
pub fn unmatched() {
    UNMATCHED.with(|u| u.set(true));
}

/// Whether [`unmatched`] was called since the operation began.
pub fn is_unmatched() -> bool {
    UNMATCHED.with(Cell::get)
}

/// The running operation read an aggregation pipeline whose checks are
/// made where mongod's are: one its query layer runs whole, or one whose
/// stages after its `$cursor` stage are each counted (RM-360). An
/// `aggregate`, or a `getMore` of its cursor, prints its count only then.
pub fn pipeline_counted() {
    PIPELINE_COUNTED.with(|u| u.set(true));
}

/// Whether [`pipeline_counted`] was called since the operation began.
pub fn is_pipeline_counted() -> bool {
    PIPELINE_COUNTED.with(Cell::get)
}

/// mongod's `exec::agg::Stage::getNext`, `n` times over: each call ticks
/// the pipeline's `ExpressionContext` interrupt checker, which checks the
/// operation every 128th tick (`InterruptChecker`, 8.2). It is the checker
/// [`check_expression`] ticks too, as a pipeline's `$map` ticks its
/// stages' (RM-360).
pub fn tick_stages(n: u64) -> Result<(), MaxTimeMsExpired> {
    let mut left = n;
    while left > 0 {
        let tick = EXPRESSION_TICK.with(Cell::get) as u64;
        if left < tick {
            EXPRESSION_TICK.with(|t| t.set((tick - left) as u32));
            return Ok(());
        }
        left -= tick;
        EXPRESSION_TICK.with(|t| t.set(EXPRESSION_CHECK_PERIOD));
        check_for_interrupt()?;
    }
    Ok(())
}

/// The pipeline interrupt checker's ticks to its next check, which a cursor
/// keeps between its batches, as mongod's `ExpressionContext` lives with
/// the cursor's pipeline (see [`resume_ticks`]).
pub fn ticks_to_check() -> u32 {
    EXPRESSION_TICK.with(Cell::get)
}

/// Resume a cursor's pipeline interrupt checker where its last batch left
/// it ([`ticks_to_check`]).
pub fn resume_ticks(ticks: u32) {
    EXPRESSION_TICK.with(|t| t.set(ticks.clamp(1, EXPRESSION_CHECK_PERIOD)));
}

/// Whether an expression ticked the interrupt checker ([`check_expression`])
/// since the last call, which resets it: mongod 8.2.11 evaluates `$map`,
/// `$filter` and `$reduce` without a tick, so a pipeline count that took one
/// is not mongod's (RM-360).
pub fn take_expression_ticked() -> bool {
    EXPRESSION_TICKED.with(|t| t.replace(false))
}

/// A fresh pipeline's interrupt checker: its first check is 128 ticks away.
pub const FRESH_TICKS: u32 = EXPRESSION_CHECK_PERIOD;

/// Restores the previous yield behaviour when dropped.
#[must_use = "the statement stops yielding when this guard drops"]
pub struct YieldGuard(Option<Relock>);

impl Drop for YieldGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        YIELD_HOOK.with(|h| *h.borrow_mut() = previous);
    }
}

/// Let the running statement yield: at each yield [`check`] runs `relock`
/// after its check -- mongod releases the statement's locks and takes them
/// again, and each grant checks for interrupt too. Without one (a
/// statement inside a transaction, whose locks cannot be released) a yield
/// is mongod's `INTERRUPT_ONLY` policy: the check alone, and no yield
/// counted.
pub fn yield_with(relock: Relock) -> YieldGuard {
    YieldGuard(YIELD_HOOK.with(|h| h.borrow_mut().replace(relock)))
}

/// No yields until the guard drops: a statement that cannot release its
/// locks, mongod's `INTERRUPT_ONLY`.
pub fn no_yields() -> YieldGuard {
    YieldGuard(YIELD_HOOK.with(|h| h.borrow_mut().take()))
}

/// Whether one of this command's polls has failed -- a kill or a spent
/// budget -- since its [`trip_scope`] began, even if the caller that saw
/// it carried on: a filter that reads an error as a non-match, say. mongod
/// fails the operation at its interrupt point; this is how the command layer
/// learns of one it would otherwise have answered `ok` (RM-313).
pub fn tripped() -> bool {
    TRIPPED.with(Cell::get)
}

fn trip() -> MaxTimeMsExpired {
    TRIPPED.with(|t| t.set(true));
    MaxTimeMsExpired
}

/// Restores interrupt checks in expressions when dropped.
#[must_use = "expressions are interruptible again when this guard drops"]
pub struct OpaqueGuard(());

impl Drop for OpaqueGuard {
    fn drop(&mut self) {
        OPAQUE.with(|o| o.set(o.get().saturating_sub(1)));
    }
}

/// Evaluate expressions that neither `killOp` nor `maxTimeMS` interrupts
/// until the guard drops: a collection validator's. mongod parses a
/// validator once, with no operation to check, so an insert killed while
/// its validator runs still inserts, and the kill reaches it only when it
/// waits for its write concern (measured 8.2.11, 2026-10-27: a 300-document
/// insert through a slow `$expr` validator, killed after 0.9 s, inserted all
/// 300). Here it also keeps a check that treats an error as passing from
/// reading an interrupt as a document that passes.
pub fn opaque() -> OpaqueGuard {
    OPAQUE.with(|o| o.set(o.get() + 1));
    OpaqueGuard(())
}

/// The check from inside an expression: one element of a `$map`, `$filter`
/// or `$reduce`, where a single evaluation can run for seconds. It is
/// mongod's `ExpressionContext::checkForInterrupt`, which checks the
/// operation every 128th call (`InterruptChecker`, 8.2), except under
/// [`opaque`].
pub fn check_expression() -> Result<(), MaxTimeMsExpired> {
    if OPAQUE.with(Cell::get) > 0 {
        return Ok(());
    }
    EXPRESSION_TICKED.with(|t| t.set(true));
    let due = EXPRESSION_TICK.with(|t| {
        let n = t.get() - 1;
        if n == 0 {
            t.set(EXPRESSION_CHECK_PERIOD);
            true
        } else {
            t.set(n);
            false
        }
    });
    if due {
        check_for_interrupt()
    } else {
        Ok(())
    }
}

/// Restores the previous kill flag when dropped.
#[must_use = "the kill flag is uninstalled when this guard drops"]
pub struct InterruptGuard {
    previous: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        INTERRUPT.with(|c| *c.borrow_mut() = previous);
    }
}

/// Install the running operation's kill flag on this thread.
pub fn watch_interrupt(flag: std::sync::Arc<std::sync::atomic::AtomicBool>) -> InterruptGuard {
    let previous = INTERRUPT.with(|c| c.borrow_mut().replace(flag));
    InterruptGuard { previous }
}

/// Scopes [`tripped`] to one command: cleared when made, and on drop the
/// enclosing command's flag again, with this one's failed poll added -- an
/// inner command's interrupt is the outer one's too.
#[must_use = "the scope ends when this guard drops"]
pub struct TripScope {
    outer: bool,
}

impl Drop for TripScope {
    fn drop(&mut self) {
        let outer = self.outer;
        TRIPPED.with(|t| t.set(outer || t.get()));
    }
}

/// Start counting failed polls for one command (see [`tripped`]).
pub fn trip_scope() -> TripScope {
    TripScope {
        outer: TRIPPED.with(|t| t.replace(false)),
    }
}

/// Whether the running operation has been killed.
pub fn interrupted() -> bool {
    INTERRUPT.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Acquire))
    })
}

/// Restores the previous budget when dropped, so a handler that dispatches an
/// inner command cannot silently widen its own limit.
#[must_use = "the deadline is disarmed when this guard drops"]
pub struct Guard {
    previous: Option<Armed>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let previous = self.previous;
        CURRENT.with(|c| c.set(previous));
    }
}

/// Arm this thread's deadline for `max_time_ms`.
///
/// `None` or a non-positive value means "no limit" — mongod's own encoding of
/// an absent budget — and arms nothing, so the polling loops keep their fast
/// path when the caller did not ask for a timeout.
pub fn arm(max_time_ms: Option<i64>) -> Guard {
    let previous = CURRENT.with(|c| c.get());
    if let Some(ms) = max_time_ms.filter(|ms| *ms > 0) {
        let armed = Armed {
            deadline: Instant::now() + Duration::from_millis(ms as u64),
        };
        CURRENT.with(|c| c.set(Some(armed)));
    }
    Guard { previous }
}

/// [`arm`], the budget counted from `since` rather than from now: an
/// operation's `maxTimeMS` runs from where mongod starts it, before the
/// operation waits for admission, so the time it queued is spent from it.
pub fn arm_since(since: Instant, max_time_ms: Option<i64>) -> Guard {
    let previous = CURRENT.with(|c| c.get());
    if let Some(ms) = max_time_ms.filter(|ms| *ms > 0) {
        let armed = Armed {
            deadline: since + Duration::from_millis(ms as u64),
        };
        CURRENT.with(|c| c.set(Some(armed)));
    }
    Guard { previous }
}

/// Neither the budget nor the kill flag, until the guard drops: for work an
/// operation must finish whatever happened to it -- the abort of an index
/// build its own interruption caused (RM-187).
pub fn suspend() -> (Guard, InterruptGuard) {
    let budget = Guard {
        previous: CURRENT.with(|c| c.replace(None)),
    };
    let interrupt = InterruptGuard {
        previous: INTERRUPT.with(|c| c.borrow_mut().take()),
    };
    (budget, interrupt)
}

/// Has this thread's deadline passed? `false` when nothing is armed.
pub fn expired() -> bool {
    CURRENT.with(|c| match c.get() {
        Some(a) => Instant::now() >= a.deadline,
        None => false,
    })
}

/// Milliseconds left on this thread's budget, or `None` if unarmed.
pub fn remaining_ms() -> Option<f64> {
    CURRENT.with(|c| {
        c.get().map(|a| {
            a.deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64()
                * 1000.0
        })
    })
}

/// The plan executor's yield point, called from every loop whose length is
/// driven by the data -- the storage scans, the pipeline, the sorter, the
/// index build.
///
/// It is mongod's `PlanYieldPolicy::shouldYieldOrInterrupt`: once
/// [`yield_period_ms`] (`internalQueryExecYieldPeriodMS`, 10 ms) has passed
/// since the operation began or last yielded, the operation yields, and
/// the yield is where it checks for interrupt ([`check_for_interrupt`]) --
/// mongod checks a read for `killOp` and `maxTimeMS` nowhere else while it
/// runs (RM-360). A yield also counts in `numYields` and takes the
/// statement's locks again ([`yield_with`]). The clock is read every
/// [`POLL_EVERY`] calls, so the poll costs a thread-local increment.
pub fn check() -> Result<(), MaxTimeMsExpired> {
    let tick = INTERRUPT_TICKS.with(|t| {
        let n = t.get().wrapping_add(1);
        t.set(n);
        n
    });
    if !tick.is_multiple_of(POLL_EVERY) {
        return Ok(());
    }
    yield_point().map(|_| ())
}

/// [`check`] at one of a plan executor's own work boundaries, where mongod
/// asks `shouldYieldOrInterrupt` before every `work()` call: the clock is
/// read on every call, not every [`POLL_EVERY`]th. True when the statement
/// yielded -- released and retook its locks, so that what it reads next is
/// read at a new snapshot -- and false when it did not, or could not (no
/// statement locks: [`no_yields`]).
pub fn yield_point() -> Result<bool, MaxTimeMsExpired> {
    let period = yield_period_ms();
    if period < 0 {
        return Ok(false);
    }
    let now = Instant::now();
    let due = YIELD_MARK.with(|m| match m.get() {
        Some(last) if now.duration_since(last) >= Duration::from_millis(period as u64) => {
            m.set(Some(now));
            true
        }
        Some(_) => false,
        None => {
            m.set(Some(now));
            false
        }
    });
    if !due {
        return Ok(false);
    }
    check_for_interrupt()?;
    let relock = YIELD_HOOK.with(|h| h.borrow().clone());
    if let Some(relock) = relock {
        YIELDS.with(|y| y.set(y.get() + 1));
        relock()?;
        return Ok(true);
    }
    Ok(false)
}

/// [`check`] without the poll interval — for coarse call sites.
///
/// Use it between pipeline STAGES or once per batch, where the call happens a
/// handful of times and skipping 63 of every 64 would mean skipping the check
/// entirely.
pub fn check_now() -> Result<(), MaxTimeMsExpired> {
    if expired() || interrupted() {
        return Err(trip());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A budget armed from a moment already past has that much less left.
    #[test]
    fn a_budget_armed_since_a_past_moment_is_partly_spent() {
        let since = Instant::now() - Duration::from_millis(50);
        let _g = arm_since(since, Some(60));
        let left = remaining_ms().unwrap();
        assert!(left <= 10.0, "{left}");
        let _g = arm_since(since, Some(40));
        assert!(expired());
    }

    /// Nothing armed must cost nothing and never fire — the fast path every
    /// command without `maxTimeMS` takes.
    #[test]
    fn unarmed_never_expires() {
        assert!(!expired());
        assert_eq!(remaining_ms(), None);
        for _ in 0..POLL_EVERY * 4 {
            check().expect("unarmed check must not error");
        }
        check_now().expect("unarmed check_now must not error");
    }

    #[test]
    fn a_zero_or_negative_budget_arms_nothing() {
        for ms in [Some(0), Some(-1), None] {
            let _g = arm(ms);
            assert!(!expired(), "{ms:?} must mean no limit");
            assert_eq!(remaining_ms(), None);
        }
    }

    #[test]
    fn an_armed_budget_reports_remaining_and_then_expires() {
        let _g = arm(Some(50));
        assert!(remaining_ms().is_some_and(|r| r > 0.0 && r <= 50.0));
        assert!(!expired());
        std::thread::sleep(Duration::from_millis(70));
        assert!(expired());
        assert_eq!(check_now(), Err(MaxTimeMsExpired));
    }

    /// `check` is mongod's yield point: a spent budget is reported only at
    /// a yield, once the yield period has passed since the operation began,
    /// and only on a call that reads the clock (every `POLL_EVERY`th).
    #[test]
    fn check_reports_a_spent_budget_only_at_a_yield() {
        begin_operation();
        let _g = arm(Some(1));
        std::thread::sleep(Duration::from_millis(5));
        // The yield clock is set, not slept against: a 5ms sleep on a loaded
        // machine overran the 10ms period and the first poll yielded
        // (2026-11-09, load 10-17).
        let period = Duration::from_millis(yield_period_ms() as u64);
        YIELD_MARK.with(|m| m.set(Some(Instant::now() + Duration::from_secs(3600))));
        for _ in 0..POLL_EVERY * 4 {
            assert_eq!(check(), Ok(()), "no yield before the period passes");
        }
        assert_eq!(interrupt_checks(), 0);
        YIELD_MARK.with(|m| m.set(Some(Instant::now() - period - Duration::from_millis(2))));
        let failed = (0..POLL_EVERY).position(|_| check().is_err());
        assert!(
            failed.is_some(),
            "the first poll past the period yields and checks"
        );
        assert_eq!(interrupt_checks(), 1, "a yield checks once");
        assert_eq!(
            yields(),
            0,
            "with no statement locks to retake, nothing is yielded"
        );
    }

    /// A statement that can yield counts the yield and retakes its locks,
    /// each grant a check of its own.
    #[test]
    fn a_yield_retakes_the_statements_locks() {
        begin_operation();
        let relocks = std::rc::Rc::new(Cell::new(0));
        let seen = relocks.clone();
        let _y = yield_with(std::rc::Rc::new(move || {
            seen.set(seen.get() + 1);
            check_for_interrupt()
        }));
        std::thread::sleep(Duration::from_millis(yield_period_ms() as u64 + 2));
        for _ in 0..POLL_EVERY {
            check().unwrap();
        }
        assert_eq!((yields(), relocks.get(), interrupt_checks()), (1, 1, 2));
    }

    /// A work boundary reads the clock on every call, not every
    /// `POLL_EVERY`th, and says whether it yielded: a batched delete reads
    /// what it staged before a yield again (RM-501).
    #[test]
    fn a_work_boundary_yields_on_its_first_call_past_the_period() {
        begin_operation();
        let period = Duration::from_millis(yield_period_ms() as u64);
        let _y = yield_with(std::rc::Rc::new(check_for_interrupt));
        YIELD_MARK.with(|m| m.set(Some(Instant::now() + Duration::from_secs(3600))));
        assert_eq!(yield_point(), Ok(false), "no yield before the period");
        YIELD_MARK.with(|m| m.set(Some(Instant::now() - period - Duration::from_millis(2))));
        assert_eq!(yield_point(), Ok(true), "the first call past it yields");
        assert_eq!(yields(), 1);
        // Without statement locks to release there is no yield to report.
        let _none = no_yields();
        YIELD_MARK.with(|m| m.set(Some(Instant::now() - period - Duration::from_millis(2))));
        assert_eq!(yield_point(), Ok(false));
        assert_eq!(yields(), 1);
    }

    /// A nested command must not be able to widen the budget it runs under,
    /// and the outer budget must survive the inner one.
    #[test]
    fn nesting_restores_the_outer_budget() {
        let _outer = arm(Some(10_000));
        let outer_left = remaining_ms().unwrap();
        {
            let _inner = arm(Some(5));
            assert!(remaining_ms().unwrap() <= 5.0, "inner narrows");
        }
        let restored = remaining_ms().unwrap();
        assert!(
            restored > 5.0 && restored <= outer_left,
            "outer budget restored, got {restored}"
        );
    }

    /// An inner `arm(None)` must leave the outer budget in force rather than
    /// clearing it — otherwise a handler could escape its limit by dispatching
    /// an untimed sub-command.
    #[test]
    fn an_unarmed_inner_scope_does_not_clear_the_outer_budget() {
        let _outer = arm(Some(10_000));
        {
            let _inner = arm(None);
            assert!(
                remaining_ms().is_some(),
                "an untimed inner command must not escape the outer budget"
            );
        }
        assert!(remaining_ms().is_some());
    }

    /// The thread-local is what makes this exact; one thread's budget must be
    /// invisible to another.
    #[test]
    fn the_budget_is_per_thread() {
        let _g = arm(Some(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(expired());
        let other = std::thread::spawn(expired).join().unwrap();
        assert!(!other, "another thread must not see this thread's deadline");
    }

    fn killed() -> (
        InterruptGuard,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        (watch_interrupt(flag.clone()), flag)
    }

    /// A kill is read by an expression's poll within one poll interval,
    /// and not at all under `opaque` -- a collection validator's evaluation.
    #[test]
    fn an_expression_reads_the_kill_unless_opaque() {
        let (_g, flag) = killed();
        flag.store(true, std::sync::atomic::Ordering::Release);
        {
            let _o = opaque();
            for _ in 0..POLL_EVERY * 2 {
                assert_eq!(check_expression(), Ok(()), "a validator is not interrupted");
            }
        }
        let failed = (0..EXPRESSION_CHECK_PERIOD).any(|_| check_expression().is_err());
        assert!(failed, "the kill is read within mongod's 128 calls");
    }

    /// A failed poll is remembered for the command, even when its caller
    /// carried on, and a nested command's failure is the outer one's.
    #[test]
    fn a_failed_poll_trips_the_command() {
        let outer = trip_scope();
        assert!(!tripped());
        {
            let _inner = trip_scope();
            let (_g, flag) = killed();
            flag.store(true, std::sync::atomic::Ordering::Release);
            assert_eq!(check_now(), Err(MaxTimeMsExpired));
            assert!(tripped());
        }
        assert!(tripped(), "the inner command's interrupt reaches the outer");
        drop(outer);
        let fresh = trip_scope();
        assert!(!tripped(), "a new command starts untripped");
        drop(fresh);
    }

    /// The kill flag alone, with no budget armed, fails `check_now`.
    #[test]
    fn a_kill_fails_check_now_without_a_budget() {
        let _scope = trip_scope();
        let (_g, flag) = killed();
        assert_eq!(check_now(), Ok(()));
        flag.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(check_now(), Err(MaxTimeMsExpired));
    }

    /// A pipeline's stage calls check the operation every 128th call, on
    /// the checker an expression ticks too, and a cursor takes the
    /// checker's place with it from batch to batch (RM-360).
    #[test]
    fn stage_calls_check_every_128th_and_carry_over() {
        begin_operation();
        tick_stages(127).unwrap();
        assert_eq!(interrupt_checks(), 0);
        tick_stages(1).unwrap();
        assert_eq!(interrupt_checks(), 1);
        tick_stages(2 * 128 + 5).unwrap();
        assert_eq!(interrupt_checks(), 3);
        let kept = ticks_to_check();
        assert_eq!(kept, 128 - 5);
        // The next batch is another operation: its checker resumes.
        begin_operation();
        assert_eq!(ticks_to_check(), FRESH_TICKS);
        resume_ticks(kept);
        tick_stages(u64::from(kept) - 1).unwrap();
        assert_eq!(interrupt_checks(), 0);
        check_expression().unwrap();
        assert_eq!(
            interrupt_checks(),
            1,
            "an expression ticks the same checker"
        );
        assert!(take_expression_ticked());
        assert!(!take_expression_ticked(), "taking it resets it");
    }

    /// The flag a counted pipeline sets lasts for its operation only.
    #[test]
    fn a_counted_pipeline_is_the_operations_alone() {
        begin_operation();
        assert!(!is_pipeline_counted());
        pipeline_counted();
        assert!(is_pipeline_counted());
        begin_operation();
        assert!(!is_pipeline_counted());
    }
}
