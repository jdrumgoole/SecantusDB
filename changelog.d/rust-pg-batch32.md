### Rust PostgreSQL server: ALTER TYPE's attributes, and typed tables

A composite type's attributes can be added, dropped and retyped as in
PostgreSQL 15.19, and `CREATE TABLE ... OF type` makes a typed table whose
columns follow the type.

#### Added

- `ALTER TYPE t ADD ATTRIBUTE`, `DROP ATTRIBUTE [IF EXISTS]` and `ALTER
  ATTRIBUTE ... TYPE`
  (`tests/backlog/test_rp_batch32.py::test_scenario[ALTER TYPE adds and drops attributes of a type in use]`,
  `[ALTER TYPE attribute errors]` and
  `[ALTER TYPE ... CASCADE alters the typed tables]`).
  - ADD and DROP are allowed while a column uses the type. A stored value
    reads an added attribute as NULL and loses a dropped one, in a column
    of the type, an array of it, or a composite over it.
  - A dropped attribute keeps its attnum as PostgreSQL's dropped slot
    (`........pg.dropped.N........` in `pg_attribute`).
  - ALTER ATTRIBUTE TYPE is 0A000 while a column uses the type.
  - A type with typed tables is 2BP01 unless the action says CASCADE,
    which changes their columns too. That includes RENAME ATTRIBUTE.
  - The errors are PostgreSQL's: 42P01 for a type that is not a
    composite's relation, 42809 for a table's row type, 42701 and 42703
    for the attribute.
- `ALTER TABLE t OF type` and `NOT OF`, with `ATExecAddOf`'s 42804s for a
  table whose columns are not the type's.

#### Fixed

- Fixes RP-156 (`tests/backlog/test_rp_batch32.py::test_scenario[CREATE TABLE OF a type takes its attributes]`
  and `[ALTER TABLE OF and NOT OF]`): `CREATE TABLE t OF type` made a table
  with no columns. It now takes the type's attributes, with `a WITH OPTIONS
  ...` constraints, and reports the type in `pg_class.reloftype`. ALTER
  TABLE cannot add, drop or rename a typed table's column (42809). DROP
  TYPE of its type is 2BP01, and CASCADE drops the table.
- Fixes RP-158 (`tests/backlog/test_rp_batch32.py::test_scenario[DROP TYPE CASCADE drops a composite's field from its values]`):
  DROP TYPE ... CASCADE removed a field from the composite types using the
  dropped type but left it in their stored values, which then read
  shifted. The values now lose the field, and its attnum is a dropped slot.

#### Known

- RP-157, filed: a dropped table column takes its attnum with it, where
  PostgreSQL leaves a dropped slot
  (`tests/backlog/test_rp_batch32.py::test_scenario[a dropped table column leaves its attnum]`).
