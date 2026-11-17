### EXPLAIN estimates a join whose WHERE filters one side by a pattern

`explain select * from e join s on s.a = e.a where s.c like 'xxx%'` printed zeros for every node on the Rust PostgreSQL server. A single table's scan already read a LIKE, ILIKE or regex match through `patternsel`, but only when the statement had one relation, so a join's pushed-down pattern conjunct had no pattern to estimate. The server now collects a join's WHERE patterns as well. Each one is assigned to its table by the column's qualifier, or by the only FROM table that has the column. The side's scan, its Hash and the join print PostgreSQL 15.19's estimates.

#### Fixed

- EXPLAIN of a join over a scan filtered by LIKE, ILIKE, NOT LIKE or a regex match prints PostgreSQL 15.19's costs and rows.

Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a join over a scan filtered by a LIKE], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a join over a scan filtered by a regex match])
