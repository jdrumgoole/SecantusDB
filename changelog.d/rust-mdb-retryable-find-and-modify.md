### Rust MongoDB server: a retryable `findAndModify` names its retry image

#### Fixed

- A retryable `findAndModify` now shows in `local.oplog.rs` with its
  session and `needsRetryImage`, as mongod 8.2.11 writes it: `postImage`
  when it returns the new document, `preImage` otherwise, and none for an
  upsert's insert.
- The image it returned is kept in `config.image_collection` (one
  document per session, without an oplog entry), as on mongod.

RM-224 part (`tests/backlog/test_rm_219_apply_ops.py::test_retryable_find_and_modify_names_its_image`).
