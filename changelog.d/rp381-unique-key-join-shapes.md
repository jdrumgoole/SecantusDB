### Rust PG server: EXPLAIN prices three more joins on a unique key

`EXPLAIN` of an inner join on a key unique on one side printed zero costs
when that side had its own WHERE, when the join was a LEFT join, or when the
ON clause carried a join filter beside the key. Each now prints the Hash Join
PostgreSQL 15.19 picks, with its costs and rows.

#### Fixed

- A side whose WHERE no index reads is priced as its sequential scan, as
  `set_cheapest` keeps it; the unique key's index stays a path for the merge
  joins and nested loops it is weighed against.
- A LEFT join on a unique key weighs those paths as a left join.
- A join filter beside a unique key is charged, as `final_cost_hashjoin`
  charges it, on the outer rows that find their match: the filter's
  selectivity counts among the semi-join factors.

Fixes RP-381 (tests/backlog/test_rp_explain_costs.py::test_plan[a join on a unique key whose indexed side is filtered], tests/backlog/test_rp_explain_costs.py::test_plan[a left join on a unique key], tests/backlog/test_rp_explain_costs.py::test_plan[a join on a unique key with a join filter])
