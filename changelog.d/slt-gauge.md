### Tooling: the sqllogictest conformance gauge (invoke validate-slt)

The SQL server gets its correctness gauge (tasks/sql-gauges-plan.md G1): the
SQLite-originated sqllogictest corpus — 622 files, millions of records — is
vendored pristine at `vendor/sqllogictest` and executed by sqllogictest-rs
over real pgwire, one fresh `SecantusPGServer` daemon per file. A
preprocessing pass (never touching the vendored tree) bridges the three
corpus/runner incompatibilities established empirically: trailing comments on
`skipif`/`onlyif` lines, value-per-line expected blocks for
`nosort`/`rowsort` multi-column records, and sqlite's implicit
`hash-threshold 8` default. The curated 30-file include list currently
passes 26/30 end-to-end; the 4 failures are declared
`EXPECTED_DIVERGENCES` (SQLite read-only views, SQLite's
division-by-zero→NULL vs PG's 22012, and the runner's missing `query I`
type coercion), so the gauge is green in its own terms and reports loudly if
a divergence resolves.

#### Added

- `vendor/sqllogictest` (shallow submodule, dev-only, excluded from
  sdist/wheel), the `slt_validation/` gauge package (preprocessor, per-file
  daemon runner with identity verification, report generator, include +
  expected-divergence lists), the `invoke validate-slt` task, and
  `docs/validation-report-slt.md` in the Sphinx toctree. Requires the
  `sqllogictest` binary (`cargo install sqllogictest-bin`).
- `pyproject.toml`: sdist excludes for `vendor/sqllogictest` /
  `slt_validation` — and the previously-missing `vendor/psycopg` /
  `psycopg_validation` entries.
