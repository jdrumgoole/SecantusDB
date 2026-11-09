//! mongod's ingress admission control (RM-355): every command is admitted
//! through one pool of tickets before it runs.
//!
//! A port of mongod 8.2.11's `IngressAdmissionController`
//! (`db/admission/ingress_admission_controller.cpp`) and of where
//! `ExecCommandDatabase::_initiateCommand` admits: after the command has
//! parsed and been authorized, under its `maxTimeMS`. A command mongod
//! subjects to the control (`isSubjectToIngressAdmissionControl`) takes a
//! ticket of normal priority, waiting for one when the pool has none free;
//! every other command, and one already holding a ticket, is admitted
//! exempt. The pool is `ingressAdmissionControllerTicketPoolSize` tickets
//! (1,000,000 by default, mongod's `DEFAULT_MAX_CONN`, so that it limits
//! nothing until it is lowered), with at most
//! `ingressAdmissionControllerMaxQueueDepth` waiting, and the whole is off
//! while `ingressAdmissionControlEnabled` is false.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bson::{Bson, Document};

use super::{AdmissionContext, Priority, Ticket, TicketHolder};
use crate::CommandError;

/// mongod's default pool size and queue depth (`DEFAULT_MAX_CONN`).
pub const DEFAULT_TICKETS: i32 = 1_000_000;

pub const ENABLED_PARAMETER: &str = "ingressAdmissionControlEnabled";
pub const POOL_SIZE_PARAMETER: &str = "ingressAdmissionControllerTicketPoolSize";
pub const QUEUE_DEPTH_PARAMETER: &str = "ingressAdmissionControllerMaxQueueDepth";

/// The server's ingress admission controller.
#[derive(Debug)]
pub struct IngressAdmissionController {
    holder: Arc<TicketHolder>,
    enabled: AtomicBool,
}

impl Default for IngressAdmissionController {
    fn default() -> Self {
        IngressAdmissionController {
            // mongod's ingress pool tracks no delinquency: it is built with
            // no delinquency callback (measured 8.2.11: a find holding its
            // ingress ticket 258 ms counted none).
            holder: Arc::new(TicketHolder::new(DEFAULT_TICKETS, DEFAULT_TICKETS, None)),
            enabled: AtomicBool::new(true),
        }
    }
}

impl IngressAdmissionController {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Apply a change to one of the controller's parameters.
    pub fn apply(&self, name: &str, value: &Bson) {
        match (name, value) {
            (ENABLED_PARAMETER, Bson::Boolean(on)) => self.enabled.store(*on, Ordering::Relaxed),
            (POOL_SIZE_PARAMETER, Bson::Int32(n)) => {
                self.holder.resize(*n, None);
            }
            (QUEUE_DEPTH_PARAMETER, Bson::Int32(n)) => self.holder.set_max_queue_depth(*n),
            _ => {}
        }
    }

    /// Admit the operation whose admissions `ctx` keeps (mongod's
    /// `admitOperation`): at normal priority when `subject`, unless it
    /// already holds a ticket, else exempt. `None` while the control is
    /// off. The ticket is the operation's until it drops.
    pub fn admit(
        &self,
        ctx: &Arc<AdmissionContext>,
        subject: bool,
        interrupted: &mut dyn FnMut() -> Result<(), CommandError>,
    ) -> Result<Option<Ticket>, CommandError> {
        if !self.enabled() {
            return Ok(None);
        }
        let _priority =
            (!subject || ctx.is_holding_ticket()).then(|| ctx.scoped_priority(Priority::Exempt));
        if let Some(ticket) = self.holder.try_acquire(ctx) {
            return Ok(Some(ticket));
        }
        self.holder.wait_for_ticket(ctx, interrupted).map(Some)
    }

    /// `serverStatus.queues.ingress`, or `None` while the control is off
    /// (mongod leaves the section out then).
    pub fn server_status(&self) -> Option<Document> {
        self.enabled().then(|| self.holder.stats())
    }
}

