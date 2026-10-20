### Rust MongoDB server: `local.oplog.rs` DDL and delete entries in mongod's shape

#### Fixed

- `local.oplog.rs` shows several entries as mongod 8.2.11 writes them:
  - a delete has no `o2`;
  - a drop carries `o2: {numRecords}`, the documents it removed;
  - a rename says `stayTemp: false`;
  - a one-index `createIndexes` is that index's spec, `v`, `unique` and
    `hidden` first;
  - `dropIndexes` keeps the index spec in `o2`.

  The stored entries are unchanged, so change streams and point-in-time
  restores read them as before. Updates, batched inserts and index builds
  over documents still differ (RM-224).

RM-224 part (`tests/backlog/test_rm_219_apply_ops.py::test_oplog_ddl_and_delete_entries_have_mongods_shape`).
