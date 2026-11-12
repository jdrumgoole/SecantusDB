### The statement after DDL re-reads only the index list that changed

After any DDL, the Rust PostgreSQL server's next statement on each
connection republishes the catalog state its planner reads. It read every
table's index list from storage again for that, because the storage's
index-list cache was dropped whole by any change of any collection. It also
copied every table definition several times to build its listings. psql's
`\d` collation query straight after a `CREATE TABLE` took about 0.010 s in a
debug build with no other table and 0.036 s with 800.

The storage now keeps each collection's index list until a change of that
collection, as PostgreSQL's relation cache keeps a relation's index list
until an invalidation names that relation. A change that names no single
collection, such as a rename, still drops every list. The republication
reads the shared table definitions in place, with one oid map. The same query
now takes about 0.027 s with 800 tables. It still grows with the schema,
because each listing derives every relation's entry again per catalog
version; RP-911 stays open for that remainder.

#### Changed

- `secantus-storage`: `list_indexes_shared` keeps an entry until a change
  of its own collection, or one naming none, is numbered after the read
  (`note_planner_write_of`, `note_coll_table_write(db, coll)`); the index
  catalog writes that name their collection invalidate only it. The
  MongoDB server reads the same cache through `secantus-storage-adapter`.
- `secantus-pg`: `publish_user_types` and `relations` read
  `shared_table_defs` rather than copying every definition, and look oids
  up in one `relation_oids` map.
- RP-911 in part
  (crates/secantus-storage/tests/index_list_cache.rs::an_index_list_is_read_again_only_after_a_change_of_its_collection);
  tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables
  stays open.
