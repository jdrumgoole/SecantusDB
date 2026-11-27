### A LEFT JOIN whose WHERE fixes the outer key reads the inner side through its index in EXPLAIN

When a WHERE fixes a LEFT JOIN's outer key to a constant, PostgreSQL gives
the nullable side's key the same constant (`reconsider_outer_join_clauses`)
and keeps the ON as a join clause it no longer needs to select. Where that
constant cuts the inner side to one row through a unique index, the plan is
a Nested Loop Left Join into that index. The Rust PostgreSQL server's
EXPLAIN printed an unpriced Hash Left Join for
`e LEFT JOIN k ON e.a = k.a WHERE e.a = 3`; it now prints the plan and costs
PostgreSQL 15 does.

#### Fixed

- EXPLAIN of a LEFT JOIN whose WHERE fixes the outer key, its nullable side
  unique on the key, prints the priced `Nested Loop Left Join` with the ON
  as its `Join Filter` and the inner side's `Index Scan` under the constant
  (a Materialize of it over many outer rows), wherever that path is cheaper
  than both hash joins.
  Fixes RP-1103 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join's fixed outer key is the inner index's cond])
- The outer scan's Filter puts the fixed key first when the WHERE also
  filters another of its columns. In PostgreSQL the ON creates the outer
  key's class before the WHERE's other classes
  (`initialize_mergeclause_eclasses`): `where e.b = 'v3' and e.a = 3`
  filters `e` by `((a = 3) AND (b = 'v3'::text))`.
  Fixes RP-1103 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join's fixed outer key leads the outer scan's filter])
