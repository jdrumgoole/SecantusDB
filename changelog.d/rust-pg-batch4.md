### Rust PostgreSQL server: backlog batch 4

More of the Rust PostgreSQL server's backlog, each fix measured against
PostgreSQL 15.19 and pinned by the backlog test that found it.

#### Added

- `pg_timezone_names` and `pg_timezone_abbrevs`, as the views and as the
  functions in FROM: PostgreSQL 15.19's 598 zone names, each with its
  abbreviation, offset and DST flag at this moment, and its 195 `Default`
  abbreviations. A client listing time zones had got 42P01. The zone
  rules are `chrono-tz`'s, so a zone whose rules its tz database and
  PostgreSQL's disagree on answers by `chrono-tz`'s (Morocco's two, on
  2026-10-15). Part of RP-015
  (`tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed[timezone-names]`,
  `[timezone-abbrevs]`).
- Corpus `timezone_views`.
