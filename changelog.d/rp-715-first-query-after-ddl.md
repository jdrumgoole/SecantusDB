### The first catalog query after DDL grows far less with the schema

After a DDL statement, the Rust PostgreSQL server's next catalog query still
grew with every table in the database. psql's `\d` collation query took about
0.010 s in a debug build with no other table, 0.049 s with 400 and 0.092 s
with 800, where PostgreSQL answers in well under a millisecond however many
tables there are. Four costs grew with the schema. A read pinned to one
type's `pg_type` row built every type first. The model of PostgreSQL's catalog
reads counted `attrelid = $1 AND attnum > 0` as a sequential scan of
`pg_attribute` and built every row to count them. Each catalog version decoded
every table definition again. Several listings copied every definition, and
read each table's index list and relation oid one table at a time.

Each of those is gone. A read of a built-in type builds only the built-in
rows. The read model treats each arm of an AND as a restriction, as
PostgreSQL's planner does, so the `\d` column read is one index scan of
`pg_attribute_relid_attnum_index` with the other quals as its filter. That is
also the count PostgreSQL 15.19 reports. A `'t'::regclass` constant keys that
scan by the relation's oid. A table definition whose stored bytes have not
changed is reused from the previous version instead of decoded. The listings
read the shared definitions and one oid map. The same query now takes about
0.011 s with no other table, 0.023 s with 400 and 0.036 s with 800. It still
grows, because the first statement after DDL republishes the session's
catalog state from every table; RP-911 tracks that remainder.

#### Fixed

- `secantus-pg`: the `pg_type` arm of `build_catalog_rows` skips every user
  type, view and namespace lookup for a key below 16384, where no user type's
  oid falls.
- `secantus-pg`: `catalog_reads.rs` visits an AND's arms with the caller's
  restriction context, so an equality beside other quals is an index scan's
  key; a `'name'::regclass` constant is resolved to its oid silently
  (`regclass_const`), since the cast already counted its own lookups.
- `secantus-pg`: `committed_table_defs` keeps each definition's stored bytes
  and reuses the previous version's decoded definition when they match.
- `secantus-pg`: `try_index_relations`, `namespaces`,
  `tableoid_expressions`, the `pg_attribute` arm and the other read-only
  listings use `shared_table_defs` rather than copying every definition, and
  `relation_oids` hands a caller one oid map instead of a lookup per table.
  `index_defs` does not build `pg_indexes` when no user index exists.
- Fixes RP-715
  (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_does_not_grow_with_the_schema).
- RP-909 in part: an equality with other quals beside it counts one index
  scan and no sequential scan
  (tests/test_rust_pgserver_slice.py::test_an_equality_with_other_quals_is_one_index_scan).
  RP-909 stays open for the extra index scans a fresh backend's first read
  counts, which a bare equality shares.
- Filed RP-911: the first statement after DDL rebuilds per-version session
  state from every table
  (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables).
