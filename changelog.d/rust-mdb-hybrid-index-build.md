### Rust MongoDB server: index builds let writes go on (RM-187, RM-224 part)

An index build over documents used to hold the storage's global lock and the
collection's lock from its first read to its catalog row: every write to the
collection, and every DDL, waited for the whole build. On a debug build over
300,000 documents the first write of another client waited 5.4 s of a 5.5 s
build. The build is now mongod 8.2's two-phase ("hybrid") build, and over
the same collection 518 writes finished while it ran, median 11.9 ms (mongod
8.2.11 on the same machine: median 10.1 ms).

#### Changed

- `createIndexes` over a collection with documents runs ONE build for the
  request's new indexes: it is set up under the collection's exclusive lock,
  recorded and logged, then scans with the collection unlocked. Every write
  to the collection meanwhile records what it did to the new indexes' keys in
  a side table (mongod's `IndexBuildInterceptor`), applied before the build
  commits. The commit takes the exclusive lock again, applies the last
  writes, keys again the documents that could not be keyed when written
  (mongod's skipped-record tracker), checks again every key two documents
  were seen to share (its duplicate-key tracker), and commits the catalog
  rows with `commitIndexBuild`. A duplicate written during a unique build
  fails the build at commit with mongod's E11000; one deleted before the
  commit does not.
- The keys go through an external sorter that keeps at most
  `maxIndexBuildMemoryUsageMegabytes` (200 by default, settable at runtime,
  at least 50, mongod's replies) in memory and spills sorted runs to
  `<dbpath>/_tmp`, so a build no longer holds all its entries at once.
- The oplog records a build as mongod 8.2.11 does: a `config.system.indexBuilds`
  row, `startIndexBuild`, the row's `commitReadyMembers` update,
  `commitIndexBuild` -- or `abortIndexBuild` with its cause -- and the row's
  delete. A change stream sees the commit as one `createIndexes` event for
  all of the build's indexes; replay and crash recovery read the three
  entries.
- While a build runs: `listIndexes` lists its indexes (`includeBuildUUIDs`,
  `includeIndexBuildInfo` describe the build), `currentOp` lists the build's
  own operation (`IndexBuildsCoordinatorMongod-N`), a request for the same
  index waits for it and one reusing its name with another key conflicts
  (86). `dropIndexes` of all of a build's indexes, `drop` and `dropDatabase`
  abort it (276, mongod's causes); dropping only some of them is refused
  (27); `killOp` of the build or of its command aborts it; `maxTimeMS` spent
  aborts it with 50 and a `writeConcernError`; `collMod` and
  `renameCollection` are refused with 12587.
- A build cut off by a crash or shutdown is run again from the start when
  the store opens, as mongod restarts unfinished builds, and commits as
  mongod's does.
- The pause failpoints `hangAfterStartingIndexBuild` and
  `hangIndexBuildBeforeCommit` hold a build still, under test commands.
- The Rust PostgreSQL server links the same storage: its `CREATE INDEX` keeps
  the one-step build, which blocks writes as PostgreSQL's does.

#### Filed

- RM-320: a transaction whose snapshot predates an index writes past it and
  the index misses the document (mongod answers 112 WriteConflict); older
  than this change.
- RM-321: an index still being built is not in `collStats` or
  `$indexStats` (mongod lists it, `building: true`).
- RM-224 keeps its `bulkWrite` session part, now with a gap test
  (`test_bulk_write_entries_carry_their_session`).

#### Fixed

- Fixes RM-187 (`tests/mongod_live/test_index_builds.py::test_writes_go_on_while_an_index_is_built`,
  `crates/secantus-storage/tests/hybrid_index_build.rs`).
- RM-224's index-build part (`tests/backlog/test_rm_219_apply_ops.py::test_oplog_entries_have_mongods_shape`).
- A `drop`'s oplog entry gives `numRecords` as an int32 while it fits, as
  mongod 8.2.11 does; it was an int64 (found by
  `tests/mongod_live/test_index_builds.py::test_what_ends_a_build`).
