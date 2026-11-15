### A CHECK resolves a qualified column against its own table only

A CHECK constraint's expression sees exactly one table, the one it constrains. The Rust PostgreSQL server now refuses another table's qualifier in a CHECK with 42P01 `missing FROM-clause entry for table "other"`, and names a qualified missing column with its qualifier (`column t816.nosuch does not exist`) at the reference's start, as PostgreSQL 15.19 does.

#### Fixed

- `check (other.x = 1)` created the table; it is now 42P01. `check (t816.nosuch = 1)` named `"nosuch"` at the column; it is now `column t816.nosuch does not exist` at the qualifier.
  Fixes RP-861 (tests/backlog/test_rp_found_rp816.py::test_a_checks_qualified_column_is_resolved_against_its_table)
