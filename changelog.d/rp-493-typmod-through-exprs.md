### Rust PG server: a column keeps its type modifier through subqueries, joins, set operations and CASE

On the Rust PostgreSQL server, a column lost its type modifier once it
passed through a FROM-subquery, a CTE, a join, a `UNION`, `GROUP BY`, or a
`CASE`, `COALESCE`, `GREATEST` / `LEAST` or `ARRAY[...]` whose operands
share one. `select z from (select 2.5::numeric(5,2) z) s` described `z` as
a bare `numeric` (modifier -1), and a CREATE TABLE AS over it made a bare
`numeric` column. Each now carries the modifier PostgreSQL's `exprTypmod`
gives it, in the RowDescription and in the table CREATE TABLE AS makes.

PostgreSQL reads that modifier from two different forms of the query, and
the server now follows each:

- **The planned query.** A portal's Describe, a populating CREATE TABLE AS
  and a SELECT from a view read it after `eval_const_expressions` has
  simplified the query, so `case when true then 'a'::varchar(3) end`
  describes `varchar(3)`.
- **The analysed query.** A prepared statement's Describe, a view's own
  columns in `pg_attribute` and CREATE TABLE AS ... WITH NO DATA read it
  as parse analysis leaves it, so the same CASE is a bare `varchar` there.

Every answer was measured on a private PostgreSQL 15.19 cluster.

#### Fixed

- `secantus-pgplan`: a new `expr_typmod` module ports `exprTypmod` to the
  raw parse tree. A cast or a column carries its modifier. `CASE`,
  `COALESCE`, `GREATEST` / `LEAST` and `ARRAY` carry one only when every
  operand has their type and the same modifier. `NULLIF`, `COLLATE` and
  `current_time(n)` carry theirs. In the planned form, a CASE's literal
  conditions and a COALESCE's literal arguments are folded first.
  `with_analysed_typmods` selects the analysed form.
- A subquery's output def keeps the modifiers of its FROM-less select and
  VALUES list. A join's scope keeps its columns' modifiers, and a USING
  column takes the two sides' common one under every join type
  (`buildMergedJoinVar`). A set operation's column takes the sides'
  common one (`select_common_typmod`), both when it is described and when
  it runs. So does a VALUES list's, compared against the column's common
  type.
- `secantus-pg`: a statement's Describe, a view's `pg_attribute` rows and
  CREATE TABLE AS ... WITH NO DATA read the analysed form. The describe
  cache keys on the form.
  Fixes RP-493 (tests/backlog/test_rp_ctas_subquery.py::test_typmod_through_subquery_and_expressions,
  tests/backlog/test_rp_ctas_subquery.py::test_typmod_planned_and_analysed)

#### Found

- RP-497: a scalar subquery's column has no type modifier, and a CASE
  whose condition folds to a constant only through computation
  (`case when 1 = 1 ...`, or a simple `case 1 when 1 ...`) is not
  simplified before the planned readers see it.
