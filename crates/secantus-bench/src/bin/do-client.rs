//! The load agent that runs **on** each client droplet.
//!
//! Three subcommands:
//!
//! * `setup` — destructive prep: drop the target collections, create the `n`
//!   index, preload `--preload` documents per worker. Run to completion before
//!   the timed phase so preload cost never lands inside the measurement and so
//!   reads have something to read.
//! * `run` — the timed phase. One OS thread per worker, each with its own
//!   connection and its own collection. Every worker waits for the `--start-at`
//!   wall-clock barrier so both client droplets load the server over the same
//!   window, then drives the op mix for `--duration` seconds and writes a JSON
//!   report to `--out`.
//! * `sample` — no load: sample `/proc/stat` and the server process's RSS into
//!   a JSON trace. Runs on the *server* droplet alongside the load, so a
//!   headline ops/s always arrives with the CPU number that says whether the
//!   server was actually the bottleneck.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use rand::{Rng, SeedableRng};
use secantus_bench::argv::Args;
use secantus_bench::histogram::{round1, round3, Histogram};
use secantus_bench::mongo::{make_document, make_payload, Conn, Payload};
use secantus_bench::opmix::{parse_op_mix, pick, Op, OP_NAMES};
use secantus_bench::report::{ClientReport, OpStats, SlowOp, Totals};
use secantus_bench::timefmt::now_epoch_secs;
use secantus_bench::BenchResult;

const BOOL_FLAGS: [&str; 2] = ["--keep-data", "--version"];
const VALUE_FLAGS: [&str; 15] = [
    "--addr",
    "--client-id",
    "--db",
    "--prefix",
    "--workers",
    "--doc-bytes",
    "--preload",
    "--op-mix",
    "--batch-size",
    "--duration",
    "--start-at",
    "--seed",
    "--out",
    "--slow-ms",
    "--payload",
];
const EXTRA_SAMPLE_FLAGS: [&str; 2] = ["--interval", "--process"];

const DEFAULT_DB: &str = "dobench";
const DEFAULT_PREFIX: &str = "load";

struct LoadArgs {
    addr: String,
    client_id: String,
    db: String,
    prefix: String,
    workers: usize,
    doc_bytes: usize,
    preload: i64,
    op_mix: String,
    batch_size: usize,
    duration: f64,
    start_at: f64,
    seed: u64,
    out: String,
    keep_data: bool,
    slow_ms: f64,
    payload: Payload,
}

fn load_args(args: &Args) -> BenchResult<LoadArgs> {
    Ok(LoadArgs {
        addr: args.required("--addr")?,
        client_id: args.required("--client-id")?,
        db: args.str_or("--db", DEFAULT_DB),
        prefix: args.str_or("--prefix", DEFAULT_PREFIX),
        workers: args.usize_or("--workers", 8)?,
        doc_bytes: args.usize_or("--doc-bytes", 8192)?,
        preload: args.i64_or("--preload", 10_000)?,
        op_mix: args.str_or("--op-mix", "insert=70,find=20,update=10"),
        batch_size: args.usize_or("--batch-size", 1)?,
        duration: args.f64_or("--duration", 60.0)?,
        start_at: args.f64_or("--start-at", 0.0)?,
        seed: args.usize_or("--seed", 0)? as u64,
        out: args.str_or("--out", "/tmp/do-client-result.json"),
        keep_data: args.has("--keep-data"),
        slow_ms: args.f64_or("--slow-ms", 0.0)?,
        payload: Payload::parse(&args.str_or("--payload", "repeat"))?,
    })
}

/// Every worker on every client droplet owns a disjoint collection, so `_id`
/// collisions and per-collection contention stay out of the measurement.
fn collection_name(prefix: &str, client_id: &str, worker: usize) -> String {
    format!("{prefix}_{client_id}_w{worker}")
}

fn connect(addr: &str, db: &str) -> BenchResult<Conn> {
    Conn::connect(addr, db, Duration::from_secs(20))
}

// -- setup ------------------------------------------------------------------

