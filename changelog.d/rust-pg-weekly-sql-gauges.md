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

#### Found

The first run, dispatched from this branch, passed nine of the ten lanes.
The sqllogictest lane failed. PostgreSQL 15.19 itself fails 9 of its 30
files on SQLite-only behaviour, and the Rust server fails the same 9 plus
one. Two Rust-server bugs are filed with gap tests:

- RP-075: `c / 0 = NULL` raises 22012 where PostgreSQL folds a strict
  operator with a NULL argument to NULL first
  (`tests/backlog/test_rp_found_2026_10_21.py::test_strict_null_folding`).
  This is the extra file, `random/aggregates/slt_good_1.test`, so the lane
  stays red until it is fixed.
- RP-076: `NULLIF` keeps its first argument's type where PostgreSQL coerces
  it, so `nullif(53, 80::numeric) / -71` is integer division
  (`...::test_nullif_result_type`). It hid inside an expected divergence,
  `random/expr/slt_good_0.test`, as a later stop point.

#### Changed

- `validate.yml` uploads a lane's report and raw output when its gauge step
  fails too, so a red lane's numbers reach the weekly reports PR.
- Five sqllogictest files are expected divergences where the Rust server and
  PostgreSQL 15.19 both stop on the corpus's SQLite answer. The reason for
  `random/expr/slt_good_0.test` is corrected: PostgreSQL stops there on an
  integer-versus-numeric answer, not on division by zero.
- The SQL reports name the server they measured, read from the report's file
  name: the Rust PostgreSQL server and its crate version, or the Python server.
  Every report used to say "Python server".
- `sql-stress` can start the Rust server (`pg_gauge_server`), and pgjdbc's
  Rust-server shard files carry the `-rust-server` suffix, so a merge never
  mixes the two servers' shards.
