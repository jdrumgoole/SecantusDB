### The Rust MongoDB server keeps mongod's query stats

Set `internalQueryStatsRateLimit` off its default of 0 and `secantusd-rs` now
records every `find`, `aggregate`, `distinct` and `count` that succeeds, as
mongod 8.2 does, and `$queryStats` lists them. Each entry is filed under
mongod's key: the query shape printed with debug type strings
(`{a: {$gt: "?number"}}`), the command's comment, read concern, API
parameters, hint and options, the client's handshake metadata and the kind of
collection, with `keyHash` and `queryShapeHash` computed as mongod computes
them. The metrics are mongod's too: execution counts and times, documents
returned and examined, working time, interrupt checks per second, and how
each run chose its plan.

A query that leaves a cursor open is recorded when the cursor closes, however
it closes, with what its `getMore`s added; a change stream at each of its
operations. A command that fails is not recorded.

#### Fixed

- `setParameter` takes `internalQueryStatsRateLimit` (a whole number of at
  least -1, coerced as mongod coerces it); -1 records every query, `n` a
  sliding window of `n` a second.
- `$queryStats` lists the store's entries: `key`, `keyHash`,
  `queryShapeHash`, `metrics` and `asOf`, in mongod 8.2.11's field order.
- A `find` with `showRecordId` hashes its `queryShapeHash` over the
  projection mongod gives it (`$recordId: {$meta: "recordId"}`), on its
  `Slow query` line, profile entry and `explain` too.
- Fixes RM-215 (tests/backlog/test_rm_found_2026_10_20b.py::test_query_stats_records_queries_when_asked)

#### Added

- `tests/mongod_live/test_query_stats.py` compares each recorded key,
  `keyHash` and `queryShapeHash` with mongod's over the query-shape corpus,
  and the metrics, cursor, planner and rate-limit behaviour.
- Filed RM-532 (`$queryStats`' cache size, explains and
  `transformIdentifiers`) and RM-533 (a date-part operator's argument
  written as an array of one fails).