thread_local! {
    /// The running command's ingress ticket.
    static HELD: std::cell::RefCell<Option<Ticket>> = const { std::cell::RefCell::new(None) };
}

/// Holds the command's ingress ticket from its admission until the command
/// has finished on this thread -- its reply and its write concern's wait --
/// and returns it then, as mongod releases it at the end of `run()`.
#[must_use = "the ticket is returned when the scope drops"]
pub struct HeldScope {
    outer: Option<Ticket>,
}

impl HeldScope {
    pub fn enter() -> HeldScope {
        HeldScope {
            outer: HELD.with(|h| h.borrow_mut().take()),
        }
    }

    /// The command's ticket, held until its scope drops.
    pub fn hold(ticket: Option<Ticket>) {
        HELD.with(|h| *h.borrow_mut() = ticket);
    }
}

impl Drop for HeldScope {
    fn drop(&mut self) {
        let outer = self.outer.take();
        let mine = HELD.with(|h| std::mem::replace(&mut *h.borrow_mut(), outer));
        drop(mine);
    }
}

/// Whether mongod 8.2.11 subjects `name` to ingress admission control: the
/// commands whose invocation overrides `isSubjectToIngressAdmissionControl`
/// to say so (read from the r8.2.11 sources, and the CRUD and catalog ones
/// measured by the pool's `normalPriority` counts, 2026-11-08). Every other
/// command is admitted exempt.
pub fn subject_to_admission_control(name: &str, doc: &Document) -> bool {
    match name {
        // Not when a replica set member's `term` marks it internal.
        "find" | "getMore" => !doc.contains_key("term"),
        "aggregate" => !doc
            .get_array("pipeline")
            .is_ok_and(|stages| exempt_pipeline(stages)),
        "count" | "distinct" | "explain" | "findAndModify" | "findandmodify" | "mapReduce"
        | "mapreduce" | "insert" | "update" | "delete" | "bulkWrite" | "create" | "drop"
        | "createIndexes" | "dropIndexes" | "deleteIndexes" | "collMod" | "listCollections"
        | "listIndexes" | "renameCollection" | "dropSearchIndex" | "updateSearchIndex"
        | "listSearchIndexes" => true,
        _ => false,
    }
}

/// Whether a pipeline holds a stage that exempts its `aggregate`: a
/// `$currentOp`, here or in a sub-pipeline (mongod's
/// `LiteParsedPipeline::isExemptFromIngressAdmissionControl`).
fn exempt_pipeline(stages: &[Bson]) -> bool {
    stages.iter().any(|stage| {
        let Some(stage) = stage.as_document() else {
            return false;
        };
        stage.iter().any(|(name, spec)| match name.as_str() {
            "$currentOp" => true,
            "$lookup" | "$unionWith" => spec
                .as_document()
                .and_then(|s| s.get_array("pipeline").ok())
                .is_some_and(|p| exempt_pipeline(p)),
            "$facet" => spec.as_document().is_some_and(|facets| {
                facets
                    .values()
                    .any(|p| p.as_array().is_some_and(|p| exempt_pipeline(p)))
            }),
            _ => false,
        })
    })
}

