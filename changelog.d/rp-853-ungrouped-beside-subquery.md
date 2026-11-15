### An ungrouped column beside a subquery is reported as ungrouped

A grouped query whose select list held a subquery, such as `select b, (select 1) from v group by a`, reported the ungrouped column `b` as a column that does not exist (42703). The Rust PostgreSQL server now reports it as PostgreSQL does: 42803, `column "v.b" must appear in the GROUP BY clause or be used in an aggregate function`, at the column.

#### Fixed

- Rust PG server: an ungrouped column beside a select-list subquery in a grouped query is 42803 naming `<alias>.<column>`, not 42703.
  Fixes RP-853 (tests/backlog/test_rp_found_rp700.py::test_an_ungrouped_column_beside_a_subquery_is_42803)
