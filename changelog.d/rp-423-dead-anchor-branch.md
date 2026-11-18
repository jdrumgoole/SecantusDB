### A regex branch whose anchor follows a character is pruned from the estimate

A regular expression such as `^w(?:^x|0)0` holds a branch that can never
match: its `^` comes after the `w` has been consumed. PostgreSQL's regex
compiler prunes that branch and reads the fixed prefix from what is left
(`w00`), which gives the planner its row estimate. The Rust PostgreSQL
server now prunes the same branches, so EXPLAIN prints PostgreSQL 15.19's
estimate for such a filter instead of zeros.

#### Fixed

- EXPLAIN estimates a regex match whose group has a branch with a `^` a
  character precedes, pruning that branch as `regc_nfa.c` does.
  Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with a dead anchor in a branch])