/// The `queues` of an operation's `Slow query` line (mongod's
/// `TicketHolderQueueStats::toBson`): each pool's admissions and time
/// queued, either left out at zero. `ctx` is the operation's ingress
/// admissions, `None` for work no command admitted (an index build's
/// thread), whose line has `ingress: {}`.
pub fn line_queues(ctx: Option<&AdmissionContext>) -> Document {
    let mut ingress = Document::new();
    if let Some(ctx) = ctx {
        if ctx.admissions() > 0 {
            ingress.insert("admissions", ctx.admissions());
        }
        let queued = ctx.total_time_queued_micros();
        if queued > 0 {
            ingress.insert("totalTimeQueuedMicros", queued);
        }
    }
    let mut out = Document::new();
    out.insert("ingress", ingress);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn never() -> impl FnMut() -> Result<(), CommandError> {
        || Ok(())
    }

    #[test]
    fn a_subject_command_takes_a_normal_ticket() {
        let c = IngressAdmissionController::default();
        let ctx = Arc::new(AdmissionContext::default());
        let ticket = c.admit(&ctx, true, &mut never()).unwrap();
        let status = c.server_status().unwrap();
        assert_eq!(status.get_i32("out").unwrap(), 1);
        drop(ticket);
        let normal = c.server_status().unwrap();
        let normal = normal.get_document("normalPriority").unwrap();
        assert_eq!(normal.get_i64("finishedProcessing").unwrap(), 1);
        assert_eq!(ctx.admissions(), 1);
    }

    #[test]
    fn an_exempt_command_is_admitted_with_no_tickets_free() {
        let c = IngressAdmissionController::default();
        c.apply(POOL_SIZE_PARAMETER, &Bson::Int32(0));
        let ctx = Arc::new(AdmissionContext::default());
        let ticket = c.admit(&ctx, false, &mut never()).unwrap();
        assert!(ticket.is_some());
        assert_eq!((ctx.admissions(), ctx.exempted_admissions()), (1, 1));
        // And the priority is the operation's again afterwards.
        assert_eq!(ctx.priority(), Priority::Normal);
    }

    #[test]
    fn a_disabled_controller_admits_nothing_and_reports_nothing() {
        let c = IngressAdmissionController::default();
        c.apply(ENABLED_PARAMETER, &Bson::Boolean(false));
        let ctx = Arc::new(AdmissionContext::default());
        assert!(c.admit(&ctx, true, &mut never()).unwrap().is_none());
        assert_eq!(ctx.admissions(), 0);
        assert!(c.server_status().is_none());
    }

    #[test]
    fn mongods_subject_commands() {
        let d = doc! {};
        for name in [
            "find",
            "insert",
            "listIndexes",
            "explain",
            "drop",
            "getMore",
        ] {
            assert!(subject_to_admission_control(name, &d), "{name}");
        }
        for name in [
            "ping",
            "hello",
            "dbStats",
            "serverStatus",
            "endSessions",
            "createSearchIndexes",
            "killCursors",
        ] {
            assert!(!subject_to_admission_control(name, &d), "{name}");
        }
        assert!(!subject_to_admission_control(
            "find",
            &doc! {"find": "c", "term": 1_i64}
        ));
    }

    #[test]
    fn a_current_op_pipeline_is_exempt_wherever_it_is() {
        let agg = |pipeline: Vec<Document>| doc! {"aggregate": 1, "pipeline": pipeline};
        assert!(subject_to_admission_control(
            "aggregate",
            &agg(vec![doc! {"$match": {}}])
        ));
        assert!(!subject_to_admission_control(
            "aggregate",
            &agg(vec![doc! {"$currentOp": {}}])
        ));
        assert!(!subject_to_admission_control(
            "aggregate",
            &agg(vec![
                doc! {"$unionWith": {"coll": "c", "pipeline": [{"$currentOp": {}}]}}
            ])
        ));
        assert!(!subject_to_admission_control(
            "aggregate",
            &agg(vec![doc! {"$facet": {"a": [{"$currentOp": {}}]}}])
        ));
    }

    #[test]
    fn the_line_leaves_out_what_is_zero() {
        assert_eq!(line_queues(None), doc! {"ingress": {}});
        let ctx = AdmissionContext::default();
        assert_eq!(line_queues(Some(&ctx)), doc! {"ingress": {}});
        ctx.admissions
            .store(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(line_queues(Some(&ctx)), doc! {"ingress": {"admissions": 1}});
        ctx.total_time_queued_micros
            .store(7, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            line_queues(Some(&ctx)),
            doc! {"ingress": {"admissions": 1, "totalTimeQueuedMicros": 7_i64}}
        );
    }
}
