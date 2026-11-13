### EXPLAIN prices a nested loop into a later column of an index

A nested loop whose join clause bounds the second (or a later) column of
the inner table's index now prints PostgreSQL's estimate. `select * from e,
c2 where e.a < c2.y`, with `c2` indexed on `(x, y)`, reads `c2` through
`Index Only Scan using c2_xy` with `Index Cond: (y > e.a)`, and every loop
is priced as PostgreSQL prices it: with no qual on `x` the clause bounds
nothing, so each scan reads the whole index and checks every entry. Before,
the join and both scans printed zeros.

#### Fixed

- EXPLAIN of a nested loop parameterized by an inequality on a later index
  column prints PostgreSQL 15.19's costs, rows and plan (RP-354, part).

#### Added

- A gap test for what RP-354 still leaves at zeros: a key one index leads
  with and another holds later.

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop into a later column of an index])
