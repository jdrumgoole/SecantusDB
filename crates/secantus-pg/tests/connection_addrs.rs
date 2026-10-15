//! `inet_client_addr()`, `inet_client_port()`, `inet_server_addr()` and
//! `inet_server_port()`: the connection's two ends, typed `inet` / `integer`
//! as PostgreSQL 15.19 types them.

use secantus_pg::PgServer;
use tokio_postgres::NoTls;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_connections_addresses_are_reported() {
    let server = PgServer::start().expect("start");
    let (client, connection) = tokio_postgres::connect(&server.dsn(), NoTls)
        .await
        .expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let row = client
        .query_one(
            "SELECT inet_client_addr()::text, inet_server_addr()::text, inet_server_port(),
                    inet_client_port() > 0, pg_typeof(inet_client_addr())::text,
                    pg_typeof(inet_server_port())::text",
            &[],
        )
        .await
        .expect("query");
    let (client_addr, server_addr): (String, String) = (row.get(0), row.get(1));
    assert_eq!(client_addr, "127.0.0.1/32");
    assert_eq!(server_addr, "127.0.0.1/32");
    assert_eq!(row.get::<_, i32>(2), i32::from(server.port()));
    assert!(row.get::<_, bool>(3));
    assert_eq!(row.get::<_, String>(4), "inet");
    assert_eq!(row.get::<_, String>(5), "integer");
}
