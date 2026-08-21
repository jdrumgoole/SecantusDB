//! `secantusd-rs` — the standalone Rust server binary (R7).
//!
//! The non-Python entry point over the same crates the embedded
//! `_secantus_server` handle (R6) uses: parse args → open the WiredTiger
//! `secantus_storage::Storage` → wrap in the R4b `StorageAdapter` →
//! `secantus_server::bind` → print the bound address → block until
//! SIGINT/SIGTERM → clean stop. Startup mirrors `secantus-server-py`'s
//! constructor so both entry points drive an identical server.

// Fast global allocator — BSON materialization drives heavy alloc churn
// (tasks/rust-perf-findings.md, Finding 1); mimalloc cuts it across all paths.
// Behind the default `mimalloc` feature so the PGO **instrumented** stage-1 build
// can be built with `--no-default-features` (system allocator). Instrumenting
// mimalloc's own allocator internals crashes on arm64 macOS: the LLVM profiling
// counter update (`__llvm_profile_instrument_target`) runs *inside* mimalloc's
// first page allocation and re-enters the half-initialized global allocator
// (EXC_BAD_ACCESS). The optimized stage-3 build (and the shipped binary) keep
// mimalloc; the collected profile is only a hint, so the allocator mismatch
// between the stages is harmless.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bson::Timestamp;
use secantus_commands::{CursorRegistry, Storage as CmdStorage};
use secantus_server::args::{parse_args, CliArgs, Parsed};
use secantus_server::bind;
use secantus_server::config::resolve;
use secantus_storage::{wt_config, Storage, StorageOptions};
use secantus_storage_adapter::StorageAdapter;

const RESTORE_HELP: &str = "\
Usage: secantusd-rs restore --source PATH --target-dir PATH [--to-timestamp SECS[,ORD]]

