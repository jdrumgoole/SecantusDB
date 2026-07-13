### The psycopg conformance gauge: SecantusDB's SQL server gets its headline number

The SQL server now has what the Mongo server has had for a year: an
external conformance gauge running a real driver's own unmodified test
suite. `invoke validate-psycopg` vendors psycopg 3.3.4 (pinned in lockstep
with the `dev`-extra wheel), spawns a `SecantusPGServer` daemon on an
ephemeral port, verifies it actually is SecantusDB (a stray real Postgres
would inflate the numbers), runs the full sync half of psycopg's suite over
`PSYCOPG_TEST_DSN`, and renders `docs/validation-report-psycopg.md` with
the per-file pass/fail/skip breakdown. It joins the weekly `validate.yml`
matrix as the fourteenth gauge — and the first for the SQL side. The
opening baseline over the full sync suite is 2415 passed of ~4100 run
(58.6%); the six-file subset that drove this month's conformance work
stands at 91%.

#### Added

- `psycopg_validation/` (runner, include list, report generator),
  `invoke validate-psycopg`, a `psycopg` lane in `validate.yml`, and the
  `vendor/psycopg` submodule @ 3.3.4. `psycopg[binary]` is now pinned
  exactly so the vendored suite and the installed wheel stay in lockstep.
