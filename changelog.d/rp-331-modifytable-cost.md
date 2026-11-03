### Rust PG server: EXPLAIN prices an INSERT, UPDATE or DELETE (RP-331)

`explain update e set b = 'y' where a = 1` printed `cost=0.00..0.00 rows=0
width=0` on the `Update on e` node and the Seq Scan below it. PostgreSQL 15.19
gives the ModifyTable its subpath's costs with zero rows and width
(`create_modifytable_path`). The subpath is a one-row `Result` with every
table column for a `VALUES` insert. For an update it is the relation's scan
emitting the assigned columns and the `ctid`; for a delete, the `ctid` alone.
The Rust server now prints those estimates.

#### Fixed

- EXPLAIN of a one-row `INSERT ... VALUES`, of an `UPDATE` with constant
  assignments and of a `DELETE` prints PostgreSQL 15.19's costs, rows and
  widths, with no RETURNING and where the relation's sequential scan is
  PostgreSQL's path.

Fixes RP-331 (tests/backlog/test_rp_explain_costs.py::test_plan[insert],
tests/backlog/test_rp_explain_costs.py::test_plan[update],
tests/backlog/test_rp_explain_costs.py::test_plan[delete])
