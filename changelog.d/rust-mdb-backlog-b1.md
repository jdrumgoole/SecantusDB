### Rust MongoDB server: update parse errors without a match, SCRAM's first step, `config` and `dbStats`, `$bucketAuto` and `bulkWrite`

A run of differences from mongod 8.2.11 on the Rust MongoDB server, each
measured against a live mongod and pinned by a test that compares the two.

#### Fixed

- **An update's parse errors no longer need a matching document.** mongod
  checks an update's operands before it reads any document. That covers
  `$inc` or `$mul` of a non-number, `$rename` to a non-string or from a
  dynamic path, a malformed `$bit`, `$pop: 2`, `$addToSet`'s `$each` of a
  non-array and `$currentDate`'s options. It reports them when nothing
  matches and when the collection does not exist. The Rust server reported
  them only once a document reached the engine, so `update` answered
  `n: 0` and `findAndModify` answered `ok: 1`. Of 39 update shapes measured
  against a missing collection, an empty one and one whose document does
  not match, 31 differed; none do now.
- **`saslStart` fails at the first step for a user that does not exist**,
  with 18 `Authentication failed.`. It used to make up credentials and fail
  at the proof. A failed `saslStart` also ends any conversation in
  progress, so a `saslContinue` after it is mongod's 17 `No SASL session
  state found`.
- **`saslStart` refuses a client nonce shorter than four characters**,
  with 18, as mongod does.
- **`config` is always a database.** mongod creates
  `config.system.sessions` (with its `lsidTTLIndex`) at startup, which is
  why a fresh mongod lists `config`. The Rust server now does the same; it
  listed `config` only once something had written there. mongod's periodic
  refresh is not modelled, so a dropped `system.sessions` comes back at the
  next start.
- **`dbStats` reports `fsUsedSize` and `fsTotalSize`** from the filesystem
  holding the store (`statvfs`), scaled like the other sizes and 0 for a
  database that does not exist. Both were always 0. Windows still reports
  0.
- **`$bucketAuto` `granularity` refuses a non-numeric, NaN or negative
  value with mongod's 40258 / 40259 / 40260**, naming the first bad value
  in input order. These were BadValue (2). The same work fixed three
  bucket answers: a zero rounded down keeps its integer type, POWERSOF2
  over `[0, 1]` ends at 0.5, and equal values that round up to zero stay in
  one bucket.
- **`bulkWrite` honours `collectionUUID`.** A mismatch fails the operation
  with 361 `CollectionUUIDMismatch` and mongod's `db`, `collectionUUID`,
  `expectedCollection` and `actualCollection` fields, and writes nothing.
  A UUID of the wrong binary subtype is 14, and one of the wrong length
  207.

Fixes RM-065 (tests/backlog/test_rm_057_084.py::test_update_parse_errors_without_a_match),
RM-141 (tests/backlog/test_rm_found_2026_10_14.py::test_sasl_start_for_an_unknown_user_fails),
RM-174 (tests/backlog/test_rm_found_2026_10_16.py::test_sasl_start_refuses_a_client_nonce_under_four_characters),
RM-066 (tests/backlog/test_rm_057_084.py::test_list_databases_always_has_admin_and_config),
RM-068 (tests/backlog/test_rm_057_084.py::test_db_stats_reports_the_filesystem),
RM-002 (tests/backlog/test_rm_001_028.py::test_bucket_auto_granularity_value_errors)
and RM-105 (tests/backlog/test_rm_085_112.py::test_bulk_write_collection_uuid_mismatch).

RM-108 (`distinct` on a view answering in sorted order) is closed as a
scope decision: mongod's order changes from call to call (13 orders in 18
calls), so no order is mongod's.
