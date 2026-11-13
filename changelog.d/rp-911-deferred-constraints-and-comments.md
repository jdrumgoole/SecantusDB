### The statement after DDL renders constraint texts and comments only when it asks for one

After any DDL, the first statement on each connection republishes the
catalog state its planner reads. In the Rust PostgreSQL server that
republication rendered every table's constraint definitions and listed
every table's and column's comments, whether or not the statement called
`pg_get_constraintdef`, `obj_description` or `col_description`. It also
derived each table's index relations and looked up each table's oid,
including for tables with no key and no index.

PostgreSQL fills its catalog caches on demand and renders a constraint's
text when it is asked for. The constraint texts and every table's comments
are now derived at a statement's first lookup of one, from the table
definitions and catalog documents captured when they were published. The
reads that can fail (the comment stores, the domain and function catalogs)
are still made at publication, so a failed read is still the error a
comment lookup raises. A table with no primary key, no unique constraint
and no index of its own is now skipped before any oid lookup when the
index relations are listed. The relation oid map is built only once some
table has an index relation. At about 800 tables, the republication's
sampled cost per catalog version fell from about 3.4 ms to 2.6 ms in a
debug build. RP-911 stays open for the rest, chiefly the catalog
collections that are re-read for each version.

#### Changed

- `secantus-pgplan`: `defer_constraint_defs` and `defer_object_comments`
  publish the constraint texts and the comments as `deferred::Slot`s;
  `deferred::Slot::new` makes a slot for a table with no `Default`.
- `secantus-pg`: `publish_user_types` defers both; `table_constraint_defs`,
  `table_constraint_comments` and `object_comments_builder` derive from
  captured inputs alone; `derive_index_relations` skips index-less tables
  and takes the oid map lazily, and `try_index_relations` keys its memo on
  the catalog documents the map is built from.
- RP-911 in part
  (crates/secantus-pgplan/src/tests.rs::constraint_texts_and_comments_are_derived_at_the_first_lookup);
  tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables_within_a_tenth
  stays open.

#### Found

- RP-927: psql's `\d` column query run straight after DDL, against a table
  with two column types, builds all of `pg_type` (0.10 s at 800 tables
  against 0.016 s with none; PostgreSQL 15.19, 0.33 ms)
  (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_over_two_column_types_costs_the_same_at_400_tables).
