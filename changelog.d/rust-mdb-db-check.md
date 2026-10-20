### Rust MongoDB server: `dbCheck`

#### Added

- `dbCheck` runs as a one-member replica set's primary runs it. It used to
  be 59 CommandNotFound.
  - The collection is read in `_id` order, in batches of `maxDocsPerBatch`
    and up to `maxCount`. Each batch writes an oplog command entry
    carrying the MD5 of its documents, which matches mongod 8.2.11's
    exactly. Start and stop entries surround the batches.
  - `dbCheckStart` / `dbCheckStop` are recorded in
    `local.system.healthlog`, which is created capped as mongod creates
    it.
  - `dbCheck: 1` checks every collection of the database, `system.views`
    included.
  - Errors are as on mongod: a missing collection is 26, a view 166, an
    unknown option 40415, a bad `validateMode` 2, and
    `extraIndexKeysCheck` without `secondaryIndex` 72.

RM-219 part (`tests/backlog/test_rm_219_db_check.py`).
