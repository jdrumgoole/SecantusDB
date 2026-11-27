### EXPLAIN prints the plan an ORDER BY under a LIMIT gets over two tables

The Rust PostgreSQL server already ran a join of two tables under an
ORDER BY and a LIMIT in the plan PostgreSQL's search picks for the rows the
LIMIT takes, but EXPLAIN still drew an unpriced Sort over a Hash Join. It
now prints that plan, priced: a Nested Loop reading the ORDER BY's index
(forward or backward) over a materialised inner, a Merge Join, or a
bounded Sort over a Hash Join, under the Limit.

#### Fixed

- `explain select e.b, k.b from e join k on e.a = k.a order by k.a limit 3`
  over two tables never analysed with no WHERE prints PostgreSQL 15.20's
  plan and costs, as do the descending ORDER BY, an OFFSET, a LEFT join
  and a self join. Tables ANALYZE measured, keys that are not integers,
  inequality joins and VERBOSE / ANALYZE still print the plan without the
  search (RP-1099 stays open for them).
- `EXPLAIN (FORMAT JSON)` prints a backward index scan as an `Index Scan`
  (or `Index Only Scan`) with `"Scan Direction": "Backward"`, as
  PostgreSQL does, instead of a node type `Index Scan Backward`.

Fixes RP-1099, in part (tests/backlog/test_rp_explain_costs.py::test_plan[an order by under a limit over a never-analysed join reads the index])
