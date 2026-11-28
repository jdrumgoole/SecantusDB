### `pg_relation_filepath` names a relation the open transaction created

The Rust PostgreSQL server now answers `pg_relation_filepath` for a table,
index, primary key or sequence created earlier in the same transaction
block, as PostgreSQL does: the transaction that creates a relation sees its
own catalog rows. Before, the function answered NULL for such a relation
until the CREATE committed. The file it names inside the block is the one
the relation keeps after COMMIT.

#### Fixed

- `secantus-pg`: `pg_relation_filepath` reads through the open block's
  transaction (`read_in_block`), as the size functions already did.
- `secantus-storage`: `collection_table_name` and `index_table_name` look the
  table up through the bound user transaction's session (`op_session`)
  instead of a fresh session that could not see the transaction's
  `_mdb_catalog` rows. Only the PostgreSQL server calls them; the MongoDB
  server links the crate but does not call either.

Fixes RP-1135 (tests/backlog/test_rp_storage_model.py::test_filepath_of_a_relation_the_open_transaction_created;
regression test tests/test_rust_pgserver_slice.py::test_relation_filepath_sees_what_the_open_block_created)
