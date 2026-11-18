### A `^` inside a group anchors a regex's estimate

A regex match whose `^` sits inside a group with nothing before it --
`c ~ '^(?:^w0)1'`, or `c ~ '(?:^w0)1'` -- printed a plan of zeros. The
planner now pulls that anchor forward as PostgreSQL's regex optimiser does,
so the fixed prefix is `w01` and the scan is estimated as PostgreSQL 15.19
estimates it.

#### Fixed

- EXPLAIN estimates a regex match with a `^` no character precedes inside
  a group (`colstats::strip_bos`). Fixes RP-423, in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with an anchor inside a group]).
