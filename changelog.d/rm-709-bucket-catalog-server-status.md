### `serverStatus` reports the time-series bucket catalog

`secantusd-mdb`'s `serverStatus` now carries mongod 8.2's `bucketCatalog`
section once a time-series collection has been written to: the bucket
counts, the execution counters in mongod's order and the state registry.

#### Fixed

- `serverStatus` had no `bucketCatalog` section. It is reported from the
  catalog's own state, with `memoryUsage` 0 until the catalog accounts its
  memory (RM-710). Two divergences found while probing are filed: the
  open/archived split after a time-backward reopening, and mongod's
  era-based clearing on drop.

Fixes RM-709 (tests/backlog/test_rm_bucket_catalog.py::test_bucket_catalog_counts_and_counters_follow_the_writes, tests/backlog/test_rm_found_2026_11_11_rm088.py::test_server_status_reports_the_bucket_catalog)
