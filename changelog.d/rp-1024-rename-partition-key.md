### A renamed partition key column keeps its table writable

Renaming the column a partitioned table is keyed on used to leave the stored key naming the old column, so every later insert into the table failed with `column "x" does not exist`. The key now follows the rename, as PostgreSQL's attnum-based key does: rows keep routing, sub-partitions keyed on the same column follow too, an expression key is renamed inside its expression, and `pg_get_partkeydef` prints the new name.

#### Fixed

- Rust PG server: `ALTER TABLE ... RENAME COLUMN` renames the column in the partition key of the table and of its sub-partitions.
  Fixes RP-1024 (tests/backlog/test_rp_found_rp860.py::test_a_renamed_partition_key_column_still_routes)
