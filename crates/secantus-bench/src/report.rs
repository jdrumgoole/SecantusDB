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

/// One operation that exceeded the `--slow-ms` threshold.
///
/// The histogram deliberately throws time away, which is exactly what a tail
/// investigation needs back: whether the slow operations arrive periodically
/// (a checkpoint, a prune, a flush) or at random (lock contention, eviction)
/// is the first fork in the diagnosis, and only timestamps answer it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlowOp {
    /// Unix epoch seconds when the operation *completed*.
    pub t: f64,
    pub op: String,
    pub ms: f64,
    pub worker: usize,
}

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
    /// Operations slower than `--slow-ms`, in completion order. Empty when the
    /// threshold is 0 (the default), so normal runs pay nothing for it.
    #[serde(default)]
    pub slow_ops: Vec<SlowOp>,
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
    /// Which database produced these numbers.
    pub engine: String,
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
    pub engine: String,
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
        engine: input.engine,
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
        format!("# {} — three-droplet benchmark", s.engine),
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

/// The median of a set of samples. Even counts average the middle two.
///
/// Median rather than mean: one pass disrupted by a noisy neighbour or a
/// checkpoint stall should not drag the headline, and with small N a mean is
/// exactly what an outlier hijacks.
pub fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// Relative spread of a set of samples: `(max - min) / median`, as a
/// percentage. This is the number that says whether the median means anything.
pub fn spread_pct(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mid = median(values);
    if mid <= 0.0 {
        return 0.0;
    }
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    round1((max - min) / mid * 100.0)
}

/// One engine's results across every pass of a run.
pub struct EngineRuns {
    pub engine: crate::engine::Engine,
    pub passes: Vec<Summary>,
}

impl EngineRuns {
    fn ops_per_sec(&self) -> Vec<f64> {
        self.passes
            .iter()
            .map(|s| s.aggregate.ops_per_sec)
            .collect()
    }

    fn latencies(&self) -> Vec<LatencySummary> {
        self.passes.iter().map(overall_latency).collect()
    }

    fn median_latency(&self) -> LatencySummary {
        let lats = self.latencies();
        let pick = |f: fn(&LatencySummary) -> f64| median(&lats.iter().map(f).collect::<Vec<_>>());
        LatencySummary {
            count: self.passes.iter().map(|s| s.aggregate.ops).sum(),
            mean_ms: pick(|l| l.mean_ms),
            min_ms: pick(|l| l.min_ms),
            p50_ms: pick(|l| l.p50_ms),
            p90_ms: pick(|l| l.p90_ms),
            p99_ms: pick(|l| l.p99_ms),
            p999_ms: pick(|l| l.p999_ms),
            max_ms: pick(|l| l.max_ms),
        }
    }

    fn median_cpu(&self) -> Option<f64> {
        let cpus: Vec<f64> = self
            .passes
            .iter()
            .filter_map(|s| s.server.sample.cpu_busy_pct_mean)
            .collect();
        if cpus.is_empty() {
            None
        } else {
            Some(median(&cpus))
        }
    }

    fn median_op_rate(&self, op: &str) -> Option<f64> {
        let rates: Vec<f64> = self
            .passes
            .iter()
            .filter_map(|s| s.per_op.get(op).map(|o| o.ops_per_sec))
            .collect();
        if rates.is_empty() {
            None
        } else {
            Some(median(&rates))
        }
    }
}

