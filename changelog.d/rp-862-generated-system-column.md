### A system column in a generation expression is refused

A stored generated column computes from the row being written, and a system
column such as `xmin` is not part of that row's value. PostgreSQL refuses it
when the table is created; the Rust PostgreSQL server now does the same,
with 42P10 `cannot use system column "xmin" in column generation expression`
at the reference, and still accepts `tableoid`.

#### Fixed

- `GENERATED ALWAYS AS (...) STORED` naming a system column other than
  `tableoid` is 42P10 at the reference, as on PostgreSQL 15.19.
  Fixes RP-862 (tests/backlog/test_rp_found_rp816.py::test_a_system_column_in_a_generated_column_is_42p10)
