### EXPLAIN VERBOSE prints a materialized nested loop's output lists

A nested loop that evaluates its ON as a `Join Filter` over a scan and a
Materialize of the other relation now prints, under `EXPLAIN VERBOSE`,
every node's `Output` and each scan's schema-qualified relation, as
PostgreSQL 15.19 does: the outer scan its physical target list, the
Materialize and the scan beneath it only the columns the join needs.

#### Fixed

- `explain verbose select * from e join k on e.a <> k.a` prints its
  `Output` lists, with a WHERE conjunct of either relation as that scan's
  prefixed `Filter`. RP-966 stays open for the remaining join shapes; a
  nested loop over a filtered inner relation that is not priced is filed
  as RP-1038.

Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a nested loop with a join filter])
