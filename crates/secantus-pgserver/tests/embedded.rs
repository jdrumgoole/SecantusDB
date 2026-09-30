//! The embedded serve path: `bind` → `address()` → a client → `stop()`.
//!
//! The durability test here is the reason this file exists. `stop()` is what
//! runs WiredTiger's close-checkpoint, and if the handle did not OWN the store
//! the checkpoint would silently not happen -- the failure mode measured on
//! 2026-08-31, where a `CREATE TABLE` + `INSERT` the client had been told
//! succeeded was gone after the process died. So the test acknowledges writes
//! over the real wire, stops, reopens the same home in a fresh server, and
//! reads them back.
//!
//! The reopen has two teeth, both of which were checked by making `stop()` leak
//! the store: WiredTiger refuses a second `wiredtiger_open` on a home this
//! process still holds (`Resource busy`), and the rows have to come back.

use std::sync::Arc;

use secantus_pgserver::{bind, DatabaseRegistry, RunningPgServer};
use secantus_storage::Storage;
use tempfile::TempDir;
use tokio::runtime::Runtime;

fn start(home: &std::path::Path) -> RunningPgServer {
    let storage = Storage::open(home.to_str().expect("utf-8 home")).expect("open storage");
    let databases = Arc::new(DatabaseRegistry::new("postgres", Vec::new()));
    bind("127.0.0.1:0", storage, databases).expect("bind")
}

/// Run `sql` statements against the server and return the rows of the last
/// `query`, if any. The client runtime is the TEST's, never the server's.
/// (These tests call `stop()` outside `block_on`; the async tests at the end
/// of the file cover stopping and dropping INSIDE a runtime.)
fn run(rt: &Runtime, server: &RunningPgServer, statements: &[&str]) {
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(&server.dsn(), tokio_postgres::NoTls)
            .await
            .expect("connect");
        let handle = tokio::spawn(async move {
            let _ = connection.await;
        });
        for sql in statements {
            client.simple_query(sql).await.expect(sql);
        }
        drop(client);
        let _ = handle.await;
    });
}

fn query_i32_column(rt: &Runtime, server: &RunningPgServer, sql: &str) -> Vec<i32> {
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(&server.dsn(), tokio_postgres::NoTls)
            .await
            .expect("connect");
        let handle = tokio::spawn(async move {
            let _ = connection.await;
        });
        let rows = client.query(sql, &[]).await.expect(sql);
        let values: Vec<i32> = rows.iter().map(|r| r.get(0)).collect();
        drop(client);
        let _ = handle.await;
        values
    })
}

#[test]
fn binds_an_ephemeral_port_and_serves_it() {
    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());

    // `127.0.0.1:0` resolved to a real port BEFORE `bind` returned -- that is
    // what makes the ephemeral-port form usable, with no window in which the
    // port is chosen but not yet listened on.
    let address = server.address();
    assert_ne!(address.port(), 0, "kernel-assigned port not reported back");

    let rt = Runtime::new().expect("runtime");
    run(&rt, &server, &["SELECT 1"]);

    server.stop();

    // The listener is gone once stopped.
    assert!(
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(500))
            .and_then(|s| {
                // A connect can still be accepted by a lingering backlog entry
                // on some platforms; a read then returns EOF immediately.
                use std::io::Read;
                s.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;
                let mut buf = [0u8; 1];
                let n = (&s).read(&mut buf)?;
                if n == 0 {
                    Err(std::io::Error::other("eof"))
                } else {
                    Ok(())
                }
            })
            .is_err(),
        "server still serving after stop()"
    );
}

