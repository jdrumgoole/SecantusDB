### The Rust MongoDB server's `explain` follows mongod's planner

`explain` on `secantusd-rs` now reports the plan `mongod` 8.2.11 reports for
the same command: the index bounds, what the fetch rechecks, whether an
index serves the sort or covers the projection, and the stages `count`,
`distinct`, `update`, `delete` and `findAndModify` have of their own. A
comparison of 786 `explain` replies went from 771 different to 72.

Two defects outside `explain` turned up and are fixed. An operator the query
language does not have was accepted over an empty or missing collection by
every command but `find`. And `count` or `delete` by `_id` on a collection
that was never written could answer an internal storage error.

#### Changed

- **Rust MongoDB server: `explain` is derived from mongod's planning
  rules.** `indexBounds` on every index scan, in mongod's notation and for
  every operator that bounds an index; `EXPRESS_IXSCAN` and `IDHACK` for a
  point lookup; `PROJECTION_COVERED`; `SORT_MERGE`; `COUNT_SCAN` and
  `RECORD_STORE_FAST_COUNT`; `DISTINCT_SCAN`; `UPDATE`, `DELETE`,
  `BATCHED_DELETE`, `EXPRESS_UPDATE` and `EXPRESS_DELETE`; `EOF` for a
  missing collection or a filter that matches nothing; a view explained
  through its pipeline. The reply has mongod's envelope (`explainVersion`,
  the planner flags, `command`, `serverParameters`).
- **`executionStats` counts what the reported plan examines**, stage by
  stage: a `LIMIT` stops its input, a multikey scan drops repeated
  documents, and a scan over several intervals examines the key that ends
  each one.
- **`explain` checks its arguments as mongod does**: the verbosity, unknown
  fields, `writeConcern`, commands that cannot be explained, a write batch
  of more or fewer than one statement. With no `verbosity` it runs at
  `allPlansExecution`.
- `explain` describes the plan mongod runs. This server's storage layer may
  read the same documents another way; `tasks/backlog.md` 7.075 lists where.
  `rejectedPlans` is always empty.

#### Fixed

- **Rust MongoDB server: an unknown query operator is refused over an empty
  or missing collection.** `{a: {$nope: 1}}` was accepted by `count`,
  `distinct`, `aggregate`, `update`, `delete` and `findAndModify` when there
  was no document to match it against, and answered as if the filter were
  valid. `find` refused it as `unsupported query operator`. All now answer
  `unknown operator: $nope` (2), as `mongod` does.
- **Rust MongoDB server: `count` and `delete` by `_id` on a collection that
  was never written** answered `WiredTiger error: No such file or
  directory`. They find nothing, as `mongod` does.
- An `explain` reply no longer carries `atClusterTime` under
  `readConcern: {level: "snapshot"}`.
