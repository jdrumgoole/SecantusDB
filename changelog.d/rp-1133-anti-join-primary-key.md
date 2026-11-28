### An anti join against a primary key is priced, as PostgreSQL prices it

`EXPLAIN` of a LEFT join that keeps only its NULL-extended rows (`... left
join k on k.a = e.a where k.a is null`) printed every cost, row count and
width as zero when either side's join key was a primary key, and over an
expression key (`k.a = e.a - 1`) drew an unpriced Nested Loop Anti Join
where PostgreSQL 15 hashes. The Rust PostgreSQL server now gives
PostgreSQL's `Hash Anti Join` with its costs, and `EXPLAIN ANALYZE` counts
the rows below it.

#### Fixed

- The hash anti join of a side whose key a B-tree leads with was refused.
  It is now priced where it wins over the other paths PostgreSQL weighs:
  the merge reading that side presorted by its index across
  `mergejoinscansel`'s fraction, the nested loop over the inner's cheapest
  path or a Material of it, and the nested loop probing the inner's index
  once an outer row (`final_cost_nestloop`'s anti join with indexed join
  quals, by an index scan and by a bitmap heap scan). Where one of those
  wins, the plan stays unpriced rather than wrongly priced.
- Fixes RP-1133 (`tests/backlog/test_rp_explain_costs.py::test_plan[an anti join against a primary key is priced]`,
  `test_plan[an anti join on an expression key against a primary key is a hash anti join]`).
- Fixes RP-1294, the same with the primary key on the preserved side
  (`tests/backlog/test_rp_explain_costs.py::test_plan[an anti join preserving a primary key is priced]`,
  `test_plan[an anti join preserving a primary key on an expression is priced]`).
  Files RP-1295: where the nested loop probing the primary key's index is
  PostgreSQL's plan (a few outer rows), it is not drawn yet.
