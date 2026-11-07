### Rust PG server: a subquery with too many columns is refused

`select (select 1, 2)`, `select array(select 1, 2)` and `select 1 in
(select 1, 2)` are 42601 as on PostgreSQL 15.19 (`subquery must return
only one column` at the subquery, `subquery has too many columns` at the
`in`). Each answered a value built from the subquery's first column (`1`,
`{1}`, `true`), correlated or not, in a select list or a WHERE.

#### Fixed

- A subquery's width is checked as PostgreSQL's `transformSubLink` checks
  it, once the subquery is analysed and before it runs: an EXPR or ARRAY
  subquery must have one output column (a `*` expanded, an ORDER BY column
  outside the select list not counted), and an ANY / ALL one as many as its
  left side, too many or too few. A row on the left (`(a, b) = (select
  ...)`, `(a, b) in (select ...)`) is a row comparison, held to the row's
  width at the operator. A column the left side names that does not exist
  is still reported first (`nosuch in (select 1, 2)` is 42703), and an
  EXISTS still ignores its targets.

Fixes RP-452 (tests/backlog/test_rp_subquery_columns.py::test_subquery_with_more_than_one_column)

#### Filed

- RP-458: a missing relation named by a string literal for a `regclass`
  parameter (`nextval('nosuchseq')`) is looked up only if the call runs,
  not as it is analysed, so `select 1 where false and nextval('nosuchseq')
  > 0` answers no rows where PostgreSQL refuses it with 42P01
  (tests/backlog/test_rp_regclass_literal.py::test_regclass_literal_is_resolved_at_analysis).
