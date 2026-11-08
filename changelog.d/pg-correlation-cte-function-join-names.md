### A subquery reads an outer WITH item's, function's or USING join's columns

The Rust PostgreSQL server now finds a correlated subquery's bare column
name in every kind of outer FROM item, as PostgreSQL does. `with w as
(select 1 a) select (select a) from w` answers 1 instead of a 42703
`column "a" does not exist`, and so does a subquery over a FROM-subquery
that reads a WITH item through a `*`, a function in FROM
(`generate_series(1, 2) g`, `json_each(...)`, `WITH ORDINALITY`), or a
USING or NATURAL join, whose merged columns come first, once. A WITH item
renamed by an alias or its own column list, a recursive one, and one read
by a later item or by a FROM-subquery's own WITH all work, and the value
keeps its type (`integer`, not text). A subquery with its own WITH that
reads the statement's WITH, `select (with w2 as (select * from w) select a
from w2)`, no longer fails with 42P01 `relation "w" does not exist`.

#### Fixed

- `secantus-pgplan`: the names an outer FROM item offers a subquery
  (`collect_from_columns`) come from colhint's range-table model of the
  whole item (`colhint::from_item_columns`). That model now knows the WITH
  items in scope (the statement's own included, read once they are in
  scope, a recursive one too), a FROM function's columns as the planner
  names its result (`joins::function_columns`), and a join's columns as
  `transformFromClauseItem` builds them, USING or NATURAL merged columns
  first and a join alias renaming the leading ones. A level's names are
  read only when one of its subqueries asks (`LevelNames`), so a statement
  with no subquery plans no FROM function for them.
- `secantus-pgplan`: a FROM-subquery an outer reference is typed from is
  planned with the WITH items around it in scope, and a query nested in a
  statement reads the statement's WITH items as an outer scope its own
  WITH items see and hide, not as items appended behind them
  (`under_visible_ctes`).
- Fixes RP-506 (tests/backlog/test_rp_fromless_analysis.py::test_correlation_over_a_cte_or_a_star_of_unknown_columns,
  tests/backlog/test_rp_fromless_analysis.py::test_subquery_with_reads_the_statements_with).
