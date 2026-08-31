//! `secantusd-pg` -- the standalone PostgreSQL-wire server (P1 slice).

use std::sync::mpsc;
use std::sync::Arc;

use secantus_pgserver::{HandlerFactory, PgHandler};
use secantus_storage::Storage;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let home = args.next().unwrap_or_else(|| "./secantus-pg-data".into());
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:25434".into());

    let storage = Arc::new(Storage::open(&home)?);
    let listener = TcpListener::bind(&addr).await?;
    // One line, flushed, so a harness can wait for readiness.
    println!("secantusd-pg listening on {addr} storage={home}");

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
                let handler = Arc::new(PgHandler::new(storage.clone(), "postgres"));
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
