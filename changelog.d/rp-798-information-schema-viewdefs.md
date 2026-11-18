### `pg_get_viewdef` prints 23 `information_schema` views

`pg_get_viewdef` now deparses `information_schema` views such as
`schemata`, `applicable_roles` and `foreign_data_wrappers` exactly as
PostgreSQL 15 prints them, where it returned NULL. A cast to an
`information_schema` domain prints as PostgreSQL's `CoerceToDomain` does,
schema-qualified and over a literal folded to the domain's base type
(`('YES'::character varying)::information_schema.yes_or_no`), and a
relation of the view's own schema prints qualified. Views the printer
cannot yet reproduce stay NULL rather than print wrong.

A set operation inside a subquery now names each branch's columns by that
branch's own names, as ruleutils does, where the printer named every
branch after the leftmost one.

#### Fixed

- `pg_get_viewdef` of 23 more `information_schema` views was NULL.
  Fixes RP-798, in part (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_an_information_schema_view)
- A cast to an `information_schema` domain printed the domain unqualified
  and a literal argument without its base-type cast
  (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_casts_to_information_schema_domains).
- A set operation's later branches in a subquery printed the leftmost
  branch's column names
  (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_names_set_operation_branches).
