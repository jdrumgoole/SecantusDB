### A `$group` read by a DISTINCT_SCAN hashes as mongod's

`explain` of a pipeline whose `$group` the Rust MongoDB server answers from a
DISTINCT_SCAN now gives mongod 8.2.11's `queryHash`, `planCacheShapeHash` and
`planCacheKey`.

#### Fixed

- The plan cache shape of such a read now carries what mongod's does: the
  pipeline's sort, the fields the `$group` reads as the projection, and the
  distinct key with its direction (`1` for a `$group` of `$last`s), for
  example `gtx||x||x0fs` (`76A99463` / `09B22F21`) rather than the bare
  query's `gtx||||fs`.

Fixes RM-697 (tests/backlog/test_rm_found_2026_11_15_rm426.py::test_a_distinct_scan_group_hashes_as_mongod_does)
