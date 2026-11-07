### Rust PG server: a column reference in an INSERT's VALUES is PostgreSQL's 42703 or 42P01, not a gap

A VALUES row of an INSERT is analysed as PostgreSQL's `transformInsertStmt`
analyses it, in the INSERT's own query level: the target table is in the
range table but cannot be referenced, so a column reference there is a
mistake, not a missing feature. Each row's references are checked before
its length and its assignment types, row by row, and before anything is
evaluated.

#### Fixed

- `insert into e values (bb)` and every other column reference in VALUES
  answered `0A000 ColumnRef is not supported yet`. A bare name is now
  42703 `column "bb" does not exist` at its token, with the HINT
  PostgreSQL gives (`Perhaps you meant to reference the column "e.b".`,
  or that `e`'s column cannot be referenced from there). A qualifier naming
  the target, by its name, its alias or its schema, is 42P01 `invalid
  reference to FROM-clause entry for table "e"`; any other is `missing
  FROM-clause entry`; four names are `cross-database references are not
  implemented`.
- `insert into e values (1/0, bb)` was `22012 division by zero`, and
  `values (bb, 'y'), (true, 'x')` reported the second row's type; both are
  the first row's 42703 now.
- A value for a generated column is refused after every row's assignment
  types are checked, as PostgreSQL's rewriter refuses it.

Fixes RP-415 (tests/backlog/test_rp_column_errors.py::test_insert_values_column_reference,
tests/backlog/test_rp_column_errors.py::test_insert_values_reference_forms)
