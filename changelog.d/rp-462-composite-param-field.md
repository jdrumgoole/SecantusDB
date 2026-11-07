### Rust PG server: a SQL function reads a field of its composite parameter

A `LANGUAGE sql` function taking a row type could not read one of its
fields by name: `gr1(r fe)` with the body `select r.a`, and `gr2(r fe)`
with `select gr2.r.a`, were 42P01 `missing FROM-clause entry for table
"r"` where PostgreSQL 15.19 answers the field. The body now resolves such
a reference as PostgreSQL's `sql_fn_post_column_ref` does, after the
column lookup has failed: `p.f`, or `fname.p.f`, is the field `f` of the
parameter `p` when `f` is a field of the parameter's type (a table's row
type or a `CREATE TYPE` composite), unless a relation of the statement
known as `p` may have a column `f`, which still comes first. A field the
type does not have, or a parameter that is not composite, keeps the 42P01.

#### Fixed

- A field of a SQL function's composite parameter, written `p.f` or
  `fname.p.f`, reads the argument's field.
  Fixes RP-462 (tests/backlog/test_rp_function_bodies.py::test_composite_parameter_field)
