### A regex with a group repeated within a bounded range is estimated

EXPLAIN over a scan filtered by a regex such as `^(w0){1,2}1` now prints
PostgreSQL 15.19's estimate instead of zeros. A group repeated `{m,n}` with
m below n fixes no character of the regex's prefix, whether or not it
captures, so the prefix stops at whatever came before it.

#### Fixed

- A regex match whose group is repeated within a bounded range is
  estimated as PostgreSQL estimates it. Fixes RP-423, in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with a group repeated within a bounded range])
