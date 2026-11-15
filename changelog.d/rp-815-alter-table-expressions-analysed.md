### ALTER TABLE analyses its expressions over an empty table

An ALTER TABLE expression that could never be evaluated was accepted as long
as no row reached it. `alter column x type bool using (x is true)` and `add
column y bool generated always as (x is true) stored` over an `int` column
of an empty table now fail with PostgreSQL's 42804 `argument of IS TRUE must
be type boolean, not type integer`, the USING at its position and the
generated column with none. `alter column x set default (1 is true)` reports
that error with no position, as PostgreSQL does, since ALTER TABLE cooks a
default under a parse state with no query text.

#### Fixed

- Rust PG server: ALTER COLUMN TYPE ... USING and ADD COLUMN ... GENERATED
  analyse their expression over the table whatever rows it holds; SET
  DEFAULT's errors carry no position.
  Fixes RP-815 (tests/backlog/test_rp_found_rp811.py::test_alter_table_expressions_are_coerced_to_boolean)
