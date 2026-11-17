### A nested loop into an index takes the inner side's own range as an index qual

A join that bounds an indexed column by the outer row (`e.a < k.a`) and also
by a constant of the inner side's own (`k.a > 2`) reads the inner table
through its index with both bounds as one Index Cond. The Rust PostgreSQL
server's EXPLAIN now prices and prints that scan as PostgreSQL 15.19 does,
`Index Cond: ((a > e.a) AND (a > 2))`, instead of zeros over two sequential
scans and a Join Filter.

#### Fixed

- EXPLAIN of a nested loop into an index whose inner side carries its own
  constant bounds of the indexed column: priced, those bounds in the Index
  Cond.
  Fixes RP-979 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop into an index with a range of the inner side])