Point-in-time recovery: rebuild a fresh data directory as the database was at a
target time by replaying a stopped server's oplog forward. The source must be a
stopped server's data directory or an extracted backup (a live data directory
can't be opened — WiredTiger holds a single-writer lock). Start a new server on
--target-dir afterwards.

  --source PATH         Stopped server's data dir (or extracted backup archive).
  --target-dir PATH     Fresh directory to rebuild into.
  --to-timestamp S[,O]  Recover to this cluster timestamp (seconds, optional
                        ordinal). Omit to replay the whole oplog ('latest').
  --preserve-oplog      Carry the replayed oplog onto the restored directory so a
                        change stream there can resume from before the restore
                        point. Default: a fresh oplog timeline (like mongorestore).
  --help                Show this help.
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("restore") {
        return match run_restore(&argv[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("secantusd-rs restore: {msg}");
                ExitCode::from(2)
            }
        };
    }
    let parsed = match parse_args(&argv) {
        Ok(Parsed::Run(pr)) => pr,
        Ok(Parsed::Help(text)) | Ok(Parsed::Version(text)) => {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("secantusd-rs: {msg}");
            eprintln!("Try 'secantusd-rs --help' for usage.");
            // argparse's exit code for bad arguments, matching the Python CLI.
            return ExitCode::from(2);
        }
    };

    // Layer the TOML file (explicit --config or auto-discovered) underneath the
    // CLI flags, then derive the resolved run args (TLS pairing + standalone →
    // replica_set_name). A config error (missing file, unknown key/table,
    // malformed TOML, inconsistent TLS) exits 2 like a bad argument.
    let (cfg, source) = match resolve(parsed.config_path.as_deref(), &parsed.overrides) {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("secantusd-rs: {msg}");
            return ExitCode::from(2);
        }
    };
    let cli = match CliArgs::from_resolved(&cfg) {
        Ok(cli) => cli,
        Err(msg) => {
            eprintln!("secantusd-rs: {msg}");
            return ExitCode::from(2);
        }
    };

    // Initialise logging to the resolved level BEFORE opening storage, so any
    // startup log records honour --log-level. `env_logger`'s default filter is
    // the resolved level; an explicit RUST_LOG still wins if the user sets it.
    init_logger(&cli.log_level);
    if let Some(src) = &source {
        log::info!("loaded config from {}", src.display());
    }

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("secantusd-rs: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Initialise `env_logger` with `level` (DEBUG/INFO/WARNING/ERROR from
/// --log-level) as the default filter. `RUST_LOG`, if set, overrides. Maps the
/// Python `logging` level names onto `log::LevelFilter` (`WARNING` → `Warn`).
fn init_logger(level: &str) {
    let filter = match level {
        "DEBUG" => log::LevelFilter::Debug,
        "INFO" => log::LevelFilter::Info,
        "WARNING" => log::LevelFilter::Warn,
        "ERROR" => log::LevelFilter::Error,
        // Unreachable — validated at parse time — but default to Info.
        _ => log::LevelFilter::Info,
    };
    env_logger::Builder::from_env(env_logger::Env::default())
        .filter_level(filter)
        .format_timestamp_secs()
        .init();
}

fn run(cli: CliArgs) -> Result<(), String> {
    // WiredTiger requires the home directory to exist; create it so any path
    // "just works" (matching the embedded handle).
    std::fs::create_dir_all(&cli.storage_path)
        .map_err(|e| format!("failed to create storage dir {}: {e}", cli.storage_path))?;

    // Open with the resolved WiredTiger knobs (--cache-size / --session-max /
    // --sync-on-commit). The daemon default is `1G` (matching the Python
    // server); the engine/library default via `Storage::open` stays `256M`.
    let wt = wt_config(
        &cli.cache_size,
        cli.session_max,
        cli.sync_on_commit,
        &cli.log_file_max,
    );
    // Storage write-path modes (--oplog-async / --oplog-nonlogged /
    // --data-nonlogged / --checkpoint-seconds): `None` defers to the matching
    // SECANTUS_* env var, so env-driven runs are unchanged.
    let opts = StorageOptions {
        wt_config: Some(wt),
        oplog_async: cli.oplog_async,
        oplog_nonlogged: cli.oplog_nonlogged,
        data_nonlogged: cli.data_nonlogged,
        checkpoint_seconds: cli.checkpoint_seconds,
        write_tickets: cli.write_tickets,
        ..StorageOptions::default()
    };
    let mut storage = Storage::open_with_options(&cli.storage_path, &opts)
        .map_err(|e| format!("failed to open storage at {}: {e:?}", cli.storage_path))?;
    // SECANTUS_DISABLE_OPLOG=1 turns oplog emission off entirely (no change
    // streams / PITR — the "drop the oplog for ~2× multi-writer throughput"
    // lever from docs/concurrency.md, previously embedded-only).
    storage.set_enable_oplog(std::env::var_os("SECANTUS_DISABLE_OPLOG").is_none());
    // Oplog retention window + hard entry cap (--oplog-retention-seconds /
    // --oplog-max-entries). Retention is seconds; truncate the float.
    storage.set_oplog_retention_seconds(cli.oplog_retention_seconds as i64);
    storage.set_oplog_max_entries(cli.oplog_max_entries);
    if let Some(dir) = &cli.oplog_archive_dir {
        storage.set_oplog_archive_dir(Some(dir.clone()));
    }

    // Keep an `Arc<Storage>` clone for the background maintenance threads
    // BEFORE the adapter takes ownership. The adapter, the threads, and (via
    // the adapter) every connection all share the one WiredTiger connection.
    let storage = Arc::new(storage);
    let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(storage.clone()));
    let cursors = Arc::new(CursorRegistry::new());
    let addr = cli.bind_addr();
    let mut running = bind(&addr, cli.server_config(), adapter, cursors)
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;

    // Background maintenance threads observe this flag and exit promptly (they
    // sleep in small increments) once shutdown is signalled.
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut workers: Vec<JoinHandle<()>> = Vec::new();

    // Periodic noop heartbeats: keep quiet change-stream cursors' resume tokens
    // inside the oplog window. 0 = disabled (the default, matching Python). The
    // same thread also opportunistically prunes the oplog to the retention /
    // count bounds — there is no other sweeper.
    if cli.noop_heartbeat_seconds > 0.0 {
        workers.push(spawn_interval(
            cli.noop_heartbeat_seconds,
            shutdown.clone(),
            {
                let storage = storage.clone();
                move || {
                    if let Err(e) = storage.emit_noop_heartbeat() {
                        log::warn!("noop heartbeat failed: {e:?}");
                    }
                    if let Err(e) = storage.prune_oplog(None) {
                        log::warn!("oplog prune failed: {e:?}");
                    }
                }
            },
        ));
    }

    // TTL sweeper: prune expired docs across all collections every N seconds
    // (mongod's default is 60s). 0 disables it. Mirrors the Python daemon.
    if cli.ttl_sweep_seconds > 0.0 {
        workers.push(spawn_interval(cli.ttl_sweep_seconds, shutdown.clone(), {
            let storage = storage.clone();
            move || {
                let now = bson::DateTime::from_millis(now_millis());
                if let Err(e) = storage.prune_ttl_all_collections(now) {
                    log::warn!("TTL sweep failed: {e:?}");
                }
            }
        }));
    }

    // The smoke test (and any wrapping launcher) reads this line to learn the
    // bound address, so it must hit stdout before we block.
    println!("secantusd-rs listening on {}", running.address());
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    // Block until SIGINT (Ctrl-C) or SIGTERM, then stop cleanly so WiredTiger
    // closes via drop.
    let (tx, rx) = mpsc::channel::<()>();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })
    .map_err(|e| format!("failed to install signal handler: {e}"))?;
    let _ = rx.recv();

    // Signal the maintenance threads first and join them so they've released
    // their storage refs (and aren't mid-write) before the server drains and
    // WiredTiger closes.
    shutdown.store(true, Ordering::SeqCst);
    for w in workers {
        let _ = w.join();
    }
    running.stop();
    // Write the PGO profile to a KNOWN path before exit. On the arm64-macOS CI
    // runner the profiling runtime never wires up its env-driven (LLVM_PROFILE_FILE)
    // at-exit write — even a clean `--version` exit produces no `.profraw` — so we
    // set the filename programmatically and flush the counters ourselves. Compiled
    // in only for the instrumented stage-1 build (the `pgo-instrument` feature);
    // the symbols exist only under `-Cprofile-generate`. Path comes from
    // `SECANTUS_PGO_OUT` (set by the release workflow); no-op if unset.
    #[cfg(feature = "pgo-instrument")]
    if let Ok(path) = std::env::var("SECANTUS_PGO_OUT") {
        if let Ok(cpath) = std::ffi::CString::new(path) {
            extern "C" {
                fn __llvm_profile_set_filename(name: *const std::os::raw::c_char);
                fn __llvm_profile_write_file() -> std::os::raw::c_int;
            }
            unsafe {
                __llvm_profile_set_filename(cpath.as_ptr());
                __llvm_profile_write_file();
            }
        }
    }
    Ok(())
}

