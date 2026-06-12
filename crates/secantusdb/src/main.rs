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

use secantus_commands::{CursorRegistry, Storage as CmdStorage};
use secantus_server::args::{parse_args, CliArgs, Parsed};
use secantus_server::bind;
use secantus_storage::Storage;
use secantus_storage_adapter::StorageAdapter;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
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
