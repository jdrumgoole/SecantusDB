### EXPLAIN estimates a regex with a group repeated twice or more

A regular-expression match whose anchored pattern repeats a group at least
twice, such as `c ~ '^(w0){2,}1'`, now gets PostgreSQL's estimate in the
Rust PostgreSQL server's EXPLAIN instead of zeros. PostgreSQL's regex
compiler builds `x{m,}` as `x{m-1,}x`, down to `x+` and the copies after it,
so the fixed prefix the planner reads is one whole copy of the group.

#### Fixed

- EXPLAIN estimates a regex match with a group repeated `{m,}`, m of 2 or
  more, capturing or not.

Fixes RP-423, in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with a group repeated twice or more])
