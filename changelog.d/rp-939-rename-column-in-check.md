### Renaming a column carries its CHECK constraints along

`ALTER TABLE ... RENAME COLUMN` now renames the column inside every CHECK
constraint of the table. PostgreSQL stores a CHECK as an analysed tree that
holds the column's number, so a rename never touches it; the server stores
the expression's text, which kept naming the old column, so every later
INSERT failed with `column "x" does not exist`. The constraint keeps
enforcing on the renamed column and `pg_get_constraintdef` prints the new
name.

#### Fixed

- Rust PG server: a column renamed under a CHECK is renamed in the CHECK.
  Fixes RP-939 (tests/backlog/test_rp_found_rp860.py::test_a_renamed_column_is_renamed_in_its_check)
