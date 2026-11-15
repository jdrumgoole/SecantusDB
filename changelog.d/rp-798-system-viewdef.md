### `pg_get_viewdef` prints the system views

`pg_get_viewdef('pg_tables'::regclass)` answered NULL on the Rust PostgreSQL
server for every system view, though a user view's definition was printed.
The server now prints PostgreSQL 15.19's own `system_views.sql` queries
through the same ruleutils printer it uses for user views, once per
process, and answers with the text PostgreSQL gives for the views the
printer reproduces exactly (`pg_tables`, `pg_roles`, `pg_indexes`,
`pg_stats` and others). The rest stay NULL rather than print a text
PostgreSQL would not.

#### Fixed

- `pg_get_viewdef` of a `pg_catalog` view the printer reproduces is its
  PostgreSQL 15.19 definition, not NULL (RP-798, trimmed: function-in-FROM
  views and the `information_schema` views stay open).
  Fixes RP-798 (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_a_system_view)
