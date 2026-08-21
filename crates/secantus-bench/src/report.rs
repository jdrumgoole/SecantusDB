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

/// Side-by-side comparison of two or more engines from the same run.
///
/// The ratio row is the point of the whole harness: same hardware, same
/// clients, same workload, same network — so a difference is the database and
/// nothing else. Ratios are stated as "first engine relative to <baseline>",
/// with throughput above 1.0 meaning faster and latency below 1.0 meaning
/// quicker, because those are opposite senses and conflating them is how
/// benchmark tables mislead.
pub fn render_comparison(run_id: &str, results: &[(crate::engine::Engine, Summary)]) -> String {
    let mut lines = vec![
        "# SecantusDB vs MongoDB — three-droplet benchmark".to_string(),
        String::new(),
        format!("run           {run_id}"),
    ];
    if let Some((_, first)) = results.first() {
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
            "workload      {}  {} B docs  batch={}  {:.0}s per engine",
            first.workload.op_mix,
            first.workload.doc_bytes,
            first.workload.batch_size,
            first.workload.duration_s
        ));
        lines.push(format!("cache         {} (both engines)", srv.cache_size));
    }
    lines.push(String::new());
    lines.push(
        "| engine | version | ops/s | docs/s | errors | p50 ms | p99 ms | p99.9 ms | server CPU |"
            .to_string(),
    );
    lines.push("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |".to_string());

    for (engine, s) in results {
        let lat = overall_latency(s);
        let cpu = s
            .server
            .sample
            .cpu_busy_pct_mean
            .map(|v| format!("{v:.1}%"))
            .unwrap_or("-".to_string());
        lines.push(format!(
            "| **{}** | {} | **{:.0}** | {:.0} | {} | {:.2} | {:.2} | {:.2} | {} |",
            engine.name(),
            short_version(&s.server.version),
            s.aggregate.ops_per_sec,
            s.aggregate.docs_per_sec,
            thousands(s.aggregate.errors),
            lat.p50_ms,
            lat.p99_ms,
            lat.p999_ms,
            cpu
        ));
    }

    if results.len() == 2 {
        let (a_engine, a) = &results[0];
        let (b_engine, b) = &results[1];
        let (la, lb) = (overall_latency(a), overall_latency(b));
        lines.push(String::new());
        lines.push(format!(
            "**{} relative to {}** — throughput >1.0 is faster, latency <1.0 is quicker:",
            a_engine.name(),
            b_engine.name()
        ));
        lines.push(String::new());
        lines.push("| metric | ratio |".to_string());
        lines.push("| --- | ---: |".to_string());
        lines.push(format!(
            "| throughput (ops/s) | {} |",
            ratio(a.aggregate.ops_per_sec, b.aggregate.ops_per_sec)
        ));
        lines.push(format!("| p50 latency | {} |", ratio(la.p50_ms, lb.p50_ms)));
        lines.push(format!("| p99 latency | {} |", ratio(la.p99_ms, lb.p99_ms)));
        lines.push(format!(
            "| p99.9 latency | {} |",
            ratio(la.p999_ms, lb.p999_ms)
        ));
        for name in OP_NAMES {
            if let (Some(x), Some(y)) = (a.per_op.get(name), b.per_op.get(name)) {
                lines.push(format!(
                    "| {name} throughput | {} |",
                    ratio(x.ops_per_sec, y.ops_per_sec)
                ));
            }
        }
    }

    let warned: Vec<String> = results
        .iter()
        .flat_map(|(e, s)| {
            s.warnings
                .iter()
                .map(move |w| format!("[{}] {w}", e.name()))
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

    #[test]
    fn the_ratio_row_states_throughput_and_latency_in_their_own_senses() {
        // secantus does 2x the throughput at half the p50: 2.00x and 0.50x.
        let results = vec![
            (
                Engine::Secantus,
                summary("secantusdb", 8000.0, 2.0, 20.0, 90.0),
            ),
            (Engine::Mongod, summary("mongod", 4000.0, 4.0, 40.0, 70.0)),
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
            (
                Engine::Secantus,
                summary("secantusdb", 8000.0, 2.0, 20.0, 90.0),
            ),
            (Engine::Mongod, summary("mongod", 4000.0, 4.0, 40.0, 70.0)),
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
        let results = vec![(
            Engine::Secantus,
            summary("secantusdb", 8000.0, 2.0, 20.0, 90.0),
        )];
        let text = render_comparison("R", &results);
        assert!(!text.contains("relative to"));
    }

    #[test]
    fn warnings_from_either_engine_are_attributed_and_kept() {
        let mut a = summary("secantusdb", 8000.0, 2.0, 20.0, 90.0);
        a.warnings.push("client-1 CPU was 93% busy".into());
        let mut b = summary("mongod", 4000.0, 4.0, 40.0, 70.0);
        b.warnings.push("17 operations errored".into());
        let text = render_comparison("R", &[(Engine::Secantus, a), (Engine::Mongod, b)]);
        assert!(
            text.contains("[secantusdb] client-1 CPU was 93% busy"),
            "{text}"
        );
        assert!(text.contains("[mongod] 17 operations errored"), "{text}");
    }

    #[test]
    fn a_zero_baseline_reports_na_rather_than_infinity() {
        let results = vec![
            (
                Engine::Secantus,
                summary("secantusdb", 8000.0, 2.0, 20.0, 90.0),
            ),
            (Engine::Mongod, summary("mongod", 0.0, 0.0, 0.0, 0.0)),
        ];
        let text = render_comparison("R", &results);
        assert!(text.contains("n/a"), "{text}");
        assert!(!text.contains("inf"), "{text}");
    }

    #[test]
    fn overall_latency_weights_clients_by_their_op_count() {
        let mut s = summary("secantusdb", 8000.0, 2.0, 20.0, 90.0);
        // Skew the clients: 3000 ops at p50 4ms and 1000 ops at p50 8ms
        // should weight to 5ms, not the unweighted 6ms.
        let entries: Vec<String> = s.per_client.keys().cloned().collect();
        s.per_client.get_mut(&entries[0]).unwrap().ops = 3000;
        s.per_client.get_mut(&entries[0]).unwrap().latency.p50_ms = 4.0;
        s.per_client.get_mut(&entries[1]).unwrap().ops = 1000;
        s.per_client.get_mut(&entries[1]).unwrap().latency.p50_ms = 8.0;
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
}
