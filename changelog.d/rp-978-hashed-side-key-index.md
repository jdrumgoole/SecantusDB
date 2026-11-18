### A hashed side read through its key's index is priced

A join on a unique key whose hashed side is cut by a range on that key,
`select * from e join k on k.a = e.a where k.a < 10`, now prints
PostgreSQL 15.19's `Hash Join (cost=8.54..26.18 rows=9 width=14)` over an
`Index Scan using k_pkey`, where it printed zeros over a sequential scan.
The index scan is the side's path in the key's order, and the index probed
by the other side's key carries the range beside the join's equality, as
in PostgreSQL's pathlist.

#### Fixed

- EXPLAIN of an inner join on a unique key whose hashed side a range on
  the key reads through its index prints PostgreSQL's estimate.
  Fixes RP-978, in part (tests/backlog/test_rp_explain_costs.py::test_plan[a range on the hashed side's key reads it through its index])
