### The statement after DDL reuses the catalog entries DDL left unchanged

After any DDL, the first statement on each connection rebuilds the catalog
state its planner reads, and in the Rust PostgreSQL server that rebuild
grew with the number of tables. Every table leaves a row type in the
composite catalog, and each catalog version decoded all of them from
storage again, re-sorted them, and copied every table's definition into the
version's list, including one copy per index relation. psql's `\d`
collation query straight after a `CREATE TABLE` took about 0.010 s in a
debug build with no other table and 0.021 s with 800.

A catalog version now shares each decoded document and table definition
with the version before it: an entry whose stored bytes did not change is
the same entry, and only what the statement wrote is decoded anew, the way
PostgreSQL rebuilds only the cache entries an invalidation names. The
version's `_id` order is carried forward from the previous one, with the
new entries placed into it, and the composite listings are built once per
version rather than once per caller. The same query now takes about
0.018 s with 800 tables. It still grows with the schema, because each
listing (the oids, the row types' fields, the index definitions, the
`tableoid` texts) is derived for every relation again per version, and
RP-911 stays open for that remainder.

#### Changed

- `secantus-pg`: the type catalog's documents are held as
  `Arc<Vec<Arc<Document>>>` (`CatalogDocs`) and the table definitions as
  `Arc<Vec<Arc<TableDef>>>` (`TableDefs`); `decode_type_catalog` reuses a
  document whose stored bytes match the previous read's and keeps the
  previous read's sorted order for the documents the two share.
  `IndexRelation::table` shares its `TableDef`. `composites` and
  `composites_with_schema` are kept per thread for the documents they were
  built from.
- RP-911 in part
  (crates/secantus-pg/src/lib.rs::type_catalog_reuse_tests::a_changed_read_is_sorted_and_shares_what_did_not_change);
  tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables
  stays open.
