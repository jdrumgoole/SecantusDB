### Rust PostgreSQL server: psql's `\d` answers at once

A correlated subquery whose equalities name the outer row on two relations
of a comma join -- psql's column query for `\d <table>` -- no longer runs
as one scan of the relations' cross product per call. psql's `\d` on a debug
build took over a minute for one column; it now answers in milliseconds.

#### Fixed

- `secantus-pgplan`: the run-once hash path for correlated subqueries
  (`semijoin_hash`) is not taken when its keys sit on more than one relation
  of the subquery's FROM, which made it build their cross product; such a
  subquery runs per outer row, each relation filtered by its key. Fixes
  RP-077 (`tests/backlog/test_rp_catalog_latency.py::test_psql_describe_collation_subquery`).
