### The statement after DDL derives each catalog listing once

After any DDL, the first statement on each connection rebuilds the catalog
state its planner reads. In the Rust PostgreSQL server, several parts of
that work were repeated once per table, or once per caller. The list of
index relations walked every table each time it was asked for: by the
relations, by the index definitions, by `pg_class`, and by a `regclass`
input. The comments, the constraint definitions and the `pg_attribute` fill
each looked up every table's oid with a separate fetch of the catalog
documents. Each column of a result whose type was not a common built-in
scanned every table's row type. psql's `\d` collation query straight after a
`CREATE TABLE` took about 0.010 s in a debug build with no other table, and
0.027 s with 800.

The index relations are now derived once per catalog version and storage
planner generation, which every write of an index catalog or of the
collection registry moves. The oid lookups share one map, and a row type is
found by name through a map built once per catalog version. The same query
now takes about 0.022 s with 800 tables. It still grows with the schema,
because each listing derives every relation's entry again per version, and
RP-911 stays open for that remainder. Its gap test now takes the fastest of
five runs and allows a quarter again plus 2 ms: its old bound of half again
plus 5 ms fell inside run-to-run noise once the gap shrank, and passed two
runs in five.

#### Changed

- `secantus-pg`: `try_index_relations` keeps its list per thread for the
  shared table definitions, the oid map and the storage's
  `planner_generation` it was derived under, outside a user transaction
  only. `constraint_defs`, `constraint_comments`, `object_comments` and
  `fill_catalog_columns` look oids up in one `relation_oids` map.
  `composite_with_schema_named` finds a row type through a map keyed by
  resolution name.
- RP-911 in part
  (tests/test_rust_pgserver_slice.py::test_the_index_relations_follow_every_index_change);
  tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables
  stays open.
- Filed RP-921, a `CREATE TABLE` that grows with the number of tables
  (tests/backlog/test_rp_catalog_latency.py::test_create_table_costs_the_same_at_400_tables),
  and RP-923, a renamed index that loses its comment
  (tests/backlog/test_rp_found_rp911.py::test_an_index_comment_survives_a_rename).
