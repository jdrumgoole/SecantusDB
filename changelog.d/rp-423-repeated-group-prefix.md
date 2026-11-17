### A regex match over a repeated group is estimated

PostgreSQL estimates a regex match from the fixed prefix its compiled NFA
starts with. A capturing group repeated with `+` is compiled as `x*x`, so the
prefix is only the group's first character (`^(w0)+1` is `w`); a
non-capturing group keeps one whole copy (`^(?:w0)+1` is `w0`). The Rust
PostgreSQL server's EXPLAIN now gives both their PostgreSQL 15.19 estimates
instead of zeros.

#### Fixed

- EXPLAIN estimates a regex match whose pattern repeats a group with `+`
  (capturing or not, greedy or not).
  Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with a repeated group])
