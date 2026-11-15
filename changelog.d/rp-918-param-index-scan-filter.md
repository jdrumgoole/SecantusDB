### A nested loop into an index keeps the inner table's own filter in its scan

On the Rust PostgreSQL server, EXPLAIN now prices a nested loop whose inner
side is a parameterized index scan when that side also has a condition of
its own that no index serves, such as a `<>`. PostgreSQL 15.19 checks it as
the index scan's `Filter` on each row the scan fetches, and the plan now
prints that way, with PostgreSQL's costs and rows, for a LEFT join's ON
over its nullable side and for an inner join alike.

#### Fixed

- `explain select * from e left join k on e.a < k.a and k.a <> 2` prints
  `Index Scan using k_pkey on k` with `Index Cond: (a > e.a)` and
  `Filter: (a <> 2)` under PostgreSQL's estimate, not two unpriced Seq
  Scans and a Join Filter.

Fixes RP-918 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with an on restriction of its nullable side]).
