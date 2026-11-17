### Joins of tables never analysed are priced

A table nobody has run ANALYZE on still gets a plan from PostgreSQL: it sizes
the table at its default 1,270 rows and takes the join key's distinct count
from `get_variable_numdistinct`'s defaults. The Rust PostgreSQL server's
EXPLAIN now does the same for a two-table join, so `select * from k, e where
k.a = e.a` over fresh tables prints PostgreSQL 15.19's `Hash Join
(cost=38.58..64.62 rows=1270 width=72)` with the unique side hashed, and a
join on non-unique keys its Merge Join, instead of an unpriced join in written
order. Row estimates now round a half to even, as PostgreSQL's `rint` does.

#### Fixed

- EXPLAIN prices a hash or merge join of two tables never analysed, and
  orders its sides as PostgreSQL does (the same plan under a DELETE ...
  USING).
  Fixes RP-1019 (tests/backlog/test_rp_dml_from.py::test_explain_prints_the_join)
- Row estimates (`clamp_row_est`) and merge / semi-join row counts round a
  half to the even neighbour, and a join's matched tuples multiply the
  selectivity first, as `approx_tuple_count` does.
