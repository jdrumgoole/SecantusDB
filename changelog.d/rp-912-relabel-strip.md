### A binary-coercible cast in a WHERE is planned as PostgreSQL plans it

A cast between `text` and `varchar` is a relabel PostgreSQL drops while
planning, so `where b::varchar <> 'x'` over a text column keeps the
column's statistics. The Rust PostgreSQL server's EXPLAIN now prints that
filter bare, `(b <> 'x'::text)`, and prices the scan and any join over it
as PostgreSQL 15.19 does, instead of keeping the casts and printing zero
costs.

#### Fixed

- EXPLAIN collapses RelabelType chains in WHERE and JOIN ON comparisons
  (`relabel::strip_relabels`) and when deparsing a planned expression.
  Fixes RP-912 (tests/backlog/test_rp_explain_costs.py::test_plan[a seq scan filter over a binary-coercible cast], tests/backlog/test_rp_explain_costs.py::test_plan[a join filter over a binary-coercible cast])
