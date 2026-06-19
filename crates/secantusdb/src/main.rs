//! `secantusdb` — the standalone Rust server binary (R7).
//!
//! The non-Python entry point over the same crates the embedded
//! `_secantus_server` handle (R6) uses: parse args → open the WiredTiger
//! `secantus_storage::Storage` → wrap in the R4b `StorageAdapter` →
//! `secantus_server::bind` → print the bound address → block until
//! SIGINT/SIGTERM → clean stop. Startup mirrors `secantus-server-py`'s
//! constructor so both entry points drive an identical server.

use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::Arc;

use bson::Timestamp;
use secantus_commands::{CursorRegistry, Storage as CmdStorage};
use secantus_server::args::{parse_args, CliArgs, Parsed};
use secantus_server::bind;
use secantus_storage::Storage;
use secantus_storage_adapter::StorageAdapter;

const RESTORE_HELP: &str = "\
Usage: secantusdb restore --source PATH --target-dir PATH [--to-timestamp SECS[,ORD]]

Point-in-time recovery: rebuild a fresh data directory as the database was at a
target time by replaying a stopped server's oplog forward. The source must be a
stopped server's data directory or an extracted backup (a live data directory
can't be opened — WiredTiger holds a single-writer lock). Start a new server on
--target-dir afterwards.

  --source PATH         Stopped server's data dir (or extracted backup archive).
  --target-dir PATH     Fresh directory to rebuild into.
  --to-timestamp S[,O]  Recover to this cluster timestamp (seconds, optional
                        ordinal). Omit to replay the whole oplog ('latest').
  --help                Show this help.
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("restore") {
        return match run_restore(&argv[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("secantusdb restore: {msg}");
                ExitCode::from(2)
            }
        };
    }
    let cli = match parse_args(&argv) {
        Ok(Parsed::Run(cli)) => cli,
        Ok(Parsed::Help(text)) | Ok(Parsed::Version(text)) => {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("secantusdb: {msg}");
            eprintln!("Try 'secantusdb --help' for usage.");
            // argparse's exit code for bad arguments, matching the Python CLI.
            return ExitCode::from(2);
        }
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("secantusdb: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: CliArgs) -> Result<(), String> {
    // WiredTiger requires the home directory to exist; create it so any path
    // "just works" (matching the embedded handle).
    std::fs::create_dir_all(&cli.storage_path)
        .map_err(|e| format!("failed to create storage dir {}: {e}", cli.storage_path))?;
    let mut storage = Storage::open(&cli.storage_path)
        .map_err(|e| format!("failed to open storage at {}: {e:?}", cli.storage_path))?;
    storage.set_enable_oplog(true);

    let adapter: Arc<dyn CmdStorage> = Arc::new(StorageAdapter::new(Arc::new(storage)));
    let cursors = Arc::new(CursorRegistry::new());
    let addr = cli.bind_addr();
    let mut running = bind(&addr, cli.server_config(), adapter, cursors)
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;

    // The smoke test (and any wrapping launcher) reads this line to learn the
    // bound address, so it must hit stdout before we block.
    println!("secantusdb listening on {}", running.address());
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

    running.stop();
    Ok(())
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

/// `secantusdb restore` — replay a stopped source's oplog into a fresh target.
/// Hand-rolled arg parsing (like the server), accepting both `--flag value` and
/// `--flag=value`.
fn run_restore(args: &[String]) -> Result<(), String> {
    let mut source: Option<String> = None;
    let mut target_dir: Option<String> = None;
    let mut to_ts: Option<Timestamp> = None;
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
    let stats = secantus_storage::replay::restore_to_timestamp(&source, &target_dir, to_ts, None)
        .map_err(|e| format!("{e:?}"))?;
    println!(
        "Restored {} operations (through oplog seq {}) into {}.\n\
         Start a server on it: secantusdb --storage-path {}",
        stats.ops_applied, stats.last_seq, target_dir, target_dir
    );
    Ok(())
}
