### A CHECK may name `tableoid`

`tableoid` is the one system column PostgreSQL lets a CHECK constraint
read, and the Rust PostgreSQL server now takes it: `check (tableoid::int >
0)` creates the constraint at CREATE TABLE and ALTER TABLE instead of
failing with `column "tableoid" does not exist`, and each row is checked
against the oid of the relation it is stored in -- for a partitioned
table, the leaf partition the row lands in. A bare `check (tableoid)` is
refused as `argument of CHECK must be type boolean, not type oid`, and the
generated name of a CHECK over `tableoid` alone is `<table>_tableoid_check`.

#### Fixed

- `tableoid` in a CHECK resolves to the row's relation oid; `oid` and the
  other identifier types are refused as a WHERE/CHECK boolean.
  Fixes RP-859 (tests/backlog/test_rp_found_rp816.py::test_tableoid_in_a_check_is_a_column,
  tests/backlog/test_rp_found_rp816.py::test_a_checks_tableoid_is_the_rows_relation)
