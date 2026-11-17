### EXPLAIN VERBOSE prints the Sort and Limit above a scan

`explain verbose select * from k where a > 5 order by b` now prints
`Output: a, b` and `Sort Key: k.b` on its Sort, over `Seq Scan on
public.k`, as PostgreSQL does. Every Sort and Limit above one relation's
scan prints the scan's targetlist, with an ORDER BY column the select
list lacks added as PostgreSQL's resjunk entry, and its sort keys
qualified with the relation's name or alias. A join's nodes and a Sort
on a computed key still print as without VERBOSE.

#### Fixed

- EXPLAIN VERBOSE prints `Output` on the Sorts and Limits over a single
  relation's scan and qualifies their `Sort Key`.
  Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a sort over a scan], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a sort over an aliased scan])
