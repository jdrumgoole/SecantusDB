### An index being built counts in the stats commands

While an index build runs, mongod already counts the index: `collStats`
and `$collStats` include it in `nindexes`, `indexSizes` and
`totalIndexSize` and name it in `indexBuilds`, `dbStats` counts it in
`indexes`, and `$indexStats` lists it with `building: true`. The Rust
MongoDB server listed a building index in `listIndexes` but left it out of
every stats answer until the build committed, so a monitor or test that
waited on `nindexes` or `$indexStats` saw a different collection from the
one mongod reports. It now answers as mongod 8.2.11 does, and the index's
`$indexStats` use count runs from when its build began rather than
restarting at the commit.

#### Fixed

- `secantus-commands` (`collStats`, `$collStats`, `dbStats`, `$indexStats`):
  the indexes of a build in progress are counted after the finished ones,
  `indexBuilds` names them, and `$indexStats` marks them `building: true`.
- `secantus-storage-adapter`: an index's `accesses.since` is set when its
  build begins and kept through the commit, as on mongod.
- Fixes RM-321 (tests/backlog/test_rm_index_build_gaps.py::test_a_building_index_counts_in_stats);
  pinned against mongod by
  tests/mongod_live/test_index_builds.py::test_a_build_in_progress_in_stats.
