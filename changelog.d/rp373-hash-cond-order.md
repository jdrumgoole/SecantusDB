### Rust PG server: EXPLAIN writes a Hash Cond outer side first

A hash join's clause written inner side first (`join k on k.b = e.b`)
was printed as written; PostgreSQL commutes it so the outer relation's
operand comes first (`get_switched_clauses`), and so does the Rust PG
server now.

#### Fixed

- `explain select * from e join k on k.b = e.b` prints `Hash Cond: (e.b = k.b)`.
  Fixes RP-373 (tests/backlog/test_rp_explain_costs.py::test_plan[a join clause written inner side first])
