### EXPLAIN estimates a filter that casts a column

A comparison like `a::text <> '5'` or `a::bigint > 5` asks PostgreSQL to
estimate an expression it holds no statistics for. It treats the cast column
as an unknown with its default distinct count, so an equality keeps one row
in two hundred and a range a third, and it charges the cast on every row: one
function call for a cast that has one, or an output and an input function for
a conversion through text. Before this change the Rust PostgreSQL server
printed zero costs and rows for any scan or LEFT join that tested a cast
column.

The server now prices such a comparison as PostgreSQL does, both as a single
table's Filter and as the ON condition over a LEFT join's preserved side,
where the test stays in the join's filter. Two bounds on the same cast pair
into one range, as they do for any expression.

#### Fixed

- `EXPLAIN` of a scan whose WHERE compares a cast of a column to an integer
  or string type with a constant gives PostgreSQL 15.19's costs and rows.
- `EXPLAIN` of a Nested Loop Left Join whose ON compares a cast of a
  preserved-side column with a constant gives PostgreSQL 15.19's costs and
  rows, over a Material of the nullable side or an index scan of it.
- Fixes RP-354 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[a left join's on conjunct over the preserved side casts a column],
  plus the scan, equality, range and index-scan variants beside it, and the
  crate test `a_cast_of_a_column_is_an_expression_with_its_functions_cost`).
  The item stays open for a regex match, a call bounding an index, and
  joins of three tables into an index or under a coerced `<>`. A
  binary-coercible cast stays RP-912's. RP-924 filed: a nested loop's Join
  Filter is printed in written order, not cheapest first.
