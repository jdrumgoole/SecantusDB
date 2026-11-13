### CREATE TABLE no longer reads every table's definition

Creating a table on the Rust PostgreSQL server took longer the more tables
the database already held: about 1.4 ms with none and 42 ms with 800 in a
debug build, where PostgreSQL 15.19 takes 0.4 ms either way. While the
statement ran, looking up the new table's index models and checking that no
index already had its name each read and decoded the whole catalog. The
statement now reads only the new table's own indexes (and those it inherits
as a partition) and checks index names without decoding any definition, so
the same `CREATE TABLE` takes 1.2 ms with no other table and 5.9 ms with
800. The remaining growth comes from the relation-name checks and is tracked
as RP-928.

#### Fixed

- `btree_built`'s three page-model lookups (`hash_shapes`, `am_shapes`,
  `btree_shapes`) ask the table's own index relations while a
  catalog-changing statement runs (`try_table_index_relations`: its catalog
  row, its index list and its partitioned ancestors'), not every table's
  list, which re-read and decoded the whole catalog three times per
  statement. Outside one they keep their per-version memos.
- `CREATE TABLE`'s "is an index already called this?" check, and
  `relation_exists` / `relation_exists_in`, list index names by table name
  read from the raw catalog rows (`all_index_docs`), with no `TableDef`
  decoded.
- No answer changes: a probe of constraint, partition and hash indexes
  answers the same before and after.
- Fixes RP-921 (tests/backlog/test_rp_catalog_latency.py::test_create_table_costs_the_same_at_400_tables).
  Regression test:
  `tests/test_rust_pgserver_slice.py::test_create_table_records_its_constraint_indexes_as_built`.
- Filed: RP-928 (the remaining growth), and from a probe on a private 15.19
  cluster RP-929..RP-932 (a hash index on an empty table, a leaf
  partition's inherited indexes, a partitioned table's index pages, a
  partition's copy of a user-named index), each with a gap test in
  `tests/backlog/test_rp_found_rp921.py`.
