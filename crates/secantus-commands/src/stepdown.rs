//! `replSetStepDown` — a single-node replica set's step-down window.
//!
//! SecantusDB advertises itself as a single-node replica-set primary, and
//! already answers `replSetGetStatus` with a full status, so it cannot honestly
//! answer `replSetStepDown` with a standalone's `76 NoReplicationEnabled`
//! ("not running with --replSet") — that would contradict its own handshake.
//! What it CAN do is reproduce what a real single-node replica set does, which
//! was measured against mongod 8.2.11 on 2026-09-29:
//!
//! | request | answer |
//! | --- | --- |
//! | `{replSetStepDown: N, force: true}` on a primary | `{ok: 1}`, then SECONDARY for N seconds |
//! | ... during that window | `hello` reports `isWritablePrimary: false, secondary: true`; WRITES fail `10107 "not primary"`; READS still work |
//! | ... after it | primary again |
//! | `{replSetStepDown: N}` with `N <= secondaryCatchUpPeriodSecs` (default 10) | 2 `BadValue` "stepdown period must be longer than secondaryCatchUpPeriodSecs" |
//! | `{replSetStepDown: N}` otherwise | 262 "No electable secondaries caught up as of \<ts\>" |
//! | a negative period | 2 `BadValue` "stepdown period must be a positive integer" |
//! | when already a secondary | 10107 "not primary so can't step down" |
//!
//! The 262 is the honest one and is why a non-forced step-down is not a
//! special case here: a single-node set genuinely has no electable secondary
//! to hand over to, so mongod's own answer is already ours.
//!
//! **What is deliberately NOT reproduced: election TIMING.** mongod's return to
//! primary is driven by its election machinery, not by the step-down period
//! alone — measured, a period of 5 came back after ~5s but a period of 0 came
//! back after ~19s, because the node must still win an election. Modelling that
//! means modelling election timeouts, which is the multi-node machinery this
//! project puts explicitly out of scope. Here the window is exactly the
//! requested period. A client that measures how long the server stays secondary
//! after a ZERO-second forced step-down will see a difference; every other
//! observable in the table above matches.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bson::{DateTime, Document};

use crate::{CommandContext, CommandError, HandlerResult};

/// mongod's default `secondaryCatchUpPeriodSecs`.
const DEFAULT_CATCH_UP_SECS: i64 = 10;

/// mongod's code for "this node is not the primary".
pub const NOT_PRIMARY: i32 = 10107;

/// Server-wide step-down window. `None` (or an elapsed instant) means primary.
#[derive(Default)]
pub struct StepDownState {
    until: Mutex<Option<Instant>>,
    /// `topologyVersion.counter`, bumped on every step-down. A driver IGNORES
    /// a "not primary" error whose topologyVersion is not newer than the one it
    /// holds, so a counter frozen at 0 makes the error look stale and the
    /// driver never learns this node stepped down.
    counter: AtomicI64,
}

