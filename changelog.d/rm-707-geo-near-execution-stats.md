### Rust MongoDB server: a `$near` explain reports mongod's GEO_NEAR executionStats

`explain` with `executionStats` of a `$near` read now reports what mongod
8.2.11's GEO_NEAR_2D / GEO_NEAR_2DSPHERE stage reports: its work counts,
`searchIntervals` (each ring's distances, `nBuffered` and `nReturned`) and
`inputStages` (each ring's FETCH over its IXSCAN, with the ring's index
bounds, keys examined and seeks), and `totalKeysExamined` /
`totalDocsExamined` from the rings' scans.

#### Fixed

- The stage printed none of these and counted 0 keys, walking the
  documents in storage order. The explain now runs mongod's ring search
  (`geoorder::near`), each ring's index scan worked as mongod's `IndexScan`
  works it, one key a work, the bounds checked and sought past as
  `IndexBoundsChecker` does (`geoorder::scan`). A `$near` cursor's
  `keysExamined` counts the same way.
- Fixes RM-707 (tests/backlog/test_rm_found_rm578.py::test_a_geo_near_stage_reports_mongods_execution_stats)

Found along the way and filed: RM-712 (a MultiPoint 2dsphere index is not
multikey), RM-713 (a `$near` with a compound 2d index's trailing field is
not GEO_NEAR_2D) and RM-714 (a LIMIT over GEO_NEAR has no work counts).
