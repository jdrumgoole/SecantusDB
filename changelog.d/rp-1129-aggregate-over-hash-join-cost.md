### An aggregate over a hash join is priced in EXPLAIN

PostgreSQL plans a plain aggregate over a join by planning the join first
and putting AGG_PLAIN over its cheapest path, so `select count(*) from e
join k on k.a = e.a` is the same Hash Join the select-list form draws, each
scan narrowed to the key it reads. The Rust PostgreSQL server priced that
join on its own (RP-330), but printed zero costs on the Aggregate and the
Hash Join below it. On an expression key (`k.a = e.a - 1`), it drew an
unpriced Nested Loop with the ON as its Join Filter instead of the Hash
Join. It now prints PostgreSQL 15's plan and costs for both.

#### Fixed

- EXPLAIN of a plain aggregate (no GROUP BY, HAVING, ORDER BY, LIMIT or
  DISTINCT) over a two-table hash join prices the join as the select of
  the rows it aggregates and the Aggregate over it (`cost_agg`, AGG_PLAIN).
  On an expression key it now hashes on that expression, as the select
  does (RP-981).
  Fixes RP-1129 (tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate over a hash join is priced], tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate over a hash join on an expression key is priced])
