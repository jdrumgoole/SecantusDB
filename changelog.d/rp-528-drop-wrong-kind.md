### Rust PG server: a DROP of the wrong kind of relation is refused

`drop table m` of a materialized view succeeded: it dropped the relation
holding the rows and left the view's definition in `pg_matviews`, with
nothing under it. Every DROP of a relation now looks each name up first, as
PostgreSQL's `RemoveRelations` does. A relation of another kind refuses the
whole statement with `DropErrorMsgWrongType`'s 42809, and nothing in the
list is dropped.

#### Fixed

- DROP TABLE, VIEW, MATERIALIZED VIEW, INDEX, SEQUENCE and FOREIGN TABLE
  refuse a relation of another kind as PostgreSQL 15.19 does, for example
  `"m" is not a table`. The HINT names the DROP that fits the relation's
  real kind: `Use DROP MATERIALIZED VIEW to remove a materialized view.`,
  `Use DROP INDEX ...`, `Use DROP SEQUENCE ...`, `Use DROP TYPE ...`,
  `Use DROP FOREIGN TABLE ...`. Before, `drop table` of a sequence, and
  `drop table if exists` of an index, answered `DROP TABLE` and dropped
  nothing, `drop materialized view` of a view, table or index was 42P01,
  and most of these refusals had no HINT or the wrong one.
- IF EXISTS no longer excuses a relation of the wrong kind
  (`drop materialized view if exists v` of a view is 42809).
- A list is checked before anything is dropped. `drop table t, m` no longer
  drops `t` before refusing `m`, and `drop table t, nosuch` reports the
  missing table ahead of `t`'s dependants. Under IF EXISTS, each missing
  name's `... does not exist, skipping` NOTICE comes before the CASCADE
  notice, in PostgreSQL's order.
- A missing foreign table is 42704 `foreign table "f" does not exist`.

Fixes RP-528 (tests/backlog/test_rp_found_rp525.py::test_drop_of_the_wrong_relation_kind,
tests/backlog/test_rp_found_rp525.py::test_drop_relation_kind_matrix).

#### Found

- RP-533: a DROP naming a schema that does not exist reports the relation
  missing, not 3F000 `schema "nosch" does not exist`
  (tests/backlog/test_rp_found_rp528.py::test_drop_in_a_missing_schema).
