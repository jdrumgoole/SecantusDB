### An update or delete tests its filter once per document

mongod tests an update's or a delete's filter once against each document it
reaches, so writing one document takes as long as finding it. The Rust
MongoDB server tested it twice: once in the read that picks the write's
documents in mongod's order, and again in the storage write that re-reads
them. A write whose filter is costly took twice as long as mongod's, and ran
out of a `maxTimeMS` that mongod's fits in.

The read now hands the write each row it matched with the bytes it matched
it as, and the write tests only a row whose bytes have changed since --
another writer's change is still caught. An update or delete of one
document, `findAndModify` (update, pipeline and remove), an update under a
`sort`, and writes inside a transaction all test once, as on mongod 8.2.11.

#### Fixed

- `update` (one or many), `delete` of one, and `findAndModify` test a
  document's filter once, not twice (`with_matched_rows`, in the
  `Storage` trait and `secantus-storage`, which the PostgreSQL server also
  links). Fixes RM-391
  (`tests/backlog/test_rm_found_2026_11_07d.py::test_a_write_tests_each_document_once`);
  storage regression tests in `crates/secantus-storage/tests/matched_rows.rs`.
- Filed RM-501: mongod's batched deleteMany tests each document twice,
  once when it stages it and again when it deletes the batch, where this
  server tests it once
  (`tests/backlog/test_rm_found_2026_11_10_rm391.py::test_a_batched_delete_many_tests_each_document_twice`).
