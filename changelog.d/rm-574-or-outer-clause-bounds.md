### Rust MongoDB server: a clause beside an `$or` that every branch's bounds decide is not rechecked

A query like `{$or: [{y: 0}, {y: 1, x: {$gt: 3}}], x: {$lt: 10}}` over
`{y: 1, x: 1}` now plans as mongod 8.2.11 does: the outer clause is pushed
into each branch's bounds and rechecked nowhere, so a covered projection is
answered from the keys (`PROJECTION_DEFAULT <- OR <- IXSCAN`) with no FETCH.
A clause some branch cannot bound is still rechecked above the `OR`.

#### Fixed

- Fixes RM-574 (tests/backlog/test_rm_found_rm471.py::test_a_clause_beside_an_or_is_answered_by_its_bounds),
  with the regression tests/mongod_live/test_or_outer_clause_bounds.py::test_a_clause_beside_an_or.
