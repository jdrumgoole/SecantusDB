### Rust PG server: EXPLAIN prices a Values Scan

`EXPLAIN values (1), (2)` printed `cost=0.00..0.00 rows=0 width=0` on its
Values Scan. It now prints PostgreSQL 15's `cost_valuesscan` estimate: one
`cpu_operator_cost` and one `cpu_tuple_cost` per row, the lists' count as
the rows, and each column's average type width under its common typmod
(`varchar(10)` in every row is 38 bytes, a mix of typmods is 32).

#### Fixed

- A VALUES list of several rows prints PostgreSQL's cost, rows and width
  on its Values Scan.

Fixes RP-326 (tests/backlog/test_rp_explain_costs.py::test_plan[values scan])
