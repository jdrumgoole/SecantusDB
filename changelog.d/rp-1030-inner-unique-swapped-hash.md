### Rust PostgreSQL server: a hash join that hashes the table written first is `Inner Unique`

`EXPLAIN` decided a join's `Inner Unique` from the table the FROM writes
second. When the cost model hashed the table written first instead, the
join printed `false` even where that table is unique on the join key.
PostgreSQL's `innerrel_is_unique` checks the side the plan reads inner.

#### Fixed

- `explain verbose select * from k join e on e.a = k.a`, with `k.a` the
  primary key and `k` hashed, now prints `Inner Unique: true` as
  PostgreSQL 15 does. The structured formats now print `"Inner Unique":
  true` for it too. Each join now records both sides' uniqueness, and a
  plan that reads the table written first as its inner side takes that
  table's. This covers a swapped hash join, a merge join with the second
  table outer, and a nested loop with the second table outer.

Fixes RP-1030 (tests/backlog/test_rp_explain_costs.py::test_plan[a hash join written inner side first is inner unique]).
