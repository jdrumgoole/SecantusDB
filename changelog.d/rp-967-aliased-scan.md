### An aliased relation's scan names its alias in EXPLAIN

`explain select * from k x where x.a > 5` now prints `Seq Scan on k x`
with its `Filter: (a > 5)`, as PostgreSQL does: the scan names the alias
after the relation, and the WHERE's quals are read with the relation in
scope under its alias. Before, the alias was dropped and the quals, which
named `x.a`, could not be printed at all, so a VERBOSE plan of the same
statement fell back to the plain one too.

#### Fixed

- EXPLAIN of a single aliased relation names the alias on its scan and
  prints its quals.
  Fixes RP-967 (tests/backlog/test_rp_explain_costs.py::test_plan[a scan of an aliased relation])
