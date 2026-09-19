//! `secantusd-pg` -- the standalone PostgreSQL-wire server (P1 slice).
//!
//! A CLI wrapper, nothing more: argument parsing, the readiness line, and the
//! signal wait. The accept loop, the shutdown drain, and the storage ownership
//! that makes the close-checkpoint run all live in `secantus_pgserver::bind`,
//! which the embedded Python handle (`_secantus_server`'s `PgServer`) calls
//! too -- so there is one serve path, not two.

use std::sync::mpsc;
use std::sync::Arc;

use secantus_pgserver::{bind, DatabaseRegistry};
use secantus_storage::Storage;

/// `--version` output: the version, and the source tree it was built from.
fn version_text() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match option_env!("SECANTUS_SOURCE_TREE").unwrap_or("") {
        "" => format!("secantusd-pg {version}\n"),
        tree => format!("secantusd-pg {version}\ntree: {tree}\n"),
    }
}

// NOT `#[tokio::main]`: `bind` owns the runtime, and `RunningPgServer::stop`
// drops it -- which panics if it happens inside a runtime context. `main` is a
// plain blocking function that waits on a signal.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `secantusd-pg [<home> [<addr>]] [--database NAME]...`: every
    // `--database` is one more name a client may connect to without a
    // `CREATE DATABASE` first; the builtin `postgres` / `template1` always are.
    let mut home: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut databases: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        // `--version` / `--help` BEFORE the positional fallthrough. Without
        // them, `--version` fell through to `home` and the server tried to open
        // a WiredTiger database in a directory called `--version`, reporting
        // `WT_TRY_SALVAGE: database corruption detected` -- an alarming answer
        // to a question every binary is expected to answer. The release
        // workflow sanity-checks the artifact with `--version`, so this is load
        // bearing rather than a nicety.
        if arg == "--version" || arg == "-V" {
            // Two lines: the version for "what did I install?", and the source
            // tree for a bug report. The crate version moves at release
            // cadence, so hundreds of commits share one string and it cannot
            // identify a build on its own. The tree line is OMITTED, not
            // blank, when built without git.
            print!("{}", version_text());
            return Ok(());
        } else if arg == "--help" || arg == "-h" {
            println!(
                "secantusd-pg {} -- standalone PostgreSQL-wire server (SecantusDB)\n\
                 \n\
                 USAGE:\n    \
                 secantusd-pg [<storage-path> [<host:port>]] [--database NAME]...\n\
                 \n\
                 ARGS:\n    \
                 <storage-path>  WiredTiger home directory (default: ./secantus-pg-data)\n    \
                 <host:port>     Bind address (default: 127.0.0.1:25434). Port 0 picks\n                    \
                 an ephemeral port and prints the one it bound.\n\
                 \n\
                 OPTIONS:\n    \
                 --database NAME  A database a client may connect to without CREATE\n                     \
                 DATABASE first. Repeatable. `postgres` and `template1`\n                     \
                 always exist.\n    \
                 -V, --version    Print version and exit\n    \
                 -h, --help       Print this help and exit",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        } else if arg == "--database" {
            let name = args.next().ok_or("--database needs a name")?;
            databases.push(name);
        } else if let Some(name) = arg.strip_prefix("--database=") {
            databases.push(name.to_string());
        } else if home.is_none() {
            home = Some(arg);
        } else if addr.is_none() {
            addr = Some(arg);
        } else {
            return Err(format!("unexpected argument: {arg}").into());
        }
    }
    let home = home.unwrap_or_else(|| "./secantus-pg-data".into());
    let addr = addr.unwrap_or_else(|| "127.0.0.1:25434".into());
    let databases = Arc::new(DatabaseRegistry::new("postgres", databases));

    // Create the home if it is missing, which is what `secantusd-rs` does
    // (`--storage-path … created if missing`). Without it a first run against a
    // fresh path failed inside WiredTiger with
    // `WiredTiger.lock: handle-open: open: No such file or directory` and a
    // `WT_TRY_SALVAGE: database corruption detected` -- a frightening answer to
    // "I pointed it at a new directory". Found by the release smoke test, which
    // is the first thing to drive this binary the way a new user would.
    std::fs::create_dir_all(&home)
        .map_err(|e| format!("could not create storage path {home}: {e}"))?;
    let storage = Storage::open(&home)?;
    let mut server = bind(&addr, storage, databases)?;

    // One line, flushed, so a harness can wait for readiness. It reports the
    // address the listener actually BOUND, not the one requested, so that
    // `127.0.0.1:0` is usable: the kernel names the port and this line is how
    // the caller learns it. A harness that instead probes for a free port and
    // passes it in cannot be made race-free -- the probe socket must close
    // before the child binds.
    println!(
        "secantusd-pg listening on {} storage={home}",
        server.address()
    );

    // Block until SIGINT or SIGTERM, then stop cleanly so WiredTiger closes.
    // WITHOUT THIS the process dies with no checkpoint and every acknowledged
    // write since the last one is lost -- measured 2026-08-31: a SIGTERM after
    // CREATE TABLE + INSERT left the catalog document and the rows both gone,
    // while the client had been told the writes succeeded. `stop` drains the
    // connections and drops the last `Arc<Storage>`, which is where the
    // close-checkpoint runs.
    let (tx, rx) = mpsc::channel::<()>();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })?;
    let _ = rx.recv();

    server.stop();
    Ok(())
}
