//! mongod's full-time diagnostic data capture (FTDC) sample, and
//! `getDiagnosticData`, which answers the latest one (RM-219).
//!
//! mongod samples once every `diagnosticDataCollectionPeriodMillis` (1000 by
//! default), on the period's boundaries (`FTDCUtil::roundTime`), and keeps
//! the most recent sample in memory: `getDiagnosticData` answers that
//! sample, never a fresh one, so two calls within one period answer the
//! same document (measured on 8.2.1 and 8.2.11, 2026-11-08). The sample is
//! one document of collectors, each stamped with when it began and ended:
//!
//! - a replica-set member: `replSetGetStatus`, the `$collStats`
//!   `storageStats` of `local.oplog.rs`, `config.transactions` and
//!   `config.image_collection`, then `serverStatus`;
//! - a standalone: `serverStatus` alone;
//! - on Linux, either one then `systemMetrics`, the machine's `/proc`
//!   counters (`system_metrics`).
//!
//! A collector that fails keeps its error in the sample (`ok: 0`, `errmsg`,
//! `code`, `codeName`), as mongod's does before its set is initiated.
//! mongod's `transportLayerStats` collector is not sampled, nor are the
//! samples written to `diagnostic.data` (RM-417).

use std::sync::Mutex;
use std::time::Duration;

use bson::{doc, Bson, DateTime, Document};

use crate::{CommandContext, CommandError, HandlerResult};

/// mongod's default `diagnosticDataCollectionPeriodMillis`.
pub const PERIOD: Duration = Duration::from_millis(1000);

/// The latest sample, shared server-wide.
#[derive(Default)]
pub struct Ftdc {
    latest: Mutex<Document>,
}

impl Ftdc {
    pub fn new() -> Self {
        Self::default()
    }

    /// The most recent sample: empty until the first is taken.
    pub fn latest(&self) -> Document {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Take a sample through `ctx` and keep it as the latest.
    pub fn collect(&self, ctx: &mut CommandContext) {
        let sample = sample(ctx);
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = sample;
    }
}

/// The next sampling time after `now_ms` (milliseconds since the epoch):
/// the next multiple of the period, as mongod's `FTDCUtil::roundTime`.
pub fn next_boundary(now_ms: i64, period: Duration) -> i64 {
    let period = (period.as_millis() as i64).max(1);
    (now_ms / period + 1) * period
}

/// A collector's result between its `start` and `end` stamps.
fn stamped(start: DateTime, result: Result<Document, CommandError>) -> Document {
    let mut out = doc! {"start": start};
    match result {
        Ok(reply) => out.extend(reply),
        Err(e) => out.extend(e.into_reply()),
    }
    out.insert("end", DateTime::now());
    out
}

/// One collection's `$collStats: {storageStats: {waitForLock: false,
/// numericOnly: true}}` document: `ns`, `host`, `localTime`, `storageStats`.
fn coll_stats(ctx: &mut CommandContext, host: &Bson, db: &str, coll: &str) -> HandlerResult {
    let storage = ctx.storage()?;
    let ns = format!("{db}.{coll}");
    if !storage.collection_exists(db, coll).unwrap_or(false) {
        return Err(CommandError::new(
            26,
            "NamespaceNotFound",
            format!(
                "Executor error during aggregate command on namespace: {ns} :: caused by :: \
                 Unable to retrieve storageStats in $collStats stage :: caused by :: \
                 Collection [{ns}] not found."
            ),
        ));
    }
    let stats = crate::stats_admin::storage_stats(storage, db, coll, 1)?;
    Ok(doc! {
        "ns": ns,
        "host": host.clone(),
        "localTime": DateTime::now(),
        "storageStats": stats,
    })
}

/// The request mongod's FTDC sends `serverStatus` (`ftdc_server.cpp`).
fn server_status_request() -> Document {
    doc! {
        "serverStatus": 1,
        "sharding": false,
        "timing": false,
        "defaultRWConcern": false,
        "transactions": {"includeLastCommitted": false},
        "mirroredReads": true,
        "oplog": true,
    }
}

/// One FTDC sample, in mongod's collector order.
fn sample(ctx: &mut CommandContext) -> Document {
    let mut out = doc! {"start": DateTime::now()};
    if ctx.replica_set_name.is_some() {
        let start = DateTime::now();
        let rs = crate::handshake::repl_set_get_status(
            &doc! {"replSetGetStatus": 1, "initialSync": 0},
            ctx,
        );
        out.insert("replSetGetStatus", stamped(start, rs));
        // `serverStatus`'s `host`, as mongod's collectors share one.
        let host = Bson::String(match &ctx.server_address {
            Some((h, p)) => format!("{h}:{p}"),
            None => "secantus".to_string(),
        });
        for (db, coll) in [
            ("local", "oplog.rs"),
            ("config", "transactions"),
            ("config", "image_collection"),
        ] {
            let start = DateTime::now();
            let stats = coll_stats(ctx, &host, db, coll);
            out.insert(format!("{db}.{coll}.stats"), stamped(start, stats));
        }
    }
    let start = DateTime::now();
    let status = crate::admin::server_status(&server_status_request(), ctx);
    out.insert("serverStatus", stamped(start, status));
    // Linux mongod's machine counters from `/proc` (RM-424); macOS mongod
    // has no such collector.
    #[cfg(target_os = "linux")]
    {
        let start = DateTime::now();
        let metrics = crate::system_metrics::collect();
        out.insert("systemMetrics", stamped(start, Ok(metrics)));
    }
    out.insert("end", DateTime::now());
    out
}

/// `getDiagnosticData`: the latest sample, `{}` before the first.
pub fn get_diagnostic_data(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let data = ctx.ftdc.as_ref().map(|f| f.latest()).unwrap_or_default();
    Ok(doc! {"data": data, "ok": 1.0})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_fall_on_the_period_boundaries() {
        assert_eq!(next_boundary(1_000, PERIOD), 2_000);
        assert_eq!(next_boundary(1_999, PERIOD), 2_000);
        assert_eq!(next_boundary(2_000, PERIOD), 3_000);
        assert_eq!(next_boundary(5, Duration::from_millis(250)), 250);
    }

    #[test]
    fn a_standalone_samples_server_status_alone() {
        let mut ctx = CommandContext::new(0);
        let s = sample(&mut ctx);
        let keys: Vec<&str> = s.keys().map(String::as_str).collect();
        let expected: &[&str] = if cfg!(target_os = "linux") {
            &["start", "serverStatus", "systemMetrics", "end"]
        } else {
            &["start", "serverStatus", "end"]
        };
        assert_eq!(keys, expected);
        let status = s.get_document("serverStatus").unwrap();
        assert_eq!(status.keys().next().map(String::as_str), Some("start"));
        assert_eq!(status.keys().last().map(String::as_str), Some("end"));
    }

    #[test]
    fn get_diagnostic_data_answers_the_latest_sample() {
        let ftdc = std::sync::Arc::new(Ftdc::new());
        let mut ctx = CommandContext::new(0);
        ctx.ftdc = Some(ftdc.clone());
        let before = get_diagnostic_data(&doc! {}, &mut ctx).unwrap();
        assert_eq!(before.get_document("data").unwrap(), &Document::new());
        ftdc.collect(&mut ctx);
        let first = get_diagnostic_data(&doc! {}, &mut ctx).unwrap();
        let again = get_diagnostic_data(&doc! {}, &mut ctx).unwrap();
        assert!(first
            .get_document("data")
            .unwrap()
            .contains_key("serverStatus"));
        assert_eq!(first, again);
    }
}
