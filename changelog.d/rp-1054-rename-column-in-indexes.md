### A renamed column is renamed in the table's indexes

`ALTER TABLE ... RENAME COLUMN` on the Rust PostgreSQL server now carries
the new name into every index of the table that names the column -- its
keys, its key expressions, its `INCLUDE` list and its `WHERE` predicate --
as PostgreSQL's index, which holds the column's attnum, does.

#### Fixed

- After `create index i on t ((x + 1)) where x > 0` and `rename column x to
  y`, the index still named `x`: the next INSERT failed with 42703 and
  `pg_get_indexdef` printed the old name. The index's stored text now has
  the new name spliced in where the old one stood, so it prints `(((y +
  1))) WHERE (y > 0)` as PostgreSQL 15.19 does, and the table takes rows.
- A plain index on a renamed column stayed keyed on the stored field the
  rows no longer carry: `pg_get_indexdef` printed NULL for it, and its
  entries keyed a field no row has. It is now built again over the new
  field, and a UNIQUE one refuses a duplicate after the rename.
- Fixes RP-1054 (`tests/backlog/test_rp_found_rp860.py::test_a_renamed_column_is_renamed_in_an_expression_index`).
  Files RP-1082: `pg_get_indexdef` prints a plain key column and an
  INCLUDE column unquoted.
