### Rust PostgreSQL server: EXPLAIN estimates a regex with a back reference

A regex match with a back reference, such as `c ~ '^(w)\1'`, printed a
zero estimate in EXPLAIN. PostgreSQL's regex compiler copies the group a
back reference names into its place, and its prefix walk reads that copy,
so `^(w)\1` has the fixed prefix `ww`. The planner now reads the regex the
same way, and the histogram match runs on a backtracking engine when the
linear one refuses the pattern.

#### Fixed

- `explain select * from w where c ~ '^(w)\1'` prints PostgreSQL 15.19's
  `Seq Scan on w (cost=0.00..18.50 rows=1 width=10)`, and an index scan
  takes the back reference's copy into its prefix range.
  Fixes RP-423, in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with a back reference])
