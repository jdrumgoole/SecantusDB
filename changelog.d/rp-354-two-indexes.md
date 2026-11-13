### EXPLAIN picks the cheaper of two indexes holding a nested loop's key

When the inner table of a nested loop has two indexes that could serve its
join clause -- one leading with the key column, another holding it later --
EXPLAIN now prices a parameterized path through each and keeps the cheaper,
as PostgreSQL's `add_path` does. `select * from e, c4 where e.a < c4.y`,
with `c4` indexed on `(x, y)` and on `(y)`, reads `c4` through `Index Scan
using c4_y` with `Index Cond: (y > e.a)` at PostgreSQL's costs, since that
index bounds each scan where the other reads it whole. Before, the join and
both scans printed zeros.

#### Fixed

- EXPLAIN of a nested loop whose inner key several B-trees hold prints the
  cheapest parameterized path's costs, rows and plan, as PostgreSQL 15.19
  does (RP-354, part).

#### Added

- Gap tests for what RP-354 still leaves at zeros: a join of three tables
  whose upper clause is a `<>`, and a join of four tables.

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop into a column two indexes hold])
