### A CHECK constraint can test the whole row

The Rust PostgreSQL server now accepts a CHECK constraint that names its
table's whole row. `check ((t.*) is not null)`, `check (t is not null)`,
`check (row(t.*) is not null)`, `check (t::text <> '...')` and
`check ((t).x > 0)` are created, enforced on every row and printed by
`pg_get_constraintdef` as PostgreSQL 15.19 prints them. Before, each one was
refused as unsupported, or as a missing column. The row is the table's row
as it is now, so a column added later is part of it, and a renamed table's
constraint still tests its own row.

#### Fixed

- A whole-row reference in a CHECK, in CREATE TABLE or ALTER TABLE ADD
  CONSTRAINT, is resolved as `transformExpr` resolves it over the
  constraint's one range-table entry. A bare table name that is not a
  column is the whole row, stored as `t.*`. `(t).x` is the column `x`, and
  it names the constraint `t_x_check`. `ROW(t.*)` is a row of the columns,
  printed `ROW(x, y)`. When a constraint is evaluated per row, the whole row
  becomes a row of the table's columns, typed as the table's row type.
- The new table's row type can be resolved while CREATE TABLE cooks its
  constraints, because `heap_create_with_catalog` makes the type first. So
  `check ((t.*) = row(1)::t)` is accepted.
- These are refused where PostgreSQL refuses them, at the same position:
  `(t.*).nosuch` is 42703 `column t.nosuch does not exist`, `other.*` is
  42P01 `missing FROM-clause entry for table "other"`, and `check (t.*)` is
  42804 `argument of CHECK must be type boolean, not type t`.
- A whole row used as a WHERE condition or as the argument of IS TRUE is
  now 42804 `argument of WHERE must be type boolean, not type t`. Before,
  it matched no rows.
- `pg_get_constraintdef` prints a whole-row Var qualified (`t.*`), and a
  cast of one as `(t.*)::text`.
- Fixes RP-860 (tests/backlog/test_rp_found_rp816.py::test_a_whole_row_reference_in_a_check_is_taken,
  tests/backlog/test_rp_found_rp816.py::test_a_whole_row_check_is_enforced).
- Regression test: `secantus-pgplan` `tests::a_check_takes_its_tables_whole_row`.
- Filed while working it: RP-939 (renaming a column leaves its CHECK naming
  the old column), RP-940 (`NOT VALID` missing from `pg_get_constraintdef`),
  RP-941 (a `regtype` comparison is printed without its `oid` casts).
