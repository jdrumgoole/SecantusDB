### Rust MongoDB server: the slow-query log and the profiler say where the time went

A `Slow query` line and a `system.profile` entry on mongod say what an
operation did, not only how long it took: the keys and documents its plan
examined, the index keys it wrote, where it waited in the storage engine, the
locks it took, the read and write concern it ran under, its plan's cache key
and the engine that ran it. The Rust MongoDB server's had the duration, and the
counts of a streamed `find`. Now every line and entry is mongod 8.2's, field for
field and in mongod's order, with values the operation counted as it ran:
`count`, `distinct`, an `aggregate` (streamed or read whole), a `find` with a
blocking sort or an `_id` point read, each update and delete statement and
`findAndModify` run mongod's plan through the same streaming executor a `find`
cursor uses and count what it examines; the store counts the index keys each
write inserts and deletes as mongod does; and WiredTiger's own per-session
statistics -- bytes read and written by the operation's thread, the time it
spent reading, writing and waiting for the cache and for locks on the engine's
schema and data handles -- are read from every session the operation used and
printed as mongod's `storage` section.

What mongod prints that this server has no mechanism for is filed, each with a
gap test: execution admission control (`queues`, `storageExecutionMicros`,
RM-355), a write's lock and flow-control counts (RM-357), `queryShapeHash`
(RM-358), a profile entry's `execStats` (RM-359), `numInterruptChecks`
(RM-360), `totalOplogSlotDurationMicros` (RM-361), `resolvedViews` (RM-362),
the counts of reads it runs another way (RM-363) and two statistics of
WiredTiger 11 (RM-364).

#### Added

- `secantus-wt`: an operation's WiredTiger statistics
  (`operation_stats_scope`): each session closed while an operation is
  counting adds what `statistics:session` counted for it, and a
  transaction's session adds each statement's share
  (`Session::collect_operation_statistics`). Read only on a connection that
  keeps statistics; both servers open theirs with `statistics=(fast)`, as
  mongod does, so the engine counted these already.
- `secantus-storage`: `operation_scope`, which adds the index keys a write
  inserts and deletes as mongod counts them (its keys per index, the whole-
  array keys this store also writes left out, the `_id` index's key; a write
  kept beside the oplog -- `config.transactions`, a retryable
  `findAndModify`'s image -- is not the operation's). An index build counts
  apart from its command, as mongod builds on a thread of its own.
- The streaming executor (`explain/stream.rs`) runs a blocking sort of whole
  documents (`SortDocs`, read whole for the first batch, documents that
  compare equal in the order they came, held to the sort's memory budget), an
  `_id` or unique-index point read (`Point`), and a COUNT_SCAN that reads
  entries alone (`Stream::of_count`). A `find` with a blocking sort now
  streams its later batches from the sorted result.
- `count`, `distinct`, update and delete statements, `findAndModify` and an
  `aggregate` read whole run their plan through it when it can run it
  (`order::counted_plan`, `order::write_selection`, `order::find_counted`); a
  write then takes the RecordIds the plan returned.
- The line and the entry (`opdebug.rs`, `slowop.rs`, `profiler/record.rs`):
  `isFromUserConnection`, `collectionType`, `planningTimeMicros` (the time
  this server's planner took), `keysExamined` / `docsExamined` /
  `hasSortStage` for every read and write statement, `keysInserted` /
  `keysDeleted`, `nMatched` / `nModified` / `nUpserted` / `ndeleted` /
  `nreturned` on a `findAndModify`'s line, `planCacheShapeHash` / `queryHash`
  / `planCacheKey`, `queryFramework`, `locks` (the global IS mongod takes to
  read the statistics counted in the entry), `flowControl: {}` for a read,
  `readConcern` and `writeConcern` with their provenance (a `getMore` of an
  `aggregate` its cursor's), `waitForWriteConcernDurationMillis` (the
  journal flush a majority write waits for), `storage`, `cpuNanos` on Linux
  (where mongod reads a thread's CPU clock), `remote` and `protocol`, all in
  mongod's order. An error is `ok: 0, errMsg, errName, errCode`, as mongod's.

#### Fixed

- A write statement's line had `ok` and `errCode` only, and no plan; an
  `update` or `delete` command's line is `db.$cmd`, its statements' lines
  carry the collection, as mongod's.
- `explain`'s COUNT_SCAN counted no key for the call that finds its end:
  mongod counts it whether or not a key lies past the bounds (`{x: {$gt: 3}}`
  over keys 0 to 11 examines 9). Fixes RM-356
  (`tests/backlog/test_rm_355_364.py::test_explain_counts_the_end_of_a_count_scan`).
- Fixes RM-023 (`tests/backlog/test_rm_355_364.py::test_a_read_that_does_not_stream_counts_what_it_examined`,
  `::test_a_write_counts_what_it_examined_and_the_keys_it_wrote`,
  `::test_a_line_names_its_engine_concerns_and_collection`,
  `::test_storage_says_what_the_engine_read`; and
  `tests/mongod_live/test_slow_query_lines.py`). Its original gap test,
  `tests/backlog/test_rm_001_028.py::test_slow_query_line_says_where_the_time_went`,
  now fails only on mongod's always-present `storageExecutionMicros` and is
  RM-355's.

#### Cost

This is not free, and the latency is measurable. An A/B of release builds
(this branch against its `origin/main` base, both servers up at once and
alternated round by round, 12 rounds of 1,000 operations, medians, an
M-series Mac at load average 2-6, 2026-10-27):

| operation | base | this | change | of which the counting of statistics and keys |
| --- | --- | --- | --- | --- |
| `find_one({_id})` | 111-122 us | 134-147 us | +20% | +14% |
| `find` of an index range | 178-196 us | 200-220 us | +12% | +10% |
| `count` through an index | 118-128 us | 151-165 us | +28% | +9% |
| `update_one` by an indexed field, `w: 1` | 149-167 us | 229-257 us | +54% | +14% |

The last column is measured by switching the operation's statistics and
key counts off in the same build. The rest of a `count`'s and a write's
cost is that their read now runs mongod's plan through the streaming
executor, which counts what it examines, where it used to be the store's
own count or read. The cost is per operation, whatever the threshold: whether
an operation is slow is known only at its end, and a session closed before
then must be read when it closes. A statement neither logged nor profiled
does not work out its plan's hashes, and an update or delete of one
document whose plan reads in the store's order reads no extra candidates.

- Reading a session's statistics when it closes costs about 1.5 us
  (release build: a session's open, cursor, search and close took 2.3 us,
  3.8 us with the read).
- The PostgreSQL server, which links the same storage, never counts an
  operation and pays one thread-local check per session close.
