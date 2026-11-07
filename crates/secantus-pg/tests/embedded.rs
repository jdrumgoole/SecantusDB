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

use secantus_pg::{bind, DatabaseRegistry, RunningPgServer};
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

/// READ COMMITTED blocks that have written are moved onto a fresh snapshot
/// at a statement's start whenever anything committed since -- here,
/// constantly, from autocommit writers on other connections. Every move
/// replays the block's writes; none of it may fail a statement PostgreSQL
/// would run (a 40001 here was the regression), lose a write, or apply one
/// twice. The counter row is updated by both sides, so a lost or doubled
/// update shows in its final value.
#[test]
fn concurrent_writers_under_the_read_committed_move() {
    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());
    let rt = Runtime::new().expect("runtime");
    run(
        &rt,
        &server,
        &[
            "CREATE TABLE noise (k int)",
            "CREATE TABLE hot (id int PRIMARY KEY, n int)",
            "INSERT INTO hot SELECT g, 0 FROM generate_series(1, 8) g",
        ],
    );
    let dsn = server.dsn();
    let (noise_n, mover_n) = rt.block_on(async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        let mut tasks = Vec::new();
        for w in 0..3i32 {
            let dsn = dsn.clone();
            tasks.push(tokio::spawn(async move {
                let (c, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
                    .await
                    .expect("connect");
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                let mut n = 0i64;
                while std::time::Instant::now() < deadline {
                    c.batch_execute(&format!(
                        "INSERT INTO noise VALUES ({w}); UPDATE hot SET n = n + 1 WHERE id = {}",
                        n % 8 + 1
                    ))
                    .await
                    .expect("autocommit writer");
                    n += 1;
                }
                (n, 0i64)
            }));
        }
        for m in 0..3i32 {
            let dsn = dsn.clone();
            tasks.push(tokio::spawn(async move {
                let (c, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
                    .await
                    .expect("connect");
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                let mut j = 0i64;
                while std::time::Instant::now() < deadline {
                    let t = format!("m{m}_{j}");
                    c.batch_execute(&format!(
                        "CREATE TABLE {t} (n int); INSERT INTO {t} VALUES (7)"
                    ))
                    .await
                    .expect("create in an implicit block");
                    c.batch_execute("BEGIN").await.expect("begin");
                    for step in [
                        format!("INSERT INTO {t} VALUES (8)"),
                        format!("UPDATE hot SET n = n + 1 WHERE id = {}", j % 8 + 1),
                        format!("INSERT INTO {t} VALUES (9)"),
                    ] {
                        c.batch_execute(&step).await.expect("statement in a block");
                    }
                    c.batch_execute("COMMIT").await.expect("commit");
                    let rows = c
                        .query(&format!("SELECT count(*) FROM {t}"), &[])
                        .await
                        .expect("count");
                    assert_eq!(rows[0].get::<_, i64>(0), 3, "{t}");
                    j += 1;
                }
                (0i64, j)
            }));
        }
        let mut totals = (0i64, 0i64);
        for t in tasks {
            let (a, b) = t.await.expect("task");
            totals.0 += a;
            totals.1 += b;
        }
        totals
    });
    let hot = query_i32_column(&rt, &server, "SELECT n FROM hot");
    let sum: i64 = hot.iter().map(|v| i64::from(*v)).sum();
    assert_eq!(
        sum,
        noise_n + mover_n,
        "an update was lost or applied twice"
    );
    let noise = query_i32_column(&rt, &server, "SELECT k FROM noise");
    assert_eq!(noise.len() as i64, noise_n);
    drop(rt);
    server.stop();
}

