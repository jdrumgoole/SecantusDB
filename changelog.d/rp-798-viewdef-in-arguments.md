### pg_get_viewdef prints the pg_stat_activity and progress-reporting views

`pg_get_viewdef` now prints twelve more `pg_catalog` views exactly as
PostgreSQL 15 does, among them `pg_stat_activity`, `pg_stat_ssl`,
`pg_stat_gssapi`, `pg_stat_replication`, `pg_stat_subscription` and every
`pg_stat_progress_*` view. Each reads a built-in function that takes
arguments and returns a record of OUT parameters, which the printer now
types. A function whose arguments name another FROM item prints `LATERAL`,
and a cast to the type a column already has no longer prints an `AS`.

#### Fixed

- Rust PG server: `pg_get_viewdef` of a view over a built-in record function
  with IN arguments, implicit `LATERAL`, and no-op casts in a target list.
  Fixes RP-798, in part
  (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_views_over_functions_with_in_arguments).
