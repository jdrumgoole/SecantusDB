### Rust PG server: EXPLAIN hashes the side PostgreSQL hashes

`EXPLAIN` of an inner join of two analysed tables on one column equality
now hashes whichever table PostgreSQL 15.19's cost model picks, so `select *
from e join k on e.b = k.b` prints `Seq Scan on k` as the outer side and the
`Hash` over `Seq Scan on e` when hashing `e` is cheaper. The `Hash Cond` is
written outer side first, `(k.b = e.b)`, as PostgreSQL writes it, and the
plan is priced. Before this, such a plan printed zeros.

#### Fixed

- The two hash paths are compared as `add_path` compares them: the
  right-hashed path is built first and kept unless the other is cheaper
  beyond the 1.01 fuzz factor, and `set_cheapest` takes the cheaper total.
  Fixes RP-359 (tests/backlog/test_rp_explain_costs.py::test_plan[hash join hashing the left table])
