### Fix `drop` (and other ops) on a never-written collection under lazy shards

Lazy shard creation (0.6.0b2) makes a collection's documents shard exist only
once something is written to it, and the Rust server's `drop_collection` ran
`purge_collection_tables` unconditionally — which opened the collection's shard
cursor and failed with a WiredTiger "No such file or directory" error when the
collection had never been written (dropping an empty / never-created collection,
a no-op in MongoDB). The purge now treats an absent shard as "no rows to remove".
The gap escaped the test suite because tests always create a collection before
dropping it; it was caught by the standalone binary's PGO-profile workload, whose
first operation is `coll.drop()` on a fresh collection. A regression test now
exercises every operation (drop / find / count / distinct / delete / update /
aggregate / findAndModify / create-index-then-drop) against never-written
collections on the Rust server.

#### Fixed

- Rust server: `drop` on a collection whose shard was never written no longer
  errors with a WiredTiger `ENOENT`; `purge_collection_tables` tolerates an
  absent documents shard (matching the other lazy-shard read/scan/merge paths).
