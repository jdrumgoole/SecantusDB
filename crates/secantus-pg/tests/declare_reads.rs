//! A cursor's query and a TABLESAMPLE's relation are reads like any other:
//! table and column privileges and row-level security reach them, as
//! PostgreSQL 15.19 has it. pg_query's node walk reaches neither, which is
//! how all three were once skipped.

use secantus_pg::PgServer;
use tokio_postgres::NoTls;

async fn connect(conninfo: &str) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(conninfo, NoTls)
        .await
        .expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The SQLSTATE `sql` fails with, or `None` when it succeeds.
async fn sqlstate(client: &tokio_postgres::Client, sql: &str) -> Option<String> {
    client
        .batch_execute(&format!("SAVEPOINT s; {sql}; RELEASE s"))
        .await
        .err()
        .map(|e| {
            let code = e.code().map(|c| c.code().to_string());
            code.unwrap_or_else(|| format!("{e}"))
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn privileges_and_rls_reach_cursor_and_tablesample_reads() {
    let server = PgServer::start().expect("start");
    let client = connect(&server.dsn()).await;
    client
        .batch_execute(
            "CREATE ROLE r;
             CREATE TABLE secret (a int); INSERT INTO secret VALUES (1);
             CREATE TABLE cols (a int, b int); INSERT INTO cols VALUES (1, 2);
             GRANT SELECT (a) ON cols TO r;
             CREATE TABLE rowsec (a int, owner text);
             INSERT INTO rowsec VALUES (1, 'r'), (2, 'other');
             GRANT SELECT ON rowsec TO r",
        )
        .await
        .expect("setup");
    client.batch_execute("BEGIN").await.unwrap();
    // RLS enabled in this block restricts the block's own later reads.
    client
        .batch_execute(
            "ALTER TABLE rowsec ENABLE ROW LEVEL SECURITY;
             CREATE POLICY p ON rowsec FOR SELECT USING (owner = current_user);
             SET ROLE r",
        )
        .await
        .unwrap();
    for (sql, want) in [
        (
            "SELECT * FROM secret TABLESAMPLE SYSTEM (100)",
            Some("42501"),
        ),
        ("DECLARE c1 CURSOR FOR SELECT * FROM secret", Some("42501")),
        ("DECLARE c2 CURSOR FOR SELECT b FROM cols", Some("42501")),
        (
            "SELECT x.b FROM cols x TABLESAMPLE SYSTEM (100)",
            Some("42501"),
        ),
        ("DECLARE c3 CURSOR FOR SELECT a FROM cols", None),
    ] {
        assert_eq!(sqlstate(&client, sql).await.as_deref(), want, "{sql}");
        if want.is_some() {
            client.batch_execute("ROLLBACK TO s").await.unwrap();
        }
    }
    for sql in [
        "SELECT a FROM rowsec",
        "SELECT a FROM rowsec TABLESAMPLE SYSTEM (100)",
    ] {
        let seen: Vec<i32> = client
            .query(sql, &[])
            .await
            .expect(sql)
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(seen, vec![1], "{sql}");
    }
    client
        .batch_execute("DECLARE c4 CURSOR FOR SELECT a FROM rowsec")
        .await
        .unwrap();
    let seen: Vec<i32> = client
        .query("FETCH ALL FROM c4", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(seen, vec![1], "a cursor over an RLS table");
    client.batch_execute("ROLLBACK").await.unwrap();
}
