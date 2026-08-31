### The Rust PostgreSQL server can be scored against psycopg's own test suite

SecantusDB's conformance gauges run real client libraries' own test suites,
unmodified, against the server. Until now the SQL gauges could only measure the
Python PostgreSQL server; setting `SECANTUS_GAUGE_SERVER=rust` now points the
psycopg gauge at the Rust one instead, so the same tests score both.

Getting there needed two things that only a real client asks for. Clients check
which server they have connected to before doing anything else, and the gauge
refuses to score a server that does not identify itself — so `SELECT version()`
and the other session functions had to work, which meant supporting a `SELECT`
with no table at all. Clients also wrap their work in transactions, so
`BEGIN`, `COMMIT` and `ROLLBACK` had to work, backed by real storage
transactions rather than accepted and ignored: a `ROLLBACK` that quietly kept
the changes would be worse than refusing the statement.

The first score is low, and deliberately published rather than buried: 694 of
psycopg's 4,238 tests pass. Every measurement before this compared the server
against expectations written alongside it; this is the first one where someone
else's tests decide. The ranked list of what they trip over is the useful part.

#### Added

- `SELECT` without a `FROM` clause, and the `version()`, `current_database()`,
  `current_schema()` and `current_user` session functions.
- `BEGIN` / `COMMIT` / `ROLLBACK`, backed by real storage transactions.
- `SECANTUS_GAUGE_SERVER=rust` support in the psycopg gauge, writing its
  results to a separate file so the two servers' scores cannot overwrite
  each other.
