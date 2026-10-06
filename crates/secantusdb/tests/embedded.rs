//! `secantus_mdb::Server` driven by the official `mongodb` driver, the way a
//! user's test suite would use it.

use std::time::Duration;

use mongodb::bson::{doc, Document};
use mongodb::sync::Client;
use secantus_mdb::Server;

fn client(server: &Server) -> Client {
    let uri = format!("{}&serverSelectionTimeoutMS=10000", server.uri());
    Client::with_uri_str(uri).expect("client")
}

#[test]
fn start_serves_and_removes_its_temporary_store() {
    let server = Server::start().expect("start");
    let path = server.storage_path().to_path_buf();
    assert!(path.is_dir());

    let coll = client(&server).database("t").collection::<Document>("c");
    coll.insert_many((0..100).map(|i| doc! {"_id": i, "even": i % 2 == 0}))
        .run()
        .expect("insert");
    assert_eq!(coll.count_documents(doc! {"even": true}).run().unwrap(), 50);

    drop(server);
    assert!(
        !path.exists(),
        "temporary store {} survived drop",
        path.display()
    );
}

#[test]
fn a_persistent_store_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("secantus-mdb-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    {
        let server = Server::builder().storage_path(&dir).start().expect("start");
        let coll = client(&server).database("t").collection::<Document>("c");
        coll.insert_one(doc! {"_id": 1, "kept": true})
            .run()
            .unwrap();
    }
    assert!(dir.is_dir(), "a persistent store is kept on drop");
    {
        let server = Server::builder()
            .storage_path(&dir)
            .start()
            .expect("restart");
        let coll = client(&server).database("t").collection::<Document>("c");
        let got = coll.find_one(doc! {"_id": 1}).run().unwrap().expect("doc");
        assert!(got.get_bool("kept").unwrap());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn advertises_a_replica_set_unless_told_not_to() {
    let rs = Server::start().unwrap();
    let hello = client(&rs)
        .database("admin")
        .run_command(doc! {"hello": 1})
        .run()
        .unwrap();
    assert_eq!(hello.get_str("setName").unwrap(), "secantus");

    let plain = Server::builder().replica_set(None).start().unwrap();
    let hello = client(&plain)
        .database("admin")
        .run_command(doc! {"hello": 1})
        .run()
        .unwrap();
    assert!(hello.get("setName").is_none(), "{hello:?}");
}

#[test]
fn test_commands_are_on_by_default() {
    let server = Server::start().unwrap();
    let reply = client(&server)
        .database("admin")
        .run_command(doc! {"configureFailPoint": "failCommand", "mode": "off"})
        .run()
        .unwrap();
    assert_eq!(reply.get_f64("ok").unwrap_or(1.0), 1.0, "{reply:?}");
}

#[test]
fn stop_is_idempotent_and_frees_the_port() {
    let mut server = Server::start().unwrap();
    let port = server.port();
    server.stop();
    server.stop();
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("port is free after stop");
}

#[tokio::test(flavor = "multi_thread")]
async fn usable_from_an_async_test() {
    let server = Server::start().unwrap();
    let client = mongodb::Client::with_uri_str(server.uri()).await.unwrap();
    let coll = client.database("t").collection::<Document>("c");
    coll.insert_one(doc! {"x": 1}).await.unwrap();
    assert_eq!(coll.count_documents(doc! {}).await.unwrap(), 1);
    drop(server); // dropping inside a runtime must not panic
}

#[tokio::test(flavor = "current_thread")]
async fn usable_from_a_current_thread_runtime() {
    let server = Server::start().unwrap();
    let client = mongodb::Client::with_uri_str(server.uri()).await.unwrap();
    client
        .database("admin")
        .run_command(doc! {"ping": 1})
        .await
        .unwrap();
}

/// Fifty servers at once, each doing real work: catches per-instance resource
/// leaks (the WiredTiger cache, threads, ports, temp directories) before users
/// do, since a test suite starts many of these.
#[test]
fn fifty_servers_in_parallel() {
    let handles: Vec<_> = (0..50)
        .map(|i| {
            std::thread::spawn(move || {
                let server = Server::start().expect("start");
                let path = server.storage_path().to_path_buf();
                let coll = client(&server).database("t").collection::<Document>("c");
                coll.insert_many((0..20).map(|j| doc! {"i": i, "j": j}))
                    .run()
                    .unwrap();
                assert_eq!(coll.count_documents(doc! {"i": i}).run().unwrap(), 20);
                let port = server.port();
                drop(server);
                assert!(!path.exists(), "server {i} left {}", path.display());
                port
            })
        })
        .collect();
    let mut ports: Vec<u16> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    ports.sort_unstable();
    ports.dedup();
    assert_eq!(ports.len(), 50, "every server got its own port");
    std::thread::sleep(Duration::from_millis(10));
}

#[test]
fn a_ttl_index_expires_documents_in_an_embedded_server() {
    // The embedded server used to run no sweeper, so a TTL index never
    // expired anything in it.
    let server = Server::builder()
        .ttl_sweep(Some(Duration::from_millis(100)))
        .start()
        .expect("start");
    let coll = client(&server).database("t").collection::<Document>("ttl");
    coll.create_index(
        mongodb::IndexModel::builder()
            .keys(doc! {"at": 1})
            .options(
                mongodb::options::IndexOptions::builder()
                    .expire_after(Duration::from_secs(0))
                    .build(),
            )
            .build(),
    )
    .run()
    .expect("TTL index");
    let past = mongodb::bson::DateTime::from_millis(0);
    coll.insert_many([
        doc! {"_id": 1, "at": past},
        doc! {"_id": 2, "no_date": true},
    ])
    .run()
    .expect("insert");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while coll.count_documents(doc! {"_id": 1}).run().unwrap() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the expired document was never swept"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // A document without the indexed date is never expired.
    assert_eq!(coll.count_documents(doc! {"_id": 2}).run().unwrap(), 1);
}

#[test]
fn a_disabled_sweeper_expires_nothing_and_a_zero_period_is_refused() {
    let server = Server::builder().ttl_sweep(None).start().expect("start");
    let coll = client(&server).database("t").collection::<Document>("ttl");
    coll.create_index(
        mongodb::IndexModel::builder()
            .keys(doc! {"at": 1})
            .options(
                mongodb::options::IndexOptions::builder()
                    .expire_after(Duration::from_secs(0))
                    .build(),
            )
            .build(),
    )
    .run()
    .expect("TTL index");
    coll.insert_one(doc! {"_id": 1, "at": mongodb::bson::DateTime::from_millis(0)})
        .run()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(coll.count_documents(doc! {}).run().unwrap(), 1);

    let err = Server::builder()
        .ttl_sweep(Some(Duration::ZERO))
        .start()
        .expect_err("a zero period is refused");
    assert!(
        err.to_string().contains("ttl_sweep must be positive"),
        "{err}"
    );
}

#[test]
fn a_noop_heartbeat_writes_to_the_oplog_and_stops_with_the_server() {
    let server = Server::builder()
        .noop_heartbeat(Some(Duration::from_millis(100)))
        .start()
        .expect("start");
    let path = server.storage_path().to_path_buf();
    let oplog = client(&server)
        .database("local")
        .collection::<Document>("oplog.rs");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while oplog.count_documents(doc! {"op": "n"}).run().unwrap() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "no noop heartbeat reached the oplog"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Stopping joins the sweepers before the store closes, so the temporary
    // store is removed rather than left behind as still open.
    drop(server);
    assert!(!path.exists(), "{} survived drop", path.display());
}
