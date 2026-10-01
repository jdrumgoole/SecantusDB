### Start the Rust MongoDB server from a Rust test

`secantus-mdb` is now a library as well as the `secantusd-rs` binary. A Rust
test suite can start a server in one line and connect the official `mongodb`
driver to it.

```rust
let server = secantus_mdb::Server::start()?;
let client = mongodb::sync::Client::with_uri_str(server.uri())?;
```

#### Added

- `Server::start()` gives each server a temporary store, which is removed on
  drop, and an OS-assigned port. It advertises the `secantus` single-node
  replica set and enables test commands, as the Python embedded handle does.
- `Server::builder()` sets a persistent store, the host, the port, the
  replica-set name (or none), auth, TLS, the WiredTiger cache (256M by default)
  and test commands. A server provides `uri()`, `address()`, `port()` and
  `stop()`, and dropping it stops it.
- A temporary store is removed only after the storage is confirmed closed. If a
  connection outlives the shutdown drain, the directory is left in place with a
  warning, rather than removed while WiredTiger still has it open.
- A runnable `quickstart` example, a doc-test, and tests through the official
  driver, including use inside `#[tokio::test]` and 50 servers in parallel.

#### Changed

- The binary's own dependencies (`ctrlc`, `env_logger`, `mimalloc`) are behind
  a default-on `bin` feature, so a test that uses only the library can turn it
  off. Release builds with `--no-default-features` now pass `--features bin`.
