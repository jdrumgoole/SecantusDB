### The oplog answers `$collStats` and `collStats` like any capped collection

The Rust MongoDB server's oplog has no catalog entry of its own: it is a view
of the oplog tables. The statistics paths only looked in the catalog and the
document tables. So `{$collStats: {storageStats: {}}}` on `local.oplog.rs`
failed with NamespaceNotFound, and so did the `local.oplog.rs.stats` collector
in each diagnostic-data sample. `collStats` and `dataSize` gave the oplog a
size of 0 and no `capped`. Following mongod 8.2.11, all of these now answer
for the oplog as they do for any capped collection: the entries are counted
and sized from the oplog itself, and the reply carries `capped`, `max` and
`maxSize`.

#### Fixed

- `secantus-storage`: `collection_sizes` and `collection_data_size` read
  `local.oplog.rs`'s size from its entries in one pass. Before, they summed
  the document tables, where the oplog's entries are not kept. The PostgreSQL
  server links this crate. Its storage of tables is unchanged.
- `secantus-commands`: `collStats`, the `$collStats` stage and the FTDC
  `$collStats` collectors treat the oplog the storage serves as present.
- Fixes RM-419 (tests/backlog/test_rm_219_diag_query_settings.py::test_oplog_storage_stats)
