### Rust PG server: EXPLAIN prices a hash join's `<>` filter and a LEFT join's filter

A hash join whose ON (or comma WHERE) holds a `<>` beside its equality, and
a LEFT join with any such filter, now print PostgreSQL 15.19's estimate
instead of zeros.

#### Fixed

- `explain select * from e join k on e.b = k.b and e.a <> k.a` prints
  `Hash Join (cost=27.50..1420.00 rows=99900 width=14)`: the `<>` is
  `neqjoinsel`, one minus `eqjoinsel` over the two columns' statistics.
  The comma form plans alike.
- `explain select * from e left join k on e.b = k.b and e.a < k.a` prints
  `Hash Left Join (cost=27.50..1420.00 rows=33333 width=14)`, with `<`,
  `<=`, `>`, `>=` or `<>` filters. A LEFT join with a WHERE stays
  unpriced.

Fixes RP-379 (tests/backlog/test_rp_explain_costs.py::test_plan[a LEFT join's equality with an inequality beside it], tests/backlog/test_rp_explain_costs.py::test_plan[a join's equality with a not-equal beside it])
