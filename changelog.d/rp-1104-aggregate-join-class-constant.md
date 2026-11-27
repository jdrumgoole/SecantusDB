### An aggregate over a join that a WHERE constant fixes is a Nested Loop in EXPLAIN

When a WHERE fixes an inner join's key to a constant, PostgreSQL gives
every member of the key's equivalence class that constant, so the join
needs no clause and is a Nested Loop into the inner side's index. A plain
aggregate above the join does not change that plan: `grouping_planner`
plans the join first and puts AGG_PLAIN over its cheapest path. The Rust
PostgreSQL server's EXPLAIN drew the select-list form as that Nested Loop
(RP-1065), but drew `select count(*) from e join k on e.a = k.a where e.a =
3` as an unpriced Aggregate over a Hash Join. It now prints the plan and
costs PostgreSQL 15 does.

#### Fixed

- EXPLAIN of a plain aggregate (no GROUP BY, HAVING, ORDER BY, LIMIT or
  DISTINCT) over a two-table inner join whose key a WHERE constant fixes
  prints the `Nested Loop` with each scan filtered by the constant, the
  inner one through its index (an `Index Only Scan` where the aggregates
  read nothing else of it), and the Aggregate priced over it.
  Fixes RP-1104 (tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate over a join's class constant])
