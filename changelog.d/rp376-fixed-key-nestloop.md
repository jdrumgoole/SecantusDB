### Rust PG server: EXPLAIN plans a join whose WHERE fixes the join key over an indexed table as PostgreSQL's Nested Loop

A WHERE that sets a join key to a constant puts the constant in the key's
equivalence class, so both sides are filtered by it and no join clause is
left (`generate_base_implied_equalities_const`). Over a table with an index
such a join is planned as joined rows, and EXPLAIN printed an unpriced
`Hash Join`. It now prints PostgreSQL 15.19's `Nested Loop`
(`cheapest_nest_loop`), with each side read through its index where
`set_cheapest` keeps that path, and the implied equality as the other side's
`Filter`.

#### Fixed

- `explain select * from e join k on e.b = k.b where k.a = 5 and k.b = 'v5'`
  prints `Nested Loop (cost=0.28..26.79 rows=100 width=14)` over `Index Scan
  using k_pkey on k`, as do the comma-join form and a constant written on
  the other side; `where k.b = 'v5'` alone prints the Nested Loop over a
  Materialize of k's Seq Scan.
- A WHERE conjunct the planner had already pushed into a join side's scan
  is no longer priced twice in that scan's filter.

Fixes RP-376 (tests/backlog/test_rp_explain_costs.py::test_plan[a key fixed by a constant over an index-scan side])
