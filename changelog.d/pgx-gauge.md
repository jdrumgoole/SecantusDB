### The pgx gauge lands — Go's strictest pgwire client now measures the SQL server

jackc/pgx joins the SQL server's conformance portfolio as its fourth external
gauge. `invoke validate-pgx` runs the vendored pgx v5.9.2 `pgconn` and
`pgproto3` test packages — the hand-rolled wire client and message codecs,
the Go analogue of the mongo-c-driver gauge on the Mongo side — completely
unmodified, pointed at a daemon server through `PGX_TEST_DATABASE`. It runs
weekly in CI alongside the psycopg, sqllogictest, and SQLAlchemy gauges.

The opening baseline is **291 passed / 87 failed / 22 skipped (77.0%)**:
the `pgproto3` wire codecs pass 99.4%, while `pgconn` (55.7%) exposes two
clear feature clusters worth their own follow-ups — pipeline mode (Sync-less
extended-protocol batching) and CancelRequest handling — now recorded in the
backlog as the next levers.

#### Added

- `pgx_validation/` (runner, package list, `go test -json` report generator),
  `vendor/pgx` submodule pinned at v5.9.2, `invoke validate-pgx`, and a
  weekly `validate.yml` row sharing the Go gauge's toolchain step.
