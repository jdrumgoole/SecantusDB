### Rust PG server: an INSERT's query and every WITH query are type-checked

An operator with no overload for its operands' types is now refused with
PostgreSQL's 42883 inside an INSERT's SELECT and inside a WITH query.
Before, `insert into k select * from e where a = true` (an integer `a`)
inserted nothing, and `with t as (select * from e where a = true) select
* from t` returned no rows, where PostgreSQL 15.19 refuses both at parse
analysis.

#### Fixed

- Every CTE's query is checked as `transformWithClause` analyses it,
  whether or not the statement reads it, with the CTEs before it in view
  (under WITH RECURSIVE, all of the clause's names). This covers a
  SELECT's WITH, an INSERT's, an UPDATE's and a DELETE's, and a
  data-modifying CTE (`with x as (update ... where a = true returning *)`).
  An UPDATE or DELETE carrying a WITH had been skipped by the check
  altogether.
- An INSERT's SELECT is checked with the INSERT's own CTEs in view, as
  `transformInsertStmt` does. Its result was discarded before.
- Fixes RP-427
  (tests/backlog/test_rp_analysis_order.py::test_insert_select_and_cte_are_type_checked),
  with a crate regression test beside it
  (`secantus-pgplan` `tests::an_insert_query_and_every_cte_body_are_type_checked`).
- Filed RP-431: a FROM subquery or CTE written `select *` is still not
  type-checked (`select * from (select * from e) t where a = true`
  returns no rows).
- Filed RP-432: an UPDATE or DELETE cannot read the CTEs of its own WITH
  (42P01 `relation "t" does not exist`).
