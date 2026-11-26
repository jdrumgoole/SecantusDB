### Rust PG server: an EXPLAIN Sort Key names the expression, not the output column

A single-table `ORDER BY` now prints its `Sort Key` as PostgreSQL does:
the expression each key resolves to, never the name a select-list entry
gives its output.

#### Fixed

- `explain select a+1 from e order by a` printed `Sort Key: ?column?`;
  it prints `Sort Key: a`. So does `select a as x from e order by x`
  (it printed `x`), and `select upper(b) from e order by b` (it printed
  `upper`).
- A bare name that matches an output name is that entry's expression, as
  `findTargetlistEntrySQL92` resolves it: `select a*2 as a from e order
  by a` prints `Sort Key: ((a * 2))`, not `__order0`.
- A primary-key column outside the select list printed as its stored
  field (`select b from k order by a`, `Sort Key: _id`); it prints `a`.

Fixes RP-1066 in part (tests/backlog/test_rp_explain_costs.py::test_plan[an order by on a column only a computed entry reads names the column], tests/backlog/test_rp_explain_costs.py::test_plan[an order by through an output name prints the expression]); the index scan PostgreSQL draws for an ORDER BY on a primary key stays open under RP-1066. Files RP-1126.
