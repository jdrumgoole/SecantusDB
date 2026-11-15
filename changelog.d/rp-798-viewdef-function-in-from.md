### `pg_get_viewdef` prints a view over a record-returning function

`pg_get_viewdef('pg_settings'::regclass)` now prints as
PostgreSQL 15.19 prints them, the star expanded to the function's OUT
parameters and the alias carrying the full column list:
`FROM pg_show_all_settings() a(name, setting, ...)`. It was NULL
because the view printer refused a FROM function returning a record.
38 of the 75 `pg_catalog` views now reproduce exactly.

#### Fixed

- `pg_get_viewdef` of a view whose FROM calls a built-in returning a
  record of OUT parameters (`pg_settings`) printed NULL.
  Fixes RP-798, trimmed (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_a_view_over_a_function_in_from)
