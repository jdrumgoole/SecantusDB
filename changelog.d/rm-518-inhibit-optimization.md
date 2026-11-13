### Rust MongoDB server: `$_internalInhibitOptimization` runs, as on mongod

mongod 8.2 runs `$_internalInhibitOptimization`, a stage that passes each
document on and that the optimizer moves no stage across and lets no stage
behind into the query. The Rust MongoDB server refused it as an unknown
stage (40324). It now runs it.

#### Fixed

- A pipeline with the stage answers as one without it. A `$match` ahead of
  it is still the query's; the stages behind it run after the read, and
  `explain` lists the stage where it was written, with its `nReturned` at
  `executionStats`. It runs in a `$facet`, a `$lookup` or `$unionWith`
  sub-pipeline, a view and a change stream.
- Its argument is checked as mongod checks it: anything but an object is
  refused with 14 ("must take a nested object"), an object with a field
  with 9 ("must take an empty object"). An update pipeline may not run it
  (72).
- mongod's dependency analysis stops at the stage, so a sort key read only
  behind it is never asked for: neither the read nor any `$sort` writes one,
  `{$meta: "sortKey"}` there is missing, and a rank is refused (9973200). A
  reader ahead of the stage still asks for the key, and a `$geoNear`'s key
  goes through. Measured against mongod 8.2.11.
- Fixes RM-518 (tests/backlog/test_rm_found_rm408.py::test_internal_inhibit_optimization_runs).
  Regression tests: `tests/mongod_live/test_inhibit_optimization.py`
  against mongod, and `agg_meta`'s
  `a_sort_key_read_only_behind_an_inhibit_is_never_asked_for`.