impl StepDownState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this node is currently a secondary because it stepped down.
    pub fn is_stepped_down(&self) -> bool {
        match self.until.lock() {
            Ok(guard) => guard.is_some_and(|deadline| Instant::now() < deadline),
            // A poisoned lock must not silently promote a secondary back to
            // primary: report the safe answer and let the write be refused.
            Err(poisoned) => poisoned
                .into_inner()
                .is_some_and(|deadline| Instant::now() < deadline),
        }
    }

    /// The current `topologyVersion.counter`.
    pub fn topology_counter(&self) -> i64 {
        self.counter.load(Ordering::SeqCst)
    }

    fn step_down_for(&self, period: Duration) {
        if let Ok(mut guard) = self.until.lock() {
            *guard = Some(Instant::now() + period);
        }
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

/// The error a write gets while this node is stepped down.
///
/// It carries `topologyVersion`, which mongod includes and drivers require:
/// without it the driver marks the server UNKNOWN and clears the pool, so the
/// next READ fails server selection even though a secondary can serve it.
/// Measured 2026-09-29 -- mongod's 10107 reply has the field, ours did not, and
/// a read straight after a refused write failed here 3 times out of 3 while
/// succeeding against mongod.
pub fn not_primary_error(state: Option<&StepDownState>) -> CommandError {
    let counter = state.map_or(0, |s| s.topology_counter());
    CommandError::new(NOT_PRIMARY, "NotWritablePrimary", "not primary").with_extra(bson::doc! {
        "topologyVersion": {
            "processId": crate::handshake::hello_process_id(),
            "counter": counter,
        },
    })
}

/// `replSetStepDown`.
pub fn replset_step_down(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // A server that is NOT advertising a replica set is a standalone, and
    // mongod answers exactly this there.
    if ctx.replica_set_name.is_none() {
        return Err(CommandError::new(
            76,
            "NoReplicationEnabled",
            "not running with --replSet",
        ));
    }
    let state = ctx.step_down.clone();
    if state.as_ref().is_some_and(|s| s.is_stepped_down()) {
        return Err(CommandError::new(
            NOT_PRIMARY,
            "NotWritablePrimary",
            "not primary so can't step down",
        ));
    }

    // mongod accepts a non-numeric or missing period under `force` (measured:
    // a string, a null and a double are all OK), so only a value that reads as
    // a NEGATIVE number is refused.
    let period_secs = crate::util::as_i64(doc.get("replSetStepDown").unwrap_or(&bson::Bson::Null));
    if period_secs.is_some_and(|n| n < 0) {
        return Err(CommandError::new(
            2,
            "BadValue",
            "stepdown period must be a positive integer",
        ));
    }
    let period = period_secs.unwrap_or(0).max(0);

    let forced = doc.get_bool("force").unwrap_or(false);
    if !forced {
        let catch_up = doc
            .get("secondaryCatchUpPeriodSecs")
            .and_then(crate::util::as_i64)
            .unwrap_or(DEFAULT_CATCH_UP_SECS);
        if period <= catch_up {
            return Err(CommandError::new(
                2,
                "BadValue",
                "stepdown period must be longer than secondaryCatchUpPeriodSecs",
            ));
        }
        // Past validation, a non-forced step-down needs a secondary to hand
        // over to. A single-node set has none -- and neither do we, so this is
        // mongod's own answer rather than a stand-in for one.
        let now = DateTime::now().try_to_rfc3339_string().unwrap_or_default();
        return Err(CommandError::new(
            262,
            "Location262",
            format!("No electable secondaries caught up as of {now}"),
        ));
    }

    if let Some(s) = state.as_ref() {
        s.step_down_for(Duration::from_secs(period as u64));
    }
    Ok(bson::doc! { "ok": 1.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;
    use std::sync::Arc;

    fn ctx(replica_set: bool) -> CommandContext {
        let mut c = CommandContext::new(1);
        c.db_name = "admin".to_string();
        c.replica_set_name = replica_set.then(|| "secantus".to_string());
        c.step_down = Some(Arc::new(StepDownState::new()));
        c
    }

    #[test]
    fn a_standalone_answers_no_replication_enabled() {
        let e = replset_step_down(&doc! {"replSetStepDown": 5_i32}, &mut ctx(false)).unwrap_err();
        assert_eq!(e.code, 76);
        assert_eq!(e.errmsg, "not running with --replSet");
    }

    #[test]
    fn forced_step_down_makes_this_node_a_secondary() {
        let mut c = ctx(true);
        assert!(!c.step_down.as_ref().unwrap().is_stepped_down());
        replset_step_down(&doc! {"replSetStepDown": 30_i32, "force": true}, &mut c).unwrap();
        assert!(c.step_down.as_ref().unwrap().is_stepped_down());
    }

    #[test]
    fn a_zero_second_window_is_already_over() {
        let mut c = ctx(true);
        replset_step_down(&doc! {"replSetStepDown": 0_i32, "force": true}, &mut c).unwrap();
        assert!(!c.step_down.as_ref().unwrap().is_stepped_down());
    }

    #[test]
    fn stepping_down_twice_is_refused_while_secondary() {
        let mut c = ctx(true);
        replset_step_down(&doc! {"replSetStepDown": 30_i32, "force": true}, &mut c).unwrap();
        let e = replset_step_down(&doc! {"replSetStepDown": 30_i32, "force": true}, &mut c)
            .unwrap_err();
        assert_eq!(e.code, NOT_PRIMARY);
        assert_eq!(e.errmsg, "not primary so can't step down");
    }

    #[test]
    fn a_negative_period_is_refused() {
        let e = replset_step_down(
            &doc! {"replSetStepDown": -1_i32, "force": true},
            &mut ctx(true),
        )
        .unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(e.errmsg, "stepdown period must be a positive integer");
    }

    #[test]
    fn a_non_numeric_period_is_accepted_under_force() {
        // Measured: mongod takes a string, a null and a double here.
        for value in [bson::Bson::String("x".into()), bson::Bson::Null] {
            let mut c = ctx(true);
            replset_step_down(&doc! {"replSetStepDown": value, "force": true}, &mut c).unwrap();
        }
    }

    #[test]
    fn unforced_step_down_shorter_than_the_catch_up_period_is_bad_value() {
        let e = replset_step_down(&doc! {"replSetStepDown": 5_i32}, &mut ctx(true)).unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(
            e.errmsg,
            "stepdown period must be longer than secondaryCatchUpPeriodSecs"
        );
    }

    #[test]
    fn unforced_step_down_has_no_secondary_to_hand_over_to() {
        let e = replset_step_down(&doc! {"replSetStepDown": 60_i32}, &mut ctx(true)).unwrap_err();
        assert_eq!(e.code, 262);
        assert!(
            e.errmsg
                .starts_with("No electable secondaries caught up as of "),
            "{}",
            e.errmsg
        );
    }

    #[test]
    fn the_catch_up_period_can_be_overridden() {
        // period 5 > catchUp 1, so validation passes and we reach the 262.
        let e = replset_step_down(
            &doc! {"replSetStepDown": 5_i32, "secondaryCatchUpPeriodSecs": 1_i32},
            &mut ctx(true),
        )
        .unwrap_err();
        assert_eq!(e.code, 262);
    }
}
