### Rust PG server: a materialized view is a dependent of what it reads

A DROP followed the views over a relation but not its materialized views.
`drop table t` of a table a materialized view read succeeded, and `drop table
t cascade` or `drop schema s cascade` left the materialized view behind,
reading a relation that was gone. A materialized view is now found exactly as
a view is, as PostgreSQL's `findDependentObjects` reaches both through their
`_RETURN` rules.

#### Fixed

- DROP TABLE, DROP VIEW, DROP SCHEMA and DROP MATERIALIZED VIEW find the views
  and materialized views that read their target, and what reads those,
  depth first and in the order they were made, as PostgreSQL 15.19 names them
  in the 2BP01 DETAIL and the CASCADE notice. RESTRICT refuses
  (`materialized view mv depends on table t`); CASCADE drops them, and a
  ROLLBACK, or a ROLLBACK TO a savepoint, brings them back with their rows.
- `DROP MATERIALIZED VIEW` takes CASCADE, and refuses as
  `cannot drop materialized view mv` (it said `cannot drop table mv`).
- A materialized view in a dropped schema is named `materialized view s.mv`,
  not `table s.mv`.

Fixes RP-525 (tests/backlog/test_rp_found_rp467.py::test_drop_reaches_a_dependent_materialized_view,
tests/backlog/test_rp_found_rp467.py::test_drop_follows_materialized_views_in_a_chain,
tests/backlog/test_rp_found_rp467.py::test_cascade_to_a_materialized_view_rolls_back).

#### Found

- RP-528: a DROP of the wrong kind of relation (`drop table` of a
  materialized view) is not refused as PostgreSQL refuses it
  (tests/backlog/test_rp_found_rp525.py::test_drop_of_the_wrong_relation_kind).
