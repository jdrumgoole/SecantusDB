### `pg_get_viewdef` prints 18 more system views

`pg_get_viewdef('pg_sequences'::regclass)` and the definitions of the `pg_stat_*_tables`, `pg_stat_*_indexes` and `pg_statio_*` views used to come back NULL. They now print exactly as PostgreSQL 15.19 prints them, 56 of the 75 `pg_catalog` views in all. The printer now knows the result type of `pg_` functions such as `pg_sequence_last_value`, and leaves a `name` column as `name` under `NOT IN` and the LIKE and regex operators, where it used to cast it to text. That cast also showed up in user views that filter a catalog's `name` column.

#### Fixed

- `pg_get_viewdef` of `pg_sequences`, `pg_stat_all_tables` / `_indexes` and their `_sys_` / `_user_` / `_xact_` forms, and every `pg_statio_*` view.
- A view's `name IN (...)`, `name ~ ...`, `name ~~ ...` print without a cast to text.
  Trims RP-798 (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_pg_sequences, tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_keeps_name_under_in_and_regex)
