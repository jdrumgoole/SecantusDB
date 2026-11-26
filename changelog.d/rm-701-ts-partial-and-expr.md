### A time-series read beside a partial index, or with an `$expr`, plans as mongod does

A partial index on a time-series collection is now built over its buckets
with the filter mongod keeps there, and a read tests the bucket filter
against it as mongod's planner does. An `$expr` comparison of a field with
a constant now derives the same bucket predicates a plain filter does.

#### Fixed

- A read that implies a partial index's filter (`v < 3000, v > 5` beside
  `partialFilterExpression: {v: {$gt: 5}}`) scans the index over the
  buckets; one that does not reads the buckets in order. A cursor over it
  returns what mongod's had buffered after a concurrent delete (1,913, not
  2,993).
- An `$expr` filter (`{$gte: ["$v", 0]}`) beside an index of its own reads
  the buckets in order, or the index when it is an `$eq`, as mongod does:
  2,303 after the delete, not 3,999.
- The query matcher knows the `$_internalExprEq`, `$_internalExprGt`,
  `$_internalExprGte`, `$_internalExprLt` and `$_internalExprLte` operators
  mongod parses (no type bracketing, a missing field below everything, an
  array on the path always matching); they were refused as unknown.

Fixes RM-701 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_read_beside_a_partial_index_scans_it_as_mongod, tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_expr_read_beside_an_index_of_its_own_buffers_as_mongod), part