/// Side-by-side comparison of the engines in a run.
///
/// The ratio row is the point of the whole harness: same hardware, same
/// clients, same workload, same network — so a difference is the database and
/// nothing else. Ratios are stated as "first engine relative to <baseline>",
/// with throughput above 1.0 meaning faster and latency below 1.0 meaning
/// quicker, because those are opposite senses and conflating them is how
/// benchmark tables mislead.
///
/// With more than one pass every figure is a **median**, and a spread column
/// reports `(max - min) / median` so the reader can see whether the medians are
/// worth quoting at all.
pub fn render_comparison(run_id: &str, results: &[EngineRuns]) -> String {
    let passes = results.first().map(|r| r.passes.len()).unwrap_or(0);
    let mut lines = vec![
        "# SecantusDB vs MongoDB — three-droplet benchmark".to_string(),
        String::new(),
        format!("run           {run_id}"),
    ];
    if let Some(first) = results.first().and_then(|r| r.passes.first()) {
        let srv = &first.server;
        lines.push(format!(
            "server        {} ({} vCPU, {} MB)  {}",
            srv.size, srv.vcpus, srv.memory_mb, srv.region
        ));
        lines.push(format!(
            "clients       {} x {}, {} workers each",
            first.clients.count, first.clients.size, first.clients.workers_each
        ));
        lines.push(format!(
            "workload      {}  {} B docs  batch={}  {:.0}s per engine per pass",
            first.workload.op_mix,
            first.workload.doc_bytes,
            first.workload.batch_size,
            first.workload.duration_s
        ));
        lines.push(format!("cache         {} (both engines)", srv.cache_size));
        lines.push(format!(
            "passes        {passes}{}",
            if passes > 1 {
                " (engines interleaved; figures below are medians)"
            } else {
                ""
            }
        ));
    }
    lines.push(String::new());
    if passes > 1 {
        lines.push(
            "| engine | version | ops/s (median) | spread | p50 ms | p99 ms | p99.9 ms | server CPU |"
                .to_string(),
        );
        lines.push("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |".to_string());
    } else {
        lines.push(
            "| engine | version | ops/s | errors | p50 ms | p99 ms | p99.9 ms | server CPU |"
                .to_string(),
        );
        lines.push("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |".to_string());
    }

    for run in results {
        let lat = run.median_latency();
        let cpu = run
            .median_cpu()
            .map(|v| format!("{v:.1}%"))
            .unwrap_or("-".to_string());
        let version = run
            .passes
            .first()
            .map(|s| short_version(&s.server.version))
            .unwrap_or("unknown".to_string());
        let rates = run.ops_per_sec();
        let third = if passes > 1 {
            format!("{:.1}%", spread_pct(&rates))
        } else {
            thousands(run.passes.first().map(|s| s.aggregate.errors).unwrap_or(0))
        };
        lines.push(format!(
            "| **{}** | {} | **{:.0}** | {} | {:.2} | {:.2} | {:.2} | {} |",
            run.engine.name(),
            version,
            median(&rates),
            third,
            lat.p50_ms,
            lat.p99_ms,
            lat.p999_ms,
            cpu
        ));
    }

    if passes > 1 {
        lines.push(String::new());
        lines.push("Per pass (ops/s), in the order they ran:".to_string());
        lines.push(String::new());
        let header: Vec<String> = (1..=passes).map(|i| format!("pass {i}")).collect();
        lines.push(format!("| engine | {} |", header.join(" | ")));
        lines.push(format!("| --- | {} |", vec!["---:"; passes].join(" | ")));
        for run in results {
            let cells: Vec<String> = run
                .ops_per_sec()
                .iter()
                .map(|v| format!("{v:.0}"))
                .collect();
            lines.push(format!("| {} | {} |", run.engine.name(), cells.join(" | ")));
        }
    }

    if results.len() == 2 {
        let (a, b) = (&results[0], &results[1]);
        let (la, lb) = (a.median_latency(), b.median_latency());
        lines.push(String::new());
        lines.push(format!(
            "**{} relative to {}** — throughput >1.0 is faster, latency <1.0 is quicker:",
            a.engine.name(),
            b.engine.name()
        ));
        lines.push(String::new());
        lines.push("| metric | ratio |".to_string());
        lines.push("| --- | ---: |".to_string());
        lines.push(format!(
            "| throughput (ops/s) | {} |",
            ratio(median(&a.ops_per_sec()), median(&b.ops_per_sec()))
        ));
        lines.push(format!("| p50 latency | {} |", ratio(la.p50_ms, lb.p50_ms)));
        lines.push(format!("| p99 latency | {} |", ratio(la.p99_ms, lb.p99_ms)));
        lines.push(format!(
            "| p99.9 latency | {} |",
            ratio(la.p999_ms, lb.p999_ms)
        ));
        for name in OP_NAMES {
            if let (Some(x), Some(y)) = (a.median_op_rate(name), b.median_op_rate(name)) {
                lines.push(format!("| {name} throughput | {} |", ratio(x, y)));
            }
        }
    }

    let warned: Vec<String> = results
        .iter()
        .flat_map(|r| {
            r.passes.iter().enumerate().flat_map(move |(i, s)| {
                s.warnings.iter().map(move |w| {
                    if r.passes.len() > 1 {
                        format!("[{} pass {}] {w}", r.engine.name(), i + 1)
                    } else {
                        format!("[{}] {w}", r.engine.name())
                    }
                })
            })
        })
        .collect();
    if !warned.is_empty() {
        lines.push(String::new());
        lines.push("## Warnings".to_string());
        lines.push(String::new());
        lines.extend(warned.into_iter().map(|w| format!("- {w}")));
    }
    lines.join("\n")
}

