### The wire-protocol gauge lands — CockroachDB's pgtest corpus runs verbatim

The SQL server's conformance portfolio gains its strictest instrument: G3,
the pgwire message-level gauge. `invoke validate-pgtest` drives CockroachDB's
`pkg/sql/pgwire/testdata/pgtest` corpus — ~54 datadriven files of raw
Parse/Bind/Describe/Execute/COPY/error exchanges with byte-exact expected
responses — using CockroachDB's own `pkg/testutils/pgtest` runner,
completely unmodified. It is the SQL analogue of the mongo-c-driver gauge:
where the driver gauges tolerate server slop, this one asserts the framing
itself.

The monorepo problem is solved by not vendoring at all: both corpus and
runner are fetched at a pinned commit through a sparse, blob-filtered clone
(about 25 MB, cached) at gauge time — the same fetch-at-runtime pattern as
the sqllogictest runner's `cargo install` — which also keeps the CockroachDB
Software License outside the repository tree. The only committed Go code is
a thin `go test` driver and a ten-line shim for one internal helper the
runner imports. SecantusDB presents as non-CockroachDB, so the corpus'
`crdb_only` exchanges skip themselves.

The opening baseline is **8 of 58 files** — honest and low by design, since
every file stops at its first byte-level mismatch; the number climbs
cluster-by-cluster the way the psycopg gauge went from 42% to 91%. The first
finding is already fixed: an unaliased cast's output column is now named
after the type's `typname` (`SELECT 2::int8` → column `int8`), where it
previously reported `?column?`.

#### Added

- `pgtest_validation/` (pinned-commit sparse fetch, verbatim upstream
  runner staging, Go driver module, report generator), `invoke
  validate-pgtest`, weekly `validate.yml` row sharing the Go toolchain step.

#### Fixed

- `sql/planner.py`: unaliased top-level cast projections are named after the
  cast target's `typname` like real PG, across the constant, single-table,
  grouped, and RETURNING paths.
