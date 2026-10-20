### The weekly SQL gauges run against the Rust PostgreSQL server

The seven SQL gauges now run every week against the Rust PostgreSQL server,
`secantusd-pg`: psycopg, SQLAlchemy, sql-stress, pgjdbc, pgtest, pgx and
sqllogictest. They had stopped when the Python servers were deprecated, which
left the Rust PostgreSQL server with no weekly conformance number.

#### Added

- Ten `validate.yml` lanes: `psycopg-rust-server`, `sqlalchemy-rust-server`,
  `sql-stress-rust-server`, `pgjdbc-rust-server-1` to `-4` (merged by the
  `pgjdbc-report` job), `pgtest-rust-server`, `pgx-rust-server` and
  `slt-rust-server`. Each builds `secantusd-pg` from the commit it measures.
- `--server python|rust` on `validate-psycopg`, `validate-sqlalchemy`,
  `sql-stress`, `validate-pgjdbc`, `validate-pgjdbc-report`, `validate-pgtest`,
  `validate-pgx` and `validate-slt`. `--server rust` writes
  `docs/validation-report-<gauge>-rust-server.md`.
- Placeholder `docs/validation-report-*-rust-server.md` pages for the seven SQL
  gauges, saying no run has happened yet, until the first weekly run replaces
  them.

#### Changed

- The SQL reports name the server they measured, read from the report's file
  name: the Rust PostgreSQL server and its crate version, or the Python server.
  Every report used to say "Python server".
- `sql-stress` can start the Rust server (`pg_gauge_server`), and pgjdbc's
  Rust-server shard files carry the `-rust-server` suffix, so a merge never
  mixes the two servers' shards.
