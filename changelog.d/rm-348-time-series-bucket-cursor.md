### A time-series aggregate reads on bucket by bucket

mongod stores a time-series collection as bucket documents and reads them
through its aggregation cursor stage: 32 buckets first, twice as many at
each refill, unpacking each into its measurements only as the cursor
reaches it. What a `getMore` returns after a write therefore depends on how
many buckets the cursor had buffered. The Rust MongoDB server read the
whole collection when the `aggregate` ran, so a cursor opened before a
delete returned every measurement it had matched.

It now reads a time-series `aggregate` the way mongod does. Each refill
works the buckets out from the collection as it is at that moment and
carries on after the last bucket it buffered. The bucket-level filter
mongod derives from the query decides which buckets are buffered. Over
4,000 measurements in 100 buckets, read one at a time with everything
deleted after the first, mongod 8.2.11 and this server both return 2,303
more.

#### Fixed

- `secantus-commands`: an `aggregate` over a time-series collection whose
  stages need the whole measurement (`$match`, `$addFields` / `$set`,
  `$unset`, an exclusion `$project`, `$unwind`, `$redact`, `$skip`,
  `$limit`) streams through the cursor stage over its derived buckets
  (`ts_buckets::BucketScan`). A refill resumes after the last bucket by its
  window's start and the RecordId of the measurement that opened it, and
  applies the bucket-level filter (`timeseries::bucket_filter`) with
  mongod's `$_internalExpr*` comparison rules. A read under a hint or a
  collation, one filtering on the meta field, and one over a collection
  with an index of its own are still read whole.
- Fixes RM-348
  (tests/backlog/test_rm_found_2026_10_27b.py::test_a_time_series_read_goes_bucket_by_bucket),
  also pinned by tests/mongod_live/test_time_series_cursors.py.
- Filed RM-455 (a time-series `find`), RM-456 (behind an inclusion
  `$project`) and RM-457 (read from an index over the buckets), with gap
  tests in tests/backlog/test_rm_found_2026_11_09_rm348.py.
