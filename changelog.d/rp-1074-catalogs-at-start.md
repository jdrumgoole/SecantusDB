### Rust PG server: the first `CREATE TABLE` of a store adds only its own table

PostgreSQL's `initdb` makes every catalog, so a `CREATE TABLE` creates
only its own relation's file and its primary key's. `secantusd-pg` made
the saved heap and B-tree page maps and the cluster's oid counter the
first time a table needed them, so the first `CREATE TABLE` of a store
added four collections and four index tables. They are now made at server
start with the other catalog collections.

#### Fixed

- The first `CREATE TABLE` of a store adds one `collection-<UUID>` table
  and one `index-<UUID>` table, as mongod's store holds a collection.
  Partly fixes RP-1074 (tests/backlog/test_rp_storage_model.py::test_a_table_is_its_own_collection_table);
  the `__sys_*` fields mongod still sees in each document are open, with
  a decision recorded on the item.
