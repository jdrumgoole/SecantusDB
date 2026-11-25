### `pg_get_viewdef` prints 32 more `information_schema` views

`pg_get_viewdef` now deparses 55 of the 65 `information_schema` views
exactly as PostgreSQL 15 prints them, up from 23: `tables`, `views`,
`triggers`, `table_privileges`, the `role_*_grants` and `*_usage` views
and more, which were NULL. The printer now models what those views
needed:

- an operator or an `IN (SELECT ...)` over an `information_schema` domain
  resolves on the domain's base type and shows the relabel PostgreSQL
  adds (`(x.is_grantable)::text = 'YES'::text`,
  `(t.cn)::integer > 2`), in system views and in user views over a column
  of such a domain;
- an `oid` compared with a `reg*` value meets at `oid`, and a `regclass`
  constant prints as `regclassout` gives it (`('pg_proc'::regclass)::oid`,
  where the source wrote `'pg_catalog.pg_proc'`);
- `(f(...)).*` over a built-in returning OUT parameters expands to one
  field selection per column (`(aclexplode(...)).grantor AS grantor, ...`);
- an untyped literal in a set operation's branch takes the column's
  resolved type (`'PUBLIC'::name`), not `text`;
- the integer bitwise operators `&`, `|` and `#`, a mixed-width pair
  widened (`(t.tgtype)::integer & 1`);
- a call with no arguments typed by its one overload
  (`getdatabaseencoding()`);
- a PARTITION BY item parenthesised as an ORDER BY item is, in the pretty
  print.

Views the printer cannot yet reproduce stay NULL rather than print wrong.

#### Fixed

- `pg_get_viewdef` of 32 more `information_schema` views was NULL.
  Fixes RP-798, in part (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_an_information_schema_view)
- A user view comparing an `information_schema` domain column, an `oid`
  with a `regclass` constant, mixed-width integers under `&`, a set
  operation's untyped literal, or expanding `(f(...)).*`, printed as
  written or wrong
  (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_user_views_over_the_system_view_shapes).
