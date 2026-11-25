### A renamed column is renamed in the generated columns computed from it

On the Rust PostgreSQL server, `ALTER TABLE ... RENAME COLUMN` now rewrites
every `GENERATED ALWAYS AS (...) STORED` expression that names the column,
as it already did for CHECK constraints and the partition key. PostgreSQL
stores the analysed expression, which refers to the column by attnum, so
the rename carries over there.

#### Fixed

- After `rename column x to y` on a table with `g int generated always as
  (x * 2) stored`, the stored expression still named `x`, so every later
  INSERT or UPDATE failed with 42703. The expression is now renamed, and
  `information_schema.columns.generation_expression` and `pg_get_expr` of
  its `pg_attrdef` row print the new name, quoted where it needs to be
  (`(("Big Y" * 2) + z)`), as PostgreSQL 15.19 does.
- Fixes RP-1053 (`tests/backlog/test_rp_found_rp860.py::test_a_renamed_column_is_renamed_in_a_generated_column`,
  `test_a_rename_to_a_quoted_name_is_printed_in_a_generated_column`).
