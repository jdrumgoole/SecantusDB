### The statement after DDL derives the row types only when it asks for one

After any DDL, the first statement on each connection republishes the
catalog state its planner reads, and in the Rust PostgreSQL server that
republication listed every table's row type, `tableoid` and publication
status whether or not the statement needed them. psql's `\d` collation
query straight after a `CREATE TABLE` took about 0.010 s in a debug build
with no other table and 0.018 s with 800.

PostgreSQL fills its catalog caches on demand, so a statement that never
names a composite type builds no composite entry. The planner's row-type
tables, the plain tables' `tableoid` texts and the publishable relations are
now published as the catalog documents they derive from, and derived at the
first lookup that needs them. The inputs are captured at publication, so
what is derived later is what that catalog version holds. A lookup of a
`pg_catalog` type's name (`oid`, `text[]`, `"char"`) never reaches a user
type, because `pg_catalog` is searched first, and it is now answered without
those tables, both in the planner and when describing a result's columns. A
`pg_attribute` read keyed on one relation decodes that relation's composite
alone, as PostgreSQL reads it through the index on `attrelid`. The same
query now takes about 0.014 s with 800 tables. RP-911 stays open for the
rest: the constraint definitions, comments, index relations and oid map are
still listed from every table per catalog version.

#### Changed

- `secantus-pgplan`: a `deferred` module (`Deferred`, `Slot`) holds a
  table's derivation until its first lookup; `defer_user_types`,
  `defer_user_composites`, `defer_user_composite_widths`,
  `defer_standalone_composites`, `defer_publishable_relations` and
  `partitions::defer_tableoids` publish one. `pgtypes::is_catalog_type_name`
  answers whether a lookup key reaches a `pg_catalog` type (case-preserved
  user names such as `Text` excepted).
- `secantus-pg`: `publish_user_types` publishes those tables deferred;
  `composite_listing` and `relation_oids_of` derive from the documents
  alone; `user_wire_type` returns early for any `pg_catalog` name; the keyed
  `pg_attribute` read filters composites by the key before decoding them;
  `derive_index_relations` reads each table's index list in place
  (`list_indexes_shared`).
- RP-911 in part
  (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables,
  now measured on two servers taking turns;
  crates/secantus-pgplan/src/tests.rs::a_catalog_type_lookup_derives_no_deferred_composite_table);
  tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables_within_a_tenth
  stays open.
