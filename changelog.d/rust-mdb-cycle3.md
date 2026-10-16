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

Fixes RM-055 (tests/backlog/test_rm_029_056.py::test_multikey_paths_survive_a_restart),
RM-026 (tests/backlog/test_rm_001_028.py::test_dbhash_includes_timeseries_buckets),
RM-128 (tests/backlog/test_rm_113_138.py::test_std_dev_pop_last_ulp).
