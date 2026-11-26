### The statement after DDL no longer grows with the schema, and tables list in the order they were written

On the Rust PostgreSQL server, the first catalog query after a `CREATE
TABLE` (psql's `\d` collation query) now takes the same time at 3,200 tables
as with none: a debug build measured 14.1 ms against 14.4 ms (a ratio of
1.02), where it was 13.1 ms against 14.3 ms. An unsorted listing of the
relations (`pg_class`, `pg_tables`, `information_schema.tables`) now
returns them in the order they were written, as PostgreSQL's heap scan
does, the same within a session and after a restart.

#### Fixed

- Since the move to mongod's storage model a full read of the table catalog
  returns its documents in RecordId order. On any schema whose names were
  not created in sorted order (`t2` after `t10`), the version's `_id`s were
  not sorted, so the patch of the definitions a commit changed (RP-911)
  and the binary search by name (RP-1073) never ran: every DDL re-read and
  compared every definition, and the next statement built a set of every
  table's name to answer `'t'::regclass`. Each catalog version now keeps
  the definitions in RecordId order with a sorted index of their names
  beside them, patched from the documents a commit changed: a changed
  definition is placed by its RecordId, where a full read finds it.
- The partitioned tables, the partitions, the temporary tables,
  `cr_relation_in`'s table and the composite index by oid are found
  through that index, or patched from what a commit changed, instead of
  by a walk over every table or composite.
- The tables a session created listed in the order of their names, and
  after a restart in the order they were created; both now list in the
  order the rows were written, a rewritten definition (`ALTER TABLE`)
  after the others, as on PostgreSQL 15.20.
- Fixes RP-1073 (`tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_3200_tables_within_a_tenth`)
  and RP-1113 (`tests/backlog/test_rp_catalog_order.py::test_relations_list_in_the_order_they_were_written_before_and_after_a_restart`).
  Files RP-1112: each `CREATE TABLE` inside a transaction block costs more
  than the one before it.