/// The all-operations latency view across both client droplets.
///
/// Each client's percentiles are weighted by its operation count. That is
/// exact when the clients are balanced (they are, by construction — identical
/// droplets running identical work) and close otherwise. The exact merge would
/// need the raw histograms, which live in the per-client JSON alongside this
/// summary for anyone who wants to recompute it.
fn overall_latency(s: &Summary) -> LatencySummary {
    let total: u64 = s.per_client.values().map(|c| c.ops).sum();
    if total == 0 {
        return LatencySummary::default();
    }
    let weighted = |f: fn(&PerClient) -> f64| -> f64 {
        let sum: f64 = s.per_client.values().map(|c| f(c) * c.ops as f64).sum();
        crate::histogram::round3(sum / total as f64)
    };
    LatencySummary {
        count: total,
        mean_ms: weighted(|c| c.latency.mean_ms),
        min_ms: s
            .per_client
            .values()
            .map(|c| c.latency.min_ms)
            .fold(f64::INFINITY, f64::min),
        p50_ms: weighted(|c| c.latency.p50_ms),
        p90_ms: weighted(|c| c.latency.p90_ms),
        p99_ms: weighted(|c| c.latency.p99_ms),
        p999_ms: weighted(|c| c.latency.p999_ms),
        max_ms: s
            .per_client
            .values()
            .map(|c| c.latency.max_ms)
            .fold(0.0, f64::max),
    }
}

fn ratio(a: f64, b: f64) -> String {
    if b <= 0.0 {
        return "n/a".to_string();
    }
    format!("{:.2}x", a / b)
}

/// Version strings are long ("db version v8.0.4"); keep the table readable.
fn short_version(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    trimmed
        .split_whitespace()
        .last()
        .unwrap_or(trimmed)
        .to_string()
}

#[cfg(test)]
mod comparison_tests {
    use super::*;
    use crate::engine::Engine;

    fn summary(engine: &str, ops_per_sec: f64, p50: f64, p99: f64, cpu: f64) -> Summary {
        let mut per_client = BTreeMap::new();
        for role in CLIENT_ROLES {
            per_client.insert(
                role.to_string(),
                PerClient {
                    ops: 1000,
                    ops_per_sec: ops_per_sec / 2.0,
                    latency: LatencySummary {
                        count: 1000,
                        p50_ms: p50,
                        p99_ms: p99,
                        p999_ms: p99 * 2.0,
                        ..LatencySummary::default()
                    },
                    ..PerClient::default()
                },
            );
        }
        let mut per_op = BTreeMap::new();
        per_op.insert(
            "insert".to_string(),
            PerOp {
                count: 2000,
                ops_per_sec,
                latency: LatencySummary::default(),
            },
        );
        Summary {
            run_id: "R".into(),
            engine: engine.into(),
            server: ServerInfo {
                version: format!("v{engine}-1.2.3"),
                cache_size: "4G".into(),
                sample: SampleSummary {
                    cpu_busy_pct_mean: Some(cpu),
                    ncpu: Some(4),
                    ..SampleSummary::default()
                },
                ..ServerInfo::default()
            },
            aggregate: Aggregate {
                ops: 2000,
                ops_per_sec,
                ..Aggregate::default()
            },
            per_client,
            per_op,
            ..Summary::default()
        }
    }

    fn one(engine: Engine, name: &str, ops: f64, p50: f64, p99: f64, cpu: f64) -> EngineRuns {
        EngineRuns {
            engine,
            passes: vec![summary(name, ops, p50, p99, cpu)],
        }
    }

    #[test]
    fn the_ratio_row_states_throughput_and_latency_in_their_own_senses() {
        // secantus does 2x the throughput at half the p50: 2.00x and 0.50x.
        let results = vec![
            one(Engine::Secantus, "secantusdb", 8000.0, 2.0, 20.0, 90.0),
            one(Engine::Mongod, "mongod", 4000.0, 4.0, 40.0, 70.0),
        ];
        let text = render_comparison("R", &results);
        assert!(text.contains("| throughput (ops/s) | 2.00x |"), "{text}");
        assert!(text.contains("| p50 latency | 0.50x |"), "{text}");
        assert!(text.contains("| p99 latency | 0.50x |"), "{text}");
        assert!(text.contains("| insert throughput | 2.00x |"), "{text}");
        // The reader must not have to guess which direction is good.
        assert!(text.contains("throughput >1.0 is faster, latency <1.0 is quicker"));
    }

