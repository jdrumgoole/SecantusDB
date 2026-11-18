### A catalog read of one user type builds that type alone

A `pg_type` read pinned to a user type's oid -- psql's `\d` column query
asks one for each column of an enum, composite or domain -- now builds that
type's row alone, as PostgreSQL reads it through `pg_type`'s oid index. It
built every enum, range and table row type, and a second such key in the
same message cycle built every row, so the first `\d` after DDL over a table
of two enum columns grew from 23 ms with no other table to 65 ms with 400
(debug build); it now costs the same either way. A key that names a user
type's array still builds every row, since an array's name depends on every
other type's.

#### Fixed

- Fixes RP-970 (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_over_two_user_types_costs_the_same_at_400_tables); keyed reads of every kind of user type are pinned by tests/test_rust_pg_type_keyed.py::test_a_keyed_user_type_read_answers_the_row_the_whole_catalog_lists.
- Filed RP-1055: `'_custom'::regtype` resolves to `custom`'s array.
