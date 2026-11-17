### A join with bounds on both sides' keys is priced as PostgreSQL plans it

`explain select * from e join k on k.a = e.a where e.a > 998 and k.a < 2000`
now prints PostgreSQL 15.19's nested loop into `k_pkey`, with the inner
side's bound beside the join's equality in `Index Cond: ((a = e.a) AND (a <
2000))`, and costs included. It printed a Hash Join of zeros before: a side
whose WHERE bounds its key but keeps most rows was given up on instead of
being read by the cheaper sequential scan. `EXPLAIN VERBOSE` of such a join,
and of a hash join whose hashed side reads its key's index, now prints its
`Output` lists and qualified names as well.

#### Fixed

- Rust PG server: a join side whose bound on its key loses the index path to
  the sequential scan is priced through that scan (`hash_side_with`, RP-926's
  `user_scan_path_reading`), and its key's bounds are the parameterized index
  scan's quals.
- Rust PG server: `EXPLAIN VERBOSE` decorates a join whose inner relation's
  WHERE conjuncts print in its scan's `Index Cond`.

Fixes RP-978, in part (tests/backlog/test_rp_explain_costs.py::test_plan[a bound on the inner key joins the parameterized index cond], tests/backlog/test_rp_explain_costs.py::test_plan[verbose of a hashed side read through its key's index])
