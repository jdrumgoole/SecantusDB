### Rust PG server: EXPLAIN counts an IN list's NULL element in its cost

`explain select * from e where a in (1, null, 3)` now prices the Seq Scan
as PostgreSQL 15.19 does, `cost=0.00..18.75`: a `ScalarArrayOpExpr` costs
half its array's length an operator a row, NULL elements included
(`estimate_array_length`), while the NULLs still select nothing.

#### Fixed

- An `IN` list's NULL elements were dropped before its cost was counted, so
  its plan cost too little (`17.50`). They now drop out of the selectivity
  only.

Fixes RP-349 (tests/backlog/test_rp_explain_costs.py::test_plan[an IN list with a NULL])
