### Rust MongoDB server: a time-series collection's DDL line names its buckets

A `Slow query` line and a profile entry for a time-series collection's
`collMod`, `collStats`, `createIndexes` or `dropIndexes` now name the
buckets collection it runs on, as mongod 8.2.11's do.

#### Fixed

- A `collMod`, `collStats`, `createIndexes` or `dropIndexes` of a
  time-series collection `ts` had a line and profile entry naming
  `<db>.ts` with `collectionType: "normal"`. mongod runs each one on the
  collection's buckets, so its line and entry name
  `<db>.system.buckets.ts`, `"timeseriesBuckets"`, whether the command
  succeeds or fails (a negative `expireAfterSeconds`, an index `collMod`
  cannot find, a `dropIndexes` of a missing index). An index build's own
  line says the same. Reads, writes, `listIndexes`, `drop` and a
  `$collStats` stage still name `<db>.ts`, as mongod's do.
- A line for any `system.buckets.` namespace said `"system"`; mongod's says
  `"timeseriesBuckets"`, whether or not a time-series collection is behind
  it (a `collStats` of `system.buckets.ts`, or of a missing
  `system.buckets.nope`, and a `find` of or an `insert` into one).
- Fixes RM-484 (`tests/backlog/test_rm_view_namespaces.py::test_a_time_series_ddl_line_names_its_buckets`,
  `tests/mongod_live/test_view_line_namespaces.py::test_a_line_names_what_mongods_does`).
- Filed, each with a gap test in `tests/backlog/test_rm_ts_ddl_lines.py`:
  RM-582, a `createIndexes` that builds nothing names its collection where
  mongod's names `<db>.$cmd`; RM-583, an index build of a time-series
  collection writes no build line; RM-584, a `collMod` of a missing
  collection names it and is profiled; RM-585, an `insert` into a
  time-series collection's buckets is not profiled; RM-586, a `find` of a
  time-series collection's buckets does not say what it examined.
