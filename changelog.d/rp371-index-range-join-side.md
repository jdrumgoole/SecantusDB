### Rust PG server: EXPLAIN prices a join side whose index reads a key range

`EXPLAIN` of a two-table join over analysed tables now prices a side whose
WHERE is a range on its index's leading column. `explain select * from e
join k on e.b = k.b where k.a < 5`, with `k.a` the primary key, prints
PostgreSQL 15.19's plan: `Hash Join (cost=8.40..34.90 rows=400 width=14)`
over a Hash of `Index Scan using k_pkey on k (cost=0.28..8.35 rows=4
width=7)` with `Index Cond: (a < 5)`. Before this it printed zeros, with
the range as a `Filter`.

#### Fixed

- A range on an index's leading column no longer stops its Index Scan
  being priced. Its selectivity already read the column's current ends
  from the index (`get_actual_variable_range`).
- An Index Scan's cost is summed in `cost_index`'s order: the index's run
  cost, the heap I/O, then the per-tuple CPU, added to the startup cost.
  In floating point the order changes the last digit EXPLAIN prints:
  this scan's total is 8.345, which prints as 8.35 summed in that order
  and as 8.34 summed the old way.

Fixes RP-371 (tests/backlog/test_rp_explain_costs.py::test_plan[a join whose index-scan side reads a key range];
regression test `costsize::tests::an_index_scan_sums_its_costs_in_cost_index_order`).
The rest of the item (a WHERE fixing the join key over an indexed table,
presorted merge inputs, parameterized inner index scans) is filed as RP-376.
