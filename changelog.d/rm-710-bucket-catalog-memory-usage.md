### The time-series bucket catalog reports the memory mongod charges

`serverStatus().bucketCatalog.memoryUsage` was always 0 on `secantusd-mdb`.
It is now the number of bytes `mongod` 8.2 itself reports: the sum of what its
bucket catalog allocates through tracking allocators, worked out structure by
structure from the 8.2.11 source instead of fitted to a measurement. The
catalog's 32 stripes, each bucket, its field-name sets, its min/max and schema
summaries, the column builders and what they keep between writes, the maps
that find them and the state registry are each charged the bytes the exemplar
(Linux x86-64: libstdc++ and Abseil's layouts) allocates for them, on every
host. A write that adds a field, a longer name or string, a document or array
value, a longer meta value, a second series or a second collection moves the
number as it moves `mongod`'s.

#### Added

- `crates/secantus-commands/src/bucket_memory.rs`: the account. `Table` is
  Abseil's `raw_hash_set` (control bytes, slots, the inline element of a
  table whose slot is no wider than 16 bytes, capacities 1, 3, 7, 15, ...);
  `FlatStore` is `FlatBSON` (the `MinMax` and `Schema` entries, the field
  names, the value buffers, the fast-lookup map) with its update logic
  ported; `metadata_bytes` replays the `BSONObjBuilder`'s buffer growth;
  `MemState` holds the stripes' maps, the registry and the statistics.
- `ColumnBuilder::memory_shape` (`secantus-core`): what a builder keeps after
  `intermediate()` (the pending values, `_prev`, the buffer from the open
  control byte), also in interleaved mode.
- A column of document or array values whose reference object gained a field
  holds `mergeObj`'s buffer (a `BSONObjBuilder`'s 512 bytes), not the object's
  own size, as the Linux mongod measures it (`ColumnShape::Interleaved`).
- `tools/probes/linux_bucket_catalog_isolate.py` and
  `tools/probes/local_bucket_catalog.py`: the Linux cases, and the same
  cases against `secantusd-mdb`, case for case.

#### Fixed

- `bucketCatalog.memoryUsage` is the exemplar's byte count. Fixes RM-710
  (`tests/backlog/test_rm_bucket_catalog.py::test_bucket_catalog_memory_usage_is_the_bytes_mongod_accounts`,
  with `test_bucket_catalog_memory_usage_in_the_exemplar_bytes` and
  `test_bucket_catalog_memory_usage_follows_each_write_in_the_exemplar_bytes`,
  36 cases measured on Linux mongod 8.2.11 and 8.2.12).
