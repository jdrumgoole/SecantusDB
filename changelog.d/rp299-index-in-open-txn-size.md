### Rust PG server: an index made in the open transaction has a size

`pg_relation_size('t_b')` right after `CREATE INDEX t_b` in the same
transaction block was 42P01 `relation "t_b" does not exist`, though the
index was already in `pg_class` and `'t_b'::regclass` found it. The size
functions run while the statement is planned, when the block's transaction
is not yet bound, so they read the committed catalog only. They now read
through the open block's transaction, as `pg_get_indexdef` already does
(RP-200), and answer 8192 for an empty index, as PostgreSQL 15.19 does.

#### Fixed

- `pg_relation_size`, `pg_table_size`, `pg_indexes_size` and
  `pg_total_relation_size` see the indexes the open transaction block
  created. Fixes RP-299
  (tests/backlog/test_rust_postgres_gaps.py::test_an_index_made_in_the_open_transaction_is_a_relation)