#[test]
fn acknowledged_writes_survive_stop_and_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let rt = Runtime::new().expect("runtime");

    let mut server = start(dir.path());
    run(
        &rt,
        &server,
        &[
            "CREATE TABLE survivors (n int)",
            "INSERT INTO survivors VALUES (1), (2), (3)",
        ],
    );
    // The close-checkpoint runs here, because the handle owns the store.
    server.stop();

    // A NEW server over the same home: both the catalog entry for the table and
    // its rows have to be there. Losing either is silent data loss -- the
    // client was told both writes succeeded.
    let reopened = start(dir.path());
    let rows = query_i32_column(&rt, &reopened, "SELECT n FROM survivors ORDER BY n");
    assert_eq!(
        rows,
        vec![1, 2, 3],
        "rows acknowledged before stop() did not survive"
    );
    drop(reopened);
}

/// A pooled PostgreSQL connection never hangs up on its own, so `stop()` has to
/// TELL it to finish rather than wait for it. Before the shutdown watch, an
/// open connection made every `stop()` block for the full drain timeout --
/// measured at 10.8s from Python, which is not a one-or-two-line ergonomic.
#[test]
fn stop_does_not_wait_on_an_idle_connection() {
    let dir = TempDir::new().expect("tempdir");
    let rt = Runtime::new().expect("runtime");
    let mut server = start(dir.path());

    // A connection deliberately left OPEN across the stop.
    let client = rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(&server.dsn(), tokio_postgres::NoTls)
            .await
            .expect("connect");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .simple_query("CREATE TABLE held (n int)")
            .await
            .expect("create");
        client
            .simple_query("INSERT INTO held VALUES (1)")
            .await
            .expect("insert");
        client
    });

    let started = std::time::Instant::now();
    server.stop();
    let elapsed = started.elapsed();
    drop(client);
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "stop() waited {elapsed:?} on an idle connection"
    );

    // And it still checkpointed on the way out.
    let reopened = start(dir.path());
    let rows = query_i32_column(&rt, &reopened, "SELECT n FROM held");
    assert_eq!(rows, vec![1]);
    drop(reopened);
}

#[test]
fn stop_is_idempotent_and_drop_is_safe() {
    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());
    server.stop();
    server.stop();
    server.stop();
    // Drop after an explicit stop must not double-close the store.
    drop(server);

    // And the store is still openable afterwards, which it would not be if the
    // teardown had left WiredTiger's lock or its files in a bad state.
    let again = start(dir.path());
    drop(again);
}

/// Serve, write, and let the handle go out of scope -- all INSIDE an async
/// test, which is how a Rust caller writes one. Then reopen the same home
/// (still inside the runtime) and read the rows back, so a drop that returned
/// without checkpointing fails here rather than passing quietly.
///
/// `stop()` shuts down the server's OWN tokio runtime, and tokio refuses that
/// from an async context ("Cannot drop a runtime in a context where blocking is
/// not allowed"), so this panicked in `Drop`. The helpers above keep every
/// `stop()` outside `block_on` for exactly that reason; a user would not know to.
async fn serve_write_and_drop_inside_a_runtime() {
    let dir = TempDir::new().expect("tempdir");
    {
        let server = start(dir.path());
        let (client, connection) = tokio_postgres::connect(&server.dsn(), tokio_postgres::NoTls)
            .await
            .expect("connect");
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute("CREATE TABLE kept (n int); INSERT INTO kept VALUES (7)")
            .await
            .expect("write");
        drop(client);
        let _ = driver.await;
        drop(server);
    }
    let mut reopened = start(dir.path());
    let (client, connection) = tokio_postgres::connect(&reopened.dsn(), tokio_postgres::NoTls)
        .await
        .expect("reconnect");
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let rows = client.query("SELECT n FROM kept", &[]).await.expect("read");
    let values: Vec<i32> = rows.iter().map(|r| r.get(0)).collect();
    assert_eq!(
        values,
        vec![7],
        "a write acknowledged before the drop was lost"
    );
    drop(client);
    let _ = driver.await;
    // An explicit stop() inside the runtime, then the drop that follows it.
    reopened.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_inside_a_current_thread_runtime_is_safe() {
    serve_write_and_drop_inside_a_runtime().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_inside_a_multi_thread_runtime_is_safe() {
    serve_write_and_drop_inside_a_runtime().await;
}