fn cmd_setup(a: &LoadArgs) -> BenchResult<()> {
    // Payload is derived per document (see `make_payload`): one shared random
    // string is incompressible within a document but perfectly compressible
    // across them, which hides the real storage cost.
    let payload_for =
        |n: i64| make_payload(a.payload, a.doc_bytes, a.seed.wrapping_add(n as u64 + 1));
    let mut conn = connect(&a.addr, &a.db)?;
    let started = Instant::now();
    let mut preloaded = 0i64;
    for worker in 0..a.workers {
        let coll = collection_name(&a.prefix, &a.client_id, worker);
        if !a.keep_data {
            conn.drop_collection(&coll)?;
        }
        // Reads and updates select by `n`; without this index each one is a
        // collection scan and the run measures scanning, not the op mix asked for.
        conn.create_index_on_n(&coll)?;
        let mut n = 0i64;
        while n < a.preload {
            let chunk = std::cmp::min(500, a.preload - n) as usize;
            let docs = (0..chunk)
                .map(|i| make_document(n + i as i64, &payload_for(n + i as i64)))
                .collect();
            conn.insert(&coll, docs)?;
            n += chunk as i64;
            preloaded += chunk as i64;
        }
    }
    println!(
        "setup ok: {} collections, {preloaded} docs preloaded in {:.1}s",
        a.workers,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

// -- run --------------------------------------------------------------------

struct WorkerResult {
    started_at: f64,
    elapsed_s: f64,
    counts: [u64; 3],
    docs: [u64; 3],
    errors: [u64; 3],
    hists: [Histogram; 3],
    first_errors: Vec<String>,
    slow_ops: Vec<SlowOp>,
}

fn run_worker(a: &LoadArgs, worker: usize) -> BenchResult<WorkerResult> {
    // A stable per-client offset: a hashed client id would vary per process and
    // make --seed fail to reproduce a run.
    let client_offset: u64 = a.client_id.bytes().map(u64::from).sum();
    let mut rng = rand::rngs::StdRng::seed_from_u64(a.seed + worker as u64 * 7919 + client_offset);
    let worker_seed = a.seed.wrapping_add(worker as u64 * 1_000_003 + 1);
    let payload_for =
        |n: i64| make_payload(a.payload, a.doc_bytes, worker_seed.wrapping_add(n as u64));
    let mix = parse_op_mix(&a.op_mix)?;
    let mut conn = connect(&a.addr, &a.db)?;
    let coll = collection_name(&a.prefix, &a.client_id, worker);

    let mut counts = [0u64; 3];
    let mut docs = [0u64; 3];
    let mut errors = [0u64; 3];
    let mut hists = [Histogram::new(), Histogram::new(), Histogram::new()];
    let mut first_errors: Vec<String> = Vec::new();
    // Bounded so a pathological run cannot exhaust memory; 200k outliers is
    // far more than any diagnosis needs.
    let mut slow_ops: Vec<SlowOp> = Vec::new();
    const SLOW_CAP: usize = 200_000;

    // `n` values already present: the preload, plus whatever this worker
    // inserts as it goes. Reads and updates select uniformly from that range.
    let mut high = a.preload;
    // Force the connection open before the barrier so handshake cost lands in
    // setup, not in the first measured operation.
    conn.ping()?;

    if a.start_at > 0.0 {
        let delay = a.start_at - now_epoch_secs();
        if delay > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(delay));
        }
    }
    let started_at = now_epoch_secs();
    let deadline = Instant::now() + Duration::from_secs_f64(a.duration);

    while Instant::now() < deadline {
        let mut op = pick(&mix, rng.random::<f64>());
        if high <= 0 && matches!(op, Op::Find | Op::Update) {
            // Nothing to read yet (preload 0, insert-first mix): spend the tick
            // on an insert rather than a guaranteed-empty query.
            op = Op::Insert;
        }
        let idx = op.index();
        let t0 = Instant::now();
        let outcome = match op {
            Op::Insert => {
                let batch: Vec<_> = (0..a.batch_size.max(1))
                    .map(|i| make_document(high + i as i64, &payload_for(high + i as i64)))
                    .collect();
                let n_docs = batch.len() as u64;
                conn.insert(&coll, batch).map(|_| n_docs)
            }
            Op::Find => conn.find_by_n(&coll, rng.random_range(0..high)).map(|_| 1),
            Op::Update => conn
                .update_by_n(&coll, rng.random_range(0..high))
                .map(|_| 1),
        };
        match outcome {
            Ok(n_docs) => {
                if op == Op::Insert {
                    high += n_docs as i64;
                }
                let elapsed_ms = t0.elapsed().as_secs_f64() * 1e3;
                if a.slow_ms > 0.0 && elapsed_ms >= a.slow_ms && slow_ops.len() < SLOW_CAP {
                    slow_ops.push(SlowOp {
                        t: now_epoch_secs(),
                        op: op.name().to_string(),
                        ms: (elapsed_ms * 1000.0).round() / 1000.0,
                        worker,
                    });
                }
                hists[idx].record(elapsed_ms * 1e3);
                counts[idx] += 1;
                docs[idx] += n_docs;
            }
            Err(e) => {
                errors[idx] += 1;
                if first_errors.len() < 5 {
                    first_errors.push(format!("{}: {e}", op.name()));
                }
                // A broken connection would otherwise spin at full speed
                // producing errors; reconnect once and carry on, still counting
                // the failure.
                if let Ok(fresh) = connect(&a.addr, &a.db) {
                    conn = fresh;
                }
            }
        }
    }

    Ok(WorkerResult {
        started_at,
        elapsed_s: now_epoch_secs() - started_at,
        counts,
        docs,
        errors,
        hists,
        first_errors,
        slow_ops,
    })
}

