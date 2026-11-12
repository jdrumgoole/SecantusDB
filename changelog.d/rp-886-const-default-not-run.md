### Rust PG server: a DEFAULT's errors come from the INSERT that runs it

`create table t (a int default 1/0)` was refused with `division by zero`,
because a constant default was folded to a stored value at CREATE TABLE
and the fold's error was raised there. PostgreSQL analyses a default where
it is declared but runs nothing, so the table is created and the error
belongs to the first INSERT that uses the default. A string literal is the
one exception: its type's input function reads it during analysis, so
`default 'x'` on an `int` column is still refused at CREATE. This server
now draws that line where PostgreSQL does.

#### Fixed

- CREATE TABLE, ALTER TABLE ... ALTER COLUMN SET DEFAULT, a domain's
  default reaching a column and ALTER DOMAIN ... SET DEFAULT keep a
  constant default whose folding raises a data or integrity error (`1/0`,
  `2147483647 + 1`, `int2 default 100000`, `'x'::text::int`, a domain's
  CHECK) as its expression. The INSERT that runs it raises the error, as
  on PostgreSQL 15.19. A string literal's input error (`default 'x'`,
  `'x' + 1`, `1/0 + 'x'::int`, `int[] default '{1,x}'`) is still raised at
  CREATE. ALTER TABLE ... ADD COLUMN still evaluates the default it keeps,
  as PostgreSQL's does: its own default or the domain's (22012 for a
  domain whose default is `1/0`), even over an empty table. A column whose
  own default replaces a failing domain default is no longer refused for
  the domain's.
- Fixes RP-886 (tests/backlog/test_rp_found_rp881.py::test_a_constant_default_is_not_run_at_create,
  tests/backlog/test_rp_found_rp881.py::test_a_default_literal_input_error_stands_at_create)

#### Added

- RP-892 filed: a folded DEFAULT skips its column's length check
  (`varchar(2) default 'abc'` stores `abc`), with its gap test
  `tests/backlog/test_rp_found_rp886.py`.
