//! The benchmark's result types, cross-machine aggregation, and rendering.
//!
//! The most important part of this module is [`Summary::warnings`]. A
//! throughput number from a distributed benchmark is only meaningful if the
//! load actually landed the way the harness intended, so every condition that
//! would make the headline misleading — a saturated client, errored ops, load
//! windows that failed to overlap, an idle server, a dead server — is detected
//! here and printed under the table rather than left for the reader to notice.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::histogram::{round1, round3, Histogram, LatencySummary};
use crate::opmix::OP_NAMES;
use crate::CLIENT_ROLES;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OpStats {
    pub count: u64,
    pub docs: u64,
    pub errors: u64,
    pub hist: Histogram,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Totals {
    pub ops: u64,
    pub docs: u64,
    pub errors: u64,
    pub ops_per_sec: f64,
    pub docs_per_sec: f64,
}

/// What one client droplet reports back.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClientReport {
    pub client_id: String,
    pub hostname: String,
    pub uri: String,
    pub workers: usize,
    pub duration_s: f64,
    pub requested_start_at: f64,
    pub actual_start_at: f64,
    /// Spread between the earliest and latest worker start on this client.
    pub start_skew_s: f64,
    pub measured_window_s: f64,
    pub op_mix: String,
    pub batch_size: usize,
    pub doc_bytes: usize,
    pub preload: i64,
    pub client_cpu_busy_pct: Option<f64>,
    pub totals: Totals,
    pub ops: BTreeMap<String, OpStats>,
    pub first_errors: Vec<String>,
    pub worker_failures: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SampleSummary {
    pub cpu_busy_pct_mean: Option<f64>,
    pub cpu_busy_pct_max: Option<f64>,
    pub rss_kb_max: Option<u64>,
    pub ncpu: Option<usize>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub size: String,
    pub region: String,
    pub vcpus: u64,
    pub memory_mb: u64,
    pub private_ip: String,
    pub version: String,
    pub cache_size: String,
    pub exec_start: String,
    pub sample: SampleSummary,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClientsInfo {
    pub size: String,
    pub count: usize,
    pub workers_each: usize,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WorkloadInfo {
    pub duration_s: f64,
    pub op_mix: String,
    pub doc_bytes: usize,
    pub batch_size: usize,
    pub preload_per_worker: i64,
    pub keep_data: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Rtt {
    pub min_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    pub mdev_ms: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PerClient {
    pub ops: u64,
    pub docs: u64,
    pub errors: u64,
    pub ops_per_sec: f64,
    pub docs_per_sec: f64,
    pub cpu_busy_pct: Option<f64>,
    pub measured_window_s: f64,
    pub worker_start_skew_s: f64,
    pub latency: LatencySummary,
    pub first_errors: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PerOp {
    pub count: u64,
    pub ops_per_sec: f64,
    pub latency: LatencySummary,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Aggregate {
    pub ops: u64,
    pub docs: u64,
    pub errors: u64,
    pub ops_per_sec: f64,
    pub docs_per_sec: f64,
    pub measured_window_s: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub run_id: String,
    pub generated_at: String,
    pub server: ServerInfo,
    pub clients: ClientsInfo,
    pub workload: WorkloadInfo,
    pub network: BTreeMap<String, Rtt>,
    pub per_client: BTreeMap<String, PerClient>,
    pub aggregate: Aggregate,
    pub per_op: BTreeMap<String, PerOp>,
    pub warnings: Vec<String>,
}

pub struct SummaryInputs {
    pub run_id: String,
    pub generated_at: String,
    pub server: ServerInfo,
    pub clients: ClientsInfo,
    pub workload: WorkloadInfo,
    pub network: BTreeMap<String, Rtt>,
    pub results: BTreeMap<String, ClientReport>,
    pub failures: Vec<String>,
}

pub fn build_summary(input: SummaryInputs) -> Summary {
    let mut merged: BTreeMap<String, Histogram> = OP_NAMES
        .iter()
        .map(|n| (n.to_string(), Histogram::new()))
        .collect();
    let mut per_client: BTreeMap<String, PerClient> = BTreeMap::new();
    let (mut ops, mut docs, mut errors) = (0u64, 0u64, 0u64);
    let mut window = 0.0f64;
    let mut starts: Vec<f64> = Vec::new();

    for role in CLIENT_ROLES {
        let Some(res) = input.results.get(role) else {
            continue;
        };
        let mut client_hist = Histogram::new();
        for name in OP_NAMES {
            let Some(entry) = res.ops.get(name) else {
                continue;
            };
            if let Some(target) = merged.get_mut(name) {
                target.merge(&entry.hist);
            }
            client_hist.merge(&entry.hist);
        }
        window = window.max(res.measured_window_s);
        starts.push(res.actual_start_at);
        ops += res.totals.ops;
        docs += res.totals.docs;
        errors += res.totals.errors;
        per_client.insert(
            role.to_string(),
            PerClient {
                ops: res.totals.ops,
                docs: res.totals.docs,
                errors: res.totals.errors,
                ops_per_sec: res.totals.ops_per_sec,
                docs_per_sec: res.totals.docs_per_sec,
                cpu_busy_pct: res.client_cpu_busy_pct,
                measured_window_s: res.measured_window_s,
                worker_start_skew_s: res.start_skew_s,
                latency: client_hist.summary(),
                first_errors: res.first_errors.clone(),
            },
        );
    }

    let window = if window > 0.0 { window } else { 1.0 };
    let aggregate = Aggregate {
        ops,
        docs,
        errors,
        ops_per_sec: round1(ops as f64 / window),
        docs_per_sec: round1(docs as f64 / window),
        measured_window_s: round3(window),
    };

    let mut warnings = input.failures.clone();
    if errors > 0 {
        warnings.push(format!(
            "{errors} operations errored — the throughput number counts only successful ops, \
             so treat this run as suspect until the errors are explained."
        ));
    }
    for (role, entry) in &per_client {
        if let Some(cpu) = entry.cpu_busy_pct {
            if cpu > 85.0 {
                warnings.push(format!(
                    "{role} CPU was {cpu:.0}% busy — the client, not the server, may be the \
                     limit. Re-run with a larger client size or fewer workers."
                ));
            }
        }
    }
    for (role, entry) in &per_client {
        if entry.worker_start_skew_s > 2.0 {
            warnings.push(format!(
                "{role}'s workers started up to {:.1}s apart — some missed the shared barrier. \
                 Raise --start-delay so every worker is connected before the clock.",
                entry.worker_start_skew_s
            ));
        }
    }
    if starts.len() == 2 && (starts[0] - starts[1]).abs() > 1.0 {
        warnings.push(format!(
            "clients started {:.1}s apart — their load windows only partly overlapped, so the \
             aggregate understates peak concurrency.",
            (starts[0] - starts[1]).abs()
        ));
    }
    if let Some(cpu) = input.server.sample.cpu_busy_pct_mean {
        if cpu < 50.0 && warnings.is_empty() {
            warnings.push(format!(
                "server CPU averaged only {cpu:.0}% — the bottleneck is somewhere other than \
                 server CPU (client capacity, network, or a serialised code path)."
            ));
        }
    }
    if per_client.len() < CLIENT_ROLES.len() {
        warnings.push("not every client reported: the aggregate below is partial.".to_string());
    }

    let per_op = merged
        .into_iter()
        .filter(|(_, h)| h.total > 0)
        .map(|(name, h)| {
            (
                name,
                PerOp {
                    count: h.total,
                    ops_per_sec: round1(h.total as f64 / window),
                    latency: h.summary(),
                },
            )
        })
        .collect();

    Summary {
        run_id: input.run_id,
        generated_at: input.generated_at,
        server: input.server,
        clients: input.clients,
        workload: input.workload,
        network: input.network,
        per_client,
        aggregate,
        per_op,
        warnings,
    }
}

/// Group digits in threes: `1234567` -> `1,234,567`.
fn thousands(n: u64) -> String {
    let raw = n.to_string();
    let head = raw.len() % 3;
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    if head > 0 {
        out.push_str(&raw[..head]);
    }
    for (idx, chunk) in raw.as_bytes()[head..].chunks(3).enumerate() {
        if idx > 0 || head > 0 {
            out.push(',');
        }
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
    }
    out
}

pub fn render_summary(s: &Summary) -> String {
    let srv = &s.server;
    let mut lines = vec![
        "# SecantusDB three-droplet benchmark".to_string(),
        String::new(),
        format!("run           {}", s.run_id),
        format!(
            "server        {}  {} ({} vCPU, {} MB)  {}",
            srv.name, srv.size, srv.vcpus, srv.memory_mb, srv.region
        ),
        format!(
            "binary        {}  (WT cache {})",
            if srv.version.is_empty() {
                "unknown"
            } else {
                &srv.version
            },
            srv.cache_size
        ),
        format!(
            "clients       {} x {}, {} workers each",
            s.clients.count, s.clients.size, s.clients.workers_each
        ),
        format!(
            "workload      {}  {} B docs  batch={}  {:.0}s",
            s.workload.op_mix, s.workload.doc_bytes, s.workload.batch_size, s.workload.duration_s
        ),
        String::new(),
        "| client | ops | ops/s | docs/s | errors | p50 ms | p99 ms | p99.9 ms | client CPU |"
            .to_string(),
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |".to_string(),
    ];

    for role in CLIENT_ROLES {
        match s.per_client.get(role) {
            None => lines.push(format!("| {role} | (no result) | | | | | | | |")),
            Some(c) => {
                let cpu = match c.cpu_busy_pct {
                    Some(v) => format!("{v:.0}%"),
                    None => "-".to_string(),
                };
                lines.push(format!(
                    "| {} | {} | {:.0} | {:.0} | {} | {:.2} | {:.2} | {:.2} | {} |",
                    role,
                    thousands(c.ops),
                    c.ops_per_sec,
                    c.docs_per_sec,
                    thousands(c.errors),
                    c.latency.p50_ms,
                    c.latency.p99_ms,
                    c.latency.p999_ms,
                    cpu
                ));
            }
        }
    }
    lines.push(format!(
        "| **total** | **{}** | **{:.0}** | **{:.0}** | **{}** | | | | |",
        thousands(s.aggregate.ops),
        s.aggregate.ops_per_sec,
        s.aggregate.docs_per_sec,
        thousands(s.aggregate.errors)
    ));

    if !s.per_op.is_empty() {
        lines.push(String::new());
        lines.push("| op | count | ops/s | p50 ms | p99 ms | p99.9 ms |".to_string());
        lines.push("| --- | ---: | ---: | ---: | ---: | ---: |".to_string());
        // Report in the mix's natural order, not the map's alphabetical one.
        for name in OP_NAMES {
            let Some(entry) = s.per_op.get(name) else {
                continue;
            };
            lines.push(format!(
                "| {} | {} | {:.0} | {:.2} | {:.2} | {:.2} |",
                name,
                thousands(entry.count),
                entry.ops_per_sec,
                entry.latency.p50_ms,
                entry.latency.p99_ms,
                entry.latency.p999_ms
            ));
        }
    }

    let sample = &srv.sample;
    if sample.cpu_busy_pct_mean.is_some() || sample.rss_kb_max.is_some() {
        lines.push(String::new());
        lines.push(format!(
            "server CPU    {} mean / {} peak of {} vCPU",
            sample
                .cpu_busy_pct_mean
                .map(|v| format!("{v:.1}%"))
                .unwrap_or("-".into()),
            sample
                .cpu_busy_pct_max
                .map(|v| format!("{v:.1}%"))
                .unwrap_or("-".into()),
            sample.ncpu.map(|v| v.to_string()).unwrap_or("?".into()),
        ));
        lines.push(format!(
            "server RSS    {} peak",
            sample
                .rss_kb_max
                .map(|kb| format!("{:.2} GiB", kb as f64 / 1024.0 / 1024.0))
                .unwrap_or("-".into())
        ));
    }
    for (role, stats) in &s.network {
        lines.push(format!(
            "network       {} -> server {:.3} ms avg (mdev {:.3} ms)",
            role, stats.avg_ms, stats.mdev_ms
        ));
    }
    if !s.warnings.is_empty() {
        lines.push(String::new());
        lines.push("## Warnings".to_string());
        lines.push(String::new());
        for w in &s.warnings {
            lines.push(format!("- {w}"));
        }
    }
    lines.join("\n")
}
