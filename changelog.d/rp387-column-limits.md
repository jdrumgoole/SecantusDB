### Rust PG server: a table of more than 1600 columns and a target list of more than 1664 entries are refused

The Rust PostgreSQL server now refuses the two widths PostgreSQL refuses with
54011 (`too_many_columns`): `CREATE TABLE` or `ALTER TABLE ADD COLUMN` past
1600 columns (a dropped column's attnum counted, as PostgreSQL counts it),
and a query level whose target list holds more than 1664 entries, counted the
way parse analysis builds it -- a `*` as every column it expands to, an
ORDER BY or GROUP BY expression the list does not hold as one more, a VALUES
row by its width. Before, a client could hand EXPLAIN's cost estimation a list
as long as it liked, and the field dedups beneath it ran in time quadratic in
that length; they are now linear (GitHub issue #2289).

#### Fixed

- Fixes RP-387 (tests/backlog/test_rp_explain_bounds.py::test_create_table_over_1600_columns_is_refused,
  tests/backlog/test_rp_explain_bounds.py::test_explain_count_target_list_over_1664_entries_is_refused,
  tests/backlog/test_rp_explain_bounds.py::test_explain_distinct_order_by_over_1664_entries_is_refused,
  tests/backlog/test_rp_explain_bounds.py::test_widths_at_the_limits_are_accepted_and_one_past_refused).
- The backlog had two items numbered RP-387; the EXPLAIN ANALYZE LEFT Nested
  Loop Join Filter item is now RP-389.
