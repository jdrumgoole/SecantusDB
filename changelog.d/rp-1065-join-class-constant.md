### A WHERE constant joins an explicit JOIN's equivalence class in EXPLAIN

When a WHERE fixes a JOIN's key to a constant, PostgreSQL puts the constant
in the keys' equivalence class: every table on the class is filtered by it,
no join clause is left, and each join is a Nested Loop into the inner
table's index. The Rust PostgreSQL server's EXPLAIN already drew that for a
comma join; it now draws it for `e JOIN k ON e.a = k.a WHERE e.a = 3` too,
and for a chain of three or more such JOINs, as PostgreSQL 15 does.

#### Fixed

- EXPLAIN of a two-table JOIN whose WHERE fixes a join key prints a priced
  Nested Loop into the inner side's Index Scan, or Index Only Scan where
  the index holds everything that side reads, with the constant as its
  `Index Cond`, rather than an unpriced Hash Join.
  Fixes RP-1065 (tests/backlog/test_rp_explain_costs.py::test_plan[an explicit join's class constant is the inner index's cond])
- EXPLAIN of a chain of inner JOINs whose keys share one class with a WHERE
  constant prints Nested Loops with each table's scan under the constant;
  its costs are still zero, as for every three-way chain (RP-1059).
  Fixes RP-1065 (tests/backlog/test_rp_explain_costs.py::test_plan[a three-way explicit join's class constant is each inner index's cond])
- The constant a class gives the other side is the WHERE's equality on the
  join key, never another conjunct of the fixing side (`where k.b = 'v3'
  and k.a = 3` once printed `Index Cond: (a = 'v3'::text)`), and a fixing
  side read through its index gives its `Index Cond`'s constant to the
  other side's `Filter`, in comma joins too.
