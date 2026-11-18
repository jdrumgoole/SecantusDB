### A regex with an anchor in one of its leading branches is estimated

A regular expression whose leading alternatives hold a `^`, such as
`c ~ '^(?:w|^x)0'`, used to leave EXPLAIN printing zeros for its scan. The
Rust PostgreSQL server now does what PostgreSQL 15.19 does: that `^` is a
second anchor arc out of the regex's start state, so the regex has no
fixed prefix, and the scan is estimated from the column's statistics
(`rows=1000` over a table where every value matches).

#### Fixed

- EXPLAIN estimates a regex match whose leading alternatives hold a `^`
  (`^(?:w|^x)0`, `(?:^w|^x)0`, `^(?:w|^x)?0`) instead of printing zeros.
  Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with an anchor in a branch])
