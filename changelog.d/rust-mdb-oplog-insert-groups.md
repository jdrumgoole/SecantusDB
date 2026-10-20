### Rust MongoDB server: batched inserts in `local.oplog.rs` are mongod's `applyOps` groups

#### Fixed

- `local.oplog.rs` shows a batch of inserts as mongod 8.2.11 logs it: one
  `applyOps` entry per group of up to 500 documents or 256 KiB, one
  document as a plain insert, and a batch with a failed document as
  single inserts. Every entry now has mongod's `t` and `v`.
- Change streams and point-in-time restores read the stored entries,
  which keep one entry per document.

RM-224 part (`tests/backlog/test_rm_219_apply_ops.py::test_oplog_insert_batches_are_mongods_apply_ops`).

A committed transaction is one `applyOps` entry under its `lsid` /
`txnNumber`, with `prevOpTime`, as mongod logs it
(`::test_oplog_transaction_is_one_apply_ops`).
