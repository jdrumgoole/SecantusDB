//! `secantusd-pg` -- the standalone PostgreSQL-wire server (P1 slice).

use std::sync::mpsc;
use std::sync::Arc;

use secantus_pgserver::{DatabaseRegistry, HandlerFactory, PgHandler};
use secantus_storage::Storage;
use tokio::net::TcpListener;

/// `--version` output: the version, and the source tree it was built from.
fn version_text() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match option_env!("SECANTUS_SOURCE_TREE").unwrap_or("") {
        "" => format!("secantusd-pg {version}\n"),
        tree => format!("secantusd-pg {version}\ntree: {tree}\n"),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let storage = Arc::new(Storage::open(&home)?);
    let listener = TcpListener::bind(&addr).await?;
    // One line, flushed, so a harness can wait for readiness. It reports the
    // address the listener actually BOUND, not the one requested, so that
    // `127.0.0.1:0` is usable: the kernel names the port and this line is how
    // the caller learns it. A harness that instead probes for a free port and
    // passes it in cannot be made race-free -- the probe socket must close
    // before the child binds.
    let bound = listener.local_addr()?;
    println!("secantusd-pg listening on {bound} storage={home}");

    // Serve until a signal arrives. The accept loop runs as a task so the main
    // task can wait on the signal and then close storage.
    let serving = {
        let storage = storage.clone();
        tokio::spawn(async move {
            loop {
                let (sock, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let handler = Arc::new(PgHandler::new(storage.clone(), databases.clone()));
                tokio::spawn(async move {
                    let _ = pgwire::tokio::process_socket(
                        sock,
                        None,
                        Arc::new(HandlerFactory(handler)),
                    )
                    .await;
                });
            }
        })
    };

    // Block until SIGINT or SIGTERM, then stop cleanly so WiredTiger closes via
    // drop. WITHOUT THIS the process dies with no checkpoint and every
    // acknowledged write since the last one is lost -- measured 2026-08-31:
    // a SIGTERM after CREATE TABLE + INSERT left the catalog document and the
    // rows both gone, while the client had been told the writes succeeded.
    let (tx, rx) = mpsc::channel::<()>();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })?;
    let _ = tokio::task::spawn_blocking(move || rx.recv()).await;

    serving.abort();
    // Drop the last `Arc` so `Storage`'s own close (and its checkpoint) runs
    // before the process exits.
    drop(serving);
    match Arc::try_unwrap(storage) {
        Ok(s) => drop(s),
        Err(still_shared) => drop(still_shared),
    }
    Ok(())
}
