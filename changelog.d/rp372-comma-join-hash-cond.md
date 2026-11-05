### Rust PG server: EXPLAIN plans a comma join's equality as its join clause

`EXPLAIN` of a two-table comma join, `FROM e, k WHERE e.b = k.b`, now
prints what PostgreSQL 15.19 prints for it, which is the same plan as the
`JOIN ... ON` form. `explain select * from e, k where e.b = k.b and k.a =
5` prints `Hash Join (cost=8.30..27.04 rows=100 width=14)` with `Hash
Cond: (e.b = k.b)` over the priced `Index Scan using k_pkey on k`. Before
this, it printed zeros, with the equality as a `Join Filter`.

#### Fixed

- A comma join's WHERE equalities between its two tables' columns are the
  join's condition (`Hash Cond`, `Merge Cond`), as an ON's are:
  PostgreSQL makes an inner join's WHERE and ON quals one list
  (`deconstruct_jointree`). A WHERE that also holds another conjunct over
  both tables is printed as before.
- The join is priced as the ON form is when its one equality is the join
  key, both tables analysed (`cost_hashjoin`, RP-330, RP-369).

Fixes RP-372 (tests/backlog/test_rp_explain_costs.py::test_plan[a comma join's equality over an index-scan side]).
Files RP-373, RP-374 and RP-375, found while working it.
