### EXPLAIN of a DELETE ... USING prints PostgreSQL's row widths

The join under an `UPDATE ... FROM` or `DELETE ... USING` now carries what
PostgreSQL's does: each relation's row identity (`ctid`) and the columns the
SET list reads, rather than every column of every table. Its EXPLAIN prints
the same widths as PostgreSQL 15.19 -- `width=12` on the join of a DELETE,
`width=10` on each scan.

#### Fixed

- EXPLAIN of a `DELETE ... USING` or an `UPDATE ... FROM` whose SET list
  reads plain columns prints each node's width as PostgreSQL does. A SET
  value computed from a constant still leaves the join unpriced (RP-435,
  trimmed).
  Fixes RP-435 in part (tests/backlog/test_rp_dml_from.py::test_explain_join_widths)
