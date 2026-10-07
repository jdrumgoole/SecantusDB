### The Rust PostgreSQL server is ready for crates.io as `secantus-pg`

The Rust PostgreSQL server's crate is now `secantus-pg`, the name it will have
on crates.io, and it can be embedded in a Rust test in one line:
`secantus_pg::PgServer::start()` opens a temporary store, binds a free port and
hands back `dsn()` / `url()` for `tokio-postgres` or any other client; dropping
it stops the server, checkpoints the store and removes it. It is the
PostgreSQL counterpart of `secantus_mdb::Server`, and like it is safe to start
and drop inside `#[tokio::test]`. The `secantusd-pg` binary is unchanged.

All four PostgreSQL-side crates are now publishable, and the release pipeline
publishes them from a `secantusd-pg-v*` tag. Nothing has been published yet;
the first publish happens at the next PostgreSQL server release.

#### Added

- `secantus_pg::PgServer` and `PgBuilder` (`.storage_path`, `.host`, `.port`,
  `.databases`, `.cache_size`, 256M by default), with `address()`, `port()`,
  `dsn()`, `url()`, `stop()` and `Drop`. A temporary store is removed only
  once `RunningPgServer::store_closed()` confirms WiredTiger closed it. Tests
  in `crates/secantus-pg/tests/pg_server.rs` (both tokio runtime flavours, a
  persistent store across a restart, 50 servers in parallel), a doc-test, and
  `examples/quickstart.rs`.
- `secantus_pg::open_storage_with_cache`, the PG server's storage open with a
  cache size other than the daemon's 4G.
- `secantus-pgwire`: the vendored pgwire 0.40.7 fork, moved from
  `crates/vendor/pgwire` to `crates/secantus-pgwire` and made a publishable
  crate. crates.io ignores `[patch]`, and the fork has diverged too far from
  upstream for one small upstream PR to replace it. The library keeps the name
  `pgwire`.
- crates.io metadata, exact `=` version pins, READMEs and `rust-version` on
  `secantus-pg`, `secantus-pgplan`, `secantus-pgcatalog` and
  `secantus-pgwire`; `cargo binstall secantus-pg` metadata pointing at the
  `secantusd-pg-v*` release archives.
- `scripts/crates_package_check.py --line {mdb,pg,all}`, and the
  `crates-package.yml` gate now packages and builds both lines from their
  tarballs. `publish-crates.yml` fires on `secantusd-pg-v*` tags as well and
  publishes the PG line (`crates_publish.py --line pg`), refusing to start
  if a MongoDB-side crate it pins is not on crates.io yet.
- `./inv rust-version-bump --line pg --to <ver>` bumps the PG version line.

#### Changed

- `crates/secantus-pgserver` is now `crates/secantus-pg` (package
  `secantus-pg`, library `secantus_pg`), licensed GPL-2.0-only as the plan
  decided; `secantus-pgplan` / `secantus-pgcatalog` stay Apache-2.0. Every
  path in the workflows, invoke tasks, gauges, probes, benches and tests moved
  with it.
- The binary's signal-handling dependencies sit behind a default-on `bin`
  feature; the Python extension builds the library without them.
- `secantusd-pg --version` prints `source: crates.io` for a build from a
  packaged crate, as `secantusd-rs` does.