/// Milliseconds since the Unix epoch (for the TTL sweep's `now`).
fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Spawn a background thread that runs `task` every `period_seconds`, waking
/// every 200ms to check `shutdown` so teardown is prompt. The first run fires
/// one full period after start (matching the Python daemon's timer). Returns a
/// `JoinHandle` so `run` can join it on shutdown.
fn spawn_interval<F>(period_seconds: f64, shutdown: Arc<AtomicBool>, mut task: F) -> JoinHandle<()>
where
    F: FnMut() + Send + 'static,
{
    let period = Duration::from_secs_f64(period_seconds);
    let tick = Duration::from_millis(200);
    thread::spawn(move || {
        let mut next = Instant::now() + period;
        while !shutdown.load(Ordering::SeqCst) {
            if Instant::now() >= next {
                task();
                next = Instant::now() + period;
            }
            thread::sleep(tick);
        }
    })
}

/// Parse `SECS[,ORD]` into a BSON `Timestamp` (ordinal defaults to 0).
fn parse_ts(s: &str) -> Result<Timestamp, String> {
    let (secs, ord) = s.split_once(',').unwrap_or((s, "0"));
    let bad = || "--to-timestamp must be SECS[,ORD]".to_string();
    Ok(Timestamp {
        time: secs.trim().parse().map_err(|_| bad())?,
        increment: ord.trim().parse().map_err(|_| bad())?,
    })
}

/// Resolve a flag's value: the inline `--flag=value`, or else the next argv slot
/// (`--flag value`), advancing `i` past it.
fn flag_value(
    args: &[String],
    i: &mut usize,
    inline: Option<String>,
    key: &str,
) -> Result<String, String> {
    if let Some(v) = inline {
        return Ok(v);
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{key} requires a value"))
}

/// `secantusd-rs restore` — replay a stopped source's oplog into a fresh target.
/// Hand-rolled arg parsing (like the server), accepting both `--flag value` and
/// `--flag=value`.
fn run_restore(args: &[String]) -> Result<(), String> {
    let mut source: Option<String> = None;
    let mut target_dir: Option<String> = None;
    let mut to_ts: Option<Timestamp> = None;
    let mut preserve_oplog = false;
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = match args[i].split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (args[i].clone(), None),
        };
        match key.as_str() {
            "--source" => source = Some(flag_value(args, &mut i, inline, &key)?),
            "--target-dir" => target_dir = Some(flag_value(args, &mut i, inline, &key)?),
            "--to-timestamp" => to_ts = Some(parse_ts(&flag_value(args, &mut i, inline, &key)?)?),
            "--preserve-oplog" => preserve_oplog = true,
            "--help" | "-h" => {
                print!("{RESTORE_HELP}");
                return Ok(());
            }
            other => return Err(format!("unknown restore option: {other}")),
        }
        i += 1;
    }
    let source = source.ok_or("--source is required")?;
    let target_dir = target_dir.ok_or("--target-dir is required")?;
    // A directory of base snapshots + oplog segments is a PITR v2 archive;
    // anything else (a backup dir / stopped data dir) is a v1 source.
    let stats = if secantus_storage::pitr_archive::is_archive_dir(&source) {
        secantus_storage::pitr_archive::restore_from_archive_dir(
            &source,
            &target_dir,
            to_ts,
            None,
            preserve_oplog,
        )
    } else {
        secantus_storage::replay::restore_to_timestamp(
            &source,
            &target_dir,
            to_ts,
            None,
            preserve_oplog,
        )
    }
    .map_err(|e| format!("{e:?}"))?;
    println!(
        "Restored {} operations (through oplog seq {}) into {}.\n\
         Start a server on it: secantusd-rs --storage-path {}",
        stats.ops_applied, stats.last_seq, target_dir, target_dir
    );
    Ok(())
}
