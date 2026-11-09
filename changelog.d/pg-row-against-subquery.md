### Rust PG server: a row compares with a subquery's row, pair by pair

`(a, c) in (select fs.a, fs.c)` with the subquery reading the outer row was
refused (0A000 `comparing document with int32`), and so was every `row op
(subquery)` -- `(a, c) = (select 1, 'y')` included, correlated or not --
because the row was compared with the subquery's first column only.
PostgreSQL's `make_row_comparison_op` compares the row with the subquery's
columns pairwise, each pair by the operator its two types resolve to.

#### Fixed

- A correlated `row IN / NOT IN / op ANY / op ALL (subquery)` compares the
  row with each subquery row field by field, with every comparison operator:
  `=` is every pair equal, `<>` any pair unequal, and `<`, `<=`, `>`, `>=`
  decide at the first unequal pair, with SQL's three-valued logic (a NULL
  before the deciding pair makes the comparison NULL).
- `row op (subquery)`, correlated or not, compares the row with the
  subquery's one row as a `RowCompareExpr`. No row is NULL; more than one
  is 21000.
- Each pair is compared as a value of the subquery column's type, so a
  `char(n)` pair ignores trailing blanks as `bpchareq` does.

Fixes RP-697 (tests/backlog/test_rp_found_rp517.py::test_row_against_a_correlated_subquery).
Filed while working it: RP-726 (`varchar = char(n)` counts the trailing
blanks), RP-727 (an uncorrelated `row < ANY (subquery)` is still refused) and
RP-728 (a row pair with no operator between its types is not 42883).
