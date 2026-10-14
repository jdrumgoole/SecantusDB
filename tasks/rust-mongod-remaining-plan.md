# Rust MongoDB server: the backlog -- plan (2026-10-14)

Scope: what is still open for `secantusd-rs` against mongod 8.2.11, taken from
`tasks/backlog.md` §7 (7.064 to 7.084, measured by the probes this month,
plus the open lines of 7.05 and 7.06). Every entry of 7.00 to 7.04 and 7.5 to
7.8 is closed. The exemplar is mongod only; the Python server is never the
reference (CLAUDE.md, "Design constraints").

The plan this file held before (2026-10-06, phases 0 to 4) is DONE; it is in
git history.

**Not all of it is code.** Roughly 60% of the open lines are code work a probe
measures, about 15 items are decisions for Joe (listed at the end), and the
rest are out of scope or accepted divergences. Sizes below are read from the
backlog, not measured: reproduce each item with its probe before starting it.

**Where things stand, measured 2026-10-14** (0.5.3-beta.194 tree, mongod
8.2.11; replies that differ):

| Probe | Differ |
| --- | --- |
| `auth_enforcement` | 388 of 12,053 (324 are the five commands of Phase 0b) |
| `diagnostic_commands` | 156 of 1,148 |
| `geo_queries` | 137 of 1,278 |
| `find_order` | 97 of 6,531 |
| `explain_commands` | 72 of 791 |
| `insert_delete_count_distinct` | 31 of 905 |
| `update_commands` | 17 of 1,295 |
| `transactions` | 14 of 1,419 |
| `user_role_commands` | 7 of 399 |
| `cursor_commands` | 7 of 776 |
| `change_stream_commands` | 6 of 409 |
| `time_series` | 6 of 187 |
| `index_admin` | 5 of 90 |
| `collation` | 4 of 764 |
| `out_and_merge` | 4 of 376 |
| `ddl_admin_commands` | 2 of 809 |
| `ttl_indexes` / `bulk_write_command` | 2 / 2 |
| `views`, `capped_collections`, `validators`, `scram_auth` | 0 |

## How each batch lands

1. Claim a branch (push it first); reproduce the items with the probe.
2. Fix them, with Rust tests pinned to mongod's answers.
3. Full check: Rust gates (workspace and each WiredTiger crate), all the
   MongoDB probes, the Python suite, the Go / Node / pymongo gauges.
4. One PR, CI, merge, teardown. Rewrite or delete the backlog lines in the
   same PR.
5. Cut a `secantusdb-v*` release when a user would notice, then the site post
   and `invoke deploy` (a Rust release ends at the live site).

## Phase 0 -- in flight at the end of 2026-10-14

- [ ] **0a. The 0.5.3-beta.194 site post.** Released (binaries, crates.io);
      the post, the pinned binary tag and the rewritten authorization section
      of `docs-rust/security.md` are on branch `site-mdb-0.5.3-beta.194`.
      Needs a PR, a merge and `invoke deploy`.
