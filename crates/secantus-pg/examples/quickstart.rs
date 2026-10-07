//! Start an embedded SecantusDB PostgreSQL server and talk to it with
//! `tokio-postgres`: `cargo run --example quickstart`.

use secantus_pg::PgServer;
use tokio_postgres::NoTls;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = PgServer::start()?;
    println!("serving on {}", server.url());

    let (client, connection) = tokio_postgres::connect(&server.dsn(), NoTls).await?;
    tokio::spawn(connection);

    client
        .batch_execute("CREATE TABLE greetings (id int PRIMARY KEY, text text)")
        .await?;
    client
        .execute(
            "INSERT INTO greetings VALUES ($1, $2)",
            &[&1i32, &"hello from secantus-pg"],
        )
        .await?;
    let row = client
        .query_one("SELECT text FROM greetings WHERE id = 1", &[])
        .await?;
    println!("{}", row.get::<_, &str>(0));
    // Dropping `server` stops it and removes its temporary store.
    Ok(())
}
