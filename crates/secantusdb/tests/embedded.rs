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
