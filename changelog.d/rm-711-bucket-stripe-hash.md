### Time-series buckets spread over the catalog's stripes as evenly as mongod's

The Rust MongoDB server picked the stripe a time-series bucket falls in with
an FNV-1a hash whose low bits follow the series' bytes one for one, so
series that differ in a single byte (the integer metadata values 0 to 19)
collided in a stripe about half as often as in `mongod`, whose Abseil hash is
seeded at random for each process and uniform. Idle-bucket expiry
(`timeseriesIdleBucketExpiryMemoryUsageThreshold`) only acts on the stripe a
new bucket opens in, so about 5% of runs closed no bucket where `mongod`
closes at least one in all but about 1 in 2,000.

#### Fixed

- The catalog's stripe hash is a randomly seeded SipHash for each process,
  as `mongod`'s is. Measured on this server with the 21-bucket probe of
  `tests/backlog/test_rm_bucket_catalog.py::test_idle_buckets_expire_past_the_memory_threshold`:
  before, 11 of 150 runs closed no bucket (mean 2.5 closed); after, none of
  150 (mean 4.8, least 2); `mongod` 8.2.11 on Linux x86-64 closed a mean of
  5.1 in 92 runs, none fewer than 2. It failed again in CI run 46042159284
  (job 140652343402, PR #3087, which changed only PostgreSQL crates), so it
  was never RM-636. Found by CI run 46029656831 (job 140609953899, PR #3085), where the
  test failed on Linux x86-64 with `expired: False`. Fixes the flake of
  RM-711 (`tests/backlog/test_rm_bucket_catalog.py::test_idle_buckets_expire_past_the_memory_threshold`).
