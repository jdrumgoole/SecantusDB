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
    ticks: u32,
}

thread_local! {
    static CURRENT: Cell<Option<Armed>> = const { Cell::new(None) };
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
            ticks: 0,
        };
        CURRENT.with(|c| c.set(Some(armed)));
    }
    Guard { previous }
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

/// Error if the budget is spent.
///
/// Call it from any loop whose length is driven by the size of the data. Cheap
/// enough for a per-document call: it reads the clock once every
/// [`POLL_EVERY`] invocations, and returns on a single thread-local read when
/// no deadline is armed at all.
pub fn check() -> Result<(), MaxTimeMsExpired> {
    CURRENT.with(|c| {
        let Some(mut a) = c.get() else { return Ok(()) };
        a.ticks = a.ticks.wrapping_add(1);
        let due = a.ticks % POLL_EVERY == 0;
        c.set(Some(a));
        if due && Instant::now() >= a.deadline {
            return Err(MaxTimeMsExpired);
        }
        Ok(())
    })
}

/// [`check`] without the poll interval — for coarse call sites.
///
/// Use it between pipeline STAGES or once per batch, where the call happens a
/// handful of times and skipping 63 of every 64 would mean skipping the check
/// entirely.
pub fn check_now() -> Result<(), MaxTimeMsExpired> {
    if expired() {
        return Err(MaxTimeMsExpired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The poll interval is the whole reason `check` is affordable per
    /// document, so it is pinned: an expired budget is NOT reported until the
    /// interval comes round.
    #[test]
    fn check_polls_every_poll_every_calls() {
        let _g = arm(Some(1));
        std::thread::sleep(Duration::from_millis(5));
        for i in 1..POLL_EVERY {
            assert_eq!(check(), Ok(()), "call {i} is before the first poll");
        }
        assert_eq!(check(), Err(MaxTimeMsExpired), "the Nth call polls");
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
}
