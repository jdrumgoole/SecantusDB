### Rust PG server: a column reference in a SELECT with no FROM is PostgreSQL's 42703 or 42P01, not a gap

A SELECT with no FROM has an empty range table, so PostgreSQL's
`transformColumnRef` fails on every column reference in it. The server now
checks for that in the order `transformSelectStmt` analyses the clauses
(target list, WHERE, HAVING, ORDER BY, GROUP BY, DISTINCT ON, OFFSET,
LIMIT, then window definitions), before anything is evaluated or folded. It
shares the error with the INSERT VALUES check from RP-415.

#### Fixed

- `select 1 + nosuch`, `select -nosuch`, `select coalesce(nosuch, 1)`, a
  CASE naming the column anywhere, and the same name in WHERE, LIMIT or
  OFFSET answered `0A000 ColumnRef is not supported yet`, and `select
  abs(nosuch)` answered 42883 `function abs(text)`. Each is now 42703
  `column "nosuch" does not exist` at the name, with no HINT.
- A qualified reference (`select nosuch.a + 1`, `select x.y.z`,
  `select nosuch.*`) is 42P01 `missing FROM-clause entry for table`; a
  four-part name in another database is `cross-database references are not
  implemented`; five parts are 42601. Alone they answered 42703 for the
  first name part, and inside an expression 0A000.
- A missing name in ORDER BY, GROUP BY or DISTINCT ON was ignored, so
  `select 1 group by nosuch` and `select 1 as x order by x + 1` returned a
  row. They are 42703 now. A bare name matching an output column (`select
  1 as x order by x`, `select abs(1) order by abs`) still works.
- `select current_database` and `select version` without parentheses
  returned a value. As on PostgreSQL, they are now columns that do not
  exist.
- What PostgreSQL analyses before a reference still fails first: an
  untyped literal's cast (`select 'x'::int, 1 + nosuch` is 22P02) and a
  call to a function that does not exist (`select nosuchfn(1), nosuch` is
  42883).

Fixes RP-441 (tests/backlog/test_rp_fromless_analysis.py::test_missing_column_in_an_expression_with_no_from,
tests/backlog/test_rp_fromless_analysis.py::test_reference_in_every_clause_with_no_from)
