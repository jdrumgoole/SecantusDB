### A streamed `find` names the plan it ran in its `planSummary`

The profile entry, slow-query line and `getMore` lines of a `find` named a
plan made again from the index catalog alone, so a rooted `$or` whose
branches won their indexes by trial could name one index twice.

#### Fixed

- A `find` over `{$or: [{a: {$gte: 2}, b: {$gte: 3}}, {a: 0, b: 0}]}` with
  indexes on `a` and `b` is profiled as `IXSCAN { a: 1 }, IXSCAN { b: 1 }`,
  as on mongod 8.2.11, not `IXSCAN { a: 1 }, IXSCAN { a: 1 }`.

Fixes RM-576 (tests/backlog/test_rm_found_rm471.py::test_a_rooted_ors_plan_summary_names_each_branchs_index)