    #[test]
    fn both_engines_appear_with_their_versions_and_cpu() {
        let results = vec![
            one(Engine::Secantus, "secantusdb", 8000.0, 2.0, 20.0, 90.0),
            one(Engine::Mongod, "mongod", 4000.0, 4.0, 40.0, 70.0),
        ];
        let text = render_comparison("R", &results);
        assert!(text.contains("secantusdb"));
        assert!(text.contains("mongod"));
        assert!(text.contains("90.0%"));
        assert!(text.contains("70.0%"));
        assert!(text.contains("cache         4G (both engines)"));
    }

    #[test]
    fn a_single_engine_needs_no_ratio_row() {
        let results = vec![one(Engine::Secantus, "secantusdb", 8000.0, 2.0, 20.0, 90.0)];
        assert!(!render_comparison("R", &results).contains("relative to"));
    }

    #[test]
    fn warnings_from_either_engine_are_attributed_and_kept() {
        let mut a = one(Engine::Secantus, "secantusdb", 8000.0, 2.0, 20.0, 90.0);
        a.passes[0]
            .warnings
            .push("client-1 CPU was 93% busy".into());
        let mut b = one(Engine::Mongod, "mongod", 4000.0, 4.0, 40.0, 70.0);
        b.passes[0].warnings.push("17 operations errored".into());
        let text = render_comparison("R", &[a, b]);
        assert!(
            text.contains("[secantusdb] client-1 CPU was 93% busy"),
            "{text}"
        );
        assert!(text.contains("[mongod] 17 operations errored"), "{text}");
    }

    #[test]
    fn a_zero_baseline_reports_na_rather_than_infinity() {
        let results = vec![
            one(Engine::Secantus, "secantusdb", 8000.0, 2.0, 20.0, 90.0),
            one(Engine::Mongod, "mongod", 0.0, 0.0, 0.0, 0.0),
        ];
        let text = render_comparison("R", &results);
        assert!(text.contains("n/a"), "{text}");
        assert!(!text.contains("inf"), "{text}");
    }

    #[test]
    fn overall_latency_weights_clients_by_their_op_count() {
        let mut s = summary("secantusdb", 8000.0, 2.0, 20.0, 90.0);
        // 3000 ops at p50 4ms and 1000 at p50 8ms weight to 5ms, not 6ms.
        let keys: Vec<String> = s.per_client.keys().cloned().collect();
        s.per_client.get_mut(&keys[0]).unwrap().ops = 3000;
        s.per_client.get_mut(&keys[0]).unwrap().latency.p50_ms = 4.0;
        s.per_client.get_mut(&keys[1]).unwrap().ops = 1000;
        s.per_client.get_mut(&keys[1]).unwrap().latency.p50_ms = 8.0;
        assert_eq!(overall_latency(&s).p50_ms, 5.0);
    }

    #[test]
    fn long_version_strings_are_shortened_for_the_table() {
        assert_eq!(short_version("db version v8.0.4"), "v8.0.4");
        assert_eq!(
            short_version("secantusd-rs 0.5.3-beta.160"),
            "0.5.3-beta.160"
        );
        assert_eq!(short_version("  "), "unknown");
    }

    // --- repeat / median ---------------------------------------------------

    #[test]
    fn median_takes_the_middle_and_averages_an_even_pair() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
        assert_eq!(median(&[7.0]), 7.0);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn an_outlier_pass_cannot_hijack_the_median() {
        // A single disrupted pass (200) must not drag the headline; the mean
        // would read 800, the median reads 600.
        let values = [600.0, 600.0, 200.0, 600.0, 1400.0];
        assert_eq!(median(&values), 600.0);
    }

    #[test]
    fn spread_reports_the_relative_range() {
        assert_eq!(spread_pct(&[100.0, 110.0]), 9.5); // 10 / 105
        assert_eq!(spread_pct(&[100.0, 100.0, 100.0]), 0.0);
        assert_eq!(spread_pct(&[100.0]), 0.0); // one pass has no spread
    }

