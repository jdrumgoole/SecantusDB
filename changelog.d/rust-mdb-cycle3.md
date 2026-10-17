### Rust MongoDB server: cycle 3 of the backlog

Differences from mongod 8.2.11, each pinned by a test that compares the
two.

#### Fixed

- **`$stdDevPop` / `$stdDevSamp` match mongod to the last digit** in
  `$group` and in the expression form. mongod runs Welford's online
  algorithm with the `m2` step a fused multiply-add; this server computed
  the mean, then the squares, and differed in the final ULP on about one
  value set in five. Checked on 800 random sets. (Over a sliding
  `$setWindowFields` window the last digit still differs: RM-181.)

- **`dbHash` hashes a time-series collection's buckets** under
  `system.buckets.<name>`, as mongod does; it left the collection out.

- **An index's multikey paths are kept in its catalog and never shrink**,
  as mongod keeps them. They were derived from the documents, so after a
  restart (or with the documents at hand) an index whose arrays were gone
  reported `isMultiKey: false`. A store written before this records none
  until a document holding an array is written again.

- **`validate` with `background: true` reads the last checkpoint**, as
  mongod does: a collection no checkpoint holds yet is 26
  NamespaceNotFound, and one written since is validated as it was.

- **`showRecordId`, `{$meta: "recordId"}` and `{$meta: "indexKey"}` give
  mongod's values.** Records are numbered per collection from 1, as on
  mongod, where they were one counter for the whole store, and the index
  key is the one the plan's index scan read. A store written before this
  keeps its old record ids on its existing documents.

- **An `aggregate` cursor is killed when the index it reads is dropped**,
  as a `find` cursor already was.

- The backlog's gap-test fixtures read the server's output after its
  banner. They left the pipe unread, and once 64 KiB of log filled it,
  every command blocked on the write: a run of a whole gap-test file
  hung for 20 seconds, then failed a test that passes on its own.

Fixes RM-055 (tests/backlog/test_rm_029_056.py::test_multikey_paths_survive_a_restart),
RM-026 (tests/backlog/test_rm_001_028.py::test_dbhash_includes_timeseries_buckets),
RM-128 (tests/backlog/test_rm_113_138.py::test_std_dev_pop_last_ulp),
RM-033 (tests/backlog/test_rm_029_056.py::test_background_validate_of_an_uncheckpointed_collection),
RM-082 (tests/backlog/test_rm_057_084.py::test_aggregate_cursor_after_its_index_is_dropped).
RM-083 in part (tests/backlog/test_rm_057_084.py::test_meta_record_id_and_index_key_projections,
tests/backlog/test_rm_found_2026_10_16.py::test_show_record_id_is_the_record_id).
