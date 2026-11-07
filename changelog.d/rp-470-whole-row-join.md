### Rust PG server: a joined table's name is its whole row

`select fs from fs, fe`, `select fs from fs join fe using (a)` and
`select fe.*::text from fs join fe using (a)` were 42703 `column "fs" does not
exist` (or 42P01 `invalid reference`): only a query over one FROM item could
read a table's whole row. A join now reads it the way PostgreSQL does. A
table's or a subquery's row is built at its scan, as a whole-row Var is, so
the NULL-extended side of an outer join has a NULL row: `select fe from fs
left join fe on false` is NULL, not `(,)`, and `count(fe)` counts it as
missing. An aliased JOIN's row, or a USING alias's, is a record of its
columns.

#### Fixed

- A whole-row reference over a join (`t`, `t.*` inside an expression, `(t).c`,
  `ROW(t.*)`) to a table, a FROM subquery, an aliased JOIN or a USING alias is
  answered, in the select list, WHERE, ORDER BY, GROUP BY and ON. The narrow
  two-table join path hands such a query to the general join planner.
- A cast whole row is named after its FROM item (`fe::text` and `fe.*::text`
  are `fe`), not `row`, over a join and over one table.

Fixes RP-470 (tests/backlog/test_rp_subquery_scope.py::test_whole_row_over_a_join,
tests/backlog/test_rp_subquery_scope.py::test_whole_row_output_names).

#### Filed

- RP-479: a LATERAL subquery's whole row, a whole row named inside a
  subquery, and `(t).*` under `ORDER BY 1` over a join are still refused.
- RP-480: a table under a column-alias list has a `record` whole row, and a
  view's whole row reports type OID 1043.
- RP-481: a missing USING column's 42703 carries a position PostgreSQL does
  not send.
