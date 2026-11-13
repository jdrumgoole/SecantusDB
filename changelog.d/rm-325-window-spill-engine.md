### Rust MongoDB server: a `$setWindowFields` spills to a spill WiredTiger, as mongod 8.2's

mongod 8.2 keeps a second WiredTiger instance for spilling, at
`<dbpath>/_tmp/spilldb`, and a `$setWindowFields` that runs over its memory
budget writes its partition cache to a table there. The table is reported
as `spilledDataStorageSize` by its file's size, which stays at the 4096
bytes WiredTiger creates it with while the engine holds it in cache. This
server wrote a file of its own and reported that file's size (310800 bytes
where mongod reports 4096).

#### Fixed

- `secantusd-rs` now opens a spill engine at `<dbpath>/_tmp/spilldb` the
  first time a stage spills, with mongod 8.2's configuration: no log, a
  cache of 2.5% of the machine's memory held between 100 and 1000 MB, one
  eviction thread, a 5% dirty target and 80% dirty and updates triggers.
  Opening it drops any table an earlier run left, as mongod does at
  startup. A store with no files (`in_memory=true`) keeps the engine under
  the system's temporary directory, as its index builds do, and removes
  it on shutdown.
- A `$setWindowFields` spill writes each document as a record keyed by
  record id into a table of that engine. The stage reports the table's
  size on disk as WiredTiger's block manager counts it, and drops the
  table when it finishes. `serverStatus`'s
  `metrics.query.setWindowFields.spilledDataStorageSize`, the profiler and
  `explain` now agree with mongod 8.2.11 for every spill the engine holds
  in cache (300 to 3000 documents, 300 KB to 14 MB, measured 2026-11-13).
- The engine crate reaches the spill engine through a hook the server sets
  for each operation (`secantus_core::spill_table`). Run on its own, with
  no server, it keeps writing a file.
- Shared storage crates: `secantus-wt` gains `Session::drop_table`, and
  `secantus-storage` gains the `spill` module. The PostgreSQL server links
  both but never opens a spill engine.
- Fixes RM-325 (tests/backlog/test_rm_found_2026_10_26.py::test_spill_storage_size).
  `tests/mongod_live/test_explain_memory_counters.py` no longer leaves out
  a window's `spilledDataStorageSize`.

#### Known gaps

- Filed while working RM-325, each with a gap test in
  `tests/backlog/test_rm_found_2026_11_13_rm325.py`:
  - RM-611: past about 14 MB mongod's engine writes zstd-compressed pages
    out, the same every run, and this server's uncompressed table does not
    match it.
  - RM-612: the SBE `$group`'s startup-only budget parameter is refused.
  - RM-613: a `$median` spills its values, not mongod's t-digest.
  - RM-614: `serverStatus` has no `spillWiredTiger` section.
