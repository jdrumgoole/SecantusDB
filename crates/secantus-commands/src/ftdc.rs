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
//! Then `transportLayerStats`, the transport layer's own counters: each
//! listener's socket backlog depth and the connections discarded because
//! their client left.
//!
//! A collector that fails keeps its error in the sample (`ok: 0`, `errmsg`,
//! `code`, `codeName`), as mongod's does before its set is initiated.
//!
//! `diagnosticDataCollectionEnabled` and `diagnosticDataCollectionPeriodMillis`
//! switch the sampling and set its period, as on mongod: a new period takes
//! effect at its next boundary, and while sampling is off `getDiagnosticData`
//! keeps answering the last sample. Each sample is also written to
//! `<dbpath>/diagnostic.data` in mongod's file format (`crate::ftdc_file`,
//! RM-417), every file opening with the metadata collectors: `buildInfo`,
//! `getCmdLineOpts` and `hostInfo`.

use std::sync::Mutex;
use std::time::Duration;

use bson::{doc, Bson, DateTime, Document};

use crate::{CommandContext, CommandError, HandlerResult};

/// mongod's default `diagnosticDataCollectionPeriodMillis`.
pub const PERIOD: Duration = Duration::from_millis(1000);

/// The parameter that switches sampling on and off.
pub const ENABLED_PARAMETER: &str = "diagnosticDataCollectionEnabled";

/// The parameter that sets the sampling period, in milliseconds.
pub const PERIOD_PARAMETER: &str = "diagnosticDataCollectionPeriodMillis";

/// Whether to sample, and how often, as the server's parameters say now.
pub fn schedule(params: &crate::params::ServerParams) -> (bool, Duration) {
    let period = params.number(PERIOD_PARAMETER, PERIOD.as_millis() as i64);
    (
        params.flag(ENABLED_PARAMETER, true),
        Duration::from_millis(period.max(1) as u64),
    )
}

/// The latest sample, shared server-wide.
#[derive(Default)]
pub struct Ftdc {
    latest: Mutex<Document>,
    /// The archive writer, opened at the first sample of a server with a
    /// dbpath; `None` again after a write fails.
    writer: Mutex<Option<crate::ftdc_file::Writer>>,
    /// Whether the writer has been opened (or found impossible) yet.
    opened: std::sync::atomic::AtomicBool,
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
        self.write(&sample, ctx);
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = sample;
    }

    /// Write `sample` to `diagnostic.data`. A write that fails is logged and
    /// stops the archive, as mongod's FTDC stops writing after an error;
    /// the in-memory sample carries on.
    fn write(&self, sample: &Document, ctx: &mut CommandContext) {
        use std::sync::atomic::Ordering;
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if !self.opened.swap(true, Ordering::SeqCst) {
            let dir = ctx.storage().ok().and_then(|s| s.data_path());
            *writer =
                dir.map(|d| crate::ftdc_file::Writer::new(crate::ftdc_file::directory_of(&d)));
        }
        let Some(w) = writer.as_mut() else { return };
        let limits = crate::ftdc_file::Limits::from_params(ctx.server_params.as_deref());
        if let Err(e) = w.write(sample, limits, || metadata(ctx)) {
            log::error!(
                "FTDC: writing diagnostic.data failed, no further samples are written: {e}"
            );
            *writer = None;
        }
    }

    /// Write the chunk in progress, when the server stops.
    pub fn close(&self) {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(w) = writer.as_mut() {
            if let Err(e) = w.close() {
                log::error!("FTDC: closing diagnostic.data failed: {e}");
            }
        }
        *writer = None;
    }
}

/// The metadata at the head of every archive file (mongod's
/// `registerStartupCollectors`): `buildInfo`, `getCmdLineOpts`, `hostInfo`.
fn metadata(ctx: &mut CommandContext) -> Document {
    let mut out = doc! {"start": DateTime::now()};
    let start = DateTime::now();
    let build = crate::handshake::build_info(&doc! {"buildInfo": 1}, ctx);
    out.insert("buildInfo", stamped(start, build));
    let start = DateTime::now();
    let opts = crate::diagnostics::get_cmd_line_opts(&doc! {"getCmdLineOpts": 1}, ctx);
    out.insert("getCmdLineOpts", stamped(start, opts));
    let start = DateTime::now();
    let host = crate::diagnostics::host_info(&doc! {"hostInfo": 1}, ctx);
    out.insert("hostInfo", stamped(start, host));
    out.insert("end", DateTime::now());
    out
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
    if !crate::stats_admin::has_stats(storage, db, coll) {
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

/// The transport layer's FTDC counters (mongod's `appendStatsForFTDC`):
/// each listener's socket backlog queue depth, then the connections it
/// discarded because their client had already disconnected. This server
/// has one listener, its TCP socket, and neither measures the depth nor
/// discards a connection, as mongod on macOS does not (RM-417 keeps
/// mongod's Linux measurement and its Unix domain socket listener).
fn transport_layer_stats(ctx: &CommandContext) -> Document {
    let listeners: Vec<Bson> = ctx
        .server_address
        .iter()
        .map(|(host, port)| {
            let address = match host.contains(':') {
                true => format!("[{host}]:{port}"),
                false => format!("{host}:{port}"),
            };
            Bson::Document(doc! {address: 0_i32})
        })
        .collect();
    doc! {
        "listenerSocketBacklogQueueDepths": listeners,
        "connsDiscardedDueToClientDisconnect": 0_i64,
    }
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
    let start = DateTime::now();
    out.insert(
        "transportLayerStats",
        stamped(start, Ok(transport_layer_stats(ctx))),
    );
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
            &[
                "start",
                "serverStatus",
                "transportLayerStats",
                "systemMetrics",
                "end",
            ]
        } else {
            &["start", "serverStatus", "transportLayerStats", "end"]
        };
        assert_eq!(keys, expected);
        let status = s.get_document("serverStatus").unwrap();
        assert_eq!(status.keys().next().map(String::as_str), Some("start"));
        assert_eq!(status.keys().last().map(String::as_str), Some("end"));
    }

    #[test]
    fn transport_layer_stats_name_each_listener() {
        let mut ctx = CommandContext::new(0);
        ctx.server_address = Some(("127.0.0.1".to_string(), 27017));
        assert_eq!(
            transport_layer_stats(&ctx),
            doc! {
                "listenerSocketBacklogQueueDepths": [{"127.0.0.1:27017": 0_i32}],
                "connsDiscardedDueToClientDisconnect": 0_i64,
            }
        );
        ctx.server_address = Some(("::1".to_string(), 27017));
        let stats = transport_layer_stats(&ctx);
        let depths = stats.get_array("listenerSocketBacklogQueueDepths").unwrap();
        assert_eq!(depths[0], Bson::Document(doc! {"[::1]:27017": 0_i32}));
    }

    #[test]
    fn the_parameters_switch_sampling_and_set_its_period() {
        let params = crate::params::ServerParams::new();
        assert_eq!(schedule(&params), (true, PERIOD));
        params.set_startup(ENABLED_PARAMETER, Bson::Boolean(false));
        params.set_startup(PERIOD_PARAMETER, Bson::Int32(250));
        assert_eq!(schedule(&params), (false, Duration::from_millis(250)));
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
