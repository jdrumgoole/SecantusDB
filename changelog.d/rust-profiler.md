### The Rust MongoDB server has a database profiler

`profile` levels 1 and 2 now write each operation to a capped 1 MB
`system.profile` in mongod 8.2.11's shape: one `update` / `remove` entry
per statement that ran, `$merge` and `$out`, `responseLength`, `client`,
`user`. Which commands are profiled, and under which namespace, was
measured one command at a time.

#### Added

- A profile `filter` is accepted, validated against the 64 fields mongod
  takes, read back as mongod normalises it (`{op: "query"}` is
  `{op: {$eq: "query"}}`), and decides both profiling and the slow-query
  log line. Settings are held in memory, per database, and a restart or
  `dropDatabase` forgets them, as on mongod.
- `system.profile` is created when the level is set, has no `_id` index,
  is not replicated (no oplog entry, no change event), and cannot be
  dropped while profiling is on.

#### Fixed

- A negative `slowms` is accepted and counts every operation as slow.
- Fixes RM-038 (`tests/backlog/test_rm_029_056.py::test_profile_filter_is_stored`,
  `::test_profile_level_creates_system_profile`) and RM-140
  (`tests/backlog/test_rm_found_2026_10_14.py::test_profile_accepts_a_negative_slowms`).
  Pinned besides by `crates/secantus-storage-adapter/tests/profiler_wt.rs`.
