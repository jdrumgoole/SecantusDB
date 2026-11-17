### EXPLAIN estimates a LEFT join over an expression-filtered preserved side

A LEFT join whose WHERE compares only expressions over its preserved side,
such as `select e.a from e left join w on e.a = w.a where length(e.b) > 2`,
now prints PostgreSQL's estimate: the filtered scan is hashed and the join is
a Hash Right Join with the scan's selectivity carried through, where before
every number was zero.

#### Fixed

- EXPLAIN of a LEFT join whose WHERE filters the preserved side by an
  expression prints PostgreSQL 15's costs and rows.
  Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a left join over a preserved scan filtered by a call])
