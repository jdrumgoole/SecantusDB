//! `secantus_pg::PgServer`, the one-line embedding API, driven the way a
//! user's test drives it: through `tokio-postgres`, inside `#[tokio::test]`.

use secantus_pg::{Error, PgServer};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_serves_and_removes_its_temporary_store() {
    let mut server = PgServer::start().expect("start");
    assert_ne!(server.port(), 0);
    let path = server.storage_path().to_path_buf();
    assert!(path.is_dir());

    let client = connect(&server.dsn()).await;
    client
        .batch_execute(
            "CREATE TABLE t (id int PRIMARY KEY, v text); INSERT INTO t VALUES (1, 'a'), (2, 'b')",
        )
        .await
        .expect("write");
    let n: i64 = client
        .query_one("SELECT count(*) FROM t", &[])
        .await
        .expect("count")
        .get(0);
    assert_eq!(n, 2);
    drop(client);

    server.stop();
    assert!(!path.exists(), "temporary store not removed: {path:?}");
    server.stop(); // idempotent
}

#[tokio::test(flavor = "current_thread")]
async fn the_url_form_connects_too() {
    let server = PgServer::start().expect("start");
    assert!(server.url().starts_with("postgresql://postgres@127.0.0.1:"));
    let client = connect(&server.url()).await;
    let one: i32 = client.query_one("SELECT 1", &[]).await.unwrap().get(0);
    assert_eq!(one, 1);
    // Dropped inside a current-thread runtime: must not panic.
}

#[test]
fn a_persistent_store_survives_a_restart() {
    let dir = tempfile::TempDir::new().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    {
        let server = PgServer::builder()
            .storage_path(dir.path())
            .start()
            .expect("start");
        rt.block_on(async {
            let client = connect(&server.dsn()).await;
            client
                .batch_execute("CREATE TABLE kept (id int); INSERT INTO kept VALUES (7)")
                .await
                .unwrap();
        });
    }
    assert!(dir.path().is_dir(), "a persistent store was removed");
    let server = PgServer::builder()
        .storage_path(dir.path())
        .start()
        .expect("restart");
    let id: i32 = rt.block_on(async {
        let client = connect(&server.dsn()).await;
        client
            .query_one("SELECT id FROM kept", &[])
            .await
            .unwrap()
            .get(0)
    });
    assert_eq!(id, 7);
}

#[test]
fn extra_databases_are_connectable() {
    let server = PgServer::builder()
        .databases(["app"])
        .start()
        .expect("start");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let name: String = rt.block_on(async {
        let a = server.address();
        let client = connect(&format!(
            "host={} port={} dbname=app user=postgres",
            a.ip(),
            a.port()
        ))
        .await;
        client
            .query_one("SELECT current_database()", &[])
            .await
            .unwrap()
            .get(0)
    });
    assert_eq!(name, "app");
}

#[test]
fn a_bad_cache_size_is_refused_before_anything_opens() {
    let err = PgServer::builder()
        .cache_size("1G,x=y")
        .start()
        .unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err}");
}

/// Fifty servers at once, each started, used and stopped: catches a
/// per-instance resource leak (WiredTiger cache, threads, ports) before users
/// do.
#[test]
fn fifty_servers_in_parallel() {
    let handles: Vec<_> = (0..50)
        .map(|i| {
            std::thread::spawn(move || {
                let server = PgServer::start().expect("start");
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let got: i32 = rt.block_on(async {
                    let client = connect(&server.dsn()).await;
                    client
                        .query_one("SELECT $1::int", &[&i])
                        .await
                        .unwrap()
                        .get(0)
                });
                assert_eq!(got, i);
                let path = server.storage_path().to_path_buf();
                drop(server);
                assert!(!path.exists());
            })
        })
        .collect();
    for h in handles {
        h.join().expect("server thread");
    }
}
