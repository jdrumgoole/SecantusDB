### Rust PG server: EXPLAIN prices a join side read through an index

`EXPLAIN` of a two-table join over analysed tables now prices a side whose
WHERE an index serves. `explain select * from e join k on e.b = k.b where
k.a = 5`, with `k.a` the primary key, prints PostgreSQL 15.19's plan: `Hash
Join (cost=8.30..27.04 rows=100 width=14)` over a Hash of `Index Scan using
k_pkey on k (cost=0.28..8.29 rows=1 width=7)` with `Index Cond: (a = 5)`.
Before this it printed zeros, with the condition as a `Filter`.

#### Fixed

- A filtered join side over a table with an index is priced as an `Index
  Scan` where `set_cheapest` keeps that path (`cost_index`, as for a lone
  scan, RP-317). Where the comparison is not modelled, the side still
  prints zeros.
- `inner_unique` from a side's own WHERE (`innerrel_is_unique`): a
  constant fixing every column of a unique index leaves at most one row.
  The hash, merge and nested-loop paths with that side inner then use the
  semi-join factors.
- The nested-loop paths are weighed against the hash and merge paths, the
  join clause paid on every pair. Where a nested loop is cheaper, or within
  the fuzz factor, the join is not priced.
- An index scan under a join prints the WHERE conjuncts on its key columns
  as `Index Cond` and the rest as `Filter`, as a lone scan does.
  Fixes RP-369 (tests/backlog/test_rp_explain_costs.py::test_plan[a join whose filtered side is an index scan],
  with `[a join's index-scan side of the columns read]` and `[a reduced left join's index-scan side]`)

#### Found

- RP-371: a range on the index-scan side, and a WHERE that also fixes the
  join key over an indexed table, still print zeros or the wrong node.
- RP-372: a comma join's WHERE equality prints as a `Join Filter` of the
  Hash Join, not its `Hash Cond`.