    #[test]
    fn multiple_passes_report_medians_a_spread_and_a_per_pass_table() {
        let runs = vec![
            EngineRuns {
                engine: Engine::Secantus,
                passes: vec![
                    summary("secantusdb", 8000.0, 2.0, 20.0, 90.0),
                    summary("secantusdb", 9000.0, 2.0, 20.0, 90.0),
                    summary("secantusdb", 8500.0, 2.0, 20.0, 90.0),
                ],
            },
            EngineRuns {
                engine: Engine::Mongod,
                passes: vec![
                    summary("mongod", 4000.0, 4.0, 40.0, 70.0),
                    summary("mongod", 4400.0, 4.0, 40.0, 70.0),
                    summary("mongod", 4200.0, 4.0, 40.0, 70.0),
                ],
            },
        ];
        let text = render_comparison("R", &runs);
        assert!(
            text.contains("passes        3 (engines interleaved"),
            "{text}"
        );
        assert!(text.contains("ops/s (median)"), "{text}");
        // medians 8500 and 4200 -> 2.02x, not the 2.00x of the first pass.
        assert!(text.contains("**8500**"), "{text}");
        assert!(text.contains("**4200**"), "{text}");
        assert!(text.contains("| throughput (ops/s) | 2.02x |"), "{text}");
        // The per-pass table has to show the order they actually ran in.
        assert!(text.contains("| pass 1 | pass 2 | pass 3 |"), "{text}");
        assert!(
            text.contains("| secantusdb | 8000 | 9000 | 8500 |"),
            "{text}"
        );
    }

    #[test]
    fn warnings_from_a_repeat_run_name_the_pass() {
        let mut runs = vec![EngineRuns {
            engine: Engine::Secantus,
            passes: vec![
                summary("secantusdb", 8000.0, 2.0, 20.0, 90.0),
                summary("secantusdb", 8000.0, 2.0, 20.0, 90.0),
            ],
        }];
        runs[0].passes[1]
            .warnings
            .push("17 operations errored".into());
        let text = render_comparison("R", &runs);
        assert!(
            text.contains("[secantusdb pass 2] 17 operations errored"),
            "{text}"
        );
    }

    #[test]
    fn a_single_pass_still_shows_errors_rather_than_a_spread_column() {
        let results = vec![one(Engine::Secantus, "secantusdb", 8000.0, 2.0, 20.0, 90.0)];
        let text = render_comparison("R", &results);
        assert!(text.contains("| errors |"), "{text}");
        assert!(!text.contains("spread"), "{text}");
    }
}

#[cfg(test)]
mod slow_op_tests {
    use super::*;

    #[test]
    fn slow_ops_round_trip_through_json() {
        let report = ClientReport {
            client_id: "c1".into(),
            slow_ops: vec![
                SlowOp {
                    t: 1000.5,
                    op: "insert".into(),
                    ms: 42.25,
                    worker: 3,
                },
                SlowOp {
                    t: 1000.6,
                    op: "find".into(),
                    ms: 7.5,
                    worker: 1,
                },
            ],
            ..ClientReport::default()
        };
        let text = serde_json::to_string(&report).unwrap();
        let back: ClientReport = serde_json::from_str(&text).unwrap();
        assert_eq!(back.slow_ops.len(), 2);
        assert_eq!(back.slow_ops[0].op, "insert");
        assert_eq!(back.slow_ops[0].ms, 42.25);
        assert_eq!(back.slow_ops[0].worker, 3);
    }

    #[test]
    fn a_report_without_slow_ops_still_parses() {
        // Reports written before --slow-ms existed, and every run with the
        // feature off, carry no `slow_ops` key at all.
        let json = r#"{"client_id":"c1","hostname":"h","uri":"u","workers":1,
            "duration_s":1.0,"requested_start_at":0.0,"actual_start_at":0.0,
            "start_skew_s":0.0,"measured_window_s":1.0,"op_mix":"insert=100",
            "batch_size":1,"doc_bytes":8,"preload":0,"client_cpu_busy_pct":null,
            "totals":{"ops":1,"docs":1,"errors":0,"ops_per_sec":1.0,"docs_per_sec":1.0},
            "ops":{},"first_errors":[],"worker_failures":[]}"#;
        let back: ClientReport = serde_json::from_str(json).unwrap();
        assert!(back.slow_ops.is_empty());
    }
}
