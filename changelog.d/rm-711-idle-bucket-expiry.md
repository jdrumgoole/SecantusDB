### The time-series bucket catalog expires idle buckets past its memory threshold

When the bucket catalog's memory passes `timeseriesIdleBucketExpiryMemoryUsageThreshold`,
`secantusd-mdb` now expires idle buckets as `mongod` 8.2 does: a bucket opened for a new
series first archives the oldest idle buckets of its stripe, then closes archived ones,
until the catalog is back under the threshold or at most
`timeseriesIdleBucketExpiryMaxCountPerAttempt` + 1 buckets have expired. The counters
`numBucketsArchivedDueToMemoryThreshold` and `numBucketsClosedDueToMemoryThreshold` count them,
and every bucket document stays stored. The decision rests on the memory account
(`bucketCatalog.memoryUsage`, RM-710), so the threshold is passed when `mongod`'s is.

#### Added

- `timeseriesIdleBucketExpiryMemoryUsageThreshold`: settable at runtime and at startup, a long
  (a percentage of the machine's memory up to 100, bytes above; zero or negative is the default,
  5); `getParameter` reports it and `setParameter` coerces and refuses as `mongod` does.
- `timeseriesIdleBucketExpiryMaxCountPerAttempt`: startup only, an int of at least 2 (default 3).

#### Fixed

- Idle buckets are expired past the threshold. Fixes RM-711
  (`tests/backlog/test_rm_bucket_catalog.py::test_idle_buckets_expire_past_the_memory_threshold`,
  with `test_idle_bucket_expiry_parameters_answer_as_mongod_does`).
