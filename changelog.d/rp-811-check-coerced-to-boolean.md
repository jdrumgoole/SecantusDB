### Rust PG server: a CHECK constraint must be a boolean expression

A table's CHECK constraint accepted an expression of any type.
`create table t (x int check (x))` created the table, and the constraint
then let every row through, 0 and NULL included. `check (x is true)` over
the same column refused every row. PostgreSQL 15.19 refuses the table
itself: `cookConstraint` coerces the expression to boolean, so `check (x)`
is 42804 `argument of CHECK must be type boolean, not type integer`, and
`check (x is true)` or `check (x and true)` raise their own test's 42804.

CREATE TABLE and ALTER TABLE ... ADD CHECK (an added column's CHECK
included) now analyse the expression as PostgreSQL does. The error comes in
PostgreSQL's order: `transformExpr` reads left to right and the whole
expression is coerced last, so a missing column written earlier is still
the 42703. An untyped literal is read as a boolean (`check ('t')` is
accepted, `check ('x')` is 22P02). Under ALTER TABLE these errors carry no
position, as on PostgreSQL, whose ALTER passes no query text to
`AddRelationNewConstraints`. That applies to an unknown column's 42703 and
a literal's 22P02 in an added CHECK too, which were positioned before.

#### Fixed

- `secantus-pgplan` (`optype.rs`): `check_constraint` walks a CHECK
  expression over its table and coerces it to boolean under `CHECK`.
- `secantus-pgplan` (`lib.rs`): `cook_check` runs it before the existing
  column check, in PostgreSQL's order of errors. `set_error_unpositioned`
  marks an error PostgreSQL reports with no position, and `error_position`
  honours it.
- Fixes RP-811 (tests/backlog/test_rp_found_rp808.py::test_a_check_constraint_is_coerced_to_boolean)

#### Filed

- RP-813: a domain's CHECK and a partial index's WHERE are not coerced to
  boolean.
- RP-814: a boolean coercion passes an array, and names a domain by its
  base type.
- RP-815: ALTER TABLE's USING, generated and DEFAULT expressions are not
  coerced when no row reaches them, and SET DEFAULT's error has a position.
- RP-816: a system column in a CHECK is 42703, not 42P10.