fn cmd_run(a: &LoadArgs) -> BenchResult<()> {
    let cpu_before = cpu_times();
    let outcomes: Vec<BenchResult<WorkerResult>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..a.workers)
            .map(|w| scope.spawn(move || run_worker(a, w)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("worker thread panicked".to_string()))
            })
            .collect()
    });
    let cpu_busy = cpu_busy_pct(cpu_before, cpu_times());

    let mut failures: Vec<String> = Vec::new();
    let mut workers: Vec<WorkerResult> = Vec::new();
    for (idx, outcome) in outcomes.into_iter().enumerate() {
        match outcome {
            Ok(res) => workers.push(res),
            Err(e) => failures.push(format!("worker {idx}: {e}")),
        }
    }

    let mut merged = [Histogram::new(), Histogram::new(), Histogram::new()];
    let mut counts = [0u64; 3];
    let mut docs = [0u64; 3];
    let mut errors = [0u64; 3];
    let mut first_errors: Vec<String> = Vec::new();
    let mut slow_ops: Vec<SlowOp> = Vec::new();
    for res in &workers {
        for i in 0..3 {
            counts[i] += res.counts[i];
            docs[i] += res.docs[i];
            errors[i] += res.errors[i];
            merged[i].merge(&res.hists[i]);
        }
        first_errors.extend(res.first_errors.iter().cloned());
        slow_ops.extend(res.slow_ops.iter().cloned());
    }
    // Completion order across all workers, so periodicity is readable.
    slow_ops.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));

    // The measured window is the span each worker actually drove load for, not
    // this process's wall clock (which includes the barrier wait). Workers share
    // a barrier, so the longest is the honest denominator.
    let window = workers
        .iter()
        .map(|w| w.elapsed_s)
        .fold(0.0f64, f64::max)
        .max(1e-9);
    let starts: Vec<f64> = workers.iter().map(|w| w.started_at).collect();
    let total_ops: u64 = counts.iter().sum();
    let total_docs: u64 = docs.iter().sum();
    let total_errors: u64 = errors.iter().sum();

    let mut ops_map: BTreeMap<String, OpStats> = BTreeMap::new();
    for (i, name) in OP_NAMES.iter().enumerate() {
        ops_map.insert(
            name.to_string(),
            OpStats {
                count: counts[i],
                docs: docs[i],
                errors: errors[i],
                hist: merged[i].clone(),
            },
        );
    }

    let report = ClientReport {
        client_id: a.client_id.clone(),
        hostname: hostname(),
        uri: a.addr.clone(),
        workers: a.workers,
        duration_s: a.duration,
        requested_start_at: a.start_at,
        actual_start_at: starts
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min)
            .min(f64::MAX),
        start_skew_s: round3(
            starts.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                - starts.iter().cloned().fold(f64::INFINITY, f64::min),
        ),
        measured_window_s: round3(window),
        op_mix: a.op_mix.clone(),
        batch_size: a.batch_size,
        doc_bytes: a.doc_bytes,
        preload: a.preload,
        client_cpu_busy_pct: cpu_busy,
        totals: Totals {
            ops: total_ops,
            docs: total_docs,
            errors: total_errors,
            ops_per_sec: round1(total_ops as f64 / window),
            docs_per_sec: round1(total_docs as f64 / window),
        },
        ops: ops_map,
        first_errors: first_errors.into_iter().take(10).collect(),
        worker_failures: failures.clone(),
        slow_ops,
    };

    let text =
        serde_json::to_string_pretty(&report).map_err(|e| format!("serialising report: {e}"))?;
    std::fs::write(&a.out, text).map_err(|e| format!("writing {}: {e}", a.out))?;
    println!(
        "run ok: {total_ops} ops ({total_docs} docs) in {window:.1}s = {:.0} ops/s, {total_errors} errors",
        total_ops as f64 / window
    );
    for line in &failures {
        eprintln!("WARNING: {line}");
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("{} worker(s) failed", failures.len()))
    }
}

// -- sample -----------------------------------------------------------------

