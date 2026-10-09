# secantus-pg

A **surrogate PostgreSQL server** for tests: it speaks the real PostgreSQL wire
protocol on a real TCP socket, parses SQL with PostgreSQL's own grammar
(libpg_query), and stores rows in WiredTiger, scoped to a single node. Point an
application's test suite at it instead of standing up a real `postgres`.

Building it compiles WiredTiger and libpg_query from source, which needs
**CMake, a C compiler and libclang**. The first build takes a few minutes. If you
only want the binary, `cargo binstall secantus-pg` fetches a prebuilt one
(Linux x86_64, macOS arm64) with no toolchain.

## In a test

```toml
[dev-dependencies]
secantus-pg = { version = "0.1.0-beta", default-features = false }  # library only
```

```rust,ignore
#[tokio::test]
async fn selects_one() {
    let server = secantus_pg::PgServer::start().unwrap(); // temp store, free port
    let (client, conn) = tokio_postgres::connect(&server.dsn(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let one: i32 = client.query_one("SELECT 1", &[]).await.unwrap().get(0);
    assert_eq!(one, 1);
    // dropping `server` stops it and removes its store -- safe inside a runtime
}
```

`PgServer::builder()` sets a persistent store (`.storage_path(..)`), a port,
extra databases (`.databases(["app"])`) and the WiredTiger cache. `.dsn()` is
the libpq key/value form, `.url()` the `postgresql://` form.

## The binary

```sh
cargo install secantus-pg --version 0.1.0-beta.6   # a pre-release: cargo needs the version
secantusd-pg ./pg-data 127.0.0.1:5433   # storage path, bind address
```

Prebuilt binaries are on the
[GitHub releases](https://github.com/jdrumgoole/SecantusDB/releases)
(`secantusd-pg-v*`).

Project home and documentation: <https://secantusdb.com>.

## Licence

GPL-2.0-only. WiredTiger, which this links statically, is GPL v2 or v3.