- [ ] **0b. Five commands mongod has and the server lacked** (7.084, 324
      rows; part of 7.082's "commands not there"): `dbHash`, `logRotate`,
      `connPoolStats`, `setDefaultRWConcern`, `setFeatureCompatibilityVersion`.
      A WIP commit is on the claim branch `rust-missing-admin-commands`
      (handlers, a probe `tools/probes/admin_commands.py`, Rust tests). It was
      written by a sub-agent stopped part-way and is NOT built, tested or
      measured. Build it, run the new probe and `auth_enforcement`, then land.
      mongod's `dbHash` is per-collection md5s plus a top-level md5; match its
      bytes on the same data, do not approximate.

## Phase 1 -- wrong answers and lost or duplicated writes

- [ ] `bulkWrite` as a retryable write is not replayed: a driver retry runs
      it twice (7.072).
- [ ] `upsertSupplied` is two steps, and the inserted document skips the
      validator (7.068).
- [ ] A clustered collection's `expireAfterSeconds` expires nothing (7.076).
- [ ] Two transactions creating one collection: mongod fails the second
      COMMIT with 112 (7.072).
- [ ] A cursor outlives a dropped index: mongod answers 175 (7.073).
- [ ] No view depth limit (165), no `system.views` collection (7.066).
- [ ] Collated `_id`: a collection created before 2026-10-11 has no claims
      for its existing documents (7.070).
- [ ] The 251 message after a server-side abort (7.065).
- [ ] Transaction change events lack `txnNumber` / `lsid`; `lsid.uid` is
      not the user's hash (7.072, 7.074).
- [ ] Capped eviction rescans the collection on every insert (7.065).

Size: one or two batches.

## Phase 2 -- the admin and diagnostic surface (7.082, 156 rows)

- [ ] Replica-set commands answered as a one-member set:
      `replSetResizeOplog`, `appendOplogNote`, `replSetInitiate` /
      `replSetReconfig`; list `local.oplog.rs`, `admin.system.version`,
      `config.system.sessions`; `electionCandidateMetrics`.
- [ ] Commands not there: `lockInfo`, `features`, `getClusterParameter`,
      `$listCatalog`, the plan-cache and query-settings commands.
- [ ] `serverStatus` sections (about 30), each only with numbers this server
      really has; a section with nothing true to say stays absent.
- [ ] `listDatabases` always lists `admin` and `config` (7.076); `dbStats`
      filesystem sizes; `hostInfo` `system` fields; `connectionStatus`
      privileges; `$indexStats` use counts; `collStats` `wiredTiger`.
- [ ] Runtime-settable parameters (`cursorTimeoutMillis`,
      `transactionLifetimeLimitSeconds`, ...). `notablescan` waits on Joe.

Size: two batches.

## Phase 3 -- geo (7.081, 137 rows)

- [ ] Results in index-scan order under a geo index: S2 cell order for
      `2dsphere`, geohash order for `2d` (about 100).
- [ ] `2d` index options and range (18), hinted geo indexes (10).
- [ ] `$centerSphere` over pi, the bounded `$geoNear` full read,
      `2dsphereIndexVersion: 2.5`, two indexes on one key in one
      `createIndexes`, `$geoNear` inside `$lookup` / `$unionWith`.

Size: one or two batches; scan order is the hard part.

## Phase 4 -- the planner and the order (7.080: 97; 7.075: 72)

- [ ] Ties between the scans a sort merges (about 15): mongod's priority
      queue.
- [ ] Which of several predicate indexes is read (1); `$match` moved ahead of
      `$addFields` (1).
- [ ] The storage router reads the index for `$ne`, `$nin`, `$exists`,
      regex and the other 19 filter shapes (7.075); this also removes the
      3.2x cost recorded under 7.080.
- [ ] `explain` of `aggregate` (about 24): slot-based plans, `DISTINCT_SCAN`,
      the added projection, `$match` ahead of `$unwind`, per-stage counts
      under `$limit`. The largest single job; last, or not at all.

Size: two batches without the aggregate `explain`, three or more with it.

## Phase 5 -- smaller tails

- [ ] Time series (7.071): sort a batch before placing it; close a bucket on
      size; the `_id` / index differences.
- [ ] Collation (7.070): `normalization: false` (ICU4X always normalises),
      the five collation types ICU4X lacks, `$densify` `bounds: "full"` across
      partitions, re-measure `$setWindowFields` partition order.
- [ ] Change streams (7.074): mongod's token-decode errors (15, 50811),
      `startAfter` a `dropDatabase` token, the unprobed items.
- [ ] `update` (7.077): 27 for `$text`, `$$CLUSTER_TIME`, the `$replaceWith`
      message. The upserted field order has no known rule.
- [ ] 7.078: `includeQueryStatsMetrics`, `distinct` over a multikey index and
      with a collation, 239 for an old `atClusterTime`.
- [ ] The `Slow query` line (7.07): `planSummary`, `keysExamined`,
      `docsExamined`, `nreturned`, `reslen`, the storage wait times.

Size: one or two batches.

## Phase 6 -- tooling and measurement

- [ ] 7.06: the binary smoke test should run the binary the provenance check
      verified; replace `timeout` in the release skill's finalize loop.
- [ ] Re-run every driver gauge against the Rust server and regenerate the
      driver panels; the `-rust-server` reports are stale (7.05).
- [ ] Re-measure throughput against mongod on the droplets: the 0.27x write
      figure (§3) is from 0.5.3-beta.160.
- [ ] Backlog entries that read as stale, to verify and close: 7.065's
      "`convertToCapped` is not implemented" (7.071 says it landed
      2026-10-11), the Go SDAM heartbeat item, the `featureCompatibilityVersion:
      "7.0"` note and the `Location91` codeName in §5.

## Decisions for Joe

Each unblocks rows that otherwise stay open.

| Decision | What it holds |
| --- | --- |
| Unknown-hint wording (mongod prints its query tree) | about 50 replies over six probes |
| Lazy cursors (`getMore` sees later writes; `batchSize: 0` runs nothing) | 3 replies, and memory on large results |
| Ties under a sort with a limit: copy libc++'s heap order | 62 |
| Index build order on disk (a catalog field the Python server must carry) | `listIndexes` order, duplicate-key index |
| A schema change waits on an open transaction | 2 |
| `explain`: mongod's plan or this server's access path | |
| Stored time-series buckets instead of derived | |
| `notablescan` | 3 |
| The profiler `filter`; `system.profile` | 6 |
| Wire compression (`OP_COMPRESSED`) | 2 |
| Server-side JavaScript (`$where`) | 3 Go leaves, several probe rows |
| Gate `configureFailPoint` / `enableTestCommands` | security |
| Python-server parity for the Rust-side changes | |
| The PostgreSQL crates' serde BSON decodes (7.077) | |
| Rust as the recommended default | product |

## Out of scope or accepted

- Out of scope: a second replica-set member, sharding, `text` and `hashed`
  indexes, `$meta` inside aggregation stages (it needs mongod's
  `DepsTracker`).
- Accepted: the Decimal128 transcendentals' last digit (Joe, 2026-10-06), a
  `w` tag set with several keys, the multikey sort that contradicts mongod's
  own tie-break.
- Not reproducible: the order of a set of roles (mongod keeps a hash set).

## Order

Phase 0, then Phase 1 released as soon as it lands, then Phases 2 and 3, a
release with refreshed gauges and a throughput re-measure (Phase 6), then
Phases 4 and 5. About one batch per long session: 9 to 12 batches, 2 or 3
more for the aggregate `explain`.