/// `(busy, total)` jiffies from `/proc/stat`, or `None` off Linux.
fn cpu_times() -> Option<(f64, f64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let line = text.lines().next()?;
    let fields: Vec<f64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|f| f.parse().ok())
        .collect();
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0.0);
    let total: f64 = fields.iter().sum();
    Some((total - idle, total))
}

fn cpu_busy_pct(before: Option<(f64, f64)>, after: Option<(f64, f64)>) -> Option<f64> {
    let (b, a) = (before?, after?);
    let (busy, total) = (a.0 - b.0, a.1 - b.1);
    if total > 0.0 {
        Some(round1(busy / total * 100.0))
    } else {
        None
    }
}

fn find_pid(process_name: &str) -> Option<u32> {
    for entry in std::fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{name}/comm")) {
            if comm.trim() == process_name {
                return name.parse().ok();
            }
        }
    }
    None
}

fn rss_kb(pid: Option<u32>) -> Option<u64> {
    let pid = pid?;
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn mem_available_kb() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn cmd_sample(args: &Args) -> BenchResult<()> {
    let duration = args.f64_or("--duration", 60.0)?;
    let interval = args.f64_or("--interval", 1.0)?;
    let process = args.str_or("--process", "secantusd-rs");
    let out = args.str_or("--out", "/tmp/do-server-sample.json");

    let mut samples: Vec<serde_json::Value> = Vec::new();
    let mut pid = find_pid(&process);
    let mut prev = cpu_times();
    let deadline = Instant::now() + Duration::from_secs_f64(duration);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs_f64(interval));
        let now = cpu_times();
        if pid.is_none() || !std::path::Path::new(&format!("/proc/{}", pid.unwrap_or(0))).exists() {
            pid = find_pid(&process);
        }
        samples.push(serde_json::json!({
            "t": round3(now_epoch_secs()),
            "cpu_busy_pct": cpu_busy_pct(prev, now),
            "rss_kb": rss_kb(pid),
            "mem_available_kb": mem_available_kb(),
        }));
        prev = now;
    }

    let cpus: Vec<f64> = samples
        .iter()
        .filter_map(|s| s.get("cpu_busy_pct").and_then(|v| v.as_f64()))
        .collect();
    let rss: Vec<u64> = samples
        .iter()
        .filter_map(|s| s.get("rss_kb").and_then(|v| v.as_u64()))
        .collect();
    let report = serde_json::json!({
        "process": process,
        "pid": pid,
        "interval_s": interval,
        "samples": samples,
        "summary": {
            "cpu_busy_pct_mean": if cpus.is_empty() { None } else {
                Some(round1(cpus.iter().sum::<f64>() / cpus.len() as f64)) },
            "cpu_busy_pct_max": cpus.iter().cloned().fold(None::<f64>, |acc, v| {
                Some(acc.map_or(v, |a: f64| a.max(v))) }),
            "rss_kb_max": rss.iter().max(),
            "ncpu": std::thread::available_parallelism().map(|n| n.get()).ok(),
        }
    });
    let text =
        serde_json::to_string_pretty(&report).map_err(|e| format!("serialising trace: {e}"))?;
    std::fs::write(&out, text).map_err(|e| format!("writing {out}: {e}"))?;
    println!(
        "sample ok: {} samples -> {out}",
        report["samples"].as_array().map_or(0, |a| a.len())
    );
    Ok(())
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

const USAGE: &str = "\
Usage: do-client <setup|run|sample> [options]

  setup   --addr HOST:PORT --client-id ID [--db D] [--prefix P] [--workers N]
          [--doc-bytes N] [--preload N] [--op-mix SPEC] [--batch-size N] [--keep-data]
  run     (the same options) --duration SECS [--start-at EPOCH] [--seed N] [--out PATH]
          [--slow-ms MS]   record every op at or above MS with its timestamp
          [--payload repeat|random]  payload entropy; random for storage measurements
  sample  [--duration SECS] [--interval SECS] [--process NAME] [--out PATH]
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--version") {
        println!("do-client {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if argv.is_empty() || argv[0] == "--help" || argv[0] == "-h" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let mut value_flags: Vec<&str> = VALUE_FLAGS.to_vec();
    value_flags.extend_from_slice(&EXTRA_SAMPLE_FLAGS);
    let args = match Args::parse(&argv, &BOOL_FLAGS, &value_flags) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.command.as_str() {
        "sample" => cmd_sample(&args),
        "setup" => load_args(&args).and_then(|a| cmd_setup(&a)),
        "run" => load_args(&args).and_then(|a| cmd_run(&a)),
        other => Err(format!("unknown subcommand {other:?}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
