### Rust PG server: `pg_attribute.attndims` counts a column's declared array dimensions

A client that reads `attndims` to find a table's array columns saw none:
every user column had 0. It is now the number of `[]` the column was
declared with, as PostgreSQL 15 records it.

#### Fixed

- `pg_attribute.attndims` of a user table's column is the number of array
  bounds its type was written with: `int[]` and `int ARRAY[4]` are 1,
  `int[][]` 2, a domain's array 1, and the internal name `_int4` 0. It was 0
  for every column.
  - ADD COLUMN and ALTER COLUMN TYPE record the new declaration, and an
    ALTER on a parent reaches its children.
  - A column PostgreSQL defines from a type OID gets 0, whatever its type:
    one from LIKE, a partition's, an inherited one (even when the child
    declares it too), one from `OF type`, and one from CREATE TABLE AS.
  - An index's key column copies its table column's value.
- Fixes RP-658 (tests/backlog/test_rp_found_rp653.py::test_array_column_attndims,
  tests/backlog/test_rp_found_rp653.py::test_attndims_follows_the_written_definition).

#### Found

- RP-662: ALTER COLUMN TYPE between two array types whose elements have an
  assignment cast (`int[]` to `int8[]`) is refused with 42804.
- RP-663: a composite type's attributes still have `attndims` 0.
