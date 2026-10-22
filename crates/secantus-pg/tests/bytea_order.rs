//! `bytea` compares as PostgreSQL compares it: byte by byte, a shorter prefix
//! first -- with or without an index.
//!
//! The server stores `bytea` as BSON BinData, and the storage and query engine
//! it shares with the MongoDB server order BinData as mongod does (length, then
//! subtype, then bytes; RM-232). This server sets PostgreSQL's order on every
//! thread it runs statements on, and an index it builds records that order and
//! keys its entries by it. Before that, `WHERE x > 'x8'` returned `x10` and
//! `x11` (longer, so "greater"), which PostgreSQL does not.

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

/// Run the setup statements, then each query, returning each query's first
/// column as `i32`s.
fn answers(
    rt: &Runtime,
    server: &RunningPgServer,
    setup: &[&str],
    queries: &[&str],
) -> Vec<Vec<i32>> {
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(&server.dsn(), tokio_postgres::NoTls)
            .await
            .expect("connect");
        let handle = tokio::spawn(async move {
            let _ = connection.await;
        });
        for sql in setup {
            client.simple_query(sql).await.expect(sql);
        }
        let mut out = Vec::new();
        for sql in queries {
            let rows = client.query(*sql, &[]).await.expect(sql);
            out.push(rows.iter().map(|r| r.get::<_, i32>(0)).collect());
        }
        drop(client);
        let _ = handle.await;
        out
    })
}

#[test]
fn bytea_orders_byte_by_byte_with_and_without_an_index() {
    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());
    let rt = Runtime::new().expect("runtime");
    let rows = "(1, 'x1'), (2, 'x10'), (3, 'x2'), (4, 'x8'), (5, 'x9'), (6, 'x11'), \
                (7, 'w'), (8, ''), (9, 'x')";
    let setup = [
        "CREATE TABLE plain (k int PRIMARY KEY, x bytea)".to_string(),
        "CREATE TABLE indexed (k int PRIMARY KEY, x bytea)".to_string(),
        "CREATE INDEX indexed_x ON indexed (x)".to_string(),
        format!("INSERT INTO plain VALUES {rows}"),
        format!("INSERT INTO indexed VALUES {rows}"),
    ];
    let setup: Vec<&str> = setup.iter().map(String::as_str).collect();
    let mut queries = Vec::new();
    for table in ["plain", "indexed"] {
        queries.push(format!("SELECT k FROM {table} WHERE x > 'x8' ORDER BY k"));
        queries.push(format!("SELECT k FROM {table} WHERE x > 'x1' ORDER BY k"));
        queries.push(format!("SELECT k FROM {table} WHERE x < 'x1' ORDER BY k"));
        queries.push(format!("SELECT k FROM {table} ORDER BY x, k"));
    }
    let queries: Vec<&str> = queries.iter().map(String::as_str).collect();
    let got = answers(&rt, &server, &setup, &queries);
    // PostgreSQL 15's answers: '' < 'w' < 'x' < 'x1' < 'x10' < 'x11' < 'x2'
    // < 'x8' < 'x9'.
    let expected: Vec<Vec<i32>> = vec![
        vec![5],
        vec![2, 3, 4, 5, 6],
        vec![7, 8, 9],
        vec![8, 7, 9, 1, 2, 6, 3, 4, 5],
    ];
    assert_eq!(got[..4], expected[..], "unindexed");
    assert_eq!(got[4..], expected[..], "indexed");
    server.stop();
}