/// The shared-row-lock table is process-wide, and two stores open in one
/// process name their rows alike (`postgres`, the collection, the RecordId).
/// A FOR SHARE lock -- or the guard a moving transaction puts on its rows --
/// in one store blocked a write of the same-named row in the OTHER: under the
/// READ COMMITTED move that surfaced as a 40001 on a lone client
/// (`dropping_inside_a_multi_thread_runtime_is_safe`, beside the stress test
/// above). Rows are keyed by store now.
#[test]
fn a_row_lock_in_one_store_does_not_block_another_store() {
    let (da, db) = (
        TempDir::new().expect("tempdir"),
        TempDir::new().expect("tempdir"),
    );
    let mut a = start(da.path());
    let mut b = start(db.path());
    let rt = Runtime::new().expect("runtime");
    for s in [&a, &b] {
        run(
            &rt,
            s,
            &[
                "CREATE TABLE t (id int PRIMARY KEY, n int)",
                "INSERT INTO t VALUES (1, 0)",
            ],
        );
    }
    let (adsn, bdsn) = (a.dsn(), b.dsn());
    rt.block_on(async move {
        let connect = |dsn: String| async move {
            let (c, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
                .await
                .expect("connect");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            c
        };
        let ca = connect(adsn).await;
        let cb = connect(bdsn).await;
        ca.batch_execute("BEGIN; SELECT n FROM t WHERE id = 1 FOR SHARE")
            .await
            .expect("share lock in store A");
        cb.batch_execute("SET lock_timeout = '1s'; UPDATE t SET n = 1 WHERE id = 1")
            .await
            .expect("a write in store B is not blocked by store A's lock");
        ca.batch_execute("COMMIT").await.expect("commit");
    });
    drop(rt);
    a.stop();
    b.stop();
}

/// A READ COMMITTED block sees every commit made before its statement,
/// however much it has written: batch 54 left a block past 256 written
/// entries on its first write's snapshot. And a commit to a table the
/// statement does not read (or a sequence advance) does not make it replay
/// its writes -- the moves that remain are the ones a statement needs.
#[test]
fn a_long_read_committed_block_sees_later_commits() {
    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());
    let rt = Runtime::new().expect("runtime");
    run(
        &rt,
        &server,
        &[
            "CREATE TABLE t (id serial PRIMARY KEY, data text)",
            "CREATE TABLE other (id serial PRIMARY KEY, v int)",
        ],
    );
    let dsn = server.dsn();
    rt.block_on(async move {
        let connect = || async {
            let (c, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
                .await
                .expect("connect");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            c
        };
        let a = connect().await;
        let b = connect().await;
        a.batch_execute("BEGIN").await.unwrap();
        for _ in 0..300 {
            // Each draws from the serial's sequence, advanced (and committed)
            // outside the block.
            a.execute("INSERT INTO t (data) VALUES ('a')", &[])
                .await
                .unwrap();
        }
        b.batch_execute("INSERT INTO t (data) VALUES ('b'); INSERT INTO other (v) VALUES (1)")
            .await
            .unwrap();
        // This INSERT reads `t` (its key), which b wrote: the block moves
        // onto a fresh snapshot, its 300 writes replayed, and carries on.
        a.execute("INSERT INTO t (data) VALUES ('a')", &[])
            .await
            .unwrap();
        let seen_t: i64 = a
            .query_one("SELECT count(*) FROM t WHERE data = 'b'", &[])
            .await
            .unwrap()
            .get(0);
        let seen_other: i64 = a
            .query_one("SELECT count(*) FROM other", &[])
            .await
            .unwrap()
            .get(0);
        let own: i64 = a
            .query_one("SELECT count(*) FROM t WHERE data = 'a'", &[])
            .await
            .unwrap()
            .get(0);
        a.batch_execute("COMMIT").await.unwrap();
        assert_eq!((seen_t, seen_other, own), (1, 1, 301));
    });
    server.stop();
}

/// An INSERT into a table whose only unique key is its primary key keeps its
/// block's snapshot across another session's INSERTs there (a clash on the
/// key is a write conflict, re-run to PostgreSQL's 23505) but not across an
/// UPDATE or DELETE: re-inserting a key another session deleted succeeds, as
/// on PostgreSQL 15.
#[test]
fn a_block_insert_sees_concurrent_deletes_and_duplicate_keys() {
    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());
    let rt = Runtime::new().expect("runtime");
    run(
        &rt,
        &server,
        &[
            "CREATE TABLE d (id int PRIMARY KEY, v text)",
            "INSERT INTO d VALUES (1, 'old'), (2, 'x')",
        ],
    );
    let dsn = server.dsn();
    rt.block_on(async move {
        let connect = || async {
            let (c, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
                .await
                .expect("connect");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            c
        };
        let a = connect().await;
        let b = connect().await;
        a.batch_execute("BEGIN; INSERT INTO d VALUES (10, 'a')")
            .await
            .unwrap();
        b.batch_execute("DELETE FROM d WHERE id = 1").await.unwrap();
        a.batch_execute("INSERT INTO d VALUES (1, 'new')")
            .await
            .expect("the deleted key is free again");
        a.batch_execute("COMMIT").await.unwrap();

        a.batch_execute("BEGIN; INSERT INTO d VALUES (11, 'a')")
            .await
            .unwrap();
        b.batch_execute("INSERT INTO d VALUES (3, 'b')")
            .await
            .unwrap();
        let err = a
            .batch_execute("INSERT INTO d VALUES (3, 'a')")
            .await
            .expect_err("a committed duplicate key");
        assert_eq!(
            err.code(),
            Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
        );
        a.batch_execute("ROLLBACK").await.unwrap();
        let rows: Vec<(i32, String)> = b
            .query("SELECT id, v FROM d ORDER BY id", &[])
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        assert_eq!(
            rows,
            vec![
                (1, "new".to_string()),
                (2, "x".to_string()),
                (3, "b".to_string()),
                (10, "a".to_string())
            ]
        );
    });
    server.stop();
}

/// A sequence is not transactional: inside a READ COMMITTED block that has
/// written, another session's `nextval` is seen at once (PostgreSQL 15:
/// `last_value` 2 here), and its own serial INSERTs' advances -- committed
/// outside the block -- do not make each later statement replay the block.
#[test]
fn a_block_reads_sequences_as_they_stand() {
    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());
    let rt = Runtime::new().expect("runtime");
    run(
        &rt,
        &server,
        &[
            "CREATE SEQUENCE q",
            "CREATE TABLE s (id serial PRIMARY KEY, v int)",
        ],
    );
    let dsn = server.dsn();
    rt.block_on(async move {
        let connect = || async {
            let (c, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
                .await
                .expect("connect");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            c
        };
        let a = connect().await;
        let b = connect().await;
        a.batch_execute("BEGIN; INSERT INTO s (v) VALUES (1)")
            .await
            .unwrap();
        b.batch_execute("SELECT nextval('q'); SELECT nextval('q')")
            .await
            .unwrap();
        let last: i64 = a
            .query_one("SELECT last_value FROM q", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(last, 2);
        b.batch_execute("INSERT INTO s (v) VALUES (2)")
            .await
            .unwrap();
        let n: i64 = a
            .query_one("SELECT count(*) FROM s", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 2);
        let next: i64 = a
            .query_one("SELECT nextval('q')", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(next, 3);
        a.batch_execute("COMMIT").await.unwrap();
    });
    server.stop();
}

/// A transaction moving onto a new snapshot guards its rows across the moment
/// its old transaction lets them go (`MoveGuard`). A writer that had already
/// been through the guard's check could write one of them right then: its
/// write stood in WiredTiger, uncommitted, while the guard turned the writer
/// away at its next check, so the writer waited for the mover and the mover's
/// replay collided with the writer's write for as long as it waited -- a cycle
/// PostgreSQL has no counterpart of, ended only by the replay giving up with a
/// 40001 (`concurrent_writers_under_the_read_committed_move`, once in CI).
/// The move now installs the guard and rolls back holding the write lock of
/// every collection it has written, which a writer holds from its check to
/// its write, so no writer is ever between the two.
///
/// The interleaving is forced: the writer is held between its check and its
/// write while the mover's block is moved. It is released when the move has
/// either begun to wait for the collection's lock (the move now) or already
/// rolled back (a move that does not take it): the writer then writes, into
/// the gap only if there is one.
#[test]
fn a_write_cannot_slip_in_under_a_moving_transactions_guard() {
    // One test, the cases in turn: the hook is process-wide. The writer's
    // statement is a lone autocommit one, the first write of a block, or a
    // write after others in its block: the server waits for the holder in a
    // different place for each.
    for isolation in ["read committed", "repeatable read"] {
        for writer in [
            "UPDATE rc_gap_hot SET n = n + 10 WHERE id = 1",
            "BEGIN; UPDATE rc_gap_hot SET n = n + 10 WHERE id = 1; COMMIT",
            "BEGIN; INSERT INTO rc_gap_noise VALUES (1); \
             UPDATE rc_gap_hot SET n = n + 10 WHERE id = 1; COMMIT",
        ] {
            a_write_slips_under_a_guard(isolation, writer);
        }
    }
}

fn a_write_slips_under_a_guard(writer_isolation: &str, writer_sql: &str) {
    use secantus_storage::{set_test_hook, TestPoint};
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    const T: &str = "rc_gap_hot";
    #[derive(Default)]
    struct Stage {
        armed: bool,
        parked: bool,
        go: bool,
        refused: bool,
    }
    let stage = Arc::new((Mutex::new(Stage::default()), Condvar::new()));
    let wait_until = {
        // Wait up to 20 s for `ready` (a stuck test fails, it does not hang).
        |stage: &Arc<(Mutex<Stage>, Condvar)>, ready: &dyn Fn(&Stage) -> bool| -> bool {
            let (lock, cv) = &**stage;
            let mut g = lock.lock().unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while !ready(&g) {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return false;
                }
                g = cv.wait_timeout(g, left).unwrap().0;
            }
            true
        }
    };
    let hook_stage = Arc::clone(&stage);
    let hook = Arc::new(move |point: TestPoint, row: &(String, String, i64)| {
        if row.1 != T {
            return;
        }
        match point {
            TestPoint::AfterRowCheck => {
                let park = {
                    let (lock, cv) = &*hook_stage;
                    let mut g = lock.lock().unwrap();
                    let park = g.armed;
                    if park {
                        g.armed = false;
                        g.parked = true;
                        cv.notify_all();
                    }
                    park
                };
                if park {
                    wait_until(&hook_stage, &|g| g.go);
                }
            }
            TestPoint::AfterRollback => {
                let release = {
                    let (lock, cv) = &*hook_stage;
                    let mut g = lock.lock().unwrap();
                    let release = g.parked && !g.go;
                    if release {
                        g.go = true;
                        cv.notify_all();
                    }
                    release
                };
                if release {
                    // The writer writes its row now, into the gap, and is
                    // turned away by the guard at its next check.
                    wait_until(&hook_stage, &|g| g.refused);
                }
            }
            TestPoint::MoveLockWait => {
                let (lock, cv) = &*hook_stage;
                let mut g = lock.lock().unwrap();
                if g.parked {
                    g.go = true;
                    cv.notify_all();
                }
            }
            TestPoint::GuardedConflict => {
                let (lock, cv) = &*hook_stage;
                let mut g = lock.lock().unwrap();
                if g.go {
                    g.refused = true;
                    cv.notify_all();
                }
            }
        }
    });
    struct Unhook;
    impl Drop for Unhook {
        fn drop(&mut self) {
            set_test_hook(None);
        }
    }

    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());
    let rt = Runtime::new().expect("runtime");
    run(
        &rt,
        &server,
        &[
            "CREATE TABLE rc_gap_hot (id int PRIMARY KEY, n int)",
            "CREATE TABLE rc_gap_noise (k int)",
            "INSERT INTO rc_gap_hot VALUES (1, 0), (2, 0), (3, 0)",
        ],
    );
    let dsn = server.dsn();
    let _unhook = Unhook;
    set_test_hook(Some(hook));
    let rows = rt.block_on(async {
        async fn connect(dsn: &str) -> tokio_postgres::Client {
            let (c, conn) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
                .await
                .expect("connect");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            c
        }
        let mover = connect(&dsn).await;
        let writer = connect(&dsn).await;
        writer
            .batch_execute(&format!(
                "SET default_transaction_isolation = '{writer_isolation}'"
            ))
            .await
            .expect("the writer's isolation level");
        let other = connect(&dsn).await;
        // The mover's block holds row 1.
        mover.batch_execute("BEGIN").await.expect("begin");
        mover
            .batch_execute("UPDATE rc_gap_hot SET n = n + 1 WHERE id = 1")
            .await
            .expect("the mover's first update");
        // A commit to the table: the mover's next statement must read a new
        // snapshot, so its block is moved.
        other
            .batch_execute("UPDATE rc_gap_hot SET n = n + 1 WHERE id = 2")
            .await
            .expect("another session's commit");
        // The writer wants row 1 too: held between its check and its write
        // (holding the store's write lock, so nothing else writes meanwhile).
        stage.0.lock().unwrap().armed = true;
        let sql = writer_sql.to_string();
        let writer_task = tokio::spawn(async move { writer.batch_execute(&sql).await });
        let s = Arc::clone(&stage);
        assert!(
            tokio::task::spawn_blocking(move || wait_until(&s, &|g| g.parked))
                .await
                .unwrap(),
            "the writer never reached its check"
        );
        let moved = mover
            .batch_execute("UPDATE rc_gap_hot SET n = n + 1 WHERE id = 3")
            .await;
        assert!(
            moved.is_ok(),
            "the mover's block could not be moved: {moved:?}"
        );
        mover.batch_execute("COMMIT").await.expect("commit");
        let written = writer_task.await.expect("writer task");
        if writer_isolation == "read committed" {
            written.expect("the writer's statement runs once the mover has committed");
        } else {
            // PostgreSQL's answer to a REPEATABLE READ writer whose row the
            // mover committed meanwhile.
            let err = written.expect_err("the writer's row changed under its snapshot");
            assert_eq!(
                err.code().map(|c| c.code()),
                Some("40001"),
                "a writer at {writer_isolation} ({writer_sql}): {err:?}"
            );
        }
        let rows = other
            .query("SELECT id, n FROM rc_gap_hot ORDER BY id", &[])
            .await
            .expect("read back");
        rows.iter()
            .map(|r| (r.get::<_, i32>(0), r.get::<_, i32>(1)))
            .collect::<Vec<_>>()
    });
    // Read committed: both updates of row 1 applied (1 + 10), once each.
    // Repeatable read: the writer's was refused, the mover's stands.
    let row_1 = if writer_isolation == "read committed" {
        11
    } else {
        1
    };
    assert_eq!(
        rows,
        vec![(1, row_1), (2, 1), (3, 1)],
        "a writer at {writer_isolation} ({writer_sql})"
    );
    drop(rt);
    server.stop();
}
