//! A GROUP BY key that is a subquery, or a position or output name standing
//! for one, groups by the subquery's value, computed once per input row; an
//! outer column the subquery reads is grouped through it (RP-700).
//!
//! A grouped query with a subquery in its select list is split so the
//! subqueries run over the grouped rows (`agg_hoist`). The split resolved no
//! GROUP BY position or output name against the select list, so `group by 1`
//! named the inner query's own first column, and the aggregate planner keyed
//! on no subquery: each statement below was 42803, 42P01 or 0A000. The
//! answers are PostgreSQL 15.19's.

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

/// Each query's first column, as `int4` or NULL.
fn answers(rt: &Runtime, server: &RunningPgServer, queries: &[&str]) -> Vec<Vec<Option<i32>>> {
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(&server.dsn(), tokio_postgres::NoTls)
            .await
            .expect("connect");
        let handle = tokio::spawn(async move {
            let _ = connection.await;
        });
        let mut out = Vec::new();
        for sql in queries {
            let rows = client.query(*sql, &[]).await.expect(sql);
            out.push(rows.iter().map(|r| r.get::<_, Option<i32>>(0)).collect());
        }
        drop(client);
        let _ = handle.await;
        out
    })
}

#[test]
fn a_group_by_subquery_key_groups_by_its_value() {
    let dir = TempDir::new().expect("tempdir");
    let mut server = start(dir.path());
    let rt = Runtime::new().expect("runtime");
    let cases: [(&str, Vec<Option<i32>>); 8] = [
        (
            "select (select v.a) from (values (1), (1), (2)) v(a) group by 1 order by 1",
            vec![Some(1), Some(2)],
        ),
        (
            "select (select v.*) from (values (1), (1), (2)) v(a) group by 1 order by 1",
            vec![Some(1), Some(2)],
        ),
        (
            "select (select v.a) x from (values (1), (1), (2)) v(a) group by x order by 1",
            vec![Some(1), Some(2)],
        ),
        (
            "select count(*)::int from (values (1), (1), (2)) v(a) \
             group by (select v.a) order by 1",
            vec![Some(1), Some(2)],
        ),
        (
            "select a from (values (1), (2), (1)) v(a) group by 1, (select 1) order by 1",
            vec![Some(1), Some(2)],
        ),
        (
            "select (select v.a) from (values (1), (1), (2)) v(a) group by rollup (1) order by 1",
            vec![Some(1), Some(2), None],
        ),
        (
            "select (select v.a) from (values (1), (1), (2)) v(a) group by 1 having count(*) > 1",
            vec![Some(1)],
        ),
        ("select (select 7) group by 1", vec![Some(7)]),
    ];
    let queries: Vec<&str> = cases.iter().map(|(q, _)| *q).collect();
    let got = answers(&rt, &server, &queries);
    for ((q, want), got) in cases.iter().zip(got) {
        assert_eq!(&got, want, "{q}");
    }
    server.stop();
}
